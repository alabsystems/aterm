// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Unicode Bidirectional Algorithm (UAX #9) line reordering for the terminal.
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
#![cfg_attr(
    trust_verify,
    feature(custom_inner_attributes, register_tool),
    register_tool(trust)
)]

mod mirroring;
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

/// A character's paired-bracket property (UAX #9 BD14/BD15), keyed by the code point of the OPENING bracket of its pair after canonical equivalence (U+2329/U+232A pair with U+3008/U+3009), so an opener and a closer match exactly when their keys are equal.
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

impl BaseDirection {
    /// Return `true` when the direction is explicitly forced (`Ltr` or `Rtl`).
    #[must_use]
    #[inline]
    pub const fn is_explicit(self) -> bool {
        !matches!(self, Self::Auto)
    }

    /// Return the default paragraph embedding level (`1` for `Rtl`, `0` otherwise).
    #[must_use]
    #[inline]
    pub const fn default_level(self) -> u8 {
        match self {
            Self::Rtl => 1,
            Self::Auto | Self::Ltr => 0,
        }
    }
}

impl BidiClass {
    /// Return `true` when this class is a strong directional class (`L`, `R`, or `AL`).
    #[must_use]
    #[inline]
    pub const fn is_strong(self) -> bool {
        matches!(self, Self::L | Self::R | Self::AL)
    }

    /// Return `true` when this class is a strong right-to-left class (`R` or `AL`).
    #[must_use]
    #[inline]
    pub const fn is_rtl_strong(self) -> bool {
        matches!(self, Self::R | Self::AL)
    }

    /// Return `true` when this class is a numeric class (`EN` or `AN`).
    #[must_use]
    #[inline]
    pub const fn is_number(self) -> bool {
        matches!(self, Self::EN | Self::AN)
    }

    /// Return `true` when this class is an isolate initiator (`LRI`, `RLI`, or `FSI`).
    #[must_use]
    #[inline]
    pub const fn is_isolate_initiator(self) -> bool {
        matches!(self, Self::LRI | Self::RLI | Self::FSI)
    }
}

impl Bracket {
    /// Return `true` when this bracket is an opening bracket (`Open(_)`).
    #[must_use]
    #[inline]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Open(_))
    }

    /// Return `true` when this bracket is a closing bracket (`Close(_)`).
    #[must_use]
    #[inline]
    pub const fn is_close(self) -> bool {
        matches!(self, Self::Close(_))
    }

    /// Return the canonical opening bracket codepoint, if any.
    #[must_use]
    #[inline]
    pub const fn canonical_open(self) -> Option<u32> {
        match self {
            Self::None => None,
            Self::Open(cp) | Self::Close(cp) => Some(cp),
        }
    }
}

/// Encode a resolved bidirectional run descriptor (`start`, `end`, `level`, `flags`) into 16 LE bytes.
#[must_use]
#[inline]
pub const fn encode_bidi_run_le(start: u32, end: u32, level: u32, flags: u32) -> [u8; 16] {
    [
        start as u8,
        (start >> 8) as u8,
        (start >> 16) as u8,
        (start >> 24) as u8,
        end as u8,
        (end >> 8) as u8,
        (end >> 16) as u8,
        (end >> 24) as u8,
        level as u8,
        (level >> 8) as u8,
        (level >> 16) as u8,
        (level >> 24) as u8,
        flags as u8,
        (flags >> 8) as u8,
        (flags >> 16) as u8,
        (flags >> 24) as u8,
    ]
}

/// Decode 16 LE bytes into a resolved bidirectional run descriptor (`start`, `end`, `level`, `flags`).
#[must_use]
#[inline]
pub const fn decode_bidi_run_le(bytes: [u8; 16]) -> (u32, u32, u32, u32) {
    let start = (bytes[0] as u32)
        | ((bytes[1] as u32) << 8)
        | ((bytes[2] as u32) << 16)
        | ((bytes[3] as u32) << 24);
    let end = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    let level = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let flags = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (start, end, level, flags)
}

/// O(1) lookup table for ASCII `0x00..=0x7F` (`Bidi_Class`), matching
/// `tables::BIDI_CLASS_RANGES` without a binary search on every ASCII cell.
const ASCII_BIDI_CLASS: [BidiClass; 128] = {
    let mut table = [L; 128];
    let mut u = 0usize;
    while u < 128 {
        table[u] = match u as u8 {
            0x00..=0x08 | 0x0E..=0x1B | 0x7F => BN,
            0x09 | 0x0B | 0x1F => S,
            0x0A | 0x0D | 0x1C..=0x1E => B,
            0x0C | 0x20 => WS,
            0x21..=0x22 | 0x26..=0x2A | 0x3B..=0x40 | 0x5B..=0x60 | 0x7B..=0x7E => ON,
            0x23..=0x25 => ET,
            0x2B | 0x2D => ES,
            0x2C | 0x2E..=0x2F | 0x3A => CS,
            0x30..=0x39 => EN,
            _ => L,
        };
        u += 1;
    }
    table
};

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
    mod bidi_ascii_proofs {
        clean {
            section BidiAsciiFastPath
            theorem AsciiTableIndexInBounds :
                forall (u : Nat),
                    Nat.lt u 128 ->
                    Nat.lt u 128 :=
                fun (u : Nat)
                    (h : Nat.lt u 128) => h

            theorem BelowHebrewSkipSound :
                forall (u : Nat) (is_rtl : Prop) (h_table : Nat.lt u 1424 -> Not is_rtl),
                    Nat.lt u 1424 ->
                    Not is_rtl :=
                fun (u : Nat) (is_rtl : Prop)
                    (h_table : Nat.lt u 1424 -> Not is_rtl)
                    (h_lt : Nat.lt u 1424) =>
                    h_table h_lt
            end BidiAsciiFastPath
        }
    }
}

/// The bidirectional class of `c` (the UCD `Bidi_Class` property).
#[must_use]
#[inline]
pub const fn bidi_class(c: char) -> BidiClass {
    let u = c as u32;
    if u < 0x80 {
        return ASCII_BIDI_CLASS[u as usize];
    }
    bidi_class_table(u)
}

#[inline]
const fn bidi_class_table(u: u32) -> BidiClass {
    let table = tables::BIDI_CLASS_RANGES;
    let mut lo = 0usize;
    let mut hi = table.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let (range_lo, range_hi, class) = table[mid];
        if range_hi < u {
            lo = mid + 1;
        } else if range_lo > u {
            hi = mid;
        } else {
            return class;
        }
    }
    L
}

/// The `Bidi_Mirroring_Glyph` of `c`: the character whose glyph depicts `c` at a right-to-left level (UAX #9 L4), as `)` for `(` and `≥` for `≤`.
#[must_use]
#[inline]
pub const fn mirror_glyph(c: char) -> Option<char> {
    let u = c as u32;
    if u < 0x0F3A {
        return match u {
            0x0028 => Some(')'),
            0x0029 => Some('('),
            0x003C => Some('>'),
            0x003E => Some('<'),
            0x005B => Some(']'),
            0x005D => Some('['),
            0x007B => Some('}'),
            0x007D => Some('{'),
            0x00AB => Some('\u{00BB}'),
            0x00BB => Some('\u{00AB}'),
            _ => None,
        };
    }
    if u > 0xFF63 {
        return None;
    }
    mirror_glyph_table(u)
}

const fn mirror_glyph_table(u: u32) -> Option<char> {
    let pairs = &mirroring::MIRROR_PAIRS;
    let count = pairs.len() / 2;
    let mut lo = 0usize;
    let mut hi = count;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let source = pairs[mid * 2];
        if source == u {
            return char::from_u32(pairs[mid * 2 + 1]);
        }
        if source < u {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    None
}

/// Canonical equivalence for the two bracket pairs that have one (BD16).
#[inline]
const fn canonical_bracket(u: u32) -> u32 {
    match u {
        0x2329 => 0x3008,
        0x232A => 0x3009,
        other => other,
    }
}

/// The paired-bracket property of `c` (see [`Bracket`]).
#[must_use]
#[inline]
pub const fn bracket_of(c: char) -> Bracket {
    let u = c as u32;
    if u < 0x80 {
        return match u as u8 {
            b'(' => Bracket::Open(0x0028),
            b')' => Bracket::Close(0x0028),
            b'[' => Bracket::Open(0x005B),
            b']' => Bracket::Close(0x005B),
            b'{' => Bracket::Open(0x007B),
            b'}' => Bracket::Close(0x007B),
            _ => Bracket::None,
        };
    }
    if u < 0x0F3A || u > 0xFF63 {
        return Bracket::None;
    }
    bracket_of_table(u)
}

const fn bracket_of_table(u: u32) -> Bracket {
    let pairs = tables::BRACKET_PAIRS;
    let mut lo = 0usize;
    let mut hi = pairs.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let (open, _) = pairs[mid];
        if open == u {
            return Bracket::Open(canonical_bracket(u));
        }
        if open < u {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let closers = tables::BRACKET_CLOSERS;
    let mut lo = 0usize;
    let mut hi = closers.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let (close, open) = closers[mid];
        if close == u {
            return Bracket::Close(canonical_bracket(open));
        }
        if close < u {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    Bracket::None
}

/// Whether rule X9 removes a character of this class (the embedding controls and `BN`).
#[must_use]
#[inline]
pub const fn removed_by_x9(class: BidiClass) -> bool {
    matches!(class, RLE | LRE | RLO | LRO | PDF | BN)
}

/// Quick test for whether `text` carries any RTL/Arabic-number/isolate trigger class
/// (`R`, `AL`, `AN`, `RLE`, `RLO`, `RLI`, `FSI`). When `false`, every level resolves even
/// under [`BaseDirection::Auto`] and [`BaseDirection::Ltr`], making visual order the identity
/// ([`BaseDirection::Rtl`] sets paragraph level 1 and can still reorder neutrals around `L`/`EN`).
#[must_use]
#[inline]
pub fn has_bidi(text: &[char]) -> bool {
    text.iter()
        .any(|&c| u32::from(c) >= 0x0590 && is_bidi_class(bidi_class_table(u32::from(c))))
}

/// Class-slice companion to [`has_bidi`] (identity guarantee holds for [`BaseDirection::Auto`]
/// and [`BaseDirection::Ltr`], not [`BaseDirection::Rtl`]).
#[must_use]
#[inline]
pub fn has_bidi_classes(classes: &[BidiClass]) -> bool {
    classes.iter().any(|&c| is_bidi_class(c))
}

#[inline]
const fn is_bidi_class(c: BidiClass) -> bool {
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
#[must_use]
pub fn reorder_visual_to_logical(text: &[char], base: BaseDirection) -> Vec<usize> {
    let levels = resolve_levels(text, base);
    let (classes, _) = classes_and_brackets(text);
    let mut order = Vec::new();
    reorder_paragraphs_into(&classes, &levels, &mut order);
    order
}

/// Compute the visual→logical **cell** permutation for a terminal row from per-CELL classes and
/// brackets (parallel to `is_wide_continuation`), so a caller that already computed them does not
/// recompute.
#[must_use]
pub fn reorder_cells_with_classes(
    cell_classes: &[BidiClass],
    cell_brackets: &[Bracket],
    is_wide_continuation: &[bool],
    base: BaseDirection,
) -> Vec<usize> {
    let mut scratch = Scratch::default();
    let mut out = Vec::new();
    reorder_cells_into::<false>(
        cell_classes,
        cell_brackets,
        is_wide_continuation,
        base,
        &mut scratch,
        &mut out,
    );
    out
}

/// Reusable working memory for the `_into` entry points: a per-row caller that keeps one of these reorders without heap allocation after warmup.
#[derive(Debug, Default)]
pub struct Scratch {
    logical: Vec<BidiClass>,
    logical_brackets: Vec<Bracket>,
    lead_cell: Vec<usize>,
    has_cont: Vec<bool>,
    // Character indices during resolution; cell indices after the public `_into` returns.
    levels: Vec<u8>,
    char_order: Vec<usize>,
    resolve: Resolve,
}

impl Scratch {
    /// Whether CELL `cell` of the row last passed to [`reorder_cells_with_classes_into`] with this scratch resolved to an odd (right-to-left) level: the cells UAX #9 L4 draws mirrored.
    #[must_use]
    #[inline]
    pub fn cell_is_rtl(&self, cell: usize) -> bool {
        self.levels.get(cell).is_some_and(|level| level % 2 == 1)
    }
}

/// Buffer-in/buffer-out companion to [`reorder_cells_with_classes`]: the visual→logical CELL permutation is written into `out` (cleared then refilled), and every working buffer lives in `scratch`.
pub fn reorder_cells_with_classes_into(
    cell_classes: &[BidiClass],
    cell_brackets: &[Bracket],
    is_wide_continuation: &[bool],
    base: BaseDirection,
    scratch: &mut Scratch,
    out: &mut Vec<usize>,
) {
    reorder_cells_into::<true>(
        cell_classes,
        cell_brackets,
        is_wide_continuation,
        base,
        scratch,
        out,
    );
}

/// Only a caller retaining scratch can query cell directions after reordering.
fn reorder_cells_into<const CELL_LEVELS: bool>(
    cell_classes: &[BidiClass],
    cell_brackets: &[Bracket],
    is_wide_continuation: &[bool],
    base: BaseDirection,
    scratch: &mut Scratch,
    out: &mut Vec<usize>,
) {
    let s = scratch;
    if !is_wide_continuation
        .iter()
        .take(cell_classes.len())
        .any(|&cont| cont)
    {
        // With one cell per character, the caller's inputs and final output
        // already have the resolver's index space. No folding buffers are needed.
        resolve_into(
            cell_classes,
            cell_brackets,
            base,
            &mut s.resolve,
            &mut s.levels,
        );
        reorder_paragraphs_into(cell_classes, &s.levels, out);
        return;
    }
    fold_cells_and_resolve(cell_classes, cell_brackets, is_wide_continuation, base, s);
    reorder_paragraphs_into(&s.logical, &s.levels, &mut s.char_order);

    out.clear();
    // Folding consumes one cell per lead and one per continuation, and expansion
    // emits both again. Reserve that known total once, including on cold callers.
    out.reserve(cell_classes.len());
    for &c in &s.char_order {
        if let Some(&lead) = s.lead_cell.get(c) {
            out.push(lead);
            if s.has_cont.get(c).copied().unwrap_or(false) {
                out.push(lead.saturating_add(1));
            }
        }
    }
    if CELL_LEVELS {
        expand_scratch_levels_to_cells(s, cell_classes.len());
    }
}

/// Normalize character levels after L2 so every public scratch query indexes cells.
fn expand_scratch_levels_to_cells(s: &mut Scratch, cells: usize) {
    if s.levels.len() == cells {
        return;
    }
    s.levels.resize(cells, 0);
    // lead[c] >= c: backwards expansion cannot overwrite an unread character level.
    for (c, &lead) in s.lead_cell.iter().enumerate().rev() {
        let level = s.levels.get(c).copied().unwrap_or(0);
        if let Some(slot) = s.levels.get_mut(lead) {
            *slot = level;
        }
        if s.has_cont.get(c).copied().unwrap_or(false)
            && let Some(slot) = s.levels.get_mut(lead.saturating_add(1))
        {
            *slot = level;
        }
    }
}

/// The resolved UAX #9 embedding level of every CELL of a terminal row, parallel to `cell_classes`: the levels [`reorder_cells_with_classes_into`] reorders by, from the same fold, so a wide glyph's continuation cell takes its lead's level.
#[must_use]
pub fn cell_levels(
    cell_classes: &[BidiClass],
    cell_brackets: &[Bracket],
    is_wide_continuation: &[bool],
    base: BaseDirection,
) -> Vec<u8> {
    let mut s = Scratch::default();
    fold_cells_and_resolve(
        cell_classes,
        cell_brackets,
        is_wide_continuation,
        base,
        &mut s,
    );
    let mut out = vec![0; cell_classes.len()];
    for (c, &lead) in s.lead_cell.iter().enumerate() {
        let level = s.levels.get(c).copied().unwrap_or(0);
        if let Some(slot) = out.get_mut(lead) {
            *slot = level;
        }
        if s.has_cont.get(c).copied().unwrap_or(false)
            && let Some(slot) = out.get_mut(lead.saturating_add(1))
        {
            *slot = level;
        }
    }
    out
}

/// Fold wide cell pairs into logical characters and resolve their levels.
/// Reordering uses this for wide rows; `cell_levels` also uses it before its
/// separate forward expansion into an owned cell-level array.
fn fold_cells_and_resolve(
    cell_classes: &[BidiClass],
    cell_brackets: &[Bracket],
    is_wide_continuation: &[bool],
    base: BaseDirection,
    s: &mut Scratch,
) {
    s.logical.clear();
    s.logical_brackets.clear();
    s.lead_cell.clear();
    s.has_cont.clear();
    let n = cell_classes.len();
    let mut i = 0;
    while i < n {
        let next = i.saturating_add(1);
        let cont = next < n && is_wide_continuation.get(next).copied().unwrap_or(false);
        if let Some(&class) = cell_classes.get(i) {
            s.logical.push(class);
            s.logical_brackets
                .push(cell_brackets.get(i).copied().unwrap_or_default());
            s.lead_cell.push(i);
            s.has_cont.push(cont);
        }
        i = i.saturating_add(if cont { 2 } else { 1 });
    }

    resolve_into(
        &s.logical,
        &s.logical_brackets,
        base,
        &mut s.resolve,
        &mut s.levels,
    );
}

/// Resolve the UAX #9 embedding level of every character in `text` (rules P1–L1 over one
/// line; removed X9 characters take the level before them).
#[must_use]
pub fn resolve_levels(text: &[char], base: BaseDirection) -> Vec<u8> {
    let (classes, brackets) = classes_and_brackets(text);
    resolve_levels_from_classes(&classes, &brackets, base)
}

/// [`resolve_levels`] from precomputed classes and brackets (a missing bracket entry reads as [`Bracket::None`]).
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

/// Apply UAX #9 rule L2 to a resolved level array, returning the visual→logical permutation.
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
            let br = brackets.get(start..end.min(brackets.len())).unwrap_or(&[]);
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

#[inline]
const fn least_greater_odd(level: u8) -> u8 {
    if level.is_multiple_of(2) {
        level.saturating_add(1)
    } else {
        level.saturating_add(2)
    }
}

#[inline]
const fn least_greater_even(level: u8) -> u8 {
    if level.is_multiple_of(2) {
        level.saturating_add(2)
    } else {
        level.saturating_add(1)
    }
}

#[inline]
const fn direction_of(level: u8) -> BidiClass {
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
        resolve_sequence(&seq, classes, brackets, levels, para, r);
    }
    r.runs = runs;
    r.run_of_char = run_of_char;
    r.seq = seq;

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
    levels: &[u8],
    para: u8,
    r: &mut Resolve,
) {
    let (Some(&first), Some(&last)) = (seq.first(), seq.last()) else {
        return;
    };
    let n = classes.len();
    let level = levels.get(first).copied().unwrap_or(para);
    let before = (0..first)
        .rev()
        .find(|&i| classes.get(i).copied().is_some_and(|c| !removed_by_x9(c)))
        .and_then(|i| levels.get(i).copied())
        .unwrap_or(para);
    let sos = direction_of(level.max(before));
    let after = if matches!(classes.get(last), Some(LRI | RLI | FSI)) {
        para
    } else {
        (last + 1..n)
            .find(|&i| classes.get(i).copied().is_some_and(|c| !removed_by_x9(c)))
            .and_then(|i| levels.get(i).copied())
            .unwrap_or(para)
    };
    let eos = direction_of(level.max(after));

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
    let mut last_strong = sos;
    for k in 0..m {
        match t(r, k) {
            c @ (L | R | AL) => last_strong = c,
            EN if last_strong == AL => put(r, k, AN),
            _ => {}
        }
    }
    for k in 0..m {
        if t(r, k) == AL {
            put(r, k, R);
        }
    }
    for k in 1..m.saturating_sub(1) {
        let (prev, cur, next) = (t(r, k - 1), t(r, k), t(r, k + 1));
        match (prev, cur, next) {
            (EN, ES | CS, EN) => put(r, k, EN),
            (AN, CS, AN) => put(r, k, AN),
            _ => {}
        }
    }
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
    for k in 0..m {
        if matches!(t(r, k), ES | ET | CS) {
            put(r, k, ON);
        }
    }
    let mut last_strong = sos;
    for k in 0..m {
        match t(r, k) {
            c @ (L | R) => last_strong = c,
            EN if last_strong == L => put(r, k, L),
            _ => {}
        }
    }

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
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lib_generated_tests.rs"]
mod generated;
