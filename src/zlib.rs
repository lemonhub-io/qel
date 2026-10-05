//! zlib (RFC 1950) + DEFLATE (RFC 1951) implemented from scratch.
//! Inflate supports stored, fixed-Huffman and dynamic-Huffman blocks.
//! Deflate uses LZ77 (hash-chain matcher) with fixed Huffman codes —
//! valid zlib output readable by any compliant decoder.

use crate::util::{adler32, GitError, Result};

// ============================== INFLATE ==============================

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,   // byte position
    bit: u32,     // bits consumed in current byte (0..8)
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, bit: 0 }
    }

    fn read_bit(&mut self) -> Result<u32> {
        if self.pos >= self.data.len() {
            return Err(GitError::Parse("deflate: unexpected end of input".into()));
        }
        let b = (self.data[self.pos] >> self.bit) & 1;
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.pos += 1;
        }
        Ok(b as u32)
    }

    /// n bits, LSB-first
    fn read_bits(&mut self, n: u32) -> Result<u32> {
        let mut v = 0u32;
        for i in 0..n {
            v |= self.read_bit()? << i;
        }
        Ok(v)
    }

    fn align_byte(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.pos += 1;
        }
    }

    /// bytes consumed so far (rounds up a partial byte)
    fn consumed(&self) -> usize {
        self.pos + if self.bit > 0 { 1 } else { 0 }
    }
}

/// Canonical Huffman decoder using the puff() scheme:
/// counts[len] = number of symbols with that code length,
/// symbols[] sorted by code length then by symbol value order.
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Huffman> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            if l > 15 {
                return Err(GitError::Parse("deflate: code length > 15".into()));
            }
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        // check over-subscription
        let mut left: i32 = 1;
        for l in 1..16 {
            left <<= 1;
            left -= counts[l] as i32;
            if left < 0 {
                return Err(GitError::Parse("deflate: over-subscribed code".into()));
            }
        }
        let mut offs = [0usize; 16];
        for l in 1..15 {
            offs[l + 1] = offs[l] + counts[l] as usize;
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize]] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Huffman { counts, symbols })
    }

    fn decode(&self, br: &mut BitReader) -> Result<u16> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: usize = 0;
        for len in 1..16 {
            code |= br.read_bit()? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return Ok(self.symbols[index + (code - first) as usize]);
            }
            index += count as usize;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(GitError::Parse("deflate: invalid huffman code".into()))
    }
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
    131, 163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
    13, 13,
];

fn fixed_lit_lengths() -> [u8; 288] {
    let mut l = [0u8; 288];
    for (i, v) in l.iter_mut().enumerate() {
        *v = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    l
}

/// Inflate a raw DEFLATE stream. Returns (decompressed, deflate bytes consumed).
pub fn inflate_raw(data: &[u8], size_hint: usize) -> Result<(Vec<u8>, usize)> {
    let mut br = BitReader::new(data);
    let mut out: Vec<u8> = Vec::with_capacity(size_hint.max(64));
    loop {
        let bfinal = br.read_bit()?;
        let btype = br.read_bits(2)?;
        match btype {
            0 => {
                br.align_byte();
                if br.pos + 4 > data.len() {
                    return Err(GitError::Parse("deflate: truncated stored block".into()));
                }
                let len = u16::from_le_bytes([data[br.pos], data[br.pos + 1]]) as usize;
                let nlen = u16::from_le_bytes([data[br.pos + 2], data[br.pos + 3]]) as usize;
                if len != (!nlen & 0xFFFF) {
                    return Err(GitError::Parse("deflate: bad stored LEN/NLEN".into()));
                }
                br.pos += 4;
                if br.pos + len > data.len() {
                    return Err(GitError::Parse("deflate: truncated stored data".into()));
                }
                out.extend_from_slice(&data[br.pos..br.pos + len]);
                br.pos += len;
            }
            1 => {
                let lit = Huffman::new(&fixed_lit_lengths())?;
                let dist = Huffman::new(&[5u8; 30])?;
                inflate_block(&mut br, &mut out, &lit, &dist)?;
            }
            2 => {
                let hlit = br.read_bits(5)? as usize + 257;
                let hdist = br.read_bits(5)? as usize + 1;
                let hclen = br.read_bits(4)? as usize + 4;
                const ORDER: [usize; 19] = [
                    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                ];
                let mut cl_lens = [0u8; 19];
                for i in 0..hclen {
                    cl_lens[ORDER[i]] = br.read_bits(3)? as u8;
                }
                let cl = Huffman::new(&cl_lens)?;
                let mut lengths = vec![0u8; hlit + hdist];
                let mut i = 0;
                while i < hlit + hdist {
                    let sym = cl.decode(&mut br)?;
                    match sym {
                        0..=15 => {
                            lengths[i] = sym as u8;
                            i += 1;
                        }
                        16 => {
                            if i == 0 {
                                return Err(GitError::Parse("deflate: repeat w/o prev".into()));
                            }
                            let prev = lengths[i - 1];
                            let rep = 3 + br.read_bits(2)? as usize;
                            for _ in 0..rep {
                                if i >= lengths.len() {
                                    return Err(GitError::Parse("deflate: len overrun".into()));
                                }
                                lengths[i] = prev;
                                i += 1;
                            }
                        }
                        17 => {
                            let rep = 3 + br.read_bits(3)? as usize;
                            i += rep;
                            if i > lengths.len() {
                                return Err(GitError::Parse("deflate: len overrun".into()));
                            }
                        }
                        18 => {
                            let rep = 11 + br.read_bits(7)? as usize;
                            i += rep;
                            if i > lengths.len() {
                                return Err(GitError::Parse("deflate: len overrun".into()));
                            }
                        }
                        _ => return Err(GitError::Parse("deflate: bad CL symbol".into())),
                    }
                }
                if lengths[256] == 0 {
                    return Err(GitError::Parse("deflate: missing end-of-block code".into()));
                }
                let lit = Huffman::new(&lengths[..hlit])?;
                let dist = Huffman::new(&lengths[hlit..])?;
                inflate_block(&mut br, &mut out, &lit, &dist)?;
            }
            _ => return Err(GitError::Parse("deflate: invalid block type".into())),
        }
        if bfinal == 1 {
            break;
        }
    }
    Ok((out, br.consumed()))
}

fn inflate_block(
    br: &mut BitReader,
    out: &mut Vec<u8>,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<()> {
    loop {
        let sym = lit.decode(br)?;
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Ok(()),
            257..=285 => {
                let li = (sym - 257) as usize;
                let len = LENGTH_BASE[li] as usize + br.read_bits(LENGTH_EXTRA[li] as u32)? as usize;
                let dsym = dist.decode(br)? as usize;
                if dsym >= 30 {
                    return Err(GitError::Parse("deflate: bad distance code".into()));
                }
                let d = DIST_BASE[dsym] as usize + br.read_bits(DIST_EXTRA[dsym] as u32)? as usize;
                if d > out.len() {
                    return Err(GitError::Parse("deflate: distance too far back".into()));
                }
                let start = out.len() - d;
                for k in 0..len {
                    let b = out[start + k];
                    out.push(b);
                }
            }
            _ => return Err(GitError::Parse("deflate: invalid length symbol".into())),
        }
    }
}

/// Inflate a zlib stream (RFC 1950). Returns (decompressed, total bytes consumed
/// including the 2-byte header and 4-byte adler32 trailer).
pub fn inflate(data: &[u8], size_hint: usize) -> Result<(Vec<u8>, usize)> {
    if data.len() < 2 {
        return Err(GitError::Parse("zlib: stream too short".into()));
    }
    let cmf = data[0];
    let flg = data[1];
    if cmf & 0x0F != 8 {
        return Err(GitError::Parse("zlib: unsupported compression method".into()));
    }
    if (cmf as u32 * 256 + flg as u32) % 31 != 0 {
        return Err(GitError::Parse("zlib: bad header check".into()));
    }
    if flg & 0x20 != 0 {
        return Err(GitError::Parse("zlib: preset dictionary unsupported".into()));
    }
    let (out, dused) = inflate_raw(&data[2..], size_hint)?;
    let end = 2 + dused;
    if end + 4 > data.len() {
        return Err(GitError::Parse("zlib: missing adler32".into()));
    }
    let stored = crate::util::be_u32(&data[end..end + 4]);
    if stored != adler32(&out) {
        return Err(GitError::Parse("zlib: adler32 mismatch".into()));
    }
    Ok((out, end + 4))
}

// ============================== DEFLATE ==============================

struct BitWriter {
    out: Vec<u8>,
    cur: u8,
    nbits: u32,
}

impl BitWriter {
    fn new() -> Self {
        BitWriter { out: Vec::new(), cur: 0, nbits: 0 }
    }

    fn write_bit(&mut self, b: u32) {
        self.cur |= ((b & 1) as u8) << self.nbits;
        self.nbits += 1;
        if self.nbits == 8 {
            self.out.push(self.cur);
            self.cur = 0;
            self.nbits = 0;
        }
    }

    /// plain value, LSB-first
    fn write_bits(&mut self, v: u32, n: u32) {
        for i in 0..n {
            self.write_bit(v >> i);
        }
    }

    /// huffman code: MSB of the code is emitted first
    fn write_code(&mut self, code: u32, len: u32) {
        for i in (0..len).rev() {
            self.write_bit(code >> i);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            self.out.push(self.cur);
        }
        self.out
    }
}

/// Fixed-huffman literal/length code for a symbol (0..=287). Returns (code, bits).
fn fixed_lit_code(sym: usize) -> (u32, u32) {
    match sym {
        0..=143 => (0x30 + sym as u32, 8),
        144..=255 => (0x190 + (sym as u32 - 144), 9),
        256..=279 => (sym as u32 - 256, 7),
        _ => (0xC0 + (sym as u32 - 280), 8),
    }
}

fn length_symbol(len: usize) -> (usize, u32, u32) {
    // returns (symbol, extra_bits, extra_value)
    debug_assert!((3..=258).contains(&len));
    if len == 258 {
        return (285, 0, 0);
    }
    for i in (0..28).rev() {
        if len >= LENGTH_BASE[i] as usize {
            return (
                257 + i,
                LENGTH_EXTRA[i] as u32,
                (len - LENGTH_BASE[i] as usize) as u32,
            );
        }
    }
    unreachable!()
}

fn dist_symbol(dist: usize) -> (usize, u32, u32) {
    debug_assert!(dist >= 1);
    for i in (0..30).rev() {
        if dist >= DIST_BASE[i] as usize {
            return (i, DIST_EXTRA[i] as u32, (dist - DIST_BASE[i] as usize) as u32);
        }
    }
    unreachable!()
}

const HASH_BITS: usize = 15;
const HASH_SIZE: usize = 1 << HASH_BITS;
const WINDOW: usize = 32768;
const MAX_CHAIN: usize = 256;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;

fn hash3(data: &[u8], i: usize) -> usize {
    let v = (data[i] as u32) | ((data[i + 1] as u32) << 8) | ((data[i + 2] as u32) << 16);
    (v.wrapping_mul(0x9E3779B1) >> (32 - HASH_BITS)) as usize
}

/// LZ77-parse `data` and emit one fixed-Huffman block into `bw`.
/// Calls `emit(bw, LitOrMatch)` internally.
struct Emitter<'a> {
    bw: &'a mut BitWriter,
}

impl<'a> Emitter<'a> {
    fn literal(&mut self, b: u8) {
        let (c, l) = fixed_lit_code(b as usize);
        self.bw.write_code(c, l);
    }
    fn backref(&mut self, len: usize, dist: usize) {
        let (sym, ebits, eval) = length_symbol(len);
        let (c, l) = fixed_lit_code(sym);
        self.bw.write_code(c, l);
        self.bw.write_bits(eval, ebits);
        let (dsym, dbits, dval) = dist_symbol(dist);
        self.bw.write_code(dsym as u32, 5);
        self.bw.write_bits(dval, dbits);
    }
}

/// Compress with a single fixed-Huffman block (BFINAL=1, BTYPE=01).
/// For very large inputs we split into multiple fixed blocks to keep
/// hash tables small in memory (chains reset per block is unnecessary —
/// we just run the matcher over the whole input and emit one block;
/// deflate allows matches across any distance within the window).
pub fn deflate_raw(data: &[u8]) -> Vec<u8> {
    let mut bw = BitWriter::new();
    // For inputs > ~16MB, emitting multiple blocks keeps memory sane.
    const BLOCK_CHUNK: usize = 8 << 20;
    let nblocks = data.len().div_ceil(BLOCK_CHUNK).max(1);
    for blk in 0..nblocks {
        let start = blk * BLOCK_CHUNK;
        let end = ((blk + 1) * BLOCK_CHUNK).min(data.len());
        let last = blk == nblocks - 1;
        bw.write_bit(last as u32);
        bw.write_bits(1, 2); // fixed huffman
        deflate_block(&mut bw, &data[..end], start);
    }
    bw.finish()
}

/// Emit matches/literals for data[start..end]; `data` is the full buffer so
/// matches may reference earlier content within the 32K window.
fn deflate_block(bw: &mut BitWriter, data: &[u8], start: usize) {
    let mut head = vec![-1i64; HASH_SIZE];
    let mut prev = vec![-1i64; data.len().min(start + WINDOW + MAX_MATCH + 64)];
    let mut em = Emitter { bw };
    let mut i = start;
    let n = data.len();
    while i < n {
        let mut best_len = 0usize;
        let mut best_dist = 0usize;
        if i + MIN_MATCH < n {
            let h = hash3(data, i);
            let mut cand = head[h];
            let mut chain = 0;
            let limit = i.saturating_sub(WINDOW);
            while cand >= 0 && chain < MAX_CHAIN {
                let c = cand as usize;
                if c < limit {
                    break;
                }
                // quick check: compare byte at current best_len
                if best_len == 0
                    || (c + best_len < n
                        && i + best_len < n
                        && data[c + best_len] == data[i + best_len])
                {
                    let mut l = 0usize;
                    while i + l < n && l < MAX_MATCH && data[c + l] == data[i + l] {
                        l += 1;
                    }
                    if l > best_len {
                        best_len = l;
                        best_dist = i - c;
                        if l >= MAX_MATCH {
                            break;
                        }
                    }
                }
                cand = prev[c % prev.len()];
                chain += 1;
            }
            // insert current position into chain
            let plen = prev.len();
            prev[i % plen] = head[h];
            head[h] = i as i64;
        }
        if best_len >= MIN_MATCH && (best_len > 3 || best_dist <= 4096) {
            em.backref(best_len, best_dist);
            // add skipped positions to the hash chains (sampled for speed)
            let next = i + best_len;
            let mut j = i + 1;
            while j < next && j + MIN_MATCH < n {
                let h = hash3(data, j);
                let plen = prev.len();
                prev[j % plen] = head[h];
                head[h] = j as i64;
                j += 1;
            }
            i = next;
        } else {
            em.literal(data[i]);
            i += 1;
        }
    }
    // end of block
    let (c, l) = fixed_lit_code(256);
    em.bw.write_code(c, l);
}

/// Compress `data` into a zlib stream (RFC 1950).
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    out.push(0x78); // CMF: 32K window, deflate
    out.push(0x01); // FLG: check bits (0x7801 % 31 == 0)
    out.extend_from_slice(&deflate_raw(data));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}
