//! Language-agnostic lexer — converts raw bytes to tokens.
//!
//! The lexer doesn't need to be perfect. It needs to be:
//!   1. Consistent — same input always produces same tokens
//!   2. Reversible — tokens can be reassembled into original bytes exactly
//!   3. Fast — it runs on every file during compression
//!
//! We use a simple hand-written lexer that handles the common cases
//! across all supported languages.

use super::token::{Token, TokenKind};
use super::grammar::LanguageGrammar;

pub struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
    lang: &'static dyn LanguageGrammar,
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8], lang: &'static dyn LanguageGrammar) -> Self {
        Self { data, pos: 0, lang }
    }

    pub fn next(&mut self) -> Option<Token> {
        if self.pos >= self.data.len() { return None; }

        let start = self.pos;
        let b = self.data[self.pos];

        // Newline
        if b == b'\n' {
            self.pos += 1;
            if self.pos < self.data.len() && self.data[self.pos] == b'\r' { self.pos += 1; }
            return Some(Token::new(TokenKind::Newline, self.data[start..self.pos].to_vec()));
        }
        if b == b'\r' {
            self.pos += 1;
            if self.pos < self.data.len() && self.data[self.pos] == b'\n' { self.pos += 1; }
            return Some(Token::new(TokenKind::Newline, self.data[start..self.pos].to_vec()));
        }

        // Whitespace (non-newline)
        if b == b' ' || b == b'\t' {
            while self.pos < self.data.len()
                && (self.data[self.pos] == b' ' || self.data[self.pos] == b'\t')
                && self.data[self.pos] != b'\n'
            { self.pos += 1; }
            return Some(Token::new(TokenKind::Unknown, self.data[start..self.pos].to_vec()));
            // Whitespace: stored as Unknown, the transform layer handles it
        }

        // Line comment //
        if b == b'/' && self.pos + 1 < self.data.len() && self.data[self.pos+1] == b'/' {
            while self.pos < self.data.len() && self.data[self.pos] != b'\n' { self.pos += 1; }
            return Some(Token::new(TokenKind::LineComment, self.data[start..self.pos].to_vec()));
        }

        // Block comment /* */
        if b == b'/' && self.pos + 1 < self.data.len() && self.data[self.pos+1] == b'*' {
            self.pos += 2;
            while self.pos + 1 < self.data.len()
                && !(self.data[self.pos] == b'*' && self.data[self.pos+1] == b'/') {
                self.pos += 1;
            }
            if self.pos + 1 < self.data.len() { self.pos += 2; }
            return Some(Token::new(TokenKind::BlockComment, self.data[start..self.pos].to_vec()));
        }

        // Hash comment # (Python, shell)
        if b == b'#' {
            while self.pos < self.data.len() && self.data[self.pos] != b'\n' { self.pos += 1; }
            return Some(Token::new(TokenKind::LineComment, self.data[start..self.pos].to_vec()));
        }

        // String literal
        if b == b'"' || b == b'\'' || b == b'`' {
            let quote = b;
            self.pos += 1;
            // Triple-quoted strings (Python """ or \'\'\'): consume as one token
            if b != b'`'
                && self.pos + 1 < self.data.len()
                && self.data[self.pos] == quote
                && self.data[self.pos + 1] == quote
            {
                self.pos += 2; // skip the second and third quote
                // Scan until matching triple-close
                while self.pos + 2 < self.data.len() {
                    if self.data[self.pos] == quote
                        && self.data[self.pos+1] == quote
                        && self.data[self.pos+2] == quote
                    {
                        self.pos += 3;
                        break;
                    }
                    if self.data[self.pos] == b'\\' { self.pos += 1; }
                    self.pos += 1;
                }
            } else {
                while self.pos < self.data.len() && self.data[self.pos] != quote {
                    if self.data[self.pos] == b'\\' { self.pos += 1; }
                    self.pos += 1;
                }
                if self.pos < self.data.len() { self.pos += 1; }
            }
            return Some(Token::new(TokenKind::StringLiteral, self.data[start..self.pos].to_vec()));
        }

        // Number
        if b.is_ascii_digit() || (b == b'-' && self.pos + 1 < self.data.len() && self.data[self.pos+1].is_ascii_digit()) {
            if b == b'-' { self.pos += 1; }
            // Hex
            if self.pos + 1 < self.data.len() && self.data[self.pos] == b'0'
                && (self.data[self.pos+1] == b'x' || self.data[self.pos+1] == b'X') {
                self.pos += 2;
                while self.pos < self.data.len() && self.data[self.pos].is_ascii_hexdigit() { self.pos += 1; }
                return Some(Token::new(TokenKind::IntLiteral, self.data[start..self.pos].to_vec()));
            }
            // Decimal / float
            let mut is_float = false;
            while self.pos < self.data.len()
                && (self.data[self.pos].is_ascii_digit()
                    || self.data[self.pos] == b'_'
                    || self.data[self.pos] == b'.')
            {
                if self.data[self.pos] == b'.' { is_float = true; }
                self.pos += 1;
            }
            // Exponent
            if self.pos < self.data.len() && (self.data[self.pos] == b'e' || self.data[self.pos] == b'E') {
                is_float = true;
                self.pos += 1;
                if self.pos < self.data.len() && (self.data[self.pos] == b'+' || self.data[self.pos] == b'-') {
                    self.pos += 1;
                }
                while self.pos < self.data.len() && self.data[self.pos].is_ascii_digit() { self.pos += 1; }
            }
            // Suffix (u32, f64, etc. in Rust)
            while self.pos < self.data.len() && self.data[self.pos].is_ascii_alphabetic() { self.pos += 1; }
            let kind = if is_float { TokenKind::FloatLiteral } else { TokenKind::IntLiteral };
            return Some(Token::new(kind, self.data[start..self.pos].to_vec()));
        }

        // Identifier or keyword
        if b.is_ascii_alphabetic() || b == b'_' {
            while self.pos < self.data.len()
                && (self.data[self.pos].is_ascii_alphanumeric() || self.data[self.pos] == b'_') {
                self.pos += 1;
            }
            let raw = self.data[start..self.pos].to_vec();
            // Check for bool/null literals
            let kind = match raw.as_slice() {
                b"true" | b"false" | b"True" | b"False" => TokenKind::BoolLiteral,
                b"null" | b"nil" | b"None" | b"nullptr" | b"undefined" => TokenKind::NullLiteral,
                _ => {
                    if self.lang.classify_keyword(&raw).is_some() {
                        TokenKind::Keyword
                    } else {
                        TokenKind::Identifier
                    }
                }
            };
            return Some(Token::new(kind, raw));
        }

        // Multi-char operators
        self.pos += 1;
        let next = if self.pos < self.data.len() { self.data[self.pos] } else { 0 };
        let next2 = if self.pos + 1 < self.data.len() { self.data[self.pos + 1] } else { 0 };

        let (kind, extra) = match (b, next, next2) {
            (b':', b':', _)  => { (TokenKind::DoubleColon, 1) }
            (b'-', b'>', _)  => { (TokenKind::Arrow, 1) }
            (b'=', b'>', _)  => { (TokenKind::FatArrow, 1) }
            (b'=', b'=', _)  => { (TokenKind::EqEq, 1) }
            (b'!', b'=', _)  => { (TokenKind::NotEq, 1) }
            (b'<', b'=', _)  => { (TokenKind::LtEq, 1) }
            (b'>', b'=', _)  => { (TokenKind::GtEq, 1) }
            (b'&', b'&', _)  => { (TokenKind::AndAnd, 1) }
            (b'|', b'|', _)  => { (TokenKind::OrOr, 1) }
            (b'.', b'.', b'.') => { (TokenKind::DotDotDot, 2) }
            (b'.', b'.', _)  => { (TokenKind::DotDot, 1) }
            (b'+', b'=', _)  => { (TokenKind::PlusEq, 1) }
            (b'-', b'=', _)  => { (TokenKind::MinusEq, 1) }
            (b'*', b'=', _)  => { (TokenKind::StarEq, 1) }
            (b'/', b'=', _)  => { (TokenKind::SlashEq, 1) }
            (b'%', b'=', _)  => { (TokenKind::PercentEq, 1) }
            (b'<', b'<', _)  => { (TokenKind::Shl, 1) }
            (b'>', b'>', _)  => { (TokenKind::Shr, 1) }
            (b'(', _, _)     => { (TokenKind::LParen, 0) }
            (b')', _, _)     => { (TokenKind::RParen, 0) }
            (b'{', _, _)     => { (TokenKind::LBrace, 0) }
            (b'}', _, _)     => { (TokenKind::RBrace, 0) }
            (b'[', _, _)     => { (TokenKind::LBracket, 0) }
            (b']', _, _)     => { (TokenKind::RBracket, 0) }
            (b';', _, _)     => { (TokenKind::Semicolon, 0) }
            (b':', _, _)     => { (TokenKind::Colon, 0) }
            (b',', _, _)     => { (TokenKind::Comma, 0) }
            (b'.', _, _)     => { (TokenKind::Dot, 0) }
            (b'+', _, _)     => { (TokenKind::Plus, 0) }
            (b'-', _, _)     => { (TokenKind::Minus, 0) }
            (b'*', _, _)     => { (TokenKind::Star, 0) }
            (b'/', _, _)     => { (TokenKind::Slash, 0) }
            (b'%', _, _)     => { (TokenKind::Percent, 0) }
            (b'=', _, _)     => { (TokenKind::Eq, 0) }
            (b'<', _, _)     => { (TokenKind::Lt, 0) }
            (b'>', _, _)     => { (TokenKind::Gt, 0) }
            (b'&', _, _)     => { (TokenKind::BitAnd, 0) }
            (b'|', _, _)     => { (TokenKind::Pipe, 0) }
            (b'^', _, _)     => { (TokenKind::BitXor, 0) }
            (b'~', _, _)     => { (TokenKind::Tilde, 0) }
            (b'!', _, _)     => { (TokenKind::Not, 0) }
            (b'@', _, _)     => { (TokenKind::At, 0) }
            (b'#', _, _)     => { (TokenKind::Hash, 0) }
            (b'?', _, _)     => { (TokenKind::Question, 0) }
            (b'\\', _, _)    => { (TokenKind::Backslash, 0) }
            _                => { (TokenKind::Unknown, 0) }
        };

        self.pos += extra;
        Some(Token::new(kind, self.data[start..self.pos].to_vec()))
    }

    /// Tokenize entire input
    pub fn tokenize(mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        while let Some(tok) = self.next() { tokens.push(tok); }
        tokens
    }
}
