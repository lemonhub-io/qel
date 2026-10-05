//! Object database: loose objects + pack files + alternates.

use crate::object::{ObjType, Oid};
use crate::pack::Pack;
use crate::util::{to_hex, GitError, Result};
use crate::zlib;
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

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
        for store in &self.stores {
            if let Some(o) = store.read_loose(oid)? {
                return Ok(Some(o));
            }
        }
        for store in &self.stores {
            for pack in store.packs() {
                let resolve = |o: &Oid| self.read_opt(o).ok().flatten();
                if let Some(obj) = pack.read_object(oid, &resolve)? {
                    return Ok(Some(obj));
                }
            }
        }
        Ok(None)
    }

    /// Read only the header: (type, size). Still decompresses, but avoids
    /// re-hashing — for loose objects we can stop early in theory; kept
    /// simple.
    pub fn read_header(&self, oid: &Oid) -> Result<(ObjType, usize)> {
        let obj = self.read(oid)?;
        Ok((obj.0, obj.1.len()))
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
        let compressed = zlib::deflate(&payload);
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
        // reset pack cache so new pack is visible
        for store in &self.stores {
            *store.packs.borrow_mut() = None;
        }
        Ok(name)
    }

    /// Loose-object hex for an oid string like "ab12..." -> path
    pub fn loose_object_exists(&self, oid: &Oid) -> bool {
        self.stores
            .iter()
            .any(|s| s.loose_path(oid).exists())
    }
}

pub fn to_hex_short(oid: &Oid, len: usize) -> String {
    let h = to_hex(&oid.0);
    h[..len.min(40)].to_string()
}
