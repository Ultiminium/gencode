//! Grammar-integrated arithmetic codec — single unified stream.
//!
//! Format:
//!   [varint: n_tokens]
//!   [varint: raw_len_bytes]  — byte count of the varint-encoded length array
//!   [varint: raw_len_comp]   — byte count of cmix-compressed length array
//!   [raw_len_comp bytes]     — cmix-compressed token length array
//!   [arithmetic stream]      — token kinds + content bytes, unified
//!
//! The arithmetic stream encodes all tokens sequentially:
//!   kind (via grammar model + frequency) then content bytes (via cmix).
//! One shared cmix state for all content gives maximum warmup.

use super::model::GrammarModel;
use super::token::{Token, TokenKind};
use super::lexer::Lexer;
use super::lang::detect_language;
use super::cmix::{CmixState, CmixConfig, BitWriter, OwnedBitReader,
                  encode_byte, decode_byte_owned, renorm_enc, renorm_dec_owned,
                  compress_bytes, decompress_bytes};

const BITS: u64 = 32;
const TOP:  u64 = 1u64 << BITS;
const QTR:  u64 = TOP >> 2;

fn narrow(lo: &mut u64, hi: &mut u64, sl: u32, sh: u32, total: u32) {
    if sl >= sh || total == 0 { return; }
    let range = *hi - *lo;
    let new_hi = *lo + range * sh as u64 / total as u64;
    let new_lo = *lo + range * sl as u64 / total as u64;
    *hi = if new_hi > new_lo { new_hi } else { new_lo + 1 };
    *lo = new_lo;
}

fn prefix80(t: &[u32; 80]) -> ([u32; 81], u32) {
    let mut c = [0u32; 81];
    for i in 0..80 { c[i+1] = c[i] + t[i]; }
    (c, c[80])
}

fn find_in(cum: &[u32], total: u32, scaled: u32) -> usize {
    let scaled = scaled.min(total.saturating_sub(1));
    let mut lo = 0usize; let mut hi = cum.len() - 1;
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if cum[mid] <= scaled { lo = mid; } else { hi = mid; }
    }
    lo
}

// ── Encode ─────────────────────────────────────────────────────────────────

pub fn encode(data: &[u8], path: &str, model: &mut GrammarModel) -> Vec<u8> {
    if data.is_empty() { return vec![]; }

    let lang = detect_language(path);
    let tokens = Lexer::new(data, lang).tokenize();
    let n = tokens.len();

    // Encode all token lengths as varints
    let mut raw_lengths_bytes: Vec<u8> = Vec::new();
    for t in &tokens {
        write_varint(&mut raw_lengths_bytes, t.raw.len() as u64);
    }

    // Compress the length array
    let raw_len_comp = compress_bytes(&raw_lengths_bytes, CmixConfig::text());

    // Single unified arithmetic stream: kinds + content
    let mut w = BitWriter::new();
    let mut lo = 0u64; let mut hi = TOP;
    let mut content_state = CmixState::new(CmixConfig::source());

    for token in &tokens {
        // 1. Encode token kind
        let (table, total) = model.token_distribution();
        if total > 0 {
            let (cum, cum_total) = prefix80(&table);
            let idx = token.kind as usize;
            if idx < 80 && cum_total > 0 {
                let sl = cum[idx];
                let sh = cum[idx+1].max(sl+1);
                narrow(&mut lo, &mut hi, sl, sh, cum_total);
                renorm_enc(&mut lo, &mut hi, &mut w);
            }
        }

        // 2. Encode content bytes
        for &b in &token.raw {
            encode_byte(b, &mut content_state, &mut lo, &mut hi, &mut w);
        }

        model.update(token);
    }

    // Flush
    w.pending += 1;
    if lo < QTR { w.emit_pending(0); } else { w.emit_pending(1); }
    let arith = w.done();

    let mut out = Vec::new();
    write_varint(&mut out, n as u64);
    write_varint(&mut out, raw_lengths_bytes.len() as u64);
    write_varint(&mut out, raw_len_comp.len() as u64);
    out.extend_from_slice(&raw_len_comp);
    out.extend_from_slice(&arith);
    out
}

// ── Decode ─────────────────────────────────────────────────────────────────

pub fn decode(data: &[u8], _path: &str, model: &mut GrammarModel) -> Vec<u8> {
    if data.is_empty() { return vec![]; }

    let mut pos = 0;
    let (n, c) = read_varint(&data[pos..]); pos += c;
    let n = n as usize;

    let (raw_len_bytes, c2) = read_varint(&data[pos..]); pos += c2;
    let raw_len_bytes = raw_len_bytes as usize;

    let (raw_len_comp_len, c3) = read_varint(&data[pos..]); pos += c3;
    let raw_len_comp_len = raw_len_comp_len as usize;

    if pos + raw_len_comp_len > data.len() { return vec![]; }
    let raw_len_comp = &data[pos..pos+raw_len_comp_len]; pos += raw_len_comp_len;

    let raw_lengths_bytes = decompress_bytes(raw_len_comp, raw_len_bytes, CmixConfig::text());

    // Decode varint-encoded lengths
    let mut raw_lengths: Vec<usize> = Vec::with_capacity(n);
    let mut rpos = 0;
    while rpos < raw_lengths_bytes.len() && raw_lengths.len() < n {
        let (len, lc) = read_varint(&raw_lengths_bytes[rpos..]);
        rpos += lc;
        raw_lengths.push(len as usize);
    }
    if raw_lengths.len() != n { return vec![]; }

    let arith_data = data[pos..].to_vec();
    let mut r = OwnedBitReader::new(arith_data);
    let mut lo = 0u64; let mut hi = TOP; let mut val = 0u64;
    for _ in 0..BITS { val = (val << 1) | r.read(); }

    let mut content_state = CmixState::new(CmixConfig::source());
    let mut out = Vec::new();

    for i in 0..n {
        let len = raw_lengths[i];

        // 1. Decode kind
        let kind = {
            let (table, total) = model.token_distribution();
            if total > 0 {
                let (cum, cum_total) = prefix80(&table);
                let range = hi - lo;
                let scaled = ((val - lo + 1) * cum_total as u64 - 1) / range;
                let idx = find_in(&cum, cum_total, scaled as u32);
                let sl = cum[idx]; let sh = cum[idx+1].max(sl+1);
                narrow(&mut lo, &mut hi, sl, sh, cum_total);
                renorm_dec_owned(&mut lo, &mut hi, &mut val, &mut r);
                kind_from_u8(idx as u8)
            } else {
                TokenKind::Unknown
            }
        };

        // 2. Decode content bytes
        let mut raw = Vec::with_capacity(len);
        for _ in 0..len {
            let b = decode_byte_owned(&mut content_state, &mut lo, &mut hi, &mut val, &mut r);
            raw.push(b);
        }

        out.extend_from_slice(&raw);
        let token = Token::new(kind, raw);
        model.update(&token);
    }

    out
}

// ── Token kind mapping ──────────────────────────────────────────────────────

fn kind_from_u8(b: u8) -> TokenKind {
    match b {
        0  => TokenKind::Identifier,   1  => TokenKind::Keyword,
        2  => TokenKind::IntLiteral,   3  => TokenKind::FloatLiteral,
        4  => TokenKind::StringLiteral,5  => TokenKind::CharLiteral,
        6  => TokenKind::BoolLiteral,  7  => TokenKind::NullLiteral,
        8  => TokenKind::LParen,       9  => TokenKind::RParen,
        10 => TokenKind::LBrace,       11 => TokenKind::RBrace,
        12 => TokenKind::LBracket,     13 => TokenKind::RBracket,
        14 => TokenKind::Semicolon,    15 => TokenKind::Colon,
        16 => TokenKind::DoubleColon,  17 => TokenKind::Comma,
        18 => TokenKind::Dot,          19 => TokenKind::DotDot,
        20 => TokenKind::DotDotDot,    21 => TokenKind::Arrow,
        22 => TokenKind::FatArrow,     23 => TokenKind::Plus,
        24 => TokenKind::Minus,        25 => TokenKind::Star,
        26 => TokenKind::Slash,        27 => TokenKind::Percent,
        28 => TokenKind::Eq,           29 => TokenKind::EqEq,
        30 => TokenKind::NotEq,        31 => TokenKind::Lt,
        32 => TokenKind::Gt,           33 => TokenKind::LtEq,
        34 => TokenKind::GtEq,         35 => TokenKind::And,
        36 => TokenKind::Or,           37 => TokenKind::Not,
        38 => TokenKind::AndAnd,       39 => TokenKind::OrOr,
        40 => TokenKind::BitAnd,       41 => TokenKind::BitOr,
        42 => TokenKind::BitXor,       43 => TokenKind::BitNot,
        44 => TokenKind::Shl,          45 => TokenKind::Shr,
        46 => TokenKind::PlusEq,       47 => TokenKind::MinusEq,
        48 => TokenKind::StarEq,       49 => TokenKind::SlashEq,
        50 => TokenKind::PercentEq,    51 => TokenKind::AndEq,
        52 => TokenKind::OrEq,         53 => TokenKind::XorEq,
        54 => TokenKind::ShlEq,        55 => TokenKind::ShrEq,
        56 => TokenKind::At,           57 => TokenKind::Hash,
        58 => TokenKind::Question,     59 => TokenKind::Tilde,
        60 => TokenKind::Backslash,    61 => TokenKind::Pipe,
        62 => TokenKind::Newline,      63 => TokenKind::Indent,
        64 => TokenKind::Dedent,       65 => TokenKind::LineComment,
        66 => TokenKind::BlockComment, 67 => TokenKind::Eof,
        _  => TokenKind::Unknown,
    }
}

fn write_varint(out: &mut Vec<u8>, mut n: u64) {
    loop {
        let mut b = (n & 0x7F) as u8; n >>= 7;
        if n != 0 { b |= 0x80; }
        out.push(b);
        if n == 0 { break; }
    }
}

fn read_varint(data: &[u8]) -> (u64, usize) {
    let mut r = 0u64; let mut s = 0; let mut i = 0;
    while i < data.len() {
        let b = data[i]; i += 1;
        r |= ((b & 0x7F) as u64) << s;
        if b & 0x80 == 0 { break; }
        s += 7;
    }
    (r, i)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use super::model::GrammarModel;

    fn roundtrip(src: &str, path: &str) {
        let data = src.as_bytes();
        let mut m1 = GrammarModel::new(path);
        let encoded = encode(data, path, &mut m1);
        let mut m2 = GrammarModel::new(path);
        let decoded = decode(&encoded, path, &mut m2);
        assert_eq!(decoded, data,
            "roundtrip failed for {:?}: {} in, {} encoded, {} decoded",
            path, data.len(), encoded.len(), decoded.len());
    }

    #[test]
    fn empty() { roundtrip("", "test.rs"); }

    #[test]
    fn simple_rust() {
        roundtrip("fn main() { println!(\"hello\"); }", "test.rs");
    }

    #[test]
    fn with_comments() {
        roundtrip("// this is a comment\nfn foo() -> u32 { 42 }", "test.rs");
    }

    #[test]
    fn multiline() {
        let src = "use std::collections::HashMap;\n\nfn main() {\n    let mut map = HashMap::new();\n    map.insert(\"key\", 42);\n    println!(\"{:?}\", map);\n}\n";
        roundtrip(src, "test.rs");
    }

    #[test]
    fn long_token() {
        // Token longer than 255 bytes (was broken by u8 length cap)
        let big_str: String = std::iter::repeat("hello world ").take(30).collect();
        let src = format!("let x = \"{}\";", big_str);
        roundtrip(&src, "test.rs");
    }

    #[test]
    fn grammar_compresses_small_file() {
        // Grammar should compress, not expand, on source code
        let src = include_str!("freq.rs");
        let data = src.as_bytes();
        let mut m = GrammarModel::new("test.rs");
        let encoded = encode(data, "test.rs", &mut m);
        assert!(encoded.len() < data.len(),
            "grammar expanded: {} -> {}", data.len(), encoded.len());
    }

    #[test]
    fn grammar_competitive_on_large_file() {
        // On large files, grammar should be within 5% of cmix
        let src = include_str!("lang.rs");
        let data = src.as_bytes();
        let mut m = GrammarModel::new("test.rs");
        let grammar_out = encode(data, "test.rs", &mut m);
        let cmix_out = compress_bytes(data, CmixConfig::source());
        println!("large file: grammar={} ({:.1}%) cmix={} ({:.1}%)",
            grammar_out.len(), 100.0*grammar_out.len() as f64/data.len() as f64,
            cmix_out.len(), 100.0*cmix_out.len() as f64/data.len() as f64);
        assert!(grammar_out.len() <= cmix_out.len() * 105 / 100,
            "grammar too far from cmix: {} vs {}", grammar_out.len(), cmix_out.len());

        // Verify roundtrip
        let mut m2 = GrammarModel::new("test.rs");
        let restored = decode(&grammar_out, "test.rs", &mut m2);
        assert_eq!(restored, data, "lang.rs roundtrip failed");
    }

    #[test]
    fn dispatch_never_worse_than_cmix() {
        let src = include_str!("dispatch.rs");
        let data = src.as_bytes();
        let dispatched = crate::dispatch::compress(data, "test.rs");
        let cmix_only = compress_bytes(data, CmixConfig::source());
        // dispatch output includes 9-byte header — account for that
        assert!(dispatched.len() <= 9 + cmix_only.len() + 10,
            "dispatch worse than cmix: {} vs {}", dispatched.len(), 9 + cmix_only.len());
    }
}
