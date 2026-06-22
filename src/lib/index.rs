use super::charset::{GChar, build_charset};
use super::adjacency::{can_adjoin, is_valid_block, constraint_score};

pub struct IndexTable {
    pub n: usize,
    pub charset: Vec<GChar>,
    pub valid_count: u128,
    pub bits: u32,
    /// sorted_chars[rank] = charset_index, sorted by (null first, then constraint_score ASC, then position ASC)
    sorted_chars: Vec<usize>,
    /// rank_of[charset_index] = rank in sorted_chars
    rank_of: Vec<usize>,
    /// conditional_suffix[pos][prev_ci][ci] = # valid blocks of length (n-pos) starting with
    /// charset[ci], given that the character before position pos was charset[prev_ci].
    /// prev_ci == cs means "no previous character" (position 0).
    conditional_suffix: Vec<Vec<Vec<u128>>>,
}

impl IndexTable {
    pub fn new(n: usize) -> Self {
        let charset = build_charset(n);
        let cs = charset.len();

        // Sort: null first, then by constraint_score ASC, then by original position ASC
        let null_ci = charset.iter().position(|g| g.ch == '0').unwrap_or(0);
        let mut sorted_chars: Vec<usize> = (0..cs).collect();
        sorted_chars.sort_by(|&a, &b| {
            if a == null_ci { return std::cmp::Ordering::Less; }
            if b == null_ci { return std::cmp::Ordering::Greater; }
            let sa = constraint_score(charset[a].ch, &charset);
            let sb = constraint_score(charset[b].ch, &charset);
            sa.cmp(&sb).then(a.cmp(&b))
        });

        let mut rank_of = vec![0usize; cs];
        for (rank, &ci) in sorted_chars.iter().enumerate() {
            rank_of[ci] = rank;
        }

        // conditional_suffix[pos][prev+1][ci]:
        // number of valid sequences of length (n - pos) where:
        //   - first char is charset[ci]
        //   - previous char was charset[prev] (prev == cs means no previous)
        // Dimension: n positions × (cs+1) prev states × cs chars
        // Use cs as sentinel for "no previous"
        let mut cond = vec![vec![vec![0u128; cs]; cs + 1]; n];

        // Base: pos = n-1 (last position)
        for prev in 0..=cs {
            for ci in 0..cs {
                let adj_ok = if prev == cs {
                    true // no previous
                } else {
                    can_adjoin(charset[prev].ch, charset[ci].ch)
                };
                cond[n - 1][prev][ci] = if adj_ok { 1 } else { 0 };
            }
        }

        // Fill backwards
        for pos in (0..n - 1).rev() {
            for prev in 0..=cs {
                for ci in 0..cs {
                    let adj_ok = if prev == cs {
                        true
                    } else {
                        can_adjoin(charset[prev].ch, charset[ci].ch)
                    };
                    if !adj_ok {
                        cond[pos][prev][ci] = 0;
                        continue;
                    }
                    // Sum over all valid next chars
                    let mut count = 0u128;
                    for ni in 0..cs {
                        count = count.saturating_add(cond[pos + 1][ci][ni]);
                    }
                    cond[pos][prev][ci] = count;
                }
            }
        }

        // Total valid blocks = sum of cond[0][cs][ci] for all ci
        let valid_count = (0..cs).fold(0u128, |a, ci| a.saturating_add(cond[0][cs][ci]));
        let bits = bits_needed(valid_count);

        Self { n, charset, valid_count, bits, sorted_chars, rank_of, conditional_suffix: cond }
    }

    /// Encode: count how many valid blocks come before this one in sorted order.
    pub fn encode_block(&self, block: &[char]) -> Result<u128, String> {
        if block.len() != self.n {
            return Err(format!("Block length {} != n={}", block.len(), self.n));
        }
        if !is_valid_block(block) {
            return Err(format!("Invalid block: {:?}", block));
        }
        let cs = self.charset.len();
        let mut index = 0u128;
        let mut prev = cs; // sentinel = no previous

        for pos in 0..self.n {
            let cur_ci = self.charset.iter().position(|g| g.ch == block[pos])
                .ok_or_else(|| format!("Char '{}' not in G{} charset", block[pos], self.n))?;
            let cur_rank = self.rank_of[cur_ci];

            // Count all chars that sort before cur_ci and are valid given prev
            for rank in 0..cur_rank {
                let sci = self.sorted_chars[rank];
                // Check adjacency with prev
                let adj_ok = if prev == cs { true } else {
                    can_adjoin(self.charset[prev].ch, self.charset[sci].ch)
                };
                if adj_ok {
                    let completions = if pos == self.n - 1 {
                        1u128
                    } else {
                        // How many valid completions from pos+1 given sci as prev?
                        (0..cs).fold(0u128, |a, ni| a.saturating_add(self.conditional_suffix[pos + 1][sci][ni]))
                    };
                    index = index.saturating_add(completions);
                }
            }

            prev = cur_ci;
        }

        Ok(index)
    }

    /// Decode: find the block at the given index.
    pub fn decode_index(&self, index: u128) -> Result<Vec<char>, String> {
        if index >= self.valid_count {
            return Ok(vec!['0'; self.n]);
        }
        let cs = self.charset.len();
        let mut block = Vec::with_capacity(self.n);
        let mut remaining = index;
        let mut prev = cs; // sentinel

        for pos in 0..self.n {
            let mut found = false;
            for &sci in &self.sorted_chars {
                let adj_ok = if prev == cs { true } else {
                    can_adjoin(self.charset[prev].ch, self.charset[sci].ch)
                };
                if !adj_ok { continue; }

                let completions = if pos == self.n - 1 {
                    1u128
                } else {
                    (0..cs).fold(0u128, |a, ni| a.saturating_add(self.conditional_suffix[pos + 1][sci][ni]))
                };

                if remaining < completions {
                    block.push(self.charset[sci].ch);
                    prev = sci;
                    found = true;
                    break;
                }
                remaining = remaining.saturating_sub(completions);
            }
            if !found {
                return Err(format!("Decode failed at pos={} remaining={}", pos, remaining));
            }
        }

        Ok(block)
    }

    pub fn charset_display(&self) -> String {
        self.charset.iter().map(|g| g.ch.to_string()).collect::<Vec<_>>().join(" ")
    }
}

pub fn bits_needed(count: u128) -> u32 {
    if count <= 1 { return 1; }
    128 - count.leading_zeros()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g4_roundtrip() {
        let table = IndexTable::new(4);
        let block = vec!['A', '1', 'b', '2'];
        let idx = table.encode_block(&block).unwrap();
        let decoded = table.decode_index(idx).unwrap();
        assert_eq!(decoded, block, "roundtrip failed idx={}", idx);
    }

    #[test]
    fn g4_null_is_zero() {
        let table = IndexTable::new(4);
        let null_block = vec!['0', '0', '0', '0'];
        let idx = table.encode_block(&null_block).unwrap();
        assert_eq!(idx, 0, "null block must be index 0, got {}", idx);
        let decoded = table.decode_index(0).unwrap();
        assert_eq!(decoded, null_block);
    }

    #[test]
    fn g4_bits() {
        let table = IndexTable::new(4);
        assert_eq!(table.bits, 14);
        assert_eq!(table.valid_count, 14393);
    }

    #[test]
    fn g4_all_roundtrip() {
        let table = IndexTable::new(4);
        for i in 0..table.valid_count.min(1000) {
            let block = table.decode_index(i).unwrap();
            let idx = table.encode_block(&block).unwrap();
            assert_eq!(idx, i, "roundtrip failed at index {}: block={:?} re-encoded={}", i, block, idx);
        }
    }

    #[test]
    fn g28_null_is_zero() {
        let table = IndexTable::new(28);
        let block = vec!['0'; 28];
        let idx = table.encode_block(&block).unwrap();
        assert_eq!(idx, 0);
        let decoded = table.decode_index(0).unwrap();
        assert_eq!(decoded, block);
    }

    #[test]
    fn g28_roundtrip() {
        let table = IndexTable::new(28);
        for i in [0u128, 1, 100, 9999].iter() {
            let block = table.decode_index(*i).unwrap();
            let idx = table.encode_block(&block).unwrap();
            assert_eq!(idx, *i, "G28 roundtrip failed at index {}: block={:?}", i, block);
        }
    }
}

#[cfg(test)]
mod debug_tests {
    use super::*;
    use crate::charset::build_charset;

    #[test]
    fn g28_encode_sample_bytes() {
        let table = IndexTable::new(28);
        let cs = &table.charset;
        let cs_len = cs.len();
        // Simulate what encode_bytes does for "hello\n" = [104, 101, 108, 108, 111, 10]
        let data = b"hello\n";
        let g_chars: Vec<char> = data.iter().map(|&b| cs[b as usize % cs_len].ch).collect();
        println!("G28 chars for 'hello\\n': {:?}", g_chars);
        println!("cs_len={}", cs_len);
        // Pad to 28
        let mut padded = g_chars.clone();
        while padded.len() % 28 != 0 { padded.push('0'); }
        println!("padded block: {:?}", padded);
        // Try encoding
        match table.encode_block(&padded) {
            Ok(idx) => {
                println!("index: {}", idx);
                let decoded = table.decode_index(idx).unwrap();
                println!("decoded: {:?}", decoded);
            }
            Err(e) => println!("encode error: {}", e),
        }
    }
}
