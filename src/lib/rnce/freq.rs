//! Frequency model — within the valid token set, learned distribution.
//!
//! Two layers:
//!   1. Grammar constrains WHICH tokens are valid (zero prob to invalid)
//!   2. Frequency model says HOW LIKELY each valid token is
//!
//! The frequency model is a context-sensitive count table.
//! Context: (current ParseState, previous TokenKind, depth)
//! This is much richer than PPM byte context because:
//!   - States encode semantic meaning, not just recent bytes
//!   - Token-level context compresses more history into less state
//!
//! Pre-training: these tables can be populated from real code corpora
//! and baked into the binary as initial counts.

use super::token::TokenKind;
use super::grammar::ParseState;
use std::collections::HashMap;

const RESCALE_AT: u32 = 32768;

/// A count table for one context
#[derive(Clone)]
pub struct CountTable {
    pub counts: [u32; 80], // indexed by TokenKind as u8, max 64 kinds
    pub total: u32,
}

impl CountTable {
    pub fn update(&mut self, kind: TokenKind) {
        let idx = kind as usize;
        if idx < 64 {
            self.counts[idx] += 1;
            self.total += 1;
            if self.total >= RESCALE_AT { self.rescale(); }
        }
    }

    pub fn rescale(&mut self) {
        self.total = 0;
        for c in &mut self.counts {
            *c = (*c + 1) / 2;
            self.total += *c;
        }
    }

    /// Probability of kind, given valid_mask restricts which tokens are valid.
    /// Returns (low, high, total) for arithmetic coding.
    pub fn prob(&self, kind: TokenKind, _valid_mask: &[bool; 80]) -> (u32, u32, u32) {
        let idx = kind as usize;
        // Must match distribution() exactly: floor=1 for all 80 slots
        let mut low = 0u32;
        let mut total = 0u32;
        for i in 0..80 {
            let c = self.counts[i].max(1);
            if i == idx { low = total; }
            total += c;
        }
        let high = low + self.counts[idx].max(1);
        (low, high, total)
    }

    /// Get the full distribution over all 64 token kinds.
    /// Every slot gets floor=1 so the cumulative array is strictly increasing.
    /// This guarantees binary search finds the correct slot during decode.
    pub fn distribution(&self, _valid_mask: &[bool; 80]) -> ([u32; 80], u32) {
        let mut table = [0u32; 80];
        let mut total = 0u32;
        for i in 0..80 {
            table[i] = self.counts[i].max(1); // floor=1 for ALL slots
            total += table[i];
        }
        (table, total)
    }
}

/// Context key for the frequency model
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FreqContext {
    pub state: ParseState,
    pub prev_token: TokenKind,
    pub depth: u8, // nesting depth, clamped to 0-15
}

impl FreqContext {
    pub fn new(state: ParseState, prev: TokenKind, depth: usize) -> Self {
        Self { state, prev_token: prev, depth: depth.min(15) as u8 }
    }

    /// Lower-order context (drops depth)
    pub fn without_depth(self) -> Self { Self { depth: 0, ..self } }

    /// Lower-order context (drops prev_token)
    pub fn without_prev(self) -> Self {
        Self { prev_token: TokenKind::Unknown, depth: 0, ..self }
    }
}

/// The full frequency model — three levels of context
pub struct FrequencyModel {
    /// Full context: (state, prev_token, depth)
    full: HashMap<FreqContext, CountTable>,
    /// Medium context: (state, prev_token)
    medium: HashMap<FreqContext, CountTable>,
    /// Low context: (state,)
    low: HashMap<ParseState, CountTable>,
    /// Global fallback
    global: CountTable,
}

impl FrequencyModel {
    pub fn new() -> Self {
        Self {
            full: HashMap::new(),
            medium: HashMap::new(),
            low: HashMap::new(),
            global: CountTable::default(),
        }
    }

    pub fn update(&mut self, ctx: FreqContext, kind: TokenKind) {
        self.full.entry(ctx).or_default().update(kind);
        self.medium.entry(ctx.without_depth()).or_default().update(kind);
        self.low.entry(ctx.state).or_default().update(kind);
        self.global.update(kind);
    }

    /// Get (low, high, total) for arithmetic coding.
    /// Uses highest-order matching context, falls back to lower orders.
    pub fn prob(&self, ctx: FreqContext, kind: TokenKind, valid_mask: &[bool; 80]) -> (u32, u32, u32) {
        // Try full context first
        if let Some(table) = self.full.get(&ctx) {
            if table.total > 0 {
                return table.prob(kind, valid_mask);
            }
        }
        // Medium context
        if let Some(table) = self.medium.get(&ctx.without_depth()) {
            if table.total > 0 {
                return table.prob(kind, valid_mask);
            }
        }
        // Low context
        if let Some(table) = self.low.get(&ctx.state) {
            if table.total > 0 {
                return table.prob(kind, valid_mask);
            }
        }
        // Global
        self.global.prob(kind, valid_mask)
    }

    /// Get full distribution for decoding
    pub fn distribution(&self, ctx: FreqContext, valid_mask: &[bool; 80]) -> ([u32; 80], u32) {
        if let Some(table) = self.full.get(&ctx) {
            if table.total > 0 { return table.distribution(valid_mask); }
        }
        if let Some(table) = self.medium.get(&ctx.without_depth()) {
            if table.total > 0 { return table.distribution(valid_mask); }
        }
        if let Some(table) = self.low.get(&ctx.state) {
            if table.total > 0 { return table.distribution(valid_mask); }
        }
        self.global.distribution(valid_mask)
    }
}

impl Default for FrequencyModel {
    fn default() -> Self { Self::new() }
}

impl Default for CountTable {
    fn default() -> Self { Self { counts: [0u32; 80], total: 0 } }
}
