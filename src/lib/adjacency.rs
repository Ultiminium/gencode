use super::charset::{CharType, GChar, char_type};

/// Returns true if character a may legally appear directly adjacent to character b
pub fn can_adjoin(a: char, b: char) -> bool {
    let ta = char_type(a);
    let tb = char_type(b);

    // null is always valid next to anything
    if ta == CharType::Null || tb == CharType::Null {
        return true;
    }
    // even-even forbidden
    if ta == CharType::Even && tb == CharType::Even {
        return false;
    }
    // odd-odd forbidden
    if ta == CharType::Odd && tb == CharType::Odd {
        return false;
    }
    // capital must be next to odd or lowercase
    if ta == CharType::Capital && tb != CharType::Odd && tb != CharType::Lower {
        return false;
    }
    if tb == CharType::Capital && ta != CharType::Odd && ta != CharType::Lower {
        return false;
    }
    // symbol must be next to odd, capital, or null
    if ta == CharType::Symbol && tb != CharType::Odd && tb != CharType::Capital && tb != CharType::Null {
        return false;
    }
    if tb == CharType::Symbol && ta != CharType::Odd && ta != CharType::Capital && ta != CharType::Null {
        return false;
    }

    true
}

/// Validate a complete block of n characters
pub fn is_valid_block(block: &[char]) -> bool {
    for i in 0..block.len().saturating_sub(1) {
        if !can_adjoin(block[i], block[i + 1]) {
            return false;
        }
    }
    true
}

/// Constraint score of a character: number of valid neighbors in charset
pub fn constraint_score(c: char, charset: &[GChar]) -> usize {
    charset.iter().filter(|g| can_adjoin(c, g.ch)).count()
}

/// Constraint score of a full block
pub fn block_score(block: &[char], charset: &[GChar]) -> usize {
    block.iter().map(|&c| constraint_score(c, charset)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::charset::build_charset;

    #[test]
    fn basic_adjacency() {
        assert!(can_adjoin('1', '2'));   // odd-even ok
        assert!(!can_adjoin('1', '3')); // odd-odd forbidden
        assert!(!can_adjoin('2', '4')); // even-even forbidden
        assert!(can_adjoin('a', 'a'));   // lower-lower ok
        assert!(can_adjoin('A', '1'));  // capital-odd ok
        assert!(can_adjoin('A', 'a'));  // capital-lower ok
        assert!(!can_adjoin('A', '2')); // capital-even forbidden
        assert!(!can_adjoin('A', 'B')); // capital-capital forbidden
        assert!(can_adjoin('0', 'A'));  // null-anything ok
    }

    #[test]
    fn valid_block() {
        assert!(is_valid_block(&['1','2','1','2']));
        assert!(!is_valid_block(&['2','2','1','2']));
        assert!(is_valid_block(&['A','1','b','2']));
    }
}
