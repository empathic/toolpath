//! A scan of output bytes for strings that must not be in a golden: the
//! developer's home directory, the temp directory a run used, anything that
//! names the machine the pipeline ran on.

/// One occurrence of a needle in the scanned bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leak {
    pub needle: String,
    /// Byte offset of the first occurrence.
    pub offset: usize,
}

/// Every needle that occurs in `bytes`, once each, in needle order. Needles
/// shorter than two bytes are ignored: an empty or one-character needle would
/// match everything and say nothing.
pub fn leaks(bytes: &[u8], needles: &[&str]) -> Vec<Leak> {
    needles
        .iter()
        .filter(|n| n.len() >= 2)
        .filter_map(|n| {
            find(bytes, n.as_bytes()).map(|offset| Leak {
                needle: (*n).to_owned(),
                offset,
            })
        })
        .collect()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The control the hermetic tool ran before trusting its own scan: a
    /// planted needle must be found, or the scan is blind.
    #[test]
    fn a_planted_needle_is_found_and_clean_text_is_not_flagged() {
        let needles = ["/Users/someone", "/tmp/goldens-run-1"];
        assert_eq!(leaks(b"x/Users/somenonex", &needles), vec![]);
        assert_eq!(
            leaks(b"x/Users/someonex", &needles),
            vec![Leak {
                needle: "/Users/someone".into(),
                offset: 1
            }]
        );
    }

    #[test]
    fn each_needle_is_reported_once_in_needle_order() {
        let text = b"/tmp/goldens-run-1 then /Users/someone then /tmp/goldens-run-1 again";
        let found = leaks(text, &["/Users/someone", "/tmp/goldens-run-1"]);
        assert_eq!(
            found.iter().map(|l| l.needle.as_str()).collect::<Vec<_>>(),
            vec!["/Users/someone", "/tmp/goldens-run-1"]
        );
        assert_eq!(found[1].offset, 0);
    }

    #[test]
    fn degenerate_needles_are_ignored() {
        assert_eq!(leaks(b"anything at all", &["", "a"]), vec![]);
    }
}
