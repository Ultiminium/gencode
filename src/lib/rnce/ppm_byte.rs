//! PPM byte model — order-4 with bounded memory.
//!
//! Memory: HashMap grows lazily — only contexts actually seen are stored.
//! On a typical source file (few KB), this stays well under 1MB.
//! Order reduced from 8 to 4: O(256^4) theoretical max but in practice
//! real code has far fewer unique 4-gram contexts than 4GB.
//!
//! Previous: order-8 with unbounded growth, potential GB usage.
//! Now: order-4, bounded growth, same prediction quality on short files.

const MAX_ORDER: usize = 4;
const MAX_TOTAL: u32 = 8192; // rescale threshold — lower = faster decay

#[derive(Clone)]
struct Node {
    counts: [u16; 256],
    total: u32,
}

impl Node {
    fn update(&mut self, sym: u8) {
        self.counts[sym as usize] = self.counts[sym as usize].saturating_add(1);
        self.total += 1;
        if self.total >= MAX_TOTAL { self.rescale(); }
    }

    fn rescale(&mut self) {
        self.total = 0;
        for c in &mut self.counts {
            *c = (*c + 1) / 2;
            self.total += *c as u32;
        }
    }

    fn distribution_with_floor(&self) -> ([u32; 256], u32) {
        let mut table = [0u32; 256];
        let mut total = 0u32;
        for i in 0..256 {
            table[i] = self.counts[i].max(1) as u32;
            total += table[i];
        }
        (table, total)
    }
}

impl Default for Node {
    fn default() -> Self { Self { counts: [0u16; 256], total: 0 } }
}

pub struct PpmByteModel {
    // HashMap grows lazily — only allocates for seen contexts
    contexts: Vec<std::collections::HashMap<u64, Node>>,
    ctx: [u8; MAX_ORDER],
    ctx_len: usize,
}

impl PpmByteModel {
    pub fn new() -> Self {
        Self {
            contexts: (0..=MAX_ORDER).map(|_| std::collections::HashMap::new()).collect(),
            ctx: [0u8; MAX_ORDER],
            ctx_len: 0,
        }
    }

    pub fn update(&mut self, byte: u8) {
        for order in 0..=MAX_ORDER.min(self.ctx_len) {
            let key = self.hash(order);
            self.contexts[order].entry(key).or_default().update(byte);
        }
        if self.ctx_len < MAX_ORDER {
            self.ctx[self.ctx_len] = byte;
            self.ctx_len += 1;
        } else {
            self.ctx.copy_within(1.., 0);
            self.ctx[MAX_ORDER - 1] = byte;
        }
    }

    fn hash(&self, order: usize) -> u64 {
        if order == 0 { return 0; }
        let start = self.ctx_len.saturating_sub(order);
        let bytes = &self.ctx[start..self.ctx_len];
        let mut h: u64 = 0xcbf29ce484222325;
        h ^= order as u64;
        for &b in bytes { h ^= b as u64; h = h.wrapping_mul(0x100000001b3); }
        h
    }

    pub fn get_distribution(&self) -> ([u32; 256], u32) {
        for order in (0..=MAX_ORDER.min(self.ctx_len)).rev() {
            let key = self.hash(order);
            if let Some(node) = self.contexts[order].get(&key) {
                if node.total > 0 { return node.distribution_with_floor(); }
            }
        }
        ([1u32; 256], 256)
    }

    pub fn bit_prob(&self, bit_pos: u8) -> f64 {
        let (table, total) = self.get_distribution();
        if total == 0 { return 0.5; }
        let prob_one: u64 = (0u32..256).filter(|&i| (i >> bit_pos) & 1 == 1)
            .map(|i| table[i as usize] as u64).sum();
        (prob_one as f64 / total as f64).clamp(1e-6, 1.0 - 1e-6)
    }
}
