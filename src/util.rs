use std::fmt;
use std::io;

pub type Result<T> = std::result::Result<T, GitError>;

#[derive(Debug)]
pub enum GitError {
    Io(io::Error),
    Parse(String),
    ObjectCorrupt(String),
    NotFound(String),
    Protocol(String),
    InvalidInput(String),
    NotARepo(String),
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitError::Io(e) => write!(f, "{}", e),
            GitError::Parse(m) => write!(f, "parse error: {}", m),
            GitError::ObjectCorrupt(m) => write!(f, "corrupt object: {}", m),
            GitError::NotFound(m) => write!(f, "{}", m),
            GitError::Protocol(m) => write!(f, "protocol error: {}", m),
            GitError::InvalidInput(m) => write!(f, "{}", m),
            GitError::NotARepo(m) => write!(f, "{}", m),
        }
    }
}

impl std::error::Error for GitError {}

impl From<io::Error> for GitError {
    fn from(e: io::Error) -> Self {
        GitError::Io(e)
    }
}

pub fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(GitError::Parse(msg.into()))
}

#[allow(dead_code)]
pub fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(GitError::InvalidInput(msg.into()))
}

// ---------- hex ----------

pub fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    s
}

pub fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

pub fn from_hex(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return err(format!("odd-length hex string: {}", s));
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len() / 2);
    for i in (0..b.len()).step_by(2) {
        let hi = hex_val(b[i]).ok_or_else(|| GitError::Parse(format!("bad hex: {}", s)))?;
        let lo = hex_val(b[i + 1]).ok_or_else(|| GitError::Parse(format!("bad hex: {}", s)))?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

// ---------- big-endian helpers ----------

pub fn be_u16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

pub fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

pub fn be_u64(b: &[u8]) -> u64 {
    u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

// ---------- CRC-32 (IEEE, same polynomial as zlib/gzip) ----------

/// CRC-32 over `data` — thin wrapper over `crc32fast` (SSE4.2
/// hardware CRC where available). Used for pack idx checksums.
pub fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

// ---------- file helpers ----------

/// Write a file atomically-ish: write to temp then rename.
pub fn write_file_atomic(path: &std::path::Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp_qel");
    {
        let mut f = std::fs::File::create(&tmp)?;
        use io::Write;
        f.write_all(data)?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn is_executable(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        false
    }
}
