//! Object database: loose objects + pack files + alternates.

use crate::object::{ObjType, Oid};
use crate::pack::Pack;
use crate::util::{to_hex, GitError, Result};
use crate::zlib;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Where an object lives on disk — for verbatim pack-entry reuse.
pub enum ObjLoc {
    Loose,
    Packed(Rc<Pack>, u64),
}

/// Read an object from any of `stores` (loose first, then packs, with
/// cross-store REF-delta resolution). Free fn so worker threads can use
/// it without an Odb.
fn read_opt_in(
    stores: &[Store],
    oid: &Oid,
) -> Result<Option<Rc<(ObjType, Vec<u8>)>>> {
    for store in stores {
        if let Some(o) = store.read_loose(oid)? {
            return Ok(Some(o));
        }
    }
    for store in stores {
        for pack in store.packs() {
            let resolve = |o: &Oid| read_opt_in(stores, o).ok().flatten();
            if let Some(obj) = pack.read_object(oid, &resolve)? {
                return Ok(Some(obj));
            }
        }
    }
    Ok(None)
}

pub struct Store {
    pub dir: PathBuf,
    packs: RefCell<Option<Vec<Rc<Pack>>>>,
}

impl Store {
    fn new(dir: PathBuf) -> Store {
        Store { dir, packs: RefCell::new(None) }
    }

    fn packs(&self) -> Vec<Rc<Pack>> {
        if self.packs.borrow().is_none() {
            let mut v = Vec::new();
            let pack_dir = self.dir.join("pack");
            if let Ok(rd) = std::fs::read_dir(&pack_dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().map(|e| e == "idx").unwrap_or(false) {
                        if let Ok(pack) = Pack::open(&p) {
                            v.push(pack);
                        }
                    }
                }
            }
            *self.packs.borrow_mut() = Some(v);
        }
        self.packs.borrow().as_ref().unwrap().clone()
    }

    fn loose_path(&self, oid: &Oid) -> PathBuf {
        let h = oid.hex();
        self.dir.join(&h[0..2]).join(&h[2..])
    }

    fn read_loose(&self, oid: &Oid) -> Result<Option<Rc<(ObjType, Vec<u8>)>>> {
        let p = self.loose_path(oid);
        let raw = match std::fs::read(&p) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let (inflated, _) = zlib::inflate(&raw, raw.len() * 3)?;
        let nul = inflated
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| GitError::ObjectCorrupt(format!("loose object {:?} bad header", p)))?;
        let header = std::str::from_utf8(&inflated[..nul])
            .map_err(|_| GitError::ObjectCorrupt("loose: bad header utf8".into()))?;
        let mut parts = header.splitn(2, ' ');
        let ty = ObjType::from_name(parts.next().unwrap_or(""))?;
        let size: usize = parts
            .next()
            .unwrap_or("0")
            .trim()
            .parse()
            .map_err(|_| GitError::ObjectCorrupt("loose: bad size".into()))?;
        let body = inflated[nul + 1..].to_vec();
        if body.len() != size {
            return Err(GitError::ObjectCorrupt(format!(
                "loose {}: size mismatch {} != {}",
                oid,
                body.len(),
                size
            )));
        }
        Ok(Some(Rc::new((ty, body))))
    }

    /// Loose oids with the given hex-prefix byte, used for prefix resolution.
    fn loose_oids(&self, prefix: &str) -> Vec<Oid> {
        let mut out = Vec::new();
        if prefix.len() < 2 {
            // scan all fanout dirs
            for i in 0..256 {
                let d = self.dir.join(format!("{:02x}", i));
                if let Ok(rd) = std::fs::read_dir(&d) {
                    for e in rd.flatten() {
                        if let Some(name) = e.file_name().to_str() {
                            let full = format!("{:02x}{}", i, name);
                            if full.starts_with(prefix) {
                                if let Ok(oid) = Oid::from_hex(&full) {
                                    out.push(oid);
                                }
                            }
                        }
                    }
                }
            }
            return out;
        }
        let d = self.dir.join(&prefix[0..2]);
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                if let Some(name) = e.file_name().to_str() {
                    let full = format!("{}{}", &prefix[0..2], name);
                    if full.starts_with(prefix) {
                        if let Ok(oid) = Oid::from_hex(&full) {
                            out.push(oid);
                        }
                    }
                }
            }
        }
        out
    }
}

pub struct Odb {
    stores: Vec<Store>,
}

impl Odb {
    pub fn new(objects_dir: PathBuf) -> Odb {
        let mut dirs = vec![objects_dir.clone()];
        // read objects/info/alternates (may be chained)
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut queue = vec![objects_dir];
        while let Some(d) = queue.pop() {
            if !seen.insert(d.clone()) {
                continue;
            }
            let alt_file = d.join("info").join("alternates");
            if let Ok(text) = std::fs::read_to_string(&alt_file) {
                for line in text.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    let p = if Path::new(line).is_absolute() {
                        PathBuf::from(line)
                    } else {
                        d.join(line)
                    };
                    let canon = p.canonicalize().unwrap_or(p);
                    if seen.insert(canon.clone()) {
                        dirs.push(canon.clone());
                        queue.push(canon);
                    }
                }
            }
        }
        Odb {
            stores: dirs.into_iter().map(Store::new).collect(),
        }
    }

    pub fn primary_dir(&self) -> &Path {
        &self.stores[0].dir
    }

    /// Read an object. Returns (type, content).
    pub fn read(&self, oid: &Oid) -> Result<Rc<(ObjType, Vec<u8>)>> {
        self.read_opt(oid)?.ok_or_else(|| {
            GitError::NotFound(format!("object {} not found", oid))
        })
    }

    pub fn read_opt(&self, oid: &Oid) -> Result<Option<Rc<(ObjType, Vec<u8>)>>> {
        read_opt_in(&self.stores, oid)
    }

    /// Read only the header: (type, size). Still decompresses, but avoids
    /// re-hashing — for loose objects we can stop early in theory; kept
    /// simple.
    #[allow(dead_code)]
    pub fn read_header(&self, oid: &Oid) -> Result<(ObjType, usize)> {
        let obj = self.read(oid)?;
        Ok((obj.0, obj.1.len()))
    }

    /// Where does an object live? Loose file or (pack, entry offset) —
    /// without inflating anything. Used by pack-reuse fast paths.
    pub fn locate(&self, oid: &Oid) -> Option<ObjLoc> {
        for store in &self.stores {
            if store.loose_path(oid).exists() {
                return Some(ObjLoc::Loose);
            }
        }
        for store in &self.stores {
            for pack in store.packs() {
                if let Some(off) = pack.find_offset(oid) {
                    return Some(ObjLoc::Packed(pack, off));
                }
            }
        }
        None
    }

    /// Split wanted oids into verbatim-reusable pack entries and fresh
    /// objects needing deltification (git's "reuse deltas" fast path).
    pub fn pack_inputs(
        &self,
        oids: &[Oid],
    ) -> Result<(Vec<crate::pack::ReuseEntry>, Vec<crate::pack::PackObj>)> {
        let wanted: HashSet<Oid> = oids.iter().copied().collect();
        // group located entries per source pack (keyed by Rc identity)
        let mut by_pack: Vec<(Rc<Pack>, Vec<(Oid, u64)>)> = Vec::new();
        let mut pack_pos: HashMap<usize, usize> = HashMap::new();
        let mut loose: Vec<Oid> = Vec::new();
        for oid in oids {
            match self.locate(oid) {
                Some(ObjLoc::Packed(pack, off)) => {
                    let k = Rc::as_ptr(&pack) as usize;
                    match pack_pos.get(&k) {
                        Some(&i) => by_pack[i].1.push((*oid, off)),
                        None => {
                            pack_pos.insert(k, by_pack.len());
                            by_pack.push((pack, vec![(*oid, off)]));
                        }
                    }
                }
                Some(ObjLoc::Loose) | None => loose.push(*oid),
            }
        }
        let mut reused = Vec::new();
        let mut fallback = Vec::new();
        for (pack, entries) in by_pack {
            let (good, rest) = crate::pack::reusable_entries(&pack, &entries, &wanted);
            reused.extend(good);
            fallback.extend(rest);
        }
        // everything not reused needs its data (parallel over chunks)
        let fresh_oids: Vec<Oid> = loose.into_iter().chain(fallback).collect();
        let fresh = self.read_many(&fresh_oids)?;
        Ok((reused, fresh))
    }

    /// Read many objects, parallelized across threads by chunking the oid
    /// list (each worker opens its own Pack handles — Rc isn't Send).
    pub fn read_many(&self, oids: &[Oid]) -> Result<Vec<crate::pack::PackObj>> {
        use crate::pack::PackObj;
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(8)
            .min(oids.len().max(1));
        if n <= 1 {
            let mut v = Vec::with_capacity(oids.len());
            for oid in oids {
                let o = self.read(oid)?;
                v.push(PackObj { oid: *oid, ty: o.0, data: o.1.clone() });
            }
            return Ok(v);
        }
        let chunk = (oids.len() + n - 1) / n;
        let mut out: Vec<PackObj> = Vec::new();
        std::thread::scope(|s| {
            let handles: Vec<_> = oids
                .chunks(chunk)
                .map(|slice| {
                    let dirs: Vec<PathBuf> =
                        self.stores.iter().map(|s| s.dir.clone()).collect();
                    s.spawn(move || {
                        // fresh Stores per thread — independent Pack handles
                        let stores: Vec<Store> =
                            dirs.into_iter().map(Store::new).collect();
                        let mut v = Vec::with_capacity(slice.len());
                        for oid in slice {
                            let o = read_opt_in(&stores, oid)?.ok_or_else(|| {
                                GitError::NotFound(format!("object {} not found", oid))
                            })?;
                            v.push(PackObj { oid: *oid, ty: o.0, data: o.1.clone() });
                        }
                        Ok::<_, GitError>(v)
                    })
                })
                .collect();
            for h in handles {
                out.extend(h.join().expect("read_many thread panicked")?);
            }
            Ok::<_, GitError>(())
        })?;
        Ok(out)
    }

    pub fn has(&self, oid: &Oid) -> bool {
        self.read_opt(oid).map(|o| o.is_some()).unwrap_or(false)
    }

    /// Write object as loose if not already present (anywhere).
    pub fn write(&self, ty: ObjType, data: &[u8]) -> Result<Oid> {
        let oid = crate::object::hash_object(ty, data);
        self.write_with_oid(&oid, ty, data)?;
        Ok(oid)
    }

    pub fn write_with_oid(&self, oid: &Oid, ty: ObjType, data: &[u8]) -> Result<()> {
        if self.has(oid) {
            return Ok(());
        }
        let store = &self.stores[0];
        let path = store.loose_path(oid);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut payload = format!("{} {}\0", ty.name(), data.len()).into_bytes();
        payload.extend_from_slice(data);
        let compressed = zlib::deflate_loose(&payload);
        crate::util::write_file_atomic(&path, &compressed)?;
        // loose objects are read-only
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444));
        }
        Ok(())
    }

    /// All oids matching a hex prefix (loose + packed).
    pub fn prefix_to_oids(&self, prefix: &str) -> Vec<Oid> {
        let mut set: HashSet<Oid> = HashSet::new();
        for store in &self.stores {
            for oid in store.loose_oids(prefix) {
                set.insert(oid);
            }
            for pack in store.packs() {
                for oid in pack.oids() {
                    if oid.hex().starts_with(prefix) {
                        set.insert(*oid);
                    }
                }
            }
        }
        let mut v: Vec<Oid> = set.into_iter().collect();
        v.sort();
        v
    }

    /// Iterate every object id in the database (loose + packed).
    pub fn all_oids(&self) -> Vec<Oid> {
        let mut set: HashSet<Oid> = HashSet::new();
        for store in &self.stores {
            for oid in store.loose_oids("") {
                set.insert(oid);
            }
            for pack in store.packs() {
                for oid in pack.oids() {
                    set.insert(*oid);
                }
            }
        }
        let mut v: Vec<Oid> = set.into_iter().collect();
        v.sort();
        v
    }

    /// Store a received/created pack (with idx) into the primary store.
    pub fn store_pack(&self, objects: &[crate::pack::PackObj]) -> Result<String> {
        let dir = self.primary_dir().join("pack");
        let name = crate::pack::store_pack(&dir, objects)?;
        self.reset_pack_cache();
        Ok(name)
    }

    /// Drop cached pack handles so newly written packs become visible.
    pub fn reset_pack_cache(&self) {
        for store in &self.stores {
            *store.packs.borrow_mut() = None;
        }
    }

    /// Pack-creation fast path: reuse packed entries verbatim, deltify
    /// only loose/fallback objects, store with idx, reset cache.
    pub fn store_pack_inputs(
        &self,
        oids: &[Oid],
    ) -> Result<Option<String>> {
        let (reused, fresh) = self.pack_inputs(oids)?;
        if reused.is_empty() && fresh.is_empty() {
            return Ok(None);
        }
        let dir = self.primary_dir().join("pack");
        let name = crate::pack::store_pack_mixed(&dir, &fresh, reused)?;
        self.reset_pack_cache();
        Ok(Some(name))
    }

    /// Loose-object hex for an oid string like "ab12..." -> path
    #[allow(dead_code)]
    pub fn loose_object_exists(&self, oid: &Oid) -> bool {
        self.stores
            .iter()
            .any(|s| s.loose_path(oid).exists())
    }
}

#[allow(dead_code)]
pub fn to_hex_short(oid: &Oid, len: usize) -> String {
    let h = to_hex(&oid.0);
    h[..len.min(40)].to_string()
}
