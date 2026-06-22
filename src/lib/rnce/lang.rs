//! Language-specific grammar implementations.
//!
//! Each language implements the LanguageGrammar trait.
//! The model detects language from file extension and uses the right grammar.
//!
//! Languages supported:
//!   - JavaScript/TypeScript (js, ts, jsx, tsx)
//!   - Python (py)
//!   - Rust (rs)
//!   - Generic (fallback for unknown languages)

use super::token::TokenKind;
use super::grammar::*;

// ── Language detection ────────────────────────────────────────────────────────

pub fn detect_language(path: &str) -> &'static dyn LanguageGrammar {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "js" | "ts" | "jsx" | "tsx" | "mjs" | "cjs" => &JavaScript,
        "py" | "pyw"                                  => &Python,
        "rs"                                          => &Rust,
        "go"                                          => &Go,
        _                                             => &Generic,
    }
}

// ── JavaScript/TypeScript ─────────────────────────────────────────────────────

pub struct JavaScript;

impl LanguageGrammar for JavaScript {
    fn valid_next(&self, state: ParseState, _stack: &ParserStack) -> ValidSet {
        use TokenKind::*;
        use ParseState::*;

        match state {
            TopLevel | Block | FunctionBody => ValidSet::of(&[
                Keyword,    // fn, if, class, import, const, let, var...
                Identifier, // expression start / assignment
                LBrace,     // block / object literal
                LParen,     // grouped expression / IIFE
                Semicolon,  // empty statement
                LineComment,
                BlockComment,
            ]),

            AfterFnKeyword => ValidSet::of(&[
                Identifier, // function name
                Star,       // generator function*
                LParen,     // anonymous function(...)
            ]),

            AfterFnName => ValidSet::of(&[LParen]),

            ParamList => ValidSet::of(&[
                Identifier, RParen, DotDotDot, LBrace, LBracket, // destructuring
            ]),

            Expr => ValidSet::of(&[
                Identifier, IntLiteral, FloatLiteral, StringLiteral,
                BoolLiteral, NullLiteral, LParen, LBracket, LBrace,
                Minus, Not, BitNot, Keyword, // new, typeof, void, delete, await
            ]),

            AfterExpr => ValidSet::of(&[
                // Binary operators
                Plus, Minus, Star, Slash, Percent,
                EqEq, NotEq, Lt, Gt, LtEq, GtEq,
                AndAnd, OrOr, And, Or, BitXor,
                Shl, Shr,
                // Assignment
                Eq, PlusEq, MinusEq, StarEq, SlashEq, PercentEq,
                // Access
                Dot, LBracket, LParen, // method call, index, call
                // Ternary
                Question,
                // End
                Semicolon, Comma, RParen, RBracket, RBrace, Colon,
                Arrow, FatArrow,
                // Optional chaining
                DotDot, // ?. handled as separate token
                Newline,
            ]),

            AfterControlKeyword => ValidSet::of(&[LParen, Identifier]),

            AfterCondition => ValidSet::of(&[LBrace, Keyword]),

            ArgList => ValidSet::of(&[
                Identifier, IntLiteral, FloatLiteral, StringLiteral,
                BoolLiteral, NullLiteral, LParen, LBracket, LBrace,
                Minus, Not, RParen, DotDotDot,
            ]),

            AfterDot => ValidSet::of(&[Identifier, Keyword]),

            ImportPath => ValidSet::of(&[Identifier, StringLiteral, Star, LBrace]),

            TypeAnnotation => ValidSet::of(&[
                Identifier, LParen, LBracket, Or, And, Question, Keyword,
            ]),

            ObjectLiteral => ValidSet::of(&[
                Identifier, StringLiteral, IntLiteral, LBracket,
                DotDotDot, RBrace, LineComment,
            ]),

            ObjectAfterKey => ValidSet::of(&[Colon, LParen]), // shorthand method

            AfterStatement => ValidSet::of(&[Semicolon, Newline, RBrace]),

            _ => ValidSet::any(),
        }
    }

    fn advance(&self, stack: &mut ParserStack, token: TokenKind, raw: &[u8]) {
        use TokenKind::*;
        use ParseState::*;

        let kw = if token == Keyword { self.classify_keyword(raw) } else { None };

        match (stack.current(), token) {
            // Function definition
            (_, Keyword) if kw == Some(KeywordKind::FunctionDef) => {
                stack.push(AfterFnKeyword);
            }
            (AfterFnKeyword, Identifier) => {
                stack.pop();
                stack.push(AfterFnName);
            }
            (AfterFnKeyword, LParen) | (AfterFnName, LParen) => {
                stack.pop();
                stack.open(Delimiter::Paren, ParamList);
            }

            // Control flow
            (_, Keyword) if kw == Some(KeywordKind::Control) => {
                stack.push(AfterControlKeyword);
            }
            (AfterControlKeyword, LParen) => {
                stack.pop();
                stack.open(Delimiter::Paren, Expr);
            }

            // Import
            (_, Keyword) if kw == Some(KeywordKind::Import) => {
                stack.push(ImportPath);
            }
            (ImportPath, StringLiteral) => { stack.pop(); }

            // Braces
            (TopLevel, LBrace) | (FunctionBody, LBrace) | (Block, LBrace) => {
                stack.open(Delimiter::Brace, Block);
            }
            (_, LBrace) => {
                stack.open(Delimiter::Brace, Block);
            }
            (_, RBrace) => {
                stack.close(Delimiter::Brace);
            }

            // Parens
            (_, LParen) => { stack.open(Delimiter::Paren, Expr); }
            (_, RParen) => { stack.close(Delimiter::Paren); }

            // Brackets
            (_, LBracket) => { stack.open(Delimiter::Bracket, Expr); }
            (_, RBracket) => { stack.close(Delimiter::Bracket); }

            // Dot access
            (_, Dot) => { stack.push(AfterDot); }
            (AfterDot, Identifier) => { stack.pop(); stack.push(AfterExpr); }

            // Expression end
            (Expr, _) if matches!(token,
                Identifier | IntLiteral | FloatLiteral |
                StringLiteral | BoolLiteral | NullLiteral
            ) => { stack.pop(); stack.push(AfterExpr); }

            _ => {}
        }
    }

    fn classify_keyword(&self, raw: &[u8]) -> Option<KeywordKind> {
        match raw {
            b"function" | b"async" => Some(KeywordKind::FunctionDef),
            b"class" => Some(KeywordKind::ClassDef),
            b"if" | b"else" | b"while" | b"for" | b"switch" | b"case" => Some(KeywordKind::Control),
            b"return" | b"yield" => Some(KeywordKind::Return),
            b"import" | b"require" | b"from" | b"export" => Some(KeywordKind::Import),
            b"const" | b"let" | b"var" => Some(KeywordKind::Variable),
            b"public" | b"private" | b"protected" => Some(KeywordKind::Visibility),
            b"await" => Some(KeywordKind::Async),
            b"new" | b"typeof" | b"instanceof" | b"void" | b"delete" |
            b"try" | b"catch" | b"finally" | b"throw" | b"break" | b"continue" |
            b"default" | b"extends" | b"super" | b"this" | b"in" | b"of" |
            b"true" | b"false" | b"null" | b"undefined" | b"type" | b"interface" |
            b"enum" | b"namespace" | b"declare" | b"abstract" | b"override" |
            b"implements" | b"static" | b"readonly" | b"as" | b"satisfies" => Some(KeywordKind::Other),
            _ => None,
        }
    }
}

// ── Python ────────────────────────────────────────────────────────────────────

pub struct Python;

impl LanguageGrammar for Python {
    fn valid_next(&self, state: ParseState, _stack: &ParserStack) -> ValidSet {
        use TokenKind::*;
        use ParseState::*;

        match state {
            TopLevel | Block | FunctionBody => ValidSet::of(&[
                Keyword, Identifier, At, // decorators
                IntLiteral, StringLiteral, // top-level expressions
                Newline, LineComment, Indent,
            ]),
            AfterFnKeyword => ValidSet::of(&[Identifier]),
            AfterFnName => ValidSet::of(&[LParen]),
            ParamList => ValidSet::of(&[Identifier, RParen, Star, DotDotDot]),
            Expr => ValidSet::of(&[
                Identifier, IntLiteral, FloatLiteral, StringLiteral,
                BoolLiteral, NullLiteral, LParen, LBracket, LBrace,
                Minus, Not, BitNot, Keyword,
            ]),
            AfterExpr => ValidSet::of(&[
                Plus, Minus, Star, Slash, Percent, StarEq,
                EqEq, NotEq, Lt, Gt, LtEq, GtEq,
                AndAnd, OrOr, And, Or, BitXor, BitAnd, BitOr,
                Eq, PlusEq, MinusEq, SlashEq, PercentEq,
                Dot, LBracket, LParen, Comma, Colon,
                Newline, Semicolon, RParen, RBracket, RBrace,
                Keyword, // in, not, is, and, or
            ]),
            AfterDot => ValidSet::of(&[Identifier]),
            ImportPath => ValidSet::of(&[Identifier, Star, LParen]),
            _ => ValidSet::any(),
        }
    }

    fn advance(&self, stack: &mut ParserStack, token: TokenKind, raw: &[u8]) {
        use TokenKind::*;
        use ParseState::*;

        let kw = if token == Keyword { self.classify_keyword(raw) } else { None };

        match (stack.current(), token) {
            (_, Keyword) if kw == Some(KeywordKind::FunctionDef) => {
                stack.push(AfterFnKeyword);
            }
            (AfterFnKeyword, Identifier) => {
                stack.pop(); stack.push(AfterFnName);
            }
            (AfterFnName, LParen) | (AfterFnKeyword, LParen) => {
                stack.pop();
                stack.open(Delimiter::Paren, ParamList);
            }
            (_, Keyword) if kw == Some(KeywordKind::Import) => {
                stack.push(ImportPath);
            }
            (ImportPath, Newline) => { stack.pop(); }
            (_, LParen) => { stack.open(Delimiter::Paren, Expr); }
            (_, RParen) => { stack.close(Delimiter::Paren); }
            (_, LBracket) => { stack.open(Delimiter::Bracket, Expr); }
            (_, RBracket) => { stack.close(Delimiter::Bracket); }
            (_, LBrace) => { stack.open(Delimiter::Brace, ObjectLiteral); }
            (_, RBrace) => { stack.close(Delimiter::Brace); }
            (_, Dot) => { stack.push(AfterDot); }
            (AfterDot, Identifier) => { stack.pop(); stack.push(AfterExpr); }
            (_, Indent) => { stack.push(Block); }
            (_, Dedent) => { stack.pop(); }
            _ => {}
        }
    }

    fn classify_keyword(&self, raw: &[u8]) -> Option<KeywordKind> {
        match raw {
            b"def" | b"lambda" => Some(KeywordKind::FunctionDef),
            b"class" => Some(KeywordKind::ClassDef),
            b"if" | b"elif" | b"else" | b"while" | b"for" | b"with" => Some(KeywordKind::Control),
            b"return" | b"yield" => Some(KeywordKind::Return),
            b"import" | b"from" | b"as" => Some(KeywordKind::Import),
            b"global" | b"nonlocal" => Some(KeywordKind::Variable),
            b"async" | b"await" => Some(KeywordKind::Async),
            b"True" | b"False" | b"None" | b"not" | b"and" | b"or" | b"in" |
            b"is" | b"del" | b"pass" | b"break" | b"continue" | b"raise" |
            b"try" | b"except" | b"finally" | b"assert" | b"print" => Some(KeywordKind::Other),
            _ => None,
        }
    }
}

// ── Rust ──────────────────────────────────────────────────────────────────────

pub struct Rust;

impl LanguageGrammar for Rust {
    fn valid_next(&self, state: ParseState, _stack: &ParserStack) -> ValidSet {
        use TokenKind::*;
        use ParseState::*;

        match state {
            TopLevel => ValidSet::of(&[
                Keyword,     // fn, pub, struct, enum, impl, use, mod, type, trait, const, static
                Hash,        // #[attribute]
                LineComment, BlockComment,
            ]),
            FunctionBody | Block => ValidSet::of(&[
                Keyword, Identifier, Hash, // attributes
                IntLiteral, StringLiteral, LParen, LBrace,
                Minus, Not, BitAnd, Star, // unary ops
                LineComment, BlockComment, RBrace,
            ]),
            AfterFnKeyword => ValidSet::of(&[Identifier, Lt]), // generics
            AfterFnName => ValidSet::of(&[LParen, Lt]),
            ParamList => ValidSet::of(&[
                Identifier, BitAnd, Star, Keyword, // &self, mut, ref
                RParen, DotDotDot,
            ]),
            TypeAnnotation => ValidSet::of(&[
                Identifier, BitAnd, Star, LParen, LBracket,
                Keyword, // dyn, impl, fn
                Question, // ?Sized
            ]),
            Expr => ValidSet::of(&[
                Identifier, IntLiteral, FloatLiteral, StringLiteral,
                BoolLiteral, NullLiteral, LParen, LBracket, LBrace,
                Minus, Not, BitAnd, Star, BitNot,
                Keyword, // if, match, loop, while, for, return, break, continue
                Or,      // |x| closure
            ]),
            AfterExpr => ValidSet::of(&[
                Plus, Minus, Star, Slash, Percent,
                EqEq, NotEq, Lt, Gt, LtEq, GtEq,
                AndAnd, OrOr, BitAnd, BitOr, BitXor, Shl, Shr,
                Eq, PlusEq, MinusEq, StarEq, SlashEq, PercentEq,
                Dot, LBracket, LParen, // field, index, call
                Question,  // ? operator
                Semicolon, Comma, RParen, RBracket, RBrace,
                Arrow, FatArrow, Colon, DoubleColon,
                // as handled as Keyword
            ]),
            AfterDot => ValidSet::of(&[Identifier, Keyword, IntLiteral]), // tuple field .0
            AfterPathSep => ValidSet::of(&[Identifier, Lt, LBrace, Star]),
            ImportPath => ValidSet::of(&[
                Identifier, Star, LBrace, DoubleColon,
            ]),
            _ => ValidSet::any(),
        }
    }

    fn advance(&self, stack: &mut ParserStack, token: TokenKind, raw: &[u8]) {
        use TokenKind::*;
        use ParseState::*;

        let kw = if token == Keyword { self.classify_keyword(raw) } else { None };

        match (stack.current(), token) {
            (_, Keyword) if kw == Some(KeywordKind::FunctionDef) => {
                stack.push(AfterFnKeyword);
            }
            (AfterFnKeyword, Identifier) => {
                stack.pop(); stack.push(AfterFnName);
            }
            (AfterFnName, LParen) | (AfterFnKeyword, LParen) => {
                stack.pop();
                stack.open(Delimiter::Paren, ParamList);
            }
            (_, Keyword) if kw == Some(KeywordKind::Import) => {
                stack.push(ImportPath);
            }
            (ImportPath, Semicolon) => { stack.pop(); }
            (_, LBrace) => { stack.open(Delimiter::Brace, Block); }
            (_, RBrace) => { stack.close(Delimiter::Brace); }
            (_, LParen) => { stack.open(Delimiter::Paren, Expr); }
            (_, RParen) => { stack.close(Delimiter::Paren); }
            (_, LBracket) => { stack.open(Delimiter::Bracket, Expr); }
            (_, RBracket) => { stack.close(Delimiter::Bracket); }
            (_, Dot) => { stack.push(AfterDot); }
            (AfterDot, _) => { stack.pop(); stack.push(AfterExpr); }
            (_, DoubleColon) => { stack.push(AfterPathSep); }
            (AfterPathSep, _) => { stack.pop(); }
            _ => {}
        }
    }

    fn classify_keyword(&self, raw: &[u8]) -> Option<KeywordKind> {
        match raw {
            b"fn" => Some(KeywordKind::FunctionDef),
            b"struct" | b"enum" | b"trait" | b"impl" | b"union" => Some(KeywordKind::ClassDef),
            b"if" | b"else" | b"while" | b"for" | b"loop" | b"match" => Some(KeywordKind::Control),
            b"return" | b"yield" => Some(KeywordKind::Return),
            b"use" | b"mod" | b"extern" | b"crate" => Some(KeywordKind::Import),
            b"let" | b"const" | b"static" | b"mut" | b"ref" => Some(KeywordKind::Variable),
            b"pub" | b"priv" => Some(KeywordKind::Visibility),
            b"async" | b"await" => Some(KeywordKind::Async),
            b"type" | b"where" | b"dyn" | b"move" | b"unsafe" | b"self" | b"Self" |
            b"super" | b"true" | b"false" | b"as" | b"in" | b"break" | b"continue" => Some(KeywordKind::Other),
            _ => None,
        }
    }
}

// ── Go ────────────────────────────────────────────────────────────────────────

pub struct Go;

impl LanguageGrammar for Go {
    fn valid_next(&self, state: ParseState, _stack: &ParserStack) -> ValidSet {
        use TokenKind::*;
        use ParseState::*;
        match state {
            TopLevel => ValidSet::of(&[Keyword, LineComment, BlockComment]),
            FunctionBody | Block => ValidSet::of(&[
                Keyword, Identifier, LBrace, LParen,
                IntLiteral, StringLiteral,
                Minus, Not, BitXor, // unary ops in Go
                LineComment, RBrace,
            ]),
            AfterFnKeyword => ValidSet::of(&[Identifier, LParen]),
            _ => ValidSet::any(),
        }
    }

    fn advance(&self, stack: &mut ParserStack, token: TokenKind, raw: &[u8]) {
        use TokenKind::*;
        use ParseState::*;
        let kw = if token == Keyword { self.classify_keyword(raw) } else { None };
        match (stack.current(), token) {
            (_, Keyword) if kw == Some(KeywordKind::FunctionDef) => { stack.push(AfterFnKeyword); }
            (AfterFnKeyword, Identifier) => { stack.pop(); stack.push(AfterFnName); }
            (AfterFnName, LParen) | (AfterFnKeyword, LParen) => {
                stack.pop(); stack.open(Delimiter::Paren, ParamList);
            }
            (_, LBrace) => { stack.open(Delimiter::Brace, Block); }
            (_, RBrace) => { stack.close(Delimiter::Brace); }
            (_, LParen) => { stack.open(Delimiter::Paren, Expr); }
            (_, RParen) => { stack.close(Delimiter::Paren); }
            (_, LBracket) => { stack.open(Delimiter::Bracket, Expr); }
            (_, RBracket) => { stack.close(Delimiter::Bracket); }
            (_, Dot) => { stack.push(ParseState::AfterDot); }
            (ParseState::AfterDot, _) => { stack.pop(); }
            _ => {}
        }
    }

    fn classify_keyword(&self, raw: &[u8]) -> Option<KeywordKind> {
        match raw {
            b"func" => Some(KeywordKind::FunctionDef),
            b"type" | b"struct" | b"interface" => Some(KeywordKind::ClassDef),
            b"if" | b"else" | b"for" | b"switch" | b"select" | b"case" => Some(KeywordKind::Control),
            b"return" => Some(KeywordKind::Return),
            b"import" | b"package" => Some(KeywordKind::Import),
            b"var" | b"const" => Some(KeywordKind::Variable),
            b"go" | b"chan" | b"defer" | b"map" | b"range" | b"break" |
            b"continue" | b"goto" | b"fallthrough" | b"true" | b"false" | b"nil" => Some(KeywordKind::Other),
            _ => None,
        }
    }
}

// ── Generic fallback ──────────────────────────────────────────────────────────

pub struct Generic;

impl LanguageGrammar for Generic {
    fn valid_next(&self, _state: ParseState, _stack: &ParserStack) -> ValidSet {
        ValidSet::any() // no constraints — falls back to pure PPM
    }
    fn advance(&self, _stack: &mut ParserStack, _token: TokenKind, _raw: &[u8]) {}
    fn classify_keyword(&self, _raw: &[u8]) -> Option<KeywordKind> { None }
}


