// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! BiDi visual-reordering bridge between [`Terminal`] rows and [`aterm_bidi`].

use super::Terminal;
use aterm_grid::VisibleRowView;
use aterm_types::{BiDiMode, ParagraphDirection};

impl Terminal {
    /// Compute the visual-to-logical cell permutation for a rendered row.
    #[must_use]
    pub fn bidi_visual_order_cells(&self, cells: &[super::RenderCell]) -> Vec<usize> {
        // Deliberate terminal fast path: with BiDi off or no codepoint at/past the first RTL
        // block (U+0590), keep pure-Latin/blank rows left-anchored in logical order (even under
        // SCP Rtl/AutoRtl) and skip per-cell scalar/flag allocations.
        if self.modes.bidi_mode == BiDiMode::Disabled || !cells.iter().any(|c| c.ch >= '\u{0590}') {
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

    /// Return visible row `row` in display order (mirrored text and visual-to-logical map).
    #[must_use]
    pub fn row_display_order(&self, row: u16) -> Option<(String, Vec<usize>)> {
        if row >= self.grid.rows() {
            return None;
        }
        let cols = self.grid.cols();
        let view = self.grid.screen_row_view(row);
        Some(
            if self.modes.bidi_mode != BiDiMode::Disabled && may_reorder(&view) {
                self.row_display_order_full(&view, cols)
            } else {
                row_logical_order(&view, cols)
            },
        )
    }

    fn row_display_order_full(&self, view: &VisibleRowView<'_>, cols: u16) -> (String, Vec<usize>) {
        let (chars, continuation) = render_scalars(view);
        let mut levels = aterm_bidi::Scratch::default();
        let mut map = visual_order_cells_into(
            self.modes.bidi_mode,
            self.modes.bidi_direction,
            self.modes.bidi_autodetection,
            &chars,
            &continuation,
            &mut levels,
        );
        map.extend(map.len()..usize::from(cols));
        let mut text = String::with_capacity(usize::from(cols));
        for &l in &map {
            if let Ok(col) = u16::try_from(l) {
                let start = text.len();
                push_text(view, col, &mut text);
                if levels.cell_is_rtl(l)
                    && let Some(first) = text.get(start..).and_then(|t| t.chars().next())
                    && let Some(mirror) = aterm_bidi::mirror_glyph(first)
                {
                    let end = start + first.len_utf8();
                    text.replace_range(start..end, mirror.encode_utf8(&mut [0; 4]));
                }
            }
        }
        let end = text.trim_end().len();
        text.truncate(end);
        (text, map)
    }

    /// Map a viewport cell `(row, col)` to its logical column and whether it resolved RTL.
    #[must_use]
    pub fn viewport_cell_logical(&self, row: u16, col: u16) -> (u16, bool) {
        if self.modes.bidi_mode == BiDiMode::Disabled {
            return (col, false);
        }
        let view = self.grid().visible_row_view(row);
        if col >= view.len() || !may_reorder(&view) {
            return (col, false);
        }
        let (chars, wide) = render_scalars(&view);
        let visual = usize::from(col);
        if visual >= chars.len() || !chars.iter().any(|&ch| ch >= '\u{0590}') {
            return (col, false);
        }
        let classes: Vec<aterm_bidi::BidiClass> =
            chars.iter().map(|&ch| aterm_bidi::bidi_class(ch)).collect();
        if !aterm_bidi::has_bidi_classes(&classes) {
            return (col, false);
        }
        let brackets: Vec<aterm_bidi::Bracket> =
            chars.iter().map(|&ch| aterm_bidi::bracket_of(ch)).collect();
        let base = base_direction_from_classes(
            self.modes.bidi_direction,
            self.modes.bidi_autodetection,
            &classes,
        );
        let levels = aterm_bidi::cell_levels(&classes, &brackets, &wide, base);
        if !levels.iter().any(|&lvl| lvl > 0) {
            return (col, false);
        }
        let order = aterm_bidi::reorder_cells_with_classes(&classes, &brackets, &wide, base);
        let logical = order.get(visual).copied().unwrap_or(visual);
        let rtl = levels.get(logical).is_some_and(|level| level % 2 == 1);
        (u16::try_from(logical).unwrap_or(col), rtl)
    }

    /// Map a painted viewport cell `(row, col)` to its logical column and RTL flag.
    #[inline]
    #[must_use]
    pub fn painted_cell_logical(&self, row: u16, col: u16) -> (u16, bool) {
        use crate::grid::LineSize;
        let col = match self.grid().row(row).map(crate::grid::Row::line_size) {
            Some(
                LineSize::DoubleWidth | LineSize::DoubleHeightTop | LineSize::DoubleHeightBottom,
            ) => col / 2,
            _ => col,
        };
        self.viewport_cell_logical(row, col)
    }

    /// Reorder each row of a render snapshot into BiDi visual order in place.
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
        let scratch = &mut self.bidi_state.scratch;
        for r in 0..frame.cells.len() {
            // A retained (unmasked) row is still the identity (see the doc).
            if let Some(mask) = refill_mask
                && mask.get(r / 64).is_none_or(|w| w & (1u64 << (r % 64)) == 0)
            {
                continue;
            }
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
            if !aterm_bidi::has_bidi_classes(&scratch.classes) {
                continue;
            }
            let base = base_direction_from_classes(dir, autodetect, &scratch.classes);
            scratch.brackets.clear();
            scratch
                .brackets
                .extend(frame.cells[r].iter().map(|c| aterm_bidi::bracket_of(c.ch)));
            aterm_bidi::reorder_cells_with_classes_into(
                &scratch.classes,
                &scratch.brackets,
                &scratch.wide,
                base,
                &mut scratch.bidi,
                &mut scratch.cell_order,
            );
            if scratch.cell_order.iter().enumerate().all(|(v, &l)| v == l) {
                continue;
            }
            scratch.inv.clear();
            scratch.inv.resize(scratch.cell_order.len(), 0usize);
            for (v, &l) in scratch.cell_order.iter().enumerate() {
                scratch.inv[l] = v;
            }
            scratch.row_tmp.clear();
            let levels = &scratch.bidi;
            scratch.row_tmp.extend(scratch.cell_order.iter().map(|&l| {
                let mut cell = frame.cells[r][l];
                if levels.cell_is_rtl(l)
                    && let Some(mirror) = aterm_bidi::mirror_glyph(cell.ch)
                {
                    cell.ch = mirror;
                }
                cell
            }));
            std::mem::swap(&mut frame.cells[r], &mut scratch.row_tmp);
            fn remap_cols<T>(entries: &mut [(usize, T)], inv: &[usize]) {
                for (c, _) in entries.iter_mut() {
                    if *c < inv.len() {
                        *c = inv[*c];
                    }
                }
                entries.sort_by_key(|(c, _)| *c);
            }
            remap_cols(&mut frame.clusters[r], &scratch.inv);
            remap_cols(&mut frame.combining[r], &scratch.inv);
            remap_cols(&mut frame.images[r], &scratch.inv);
            if frame.cursor_row == r && frame.cursor_col < scratch.inv.len() {
                frame.cursor_col = scratch.inv[frame.cursor_col];
            }
            if let Some(map) = frame.visual_to_logical.get_mut(r) {
                map.clear();
                map.extend(
                    scratch
                        .cell_order
                        .iter()
                        .map(|&l| u16::try_from(l).unwrap_or(u16::MAX)),
                );
            }
            reordered_any = true;
        }
        reordered_any
    }
}

/// Whether `view` holds a stored codepoint at or past the first RTL block
/// (U+0590): the guard of [`Terminal::bidi_visual_order_cells`], read off the
/// cells before anything is gathered.
#[inline]
fn may_reorder(view: &VisibleRowView<'_>) -> bool {
    (0..view.len()).any(|c| view.cell(c).is_some_and(|cell| cell.char() >= '\u{0590}'))
}

fn render_scalars(view: &VisibleRowView<'_>) -> (Vec<char>, Vec<bool>) {
    let len = usize::from(view.len());
    let mut chars = Vec::with_capacity(len);
    let mut continuation = Vec::with_capacity(len);
    let mut prev_wide = false;
    for c in 0..view.len() {
        let Some(cell) = view.cell(c) else {
            prev_wide = false;
            continue;
        };
        let is_cont = cell.is_wide_continuation() && !cell.is_wide() && prev_wide;
        prev_wide = cell.is_wide();
        let raw = if cell.is_complex() {
            view.cell_data(c, cell).complex_base().unwrap_or('\u{FFFD}')
        } else {
            cell.char()
        };
        chars.push(if is_cont || raw == '\0' { ' ' } else { raw });
        continuation.push(is_cont);
    }
    (chars, continuation)
}

fn row_logical_order(view: &VisibleRowView<'_>, cols: u16) -> (String, Vec<usize>) {
    let mut text = String::with_capacity(usize::from(cols));
    for col in 0..cols {
        push_text(view, col, &mut text);
    }
    let end = text.trim_end().len();
    text.truncate(end);
    (text, (0..usize::from(cols)).collect())
}

/// Column `col`'s text in a row read: nothing for a wide glyph's continuation
/// or a column past the row.
#[inline]
fn push_text(view: &VisibleRowView<'_>, col: u16, out: &mut String) {
    if !view.is_wide_continuation(col)
        && let Some(cell) = view.cell(col)
    {
        view.push_cell_text(col, cell, out);
    }
}

/// Compute the visual-to-logical cell permutation from BiDi config, scalars, and wide flags.
#[inline]
#[must_use]
pub fn compute_visual_order_cells(
    mode: BiDiMode,
    dir: ParagraphDirection,
    autodetect: bool,
    cell_chars: &[char],
    is_wide_continuation: &[bool],
) -> Vec<usize> {
    visual_order_cells_into(
        mode,
        dir,
        autodetect,
        cell_chars,
        is_wide_continuation,
        &mut aterm_bidi::Scratch::default(),
    )
}

fn visual_order_cells_into(
    mode: BiDiMode,
    dir: ParagraphDirection,
    autodetect: bool,
    cell_chars: &[char],
    is_wide_continuation: &[bool],
    levels: &mut aterm_bidi::Scratch,
) -> Vec<usize> {
    if mode == BiDiMode::Disabled || !cell_chars.iter().any(|&c| c >= '\u{0590}') {
        return (0..cell_chars.len()).collect();
    }
    let classes: Vec<aterm_bidi::BidiClass> = cell_chars
        .iter()
        .map(|&c| aterm_bidi::bidi_class(c))
        .collect();
    if !aterm_bidi::has_bidi_classes(&classes) {
        return (0..cell_chars.len()).collect();
    }
    let base = base_direction_from_classes(dir, autodetect, &classes);
    let brackets: Vec<aterm_bidi::Bracket> = cell_chars
        .iter()
        .map(|&c| aterm_bidi::bracket_of(c))
        .collect();
    let mut order = Vec::new();
    aterm_bidi::reorder_cells_with_classes_into(
        &classes,
        &brackets,
        is_wide_continuation,
        base,
        levels,
        &mut order,
    );
    order
}

/// Map the engine's [`ParagraphDirection`] onto an `aterm-bidi`
/// `BaseDirection`, reading the row's precomputed per-cell
/// [`BidiClass`](aterm_bidi::BidiClass)es.
#[inline]
fn base_direction_from_classes(
    dir: ParagraphDirection,
    autodetect: bool,
    classes: &[aterm_bidi::BidiClass],
) -> aterm_bidi::BaseDirection {
    use aterm_bidi::{BaseDirection, BidiClass};
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
        _ => default_ltr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::{SelectionSide, SelectionType};
    use crate::terminal::Terminal;

    const ALEF: char = '\u{05D0}';
    const BET: char = '\u{05D1}';

    #[test]
    fn scp_direction_change_reorders_and_damages_an_existing_row() {
        let mut term = Terminal::new(2, 12);
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
    fn cell_frame_mirrors_right_to_left_brackets() {
        let mut term = Terminal::new(2, 8);
        term.process(format!("{ALEF}({BET})\u{05D2}\r\n({ALEF}{BET})").as_bytes());
        let frame = term.cell_frame(2, 8);
        let row =
            |r: usize, n: usize| -> String { frame.cells[r][..n].iter().map(|c| c.ch).collect() };
        assert_eq!(row(0, 5), format!("\u{05D2}({BET}){ALEF}"));
        assert_eq!(row(1, 4), format!("({BET}{ALEF})"));
    }

    #[test]
    fn autortl_defaults_rtl_only_without_strong_chars() {
        use aterm_bidi::BaseDirection;
        let base = |dir, autodetect, scalars: &[char]| {
            let classes: Vec<_> = scalars.iter().map(|&c| aterm_bidi::bidi_class(c)).collect();
            base_direction_from_classes(dir, autodetect, &classes)
        };
        let cases = [
            (
                ParagraphDirection::AutoRtl,
                true,
                &[' ', '.'][..],
                BaseDirection::Rtl,
            ),
            (
                ParagraphDirection::AutoRtl,
                true,
                &['a'][..],
                BaseDirection::Auto,
            ),
            (
                ParagraphDirection::AutoRtl,
                true,
                &[ALEF][..],
                BaseDirection::Auto,
            ),
            (
                ParagraphDirection::Auto,
                false,
                &[ALEF, 'a'][..],
                BaseDirection::Ltr,
            ),
            (
                ParagraphDirection::Auto,
                true,
                &[ALEF, 'a'][..],
                BaseDirection::Auto,
            ),
            (
                ParagraphDirection::AutoRtl,
                false,
                &['a'][..],
                BaseDirection::Rtl,
            ),
            (
                ParagraphDirection::Ltr,
                true,
                &[ALEF][..],
                BaseDirection::Ltr,
            ),
            (
                ParagraphDirection::Rtl,
                true,
                &['a'][..],
                BaseDirection::Rtl,
            ),
        ];
        for (dir, autodetect, scalars, want) in cases {
            assert_eq!(
                base(dir, autodetect, scalars),
                want,
                "{dir:?} autodetect={autodetect} {scalars:?}"
            );
        }
    }

    const CJK: char = '\u{4E2D}';

    #[test]
    fn cells_implicit_reorders_rtl_keeping_wide_pair() {
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
    fn row_display_order_is_the_renderers_permutation() {
        let mut t = Terminal::new(2, 10);
        t.process("x \u{05D0}\u{05D1} \u{4E2D}y".as_bytes());
        let (text, map) = t.row_display_order(0).expect("on screen");
        let painted = t.bidi_visual_order_cells(&t.render_row(0));
        assert_eq!(map[..painted.len()], painted[..]);
        assert!(
            map[painted.len()..]
                .iter()
                .copied()
                .eq(painted.len()..map.len())
        );
        assert_ne!(map, (0..map.len()).collect::<Vec<_>>(), "the RTL run moved");
        assert!(
            text.contains("\u{05D1}\u{05D0}"),
            "BET before ALEF on screen: {text:?}"
        );
        assert!(
            text.contains('\u{4E2D}'),
            "the wide glyph is whole: {text:?}"
        );
        let mut brackets = Terminal::new(1, 10);
        brackets.process("(\u{05D0}(\u{05D1})\u{05D2})".as_bytes());
        let (text, _) = brackets.row_display_order(0).expect("on screen");
        let frame = brackets.cell_frame(1, 10);
        let painted: String = frame.cells[0][..7].iter().map(|c| c.ch).collect();
        assert_eq!(text, painted, "the read is what the frame paints");
        assert_eq!(text, "(\u{05D2}(\u{05D1})\u{05D0})");
        let mut ltr = Terminal::new(2, 10);
        ltr.process(b"hello");
        let (text, map) = ltr.row_display_order(0).expect("on screen");
        assert_eq!(text, "hello");
        assert_eq!(map, (0..10).collect::<Vec<_>>());
        assert!(ltr.row_display_order(2).is_none(), "off the screen");
    }

    #[test]
    fn ltr_fast_path_equals_the_full_pass() {
        const COLS: u16 = 12;
        let corpus: &[(&str, &str, bool, bool, bool, &str)] = &[
            ("ascii", "hello world", true, false, false, "hello world"),
            (
                "below U+0590",
                "caf\u{E9} \u{3B1}\u{3B2} \u{436}",
                true,
                false,
                false,
                "caf\u{E9} \u{3B1}\u{3B2} \u{436}",
            ),
            (
                "NFD combining",
                "e\u{301}a\u{308}o",
                true,
                false,
                false,
                "e\u{301}a\u{308}o",
            ),
            (
                "interior NUL cells",
                "a\x1b[3Cb",
                true,
                false,
                false,
                "a   b",
            ),
            (
                "interior and trailing blanks",
                "a  b   ",
                true,
                false,
                false,
                "a  b",
            ),
            (
                "DECSCA-protected",
                "\x1b[1\"qab\x1b[0\"qc",
                true,
                false,
                false,
                "abc",
            ),
            (
                "BiDi off, Hebrew",
                "x \u{5D0}\u{5D1} y",
                false,
                false,
                false,
                "x \u{5D0}\u{5D1} y",
            ),
            (
                "BiDi off, complex right-to-left",
                "x \u{10900}\u{10901} y",
                false,
                false,
                false,
                "x \u{10900}\u{10901} y",
            ),
            (
                "CJK wide",
                "\u{4E2D}\u{6587}x",
                true,
                true,
                false,
                "\u{4E2D}\u{6587}x",
            ),
            (
                "DECSCA-protected after a wide glyph",
                "\u{4E2D}\x1b[1\"qab\x1b[0\"q",
                true,
                true,
                false,
                "\u{4E2D}ab",
            ),
            (
                "ZWJ emoji",
                "a\u{1F469}\u{200D}\u{1F4BB}b",
                true,
                true,
                false,
                "a\u{1F469}\u{200D}\u{1F4BB}b",
            ),
            (
                "Hebrew with a point",
                "x \u{5D0}\u{5B7}\u{5D1} y",
                true,
                true,
                true,
                "x \u{5D1}\u{5D0}\u{5B7} y",
            ),
            (
                "complex right-to-left",
                "x \u{10900}\u{10901} y",
                true,
                true,
                true,
                "x \u{10901}\u{10900} y",
            ),
            (
                "DECSCA-protected Hebrew",
                "x \x1b[1\"q\u{5D0}\u{5D1}\x1b[0\"q y",
                true,
                true,
                true,
                "x \u{5D1}\u{5D0} y",
            ),
            (
                "SCP 2, unwritten tail",
                "\x1b[2 kx \u{5D0}\u{5D1} y",
                true,
                true,
                true,
                "y \u{5D1}\u{5D0} x",
            ),
        ];
        for &(case, bytes, bidi, let_in, moves, text) in corpus {
            let mut t = Terminal::new(1, COLS);
            if !bidi {
                t.modes.bidi_mode = BiDiMode::Disabled;
            }
            t.process(bytes.as_bytes());
            let view = t.grid.screen_row_view(0);
            assert_eq!(bidi && may_reorder(&view), let_in, "{case}: the guard");
            let full = t.row_display_order_full(&view, COLS);
            let logical = row_logical_order(&view, COLS);
            let read = t.row_display_order(0).expect("on screen");
            let arm = if let_in { &full } else { &logical };
            assert_eq!(&read, arm, "{case}");
            assert_eq!(full.0, text, "{case}");
            let painted = t.bidi_visual_order_cells(&t.render_row(0));
            assert_eq!(
                full.1[..painted.len()],
                painted[..],
                "{case}: the frame's map"
            );
            assert!(
                full.1[painted.len()..]
                    .iter()
                    .copied()
                    .eq(painted.len()..usize::from(COLS)),
                "{case}: the blank tail stays"
            );
            if moves {
                assert_ne!(logical.0, full.0, "{case}");
                assert_ne!(logical.1, full.1, "{case}");
            } else {
                assert_eq!(logical, full, "{case}");
            }
        }
    }

    const MIXED: &str = "abc \u{05E9}\u{05DC}\u{05D5}\u{05DD} \u{05E2}\u{05D5}\u{05DC}\u{05DD} def";
    const OLAM: &str = "\u{05E2}\u{05D5}\u{05DC}\u{05DD}";

    fn painted(frame: &crate::render::RenderInput) -> Vec<usize> {
        let row = &frame.cells[0];
        (0..frame.cols)
            .filter(|&v| {
                let wide_lead = row.get(v + 1).is_some_and(|c| c.wide);
                let wide_cont = row.get(v).is_some_and(|c| c.wide);
                frame.selection_contains_cell(0, v, wide_lead, wide_cont)
            })
            .collect()
    }

    fn painted_text_logical(frame: &crate::render::RenderInput, cols: &[usize]) -> String {
        let mut logical: Vec<(usize, char)> = cols
            .iter()
            .map(|&v| (frame.logical_col(0, v), frame.cells[0][v].ch))
            .collect();
        logical.sort_unstable();
        logical.into_iter().map(|(_, ch)| ch).collect()
    }

    #[test]
    fn a_logical_selection_lights_the_cells_it_copies_on_a_reordered_row() {
        let mut term = Terminal::new(4, 30);
        term.process(MIXED.as_bytes());
        let sel = term.text_selection_mut();
        sel.start_selection(0, 9, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 12, SelectionSide::Right);
        sel.complete_selection();
        assert_eq!(term.selection_to_string().as_deref(), Some(OLAM));

        let frame = term.cell_frame(4, 30);
        assert_eq!(
            frame.visual_to_logical_row(0)[..14],
            [0, 1, 2, 3, 12, 11, 10, 9, 8, 7, 6, 5, 4, 13],
            "the frame records the row's visual→logical map"
        );
        assert!(
            frame.visual_to_logical_row(1).is_empty(),
            "an unpermuted row records no map"
        );
        let lit = painted(&frame);
        assert_eq!(
            lit,
            vec![4, 5, 6, 7],
            "the highlight sits where עולם is painted"
        );
        assert_eq!(painted_text_logical(&frame, &lit), OLAM);

        let sel = term.text_selection_mut();
        sel.start_selection(0, 2, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 4, SelectionSide::Right);
        sel.complete_selection();
        let frame = term.cell_frame(4, 30);
        let lit = painted(&frame);
        assert_eq!(lit, vec![2, 3, 12]);
        assert_eq!(
            painted_text_logical(&frame, &lit),
            term.selection_to_string().unwrap_or_default()
        );

        let mut ltr = Terminal::new(4, 30);
        ltr.process(b"abc hello world def");
        let sel = ltr.text_selection_mut();
        sel.start_selection(0, 4, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 8, SelectionSide::Right);
        sel.complete_selection();
        let frame = ltr.cell_frame(4, 30);
        assert!(frame.visual_to_logical.iter().all(Vec::is_empty));
        assert_eq!(painted(&frame), vec![4, 5, 6, 7, 8]);
        assert_eq!(ltr.selection_to_string().as_deref(), Some("hello"));
    }

    #[test]
    fn a_refill_drops_the_map_of_a_row_that_no_longer_reorders() {
        let mut term = Terminal::new(4, 30);
        term.process(MIXED.as_bytes());
        let mut scratch = crate::render::RenderInput::empty();
        term.cell_frame_into(&mut scratch, 4, 30);
        assert!(!scratch.visual_to_logical_row(0).is_empty());
        term.process(b"\r\x1b[2Kabc hello world def");
        term.cell_frame_into(&mut scratch, 4, 30);
        assert!(scratch.visual_to_logical.iter().all(Vec::is_empty));
        assert_eq!(scratch.logical_col(0, 5), 5);
    }

    #[test]
    fn viewport_cell_logical_follows_the_painted_order() {
        let mut term = Terminal::new(4, 30);
        term.process(MIXED.as_bytes());
        assert_eq!(term.viewport_cell_logical(0, 1), (1, false), "LTR cell");
        assert_eq!(term.viewport_cell_logical(0, 4), (12, true));
        assert_eq!(term.viewport_cell_logical(0, 5), (11, true));
        assert_eq!(term.viewport_cell_logical(0, 12), (4, true));
        assert_eq!(
            term.viewport_cell_logical(0, 8),
            (8, true),
            "the blank between the words joins their RTL run (UAX #9 N1)"
        );
        assert_eq!(
            term.viewport_cell_logical(0, 13),
            (13, false),
            "the blank before `def` resolves to the LTR paragraph"
        );
        assert_eq!(
            term.viewport_cell_logical(0, 25),
            (25, false),
            "past the text"
        );
        assert_eq!(term.viewport_cell_logical(1, 3), (3, false), "a blank row");

        term.process(b"\r\n\r\n\r\n\r\n");
        term.scroll_display(1);
        assert_eq!(term.grid().display_offset(), 1);
        let frame = term.cell_frame(4, 30);
        let row: String = frame.cells[0].iter().map(|c| c.ch).collect();
        assert!(
            row.starts_with("abc \u{05DD}\u{05DC}\u{05D5}\u{05E2}"),
            "{row:?}"
        );
        assert_eq!(term.viewport_cell_logical(0, 5), (11, true));

        let mut off = Terminal::new(4, 30);
        off.process(MIXED.as_bytes());
        off.modes.bidi_mode = BiDiMode::Disabled;
        assert_eq!(off.viewport_cell_logical(0, 5), (5, false));
        assert!(
            off.cell_frame(4, 30)
                .visual_to_logical
                .iter()
                .all(Vec::is_empty)
        );
    }

    #[test]
    fn a_number_inside_an_rtl_run_pins_the_maps_direction() {
        const ROW: &str = "abc \u{05D0}\u{05D1} 12 \u{05D2} def";
        let mut term = Terminal::new(4, 30);
        term.process(ROW.as_bytes());
        let frame = term.cell_frame(4, 30);
        let shown: String = frame.cells[0][..15].iter().map(|c| c.ch).collect();
        assert_eq!(shown, "abc \u{05D2} 12 \u{05D1}\u{05D0} def");
        assert_eq!(
            frame.visual_to_logical_row(0)[..15],
            [0, 1, 2, 3, 10, 9, 7, 8, 6, 5, 4, 11, 12, 13, 14],
            "visual→logical, not its inverse ([.., 10, 9, 8, 6, 7, 5, 4, ..])"
        );
        for v in 0..30 {
            assert_eq!(
                frame.visual_col(0, frame.logical_col(0, v)),
                v,
                "visual {v}"
            );
        }

        let sel = term.text_selection_mut();
        sel.start_selection(0, 7, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(0, 8, SelectionSide::Right);
        sel.complete_selection();
        assert_eq!(term.selection_to_string().as_deref(), Some("12"));
        let frame = term.cell_frame(4, 30);
        let lit = painted(&frame);
        assert_eq!(lit, vec![6, 7]);
        assert_eq!(painted_text_logical(&frame, &lit), "12");

        assert_eq!(term.viewport_cell_logical(0, 6), (7, false));
        assert_eq!(term.viewport_cell_logical(0, 7), (8, false));
        assert_eq!(term.viewport_cell_logical(0, 8), (6, true));
        assert_eq!(term.viewport_cell_logical(0, 4), (10, true), "ג");
        assert_eq!(term.viewport_cell_logical(0, 10), (4, true), "א");
    }

    fn row0(term: &mut Terminal, rows: usize, cols: usize, n: usize) -> String {
        term.cell_frame(rows, cols).cells[0]
            .iter()
            .take(n)
            .map(|c| c.ch)
            .collect()
    }

    const LS_ROW: &str = "\u{05D0}\u{05D1}.txt  foo.txt  bar.txt";
    const LS_ROW_LTR: &str = "\u{05D1}\u{05D0}.txt  foo.txt  bar.txt";

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

        term.process(b"\x1b[?2501h");
        assert!(
            term.has_damage(),
            "toggling 2501 must damage the presented grid"
        );
        let frame = term.cell_frame(2, 32);
        let full: String = frame.cells[0].iter().map(|c| c.ch).collect();
        assert_mirrored(full.trim_start());
        term.process(b"\x1b[?2501$p");
        assert_eq!(term.take_response().unwrap_or_default(), b"\x1b[?2501;1$y");

        term.process(b"\x1b[?2501l");
        assert_eq!(row0(&mut term, 2, 32, 24), LS_ROW_LTR);
    }

    #[test]
    fn scp_zero_is_the_terminal_default_not_autodetect() {
        let mut term = Terminal::new(2, 32);
        term.process(LS_ROW.as_bytes());
        term.process(b"\x1b[2 k");
        let frame = term.cell_frame(2, 32);
        let full: String = frame.cells[0].iter().map(|c| c.ch).collect();
        assert_mirrored(full.trim_start());
        term.process(b"\x1b[1 k");
        let ltr = row0(&mut term, 2, 32, 24);
        assert_eq!(ltr, LS_ROW_LTR);
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
                        let order = compute_visual_order_cells(
                            BiDiMode::Implicit,
                            ParagraphDirection::Auto,
                            false,
                            &scalars,
                            &vec![false; scalars.len()],
                        );
                        for (l, &c) in scalars.iter().enumerate() {
                            if c.is_ascii() && c != ' ' {
                                assert_eq!(
                                    order[l], l,
                                    "{row:?}: ASCII cell {l} ({c:?}) moved; order={order:?}"
                                );
                            }
                        }
                        let rtl_first = format!("{rtl}{blanks}{lead}{blanks}{trail}");
                        let scalars: Vec<char> = rtl_first.chars().collect();
                        let order = compute_visual_order_cells(
                            BiDiMode::Implicit,
                            ParagraphDirection::Auto,
                            true,
                            &scalars,
                            &vec![false; scalars.len()],
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
