//! Pack file (.pack) and pack index (.idx) support.
//! - reading: parse entries, resolve REF_DELTA and OFS_DELTA chains,
//!   idx v1/v2 lookup
//! - writing: emit valid pack v2 + idx v2 (full objects, no deltas —
//!   always legal)

use crate::object::{hash_object, ObjType, Oid};
use crate::sha1::Sha1;
use crate::util::{be_u32, be_u64, crc32, GitError, Result};
use crate::zlib;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub const OBJ_OFS_DELTA: u8 = 6;
pub const OBJ_REF_DELTA: u8 = 7;

// ============================== delta ==============================

/// Apply a git-format delta to `base`.
pub fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>> {
    let mut pos = 0usize;
    let src_size = read_varint(delta, &mut pos)?;
    if src_size as usize != base.len() {
        return Err(GitError::ObjectCorrupt(format!(
            "delta base size mismatch: expected {} got {}",
            src_size,
            base.len()
        )));
    }
    let tgt_size = read_varint(delta, &mut pos)? as usize;
    // A corrupt delta can claim an absurd target size; cap the eager
    // allocation and let the vector grow as copies actually land.
    let mut out = Vec::with_capacity(tgt_size.min(1 << 28));
    while pos < delta.len() {
        let cmd = delta[pos];
        pos += 1;
        if cmd & 0x80 != 0 {
            // copy from base
            let mut offset = 0usize;
            let mut size = 0usize;
            for i in 0..4 {
                if cmd & (1 << i) != 0 {
                    offset |= (*delta.get(pos).ok_or_else(|| {
                        GitError::ObjectCorrupt("delta copy field truncated".into())
                    })? as usize)
                        << (i * 8);
                    pos += 1;
                }
            }
            for i in 0..3 {
                if cmd & (0x10 << i) != 0 {
                    size |= (*delta.get(pos).ok_or_else(|| {
                        GitError::ObjectCorrupt("delta size field truncated".into())
                    })? as usize)
                        << (i * 8);
                    pos += 1;
                }
            }
            if size == 0 {
                size = 0x10000;
            }
            if offset + size > base.len() {
                return Err(GitError::ObjectCorrupt("delta copy out of range".into()));
            }
            out.extend_from_slice(&base[offset..offset + size]);
        } else if cmd != 0 {
            // literal insert
            let n = cmd as usize;
            if pos + n > delta.len() {
                return Err(GitError::ObjectCorrupt("delta insert out of range".into()));
            }
            out.extend_from_slice(&delta[pos..pos + n]);
            pos += n;
        } else {
            return Err(GitError::ObjectCorrupt("delta opcode 0".into()));
        }
    }
    if out.len() != tgt_size {
        return Err(GitError::ObjectCorrupt(format!(
            "delta result size mismatch: expected {} got {}",
            tgt_size,
            out.len()
        )));
    }
    Ok(out)
}

fn read_varint(data: &[u8], pos: &mut usize) -> Result<u64> {
    let mut v: u64 = 0;
    let mut shift = 0;
    loop {
        if *pos >= data.len() {
            return Err(GitError::Parse("varint overrun".into()));
        }
        if shift >= 64 {
            return Err(GitError::Parse("varint too long".into()));
        }
        let b = data[*pos];
        *pos += 1;
        v |= ((b & 0x7f) as u64) << shift;
        shift += 7;
        if b & 0x80 == 0 {
            break;
        }
    }
    Ok(v)
}

// ============================== pack entry parsing ==============================

pub struct EntryHeader {
    pub type_id: u8,
    pub size: u64,
    /// for OFS_DELTA: offset of base object within the pack
    pub base_offset: Option<u64>,
    /// for REF_DELTA: oid of base object
    pub base_oid: Option<Oid>,
    /// offset in pack where the zlib stream starts
    pub data_offset: usize,
    /// offset where the whole entry starts
    pub entry_offset: usize,
}

/// Read the entry header starting at `offset`.
pub fn parse_entry_header(pack: &[u8], offset: usize) -> Result<EntryHeader> {
    let mut i = offset;
    if i >= pack.len() {
        return Err(GitError::Parse("pack: entry offset out of range".into()));
    }
    let byte_at = |i: usize| -> Result<u8> {
        pack.get(i)
            .copied()
            .ok_or_else(|| GitError::Parse("pack: truncated entry header".into()))
    };
    let mut c = byte_at(i)?;
    i += 1;
    let type_id = (c >> 4) & 0x7;
    let mut size: u64 = (c & 0x0f) as u64;
    let mut shift = 4;
    while c & 0x80 != 0 {
        c = byte_at(i)?;
        i += 1;
        size |= ((c & 0x7f) as u64) << shift;
        shift += 7;
    }
    let mut base_offset = None;
    let mut base_oid = None;
    match type_id {
        OBJ_OFS_DELTA => {
            // offset encoding: n bytes; value = ((value+1)<<7) | (b&0x7f)
            let mut b = byte_at(i)?;
            i += 1;
            let mut off: u64 = (b & 0x7f) as u64;
            while b & 0x80 != 0 {
                b = byte_at(i)?;
                i += 1;
                off = ((off + 1) << 7) | (b & 0x7f) as u64;
            }
            base_offset = Some((offset as u64).checked_sub(off).ok_or_else(|| {
                GitError::Parse("pack: ofs-delta underflow".into())
            })?);
        }
        OBJ_REF_DELTA => {
            if i + 20 > pack.len() {
                return Err(GitError::Parse("pack: truncated ref-delta base".into()));
            }
            base_oid = Some(Oid::from_bytes(&pack[i..i + 20])?);
            i += 20;
        }
        _ => {}
    }
    Ok(EntryHeader {
        type_id,
        size,
        base_offset,
        base_oid,
        data_offset: i,
        entry_offset: offset,
    })
}

// ============================== pack + idx reading ==============================

struct IdxData {
    /// sorted oid list
    oids: Vec<Oid>,
    /// parallel offset list
    offsets: Vec<u64>,
    fanout: [u32; 256],
}

pub struct Pack {
    #[allow(dead_code)]
    pub pack_path: PathBuf,
    pub data: Vec<u8>,
    idx: IdxData,
    /// cache of resolved objects keyed by pack offset
    cache: RefCell<HashMap<u64, Rc<(ObjType, Vec<u8>)>>>,
}

fn load_idx(path: &Path) -> Result<IdxData> {
    let raw = std::fs::read(path)?;
    if raw.len() >= 8 && &raw[0..4] == b"\xfftOc" {
        // idx v2
        let version = be_u32(&raw[4..8]);
        if version != 2 {
            return Err(GitError::Parse(format!("idx version {} unsupported", version)));
        }
        if raw.len() < 8 + 256 * 4 {
            return Err(GitError::Parse("idx: truncated fanout".into()));
        }
        let mut fanout = [0u32; 256];
        for i in 0..256 {
            fanout[i] = be_u32(&raw[8 + i * 4..]);
        }
        let n = fanout[255] as usize;
        let sha_base = 8 + 256 * 4;
        let crc_base = sha_base + n * 20;
        let off_base = crc_base + n * 4;
        let big_base = off_base + n * 4;
        if raw.len() < big_base {
            return Err(GitError::Parse("idx: truncated tables".into()));
        }
        let big_avail = (raw.len() - big_base) / 8;
        let mut oids = Vec::with_capacity(n);
        let mut offsets = Vec::with_capacity(n);
        for i in 0..n {
            oids.push(Oid::from_bytes(&raw[sha_base + i * 20..sha_base + i * 20 + 20])?);
            let o32 = be_u32(&raw[off_base + i * 4..]);
            let off = if o32 & 0x8000_0000 != 0 {
                let bi = (o32 & 0x7fff_ffff) as usize;
                if bi >= big_avail {
                    return Err(GitError::Parse("idx: large-offset slot out of range".into()));
                }
                be_u64(&raw[big_base + bi * 8..])
            } else {
                o32 as u64
            };
            offsets.push(off);
        }
        Ok(IdxData { oids, offsets, fanout })
    } else {
        // idx v1: fanout at 0, then entries (4-byte offset + 20-byte sha)
        if raw.len() < 1024 {
            return Err(GitError::Parse("idx too short".into()));
        }
        let mut fanout = [0u32; 256];
        for i in 0..256 {
            fanout[i] = be_u32(&raw[i * 4..]);
        }
        let n = fanout[255] as usize;
        let base = 1024;
        if raw.len() < base + n * 24 {
            return Err(GitError::Parse("idx: truncated v1 entries".into()));
        }
        let mut oids = Vec::with_capacity(n);
        let mut offsets = Vec::with_capacity(n);
        for i in 0..n {
            let rec = base + i * 24;
            offsets.push(be_u32(&raw[rec..]) as u64);
            oids.push(Oid::from_bytes(&raw[rec + 4..rec + 24])?);
        }
        Ok(IdxData { oids, offsets, fanout })
    }
}

impl Pack {
    pub fn open(idx_path: &Path) -> Result<Rc<Pack>> {
        let pack_path = idx_path.with_extension("pack");
        let data = std::fs::read(&pack_path)?;
        if data.len() < 12 + 20 || &data[0..4] != b"PACK" {
            return Err(GitError::Parse(format!("bad pack file {:?}", pack_path)));
        }
        let idx = load_idx(idx_path)?;
        Ok(Rc::new(Pack {
            pack_path,
            data,
            idx,
            cache: RefCell::new(HashMap::new()),
        }))
    }

    /// Find all entry offsets by scanning a pack buffer.
    pub fn scan_entries(pack: &[u8]) -> Result<Vec<u64>> {
        if pack.len() < 12 || &pack[0..4] != b"PACK" {
            return Err(GitError::Parse("bad pack header".into()));
        }
        let n = be_u32(&pack[8..12]) as usize;
        let mut offs = Vec::with_capacity(n);
        let mut pos = 12usize;
        for _ in 0..n {
            offs.push(pos as u64);
            let h = parse_entry_header(pack, pos)?;
            let (_, used) = zlib::inflate(&pack[h.data_offset..], h.size as usize)?;
            pos = h.data_offset + used;
        }
        Ok(offs)
    }

    #[allow(dead_code)]
    pub fn contains(&self, oid: &Oid) -> bool {
        self.find_offset(oid).is_some()
    }

    pub fn find_offset(&self, oid: &Oid) -> Option<u64> {
        let first_byte = oid.0[0] as usize;
        let lo = if first_byte == 0 {
            0
        } else {
            self.idx.fanout[first_byte - 1] as usize
        };
        let hi = self.idx.fanout[first_byte] as usize;
        let slice = &self.idx.oids[lo..hi];
        match slice.binary_search(oid) {
            Ok(i) => Some(self.idx.offsets[lo + i]),
            Err(_) => None,
        }
    }

    /// Number of objects in this pack.
    pub fn len(&self) -> usize {
        self.idx.oids.len()
    }

    /// Iterate all object ids in idx order.
    pub fn oids(&self) -> &[Oid] {
        &self.idx.oids
    }

    /// Resolve the object at `offset`; resolves delta chains internally.
    /// `resolve_ref` looks up base objects for REF_DELTA outside this pack
    /// (needed for thin packs); may return None if unavailable.
    pub fn read_at(
        &self,
        offset: u64,
        resolve_ref: &dyn Fn(&Oid) -> Option<Rc<(ObjType, Vec<u8>)>>,
    ) -> Result<Rc<(ObjType, Vec<u8>)>> {
        if let Some(v) = self.cache.borrow().get(&offset) {
            return Ok(v.clone());
        }
        let h = parse_entry_header(&self.data, offset as usize)?;
        let result: Rc<(ObjType, Vec<u8>)> = match h.type_id {
            1 | 2 | 3 | 4 => {
                let ty = ObjType::from_pack_id(h.type_id)?;
                let (body, _) = zlib::inflate(&self.data[h.data_offset..], h.size as usize)?;
                if body.len() as u64 != h.size {
                    return Err(GitError::ObjectCorrupt("pack: size mismatch".into()));
                }
                Rc::new((ty, body))
            }
            OBJ_OFS_DELTA => {
                let base = self.read_at(h.base_offset.unwrap(), resolve_ref)?;
                let (delta, _) = zlib::inflate(&self.data[h.data_offset..], h.size as usize)?;
                let body = apply_delta(&base.1, &delta)?;
                Rc::new((base.0, body))
            }
            OBJ_REF_DELTA => {
                let base = match self.find_offset(&h.base_oid.unwrap()) {
                    Some(boff) => self.read_at(boff, resolve_ref)?,
                    None => resolve_ref(&h.base_oid.unwrap()).ok_or_else(|| {
                        GitError::ObjectCorrupt(format!(
                            "ref-delta base {} not found",
                            h.base_oid.unwrap()
                        ))
                    })?,
                };
                let (delta, _) = zlib::inflate(&self.data[h.data_offset..], h.size as usize)?;
                let body = apply_delta(&base.1, &delta)?;
                Rc::new((base.0, body))
            }
            t => return Err(GitError::Parse(format!("pack: bad type id {}", t))),
        };
        self.cache.borrow_mut().insert(offset, result.clone());
        Ok(result)
    }

    /// Read + resolve by oid.
    pub fn read_object(
        &self,
        oid: &Oid,
        resolve_ref: &dyn Fn(&Oid) -> Option<Rc<(ObjType, Vec<u8>)>>,
    ) -> Result<Option<Rc<(ObjType, Vec<u8>)>>> {
        match self.find_offset(oid) {
            Some(off) => Ok(Some(self.read_at(off, resolve_ref)?)),
            None => Ok(None),
        }
    }
}

// ============================== received pack resolution ==============================

struct PackResolver<'a> {
    headers: Vec<EntryHeader>,
    bodies: &'a [Vec<u8>],
    off_to_idx: HashMap<u64, usize>,
    oid_to_idx: HashMap<Oid, usize>,
    resolved: Vec<Option<Rc<(ObjType, Vec<u8>)>>>,
    resolving: Vec<bool>,
    resolve_ref: &'a dyn Fn(&Oid) -> Option<Rc<(ObjType, Vec<u8>)>>,
    used_external: bool,
}

impl<'a> PackResolver<'a> {
    fn resolve_one(&mut self, i: usize) -> Result<Rc<(ObjType, Vec<u8>)>> {
        if let Some(r) = &self.resolved[i] {
            return Ok(r.clone());
        }
        if self.resolving[i] {
            return Err(GitError::ObjectCorrupt("circular delta chain".into()));
        }
        self.resolving[i] = true;
        let (type_id, base_off, base_oid) = {
            let h = &self.headers[i];
            (h.type_id, h.base_offset, h.base_oid)
        };
        let r: Rc<(ObjType, Vec<u8>)> = match type_id {
            1 | 2 | 3 | 4 => Rc::new((ObjType::from_pack_id(type_id)?, self.bodies[i].clone())),
            OBJ_OFS_DELTA => {
                let bi = *self
                    .off_to_idx
                    .get(&base_off.unwrap())
                    .ok_or_else(|| GitError::ObjectCorrupt("ofs-delta base missing".into()))?;
                let base = self.resolve_one(bi)?;
                Rc::new((base.0, apply_delta(&base.1, &self.bodies[i])?))
            }
            OBJ_REF_DELTA => {
                let base_oid = base_oid.unwrap();
                // in-pack base?
                let bi = self.oid_to_idx.get(&base_oid).copied();
                let base = if let Some(bi) = bi {
                    self.resolve_one(bi)?
                } else if let Some(ext) = (self.resolve_ref)(&base_oid) {
                    self.used_external = true;
                    ext
                } else {
                    // base must be an unresolved pack entry: resolve all
                    // remaining entries until we discover it
                    let mut found = None;
                    for j in 0..self.headers.len() {
                        if self.resolved[j].is_none() {
                            let _ = self.resolve_one(j);
                        }
                        if let Some(&k) = self.oid_to_idx.get(&base_oid) {
                            found = Some(k);
                            break;
                        }
                    }
                    match found {
                        Some(k) => self.resolve_one(k)?,
                        None => {
                            return Err(GitError::ObjectCorrupt(format!(
                                "ref-delta base {} missing",
                                base_oid
                            )))
                        }
                    }
                };
                Rc::new((base.0, apply_delta(&base.1, &self.bodies[i])?))
            }
            t => return Err(GitError::Parse(format!("pack: bad type {}", t))),
        };
        let oid = hash_object(r.0, &r.1);
        self.oid_to_idx.insert(oid, i);
        self.resolving[i] = false;
        self.resolved[i] = Some(r.clone());
        Ok(r)
    }
}

/// Result of fully resolving a pack.
pub struct ResolvedPack {
    /// (oid, type, full content) in pack order
    pub objects: Vec<(Oid, ObjType, Vec<u8>)>,
    /// entry offsets in pack order (for idx writing)
    pub offsets: Vec<u64>,
    /// true if any ref-delta base came from outside the pack (thin pack)
    pub thin: bool,
}

/// Fully resolve a received pack (possibly thin) into a flat object list.
/// `resolve_ref` must return objects already in our odb for ref-deltas
/// whose base is not inside the pack.
pub fn resolve_pack(
    pack_bytes: &[u8],
    resolve_ref: &dyn Fn(&Oid) -> Option<Rc<(ObjType, Vec<u8>)>>,
) -> Result<ResolvedPack> {
    if pack_bytes.len() < 12 || &pack_bytes[0..4] != b"PACK" {
        return Err(GitError::Parse("bad pack header".into()));
    }
    let count = be_u32(&pack_bytes[8..12]) as usize;
    // single pass: inflate each entry once to discover its compressed length
    // and cache the raw entry body (object or delta) for resolution
    let mut headers = Vec::with_capacity(count);
    let mut bodies = Vec::with_capacity(count);
    let mut offsets = Vec::with_capacity(count);
    let mut pos = 12usize;
    for _ in 0..count {
        let h = parse_entry_header(pack_bytes, pos)?;
        let (body, used) = zlib::inflate(&pack_bytes[h.data_offset..], h.size as usize)?;
        pos = h.data_offset + used;
        offsets.push(h.entry_offset as u64);
        headers.push(h);
        bodies.push(body);
    }
    let off_to_idx: HashMap<u64, usize> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| (h.entry_offset as u64, i))
        .collect();
    let mut r = PackResolver {
        headers,
        bodies: &bodies,
        off_to_idx,
        oid_to_idx: HashMap::new(),
        resolved: vec![None; count],
        resolving: vec![false; count],
        resolve_ref,
        used_external: false,
    };
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let obj = r.resolve_one(i)?;
        let oid = hash_object(obj.0, &obj.1);
        out.push((oid, obj.0, obj.1.clone()));
    }
    Ok(ResolvedPack {
        objects: out,
        offsets,
        thin: r.used_external,
    })
}

// ============================== delta creation ==============================

/// Position index over a base buffer for delta matching: head/next hash
/// chains over 8-byte keys at stride-DELTA_STRIDE positions.
struct DeltaIndex {
    head: Vec<i32>,
    next: Vec<i32>,
}

const DELTA_STRIDE: usize = 4;
const DELTA_HBITS: usize = 16;

impl DeltaIndex {
    fn build(base: &[u8]) -> DeltaIndex {
        let mut head = vec![-1i32; 1 << DELTA_HBITS];
        let mut next = vec![-1i32; base.len() / DELTA_STRIDE + 1];
        for i in (0..base.len()).step_by(DELTA_STRIDE) {
            if i + 8 > base.len() {
                break;
            }
            let k = u64::from_le_bytes(base[i..i + 8].try_into().unwrap());
            let h = (k.wrapping_mul(0x9E3779B97F4A7C15) >> (64 - DELTA_HBITS)) as usize;
            next[i / DELTA_STRIDE] = head[h];
            head[h] = i as i32;
        }
        DeltaIndex { head, next }
    }
}

/// Create a git-format delta: varint(src_size) varint(tgt_size) followed by
/// copy ops (0x80|flags + offset/size bytes) and literal inserts (1..=127).
#[allow(dead_code)]
pub fn create_delta(base: &[u8], target: &[u8]) -> Vec<u8> {
    create_delta_idx(base, target, &DeltaIndex::build(base))
}

fn create_delta_idx(base: &[u8], target: &[u8], idx: &DeltaIndex) -> Vec<u8> {
    let mut out = Vec::with_capacity(target.len() / 4 + 32);
    write_le_varint(&mut out, base.len() as u64);
    write_le_varint(&mut out, target.len() as u64);
    if base.len() < 8 || target.is_empty() {
        emit_insert(&mut out, target);
        return out;
    }
    let DeltaIndex { head, next } = idx;
    let mut i = 0usize; // target scan pos
    let mut lit = 0usize; // start of pending literal run
    while i + 8 <= target.len() {
        let k = u64::from_le_bytes(target[i..i + 8].try_into().unwrap());
        let h = (k.wrapping_mul(0x9E3779B97F4A7C15) >> (64 - DELTA_HBITS)) as usize;
        let mut cand = head[h];
        let mut best_len = 0usize;
        let mut best_off = 0usize;
        let mut probes = 0;
        while cand >= 0 && probes < 4 {
            let c = cand as usize;
            // forward-extend the match in u64 chunks
            let mut l = 0usize;
            let maxl = (target.len() - i).min(base.len() - c);
            while l + 8 <= maxl
                && u64::from_le_bytes(base[c + l..c + l + 8].try_into().unwrap())
                    == u64::from_le_bytes(target[i + l..i + l + 8].try_into().unwrap())
            {
                l += 8;
            }
            while l < maxl && base[c + l] == target[i + l] {
                l += 1;
            }
            if l > best_len {
                best_len = l;
                best_off = c;
            }
            cand = next[c / DELTA_STRIDE];
            probes += 1;
        }
        if best_len >= 12 {
            if lit < i {
                emit_insert(&mut out, &target[lit..i]);
            }
            emit_copy(&mut out, best_off, best_len);
            i += best_len;
            lit = i;
        } else {
            i += 1;
            // literal op carries at most 127 bytes
            if i - lit == 127 {
                emit_insert(&mut out, &target[lit..i]);
                lit = i;
            }
        }
    }
    if lit < target.len() {
        emit_insert(&mut out, &target[lit..]);
    }
    out
}

fn write_le_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            break;
        }
    }
}

fn emit_insert(out: &mut Vec<u8>, mut data: &[u8]) {
    while !data.is_empty() {
        let n = data.len().min(127);
        out.push(n as u8);
        out.extend_from_slice(&data[..n]);
        data = &data[n..];
    }
}

fn emit_copy(out: &mut Vec<u8>, off: usize, mut size: usize) {
    let mut off = off;
    while size > 0 {
        let n = size.min(0xFFFFFF);
        let mut op = 0x80u8;
        let mut bytes = [0u8; 7];
        let mut nb = 0;
        for b in 0..4 {
            let v = (off >> (b * 8)) & 0xff;
            if v != 0 {
                op |= 1 << b;
                bytes[nb] = v as u8;
                nb += 1;
            }
        }
        // size bytes come after offset bytes in the op layout:
        // flags bits 0-3 = offset bytes, bits 4-6 = size bytes
        let mut size_bytes = [0u8; 3];
        let mut snb = 0;
        for b in 0..3 {
            let v = (n >> (b * 8)) & 0xff;
            if v != 0 {
                op |= 0x10 << b;
                size_bytes[snb] = v as u8;
                snb += 1;
            }
        }
        out.push(op);
        out.extend_from_slice(&bytes[..nb]);
        out.extend_from_slice(&size_bytes[..snb]);
        off += n;
        size -= n;
    }
}

/// Encode the OFS_DELTA negative-offset value (base-128 with +1 shifts).
fn encode_ofs_delta(mut off: u64) -> Vec<u8> {
    let mut bytes = vec![(off & 0x7f) as u8];
    off >>= 7;
    while off != 0 {
        off -= 1;
        bytes.push(0x80 | (off & 0x7f) as u8);
        off >>= 7;
    }
    bytes.reverse();
    bytes
}

/// Attempt delta compression when writing a pack: each object is tried
/// against the previous WINDOW objects of the same type; the smallest
/// delta under half the object's size wins.
/// Returns (pack_bytes, oids_in_pack_order).
pub fn write_pack_delta(objects: &[PackObj]) -> (Vec<u8>, Vec<Oid>) {
    // order: commits first (checkout speed), then trees, blobs, tags;
    // within a type, larger objects first so they serve as delta bases
    fn rank(t: ObjType) -> u8 {
        match t {
            ObjType::Commit => 0,
            ObjType::Tree => 1,
            ObjType::Blob => 2,
            ObjType::Tag => 3,
        }
    }
    let mut order: Vec<usize> = (0..objects.len()).collect();
    order.sort_by_key(|&i| {
        (
            rank(objects[i].ty),
            std::cmp::Reverse(objects[i].data.len()),
        )
    });
    // Phase 1 (parallel): partition `order` into per-thread contiguous
    // chunks — each thread keeps its own `recent` window (same scheme as
    // `git pack-objects --threads`). Each produces deflated entry bodies;
    // headers are encoded serially afterwards because OFS_DELTA distances
    // depend on absolute pack offsets.
    struct EntryPlan {
        /// index into `order` of the delta base (same chunk), if delta'd
        base_pos: Option<usize>,
        /// uncompressed size that goes in the entry header
        raw_size: u64,
        /// is_delta
        delta: bool,
        /// deflated body
        body: Vec<u8>,
    }
    fn deltify_chunk(objects: &[PackObj], chunk: &[(usize, usize)]) -> Vec<EntryPlan> {
        const WINDOW: usize = 10;
        type Recent = (usize, usize, std::rc::Rc<std::cell::OnceCell<DeltaIndex>>);
        let mut recent: [Vec<Recent>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        let mut plans = Vec::with_capacity(chunk.len());
        for &(pos, oi) in chunk {
            let o = &objects[oi];
            let r = rank(o.ty) as usize;
            let mut best: Option<(usize, Vec<u8>)> = None; // (base order pos, delta)
            for (bi, bpos, cell) in recent[r].iter().rev().take(WINDOW) {
                let b = &objects[*bi];
                // skip mismatched sizes — they never delta well; small objects
                // need near-identical bases so gate tighter
                let (lo, hi) = if o.data.len() < 8192 { (4, 4) } else { (8, 8) };
                if b.data.len() > o.data.len() * hi + 64 || o.data.len() > b.data.len() * lo + 64 {
                    continue;
                }
                let idx = cell.get_or_init(|| DeltaIndex::build(&b.data));
                let d = create_delta_idx(&b.data, &o.data, idx);
                if d.len() * 2 < o.data.len()
                    && best.as_ref().map(|x| d.len() < x.1.len()).unwrap_or(true)
                {
                    best = Some((*bpos, d));
                }
            }
            let plan = match best {
                Some((bpos, d)) => EntryPlan {
                    base_pos: Some(bpos),
                    raw_size: d.len() as u64,
                    delta: true,
                    body: zlib::deflate(&d),
                },
                None => EntryPlan {
                    base_pos: None,
                    raw_size: o.data.len() as u64,
                    delta: false,
                    body: zlib::deflate(&o.data),
                },
            };
            recent[r].push((oi, pos, std::rc::Rc::new(std::cell::OnceCell::new())));
            if recent[r].len() > WINDOW {
                recent[r].remove(0);
            }
            plans.push(plan);
        }
        plans
    }

    // (order position, object index) pairs per chunk. Chunks are split
    // by cumulative uncompressed size — delta cost scales with data
    // size, so position-even chunks would pile all large blobs on one
    // thread.
    let indexed: Vec<(usize, usize)> = order.iter().copied().enumerate().collect();
    let nthreads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(8)
        .min(indexed.len().max(1));
    let total: usize = objects.iter().map(|o| o.data.len().min(1 << 20)).sum();
    let target = (total / nthreads).max(1);
    let mut chunk_bounds: Vec<(usize, usize)> = Vec::new();
    let (mut start, mut acc) = (0usize, 0usize);
    for (pos, &oi) in order.iter().enumerate() {
        acc += objects[oi].data.len().min(1 << 20);
        if acc >= target && pos > start && chunk_bounds.len() + 1 < nthreads {
            chunk_bounds.push((start, pos));
            start = pos;
            acc = 0;
        }
    }
    chunk_bounds.push((start, indexed.len()));
    let mut plans: Vec<EntryPlan> = Vec::with_capacity(indexed.len());
    if chunk_bounds.len() <= 1 {
        plans = deltify_chunk(objects, &indexed[..]);
    } else {
        std::thread::scope(|s| {
            let handles: Vec<_> = chunk_bounds
                .iter()
                .map(|&(a, b)| {
                    let chunk: &[(usize, usize)] = &indexed[a..b];
                    s.spawn(move || deltify_chunk(objects, chunk))
                })
                .collect();
            for h in handles {
                plans.extend(h.join().expect("deltify thread panicked"));
            }
        });
    }

    // Phase 2 (serial): assign pack offsets and encode headers.
    let mut out = Vec::new();
    out.extend_from_slice(b"PACK");
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&(objects.len() as u32).to_be_bytes());
    let mut offsets: Vec<u64> = vec![0; order.len()]; // by order position
    for (pos, plan) in plans.iter().enumerate() {
        let entry_off = out.len() as u64;
        offsets[pos] = entry_off;
        let oi = order[pos];
        match plan.base_pos {
            Some(bpos) => {
                debug_assert!(plan.delta);
                write_entry_header(&mut out, OBJ_OFS_DELTA, plan.raw_size);
                out.extend_from_slice(&encode_ofs_delta(entry_off - offsets[bpos]));
                out.extend_from_slice(&plan.body);
            }
            None => {
                write_entry_header(&mut out, obj_type_id(objects[oi].ty), plan.raw_size);
                out.extend_from_slice(&plan.body);
            }
        }
    }
    let mut h = Sha1::new();
    h.update(&out);
    out.extend_from_slice(&h.finalize());
    let oids = order.iter().map(|&i| objects[i].oid).collect();
    (out, oids)
}

fn write_entry_header(out: &mut Vec<u8>, type_id: u8, mut size: u64) {
    let mut first = (type_id << 4) | (size & 0x0f) as u8;
    size >>= 4;
    loop {
        if size == 0 {
            out.push(first);
            break;
        }
        out.push(first | 0x80);
        first = (size & 0x7f) as u8;
        size >>= 7;
    }
}

fn obj_type_id(t: ObjType) -> u8 {
    t as u8
}

// ============================== pack + idx writing ==============================

pub struct PackObj {
    pub oid: Oid,
    pub ty: ObjType,
    pub data: Vec<u8>,
}

/// Write a pack v2 containing full (non-delta) objects. Returns (pack_bytes, entries_meta)
/// where entries_meta = (oid, offset, crc32) for idx generation.
#[allow(dead_code)]
pub fn write_pack(objects: &[PackObj]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"PACK");
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&(objects.len() as u32).to_be_bytes());
    for o in objects {
        // varint header
        let mut size = o.data.len() as u64;
        let mut first = ((o.ty as u8) << 4) | (size & 0x0f) as u8;
        size >>= 4;
        let mut hdr = Vec::new();
        loop {
            if size == 0 {
                hdr.push(first);
                break;
            }
            hdr.push(first | 0x80);
            first = (size & 0x7f) as u8;
            size >>= 7;
        }
        out.extend_from_slice(&hdr);
        out.extend_from_slice(&zlib::deflate(&o.data));
    }
    let mut h = Sha1::new();
    h.update(&out);
    let sum = h.finalize();
    out.extend_from_slice(&sum);
    out
}

/// Write idx v2 for `pack_bytes` + `objects` (oids in the same order they
/// were written to the pack — we recompute offsets by scanning).
pub fn write_idx(pack_bytes: &[u8], oids: &[Oid]) -> Result<Vec<u8>> {
    let offsets = Pack::scan_entries(pack_bytes)?;
    write_idx_offsets(pack_bytes, oids, &offsets)
}

/// Write idx v2 when entry offsets are already known (pack order).
pub fn write_idx_offsets(
    pack_bytes: &[u8],
    oids: &[Oid],
    offsets: &[u64],
) -> Result<Vec<u8>> {
    let mut entries: Vec<(Oid, u64, u32)> = Vec::with_capacity(oids.len());
    for (i, &oid) in oids.iter().enumerate() {
        let start = offsets[i] as usize;
        let end = if i + 1 < offsets.len() {
            offsets[i + 1] as usize
        } else {
            pack_bytes.len() - 20
        };
        entries.push((oid, offsets[i], crc32(&pack_bytes[start..end])));
    }
    entries.sort_by_key(|e| e.0);

    let mut fanout = [0u32; 256];
    for e in &entries {
        fanout[e.0 .0[0] as usize] += 1;
    }
    for i in 1..256 {
        fanout[i] += fanout[i - 1];
    }

    let mut out = Vec::new();
    out.extend_from_slice(b"\xfftOc");
    out.extend_from_slice(&2u32.to_be_bytes());
    for f in fanout {
        out.extend_from_slice(&f.to_be_bytes());
    }
    for e in &entries {
        out.extend_from_slice(&e.0 .0);
    }
    for e in &entries {
        out.extend_from_slice(&e.2.to_be_bytes());
    }
    let mut big: Vec<u64> = Vec::new();
    for e in &entries {
        if e.1 < 0x8000_0000 {
            out.extend_from_slice(&(e.1 as u32).to_be_bytes());
        } else {
            out.extend_from_slice(&((big.len() as u32) | 0x8000_0000).to_be_bytes());
            big.push(e.1);
        }
    }
    for o in big {
        out.extend_from_slice(&o.to_be_bytes());
    }
    // pack checksum, then idx checksum
    out.extend_from_slice(&pack_bytes[pack_bytes.len() - 20..]);
    let mut h = Sha1::new();
    h.update(&out);
    let sum = h.finalize();
    out.extend_from_slice(&sum);
    Ok(out)
}

/// Store a complete (non-thin) pack verbatim plus a generated idx into `dir`.
/// Returns the pack file base name. `oids` must be in pack order.
pub fn store_pack_bytes(
    dir: &Path,
    pack_bytes: &[u8],
    oids: &[Oid],
    offsets: &[u64],
) -> Result<String> {
    let hash = &pack_bytes[pack_bytes.len() - 20..];
    let name = format!("pack-{}", crate::util::to_hex(hash));
    let idx_bytes = write_idx_offsets(pack_bytes, oids, offsets)?;
    std::fs::create_dir_all(dir)?;
    let pack_path = dir.join(format!("{}.pack", name));
    let idx_path = dir.join(format!("{}.idx", name));
    if !pack_path.exists() {
        std::fs::write(&pack_path, pack_bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&pack_path, std::fs::Permissions::from_mode(0o444));
        }
    }
    if !idx_path.exists() {
        std::fs::write(&idx_path, &idx_bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&idx_path, std::fs::Permissions::from_mode(0o444));
        }
    }
    Ok(name)
}

/// Write a pack + idx into `dir` (e.g. .git/objects/pack). Returns the
/// generated file base name.
pub fn store_pack(dir: &Path, objects: &[PackObj]) -> Result<String> {
    let (pack_bytes, oids) = write_pack_delta(objects);
    let idx_bytes = write_idx(&pack_bytes, &oids)?;
    let hash = &pack_bytes[pack_bytes.len() - 20..];
    let name = format!("pack-{}", crate::util::to_hex(hash));
    std::fs::create_dir_all(dir)?;
    let pack_path = dir.join(format!("{}.pack", name));
    let idx_path = dir.join(format!("{}.idx", name));
    // existing files are identical (content-addressed name) and may be
    // read-only — don't rewrite them.
    if !pack_path.exists() {
        std::fs::write(&pack_path, &pack_bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &pack_path,
                std::fs::Permissions::from_mode(0o444),
            );
        }
    }
    if !idx_path.exists() {
        std::fs::write(&idx_path, idx_bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &idx_path,
                std::fs::Permissions::from_mode(0o444),
            );
        }
    }
    Ok(name)
}
