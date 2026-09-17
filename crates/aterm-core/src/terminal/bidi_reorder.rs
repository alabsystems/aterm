// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! BiDi visual-reordering bridge (feature `bidi`).
//!
//! Wires the engine's BiDi configuration ([`BiDiMode`] + [`ParagraphDirection`])
//! to the UAX #9 implicit reordering in the `aterm-bidi` crate. Compiled ONLY
//! when the off-by-default `bidi` feature is enabled; with the feature off this
//! module is absent and the engine build is byte-identical (the no-op posture in
//! `bidi_stubs.rs` is unaffected).
//!
//! Scope: this produces the per-line visual→logical column permutation AND
//! applies it at render time. [`Terminal::apply_bidi_reorder`] permutes each row
//! of the render snapshot (built by `cell_frame_into`) into visual order, so with
//! the `bidi` feature enabled (it is, in `aterm-gui`) RTL runs display correctly
//! on BOTH the CPU and GPU renderers and in the `image` introspection capture.
//! Runtime-gated by [`BiDiMode`] (default `Implicit`); pure-LTR rows are skipped.
//!
//! The paragraph direction each row is resolved against is the SCP direction
//! (`CSI Ps SP k`; the default `Auto` means the terminal's default, LTR).
//! First-strong autodetection (UAX #9 P2/P3) runs ONLY while DECSET 2501
//! (`modes.bidi_autodetection`) is set — the Terminal WG default is disabled,
//! and with it disabled the SCP direction is used directly. That gate is what
//! keeps an `ls` row whose first entry is Hebrew from becoming an RTL paragraph
//! and mirroring its LTR columns end to end while the grid (and `ctl text`)
//! still hold the logical order.

use super::Terminal;
use aterm_types::{BiDiMode, ParagraphDirection};

impl Terminal {
    /// Visual→logical column permutation for a row whose cells hold the scalar
    /// values `scalars`, honoring this terminal's current BiDi mode/direction.
    ///
    /// `result[v] == l` means the logical cell at index `l` is drawn at visual
    /// column `v` (left to right). Returns the identity permutation `0,1,2,…` when
    /// BiDi is disabled or the line is pure left-to-right, so a renderer can apply
    /// the result unconditionally.
    #[must_use]
    pub fn bidi_visual_order(&self, scalars: &[char]) -> Vec<usize> {
        compute_visual_order(
            self.modes.bidi_mode,
            self.modes.bidi_direction,
            self.modes.bidi_autodetection,
            scalars,
        )
    }

    /// Visual→logical CELL permutation for a rendered row, honoring this terminal's
    /// BiDi mode/direction AND wide-glyph cell pairing.
    ///
    /// A renderer holds [`RenderCell`](super::RenderCell) rows; a wide glyph (CJK,
    /// wide emoji) occupies two cells — a lead cell plus a right-half continuation
    /// — that must stay paired and unmirrored when the line is reordered.
    /// `result[v] == l` means the logical cell at index `l` is drawn at visual
    /// column `v` (left to right). Returns the identity permutation when BiDi is
    /// disabled or the row is pure left-to-right, so a renderer can apply it
    /// unconditionally. This is the cell-level companion to [`Self::bidi_visual_order`]
    /// and the entry point a renderer integration calls per visible row.
    #[must_use]
    pub fn bidi_visual_order_cells(&self, cells: &[super::RenderCell]) -> Vec<usize> {
        // Fast path: with BiDi off, skip the per-cell scalar/flag allocations.
        if self.modes.bidi_mode == BiDiMode::Disabled {
            return (0..cells.len()).collect();
        }
        let chars: Vec<char> = cells.iter().map(|c| c.ch).collect();
        let wide: Vec<bool> = cells.iter().map(|c| c.wide).collect();
        compute_visual_order_cells(
            self.modes.bidi_mode,
            self.modes.bidi_direction,
            self.modes.bidi_autodetection,
            &chars,
            &wide,
        )
    }

    /// Reorder each row of a render snapshot into BiDi VISUAL order, in place.
    ///
    /// This is the render-time application of [`Self::bidi_visual_order_cells`]:
    /// it permutes the dense [`RenderCell`](super::RenderCell) row AND the
    /// column-indexed sparse arrays (clusters / combining marks / inline images)
    /// AND the cursor column, so a renderer that draws the snapshot left-to-right
    /// shows right-to-left runs in the correct visual order. Called from
    /// [`Terminal::cell_frame_into`](super::Terminal::cell_frame_into), so BOTH
    /// the CPU and GPU renderers (and the `image` introspection capture) get
    /// visual order for free.
    ///
    /// Pure-LTR rows are skipped via a cheap first-RTL-block guard, so frames
    /// with no right-to-left content are byte-identical to the non-BiDi path.
    /// A no-op when BiDi is disabled.
    ///
    /// Every row is resolved against the SCP paragraph direction; UAX #9
    /// first-strong autodetection runs only while DECSET 2501 is set (see
    /// [`base_direction_from_classes`]). Under the shipping default — SCP 0,
    /// 2501 reset — that is an LTR paragraph per row, so a row is permuted
    /// only where an RTL run reverses in place: LTR words, digits and written
    /// blanks keep their columns, and the cursor stays on its cell.
    ///
    /// `refill_mask` is the DMG-1 damage-scoped arm's row mask (`None` on the
    /// full arm — every row is processed, the historical behaviour). Rows the
    /// mask does not name are RETAINED rows, and the carrier only allows them
    /// to be retained while [`RenderInput::engine_row_order`] was `Logical`
    /// — i.e. the previous fill permuted nothing, so every retained channel is
    /// still in LOGICAL order — and while a mode/direction/autodetection
    /// change (each of which calls `invalidate_bidi_all`, marking FULL damage)
    /// has not forced the full arm. Under those two facts a retained row's
    /// reorder decision is a pure function of unchanged inputs and re-derives
    /// as the identity, so skipping it is exact rather than merely cheap.
    /// Skipping is also what keeps the scoped arm's cost `O(damaged rows ×
    /// cols)` in a `bidi` build: the first-RTL-block guard would otherwise
    /// re-scan every retained cell.
    ///
    /// RETURNS whether any row was actually permuted — the value the fill
    /// stamps into `engine_row_order`. A `true` costs the NEXT frame its
    /// scoped arm (retained rows would be in visual order, and re-permuting
    /// them would double-permute); it never makes THIS frame wrong, because
    /// every row this fill permuted it permuted from logical order exactly
    /// once.
    pub(crate) fn apply_bidi_reorder(
        &mut self,
        frame: &mut crate::render::RenderInput,
        refill_mask: Option<&[u64]>,
    ) -> bool {
        if self.modes.bidi_mode == BiDiMode::Disabled {
            return false;
        }
        let mut reordered_any = false;
        let dir = self.modes.bidi_direction;
        let autodetect = self.modes.bidi_autodetection;
        // Reusable scratch buffers (held on `bidi_state` so their capacity persists
        // across rows AND frames); cleared + refilled per row, never reallocated for
        // a stable terminal size. Output is byte-identical to the per-row-allocating
        // path.
        let scratch = &mut self.bidi_state.scratch;
        for r in 0..frame.cells.len() {
            // Retained (unmasked) row on the damage-scoped arm: provably still
            // the identity under the carrier's preconditions (see the doc).
            if let Some(mask) = refill_mask
                && mask.get(r / 64).is_none_or(|w| w & (1u64 << (r % 64)) == 0)
            {
                continue;
            }
            // The cheap guard skips any row with no codepoint in or after the first
            // RTL block (U+0590); only such rows can reorder. Then compute the
            // per-cell BiDi classes + wide flags ONCE into the reused scratch.
            {
                let row = &frame.cells[r];
                if !row.iter().any(|c| c.ch >= '\u{0590}') {
                    continue;
                }
                scratch.classes.clear();
                scratch.wide.clear();
                scratch
                    .classes
                    .extend(row.iter().map(|c| aterm_bidi::bidi_class(c.ch)));
                scratch.wide.extend(row.iter().map(|c| c.wide));
            }
            // Pure-LTR (no R/AL/AN) → identity permutation; nothing to reorder.
            if !aterm_bidi::has_bidi_classes(&scratch.classes) {
                continue;
            }
            let base = base_direction_from_classes(dir, autodetect, &scratch.classes);
            // Resolve the visual→logical CELL permutation into `scratch.cell_order`,
            // reusing the inner UAX #9 working buffers (logical/lead_cell/has_cont/
            // types/levels/char_order) — no per-row heap allocation after warmup.
            // Output is byte-identical to `reorder_cells_with_classes`.
            aterm_bidi::reorder_cells_with_classes_into(
                &scratch.classes,
                &scratch.wide,
                base,
                &mut scratch.logical,
                &mut scratch.lead_cell,
                &mut scratch.has_cont,
                &mut scratch.types,
                &mut scratch.levels,
                &mut scratch.char_order,
                &mut scratch.cell_order,
            );
            // Identity (e.g. RTL-capable chars that still resolve LTR): skip.
            if scratch.cell_order.iter().enumerate().all(|(v, &l)| v == l) {
                continue;
            }
            // Inverse permutation: inv[logical] = visual column (reused buffer).
            scratch.inv.clear();
            scratch.inv.resize(scratch.cell_order.len(), 0usize);
            for (v, &l) in scratch.cell_order.iter().enumerate() {
                scratch.inv[l] = v;
            }
            // Dense cells: visual[v] = logical[order[v]] (RenderCell is Copy). Build
            // into the reused row buffer, then swap it into place — no fresh Vec.
            scratch.row_tmp.clear();
            scratch
                .row_tmp
                .extend(scratch.cell_order.iter().map(|&l| frame.cells[r][l]));
            std::mem::swap(&mut frame.cells[r], &mut scratch.row_tmp);
            // Sparse, column-indexed arrays: remap each logical col to visual.
            fn remap_cols<T>(entries: &mut [(usize, T)], inv: &[usize]) {
                for (c, _) in entries.iter_mut() {
                    if *c < inv.len() {
                        *c = inv[*c];
                    }
                }
                // Reordering permutes the visual columns, so the (col, _) keys are no
                // longer ascending. Re-sort by column — each list holds one entry per
                // column, so this is a total order and output-neutral — so the
                // per-column lookups (RenderInput::cluster_at/combining_at/image_at and
                // the CPU cluster_for/combining_for/image_covers) stay binary-searchable
                // instead of falling back to O(cols²) linear scans on a dense reordered
                // row. Runs only on genuinely-reordered rows (identity is skipped above).
                entries.sort_unstable_by_key(|(c, _)| *c);
            }
            remap_cols(&mut frame.clusters[r], &scratch.inv);
            remap_cols(&mut frame.combining[r], &scratch.inv);
            remap_cols(&mut frame.images[r], &scratch.inv);
            // The cursor follows its logical cell to its new visual column.
            if frame.cursor_row == r && frame.cursor_col < scratch.inv.len() {
                frame.cursor_col = scratch.inv[frame.cursor_col];
            }
            reordered_any = true;
        }
        reordered_any
    }
}

/// Pure mapping from BiDi config + line scalars to the visual→logical permutation.
///
/// `autodetect` is DECSET 2501 (`modes.bidi_autodetection`): whether UAX #9
/// first-strong detection may override the SCP direction `dir` (see
/// [`base_direction`]). Kept free-standing (not a method) so it is testable
/// without constructing a `Terminal`. `Terminal::bidi_visual_order` is the
/// one-line wrapper over it.
#[must_use]
pub fn compute_visual_order(
    mode: BiDiMode,
    dir: ParagraphDirection,
    autodetect: bool,
    scalars: &[char],
) -> Vec<usize> {
    // Disabled, or a pure-LTR line: identity (the common, hot case).
    if mode == BiDiMode::Disabled || !aterm_bidi::has_bidi(scalars) {
        return (0..scalars.len()).collect();
    }
    let base = base_direction(dir, autodetect, scalars);
    aterm_bidi::reorder_visual_to_logical(scalars, base)
}

/// Cell-level companion to [`compute_visual_order`]: parallel per-cell scalar and
/// wide-continuation slices in, visual→logical CELL permutation out. Kept
/// free-standing so it is testable without constructing a `Terminal`; wide-glyph
/// cell pairs are kept together (see [`aterm_bidi::reorder_cells`]).
/// `autodetect` is DECSET 2501, as for [`compute_visual_order`].
#[must_use]
pub fn compute_visual_order_cells(
    mode: BiDiMode,
    dir: ParagraphDirection,
    autodetect: bool,
    cell_chars: &[char],
    is_wide_continuation: &[bool],
) -> Vec<usize> {
    if mode == BiDiMode::Disabled {
        return (0..cell_chars.len()).collect();
    }
    // Compute the per-cell BiDi classes ONCE, then derive the `has_bidi` check,
    // the base direction, AND the level resolution from the same slice (instead
    // of recomputing `bidi_class` 2–3x over the row). Output is identical.
    let classes: Vec<aterm_bidi::BidiClass> = cell_chars
        .iter()
        .map(|&c| aterm_bidi::bidi_class(c))
        .collect();
    if !aterm_bidi::has_bidi_classes(&classes) {
        return (0..cell_chars.len()).collect();
    }
    let base = base_direction_from_classes(dir, autodetect, &classes);
    aterm_bidi::reorder_cells_with_classes(&classes, is_wide_continuation, base)
}

/// Map the engine's [`ParagraphDirection`] onto an `aterm-bidi` `BaseDirection`.
///
/// `autodetect` is DECSET 2501 (`modes.bidi_autodetection`). The Terminal WG
/// recommendation makes it the ONLY switch for first-strong detection: SCP 0
/// selects "the terminal's default" direction (LTR), 2501 defaults to disabled,
/// and while it is disabled "the model's corresponding flag is used directly".
/// So `Auto` is an LTR paragraph and `AutoRtl` an RTL one until 2501 is set;
/// only then does either run UAX #9 P2/P3 over the row. Mapping `Auto` to
/// `BaseDirection::Auto` unconditionally made every row whose first strong
/// character is R/AL an RTL paragraph, and L2 then mirrored its LTR columns.
///
/// `AutoRtl` under autodetection (default RTL when the line has no strong
/// character) has no direct UAX #9 analogue: it resolves to `Auto` when a
/// strong L/R/AL character is present and `Rtl` otherwise, matching its
/// "default to RTL" intent. SCP 1/2 (`Ltr`/`Rtl`) force their direction.
fn base_direction(
    dir: ParagraphDirection,
    autodetect: bool,
    scalars: &[char],
) -> aterm_bidi::BaseDirection {
    use aterm_bidi::{BaseDirection, BidiClass};
    // The terminal's default: first-strong detection only while 2501 is set.
    let default_ltr = if autodetect {
        BaseDirection::Auto
    } else {
        BaseDirection::Ltr
    };
    match dir {
        ParagraphDirection::Auto => default_ltr,
        ParagraphDirection::Ltr => BaseDirection::Ltr,
        ParagraphDirection::Rtl => BaseDirection::Rtl,
        ParagraphDirection::AutoRtl => {
            if !autodetect {
                return BaseDirection::Rtl;
            }
            let has_strong = scalars.iter().any(|&c| {
                matches!(
                    aterm_bidi::bidi_class(c),
                    BidiClass::L | BidiClass::R | BidiClass::AL
                )
            });
            if has_strong {
                BaseDirection::Auto
            } else {
                BaseDirection::Rtl
            }
        }
        // `ParagraphDirection` is #[non_exhaustive]; any future variant behaves
        // as `Auto`, the terminal's default.
        _ => default_ltr,
    }
}

/// Class-slice companion to [`base_direction`] for callers that already computed
/// the per-cell [`BidiClass`](aterm_bidi::BidiClass) slice (the cell-reorder
/// path). Identical mapping; only the `AutoRtl` strong-character scan reads the
/// precomputed classes instead of recomputing `bidi_class`.
fn base_direction_from_classes(
    dir: ParagraphDirection,
    autodetect: bool,
    classes: &[aterm_bidi::BidiClass],
) -> aterm_bidi::BaseDirection {
    use aterm_bidi::{BaseDirection, BidiClass};
    // The terminal's default: first-strong detection only while 2501 is set.
    let default_ltr = if autodetect {
        BaseDirection::Auto
    } else {
        BaseDirection::Ltr
    };
    match dir {
        ParagraphDirection::Auto => default_ltr,
        ParagraphDirection::Ltr => BaseDirection::Ltr,
        ParagraphDirection::Rtl => BaseDirection::Rtl,
        ParagraphDirection::AutoRtl => {
            if !autodetect {
                return BaseDirection::Rtl;
            }
            let has_strong = classes
                .iter()
                .any(|&c| matches!(c, BidiClass::L | BidiClass::R | BidiClass::AL));
            if has_strong {
                BaseDirection::Auto
            } else {
                BaseDirection::Rtl
            }
        }
        // `ParagraphDirection` is #[non_exhaustive]; any future variant behaves
        // as `Auto`, the terminal's default.
        _ => default_ltr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Terminal;

    const ALEF: char = '\u{05D0}';
    const BET: char = '\u{05D1}';

    /// End-to-end: a render snapshot of a Hebrew line is reordered into visual
    /// (right-to-left) order by `cell_frame` (which calls `apply_bidi_reorder`),
    /// while a pure-ASCII line is left in logical order.
    #[test]
    fn cell_frame_reorders_rtl_row_visually() {
        let mut term = Terminal::new(4, 8);
        // Default BiDiMode is Implicit, so a fresh terminal reorders RTL.
        assert_ne!(term.modes.bidi_mode, BiDiMode::Disabled);

        // Write three Hebrew letters: logical order ALEF, BET, GIMEL.
        term.process("\u{05D0}\u{05D1}\u{05D2}".as_bytes());
        let frame = term.cell_frame(4, 8);
        let row: Vec<char> = frame.cells[0].iter().take(3).map(|c| c.ch).collect();
        // Visual order is reversed for an RTL run.
        assert_eq!(
            row,
            vec!['\u{05D2}', '\u{05D1}', '\u{05D0}'],
            "Hebrew run must render right-to-left in the snapshot"
        );
    }

    #[test]
    fn scp_direction_change_reorders_and_damages_an_existing_row() {
        let mut term = Terminal::new(2, 12);
        // Mixed strong LTR/RTL runs make the paragraph base direction
        // observable; a lone Hebrew run reverses identically under both bases.
        term.process("abc \u{05D0}\u{05D1}\u{05D2}".as_bytes());
        term.process(b"\x1b[2 k");
        let rtl: Vec<char> = term.cell_frame(2, 12).cells[0]
            .iter()
            .take(7)
            .map(|cell| cell.ch)
            .collect();
        term.take_damage();
        let before = term.damage_epoch();

        term.process(b"\x1b[1 k");
        let ltr: Vec<char> = term.cell_frame(2, 12).cells[0]
            .iter()
            .take(7)
            .map(|cell| cell.ch)
            .collect();

        assert_ne!(ltr, rtl, "SCP must reproject the already-stored row");
        assert!(term.has_damage());
        assert!(term.damage_epoch() > before);
    }

    #[test]
    fn cell_frame_leaves_ascii_row_in_logical_order() {
        let mut term = Terminal::new(4, 8);
        term.process(b"abc");
        let frame = term.cell_frame(4, 8);
        let row: Vec<char> = frame.cells[0].iter().take(3).map(|c| c.ch).collect();
        assert_eq!(row, vec!['a', 'b', 'c'], "ASCII stays in logical order");
    }

    /// The shipping default: DECSET 2501 reset, so no first-strong detection.
    fn cv(mode: BiDiMode, dir: ParagraphDirection, s: &str) -> Vec<usize> {
        compute_visual_order(mode, dir, false, &s.chars().collect::<Vec<_>>())
    }

    #[test]
    fn disabled_is_always_identity() {
        // Even with RTL content, Disabled keeps logical order.
        assert_eq!(
            cv(
                BiDiMode::Disabled,
                ParagraphDirection::Auto,
                "\u{05D0}\u{05D1}"
            ),
            vec![0, 1]
        );
    }

    #[test]
    fn pure_ltr_is_identity_without_invoking_reorder() {
        assert_eq!(
            cv(BiDiMode::Implicit, ParagraphDirection::Auto, "abc"),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn implicit_reorders_rtl() {
        // Hebrew run reverses under implicit auto-detection.
        assert_eq!(
            cv(
                BiDiMode::Implicit,
                ParagraphDirection::Auto,
                "\u{05D0}\u{05D1}"
            ),
            vec![1, 0]
        );
    }

    #[test]
    fn explicit_mode_also_reorders_via_the_bridge() {
        // The bridge reorders for any non-Disabled mode (Explicit included) — the
        // mode distinction (engine-driven vs auto) is handled upstream; here a
        // non-disabled mode means "produce a visual order".
        assert_eq!(
            cv(BiDiMode::Explicit, ParagraphDirection::Rtl, "ab"),
            vec![0, 1]
        );
    }

    #[test]
    fn autortl_defaults_rtl_only_without_strong_chars() {
        use aterm_bidi::BaseDirection;
        // With DECSET 2501 set, a neutral-only line under AutoRtl uses an RTL
        // base; with a strong char it auto-detects normally.
        assert_eq!(
            base_direction(ParagraphDirection::AutoRtl, true, &[' ', '.']),
            BaseDirection::Rtl
        );
        assert_eq!(
            base_direction(ParagraphDirection::AutoRtl, true, &['a']),
            BaseDirection::Auto
        );
        assert_eq!(
            base_direction(ParagraphDirection::AutoRtl, true, &[ALEF]),
            BaseDirection::Auto
        );
        // With 2501 reset (the default) the SCP direction is used directly:
        // `Auto` is the terminal's default, LTR; `AutoRtl` is RTL, strong
        // characters or not. SCP 1/2 force their direction either way.
        assert_eq!(
            base_direction(ParagraphDirection::Auto, false, &[ALEF, 'a']),
            BaseDirection::Ltr
        );
        assert_eq!(
            base_direction(ParagraphDirection::Auto, true, &[ALEF, 'a']),
            BaseDirection::Auto
        );
        assert_eq!(
            base_direction(ParagraphDirection::AutoRtl, false, &['a']),
            BaseDirection::Rtl
        );
        assert_eq!(
            base_direction(ParagraphDirection::Ltr, true, &[ALEF]),
            BaseDirection::Ltr
        );
        assert_eq!(
            base_direction(ParagraphDirection::Rtl, true, &['a']),
            BaseDirection::Rtl
        );
        // The class-slice twin agrees with the scalar mapping on every arm.
        for dir in [
            ParagraphDirection::Auto,
            ParagraphDirection::AutoRtl,
            ParagraphDirection::Ltr,
            ParagraphDirection::Rtl,
        ] {
            for autodetect in [false, true] {
                for scalars in [&[' ', '.'][..], &['a'][..], &[ALEF][..]] {
                    let classes: Vec<_> =
                        scalars.iter().map(|&c| aterm_bidi::bidi_class(c)).collect();
                    assert_eq!(
                        base_direction_from_classes(dir, autodetect, &classes),
                        base_direction(dir, autodetect, scalars),
                        "{dir:?} autodetect={autodetect} {scalars:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn terminal_method_uses_default_implicit_config() {
        // A fresh Terminal defaults to Implicit/Auto, so the method reorders RTL.
        let term = Terminal::new(24, 80);
        assert_eq!(term.bidi_visual_order(&[ALEF, BET]), vec![1, 0]);
        // Plain ASCII stays identity.
        assert_eq!(term.bidi_visual_order(&['x', 'y', 'z']), vec![0, 1, 2]);
    }

    const CJK: char = '\u{4E2D}'; // 中 — a wide (2-cell) glyph

    #[test]
    fn cells_disabled_is_identity() {
        // Disabled mode: identity even with RTL + wide content, autodetected or not.
        let chars = [ALEF, CJK, ' '];
        let wide = [false, false, true];
        for autodetect in [false, true] {
            assert_eq!(
                compute_visual_order_cells(
                    BiDiMode::Disabled,
                    ParagraphDirection::Auto,
                    autodetect,
                    &chars,
                    &wide
                ),
                vec![0, 1, 2]
            );
        }
    }

    #[test]
    fn cells_implicit_reorders_rtl_keeping_wide_pair() {
        // ALEF (R, 1 cell) + 中 (L, 2 cells). Under the default LTR paragraph
        // (2501 reset) the lone Hebrew letter is a one-cell RTL run and the
        // row is the identity. With 2501 set the first strong character makes
        // an RTL paragraph → 中 moves left of the Hebrew letter but its
        // [lead, continuation] cells stay in order.
        let chars = [ALEF, CJK, ' '];
        let wide = [false, false, true];
        assert_eq!(
            compute_visual_order_cells(
                BiDiMode::Implicit,
                ParagraphDirection::Auto,
                false,
                &chars,
                &wide
            ),
            vec![0, 1, 2]
        );
        assert_eq!(
            compute_visual_order_cells(
                BiDiMode::Implicit,
                ParagraphDirection::Auto,
                true,
                &chars,
                &wide
            ),
            vec![1, 2, 0]
        );
    }

    #[test]
    fn terminal_cell_method_uses_default_config_and_real_cells() {
        // Drive a real engine so the method runs over actual RenderCell rows.
        let mut term = Terminal::new(2, 8);
        term.process("ab".as_bytes());
        let cells = term.render_row(0);
        // Pure ASCII row → identity.
        assert_eq!(
            term.bidi_visual_order_cells(&cells),
            (0..cells.len()).collect::<Vec<_>>()
        );

        let mut rtl = Terminal::new(2, 8);
        rtl.process("\u{05D0}\u{05D1}".as_bytes()); // ALEF BET
        let rcells = rtl.render_row(0);
        // The two Hebrew lead cells reverse; trailing blanks stay in place.
        let order = rtl.bidi_visual_order_cells(&rcells);
        assert_eq!(
            order[0], 1,
            "first visual cell is the 2nd Hebrew letter; got {order:?}"
        );
        assert_eq!(
            order[1], 0,
            "second visual cell is the 1st Hebrew letter; got {order:?}"
        );
        // Still a permutation of all cells.
        let mut seen = order.clone();
        seen.sort_unstable();
        assert_eq!(seen, (0..rcells.len()).collect::<Vec<_>>());
    }
    /// Row 0 of a fresh render snapshot as a string, `n` columns wide.
    fn row0(term: &mut Terminal, rows: usize, cols: usize, n: usize) -> String {
        term.cell_frame(rows, cols).cells[0]
            .iter()
            .take(n)
            .map(|c| c.ch)
            .collect()
    }

    /// An `ls`-shaped row whose first entry is Hebrew. The grid holds
    /// "אב.txt  foo.txt  bar.txt"; under the shipping default (SCP 0, DECSET
    /// 2501 reset) the paragraph is LTR, so only the Hebrew run reverses and
    /// the LTR columns keep their places. Before the 2501 gate the first
    /// strong character made an RTL paragraph and L2 mirrored the whole row
    /// to "txt  foo.txt  bar.txt.בא" — the columns transposed on glass while
    /// `ctl text` (the grid) still read fine.
    const LS_ROW: &str = "\u{05D0}\u{05D1}.txt  foo.txt  bar.txt";
    const LS_ROW_LTR: &str = "\u{05D1}\u{05D0}.txt  foo.txt  bar.txt";

    /// The same row as an RTL paragraph (what 2501 / SCP 2 produce): every
    /// LTR word stays intact, the words swap end to end, the Hebrew reverses.
    fn assert_mirrored(row: &str) {
        assert!(
            row.starts_with("txt  foo.txt  bar.txt.\u{05D1}\u{05D0}"),
            "expected the mirrored RTL-paragraph layout, got {row:?}"
        );
    }

    #[test]
    fn default_row_with_rtl_first_keeps_ltr_columns() {
        let mut term = Terminal::new(2, 32);
        assert!(!term.modes.bidi_autodetection, "2501 is reset by default");
        term.process(LS_ROW.as_bytes());
        assert_eq!(row0(&mut term, 2, 32, 24), LS_ROW_LTR);

        // A table row: digits, an RTL word, digits, a word. The RTL word
        // attaches the following digits (UAX #9 N1 treats EN as R), so the
        // pair "אב  34" reverses as a unit; the leading "12" and trailing
        // "end" columns are untouched.
        let mut table = Terminal::new(2, 32);
        table.process("  12  \u{05D0}\u{05D1}  34  end".as_bytes());
        assert_eq!(
            row0(&mut table, 2, 32, 17),
            "  12  34  \u{05D1}\u{05D0}  end"
        );
    }

    #[test]
    fn decset_2501_turns_first_strong_autodetection_on_and_off() {
        let mut term = Terminal::new(2, 32);
        term.process(b"\x1b[?2501$p");
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b[?2501;2$y",
            "DECRQM must report autodetection reset on a fresh terminal"
        );
        term.process(LS_ROW.as_bytes());
        assert_eq!(row0(&mut term, 2, 32, 24), LS_ROW_LTR);
        term.take_damage();

        // Set: the row's first strong character is Hebrew → RTL paragraph.
        // The already-stored row must be reprojected AND damaged, so a
        // renderer that retained it repaints.
        term.process(b"\x1b[?2501h");
        assert!(
            term.has_damage(),
            "toggling 2501 must damage the presented grid"
        );
        // The mirrored layout right-anchors: written blanks move left.
        let frame = term.cell_frame(2, 32);
        let full: String = frame.cells[0].iter().map(|c| c.ch).collect();
        assert_mirrored(full.trim_start());
        term.process(b"\x1b[?2501$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?2501;1$y");

        // Reset: back to the LTR paragraph.
        term.process(b"\x1b[?2501l");
        assert_eq!(row0(&mut term, 2, 32, 24), LS_ROW_LTR);
    }

    #[test]
    fn scp_zero_is_the_terminal_default_not_autodetect() {
        let mut term = Terminal::new(2, 32);
        term.process(LS_ROW.as_bytes());
        // SCP 2 forces an RTL paragraph: the row mirrors.
        term.process(b"\x1b[2 k");
        let frame = term.cell_frame(2, 32);
        let full: String = frame.cells[0].iter().map(|c| c.ch).collect();
        assert_mirrored(full.trim_start());
        // SCP 1 forces LTR.
        term.process(b"\x1b[1 k");
        let ltr = row0(&mut term, 2, 32, 24);
        assert_eq!(ltr, LS_ROW_LTR);
        // SCP 0 is "the terminal's default", which is the same LTR paragraph —
        // not first-strong detection.
        term.process(b"\x1b[0 k");
        assert_eq!(term.modes.bidi_direction, ParagraphDirection::Auto);
        assert_eq!(row0(&mut term, 2, 32, 24), ltr);
    }

    #[test]
    fn decstr_resets_autodetection() {
        let mut term = Terminal::new(2, 32);
        term.process(LS_ROW.as_bytes());
        term.process(b"\x1b[?2501h");
        assert!(term.modes.bidi_autodetection);
        term.process(b"\x1b[!p");
        assert!(!term.modes.bidi_autodetection, "DECSTR resets 2501");
        assert_eq!(row0(&mut term, 2, 32, 24), LS_ROW_LTR);
    }

    /// A pure-RTL word padded with WRITTEN blanks (an app that pads its
    /// columns) stays left-anchored under the default LTR paragraph, and the
    /// cursor stays on its cell. Under the old RTL-paragraph resolution UAX #9
    /// L1 put the trailing blanks at paragraph level 1 too, so the whole row
    /// reversed: the word jumped to the right edge and the cursor to column 0.
    #[test]
    fn pure_rtl_row_stays_left_anchored() {
        let mut term = Terminal::new(2, 8);
        term.process("\u{05D0}\u{05D1}\u{05D2}     ".as_bytes());
        let frame = term.cell_frame(2, 8);
        let row: String = frame.cells[0].iter().map(|c| c.ch).collect();
        assert_eq!(row, "\u{05D2}\u{05D1}\u{05D0}     ");
        assert_eq!(frame.cursor_row, 0);
        assert_eq!(
            frame.cursor_col, 7,
            "the cursor follows its logical cell, which did not move"
        );
    }

    /// Fixed-point property of the default (2501 reset) resolution: for a row
    /// shaped [ASCII word][blanks][Hebrew word][blanks][ASCII word], every
    /// ASCII cell is a fixed point of the permutation — an LTR paragraph
    /// never moves an L run, whatever the RTL run between them does.
    #[test]
    fn ltr_cells_are_fixed_points_under_the_default_paragraph() {
        let ascii = ["a", "foo.txt", "x1", "README", "end"];
        let hebrew = ["\u{05D0}", "\u{05D0}\u{05D1}", "\u{05D0}\u{05D1}\u{05D2}"];
        let arabic = ["\u{0627}", "\u{0627}\u{0644}\u{0641}"];
        for lead in ascii {
            for rtl in hebrew.iter().chain(arabic.iter()) {
                for pad in 1..=3usize {
                    for trail in ascii {
                        let blanks = " ".repeat(pad);
                        let row = format!("{lead}{blanks}{rtl}{blanks}{trail}");
                        let scalars: Vec<char> = row.chars().collect();
                        let order = compute_visual_order(
                            BiDiMode::Implicit,
                            ParagraphDirection::Auto,
                            false,
                            &scalars,
                        );
                        for (l, &c) in scalars.iter().enumerate() {
                            if c.is_ascii() && c != ' ' {
                                assert_eq!(
                                    order[l], l,
                                    "{row:?}: ASCII cell {l} ({c:?}) moved; order={order:?}"
                                );
                            }
                        }
                        // Negative control: with 2501 set a leading RTL word is
                        // the paragraph's first strong character, so the row
                        // mirrors — its first logical cell lands in the LAST
                        // visual column (the old default's signature).
                        let rtl_first = format!("{rtl}{blanks}{lead}{blanks}{trail}");
                        let scalars: Vec<char> = rtl_first.chars().collect();
                        let order = compute_visual_order(
                            BiDiMode::Implicit,
                            ParagraphDirection::Auto,
                            true,
                            &scalars,
                        );
                        assert_eq!(
                            order[scalars.len() - 1],
                            0,
                            "{rtl_first:?}: autodetection must mirror the row; order={order:?}"
                        );
                    }
                }
            }
        }
    }
}
