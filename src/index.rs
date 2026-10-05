//! Git index (staging area): read v2/v3/v4, write v2/v3.

use crate::object::Oid;
use crate::sha1::Sha1;
use crate::util::{be_u16, be_u32, GitError, Result};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct IndexEntry {
    pub ctime_s: u32,
    pub ctime_n: u32,
    pub mtime_s: u32,
    pub mtime_n: u32,
    pub dev: u32,
    pub ino: u32,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u32,
    pub oid: Oid,
    pub assume_valid: bool,
    pub stage: u8,
    pub skip_worktree: bool,
    pub intent_to_add: bool,
    pub path: String,
}

impl IndexEntry {
    pub fn flags(&self) -> u16 {
        let mut f: u16 = 0;
        if self.assume_valid {
            f |= 0x8000;
        }
        if self.skip_worktree || self.intent_to_add {
            f |= 0x4000; // extended -> forces v3
        }
        f |= ((self.stage as u16) & 0x3) << 12;
        let namelen = self.path.len().min(0xFFF);
        f | namelen as u16
    }
}

#[derive(Default, Clone)]
pub struct Index {
    pub entries: Vec<IndexEntry>,
    /// raw bytes of extensions we don't understand? we drop them (safe).
    #[allow(dead_code)]
    pub version: u32,
}

impl Index {
    pub fn load(path: &Path) -> Result<Index> {
        match std::fs::read(path) {
            Ok(raw) => Index::parse(&raw),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Index::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn parse(raw: &[u8]) -> Result<Index> {
        if raw.len() < 12 + 20 || &raw[0..4] != b"DIRC" {
            return Err(GitError::Parse("index: bad signature".into()));
        }
        // verify checksum
        let (body, sum) = raw.split_at(raw.len() - 20);
        let mut h = Sha1::new();
        h.update(body);
        if h.finalize() != sum {
            return Err(GitError::Parse("index: bad checksum".into()));
        }
        let version = be_u32(&raw[4..8]);
        if !(2..=4).contains(&version) {
            return Err(GitError::Parse(format!("index: version {} unsupported", version)));
        }
        let count = be_u32(&raw[8..12]) as usize;
        let mut pos = 12usize;
        let mut entries = Vec::with_capacity(count);
        let mut prev_path = String::new();
        for _ in 0..count {
            if pos + 62 > raw.len() {
                return Err(GitError::Parse("index: truncated entry".into()));
            }
            let e = &raw[pos..];
            let ctime_s = be_u32(&e[0..4]);
            let ctime_n = be_u32(&e[4..8]);
            let mtime_s = be_u32(&e[8..12]);
            let mtime_n = be_u32(&e[12..16]);
            let dev = be_u32(&e[16..20]);
            let ino = be_u32(&e[20..24]);
            let mode = be_u32(&e[24..28]);
            let uid = be_u32(&e[28..32]);
            let gid = be_u32(&e[32..36]);
            let size = be_u32(&e[36..40]);
            let oid = Oid::from_bytes(&e[40..60])?;
            let flags = be_u16(&e[60..62]);
            let assume_valid = flags & 0x8000 != 0;
            let extended = flags & 0x4000 != 0;
            let stage = ((flags >> 12) & 0x3) as u8;
            let namelen = (flags & 0xFFF) as usize;
            let mut p = pos + 62;
            let (mut skip_worktree, mut intent_to_add) = (false, false);
            if extended {
                if version < 3 {
                    return Err(GitError::Parse("index: extended flags in v2".into()));
                }
                let x = be_u16(&raw[p..p + 2]);
                skip_worktree = x & 0x4000 != 0;
                intent_to_add = x & 0x2000 != 0;
                p += 2;
            }
            let path: String;
            if version == 4 {
                // prefix compression: varint strip + NUL-terminated suffix
                let mut strip: usize = 0;
                loop {
                    let b = raw[p];
                    p += 1;
                    strip = (strip << 7) | (b & 0x7f) as usize;
                    if b & 0x80 == 0 {
                        break;
                    }
                    strip += 1;
                }
                let nul = raw[p..]
                    .iter()
                    .position(|&b| b == 0)
                    .ok_or_else(|| GitError::Parse("index v4: no NUL".into()))?;
                let keep = prev_path.len() - strip;
                let mut s = String::with_capacity(keep + nul);
                s.push_str(&prev_path[..keep]);
                s.push_str(std::str::from_utf8(&raw[p..p + nul]).map_err(|_| {
                    GitError::Parse("index v4: bad utf8 path".into())
                })?);
                p += nul + 1;
                path = s;
            } else {
                let name_start = p;
                let name_len = if namelen == 0xFFF {
                    raw[name_start..]
                        .iter()
                        .position(|&b| b == 0)
                        .ok_or_else(|| GitError::Parse("index: unterminated name".into()))?
                } else {
                    namelen
                };
                path = String::from_utf8_lossy(&raw[name_start..name_start + name_len])
                    .to_string();
                // entry padded to multiple of 8, with 1-8 NULs
                let fixed = 62 + if extended { 2 } else { 0 };
                let entry_len = (fixed + name_len + 8) / 8 * 8;
                p = pos + entry_len;
            }
            entries.push(IndexEntry {
                ctime_s, ctime_n, mtime_s, mtime_n, dev, ino, mode, uid, gid, size,
                oid, assume_valid, stage, skip_worktree, intent_to_add,
                path: path.clone(),
            });
            prev_path = path;
            pos = p;
        }
        Ok(Index { entries, version })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let needs_v3 = self
            .entries
            .iter()
            .any(|e| e.skip_worktree || e.intent_to_add);
        let version = if needs_v3 { 3u32 } else { 2u32 };
        let mut out = Vec::new();
        out.extend_from_slice(b"DIRC");
        out.extend_from_slice(&version.to_be_bytes());
        out.extend_from_slice(&(self.entries.len() as u32).to_be_bytes());
        for e in &self.entries {
            out.extend_from_slice(&e.ctime_s.to_be_bytes());
            out.extend_from_slice(&e.ctime_n.to_be_bytes());
            out.extend_from_slice(&e.mtime_s.to_be_bytes());
            out.extend_from_slice(&e.mtime_n.to_be_bytes());
            out.extend_from_slice(&e.dev.to_be_bytes());
            out.extend_from_slice(&e.ino.to_be_bytes());
            out.extend_from_slice(&e.mode.to_be_bytes());
            out.extend_from_slice(&e.uid.to_be_bytes());
            out.extend_from_slice(&e.gid.to_be_bytes());
            out.extend_from_slice(&e.size.to_be_bytes());
            out.extend_from_slice(&e.oid.0);
            let extended = e.skip_worktree || e.intent_to_add;
            out.extend_from_slice(&e.flags().to_be_bytes());
            if extended {
                let mut x: u16 = 0;
                if e.skip_worktree {
                    x |= 0x4000;
                }
                if e.intent_to_add {
                    x |= 0x2000;
                }
                out.extend_from_slice(&x.to_be_bytes());
            }
            out.extend_from_slice(e.path.as_bytes());
            let fixed = if extended { 64usize } else { 62usize };
            let entry_len = (fixed + e.path.len() + 8) / 8 * 8;
            for _ in 0..entry_len - fixed - e.path.len() {
                out.push(0);
            }
        }
        let mut h = Sha1::new();
        h.update(&out);
        let sum = h.finalize();
        out.extend_from_slice(&sum);
        out
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        Ok(crate::util::write_file_atomic(path, &self.serialize())?)
    }

    pub fn find(&self, path: &str, stage: u8) -> Option<&IndexEntry> {
        self.entries.iter().find(|e| e.path == path && e.stage == stage)
    }

    pub fn find_any(&self, path: &str) -> Option<&IndexEntry> {
        self.entries.iter().find(|e| e.path == path)
    }

    /// Insert or replace the stage-0 entry for `path`; removes conflict stages.
    pub fn upsert(&mut self, entry: IndexEntry) {
        self.entries
            .retain(|e| !(e.path == entry.path));
        self.insert_sorted(entry);
    }

    pub fn insert_sorted(&mut self, entry: IndexEntry) {
        let key = (entry.path.clone(), entry.stage);
        let pos = self
            .entries
            .binary_search_by(|e| (e.path.clone(), e.stage).cmp(&key))
            .unwrap_or_else(|i| i);
        self.entries.insert(pos, entry);
    }

    pub fn remove_path(&mut self, path: &str) -> bool {
        let n = self.entries.len();
        self.entries.retain(|e| e.path != path);
        self.entries.len() != n
    }

    /// Remove all entries under `dir/` (for rm -r / checkout).
    #[allow(dead_code)]
    pub fn remove_dir(&mut self, dir: &str) {
        let prefix = format!("{}/", dir.trim_end_matches('/'));
        self.entries.retain(|e| !e.path.starts_with(&prefix) && e.path != dir);
    }

    pub fn sort(&mut self) {
        self.entries.sort_by(|a, b| (&a.path, a.stage).cmp(&(&b.path, b.stage)));
    }

    pub fn has_conflicts(&self) -> bool {
        self.entries.iter().any(|e| e.stage != 0)
    }

    /// All entries under a directory prefix.
    pub fn paths_under(&self, dir: &str) -> Vec<&IndexEntry> {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{}/", dir.trim_end_matches('/'))
        };
        self.entries
            .iter()
            .filter(|e| e.path.starts_with(&prefix))
            .collect()
    }
}

/// Build an index entry from filesystem metadata.
#[cfg(unix)]
pub fn entry_from_stat(meta: &std::fs::Metadata, oid: Oid, path: &str) -> IndexEntry {
    use std::os::unix::fs::MetadataExt;
    let mode = if meta.file_type().is_symlink() {
        0o120000
    } else if crate::util::is_executable(meta) {
        0o100755
    } else {
        0o100644
    };
    IndexEntry {
        ctime_s: meta.ctime() as u32,
        ctime_n: meta.ctime_nsec() as u32,
        mtime_s: meta.mtime() as u32,
        mtime_n: meta.mtime_nsec() as u32,
        dev: meta.dev() as u32,
        ino: meta.ino() as u32,
        mode,
        uid: meta.uid(),
        gid: meta.gid(),
        size: meta.size() as u32,
        oid,
        assume_valid: false,
        stage: 0,
        skip_worktree: false,
        intent_to_add: false,
        path: path.to_string(),
    }
}

/// Check whether a file's stat matches the index entry (fast "unchanged" test).
#[cfg(unix)]
pub fn stat_matches(e: &IndexEntry, meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    e.mtime_s == meta.mtime() as u32
        && e.mtime_n == meta.mtime_nsec() as u32
        && e.ctime_s == meta.ctime() as u32
        && e.ctime_n == meta.ctime_nsec() as u32
        && e.ino == meta.ino() as u32
        && e.size == meta.size() as u32
        && e.uid == meta.uid()
        && e.gid == meta.gid()
}
