//! GrammarModel — the top-level model integrating grammar + frequency + arithmetic.
//!
//! This is what RNCE calls instead of PpmModel.
//!
//! For source code files: uses grammar-integrated model.
//! For binary/text: falls back to PpmModel (byte-level).
//!
//! Encode flow:
//!   1. Lex next token from raw bytes
//!   2. Get valid next token set from grammar
//!   3. Build valid_mask from token kinds
//!   4. Get frequency distribution restricted to valid tokens
//!   5. Encode token KIND with arithmetic coder
//!   6. Encode token CONTENT (identifier name, literal value, etc.)
//!      using a secondary byte-level model within that token class
//!
//! Decode flow:
//!   1. Get valid set from grammar
//!   2. Decode token KIND
//!   3. Decode token CONTENT from secondary model
//!   4. Reconstruct raw bytes
//!   5. Update grammar state

use super::token::{Token, TokenKind};
use super::grammar::{LanguageGrammar, ParserStack};
use super::freq::{FrequencyModel, FreqContext};
use super::lang::detect_language;

/// The grammar-integrated compression model
pub struct GrammarModel {
    /// Language-specific grammar rules
    lang: &'static dyn LanguageGrammar,
    /// Parser state stack
    stack: ParserStack,
    /// Token-level frequency model
    freq: FrequencyModel,
    /// Secondary byte model — within each token class,
    /// what are the actual bytes? (identifier names, string content, etc.)
    byte_models: TokenByteModels,
    /// Previous token kind (for frequency context)
    prev_token: TokenKind,
    /// Total tokens encoded so far
    token_count: u64,
}

impl GrammarModel {
    pub fn new(path: &str) -> Self {
        Self {
            lang: detect_language(path),
            stack: ParserStack::new(),
            freq: FrequencyModel::new(),
            byte_models: TokenByteModels::new(),
            prev_token: TokenKind::Unknown,
            token_count: 0,
        }
    }

    /// Get the valid token mask for the current state
    pub fn valid_mask(&self) -> [bool; 80] {
        let valid = self.stack.valid_next(self.lang);
        let mut mask = [false; 80];
        if valid.unconstrained {
            mask.fill(true);
        } else {
            for kind in &valid.kinds {
                let idx = *kind as usize;
                if idx < 64 { mask[idx] = true; }
            }
        }
        mask
    }

    /// Get frequency context for current state
    pub fn freq_ctx(&self) -> FreqContext {
        FreqContext::new(
            self.stack.current(),
            self.prev_token,
            self.stack.depth(),
        )
    }

    /// Update model after encoding/decoding a token
    pub fn update(&mut self, token: &Token) {
        let ctx = self.freq_ctx();
        self.freq.update(ctx, token.kind);
        self.byte_models.update(token.kind, &token.raw);
        self.stack.advance(token.kind, &token.raw, self.lang);
        self.prev_token = token.kind;
        self.token_count += 1;
    }

    /// Get token-kind probability for arithmetic coding
    /// Returns (low, high, total) in token-kind space
    pub fn token_prob(&self, kind: TokenKind) -> (u32, u32, u32) {
        let mask = self.valid_mask();
        let ctx = self.freq_ctx();
        self.freq.prob(ctx, kind, &mask)
    }

    /// Get full token distribution for decoding
    pub fn token_distribution(&self) -> ([u32; 80], u32) {
        let mask = self.valid_mask();
        let ctx = self.freq_ctx();
        self.freq.distribution(ctx, &mask)
    }

    /// Get byte-level model for a token class (for encoding token content)
    pub fn byte_model(&self, kind: TokenKind) -> &ByteModel {
        self.byte_models.get(kind)
    }

    pub fn byte_model_mut(&mut self, kind: TokenKind) -> &mut ByteModel {
        self.byte_models.get_mut(kind)
    }
}

// ── Byte model for token content ──────────────────────────────────────────────

/// Global byte frequency model per token class.
/// Simple and correct: no context, just global frequencies.
/// The context-aware model can be added once basic correctness is verified.
pub struct ByteModel {
    global: [u32; 256],
    global_total: u32,
}

impl ByteModel {
    pub fn new() -> Self {
        Self {
            global: [1u32; 256], // uniform prior
            global_total: 256,
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.global[b as usize] += 1;
            self.global_total += 1;
        }
    }

    /// Get (low, high, total) for byte `b`
    pub fn byte_prob(&self, b: u8, _prev2: Option<u8>, _prev1: Option<u8>) -> (u32, u32, u32) {
        let low: u32 = self.global[..b as usize].iter().sum();
        let high = low + self.global[b as usize];
        (low, high, self.global_total)
    }

    /// Get full byte distribution for decoding
    pub fn byte_distribution(&self, _prev2: Option<u8>, _prev1: Option<u8>) -> ([u32; 256], u32) {
        (self.global, self.global_total)
    }
}

/// One ByteModel per token kind
struct TokenByteModels {
    models: Vec<ByteModel>,
}

impl TokenByteModels {
    fn new() -> Self {
        Self { models: (0..80).map(|_| ByteModel::new()).collect() }
    }

    fn get(&self, kind: TokenKind) -> &ByteModel {
        let idx = (kind as usize).min(79);
        &self.models[idx]
    }

    fn get_mut(&mut self, kind: TokenKind) -> &mut ByteModel {
        let idx = (kind as usize).min(79);
        &mut self.models[idx]
    }

    fn update(&mut self, kind: TokenKind, raw: &[u8]) {
        self.get_mut(kind).update(raw);
    }
}
