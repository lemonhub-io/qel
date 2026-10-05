//! zlib (RFC 1950) + DEFLATE (RFC 1951) implemented from scratch.
//! Inflate supports stored, fixed-Huffman and dynamic-Huffman blocks.
//! Deflate uses LZ77 (hash-chain matcher) with fixed Huffman codes —
//! valid zlib output readable by any compliant decoder.

use crate::util::{adler32, GitError, Result};

// ============================== INFLATE ==============================

/// Bit-level reader with a 64-bit bit buffer; bits arrive LSB-first.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,   // next byte to load into bitbuf
    bitbuf: u64,
    bitcnt: u32,  // valid bits in bitbuf
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader {
            data,
            pos: 0,
            bitbuf: 0,
            bitcnt: 0,
        }
    }

    #[inline(always)]
    fn fill(&mut self) {
        if self.bitcnt >= 56 {
            return;
        }
        // fast path: 8 bytes available
        if self.pos + 8 <= self.data.len() {
            let chunk = u64::from_le_bytes(self.data[self.pos..self.pos + 8].try_into().unwrap());
            self.bitbuf |= chunk << self.bitcnt;
            self.pos += (63 - self.bitcnt) as usize >> 3;
            self.bitcnt |= 56;
            return;
        }
        while self.bitcnt <= 56 && self.pos < self.data.len() {
            self.bitbuf |= (self.data[self.pos] as u64) << self.bitcnt;
            self.pos += 1;
            self.bitcnt += 8;
        }
    }

    /// Peek at up to n bits without consuming (n <= 56). Bits past EOF read 0.
    #[inline(always)]
    fn peek(&mut self) -> u64 {
        self.fill();
        self.bitbuf
    }

    #[inline(always)]
    fn drop_bits(&mut self, n: u32) {
        self.bitbuf >>= n;
        self.bitcnt -= n;
    }

    #[inline(always)]
    fn read_bits(&mut self, n: u32) -> Result<u32> {
        self.fill();
        if self.bitcnt < n {
            return Err(GitError::Parse("deflate: unexpected end of input".into()));
        }
        let v = (self.bitbuf & ((1u64 << n) - 1)) as u32;
        self.bitbuf >>= n;
        self.bitcnt -= n;
        Ok(v)
    }

    fn align_byte(&mut self) {
        let r = self.bitcnt % 8;
        self.bitbuf >>= r;
        self.bitcnt -= r;
    }

    /// bytes consumed so far (rounds up a partial byte)
    fn consumed(&self) -> usize {
        self.pos - (self.bitcnt as usize / 8)
    }
}

/// Canonical Huffman decoder: canonical counts+symbols for the slow path,
/// plus a single-level lookup table over the first FAST_BITS stream bits.
const FAST_BITS: usize = 9;
const FAST_SIZE: usize = 1 << FAST_BITS;

struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
    /// fast[i] = (symbol+1) << 5 | codelen when the low `len` bits of i are
    /// the bit-reversed code; 0 means "not a short code" (long/invalid —
    /// fall back to the canonical walk).
    fast: Vec<u32>,
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
        // canonical first-code values per length
        let mut next = [0u32; 16];
        let mut code = 0u32;
        for l in 1..16 {
            code = (code + counts[l - 1] as u32) << 1;
            next[l] = code;
        }
        let mut fast = vec![0u32; FAST_SIZE];
        let mut rank = [0u32; 16];
        for (sym, &l) in lengths.iter().enumerate() {
            if l == 0 {
                continue;
            }
            let l = l as usize;
            let code = next[l] + rank[l];
            rank[l] += 1;
            if l <= FAST_BITS {
                // stream bits arrive LSB-first but code is emitted MSB-first:
                // table index = bit-reversed code in the low l bits
                let rev = reverse_bits(code, l);
                let entry = ((sym as u32 + 1) << 5) | l as u32;
                let mut idx = rev as usize;
                while idx < FAST_SIZE {
                    fast[idx] = entry;
                    idx += 1 << l;
                }
            }
        }
        Ok(Huffman {
            counts,
            symbols,
            fast,
        })
    }

    #[inline(always)]
    fn decode(&self, br: &mut BitReader) -> Result<u16> {
        let bits = br.peek();
        let e = self.fast[(bits & (FAST_SIZE as u64 - 1)) as usize];
        if e != 0 {
            let len = e & 31;
            if len as u32 <= br.bitcnt {
                br.drop_bits(len as u32);
                return Ok(((e >> 5) - 1) as u16);
            }
            return Err(GitError::Parse("deflate: truncated huffman code".into()));
        }
        // long or invalid code: canonical walk, one bit at a time
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: usize = 0;
        for len in 1..16 {
            br.fill();
            if br.bitcnt == 0 {
                return Err(GitError::Parse("deflate: unexpected end of input".into()));
            }
            code = (code << 1) | (br.bitbuf & 1) as i32;
            br.bitbuf >>= 1;
            br.bitcnt -= 1;
            let count = self.counts[len] as i32;
            if code - first < count {
                return Ok(self.symbols[index + (code - first) as usize]);
            }
            index += count as usize;
            first = (first + count) << 1;
        }
        Err(GitError::Parse("deflate: invalid huffman code".into()))
    }
}

#[inline]
fn reverse_bits(v: u32, n: usize) -> u32 {
    let mut r = 0u32;
    for i in 0..n {
        r |= ((v >> i) & 1) << (n - 1 - i);
    }
    r
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
        let bfinal = br.read_bits(1)?;
        let btype = br.read_bits(2)?;
        match btype {
            0 => {
                br.align_byte();
                // bitbuf may hold buffered bytes; the stored block begins at
                // the first unconsumed byte
                let p = br.pos - br.bitcnt as usize / 8;
                if p + 4 > data.len() {
                    return Err(GitError::Parse("deflate: truncated stored block".into()));
                }
                let len = u16::from_le_bytes([data[p], data[p + 1]]) as usize;
                let nlen = u16::from_le_bytes([data[p + 2], data[p + 3]]) as usize;
                if len != (!nlen & 0xFFFF) {
                    return Err(GitError::Parse("deflate: bad stored LEN/NLEN".into()));
                }
                if p + 4 + len > data.len() {
                    return Err(GitError::Parse("deflate: truncated stored data".into()));
                }
                out.extend_from_slice(&data[p + 4..p + 4 + len]);
                // restart buffering after the stored bytes
                br.pos = p + 4 + len;
                br.bitbuf = 0;
                br.bitcnt = 0;
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
                if d >= len {
                    out.extend_from_within(start..start + len);
                } else {
                    out.reserve(len);
                    for k in 0..len {
                        let b = out[start + k];
                        out.push(b);
                    }
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

/// Bit-level writer with a 64-bit bit buffer; bits flushed LSB-first.
struct BitWriter {
    out: Vec<u8>,
    bitbuf: u64,
    bitcnt: u32,
}

impl BitWriter {
    fn new() -> Self {
        BitWriter {
            out: Vec::new(),
            bitbuf: 0,
            bitcnt: 0,
        }
    }

    #[inline(always)]
    fn flush_bytes(&mut self) {
        while self.bitcnt >= 8 {
            self.out.push(self.bitbuf as u8);
            self.bitbuf >>= 8;
            self.bitcnt -= 8;
        }
    }

    /// plain value, LSB-first
    #[inline(always)]
    fn write_bits(&mut self, v: u32, n: u32) {
        if n == 0 {
            return;
        }
        self.bitbuf |= (v as u64 & ((1u64 << n) - 1)) << self.bitcnt;
        self.bitcnt += n;
        self.flush_bytes();
    }

    /// huffman code: MSB of the code is emitted first
    #[inline(always)]
    fn write_code(&mut self, code: u32, len: u32) {
        let rev = (code as u64).reverse_bits() >> (64 - len);
        self.bitbuf |= rev << self.bitcnt;
        self.bitcnt += len;
        self.flush_bytes();
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bitcnt > 0 {
            self.out.push(self.bitbuf as u8);
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
const MAX_CHAIN: usize = 64;
const GOOD_MATCH: usize = 32;
const NICE_MATCH: usize = 128;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;

fn hash3(data: &[u8], i: usize) -> usize {
    let v = (data[i] as u32) | ((data[i + 1] as u32) << 8) | ((data[i + 2] as u32) << 16);
    (v.wrapping_mul(0x9E3779B1) >> (32 - HASH_BITS)) as usize
}

thread_local! {
    /// Hash-chain tables reused across deflate calls in a thread. Entries are
    /// absolute positions in the *current* input; stale entries from previous
    /// buffers are detected by the window/bounds guards in the matcher.
    static DEF_HEAD: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
    static DEF_PREV: std::cell::RefCell<Vec<i32>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Compress with fixed-Huffman blocks. Inputs are chunked at 8MB per block.
pub fn deflate_raw(data: &[u8]) -> Vec<u8> {
    let mut bw = BitWriter::new();
    const BLOCK_CHUNK: usize = 8 << 20;
    let nblocks = data.len().div_ceil(BLOCK_CHUNK).max(1);
    DEF_HEAD.with(|h| {
        DEF_PREV.with(|p| {
            let mut head = h.borrow_mut();
            let mut prev = p.borrow_mut();
            head.clear();
            head.resize(HASH_SIZE, -1);
            prev.clear();
            prev.resize(WINDOW, -1);
            for blk in 0..nblocks {
                let start = blk * BLOCK_CHUNK;
                let end = ((blk + 1) * BLOCK_CHUNK).min(data.len());
                let last = blk == nblocks - 1;
                bw.write_bits(last as u32, 1);
                bw.write_bits(1, 2); // fixed huffman
                deflate_block(&mut bw, data, start, end, &mut head, &mut prev);
            }
        })
    });
    bw.finish()
}

/// Emit matches/literals for data[start..end]; hash chains are indexed by
/// absolute position and bounded by the 32K window.
fn deflate_block(
    bw: &mut BitWriter,
    data: &[u8],
    start: usize,
    end: usize,
    head: &mut [i32],
    prev: &mut [i32],
) {
    let n = end;
    let mut i = start;
    while i < n {
        let mut best_len = 0usize;
        let mut best_dist = 0usize;
        if i + MIN_MATCH < n {
            let h = hash3(data, i);
            let mut cand = head[h];
            let mut chain = 0u32;
            let limit = i.saturating_sub(WINDOW);
            while cand >= 0 {
                let max_chain = if best_len >= GOOD_MATCH {
                    MAX_CHAIN / 4
                } else {
                    MAX_CHAIN
                };
                if chain >= max_chain as u32 {
                    break;
                }
                let c = cand as usize;
                // stale entries (previous buffers / overwritten slots) show up
                // as out-of-window or forward positions — end the chain there
                if c < limit || c >= i {
                    break;
                }
                // quick check: compare byte at current best_len
                if best_len == 0
                    || (c + best_len < n
                        && i + best_len < n
                        && data[c + best_len] == data[i + best_len])
                {
                    let mut l = 0usize;
                    let maxl = (n - i).min(MAX_MATCH);
                    while l + 8 <= maxl
                        && u64::from_le_bytes(data[c + l..c + l + 8].try_into().unwrap())
                            == u64::from_le_bytes(data[i + l..i + l + 8].try_into().unwrap())
                    {
                        l += 8;
                    }
                    while l < maxl && data[c + l] == data[i + l] {
                        l += 1;
                    }
                    if l > best_len {
                        best_len = l;
                        best_dist = i - c;
                        if l >= NICE_MATCH {
                            break;
                        }
                    }
                }
                cand = prev[c & (WINDOW - 1)];
                chain += 1;
            }
            // insert current position into chain
            prev[i & (WINDOW - 1)] = head[h];
            head[h] = i as i32;
        }
        if best_len >= MIN_MATCH && (best_len > 3 || best_dist <= 4096) {
            emit_backref(bw, best_len, best_dist);
            // add skipped positions to the hash chains; for long matches
            // only sample — keeps the table hot without per-byte cost
            let next = i + best_len;
            let step = if best_len >= GOOD_MATCH { 4 } else { 1 };
            let mut j = i + 1;
            while j < next && j + MIN_MATCH < n {
                let h = hash3(data, j);
                prev[j & (WINDOW - 1)] = head[h];
                head[h] = j as i32;
                j += step;
            }
            i = next;
        } else {
            let (c, l) = fixed_lit_code(data[i] as usize);
            bw.write_code(c, l);
            i += 1;
        }
    }
    // end of block
    let (c, l) = fixed_lit_code(256);
    bw.write_code(c, l);
}

#[inline]
fn emit_backref(bw: &mut BitWriter, len: usize, dist: usize) {
    let (sym, ebits, eval) = length_symbol(len);
    let (c, l) = fixed_lit_code(sym);
    bw.write_code(c, l);
    bw.write_bits(eval, ebits);
    let (dsym, dbits, dval) = dist_symbol(dist);
    bw.write_code(dsym as u32, 5);
    bw.write_bits(dval, dbits);
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
