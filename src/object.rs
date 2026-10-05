use crate::sha1::Sha1;
use crate::util::{from_hex, to_hex, GitError, Result};
use std::fmt;

// ============================== Oid ==============================

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Oid(pub [u8; 20]);

impl Oid {
    pub const ZERO: Oid = Oid([0u8; 20]);

    pub fn from_hex(s: &str) -> Result<Oid> {
        let b = from_hex(s)?;
        if b.len() != 20 {
            return Err(GitError::Parse(format!("bad object id: {}", s)));
        }
        let mut a = [0u8; 20];
        a.copy_from_slice(&b);
        Ok(Oid(a))
    }

    pub fn from_bytes(b: &[u8]) -> Result<Oid> {
        if b.len() != 20 {
            return Err(GitError::Parse("bad object id length".into()));
        }
        let mut a = [0u8; 20];
        a.copy_from_slice(b);
        Ok(Oid(a))
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0u8; 20]
    }

    pub fn hex(&self) -> String {
        to_hex(&self.0)
    }

    pub fn short(&self, n: usize) -> String {
        self.hex()[..n.min(40)].to_string()
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.hex())
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({})", self.hex())
    }
}

// ============================== Object types ==============================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjType {
    Commit = 1,
    Tree = 2,
    Blob = 3,
    Tag = 4,
}

impl ObjType {
    pub fn name(&self) -> &'static str {
        match self {
            ObjType::Commit => "commit",
            ObjType::Tree => "tree",
            ObjType::Blob => "blob",
            ObjType::Tag => "tag",
        }
    }

    pub fn from_name(s: &str) -> Result<ObjType> {
        match s {
            "commit" => Ok(ObjType::Commit),
            "tree" => Ok(ObjType::Tree),
            "blob" => Ok(ObjType::Blob),
            "tag" => Ok(ObjType::Tag),
            _ => Err(GitError::Parse(format!("bad object type: {}", s))),
        }
    }

    pub fn from_pack_id(id: u8) -> Result<ObjType> {
        match id {
            1 => Ok(ObjType::Commit),
            2 => Ok(ObjType::Tree),
            3 => Ok(ObjType::Blob),
            4 => Ok(ObjType::Tag),
            _ => Err(GitError::Parse(format!("bad pack object type: {}", id))),
        }
    }
}

/// hash_object: sha1("{type} {len}\\0" + payload)
pub fn hash_object(ty: ObjType, data: &[u8]) -> Oid {
    let mut h = Sha1::new();
    h.update(ty.name().as_bytes());
    h.update(b" ");
    h.update(data.len().to_string().as_bytes());
    h.update(b"\0");
    h.update(data);
    Oid(h.finalize())
}

// ============================== Commit ==============================

#[derive(Clone, Debug)]
pub struct Ident {
    pub name: String,
    pub email: String,
    pub time: i64,
    pub tz: String, // e.g. "+0200"
}

impl Ident {
    /// Serialize: "Name <email> time tz"
    pub fn to_string(&self) -> String {
        format!("{} <{}> {} {}", self.name, self.email, self.time, self.tz)
    }

    pub fn parse(s: &str) -> Result<Ident> {
        // "Name <email> ts tz" — name may contain '<'/'>'? In practice no.
        let lt = s
            .rfind('<')
            .ok_or_else(|| GitError::Parse(format!("bad ident: {}", s)))?;
        let gt = s[lt..]
            .find('>')
            .map(|i| lt + i)
            .ok_or_else(|| GitError::Parse(format!("bad ident: {}", s)))?;
        let name = s[..lt].trim_end().to_string();
        let email = s[lt + 1..gt].to_string();
        let rest = s[gt + 1..].trim();
        let mut it = rest.split_whitespace();
        let time: i64 = it
            .next()
            .unwrap_or("0")
            .parse()
            .map_err(|_| GitError::Parse(format!("bad ident time: {}", s)))?;
        let tz = it.next().unwrap_or("+0000").to_string();
        Ok(Ident { name, email, time, tz })
    }

    /// "Name <email>" portion (for reflog)
    pub fn who(&self) -> String {
        format!("{} <{}>", self.name, self.email)
    }
}

#[derive(Clone, Debug)]
pub struct Commit {
    pub tree: Oid,
    pub parents: Vec<Oid>,
    pub author: Ident,
    pub committer: Ident,
    /// raw header lines preserved verbatim (gpgsig, mergetag, encoding, ...)
    pub extra_headers: Vec<(String, String)>,
    pub message: String,
}

impl Commit {
    pub fn parse(data: &[u8]) -> Result<Commit> {
        let text = String::from_utf8_lossy(data);
        let mut tree = None;
        let mut parents = Vec::new();
        let mut author = None;
        let mut committer = None;
        let mut extra: Vec<(String, String)> = Vec::new();
        let mut lines = text.split('\n');
        // headers until blank line; continuation lines start with space
        let mut last_key: Option<usize> = None;
        for line in &mut lines {
            if line.is_empty() {
                break;
            }
            if line.starts_with(' ') {
                if let Some(i) = last_key {
                    if let Some((_, v)) = extra.get_mut(i) {
                        v.push('\n');
                        v.push_str(&line[1..]);
                        continue;
                    }
                }
                continue;
            }
            let (key, value) = match line.split_once(' ') {
                Some((k, v)) => (k, v),
                None => (line, ""),
            };
            match key {
                "tree" => tree = Some(Oid::from_hex(value)?),
                "parent" => parents.push(Oid::from_hex(value)?),
                "author" => author = Some(Ident::parse(value)?),
                "committer" => committer = Some(Ident::parse(value)?),
                _ => {
                    extra.push((key.to_string(), value.to_string()));
                    last_key = Some(extra.len() - 1);
                }
            }
        }
        let message: String = lines.collect::<Vec<_>>().join("\n");
        Ok(Commit {
            tree: tree.ok_or_else(|| GitError::Parse("commit: missing tree".into()))?,
            parents,
            author: author.ok_or_else(|| GitError::Parse("commit: missing author".into()))?,
            committer: committer
                .ok_or_else(|| GitError::Parse("commit: missing committer".into()))?,
            extra_headers: extra,
            message,
        })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut s = String::new();
        s.push_str(&format!("tree {}\n", self.tree));
        for p in &self.parents {
            s.push_str(&format!("parent {}\n", p));
        }
        s.push_str(&format!("author {}\n", self.author.to_string()));
        s.push_str(&format!("committer {}\n", self.committer.to_string()));
        for (k, v) in &self.extra_headers {
            s.push_str(k);
            s.push(' ');
            // multi-line values use continuation lines
            let mut parts = v.split('\n');
            if let Some(first) = parts.next() {
                s.push_str(first);
            }
            for cont in parts {
                s.push('\n');
                s.push(' ');
                s.push_str(cont);
            }
            s.push('\n');
        }
        s.push('\n');
        s.push_str(&self.message);
        s.into_bytes()
    }

    pub fn committer_time(&self) -> i64 {
        self.committer.time
    }

    pub fn summary(&self) -> String {
        self.message.lines().next().unwrap_or("").to_string()
    }
}

// ============================== Tag ==============================

#[derive(Clone, Debug)]
pub struct Tag {
    pub object: Oid,
    pub target_type: ObjType,
    pub tag: String,
    pub tagger: Option<Ident>,
    pub message: String,
}

impl Tag {
    pub fn parse(data: &[u8]) -> Result<Tag> {
        let text = String::from_utf8_lossy(data);
        let mut object = None;
        let mut ttype = None;
        let mut tagname = None;
        let mut tagger = None;
        let mut lines = text.split('\n');
        for line in &mut lines {
            if line.is_empty() {
                break;
            }
            let (key, value) = match line.split_once(' ') {
                Some((k, v)) => (k, v),
                None => (line, ""),
            };
            match key {
                "object" => object = Some(Oid::from_hex(value)?),
                "type" => ttype = Some(ObjType::from_name(value)?),
                "tag" => tagname = Some(value.to_string()),
                "tagger" => tagger = Some(Ident::parse(value)?),
                _ => {}
            }
        }
        let message: String = lines.collect::<Vec<_>>().join("\n");
        Ok(Tag {
            object: object.ok_or_else(|| GitError::Parse("tag: missing object".into()))?,
            target_type: ttype.ok_or_else(|| GitError::Parse("tag: missing type".into()))?,
            tag: tagname.ok_or_else(|| GitError::Parse("tag: missing tag".into()))?,
            tagger,
            message,
        })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut s = String::new();
        s.push_str(&format!("object {}\n", self.object));
        s.push_str(&format!("type {}\n", self.target_type.name()));
        s.push_str(&format!("tag {}\n", self.tag));
        if let Some(t) = &self.tagger {
            s.push_str(&format!("tagger {}\n", t.to_string()));
        }
        s.push('\n');
        s.push_str(&self.message);
        s.into_bytes()
    }
}

// ============================== Tree ==============================

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    pub mode: u32, // e.g. 0o100644
    pub name: String,
    pub oid: Oid,
}

impl TreeEntry {
    pub fn is_tree(&self) -> bool {
        self.mode & 0o170000 == 0o040000
    }
    pub fn is_gitlink(&self) -> bool {
        self.mode == 0o160000
    }
    pub fn is_symlink(&self) -> bool {
        self.mode & 0o170000 == 0o120000
    }
    pub fn mode_str(&self) -> String {
        format!("{:o}", self.mode)
    }
    pub fn type_name(&self) -> &'static str {
        if self.is_tree() {
            "tree"
        } else if self.is_gitlink() {
            "commit"
        } else {
            "blob"
        }
    }
}

/// Parse tree object payload.
pub fn parse_tree(data: &[u8]) -> Result<Vec<TreeEntry>> {
    let mut entries = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let sp = data[i..]
            .iter()
            .position(|&b| b == b' ')
            .ok_or_else(|| GitError::Parse("tree: missing space".into()))?
            + i;
        let mode_str = std::str::from_utf8(&data[i..sp])
            .map_err(|_| GitError::Parse("tree: bad mode".into()))?;
        let mode = u32::from_str_radix(mode_str, 8)
            .map_err(|_| GitError::Parse("tree: bad mode".into()))?;
        let nul = data[sp + 1..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| GitError::Parse("tree: missing NUL".into()))?
            + sp
            + 1;
        let name = String::from_utf8_lossy(&data[sp + 1..nul]).to_string();
        if nul + 21 > data.len() {
            return Err(GitError::Parse("tree: truncated oid".into()));
        }
        let oid = Oid::from_bytes(&data[nul + 1..nul + 21])?;
        entries.push(TreeEntry { mode, name, oid });
        i = nul + 21;
    }
    Ok(entries)
}

/// Serialize tree entries (must already be sorted by tree-order).
pub fn serialize_tree(entries: &[TreeEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        out.extend_from_slice(format!("{:o} ", e.mode).as_bytes());
        out.extend_from_slice(e.name.as_bytes());
        out.push(0);
        out.extend_from_slice(&e.oid.0);
    }
    out
}

/// Git's tree ordering: names compared bytewise, but directories compare as
/// if their name had a trailing '/'.
pub fn tree_entry_cmp(a: &TreeEntry, b: &TreeEntry) -> std::cmp::Ordering {
    let an = a.name.as_bytes();
    let bn = b.name.as_bytes();
    let n = an.len().min(bn.len());
    for i in 0..n {
        if an[i] != bn[i] {
            return an[i].cmp(&bn[i]);
        }
    }
    let ac = if an.len() > n {
        an[n]
    } else if a.is_tree() {
        b'/'
    } else {
        0
    };
    let bc = if bn.len() > n {
        bn[n]
    } else if b.is_tree() {
        b'/'
    } else {
        0
    };
    ac.cmp(&bc)
}
