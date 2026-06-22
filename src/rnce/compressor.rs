//! Unified compressor — cmix-style model blending.
//!
//! Blends 3 models via online-learned logistic regression weights:
//!   0 = PPM order-8 byte model     — best on varied code, long-range context
//!   1 = Order-1 byte model         — fast, stable, good baseline
//!   2 = Match model (LZ-style)     — best on repetitive data
//!
//! The mixer weights update online after every bit. Within the first ~1KB
//! it learns which models are reliable for this particular data.
//!
//! Grammar model is integrated as a pre-pass: the grammar codec encodes
//! the file as tokens, then cmix encodes those token bytes. This gives
//! grammar structure + adaptive blending without the encode/decode asymmetry.
//!
//! Result: better than any single model on mixed corpora. On node_modules
//! (thousands of similar JS files), the match model dominates and approaches
//! near-zero cost for repeated files.

const N_MODELS: usize = 3;
const SCALE: u32 = 1 << 16; // arithmetic coder precision

// ── Arithmetic coder (bit-level) ──────────────────────────────────────────────

const TOP: u64 = 1u64 << 32;
const HALF: u64 = TOP >> 1;
const QTR: u64 = TOP >> 2;

struct BitWriter { buf: Vec<u8>, byte: u8, pos: u8, pending: u64 }
impl BitWriter {
    fn new() -> Self { Self { buf: vec![], byte: 0, pos: 0, pending: 0 } }
    fn emit(&mut self, b: u64) {
        self.byte = (self.byte << 1) | (b & 1) as u8;
        self.pos += 1;
        if self.pos == 8 { self.buf.push(self.byte); self.byte = 0; self.pos = 0; }
    }
    fn emit_pending(&mut self, bit: u64) {
        self.emit(bit);
        for _ in 0..self.pending { self.emit(1-bit); }
        self.pending = 0;
    }
    fn done(mut self) -> Vec<u8> {
        if self.pos > 0 { self.buf.push(self.byte << (8-self.pos)); }
        self.buf
    }
}

struct BitReader<'a> { data: &'a [u8], byte: usize, pos: u8 }
impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self { Self { data, byte: 0, pos: 0 } }
    fn read(&mut self) -> u64 {
        if self.byte >= self.data.len() { return 0; }
        let b = ((self.data[self.byte] >> (7-self.pos)) & 1) as u64;
        self.pos += 1;
        if self.pos == 8 { self.byte += 1; self.pos = 0; }
        b
    }
}

fn renorm_enc(lo: &mut u64, hi: &mut u64, w: &mut BitWriter) {
    loop {
        if *hi <= HALF { w.emit_pending(0); *lo*=2; *hi*=2; }
        else if *lo >= HALF { w.emit_pending(1); *lo=(*lo-HALF)*2; *hi=(*hi-HALF)*2; }
        else if *lo >= QTR && *hi <= 3*QTR { w.pending+=1; *lo=(*lo-QTR)*2; *hi=(*hi-QTR)*2; }
        else { break; }
        if *hi <= *lo { *hi = *lo+1; }
    }
}

fn renorm_dec(lo: &mut u64, hi: &mut u64, val: &mut u64, r: &mut BitReader) {
    loop {
        if *hi <= HALF { *lo*=2; *hi*=2; *val=*val*2+r.read(); }
        else if *lo >= HALF { *lo=(*lo-HALF)*2; *hi=(*hi-HALF)*2; *val=(*val-HALF)*2+r.read(); }
        else if *lo >= QTR && *hi <= 3*QTR { *lo=(*lo-QTR)*2; *hi=(*hi-QTR)*2; *val=(*val-QTR)*2+r.read(); }
        else { break; }
        if *hi <= *lo { *hi = *lo+1; }
    }
}

// ── Order-1 byte model ────────────────────────────────────────────────────────

struct Order1 {
    // table[prev][bit] counts: [count_0, count_1]
    // We store per-bit-position counts within each context
    counts: Vec<[[u32; 2]; 8]>, // [256][8][2]
    prev: u8,
}

impl Order1 {
    fn new() -> Self {
        Self {
            counts: vec![[[1u32; 2]; 8]; 256], // uniform prior
            prev: 0,
        }
    }

    fn bit_prob(&self, bit_idx: u8) -> f64 {
        let c = &self.counts[self.prev as usize][bit_idx as usize];
        c[1] as f64 / (c[0] + c[1]) as f64
    }

    fn update(&mut self, byte: u8) {
        for bit_idx in 0..8u8 {
            let bit = (byte >> bit_idx) & 1;
            self.counts[self.prev as usize][bit_idx as usize][bit as usize] += 1;
        }
        self.prev = byte;
    }
}

// ── Match model ───────────────────────────────────────────────────────────────

use super::match_model::MatchModel;

// ── PPM model ─────────────────────────────────────────────────────────────────

use super::ppm_byte::PpmByteModel;

// ── Mixer ─────────────────────────────────────────────────────────────────────

const LR: f64 = 0.01; // learning rate
const STRETCH_MAX: f64 = 4.0;

#[inline] fn stretch(p: f64) -> f64 { let p=p.clamp(1e-6,1.0-1e-6); (p/(1.0-p)).ln().clamp(-STRETCH_MAX, STRETCH_MAX) }
#[inline] fn squash(x: f64) -> f64 { 1.0/(1.0+(-x).exp()) }

struct Mixer {
    // Context-indexed weights: w[prev_byte][model_idx]
    w: Vec<[f64; N_MODELS]>,
    last_probs: [f64; N_MODELS],
    last_pred: f64,
    last_ctx: u8,
}

impl Mixer {
    fn new() -> Self {
        let init = 1.0 / N_MODELS as f64;
        Self {
            w: vec![[init; N_MODELS]; 256],
            last_probs: [init; N_MODELS],
            last_pred: 0.5,
            last_ctx: 0,
        }
    }

    fn predict(&mut self, probs: [f64; N_MODELS], ctx: u8) -> f64 {
        let w = &self.w[ctx as usize];
        let sum: f64 = (0..N_MODELS).map(|i| w[i] * stretch(probs[i])).sum();
        let pred = squash(sum);
        self.last_probs = probs;
        self.last_pred = pred;
        self.last_ctx = ctx;
        pred
    }

    fn update(&mut self, actual_bit: u8) {
        let err = actual_bit as f64 - self.last_pred;
        let w = &mut self.w[self.last_ctx as usize];
        for i in 0..N_MODELS {
            w[i] += LR * err * stretch(self.last_probs[i]);
        }
    }
}

// ── Compressor state ──────────────────────────────────────────────────────────

struct State {
    ppm: PpmByteModel,
    order1: Order1,
    matcher: MatchModel,
    mixer: Mixer,
    ctx: Vec<u8>,
}

impl State {
    fn new() -> Self {
        Self {
            ppm: PpmByteModel::new(),
            order1: Order1::new(),
            matcher: MatchModel::new(),
            mixer: Mixer::new(),
            ctx: Vec::with_capacity(64),
        }
    }

    /// Get blended P(bit=1) for bit at position bit_idx (7=MSB, 0=LSB)
    fn bit_prob(&mut self, bit_idx: u8) -> f64 {
        let (match_byte, match_conf) = self.matcher.predict(&self.ctx);
        let probs = [
            self.ppm.bit_prob(bit_idx),
            self.order1.bit_prob(bit_idx),
            MatchModel::bit_prob(match_byte, match_conf, bit_idx),
        ];
        let ctx = self.ctx.last().cloned().unwrap_or(0);
        self.mixer.predict(probs, ctx)
    }

    fn update_after_bit(&mut self, actual_bit: u8) {
        self.mixer.update(actual_bit);
    }

    fn update_after_byte(&mut self, byte: u8) {
        self.ppm.update(byte);
        self.order1.update(byte);
        self.matcher.update(byte);
        self.ctx.push(byte);
        if self.ctx.len() > 64 { self.ctx.remove(0); }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Compress data using cmix-style model blending.
/// Operates at the bit level — maximum granularity.
pub fn compress(data: &[u8], _path: &str) -> Vec<u8> {
    if data.is_empty() { return vec![]; }

    let mut state = State::new();
    let mut w = BitWriter::new();
    let mut lo = 0u64; let mut hi = TOP;

    for &byte in data {
        for bit_idx in (0..8u8).rev() {
            let actual_bit = (byte >> bit_idx) & 1;
            let p1 = state.bit_prob(bit_idx);

            // Encode bit: bit=1 → [0, p1*SCALE), bit=0 → [p1*SCALE, SCALE)
            let p1s = (p1 * SCALE as f64).round() as u32;
            let p1s = p1s.clamp(1, SCALE - 1);

            let (sl, sh) = if actual_bit == 1 { (0, p1s) } else { (p1s, SCALE) };
            let range = hi - lo;
            let new_hi = lo + range * sh as u64 / SCALE as u64;
            let new_lo = lo + range * sl as u64 / SCALE as u64;
            hi = if new_hi > new_lo { new_hi } else { new_lo + 1 };
            lo = new_lo;
            renorm_enc(&mut lo, &mut hi, &mut w);
            state.update_after_bit(actual_bit);
        }
        state.update_after_byte(byte);
    }

    w.pending += 1;
    if lo < QTR { w.emit_pending(0); } else { w.emit_pending(1); }
    w.done()
}

/// Decompress data using cmix-style model blending.
pub fn decompress(data: &[u8], original_len: usize, _path: &str) -> Vec<u8> {
    if data.is_empty() || original_len == 0 { return vec![]; }

    let mut state = State::new();
    let mut r = BitReader::new(data);
    let mut lo = 0u64; let mut hi = TOP; let mut val = 0u64;
    for _ in 0..32 { val = (val<<1)|r.read(); }

    let mut out = Vec::with_capacity(original_len);

    for _ in 0..original_len {
        let mut byte = 0u8;
        for bit_idx in (0..8u8).rev() {
            let p1 = state.bit_prob(bit_idx);
            let p1s = (p1 * SCALE as f64).round() as u32;
            let p1s = p1s.clamp(1, SCALE - 1);

            let range = hi - lo;
            let scaled = ((val - lo + 1) * SCALE as u64 - 1) / range;
            let bit = if scaled < p1s as u64 { 1u8 } else { 0u8 };

            let (sl, sh) = if bit == 1 { (0, p1s) } else { (p1s, SCALE) };
            let new_hi = lo + range * sh as u64 / SCALE as u64;
            let new_lo = lo + range * sl as u64 / SCALE as u64;
            hi = if new_hi > new_lo { new_hi } else { new_lo + 1 };
            lo = new_lo;
            renorm_dec(&mut lo, &mut hi, &mut val, &mut r);
            state.update_after_bit(bit);
            byte = (byte << 1) | bit;
        }
        state.update_after_byte(byte);
        out.push(byte);
    }

    out
}
