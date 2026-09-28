// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Unicode Bidirectional Algorithm (UAX #9) line reordering for the terminal.
//!
//! Terminal lines are stored in LOGICAL order (the order characters were
//! written). To display mixed left-to-right / right-to-left text correctly the
//! renderer needs the VISUAL order — the left-to-right sequence of columns on
//! screen. This crate computes that reordering: given a line's characters it
//! resolves UAX #9 embedding levels and returns a visual→logical permutation the
//! renderer can apply per row.
//!
//! ## Scope
//!
//! The whole algorithm through rule L2, over one line:
//!
//! - **P1–P3** — the line splits into paragraphs at `B`; each paragraph's base
//!   level comes from its first strong character (skipping isolates) or a
//!   caller-forced direction.
//! - **X1–X10** — explicit embeddings, overrides and isolates (LRE/RLE/LRO/RLO/
//!   PDF, LRI/RLI/FSI/PDI) on the 125-deep directional status stack, X9's
//!   removals, and the isolating run sequences with their `sos`/`eos`.
//! - **W1–W7**, **N0** (bracket pairs, BD16, with canonical equivalence and the
//!   NSM follow-up), **N1/N2**, **I1/I2**, **L1** and **L2**.
//! - The full `Bidi_Class` property ([`bidi_class`]) and `Bidi_Paired_Bracket`
//!   pairs ([`bracket_of`]), generated from the UCD (`src/tables.rs`,
//!   `tables/gen.py`).
//!
//! Conformance is pinned against the UCD's own `BidiTest.txt` and
//! `BidiCharacterTest.txt` (`tests/uax9_conformance.rs`). L3/L4 (combining-mark
//! and mirrored-glyph rendering) belong to the renderer and are out of scope.
//!
//! Characters removed by X9 (the embedding controls and `BN`) take the level of
//! the character before them, so every character — removed or not — has a place
//! in the visual order and the result is always a permutation of the input.
//!
//! ## Example
//!
//! ```
//! use aterm_bidi::{reorder_visual_to_logical, BaseDirection};
//! // "abc" is pure LTR → identity order.
//! assert_eq!(reorder_visual_to_logical(&['a', 'b', 'c'], BaseDirection::Auto), vec![0, 1, 2]);
//! // A pure-RTL line (Hebrew aleph-bet-gimel) displays reversed.
//! let hebrew = ['\u{05D0}', '\u{05D1}', '\u{05D2}'];
//! assert_eq!(reorder_visual_to_logical(&hebrew, BaseDirection::Auto), vec![2, 1, 0]);
//! ```

#![forbid(unsafe_code)]

mod tables;

/// The base (paragraph) direction to resolve a line against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BaseDirection {
    /// Detect from the first strong character (UAX #9 P2/P3); default LTR if none.
    Auto,
    /// Force left-to-right (base level 0).
    Ltr,
    /// Force right-to-left (base level 1).
    Rtl,
}

/// The UAX #9 bidirectional character classes (`Bidi_Class`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BidiClass {
    /// Left-to-Right (strong).
    L,
    /// Right-to-Left (strong).
    R,
    /// Right-to-Left Arabic (strong).
    AL,
    /// European Number.
    EN,
    /// European Number Separator.
    ES,
    /// European Number Terminator.
    ET,
    /// Arabic Number.
    AN,
    /// Common Number Separator.
    CS,
    /// Non-Spacing Mark.
    NSM,
    /// Boundary Neutral.
    BN,
    /// Paragraph Separator.
    B,
    /// Segment Separator.
    S,
    /// Whitespace.
    WS,
    /// Other Neutral.
    ON,
    /// Left-to-Right Embedding.
    LRE,
    /// Left-to-Right Override.
    LRO,
    /// Right-to-Left Embedding.
    RLE,
    /// Right-to-Left Override.
    RLO,
    /// Pop Directional Format.
    PDF,
    /// Left-to-Right Isolate.
    LRI,
    /// Right-to-Left Isolate.
    RLI,
    /// First Strong Isolate.
    FSI,
    /// Pop Directional Isolate.
    PDI,
}

use BidiClass::{
    AL, AN, B, BN, CS, EN, ES, ET, FSI, L, LRE, LRI, LRO, NSM, ON, PDF, PDI, R, RLE, RLI, RLO, S,
    WS,
};

/// A character's paired-bracket property (UAX #9 BD14/BD15), keyed by the
/// code point of the OPENING bracket of its pair after canonical equivalence
/// (U+2329/U+232A pair with U+3008/U+3009), so an opener and a closer match
/// exactly when their keys are equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Bracket {
    /// Not a paired bracket.
    #[default]
    None,
    /// An opening paired bracket.
    Open(u32),
    /// A closing paired bracket.
    Close(u32),
}

/// The bidirectional class of `c` (the UCD `Bidi_Class` property).
#[must_use]
pub fn bidi_class(c: char) -> BidiClass {
    let u = u32::from(c);
    let table = tables::BIDI_CLASS_RANGES;
    match table.binary_search_by(|&(lo, hi, _)| {
        if hi < u {
            std::cmp::Ordering::Less
        } else if lo > u {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    }) {
        Ok(i) => table.get(i).map_or(L, |&(_, _, class)| class),
        Err(_) => L,
    }
}

/// Canonical equivalence for the two bracket pairs that have one (BD16).
fn canonical_bracket(u: u32) -> u32 {
    match u {
        0x2329 => 0x3008,
        0x232A => 0x3009,
        other => other,
    }
}

/// The paired-bracket property of `c` (see [`Bracket`]).
#[must_use]
pub fn bracket_of(c: char) -> Bracket {
    let u = u32::from(c);
    if tables::BRACKET_PAIRS
        .binary_search_by_key(&u, |&(open, _)| open)
        .is_ok()
    {
        return Bracket::Open(canonical_bracket(u));
    }
    let closers = tables::BRACKET_CLOSERS;
    match closers.binary_search_by_key(&u, |&(close, _)| close) {
        Ok(i) => closers.get(i).map_or(Bracket::None, |&(_, open)| {
            Bracket::Close(canonical_bracket(open))
        }),
        Err(_) => Bracket::None,
    }
}

/// Whether rule X9 removes a character of this class (the embedding controls
/// and `BN`). Removed characters get no level of their own in UAX #9's
/// conformance data; here they take the level before them (see the crate docs).
#[must_use]
pub fn removed_by_x9(class: BidiClass) -> bool {
    matches!(class, RLE | LRE | RLO | LRO | PDF | BN)
}

/// Quick test for whether a line needs the Bidirectional Algorithm at all.
///
/// Returns `true` if any character is right-to-left (`R`/`AL`), an Arabic
/// number (`AN`), or an explicit control that can open a right-to-left level
/// (RLE/RLO/RLI/FSI). A line for which this is `false` resolves to even levels
/// only, so its visual order is the identity and the renderer can skip it.
#[must_use]
pub fn has_bidi(text: &[char]) -> bool {
    text.iter().any(|&c| is_bidi_class(bidi_class(c)))
}

/// Class-slice companion to [`has_bidi`].
#[must_use]
pub fn has_bidi_classes(classes: &[BidiClass]) -> bool {
    classes.iter().any(|&c| is_bidi_class(c))
}

fn is_bidi_class(c: BidiClass) -> bool {
    matches!(c, R | AL | AN | RLE | RLO | RLI | FSI)
}

fn classes_and_brackets(text: &[char]) -> (Vec<BidiClass>, Vec<Bracket>) {
    let mut classes = Vec::with_capacity(text.len());
    let mut brackets = Vec::with_capacity(text.len());
    for &c in text {
        classes.push(bidi_class(c));
        brackets.push(bracket_of(c));
    }
    (classes, brackets)
}

/// Compute the visual→logical index permutation for `text`.
///
/// The returned `Vec` has the same length as `text`; `result[v] == l` means the
/// character at logical index `l` is drawn at visual column `v` (left to right).
/// For pure-LTR input this is the identity `0,1,2,…`.
#[must_use]
pub fn reorder_visual_to_logical(text: &[char], base: BaseDirection) -> Vec<usize> {
    let levels = resolve_levels(text, base);
    let (classes, _) = classes_and_brackets(text);
    let mut order = Vec::new();
    reorder_paragraphs_into(&classes, &levels, &mut order);
    order
}

/// Convenience wrapper over [`reorder_visual_to_logical`] taking a `&str`.
#[must_use]
#[cfg(test)]
pub fn reorder_str(s: &str, base: BaseDirection) -> Vec<usize> {
    let chars: Vec<char> = s.chars().collect();
    reorder_visual_to_logical(&chars, base)
}

/// Compute the visual→logical **cell** permutation for a terminal row.
///
/// Terminal rows store a WIDE glyph (CJK, wide emoji) as two cells: a lead cell
/// carrying the glyph and a right-half *continuation* cell. The Bidirectional
/// Algorithm runs on the LOGICAL CHARACTERS — one per lead/single cell — and each
/// character's cell(s) are then emitted as a unit in lead-then-continuation
/// order: a wide glyph is never mirrored, only its *position* in the line is
/// reordered. The result is always a permutation of `0..n`; a malformed
/// continuation cell with no lead is treated as its own character.
#[must_use]
#[cfg(test)]
pub fn reorder_cells(
    cell_chars: &[char],
    is_wide_continuation: &[bool],
    base: BaseDirection,
) -> Vec<usize> {
    let (classes, brackets) = classes_and_brackets(cell_chars);
    reorder_cells_with_classes(&classes, &brackets, is_wide_continuation, base)
}

/// Class-taking companion to [`reorder_cells`]: per-CELL classes and brackets
/// (parallel to `is_wide_continuation`), so a caller that already computed them
/// does not recompute. A missing bracket entry reads as [`Bracket::None`].
#[must_use]
pub fn reorder_cells_with_classes(
    cell_classes: &[BidiClass],
    cell_brackets: &[Bracket],
    is_wide_continuation: &[bool],
    base: BaseDirection,
) -> Vec<usize> {
    let mut scratch = Scratch::default();
    let mut out = Vec::new();
    reorder_cells_with_classes_into(
        cell_classes,
        cell_brackets,
        is_wide_continuation,
        base,
        &mut scratch,
        &mut out,
    );
    out
}

/// Reusable working memory for the `_into` entry points: a per-row caller that
/// keeps one of these reorders without heap allocation after warmup.
#[derive(Debug, Default)]
pub struct Scratch {
    logical: Vec<BidiClass>,
    logical_brackets: Vec<Bracket>,
    lead_cell: Vec<usize>,
    has_cont: Vec<bool>,
    levels: Vec<u8>,
    char_order: Vec<usize>,
    resolve: Resolve,
}

/// Buffer-in/buffer-out companion to [`reorder_cells_with_classes`]: the
/// visual→logical CELL permutation is written into `out` (cleared then
/// refilled), and every working buffer lives in `scratch`.
pub fn reorder_cells_with_classes_into(
    cell_classes: &[BidiClass],
    cell_brackets: &[Bracket],
    is_wide_continuation: &[bool],
    base: BaseDirection,
    scratch: &mut Scratch,
    out: &mut Vec<usize>,
) {
    // 1. Fold cells into logical characters: each non-continuation cell starts a
    //    character that also owns the immediately-following continuation cell.
    let s = scratch;
    s.logical.clear();
    s.logical_brackets.clear();
    s.lead_cell.clear();
    s.has_cont.clear();
    let mut i = 0;
    while i < cell_classes.len() {
        let cont = is_wide_continuation
            .get(i.wrapping_add(1))
            .copied()
            .unwrap_or(false);
        if let Some(&class) = cell_classes.get(i) {
            s.logical.push(class);
            s.logical_brackets
                .push(cell_brackets.get(i).copied().unwrap_or_default());
            s.lead_cell.push(i);
            s.has_cont.push(cont);
        }
        i = i.saturating_add(if cont { 2 } else { 1 });
    }

    // 2. Reorder the logical characters.
    resolve_into(
        &s.logical,
        &s.logical_brackets,
        base,
        &mut s.resolve,
        &mut s.levels,
    );
    reorder_paragraphs_into(&s.logical, &s.levels, &mut s.char_order);

    // 3. Expand each logical character back to its cell(s).
    out.clear();
    for &c in &s.char_order {
        if let Some(&lead) = s.lead_cell.get(c) {
            out.push(lead);
            if s.has_cont.get(c).copied().unwrap_or(false) {
                out.push(lead.saturating_add(1));
            }
        }
    }
}

/// Resolve the UAX #9 embedding level of every character in `text` (see the
/// crate docs for the scope; removed X9 characters take the level before them).
#[must_use]
pub fn resolve_levels(text: &[char], base: BaseDirection) -> Vec<u8> {
    let (classes, brackets) = classes_and_brackets(text);
    resolve_levels_from_classes(&classes, &brackets, base)
}

/// [`resolve_levels`] from precomputed classes and brackets (a missing bracket
/// entry reads as [`Bracket::None`]).
#[must_use]
pub fn resolve_levels_from_classes(
    classes: &[BidiClass],
    brackets: &[Bracket],
    base: BaseDirection,
) -> Vec<u8> {
    let mut resolve = Resolve::default();
    let mut levels = Vec::new();
    resolve_into(classes, brackets, base, &mut resolve, &mut levels);
    levels
}

/// The base level of the FIRST paragraph of `text` (0 = LTR, 1 = RTL).
#[must_use]
#[cfg(test)]
pub fn paragraph_level(text: &[char], base: BaseDirection) -> u8 {
    let (classes, _) = classes_and_brackets(text);
    let end = classes
        .iter()
        .position(|&c| c == B)
        .map_or(classes.len(), |i| i + 1);
    let mut matching = Vec::new();
    match_isolates(classes.get(..end).unwrap_or(&[]), &mut matching);
    paragraph_level_of(classes.get(..end).unwrap_or(&[]), &matching, base)
}

/// Apply UAX #9 rule L2 to a resolved level array, returning the visual→logical
/// permutation.
///
/// Reverses contiguous runs from the highest level down to the lowest odd level.
#[must_use]
pub fn reorder_from_levels(levels: &[u8]) -> Vec<usize> {
    let mut order = Vec::new();
    reorder_from_levels_into(levels, &mut order);
    order
}

/// Buffer-out companion to [`reorder_from_levels`].
pub fn reorder_from_levels_into(levels: &[u8], order_out: &mut Vec<usize>) {
    order_out.clear();
    order_out.extend(0..levels.len());
    reverse_by_levels(levels, order_out);
}

/// L2 within each paragraph of the line: a line that holds a `B` is two
/// paragraphs, each reordered on its own and kept in logical sequence.
fn reorder_paragraphs_into(classes: &[BidiClass], levels: &[u8], order_out: &mut Vec<usize>) {
    order_out.clear();
    order_out.extend(0..levels.len());
    let mut start = 0;
    while start < levels.len() {
        let end = classes
            .get(start..)
            .and_then(|rest| rest.iter().position(|&c| c == B))
            .map_or(levels.len(), |off| start + off + 1)
            .min(levels.len());
        if let (Some(lv), Some(ord)) = (levels.get(start..end), order_out.get_mut(start..end)) {
            reverse_by_levels(lv, ord);
        }
        start = end;
    }
}

/// Rule L2 over one line whose `order` starts as that line's logical indices.
fn reverse_by_levels(levels: &[u8], order: &mut [usize]) {
    let n = levels.len();
    if n == 0 {
        return;
    }
    let max_level = levels.iter().copied().max().unwrap_or(0);
    let lowest_odd = levels
        .iter()
        .copied()
        .filter(|l| l % 2 == 1)
        .min()
        .unwrap_or(u8::MAX);
    if lowest_odd > max_level {
        return;
    }
    let mut level = max_level;
    loop {
        let mut i = 0;
        while i < n {
            if levels.get(i).is_some_and(|&l| l >= level) {
                let start = i;
                while i < n && levels.get(i).is_some_and(|&l| l >= level) {
                    i += 1;
                }
                if let Some(run) = order.get_mut(start..i) {
                    run.reverse();
                }
            } else {
                i += 1;
            }
        }
        if level <= lowest_odd {
            break;
        }
        level = level.saturating_sub(1);
    }
}

// ---------------------------------------------------------------------------
// The algorithm
// ---------------------------------------------------------------------------

/// The deepest explicit embedding level (BD2).
const MAX_DEPTH: u8 = 125;
/// The bracket-pair stack bound (BD16): past it, pairing stops for the sequence.
const MAX_BRACKET_STACK: usize = 63;

/// Working memory for one resolution (reused across calls through [`Scratch`]).
#[derive(Debug, Default)]
struct Resolve {
    types: Vec<BidiClass>,
    matching_pdi: Vec<usize>,
    stack: Vec<StatusEntry>,
    runs: Vec<(usize, usize)>,
    run_of_char: Vec<usize>,
    seq: Vec<usize>,
    bracket_stack: Vec<(u32, usize)>,
    pairs: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, Copy)]
struct StatusEntry {
    level: u8,
    override_to: Option<BidiClass>,
    isolate: bool,
}

/// Resolve `classes` (and their brackets) into `levels` — P1 splits at `B`,
/// then each paragraph runs X1–L1.
fn resolve_into(
    classes: &[BidiClass],
    brackets: &[Bracket],
    base: BaseDirection,
    r: &mut Resolve,
    levels: &mut Vec<u8>,
) {
    levels.clear();
    levels.resize(classes.len(), 0);
    let mut start = 0;
    while start < classes.len() {
        let end = classes
            .get(start..)
            .and_then(|rest| rest.iter().position(|&c| c == B))
            .map_or(classes.len(), |off| start + off + 1)
            .min(classes.len());
        if let (Some(cls), Some(lv)) = (classes.get(start..end), levels.get_mut(start..end)) {
            let br = brackets.get(start..end).unwrap_or(&[]);
            resolve_paragraph(cls, br, base, r, lv);
        }
        start = end;
    }
}

/// BD9: `matching[i]` is the index of the PDI matching the isolate initiator at
/// `i` (or `n` when it has none), and for a matched PDI the index of its
/// initiator; `usize::MAX` elsewhere.
fn match_isolates(classes: &[BidiClass], matching: &mut Vec<usize>) {
    let n = classes.len();
    matching.clear();
    matching.resize(n, usize::MAX);
    let mut open: Vec<usize> = Vec::new();
    for (i, &c) in classes.iter().enumerate() {
        match c {
            LRI | RLI | FSI => {
                if let Some(m) = matching.get_mut(i) {
                    *m = n;
                }
                open.push(i);
            }
            PDI => {
                if let Some(init) = open.pop() {
                    if let Some(m) = matching.get_mut(init) {
                        *m = i;
                    }
                    if let Some(m) = matching.get_mut(i) {
                        *m = init;
                    }
                }
            }
            _ => {}
        }
    }
}

/// P2/P3 over `classes[..]`, skipping isolated content: 1 when the first strong
/// character is R/AL, else 0.
fn first_strong_level(
    classes: &[BidiClass],
    matching: &[usize],
    from: usize,
    to: usize,
) -> Option<u8> {
    let mut i = from;
    while i < to {
        match classes.get(i).copied() {
            Some(L) => return Some(0),
            Some(R | AL) => return Some(1),
            Some(LRI | RLI | FSI) => {
                // Skip to the matching PDI (or the end when unmatched).
                let m = matching.get(i).copied().unwrap_or(to);
                if m >= to {
                    return None;
                }
                i = m;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn paragraph_level_of(classes: &[BidiClass], matching: &[usize], base: BaseDirection) -> u8 {
    match base {
        BaseDirection::Ltr => 0,
        BaseDirection::Rtl => 1,
        BaseDirection::Auto => first_strong_level(classes, matching, 0, classes.len()).unwrap_or(0),
    }
}

fn least_greater_odd(level: u8) -> u8 {
    if level.is_multiple_of(2) {
        level.saturating_add(1)
    } else {
        level.saturating_add(2)
    }
}

fn least_greater_even(level: u8) -> u8 {
    if level.is_multiple_of(2) {
        level.saturating_add(2)
    } else {
        level.saturating_add(1)
    }
}

fn direction_of(level: u8) -> BidiClass {
    if level.is_multiple_of(2) { L } else { R }
}

/// X1–L1 over one paragraph.
fn resolve_paragraph(
    classes: &[BidiClass],
    brackets: &[Bracket],
    base: BaseDirection,
    r: &mut Resolve,
    levels: &mut [u8],
) {
    let n = classes.len();
    let mut matching = std::mem::take(&mut r.matching_pdi);
    match_isolates(classes, &mut matching);
    let para = paragraph_level_of(classes, &matching, base);

    // X1–X8: explicit levels and overrides.
    r.types.clear();
    r.types.extend_from_slice(classes);
    r.stack.clear();
    r.stack.push(StatusEntry {
        level: para,
        override_to: None,
        isolate: false,
    });
    let mut overflow_isolates = 0usize;
    let mut overflow_embeddings = 0usize;
    let mut valid_isolates = 0usize;
    for i in 0..n {
        let class = classes.get(i).copied().unwrap_or(ON);
        let top = r.stack.last().copied().unwrap_or(StatusEntry {
            level: para,
            override_to: None,
            isolate: false,
        });
        match class {
            RLE | LRE | RLO | LRO => {
                let new_level = if matches!(class, RLE | RLO) {
                    least_greater_odd(top.level)
                } else {
                    least_greater_even(top.level)
                };
                if new_level <= MAX_DEPTH && overflow_isolates == 0 && overflow_embeddings == 0 {
                    r.stack.push(StatusEntry {
                        level: new_level,
                        override_to: match class {
                            RLO => Some(R),
                            LRO => Some(L),
                            _ => None,
                        },
                        isolate: false,
                    });
                } else if overflow_isolates == 0 {
                    overflow_embeddings += 1;
                }
                set(levels, i, top.level);
            }
            RLI | LRI | FSI => {
                set(levels, i, top.level);
                if let Some(o) = top.override_to {
                    set(&mut r.types, i, o);
                }
                let rtl = match class {
                    RLI => true,
                    LRI => false,
                    _ => {
                        let end = matching.get(i).copied().unwrap_or(n).min(n);
                        first_strong_level(classes, &matching, i + 1, end) == Some(1)
                    }
                };
                let new_level = if rtl {
                    least_greater_odd(top.level)
                } else {
                    least_greater_even(top.level)
                };
                if new_level <= MAX_DEPTH && overflow_isolates == 0 && overflow_embeddings == 0 {
                    valid_isolates += 1;
                    r.stack.push(StatusEntry {
                        level: new_level,
                        override_to: None,
                        isolate: true,
                    });
                } else {
                    overflow_isolates += 1;
                }
            }
            PDI => {
                if overflow_isolates > 0 {
                    overflow_isolates -= 1;
                } else if valid_isolates > 0 {
                    overflow_embeddings = 0;
                    while r.stack.last().is_some_and(|e| !e.isolate) {
                        r.stack.pop();
                    }
                    r.stack.pop();
                    valid_isolates -= 1;
                }
                let top = r.stack.last().copied().unwrap_or(top);
                set(levels, i, top.level);
                if let Some(o) = top.override_to {
                    set(&mut r.types, i, o);
                }
            }
            PDF => {
                if overflow_isolates > 0 {
                } else if overflow_embeddings > 0 {
                    overflow_embeddings -= 1;
                } else if !top.isolate && r.stack.len() >= 2 {
                    r.stack.pop();
                }
                set(levels, i, top.level);
            }
            B => set(levels, i, para),
            BN => set(levels, i, top.level),
            _ => {
                set(levels, i, top.level);
                if let Some(o) = top.override_to {
                    set(&mut r.types, i, o);
                }
            }
        }
    }

    // X9: removed characters are skipped from here on (`removed_by_x9`).
    // X10: level runs over the kept characters, then isolating run sequences.
    r.runs.clear();
    r.run_of_char.clear();
    r.run_of_char.resize(n, usize::MAX);
    let mut current: Option<(usize, usize, u8)> = None;
    for i in 0..n {
        if classes.get(i).copied().is_none_or(removed_by_x9) {
            continue;
        }
        let level = levels.get(i).copied().unwrap_or(para);
        match current {
            Some((start, _, l)) if l == level => current = Some((start, i, l)),
            Some((start, last, _)) => {
                r.runs.push((start, last));
                current = Some((i, i, level));
            }
            None => current = Some((i, i, level)),
        }
    }
    if let Some((start, last, _)) = current {
        r.runs.push((start, last));
    }
    for (k, &(start, last)) in r.runs.iter().enumerate() {
        for i in start..=last {
            if let Some(slot) = r.run_of_char.get_mut(i) {
                *slot = k;
            }
        }
    }

    let runs = std::mem::take(&mut r.runs);
    let run_of_char = std::mem::take(&mut r.run_of_char);
    let mut seq = std::mem::take(&mut r.seq);
    for &(start, _) in &runs {
        // A run that begins with a PDI matching an initiator continues that
        // initiator's sequence; it never starts one.
        if classes.get(start) == Some(&PDI) && matching.get(start).is_some_and(|&m| m < n) {
            continue;
        }
        seq.clear();
        let mut run = run_of_char.get(start).copied().unwrap_or(usize::MAX);
        while let Some(&(s, e)) = runs.get(run) {
            for i in s..=e {
                if classes.get(i).copied().is_some_and(|c| !removed_by_x9(c)) {
                    seq.push(i);
                }
            }
            let last = e;
            let continues = matches!(classes.get(last), Some(LRI | RLI | FSI))
                && matching.get(last).is_some_and(|&m| m < n);
            if !continues {
                break;
            }
            let pdi = matching.get(last).copied().unwrap_or(n);
            run = run_of_char.get(pdi).copied().unwrap_or(usize::MAX);
        }
        resolve_sequence(&seq, classes, brackets, &matching, levels, para, r);
    }
    r.runs = runs;
    r.run_of_char = run_of_char;
    r.seq = seq;

    // I1/I2 on the kept characters; removed ones take the level before them.
    let mut previous = para;
    for i in 0..n {
        let class = classes.get(i).copied().unwrap_or(ON);
        if removed_by_x9(class) {
            set(levels, i, previous);
            continue;
        }
        let level = levels.get(i).copied().unwrap_or(para);
        let t = r.types.get(i).copied().unwrap_or(ON);
        let resolved = if level.is_multiple_of(2) {
            match t {
                R => level.saturating_add(1),
                AN | EN => level.saturating_add(2),
                _ => level,
            }
        } else {
            match t {
                L | EN | AN => level.saturating_add(1),
                _ => level,
            }
        };
        set(levels, i, resolved);
        previous = resolved;
    }

    // L1: separators, and whitespace/isolate controls (with removed characters
    // among them) before a separator or at the end of the line, go to `para`.
    let mut trailing = true;
    for i in (0..n).rev() {
        let class = classes.get(i).copied().unwrap_or(ON);
        match class {
            S | B => {
                set(levels, i, para);
                trailing = true;
            }
            WS | LRI | RLI | FSI | PDI => {
                if trailing {
                    set(levels, i, para);
                }
            }
            c if removed_by_x9(c) => {
                if trailing {
                    set(levels, i, para);
                }
            }
            _ => trailing = false,
        }
    }
    r.matching_pdi = matching;
}

fn set<T: Copy>(slice: &mut [T], i: usize, value: T) {
    if let Some(slot) = slice.get_mut(i) {
        *slot = value;
    }
}

/// W1–W7, N0, N1/N2 over one isolating run sequence (`seq` holds its kept
/// characters' indices in order).
fn resolve_sequence(
    seq: &[usize],
    classes: &[BidiClass],
    brackets: &[Bracket],
    matching: &[usize],
    levels: &[u8],
    para: u8,
    r: &mut Resolve,
) {
    let (Some(&first), Some(&last)) = (seq.first(), seq.last()) else {
        return;
    };
    let n = classes.len();
    let level = levels.get(first).copied().unwrap_or(para);
    // sos: the higher of this level and the level of the kept character before
    // the sequence (or the paragraph level).
    let before = (0..first)
        .rev()
        .find(|&i| classes.get(i).copied().is_some_and(|c| !removed_by_x9(c)))
        .and_then(|i| levels.get(i).copied())
        .unwrap_or(para);
    let sos = direction_of(level.max(before));
    // eos: the same after the sequence — unless it ends with an (unmatched)
    // isolate initiator, which takes the paragraph level.
    let after = if matches!(classes.get(last), Some(LRI | RLI | FSI)) {
        para
    } else {
        (last + 1..n)
            .find(|&i| classes.get(i).copied().is_some_and(|c| !removed_by_x9(c)))
            .and_then(|i| levels.get(i).copied())
            .unwrap_or(para)
    };
    let eos = direction_of(level.max(after));
    let _ = matching;

    let t = |r: &Resolve, k: usize| -> BidiClass {
        seq.get(k)
            .and_then(|&i| r.types.get(i).copied())
            .unwrap_or(ON)
    };
    let put = |r: &mut Resolve, k: usize, c: BidiClass| {
        if let Some(&i) = seq.get(k) {
            set(&mut r.types, i, c);
        }
    };
    let m = seq.len();

    // W1: NSM takes the previous type (ON after an isolate control; sos first).
    for k in 0..m {
        if t(r, k) == NSM {
            let prev = if k == 0 {
                sos
            } else {
                match t(r, k - 1) {
                    LRI | RLI | FSI | PDI => ON,
                    other => other,
                }
            };
            put(r, k, prev);
        }
    }
    // W2: EN after AL (looking back to the last strong type) becomes AN.
    let mut last_strong = sos;
    for k in 0..m {
        match t(r, k) {
            c @ (L | R | AL) => last_strong = c,
            EN if last_strong == AL => put(r, k, AN),
            _ => {}
        }
    }
    // W3: AL → R.
    for k in 0..m {
        if t(r, k) == AL {
            put(r, k, R);
        }
    }
    // W4: a single ES between ENs, or a single CS between two numbers of one type.
    for k in 1..m.saturating_sub(1) {
        let (prev, cur, next) = (t(r, k - 1), t(r, k), t(r, k + 1));
        match (prev, cur, next) {
            (EN, ES | CS, EN) => put(r, k, EN),
            (AN, CS, AN) => put(r, k, AN),
            _ => {}
        }
    }
    // W5: a run of ETs adjacent to EN becomes EN.
    let mut k = 0;
    while k < m {
        if t(r, k) == ET {
            let start = k;
            while k < m && t(r, k) == ET {
                k += 1;
            }
            let before_en = start > 0 && t(r, start - 1) == EN;
            let after_en = k < m && t(r, k) == EN;
            if before_en || after_en {
                for j in start..k {
                    put(r, j, EN);
                }
            }
        } else {
            k += 1;
        }
    }
    // W6: remaining separators and terminators → ON.
    for k in 0..m {
        if matches!(t(r, k), ES | ET | CS) {
            put(r, k, ON);
        }
    }
    // W7: EN after L (looking back to the last strong type, sos first) → L.
    let mut last_strong = sos;
    for k in 0..m {
        match t(r, k) {
            c @ (L | R) => last_strong = c,
            EN if last_strong == L => put(r, k, L),
            _ => {}
        }
    }

    // N0: paired brackets (BD16) among ON characters.
    let embedding = direction_of(level);
    r.bracket_stack.clear();
    r.pairs.clear();
    for k in 0..m {
        let Some(&i) = seq.get(k) else { continue };
        if t(r, k) != ON {
            continue;
        }
        match brackets.get(i).copied().unwrap_or_default() {
            Bracket::Open(key) => {
                if r.bracket_stack.len() >= MAX_BRACKET_STACK {
                    break;
                }
                r.bracket_stack.push((key, k));
            }
            Bracket::Close(key) => {
                if let Some(depth) = r.bracket_stack.iter().rposition(|&(open, _)| open == key) {
                    if let Some(&(_, open_k)) = r.bracket_stack.get(depth) {
                        r.pairs.push((open_k, k));
                    }
                    r.bracket_stack.truncate(depth);
                }
            }
            Bracket::None => {}
        }
    }
    r.pairs.sort_unstable();
    let strong = |c: BidiClass| match c {
        L => Some(L),
        R | AN | EN => Some(R),
        _ => None,
    };
    let pairs = std::mem::take(&mut r.pairs);
    for &(open_k, close_k) in &pairs {
        let mut found_embedding = false;
        let mut found_opposite = false;
        for k in open_k + 1..close_k {
            match strong(t(r, k)) {
                Some(d) if d == embedding => found_embedding = true,
                Some(_) => found_opposite = true,
                None => {}
            }
        }
        let resolved = if found_embedding {
            Some(embedding)
        } else if found_opposite {
            let context = (0..open_k)
                .rev()
                .find_map(|k| strong(t(r, k)))
                .unwrap_or(sos);
            Some(if context == embedding {
                embedding
            } else {
                context
            })
        } else {
            None
        };
        if let Some(d) = resolved {
            for bracket_k in [open_k, close_k] {
                put(r, bracket_k, d);
                // NSMs that originally followed the bracket take its new type.
                let mut j = bracket_k + 1;
                while j < m {
                    let Some(&idx) = seq.get(j) else { break };
                    if classes.get(idx) != Some(&NSM) {
                        break;
                    }
                    put(r, j, d);
                    j += 1;
                }
            }
        }
    }
    r.pairs = pairs;

    // N1/N2: runs of neutrals and isolate controls take the direction on both
    // sides when it agrees (numbers count as R), else the embedding direction.
    let is_ni = |c: BidiClass| matches!(c, B | S | WS | ON | LRI | RLI | FSI | PDI);
    let mut k = 0;
    while k < m {
        if is_ni(t(r, k)) {
            let start = k;
            while k < m && is_ni(t(r, k)) {
                k += 1;
            }
            let before = if start == 0 {
                sos
            } else {
                strong(t(r, start - 1)).unwrap_or(embedding)
            };
            let after = if k >= m {
                eos
            } else {
                strong(t(r, k)).unwrap_or(embedding)
            };
            let d = if before == after { before } else { embedding };
            for j in start..k {
                put(r, j, d);
            }
        } else {
            k += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Hebrew aleph and Arabic alef as named test scalars (others use literals).
    const ALEF: char = '\u{05D0}'; // Hebrew R
    const AR_ALEF: char = '\u{0627}'; // Arabic AL

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn class_table_spot_checks() {
        assert_eq!(bidi_class('a'), L);
        assert_eq!(bidi_class('Z'), L);
        assert_eq!(bidi_class('5'), EN);
        assert_eq!(bidi_class('+'), ES);
        assert_eq!(bidi_class('$'), ET);
        assert_eq!(bidi_class(','), CS);
        assert_eq!(bidi_class(' '), WS);
        assert_eq!(bidi_class('!'), ON);
        assert_eq!(bidi_class('('), ON);
        assert_eq!(bidi_class(ALEF), R);
        assert_eq!(bidi_class(AR_ALEF), AL);
        assert_eq!(bidi_class('\u{0660}'), AN); // Arabic-Indic zero
        assert_eq!(bidi_class('\u{05B0}'), NSM); // Hebrew point sheva
        assert_eq!(bidi_class('\n'), B);
        assert_eq!(bidi_class('\t'), S);
    }

    #[test]
    fn pure_ltr_is_identity() {
        assert_eq!(reorder_str("abc", BaseDirection::Auto), vec![0, 1, 2]);
        assert_eq!(
            reorder_str("hello world", BaseDirection::Auto),
            (0..11).collect::<Vec<_>>()
        );
        assert!(!has_bidi(&chars("hello world 123")));
    }

    #[test]
    fn empty_line_is_empty() {
        assert_eq!(reorder_str("", BaseDirection::Auto), Vec::<usize>::new());
        assert_eq!(resolve_levels(&[], BaseDirection::Auto), Vec::<u8>::new());
    }

    #[test]
    fn pure_rtl_reverses() {
        // Auto detects RTL from the first strong char; the line displays reversed.
        let t = chars("\u{05D0}\u{05D1}\u{05D2}"); // ALEF BET GIMEL
        assert!(has_bidi(&t));
        assert_eq!(paragraph_level(&t, BaseDirection::Auto), 1);
        assert_eq!(
            reorder_visual_to_logical(&t, BaseDirection::Auto),
            vec![2, 1, 0]
        );
    }

    #[test]
    fn arabic_pure_rtl_reverses() {
        let t = chars("\u{0627}\u{0628}"); // AR_ALEF AR_BEH
        assert_eq!(
            reorder_visual_to_logical(&t, BaseDirection::Auto),
            vec![1, 0]
        );
    }

    #[test]
    fn ltr_with_trailing_rtl_run() {
        // "a " + ALEF BET : base LTR (first strong 'a'); the Hebrew run reverses
        // and sits after the Latin prefix.
        let t = chars("a \u{05D0}\u{05D1}");
        assert_eq!(paragraph_level(&t, BaseDirection::Auto), 0);
        // logical: 0='a' 1=' ' 2=ALEF 3=BET
        // visual : a, space, BET, ALEF  → [0,1,3,2]
        assert_eq!(
            reorder_visual_to_logical(&t, BaseDirection::Auto),
            vec![0, 1, 3, 2]
        );
    }

    #[test]
    fn numbers_stay_ltr_inside_rtl() {
        // ALEF '1' '2' with base RTL: the digits keep their L-to-R order but the
        // whole line is RTL, so visually the digits sit to the LEFT of the letter.
        let t = chars("\u{05D0}12");
        // logical 0=ALEF 1='1' 2='2'; levels: ALEF=1, digits get level 2 (EN at
        // odd base → +1 = 2). L2 → visual [1,2,0] = "12" then ALEF.
        assert_eq!(resolve_levels(&t, BaseDirection::Auto), vec![1, 2, 2]);
        assert_eq!(
            reorder_visual_to_logical(&t, BaseDirection::Auto),
            vec![1, 2, 0]
        );
    }

    #[test]
    fn forced_rtl_base_on_latin() {
        // Force RTL base on a Latin word: each Latin char gets level 1+1=2 (L at
        // odd base), reversed once at level 2 then once at level 1 → net identity
        // order but right-aligned conceptually. The permutation here is identity
        // because the double reversal cancels for a single uniform run.
        let t = chars("ab");
        assert_eq!(resolve_levels(&t, BaseDirection::Rtl), vec![2, 2]);
        assert_eq!(reorder_from_levels(&[2, 2]), vec![0, 1]);
    }

    #[test]
    fn neutral_between_same_direction_takes_that_direction() {
        // ALEF '-' BET (RTL base): the '-' (ES→ON, neutral) is between two R, so
        // N1 makes it R; whole run reverses together.
        let t = chars("\u{05D0}-\u{05D1}");
        assert_eq!(resolve_levels(&t, BaseDirection::Auto), vec![1, 1, 1]);
        assert_eq!(
            reorder_visual_to_logical(&t, BaseDirection::Auto),
            vec![2, 1, 0]
        );
    }

    #[test]
    fn mixed_english_hebrew_english() {
        // "hi " + ALEF BET + " bye" — base LTR. The middle Hebrew run reverses;
        // the Latin segments stay in order around it.
        let t = chars("hi \u{05D0}\u{05D1} bye");
        // indices: 0 h,1 i,2 space,3 ALEF,4 BET,5 space,6 b,7 y,8 e
        // Hebrew run [3,4] reverses → [4,3]; spaces are neutral between L and R /
        // R and L → embedding (L). Visual: 0,1,2,4,3,5,6,7,8
        assert_eq!(
            reorder_visual_to_logical(&t, BaseDirection::Auto),
            vec![0, 1, 2, 4, 3, 5, 6, 7, 8]
        );
    }

    #[test]
    fn l1_resets_trailing_whitespace_in_rtl() {
        // RTL base line ending in spaces: the trailing spaces reset to the base
        // level (1) so they stay at the visual start, not interleaved oddly.
        let t = chars("\u{05D0}  ");
        let levels = resolve_levels(&t, BaseDirection::Rtl);
        // ALEF level 1; two trailing WS reset to base 1 (not bumped).
        assert_eq!(levels, vec![1, 1, 1]);
    }

    #[test]
    fn reorder_from_levels_matches_unicode_l2_example() {
        // Classic UAX #9 L2 illustration: levels [0,0,0,1,1,2] → reverse level-2
        // run, then level-1+ runs. Verify the permutation is a valid involution of
        // the reversals.
        let levels = [0u8, 0, 0, 1, 1, 2];
        let order = reorder_from_levels(&levels);
        // level 2: reverse [5,6) → no-op (single). level 1: reverse positions
        // 3..6 → [5,4,3]. Final: [0,1,2,5,4,3].
        assert_eq!(order, vec![0, 1, 2, 5, 4, 3]);
    }

    const CJK: char = '\u{4E2D}'; // 中 — a wide (2-cell) glyph, bidi class L

    #[test]
    fn reorder_cells_ascii_is_identity() {
        let cc = chars("hello");
        let wide = vec![false; cc.len()];
        assert_eq!(
            reorder_cells(&cc, &wide, BaseDirection::Auto),
            (0..cc.len()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn reorder_cells_wide_glyph_stays_paired_ltr() {
        // "中" occupies cells [lead, continuation]; pure-LTR → identity, the pair
        // kept together and in order.
        let cc = vec![CJK, ' '];
        let wide = vec![false, true];
        assert_eq!(reorder_cells(&cc, &wide, BaseDirection::Auto), vec![0, 1]);
    }

    #[test]
    fn reorder_cells_rtl_with_wide_glyph_keeps_pair_unmirrored() {
        // Logical: ALEF (R, 1 cell), 中 (L, 2 cells). Base auto → RTL. The Hebrew
        // letter ends on the right; 中 moves left but its two cells stay in
        // lead-then-continuation order (the glyph is NOT mirrored).
        let cc = vec![ALEF, CJK, ' '];
        let wide = vec![false, false, true];
        // visual: 中-lead(1), 中-cont(2), ALEF(0)
        assert_eq!(
            reorder_cells(&cc, &wide, BaseDirection::Auto),
            vec![1, 2, 0]
        );
    }

    #[test]
    fn reorder_cells_is_always_a_permutation() {
        let cases: &[(Vec<char>, Vec<bool>)] = &[
            (vec![CJK, ' ', ALEF, '1'], vec![false, true, false, false]),
            (
                vec![ALEF, CJK, ' ', '!', 'a'],
                vec![false, false, true, false, false],
            ),
            // "ab中 cd" as CELLS: 中's continuation space is an explicit cell.
            (
                vec!['a', 'b', CJK, ' ', ' ', 'c', 'd'],
                vec![false, false, false, true, false, false, false],
            ),
        ];
        for (cc, wide) in cases {
            let order = reorder_cells(cc, wide, BaseDirection::Auto);
            let mut seen = order.clone();
            seen.sort_unstable();
            assert_eq!(
                seen,
                (0..cc.len()).collect::<Vec<_>>(),
                "not a permutation: {cc:?}"
            );
        }
    }

    #[test]
    fn reorder_cells_malformed_leading_continuation_is_safe() {
        // A continuation flag with no preceding lead must not panic and must still
        // yield a valid permutation (the stray cell is treated as single-width).
        let cc = vec![' ', 'a'];
        let wide = vec![true, false];
        let order = reorder_cells(&cc, &wide, BaseDirection::Auto);
        let mut seen = order.clone();
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1]);
    }

    #[test]
    fn permutation_is_always_valid() {
        // Whatever the input, the result must be a permutation of 0..n.
        for s in [
            "abc",
            "\u{05D0}\u{05D1}1 2",
            "a\u{0628}c\u{0627}!",
            "12.34 \u{05D0}",
        ] {
            let t = chars(s);
            let order = reorder_visual_to_logical(&t, BaseDirection::Auto);
            let mut seen = order.clone();
            seen.sort_unstable();
            assert_eq!(
                seen,
                (0..t.len()).collect::<Vec<_>>(),
                "not a permutation for {s:?}"
            );
        }
    }

    #[test]
    fn arithmetic_run_is_ltr_identity() {
        // "1+2=3" is all EN/ES/ON — no strong RTL, base LTR. Even-level EN runs
        // never reorder (no odd level), so it stays in logical order.
        let t = chars("1+2=3");
        assert!(!has_bidi(&t));
        assert_eq!(
            reorder_visual_to_logical(&t, BaseDirection::Auto),
            vec![0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn neutral_only_line_is_identity() {
        // Pure punctuation/whitespace: no strong char → base LTR, all neutrals
        // resolve to the embedding direction (L), identity order.
        let t = chars("  .!? ");
        assert_eq!(paragraph_level(&t, BaseDirection::Auto), 0);
        assert_eq!(
            reorder_visual_to_logical(&t, BaseDirection::Auto),
            (0..t.len()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn arabic_number_in_arabic_text_stays_ltr() {
        // Arabic letter + two Arabic-Indic digits (AN). Base RTL (first strong AL→R
        // after W3). AN at odd base → +1 = level 2; the letter stays level 1. The
        // digits keep logical order and sit to the LEFT of the letter visually.
        let t = chars("\u{0627}\u{0660}\u{0661}"); // ALEF, Arabic-Indic 0, 1
        assert_eq!(resolve_levels(&t, BaseDirection::Auto), vec![1, 2, 2]);
        assert_eq!(
            reorder_visual_to_logical(&t, BaseDirection::Auto),
            vec![1, 2, 0]
        );
    }

    #[test]
    fn class_taking_entry_points_match_char_taking() {
        // The precomputed-class companions must produce byte-identical output to
        // the char-taking originals (the dedup the bridge relies on).
        for s in [
            "",
            "abc",
            "\u{05D0}\u{05D1}\u{05D2}",
            "hi \u{05D0}\u{05D1} bye",
            "\u{05D0}12",
            "12.34 \u{05D0}",
            "a\u{0628}c\u{0627}!",
        ] {
            let t = chars(s);
            let classes: Vec<BidiClass> = t.iter().map(|&c| bidi_class(c)).collect();
            let brackets: Vec<Bracket> = t.iter().map(|&c| bracket_of(c)).collect();
            assert_eq!(has_bidi(&t), has_bidi_classes(&classes), "has_bidi {s:?}");
            for base in [BaseDirection::Auto, BaseDirection::Ltr, BaseDirection::Rtl] {
                assert_eq!(
                    resolve_levels(&t, base),
                    resolve_levels_from_classes(&classes, &brackets, base),
                    "resolve_levels {s:?} {base:?}"
                );
            }
        }
    }

    #[test]
    fn reorder_cells_with_classes_matches_reorder_cells() {
        let cases: &[(Vec<char>, Vec<bool>)] = &[
            (chars("hello"), vec![false; 5]),
            (vec![CJK, ' '], vec![false, true]),
            (vec![ALEF, CJK, ' '], vec![false, false, true]),
            (vec![CJK, ' ', ALEF, '1'], vec![false, true, false, false]),
            (vec![' ', 'a'], vec![true, false]),
        ];
        for (cc, wide) in cases {
            let classes: Vec<BidiClass> = cc.iter().map(|&c| bidi_class(c)).collect();
            let brackets: Vec<Bracket> = cc.iter().map(|&c| bracket_of(c)).collect();
            for base in [BaseDirection::Auto, BaseDirection::Ltr, BaseDirection::Rtl] {
                assert_eq!(
                    reorder_cells(cc, wide, base),
                    reorder_cells_with_classes(&classes, &brackets, wide, base),
                    "{cc:?} {base:?}"
                );
            }
        }
    }

    #[test]
    fn into_variants_match_vec_returning() {
        // The buffer-in/buffer-out companions produce byte-identical output to the
        // Vec-returning wrappers (the render bridge relies on this equality), and
        // one Scratch reused across every case exercises the clear+refill path.
        let cell_cases: &[(Vec<char>, Vec<bool>)] = &[
            (chars("hello"), vec![false; 5]),
            (chars("\u{05D0}\u{05D1}\u{05D2}"), vec![false; 3]),
            (chars("hi \u{05D0}\u{05D1} bye"), vec![false; 9]),
            (chars("\u{05D0}12"), vec![false; 3]),
            (chars("a \u{2067}(\u{05D0}) b\u{2069} c"), vec![false; 10]),
            (vec![ALEF, CJK, ' '], vec![false, false, true]),
            (vec![CJK, ' ', ALEF, '1'], vec![false, true, false, false]),
            (vec![' ', 'a'], vec![true, false]),
            (chars(""), vec![]),
        ];
        let mut scratch = Scratch::default();
        let mut cell_order = Vec::new();
        for (cc, wide) in cell_cases {
            let classes: Vec<BidiClass> = cc.iter().map(|&c| bidi_class(c)).collect();
            let brackets: Vec<Bracket> = cc.iter().map(|&c| bracket_of(c)).collect();
            for base in [BaseDirection::Auto, BaseDirection::Ltr, BaseDirection::Rtl] {
                reorder_cells_with_classes_into(
                    &classes,
                    &brackets,
                    wide,
                    base,
                    &mut scratch,
                    &mut cell_order,
                );
                assert_eq!(
                    cell_order,
                    reorder_cells_with_classes(&classes, &brackets, wide, base),
                    "reorder_cells_with_classes_into {cc:?} {base:?}"
                );
                let levels = resolve_levels_from_classes(&classes, &brackets, base);
                let mut order_out = Vec::new();
                reorder_from_levels_into(&levels, &mut order_out);
                assert_eq!(order_out, reorder_from_levels(&levels), "{cc:?} {base:?}");
            }
        }
    }

    #[test]
    fn levels_never_drop_below_base() {
        // Spot the invariant the property test generalizes: I1/I2 only raise levels
        // and L1 resets to the base, so the minimum level equals the base level.
        for (s, base) in [
            ("a\u{05D0}1 b", BaseDirection::Auto),
            ("\u{05D0}a1", BaseDirection::Rtl),
            ("mixed \u{0628} text", BaseDirection::Ltr),
        ] {
            let t = chars(s);
            let para = paragraph_level(&t, base);
            for (i, &l) in resolve_levels(&t, base).iter().enumerate() {
                assert!(l >= para, "level {l} at {i} below base {para} for {s:?}");
            }
        }
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// A representative bidi alphabet exercising every resolved class: LTR letters,
    /// European numbers/separators/terminators, Hebrew (R), Arabic (AL) +
    /// Arabic-Indic digits (AN), common separators, neutrals, whitespace, and the
    /// directional marks.
    fn bidi_char() -> impl Strategy<Value = char> {
        prop::sample::select(vec![
            'a', 'Z', '5', '0', '+', '-', '$', '%', ',', '.', ':', '/', ' ', '\t', '\n', '!', '(',
            ')', '\u{05D0}', '\u{05D1}', '\u{05EA}', '\u{0627}', '\u{0628}', '\u{0660}',
            '\u{0669}', '\u{200F}', '\u{200E}', '[', ']', '\u{0301}', '\u{202A}', '\u{202B}',
            '\u{202C}', '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ])
    }

    fn bidi_line() -> impl Strategy<Value = Vec<char>> {
        prop::collection::vec(bidi_char(), 0..48)
    }

    proptest! {
        /// The visual order is ALWAYS a permutation of 0..n, for any input and base
        /// — the load-bearing safety property (no cell dropped or duplicated).
        #[test]
        fn reorder_is_always_a_permutation(line in bidi_line()) {
            for base in [BaseDirection::Auto, BaseDirection::Ltr, BaseDirection::Rtl] {
                let order = reorder_visual_to_logical(&line, base);
                prop_assert_eq!(order.len(), line.len());
                let mut sorted = order.clone();
                sorted.sort_unstable();
                prop_assert_eq!(sorted, (0..line.len()).collect::<Vec<_>>());
            }
        }

        /// Levels match the input length and never fall below their PARAGRAPH's
        /// base level (explicit embeddings and I1/I2 only raise; L1 resets to
        /// base). A `B` ends a paragraph, and the next one has its own base.
        #[test]
        fn levels_well_formed(line in bidi_line()) {
            for base in [BaseDirection::Auto, BaseDirection::Ltr, BaseDirection::Rtl] {
                let levels = resolve_levels(&line, base);
                prop_assert_eq!(levels.len(), line.len());
                let mut start = 0;
                while start < line.len() {
                    let end = line[start..]
                        .iter()
                        .position(|&c| bidi_class(c) == B)
                        .map_or(line.len(), |off| start + off + 1);
                    let para = paragraph_level(&line[start..end], base);
                    for &l in &levels[start..end] {
                        prop_assert!(l >= para);
                    }
                    start = end;
                }
            }
        }

        /// A line with no strong-RTL and no Arabic number is pure LTR → identity.
        #[test]
        fn ltr_only_is_identity(s in "[a-zA-Z0-9 +\\-.,:()!]{0,48}") {
            let line: Vec<char> = s.chars().collect();
            prop_assert!(!has_bidi(&line));
            let order = reorder_visual_to_logical(&line, BaseDirection::Auto);
            prop_assert_eq!(order, (0..line.len()).collect::<Vec<_>>());
        }

        /// Resolution is deterministic (same input → same levels).
        #[test]
        fn resolution_is_deterministic(line in bidi_line()) {
            prop_assert_eq!(
                resolve_levels(&line, BaseDirection::Auto),
                resolve_levels(&line, BaseDirection::Auto)
            );
        }

        /// The class-taking entry points are byte-identical to the char-taking
        /// originals for any input and base (the dedup the bridge depends on).
        #[test]
        fn class_taking_matches_char_taking(line in bidi_line()) {
            let classes: Vec<BidiClass> = line.iter().map(|&c| bidi_class(c)).collect();
            let brackets: Vec<Bracket> = line.iter().map(|&c| bracket_of(c)).collect();
            let wide = vec![false; line.len()];
            prop_assert_eq!(has_bidi(&line), has_bidi_classes(&classes));
            for base in [BaseDirection::Auto, BaseDirection::Ltr, BaseDirection::Rtl] {
                prop_assert_eq!(
                    resolve_levels(&line, base),
                    resolve_levels_from_classes(&classes, &brackets, base)
                );
                prop_assert_eq!(
                    reorder_cells(&line, &wide, base),
                    reorder_cells_with_classes(&classes, &brackets, &wide, base)
                );
            }
        }

        /// The `_into` companion is byte-identical to the Vec-returning wrapper
        /// for any input and base, with one Scratch reused across bases.
        #[test]
        fn into_variants_match_vec_returning_prop(line in bidi_line()) {
            let classes: Vec<BidiClass> = line.iter().map(|&c| bidi_class(c)).collect();
            let brackets: Vec<Bracket> = line.iter().map(|&c| bracket_of(c)).collect();
            let wide = vec![false; line.len()];
            let mut scratch = Scratch::default();
            let mut cell_order = Vec::new();
            for base in [BaseDirection::Auto, BaseDirection::Ltr, BaseDirection::Rtl] {
                reorder_cells_with_classes_into(
                    &classes, &brackets, &wide, base, &mut scratch, &mut cell_order,
                );
                prop_assert_eq!(
                    &cell_order,
                    &reorder_cells_with_classes(&classes, &brackets, &wide, base)
                );
            }
        }
    }
}
