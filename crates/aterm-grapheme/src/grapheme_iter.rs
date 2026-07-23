// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Zero-allocation grapheme cluster iterator (UAX #29).
//!
//! Implements Unicode Technical Annex #29 extended grapheme cluster
//! segmentation using a generated Grapheme_Cluster_Break (GCB) property
//! table (see [`crate::tables::gcb`]) and a 15x15 break decision matrix.
//!
//! Replaces the `unicode-segmentation` crate dependency (#7698, #7737).
//!
//! # UAX #29 Rule Coverage
//!
//! **Fully implemented:**
//! - GB1/GB2  — break at start/end of text
//! - GB3      — CR × LF
//! - GB4/GB5  — break after/before Control, CR, or LF
//! - GB6      — L × (L | V | LV | LVT)
//! - GB7      — (LV | V) × (V | T)
//! - GB8      — (LVT | T) × T
//! - GB9      — × (Extend | ZWJ)
//! - GB9a     — × SpacingMark
//! - GB9b     — Prepend ×
//! - GB11     — Extended_Pictographic Extend\* ZWJ × Extended_Pictographic
//!   (stateful lookahead via `in_ext_pict_zwj`)
//! - GB12/GB13 — Regional_Indicator × Regional_Indicator at odd indices
//!   (parity tracked via `ri_count`)
//! - GB999    — break everywhere else
//!
//! - GB9c     — InCB=Consonant [InCB={Extend,Linker}]* InCB=Linker
//!   [InCB={Extend,Linker}]* × InCB=Consonant
//!   (Indic conjunct cluster — tracked via `incb_*` state bits)
//!
//! **Not implemented:** none of the UAX #29 extended grapheme rules are missing.
//!
//! # Algorithm
//!
//! The state machine operates on pairs of GCB classes: the previous character's
//! GCB class and the next character's GCB class. For each pair, the generated
//! `GRAPHEME_BREAK[prev][next]` matrix answers "break between prev and next?"
//! This handles GB3–GB9b and GB999 directly.
//!
//! Two pieces of state are maintained on top of the matrix for the contextual
//! rules:
//!
//! - `ri_count`: number of consecutive Regional_Indicator codepoints seen
//!   since the last non-RI. GB12/13 says RI×RI holds only when the number of
//!   preceding RIs (including the one to the left of the boundary) is odd.
//! - `in_ext_pict_zwj`: `true` when we are inside an
//!   Extended_Pictographic Extend\* ZWJ prefix, meaning a following
//!   Extended_Pictographic should join via GB11.

use crate::tables::gcb::{GCB, GRAPHEME_BREAK, InCB, gcb_class, incb_class};

// ---------------------------------------------------------------------------
// Core break decision
// ---------------------------------------------------------------------------

/// Segmentation state carried between codepoints.
///
/// The GB11 emoji-ZWJ chain is tracked in the iterator (via
/// `last_base_was_ext_pict`) because it requires looking back past `Extend`
/// codepoints to the most recent non-Extend, non-ZWJ base. Only the "are we
/// in a complete ExtPict Extend\* ZWJ prefix?" bit flows through `BreakState`.
#[derive(Debug, Clone, Copy, Default)]
struct BreakState {
    /// Number of consecutive Regional_Indicator codepoints since the last
    /// non-RI character. Used by GB12/GB13 to pair odd-numbered RIs.
    ri_count: u32,
    /// Whether the prefix `Extended_Pictographic Extend* ZWJ` has just been
    /// completed (i.e. `prev == ZWJ` and a matching prefix preceded it).
    /// When set, a following `Extended_Pictographic` joins via GB11.
    in_ext_pict_zwj: bool,
    /// GB9c tracking — set after we see an `InCB=Consonant` and have not
    /// yet seen a character that resets the conjunct chain.
    /// When set, a run of `InCB∈{Extend,Linker}` has been consumed and is
    /// still eligible to complete a conjunct.
    incb_after_consonant: bool,
    /// GB9c tracking — within the `InCB∈{Extend,Linker}` run following a
    /// Consonant, at least one `InCB=Linker` has been seen. When both this
    /// and `incb_after_consonant` are set, the next `InCB=Consonant` must
    /// NOT introduce a break (GB9c).
    incb_linker_seen: bool,
}

/// Decide whether there is a cluster break between `prev` and `next`.
///
/// `state` carries stateful context (RI parity, GB11 emoji-ZWJ chain) built
/// up from all codepoints before `prev`; this function does not modify it
/// (the iterator updates state after the decision).
#[inline]
fn is_break(prev: GCB, next: GCB, next_incb: InCB, state: &BreakState) -> bool {
    // GB12/GB13: RI × RI only pairs when the preceding RI count is odd.
    // `state.ri_count` is the number of RIs including `prev` if `prev == RI`.
    if prev == GCB::Regional_Indicator && next == GCB::Regional_Indicator {
        // odd => no break (complete a pair); even => break (start new pair).
        return state.ri_count.is_multiple_of(2);
    }

    // GB11: ZWJ × Extended_Pictographic only when inside an ExtPict Extend* ZWJ
    // prefix. The generated table already encodes "ZWJ × ExtPict = no break";
    // we override to BREAK when state says we are not in such a prefix.
    if prev == GCB::ZWJ && next == GCB::Extended_Pictographic && !state.in_ext_pict_zwj {
        return true;
    }

    // GB9c: InCB=Consonant [InCB∈{Extend,Linker}]* InCB=Linker
    //       [InCB∈{Extend,Linker}]* × InCB=Consonant.
    // When the conjunct state is complete, override any pending break between
    // the final InCB=Consonant and the preceding chain.
    if next_incb == InCB::Consonant && state.incb_after_consonant && state.incb_linker_seen {
        return false;
    }

    // Default decision from the generated 15x15 matrix.
    GRAPHEME_BREAK[prev as usize][next as usize]
}

// ---------------------------------------------------------------------------
// Graphemes iterator (yields &str)
// ---------------------------------------------------------------------------

/// Iterator over the grapheme clusters of a string slice.
///
/// Yields `&str` slices, each representing one user-perceived character.
/// This is a zero-allocation iterator suitable for hot paths.
///
/// Created by [`GraphemeClusters::graphemes`].
#[derive(Debug, Clone)]
pub struct Graphemes<'a> {
    /// The remaining (unprocessed) portion of the input.
    rest: &'a str,
    /// Precomputed `(GCB, InCB)` classes of the first character of `rest`.
    ///
    /// `find_first_cluster_end` classifies the codepoint at the break boundary
    /// to decide the break; that boundary char becomes the base of the next
    /// cluster. Carrying its classes forward lets the next `next()` reuse them
    /// instead of re-classifying the same codepoint. `None` on the first call
    /// (nothing precomputed yet) and whenever `rest` is empty.
    next_base: Option<(GCB, InCB)>,
}

impl<'a> Graphemes<'a> {
    /// Create a new grapheme iterator over a string slice.
    #[inline]
    pub(crate) fn new(s: &'a str) -> Self {
        Self {
            rest: s,
            next_base: None,
        }
    }
}

impl<'a> Iterator for Graphemes<'a> {
    type Item = &'a str;

    #[inline]
    fn next(&mut self) -> Option<&'a str> {
        if self.rest.is_empty() {
            return None;
        }

        // Scan forward, consuming characters until a break is found (or the
        // end of input is reached). The returned slice is the first cluster.
        // The returned boundary classes are the classes of the first char of
        // the new `rest`, reused on the next call to avoid re-classifying it.
        let (end, next_base) = find_first_cluster_end(self.rest, self.next_base);
        let grapheme = &self.rest[..end];
        self.rest = &self.rest[end..];
        self.next_base = next_base;
        Some(grapheme)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.rest.len();
        if len == 0 {
            (0, Some(0))
        } else {
            // At least 1 grapheme, at most one per byte.
            (1, Some(len))
        }
    }
}

/// Locate the byte-end of the first grapheme cluster in `s`.
///
/// `first_classes`, when `Some`, supplies the already-computed `(GCB, InCB)` of
/// the first character of `s` (it was classified as the break boundary of the
/// previous cluster), so it is not re-classified; `None` means classify it here
/// (the first call on a fresh iterator).
///
/// Returns `(end, boundary_classes)` where `end` is `s.len()` if the entire
/// string is a single cluster. `boundary_classes` carries the `(GCB, InCB)` of
/// the codepoint at the returned break boundary — i.e. the first char of
/// `s[end..]` — so the next call can reuse it; it is `None` when `end == s.len()`
/// (the remainder is one cluster, so `s[end..]` is empty).
///
/// Panics only on an empty string (which the caller guards against).
#[inline]
#[allow(
    clippy::expect_used,
    reason = "Precondition: caller guarantees s is non-empty"
)]
fn find_first_cluster_end(
    s: &str,
    first_classes: Option<(GCB, InCB)>,
) -> (usize, Option<(GCB, InCB)>) {
    debug_assert!(!s.is_empty());
    let mut iter = s.char_indices();

    // Consume the first character — the base of the cluster. Reuse the
    // precomputed classes when available (the boundary char of the previous
    // cluster); otherwise classify it now. Either way the state init below is
    // derived solely from these classes, so behavior is identical.
    let (_, first) = iter.next().expect("s is non-empty");
    let (mut prev_gcb, first_incb) =
        first_classes.unwrap_or_else(|| (gcb_class(first), incb_class(first)));

    // Track whether the last non-Extend non-ZWJ base was Extended_Pictographic.
    // A following Extend* ZWJ establishes the GB11 prefix.
    let mut last_base_was_ext_pict = prev_gcb == GCB::Extended_Pictographic;
    let mut state = BreakState {
        ri_count: if prev_gcb == GCB::Regional_Indicator {
            1
        } else {
            0
        },
        in_ext_pict_zwj: false,
        incb_after_consonant: first_incb == InCB::Consonant,
        incb_linker_seen: false,
    };

    for (idx, c) in iter {
        let next_gcb = gcb_class(c);
        let next_incb = incb_class(c);

        // Rebuild the GB11 chain flag immediately before the decision, so
        // that a ZWJ following ExtPict Extend* transitions into the chain.
        if prev_gcb == GCB::ZWJ && last_base_was_ext_pict {
            state.in_ext_pict_zwj = true;
        }

        if is_break(prev_gcb, next_gcb, next_incb, &state) {
            // The boundary char `c` (at `idx`) becomes the base of the next
            // cluster; hand its already-computed classes to the next call.
            return (idx, Some((next_gcb, next_incb)));
        }

        // Advance state for the next iteration.
        match next_gcb {
            GCB::Extended_Pictographic => {
                last_base_was_ext_pict = true;
                state.in_ext_pict_zwj = false;
            }
            GCB::Extend => {
                // No change to last_base tracker — Extend extends the base.
            }
            GCB::ZWJ => {
                // Defer flipping `in_ext_pict_zwj` until the next iteration,
                // where we can consult `last_base_was_ext_pict`.
            }
            _ => {
                last_base_was_ext_pict = false;
                state.in_ext_pict_zwj = false;
            }
        }

        // RI parity tracking.
        if next_gcb == GCB::Regional_Indicator {
            state.ri_count = state.ri_count.wrapping_add(1);
        } else {
            state.ri_count = 0;
        }

        // GB9c state transitions for the Indic conjunct chain.
        //
        // - InCB=Consonant: starts (or restarts) a new chain. Any prior
        //   chain with a linker would already have been completed via the
        //   no-break decision above; here we reset and arm for the next.
        // - InCB=Linker: remains in-chain and records that a linker has
        //   been observed since the last Consonant.
        // - InCB=Extend: remains in-chain but does not by itself satisfy
        //   the "linker required" clause.
        // - InCB=None: the chain is broken — the next Consonant must
        //   start a fresh chain.
        match next_incb {
            InCB::Consonant => {
                state.incb_after_consonant = true;
                state.incb_linker_seen = false;
            }
            InCB::Linker => {
                // Stay in chain and note the linker.
                state.incb_linker_seen = true;
            }
            InCB::Extend => {
                // Stay in chain; linker state unchanged.
            }
            InCB::None => {
                state.incb_after_consonant = false;
                state.incb_linker_seen = false;
            }
        }

        prev_gcb = next_gcb;
    }

    // Whole remainder is a single cluster: `s[s.len()..]` is empty, so the
    // following `next()` short-circuits and no boundary classes are needed.
    (s.len(), None)
}

// ---------------------------------------------------------------------------
// GraphemeIndices iterator (yields (usize, &str))
// ---------------------------------------------------------------------------

/// Iterator over grapheme clusters with their byte offsets.
///
/// Yields `(byte_offset, &str)` pairs. The byte offset is relative to the
/// beginning of the original string.
///
/// Created by [`GraphemeClusters::grapheme_indices`].
#[derive(Debug, Clone)]
pub struct GraphemeIndices<'a> {
    /// The original string, kept for byte-offset calculation.
    original: &'a str,
    /// Inner graphemes iterator.
    inner: Graphemes<'a>,
}

impl<'a> GraphemeIndices<'a> {
    /// Create a new grapheme-indices iterator over a string slice.
    #[inline]
    pub(crate) fn new(s: &'a str) -> Self {
        Self {
            original: s,
            inner: Graphemes::new(s),
        }
    }
}

impl<'a> Iterator for GraphemeIndices<'a> {
    type Item = (usize, &'a str);

    #[inline]
    fn next(&mut self) -> Option<(usize, &'a str)> {
        self.inner.next().map(|grapheme| {
            // Both `grapheme` and `self.original` point into the same
            // allocation, so pointer arithmetic gives the byte offset.
            let offset = grapheme.as_ptr() as usize - self.original.as_ptr() as usize;
            (offset, grapheme)
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

// ---------------------------------------------------------------------------
// Extension trait
// ---------------------------------------------------------------------------

/// Extension trait providing grapheme cluster iteration on `&str`.
///
/// This replaces the `UnicodeSegmentation` trait from the `unicode-segmentation`
/// crate. The API is intentionally similar for drop-in replacement.
///
/// # Example
///
/// ```
/// use aterm_grapheme::GraphemeClusters;
///
/// let text = "cafe\u{0301}";  // "café" with combining acute
/// let graphemes: Vec<&str> = text.graphemes().collect();
/// assert_eq!(graphemes, &["c", "a", "f", "e\u{0301}"]);
/// ```
pub trait GraphemeClusters {
    /// Iterate over grapheme clusters.
    fn graphemes(&self) -> Graphemes<'_>;

    /// Iterate over grapheme clusters with byte offsets.
    fn grapheme_indices(&self) -> GraphemeIndices<'_>;
}

impl GraphemeClusters for str {
    #[inline]
    fn graphemes(&self) -> Graphemes<'_> {
        Graphemes::new(self)
    }

    #[inline]
    fn grapheme_indices(&self) -> GraphemeIndices<'_> {
        GraphemeIndices::new(self)
    }
}

// ---------------------------------------------------------------------------
// Unit tests (module-local smoke coverage; the broad UAX #29 suite lives in
// `tests/uax29.rs` and cross-validates against unicode-segmentation).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ascii() {
        let g: Vec<&str> = "hello".graphemes().collect();
        assert_eq!(g, &["h", "e", "l", "l", "o"]);
    }

    #[test]
    fn test_empty() {
        let g: Vec<&str> = "".graphemes().collect();
        assert!(g.is_empty());
    }

    #[test]
    fn test_combining_accent() {
        // "café" with combining acute on 'e'
        let g: Vec<&str> = "cafe\u{0301}".graphemes().collect();
        assert_eq!(g, &["c", "a", "f", "e\u{0301}"]);
    }

    #[test]
    fn test_multiple_combining() {
        // 'a' + combining diaeresis + combining acute
        let g: Vec<&str> = "a\u{0308}\u{0301}b".graphemes().collect();
        assert_eq!(g, &["a\u{0308}\u{0301}", "b"]);
    }

    #[test]
    fn test_emoji_simple() {
        let g: Vec<&str> = "\u{1F600}".graphemes().collect();
        assert_eq!(g, &["\u{1F600}"]);
    }

    #[test]
    fn test_emoji_with_skin_tone() {
        // Wave + skin tone modifier
        let g: Vec<&str> = "\u{1F44B}\u{1F3FD}".graphemes().collect();
        assert_eq!(g, &["\u{1F44B}\u{1F3FD}"]);
    }

    #[test]
    fn test_emoji_zwj_family() {
        // Man + ZWJ + Woman + ZWJ + Girl + ZWJ + Boy
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        let g: Vec<&str> = family.graphemes().collect();
        assert_eq!(g, &[family]);
    }

    #[test]
    fn test_flag_emoji() {
        // US flag: regional indicator U + regional indicator S
        let flag = "\u{1F1FA}\u{1F1F8}";
        let g: Vec<&str> = flag.graphemes().collect();
        assert_eq!(g, &[flag]);
    }

    #[test]
    fn test_three_regional_indicators() {
        // Three RI symbols: should pair as (RI+RI) + (RI)
        let three = "\u{1F1FA}\u{1F1F8}\u{1F1EC}";
        let g: Vec<&str> = three.graphemes().collect();
        assert_eq!(g.len(), 2);
        assert_eq!(g[0], "\u{1F1FA}\u{1F1F8}");
        assert_eq!(g[1], "\u{1F1EC}");
    }

    #[test]
    fn test_four_regional_indicators() {
        // Four RI symbols: (RI+RI) + (RI+RI) = two flags
        let four = "\u{1F1FA}\u{1F1F8}\u{1F1EC}\u{1F1E7}";
        let g: Vec<&str> = four.graphemes().collect();
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn test_variation_selector_16() {
        // Digit 1 + VS16 + combining enclosing keycap = keycap "1️⃣"
        let keycap = "1\u{FE0F}\u{20E3}";
        let g: Vec<&str> = keycap.graphemes().collect();
        assert_eq!(g, &[keycap]);
    }

    #[test]
    fn test_cjk() {
        let g: Vec<&str> = "中文".graphemes().collect();
        assert_eq!(g, &["中", "文"]);
    }

    #[test]
    fn test_mixed() {
        let g: Vec<&str> = "a中b".graphemes().collect();
        assert_eq!(g, &["a", "中", "b"]);
    }

    #[test]
    fn test_crlf() {
        let g: Vec<&str> = "a\r\nb".graphemes().collect();
        assert_eq!(g, &["a", "\r\n", "b"]);
    }

    #[test]
    fn test_cr_alone() {
        let g: Vec<&str> = "a\rb".graphemes().collect();
        assert_eq!(g, &["a", "\r", "b"]);
    }

    #[test]
    fn test_grapheme_indices_offsets() {
        let indices: Vec<(usize, &str)> = "cafe\u{0301}!".grapheme_indices().collect();
        assert_eq!(indices[0], (0, "c"));
        assert_eq!(indices[1], (1, "a"));
        assert_eq!(indices[2], (2, "f"));
        assert_eq!(indices[3], (3, "e\u{0301}"));
        // 'e' is 1 byte, combining acute is 2 bytes => next at 3+1+2 = 6
        assert_eq!(indices[4], (6, "!"));
    }

    #[test]
    fn test_grapheme_indices_cjk() {
        let indices: Vec<(usize, &str)> = "a中b".grapheme_indices().collect();
        assert_eq!(indices[0], (0, "a"));
        assert_eq!(indices[1], (1, "中")); // 'a' is 1 byte
        assert_eq!(indices[2], (4, "b")); // '中' is 3 bytes
    }

    #[test]
    fn test_hangul_jamo() {
        // Hangul syllable: composed form is a single codepoint = 1 grapheme.
        let g: Vec<&str> = "한".graphemes().collect();
        assert_eq!(g, &["한"]);
    }

    #[test]
    fn test_hangul_jamo_sequence() {
        // Hangul L + V + T (decomposed): forms a single syllable via GB6-GB8.
        let g: Vec<&str> = "\u{1100}\u{1161}\u{11A8}".graphemes().collect();
        assert_eq!(g, &["\u{1100}\u{1161}\u{11A8}"]);
    }

    #[test]
    fn test_devanagari_combining() {
        // Devanagari: ka + vowel sign aa (combining) = 1 grapheme
        let g: Vec<&str> = "\u{0915}\u{093E}".graphemes().collect();
        assert_eq!(g, &["\u{0915}\u{093E}"]);
    }

    #[test]
    fn test_tag_sequence_flag() {
        // England flag: black flag + tag G + tag B + tag E + tag N + tag G + cancel tag
        let england = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}";
        let g: Vec<&str> = england.graphemes().collect();
        assert_eq!(g, &[england]);
    }

    #[test]
    fn test_khmer_combining() {
        // Khmer: base consonant + dependent vowel sign (Mc)
        let g: Vec<&str> = "\u{1780}\u{17B6}".graphemes().collect();
        assert_eq!(g, &["\u{1780}\u{17B6}"]);
    }

    #[test]
    fn test_ethiopic_combining() {
        // Ethiopic: base + combining gemination mark (Mn)
        let g: Vec<&str> = "\u{1200}\u{135D}".graphemes().collect();
        assert_eq!(g, &["\u{1200}\u{135D}"]);
    }

    #[test]
    fn test_zwnj_breaks() {
        // ZWNJ (U+200C) has GCB class Extend and should NOT start a new cluster.
        let g: Vec<&str> = "a\u{200C}b".graphemes().collect();
        assert_eq!(g, &["a\u{200C}", "b"]);
    }

    #[test]
    fn test_zwj_standalone() {
        // ZWJ at start — there is no preceding ExtPict, so the following
        // Extended_Pictographic is a separate cluster.
        let g: Vec<&str> = "\u{200D}\u{1F600}".graphemes().collect();
        // ZWJ is GCB::ZWJ; followed by ExtPict, GB11 requires a prior
        // ExtPict Extend* prefix. Since there is none, the rule that wins
        // is still "ZWJ × ExtPict = no break" via GRAPHEME_BREAK[ZWJ][ExtPict]
        // unless we gate it on state.in_ext_pict_zwj (which we do).
        // Result: break between ZWJ and the emoji.
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn test_extpict_zwj_extpict() {
        // Pure GB11: ExtPict × ZWJ × ExtPict (no intervening Extend).
        // 🤝 ZWJ 🤝
        let s = "\u{1F91D}\u{200D}\u{1F91D}";
        let g: Vec<&str> = s.graphemes().collect();
        assert_eq!(g, &[s]);
    }

    #[test]
    fn test_extpict_extend_zwj_extpict() {
        // GB11 with Extend between ExtPict and ZWJ: ExtPict Extend+ ZWJ ExtPict.
        // eye + VS15 (Extend) + ZWJ + speech balloon
        let s = "\u{1F441}\u{FE0F}\u{200D}\u{1F5E8}";
        let g: Vec<&str> = s.graphemes().collect();
        assert_eq!(g, &[s]);
    }

    #[test]
    fn test_six_regional_indicators() {
        // Six RIs = three flags, not two (RI pairs left-to-right).
        let six = "\u{1F1FA}\u{1F1F8}\u{1F1EC}\u{1F1E7}\u{1F1EB}\u{1F1F7}";
        let g: Vec<&str> = six.graphemes().collect();
        assert_eq!(g.len(), 3);
    }

    #[test]
    fn test_ri_followed_by_letter() {
        // RI + letter: RI is a singleton, letter starts its own cluster.
        let g: Vec<&str> = "\u{1F1FA}a".graphemes().collect();
        assert_eq!(g, &["\u{1F1FA}", "a"]);
    }

    #[test]
    fn test_empty_iteration() {
        let mut iter = "".graphemes();
        assert_eq!(iter.next(), None);
        let mut iter = "".grapheme_indices();
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_gb9c_devanagari_conjunct() {
        // GB9c: Devanagari ka + virama (Linker) + ya = single conjunct cluster.
        // KA=U+0915 (InCB=Consonant), VIRAMA=U+094D (InCB=Linker),
        // YA=U+092F (InCB=Consonant).
        let s = "\u{0915}\u{094D}\u{092F}";
        let g: Vec<&str> = s.graphemes().collect();
        assert_eq!(g, &[s], "GB9c: ka + virama + ya should be one cluster");
    }

    #[test]
    fn test_gb9c_malayalam_conjunct() {
        // GB9c: Malayalam ka + virama (Linker) + sa = single conjunct cluster.
        // KA=U+0D15 (InCB=Consonant), VIRAMA=U+0D4D (InCB=Linker),
        // SA=U+0D38 (InCB=Consonant).
        let s = "\u{0D15}\u{0D4D}\u{0D38}";
        let g: Vec<&str> = s.graphemes().collect();
        assert_eq!(g, &[s], "GB9c: ka + virama + sa should be one cluster");
    }

    #[test]
    fn test_gb9c_consonant_without_linker_breaks() {
        // Without the linker, two Consonants form two clusters
        // (no Extend or Linker between them).
        let s = "\u{0915}\u{092F}";
        let g: Vec<&str> = s.graphemes().collect();
        assert_eq!(g.len(), 2, "Consonants without linker should break");
    }

    #[test]
    fn test_gb9c_linker_requires_consonant_start() {
        // Linker alone at the start does not set up a conjunct chain.
        let s = "\u{094D}\u{0915}";
        let g: Vec<&str> = s.graphemes().collect();
        // Linker is InCB=Extend-class so it clings to preceding text,
        // but here there is no preceding base in the same cluster.
        // The leading linker becomes its own cluster; ka starts fresh.
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn test_boundary_class_reuse_across_clusters() {
        // Regression for the next_base threading: the boundary codepoint that
        // ends one cluster becomes the base of the next, and its precomputed
        // (GCB, InCB) classes are reused. Exercise boundaries whose classes
        // drive state init (Consonant for GB9c, Regional_Indicator for GB12/13,
        // Extended_Pictographic for GB11) to confirm the reused classes
        // reconstruct the same state as a fresh classification would.

        // Two adjacent Devanagari conjuncts: each conjunct ends and the next
        // begins on an InCB=Consonant whose incb_after_consonant init matters.
        // ka+virama+ya | ka+virama+ya
        let conjuncts = "\u{0915}\u{094D}\u{092F}\u{0915}\u{094D}\u{092F}";
        let g: Vec<&str> = conjuncts.graphemes().collect();
        assert_eq!(
            g,
            &["\u{0915}\u{094D}\u{092F}", "\u{0915}\u{094D}\u{092F}"],
            "GB9c: two conjuncts split exactly at the consonant boundary"
        );

        // Odd RI run boundary: (RI RI)(RI a). The third RI is a boundary base
        // whose ri_count must re-init to 1, not inherit the prior parity.
        let ris = "\u{1F1FA}\u{1F1F8}\u{1F1EC}a";
        let g: Vec<&str> = ris.graphemes().collect();
        assert_eq!(g, &["\u{1F1FA}\u{1F1F8}", "\u{1F1EC}", "a"]);

        // ExtPict boundary then standalone ZWJ+ExtPict: the second emoji is a
        // boundary base; last_base_was_ext_pict must derive from its class.
        let emoji = "\u{1F600}\u{1F601}";
        let g: Vec<&str> = emoji.graphemes().collect();
        assert_eq!(g, &["\u{1F600}", "\u{1F601}"]);

        // grapheme_indices must agree (same threading through the inner iter).
        let idx: Vec<(usize, &str)> = conjuncts.grapheme_indices().collect();
        assert_eq!(idx[0].0, 0);
        assert_eq!(idx[1].0, "\u{0915}\u{094D}\u{092F}".len());
    }

    #[test]
    fn test_plane14_tag_classification() {
        // Regression: Plane 14 tag characters were previously out of the GCB
        // table range and defaulted to Other, breaking subdivision-flag
        // sequences. Verify they classify as Extend.
        use crate::tables::gcb::{GCB, gcb_class};
        assert_eq!(gcb_class('\u{E0020}'), GCB::Extend);
        assert_eq!(gcb_class('\u{E0067}'), GCB::Extend);
        assert_eq!(gcb_class('\u{E007F}'), GCB::Extend);
        assert_eq!(gcb_class('\u{E0100}'), GCB::Extend); // VS17
        assert_eq!(gcb_class('\u{E01EF}'), GCB::Extend); // VS256
    }
}
