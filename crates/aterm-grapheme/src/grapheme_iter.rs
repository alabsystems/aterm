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

// ---------------------------------------------------------------------------
// Core break decision
// ---------------------------------------------------------------------------

/// Segmentation state carried between codepoints.
///
/// The GB11 emoji-ZWJ chain is tracked in the iterator (via
/// `ext_pict_then_extends`, "the text so far ends with ExtPict Extend\*")
/// because it requires looking back past `Extend` codepoints to the
/// pictograph. Only the "are we in a complete ExtPict Extend\* ZWJ prefix?"
/// bit flows through `BreakState`.
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

// A `clean { … }` island is Clean surface that only a Trust rustc can parse. A
// stock rustc (the cross cells of `verify cells`, the x86_64-apple-darwin
// compat slice) parses a cfg'd-out module's body all the same, so the island's
// module goes through this macro, which drops its tokens there and emits them
// unchanged under `cfg(clean_islands)`: set by this crate's build script
// whenever the compiler is Trust (crates/aterm-log/clean_islands.rs), so the
// island is kernel-checked at every Trust build, not only the verify lane.
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

    // Consume the first character — the base of the cluster. Reuse the
    // precomputed classes when available (the boundary char of the previous
    // cluster); otherwise classify it now. Either way the state init below is
    // derived solely from these classes, so behavior is identical.
    let (_, first) = iter.next().expect("s is non-empty");
    let (mut prev_gcb, first_incb) =
        first_classes.unwrap_or_else(|| (gcb_class(first), incb_class(first)));

    // Whether the text so far ends with `ExtPict Extend*`: the only thing a
    // ZWJ may follow for GB11 to join the next pictograph. A ZWJ ends it (a
    // second ZWJ, or an Extend after the ZWJ, is no longer that prefix).
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
            // The boundary char `c` (at `idx`) becomes the base of the next
            // cluster; hand its already-computed classes to the next call.
            return (idx, Some((next_gcb, next_incb)));
        }

        // Advance the GB11 state: `in_ext_pict_zwj` holds exactly when the
        // text so far ends with `ExtPict Extend* ZWJ`.
        match next_gcb {
            GCB::Extended_Pictographic => {
                ext_pict_then_extends = true;
                state.in_ext_pict_zwj = false;
            }
            GCB::Extend => {
                // Extend continues `ExtPict Extend*` but ends `... ZWJ`.
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

/// Extension trait providing grapheme cluster iteration on `&str`, with an
/// API close to `unicode-segmentation`'s `UnicodeSegmentation`.
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
// Unit tests. The UAX #29 behaviour itself is pinned by
// `tests/uax29_conformance.rs` (the official GraphemeBreakTest file plus the
// recorded answers where unicode-segmentation disagreed); only what that suite
// cannot reach stays here — code points the official file never uses (U+135D,
// U+1F91D) and two regressions in this iterator's own state threading and
// tables.
// ---------------------------------------------------------------------------

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
