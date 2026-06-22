//! G streaming encoder/decoder API.
//!
//! Processes large data in chunks without loading everything into memory.
//! The LZ77 window and partial block state persist across chunk boundaries.
//!
//! Stream format:
//!   [9 bytes header: "GSTR" + flags(1) + n(4 LE)]
//!   [index bitstream: G block indices packed bit-by-bit]
//!   [16 bytes trailer: original_bits(8 LE) + block_count(8 LE)]
//!
//! The index stream is RNCE-compressed in one pass at finish() time,
//! keeping the RNCE model quality high regardless of chunk sizes.
//!
//! # Example
//! ```no_run
//! # use gencode::stream_api::GEncoder;
//! let mut enc = GEncoder::new(28);
//! let mut out = Vec::new();
//! enc.write_header(&mut out);
//! enc.feed(b"first chunk", &mut out).unwrap();
//! enc.feed(b"second chunk", &mut out).unwrap();
//! enc.finish(&mut out).unwrap();
//! ```

use super::index::IndexTable;
use super::stream::{BitWriter, BitReader, FLAG_ENTROPY, FLAG_LZ77};
use super::rnce::{compress_index_stream_exact, decompress_index_stream_exact};

const LOSSLESS_N_MIN: usize = 24; // G24+ have valid_count = 2^128-1

// ── GEncoder ─────────────────────────────────────────────────────────────────

pub struct GEncoder {
    n: usize,
    table: IndexTable,
    entropy: bool,
    lz77: bool,

    // Accumulated index bit stream (flushed at finish)
    index_writer: BitWriter,
    block_count: u64,
    original_bits: u64,

    // Partial bits not yet forming a full block
    partial: u128,
    partial_bits: u32,

    // LZ77 state
    lz77_window: Vec<u8>,    // recent decoded bytes (for backreference)
    lz77_hash_head: Vec<u32>,
    lz77_hash_prev: Vec<u32>,
    lz77_pending: Vec<u8>,   // input bytes not yet LZ77-encoded

    finished: bool,
}

const WINDOW:    usize = 8 * 1024 * 1024;
const HASH_BITS: usize = 18;
const HASH_SIZE: usize = 1 << HASH_BITS;
const HASH_MASK: usize = HASH_SIZE - 1;
const MAX_CHAIN: usize = 128;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const NIL:       u32   = u32::MAX;

impl GEncoder {
    pub fn new(n: usize) -> Self {
        Self::with_options(n, true, n >= LOSSLESS_N_MIN)
    }

    pub fn with_options(n: usize, entropy: bool, lz77: bool) -> Self {
        Self {
            n, table: IndexTable::new(n), entropy, lz77,
            index_writer: BitWriter::new(),
            block_count: 0, original_bits: 0,
            partial: 0, partial_bits: 0,
            lz77_window: Vec::with_capacity(WINDOW),
            lz77_hash_head: vec![NIL; HASH_SIZE],
            lz77_hash_prev: vec![NIL; WINDOW],
            lz77_pending: Vec::new(),
            finished: false,
        }
    }

    /// Write the 9-byte stream header into `out`.
    pub fn write_header(&self, out: &mut Vec<u8>) {
        let flags = if self.entropy { FLAG_ENTROPY } else { 0 }
                  | if self.lz77    { FLAG_LZ77    } else { 0 };
        out.extend_from_slice(b"GSTR");
        out.push(flags);
        out.extend_from_slice(&(self.n as u32).to_le_bytes());
    }

    /// Feed a chunk of input. The index stream is accumulated internally.
    /// `out` will be empty until `finish()` — call finish() to get all output.
    pub fn feed(&mut self, data: &[u8], _out: &mut Vec<u8>) -> Result<(), String> {
        if self.finished { return Err("encoder already finished".to_string()); }
        if data.is_empty() { return Ok(()); }

        if self.lz77 && super::lz77::looks_compressible(data) {
            // LZ77 encode: stream through the sliding window
            self.lz77_pending.extend_from_slice(data);
            self.flush_lz77()?;
        } else {
            self.push_bytes(data)?;
        }
        Ok(())
    }

    /// Finish encoding. Compresses the full index stream and writes everything to `out`.
    pub fn finish(&mut self, out: &mut Vec<u8>) -> Result<(), String> {
        if self.finished { return Ok(()); }
        self.finished = true;

        // Flush remaining LZ77 input as literals
        let pending = std::mem::take(&mut self.lz77_pending);
        for &b in &pending {
            // Emit literal token
            self.push_bytes(&[0x00, b])?;
        }

        // Flush partial block (pad with zeros)
        if self.partial_bits > 0 {
            let bits = self.table.bits;
            let idx = self.partial << (bits - self.partial_bits);
            self.index_writer.write_bits(idx, bits);
            self.block_count += 1;
            self.partial = 0;
            self.partial_bits = 0;
        }

        // Finalize index stream
        let index_writer = std::mem::replace(&mut self.index_writer, BitWriter::new());
        let raw = index_writer.finish();

        // Compress with RNCE if enabled
        let payload = if self.entropy && !raw.is_empty() {
            compress_index_stream_exact(
                &raw,
                self.n as u8,
                self.original_bits,
                self.block_count,
                self.table.bits,
            )
        } else {
            raw.clone()
        };

        // Write: [4-byte payload len] [payload] [8-byte original_bits] [8-byte block_count]
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&self.original_bits.to_le_bytes());
        out.extend_from_slice(&self.block_count.to_le_bytes());
        Ok(())
    }

    fn push_bytes(&mut self, data: &[u8]) -> Result<(), String> {
        let bits = self.table.bits;
        let vc = self.table.valid_count;
        let sentinel = vc.saturating_sub(1);
        let needs_escape = vc < u128::MAX;

        for &b in data {
            self.partial = (self.partial << 8) | b as u128;
            self.partial_bits += 8;
            self.original_bits += 8;

            while self.partial_bits >= bits {
                let idx = self.partial >> (self.partial_bits - bits);
                self.partial_bits -= bits;
                self.partial &= (1u128 << self.partial_bits).wrapping_sub(1);

                if needs_escape {
                    if idx >= vc {
                        self.index_writer.write_bits(sentinel, bits);
                        self.index_writer.write_bits(idx, bits);
                        self.block_count += 2;
                    } else if idx == sentinel {
                        self.index_writer.write_bits(sentinel, bits);
                        self.index_writer.write_bits(sentinel, bits);
                        self.block_count += 2;
                    } else {
                        self.index_writer.write_bits(idx, bits);
                        self.block_count += 1;
                    }
                } else {
                    self.index_writer.write_bits(idx, bits);
                    self.block_count += 1;
                }
            }
        }
        Ok(())
    }

    fn flush_lz77(&mut self) -> Result<(), String> {
        // Process lz77_pending through the sliding window
        let data = std::mem::take(&mut self.lz77_pending);
        let n = data.len();
        let mut pos = 0;

        while pos < n {
            // Try to find a match
            let best = if pos + 4 <= n {
                self.find_match(&data, pos)
            } else {
                None
            };

            if let Some((offset, length)) = best {
                // Emit copy token: 0x01 + 3-byte offset + 2-byte length
                let mut token = [0u8; 6];
                token[0] = 0x01;
                token[1] = (offset & 0xFF) as u8;
                token[2] = ((offset >> 8) & 0xFF) as u8;
                token[3] = ((offset >> 16) & 0xFF) as u8;
                token[4] = (length & 0xFF) as u8;
                token[5] = ((length >> 8) & 0xFF) as u8;
                self.push_bytes(&token)?;

                // Update window and hash for matched region
                for i in pos..pos + length {
                    self.update_window(data[i], &data, i);
                    if i + 4 <= n {
                        self.insert_hash(&data, i);
                    }
                }
                pos += length;
            } else {
                // Emit literal token: 0x00 + byte
                self.push_bytes(&[0x00, data[pos]])?;
                self.update_window(data[pos], &data, pos);
                if pos + 4 <= n {
                    self.insert_hash(&data, pos);
                }
                pos += 1;
            }
        }
        Ok(())
    }

    fn find_match(&self, data: &[u8], pos: usize) -> Option<(usize, usize)> {
        let h = hash4(data, pos);
        let abs_pos = self.lz77_window.len() + pos;
        let limit = abs_pos.saturating_sub(WINDOW);

        let mut best_len = MIN_MATCH - 1;
        let mut best_offset = 0usize;
        let mut cur = self.lz77_hash_head[h];
        let mut steps = 0;

        while cur != NIL && steps < MAX_CHAIN {
            let cand_abs = cur as usize;
            if cand_abs < limit { break; }
            let offset = abs_pos - cand_abs;
            let len = self.match_len(data, pos, cand_abs, (data.len() - pos).min(MAX_MATCH));
            if len > best_len {
                best_len = len;
                best_offset = offset;
                if len == MAX_MATCH { break; }
            }
            // Walk chain through window prev table
            let slot = cand_abs & (WINDOW - 1);
            cur = self.lz77_hash_prev[slot];
            steps += 1;
        }

        if best_len >= MIN_MATCH { Some((best_offset, best_len)) } else { None }
    }

    fn match_len(&self, data: &[u8], pos: usize, cand_abs: usize, max: usize) -> usize {
        let win_len = self.lz77_window.len();
        let mut len = 0;
        while len < max {
            let a = data[pos + len];
            let cand_pos = cand_abs + len;
            let b = if cand_pos < win_len {
                self.lz77_window[cand_pos]
            } else {
                data[cand_pos - win_len]
            };
            if a != b { break; }
            len += 1;
        }
        len
    }

    fn update_window(&mut self, byte: u8, _data: &[u8], _pos: usize) {
        if self.lz77_window.len() >= WINDOW {
            // Remove oldest byte — shift window (expensive but correct)
            // In production, use a circular buffer; this works for correctness
            let remove_slot = self.lz77_window.len() & (WINDOW - 1);
            self.lz77_window.push(byte);
            if self.lz77_window.len() > WINDOW {
                self.lz77_window.remove(0); // circular remove
            }
        } else {
            self.lz77_window.push(byte);
        }
    }

    fn insert_hash(&mut self, data: &[u8], pos: usize) {
        let h = hash4(data, pos);
        let abs_pos = self.lz77_window.len() + pos;
        let slot = abs_pos & (WINDOW - 1);
        self.lz77_hash_prev[slot] = self.lz77_hash_head[h];
        self.lz77_hash_head[h] = abs_pos as u32;
    }

    pub fn n(&self) -> usize { self.n }
    pub fn block_count(&self) -> u64 { self.block_count }
    pub fn original_bits(&self) -> u64 { self.original_bits }
}

fn hash4(data: &[u8], pos: usize) -> usize {
    let v = u32::from_le_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]);
    ((v.wrapping_mul(0x9E3779B1) >> (32 - HASH_BITS)) as usize) & HASH_MASK
}

// ── GDecoder ─────────────────────────────────────────────────────────────────

pub struct GDecoder {
    n: usize,
    entropy: bool,
    lz77: bool,
    header_parsed: bool,
}

impl GDecoder {
    pub fn new() -> Self {
        Self { n: 28, entropy: true, lz77: true, header_parsed: false }
    }

    /// Parse the 9-byte stream header.
    pub fn feed_header(&mut self, data: &[u8]) -> Result<(), String> {
        if data.len() < 9 {
            return Err(format!("Stream header too short: {} bytes (need 9)", data.len()));
        }
        if &data[0..4] != b"GSTR" {
            return Err(format!("Not a G stream — expected 'GSTR', got {:?}", &data[0..4]));
        }
        let flags = data[4];
        self.entropy = flags & FLAG_ENTROPY != 0;
        self.lz77    = flags & FLAG_LZ77    != 0;
        self.n = u32::from_le_bytes(data[5..9].try_into().unwrap()) as usize;
        self.header_parsed = true;
        Ok(())
    }

    /// Decode a complete stream payload (everything after the header).
    /// `stream_data` = [4-byte payload len][payload][8-byte original_bits][8-byte block_count]
    pub fn decode_stream(&self, stream_data: &[u8]) -> Result<Vec<u8>, String> {
        if !self.header_parsed {
            return Err("Call feed_header() before decode_stream()".to_string());
        }
        if stream_data.len() < 20 {
            return Err(format!("Stream payload too short: {} bytes", stream_data.len()));
        }

        let payload_len = u32::from_le_bytes(stream_data[0..4].try_into().unwrap()) as usize;
        if stream_data.len() < 4 + payload_len + 16 {
            return Err("Stream payload truncated".to_string());
        }

        let payload = &stream_data[4..4 + payload_len];
        let trailer = &stream_data[4 + payload_len..4 + payload_len + 16];
        let original_bits = u64::from_le_bytes(trailer[0..8].try_into().unwrap());
        let block_count   = u64::from_le_bytes(trailer[8..16].try_into().unwrap());

        // Decompress index stream
        let table = IndexTable::new(self.n);
        let bits = table.bits;

        let raw = if self.entropy {
            let (idx_bytes, _, _, _, _) = decompress_index_stream_exact(payload)
                .ok_or("RNCE decompression failed")?;
            idx_bytes
        } else {
            payload.to_vec()
        };

        // Reconstruct original bits — decode escape sequences
        let vc = table.valid_count;
        let sentinel = vc.saturating_sub(1);
        let needs_escape = vc < u128::MAX;

        let mut reader = BitReader::new(raw);
        let mut writer = super::stream::BitWriter::new();
        let mut bits_written = 0u64;
        let mut blocks_consumed = 0u64;

        while blocks_consumed < block_count {
            let idx = reader.read_bits(bits).ok_or("Unexpected end of index stream")?;
            blocks_consumed += 1;

            let actual_idx = if needs_escape && idx == sentinel {
                let next = reader.read_bits(bits).ok_or("Truncated escape sequence")?;
                blocks_consumed += 1;
                next // next == sentinel means value was sentinel (double-escape)
            } else {
                idx
            };

            let remaining = original_bits - bits_written;
            if remaining == 0 { break; }
            let to_write = bits.min(remaining as u32);
            if to_write < bits {
                writer.write_bits(actual_idx >> (bits - to_write), to_write);
            } else {
                writer.write_bits(actual_idx, bits);
            }
            bits_written += to_write as u64;
        }

        let mut decoded = writer.finish();
        let original_bytes = ((original_bits + 7) / 8) as usize;
        decoded.truncate(original_bytes);

        // LZ77 decode if enabled
        if self.lz77 {
            super::lz77::decode(&decoded)
                .map_err(|e| format!("LZ77 decode failed: {}", e))
        } else {
            Ok(decoded)
        }
    }

    pub fn n(&self) -> usize { self.n }
}

// ── Convenience: encode/decode whole buffers via streaming path ───────────────

/// Encode `data` using the streaming encoder. Equivalent to buffered encode
/// but uses the streaming format (GSTR header).
pub fn stream_encode(data: &[u8], n: usize, entropy: bool) -> Result<Vec<u8>, String> {
    let lz77 = n >= LOSSLESS_N_MIN && super::lz77::looks_compressible(data);
    let mut enc = GEncoder::with_options(n, entropy, lz77);
    let mut out = Vec::new();
    enc.write_header(&mut out);
    enc.feed(data, &mut out)?;
    enc.finish(&mut out)?;
    Ok(out)
}

/// Decode a streaming G stream (GSTR format).
pub fn stream_decode(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 9 {
        return Err("Too short to be a G stream".to_string());
    }
    let mut dec = GDecoder::new();
    dec.feed_header(&data[0..9])?;
    dec.decode_stream(&data[9..])
}

// ── C FFI ─────────────────────────────────────────────────────────────────────

pub struct GEncoderHandle {
    encoder: GEncoder,
    header_emitted: bool,
    out_buf: Vec<u8>,
}

#[unsafe(no_mangle)]
pub extern "C" fn g_encoder_new(n: i32) -> *mut GEncoderHandle {
    if n <= 0 { return std::ptr::null_mut(); }
    let enc = GEncoder::new(n as usize);
    Box::into_raw(Box::new(GEncoderHandle {
        encoder: enc,
        header_emitted: false,
        out_buf: Vec::new(),
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn g_encoder_feed(
    handle: *mut GEncoderHandle,
    data: *const u8, len: usize,
    out: *mut u8, out_cap: usize, out_len: *mut usize,
) -> i32 {
    if handle.is_null() || data.is_null() || out.is_null() || out_len.is_null() {
        return super::ffi::G_ERR_NULL;
    }
    let h = unsafe { &mut *handle };

    // Emit header on first feed
    if !h.header_emitted {
        h.encoder.write_header(&mut h.out_buf);
        h.header_emitted = true;
    }

    let input = unsafe { std::slice::from_raw_parts(data, len) };
    let mut tmp = Vec::new();
    if h.encoder.feed(input, &mut tmp).is_err() {
        return super::ffi::G_ERR_ENCODE;
    }
    h.out_buf.extend(tmp);

    let avail = h.out_buf.len();
    if avail > out_cap { unsafe { *out_len = 0; } return super::ffi::G_ERR_BUFFER; }
    unsafe {
        std::ptr::copy_nonoverlapping(h.out_buf.as_ptr(), out, avail);
        *out_len = avail;
    }
    h.out_buf.clear();
    super::ffi::G_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn g_encoder_finish(
    handle: *mut GEncoderHandle,
    out: *mut u8, out_cap: usize, out_len: *mut usize,
) -> i32 {
    if handle.is_null() || out.is_null() || out_len.is_null() {
        return super::ffi::G_ERR_NULL;
    }
    let h = unsafe { &mut *handle };

    if h.encoder.finish(&mut h.out_buf).is_err() {
        return super::ffi::G_ERR_ENCODE;
    }

    let avail = h.out_buf.len();
    if avail > out_cap { unsafe { *out_len = 0; } return super::ffi::G_ERR_BUFFER; }
    unsafe {
        std::ptr::copy_nonoverlapping(h.out_buf.as_ptr(), out, avail);
        *out_len = avail;
    }
    h.out_buf.clear();
    super::ffi::G_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn g_encoder_free(handle: *mut GEncoderHandle) {
    if !handle.is_null() { unsafe { drop(Box::from_raw(handle)); } }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(data: &[u8], n: usize) {
        let encoded = stream_encode(data, n, false).unwrap();
        let decoded = stream_decode(&encoded).unwrap();
        assert_eq!(decoded, data,
            "n={} roundtrip failed: {} in -> {} encoded -> {} decoded",
            n, data.len(), encoded.len(), decoded.len());
    }

    #[test]
    fn empty() { roundtrip(b"", 28); }

    #[test]
    fn single_byte() { roundtrip(b"x", 28); }

    #[test]
    fn hello() { roundtrip(b"hello world", 28); }

    #[test]
    fn binary_1kb() {
        let data: Vec<u8> = (0..1024).map(|i| (i * 17 % 256) as u8).collect();
        roundtrip(&data, 28);
    }

    #[test]
    fn binary_64kb() {
        let data: Vec<u8> = (0..65536).map(|i| (i * 31 % 256) as u8).collect();
        roundtrip(&data, 28);
    }

    #[test]
    fn chunked_equals_single() {
        let data: Vec<u8> = (0..4096).map(|i| (i % 256) as u8).collect();

        let single = stream_encode(&data, 28, false).unwrap();

        // Feed same data in many small chunks
        let mut enc = GEncoder::with_options(28, false, false);
        let mut out = Vec::new();
        enc.write_header(&mut out);
        for chunk in data.chunks(64) {
            enc.feed(chunk, &mut out).unwrap();
        }
        enc.finish(&mut out).unwrap();

        // Decode both — should get same result
        let dec1 = stream_decode(&single).unwrap();
        let dec2 = stream_decode(&out).unwrap();
        assert_eq!(dec1, data);
        assert_eq!(dec2, data);
    }

    #[test]
    fn multiple_n_values() {
        // Only G24+ are lossless (valid_count = 2^128-1).
        // G4-G20 have valid_count < 2^bits and are lossy for binary data.
        let data = b"testing G streaming across multiple Gn instances";
        for n in [24, 28, 32] {
            roundtrip(data, n);
        }
    }

    #[test]
    fn lossless_boundary() {
        // Verify which Gn instances are lossless
        let data: Vec<u8> = (0u8..=255u8).collect();
        for n in [24, 28, 32] {
            let enc = stream_encode(&data, n, false).unwrap();
            let dec = stream_decode(&enc).unwrap();
            assert_eq!(dec, data, "G{} should be lossless", n);
        }
    }

    #[test]
    fn header_roundtrip() {
        let enc = GEncoder::new(28);
        let mut h = Vec::new();
        enc.write_header(&mut h);
        assert_eq!(&h[0..4], b"GSTR");
        let mut dec = GDecoder::new();
        dec.feed_header(&h).unwrap();
        assert_eq!(dec.n(), 28);
    }

    #[test]
    fn bad_header_rejected() {
        let mut dec = GDecoder::new();
        assert!(dec.feed_header(b"NOPE\x00\x1c\x00\x00\x00").is_err());
    }

    #[test]
    fn with_entropy() {
        let data = b"hello streaming with RNCE entropy layer enabled";
        let encoded = stream_encode(data, 28, true).unwrap();
        let decoded = stream_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn large_random() {
        let data: Vec<u8> = (0..100_000u32).map(|i| {
            let mut x = i.wrapping_mul(2654435761);
            x ^= x >> 16;
            (x & 0xFF) as u8
        }).collect();
        roundtrip(&data, 28);
    }

    #[test]
    fn original_bits_tracked() {
        let data = b"hello";
        let mut enc = GEncoder::with_options(28, false, false);
        let mut out = Vec::new();
        enc.write_header(&mut out);
        enc.feed(data, &mut out).unwrap();
        enc.finish(&mut out).unwrap();
        assert_eq!(enc.original_bits(), 40); // 5 bytes = 40 bits
    }
}
