// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Render-ready per-cell extraction for CPU/GPU rasterizers.

use super::Terminal;
use super::color_resolve::{StyleResolveOpts, resolve_colors_raw_opts};
use crate::grid::{Cell, CellFlags, Grid};

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

/// Out-params for the folded emoji-cluster + combining-mark extraction
/// (`render_row_into_impl`): `(clusters, combining)`, where each entry is the
/// cell column paired with its `(base+combining)` string / combining `char`s.
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
    /// Resolve the packed underline bits to a single variant. The composite styles
    /// share bits with the singletons (DOTTED = UNDERLINE|CURLY, DASHED =
    /// DOUBLE|CURLY), so they are tested before the singletons.
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
    /// Subscript: small, its foot on the cell's floor, and moved `nudge`
    /// [`Script::NUDGES_PER_CELL`]ths of a cell sideways (negative: left).
    Sub {
        /// Sideways offset of the glyph, in [`Script::NUDGES_PER_CELL`]ths
        /// of a cell.
        nudge: i8,
    },
}

impl Script {
    /// The steps a cell is divided into for [`Script::Sub`]'s `nudge`.
    pub const NUDGES_PER_CELL: i8 = 16;
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
    /// Overline (SGR 53): a line along the cell top, drawn in [`overline_color`](RenderCell::overline_color) (or [`fg`](RenderCell::fg)).
    pub overline: bool,
    /// SGR 58 underline colour, when set; otherwise the underline uses `fg`.
    pub underline_color: Option<[u8; 3]>,
    /// Overline colour, when set; otherwise the overline uses `fg`.
    pub overline_color: Option<[u8; 3]>,
    /// The glyph's vertical script ([`Script`]).
    pub script: Script,
}

/// A blank cell with `ch == ' '` (not `'\0'`), since renderers blit `ch`
/// unconditionally.
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

/// The plain-cell style memo's key: a plain cell's style depends on its colors and flags, and
/// on whether it holds a shape the theme-pair floor leaves alone
/// ([`super::color_resolve::is_shape_glyph`]).
type PlainStyleKey = (crate::grid::PackedColors, CellFlags, bool);

/// What the memo keeps per key: foreground, background, underline, and the
/// strikethrough, overline, bold and italic flags.
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

    /// Resolve a visible row into render-ready cells, one per stored column, colours fully resolved through [`color_resolve`](super::color_resolve).
    #[must_use]
    #[inline]
    pub fn render_row(&self, row: usize) -> Vec<RenderCell> {
        let mut out = Vec::new();
        self.render_row_into(row, &mut out);
        out
    }

    /// Like [`render_row`](Self::render_row), but fills a caller-owned `out` (cleared first) so a caller scanning many rows reuses one buffer.
    #[inline]
    pub fn render_row_into(&self, row: usize, out: &mut Vec<RenderCell>) {
        self.render_row_into_impl(row, out, None, false);
    }

    /// The visible `row` as [`cell_frame_into`](Self::cell_frame_into) extracts it, before BiDi reordering.
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

    /// Like [`render_row_at_screen`](Self::render_row_at_screen), but fills a caller-owned `out` (cleared first).
    #[inline]
    pub fn render_row_at_screen_into(&self, screen_row: usize, out: &mut Vec<RenderCell>) {
        self.render_row_into_impl(screen_row, out, None, true);
    }

    /// Core of [`render_row_into`](Self::render_row_into), with an optional `extras_out` sink
    /// for the emoji-cluster and combining-mark classification.
    fn render_row_into_impl(
        &self,
        row: usize,
        out: &mut Vec<RenderCell>,
        mut extras_out: Option<ExtrasOut<'_>>,
        screen: bool,
    ) {
        out.clear();
        if let Some((clusters, combining)) = extras_out.as_mut() {
            clusters.clear();
            combining.clear();
        }
        let Ok(visible_row) = u16::try_from(row) else {
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

    /// The HISTORY row `rev_idx` lines above the live screen (0 = the newest scrolled-off line) into render-ready cells, as [`render_row_at_screen_into`](Self::render_row_at_screen_into) resolves a live row and whatever the viewport shows.
    pub fn render_history_row_into(&self, rev_idx: usize, out: &mut Vec<RenderCell>) {
        out.clear();
        let view = self.grid().history_row_view(rev_idx);
        self.render_view_into(&view, out, None);
    }

    /// The per-cell resolution pass of [`render_row_into_impl`](Self::render_row_into_impl)
    /// over one resolved row `view` (`out` and `extras_out` cleared by the caller).
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
                    // Multi-codepoint emoji sequence (ZWJ, skin tone, keycap,
                    // regional-indicator pair): surface the whole grapheme for
                    // shaping. Skip a NUL base.
                    if raw != '\0' {
                        let bytes = raw.len_utf8() + marks_utf8_bytes;
                        let mut s = String::with_capacity(bytes);
                        s.push(raw);
                        s.extend(marks);
                        clusters.push((col as usize, s.into_boxed_str()));
                    }
                } else if has_any_mark {
                    let overlay: Box<[char]> = marks.filter(|&c| !draws_no_glyph(c)).collect();
                    if !overlay.is_empty() {
                        combining_out.push((col as usize, overlay));
                    }
                }
            }
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
        if grid.extras().is_empty() {
            return;
        }
        let screen_row = visible_row.saturating_sub(u16::try_from(offset).unwrap_or(u16::MAX));
        if screen_row >= grid.rows() {
            return;
        }
        let has_kitty = !self.transient.kitty_images.is_empty();
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
                // Kitty Unicode placeholder cell: synthesize an ImageRef so it rides
                // the same (pixel-tested) render path as a direct placement.
                out.push((col as usize, iref));
            }
        }
        if !self.transient.kitty_floating.is_empty() {
            out.extend(
                self.kitty_floating_tiles()
                    .into_iter()
                    .filter(|&(row, ..)| row == screen_row)
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

    /// The live screen's inline-image placements, row `r` being live screen row `r` whatever the viewport shows.
    #[must_use]
    pub fn live_images_frame(&self, rows: usize) -> Vec<Vec<(usize, aterm_grid::ImageRef)>> {
        let mut images = Vec::new();
        self.images_frame_of(&mut images, rows, ImageRows::Live);
        images
    }

    /// Batch equivalent of [`images_row_into`](Self::images_row_into) for every
    /// row in `0..rows` of the current viewport.
    fn images_frame_into(&self, images: &mut Vec<Vec<(usize, aterm_grid::ImageRef)>>, rows: usize) {
        self.images_frame_of(images, rows, ImageRows::Viewport);
    }

    /// The image placements of rows `0..rows` of `frame`, driven by one pass
    /// over the extras map instead of `rows x cols` probes, so the cost is
    /// proportional to the extras present.
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
        if grid.extras().is_empty() {
            for row in images.iter_mut().take(rows.min(offset)) {
                if row.len() > 1 {
                    row.sort_by_key(|&(col, _)| col);
                }
            }
            return;
        }
        let max_screen_row = usize::from(grid.rows());
        let has_kitty = !self.transient.kitty_images.is_empty();
        if !self.transient.kitty_floating.is_empty() {
            for (row, col, tile) in self.kitty_floating_tiles() {
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
                // Kitty Unicode placeholder cell: synthesize an ImageRef so it
                // rides the same (pixel-tested) render path as a direct placement.
                images[r].push((usize::from(coord.col), iref));
            }
        }
        for row in images.iter_mut() {
            if row.len() > 1 {
                row.sort_by_key(|&(col, _)| col);
            }
        }
        if !self.transient.kitty_floating.is_empty() {
            for row in images.iter_mut() {
                if row.len() > 1 {
                    order_image_layers(row);
                }
            }
        }
    }

    /// If the cell at (`row`,`col`) is a Kitty Unicode placeholder (U+10EEEE), decode it
    /// ([`decode_cell`](super::kitty_placeholder::decode_cell)) and return an
    /// [`ImageRef`](aterm_grid::ImageRef) into the image its virtual placement shows, so a
    /// virtual placement reuses the direct-placement compositor.
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

    /// The image a placeholder naming image `image_id` and virtual placement
    /// `placement_id` draws: that placement's (laid out by its `U=1` put); for
    /// placement `0`, the image's newest virtual placement — kitty lets the
    /// terminal choose — or, when it has none, the stored image as its transmit
    /// laid it out.
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

    /// The tiles of the Kitty placements relative to a virtual placement
    /// ([`kitty_floating`](super::kitty_floating)), each where it shows now:
    /// `(screen row, col, tile)` for every footprint cell on screen.
    fn kitty_floating_tiles(&self) -> Vec<(u16, u16, aterm_grid::ImageRef)> {
        let floating = &self.transient.kitty_floating;
        super::kitty_floating::floating_tiles(self.grid(), &self.transient)
            .into_iter()
            .map(|tile| {
                let placement = &floating[tile.index];
                let image = aterm_grid::ImageRef {
                    image: std::sync::Arc::clone(&placement.image),
                    cell_row: tile.cell_row,
                    cell_col: tile.cell_col,
                    kitty: Some(placement.tag),
                };
                (tile.row, tile.col, image)
            })
            .collect()
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
        self.cell_frame_fill(scratch, rows, cols, None);
    }

    /// Damage-scoped variant of [`cell_frame_into`](Self::cell_frame_into) that also consumes the damage session.
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
            self.cell_frame_fill(scratch, rows, cols, Some(&mask));
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
            self.cell_frame_fill(scratch, rows, cols, Some(&mask));
            self.refill_mask_scratch = mask;
            #[cfg(debug_assertions)]
            {
                let mut oracle = scratch.clone();
                self.cell_frame_fill(&mut oracle, rows, cols, None);
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
        self.cell_frame_fill(scratch, rows, cols, None);
        self.take_damage();
        scratch.extract_gen = self.extract_gen;
        crate::render::FrameRefill::Full { cause }
    }

    /// The damage-scoped continuity check (clauses justified on
    /// [`cell_frame_damage_scoped_into`](Self::cell_frame_damage_scoped_into)).
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

    /// The scrolled continuity proof: may a scratch filled while parked in
    /// history be carried to the engine's current offset by rotating its rows
    /// and re-resolving only the exposed strip?
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

    /// Carry a parked scratch to a new offset: rotate its three retained
    /// per-row channels (`cells`, `clusters`, `combining`; every other per-row
    /// channel is rewritten each fill) and write the exposed strip's refill
    /// mask into `mask` (the resident `refill_mask_scratch`).
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

    /// Fill `scratch.apron_row` with the row just below a `rows`-tall viewport:
    /// the apron at offset `d` is the bottom viewport row at offset `d - 1`,
    /// which a sub-row up-glide slides in.
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

    /// Everything a row is resolved against beside the grid, none of which moves `content_gen` (their mutators only mark `Damage::Full`, and a config reload changes the style policy with no output at all).
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

    /// The shared fill behind [`cell_frame_into`](Self::cell_frame_into)
    /// (`refill_mask: None`, every row) and the damage-scoped arm
    /// (`Some(mask)`, only marked rows re-resolve).
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
    fn cell_frame_fill(
        &mut self,
        scratch: &mut crate::render::RenderInput,
        rows: usize,
        cols: usize,
        refill_mask: Option<&[u64]>,
    ) {
        scratch.rows = rows;
        scratch.cols = cols;

        scratch.cells.resize_with(rows, Vec::new);
        scratch.clusters.resize_with(rows, Vec::new);
        scratch.combining.resize_with(rows, Vec::new);
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
                Some((&mut scratch.clusters[r], &mut scratch.combining[r])),
                false,
            );
        }
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
        scratch.selection_clip = None;
        scratch.selections.clear();
        scratch.chrome_rasters.clear();

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
            // A pristine, unconfigured `Terminal::new`'s VT black is not a host
            // theme decision; any host/OSC background or DECSCNM makes the
            // terminal authoritative.
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
        scratch.process_sequence = self.transient.pipeline_timestamps.process_sequence;

        scratch.terminal_id = self.extract_identity;
        scratch.extract_gen = self.extract_gen;
        scratch.engine_alt = self.is_alternate_screen();
        scratch.engine_fill_seq = scratch.snapshot_seq;
        scratch.row_shift = 0;
        scratch.shifted_fill_seq = scratch.snapshot_seq;
        scratch.composed_fill_seq = 0;

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

/// A combining char that marks its cell as a multi-codepoint emoji sequence:
/// ZWJ (U+200D), a skin-tone modifier (U+1F3FB–U+1F3FF), combining enclosing
/// keycap (U+20E3), a regional indicator (U+1F1E6–U+1F1FF, the second half of
/// a flag pair), or a tag character (U+E0020–U+E007F, a 🏴 subdivision flag).
#[inline]
const fn is_emoji_sequence_marker(c: char) -> bool {
    matches!(
        c as u32,
        0x200D | 0x20E3 | 0x1F3FB..=0x1F3FF | 0x1F1E6..=0x1F1FF | 0xE0020..=0xE007F
    )
}

/// A combining char that attaches to its base but has no glyph of its own, so it must never
/// reach the CPU/GPU mark loops: they blit whatever `glyph_key` returns for a mark, which for
/// one of these is the primary's `.notdef` box over the base.
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
    /// The viewport: rows `0..display_offset` are history (read through the
    /// grid's own viewport) and live screen row `s` is row `s + display_offset`.
    Viewport,
    /// The live screen: row `s` is live screen row `s`, whatever the viewport
    /// shows.
    Live,
}

/// A scrolled-back viewport row's direct image placements, in `extras_iter`
/// order (callers sort), expanded from the history line's image spans.
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

/// Order one row's image list by column and, within a cell, by draw order
/// ([`ImageRef::draw_key`](aterm_grid::ImageRef::draw_key)) — stable, so the
/// layers a cell already holds in that order keep it — keeping each cell's
/// top [`MAX_IMAGE_LAYERS`](aterm_grid::MAX_IMAGE_LAYERS), as a cell's own
/// stack does.
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

/// Deterministic cost meter for the batch inline-image reader (the shape of
/// `aterm_grid::test_counters`): a complexity claim is asserted as an operation count, not
/// wall-clock.
#[cfg(test)]
mod image_probe_meter {
    thread_local! {
        static PROBES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// One extras lookup.
    pub(super) fn charge() {
        PROBES.with(|c| c.set(c.get() + 1));
    }

    /// Read and reset, so each measured stretch starts from zero.
    pub(super) fn take() -> usize {
        PROBES.with(|c| {
            let v = c.get();
            c.set(0);
            v
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `logical_row_into` is the frame's own extraction of the row: its cells,
    /// clusters and combining marks equal `cell_frame_into`'s for a row the frame
    /// leaves in logical order, and it says when the frame does not: a DEC double
    /// width line, and (with BiDi) a row it reorders.
    #[test]
    fn a_logical_row_is_the_frames_row_and_says_when_the_frame_moves_it() {
        let mut term = Terminal::new(4, 12);
        term.process("$ e\u{301}x 👍🏽\r\n\x1b#6wide\r\n".as_bytes());
        term.process("$ \u{5d0}\u{5d1} ab".as_bytes());
        let mut frame = crate::render::RenderInput::empty();
        term.cell_frame_into(&mut frame, 4, 12);
        let (mut cells, mut clusters, mut combining) = (Vec::new(), Vec::new(), Vec::new());
        assert!(term.logical_row_into(0, &mut cells, &mut clusters, &mut combining));
        assert_eq!(Some(&cells), frame.cells.first());
        assert_eq!(Some(&clusters), frame.clusters.first());
        assert_eq!(Some(&combining), frame.combining.first());
        assert!(
            !clusters.is_empty() && !combining.is_empty(),
            "fixture: marks on row 0"
        );
        assert!(
            !term.logical_row_into(1, &mut cells, &mut clusters, &mut combining),
            "a DEC double-width line is drawn at another size"
        );
        assert!(
            !term.logical_row_into(2, &mut cells, &mut clusters, &mut combining),
            "a reordered row is drawn in another order"
        );
        assert!(
            term.logical_row_into(3, &mut cells, &mut clusters, &mut combining),
            "an empty row is logical"
        );
        assert!(cells.is_empty() && clusters.is_empty() && combining.is_empty());
    }

    /// OSC 8 hyperlinked cells render underlined, unless the program chose its
    /// own decoration.
    #[test]
    fn a_hyperlink_is_underlined_and_a_program_decoration_still_wins() {
        let mut term = Terminal::new(2, 32);
        term.process(b"go\x1b]8;;https://evil.example/steal\x1b\\ogle\x1b]8;;\x1b\\.com");
        let row = term.render_row(0);
        let marked: String = row
            .iter()
            .take(10)
            .map(|cell| match cell.underline {
                UnderlineStyle::Single => '_',
                UnderlineStyle::None => ' ',
                _ => '?',
            })
            .collect();
        assert_eq!(
            marked, "  ____    ",
            "exactly the linked cells carry the mark"
        );
        assert!(
            row.iter()
                .take(10)
                .all(|cell| cell.underline_color.is_none()),
            "a link underline must not invent a colour"
        );

        let mut styled = Terminal::new(2, 32);
        styled.process(b"\x1b[4:3m\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\\x1b[m");
        assert!(
            styled
                .render_row(0)
                .iter()
                .take(4)
                .all(|cell| cell.underline == UnderlineStyle::Curly),
            "the program's curly underline must survive the link mark"
        );
    }

    /// A scratch reused across a split and an unsplit must not carry the split
    /// compositor's per-pane spans or selections into a direct snapshot.
    #[test]
    fn cell_frame_into_clears_compositor_pane_spans() {
        use crate::render::{DefaultBgSpan, LineSizeSpan, PaneSelection, SelectionClip};
        let mut term = Terminal::new(2, 8);
        let mut scratch = crate::render::RenderInput::empty();
        scratch.line_size_spans =
            vec![vec![LineSizeSpan::new(0, 4, crate::grid::LineSize::DoubleWidth)]; 3];
        scratch.default_bg_spans = vec![vec![DefaultBgSpan::new(0, 4, 0x0011_2233)]; 3];
        scratch.selection_clip = Some(SelectionClip::new(0, 2, 5, 8));
        scratch.selections = vec![PaneSelection {
            selection: crate::selection::TextSelection::new(),
            clip: SelectionClip::new(0, 2, 5, 8),
            bg: 0x0001_0203,
            fg: 0x0004_0506,
            inactive: true,
        }];

        term.cell_frame_into(&mut scratch, 2, 8);

        assert_eq!(scratch.line_size_spans, vec![Vec::new(); 2]);
        assert_eq!(scratch.default_bg_spans, vec![Vec::new(); 2]);
        assert_eq!(scratch.selection_clip, None);
        assert!(
            scratch.selections.is_empty(),
            "the per-pane list would REPLACE the scalar selection"
        );
    }

    #[test]
    fn cell_frame_into_tracks_live_selection_colors_and_resets() {
        let mut term = Terminal::new(2, 8);
        let configured_bg = aterm_types::Rgb::new(0x10, 0x20, 0x30);
        let configured_fg = aterm_types::Rgb::new(0x40, 0x50, 0x60);
        term.set_default_selection_background(Some(configured_bg));
        term.set_default_selection_foreground(Some(configured_fg));
        let mut scratch = crate::render::RenderInput::empty();

        term.process(b"\x1b]17;rgb:aaaa/bbbb/cccc\x1b\\");
        term.process(b"\x1b]19;rgb:1111/2222/3333\x1b\\");
        term.cell_frame_into(&mut scratch, 2, 8);
        assert_eq!(scratch.selection_bg, 0x00aa_bbcc);
        assert_eq!(scratch.selection_fg, 0x0011_2233);

        term.process(b"\x1b]117\x07\x1b]119\x07");
        term.cell_frame_into(&mut scratch, 2, 8);
        assert_eq!(scratch.selection_bg, 0x0010_2030);
        assert_eq!(scratch.selection_fg, 0x0040_5060);
    }

    #[test]
    fn cell_frame_owns_live_sparse_background_and_cursor_colors() {
        let packed = |color: aterm_types::Rgb| {
            (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
        };
        let mut pristine = Terminal::new(2, 8);
        let initial = pristine.cell_frame(2, 8);
        assert_eq!(
            initial.default_bg,
            crate::render::COLOR_UNSET,
            "an unconfigured raw terminal preserves the renderer-theme fallback",
        );
        assert_eq!(
            initial.cursor_color,
            crate::render::COLOR_UNSET,
            "an unconfigured raw terminal preserves the renderer cursor fallback",
        );
        pristine.process(b"\x1b[?5h");
        assert_eq!(
            pristine.cell_frame(2, 8).default_bg,
            packed(aterm_types::DEFAULT_FOREGROUND),
            "DECSCNM alone makes even the raw terminal's effective blank authoritative",
        );

        let mut protocol_only = Terminal::new(2, 8);
        protocol_only.process(b"\x1b]11;rgb:12/34/56\x1b\\\x1b]12;rgb:de/ad/be\x1b\\");
        let protocol_live = protocol_only.cell_frame(2, 8);
        assert_eq!(protocol_live.default_bg, 0x0012_3456);
        assert_eq!(protocol_live.cursor_color, 0x00de_adbe);
        protocol_only.process(b"\x1b]111\x07\x1b]112\x07");
        let protocol_reset = protocol_only.cell_frame(2, 8);
        assert_eq!(
            protocol_reset.default_bg,
            packed(aterm_types::DEFAULT_BACKGROUND),
            "OSC 111 exposes the raw terminal's configured black reset baseline",
        );
        assert_eq!(
            protocol_reset.cursor_color,
            packed(aterm_types::DEFAULT_FOREGROUND),
            "OSC 112 restores the raw terminal's dynamic foreground cursor baseline",
        );

        let mut term = Terminal::new(2, 8);
        let configured_fg = aterm_types::Rgb::new(0x11, 0x22, 0x33);
        let configured_bg = aterm_types::Rgb::new(0x44, 0x55, 0x66);
        let configured_cursor = aterm_types::Rgb::new(0x77, 0x88, 0x99);
        term.set_default_foreground(configured_fg);
        term.set_default_background(configured_bg);
        term.set_default_cursor_color(Some(configured_cursor));

        let configured = term.cell_frame(2, 8);
        assert_eq!(configured.default_bg, 0x0044_5566);
        assert_eq!(configured.cursor_color, 0x0077_8899);
        assert!(
            configured.cells.iter().all(Vec::is_empty),
            "the color contract must cover genuinely sparse rows, not only painted cells",
        );

        term.process(
            b"\x1b]10;rgb:aa/bb/cc\x1b\\\
              \x1b]11;rgb:12/34/56\x1b\\\
              \x1b]12;rgb:de/ad/be\x1b\\",
        );
        let live = term.cell_frame(2, 8);
        assert_eq!(live.default_bg, 0x0012_3456);
        assert_eq!(live.cursor_color, 0x00de_adbe);

        let mut scratch = crate::render::RenderInput::empty();
        scratch.default_bg = 0x0001_0203;
        scratch.cursor_color = 0x0004_0506;
        term.process(b"\x1b[?5h");
        term.cell_frame_into(&mut scratch, 2, 8);
        assert_eq!(
            scratch.default_bg, 0x00aa_bbcc,
            "DECSCNM makes the live default foreground the implicit blank background",
        );
        assert_eq!(
            scratch.cursor_color, 0x00de_adbe,
            "DECSCNM does not replace an explicit OSC 12 cursor color",
        );
        assert_eq!(scratch.implicit_blank, term.implicit_blank_render_cell());
        assert_eq!(
            (scratch.implicit_blank.fg, scratch.implicit_blank.bg),
            ([0x12, 0x34, 0x56], [0xaa, 0xbb, 0xcc])
        );
        let mut reused = crate::render::RenderInput::empty();
        reused.clone_from(&scratch);
        assert_eq!(reused.implicit_blank, scratch.implicit_blank);

        term.process(b"\x1b]21;cursor=\x1b\\\x1b]10;rgb:fe/01/7f\x1b\\");
        let dynamic = term.cell_frame(2, 8);
        assert_eq!(dynamic.default_bg, 0x00fe_017f);
        assert_eq!(dynamic.cursor_color, 0x00fe_017f);

        term.process(b"\x1b[?5l\x1b]110\x07\x1b]111\x07\x1b]112\x07");
        term.cell_frame_into(&mut scratch, 2, 8);
        assert_eq!(scratch.default_bg, 0x0044_5566);
        assert_eq!(scratch.cursor_color, 0x0077_8899);
    }

    /// The frame carries both row-identity stamps from its extraction
    /// boundary: the footer revision a protected-footer splice moves, and the
    /// renumber epoch a width reflow moves without moving the revision.
    #[test]
    fn cell_frame_stamps_row_revision_and_history_renumber_epoch() {
        let mut spliced = Terminal::new(5, 10);
        spliced.process(b"\x1b[1;1HA\x1b[1;3r\x1b[3;1H\r\nX\x1b[r");
        assert_eq!(spliced.absolute_row_revision(), 1);
        assert_eq!(spliced.cell_frame(5, 10).absolute_row_revision, 1);

        let mut term = Terminal::new(5, 10);
        for line in 0..8 {
            term.process(format!("line{line:02}-{}\r\n", "x".repeat(14)).as_bytes());
        }
        let before = term.cell_frame(5, 10);
        let epoch_before = term.grid().history_renumber_epoch();
        assert_eq!(
            before.history_renumber_epoch, epoch_before,
            "snapshot carries the grid's epoch from its extraction boundary"
        );
        let revision_before = term.absolute_row_revision();

        term.resize(5, 7);
        assert!(
            term.grid().history_renumber_epoch() > epoch_before,
            "fixture: a width rewrap renumbers retained history"
        );
        assert_eq!(
            term.absolute_row_revision(),
            revision_before,
            "…and the footer revision, the stamp that used to be the only gate, \
             cannot see it"
        );

        let after = term.cell_frame(5, 7);
        assert_eq!(
            after.history_renumber_epoch,
            term.grid().history_renumber_epoch(),
            "the post-reflow frame carries the post-reflow epoch"
        );
        assert_ne!(
            after.history_renumber_epoch, before.history_renumber_epoch,
            "so a host comparing the two frames sees the renumbering"
        );
    }

    /// `images_frame_into` against the per-row `images_row` reference for
    /// `rows` rows: same columns, same image, same tile.
    fn assert_images_parity(term: &Terminal, rows: usize, what: &str) -> usize {
        let mut batch: Vec<Vec<(usize, aterm_grid::ImageRef)>> = Vec::new();
        term.images_frame_into(&mut batch, rows);
        assert_eq!(batch.len(), rows, "{what}");
        let mut total = 0usize;
        for (r, got) in batch.iter().enumerate() {
            let want = term.images_row(r);
            assert_eq!(got.len(), want.len(), "{what}: row {r} entry count");
            for ((gc, gi), (wc, wi)) in got.iter().zip(&want) {
                assert_eq!(gc, wc, "{what}: row {r} column order");
                assert!(
                    std::sync::Arc::ptr_eq(&gi.image, &wi.image),
                    "{what}: row {r} image identity"
                );
                assert_eq!(
                    (gi.cell_row, gi.cell_col),
                    (wi.cell_row, wi.cell_col),
                    "{what}: row {r} tile coordinates"
                );
            }
            total += got.len();
        }
        total
    }

    /// The batch `images_frame_into` fill is identical to the per-row `images_row` for every
    /// row: a direct iTerm2 placement, a Kitty placeholder, non-image extras that emit nothing,
    /// a scrolled extras map with stale entries, a `rows` request past the grid, and a viewport
    /// scrolled back over the image (the batch walks the history row's extras, the reference
    /// probes).
    #[test]
    fn images_frame_into_matches_per_row() {
        let mut term = Terminal::new(10, 30);
        term.set_cell_pixel_size(10, 20);

        term.process(b"\x1b]8;;http://example.com\x1b\\L\x1b]8;;\x1b\\");
        term.process(b"\x1b[4m\x1b[58;2;10;20;30mU\x1b[0m\r\n");

        let b64 = aterm_codec::base64::encode(b"not-a-real-png").expect("encode");
        term.process(format!("\x1b]1337;File=inline=1;width=3;height=2:{b64}\x1b\\").as_bytes());

        let raw = aterm_codec::base64::encode(&[0u8; 10 * 20 * 4]).expect("encode");
        term.process(format!("\x1b_Ga=t,f=32,s=10,v=20,i=5;{raw}\x1b\\").as_bytes());
        term.process("\x1b[38;5;5m\u{10EEEE}\u{0305}\u{0305}\x1b[0m".as_bytes());

        term.process(b"\x1b[10;1H\r\n\r\n");
        let rows = usize::from(term.grid().rows()) + 5;
        assert!(assert_images_parity(&term, rows, "live") > 0, "non-vacuous");

        for i in 0..30 {
            term.process(format!("f{i}\r\n").as_bytes());
        }
        term.scroll_to_top();
        assert!(
            assert_images_parity(&term, usize::from(term.grid().rows()), "scrolled back") > 0,
            "the scrolled-back footprint is present in both readers"
        );
    }

    /// The live reader keys its rows by live screen row whatever the viewport
    /// shows.
    #[test]
    fn live_images_frame_ignores_the_viewport() {
        use super::super::kitty_placeholder::PLACEHOLDER;
        let mut term = Terminal::new(8, 20);
        term.set_cell_pixel_size(10, 20);
        for i in 0..12 {
            term.process(format!("h{i}\r\n").as_bytes());
        }
        let b64 = aterm_codec::base64::encode(b"not-a-real-png").expect("encode");
        let mut seq = b"\x1b[4;1H\x1b]1337;File=inline=1;width=3;height=1:".to_vec();
        seq.extend_from_slice(b64.as_bytes());
        seq.extend_from_slice(b"\x1b\\");
        term.process(&seq);
        let raw = vec![0u8; 10 * 20 * 4];
        let mut apc = b"\x1b_Ga=t,f=32,s=10,v=20,q=2,i=5;".to_vec();
        apc.extend_from_slice(
            aterm_codec::base64::encode(&raw)
                .expect("encode")
                .as_bytes(),
        );
        apc.extend_from_slice(b"\x1b\\");
        term.process(&apc);
        let mut ph = b"\x1b[5;15H\x1b[38;5;5m".to_vec();
        let mut buf = [0u8; 4];
        for c in [PLACEHOLDER, '\u{0305}', '\u{0305}'] {
            ph.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        ph.extend_from_slice(b"\x1b[0m");
        term.process(&ph);
        let rows = usize::from(term.grid().rows());
        let placed = |frame: Vec<Vec<(usize, aterm_grid::ImageRef)>>| -> Vec<(usize, usize)> {
            frame
                .iter()
                .enumerate()
                .flat_map(|(r, row)| row.iter().map(move |&(c, _)| (r, c)))
                .collect()
        };
        let live = placed(term.live_images_frame(rows));
        assert_eq!(
            live,
            vec![(3, 0), (3, 1), (3, 2), (4, 14)],
            "the premise: the image sits on live row 3 and the placeholder at live (4, 14)"
        );
        assert_eq!(
            placed(term.images_frame(rows)),
            live,
            "at the live viewport the two readers agree"
        );

        term.scroll_display(2);
        assert_eq!(
            term.grid().display_offset(),
            2,
            "the premise: the viewport moved"
        );
        assert_ne!(
            term.grid().resolved_char(4, 14),
            Some(PLACEHOLDER),
            "the premise: the viewport row over live row 4 holds no placeholder"
        );
        assert_eq!(
            placed(term.live_images_frame(rows)),
            live,
            "the live frame does not follow the viewport"
        );
        let viewport = vec![(5, 0), (5, 1), (5, 2), (6, 14)];
        assert_eq!(
            placed(term.images_frame(rows)),
            viewport,
            "the viewport frame does"
        );
        assert_eq!(
            placed((0..rows).map(|r| term.images_row(r)).collect()),
            viewport,
            "and so does the per-row viewport reader"
        );
    }

    /// Cost as an operation count: a plain-text screen costs 0 probes, and
    /// with one hyperlink the batch reader pays one lookup per extra, not
    /// rows x cols.
    #[test]
    fn images_frame_costs_the_extras_not_rows_times_cols() {
        const ROWS: u16 = 40;
        const COLS: u16 = 120;
        let mut term = Terminal::new(ROWS, COLS);
        let rows = usize::from(ROWS);

        term.process(b"plain text, no extras at all\r\n");
        assert!(
            term.grid().extras().is_empty(),
            "no extras on a plain screen"
        );
        let _ = image_probe_meter::take();
        let _ = term.images_frame(rows);
        assert_eq!(
            image_probe_meter::take(),
            0,
            "the empty-map gate keeps the batch reader off a plain screen"
        );

        term.process(b"\x1b]8;;https://example.com\x1b\\L\x1b]8;;\x1b\\");
        let extras = term.grid().extras().len();
        assert_eq!(extras, 1, "one linked cell is one extras entry");
        let _ = image_probe_meter::take();
        let _ = term.images_frame(rows);
        assert_eq!(
            image_probe_meter::take(),
            extras,
            "the batch reader charges the extras that exist, not rows x cols"
        );
    }

    /// The presentation flags a render cell carries for the renderer's face
    /// choice: VS16 widens a text-default base and asks for colour, VS15
    /// narrows a default emoji and asks for text, and neither a bare
    /// text-default base nor a wide CJK ideograph asks for either.
    #[test]
    fn variation_selectors_set_the_presentation_flags() {
        for (text, emoji, mono, wide) in [
            ("\u{2764}\u{FE0F}", true, false, true),
            ("\u{1F600}\u{FE0E}", false, true, false),
            ("\u{2764}", false, false, false),
            ("\u{65E5}", false, false, true),
        ] {
            let mut term = Terminal::new(1, 8);
            term.process(text.as_bytes());
            let cells = term.render_row(0);
            assert!(!cells[0].wide, "{text:?}: the lead is not a continuation");
            assert_eq!(
                (cells[0].emoji_presentation, cells[0].text_presentation),
                (emoji, mono),
                "{text:?}: lead presentation"
            );
            assert_eq!(cells.get(1).is_some_and(|c| c.wide), wide, "{text:?}");
            if wide {
                assert!(
                    !cells[1].emoji_presentation && !cells[1].text_presentation,
                    "{text:?}: the continuation carries no flag"
                );
            }
        }
    }

    /// SGR underline styles, strikethrough and overline reach the render cell.
    #[test]
    fn sgr_decorations_surface_on_render_cells() {
        let mut term = Terminal::new(1, 16);
        term.process(
            b"\x1b[4mA\x1b[0m\x1b[21mB\x1b[0m\x1b[4:3mC\x1b[0m\x1b[9mD\x1b[0m\x1b[53mE\x1b[0m",
        );
        let cells = term.render_row(0);
        let decor = |c: &RenderCell| (c.underline, c.strikethrough, c.overline);
        assert_eq!(
            cells[..5].iter().map(decor).collect::<Vec<_>>(),
            [
                (UnderlineStyle::Single, false, false),
                (UnderlineStyle::Double, false, false),
                (UnderlineStyle::Curly, false, false),
                (UnderlineStyle::None, true, false),
                (UnderlineStyle::None, false, true),
            ]
        );
    }

    /// SGR 58:5:n (indexed underline colour) resolves against the live palette
    /// at extraction time.
    #[test]
    fn indexed_underline_color_resolves_against_live_palette() {
        let mut term = Terminal::new(2, 8);
        term.process(b"\x1b[4;58:5:1mU\x1b[0m");
        let cells = term.render_row(0);
        assert_eq!(cells[0].underline, UnderlineStyle::Single);
        let red = term.color_palette().get(1);
        assert_eq!(
            cells[0].underline_color,
            Some([red.r, red.g, red.b]),
            "SGR 58:5:1 resolves to palette entry 1"
        );
        let mut tc = crate::config::TerminalConfig::default();
        tc.allow_palette_reconfigure = true;
        term.apply_config(&tc);
        term.process(b"\x1b]4;1;rgb:12/34/56\x07");
        let cells = term.render_row(0);
        assert_eq!(
            cells[0].underline_color,
            Some([0x12, 0x34, 0x56]),
            "OSC 4 palette change must re-color the indexed underline"
        );
    }

    /// [`Terminal::presentation_fingerprint`] keys caches of resolved rows (the apron memo, the
    /// control socket's `screen since=<etag>`), so every input the resolver reads beside the
    /// grid must move it.
    #[test]
    fn presentation_fingerprint_moves_with_every_resolver_input() {
        use crate::config::TerminalConfig;
        type Arm = (&'static str, fn(&mut Terminal, &mut TerminalConfig));
        let mut tc = TerminalConfig {
            allow_palette_reconfigure: true,
            ..TerminalConfig::default()
        };
        let mut term = Terminal::new(2, 12);
        term.apply_config(&tc);
        term.process(b"\x1b[1;33mB\x1b[0m \x1b[2mD\x1b[0m \x1b[31mR\x1b[0m \x1b[34;42mG\x1b[0m");
        let arms: &[Arm] = &[
            ("bold_is_bright (config reload)", |t, c| {
                c.bold_is_bright = !c.bold_is_bright;
                t.apply_config(c);
            }),
            ("faint_opacity (config reload)", |t, c| {
                c.faint_opacity = 0.9;
                t.apply_config(c);
            }),
            ("indexed_minimum_contrast (config reload)", |t, c| {
                c.indexed_minimum_contrast = 21.0;
                t.apply_config(c);
            }),
            ("palette entry (OSC 4)", |t, _| {
                t.process(b"\x1b]4;1;#00ff00\x1b\\");
            }),
            ("default fg (OSC 10)", |t, _| {
                t.process(b"\x1b]10;#123456\x1b\\");
            }),
            ("default bg (OSC 11)", |t, _| {
                t.process(b"\x1b]11;#654321\x1b\\");
            }),
            ("reverse video (DECSCNM)", |t, _| t.process(b"\x1b[?5h")),
        ];
        for (name, arm) in arms {
            let (row, print) = (term.render_row(0), term.presentation_fingerprint());
            arm(&mut term, &mut tc);
            assert_ne!(
                term.render_row(0),
                row,
                "{name}: the premise: a resolved cell changed"
            );
            assert_ne!(
                term.presentation_fingerprint(),
                print,
                "{name}: re-coloured a resolved row under an unmoved fingerprint"
            );
        }
        let print = term.presentation_fingerprint();
        term.process(b"\x1b[2;1Hplain text");
        assert_eq!(
            term.presentation_fingerprint(),
            print,
            "writing cells is content, not presentation"
        );
    }

    /// Every non-plain grapheme lands in exactly one lane: an emoji sequence
    /// (ZWJ family, skin tone, keycap, regional-indicator flag, tag-sequence
    /// subdivision flag) is a shaping cluster at its lead column; a real
    /// diacritic run is an overlay holding every mark in arrival order; a VS16
    /// emoji, an unpaired third regional indicator and a default-ignorable mark
    /// (a lone tag, an ideographic variation selector) are neither.
    #[test]
    fn graphemes_split_into_emoji_clusters_and_diacritic_overlays() {
        const FAMILY: &str = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        const SKIN: &str = "\u{1F44D}\u{1F3FD}";
        const KEYCAP: &str = "1\u{FE0F}\u{20E3}";
        const ENGLAND: &str = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}";
        const US: &str = "\u{1F1FA}\u{1F1F8}";
        let mut term = Terminal::new(1, 48);
        term.process(
            format!(
                "{FAMILY} {SKIN} {KEYCAP} \u{2764}\u{FE0F} e\u{0301}\u{0302}\u{0323} \
                 {ENGLAND} {US}\u{1F1EB} A\u{E0001} \u{4E00}\u{E0100}x"
            )
            .as_bytes(),
        );
        let frame = term.cell_frame(1, 48);
        let (cells, clusters) = (&frame.cells[0], &frame.clusters[0]);
        assert_eq!(
            clusters.iter().map(|(_, s)| s.as_ref()).collect::<Vec<_>>(),
            [FAMILY, SKIN, KEYCAP, ENGLAND, US],
            "{clusters:?}"
        );
        for (col, cluster) in clusters {
            assert_eq!(
                Some(cells[*col].ch),
                cluster.chars().next(),
                "{cluster:?} sits at its lead column"
            );
        }
        let lone_ri = cells.iter().position(|c| c.ch == '\u{1F1EB}');
        assert!(
            lone_ri.is_some_and(|col| clusters.iter().all(|(c, _)| *c != col)),
            "the third regional indicator stands alone, not folded into the flag"
        );
        let overlays = &frame.combining[0];
        assert_eq!(
            overlays.len(),
            1,
            "only the diacritic overlays: {overlays:?}"
        );
        assert_eq!(cells[overlays[0].0].ch, 'e');
        assert_eq!(
            overlays[0].1.as_ref(),
            ['\u{0301}', '\u{0302}', '\u{0323}'],
            "every mark, in arrival order"
        );
    }

    #[test]
    fn render_row_protected_text_is_visible() {
        let mut term = Terminal::new(2, 8);
        term.process(b"\x1b[1\"qSECRET\x1b[0\"q");
        let cells = term.render_row(0);
        let text: String = cells.iter().take(6).map(|c| c.ch).collect();
        assert_eq!(text, "SECRET", "protected text must render, not blank");
        assert!(
            !cells[0].wide,
            "a protected cell is not a wide continuation"
        );
    }

    /// A rendition belongs to the character, not the column: the spacer of a
    /// wide glyph (CJK, or VS16-widened through its own spacer path) renders
    /// blank, flagged `wide`, in the lead's colours and rules, since the
    /// rasterizer spills the glyph's right half into it.
    #[test]
    fn a_wide_glyphs_spacer_carries_the_leads_rendition() {
        let look = |c: &RenderCell| (c.fg, c.bg, c.underline, c.strikethrough, c.overline);
        for (text, inverse) in [
            ("\x1b[7m\u{6F22}\x1b[0mZ", true),
            ("\x1b[31;44;7m\u{6F22}\x1b[0mZ", true),
            ("\x1b[7m\u{231A}\u{FE0F}\x1b[0mZ", true),
            ("\x1b[4;9;53m\u{6F22}\x1b[0mZ", false),
        ] {
            let mut term = Terminal::new(1, 8);
            term.process(text.as_bytes());
            let cells = term.render_row(0);
            assert!(
                cells[1].wide && cells[1].ch == ' ',
                "{text:?}: a blank spacer"
            );
            assert_eq!(look(&cells[1]), look(&cells[0]), "{text:?}");
            assert_eq!(cells[2].ch, 'Z', "{text:?}: Z follows the two cells");
            assert_ne!(look(&cells[2]), look(&cells[0]), "{text:?}: the run ended");
            if inverse {
                assert_ne!(cells[1].fg, cells[1].bg, "{text:?}");
            }
        }
    }

    /// The apron at offset `d` is the bottom viewport row at offset `d - 1`,
    /// for a shallow scroll (a live row), at the `d == rows` seam, and deeper
    /// than the frame (a history row); there is no apron at the live bottom.
    #[test]
    fn apron_row_is_the_bottom_row_one_offset_shallower() {
        let (rows, cols) = (6usize, 10usize);
        let seeded = || {
            let mut t = Terminal::new(rows as u16, cols as u16);
            for i in 0..30 {
                t.process(format!("line {i:02}\r\n").as_bytes());
            }
            t
        };
        let mut live = seeded();
        let at_live = live.cell_frame(rows, cols);
        assert_eq!(at_live.display_offset, 0);
        assert!(
            !at_live.apron_row.present,
            "no row lies below the live bottom"
        );
        assert!(at_live.apron_row.cells.is_empty());

        for d in [1i32, 3, 5, 6, 7, 20] {
            let mut t = seeded();
            t.scroll_display(d);
            let at_d = t.cell_frame(rows, cols);
            assert_eq!(at_d.display_offset, d, "fixture reaches offset {d}");
            assert!(
                at_d.apron_row.present,
                "d={d}: a row lies below the viewport"
            );
            t.scroll_display(-1);
            let at_d1 = t.cell_frame(rows, cols);
            assert_eq!(at_d1.display_offset, d - 1);
            assert_eq!(
                at_d.apron_row.cells,
                at_d1.cells[rows - 1],
                "d={d}: the apron is the bottom row one offset shallower"
            );
            assert_eq!(at_d.apron_row.clusters, at_d1.clusters[rows - 1], "d={d}");
            assert_eq!(at_d.apron_row.combining, at_d1.combining[rows - 1], "d={d}");
            assert_eq!(
                at_d.apron_row.line_size,
                at_d1.line_sizes[rows - 1],
                "d={d}"
            );
            assert_eq!(
                at_d.apron_row.cursor_col,
                (d == 1).then_some(0),
                "d={d}: the caret rides the apron only when its projected row is it"
            );
        }
        let mut t = seeded();
        t.scroll_display(2);
        let text: String = t
            .cell_frame(rows, cols)
            .apron_row
            .cells
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(text.starts_with("line "), "apron text: {text:?}");
    }

    /// Cursor-only updates do not bump the row-content generation. The apron
    /// keeps its materialized row, but its caret must follow the current DEC
    /// cursor so the incoming strip cannot show a stale position or visibility.
    #[test]
    fn apron_memo_refreshes_cursor_without_rematerializing_the_row() {
        let (rows, cols) = (6usize, 10usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        for i in 0..30 {
            term.process(format!("line {i:02}\r\n").as_bytes());
        }
        term.scroll_display(1);
        let mut scratch = crate::render::RenderInput::empty();
        term.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
        assert_eq!(scratch.apron_row.cursor_col, Some(0));
        let stamp = scratch.apron_stamp;
        let content_gen = term.grid().content_gen();

        for (escape, want) in [
            (b"\x1b[6;4H".as_slice(), Some(3)),
            (b"\x1b[?25l".as_slice(), None),
            (b"\x1b[?25h".as_slice(), Some(3)),
            (b"\x1b[5;4H".as_slice(), None),
        ] {
            term.process(escape);
            assert_eq!(term.grid().content_gen(), content_gen);
            term.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
            assert_eq!(scratch.apron_stamp, stamp, "row memo stays reusable");
            assert_eq!(scratch.apron_row.cursor_col, want, "escape {escape:?}");
        }
    }

    /// The apron holds resolved colour: a recolor (host palette push, OSC 4,
    /// default-background change, DECSCNM) moves no `content_gen`, so after
    /// each recolor the kept apron must equal a fresh extraction, while a
    /// stationary fill between recolors keeps the memo.
    #[test]
    fn apron_memo_rematerializes_on_every_recolor() {
        let (rows, cols) = (6usize, 12usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        for i in 0..30 {
            term.process(format!("\x1b[31mline {i:02}\x1b[0m\r\n").as_bytes());
        }
        term.scroll_display(2);
        term.set_allow_palette_reconfigure(true);
        let mut scratch = crate::render::RenderInput::empty();
        let _ = term.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
        assert!(scratch.apron_row.present, "precondition: an apron at d=2");
        let fresh = |t: &mut Terminal| t.cell_frame(rows, cols).apron_row;
        assert_eq!(scratch.apron_row, fresh(&mut term));
        let recolors: [(&str, &dyn Fn(&mut Terminal)); 4] = [
            ("OSC 4", &|t: &mut Terminal| {
                t.process(b"\x1b]4;1;rgb:00/ff/00\x07")
            }),
            ("palette red", &|t: &mut Terminal| {
                t.set_palette_color(1, aterm_types::Rgb::new(0x12, 0x34, 0x56))
            }),
            ("default bg", &|t: &mut Terminal| {
                t.set_default_background(aterm_types::Rgb::new(0xFA, 0xFA, 0xFA))
            }),
            ("DECSCNM", &|t: &mut Terminal| t.process(b"\x1b[?5h")),
        ];
        for (name, recolor) in recolors {
            let before = scratch.apron_row.clone();
            let content_gen = term.grid().content_gen();
            recolor(&mut term);
            assert_eq!(
                term.grid().content_gen(),
                content_gen,
                "{name}: a recolor moves no content_gen"
            );
            let _ = term.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
            assert_eq!(
                scratch.apron_row,
                fresh(&mut term),
                "{name}: the apron follows the recolor"
            );
            assert_ne!(
                scratch.apron_row, before,
                "{name}: non-vacuous — the colours moved"
            );
            let stamp = scratch.apron_stamp;
            let _ = term.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
            assert_eq!(
                scratch.apron_stamp, stamp,
                "{name}: a stationary fill keeps the memo"
            );
        }
    }

    /// A DEC line-size change (DECSWL, `ESC # 5`) is a content change, so the
    /// memoized apron does not keep a stale `DoubleWidth`.
    #[test]
    fn the_apron_follows_a_dec_line_size_change() {
        use crate::grid::LineSize;
        let (rows, cols) = (6usize, 12usize);
        let mut term = Terminal::new(rows as u16, cols as u16);
        for i in 0..30 {
            term.process(format!("line {i:02}\r\n").as_bytes());
        }
        term.scroll_display(2);
        let mut scratch = crate::render::RenderInput::empty();
        term.process(b"\x1b[5;1H\x1b#6");
        let _ = term.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
        assert_eq!(
            term.grid().display_offset(),
            2,
            "precondition: still parked"
        );
        assert_eq!(scratch.apron_row.line_size, LineSize::DoubleWidth);
        let before = term.grid().content_gen();
        term.process(b"\x1b#5");
        assert_ne!(
            term.grid().content_gen(),
            before,
            "DECSWL is a content change"
        );
        let _ = term.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
        assert_eq!(scratch.apron_row.line_size, LineSize::SingleWidth);
        assert_eq!(scratch.apron_row, term.cell_frame(rows, cols).apron_row);
    }

    /// One scratch, two terminals: tabs of a window share `input_scratch`, and
    /// two grids fed the same output park at equal `(offset, content_gen,
    /// epoch, rows, cols)`.
    #[test]
    fn apron_memo_never_carries_another_terminals_row() {
        let (rows, cols) = (6usize, 12usize);
        let fed = |ch: char| {
            let mut t = Terminal::new(rows as u16, cols as u16);
            for _ in 0..30 {
                t.process(format!("{}\r\n", ch.to_string().repeat(8)).as_bytes());
            }
            t.scroll_display(2);
            t
        };
        let (mut a, mut b) = (fed('a'), fed('b'));
        assert_eq!(
            a.grid().content_gen(),
            b.grid().content_gen(),
            "precondition: the counters collide"
        );
        let mut scratch = crate::render::RenderInput::empty();
        let _ = a.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
        let _ = b.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
        assert_eq!(
            scratch.apron_row,
            b.cell_frame(rows, cols).apron_row,
            "B's strip shows B's row"
        );
        let text: String = scratch.apron_row.cells.iter().map(|c| c.ch).collect();
        assert!(text.starts_with("bbbb"), "{text:?}");
    }

    /// `render_row_at_screen` is the live-frame twin of `render_row`: identical
    /// at `display_offset == 0`, but ignores a GUI scroll-back, so socket
    /// `cell`/`screen`/`cells` reads never pair scrolled-back colours with the
    /// live glyph.
    #[test]
    fn render_row_at_screen_ignores_display_offset() {
        let text = |cells: &[RenderCell]| cells.iter().map(|c| c.ch).collect::<String>();
        let mut term = Terminal::new(2, 8);
        term.process(b"L0aaa\r\nL1bbb\r\nL2ccc\r\nL3ddd");

        assert_eq!(
            text(&term.render_row(0)),
            text(&term.render_row_at_screen(0))
        );
        let live0 = text(&term.render_row_at_screen(0));
        let live1 = text(&term.render_row_at_screen(1));

        term.scroll_display(1);

        assert_eq!(
            text(&term.render_row_at_screen(0)),
            live0,
            "screen frame is offset-independent"
        );
        assert_eq!(
            text(&term.render_row_at_screen(1)),
            live1,
            "screen frame is offset-independent"
        );
        assert_ne!(
            text(&term.render_row(0)),
            live0,
            "offset-aware render_row moved with the scroll"
        );
    }

    /// The DEC cursor belongs to the active grid, not retained history: a
    /// scroll that pushes its row off the viewport bottom hides it, returning
    /// live restores it, and a small scroll that leaves its row on screen
    /// keeps it drawn, projected down by the offset.
    #[test]
    fn cell_frame_projects_the_dec_cursor_while_its_row_stays_on_screen() {
        let mut term = Terminal::new(2, 8);
        term.process(b"L0\r\nL1\r\nL2\r\nL3");
        assert!(term.cell_frame(2, 8).cursor_visible);
        term.scroll_display(1);
        assert!(
            !term.cell_frame(2, 8).cursor_visible,
            "a cursor row scrolled off the viewport bottom stays hidden"
        );
        term.scroll_to_bottom();
        assert!(term.cell_frame(2, 8).cursor_visible, "live again");

        let mut tall = Terminal::new(6, 8);
        tall.process(b"top\r\n\r\n\r\n\r\n\r\nmore\r\nmore\r\nmore\x1b[1;1H");
        assert_eq!(tall.cell_frame(6, 8).cursor_row, 0);
        tall.scroll_display(2);
        let scrolled = tall.cell_frame(6, 8);
        assert!(scrolled.display_offset >= 2, "fixture entered history");
        assert!(scrolled.cursor_visible, "its row is still on screen");
        assert_eq!(
            scrolled.cursor_row, scrolled.display_offset as usize,
            "projected down by the offset from its active-grid row 0"
        );
    }

    /// The same real-parser row must keep its glyphs, selectors and ordered
    /// marks when its live split representation becomes a history grapheme.
    #[test]
    fn borrowed_marks_preserve_live_ring_and_tiered_rendering() {
        const CORPUS: &str = "👩\u{200D}💻 👍🏽 🇺🇸 1\u{FE0F}\u{20E3} ❤\u{FE0F} ⌚\u{FE0E} e\u{0301}\u{0323} A\u{E0001} 中";
        for tiered in [false, true] {
            let mut term = if tiered {
                Terminal::with_scrollback(3, 48, 1, crate::scrollback::Scrollback::with_defaults())
            } else {
                Terminal::new(3, 48)
            };
            term.process(CORPUS.as_bytes());
            let live = term.cell_frame(3, 48);
            assert_eq!(
                live.clusters[0].len(),
                4,
                "the cluster arm must be exercised"
            );
            assert_eq!(live.combining[0].len(), 1, "only real diacritics overlay");
            assert_eq!(live.combining[0][0].1.as_ref(), ['\u{0301}', '\u{0323}']);
            assert!(live.cells[0].iter().any(|c| c.text_presentation));
            assert!(live.cells[0].iter().any(|c| c.emoji_presentation));
            assert!(!term.render_row(0).is_empty());

            term.process(b"\r\nnew one\r\nnew two\r\nnew three\r\nnew four");
            if tiered {
                // The first row is older than the one-row ring; flush the
                // deferred buffer so this arm genuinely reads the tier store.
                let _ = term.grid_mut().scrollback_mut();
            }
            term.grid_mut().scroll_to_top();
            assert!(matches!(
                term.grid().visible_row_view(0),
                aterm_grid::VisibleRowView::History { .. }
            ));
            let history = term.cell_frame(3, 48);
            assert_eq!(
                history.cells[0], live.cells[0],
                "tiered={tiered}: glyphs/flags"
            );
            assert_eq!(
                history.clusters[0], live.clusters[0],
                "tiered={tiered}: clusters"
            );
            assert_eq!(
                history.combining[0], live.combining[0],
                "tiered={tiered}: marks"
            );
            assert_eq!(term.render_row(0), live.cells[0]);
            term.grid_mut().scroll_to_bottom();
            assert_ne!(
                term.render_row(0),
                live.cells[0],
                "the history read was not live data"
            );
        }
    }

    /// Randomized differential fuzz over a mutation alphabet chosen to attack
    /// the continuity proof.
    #[test]
    fn dmg1_fuzz_scoped_vs_full_over_random_mutation_streams() {
        use crate::render::FrameRefill;
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                let mut x = self.0;
                x ^= x >> 12;
                x ^= x << 25;
                x ^= x >> 27;
                self.0 = x;
                x.wrapping_mul(0x2545_F491_4F6C_DD1D)
            }
            fn below(&mut self, n: u64) -> u64 {
                self.next() % n
            }
        }

        let (rows, cols) = (10usize, 32usize);
        let mut total_scoped = 0usize;
        let mut total_full = 0usize;

        for seed in 1u64..=60 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.set_allow_palette_reconfigure(true);
            let mut scoped = crate::render::RenderInput::empty();

            for step_i in 0..40 {
                let burst = 1 + rng.below(4);
                let mut what = 0u64;
                for _ in 0..burst {
                    what = rng.below(19);
                    match what {
                        0 => term.process(b"a"),
                        1 => term.process(b"hello world"),
                        2 => {
                            let r = 1 + rng.below(rows as u64);
                            let c = 1 + rng.below(cols as u64);
                            term.process(format!("\x1b[{r};{c}H").as_bytes());
                        }
                        3 => term.process(b"\x1b[1;31mred\x1b[0m"),
                        4 => term.process(b"\r\n"),
                        5 => term.process(b"\x1b[2J"),
                        6 => term.process(b"\x1b[K"),
                        7 => term.process("\u{6f22}\u{5b57}".as_bytes()),
                        8 => term.process("e\u{0301}o\u{0308}".as_bytes()),
                        9 => term.process("\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}".as_bytes()),
                        10 => term.process(b"\x1b[?5h"),
                        11 => term.process(b"\x1b[?5l"),
                        12 => {
                            let idx = rng.below(8);
                            let v = rng.below(255);
                            term.process(
                                format!("\x1b]4;{idx};rgb:{v:02x}/{v:02x}/{v:02x}\x07").as_bytes(),
                            );
                        }
                        13 => term.process(b"\x1b]11;rgb:00/00/40\x07"),
                        14 => term.process(b"\x1b[?1049h"),
                        15 => term.process(b"\x1b[?1049l"),
                        16 => term.take_damage(),
                        17 => term.process("\u{05d0}\u{05d1}\u{05d2} x".as_bytes()),
                        _ => {
                            if rng.below(2) == 0 {
                                term.scroll_display(1 + rng.below(3) as i32);
                            } else {
                                term.scroll_to_bottom();
                            }
                        }
                    }
                }

                let reference = term.cell_frame(rows, cols);
                let refill = term.cell_frame_damage_scoped_into(&mut scoped, rows, cols);
                match refill {
                    FrameRefill::Full { .. } => total_full += 1,
                    FrameRefill::Scoped { .. } => total_scoped += 1,
                }
                assert!(
                    scoped == reference,
                    "DIVERGENCE seed={seed} step={step_i} op={what} refill={refill:?}"
                );
            }
        }
        assert!(
            total_scoped > 200,
            "fuzz under-exercised the scoped arm: {total_scoped}"
        );
        assert!(
            total_full > 200,
            "fuzz under-exercised the full arm: {total_full}"
        );
        eprintln!("DMG1-FUZZ arms: scoped={total_scoped} full={total_full}");
    }

    /// The split compositor's shared `pane_scratch`: two same-dims terminals
    /// alternately refilling one scratch.
    #[test]
    fn dmg1_shared_scratch_across_two_terminals_never_leaks_rows() {
        let (rows, cols) = (6usize, 24usize);
        let mut a = Terminal::new(rows as u16, cols as u16);
        let mut b = Terminal::new(rows as u16, cols as u16);
        a.process(b"AAAA pane one\r\nsecond A row");
        b.process(b"BBBB pane two\r\nsecond B row");

        let mut shared = crate::render::RenderInput::empty();
        for round in 0..4 {
            a.process(format!("\x1b[1;1Ha{round}").as_bytes());
            let ref_a = a.cell_frame(rows, cols);
            let ra = a.cell_frame_damage_scoped_into(&mut shared, rows, cols);
            assert!(
                shared == ref_a,
                "round {round}: pane A frame diverged (leak from B?) refill={ra:?}"
            );

            b.process(format!("\x1b[1;1Hb{round}").as_bytes());
            let ref_b = b.cell_frame(rows, cols);
            let rb = b.cell_frame_damage_scoped_into(&mut shared, rows, cols);
            assert!(
                shared == ref_b,
                "round {round}: pane B frame diverged (leak from A?) refill={rb:?}"
            );
        }
    }

    /// Per-cause attribution, one clause at a time: from a settled scratch
    /// that provably takes the scoped arm, apply exactly one mutation and
    /// require that clause's own cause.
    #[test]
    fn dmg1_every_continuity_clause_reports_its_own_cause() {
        use crate::render::{FrameRefill, FullRefillCause, RenderInput, RowOrder};

        const ROWS: usize = 8;
        const COLS: usize = 32;

        /// A terminal with scrollback and a scratch whose continuity chain is
        /// live: the next damage-scoped refill takes the scoped arm.
        fn settled() -> (Terminal, RenderInput) {
            let mut term = Terminal::new(ROWS as u16, COLS as u16);
            for i in 0..20 {
                term.process(format!("line {i}\r\n").as_bytes());
            }
            let mut scratch = RenderInput::empty();
            let _ = term.cell_frame_damage_scoped_into(&mut scratch, ROWS, COLS);
            term.process(b"\x1b[1;1Hz");
            let r = term.cell_frame_damage_scoped_into(&mut scratch, ROWS, COLS);
            assert!(
                matches!(r, FrameRefill::Scoped { .. }),
                "the fixture must start settled on the scoped arm, got {r:?}"
            );
            (term, scratch)
        }

        type Mutate = fn(&mut Terminal, &mut RenderInput);
        let cases: &[(&str, Mutate, FullRefillCause)] = &[
            (
                "the last engine fill left a row in visual order",
                |_t, s| s.engine_row_order = RowOrder::BidiVisual,
                FullRefillCause::BidiVisual,
            ),
            (
                "the scratch carries no terminal identity",
                |_t, s| s.terminal_id = 0,
                FullRefillCause::ScratchUnstamped,
            ),
            (
                "the scratch belongs to another terminal",
                |_t, s| s.terminal_id = s.terminal_id.wrapping_add(1).max(1),
                FullRefillCause::TerminalMismatch,
            ),
            (
                "another consumer closed the damage session",
                |t, _s| t.take_damage(),
                FullRefillCause::DamageTaken,
            ),
            (
                "a host mutator wrote cells and bumped the seq",
                |_t, s| s.snapshot_seq = s.snapshot_seq.wrapping_add(1),
                FullRefillCause::HostMutation,
            ),
            (
                "a host row prepend is still in force",
                |_t, s| s.row_shift = 1,
                FullRefillCause::RowShift,
            ),
            (
                "the alternate-screen bit disagrees",
                |_t, s| s.engine_alt = !s.engine_alt,
                FullRefillCause::AltScreen,
            ),
            (
                "the scratch's own dims are stale",
                |_t, s| s.cols += 1,
                FullRefillCause::ScratchRows,
            ),
            (
                "the scratch grew a row without stamping a shift",
                |_t, s| s.cells.push(Vec::new()),
                FullRefillCause::ScratchRowCount,
            ),
            (
                "the window and its engine are out of step",
                |t, _s| t.resize((ROWS + 2) as u16, COLS as u16),
                FullRefillCause::EngineDims,
            ),
            (
                "the engine's viewport is scrolled back",
                |t, _s| t.scroll_display(1),
                FullRefillCause::EngineScrolled,
            ),
            (
                "the scratch was filled while scrolled back",
                |_t, s| s.display_offset = 1,
                FullRefillCause::ScratchScrolled,
            ),
            (
                "the live grid scrolled under the retained rows",
                |_t, s| s.base_y = s.base_y.wrapping_add(1),
                FullRefillCause::BaseY,
            ),
            (
                "row identity changed without base_y moving",
                |_t, s| s.absolute_row_revision = s.absolute_row_revision.wrapping_add(1),
                FullRefillCause::RowRevision,
            ),
            (
                "the tracker holds whole-grid damage",
                |t, _s| t.process(b"\x1b[?5h"),
                FullRefillCause::FullDamage,
            ),
        ];

        for (what, mutate, want) in cases {
            let (mut term, mut scratch) = settled();
            mutate(&mut term, &mut scratch);
            let r = term.cell_frame_damage_scoped_into(&mut scratch, ROWS, COLS);
            assert_eq!(
                r,
                FrameRefill::Full { cause: *want },
                "{what}: the full arm must be attributed to `{}`",
                want.as_str()
            );
        }

        let mut covered: Vec<FullRefillCause> = cases.iter().map(|(_, _, c)| *c).collect();
        covered.sort_unstable();
        covered.dedup();
        assert_eq!(
            covered,
            FullRefillCause::ALL.to_vec(),
            "every FullRefillCause must be produced by exactly one clause here"
        );

        let mut labels: Vec<&str> = FullRefillCause::ALL.iter().map(|c| c.as_str()).collect();
        let n = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), n, "cause labels must be distinct");
        assert!(
            labels.iter().all(|l| {
                !l.is_empty()
                    && l.bytes()
                        .all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit())
            }),
            "cause labels must be snake_case ASCII: {labels:?}"
        );
        for (i, c) in FullRefillCause::ALL.iter().enumerate() {
            assert_eq!(c.index(), i, "FullRefillCause::ALL must be in index order");
        }
    }

    /// Fixture: a terminal with history and a scratch parked in it (filled at a
    /// non-zero offset), so the next scrolled refill is a candidate for the
    /// scrolled proof.
    fn scr2_parked_in_history(
        rows: usize,
        cols: usize,
        offset: i32,
    ) -> (Terminal, crate::render::RenderInput) {
        use crate::render::{FrameRefill, FullRefillCause};
        let mut term = Terminal::new(rows as u16, cols as u16);
        for i in 0..24 {
            term.process(format!("line {i}\r\n").as_bytes());
        }
        let mut scratch = crate::render::RenderInput::empty();
        let _ = term.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
        term.scroll_display(offset);
        let r = term.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::EngineScrolled
            },
            "entering history from the live bottom keeps the full arm under the ENGINE's offset"
        );
        assert_eq!(
            term.grid().display_offset(),
            usize::try_from(offset).expect("a positive park offset"),
            "precondition: the viewport is parked where the fixture put it"
        );
        (term, scratch)
    }

    /// The scrolled debug check compares engine channels only: the GUI stamps
    /// host-owned overlay channels (wallpaper, atlases, effect streams,
    /// sprites, ink, cursor overrides) into the scratch after each fill.
    #[test]
    fn scr2_debug_net_ignores_host_overlays() {
        use crate::render::FrameRefill;
        let (mut term, mut scratch) = scr2_parked_in_history(8, 20, 3);
        scratch.cursor_fill_override = Some(0x0012_3456);
        scratch.cursor_trail_color = 0x00AB_CDEF;
        term.scroll_display(1);
        let r = term.cell_frame_damage_scoped_into(&mut scratch, 8, 20);
        assert_eq!(r, FrameRefill::Scoped { rows_refilled: 1 });
        assert_eq!(
            scratch.cursor_fill_override,
            Some(0x0012_3456),
            "host overlays untouched"
        );
    }

    /// One scrolled oracle step: a fresh full extract, then the scoped refill,
    /// then content equality; the row-revision lane must be withheld from
    /// every frame filled while scrolled.
    fn scr2_step(
        term: &mut Terminal,
        scratch: &mut crate::render::RenderInput,
        rows: usize,
        cols: usize,
        what: &str,
    ) -> crate::render::FrameRefill {
        let reference = term.cell_frame(rows, cols);
        let refill = term.cell_frame_damage_scoped_into(scratch, rows, cols);
        assert!(
            *scratch == reference,
            "scrolled scoped extraction diverged from the full extract after: {what} ({refill:?})"
        );
        if scratch.display_offset != 0 {
            assert_eq!(
                scratch.row_rev_lane, 0,
                "a fill at a non-zero offset must not publish a row-revision lane ({what})"
            );
        }
        refill
    }

    /// History evicted from under a viewport parked at the top: the grid
    /// re-clamps the offset and survivors keep their absolute numbers, so the
    /// rotation stays right whether or not the evictor bumped `content_gen`.
    #[test]
    fn scr2_eviction_under_a_parked_viewport_matches_the_full_extract() {
        const ROWS: usize = 8;
        const COLS: usize = 32;
        let (mut term, mut scratch) = scr2_parked_in_history(ROWS, COLS, 2);
        term.scroll_to_top();
        let _ = scr2_step(&mut term, &mut scratch, ROWS, COLS, "to top");
        let before = term.grid().scrollback_lines();
        term.grid_mut().set_scrollback_line_limit(Some(10));
        assert!(
            term.grid().scrollback_lines() < before,
            "precondition: lines were evicted"
        );
        assert_eq!(
            term.grid().display_offset(),
            term.grid().scrollback_lines(),
            "the shrink re-clamped the parked offset to the new top"
        );
        let _ = scr2_step(
            &mut term,
            &mut scratch,
            ROWS,
            COLS,
            "limit shrink while parked at top",
        );
        term.scroll_display(-1);
        assert_eq!(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "step after eviction"),
            crate::render::FrameRefill::Scoped { rows_refilled: 1 },
            "the scrub is scoped again on the frame after the eviction"
        );
    }

    /// Scrolled reach: the refilled strip is exactly the rows the offset moved
    /// (one for a one-row step, three for steps of one and two between fills,
    /// whose tracker bits say `0..2`, zero for a frame that moved nothing).
    #[test]
    fn scr2_scrolled_wheel_step_refills_only_the_exposed_strip() {
        use crate::render::FrameRefill;
        const ROWS: usize = 8;
        const COLS: usize = 32;
        let (mut term, mut scratch) = scr2_parked_in_history(ROWS, COLS, 3);

        term.scroll_display(1);
        assert_eq!(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "1-row step up"),
            FrameRefill::Scoped { rows_refilled: 1 },
            "a 1-row wheel step up re-resolves exactly the one exposed row"
        );
        term.scroll_display(-1);
        assert_eq!(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "1-row step down"),
            FrameRefill::Scoped { rows_refilled: 1 },
            "a 1-row wheel step down re-resolves exactly the one exposed row"
        );
        assert_eq!(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "parked, untouched"),
            FrameRefill::Scoped { rows_refilled: 0 },
            "a blink/pill frame parked in history refills nothing"
        );
        term.process(b"\x1b[2;2H");
        assert_eq!(
            scr2_step(
                &mut term,
                &mut scratch,
                ROWS,
                COLS,
                "cursor-only batch while parked"
            ),
            FrameRefill::Scoped { rows_refilled: 0 },
            "a cursor-only batch while parked re-pins to the same offset and refills nothing"
        );
        term.scroll_display(1);
        term.scroll_display(2);
        assert_eq!(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "compound 1+2"),
            FrameRefill::Scoped { rows_refilled: 3 },
            "two steps between fills expose their NET rows, not the tracker's union"
        );
        term.scroll_display(2);
        term.scroll_display(-1);
        assert_eq!(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "reversal 2-1"),
            FrameRefill::Scoped { rows_refilled: 1 },
            "a reversal within a frame exposes its net one row"
        );
        term.scroll_display(2);
        term.scroll_display(-2);
        assert_eq!(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "reversal 2-2"),
            FrameRefill::Scoped { rows_refilled: 0 },
            "a reversal that nets zero exposes nothing"
        );
    }

    /// Scrolled oracle: byte equality with a fresh full extraction across a
    /// compound move whose net exceeds the viewport while the tracker stays
    /// partial, the clamp at the top of history, and output while parked (the
    /// re-pin, and a write into a visible live row).
    #[test]
    fn scr2_scrolled_scoped_extraction_matches_full_extract_across_history_ends_and_output() {
        use crate::render::{FrameRefill, FullRefillCause};
        const ROWS: usize = 8;
        const COLS: usize = 32;
        const FULL: FrameRefill = FrameRefill::Full {
            cause: FullRefillCause::EngineScrolled,
        };
        let (mut term, mut scratch) = scr2_parked_in_history(ROWS, COLS, 2);
        let mut step =
            |term: &mut Terminal, what: &str| scr2_step(term, &mut scratch, ROWS, COLS, what);

        for _ in 0..3 {
            term.scroll_display(3);
        }
        assert_eq!(step(&mut term, "compound 3+3+3"), FULL);

        term.scroll_to_top();
        assert_eq!(
            step(&mut term, "scroll_to_top"),
            FrameRefill::Scoped { rows_refilled: 6 }
        );
        term.scroll_display(5);
        assert_eq!(
            step(&mut term, "clamped at top"),
            FrameRefill::Scoped { rows_refilled: 0 }
        );

        term.scroll_display(-12);
        assert_eq!(step(&mut term, "12-row jump down"), FULL);
        term.process(b"tail\r\n");
        assert_eq!(term.grid().display_offset(), 6, "SCR-1 re-pinned the view");
        assert_eq!(step(&mut term, "output re-pinned the viewport"), FULL);
        term.scroll_display(1);
        assert_eq!(
            step(&mut term, "step after output"),
            FrameRefill::Scoped { rows_refilled: 1 }
        );
        term.process(b"\x1b[1;1HX");
        assert_eq!(step(&mut term, "write into a visible live row"), FULL);
        assert_eq!(
            step(&mut term, "parked after the write"),
            FrameRefill::Scoped { rows_refilled: 0 }
        );
    }
}
