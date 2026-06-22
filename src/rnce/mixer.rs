//! Prediction Mixer — cmix-style logistic regression blending.
//!
//! Each model outputs a probability p ∈ (0,1) for the next symbol.
//! The mixer combines them via logistic regression:
//!   stretch(p) = ln(p / (1-p))   [logit transform]
//!   blended = sigmoid(Σ w_i * stretch(p_i))
//!
//! Weights update after each symbol using gradient descent:
//!   error = actual_bit - prediction
//!   w_i += learning_rate * error * stretch(p_i)
//!
//! This is exactly what cmix/PAQ do. The mixer learns which models
//! are reliable in which contexts, online, without any offline training.
//!
//! We run at the BYTE level — 8 predictions per byte (one per bit).
//! This gives maximum granularity and matches arithmetic coding naturally.

const N_MODELS: usize = 4;
const LEARNING_RATE: f64 = 0.005;
const STRETCH_CLAMP: f64 = 5.0; // clamp logit to [-5, 5] for stability

/// Logit (stretch) transform: maps [0,1] → (-∞, +∞)
#[inline]
fn stretch(p: f64) -> f64 {
    let p = p.clamp(1e-6, 1.0 - 1e-6);
    (p / (1.0 - p)).ln().clamp(-STRETCH_CLAMP, STRETCH_CLAMP)
}

/// Sigmoid (squash) transform: maps (-∞, +∞) → (0,1)
#[inline]
fn squash(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// The prediction mixer
pub struct Mixer {
    weights: [f64; N_MODELS],
    /// Context hash for weight indexing (different weights per context)
    /// We use 256 contexts (last byte) for now
    ctx_weights: Vec<[f64; N_MODELS]>,
    last_inputs: [f64; N_MODELS],
    last_prediction: f64,
}

impl Mixer {
    pub fn new() -> Self {
        // Initialize weights equally — all models start equal
        let equal = 1.0 / N_MODELS as f64;
        Self {
            weights: [equal; N_MODELS],
            ctx_weights: vec![[equal; N_MODELS]; 256],
            last_inputs: [0.0; N_MODELS],
            last_prediction: 0.5,
        }
    }

    /// Blend N model predictions into one.
    /// probs[i] = probability that next bit is 1, from model i.
    /// ctx = current context byte (for context-sensitive weights)
    pub fn predict(&mut self, probs: [f64; N_MODELS], ctx: u8) -> f64 {
        let w = &self.ctx_weights[ctx as usize];
        let mut sum = 0.0;
        for i in 0..N_MODELS {
            let stretched = stretch(probs[i]);
            sum += w[i] * stretched;
        }
        let blended = squash(sum);
        self.last_inputs = probs;
        self.last_prediction = blended;
        blended
    }

    /// Update weights based on what actually happened.
    /// actual = 1 if bit was 1, 0 if bit was 0.
    pub fn update(&mut self, actual: u8, ctx: u8) {
        let error = actual as f64 - self.last_prediction;
        let w = &mut self.ctx_weights[ctx as usize];
        for i in 0..N_MODELS {
            let stretched = stretch(self.last_inputs[i]);
            w[i] += LEARNING_RATE * error * stretched;
        }
    }

    /// Convert blended bit-probability to byte probability range.
    /// Returns (low, high, total) for arithmetic coder.
    /// For a byte b, we need: P(byte = b) = Π P(bit_j = b_j | b_0..b_{j-1})
    /// This requires 8 sequential bit predictions.
    pub fn byte_prob(&mut self, byte: u8, bit_probs: &mut dyn FnMut(u8) -> [f64; N_MODELS], ctx: u8) -> f64 {
        let mut prob = 1.0f64;
        for bit_idx in (0..8).rev() {
            let bit = (byte >> bit_idx) & 1;
            let probs = bit_probs(bit_idx);
            let p1 = self.predict(probs, ctx);
            prob *= if bit == 1 { p1 } else { 1.0 - p1 };
        }
        prob
    }
}

/// Convert a byte-level probability (0..1) to arithmetic range (low, high, total).
/// Uses a precision of 2^16 to avoid overflow while maintaining precision.
#[inline]
pub fn prob_to_range(p: f64, total: u32) -> (u32, u32) {
    let scaled = (p * total as f64).round() as u32;
    let scaled = scaled.clamp(1, total - 1);
    (0, scaled) // low=0, high=scaled means P(this symbol) = scaled/total
}
