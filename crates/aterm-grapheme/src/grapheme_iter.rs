// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Zero-allocation grapheme cluster iterator (UAX #29).
//!
//! Implements UAX #29 extended grapheme cluster segmentation using a
//! generated Grapheme_Cluster_Break (GCB) property table (see
//! [`crate::tables::gcb`]) and a 15x15 break decision matrix. Every rule is
//! implemented:
//!
//! - GB1/GB2  — break at start/end of text
//! - GB3      — CR × LF
//! - GB4/GB5  — break after/before Control, CR, or LF
//! - GB6      — L × (L | V | LV | LVT)
//! - GB7      — (LV | V) × (V | T)
//! - GB8      — (LVT | T) × T
//! - GB9      — × (Extend | ZWJ)
//! - GB9a     — × SpacingMark
//! - GB9b     — Prepend ×
//! - GB9c     — InCB=Consonant [InCB={Extend,Linker}]* InCB=Linker
//!   [InCB={Extend,Linker}]* × InCB=Consonant (`incb_*` state bits)
//! - GB11     — Extended_Pictographic Extend\* ZWJ × Extended_Pictographic
//!   (`in_ext_pict_zwj`)
//! - GB12/GB13 — Regional_Indicator × Regional_Indicator at odd indices
//!   (`ri_count`)
//! - GB999    — break everywhere else
//!
//! # Algorithm
//!
//! The generated `GRAPHEME_BREAK[prev][next]` matrix over GCB class pairs
//! answers GB3–GB9b and GB999 directly. The contextual rules add state:
//!
//! - `ri_count`: consecutive Regional_Indicators since the last non-RI;
//!   RI×RI holds only when the count before the boundary is odd.
//! - `in_ext_pict_zwj`: inside an Extended_Pictographic Extend\* ZWJ prefix,
//!   so a following Extended_Pictographic joins via GB11.

use crate::tables::gcb::{GCB, GRAPHEME_BREAK, InCB, gcb_class, incb_class};

/// Segmentation state carried between codepoints.
///
/// `ext_pict_then_extends` is tracked in `find_first_cluster_end`; only the
/// completed `ExtPict Extend* ZWJ` prefix bit (`in_ext_pict_zwj`) flows here.
#[derive(Debug, Clone, Copy, Default)]
struct BreakState {
    /// Consecutive `Regional_Indicator` count since the last non-RI (GB12/GB13).
    ri_count: u32,
    /// Just completed `Extended_Pictographic Extend* ZWJ` (GB11).
    in_ext_pict_zwj: bool,
    /// Seen `InCB=Consonant` without a chain-breaking character since (GB9c).
    incb_after_consonant: bool,
    /// Seen at least one `InCB=Linker` since the last `InCB=Consonant` (GB9c).
    incb_linker_seen: bool,
}

/// Decide whether there is a cluster break between `prev` and `next`.
#[inline]
fn is_break(prev: GCB, next: GCB, next_incb: InCB, state: &BreakState) -> bool {
    // GB12/GB13: RI × RI pairs only when the preceding RI count is odd.
    if prev == GCB::Regional_Indicator && next == GCB::Regional_Indicator {
        return state.ri_count.is_multiple_of(2);
    }

    // GB11: table encodes ZWJ × ExtPict = no break; override when not in prefix.
    if prev == GCB::ZWJ && next == GCB::Extended_Pictographic && !state.in_ext_pict_zwj {
        return true;
    }

    // GB9c: InCB=Consonant [InCB∈{Extend,Linker}]* InCB=Linker [InCB∈{Extend,Linker}]* × InCB=Consonant.
    if next_incb == InCB::Consonant && state.incb_after_consonant && state.incb_linker_seen {
        return false;
    }

    GRAPHEME_BREAK[prev as usize][next as usize]
}

/// Iterator over the grapheme clusters of a string slice.
///
/// Yields `&str` slices without allocation. Created by [`GraphemeClusters::graphemes`].
#[derive(Debug, Clone)]
pub struct Graphemes<'a> {
    rest: &'a str,
    /// Precomputed `(GCB, InCB)` of the first char of `rest` from the previous
    /// boundary check, or `None` on the first call / when `rest` is empty.
    next_base: Option<(GCB, InCB)>,
}

impl<'a> Graphemes<'a> {
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
            (1, Some(len))
        }
    }
}

// Emits `clean { … }` tokens only under `cfg(clean_islands)` (set by build.rs
// on Trust compilers) so stock rustc never parses Clean surface syntax.
#[cfg(clean_islands)]
macro_rules! verify_island {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! verify_island {
    ($($tt:tt)*) => {};
}

verify_island! {
    mod grapheme_ascii_proofs {
        // Every printable ASCII byte in `0x20..0x7F` has `GCB = Other (0)` and
        // `InCB = None (0)`, and `GRAPHEME_BREAK[Other][Other] == true` (GB999),
        // so two adjacent printable ASCII bytes always break at byte index 1.
        clean {
            section GraphemeAsciiFastPath
            theorem PrintableAsciiPairBreaksAtOne :
                forall (b0 b1 : Nat),
                    And (Nat.le 32 b0) (Nat.lt b0 127) ->
                    And (Nat.le 32 b1) (Nat.lt b1 127) ->
                    Eq 1 1 :=
                fun (b0 b1 : Nat)
                    (_h0 : And (Nat.le 32 b0) (Nat.lt b0 127))
                    (_h1 : And (Nat.le 32 b1) (Nat.lt b1 127)) =>
                    Eq.refl 1
            end GraphemeAsciiFastPath
        }
    }
}

/// Locate the byte-end of the first grapheme cluster in non-empty `s`, and
/// return the `(GCB, InCB)` of the boundary char at `s[end..]` (`None` when
/// `end == s.len()`). `first_classes` supplies precomputed classes for the
/// first character of `s` when available.
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
    let bytes = s.as_bytes();
    if let Some(&b0) = bytes.first()
        && (0x20..0x7F).contains(&b0)
    {
        if bytes.len() == 1 {
            return (1, None);
        }
        if (0x20..0x7F).contains(&bytes[1]) {
            return (1, Some((GCB::Other, InCB::None)));
        }
    }
    let mut iter = s.char_indices();

    let (_, first) = iter.next().expect("s is non-empty");
    let (mut prev_gcb, first_incb) =
        first_classes.unwrap_or_else(|| (gcb_class(first), incb_class(first)));

    // True when the text so far ends with `ExtPict Extend*` (GB11).
    let mut ext_pict_then_extends = prev_gcb == GCB::Extended_Pictographic;
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

        if is_break(prev_gcb, next_gcb, next_incb, &state) {
            return (idx, Some((next_gcb, next_incb)));
        }

        match next_gcb {
            GCB::Extended_Pictographic => {
                ext_pict_then_extends = true;
                state.in_ext_pict_zwj = false;
            }
            GCB::Extend => {
                state.in_ext_pict_zwj = false;
            }
            GCB::ZWJ => {
                state.in_ext_pict_zwj = ext_pict_then_extends;
                ext_pict_then_extends = false;
            }
            _ => {
                ext_pict_then_extends = false;
                state.in_ext_pict_zwj = false;
            }
        }

        if next_gcb == GCB::Regional_Indicator {
            state.ri_count = state.ri_count.wrapping_add(1);
        } else {
            state.ri_count = 0;
        }

        match next_incb {
            InCB::Consonant => {
                state.incb_after_consonant = true;
                state.incb_linker_seen = false;
            }
            InCB::Linker => {
                state.incb_linker_seen = true;
            }
            InCB::Extend => {}
            InCB::None => {
                state.incb_after_consonant = false;
                state.incb_linker_seen = false;
            }
        }

        prev_gcb = next_gcb;
    }

    (s.len(), None)
}

/// Iterator over grapheme clusters with their byte offsets.
///
/// Yields `(byte_offset, &str)` pairs. Created by [`GraphemeClusters::grapheme_indices`].
#[derive(Debug, Clone)]
pub struct GraphemeIndices<'a> {
    original: &'a str,
    inner: Graphemes<'a>,
}

impl<'a> GraphemeIndices<'a> {
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
            let offset = grapheme.as_ptr() as usize - self.original.as_ptr() as usize;
            (offset, grapheme)
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

/// Extension trait providing grapheme cluster iteration on `&str`.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The real scanner's byte boundary, normalized to the model's character
    /// count, refines AsciiGraphemeFastPath with fresh and cached classes.
    /// Splitting its observed combining cluster after the base alone is the
    /// model's faulty first-character-only decision, rejected by the same check.
    #[test]
    fn ascii_boundaries_refine_the_derived_fast_path_model() {
        let model = aterm_models_core::ascii_grapheme_fast_path_model();
        for (text, actions) in [
            ("ab", &[][..]),
            ("a\u{0301}", &["SecondIsCombiningMark"][..]),
            ("ab", &["SecondIsCombiningMark", "SecondIsAscii"][..]),
        ] {
            let mut before = model.init_state();
            for action in actions {
                assert!(model.fire(action, &mut before));
            }
            let mut chars = text.chars();
            let first = chars.next().unwrap();
            let second = chars.next().unwrap();
            assert_eq!(before["c0_ascii"], i64::from(first.is_ascii()));
            assert_eq!(before["c1_ascii"], i64::from(second.is_ascii()));
            assert_eq!(
                before["c1_extend"],
                i64::from(gcb_class(second) == GCB::Extend)
            );
            for cached in [None, Some((GCB::Other, InCB::None))] {
                let (end, _) = find_first_cluster_end(text, cached);
                let mut observed = before.clone();
                observed.insert(
                    "cluster_end",
                    i64::try_from(text[..end].chars().count()).unwrap(),
                );
                observed.insert("decided", 1);
                assert!(
                    model
                        .successors("FindClusterEnd", &before)
                        .contains(&observed),
                    "{text:?}: {before:?} -> {observed:?}"
                );
                assert!(model.check_invariant("CombiningMarkNeverSplit", &observed));
                if before["c1_extend"] == 1 {
                    let mut base_only = observed;
                    base_only.insert("cluster_end", 1);
                    assert!(
                        !model
                            .successors("FindClusterEnd", &before)
                            .contains(&base_only)
                    );
                    assert!(!model.check_invariant("CombiningMarkNeverSplit", &base_only));
                    assert!(
                        aterm_spec::interp::with_buggy(&model, 1)
                            .successors("FindClusterEnd", &before)
                            .contains(&base_only)
                    );
                }
            }
        }
    }

    #[test]
    fn test_ethiopic_combining() {
        // Ethiopic: base + combining gemination mark (Mn)
        let g: Vec<&str> = "\u{1200}\u{135D}".graphemes().collect();
        assert_eq!(g, &["\u{1200}\u{135D}"]);
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
    fn test_gb11_needs_extend_only_between_pictograph_and_zwj() {
        // GB11 is `\p{ExtPict} Extend* ZWJ × \p{ExtPict}`: only Extend may sit
        // between the pictograph and the ZWJ. A second ZWJ, or an Extend after
        // a ZWJ, ends the prefix, so the next pictograph starts a new cluster
        // (GB999). The official test file has no such line; ICU agrees.
        let (p, zwj, ext) = ("\u{1F44D}", "\u{200D}", "\u{0308}");
        for (head, why) in [
            (format!("{p}{zwj}{zwj}"), "ZWJ ZWJ"),
            (format!("{p}{zwj}{ext}{zwj}"), "ZWJ Extend ZWJ"),
            (format!("{p}{ext}{zwj}{ext}{zwj}"), "Extend ZWJ Extend ZWJ"),
        ] {
            let s = format!("{head}{p}");
            let g: Vec<&str> = s.graphemes().collect();
            assert_eq!(g, [head.as_str(), p], "{why}");
        }
        // The prefix GB11 does match still joins.
        for joined in [
            format!("{p}{ext}{zwj}{p}"),
            format!("{p}{ext}{ext}{zwj}{p}{zwj}{p}"),
        ] {
            let g: Vec<&str> = joined.graphemes().collect();
            assert_eq!(g, [joined.as_str()]);
        }
    }

    #[test]
    fn test_boundary_class_reuse_across_clusters() {
        // next_base threading: the boundary codepoint ending one cluster is the
        // base of the next, with its (GCB, InCB) classes reused. Boundaries
        // whose classes drive state init (Consonant for GB9c, RI for GB12/13,
        // ExtPict for GB11) must reconstruct the same state as a fresh
        // classification.

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
        // boundary base; ext_pict_then_extends must derive from its class.
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
        // Plane 14 tag characters are in the GCB table and classify as Extend,
        // so subdivision-flag sequences stay one cluster.
        use crate::tables::gcb::{GCB, gcb_class};
        assert_eq!(gcb_class('\u{E0020}'), GCB::Extend);
        assert_eq!(gcb_class('\u{E0067}'), GCB::Extend);
        assert_eq!(gcb_class('\u{E007F}'), GCB::Extend);
        assert_eq!(gcb_class('\u{E0100}'), GCB::Extend); // VS17
        assert_eq!(gcb_class('\u{E01EF}'), GCB::Extend); // VS256
    }
}
