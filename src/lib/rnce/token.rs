//! Token types — language-agnostic representation of source code tokens.
//!
//! Every language maps to these kinds. The model operates on TokenKinds,
//! not raw bytes. This is what gives us the grammar constraint.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // Structure
    Identifier,      // any name: foo, myVar, MyStruct
    Keyword,         // language keyword: fn, if, class, def
    
    // Literals
    IntLiteral,      // 42, 0xFF, 0b1010
    FloatLiteral,    // 3.14, 1e-5
    StringLiteral,   // "hello", 'world', `template`
    CharLiteral,     // 'a' in languages that distinguish
    BoolLiteral,     // true, false
    NullLiteral,     // null, nil, None, nullptr
    
    // Delimiters
    LParen,          // (
    RParen,          // )
    LBrace,          // {
    RBrace,          // }
    LBracket,        // [
    RBracket,        // ]
    Semicolon,       // ;
    Colon,           // :
    DoubleColon,     // ::
    Comma,           // ,
    Dot,             // .
    DotDot,          // ..
    DotDotDot,       // ...
    Arrow,           // ->
    FatArrow,        // =>
    
    // Operators
    Plus, Minus, Star, Slash, Percent,
    Eq, EqEq, NotEq, Lt, Gt, LtEq, GtEq,
    And, Or, Not, AndAnd, OrOr,
    BitAnd, BitOr, BitXor, BitNot, Shl, Shr,
    PlusEq, MinusEq, StarEq, SlashEq, PercentEq,
    AndEq, OrEq, XorEq, ShlEq, ShrEq,
    At,              // @ (decorators, macros)
    Hash,            // # (preprocessor, attributes)
    Question,        // ?
    Tilde,           // ~
    Backslash,       // \
    Pipe,            // | (closures in Rust, alternatives in patterns)
    
    // Whitespace (significant in Python)
    Newline,
    Indent,          // increase in indentation
    Dedent,          // decrease in indentation
    
    // Comments (deduplicated by transform, but model needs to know they exist)
    LineComment,
    BlockComment,
    
    // Special
    Eof,
    Unknown,         // anything the lexer can't classify
}

impl TokenKind {
    /// How many distinct token kinds exist
    pub const COUNT: usize = 64; // generous upper bound

    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn is_expression_start(&self) -> bool {
        matches!(self,
            TokenKind::Identifier | TokenKind::IntLiteral | TokenKind::FloatLiteral |
            TokenKind::StringLiteral | TokenKind::BoolLiteral | TokenKind::NullLiteral |
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace |
            TokenKind::Minus | TokenKind::Not | TokenKind::BitNot |
            TokenKind::Star | TokenKind::BitAnd // unary * and &
        )
    }

    pub fn is_statement_start(&self) -> bool {
        matches!(self,
            TokenKind::Keyword | TokenKind::Identifier |
            TokenKind::LBrace | TokenKind::Semicolon
        ) || self.is_expression_start()
    }

    pub fn is_type_start(&self) -> bool {
        matches!(self,
            TokenKind::Identifier | TokenKind::LParen | TokenKind::LBracket |
            TokenKind::Star | TokenKind::BitAnd | TokenKind::Question
        )
    }
}

/// A token with its kind and the raw bytes it came from
#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub raw: Vec<u8>,  // original bytes — needed for reconstruction
}

impl Token {
    pub fn new(kind: TokenKind, raw: Vec<u8>) -> Self { Self { kind, raw } }
}
