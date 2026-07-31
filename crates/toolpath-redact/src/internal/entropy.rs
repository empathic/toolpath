//! Shannon entropy over a matched value's character distribution, used to
//! down-score matches that look right but are actually low-entropy
//! (repeated characters, placeholder text).

pub fn shannon(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut counts = std::collections::HashMap::new();
    for ch in s.chars() {
        *counts.entry(ch).or_insert(0usize) += 1;
    }
    let n = s.chars().count() as f64;
    -counts
        .values()
        .map(|&c| {
            let p = c as f64 / n;
            p * p.log2()
        })
        .sum::<f64>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_is_zero_entropy() {
        assert_eq!(shannon(""), 0.0);
    }

    #[test]
    fn repeated_character_is_zero_entropy() {
        assert_eq!(shannon("aaaaaaaaaa"), 0.0);
    }

    #[test]
    fn uniform_four_symbol_distribution_is_two_bits() {
        assert!((shannon("abcdabcdabcd") - 2.0).abs() < 1e-9);
    }
}
