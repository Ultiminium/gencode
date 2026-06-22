//! G index stream compression — RNCE as the entropy layer for G encoding.
//!
//! G produces a stream of block indices. Each index is a fixed number of bits
//! (bits(n) for a Gn instance). This module compresses that raw bit stream
//! using RNCE's cmix model — treating the index bytes as the data to compress.
//!
//! Why cmix works well here:
//!   - G indices are skewed toward low values (constrained blocks dominate)
//!   - The index distribution is learned online by cmix's PPM + order-1 + match models
//!   - Cross-block context (adjacency constraints) creates local patterns cmix exploits
//!
//! The "grammar" of G indices:
//!   - Indices are not fully independent — the last character of one block
//!     constrains valid first characters of the next, which constrains valid indices
//!   - cmix's context window captures this implicitly through byte-level patterns
//!
//! Format (what this module stores):
//!   [1 byte: Gn value (n)]
//!   [8 bytes: original_bits (u64 LE) — exact input bits encoded]
//!   [8 bytes: block_count (u64 LE)]
//!   [cmix-compressed index stream]

use super::cmix::{CmixConfig, compress_bytes, decompress_bytes};

const HEADER_SIZE: usize = 1 + 8 + 8; // n + original_bits + block_count

/// Compress a raw G index bit stream using RNCE's cmix model.
///
/// `index_bytes` — the raw bit-packed index stream from G encoding
/// `n` — the Gn instance (block size / state count)
/// `original_bits` — exact number of input bits encoded (for precise truncation on decode)
/// `block_count` — number of G blocks encoded
pub fn compress_index_stream(
    index_bytes: &[u8],
    n: u8,
    original_bits: u64,
    block_count: u64,
) -> Vec<u8> {
    // G index streams have structure cmix exploits well:
    // - Index bytes cluster around low values (constrained blocks)
    // - Repetitive patterns in structured data show as match model hits
    // - PPM order-4 captures multi-block correlations
    let compressed = compress_bytes(index_bytes, CmixConfig::source());

    let mut out = Vec::with_capacity(HEADER_SIZE + compressed.len());
    out.push(n);
    out.extend_from_slice(&original_bits.to_le_bytes());
    out.extend_from_slice(&block_count.to_le_bytes());
    out.extend_from_slice(&compressed);
    out
}

/// Decompress a RNCE-compressed G index stream.
///
/// Returns `(index_bytes, n, original_bits, block_count)`.
pub fn decompress_index_stream(data: &[u8]) -> Option<(Vec<u8>, u8, u64, u64)> {
    if data.len() < HEADER_SIZE { return None; }

    let n = data[0];
    let original_bits = u64::from_le_bytes(data[1..9].try_into().ok()?);
    let block_count = u64::from_le_bytes(data[9..17].try_into().ok()?);
    let compressed = &data[HEADER_SIZE..];

    // Decompress — original size is the index stream length
    // The index stream encodes block_count * bits(n) bits, packed into bytes
    let bits_per_block = bits_needed_for_n(n);
    let total_bits = block_count * bits_per_block as u64;
    let index_bytes_len = (total_bits + 7) / 8;

    let index_bytes = decompress_bytes(compressed, index_bytes_len as usize, CmixConfig::source());
    Some((index_bytes, n, original_bits, block_count))
}

/// Approximate bits(n) for a Gn instance.
/// Matches the calculation in G's IndexTable::bits field.
/// For the full calculation G needs to be called, but we store bits_per_block
/// in the header so this is only used as a fallback.
fn bits_needed_for_n(n: u8) -> u32 {
    // Conservative estimate — actual value stored separately in G's frame header
    // This is used only to compute decompressed size for cmix
    match n {
        4  => 14,
        8  => 35,
        16 => 82,
        28 => 128,
        32 => 148,
        _  => (n as u32) * 5, // rough estimate
    }
}

/// Compress a G index stream with explicit bits_per_block known.
/// Use this when you have the exact value from G's IndexTable.
pub fn compress_index_stream_exact(
    index_bytes: &[u8],
    n: u8,
    original_bits: u64,
    block_count: u64,
    bits_per_block: u32,
) -> Vec<u8> {
    let compressed = compress_bytes(index_bytes, CmixConfig::source());
    let index_bytes_len = index_bytes.len() as u64;

    // Header: n(1) + original_bits(8) + block_count(8) + bits_per_block(4) + index_bytes_len(8)
    let mut out = Vec::with_capacity(HEADER_SIZE + 4 + 8 + compressed.len());
    out.push(n);
    out.extend_from_slice(&original_bits.to_le_bytes());
    out.extend_from_slice(&block_count.to_le_bytes());
    out.extend_from_slice(&bits_per_block.to_le_bytes());
    out.extend_from_slice(&index_bytes_len.to_le_bytes()); // actual byte count
    out.extend_from_slice(&compressed);
    out
}

/// Decompress with exact bits_per_block stored in header.
pub fn decompress_index_stream_exact(data: &[u8]) -> Option<(Vec<u8>, u8, u64, u64, u32)> {
    if data.len() < HEADER_SIZE + 4 + 8 { return None; }

    let n = data[0];
    let original_bits = u64::from_le_bytes(data[1..9].try_into().ok()?);
    let block_count = u64::from_le_bytes(data[9..17].try_into().ok()?);
    let bits_per_block = u32::from_le_bytes(data[17..21].try_into().ok()?);
    let index_bytes_len = u64::from_le_bytes(data[21..29].try_into().ok()?) as usize;
    let compressed = &data[29..];

    let index_bytes = decompress_bytes(compressed, index_bytes_len, CmixConfig::source());
    Some((index_bytes, n, original_bits, block_count, bits_per_block))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_small() {
        // Simulate a small G4 index stream
        let data: Vec<u8> = (0..64).map(|i| (i * 7 % 256) as u8).collect();
        let compressed = compress_index_stream_exact(&data, 4, 512, 36, 14);
        let (restored, n, ob, bc, bpb) = decompress_index_stream_exact(&compressed).unwrap();
        assert_eq!(n, 4);
        assert_eq!(ob, 512);
        assert_eq!(bc, 36);
        assert_eq!(bpb, 14);
        assert_eq!(&restored[..data.len()], &data[..]);
    }

    #[test]
    fn roundtrip_g28_sized() {
        // Simulate a G28 index stream for 1KB of data
        // 1024 bytes = 8192 bits, 128 bits/block = 64 blocks, 1024 index bytes
        let data: Vec<u8> = (0..1024).map(|i| (i * 13 % 256) as u8).collect();
        let compressed = compress_index_stream_exact(&data, 28, 8192, 64, 128);
        let (restored, n, ob, bc, bpb) = decompress_index_stream_exact(&compressed).unwrap();
        assert_eq!(n, 28);
        assert_eq!(bc, 64);
        assert_eq!(bpb, 128);
        assert_eq!(&restored[..data.len()], &data[..]);
        println!("G28 1KB: index_stream={} compressed={} ({:.1}%)",
            data.len(), compressed.len(),
            100.0 * compressed.len() as f64 / data.len() as f64);
    }

    #[test]
    fn compresses_skewed_indices() {
        // Low indices (constrained blocks) should compress better than uniform
        let uniform: Vec<u8> = (0..512).map(|i| (i % 256) as u8).collect();
        let skewed: Vec<u8> = (0..512).map(|i| (i % 16) as u8).collect(); // low indices only

        let c_uniform = compress_index_stream_exact(&uniform, 28, 4096, 32, 128);
        let c_skewed = compress_index_stream_exact(&skewed, 28, 4096, 32, 128);

        println!("uniform: {} -> {} ({:.1}%)",
            uniform.len(), c_uniform.len(),
            100.0 * c_uniform.len() as f64 / uniform.len() as f64);
        println!("skewed:  {} -> {} ({:.1}%)",
            skewed.len(), c_skewed.len(),
            100.0 * c_skewed.len() as f64 / skewed.len() as f64);

        assert!(c_skewed.len() < c_uniform.len(),
            "skewed indices should compress better: {} vs {}", c_skewed.len(), c_uniform.len());
    }
}

    #[test]
    fn exact_lossless_on_real_g_output() {
        // Test that RNCE roundtrip is byte-exact on actual G index stream data
        // Simulate what G produces for "hello world this is a test of G + RNCE\n"
        // 39 bytes -> 3 G28 blocks -> 48 bytes of index stream
        // Use a realistic byte pattern
        let data: Vec<u8> = vec![
            0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0,
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
            0x47, 0x20, 0x2b, 0x20, 0x52, 0x4e, 0x43, 0x45,
            0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89,
            0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80,
        ];
        let n = 28u8;
        let original_bits = 312u64; // 39 bytes * 8
        let block_count = 3u64;
        let bits_per_block = 128u32;

        let compressed = compress_index_stream_exact(&data, n, original_bits, block_count, bits_per_block);
        let (restored, _, _, _, _) = decompress_index_stream_exact(&compressed).unwrap();

        assert_eq!(restored.len(), data.len(), "length mismatch");
        assert_eq!(restored, data, "data mismatch at bytes: {:?}",
            restored.iter().zip(data.iter()).enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn lossless_on_random_data() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        // Generate pseudo-random bytes
        let data: Vec<u8> = (0..1024u32).map(|i| {
            let mut h = DefaultHasher::new();
            i.hash(&mut h);
            (h.finish() & 0xFF) as u8
        }).collect();
        
        let compressed = compress_index_stream_exact(&data, 28, 8192, 64, 128);
        let (restored, _, _, _, _) = decompress_index_stream_exact(&compressed).unwrap();
        
        assert_eq!(restored.len(), data.len());
        let diffs: Vec<usize> = restored.iter().zip(data.iter()).enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| i)
            .collect();
        assert!(diffs.is_empty(), "diffs at indices: {:?}", &diffs[..diffs.len().min(10)]);
    }

    #[test]
    fn lossless_on_many_random_inputs() {
        use super::cmix::{compress_bytes, decompress_bytes, CmixConfig};
        // Use a simple LCG for repeatable pseudo-random
        let mut seed = 0xdeadbeef_u64;
        let mut next = || { seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (seed >> 33) as u8 };
        
        for trial in 0..100 {
            let len = 16 + (trial * 7 % 200);
            let data: Vec<u8> = (0..len).map(|_| next()).collect();
            let compressed = compress_bytes(&data, CmixConfig::source());
            let restored = decompress_bytes(&compressed, len, CmixConfig::source());
            assert_eq!(restored, data, "trial {} len={} failed", trial, len);
        }
    }
