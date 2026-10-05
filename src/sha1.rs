//! SHA-1 hashing.
//!
//! Thin wrapper over the RustCrypto `sha1` crate (SHA-NI/SIMD
//! accelerated where available). SHA-1 is a byte-level primitive — the
//! Git-specific part is *what* gets hashed (object headers, pack
//! trailers, index checksums), which all lives in the callers.
//! The `Sha1`/`sha1` API is unchanged from the in-house version.

use sha1::Digest;

/// Streaming SHA-1 hasher.
pub struct Sha1(sha1::Sha1);

impl Sha1 {
    pub fn new() -> Self {
        Sha1(sha1::Sha1::new())
    }
    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
    pub fn finalize(self) -> [u8; 20] {
        self.0.finalize().into()
    }
}

/// One-shot SHA-1.
#[allow(dead_code)]
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h = Sha1::new();
    h.update(data);
    h.finalize()
}

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}
