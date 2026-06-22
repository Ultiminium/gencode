//! RNCE — Grammar-integrated compression with cmix fallback.
//!
//! Pipeline:
//!   detect_type → choose method → grammar+cmix blend → arithmetic code
//!
//! Methods by type:
//!   Source code  → grammar model (token-level) + cmix byte fallback
//!   Structured   → cmix (PPM+match+order1)
//!   Text         → cmix (PPM+order1, no match)
//!   Binary       → cmix (match+order1, no PPM grammar)
//!   Compressed   → store raw (already compressed)

pub mod token;
pub mod grammar;
pub mod freq;
pub mod model;
pub mod lang;
pub mod lexer;
pub mod codec;
pub mod ppm_byte;
pub mod match_model;
pub mod cmix;
pub mod dispatch;
pub mod g_index;

pub use model::GrammarModel;
pub use token::{Token, TokenKind};
pub use dispatch::{compress, decompress};
pub use g_index::{compress_index_stream_exact, decompress_index_stream_exact};

/// Debug: return sizes of both grammar and cmix for a file
pub fn debug_sizes(data: &[u8], path: &str) -> (usize, usize) {
    use dispatch::DataType;
    use cmix::{CmixConfig, compress_bytes};
    
    let dtype = dispatch::detect(path, data);
    if dtype != DataType::Source { return (0, 0); }
    
    // We need to call the internal grammar_compress - let's just measure via compress
    // Try grammar by temporarily forcing it
    let c = compress_bytes(data, CmixConfig::source());
    
    // For grammar, call compress and check method byte
    let full = dispatch::compress(data, path);
    let method = full.first().copied().unwrap_or(0);
    
    (if method == 0x01 { full.len() } else { usize::MAX }, c.len())
}
