//! Dispatch — detect data type, choose compression method, handle fallback.
//!
//! Grammar model is tried for source code. When lexer hits Unknown tokens
//! or tokens the grammar can't predict, those segments fall back to cmix.
//!
//! Format: [method:u8][original_len:u64LE][payload]
//!
//! Grammar payload: [n_segs:u32][seg_type:u8 + orig_len:u32 + comp_len:u32 + bytes...]
//! cmix payload:    raw cmix stream

use super::cmix::{CmixConfig, compress_bytes, decompress_bytes};
use super::codec as gc;
use super::model::GrammarModel;

// ── Data type detection ────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DataType {
    Source, Structured, Text, Binary, Compressed,
}

fn ext(path: &str) -> &str {
    path.rsplit('.').next().map(|e| e).unwrap_or("")
}

pub fn detect(path: &str, data: &[u8]) -> DataType {
    if is_compressed_magic(data) { return DataType::Compressed; }
    match ext(path).to_ascii_lowercase().as_str() {
        "js"|"ts"|"jsx"|"tsx"|"mjs"|"cjs"|
        "rs"|"py"|"pyw"|"go"|"c"|"h"|"cpp"|"cc"|"cxx"|"hpp"|
        "java"|"kt"|"swift"|"cs"|"fs"|"rb"|"php"|"lua"|
        "sh"|"bash"|"zsh"|"fish"|"ps1"|"sql"|"graphql"|"proto"|
        "ex"|"exs"|"dart"|"zig"|"nim"|"scala"|"hs" => DataType::Source,

        "json"|"json5"|"jsonc"|"yaml"|"yml"|"toml"|"ini"|"cfg"|
        "xml"|"html"|"htm"|"svg"|"vue"|"svelte"|"css"|"scss"|"less" => DataType::Structured,

        "md"|"mdx"|"txt"|"rst"|"log"|"csv"|"tsv"|"tex" => DataType::Text,

        "gz"|"br"|"zst"|"xz"|"bz2"|"lz4"|"zip"|"7z"|"rar"|
        "png"|"jpg"|"jpeg"|"gif"|"webp"|"avif"|
        "mp3"|"mp4"|"mkv"|"webm"|"flac"|"wav"|
        "wasm"|"pdf"|"ttf"|"otf"|"woff"|"woff2"|
        "retype"|"rnce" => DataType::Compressed,

        _ => sniff(data),
    }
}

fn is_compressed_magic(data: &[u8]) -> bool {
    if data.len() < 4 { return false; }
    let m = &data[..4];
    matches!(m,
        [0x1f,0x8b,..]       | // gzip
        [0x28,0xb5,0x2f,0xfd]| // zstd
        [0x50,0x4b,0x03,0x04]| // zip
        [0xfd,0x37,0x7a,0x58]| // xz
        [0x89,0x50,0x4e,0x47]| // png
        [0xff,0xd8,0xff,..]   | // jpg
        [0x47,0x49,0x46,0x38]| // gif
        [0x25,0x50,0x44,0x46]| // pdf
        [0x7f,0x45,0x4c,0x46]| // elf
        [0x00,0x61,0x73,0x6d]  // wasm
    ) || (data.len() >= 12 && &data[8..12] == b"WEBP")
}

fn sniff(data: &[u8]) -> DataType {
    let sample = &data[..data.len().min(512)];
    let nulls = sample.iter().filter(|&&b| b == 0).count();
    let high  = sample.iter().filter(|&&b| b > 127).count();
    if nulls > sample.len() / 16 || high > sample.len() / 3 { DataType::Binary }
    else { DataType::Text }
}

// ── Method codes ───────────────────────────────────────────────────────────────

const M_GRAMMAR: u8 = 0x01; // grammar+cmix fallback segments
const M_CMIX_S:  u8 = 0x02; // cmix source config
const M_CMIX_T:  u8 = 0x03; // cmix text config
const M_CMIX_B:  u8 = 0x04; // cmix binary config
const M_RAW:     u8 = 0xFF;

const SEG_G: u8 = 0x01; // grammar segment
const SEG_C: u8 = 0x02; // cmix fallback segment

// ── Public API ────────────────────────────────────────────────────────────────

pub fn compress(data: &[u8], path: &str) -> Vec<u8> {
    if data.is_empty() { return make_raw(&[]); }

    let dtype = detect(path, data);
    if dtype == DataType::Compressed { return make_raw(data); }

    let (method, payload) = match dtype {
        DataType::Source => {
            // Grammar needs warmup — only beneficial on files > 8KB
            // Below that, cmix wins due to fixed overhead in grammar format
            let c = compress_bytes(data, CmixConfig::source());
            if data.len() >= 8192 {
                let g = grammar_compress(data, path);
                if g.len() < c.len() { (M_GRAMMAR, g) } else { (M_CMIX_S, c) }
            } else {
                (M_CMIX_S, c)
            }
        }
        DataType::Structured => (M_CMIX_S, compress_bytes(data, CmixConfig::structured())),
        DataType::Text       => (M_CMIX_T, compress_bytes(data, CmixConfig::text())),
        DataType::Binary     => (M_CMIX_B, compress_bytes(data, CmixConfig::binary())),
        DataType::Compressed => unreachable!(),
    };

    // Never expand — compare total output size (header + payload) vs raw store (header + data)
    let total_compressed = 9 + payload.len();
    let total_raw = 9 + data.len();
    if total_compressed >= total_raw { return make_raw(data); }

    let mut out = Vec::with_capacity(total_compressed);
    out.push(method);
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

pub fn decompress(data: &[u8], path: &str) -> Vec<u8> {
    if data.len() < 9 { return data.to_vec(); }
    let method = data[0];
    let orig_len = u64::from_le_bytes(data[1..9].try_into().unwrap()) as usize;
    let payload = &data[9..];

    match method {
        M_RAW    => payload[..orig_len.min(payload.len())].to_vec(),
        M_GRAMMAR => grammar_decompress(payload, path),
        M_CMIX_S => decompress_bytes(payload, orig_len, CmixConfig::source()),
        M_CMIX_T => decompress_bytes(payload, orig_len, CmixConfig::text()),
        M_CMIX_B => decompress_bytes(payload, orig_len, CmixConfig::binary()),
        _        => vec![],
    }
}

fn make_raw(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(9 + data.len());
    out.push(M_RAW);
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    out.extend_from_slice(data);
    out
}

// ── Grammar segmentation ───────────────────────────────────────────────────────

/// Segment: grammar-parseable tokens → grammar codec
///          Unknown/unparseable tokens → cmix fallback
fn grammar_compress(data: &[u8], path: &str) -> Vec<u8> {
    use super::lexer::Lexer;
    use super::lang::detect_language;
    use super::token::TokenKind;

    let lang = detect_language(path);
    let tokens = Lexer::new(data, lang).tokenize();

    // Verify round-trip — if lexer broke, find where and use segment-level fallback
    // instead of discarding grammar compression for the entire file.
    let reassembled: Vec<u8> = tokens.iter().flat_map(|t| t.raw.iter().cloned()).collect();
    if reassembled != data {
        // Find the first divergence point
        let split = reassembled.iter().zip(data.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(reassembled.len().min(data.len()));

        // Anything before split is good — compress with grammar if large enough
        // The broken tail gets cmix
        if split > 256 {
            let good = &data[..split];
            let bad  = &data[split..];
            let mut segs: Vec<(u8, Vec<u8>, Vec<u8>)> = Vec::new();

            // Re-run grammar on the good prefix
            let good_tokens: Vec<_> = Lexer::new(good, lang).tokenize();
            let good_reassembled: Vec<u8> = good_tokens.iter().flat_map(|t| t.raw.iter().cloned()).collect();
            if good_reassembled == good {
                // Grammar works on prefix — compress it
                let mut m = GrammarModel::new(path);
                let g = gc::encode(good, path, &mut m);
                segs.push((SEG_G, good.to_vec(), g));
            } else {
                segs.push((SEG_C, good.to_vec(), compress_bytes(good, CmixConfig::source())));
            }

            // Broken suffix → cmix
            if !bad.is_empty() {
                segs.push((SEG_C, bad.to_vec(), compress_bytes(bad, CmixConfig::source())));
            }
            return encode_segs(&segs);
        }

        // Short file with broken lexer — just cmix the whole thing
        let c = compress_bytes(data, CmixConfig::source());
        return encode_segs(&[(SEG_C, data.to_vec(), c)]);
    }

    // Classify each token
    let mut segs: Vec<(u8, Vec<u8>, Vec<u8>)> = Vec::new(); // (type, raw, compressed)
    let mut g_raw: Vec<u8> = Vec::new();
    let mut c_raw: Vec<u8> = Vec::new();
    let mut in_grammar = true;

    let flush_g = |g_raw: &mut Vec<u8>, segs: &mut Vec<(u8, Vec<u8>, Vec<u8>)>, path: &str| {
        if g_raw.is_empty() { return; }
        let raw = g_raw.clone();
        let mut m = GrammarModel::new(path);
        let comp = gc::encode(&raw, path, &mut m);
        segs.push((SEG_G, raw, comp));
        g_raw.clear();
    };

    let flush_c = |c_raw: &mut Vec<u8>, segs: &mut Vec<(u8, Vec<u8>, Vec<u8>)>| {
        if c_raw.is_empty() { return; }
        let raw = c_raw.clone();
        let comp = compress_bytes(&raw, CmixConfig::source());
        segs.push((SEG_C, raw, comp));
        c_raw.clear();
    };

    for token in &tokens {
        // Grammar-friendly: anything the grammar model handles better than raw bytes.
        // Unknown tokens (whitespace the lexer couldn't classify, weird chars) → cmix.
        // Very long tokens (>128 bytes string literals) → cmix (byte model wins there).
        let grammar_friendly = match token.kind {
            TokenKind::Unknown => false,
            _ if token.raw.len() > 128 => false,
            _ => true,
        };

        if grammar_friendly {
            if !in_grammar {
                flush_c(&mut c_raw, &mut segs);
                in_grammar = true;
            }
            g_raw.extend_from_slice(&token.raw);
        } else {
            if in_grammar {
                flush_g(&mut g_raw, &mut segs, path);
                in_grammar = false;
            }
            c_raw.extend_from_slice(&token.raw);
        }
    }
    flush_g(&mut g_raw, &mut segs, path);
    flush_c(&mut c_raw, &mut segs);

    // Consolidate: if only one segment and it's cmix, just return cmix bytes
    if segs.len() == 1 && segs[0].0 == SEG_C {
        return encode_segs(&segs);
    }

    encode_segs(&segs)
}

fn grammar_decompress(data: &[u8], path: &str) -> Vec<u8> {
    let segs = decode_segs(data);
    let mut out = Vec::new();
    for (seg_type, comp, orig_len) in segs {
        match seg_type {
            SEG_G => {
                let mut m = GrammarModel::new(path);
                out.extend_from_slice(&gc::decode(&comp, path, &mut m));
            }
            SEG_C => {
                out.extend_from_slice(&decompress_bytes(&comp, orig_len, CmixConfig::source()));
            }
            _ => {}
        }
    }
    out
}

// ── Segment wire format ────────────────────────────────────────────────────────
// [n_segs: u32]
// per segment: [type:u8][orig_len:u32][comp_len:u32][comp_bytes]

fn encode_segs(segs: &[(u8, Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(segs.len() as u32).to_le_bytes());
    for (seg_type, raw, comp) in segs {
        out.push(*seg_type);
        out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        out.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        out.extend_from_slice(comp);
    }
    out
}

fn decode_segs(data: &[u8]) -> Vec<(u8, Vec<u8>, usize)> {
    if data.len() < 4 { return vec![]; }
    let n = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    let mut pos = 4;
    let mut out = Vec::new();
    for _ in 0..n {
        if pos + 9 > data.len() { break; }
        let seg_type = data[pos]; pos += 1;
        let orig_len = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap()) as usize; pos += 4;
        let comp_len = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap()) as usize; pos += 4;
        if pos + comp_len > data.len() { break; }
        let comp = data[pos..pos+comp_len].to_vec(); pos += comp_len;
        out.push((seg_type, comp, orig_len));
    }
    out
}
