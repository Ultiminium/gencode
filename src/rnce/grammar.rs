//! Grammar model — given parse state, what tokens are valid next?
//!
//! This is the core of the model. At any point in a parse,
//! the grammar constrains what can come next to a small set.
//! We assign zero probability to everything outside that set.
//!
//! The grammar is represented as a set of parse states and transitions.
//! It doesn't need to be a complete formal grammar — just accurate enough
//! to constrain the probability distribution meaningfully.
//!
//! Key insight: even a loose grammar that's right 80% of the time
//! dramatically improves compression, because when it's right,
//! it eliminates 200+ of 256 possible bytes from consideration.

use super::token::TokenKind;

/// A set of valid next token kinds
#[derive(Debug, Clone)]
pub struct ValidSet {
    pub kinds: Vec<TokenKind>,
    /// If true, anything is valid (grammar doesn't constrain here)
    pub unconstrained: bool,
}

impl ValidSet {
    pub fn any() -> Self { Self { kinds: vec![], unconstrained: true } }
    pub fn of(kinds: &[TokenKind]) -> Self { Self { kinds: kinds.to_vec(), unconstrained: false } }
    pub fn contains(&self, k: TokenKind) -> bool {
        self.unconstrained || self.kinds.contains(&k)
    }
    pub fn count(&self) -> usize {
        if self.unconstrained { 256 } else { self.kinds.len() }
    }
}

/// Parse state — what are we currently parsing?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParseState {
    // Top-level context
    TopLevel,
    // Inside a function/method body
    FunctionBody,
    // Inside a block { ... }
    Block,
    // After an expression — expecting operator or end of expression
    AfterExpr,
    // Expecting an expression
    Expr,
    // Inside a type annotation
    TypeAnnotation,
    // Inside a parameter list
    ParamList,
    // Inside an argument list (call)
    ArgList,
    // Inside an array/list literal
    ArrayLiteral,
    // Inside an object/dict literal  
    ObjectLiteral,
    // After an object key, expecting :
    ObjectAfterKey,
    // After a keyword like `if`, `while`, expecting condition
    AfterControlKeyword,
    // After `if (cond)` or `while (cond)`, expecting body
    AfterCondition,
    // After a statement, expecting ; or newline
    AfterStatement,
    // Inside an import/use statement
    ImportPath,
    // After `fn`/`def`/`function`, expecting name
    AfterFnKeyword,
    // After function name, expecting (
    AfterFnName,
    // Inside a string literal
    InString,
    // After `.` — expecting identifier (method/field)
    AfterDot,
    // After `::` or `->` — expecting identifier
    AfterPathSep,
}

/// Parser stack — tracks nesting
#[derive(Debug, Clone)]
pub struct ParserStack {
    states: Vec<ParseState>,
    // Track what delimiter we're inside
    delimiters: Vec<Delimiter>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Delimiter {
    Paren,    // (
    Brace,    // {
    Bracket,  // [
}

impl ParserStack {
    pub fn new() -> Self {
        Self {
            states: vec![ParseState::TopLevel],
            delimiters: vec![],
        }
    }

    pub fn current(&self) -> ParseState {
        *self.states.last().unwrap_or(&ParseState::TopLevel)
    }

    pub fn push(&mut self, state: ParseState) {
        self.states.push(state);
    }

    pub fn pop(&mut self) -> Option<ParseState> {
        if self.states.len() > 1 { self.states.pop() } else { None }
    }

    pub fn open(&mut self, delim: Delimiter, state: ParseState) {
        self.delimiters.push(delim);
        self.push(state);
    }

    pub fn close(&mut self, delim: Delimiter) {
        if self.delimiters.last() == Some(&delim) {
            self.delimiters.pop();
            self.pop();
        }
    }

    pub fn depth(&self) -> usize { self.delimiters.len() }

    /// What tokens are valid in the current state?
    pub fn valid_next(&self, lang: &dyn LanguageGrammar) -> ValidSet {
        lang.valid_next(self.current(), self)
    }

    /// Update state after seeing a token
    pub fn advance(&mut self, token: TokenKind, raw: &[u8], lang: &dyn LanguageGrammar) {
        lang.advance(self, token, raw);
    }
}

/// Language-specific grammar rules
pub trait LanguageGrammar: Send + Sync {
    /// Given current parse state, what token kinds are valid next?
    fn valid_next(&self, state: ParseState, stack: &ParserStack) -> ValidSet;

    /// Update parser stack after seeing a token
    fn advance(&self, stack: &mut ParserStack, token: TokenKind, raw: &[u8]);

    /// Detect the keyword type for a raw identifier
    fn classify_keyword(&self, raw: &[u8]) -> Option<KeywordKind>;
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeywordKind {
    FunctionDef,   // fn, def, function, func
    ClassDef,      // class, struct, enum, trait, interface
    Control,       // if, else, elif, while, for, loop, switch, case
    Return,        // return, yield
    Import,        // import, use, require, include, from
    Variable,      // let, const, var, mut, val
    Type,          // type, typedef, alias
    Visibility,    // pub, public, private, protected
    Async,         // async, await
    Other,
}
