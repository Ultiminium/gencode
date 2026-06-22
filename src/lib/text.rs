//! G canonical text format (.gt)
//!
//! A `.gt` file is a human-readable, copy-pasteable representation of G-encoded data.
//! Each block is one line of exactly n characters drawn from the Gn character set.
//! Adjacency rules apply — a valid `.gt` file is structurally self-validating.
//!
//! Format:
//!   Line 1:  Gn/{major}/{minor}/{patch}/{n}/{original_bytes}
//!   Line 2+: one block per line, exactly n characters each
//!
//! Example (G4, encoding "hi\n"):
//!   Gn/0/1/0/4/3
//!   1aA2
//!   bB3c
//!   0000
//!
//! Backwards compatibility: the version triple (major/minor/patch) selects the
//! decode algorithm. Future versions can change the encoding without breaking
//! existing files — the decoder dispatches on version.
//!
//! Null character: `0` — appears as a literal `0` in the text. Trailing nulls
//! in the last block are padding; `original_bytes` tells the decoder exactly
//! where real data ends.

use super::charset::build_charset;
use super::adjacency::{can_adjoin, is_valid_block};
use super::index::IndexTable;
use super::stream::{BitWriter, BitReader};

pub const VERSION_MAJOR: u32 = 0;
pub const VERSION_MINOR: u32 = 1;
pub const VERSION_PATCH: u32 = 0; // .gt format version, not crate version

// ── Header ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct GtHeader {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub n: usize,
    pub original_bytes: u64,
}

impl GtHeader {
    pub fn new(n: usize, original_bytes: u64) -> Self {
        Self {
            major: VERSION_MAJOR,
            minor: VERSION_MINOR,
            patch: VERSION_PATCH,
            n,
            original_bytes,
        }
    }

    pub fn to_string(&self) -> String {
        format!("Gn/{}/{}/{}/{}/{}",
            self.major, self.minor, self.patch, self.n, self.original_bytes)
    }

    pub fn parse(line: &str) -> Result<Self, String> {
        let line = line.trim();
        if !line.starts_with("Gn/") {
            return Err(format!("Invalid G text header: must start with 'Gn/', got {:?}", line));
        }
        let parts: Vec<&str> = line[3..].split('/').collect();
        if parts.len() != 5 {
            return Err(format!("Invalid G text header: expected 5 fields after 'Gn/', got {}", parts.len()));
        }
        let major = parts[0].parse::<u32>().map_err(|_| format!("Invalid major version: {:?}", parts[0]))?;
        let minor = parts[1].parse::<u32>().map_err(|_| format!("Invalid minor version: {:?}", parts[1]))?;
        let patch = parts[2].parse::<u32>().map_err(|_| format!("Invalid patch version: {:?}", parts[2]))?;
        let n = parts[3].parse::<usize>().map_err(|_| format!("Invalid n value: {:?}", parts[3]))?;
        let original_bytes = parts[4].parse::<u64>().map_err(|_| format!("Invalid original_bytes: {:?}", parts[4]))?;

        if n == 0 {
            return Err("n must be > 0".to_string());
        }

        Ok(Self { major, minor, patch, n, original_bytes })
    }
}

// ── Encode bytes → .gt text ────────────────────────────────────────────────

/// Encode raw bytes into G canonical text format.
/// Returns a string ready to write to a `.gt` file.
pub fn encode_to_text(data: &[u8], n: usize) -> Result<String, String> {
    dispatch_encode(data, n, VERSION_MAJOR, VERSION_MINOR, VERSION_PATCH)
}

fn dispatch_encode(data: &[u8], n: usize, major: u32, minor: u32, patch: u32) -> Result<String, String> {
    match (major, minor, patch) {
        (0, 1, 0) => encode_v010(data, n),
        _ => Err(format!("Unsupported G text version {}.{}.{}", major, minor, patch)),
    }
}

fn encode_v010(data: &[u8], n: usize) -> Result<String, String> {
    let original_bytes = data.len() as u64;
    let header = GtHeader::new(n, original_bytes);
    let table = IndexTable::new(n);
    let bits = table.bits;
    let charset: Vec<char> = table.charset.iter().map(|g| g.ch).collect();

    // Encode input bits → block indices → G character blocks
    let total_input_bits = data.len() * 8;
    let blocks_needed = if total_input_bits == 0 { 0 } else {
        (total_input_bits + bits as usize - 1) / bits as usize
    };
    let padded_bytes = (blocks_needed * bits as usize + 7) / 8;
    let mut padded = data.to_vec();
    padded.resize(padded_bytes, 0u8);

    let mut reader = BitReader::new(padded);
    let mut lines = vec![header.to_string()];

    for _ in 0..blocks_needed {
        let idx = reader.read_bits(bits).unwrap_or(0);
        // Decode index → G block characters
        let block_chars = table.decode_index(idx)?;
        let block_str: String = block_chars.iter().collect();
        lines.push(block_str);
    }

    // If data was empty, add one null block so the file isn't empty
    if blocks_needed == 0 {
        lines.push("0".repeat(n));
    }

    Ok(lines.join("\n") + "\n")
}

// ── Decode .gt text → bytes ────────────────────────────────────────────────

/// Decode G canonical text format back to raw bytes.
pub fn decode_from_text(text: &str) -> Result<Vec<u8>, String> {
    let mut lines = text.lines();

    let header_line = lines.next()
        .ok_or("Empty G text file")?;
    let header = GtHeader::parse(header_line)?;

    dispatch_decode(lines, &header)
}

fn dispatch_decode<'a>(lines: impl Iterator<Item=&'a str>, header: &GtHeader) -> Result<Vec<u8>, String> {
    match (header.major, header.minor, header.patch) {
        (0, 1, 0) => decode_v010(lines, header),
        _ => Err(format!("Unsupported G text version {}.{}.{} — please update your G installation",
            header.major, header.minor, header.patch)),
    }
}

fn decode_v010<'a>(lines: impl Iterator<Item=&'a str>, header: &GtHeader) -> Result<Vec<u8>, String> {
    let n = header.n;
    let table = IndexTable::new(n);
    let bits = table.bits;
    let charset: Vec<char> = table.charset.iter().map(|g| g.ch).collect();

    let mut writer = BitWriter::new();
    let mut bits_written = 0u64;
    let original_bits = header.original_bytes * 8;
    let mut block_count = 0u64;

    for (line_num, line) in lines.enumerate() {
        let line = line.trim();
        if line.is_empty() { continue; }

        // Validate block length
        let chars: Vec<char> = line.chars().collect();
        if chars.len() != n {
            return Err(format!("Line {}: block has {} chars, expected {} (n={})",
                line_num + 2, chars.len(), n, n));
        }

        // Validate all chars are in charset
        for (ci, &c) in chars.iter().enumerate() {
            if !charset.contains(&c) {
                return Err(format!("Line {}, char {}: '{}' not in G{} charset",
                    line_num + 2, ci, c, n));
        }
        }

        // Validate adjacency rules
        if !is_valid_block(&chars) {
            return Err(format!("Line {}: adjacency rule violation in block {:?}",
                line_num + 2, line));
        }

        // Encode block → index → bits
        let idx = table.encode_block(&chars)?;

        // Write only the bits we still need
        let remaining = original_bits.saturating_sub(bits_written);
        if remaining == 0 { break; }
        let bits_to_write = bits.min(remaining as u32);
        if bits_to_write < bits {
            let shifted = idx >> (bits - bits_to_write);
            writer.write_bits(shifted, bits_to_write);
        } else {
            writer.write_bits(idx, bits);
        }
        bits_written += bits_to_write as u64;
        block_count += 1;
    }

    let mut out = writer.finish();
    out.truncate(header.original_bytes as usize);
    Ok(out)
}

// ── Validate a .gt file ────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ValidationResult {
    pub valid: bool,
    pub header: Option<GtHeader>,
    pub block_count: usize,
    pub errors: Vec<String>,
}

pub fn validate_text(text: &str) -> ValidationResult {
    let mut errors = Vec::new();
    let mut lines = text.lines();

    let header = match lines.next() {
        None => {
            errors.push("Empty file".to_string());
            return ValidationResult { valid: false, header: None, block_count: 0, errors };
        }
        Some(l) => match GtHeader::parse(l) {
            Ok(h) => h,
            Err(e) => {
                errors.push(e);
                return ValidationResult { valid: false, header: None, block_count: 0, errors };
            }
        }
    };

    let n = header.n;
    let table = IndexTable::new(n);
    let charset: Vec<char> = table.charset.iter().map(|g| g.ch).collect();
    let mut block_count = 0;

    for (line_num, line) in lines.enumerate() {
        let line = line.trim();
        if line.is_empty() { continue; }

        let chars: Vec<char> = line.chars().collect();

        if chars.len() != n {
            errors.push(format!("Line {}: expected {} chars, got {}",
                line_num + 2, n, chars.len()));
            continue;
        }

        for (ci, &c) in chars.iter().enumerate() {
            if !charset.contains(&c) {
                errors.push(format!("Line {}, pos {}: '{}' not in G{} charset",
                    line_num + 2, ci, c, n));
            }
        }

        if !is_valid_block(&chars) {
            errors.push(format!("Line {}: adjacency violation", line_num + 2));
        }

        block_count += 1;
    }

    ValidationResult {
        valid: errors.is_empty(),
        header: Some(header),
        block_count,
        errors,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = GtHeader::new(28, 12345);
        let s = h.to_string();
        assert_eq!(s, "Gn/0/1/0/28/12345");
        let h2 = GtHeader::parse(&s).unwrap();
        assert_eq!(h, h2);
    }

    #[test]
    fn header_parse_versions() {
        // Future version
        let h = GtHeader::parse("Gn/1/2/3/16/999").unwrap();
        assert_eq!(h.major, 1);
        assert_eq!(h.minor, 2);
        assert_eq!(h.patch, 3);
        assert_eq!(h.n, 16);
        assert_eq!(h.original_bytes, 999);
    }

    #[test]
    fn header_invalid() {
        assert!(GtHeader::parse("base64abc").is_err());
        assert!(GtHeader::parse("Gn/0/1").is_err());
        assert!(GtHeader::parse("Gn/x/1/0/28/100").is_err());
    }

    #[test]
    fn empty_roundtrip() {
        let data: &[u8] = b"";
        let text = encode_to_text(data, 28).unwrap();
        let restored = decode_from_text(&text).unwrap();
        assert_eq!(restored, data);
    }

    #[test]
    fn text_roundtrip_g4() {
        let data = b"hello world";
        let text = encode_to_text(data, 4).unwrap();
        println!("G4 text:\n{}", text);
        let restored = decode_from_text(&text).unwrap();
        assert_eq!(restored, data);
    }

    #[test]
    fn text_roundtrip_g28() {
        let data = b"the quick brown fox jumps over the lazy dog";
        let text = encode_to_text(data, 28).unwrap();
        println!("G28 text:\n{}", text);
        let restored = decode_from_text(&text).unwrap();
        assert_eq!(restored, data);
    }

    #[test]
    fn text_is_readable() {
        let data = b"hello";
        let text = encode_to_text(data, 4).unwrap();
        // Every char in the body lines should be printable ASCII
        for line in text.lines().skip(1) {
            for c in line.chars() {
                assert!(c.is_ascii() && !c.is_ascii_control(),
                    "non-printable char in G text: {:?}", c);
            }
        }
    }

    #[test]
    fn text_blocks_are_valid_g() {
        let data = b"testing G canonical text form";
        let text = encode_to_text(data, 28).unwrap();
        let result = validate_text(&text);
        assert!(result.valid, "validation errors: {:?}", result.errors);
    }

    #[test]
    fn backwards_compat_future_version() {
        // A file from a future version should fail with a clear error
        let future = "Gn/99/0/0/28/5\nhello\n";
        let err = decode_from_text(future).unwrap_err();
        assert!(err.contains("Unsupported"), "expected unsupported version error, got: {}", err);
    }

    #[test]
    fn odd_length_roundtrip() {
        for len in [1, 3, 7, 13, 57, 100, 255] {
            let data: Vec<u8> = (0..len).map(|i| (i * 17 % 256) as u8).collect();
            let text = encode_to_text(&data, 28).unwrap();
            let restored = decode_from_text(&text).unwrap();
            assert_eq!(restored, data, "failed for len={}", len);
        }
    }

    #[test]
    fn binary_roundtrip() {
        let data: Vec<u8> = (0..=255u8).collect();
        let text = encode_to_text(&data, 28).unwrap();
        let restored = decode_from_text(&text).unwrap();
        assert_eq!(restored, data);
    }

    #[test]
    fn validation_catches_bad_char() {
        let bad = "Gn/0/1/0/4/3\n1aA2\nXYZW\n"; // X not in G4 charset
        let result = validate_text(bad);
        assert!(!result.valid);
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn validation_catches_wrong_length() {
        let bad = "Gn/0/1/0/4/3\n1aA2\n1a\n"; // block too short
        let result = validate_text(bad);
        assert!(!result.valid);
    }
}
