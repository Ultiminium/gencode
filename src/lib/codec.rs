use super::index::IndexTable;
use super::stream::{BitWriter, BitReader, FrameHeader, FLAG_ENTROPY, FLAG_CONTEXT, FLAG_ADAPTIVE, FLAG_LZ77};
use super::rnce::{compress_index_stream_exact, decompress_index_stream_exact};
use super::adjacency::can_adjoin;
use indicatif::{ProgressBar, ProgressStyle};

const CHUNK_BYTES: usize = 64 * 1024;
// All Gn instances — G4-G20 use escape coding for out-of-range indices
const ADAPTIVE_CANDIDATES: &[usize] = &[4, 8, 12, 16, 20, 24, 28, 32];

pub struct EncodeOptions {
    pub n: usize,
    pub show_progress: bool,
    pub repair_invalid: bool,
    pub entropy: bool,
    pub context: bool,
    pub adaptive: bool,
    pub lz77: bool,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self { n: 28, show_progress: true, repair_invalid: true, entropy: true, context: true, adaptive: true, lz77: true }
    }
}

pub struct DecodeOptions {
    pub n: usize,
    pub show_progress: bool,
    pub skip_nulls: bool,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self { n: 0, show_progress: true, skip_nulls: true }
    }
}

pub struct EncodeResult {
    pub data: Vec<u8>,
    pub blocks: u64,
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub repairs: u64,
    pub chunks: usize,
    pub entropy_savings: usize,
    pub context_savings: usize,
}

pub struct DecodeResult {
    pub data: Vec<u8>,
    pub blocks: u64,
    pub nulls_skipped: u64,
}

// ─── Encoding ────────────────────────────────────────────────────────────────

pub fn encode_bytes(data: &[u8], opts: &EncodeOptions) -> Result<EncodeResult, String> {
    // LZ77 preprocessing — compress before G index encoding
    let original_input_bytes = data.len();
    let lz77_buf: Vec<u8>;
    let data = if opts.lz77 && !data.is_empty() && super::lz77::looks_compressible(data) {
        let encoded = super::lz77::encode(data);
        // Only use LZ77 output if it's actually smaller
        if encoded.len() < data.len() {
            lz77_buf = encoded;
            &lz77_buf[..]
        } else {
            lz77_buf = Vec::new();
            data
        }
    } else {
        lz77_buf = Vec::new();
        data
    };
    // Re-derive flags based on whether LZ77 was actually applied
    let lz77_applied = opts.lz77 && !lz77_buf.is_empty();
    let input_bytes = original_input_bytes;
    let n = if opts.adaptive { 28 } else { opts.n }; // adaptive picks per-chunk below

    let flags =
        if opts.entropy   { FLAG_ENTROPY  } else { 0 } |
        if opts.context   { FLAG_CONTEXT  } else { 0 } |
        if opts.adaptive  { FLAG_ADAPTIVE } else { 0 } |
        if lz77_applied   { FLAG_LZ77     } else { 0 };

    let pb = if opts.show_progress {
        let pb = ProgressBar::new(input_bytes as u64);
        pb.set_style(ProgressStyle::default_bar()
            .template("  {spinner:.cyan} [{bar:40.cyan/blue}] {bytes}/{total_bytes}  {elapsed_precise}  {msg}")
            .unwrap()
            .progress_chars("=>-"));
        pb.set_message(format!("encoding G{}", n));
        Some(pb)
    } else { None };

    let mut frame = FrameHeader::new(flags);
    let mut all_payloads: Vec<Vec<u8>> = Vec::new();
    let mut total_blocks = 0u64;
    let mut total_entropy_savings = 0usize;

    // Split into chunks for adaptive mode
    let chunk_size = if opts.adaptive { CHUNK_BYTES } else { data.len().max(1) };
    let chunks: Vec<&[u8]> = data.chunks(chunk_size).collect();

    for chunk_data in &chunks {
        let chunk_n = if opts.adaptive {
            let pn = pick_best_n(chunk_data);
            pn
        } else {
            opts.n
        };

        let table = IndexTable::new(chunk_n);
        let bits = table.bits;
        let valid_count = table.valid_count;
        let chunk_bytes = chunk_data.len();

        // Pad input to exact multiple of bits so every bit gets encoded
        let total_input_bits = chunk_bytes * 8;
        let blocks_needed = (total_input_bits + bits as usize - 1) / bits as usize;
        let padded_bits = blocks_needed * bits as usize;
        let padded_bytes = (padded_bits + 7) / 8;
        let mut padded_input = chunk_data.to_vec();
        padded_input.resize(padded_bytes, 0u8); // zero-pad to full blocks

        let mut reader = BitReader::new(padded_input);
        let mut writer = BitWriter::new();
        let mut block_count = 0u64;

        let vc = valid_count;
        let sentinel = vc.saturating_sub(1);
        let needs_escape = vc < u128::MAX;

        for _ in 0..blocks_needed {
            match reader.read_bits(bits) {
                Some(idx) => {
                    if needs_escape {
                        if idx >= vc {
                            // Out of range: emit sentinel + raw idx
                            writer.write_bits(sentinel, bits);
                            writer.write_bits(idx, bits);
                            block_count += 2;
                        } else if idx == sentinel {
                            // Sentinel collision: double-escape
                            writer.write_bits(sentinel, bits);
                            writer.write_bits(sentinel, bits);
                            block_count += 2;
                        } else {
                            writer.write_bits(idx, bits);
                            block_count += 1;
                        }
                    } else {
                        // G24+: all values valid, no escaping
                        writer.write_bits(idx, bits);
                        block_count += 1;
                    }
                    if let Some(ref pb) = pb { pb.inc((bits as u64 + 7) / 8); }
                }
                None => break,
            }
        }

        let raw = writer.finish();
        let raw_size = raw.len();

        let (payload, compressed_size) = if opts.entropy {
            // RNCE as entropy layer: compress G index stream using cmix
            let c = compress_index_stream_exact(
                &raw, chunk_n as u8, (chunk_bytes * 8) as u64, block_count, table.bits
            );
            let cs = c.len();
            total_entropy_savings += raw_size.saturating_sub(cs);
            (c, cs as u64)
        } else {
            let len = raw.len() as u64;
            (raw, len)
        };

        frame.add_chunk(chunk_n as u16, block_count, compressed_size, (chunk_bytes * 8) as u64); // store bits not bytes
        all_payloads.push(payload);
        total_blocks += block_count;
    }

    if let Some(ref pb) = pb { pb.finish_with_message("done"); }

    let mut out = Vec::new();
    frame.write(&mut out).map_err(|e| e.to_string())?;
    for p in all_payloads { out.extend(p); }
    let output_bytes = out.len();

    Ok(EncodeResult {
        data: out, blocks: total_blocks, input_bytes, output_bytes,
        repairs: 0, chunks: chunks.len(),
        entropy_savings: total_entropy_savings, context_savings: 0,
    })
}

pub fn encode_string(s: &str, opts: &EncodeOptions) -> Result<EncodeResult, String> {
    // Encode UTF-8 bytes of the string
    encode_bytes(s.as_bytes(), opts)
}

pub fn decode_to_bytes(data: &[u8], opts: &DecodeOptions) -> Result<DecodeResult, String> {
    let frame = FrameHeader::read(&mut data.as_ref()).map_err(|e| e.to_string())?;
    let use_entropy = frame.flags & FLAG_ENTROPY != 0;
    let use_lz77 = frame.flags & FLAG_LZ77 != 0;
    let header_size = frame.byte_size();
    let mut offset = header_size;

    let total_blocks: u64 = frame.chunks.iter().map(|c| c.block_count).sum();
    let total_original_bits: u64 = frame.chunks.iter().map(|c| c.original_bytes).sum(); // stored as bits

    let pb = if opts.show_progress {
        let pb = ProgressBar::new(total_blocks);
        pb.set_style(ProgressStyle::default_bar()
            .template("  {spinner:.cyan} [{bar:40.cyan/blue}] {pos}/{len} blocks  {elapsed_precise}  {msg}")
            .unwrap()
            .progress_chars("=>-"));
        pb.set_message("decoding");
        Some(pb)
    } else { None };

    let mut out = Vec::with_capacity((total_original_bits / 8 + 1) as usize);

    for chunk in &frame.chunks {
        let n = chunk.n as usize;
        let table = IndexTable::new(n);
        let bits = table.bits;
        let payload_size = chunk.compressed_size as usize;
        let original_bits = chunk.original_bytes as usize; // stored as bits
        let original_bytes = (original_bits + 7) / 8;

        let payload = data.get(offset..offset + payload_size)
            .ok_or_else(|| format!("Truncated at offset {}", offset))?;
        offset += payload_size;

        let raw = if use_entropy {
            // RNCE entropy decode: decompress G index stream
            let (index_bytes, _, _, _, _) = decompress_index_stream_exact(payload)
                .ok_or_else(|| "RNCE index stream decode failed".to_string())?;
            index_bytes
        } else {
            payload.to_vec()
        };

        // Read block indices back, decode escape sequences, write original_bits bits
        let vc = table.valid_count;
        let sentinel = vc.saturating_sub(1);
        let needs_escape = vc < u128::MAX;

        let mut reader = BitReader::new(raw);
        let mut writer = BitWriter::new();
        let mut bits_written = 0usize;
        let mut blocks_consumed = 0u64;

        while blocks_consumed < chunk.block_count {
            let idx = reader.read_bits(bits).ok_or("Unexpected end of bitstream")?;
            blocks_consumed += 1;

            let actual_idx = if needs_escape && idx == sentinel {
                // Escape sequence — read the next block for the actual value
                let next = reader.read_bits(bits).ok_or("Unexpected end of bitstream after escape")?;
                blocks_consumed += 1;
                // next == sentinel means double-escape (the value WAS sentinel)
                next
            } else {
                idx
            };

            let remaining_bits = original_bits - bits_written;
            if remaining_bits == 0 { break; }
            let bits_to_write = (bits as usize).min(remaining_bits) as u32;
            if bits_to_write < bits {
                let shifted = actual_idx >> (bits - bits_to_write);
                writer.write_bits(shifted, bits_to_write);
            } else {
                writer.write_bits(actual_idx, bits);
            }
            bits_written += bits_to_write as usize;
            if let Some(ref pb) = pb { pb.inc(1); }
        }

        let chunk_out = writer.finish();
        out.extend(chunk_out);
    }

    if let Some(ref pb) = pb { pb.finish_with_message("done"); }

    // LZ77 post-processing — decompress token stream back to original bytes
    let final_data = if use_lz77 {
        super::lz77::decode(&out).map_err(|e| format!("LZ77 decode failed: {}", e))?
    } else {
        out
    };

    Ok(DecodeResult { data: final_data, blocks: total_blocks, nulls_skipped: 0 })
}

pub fn decode_to_string(data: &[u8], opts: &DecodeOptions) -> Result<String, String> {
    let result = decode_to_bytes(data, opts)?;
    String::from_utf8(result.data).map_err(|e| format!("UTF-8 decode error: {}", e))
}

fn pick_best_n(chunk: &[u8]) -> usize {
    let mut best_n = 28;
    let mut best_size = usize::MAX;

    for &n in ADAPTIVE_CANDIDATES {
        let table = IndexTable::new(n);
        let bits = table.bits;
        let valid_count = table.valid_count;

        // Pad chunk to full block boundary
        let total_bits = chunk.len() * 8;
        let blocks_needed = (total_bits + bits as usize - 1) / bits as usize;
        let padded_bytes = (blocks_needed * bits as usize + 7) / 8;
        let mut padded = chunk.to_vec();
        padded.resize(padded_bytes, 0u8);

        let vc = valid_count;
        let sentinel = vc.saturating_sub(1);
        let needs_escape = vc < u128::MAX;

        let mut reader = BitReader::new(padded);
        let mut writer = BitWriter::new();
        let mut actual_blocks = 0u64;
        for _ in 0..blocks_needed {
            match reader.read_bits(bits) {
                Some(idx) => {
                    if needs_escape {
                        if idx >= vc {
                            writer.write_bits(sentinel, bits);
                            writer.write_bits(idx, bits);
                            actual_blocks += 2;
                        } else if idx == sentinel {
                            writer.write_bits(sentinel, bits);
                            writer.write_bits(sentinel, bits);
                            actual_blocks += 2;
                        } else {
                            writer.write_bits(idx, bits);
                            actual_blocks += 1;
                        }
                    } else {
                        writer.write_bits(idx, bits);
                        actual_blocks += 1;
                    }
                }
                None => break,
            }
        }
        let raw = writer.finish();
        let size = compress_index_stream_exact(&raw, n as u8, 0, actual_blocks, table.bits).len();
        if size < best_size { best_size = size; best_n = n; }
    }

    best_n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eopts(n: usize) -> EncodeOptions {
        EncodeOptions { n, show_progress: false, repair_invalid: true, entropy: false, context: false, adaptive: false, lz77: false }
    }
    fn dopts() -> DecodeOptions {
        DecodeOptions { n: 0, show_progress: false, skip_nulls: true}
    }

    #[test]
    fn byte_roundtrip_g4() {
        let data = b"hello world this is a test";
        let enc = encode_bytes(data, &eopts(4)).unwrap();
        let dec = decode_to_bytes(&enc.data, &dopts()).unwrap();
        assert_eq!(dec.data, data, "G4 roundtrip failed");
    }

    #[test]
    fn byte_roundtrip_g28() {
        let data = b"the quick brown fox jumps over the lazy dog 1234567890";
        let enc = encode_bytes(data, &eopts(28)).unwrap();
        let dec = decode_to_bytes(&enc.data, &dopts()).unwrap();
        assert_eq!(dec.data, data, "G28 roundtrip failed");
    }

    #[test]
    fn binary_roundtrip_g28() {
        let data: Vec<u8> = (0..=255u8).cycle().take(1024).collect();
        let enc = encode_bytes(&data, &eopts(28)).unwrap();
        let dec = decode_to_bytes(&enc.data, &dopts()).unwrap();
        assert_eq!(dec.data, data, "binary roundtrip failed");
    }

    #[test]
    fn entropy_roundtrip() {
        let data = b"entropy layer test data here for compression";
        let opts = EncodeOptions { n: 28, show_progress: false, repair_invalid: true, entropy: true, context: false, adaptive: false, lz77: false };
        let enc = encode_bytes(data, &opts).unwrap();
        let dec = decode_to_bytes(&enc.data, &dopts()).unwrap();
        assert_eq!(dec.data, data);
    }

    #[test]
    fn adaptive_roundtrip() {
        let data: Vec<u8> = (0..4096).map(|i| (i % 256) as u8).collect();
        let opts = EncodeOptions { n: 28, show_progress: false, repair_invalid: true, entropy: true, context: false, adaptive: true, lz77: false };
        let enc = encode_bytes(&data, &opts).unwrap();
        let dec = decode_to_bytes(&enc.data, &dopts()).unwrap();
        assert_eq!(dec.data, data);
    }

    #[test]
    fn string_roundtrip() {
        let s = "hello G system";
        let opts = eopts(28);
        let enc = encode_string(s, &opts).unwrap();
        let dec = decode_to_string(&enc.data, &dopts()).unwrap();
        assert_eq!(dec, s);
    }

    #[test]
    fn odd_length_roundtrip() {
        // Test files that aren't multiples of block bit size
        for len in [1, 7, 13, 57, 100, 255, 1000] {
            let data: Vec<u8> = (0..len).map(|i| (i * 17 % 256) as u8).collect();
            let enc = encode_bytes(&data, &eopts(28)).unwrap();
            let dec = decode_to_bytes(&enc.data, &dopts()).unwrap();
            assert_eq!(dec.data, data, "odd length {} failed", len);
        }
    }
}

#[cfg(test)]
mod decode_debug {
    use super::*;
    #[test]
    fn exact_57_bytes() {
        let data = b"hello world this is a real test of the G encoding system\n";
        assert_eq!(data.len(), 57);
        let opts = EncodeOptions { n: 28, show_progress: false, repair_invalid: true,
            entropy: false, context: false, adaptive: false, lz77: false };
        let enc = encode_bytes(data, &opts).unwrap();
        let dopts = DecodeOptions { n: 0, show_progress: false, skip_nulls: true};
        let dec = decode_to_bytes(&enc.data, &dopts).unwrap();

        assert_eq!(dec.data.len(), 57, "wrong length: got {}", dec.data.len());
        assert_eq!(dec.data, data.to_vec());
    }
}

    #[test]
    fn text_roundtrip_entropy() {
        let data = b"hello world this is a test of G + RNCE\n";
        let opts = EncodeOptions { n: 28, show_progress: false, repair_invalid: true,
            entropy: true, context: false, adaptive: false, lz77: false };
        let enc = encode_bytes(data, &opts).unwrap();
        let dopts = DecodeOptions { n: 0, show_progress: false, skip_nulls: true};
        let dec = decode_to_bytes(&enc.data, &dopts).unwrap();
        assert_eq!(dec.data, data.to_vec(),
            "mismatch at bytes: {:?}",
            dec.data.iter().zip(data.iter()).enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(i, (a, b))| format!("[{}] got=0x{:02x} want=0x{:02x}", i, a, b))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn adaptive_binary_roundtrip_stress() {
        // Test adaptive mode with many different random-looking inputs
        let mut seed = 0xdeadbeef_u64;
        let mut next_byte = || { 
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); 
            (seed >> 33) as u8 
        };
        
        for trial in 0..20 {
            let len = 512 + trial * 128;
            let data: Vec<u8> = (0..len).map(|_| next_byte()).collect();
            let opts = EncodeOptions { n: 28, show_progress: false, repair_invalid: true,
                entropy: true, context: false, adaptive: true, lz77: false };
            let enc = encode_bytes(&data, &opts).unwrap();
            let dopts = DecodeOptions { n: 0, show_progress: false, skip_nulls: true};
            let dec = decode_to_bytes(&enc.data, &dopts).unwrap();
            let diffs: Vec<usize> = dec.data.iter().zip(data.iter()).enumerate()
                .filter(|(_, (a, b))| a != b).map(|(i, _)| i).collect();
            assert!(diffs.is_empty(), 
                "trial {} len={}: {} diffs, first at {:?}", trial, len, diffs.len(), diffs.first());
        }
    }

    #[test]
    fn adaptive_debug_512() {
        // Same seed as stress test, trial 0, len=512
        let mut seed = 0xdeadbeef_u64;
        let mut next_byte = || { 
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); 
            (seed >> 33) as u8 
        };
        let data: Vec<u8> = (0..512).map(|_| next_byte()).collect();
        
        // Test non-adaptive first (should pass)
        let opts_na = EncodeOptions { n: 28, show_progress: false, repair_invalid: true,
            entropy: true, context: false, adaptive: false, lz77: false };
        let enc_na = encode_bytes(&data, &opts_na).unwrap();
        let dopts = DecodeOptions { n: 0, show_progress: false, skip_nulls: true};
        let dec_na = decode_to_bytes(&enc_na.data, &dopts).unwrap();
        assert_eq!(dec_na.data, data, "non-adaptive failed!");
        
        // Now adaptive
        let opts_a = EncodeOptions { n: 28, show_progress: false, repair_invalid: true,
            entropy: true, context: false, adaptive: true, lz77: false };
        let enc_a = encode_bytes(&data, &opts_a).unwrap();
        
        let dec_a = decode_to_bytes(&enc_a.data, &dopts).unwrap();
        
        let diffs: Vec<_> = dec_a.data.iter().zip(data.iter()).enumerate()
            .filter(|(_, (a,b))| a != b)
            .take(5)
            .map(|(i, (a,b))| format!("[{}] got=0x{:02x} want=0x{:02x}", i, a, b))
            .collect();
        assert!(diffs.is_empty(), "adaptive failed");
    }

    #[test]
    fn g16_valid_count() {
        let table = crate::index::IndexTable::new(16);
        // Check if a specific 82-bit value gets clamped
        let test_idx: u128 = 0xf8b143360000000000000000u128 >> (128-82); // top 82 bits of original data
    }

    #[test]
    fn valid_count_vs_bits() {
        // Check which Gn instances have valid_count >= 2^bits (lossless direct encoding)
        for n in [4, 8, 12, 16, 20, 24, 28, 32] {
            let table = crate::index::IndexTable::new(n);
            let max_idx = if table.bits >= 128 { u128::MAX } else { (1u128 << table.bits) - 1 };
            let lossless = table.valid_count >= max_idx || table.valid_count == u128::MAX;
        }
    }
