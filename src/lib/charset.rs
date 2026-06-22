/// Character categories in the G system
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CharType {
    Null,
    Odd,
    Even,
    Lower,
    Capital,
    Symbol,
}

/// A character in the G system with its type
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GChar {
    pub ch: char,
    pub kind: CharType,
}

impl GChar {
    pub fn new(ch: char) -> Self {
        let kind = char_type(ch);
        Self { ch, kind }
    }
}

/// Determine the type of a character
pub fn char_type(c: char) -> CharType {
    match c {
        '0' => CharType::Null,
        '1' | '3' | '5' | '7' | '9' => CharType::Odd,
        '2' | '4' | '6' | '8' => CharType::Even,
        'a'..='z' => CharType::Lower,
        'A'..='Z' => CharType::Capital,
        _ => CharType::Symbol,
    }
}

const DIGITS: &[char] = &['1','2','3','4','5','6','7','8','9'];
const LOWERS: &[char] = &['a','b','c','d','e','f','g','h','i','j','k','l','m','n','o','p','q','r','s','t','u','v','w','x','y','z'];
const CAPITALS: &[char] = &['A','B','C','D','E','F','G','H','I','J','K','L','M','N','O','P','Q','R','S','T','U','V','W','X','Y','Z'];
const SYMBOLS: &[char] = &['!','@','#','$','%','^','&','*','(',')','-','_','=','+','[',']','{','}','|',';',':',',','.','<','>','?','/','~','`'];

/// Build the complete character set for Gn
/// Returns vec of GChar in canonical expansion order (null first, then interleaved per-track)
pub fn build_charset(n: usize) -> Vec<GChar> {
    let mut chars = vec![GChar::new('0')]; // null always first
    let mut seen = std::collections::HashSet::new();
    seen.insert('0');

    for i in 0..n {
        // digit track
        let d = if i < DIGITS.len() {
            DIGITS[i]
        } else {
            let si = i - DIGITS.len();
            if si < SYMBOLS.len() { SYMBOLS[si] } else { '?' }
        };
        if seen.insert(d) { chars.push(GChar::new(d)); }

        // lowercase track
        let l = if i < LOWERS.len() {
            LOWERS[i]
        } else {
            let si = i - LOWERS.len();
            if si < SYMBOLS.len() { SYMBOLS[si] } else { '?' }
        };
        if seen.insert(l) { chars.push(GChar::new(l)); }

        // capital track
        let cap = if i < CAPITALS.len() {
            CAPITALS[i]
        } else {
            let si = i - CAPITALS.len();
            if si < SYMBOLS.len() { SYMBOLS[si] } else { '?' }
        };
        if seen.insert(cap) { chars.push(GChar::new(cap)); }
    }

    chars
}

/// Index of a char in the canonical charset order (for tiebreaker sorting)
pub fn canonical_index(c: char, charset: &[GChar]) -> usize {
    charset.iter().position(|g| g.ch == c).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g4_charset() {
        let cs = build_charset(4);
        let chars: Vec<char> = cs.iter().map(|g| g.ch).collect();
        assert!(chars.contains(&'0'));
        assert!(chars.contains(&'1'));
        assert!(chars.contains(&'a'));
        assert!(chars.contains(&'A'));
        assert!(chars.contains(&'4'));
        assert!(chars.contains(&'d'));
        assert!(chars.contains(&'D'));
        assert_eq!(cs.len(), 13); // 0 + 4 digits + 4 lower + 4 capital (some may dedup at high n)
    }

    #[test]
    fn g2_charset() {
        let cs = build_charset(2);
        assert_eq!(cs.len(), 7); // 0,1,a,A,2,b,B
    }
}
