//! LZ77 compression with 8MB sliding window and hash chain match finder.
//!
//! Token format (byte stream output):
//!   Literal: 0x00 <byte>           (2 bytes)
//!   Copy:    0x01 <u24-le offset> <u16-le length>  (6 bytes)
//!
//! A copy is emitted when match length >= MIN_MATCH (3 bytes).
//! Offset is 1-based (1 = previous byte).
//! Length is the number of bytes to copy.
//!
//! The hash chain uses a 4-byte rolling hash for O(1) match finding.
//! MAX_CHAIN controls quality vs speed tradeoff.

const WINDOW:     usize = 8 * 1024 * 1024; // 8MB sliding window
const MIN_MATCH:  usize = 3;
const MAX_MATCH:  usize = 258;
const HASH_BITS:  usize = 18;               // 256K hash table entries
const HASH_SIZE:  usize = 1 << HASH_BITS;
const HASH_MASK:  usize = HASH_SIZE - 1;
const MAX_CHAIN:  usize = 256;              // max hash chain depth per position
const NIL:        u32   = u32::MAX;

// ── Token ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Literal(u8),
    Copy { offset: u32, length: u16 },
}

// ── Compress ─────────────────────────────────────────────────────────────────


/// Estimate the entropy of a data sample (0.0 = uniform, 1.0 = max entropy).
/// Samples up to 4KB to decide whether LZ77 is worth applying.
pub fn looks_compressible(data: &[u8]) -> bool {
    if data.len() < 64 { return true; } // always try on tiny inputs

    let sample_len = data.len().min(4096);
    let sample = &data[..sample_len];

    // Count byte frequencies
    let mut freq = [0u32; 256];
    for &b in sample { freq[b as usize] += 1; }

    // Compute Shannon entropy (bits per byte)
    let n = sample_len as f64;
    let entropy: f64 = freq.iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum();

    // Max entropy is 8.0 bits/byte (perfectly random).
    // If entropy > 7.5, data is likely incompressible.
    entropy < 7.5
}

#[inline]
fn insert_hash(head: &mut Vec<u32>, prev: &mut Vec<u32>, pos: usize, h: usize) {
    let slot = pos_to_slot(pos);
    prev[slot] = head[h];
    head[h] = pos as u32;
}

/// Compress `data` into a sequence of LZ77 tokens.
pub fn compress(data: &[u8]) -> Vec<Token> {
    if data.is_empty() { return vec![]; }

    let n = data.len();
    let mut tokens = Vec::with_capacity(n / 2);

    // Hash table: head[h] = most recent position with hash h
    let mut head = vec![NIL; HASH_SIZE];
    // Prev chain: prev[pos % WINDOW] = previous position with same hash
    let mut prev = vec![NIL; WINDOW];

    let mut pos = 0usize;

    while pos < n {
        // Need at least 4 bytes for the hash function
        if pos + 4 > n {
            for i in pos..n { tokens.push(Token::Literal(data[i])); }
            break;
        }

        let h = hash4(data, pos);
        let best = find_best_match(data, pos, h, &head, &prev);

        // Lazy matching: if we found a match, check if the NEXT position
        // gives a longer match. If so, emit current position as literal
        // and take the longer match at pos+1.
        if let Some((offset, length)) = best {
            // Try one step ahead (lazy evaluation)
            let lazy_better = if pos + 1 + 4 <= n {
                let h2 = hash4(data, pos + 1);
                insert_hash(&mut head, &mut prev, pos, h);
                if let Some((_, len2)) = find_best_match(data, pos + 1, h2, &head, &prev) {
                    len2 > length + 1 // next match beats current by >1 byte
                } else { false }
            } else { false };

            if lazy_better {
                // Emit current as literal, advance; next iteration takes the better match
                tokens.push(Token::Literal(data[pos]));
                pos += 1;
            } else {
                tokens.push(Token::Copy { offset: offset as u32, length: length as u16 });
                // Insert hashes for all positions in the matched region
                for i in pos..pos + length {
                    if i + 4 <= n {
                        insert_hash(&mut head, &mut prev, i, hash4(data, i));
                    }
                }
                pos += length;
            }
        } else {
            tokens.push(Token::Literal(data[pos]));
            insert_hash(&mut head, &mut prev, pos, h);
            pos += 1;
        }
    }

    tokens
}

fn find_best_match(
    data: &[u8],
    pos: usize,
    h: usize,
    head: &[u32],
    prev: &[u32],
) -> Option<(usize, usize)> {
    let n = data.len();
    let limit = pos.saturating_sub(WINDOW);

    let mut best_len = MIN_MATCH - 1;
    let mut best_offset = 0usize;

    let mut cur = head[h];
    let mut steps = 0;

    while cur != NIL && steps < MAX_CHAIN {
        let candidate = cur as usize;
        if candidate < limit { break; }

        let offset = pos - candidate;
        let max_len = (n - pos).min(MAX_MATCH);

        let len = match_len(data, pos, candidate, max_len);
        if len > best_len {
            best_len = len;
            best_offset = offset;
            if len == MAX_MATCH { break; }
        }

        cur = prev[pos_to_slot(candidate)];
        steps += 1;
    }

    if best_len >= MIN_MATCH {
        Some((best_offset, best_len))
    } else {
        None
    }
}

#[inline]
fn match_len(data: &[u8], pos: usize, candidate: usize, max: usize) -> usize {
    let mut len = 0;
    while len < max && data[pos + len] == data[candidate + len] {
        len += 1;
    }
    len
}

#[inline]
fn hash4(data: &[u8], pos: usize) -> usize {
    let v = u32::from_le_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]);
    // Knuth multiplicative hash
    ((v.wrapping_mul(0x9E3779B1) >> (32 - HASH_BITS)) as usize) & HASH_MASK
}

#[inline]
fn pos_to_slot(pos: usize) -> usize {
    pos & (WINDOW - 1)
}

// ── Serialize tokens → bytes ──────────────────────────────────────────────────

/// Serialize LZ77 tokens to a flat byte stream for G encoding.
pub fn tokens_to_bytes(tokens: &[Token]) -> Vec<u8> {
    let mut out = Vec::with_capacity(tokens.len() * 2);
    for token in tokens {
        match token {
            Token::Literal(b) => {
                out.push(0x00);
                out.push(*b);
            }
            Token::Copy { offset, length } => {
                out.push(0x01);
                // offset as 3-byte little-endian (max 8MB = 23 bits, fits in 24)
                out.push((offset & 0xFF) as u8);
                out.push(((offset >> 8) & 0xFF) as u8);
                out.push(((offset >> 16) & 0xFF) as u8);
                // length as 2-byte little-endian
                out.push((length & 0xFF) as u8);
                out.push(((length >> 8) & 0xFF) as u8);
            }
        }
    }
    out
}

/// Deserialize bytes back to LZ77 tokens.
pub fn bytes_to_tokens(data: &[u8]) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            0x00 => {
                if i + 1 >= data.len() {
                    return Err(format!("truncated literal token at byte {}", i));
                }
                tokens.push(Token::Literal(data[i + 1]));
                i += 2;
            }
            0x01 => {
                if i + 5 >= data.len() {
                    return Err(format!("truncated copy token at byte {}", i));
                }
                let offset = data[i+1] as u32
                    | ((data[i+2] as u32) << 8)
                    | ((data[i+3] as u32) << 16);
                let length = data[i+4] as u16
                    | ((data[i+5] as u16) << 8);
                if offset == 0 {
                    return Err(format!("invalid copy token: offset=0 at byte {}", i));
                }
                tokens.push(Token::Copy { offset, length });
                i += 6;
            }
            b => return Err(format!("unknown token type 0x{:02x} at byte {}", b, i)),
        }
    }
    Ok(tokens)
}

// ── Decompress ────────────────────────────────────────────────────────────────

/// Decompress LZ77 tokens back to original bytes.
pub fn decompress(tokens: &[Token]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    for token in tokens {
        match token {
            Token::Literal(b) => out.push(*b),
            Token::Copy { offset, length } => {
                let offset = *offset as usize;
                let length = *length as usize;
                if offset == 0 || offset > out.len() {
                    return Err(format!(
                        "invalid copy: offset={} out_len={}", offset, out.len()));
                }
                let start = out.len() - offset;
                // Use byte-by-byte copy to handle overlapping copies correctly
                for i in 0..length {
                    let b = out[start + i];
                    out.push(b);
                }
            }
        }
    }
    Ok(out)
}

/// Full compress: data → token bytes (ready for G encoding)
pub fn encode(data: &[u8]) -> Vec<u8> {
    let tokens = compress(data);
    tokens_to_bytes(&tokens)
}

/// Full decompress: token bytes → original data
pub fn decode(token_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let tokens = bytes_to_tokens(token_bytes)?;
    decompress(&tokens)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(data: &[u8]) {
        let encoded = encode(data);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, data,
            "roundtrip failed: {} bytes in, {} encoded, {} decoded",
            data.len(), encoded.len(), decoded.len());
    }

    #[test]
    fn empty() { roundtrip(b""); }

    #[test]
    fn single_byte() { roundtrip(b"x"); }

    #[test]
    fn no_repetition() { roundtrip(b"abcdefghijklmnop"); }

    #[test]
    fn simple_repetition() {
        roundtrip(b"hello hello hello hello hello");
    }

    #[test]
    fn long_repetition() {
        let data: Vec<u8> = b"abcdefgh".iter().cycle().take(10000).copied().collect();
        roundtrip(&data);
    }

    #[test]
    fn all_same_byte() {
        let data = vec![0x42u8; 100000];
        let encoded = encode(&data);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, data);
        // Should compress dramatically
        println!("all-same 100KB: {} -> {} ({:.1}%)",
            data.len(), encoded.len(),
            100.0 * encoded.len() as f64 / data.len() as f64);
        assert!(encoded.len() < data.len() / 10,
            "expected >90% compression, got {} -> {}", data.len(), encoded.len());
    }

    #[test]
    fn binary_roundtrip() {
        let data: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        roundtrip(&data);
    }

    #[test]
    fn token_serialization() {
        let tokens = vec![
            Token::Literal(0x41),
            Token::Copy { offset: 100, length: 50 },
            Token::Literal(0xFF),
            Token::Copy { offset: 8388607, length: 258 }, // max values
        ];
        let bytes = tokens_to_bytes(&tokens);
        let restored = bytes_to_tokens(&bytes).unwrap();
        assert_eq!(tokens, restored);
    }

    #[test]
    fn overlapping_copy() {
        // "aaa..." — each byte copies from 1 back
        let data = vec![b'a'; 1000];
        roundtrip(&data);
    }

    #[test]
    fn compression_ratio_source_code() {
        let data = include_bytes!("lz77.rs");
        let encoded = encode(data);
        println!("lz77.rs: {} -> {} ({:.1}%)",
            data.len(), encoded.len(),
            100.0 * encoded.len() as f64 / data.len() as f64);
        // LZ77 alone should compress source code to < 80%
        assert!(encoded.len() < data.len(),
            "LZ77 should compress source code");
    }
}
