//! Zlib/DEFLATE codec.
//!
//! This module is a thin wrapper over `flate2` (zlib-rs backend): Git's
//! object compression is plain RFC 1950 zlib, and the codec is a
//! byte-level primitive, not Git semantics. Keeping a maintained,
//! hardware-tuned DEFLATE here is strictly better than a hand-rolled
//! one — the Git format work (object headers, pack structure, delta
//! resolution) all lives elsewhere.
//!
//! The public surface is unchanged from the original in-house codec so
//! callers are unaffected: `inflate` decompresses a zlib stream from the
//! front of `data` and reports how many input bytes it consumed (pack
//! entries are streams inside a larger buffer), and `deflate` produces
//! a zlib stream at git's default compression level (6).

use crate::util::{GitError, Result};
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

/// Inflate a zlib stream at the front of `data`.
///
/// Returns `(decompressed, consumed)` where `consumed` is the number of
/// input bytes the stream occupied — callers rely on this to find the
/// next object inside a pack buffer.
pub fn inflate(data: &[u8], size_hint: usize) -> Result<(Vec<u8>, usize)> {
    inflate_impl(data, size_hint, true)
}

/// Inflate a raw DEFLATE stream (no zlib header/adler trailer).
#[allow(dead_code)]
pub fn inflate_raw(data: &[u8], size_hint: usize) -> Result<(Vec<u8>, usize)> {
    inflate_impl(data, size_hint, false)
}

fn inflate_impl(data: &[u8], size_hint: usize, zlib_header: bool) -> Result<(Vec<u8>, usize)> {
    let mut d = Decompress::new(zlib_header);
    let mut out: Vec<u8> = Vec::with_capacity(size_hint.max(64));
    loop {
        let consumed = d.total_in() as usize;
        let status = d
            .decompress_vec(&data[consumed..], &mut out, FlushDecompress::None)
            .map_err(|e| GitError::Parse(format!("deflate: {}", e)))?;
        match status {
            Status::StreamEnd => return Ok((out, d.total_in() as usize)),
            Status::Ok | Status::BufError => {
                if out.len() < out.capacity() {
                    // No progress possible: input is exhausted mid-stream.
                    return Err(GitError::Parse("deflate: truncated stream".into()));
                }
                out.reserve(64 * 1024);
            }
        }
    }
}

/// Deflate `data` as a zlib stream (RFC 1950) at git's default level.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    deflate_impl(data, true)
}

/// Deflate `data` as a raw DEFLATE stream.
#[allow(dead_code)]
pub fn deflate_raw(data: &[u8]) -> Vec<u8> {
    deflate_impl(data, false)
}

fn deflate_impl(data: &[u8], zlib_header: bool) -> Vec<u8> {
    // zlib compressBound: source len + ~0.1% + slack.
    let bound = data.len() + data.len() / 1000 + 64;
    let mut out: Vec<u8> = Vec::with_capacity(bound);
    let mut c = Compress::new(Compression::default(), zlib_header);
    let mut in_pos = 0usize;
    loop {
        let before = c.total_in() as usize;
        let status = c
            .compress_vec(&data[in_pos..], &mut out, FlushCompress::Finish)
            .expect("deflate: compress_vec failed");
        in_pos += c.total_in() as usize - before;
        match status {
            Status::StreamEnd => return out,
            Status::Ok | Status::BufError => out.reserve(bound.max(64 * 1024)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for case in [
            b"".to_vec(),
            b"hello world".to_vec(),
            vec![0u8; 100_000],
            (0..255u8).cycle().take(300_000).collect(),
        ] {
            let packed = deflate(&case);
            let (back, used) = inflate(&packed, case.len()).unwrap();
            assert_eq!(back, case);
            assert_eq!(used, packed.len());
        }
    }

    #[test]
    fn inflate_reports_consumed_prefix() {
        let packed = deflate(b"payload");
        let mut buf = packed.clone();
        buf.extend_from_slice(b"trailing-junk");
        let (out, used) = inflate(&buf, 7).unwrap();
        assert_eq!(out, b"payload");
        assert_eq!(used, packed.len());
    }
}
