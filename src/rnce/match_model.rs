//! Match model — LZ-style prediction via rolling hash.
//!
//! Memory budget: 64KB window + 256KB hash table = ~320KB per instance.
//! Previous: 4MB + 4MB = 8MB. This was crashing terminals.
//!
//! Trade-off: shorter match window means less compression on large repetitive
//! corpora, but the model still helps enormously on repeated patterns within
//! the last 64KB — which covers the vast majority of real code patterns
//! (function signatures, import blocks, repeated JSON keys, etc.)

const WINDOW: usize = 1 << 16;    // 64KB rolling window (was 4MB)
const HASH_BITS: usize = 16;      // 64K hash slots (was 1M)
const HASH_SIZE: usize = 1 << HASH_BITS;
#[allow(dead_code)]
const HASH_MASK: u32 = (HASH_SIZE - 1) as u32;
const MIN_MATCH: usize = 3;       // minimum match length
const MAX_MATCH: usize = 32;      // max match length to search

pub struct MatchModel {
    buf: Box<[u8; WINDOW]>,
    buf_pos: u16,                  // u16 because WINDOW = 64K fits in u16
    buf_filled: u32,
    table: Box<[u32; HASH_SIZE]>,  // stores positions as u32 (WINDOW fits in u16 but u32 for sentinel)
    ctx_hash: u32,
}

impl MatchModel {
    pub fn new() -> Self {
        Self {
            buf: Box::new([0u8; WINDOW]),
            buf_pos: 0,
            buf_filled: 0,
            table: Box::new([u32::MAX; HASH_SIZE]),
            ctx_hash: 0,
        }
    }

    pub fn update(&mut self, byte: u8) {
        // Store byte in ring buffer
        let pos = self.buf_pos as usize;
        self.buf[pos] = byte;
        self.buf_pos = self.buf_pos.wrapping_add(1); // wraps at 64K naturally for u16
        if self.buf_filled < WINDOW as u32 { self.buf_filled += 1; }

        // Update rolling hash
        self.ctx_hash = self.ctx_hash.wrapping_mul(0x08104225).wrapping_add(byte as u32);

        // Store position in hash table
        let slot = (self.ctx_hash >> (32 - HASH_BITS)) as usize;
        self.table[slot] = pos as u32;
    }

    pub fn predict(&self, ctx: &[u8]) -> (u8, f64) {
        if ctx.len() < MIN_MATCH || self.buf_filled < MIN_MATCH as u32 + 1 {
            return (0, 0.0);
        }

        // Hash the last MIN_MATCH bytes
        let mut h = 0u32;
        let start = ctx.len().saturating_sub(MIN_MATCH);
        for &b in &ctx[start..] {
            h = h.wrapping_mul(0x08104225).wrapping_add(b as u32);
        }
        let slot = (h >> (32 - HASH_BITS)) as usize;
        let match_start = self.table[slot];
        if match_start == u32::MAX { return (0, 0.0); }

        let match_start = match_start as usize;
        let filled = self.buf_filled as usize;

        // Verify MIN_MATCH bytes
        for i in 0..MIN_MATCH {
            let ctx_byte = ctx[ctx.len() - MIN_MATCH + i];
            let buf_idx = match_start.wrapping_add(i).wrapping_sub(MIN_MATCH - 1) & (WINDOW - 1);
            if buf_idx >= filled { return (0, 0.0); }
            if self.buf[buf_idx] != ctx_byte { return (0, 0.0); }
        }

        // Extend match
        let mut match_len = MIN_MATCH;
        while match_len < MAX_MATCH && match_len < ctx.len() {
            let ctx_byte = ctx[ctx.len() - match_len - 1];
            let buf_idx = match_start.wrapping_sub(match_len) & (WINDOW - 1);
            if buf_idx >= filled { break; }
            if self.buf[buf_idx] != ctx_byte { break; }
            match_len += 1;
        }

        // Predicted next byte
        let next_pos = (match_start + 1) & (WINDOW - 1);
        if next_pos >= filled { return (0, 0.0); }
        let predicted = self.buf[next_pos];

        // Confidence: match_len=3 → 0.4, match_len=8 → 0.7, match_len=16 → 0.86
        let confidence = 1.0 - 1.0 / (1.0 + match_len as f64 / 4.0);
        (predicted, confidence.min(0.97)) // cap at 0.97 to avoid p=1.0
    }

    pub fn bit_prob(predicted: u8, confidence: f64, bit_pos: u8) -> f64 {
        if confidence < 0.01 { return 0.5; }
        let predicted_bit = (predicted >> bit_pos) & 1;
        if predicted_bit == 1 {
            (0.5 + confidence * 0.5).clamp(0.01, 0.99)
        } else {
            (0.5 - confidence * 0.5).clamp(0.01, 0.99)
        }
    }
}
