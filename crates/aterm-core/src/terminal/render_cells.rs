// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Render-ready per-cell extraction for CPU/GPU rasterizers.

use super::Terminal;
use super::color_resolve::{StyleResolveOpts, resolve_colors_raw_opts};
use crate::grid::{Cell, CellFlags, Grid};
use crate::render::reuse_frame_extra;

#[cfg(clean_islands)]
macro_rules! trust_island {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! trust_island {
    ($($tt:tt)*) => {};
}

trust_island! {
    clean {
        namespace RenderCellsProofs

        -- Abstract palette digest cache state: liveDigest is the true 256-slot
        -- digest and cachedDigest is the memoized slot (0 = uncached).
        structure PaletteCache where
          liveDigest : Nat
          cachedDigest : Nat
        deriving Repr

        -- Every mutable palette borrow invalidates cachedDigest to 0 while
        -- installing the new live digest.
        def mutatePalette (s : PaletteCache) (newDigest : Nat) : PaletteCache :=
          { s with liveDigest := newDigest, cachedDigest := 0 }

        -- Reading the palette digest fills cachedDigest from liveDigest when
        -- uncached (0), and returns the coherent digest.
        def refreshPaletteCache (s : PaletteCache) : PaletteCache :=
          { s with cachedDigest := s.liveDigest }

        -- Theorem: mutating the palette and refreshing the cache always yields a
        -- cached digest equal to the new live palette digest.
        theorem mutate_then_refresh_is_coherent (s : PaletteCache) (d : Nat) :
            (refreshPaletteCache (mutatePalette s d)).cachedDigest =
              (mutatePalette s d).liveDigest := by
          rfl

        -- Theorem: a freshly mutated palette has its cache slot cleared (0).
        theorem mutate_clears_cached_digest (s : PaletteCache) (d : Nat) :
            (mutatePalette s d).cachedDigest = 0 := by
          rfl

        -- Consecutive plain cells with identical (PackedColors, CellFlags) key
        -- resolve to identical (fg, bg, underline, flags) under the same row
        -- palette and style policy, so memoizing the last plain key is exact.
        def resolvePlainCellStyle (key memoKey memoVal freshVal : Nat) : Nat :=
          if Nat.beq key memoKey then memoVal else freshVal

        theorem plain_style_memo_hit_is_exact (val : Nat) :
            resolvePlainCellStyle 1 1 val val = val := by
          rfl

        theorem plain_style_memo_miss_recomputes (memoVal freshVal : Nat) :
            resolvePlainCellStyle 2 1 memoVal freshVal = freshVal := by
          rfl

        -- Single-pass combining-mark scan computes (has_any, has_vs15,
        -- has_emoji_marker, utf8_bytes) in one traversal instead of cloning
        -- the mark iterator 4 times per complex/styled cell.
        structure MarkScan where
          hasAny : Bool
          hasVs15 : Bool
          hasEmojiMarker : Bool
          utf8Bytes : Nat
        deriving Repr

        def emptyMarkScan : MarkScan :=
          { hasAny := false, hasVs15 := false, hasEmojiMarker := false, utf8Bytes := 0 }

        theorem empty_mark_scan_has_none :
            emptyMarkScan.hasAny = false := by
          rfl

        end RenderCellsProofs
    }
}

type ExtrasOut<'a> = (
    &'a mut Vec<(usize, Box<str>)>,
    &'a mut Vec<(usize, Box<[char]>)>,
);

/// The line-decoration style under a cell (SGR 4 / 4:n / 21).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UnderlineStyle {
    /// No underline.
    #[default]
    None,
    /// SGR 4 — a single straight line.
    Single,
    /// SGR 21 / 4:2 — two stacked straight lines.
    Double,
    /// SGR 4:3 — a wavy line (editors' squiggle for diagnostics).
    Curly,
    /// SGR 4:4 — a dotted line.
    Dotted,
    /// SGR 4:5 — a dashed line.
    Dashed,
}

impl UnderlineStyle {
    /// Resolve the packed underline bits to a single variant.
    #[inline]
    const fn from_flags(cflags: CellFlags) -> Self {
        if cflags.contains(CellFlags::DOTTED_UNDERLINE) {
            Self::Dotted
        } else if cflags.contains(CellFlags::DASHED_UNDERLINE) {
            Self::Dashed
        } else if cflags.contains(CellFlags::CURLY_UNDERLINE) {
            Self::Curly
        } else if cflags.contains(CellFlags::DOUBLE_UNDERLINE) {
            Self::Double
        } else if cflags.contains(CellFlags::UNDERLINE) {
            Self::Single
        } else {
            Self::None
        }
    }
}

/// A cell's vertical script: a smaller glyph set low in its cell (`Sub`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Script {
    /// The ordinary glyph.
    #[default]
    Normal,
    /// Subscript: small. On a colour emoji (an emoji-presented cell), the emoji drawn small and
    /// centred in its cell's rows ([`Script::small_emoji_rows`]), its nudge ignored.
    Sub {
        /// Sideways offset of the glyph, in [`Script::NUDGES_PER_CELL`]ths of a cell.
        nudge: i8,
    },
}

impl Script {
    /// The steps a cell is divided into for [`Script::Sub`]'s `nudge`.
    pub const NUDGES_PER_CELL: i8 = 16;

    /// The rows a small colour emoji ([`Script::Sub`] on an emoji-presented cell) takes in a cell
    /// `cell_h` rows tall, `(top, rows)`: seven tenths of the cell, rounded, centred on its
    /// middle, where a rule's stroke runs (20 rows from row 4 in a 28-row cell). The renderer
    /// draws the emoji inside them; chrome that rings it reads them too.
    #[must_use]
    pub const fn small_emoji_rows(cell_h: usize) -> (usize, usize) {
        let rows = (cell_h / 10) * 7 + ((cell_h % 10) * 7 + 5) / 10;
        ((cell_h - rows) / 2, rows)
    }
}

/// A single render-ready terminal cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent SGR/geometry rendition flag the renderer reads directly; a bitfield would obscure the public render API"
)]
pub struct RenderCell {
    /// The character to draw (`' '` for empty / NUL cells).
    pub ch: char,
    /// Final foreground color as `[r, g, b]`.
    pub fg: [u8; 3],
    /// Final background color as `[r, g, b]`.
    pub bg: [u8; 3],
    /// True when this column is the right half (continuation) of a wide glyph.
    pub wide: bool,
    /// True when this lead cell requested emoji presentation.
    pub emoji_presentation: bool,
    /// True when this lead cell carries an explicit VS15 (U+FE0E) request and is narrow.
    pub text_presentation: bool,
    /// SGR 1 bold: the renderer rasterizes the glyph with extra stroke weight.
    pub bold: bool,
    /// SGR 3 italic: the renderer rasterizes the glyph with a synthetic slant.
    pub italic: bool,
    /// Underline decoration (SGR 4 family).
    pub underline: UnderlineStyle,
    /// Strikethrough (SGR 9): a line through the cell middle, in `fg`.
    pub strikethrough: bool,
    /// Overline (SGR 53).
    pub overline: bool,
    /// SGR 58 underline colour, when set; otherwise the underline uses `fg`.
    pub underline_color: Option<[u8; 3]>,
    /// Overline colour, when set; otherwise the overline uses `fg`.
    pub overline_color: Option<[u8; 3]>,
    /// The glyph's vertical script ([`Script`]).
    pub script: Script,
}

impl Default for RenderCell {
    #[inline]
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: [0, 0, 0],
            bg: [0, 0, 0],
            wide: false,
            emoji_presentation: false,
            text_presentation: false,
            bold: false,
            italic: false,
            underline: UnderlineStyle::None,
            strikethrough: false,
            overline: false,
            underline_color: None,
            overline_color: None,
            script: Script::Normal,
        }
    }
}

/// The least WCAG contrast between a glyph's ink and its ground at which
/// [`Terminal::screen_row_echo_cells_into`] counts the glyph shown.
pub const ECHO_MIN_CONTRAST: f32 = 1.5;

type PlainStyleKey = (crate::grid::PackedColors, CellFlags, bool);

type PlainStyle = ([u8; 3], [u8; 3], UnderlineStyle, bool, bool, bool, bool);

impl Terminal {
    /// Resolve the render-ready cell for an unmaterialized grid column.
    #[must_use]
    pub fn implicit_blank_render_cell(&self) -> RenderCell {
        let (fg, bg) = super::color_resolve::resolve_colors(
            &Cell::EMPTY,
            None,
            self.color_palette(),
            self.default_foreground(),
            self.default_background(),
            self.modes().reverse_video(),
        );
        RenderCell {
            fg: [fg.r, fg.g, fg.b],
            bg: [bg.r, bg.g, bg.b],
            ..RenderCell::default()
        }
    }

    /// Resolve a visible row into render-ready cells.
    #[must_use]
    #[inline]
    pub fn render_row(&self, row: usize) -> Vec<RenderCell> {
        let mut out = Vec::new();
        self.render_row_into(row, &mut out);
        out
    }

    /// Like [`render_row`](Self::render_row), but fills a caller-owned `out` (cleared first).
    #[inline]
    pub fn render_row_into(&self, row: usize, out: &mut Vec<RenderCell>) {
        self.render_row_into_impl(row, out, None, false);
    }

    /// The visible `row` as [`cell_frame_into`](Self::cell_frame_into) extracts it, before BiDi
    /// reordering.
    pub fn logical_row_into(
        &self,
        row: usize,
        cells: &mut Vec<RenderCell>,
        clusters: &mut Vec<(usize, Box<str>)>,
        combining: &mut Vec<(usize, Box<[char]>)>,
    ) -> bool {
        self.render_row_into_impl(row, cells, Some((clusters, combining)), false);
        let single_width = u16::try_from(row)
            .ok()
            .and_then(|vr| self.grid().row(vr))
            .is_none_or(|r| r.line_size() == crate::grid::LineSize::SingleWidth);
        let reordered = self.modes.bidi_mode != aterm_types::BiDiMode::Disabled
            && cells.iter().any(|c| c.ch >= '\u{0590}')
            && self
                .bidi_visual_order_cells(cells)
                .iter()
                .enumerate()
                .any(|(visual, &logical)| visual != logical);
        single_width && !reordered
    }

    /// Live-frame twin of [`render_row`](Self::render_row).
    #[must_use]
    #[inline]
    pub fn render_row_at_screen(&self, screen_row: usize) -> Vec<RenderCell> {
        let mut out = Vec::new();
        self.render_row_at_screen_into(screen_row, &mut out);
        out
    }

    /// Like [`render_row_at_screen`](Self::render_row_at_screen), but fills a caller-owned
    /// `out` (cleared first).
    #[inline]
    pub fn render_row_at_screen_into(&self, screen_row: usize, out: &mut Vec<RenderCell>) {
        self.render_row_into_impl(screen_row, out, None, true);
    }

    /// The live screen row `row` as an echo proof reads it, whatever the viewport shows (scroll
    /// does not move it, as [`Self::print_anchor_glyph`] reads): `chars` gets every column's
    /// char in [`Self::row_cols_into`]'s convention (`'\0'` at a wide continuation, `' '` for a
    /// blank) and `visible` whether that column's glyph is shown. A glyph is shown when it is not
    /// concealed (SGR 8), carries no combining mark and no image, and both the colours its program
    /// asked for (resolved through the live palette, no contrast floor) and the colours it is
    /// drawn in contrast by at least [`ECHO_MIN_CONTRAST`]. Both vectors are cleared first and left
    /// empty for a row past the screen; `drawn` is scratch.
    pub fn screen_row_echo_cells_into(
        &self,
        row: u16,
        chars: &mut Vec<char>,
        visible: &mut Vec<bool>,
        drawn: &mut Vec<RenderCell>,
    ) {
        chars.clear();
        visible.clear();
        if row >= self.grid.rows() {
            drawn.clear();
            return;
        }
        self.render_row_at_screen_into(usize::from(row), drawn);
        let blank = self.implicit_blank_render_cell();
        let view = self.grid.screen_row_view(row);
        let (palette, fg0, bg0) = (
            self.color_palette(),
            self.default_foreground(),
            self.default_background(),
        );
        let reverse = self.modes().reverse_video();
        let rgb = |c: [u8; 3]| aterm_types::Rgb {
            r: c[0],
            g: c[1],
            b: c[2],
        };
        let cols = self.grid.cols();
        chars.reserve(usize::from(cols));
        visible.reserve(usize::from(cols));
        for col in 0..cols {
            chars.push(view.col_char(col));
            let shown = drawn.get(usize::from(col)).unwrap_or(&blank);
            let drawn_contrast = super::color_resolve::contrast_ratio(rgb(shown.fg), rgb(shown.bg));
            let asked = match view.cell(col) {
                None => true,
                Some(cell) => {
                    let extra = view.cell_data(col, cell).cell_extra();
                    let (fg, bg) = super::color_resolve::resolve_colors(
                        &cell, extra, palette, fg0, bg0, reverse,
                    );
                    !cell.flags().contains(CellFlags::HIDDEN)
                        && extra.is_none_or(|e| e.combining().is_empty() && e.images().is_empty())
                        && super::color_resolve::contrast_ratio(fg, bg) >= ECHO_MIN_CONTRAST
                }
            };
            visible.push(asked && drawn_contrast >= ECHO_MIN_CONTRAST);
        }
    }

    fn render_row_into_impl(
        &self,
        row: usize,
        out: &mut Vec<RenderCell>,
        mut extras_out: Option<ExtrasOut<'_>>,
        screen: bool,
    ) {
        out.clear();
        let Ok(visible_row) = u16::try_from(row) else {
            if let Some((clusters, combining)) = extras_out.as_mut() {
                clusters.clear();
                combining.clear();
            }
            return;
        };
        let grid = self.grid();
        let view = if screen {
            grid.screen_row_view(visible_row)
        } else {
            grid.visible_row_view(visible_row)
        };
        self.render_view_into(&view, out, extras_out);
    }

    /// The HISTORY row `rev_idx` lines above the live screen (0 = the newest scrolled-off line)
    /// into render-ready cells.
    pub fn render_history_row_into(&self, rev_idx: usize, out: &mut Vec<RenderCell>) {
        out.clear();
        let view = self.grid().history_row_view(rev_idx);
        self.render_view_into(&view, out, None);
    }

    /// The per-cell resolution pass of [`render_row_into_impl`](Self::render_row_into_impl) over
    /// one resolved row `view`.
    #[allow(
        clippy::too_many_lines,
        reason = "single per-cell resolution pass (colors + decorations + emoji/combining extraction) over a row"
    )]
    fn render_view_into(
        &self,
        view: &aterm_grid::VisibleRowView<'_>,
        out: &mut Vec<RenderCell>,
        mut extras_out: Option<ExtrasOut<'_>>,
    ) {
        let palette = self.color_palette();
        let default_fg = self.default_foreground();
        let default_bg = self.default_background();
        let reverse_video = self.modes().reverse_video();
        let style_opts = StyleResolveOpts {
            bold_is_bright: self.color.bold_is_bright,
            faint_opacity: self.color.faint_opacity,
            indexed_minimum_contrast: self.color.indexed_minimum_contrast,
        };

        let cells = view.cells_slice();
        let (mut clusters_written, mut combining_written) = (0, 0);
        out.reserve(cells.len());
        let mut prev_wide = false;
        let mut plain_style_memo: Option<(PlainStyleKey, PlainStyle)> = None;
        for (col, &cell) in (0..view.len()).zip(cells) {
            let data = view.cell_data(col, cell);

            if !cell.is_complex()
                && !cell.fg_needs_overflow()
                && !cell.bg_needs_overflow()
                && data.cell_extra().is_none()
            {
                let key = (
                    cell.colors(),
                    cell.flags(),
                    super::color_resolve::is_shape_glyph(cell.char()),
                );
                let (fg, bg, underline, strikethrough, overline, bold, italic) =
                    match plain_style_memo {
                        Some((k, resolved)) if k == key => resolved,
                        _ => {
                            let (fg_c, bg_c) = resolve_colors_raw_opts(
                                &cell,
                                None,
                                None,
                                palette,
                                default_fg,
                                default_bg,
                                reverse_video,
                                style_opts,
                            );
                            let cflags = key.1;
                            let resolved = (
                                [fg_c.r, fg_c.g, fg_c.b],
                                [bg_c.r, bg_c.g, bg_c.b],
                                UnderlineStyle::from_flags(cflags),
                                cflags.contains(CellFlags::STRIKETHROUGH),
                                cflags.contains(CellFlags::OVERLINE),
                                cflags.contains(CellFlags::BOLD),
                                cflags.contains(CellFlags::ITALIC),
                            );
                            plain_style_memo = Some((key, resolved));
                            resolved
                        }
                    };
                let is_wide = cell.is_wide();
                let wide = cell.is_wide_continuation() && !is_wide && prev_wide;
                prev_wide = is_wide;
                let raw = cell.char();
                let ch = if wide || raw == '\0' { ' ' } else { raw };
                let emoji_presentation =
                    !wide && is_wide && super::handler::is_vs16_emoji_capable(ch);
                out.push(RenderCell {
                    ch,
                    fg,
                    bg,
                    wide,
                    emoji_presentation,
                    text_presentation: false,
                    bold,
                    italic,
                    underline,
                    strikethrough,
                    overline,
                    underline_color: None,
                    overline_color: None,
                    script: Script::Normal,
                });
                continue;
            }

            let (fg, bg) = resolve_colors_raw_opts(
                &cell,
                data.fg_rgb(),
                data.bg_rgb(),
                palette,
                default_fg,
                default_bg,
                reverse_video,
                style_opts,
            );

            let wide = cell.is_wide_continuation() && !cell.is_wide() && prev_wide;
            prev_wide = cell.is_wide();
            let raw = if cell.is_complex() {
                data.complex_base().unwrap_or('\u{FFFD}')
            } else {
                cell.char()
            };
            let ch = if wide || raw == '\0' { ' ' } else { raw };

            let marks = data.marks();
            let mut has_any_mark = false;
            let mut has_vs15 = false;
            let mut has_emoji_marker = false;
            let mut marks_utf8_bytes = 0usize;
            for m in marks.clone() {
                has_any_mark = true;
                if m == '\u{FE0E}' {
                    has_vs15 = true;
                }
                if is_emoji_sequence_marker(m) {
                    has_emoji_marker = true;
                }
                marks_utf8_bytes += m.len_utf8();
            }

            let emoji_presentation =
                !wide && cell.is_wide() && super::handler::is_vs16_emoji_capable(ch);
            let text_presentation = !wide && !cell.is_wide() && has_vs15;

            let cflags = cell.flags();
            let underline = UnderlineStyle::from_flags(cflags);
            let strikethrough = cflags.contains(CellFlags::STRIKETHROUGH);
            let overline = cflags.contains(CellFlags::OVERLINE);
            let bold = cflags.contains(CellFlags::BOLD);
            let italic = cflags.contains(CellFlags::ITALIC);
            let extra = data.cell_extra();
            let underline_color = if underline == UnderlineStyle::None {
                None
            } else {
                extra.and_then(|e| {
                    e.underline_color().or_else(|| {
                        e.underline_color_index().map(|i| {
                            let c = palette.get(i);
                            [c.r, c.g, c.b]
                        })
                    })
                })
            };
            let underline = if underline == UnderlineStyle::None
                && extra.is_some_and(|extra| extra.hyperlink().is_some())
            {
                UnderlineStyle::Single
            } else {
                underline
            };

            out.push(RenderCell {
                ch,
                fg: [fg.r, fg.g, fg.b],
                bg: [bg.r, bg.g, bg.b],
                wide,
                emoji_presentation,
                text_presentation,
                bold,
                italic,
                underline,
                strikethrough,
                overline,
                underline_color,
                overline_color: None,
                script: Script::Normal,
            });

            if let Some((clusters, combining_out)) = extras_out.as_mut() {
                if has_emoji_marker {
                    // Emoji sequence: surface the whole grapheme for shaping. Skip a NUL base.
                    if raw != '\0' {
                        let bytes = raw.len_utf8() + marks_utf8_bytes;
                        let compare_marks = marks.clone();
                        reuse_frame_extra(
                            clusters,
                            &mut clusters_written,
                            col as usize,
                            |old| old.chars().eq(std::iter::once(raw).chain(compare_marks)),
                            || {
                                let mut s = String::with_capacity(bytes);
                                s.push(raw);
                                s.extend(marks);
                                s.into_boxed_str()
                            },
                        );
                    }
                } else if has_any_mark {
                    let mut overlay = marks.filter(|&c| !draws_no_glyph(c)).peekable();
                    if overlay.peek().is_some() {
                        let compare_marks = overlay.clone();
                        reuse_frame_extra(
                            combining_out,
                            &mut combining_written,
                            col as usize,
                            |old| old.iter().copied().eq(compare_marks),
                            || overlay.collect(),
                        );
                    }
                }
            }
        }
        if let Some((clusters, combining)) = extras_out {
            clusters.truncate(clusters_written);
            combining.truncate(combining_written);
        }
    }

    /// Inline-image placements for the visible `row`, sparse.
    #[must_use]
    pub fn images_row(&self, row: usize) -> Vec<(usize, aterm_grid::ImageRef)> {
        let mut out = Vec::new();
        self.images_row_into(row, &mut out);
        out
    }

    /// Like [`images_row`](Self::images_row), but fills a caller-owned `out` (cleared first).
    pub(crate) fn images_row_into(&self, row: usize, out: &mut Vec<(usize, aterm_grid::ImageRef)>) {
        out.clear();
        let Ok(visible_row) = u16::try_from(row) else {
            return;
        };
        let grid = self.grid();
        let offset = grid.display_offset();
        if usize::from(visible_row) < offset {
            push_history_images(grid, visible_row, out);
            out.sort_by_key(|&(col, _)| col);
            return;
        }
        let has_kitty = !self.transient.kitty_images.is_empty();
        if grid.extras().is_empty()
            || (!has_kitty
                && self.transient.kitty_floating.is_empty()
                && !grid.extras().any_image())
        {
            return;
        }
        let screen_row = visible_row.saturating_sub(u16::try_from(offset).unwrap_or(u16::MAX));
        if screen_row >= grid.rows() {
            return;
        }
        for col in 0..grid.cols() {
            let Some(extra) = grid.cell_extra(screen_row, col) else {
                continue;
            };
            let layers = extra.images();
            if !layers.is_empty() {
                out.extend(layers.iter().map(|image| (col as usize, image.clone())));
            } else if has_kitty
                && let Some(iref) = self.placeholder_image_ref(screen_row, col, extra)
            {
                out.push((col as usize, iref));
            }
        }
        if !self.transient.kitty_floating.is_empty() {
            out.extend(
                self.kitty_floating_tiles(screen_row..screen_row.saturating_add(1))
                    .map(|(_, col, tile)| (usize::from(col), tile)),
            );
            order_image_layers(out);
        }
    }

    /// Every visible row's inline-image placements in one pass over the extras map.
    #[must_use]
    pub fn images_frame(&self, rows: usize) -> Vec<Vec<(usize, aterm_grid::ImageRef)>> {
        let mut images = Vec::new();
        self.images_frame_into(&mut images, rows);
        images
    }

    /// The live screen's inline-image placements.
    #[must_use]
    pub fn live_images_frame(&self, rows: usize) -> Vec<Vec<(usize, aterm_grid::ImageRef)>> {
        let mut images = Vec::new();
        self.images_frame_of(&mut images, rows, ImageRows::Live);
        images
    }

    fn images_frame_into(&self, images: &mut Vec<Vec<(usize, aterm_grid::ImageRef)>>, rows: usize) {
        self.images_frame_of(images, rows, ImageRows::Viewport);
    }

    fn images_frame_of(
        &self,
        images: &mut Vec<Vec<(usize, aterm_grid::ImageRef)>>,
        rows: usize,
        frame: ImageRows,
    ) {
        images.resize_with(rows, Vec::new);
        for row in images.iter_mut() {
            row.clear();
        }
        let grid = self.grid();
        let offset = match frame {
            ImageRows::Viewport => grid.display_offset(),
            ImageRows::Live => 0,
        };
        for (r, images_row) in images.iter_mut().enumerate().take(rows.min(offset)) {
            let Ok(visible_row) = u16::try_from(r) else {
                break;
            };
            push_history_images(grid, visible_row, images_row);
        }
        let has_kitty = !self.transient.kitty_images.is_empty();
        if grid.extras().is_empty()
            || (!has_kitty
                && self.transient.kitty_floating.is_empty()
                && !grid.extras().any_image())
        {
            for row in images.iter_mut().take(rows.min(offset)) {
                if row.len() > 1 {
                    row.sort_by_key(|&(col, _)| col);
                }
            }
            return;
        }
        let max_screen_row = usize::from(grid.rows());
        if !self.transient.kitty_floating.is_empty() {
            let screen_rows = u16::try_from(rows.saturating_sub(offset)).unwrap_or(u16::MAX);
            for (row, col, tile) in self.kitty_floating_tiles(0..screen_rows) {
                if let Some(r) = usize::from(row).checked_add(offset)
                    && r < rows
                {
                    images[r].push((usize::from(col), tile));
                }
            }
        }
        for (coord, extra) in grid.extras().iter() {
            #[cfg(test)]
            image_probe_meter::charge();
            let Some(r) = usize::from(coord.row).checked_add(offset) else {
                continue;
            };
            if usize::from(coord.row) >= max_screen_row || r >= rows || coord.col >= grid.cols() {
                continue;
            }
            let layers = extra.images();
            if !layers.is_empty() {
                images[r].extend(
                    layers
                        .iter()
                        .map(|image| (usize::from(coord.col), image.clone())),
                );
            } else if has_kitty
                && let Some(iref) = self.placeholder_image_ref(coord.row, coord.col, extra)
            {
                images[r].push((usize::from(coord.col), iref));
            }
        }
        let floating = !self.transient.kitty_floating.is_empty();
        for row in images.iter_mut() {
            if row.len() > 1 {
                if floating {
                    order_image_layers(row);
                } else {
                    row.sort_by_key(|&(col, _)| col);
                }
            }
        }
    }

    fn placeholder_image_ref(
        &self,
        row: u16,
        col: u16,
        extra: &aterm_grid::CellExtra,
    ) -> Option<aterm_grid::ImageRef> {
        let cell = super::kitty_placeholder::decode_cell(self.grid(), row, col, extra)?;
        let image = self.kitty_placeholder_image(cell.image_id, cell.placement_id)?;
        Some(aterm_grid::ImageRef {
            image: std::sync::Arc::clone(image),
            cell_row: cell.row,
            cell_col: cell.col,
            kitty: None,
        })
    }

    fn kitty_placeholder_image(
        &self,
        image_id: u32,
        placement_id: u32,
    ) -> Option<&std::sync::Arc<aterm_grid::ImageData>> {
        let stored = self.transient.kitty_images.get(&image_id)?;
        match super::kitty_floating::placeholder_virtual(&self.transient, image_id, placement_id) {
            Some(v) => Some(&v.image),
            None => (placement_id == 0).then_some(stored),
        }
    }

    fn kitty_floating_tiles(
        &self,
        rows: std::ops::Range<u16>,
    ) -> impl Iterator<Item = (u16, u16, aterm_grid::ImageRef)> + '_ {
        let floating = &self.transient.kitty_floating;
        super::kitty_floating::floating_tiles(self.grid(), &self.transient, rows).map(|tile| {
            let placement = &floating[tile.index];
            let image = aterm_grid::ImageRef {
                image: std::sync::Arc::clone(&placement.image),
                cell_row: tile.cell_row,
                cell_col: tile.cell_col,
                kitty: Some(placement.tag),
            };
            (tile.row, tile.col, image)
        })
    }

    /// Build the engine's render snapshot for one frame.
    #[must_use]
    pub fn cell_frame(&mut self, rows: usize, cols: usize) -> crate::render::RenderInput {
        let mut scratch = crate::render::RenderInput::empty();
        self.cell_frame_into(&mut scratch, rows, cols);
        scratch
    }

    /// Like [`cell_frame`](Self::cell_frame), but refills a caller-owned `scratch` in place.
    #[inline]
    pub fn cell_frame_into(
        &mut self,
        scratch: &mut crate::render::RenderInput,
        rows: usize,
        cols: usize,
    ) {
        self.cell_frame_fill::<true>(scratch, rows, cols, None);
    }

    /// Damage-scoped variant of [`cell_frame_into`](Self::cell_frame_into) that also consumes the
    /// damage session.
    pub fn cell_frame_damage_scoped_into(
        &mut self,
        scratch: &mut crate::render::RenderInput,
        rows: usize,
        cols: usize,
    ) -> crate::render::FrameRefill {
        let Some(cause) = self.damage_scoped_refill_refusal(scratch, rows, cols) else {
            let mut mask = std::mem::take(&mut self.refill_mask_scratch);
            mask.clear();
            mask.resize(rows.div_ceil(64), 0);
            let mut rows_refilled = 0usize;
            for r in self.grid().damage().damaged_rows(self.rows()) {
                let r = usize::from(r);
                if r < rows {
                    mask[r / 64] |= 1u64 << (r % 64);
                    rows_refilled += 1;
                }
            }
            self.cell_frame_fill::<true>(scratch, rows, cols, Some(&mask));
            self.refill_mask_scratch = mask;
            self.take_damage();
            scratch.extract_gen = self.extract_gen;
            return crate::render::FrameRefill::Scoped { rows_refilled };
        };
        if cause == crate::render::FullRefillCause::EngineScrolled
            && let Some(delta) = self.scrolled_scoped_refill_delta(scratch, rows)
        {
            let mut mask = std::mem::take(&mut self.refill_mask_scratch);
            Self::rotate_scrolled_scratch(scratch, rows, delta, &mut mask);
            self.cell_frame_fill::<true>(scratch, rows, cols, Some(&mask));
            self.refill_mask_scratch = mask;
            #[cfg(debug_assertions)]
            {
                let mut oracle = scratch.clone();
                self.cell_frame_fill::<true>(&mut oracle, rows, cols, None);
                debug_assert!(
                    *scratch == oracle,
                    "SCR-2: a rotated scratch diverged from a full extraction (delta {delta})"
                );
            }
            self.take_damage();
            scratch.extract_gen = self.extract_gen;
            return crate::render::FrameRefill::Scoped {
                rows_refilled: delta.unsigned_abs(),
            };
        }
        self.cell_frame_fill::<true>(scratch, rows, cols, None);
        self.take_damage();
        scratch.extract_gen = self.extract_gen;
        crate::render::FrameRefill::Full { cause }
    }

    fn damage_scoped_refill_refusal(
        &self,
        scratch: &crate::render::RenderInput,
        rows: usize,
        cols: usize,
    ) -> Option<crate::render::FullRefillCause> {
        use crate::render::FullRefillCause as Cause;
        if scratch.engine_row_order != crate::render::RowOrder::Logical {
            return Some(Cause::BidiVisual);
        }
        let grid = self.grid();
        let base_y = i64::try_from(grid.base_y()).unwrap_or(i64::MAX);
        if scratch.terminal_id == 0 {
            return Some(Cause::ScratchUnstamped);
        }
        if scratch.terminal_id != self.extract_identity {
            return Some(Cause::TerminalMismatch);
        }
        if scratch.extract_gen != self.extract_gen {
            return Some(Cause::DamageTaken);
        }
        if scratch.snapshot_seq != scratch.engine_fill_seq {
            return Some(Cause::HostMutation);
        }
        if scratch.row_shift != 0 {
            return Some(Cause::RowShift);
        }
        if scratch.engine_alt != self.is_alternate_screen() {
            return Some(Cause::AltScreen);
        }
        if scratch.rows != rows || scratch.cols != cols {
            return Some(Cause::ScratchRows);
        }
        if scratch.cells.len() != rows {
            return Some(Cause::ScratchRowCount);
        }
        if rows != usize::from(self.rows()) || cols != usize::from(self.cols()) {
            return Some(Cause::EngineDims);
        }
        if grid.display_offset() != 0 {
            return Some(Cause::EngineScrolled);
        }
        if scratch.display_offset != 0 {
            return Some(Cause::ScratchScrolled);
        }
        if scratch.base_y != base_y {
            return Some(Cause::BaseY);
        }
        if scratch.absolute_row_revision != self.absolute_row_revision() {
            return Some(Cause::RowRevision);
        }
        if grid.damage().is_full() {
            return Some(Cause::FullDamage);
        }
        None
    }

    fn scrolled_scoped_refill_delta(
        &self,
        scratch: &crate::render::RenderInput,
        rows: usize,
    ) -> Option<isize> {
        let grid = self.grid();
        if scratch.display_offset <= 0 {
            return None;
        }
        if scratch.content_seq != self.content_seq() {
            return None;
        }
        if scratch.history_renumber_epoch != grid.history_renumber_epoch() {
            return None;
        }
        if scratch.base_y != i64::try_from(grid.base_y()).unwrap_or(i64::MAX) {
            return None;
        }
        if scratch.absolute_row_revision != self.absolute_row_revision() {
            return None;
        }
        if grid.damage().is_full() {
            return None;
        }
        if scratch.clusters.len() != rows || scratch.combining.len() != rows {
            return None;
        }
        let delta = i64::try_from(grid.display_offset()).ok()? - i64::from(scratch.display_offset);
        if delta.unsigned_abs() >= u64::try_from(rows).ok()? {
            return None;
        }
        isize::try_from(delta).ok()
    }

    fn rotate_scrolled_scratch(
        scratch: &mut crate::render::RenderInput,
        rows: usize,
        delta: isize,
        mask: &mut Vec<u64>,
    ) {
        let n = delta.unsigned_abs();
        let exposed = match delta.cmp(&0) {
            std::cmp::Ordering::Greater => {
                scratch.cells.rotate_right(n);
                scratch.clusters.rotate_right(n);
                scratch.combining.rotate_right(n);
                0..n
            }
            std::cmp::Ordering::Less => {
                scratch.cells.rotate_left(n);
                scratch.clusters.rotate_left(n);
                scratch.combining.rotate_left(n);
                rows - n..rows
            }
            std::cmp::Ordering::Equal => 0..0,
        };
        mask.clear();
        mask.resize(rows.div_ceil(64), 0);
        for r in exposed {
            mask[r / 64] |= 1u64 << (r % 64);
        }
    }

    fn apron_row_into(&self, scratch: &mut crate::render::RenderInput, rows: usize) {
        let d = self.grid().display_offset();
        let cursor_col = (self.cursor_visible() && self.projected_cursor_row() == rows)
            .then(|| self.cursor().col as usize);
        let stamp = crate::render::ApronStamp {
            offset: d,
            content_gen: self.grid().content_gen(),
            renumber_epoch: self.grid().history_renumber_epoch(),
            rows,
            cols: scratch.cols,
            presentation: if d == 0 {
                0
            } else {
                self.presentation_fingerprint()
            },
            terminal: self.extract_identity,
        };
        if scratch.apron_stamp == Some(stamp) {
            // An absent apron (live bottom or a BiDi row) must stay absent.
            if scratch.apron_row.present {
                scratch.apron_row.cursor_col = cursor_col;
            }
            return;
        }
        scratch.apron_stamp = Some(stamp);
        let ap = &mut scratch.apron_row;
        if d == 0 || rows == 0 {
            ap.clear();
            return;
        }
        if rows >= d {
            let screen_row = rows - d;
            self.render_row_into_impl(
                screen_row,
                &mut ap.cells,
                Some((&mut ap.clusters, &mut ap.combining)),
                true,
            );
            ap.line_size = u16::try_from(screen_row)
                .ok()
                .and_then(|sr| self.grid().row_at_screen(sr))
                .map_or(
                    crate::grid::LineSize::SingleWidth,
                    crate::grid::Row::line_size,
                );
        } else {
            self.render_row_into_impl(
                rows,
                &mut ap.cells,
                Some((&mut ap.clusters, &mut ap.combining)),
                false,
            );
            ap.line_size = u16::try_from(rows)
                .ok()
                .and_then(|vr| self.grid().row_past_viewport(vr))
                .map_or(
                    crate::grid::LineSize::SingleWidth,
                    crate::grid::Row::line_size,
                );
        }
        if self.modes.bidi_mode != aterm_types::BiDiMode::Disabled
            && ap.cells.iter().any(|c| c.ch >= '\u{0590}')
            && self
                .bidi_visual_order_cells(&ap.cells)
                .iter()
                .enumerate()
                .any(|(visual, &logical)| visual != logical)
        {
            ap.clear();
            return;
        }
        ap.cursor_col = cursor_col;
        ap.present = true;
    }

    /// Everything a row is resolved against beside the grid.
    #[must_use]
    pub fn presentation_fingerprint(&self) -> u64 {
        use std::hash::Hasher as _;
        let mut h = aterm_hash::FxHasher::default();
        h.write_u64(self.color.palette_fingerprint());
        h.write_u32(pack_rgb(self.default_foreground()));
        h.write_u32(pack_rgb(self.default_background()));
        let modes = self.modes();
        h.write_u8(u8::from(modes.reverse_video()));
        h.write_u8(u8::from(self.color.bold_is_bright));
        h.write_u32(self.color.faint_opacity.to_bits());
        h.write_u32(self.color.indexed_minimum_contrast.to_bits());
        h.write_u8(modes.bidi_mode as u8);
        h.write_u8(modes.bidi_direction as u8);
        h.write_u8(u8::from(modes.bidi_autodetection));
        h.finish()
    }

    /// The shared fill behind [`cell_frame_into`](Self::cell_frame_into) (`refill_mask: None`,
    /// every row) and the damage-scoped arm (`Some(mask)`, only marked rows re-resolve).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "display_offset is a scrollback row count that fits i32 in practice; \
                  the snapshot field is i32 (viewport row = r - display_offset)"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one fill boundary stamps every RenderInput channel coherently"
    )]
    pub(super) fn cell_frame_fill<const PAINT: bool>(
        &mut self,
        scratch: &mut crate::render::RenderInput,
        rows: usize,
        cols: usize,
        refill_mask: Option<&[u64]>,
    ) {
        scratch.rows = rows;
        scratch.cols = cols;

        scratch.cells.resize_with(rows, Vec::new);
        if PAINT {
            scratch.clusters.resize_with(rows, Vec::new);
            scratch.combining.resize_with(rows, Vec::new);
        }
        for r in 0..rows {
            // Row granularity only.
            if let Some(mask) = refill_mask
                && mask.get(r / 64).is_none_or(|w| w & (1u64 << (r % 64)) == 0)
            {
                continue;
            }
            self.render_row_into_impl(
                r,
                &mut scratch.cells[r],
                if PAINT {
                    Some((&mut scratch.clusters[r], &mut scratch.combining[r]))
                } else {
                    None
                },
                false,
            );
        }
        if PAINT {
            self.apron_row_into(scratch, rows);

            self.images_frame_into(&mut scratch.images, rows);

            scratch.line_sizes.clear();
            scratch.line_sizes.extend((0..rows).map(|r| {
                u16::try_from(r)
                    .ok()
                    .and_then(|vr| self.grid().row(vr))
                    .map_or(
                        crate::grid::LineSize::SingleWidth,
                        crate::grid::Row::line_size,
                    )
            }));
            scratch.line_size_spans.resize_with(rows, Vec::new);
            for spans in &mut scratch.line_size_spans {
                spans.clear();
            }
            scratch.default_bg_spans.resize_with(rows, Vec::new);
            for spans in &mut scratch.default_bg_spans {
                spans.clear();
            }
        }
        scratch.selection_clip = None;
        scratch.selections.clear();
        if PAINT {
            scratch.retire_chrome_rasters();
        }

        let cur = self.cursor();
        scratch.cursor_col = cur.col as usize;
        let display_offset = self.grid().display_offset();
        scratch.cursor_row = self.projected_cursor_row();
        scratch.cursor_visible = self.cursor_row_on_screen(rows).is_some();
        scratch.cursor_style = self.cursor_style();
        scratch.display_offset = display_offset as i32;
        scratch.base_y = i64::try_from(self.grid().base_y()).unwrap_or(i64::MAX);
        scratch.absolute_row_revision = self.absolute_row_revision();
        scratch.history_renumber_epoch = self.grid().history_renumber_epoch();
        scratch.holder = self.holder_view();
        scratch.screen_mutations = self.screen_mutations();
        scratch.selection.clone_from(self.text_selection());
        scratch.selection_bg = self
            .selection_background()
            .map_or(crate::render::COLOR_UNSET, pack_rgb);
        scratch.selection_fg = self
            .selection_foreground()
            .map_or(crate::render::COLOR_DYNAMIC, pack_rgb);
        let reverse_video = self.modes().reverse_video();
        let implicit_blank = self.implicit_blank_render_cell();
        scratch.implicit_blank = implicit_blank;
        if self.color.frame_background_authoritative || reverse_video {
            let [r, g, b] = implicit_blank.bg;
            scratch.default_bg = u32::from_be_bytes([0, r, g, b]);
        } else {
            scratch.default_bg = crate::render::COLOR_UNSET;
        }
        if self.color.frame_foreground_authoritative || reverse_video {
            let [r, g, b] = implicit_blank.fg;
            scratch.default_fg = u32::from_be_bytes([0, r, g, b]);
        } else {
            scratch.default_fg = crate::render::COLOR_UNSET;
        }
        if self.color.frame_cursor_authoritative
            || (self.color.cursor_color.is_none() && self.color.frame_foreground_authoritative)
        {
            scratch.cursor_color = pack_rgb(
                self.cursor_color()
                    .unwrap_or_else(|| self.default_foreground()),
            );
        } else {
            scratch.cursor_color = crate::render::COLOR_UNSET;
        }

        scratch.snapshot_seq = self.damage_epoch();
        scratch.content_seq = self.content_seq();
        scratch.drawn_at = self.drawn_at();
        scratch.process_sequence = self.transient.pipeline_timestamps.process_sequence;

        scratch.terminal_id = self.extract_identity;
        scratch.extract_gen = self.extract_gen;
        scratch.engine_alt = self.is_alternate_screen();
        scratch.grid_instance = self.grid.instance();
        scratch.engine_fill_seq = scratch.snapshot_seq;
        scratch.row_shift = 0;
        scratch.shifted_fill_seq = scratch.snapshot_seq;
        scratch.composed_fill_seq = 0;

        if PAINT {
            scratch.row_rev.clear();
            {
                let revisions = self.grid.row_revisions();
                scratch.row_rev.extend_from_slice(revisions);
            }
            scratch.row_rev.resize(rows, 0);
            scratch.row_rev_lane = if self.grid().damage().is_full() || display_offset != 0 {
                0
            } else {
                self.extract_identity
            };
        }

        for map in &mut scratch.visual_to_logical {
            map.clear();
        }
        scratch.visual_to_logical.resize_with(rows, Vec::new);

        scratch.engine_row_order = if self.apply_bidi_reorder(scratch, refill_mask) {
            crate::render::RowOrder::BidiVisual
        } else {
            crate::render::RowOrder::Logical
        };
    }
}

/// A combining char that marks its cell as a multi-codepoint emoji sequence.
#[inline]
const fn is_emoji_sequence_marker(c: char) -> bool {
    matches!(
        c as u32,
        0x200D | 0x20E3 | 0x1F3FB..=0x1F3FF | 0x1F1E6..=0x1F1FF | 0xE0020..=0xE007F
    )
}

/// A combining char that attaches to its base but has no glyph of its own.
#[inline]
const fn draws_no_glyph(c: char) -> bool {
    matches!(c as u32, 0xFE0E | 0xFE0F | 0xE0000..=0xE0FFF)
}

/// `0x00RRGGBB`: the packing of the snapshot's colour scalars and the apron fingerprint.
#[inline]
const fn pack_rgb(c: aterm_types::Rgb) -> u32 {
    u32::from_be_bytes([0, c.r, c.g, c.b])
}

/// Which rows an image-placement fill describes.
#[derive(Clone, Copy)]
enum ImageRows {
    /// The viewport.
    Viewport,
    /// The live screen: row `s` is live screen row `s`, whatever the viewport shows.
    Live,
}

fn push_history_images(
    grid: &Grid,
    visible_row: u16,
    out: &mut Vec<(usize, aterm_grid::ImageRef)>,
) {
    if let aterm_grid::VisibleRowView::History { mat } = grid.visible_row_view(visible_row) {
        let cols = grid.cols();
        for (col, extra) in mat.extras_iter() {
            if col < cols {
                out.extend(
                    extra
                        .images()
                        .iter()
                        .map(|image| (usize::from(col), image.clone())),
                );
            }
        }
    }
}

fn order_image_layers(row: &mut Vec<(usize, aterm_grid::ImageRef)>) {
    row.sort_by_key(|(col, image)| (*col, image.draw_key()));
    let mut kept = 0;
    let mut start = 0;
    while start < row.len() {
        let col = row[start].0;
        let end = start + row[start..].partition_point(|(c, _)| *c == col);
        for at in end.saturating_sub(aterm_grid::MAX_IMAGE_LAYERS).max(start)..end {
            row.swap(kept, at);
            kept += 1;
        }
        start = end;
    }
    row.truncate(kept);
}

/// Deterministic cost meter for the batch inline-image reader.
#[cfg(test)]
#[path = "render_cells_image_probe_meter_tests.rs"]
mod image_probe_meter;

#[cfg(test)]
#[path = "render_cells_tests.rs"]
mod tests;
