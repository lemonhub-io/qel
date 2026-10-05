//! pkt-line framing used by the git wire protocol.

use crate::util::{GitError, Result};
use std::io::{Read, Write};

pub const FLUSH: &[u8] = b"0000";
pub const DELIM: &[u8] = b"0001";
pub const RESPONSE_END: &[u8] = b"0002";

pub fn encode(data: &[u8]) -> Vec<u8> {
    let len = data.len() + 4;
    let mut out = Vec::with_capacity(len);
    out.extend_from_slice(format!("{:04x}", len).as_bytes());
    out.extend_from_slice(data);
    out
}

pub fn encode_str(s: &str) -> Vec<u8> {
    encode(s.as_bytes())
}

/// Read one pkt-line. Returns:
/// - Ok(Some(data)) for a data packet
/// - Ok(None) for a flush packet (0000)
pub fn read(r: &mut dyn Read) -> Result<Option<Vec<u8>>> {
    let mut hdr = [0u8; 4];
    match r.read_exact(&mut hdr) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(GitError::Protocol("unexpected EOF in pkt-line".into()))
        }
        Err(e) => return Err(e.into()),
    }
    let s = std::str::from_utf8(&hdr)
        .map_err(|_| GitError::Protocol("bad pkt-line header".into()))?;
    let len = usize::from_str_radix(s.trim(), 16)
        .map_err(|_| GitError::Protocol(format!("bad pkt-line length: {}", s)))?;
    match len {
        0 => return Ok(None),
        1 | 2 => return Ok(Some(vec![len as u8])), // delim/response-end markers
        3 => return Err(GitError::Protocol("invalid pkt-line 0003".into())),
        _ => {}
    }
    let mut data = vec![0u8; len - 4];
    r.read_exact(&mut data)?;
    Ok(Some(data))
}

/// Read all pkt-lines until flush.
pub fn read_until_flush(r: &mut dyn Read) -> Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    while let Some(d) = read(r)? {
        if d == vec![1u8] || d == vec![2u8] {
            continue; // treat delim markers as separators, skip
        }
        out.push(d);
    }
    Ok(out)
}

pub fn write_all(w: &mut dyn Write, lines: &[Vec<u8>]) -> Result<()> {
    for l in lines {
        w.write_all(&encode(l))?;
    }
    Ok(())
}
