//! cmix-style model blending — bit-level arithmetic coding.
//!
//! Three models blended via online logistic regression:
//!   PPM order-8    — structured/source code
//!   Order-1        — stable baseline, fast adaptation
//!   Match (LZ)     — repetitive data, node_modules
//!
//! Weights learned online per-context. Within 1KB the mixer discovers
//! which models are reliable for this specific data.
//!
//! Format: raw compressed bytes (caller stores original_len separately).

use super::ppm_byte::PpmByteModel;
use super::match_model::MatchModel;

const TOP:  u64 = 1u64 << 32;
const HALF: u64 = TOP >> 1;
const QTR:  u64 = TOP >> 2;
const SCALE: u32 = 1 << 16;

// ── Bit I/O ────────────────────────────────────────────────────────────────────

pub struct BitWriter { pub buf: Vec<u8>, pub byte: u8, pub pos: u8, pub pending: u64 }
impl BitWriter {
    pub fn new() -> Self { Self { buf: vec![], byte: 0, pos: 0, pending: 0 } }
    pub fn emit(&mut self, b: u64) {
        self.byte = (self.byte << 1) | (b & 1) as u8;
        self.pos += 1;
        if self.pos == 8 { self.buf.push(self.byte); self.byte = 0; self.pos = 0; }
    }
    pub fn emit_pending(&mut self, bit: u64) {
        self.emit(bit);
        for _ in 0..self.pending { self.emit(1-bit); }
        self.pending = 0;
    }
    pub fn done(mut self) -> Vec<u8> {
        if self.pos > 0 { self.buf.push(self.byte << (8-self.pos)); }
        self.buf
    }
}

pub struct BitReader<'a> { pub data: &'a [u8], pub byte: usize, pub pos: u8 }
impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self { Self { data, byte: 0, pos: 0 } }
    pub fn read(&mut self) -> u64 {
        if self.byte >= self.data.len() { return 0; }
        let b = ((self.data[self.byte] >> (7-self.pos)) & 1) as u64;
        self.pos += 1;
        if self.pos == 8 { self.byte += 1; self.pos = 0; }
        b
    }
}

/// Owned bit reader — same as BitReader but owns its data (no lifetime)
pub struct OwnedBitReader { pub data: Vec<u8>, pub byte: usize, pub pos: u8 }
impl OwnedBitReader {
    pub fn new(data: Vec<u8>) -> Self { Self { data, byte: 0, pos: 0 } }
    pub fn read(&mut self) -> u64 {
        if self.byte >= self.data.len() { return 0; }
        let b = ((self.data[self.byte] >> (7-self.pos)) & 1) as u64;
        self.pos += 1;
        if self.pos == 8 { self.byte += 1; self.pos = 0; }
        b
    }
}

pub fn renorm_enc(lo: &mut u64, hi: &mut u64, w: &mut BitWriter) {
    loop {
        if *hi <= HALF { w.emit_pending(0); *lo*=2; *hi*=2; }
        else if *lo >= HALF { w.emit_pending(1); *lo=(*lo-HALF)*2; *hi=(*hi-HALF)*2; }
        else if *lo >= QTR && *hi <= 3*QTR { w.pending+=1; *lo=(*lo-QTR)*2; *hi=(*hi-QTR)*2; }
        else { break; }
        if *hi <= *lo { *hi = *lo+1; }
    }
}

pub fn renorm_dec(lo: &mut u64, hi: &mut u64, val: &mut u64, r: &mut BitReader) {
    loop {
        if *hi <= HALF { *lo*=2; *hi*=2; *val=*val*2+r.read(); }
        else if *lo >= HALF { *lo=(*lo-HALF)*2; *hi=(*hi-HALF)*2; *val=(*val-HALF)*2+r.read(); }
        else if *lo >= QTR && *hi <= 3*QTR { *lo=(*lo-QTR)*2; *hi=(*hi-QTR)*2; *val=(*val-QTR)*2+r.read(); }
        else { break; }
        if *hi <= *lo { *hi = *lo+1; }
    }
}

// ── Models ─────────────────────────────────────────────────────────────────────

/// Order-1 byte model with per-bit-position counts.
/// counts[prev_byte][bit_position] = [count_0, count_1]
/// bit_position: 7=MSB, 0=LSB (same as encode loop ordering)
struct Order1 {
    counts: Box<[[[u32; 2]; 8]; 256]>,
    prev: u8,
}

impl Order1 {
    fn new() -> Self {
        let counts = Box::new([[[1u32; 2]; 8]; 256]);
        Self { counts, prev: 0 }
    }

    /// P(bit=1) for bit at position bit_pos (7=MSB..0=LSB)
    fn bit_prob(&self, bit_pos: u8) -> f64 {
        let c = &self.counts[self.prev as usize][bit_pos as usize];
        c[1] as f64 / (c[0] + c[1]) as f64
    }

    fn update(&mut self, byte: u8) {
        for bit_pos in 0..8u8 {
            let bit = (byte >> bit_pos) & 1;
            self.counts[self.prev as usize][bit_pos as usize][bit as usize] += 1;
        }
        self.prev = byte;
    }
}

// ── Mixer ──────────────────────────────────────────────────────────────────────

const N: usize = 3;  // PPM, Order1, Match
const LR: f64 = 0.005;
const CLAMP: f64 = 5.0;
const L2: f64 = 1e-4;  // L2 regularization — prevents weight blow-up after confident misses

#[inline] fn stretch(p: f64) -> f64 { let p=p.clamp(1e-6,1.0-1e-6); (p/(1.0-p)).ln().clamp(-CLAMP,CLAMP) }
#[inline] fn squash(x: f64) -> f64 { 1.0/(1.0+(-x).exp()) }

struct Mixer {
    // Context-indexed weights: w[ctx][model]
    w: Vec<[f64; N]>,
    last_in: [f64; N],
    last_pred: f64,
    last_ctx: u8,
}

impl Mixer {
    fn new() -> Self {
        let eq = 1.0 / N as f64;
        Self { w: vec![[eq; N]; 256], last_in: [eq; N], last_pred: 0.5, last_ctx: 0 }
    }

    fn predict(&mut self, probs: [f64; N], ctx: u8) -> f64 {
        let w = &self.w[ctx as usize];
        let sum: f64 = (0..N).map(|i| w[i] * stretch(probs[i])).sum();
        let pred = squash(sum);
        self.last_in = probs;
        self.last_pred = pred;
        self.last_ctx = ctx;
        pred
    }

    fn update(&mut self, actual: u8) {
        let err = actual as f64 - self.last_pred;
        let w = &mut self.w[self.last_ctx as usize];
        for i in 0..N {
            // Gradient step + L2 regularization (weight decay)
            // L2 pulls weights toward 0, preventing runaway after confident wrong predictions
            w[i] += LR * err * stretch(self.last_in[i]) - L2 * w[i];
        }
    }
}

// ── Config ─────────────────────────────────────────────────────────────────────

/// Which models to enable for this data type
#[derive(Clone, Copy)]
pub struct CmixConfig {
    pub use_ppm:   bool,
    pub use_order1: bool,
    pub use_match: bool,
}

impl CmixConfig {
    pub fn source()     -> Self { Self { use_ppm: true,  use_order1: true,  use_match: true  } }
    pub fn structured() -> Self { Self { use_ppm: true,  use_order1: true,  use_match: true  } }
    pub fn text()       -> Self { Self { use_ppm: true,  use_order1: true,  use_match: false } }
    pub fn binary()     -> Self { Self { use_ppm: false, use_order1: true,  use_match: true  } }
}

// ── State ──────────────────────────────────────────────────────────────────────

pub struct CmixState {
    ppm: PpmByteModel,
    o1: Order1,
    matcher: MatchModel,
    mixer: Mixer,
    // Ring buffer instead of Vec with remove(0) — O(1) instead of O(n) per byte
    ctx_buf: [u8; 64],
    ctx_len: usize,
    ctx_head: usize, // write position (oldest = ctx_head when full)
    cfg: CmixConfig,
}

impl CmixState {
    pub fn new(cfg: CmixConfig) -> Self {
        Self {
            ppm: PpmByteModel::new(),
            o1: Order1::new(),
            matcher: MatchModel::new(),
            mixer: Mixer::new(),
            ctx_buf: [0u8; 64],
            ctx_len: 0,
            ctx_head: 0,
            cfg,
        }
    }

    /// Returns context bytes in order (oldest first) as a stack-allocated slice.
    fn ctx_slice(&self) -> ([u8; 64], usize) {
        let mut out = [0u8; 64];
        let len = self.ctx_len;
        if len == 0 { return (out, 0); }
        if len < 64 {
            // Not full yet — data sits at [0..len] in order
            out[..len].copy_from_slice(&self.ctx_buf[..len]);
        } else {
            // Full ring — oldest is at ctx_head
            let tail = 64 - self.ctx_head;
            out[..tail].copy_from_slice(&self.ctx_buf[self.ctx_head..]);
            out[tail..64].copy_from_slice(&self.ctx_buf[..self.ctx_head]);
        }
        (out, len)
    }

    fn bit_prob(&mut self, bit_pos: u8) -> f64 {
        let ppm_p  = if self.cfg.use_ppm   { self.ppm.bit_prob(bit_pos) } else { 0.5 };
        let o1_p   = if self.cfg.use_order1 { self.o1.bit_prob(bit_pos)  } else { 0.5 };
        let (ctx_arr, ctx_len) = self.ctx_slice();
        let ctx_slice = &ctx_arr[..ctx_len];
        let (match_byte, conf) = self.matcher.predict(ctx_slice);
        let mat_p  = if self.cfg.use_match  { MatchModel::bit_prob(match_byte, conf, bit_pos) } else { 0.5 };
        let probs = [ppm_p, o1_p, mat_p];
        let ctx = ctx_slice.last().cloned().unwrap_or(0);
        self.mixer.predict(probs, ctx)
    }

    fn update_bit(&mut self, actual: u8) { self.mixer.update(actual); }

    fn update_byte(&mut self, byte: u8) {
        if self.cfg.use_ppm    { self.ppm.update(byte); }
        if self.cfg.use_order1 { self.o1.update(byte); }
        if self.cfg.use_match  { self.matcher.update(byte); }
        // Ring buffer write — O(1), no shifting
        self.ctx_buf[self.ctx_head] = byte;
        self.ctx_head = (self.ctx_head + 1) & 63; // wraps at 64
        if self.ctx_len < 64 { self.ctx_len += 1; }
    }
}

// ── Public encode/decode byte ─────────────────────────────────────────────────

/// Encode one byte into the arithmetic stream.
pub fn encode_byte(
    byte: u8,
    state: &mut CmixState,
    lo: &mut u64, hi: &mut u64,
    w: &mut BitWriter,
) {
    for bit_pos in (0..8u8).rev() {
        let actual = (byte >> bit_pos) & 1;
        let p1 = state.bit_prob(bit_pos);
        let p1s = (p1 * SCALE as f64).round() as u32;
        let p1s = p1s.clamp(1, SCALE-1);
        let (sl, sh) = if actual == 1 { (0, p1s) } else { (p1s, SCALE) };
        let range = *hi - *lo;
        let nh = *lo + range * sh as u64 / SCALE as u64;
        let nl = *lo + range * sl as u64 / SCALE as u64;
        *hi = if nh > nl { nh } else { nl+1 };
        *lo = nl;
        renorm_enc(lo, hi, w);
        state.update_bit(actual);
    }
    state.update_byte(byte);
}

/// Decode one byte from the arithmetic stream.
pub fn decode_byte(
    state: &mut CmixState,
    lo: &mut u64, hi: &mut u64, val: &mut u64,
    r: &mut BitReader,
) -> u8 {
    let mut byte = 0u8;
    for bit_pos in (0..8u8).rev() {
        let p1 = state.bit_prob(bit_pos);
        let p1s = (p1 * SCALE as f64).round() as u32;
        let p1s = p1s.clamp(1, SCALE-1);
        let range = *hi - *lo;
        let scaled = ((*val - *lo + 1) * SCALE as u64 - 1) / range;
        let bit = if scaled < p1s as u64 { 1u8 } else { 0u8 };
        let (sl, sh) = if bit == 1 { (0, p1s) } else { (p1s, SCALE) };
        let nh = *lo + range * sh as u64 / SCALE as u64;
        let nl = *lo + range * sl as u64 / SCALE as u64;
        *hi = if nh > nl { nh } else { nl+1 };
        *lo = nl;
        renorm_dec(lo, hi, val, r);
        state.update_bit(bit);
        byte = (byte << 1) | bit;
    }
    state.update_byte(byte);
    byte
}

/// Encode one byte using an owned bit writer (same logic as encode_byte).
pub fn encode_byte_owned(
    byte: u8,
    state: &mut CmixState,
    lo: &mut u64, hi: &mut u64,
    w: &mut BitWriter,
) {
    encode_byte(byte, state, lo, hi, w);
}

/// Decode one byte from an owned bit reader.
pub fn decode_byte_owned(
    state: &mut CmixState,
    lo: &mut u64, hi: &mut u64, val: &mut u64,
    r: &mut OwnedBitReader,
) -> u8 {
    let mut byte = 0u8;
    for bit_pos in (0..8u8).rev() {
        let p1 = state.bit_prob(bit_pos);
        let p1s = (p1 * SCALE as f64).round() as u32;
        let p1s = p1s.clamp(1, SCALE-1);
        let range = *hi - *lo;
        let scaled = ((*val - *lo + 1) * SCALE as u64 - 1) / range;
        let bit = if scaled < p1s as u64 { 1u8 } else { 0u8 };
        let (sl, sh) = if bit == 1 { (0, p1s) } else { (p1s, SCALE) };
        let nh = *lo + range * sh as u64 / SCALE as u64;
        let nl = *lo + range * sl as u64 / SCALE as u64;
        *hi = if nh > nl { nh } else { nl+1 };
        *lo = nl;
        renorm_dec_owned(lo, hi, val, r);
        state.update_bit(bit);
        byte = (byte << 1) | bit;
    }
    state.update_byte(byte);
    byte
}

pub fn renorm_dec_owned(lo: &mut u64, hi: &mut u64, val: &mut u64, r: &mut OwnedBitReader) {
    loop {
        if *hi <= HALF { *lo*=2; *hi*=2; *val=*val*2+r.read(); }
        else if *lo >= HALF { *lo=(*lo-HALF)*2; *hi=(*hi-HALF)*2; *val=(*val-HALF)*2+r.read(); }
        else if *lo >= QTR && *hi <= 3*QTR { *lo=(*lo-QTR)*2; *hi=(*hi-QTR)*2; *val=(*val-QTR)*2+r.read(); }
        else { break; }
        if *hi <= *lo { *hi = *lo+1; }
    }
}

// ── Standalone compress/decompress ────────────────────────────────────────────

pub fn compress_bytes(data: &[u8], cfg: CmixConfig) -> Vec<u8> {
    if data.is_empty() { return vec![]; }
    let mut state = CmixState::new(cfg);
    let mut w = BitWriter::new();
    let mut lo = 0u64; let mut hi = TOP;
    for &byte in data { encode_byte(byte, &mut state, &mut lo, &mut hi, &mut w); }
    w.pending += 1;
    if lo < QTR { w.emit_pending(0); } else { w.emit_pending(1); }
    w.done()
}

pub fn decompress_bytes(data: &[u8], original_len: usize, cfg: CmixConfig) -> Vec<u8> {
    if data.is_empty() || original_len == 0 { return vec![]; }
    let mut state = CmixState::new(cfg);
    let mut r = BitReader::new(data);
    let mut lo = 0u64; let mut hi = TOP; let mut val = 0u64;
    for _ in 0..32 { val = (val<<1)|r.read(); }
    (0..original_len).map(|_| decode_byte(&mut state, &mut lo, &mut hi, &mut val, &mut r)).collect()
}
