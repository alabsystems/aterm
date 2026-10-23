// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Allocation-free literal matching helpers.

/// Iterator over overlapping ASCII case-insensitive match byte offsets.
pub(crate) struct AsciiCaseInsensitiveMatches<'a> {
    haystack: &'a [u8],
    needle: &'a [u8],
    next_start: usize,
}

impl<'a> AsciiCaseInsensitiveMatches<'a> {
    /// Create an iterator over matches in `haystack`.
    #[inline]
    pub(crate) fn new(haystack: &'a str, needle: &'a str) -> Self {
        debug_assert!(haystack.is_ascii());
        debug_assert!(needle.is_ascii());
        Self {
            haystack: haystack.as_bytes(),
            needle: needle.as_bytes(),
            next_start: 0,
        }
    }
}

impl Iterator for AsciiCaseInsensitiveMatches<'_> {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let needle_len = self.needle.len();
        if needle_len == 0 {
            return None;
        }
        let max_start = self.haystack.len().checked_sub(needle_len)?;
        let &first = self.needle.first()?;

        if self.haystack.len() <= 8 {
            while self.next_start <= max_start {
                let candidate = self.next_start;
                self.next_start = candidate.checked_add(1)?;
                let match_end = candidate.checked_add(needle_len)?;
                let candidate_bytes = self.haystack.get(candidate..match_end)?;
                if candidate_bytes.eq_ignore_ascii_case(self.needle) {
                    return Some(candidate);
                }
            }
            return None;
        }

        let search_end = max_start.checked_add(1)?;
        let lower = first.to_ascii_lowercase();
        let upper = first.to_ascii_uppercase();

        while self.next_start <= max_start {
            let tail = self.haystack.get(self.next_start..search_end)?;
            let relative = if lower == upper {
                crate::bytesearch::memchr(lower, tail)
            } else {
                crate::bytesearch::memchr2(lower, upper, tail)
            }?;
            let candidate = self.next_start.checked_add(relative)?;
            self.next_start = candidate.checked_add(1)?;
            let match_end = candidate.checked_add(needle_len)?;
            let candidate_bytes = self.haystack.get(candidate..match_end)?;
            if candidate_bytes.eq_ignore_ascii_case(self.needle) {
                return Some(candidate);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::AsciiCaseInsensitiveMatches;

    fn offsets(haystack: &str, needle: &str) -> Vec<usize> {
        AsciiCaseInsensitiveMatches::new(haystack, needle).collect()
    }

    #[test]
    fn finds_every_ascii_case_insensitive_occurrence() {
        for (haystack, needle, want) in [
            ("aA-x", "a", vec![0, 1]),
            ("aA-x", "A", vec![0, 1]),
            ("aA-x", "-", vec![2]),
            ("aAaAa", "AA", vec![0, 1, 2, 3]),
            ("BANANA", "ana", vec![1, 3]),
            ("abc", "ABC", vec![0]),
            ("abc", "", vec![]),
            ("abc", "abcd", vec![]),
            ("abc", "z", vec![]),
        ] {
            assert_eq!(offsets(haystack, needle), want, "{haystack:?} / {needle:?}");
        }
    }
}
