// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Render-ready per-cell extraction for CPU/GPU rasterizers.
//!
//! Bridges the grid's packed cell storage and the central color resolver
//! ([`color_resolve`](super::color_resolve)) into a flat, render-ready row of
//! [`RenderCell`]s. Each cell carries the resolved character plus final
//! foreground/background RGB with every style attribute already applied:
//! palette indices, RGB overflow, bold-to-bright, dim, inverse, hidden, and
//! terminal-level reverse video (DECSCNM).

use super::Terminal;
use super::color_resolve::{StyleResolveOpts, resolve_colors_raw_opts};
use crate::grid::{Cell, CellFlags, Grid};

/// Out-params for the folded emoji-cluster + combining-mark extraction
/// (`render_row_into_impl`): `(clusters, combining)`, where each entry is the
/// cell column paired with its `(base+combining)` string / combining `char`s.
/// Factored out to keep the per-cell pass signature readable.
type ExtrasOut<'a> = (
    &'a mut Vec<(usize, Box<str>)>,
    &'a mut Vec<(usize, Box<[char]>)>,
);

/// The line-decoration style under a cell (SGR 4 / 4:n / 21). The terminal
/// packs these as `UNDERLINE` / `DOUBLE_UNDERLINE` / `CURLY_UNDERLINE` bit
/// combinations; [`RenderCell`] resolves them to one variant for the renderer.
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
    fn from_flags(cflags: CellFlags) -> Self {
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

/// A cell's vertical script: a SMALLER glyph set low in its cell (`Sub`).
/// The renderers scale the glyph and move it as one raster
/// (`aterm_render::script_drop`); rule and box glyphs and colour emoji keep
/// their full size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Script {
    /// The ordinary glyph.
    #[default]
    Normal,
    /// Subscript: small, its foot on the cell's floor, and moved `nudge`
    /// [`Script::NUDGES_PER_CELL`]ths of a cell sideways (negative: left).
    /// A small glyph centred in a full cell stands far from its neighbours;
    /// the nudge lets a row builder set a line of them at a tighter pitch
    /// than the grid's, one character still per cell.
    Sub {
        /// Sideways offset of the glyph, in [`Script::NUDGES_PER_CELL`]ths
        /// of a cell.
        nudge: i8,
    },
}

impl Script {
    /// The steps a cell is divided into for [`Script::Sub`]'s `nudge`: fine
    /// enough that a nudge rounds by well under a pixel at text sizes, and
    /// one byte reaches about eight cells either way.
    pub const NUDGES_PER_CELL: i8 = 16;
}

/// A single render-ready terminal cell.
///
/// Colors are final RGB triples; the renderer can fill the cell rect with
/// [`bg`](RenderCell::bg) and blit the glyph for [`ch`](RenderCell::ch) in
/// [`fg`](RenderCell::fg) with no further attribute logic.
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
    ///
    /// Such a column has no glyph of its own (`ch` is a space); renderers
    /// should fill its background but leave drawing the glyph to the wide
    /// lead cell, whose rasterized bitmap naturally overflows into it.
    pub wide: bool,
    /// True when this lead cell requested emoji presentation: a text-default
    /// emoji base (`is_vs16_emoji_capable`) that VS16 (U+FE0F) widened to two
    /// cells, so the renderer prefers the colour-emoji face over the mono
    /// fonts. `❤️` (U+2764 U+FE0F) is the canonical case; bare `❤` stays
    /// narrow and mono.
    pub emoji_presentation: bool,
    /// True when this lead cell carries an explicit VS15 (U+FE0E) request and
    /// is narrow: a default-emoji scalar such as `😀︎` takes the text/mono path
    /// in one cell. A later effective VS16 re-widens the cell and clears this
    /// (`CellFlags::WIDE` is then set).
    pub text_presentation: bool,
    /// SGR 1 bold: the renderer rasterizes the glyph with extra stroke weight.
    /// (Bold-to-bright colour, when enabled, is already applied in `fg`.)
    pub bold: bool,
    /// SGR 3 italic: the renderer rasterizes the glyph with a synthetic slant.
    pub italic: bool,
    /// Underline decoration (SGR 4 family). Drawn as line(s) in
    /// [`underline_color`](RenderCell::underline_color) (or [`fg`](RenderCell::fg)).
    pub underline: UnderlineStyle,
    /// Strikethrough (SGR 9): a line through the cell middle, in `fg`.
    pub strikethrough: bool,
    /// Overline (SGR 53): a line along the cell top, drawn in
    /// [`overline_color`](RenderCell::overline_color) (or [`fg`](RenderCell::fg)).
    pub overline: bool,
    /// SGR 58 underline colour, when set; otherwise the underline uses `fg`.
    pub underline_color: Option<[u8; 3]>,
    /// Overline colour, when set; otherwise the overline uses `fg`.
    ///
    /// Chrome-only: no escape sequence sets an overline colour (SGR 58/59 is
    /// the underline's), so [`Terminal::render_row`] always leaves it `None`.
    /// aterm's own chrome row builders use it for a seam that must hold one
    /// tone across cells with different inks.
    pub overline_color: Option<[u8; 3]>,
    /// The glyph's vertical script ([`Script`]).
    ///
    /// A CHROME-ONLY channel, like [`overline_color`](RenderCell::overline_color):
    /// [`Terminal::render_row`] always leaves it [`Script::Normal`] — the
    /// grid's SGR 74 is not read into it, since that would change what
    /// terminal programs draw. aterm's own row builders set it: the Claude
    /// Code footer writes its facts into the composer rule as `Sub`, small and
    /// low, below the cursor trail's vivid rail above them.
    pub script: Script,
}

/// A blank cell with `ch == ' '` (not `'\0'`), since renderers blit `ch`
/// unconditionally. Colors are black on black: a `Default` cell is a
/// placeholder, and real frame paths resolve colors through the live palette.
impl Default for RenderCell {
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

impl Terminal {
    /// Resolve the render-ready cell for an unmaterialized grid column.
    ///
    /// A missing row tail is the terminal's implicit [`Cell::EMPTY`], not the
    /// last stored cell's SGR. Hosts use this to pad a snapshot row to its
    /// width (split composition, web/wasm snapshots, control queries), on the
    /// same live palette/default-color/DECSCNM path as materialized cells.
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

    /// Resolve a visible row into render-ready cells, one per stored column.
    ///
    /// Each returned [`RenderCell`] has its foreground/background fully
    /// resolved through [`color_resolve`](super::color_resolve): palette
    /// indices, RGB overflow (ring buffer + overflow map), bold-to-bright,
    /// dim, inverse, hidden, and terminal-level reverse video (DECSCNM).
    ///
    /// Returns an empty vector for out-of-range rows.
    #[must_use]
    pub fn render_row(&self, row: usize) -> Vec<RenderCell> {
        let mut out = Vec::new();
        self.render_row_into(row, &mut out);
        out
    }

    /// Like [`render_row`](Self::render_row), but fills a caller-owned `out`
    /// buffer instead of allocating a fresh `Vec` each call — the per-frame
    /// extract path reuses one buffer across rows/frames. `out` is `clear()`ed
    /// first, then pushed exactly the cells [`render_row`](Self::render_row)
    /// would return (it IS the one code path), so the result is byte-identical.
    /// Out-of-range rows leave `out` empty.
    ///
    /// Used by the engine's own snapshot builder
    /// [`cell_frame_into`](Self::cell_frame_into) (A-3) and by external callers
    /// that scan many rows and reuse one buffer (e.g. the sparkle-words rescanner);
    /// one-shot callers use the allocating [`render_row`](Self::render_row).
    pub fn render_row_into(&self, row: usize, out: &mut Vec<RenderCell>) {
        self.render_row_into_impl(row, out, None, false);
    }

    /// Live-frame twin of [`render_row`](Self::render_row): resolves the row at
    /// `screen_row` ignoring `display_offset`, so socket introspection reads
    /// (`cell`/`screen`/`cells`) get live colours and decorations that pair
    /// with the live [`cell_grapheme`](Self::cell_grapheme) glyph. Identical to
    /// `render_row` at `display_offset == 0`.
    #[must_use]
    pub fn render_row_at_screen(&self, screen_row: usize) -> Vec<RenderCell> {
        let mut out = Vec::new();
        self.render_row_at_screen_into(screen_row, &mut out);
        out
    }

    /// Like [`render_row_at_screen`](Self::render_row_at_screen), but fills a
    /// caller-owned `out` buffer instead of allocating a fresh `Vec` per call —
    /// the LIVE-frame twin of [`render_row_into`](Self::render_row_into).
    ///
    /// `out` is `clear()`ed then pushed exactly the cells
    /// [`render_row_at_screen`](Self::render_row_at_screen) would return: it IS
    /// the one code path (the allocating form delegates here), so the result is
    /// byte-identical by construction. The socket introspection gather
    /// (`aterm-gui`'s `gather_styled_frame`) walks every visible row on every
    /// `screen` read and `subscribe cells` push with the `Terminal` mutex held;
    /// one buffer reused across the rows replaces a `Vec` allocation per row.
    pub fn render_row_at_screen_into(&self, screen_row: usize, out: &mut Vec<RenderCell>) {
        self.render_row_into_impl(screen_row, out, None, true);
    }

    /// Core of [`render_row_into`](Self::render_row_into), with an optional
    /// `extras_out` sink for the emoji-cluster and combining-mark
    /// classification.
    ///
    /// [`cell_frame_into`](Self::cell_frame_into) passes
    /// `Some((clusters, combining))` so that extraction shares this loop's one
    /// [`CellExtra`](aterm_grid::CellExtra) read per cell instead of two extra
    /// full-grid passes; the output matches `cluster_row_into` /
    /// `combining_row_into`.
    #[allow(
        clippy::too_many_lines,
        reason = "single per-cell resolution pass (colors + decorations + emoji/combining extraction) over a row"
    )]
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
        // Row source. `screen`: the live row, independent of display offset, so
        // introspection reads pair with the live `cell_grapheme` glyph.
        // Otherwise the offset-aware source the GUI renderer needs: a
        // scrolled-back row supplies its materialized cells and extras from
        // history. The two coincide at display_offset == 0.
        let view = if screen {
            grid.screen_row_view(visible_row)
        } else {
            grid.visible_row_view(visible_row)
        };

        let palette = self.color_palette();
        let default_fg = self.default_foreground();
        let default_bg = self.default_background();
        let reverse_video = self.modes().reverse_video();
        // Host style policy (W5): bold-to-bright promotion + faint opacity,
        // set by `apply_config` (config `bold_is_bright` / `faint_opacity`).
        let style_opts = StyleResolveOpts {
            bold_is_bright: self.color.bold_is_bright,
            faint_opacity: self.color.faint_opacity,
        };

        let cols = view.len();
        out.reserve(cols as usize);
        // Whether `view.cell(col - 1)` is wide (false at col 0 and past a missing cell).
        let mut prev_wide = false;
        for col in 0..cols {
            let Some(cell) = view.cell(col) else {
                prev_wide = false;
                continue;
            };

            // One flag-gated overflow lookup covering fg/bg RGB, resolved char
            // and cell extra; a no-op for the common plain cell. The view keys
            // the live probe on the display-mapped screen row and swaps in
            // materialized history extras for scrolled-back rows.
            let data = view.cell_data(col, cell);

            // Style-interned cells keep their colors in the StyleTable, so their
            // raw `colors()` is a StyleId payload; rehydrate to an inline cell
            // (plus explicit RGB) before resolving. No production path sets
            // `CellFlags::USES_STYLE_ID` (its writers are test-only), which the
            // assertion below checks; the branch stays because the cell encoding
            // reserves the bit.
            debug_assert!(
                !cell.uses_style_id(),
                "render_row: a live cell carries USES_STYLE_ID, but nothing interns \
                 styles any more — reviving style-id cells means reviving the intern"
            );
            let (eff_cell, fg_rgb, bg_rgb) = if cell.uses_style_id() {
                let extra_flags = cell.flags().difference(CellFlags::USES_STYLE_ID);
                let (fg, bg, flags) = grid.resolve_style_to_colors(cell.style_id(), extra_flags);
                let fg_rgb = fg.is_rgb().then(|| {
                    let (r, g, b) = fg.rgb_components();
                    [r, g, b]
                });
                let bg_rgb = bg.is_rgb().then(|| {
                    let (r, g, b) = bg.rgb_components();
                    [r, g, b]
                });
                (Cell::with_style(cell.char(), fg, bg, flags), fg_rgb, bg_rgb)
            } else {
                (cell, data.fg_rgb(), data.bg_rgb())
            };

            let (fg, bg) = resolve_colors_raw_opts(
                &eff_cell,
                fg_rgb,
                bg_rgb,
                palette,
                default_fg,
                default_bg,
                reverse_video,
                style_opts,
            );

            // A true wide continuation must be told apart from a
            // DECSCA-protected cell: `PROTECTED` and `WIDE_CONTINUATION` share
            // bit 10. A real continuation has the bit, is not itself wide, and
            // sits right of a wide cell (the `Row::is_cell_wide_continuation`
            // rule, inlined with the previous column carried across the loop).
            let wide = cell.is_wide_continuation() && !cell.is_wide() && prev_wide;
            prev_wide = cell.is_wide();
            // Base codepoint for this cell (ring/HashMap complex char for complex
            // cells, else the packed char), reused below for the render glyph, the
            // emoji-cluster base, and the '\0' -> space mapping. For a live row this
            // is the live complex char; for a scrolled-back row it is the
            // materialized history base.
            let raw = if cell.is_complex() {
                data.complex_base().unwrap_or('\u{FFFD}')
            } else {
                cell.char()
            };
            let ch = if wide || raw == '\0' { ' ' } else { raw };

            // Read the selector/cluster tail once. Besides feeding the optional
            // cluster/combining snapshot below, it preserves an explicit VS15
            // request for the renderer: Unicode default presentation alone cannot
            // distinguish bare 😀 (colour) from narrow `😀︎` (text).
            let marks = data.marks();

            // Emoji presentation: an emoji-capable base is narrow by default, so
            // a wide lead cell holding one was widened by VS16
            // (`widen_previous_cell_for_vs16`).
            let emoji_presentation =
                !wide && cell.is_wide() && super::handler::is_vs16_emoji_capable(ch);
            // VS15 text presentation is authoritative only while the resulting
            // lead remains narrow. This also handles selector replay exactly:
            // `⌚︎️` is wide again (VS16 took effect) and therefore not text,
            // while an ineffective later VS16 at the row edge leaves VS15 active.
            let text_presentation =
                !wide && !cell.is_wide() && marks.clone().any(|c| c == '\u{FE0E}');

            // Line decorations (SGR 4 family / 9 / 53).
            let cflags = eff_cell.flags();
            let underline = UnderlineStyle::from_flags(cflags);
            let strikethrough = cflags.contains(CellFlags::STRIKETHROUGH);
            let overline = cflags.contains(CellFlags::OVERLINE);
            let bold = cflags.contains(CellFlags::BOLD);
            let italic = cflags.contains(CellFlags::ITALIC);
            // Single CellExtra read (from the unified lookup above) serves the
            // SGR 58 underline colour and the emoji-cluster / combining-mark
            // classification below. Live: the flag-gated live extra; history: the
            // materialized extra (shape-identical, so combining()/hyperlink() read
            // the same way; underline colour is not restored into scrollback).
            let extra = data.cell_extra();
            let underline_color = if underline == UnderlineStyle::None {
                None
            } else {
                extra.and_then(|e| {
                    // SGR 58:2 explicit RGB wins; SGR 58:5 stores a palette
                    // index, resolved here against the live palette each frame
                    // so an OSC 4 change re-colors the underline.
                    e.underline_color().or_else(|| {
                        e.underline_color_index().map(|i| {
                            let c = palette.get(i);
                            [c.r, c.g, c.b]
                        })
                    })
                })
            };
            // Hyperlinked cells get an underline: OSC 8 lets the visible text
            // and the destination be unrelated, so a link must look like one
            // before it is trusted. A decoration the program chose wins (this
            // fills only the undecorated case), and the colour stays `None` so
            // the line takes the text's own ink.
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
                // No escape sequence can set this: the overline colour channel
                // is aterm's own chrome seam, never a rendition a program may
                // ask for (see `RenderCell::overline_color`).
                overline_color: None,
                script: Script::Normal,
            });

            // Emoji-cluster / combining-mark extraction, folded in from
            // `cluster_row_into` / `combining_row_into` (byte-identical at
            // display_offset == 0) so the per-frame snapshot probes the extras
            // once per cell instead of in two extra full-grid passes. Only
            // requested by `cell_frame_into`. `marks()` reads the live combining
            // slice, or reconstructs a materialized cell's cluster tail.
            if let Some((clusters, combining_out)) = extras_out.as_mut() {
                if marks.clone().any(is_emoji_sequence_marker) {
                    // Multi-codepoint emoji sequence (ZWJ, skin tone, keycap,
                    // regional-indicator pair): surface the whole grapheme for
                    // shaping. Skip a NUL base, matching `cluster_row_into`.
                    if raw != '\0' {
                        // Count UTF-8 bytes, not scalars: even a flag's two
                        // scalars occupy eight bytes. Exact capacity avoids
                        // growing the string and shrinking it again for Box.
                        let bytes =
                            raw.len_utf8() + marks.clone().map(char::len_utf8).sum::<usize>();
                        let mut s = String::with_capacity(bytes);
                        s.push(raw);
                        s.extend(marks);
                        clusters.push((col as usize, s.into_boxed_str()));
                    }
                } else if marks.clone().next().is_some() {
                    // Plain combining diacritics: overlay every mark that has a
                    // glyph of its own (`draws_no_glyph` names the exceptions),
                    // matching `combining_row_into`.
                    let overlay: Box<[char]> = marks.filter(|&c| !draws_no_glyph(c)).collect();
                    if !overlay.is_empty() {
                        combining_out.push((col as usize, overlay));
                    }
                }
            }
        }
    }

    /// Emoji grapheme-cluster strings for the visible `row`, sparse: one
    /// `(col, cluster)` per cell whose combining marks form a multi-codepoint
    /// emoji sequence (ZWJ 👨‍👩‍👧, skin tone 👍🏽, keycap 1️⃣), which the renderer
    /// shapes to one colour glyph.
    ///
    /// Excludes pure VS15/VS16 clusters (e.g. ❤️), which use
    /// [`RenderCell::emoji_presentation`]. `col` is the wide lead cell, where
    /// the renderer blits the glyph.
    #[must_use]
    #[cfg(test)]
    pub fn cluster_row(&self, row: usize) -> Vec<(usize, Box<str>)> {
        let mut out = Vec::new();
        self.cluster_row_into(row, &mut out);
        out
    }

    /// Like [`cluster_row`](Self::cluster_row), but fills a caller-owned `out`
    /// buffer instead of allocating a fresh `Vec`. `out` is `clear()`ed first,
    /// then pushed exactly the `(col, cluster)` pairs
    /// [`cluster_row`](Self::cluster_row) would return (the one code path), so
    /// the result is byte-identical. The owned cluster strings (`Box<str>`) are
    /// still allocated per cluster — only the per-row container Vec is reused.
    ///
    /// `pub(crate)`: consumed only by [`cell_frame_into`](Self::cell_frame_into).
    #[cfg(test)]
    pub(crate) fn cluster_row_into(&self, row: usize, out: &mut Vec<(usize, Box<str>)>) {
        out.clear();
        let Ok(visible_row) = u16::try_from(row) else {
            return;
        };
        let grid = self.grid();
        // Fast path: emoji clusters live in cell extras (combining marks). With
        // no extras anywhere there is nothing to scan — the common case (plain
        // text) pays a single bool check instead of a per-column probe.
        if grid.extras().is_empty() {
            return;
        }
        let Some(grid_row) = grid.row(visible_row) else {
            return;
        };
        let cols = grid_row.len();
        for col in 0..cols {
            let Some(extra) = grid.cell_extra(visible_row, col) else {
                continue;
            };
            let combining = extra.combining();
            if !combining.iter().copied().any(is_emoji_sequence_marker) {
                continue;
            }
            let Some(base) = grid.resolved_char(visible_row, col) else {
                continue;
            };
            if base == '\0' {
                continue;
            }
            let mut s = String::with_capacity(2 + combining.len());
            s.push(base);
            s.extend(combining.iter().copied());
            out.push((col as usize, s.into_boxed_str()));
        }
    }

    /// Combining marks to overlay per cell of the visible `row`, sparse: one
    /// `(col, marks)` per cell carrying diacritics (é = e + U+0301, …); the
    /// renderer blits each mark over the base.
    ///
    /// Excludes emoji sequences ([`cluster_row`](Self::cluster_row)), bare
    /// VS15/VS16 selectors ([`RenderCell::emoji_presentation`]) and plane-14
    /// default-ignorables that draw nothing (`draws_no_glyph`). Marks keep
    /// arrival order so stacked diacritics layer correctly.
    #[must_use]
    #[cfg(test)]
    pub fn combining_row(&self, row: usize) -> Vec<(usize, Box<[char]>)> {
        let mut out = Vec::new();
        self.combining_row_into(row, &mut out);
        out
    }

    /// Like [`combining_row`](Self::combining_row), but fills a caller-owned
    /// `out` buffer instead of allocating a fresh `Vec`. `out` is `clear()`ed
    /// first, then pushed exactly the `(col, marks)` pairs
    /// [`combining_row`](Self::combining_row) would return (the one code path),
    /// so the result is byte-identical. The owned mark slices (`Box<[char]>`)
    /// are still allocated per cell — only the per-row container Vec is reused.
    ///
    /// `pub(crate)`: consumed only by [`cell_frame_into`](Self::cell_frame_into).
    #[cfg(test)]
    pub(crate) fn combining_row_into(&self, row: usize, out: &mut Vec<(usize, Box<[char]>)>) {
        out.clear();
        let Ok(visible_row) = u16::try_from(row) else {
            return;
        };
        let grid = self.grid();
        if grid.extras().is_empty() {
            return;
        }
        let Some(grid_row) = grid.row(visible_row) else {
            return;
        };
        for col in 0..grid_row.len() {
            let Some(extra) = grid.cell_extra(visible_row, col) else {
                continue;
            };
            let combining = extra.combining();
            if combining.is_empty() || combining.iter().copied().any(is_emoji_sequence_marker) {
                continue;
            }
            // Overlay every combining char that has a glyph of its own; the
            // presentation selectors and plane 14 attach but draw nothing
            // (`draws_no_glyph`).
            let marks: Box<[char]> = combining
                .iter()
                .copied()
                .filter(|&c| !draws_no_glyph(c))
                .collect();
            if marks.is_empty() {
                continue;
            }
            out.push((col as usize, marks));
        }
    }

    /// Inline-image placements for the visible `row`, sparse: one `(col,
    /// ImageRef)` for every image layer covering a cell (iTerm2 OSC 1337, sixel,
    /// Kitty), ascending by column and, within a cell, bottom layer first — the
    /// order both renderers draw them in. The renderer decodes each image once
    /// (keyed by the `Arc` inside the ref) and blits the cell's tile; a cell an
    /// image covers above the text skips its glyph (its background still
    /// fills). Cells absent here take the ordinary glyph dispatch.
    #[must_use]
    pub fn images_row(&self, row: usize) -> Vec<(usize, aterm_grid::ImageRef)> {
        let mut out = Vec::new();
        self.images_row_into(row, &mut out);
        out
    }

    /// Like [`images_row`](Self::images_row), but fills a caller-owned `out`
    /// buffer instead of allocating a fresh `Vec`. `out` is `clear()`ed first,
    /// then pushed exactly the `(col, ImageRef)` pairs
    /// [`images_row`](Self::images_row) would return (the one code path), so the
    /// result is byte-identical. Each pushed `ImageRef` is a cheap `Arc` clone +
    /// two `u16`; the (large) image payload is shared, not copied.
    ///
    /// `pub(crate)`: the per-frame snapshot uses the batch
    /// [`images_frame_into`](Self::images_frame_into) instead; this remains the
    /// single-row reference path (and the parity oracle for the batch fill).
    pub(crate) fn images_row_into(&self, row: usize, out: &mut Vec<(usize, aterm_grid::ImageRef)>) {
        out.clear();
        let Ok(visible_row) = u16::try_from(row) else {
            return;
        };
        let grid = self.grid();
        let offset = grid.display_offset();
        if usize::from(visible_row) < offset {
            // Scrolled-back row: its pictures live on the history line.
            //
            // Kitty Unicode placeholder cells are not resolved for a history row
            // (as in the batch fill): `placeholder_image_ref` reads the live
            // grid's `resolved_char`, which is not this row's.
            push_history_images(grid, visible_row, out);
            // Stable: a cell's layers keep their draw order.
            out.sort_by_key(|&(col, _)| col);
            return;
        }
        // Fast path: with no extras anywhere there are no image cells, so the
        // common case (plain text) pays a single bool check.
        if grid.extras().is_empty() {
            return;
        }
        // The live extras map is keyed by screen row; at a scrolled viewport
        // the live rows sit `display_offset` slots lower. Identity when not
        // scrolled.
        let screen_row = visible_row.saturating_sub(u16::try_from(offset).unwrap_or(u16::MAX));
        // Scan the full grid width, not `grid_row.len()`: an image cell carries
        // only an extra, so the row may not be materialized; `cell_extra` reads
        // the extras map directly.
        if screen_row >= grid.rows() {
            return;
        }
        // A Kitty Unicode placeholder can only resolve to a stored image, and
        // `placeholder_image_ref` starts its lookup with
        // `self.transient.kitty_images.get(&image_id)?` — so with no images
        // transmitted it is provably `None` for every cell.
        // Hoisting the emptiness test skips its `resolved_char` probe entirely;
        // `transient` cannot change under this `&self` borrow, so the gate is
        // behaviour-identical.
        let has_kitty = !self.transient.kitty_images.is_empty();
        for col in 0..grid.cols() {
            #[cfg(test)]
            image_probe_meter::charge();
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

    /// Every visible row's inline-image placements in one pass over the extras
    /// map: the allocating frame-level twin of
    /// [`images_row`](Self::images_row), with the same rows, column order and
    /// `ImageRef`s as calling it for every row. Used for off-frame
    /// whole-screen reads (the control socket's styled frame); the windowed
    /// frontend uses [`cell_frame_into`](Self::cell_frame_into).
    #[must_use]
    pub fn images_frame(&self, rows: usize) -> Vec<Vec<(usize, aterm_grid::ImageRef)>> {
        let mut images = Vec::new();
        self.images_frame_into(&mut images, rows);
        images
    }

    /// Batch equivalent of [`images_row_into`](Self::images_row_into) for every
    /// row in `0..rows`, driven by one pass over the extras map instead of
    /// `rows x cols` probes, so the cost is proportional to the extras present.
    ///
    /// Output matches the per-row accessor: entries outside the current grid
    /// (stale after a shrink) are dropped, and each row is stably sorted by
    /// column (map iteration order is arbitrary; each cell's layers stay
    /// bottom first).
    ///
    /// The outer Vec is resized keeping existing inner Vecs, and each inner Vec
    /// is cleared in place so its capacity survives across frames.
    fn images_frame_into(&self, images: &mut Vec<Vec<(usize, aterm_grid::ImageRef)>>, rows: usize) {
        images.resize_with(rows, Vec::new);
        for row in images.iter_mut() {
            row.clear();
        }
        let grid = self.grid();
        let offset = grid.display_offset();
        // Scrolled-back rows first: viewport rows `0..display_offset` are
        // history, whose pictures ride the history lines' image spans rather
        // than the live extras map. Each row's materialization is memoized
        // (`viewport_row_cache`).
        //
        // Kitty Unicode placeholder cells are not resolved here:
        // `placeholder_image_ref` reads the live grid's `resolved_char`, not the
        // history row's. Direct placements (OSC 1337, sixel) are restored.
        for (r, images_row) in images.iter_mut().enumerate().take(rows.min(offset)) {
            let Ok(visible_row) = u16::try_from(r) else {
                break;
            };
            push_history_images(grid, visible_row, images_row);
        }
        // No extras anywhere means no live image cells: plain text pays one
        // bool check.
        if grid.extras().is_empty() {
            for row in images.iter_mut() {
                row.sort_by_key(|&(col, _)| col);
            }
            return;
        }
        // `iter()` yields external coordinates with stale entries filtered;
        // clamp the screen row to the grid and the viewport row to `rows` (they
        // differ during a resize and by `display_offset` while scrolled back).
        let max_screen_row = usize::from(grid.rows());
        // With no transmitted Kitty images, `placeholder_image_ref` is provably
        // `None` for every entry (every `Some` return threads through
        // `self.transient.kitty_images.get(&image_id)?`), so the whole
        // placeholder branch — a `resolved_char` row lookup + cell read per
        // non-image extra, i.e. per hyperlink, combining mark and underline
        // colour on screen — is dead work. Hoist the emptiness test out of the
        // loop: it is a pure `&self` read and `transient` cannot change while
        // this borrow is live, so the emitted rows are byte-identical.
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
            // The map is keyed by screen row; at a scrolled viewport a live row
            // sits `display_offset` slots lower (identity at 0).
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
        // Stable: map iteration yields each cell once, its layers in draw
        // order, and the sort must keep them so.
        for row in images.iter_mut() {
            row.sort_by_key(|&(col, _)| col);
        }
        // A floating placement's tiles join the cells' own layers in draw
        // order.
        if !self.transient.kitty_floating.is_empty() {
            for row in images.iter_mut() {
                order_image_layers(row);
            }
        }
    }

    /// If the cell at (`row`,`col`) is a Kitty Unicode placeholder (U+10EEEE),
    /// decode it ([`decode_cell`](super::kitty_placeholder::decode_cell)) and
    /// return an [`ImageRef`](aterm_grid::ImageRef) into the image its virtual
    /// placement shows. The pixel-exact sub-tile blit is the renderer's existing
    /// ImageRef job, so a virtual placement reuses the proven direct-placement
    /// compositor. Returns `None` for any non-placeholder cell, an unknown image
    /// id, or a placement id the image has no virtual placement for.
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
    /// laid it out. A placement id the image has no virtual placement for
    /// draws nothing.
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
    /// `(screen row, col, tile)` for every footprint cell on screen. Empty, and
    /// free, while there are none.
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

    /// Build the engine's render snapshot for one frame: a plain-owned
    /// [`RenderInput`](crate::render::RenderInput) a renderer paints without
    /// any `&Terminal` borrow, freshly allocated.
    ///
    /// [`cell_frame_into`](Self::cell_frame_into) is the allocation-reusing
    /// per-frame path; this wrapper allocates then delegates. Live default
    /// background and cursor colors are resolved with the cells (OSC resets,
    /// dynamic cursor fallback, DECSCNM).
    ///
    /// `&mut self` because the snapshot is stamped with
    /// [`damage_epoch`](Self::damage_epoch), which latches.
    #[must_use]
    pub fn cell_frame(&mut self, rows: usize, cols: usize) -> crate::render::RenderInput {
        let mut scratch = crate::render::RenderInput::empty();
        self.cell_frame_into(&mut scratch, rows, cols);
        scratch
    }

    /// Like [`cell_frame`](Self::cell_frame), but refills a caller-owned
    /// `scratch` in place: the per-frame path the windowed frontend calls under
    /// the `Terminal` lock.
    ///
    /// The per-row containers (`cells`, `clusters`, `combining`) are resized to
    /// `rows` keeping their inner Vecs, and each row is cleared and refilled,
    /// so with stable dimensions nothing reallocates except per-cluster owned
    /// content (`Box<str>` per emoji cluster, `Box<[char]>` per combining cell).
    /// Plain rows are allocation-free in steady state. Never `.clear()` the
    /// outer containers: that drops the inner Vecs' capacity. `line_size_spans`
    /// and `default_bg_spans` rows are cleared, since an engine snapshot is
    /// never a composed split frame. The data matches
    /// [`cell_frame`](Self::cell_frame).
    ///
    /// The snapshot's [`snapshot_seq`](crate::render::RenderInput::snapshot_seq)
    /// is [`damage_epoch`](Self::damage_epoch), read under the caller's lock,
    /// so a later `damage_epoch()` detects staleness.
    pub fn cell_frame_into(
        &mut self,
        scratch: &mut crate::render::RenderInput,
        rows: usize,
        cols: usize,
    ) {
        self.cell_frame_fill(scratch, rows, cols, None);
    }

    /// Damage-scoped variant of [`cell_frame_into`](Self::cell_frame_into) that
    /// also consumes the damage session.
    ///
    /// Refills only the damage tracker's damaged rows into the persistent
    /// `scratch` when the scratch is provably an identical baseline for the
    /// undamaged rows, and falls back to the full refill otherwise. Either way
    /// it then calls `take_damage()` and restamps the scratch's continuity
    /// tokens so the next frame can chain. The `Full` arm names the refusing
    /// clause ([`FullRefillCause`](crate::render::FullRefillCause)).
    ///
    /// The continuity proof:
    /// - `terminal_id` match (nonzero): this terminal last filled the scratch,
    ///   so a compositor scratch shared across panes cannot leak rows.
    /// - `extract_gen` match: no consumer took damage since the fill, so the
    ///   tracker's bits are a superset of the rows that changed.
    /// - `snapshot_seq == engine_fill_seq`: no host mutator (stream fade,
    ///   prediction ghosts, strip splice) wrote content since the fill.
    /// - dims and `cells.len()` match.
    /// - `display_offset == 0` on both sides: tracker rows are live-grid rows,
    ///   which coincide with viewport rows only at offset 0. A scratch parked in
    ///   history may instead pass the scrolled proof
    ///   ([`scrolled_scoped_refill_delta`](Self::scrolled_scoped_refill_delta)).
    /// - `base_y` / `absolute_row_revision` match: no scroll or protected-footer
    ///   insertion shifted retained rows (scroll damage marks only the exposed
    ///   strip, so bits alone cannot describe a shift).
    /// - alt bit match, and `!Damage::Full`.
    /// - `engine_row_order == RowOrder::Logical`: the BiDi reorder is in-place
    ///   and not idempotent, so only rows the previous fill did not permute may
    ///   be retained. Mode/direction setters call `invalidate_bidi_all`, which
    ///   marks full damage.
    ///
    /// Row granularity only: ligatures and clusters couple columns, so the
    /// tracker's column bounds are not used.
    ///
    /// Cost: `O(damaged rows × cols + rows)`; the `O(rows)` floor is the scalar
    /// restamp and mask scan, and the images pass stays a full extras-map walk.
    /// Equality with the full path is pinned by
    /// `damage_scoped_extraction_matches_full_extract_over_mutation_corpus`.
    pub fn cell_frame_damage_scoped_into(
        &mut self,
        scratch: &mut crate::render::RenderInput,
        rows: usize,
        cols: usize,
    ) -> crate::render::FrameRefill {
        let Some(cause) = self.damage_scoped_refill_refusal(scratch, rows, cols) else {
            // Copy the tracker's row bits into the resident mask before the
            // fill (the tracker read borrows `&self`, the fill needs `&mut
            // self`); the mask is taken out and put back, so its capacity
            // survives and a steady-state frame allocates nothing.
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
            // The fill stamped the pre-take generation; `take_damage` just
            // bumped it. This scratch closed the session, so it stays the valid
            // baseline for the next frame.
            scratch.extract_gen = self.extract_gen;
            return crate::render::FrameRefill::Scoped { rows_refilled };
        };
        // A viewport parked in history has its own continuity proof, which
        // reads no tracker bits (they index live rows, not viewport rows). The
        // exposed strip comes from the offset delta, and "no other row
        // changed" from the content generation. Consulted only under
        // `EngineScrolled`, after the ordered check; entering history from the
        // live bottom (a scratch at offset 0) still reports `EngineScrolled`.
        if cause == crate::render::FullRefillCause::EngineScrolled
            && let Some(delta) = self.scrolled_scoped_refill_delta(scratch, rows)
        {
            let mut mask = std::mem::take(&mut self.refill_mask_scratch);
            Self::rotate_scrolled_scratch(scratch, rows, delta, &mut mask);
            self.cell_frame_fill(scratch, rows, cols, Some(&mask));
            self.refill_mask_scratch = mask;
            // Debug check: the scrolled proof reads no tracker bit, so a mutator
            // that rewrites a cell with only partial damage would slip past it;
            // debug builds compare against a full extraction. Runs before
            // `take_damage`, so the oracle reads the same grid.
            #[cfg(debug_assertions)]
            {
                // Seeded from the scratch, not `empty()`: the fill never writes
                // the host-owned overlay channels, yet content `PartialEq`
                // compares them. A `None`-mask refill of the clone rewrites every
                // engine-owned channel, so only an engine divergence differs.
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
        // Any continuity break lands here: always sound, and it re-establishes
        // a valid baseline. The refusing clause rides out with the arm.
        self.cell_frame_fill(scratch, rows, cols, None);
        self.take_damage();
        scratch.extract_gen = self.extract_gen;
        crate::render::FrameRefill::Full { cause }
    }

    /// The damage-scoped continuity check (clauses justified on
    /// [`cell_frame_damage_scoped_into`](Self::cell_frame_damage_scoped_into)).
    /// A pure read; any refusal only costs the full refill.
    ///
    /// `None` means every clause held and the scoped arm is legal;
    /// `Some(cause)` names the first clause that refused.
    ///
    /// Clause order is part of each cause's meaning (a scrolled frame that
    /// also took full damage reports the scroll) and is pinned by the corpus
    /// tests, so do not reorder.
    fn damage_scoped_refill_refusal(
        &self,
        scratch: &crate::render::RenderInput,
        rows: usize,
        cols: usize,
    ) -> Option<crate::render::FullRefillCause> {
        use crate::render::FullRefillCause as Cause;
        // BiDi reorder is an in-place, non-idempotent row permutation, so
        // retained rows may only be kept while still in logical order, which
        // `engine_row_order` records for the last engine fill. Mode/direction
        // changes call `invalidate_bidi_all`, which marks full damage and is
        // caught by the `!is_full()` clause below.
        if scratch.engine_row_order != crate::render::RowOrder::Logical {
            return Some(Cause::BidiVisual);
        }
        let grid = self.grid();
        // Same conversion the fill stamps, so the comparison can never differ
        // by conversion policy alone.
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
        // No host row prepend in force: a spliced scratch holds engine row `r`
        // at index `r + row_shift`, so retaining "row r" would serve another
        // row's content. The frontend undoes its tab-strip prepend
        // (`RenderInput::undo_host_row_prepend`) before handing the scratch
        // back; anything it could not undo takes the full arm here.
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
    ///
    /// Consulted by [`cell_frame_damage_scoped_into`](Self::cell_frame_damage_scoped_into)
    /// only after [`damage_scoped_refill_refusal`](Self::damage_scoped_refill_refusal)
    /// refused under `EngineScrolled`, so every earlier clause (identity,
    /// generation, host mutation, row shift, alt bit, dims) held. Returns the
    /// signed offset delta (positive = viewport moved up into older history),
    /// or `None` to keep the full arm.
    ///
    /// Viewport row `r` shows absolute row `base_y - display_offset + r`. With
    /// `base_y` pinned, an offset change of `delta` maps every retained row to
    /// `r + delta`, and only `|delta|` rows are new. The strip comes from the
    /// two offsets, never the tracker, whose per-call exposure marks undercount
    /// a net move made of several `scroll_display`s.
    ///
    /// Clauses:
    /// - the scratch is parked in history (`display_offset > 0`); entering
    ///   history from offset 0 keeps the full arm.
    /// - `content_seq` match: the grid's `content_gen` (also the key of the
    ///   viewport-row memo in `viewport_row_cache.rs`) is bumped by every cell,
    ///   line or scrollback write, including direct `Damage::Full` assignments,
    ///   and not by a pure viewport scroll. Partial marks that do not bump it:
    ///   the cursor cell (restamped every fill), the display-offset strip (this
    ///   proof's subject), image placement (the images pass walks the whole
    ///   frame) and Kitty CSI +T unscroll (caught below).
    /// - `history_renumber_epoch` match: absolute keys did not shift wholesale.
    /// - `base_y` match: the live grid did not scroll under the offset.
    /// - `absolute_row_revision` match: no protected-footer insertion.
    /// - `!Damage::Full`: palette / DECSCNM / reflow may move no `content_gen`.
    /// - `clusters` and `combining` have the scratch's height, so they rotate.
    /// - `|delta| < rows`: a compound move of small steps can stay partial yet
    ///   expose the whole viewport.
    ///
    /// A scrolled fill publishes no row-revision lane (`row_rev` is indexed by
    /// live rows).
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
        // Same conversion the fill stamps, so the comparison can never differ
        // by conversion policy alone.
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
    ///
    /// `delta > 0`: the viewport moved up, so row `r` moves to `r + delta`
    /// (`rotate_right`) and rows `0..delta` are exposed. `delta < 0`: moved
    /// down, `rotate_left`, and the bottom `|delta|` rows are exposed. The
    /// rotated-out inner Vecs land on the exposed rows, which the masked fill
    /// clears and refills in place.
    ///
    /// Caller guarantees `|delta| < rows` and all three channels have `rows`
    /// entries, so the rotations cannot panic.
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
    /// which a sub-row up-glide slides in. For `rows >= d` that is live screen
    /// row `rows - d`, read offset-independently (as `render_row_at_screen`
    /// does); deeper in history it is the history row `visible_row_view`
    /// resolves for index `rows`. At `d == 0` the apron is absent. No images:
    /// the renderer's image pass is keyed to viewport rows.
    fn apron_row_into(&self, scratch: &mut crate::render::RenderInput, rows: usize) {
        let d = self.grid().display_offset();
        // The caret is a present-time scalar, not row content: moving or hiding
        // it does not bump `content_gen`. Refresh it even when the row memo
        // hits, so a sub-row glide cannot paint yesterday's caret in its strip.
        let cursor_col = (self.cursor_visible() && self.projected_cursor_row() == rows)
            .then(|| self.cursor().col as usize);
        // The apron row is a pure function of the offset, content generation,
        // history renumbering, frame shape, presentation and filling terminal
        // (not the caret), so a stationary repaint re-reads nothing, while a
        // recolor or another tab's fill into this shared scratch refreshes it.
        let stamp = crate::render::ApronStamp {
            offset: d,
            content_gen: self.grid().content_gen(),
            renumber_epoch: self.grid().history_renumber_epoch(),
            rows,
            cols: scratch.cols,
            // Nothing below the live bottom to resolve: skip the fingerprint.
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
            // Deeper in history than the frame is tall: viewport index `rows` is
            // itself a history row, which `visible_row_view` resolves directly.
            self.render_row_into_impl(
                rows,
                &mut ap.cells,
                Some((&mut ap.clusters, &mut ap.combining)),
                false,
            );
            // The ring keeps a scrolled-off row's DEC line size, and `row(vr)`
            // reports it for an in-viewport history index — so must the apron.
            ap.line_size = u16::try_from(rows)
                .ok()
                .and_then(|vr| self.grid().row_past_viewport(vr))
                .map_or(
                    crate::grid::LineSize::SingleWidth,
                    crate::grid::Row::line_size,
                );
        }
        // When the cursor's projected row (`projected_cursor_row`) is exactly
        // the apron row, carry the caret so it rides in with the row. The apron
        // skips `apply_bidi_reorder`, so a row whose visual order differs from
        // its logical order gets the placeholder instead. Keyed on the row's
        // order, not `bidi_mode`, which is `Implicit` in the shipping app.
        #[cfg(feature = "bidi")]
        if self
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

    /// Everything a row is resolved against beside the grid, none of which
    /// moves `content_gen` (their mutators only mark `Damage::Full`, and a
    /// config reload changes the style policy with no output at all): the live
    /// palette, default colours, DECSCNM, the style policy (`bold_is_bright`,
    /// `faint_opacity`) and the BiDi modes. A cache of resolved rows keys on
    /// this value: the apron memo, and `screen since=<etag>` through
    /// `aterm-gui`'s `render_sig`. An input the resolver gains belongs here,
    /// or both serve stale colours.
    #[must_use]
    pub fn presentation_fingerprint(&self) -> u64 {
        use std::hash::Hasher as _;
        let mut h = aterm_hash::FxHasher::default();
        let palette = self.color_palette();
        for i in 0..=u8::MAX {
            h.write_u32(pack_rgb(palette.get(i)));
        }
        h.write_u32(pack_rgb(self.default_foreground()));
        h.write_u32(pack_rgb(self.default_background()));
        let modes = self.modes();
        h.write_u8(u8::from(modes.reverse_video()));
        h.write_u8(u8::from(self.color.bold_is_bright));
        h.write_u32(self.color.faint_opacity.to_bits());
        h.write_u8(modes.bidi_mode as u8);
        h.write_u8(modes.bidi_direction as u8);
        h.write_u8(u8::from(modes.bidi_autodetection));
        h.finish()
    }

    /// The shared fill behind [`cell_frame_into`](Self::cell_frame_into)
    /// (`refill_mask: None`, every row) and the damage-scoped arm
    /// (`Some(mask)`, only marked rows re-resolve). Everything except the
    /// per-row cell loop runs identically on both arms, so the scoped arm
    /// cannot skew a scalar or non-cell channel.
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

        // Resize the per-row containers to `rows`, keeping existing inner Vecs,
        // then refill each row in place. One combined pass fills cells,
        // emoji clusters and combining marks, probing the extras map once per
        // cell; `cells[r]` / `clusters[r]` / `combining[r]` are disjoint fields,
        // so the three borrows coexist.
        scratch.cells.resize_with(rows, Vec::new);
        scratch.clusters.resize_with(rows, Vec::new);
        scratch.combining.resize_with(rows, Vec::new);
        for r in 0..rows {
            // Damage-scoped arm: rows the tracker did not mark are identical in
            // the scratch (the caller's proof), so their refill is skipped.
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
        // The apron row sits at viewport index `rows`, outside any tracker
        // mask, and changes whenever output lands below a scrolled-back
        // viewport, so it is stamped on every fill.
        self.apron_row_into(scratch, rows);

        // Images are a separate pass: an image cell carries only an extra, so
        // its row may be unmaterialized and the cell loop would miss it. The
        // batch fill walks the extras map once per frame.
        self.images_frame_into(&mut scratch.images, rows);

        // A single terminal's frame is uniform per row, so it carries no
        // line-size runs; clear any a previous split frame left in this reused
        // scratch.
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
        // `scratch` is also reused by the GUI's split compositor. A subsequent
        // direct terminal snapshot must not inherit pane-local DEC geometry from
        // that composed frame. Preserve each inner Vec's capacity while resetting
        // the semantic row count and contents.
        scratch.line_size_spans.resize_with(rows, Vec::new);
        for spans in &mut scratch.line_size_spans {
            spans.clear();
        }
        scratch.default_bg_spans.resize_with(rows, Vec::new);
        for spans in &mut scratch.default_bg_spans {
            spans.clear();
        }
        // Like the pane spans above, the selection clip belongs to a composed
        // frame, never to the terminal snapshot itself. A direct single-pane
        // extraction restores the historical unbounded selection predicate.
        scratch.selection_clip = None;
        // Likewise the per-pane list: a non-empty `selections` makes the
        // renderer read it instead of the scalar selection stamped here.
        scratch.selections.clear();
        // The host's pixel-resolution chrome rows (the message band's meter)
        // are painted onto a composed frame after this extraction; a bare
        // snapshot carries none.
        scratch.chrome_rasters.clear();

        let cur = self.cursor();
        scratch.cursor_col = cur.col as usize;
        let display_offset = self.grid().display_offset();
        // The DEC cursor is anchored in the active grid; its viewport row is
        // that row pushed down by the scrollback offset. Show it only while the
        // projected row is on screen (as xterm/kitty do). Host copy/vi modes
        // override this after extraction. Both halves come from
        // [`Terminal::projected_cursor_row`] / [`Terminal::cursor_row_on_screen`],
        // which hosts also use to locate a pane's caret.
        scratch.cursor_row = self.projected_cursor_row();
        scratch.cursor_visible = self.cursor_row_on_screen(rows).is_some();
        scratch.cursor_style = self.cursor_style();
        scratch.display_offset = display_offset as i32;
        // Capture base_y (absolute row of the top visible line) under the same
        // lock as the cells, so a host re-anchoring absolute rows (find
        // highlight) uses a consistent value.
        scratch.base_y = i64::try_from(self.grid().base_y()).unwrap_or(i64::MAX);
        // Capture the protected-footer insertion revision at the same extraction
        // boundary. A host can therefore fail closed when retained absolute-row
        // geometry no longer describes this exact frame.
        scratch.absolute_row_revision = self.absolute_row_revision();
        // Also the wholesale renumbering the footer revision cannot see: a
        // width reflow renumbers every retained row without moving
        // `absolute_row_revision`.
        scratch.history_renumber_epoch = self.grid().history_renumber_epoch();
        // `clone_from` reuses the destination's existing allocation where the
        // selection's owned data permits, instead of dropping + reallocating.
        scratch.selection.clone_from(self.text_selection());
        scratch.selection_bg = self
            .selection_background()
            .map_or(crate::render::COLOR_UNSET, pack_rgb);
        scratch.selection_fg = self
            .selection_foreground()
            .map_or(crate::render::COLOR_DYNAMIC, pack_rgb);
        // These colors describe the terminal snapshot, not host presentation
        // policy, and are stamped with the cells so every `cell_frame*`
        // consumer sees one coherent OSC/DECSCNM state. An unmaterialized tail
        // is `Cell::EMPTY`: its background follows OSC 11 and swaps under
        // DECSCNM. A dynamic cursor (OSC 21 `cursor=`) follows OSC 10.
        let reverse_video = self.modes().reverse_video();
        // One blank cell feeds both stamps, so they are coherent (DECSCNM swaps
        // the pair together), but each has its own authority test: with only
        // OSC 10 configured, the background must stay unset so the renderer
        // keeps its host-theme fallback. DECSCNM makes both authoritative.
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

        // Stamp the snapshot with the engine's monotone damage epoch (A-3 seq).
        // O(1), and idempotent within a damage session, so reading it here is free
        // even when the frontend also reads it for its present early-out.
        scratch.snapshot_seq = self.damage_epoch();
        scratch.content_seq = self.content_seq();
        scratch.process_sequence = self.transient.pipeline_timestamps.process_sequence;

        // Continuity tokens, stamped by every engine fill (full or scoped).
        // A host mutator that later bumps `snapshot_seq` thereby forces the
        // next fill to the full arm.
        scratch.terminal_id = self.extract_identity;
        scratch.extract_gen = self.extract_gen;
        scratch.engine_alt = self.is_alternate_screen();
        scratch.engine_fill_seq = scratch.snapshot_seq;
        // An engine fill is unshifted: any host row prepend is gone, so the
        // shift count is zero and the provenance token is re-armed. Only a
        // blessed prepend (`note_host_row_prepend`) keeps them in step
        // afterwards.
        scratch.row_shift = 0;
        scratch.shifted_fill_seq = scratch.snapshot_seq;
        // An engine fill is never a composite, so any composed-frame blessing
        // left in this reused buffer is disowned.
        scratch.composed_fill_seq = 0;

        // Per-row revision lane: publish which rows changed so consumers need
        // not compare every cell. `row_revisions` folds the current damage
        // session first. Copied per row since the snapshot outlives the lock;
        // a host that later re-shapes the rows leaves the length mismatched,
        // which the consumer treats as untrusted.
        scratch.row_rev.clear();
        {
            let revisions = self.grid_mut().row_revisions();
            scratch.row_rev.extend_from_slice(revisions);
        }
        scratch.row_rev.resize(rows, 0);
        // The engine fill vouches for the lane except in two cases:
        //
        // `Damage::Full` keeps no mark clock, so the fold cannot tell a repeated
        // read of one full session from a new write; re-stamping every row
        // would turn identical extracts into full repaints. Publishing no lane
        // falls back to the exact whole-grid compare.
        //
        // A scrolled fill: the lane is indexed by live rows, but this
        // snapshot's rows are viewport rows. The fold still runs so the clock
        // keeps its marks; `compute_dirty_rows` already requires
        // `display_offset == 0` before trusting a lane.
        scratch.row_rev_lane = if self.grid().damage().is_full() || display_offset != 0 {
            0
        } else {
            self.extract_identity
        };

        // Every row starts in logical order: an identity map per row, kept as
        // an empty inner Vec so a permuted row's buffer is reused. A row the
        // scoped arm retains is the identity (it may retain rows only after a
        // fill that permuted none), so clearing it is exact.
        for map in &mut scratch.visual_to_logical {
            map.clear();
        }
        scratch.visual_to_logical.resize_with(rows, Vec::new);

        // BiDi visual reordering (feature `bidi`): permute each row into visual
        // order for both renderers and the `image` capture (see
        // `apply_bidi_reorder`), recording each permuted row's map. It is
        // mask-aware and reports whether it permuted anything, so the next
        // refill can require logical order. Feature-off builds stamp `false`.
        #[cfg(feature = "bidi")]
        {
            scratch.engine_row_order = if self.apply_bidi_reorder(scratch, refill_mask) {
                crate::render::RowOrder::BidiVisual
            } else {
                crate::render::RowOrder::Logical
            };
        }
        #[cfg(not(feature = "bidi"))]
        {
            scratch.engine_row_order = crate::render::RowOrder::Logical;
        }
    }
}

/// A combining char that marks its cell as a multi-codepoint emoji sequence:
/// ZWJ (U+200D), a skin-tone modifier (U+1F3FB–U+1F3FF), combining enclosing
/// keycap (U+20E3), a regional indicator (U+1F1E6–U+1F1FF, the second half of
/// a flag pair), or a tag character (U+E0020–U+E007F, a 🏴 subdivision flag).
/// VS15/VS16 are presentation selectors, not sequence markers.
#[inline]
fn is_emoji_sequence_marker(c: char) -> bool {
    matches!(
        c as u32,
        0x200D | 0x20E3 | 0x1F3FB..=0x1F3FF | 0x1F1E6..=0x1F1FF | 0xE0020..=0xE007F
    )
}

/// A combining char that attaches to its base but has NO glyph of its own, so
/// it must never reach the CPU/GPU mark loops — they blit whatever `glyph_key`
/// returns for a mark, and for one of these that is the primary's `.notdef`
/// box stamped over the base. The presentation selectors VS15/VS16
/// (U+FE0E/U+FE0F) only widen or narrow the base; plane 14 (U+E0000–U+E0FFF:
/// Tags, the Variation Selectors Supplement, and the reserved gaps) is
/// Default_Ignorable_Code_Point end to end, so a lone tag after a letter or an
/// ideographic variation selector after its ideograph draws the base alone.
/// Filtering in this one frame builder, which both renderers consume, keeps
/// CPU/GPU parity by construction.
#[inline]
fn draws_no_glyph(c: char) -> bool {
    matches!(c as u32, 0xFE0E | 0xFE0F | 0xE0000..=0xE0FFF)
}

/// `0x00RRGGBB`: the packing of the snapshot's colour scalars and the apron fingerprint.
fn pack_rgb(c: aterm_types::Rgb) -> u32 {
    u32::from_be_bytes([0, c.r, c.g, c.b])
}

/// A scrolled-back viewport row's direct image placements, in `extras_iter`
/// order (callers sort), expanded from the history line's image spans. A
/// no-op for a row that is not history.
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

/// Deterministic cost meter for the two inline-image readers, in the tree's
/// established shape (`aterm_grid::test_counters`): a complexity claim about a
/// screen read is asserted as an operation count, not as wall-clock, which no
/// test can hold steady.
///
/// One charge per extras lookup the reader performs — a `cell_extra` probe in
/// the per-row sweep, a visited map entry in the batch pass — so the two are
/// directly comparable and `images_frame_costs_the_extras_not_rows_times_cols`
/// can pin the difference between them.
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

    /// OSC 8 hyperlinked cells render underlined, unless the program chose its
    /// own decoration.
    #[test]
    fn a_hyperlink_is_underlined_and_a_program_decoration_still_wins() {
        let mut term = Terminal::new(2, 32);
        // The phishing shape: the text says one host, the link addresses another.
        term.process(b"go\x1b]8;;https://evil.example/steal\x1b\\ogle\x1b]8;;\x1b\\.com");
        let row = term.render_row(0);
        let underlines = row
            .iter()
            .take(10)
            .map(|cell| (cell.ch, cell.underline))
            .collect::<Vec<_>>();
        assert_eq!(
            underlines,
            vec![
                ('g', UnderlineStyle::None),
                ('o', UnderlineStyle::None),
                ('o', UnderlineStyle::Single),
                ('g', UnderlineStyle::Single),
                ('l', UnderlineStyle::Single),
                ('e', UnderlineStyle::Single),
                ('.', UnderlineStyle::None),
                ('c', UnderlineStyle::None),
                ('o', UnderlineStyle::None),
                ('m', UnderlineStyle::None),
            ],
            "exactly the linked cells carry the mark"
        );
        // The line takes the text's own ink: no link colour the theme never chose.
        assert!(
            row.iter()
                .take(10)
                .all(|cell| cell.underline_color.is_none()),
            "a link underline must not invent a colour"
        );

        // A decoration the program chose outranks the link's: an author who
        // styled their own link keeps their styling.
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

    #[test]
    fn render_row_out_of_range_is_empty() {
        let term = Terminal::new(2, 4);
        assert!(term.render_row(99).is_empty());
    }

    #[test]
    fn cell_frame_into_clears_compositor_pane_spans() {
        let mut term = Terminal::new(2, 8);
        let mut scratch = crate::render::RenderInput::empty();
        scratch.line_size_spans = vec![
            vec![crate::render::LineSizeSpan::new(
                0,
                4,
                crate::grid::LineSize::DoubleWidth,
            )],
            vec![crate::render::LineSizeSpan::new(
                5,
                8,
                crate::grid::LineSize::DoubleHeightTop,
            )],
            vec![crate::render::LineSizeSpan::new(
                0,
                8,
                crate::grid::LineSize::DoubleHeightBottom,
            )],
        ];
        scratch.default_bg_spans = vec![
            vec![crate::render::DefaultBgSpan::new(0, 4, 0x0011_2233)],
            vec![crate::render::DefaultBgSpan::new(5, 8, 0x0044_5566)],
            vec![crate::render::DefaultBgSpan::new(0, 8, 0x0077_8899)],
        ];
        scratch.selection_clip = Some(crate::render::SelectionClip::new(0, 2, 5, 8));
        scratch.selections = vec![crate::render::PaneSelection {
            selection: crate::selection::TextSelection::new(),
            clip: crate::render::SelectionClip::new(0, 2, 5, 8),
            bg: 0x0001_0203,
            fg: 0x0004_0506,
            inactive: true,
        }];
        let line_capacities = [
            scratch.line_size_spans[0].capacity(),
            scratch.line_size_spans[1].capacity(),
        ];
        let bg_capacities = [
            scratch.default_bg_spans[0].capacity(),
            scratch.default_bg_spans[1].capacity(),
        ];

        term.cell_frame_into(&mut scratch, 2, 8);

        assert_eq!(scratch.line_size_spans.len(), 2);
        assert!(
            scratch.line_size_spans.iter().all(Vec::is_empty),
            "a direct terminal snapshot must not inherit split-compositor spans"
        );
        assert_eq!(scratch.default_bg_spans.len(), 2);
        assert!(
            scratch.default_bg_spans.iter().all(Vec::is_empty),
            "a direct terminal snapshot must not inherit split default provenance"
        );
        for r in 0..2 {
            assert_eq!(scratch.line_size_spans[r].capacity(), line_capacities[r]);
            assert_eq!(scratch.default_bg_spans[r].capacity(), bg_capacities[r]);
        }
        assert_eq!(
            scratch.selection_clip, None,
            "a direct terminal snapshot must not inherit a split selection clip"
        );
        assert!(
            scratch.selections.is_empty(),
            "…nor the per-pane list, which would REPLACE the scalar selection"
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

        // Negative control for the kept-scratch path: extraction must overwrite
        // stale host values rather than accidentally preserving a prior frame.
        let mut scratch = crate::render::RenderInput::empty();
        scratch.default_bg = 0x0001_0203;
        scratch.cursor_color = 0x0004_0506;
        term.process(b"\x1b[?5h"); // DECSCNM reverse video.
        term.cell_frame_into(&mut scratch, 2, 8);
        assert_eq!(
            scratch.default_bg, 0x00aa_bbcc,
            "DECSCNM makes the live default foreground the implicit blank background",
        );
        assert_eq!(
            scratch.cursor_color, 0x00de_adbe,
            "DECSCNM does not replace an explicit OSC 12 cursor color",
        );

        // An empty OSC 21 cursor value selects dynamic behavior. Its frame color
        // must follow a later OSC 10 update without any host-side restamping.
        term.process(b"\x1b]21;cursor=\x1b\\\x1b]10;rgb:fe/01/7f\x1b\\");
        let dynamic = term.cell_frame(2, 8);
        assert_eq!(dynamic.default_bg, 0x00fe_017f);
        assert_eq!(dynamic.cursor_color, 0x00fe_017f);

        // Reset every live input to its host-configured baseline and leave
        // DECSCNM. This distinguishes reset semantics from hard-coded theme
        // literals inside a consumer.
        term.process(b"\x1b[?5l\x1b]110\x07\x1b]111\x07\x1b]112\x07");
        term.cell_frame_into(&mut scratch, 2, 8);
        assert_eq!(scratch.default_bg, 0x0044_5566);
        assert_eq!(scratch.cursor_color, 0x0077_8899);
    }

    #[test]
    fn implicit_blank_uses_live_defaults_and_decscnm() {
        let mut term = Terminal::new(2, 8);
        let fg = aterm_types::Rgb::new(0x12, 0x34, 0x56);
        let bg = aterm_types::Rgb::new(0xA1, 0xB2, 0xC3);
        term.set_default_foreground(fg);
        term.set_default_background(bg);

        let normal = term.implicit_blank_render_cell();
        assert_eq!(normal.ch, ' ');
        assert_eq!(normal.fg, [fg.r, fg.g, fg.b]);
        assert_eq!(normal.bg, [bg.r, bg.g, bg.b]);
        assert!(!normal.wide);
        assert_eq!(normal.underline, UnderlineStyle::None);

        term.process(b"\x1b[?5h"); // DECSCNM reverse video.
        let reversed = term.implicit_blank_render_cell();
        assert_eq!(
            reversed.fg,
            [bg.r, bg.g, bg.b],
            "DECSCNM swaps the implicit blank's live foreground"
        );
        assert_eq!(
            reversed.bg,
            [fg.r, fg.g, fg.b],
            "DECSCNM swaps the implicit blank's live background"
        );
    }

    #[test]
    fn frame_keeps_the_implicit_blank_from_its_own_extraction() {
        let mut term = Terminal::new(2, 8);
        let mut frame = crate::render::RenderInput::empty();
        // The always-concrete blank is distinct from the renderer-facing
        // COLOR_UNSET scalars of an unconfigured terminal.
        term.cell_frame_into(&mut frame, 2, 8);
        assert_eq!(frame.implicit_blank, term.implicit_blank_render_cell());
        assert_eq!(frame.default_fg, crate::render::COLOR_UNSET);

        term.process(b"\x1b]10;rgb:12/34/56\x1b\\\x1b]11;rgb:a1/b2/c3\x1b\\");
        term.cell_frame_into(&mut frame, 2, 8);
        let extracted = frame.implicit_blank;
        assert_eq!(extracted.fg, [0x12, 0x34, 0x56]);
        assert_eq!(extracted.bg, [0xa1, 0xb2, 0xc3]);
        assert_eq!(frame.clone().implicit_blank, extracted);
        let mut reused = crate::render::RenderInput::empty();
        reused.clone_from(&frame);
        assert_eq!(reused.implicit_blank, extracted);

        // A later terminal mutation cannot recolour a retained cell frame.
        term.process(b"\x1b[?5h");
        assert_ne!(extracted, term.implicit_blank_render_cell());
        assert_eq!(frame.implicit_blank, extracted);
        term.cell_frame_into(&mut frame, 2, 8);
        assert_eq!(frame.implicit_blank, term.implicit_blank_render_cell());
        assert_eq!(frame.implicit_blank.fg, [0xa1, 0xb2, 0xc3]);
        assert_eq!(frame.implicit_blank.bg, [0x12, 0x34, 0x56]);
    }

    #[test]
    fn cell_frame_stamps_absolute_row_revision() {
        let mut term = Terminal::new(5, 10);
        let before = term.cell_frame(5, 10);
        assert_eq!(before.absolute_row_revision, 0);

        // Insert one history row before two protected footer rows: the
        // non-uniform coordinate change the frame stamp fences. The displaced
        // top row must be written; never-written rows scroll without archival.
        term.process(b"\x1b[1;1HA");
        term.process(b"\x1b[1;3r\x1b[3;1H\r\nX\x1b[r");
        assert_eq!(term.absolute_row_revision(), 1);

        let after = term.cell_frame(5, 10);
        assert_eq!(
            after.absolute_row_revision,
            term.absolute_row_revision(),
            "snapshot carries the terminal revision from its extraction boundary"
        );
    }

    /// A width reflow renumbers every retained row without moving
    /// `absolute_row_revision`, so the frame carries the renumber epoch from
    /// the same extraction boundary.
    #[test]
    fn cell_frame_stamps_history_renumber_epoch() {
        let mut term = Terminal::new(5, 10);
        // Lines long enough to soft-wrap at width 10 and deep enough to push
        // history into scrollback — the rows a narrowing rewrap renumbers.
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

    /// The batch `images_frame_into` fill must be byte-identical to the per-row
    /// `images_row` reference for every row — covering a direct iTerm2
    /// placement, a Kitty Unicode placeholder, non-image extras (hyperlink /
    /// colored underline) that must not emit entries, scrolled state (extras
    /// `row_offset` > 0 with stale scrolled-off entries in the map), and a
    /// `rows` request larger than the grid.
    #[test]
    fn images_frame_into_matches_per_row() {
        let mut term = Terminal::new(10, 30);
        term.set_cell_pixel_size(10, 20);

        // Non-image extras: a hyperlink cell and a colored-underline cell make
        // the extras map non-empty (the per-row scan's worst case).
        term.process(b"\x1b]8;;http://example.com\x1b\\L\x1b]8;;\x1b\\");
        term.process(b"\x1b[4m\x1b[58;2;10;20;30mU\x1b[0m\r\n");

        // Direct OSC 1337 placement: 3 cols x 2 rows at rows 1..=2.
        let b64 = aterm_codec::base64::encode(b"not-a-real-png").expect("encode");
        let mut seq = b"\x1b]1337;File=inline=1;width=3;height=2:".to_vec();
        seq.extend_from_slice(b64.as_bytes());
        seq.extend_from_slice(b"\x1b\\");
        term.process(&seq);

        // A stored Kitty image (a=t, id 5) shown via a Unicode placeholder cell.
        let raw = vec![0u8; 10 * 20 * 4];
        let mut apc = b"\x1b_Ga=t,f=32,s=10,v=20,i=5;".to_vec();
        apc.extend_from_slice(
            aterm_codec::base64::encode(&raw)
                .expect("encode")
                .as_bytes(),
        );
        apc.extend_from_slice(b"\x1b\\");
        term.process(&apc);
        let mut ph = b"\x1b[38;5;5m".to_vec();
        let mut buf = [0u8; 4];
        for c in ['\u{10EEEE}', '\u{0305}', '\u{0305}'] {
            ph.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        ph.extend_from_slice(b"\x1b[0m");
        term.process(&ph);

        // Scroll twice from the bottom row: the extras map now carries a
        // nonzero row_offset and stale scrolled-off entries the batch path
        // must filter exactly like the per-row probes do.
        term.process(b"\x1b[10;1H\r\n\r\n");

        let rows = usize::from(term.grid().rows()) + 5; // beyond the grid too
        let mut batch: Vec<Vec<(usize, aterm_grid::ImageRef)>> = Vec::new();
        term.images_frame_into(&mut batch, rows);
        assert_eq!(batch.len(), rows);
        let mut total = 0usize;
        for (r, got) in batch.iter().enumerate() {
            let want = term.images_row(r);
            assert_eq!(got.len(), want.len(), "row {r} entry count");
            for ((gc, gi), (wc, wi)) in got.iter().zip(&want) {
                assert_eq!(gc, wc, "row {r} column order");
                assert!(
                    std::sync::Arc::ptr_eq(&gi.image, &wi.image),
                    "row {r} image identity"
                );
                assert_eq!(
                    (gi.cell_row, gi.cell_col),
                    (wi.cell_row, wi.cell_col),
                    "row {r} tile coordinates"
                );
            }
            total += got.len();
        }
        assert!(total > 0, "the frame must actually contain image cells");
    }

    /// Batch-vs-per-row image parity with the viewport scrolled back over the
    /// image: the batch loop walks the materialized history row's extras while
    /// the per-row accessor probes it.
    #[test]
    fn images_frame_into_matches_per_row_while_scrolled_back() {
        let mut term = Terminal::new(8, 20);
        let b64 = aterm_codec::base64::encode(b"not-a-real-png").expect("encode");
        let mut seq = b"\x1b]1337;File=inline=1;width=3;height=2:".to_vec();
        seq.extend_from_slice(b64.as_bytes());
        seq.extend_from_slice(b"\x1b\\");
        term.process(&seq);
        for i in 0..30 {
            term.process(format!("f{i}\r\n").as_bytes());
        }
        term.scroll_to_top();

        let rows = usize::from(term.grid().rows());
        let mut batch: Vec<Vec<(usize, aterm_grid::ImageRef)>> = Vec::new();
        term.images_frame_into(&mut batch, rows);
        let mut total = 0usize;
        for (r, got) in batch.iter().enumerate() {
            let want = term.images_row(r);
            assert_eq!(got.len(), want.len(), "row {r} entry count");
            for ((gc, gi), (wc, wi)) in got.iter().zip(&want) {
                assert_eq!(gc, wc, "row {r} column order");
                assert!(
                    std::sync::Arc::ptr_eq(&gi.image, &wi.image),
                    "row {r} image identity"
                );
                assert_eq!(
                    (gi.cell_row, gi.cell_col),
                    (wi.cell_row, wi.cell_col),
                    "row {r} tile coordinates"
                );
            }
            total += got.len();
        }
        assert_eq!(
            total, 6,
            "the scrolled-back 3x2 footprint must be fully present in BOTH readers"
        );
    }

    /// Cost as an operation count: the per-row reader probes every cell once
    /// any extra exists (e.g. one hyperlink), while the batch reader pays one
    /// lookup per extra. Both states are asserted: a plain-text screen costs 0
    /// probes either way, a hyperlink screen does not.
    #[test]
    fn images_frame_costs_the_extras_not_rows_times_cols() {
        const ROWS: u16 = 40;
        const COLS: u16 = 120;
        let mut term = Terminal::new(ROWS, COLS);
        let rows = usize::from(ROWS);

        // Plain text: nothing in the extras map, so both readers take the gate.
        term.process(b"plain text, no extras at all\r\n");
        assert!(
            term.grid().extras().is_empty(),
            "no extras on a plain screen"
        );
        let _ = image_probe_meter::take();
        for r in 0..rows {
            let _ = term.images_row(r);
        }
        assert_eq!(
            image_probe_meter::take(),
            0,
            "the empty-map gate must keep the per-row sweep off a plain screen"
        );
        let _ = term.images_frame(rows);
        assert_eq!(
            image_probe_meter::take(),
            0,
            "same gate in the batch reader"
        );

        // One hyperlink, zero images — the case the gate does not rescue.
        term.process(b"\x1b]8;;https://example.com\x1b\\L\x1b]8;;\x1b\\");
        let extras = term.grid().extras().len();
        assert_eq!(extras, 1, "one linked cell is one extras entry");
        let _ = image_probe_meter::take();
        for r in 0..rows {
            let _ = term.images_row(r);
        }
        assert_eq!(
            image_probe_meter::take(),
            rows * usize::from(COLS),
            "the per-row sweep charges the WHOLE screen for one hyperlink"
        );
        let _ = term.images_frame(rows);
        assert_eq!(
            image_probe_meter::take(),
            extras,
            "the batch reader charges the extras that exist, not rows x cols"
        );
    }

    #[test]
    fn render_row_default_colors() {
        let mut term = Terminal::new(2, 8);
        term.process(b"Hi");
        let cells = term.render_row(0);
        assert!(cells.len() >= 2);
        assert_eq!(cells[0].ch, 'H');
        assert_eq!(cells[1].ch, 'i');
        // Default fg/bg come straight from the terminal defaults.
        let fg = term.default_foreground();
        let bg = term.default_background();
        assert_eq!(cells[0].fg, [fg.r, fg.g, fg.b]);
        assert_eq!(cells[0].bg, [bg.r, bg.g, bg.b]);
    }

    /// A bare `Terminal::new()` defaults to the single-source constants in
    /// aterm-types.
    #[test]
    fn terminal_new_defaults_are_single_source() {
        let term = Terminal::new(2, 4);
        assert_eq!(term.default_foreground(), aterm_types::DEFAULT_FOREGROUND);
        assert_eq!(term.default_background(), aterm_types::DEFAULT_BACKGROUND);
    }

    /// OSC 110 (reset default fg) restores the configured (themed) default,
    /// never a transient OSC 10 value nor the spec default.
    #[test]
    fn osc_110_resets_to_configured_default_foreground() {
        use crate::config::TerminalConfig;
        let mut term = Terminal::new(2, 8);
        // A themed configured default fg (#112233); allow runtime colour ops.
        let mut tc = TerminalConfig::default();
        tc.default_foreground = aterm_types::Rgb::new(0x11, 0x22, 0x33);
        tc.allow_palette_reconfigure = true;
        term.apply_config(&tc);
        // OSC 10 sets the dynamic default fg → magenta.
        term.process(b"\x1b]10;rgb:ff/00/ff\x07");
        assert_eq!(
            term.default_foreground(),
            aterm_types::Rgb::new(0xff, 0x00, 0xff),
            "OSC 10 set took effect"
        );
        // OSC 110 resets to the configured themed value, not magenta/spec.
        term.process(b"\x1b]110\x07");
        assert_eq!(
            term.default_foreground(),
            aterm_types::Rgb::new(0x11, 0x22, 0x33),
            "OSC 110 resets to the configured (themed) default"
        );
        assert_ne!(
            term.default_foreground(),
            aterm_types::Rgb::new(0xff, 0x00, 0xff)
        );
    }

    #[test]
    fn vs16_widened_emoji_sets_emoji_presentation() {
        // ❤️ = U+2764 (HEAVY BLACK HEART, text default) + U+FE0F (VS16). VS16
        // widens it to 2 cells and requests colour presentation.
        let mut term = Terminal::new(2, 8);
        term.process("\u{2764}\u{FE0F}".as_bytes());
        let cells = term.render_row(0);
        assert_eq!(cells[0].ch, '\u{2764}');
        assert!(!cells[0].wide, "lead cell is not a continuation");
        assert!(
            cells[0].emoji_presentation,
            "VS16-widened ❤️ lead must request emoji presentation"
        );
        // The right half is a wide continuation carrying no glyph / no flag.
        assert!(cells[1].wide, "second column is the wide continuation");
        assert!(
            !cells[1].emoji_presentation,
            "continuation cell carries no presentation flag"
        );
    }

    #[test]
    fn vs15_narrowed_default_emoji_sets_text_presentation() {
        // Bare 😀 defaults to a wide colour emoji. VS15 both narrows its
        // materialized geometry and must survive in the render snapshot so the
        // renderer can force the text/mono face instead of painting a 2-cell
        // colour bitmap over the following cell.
        let mut term = Terminal::new(2, 8);
        term.process("😀\u{FE0E}".as_bytes());
        let cells = term.render_row(0);
        assert_eq!(cells[0].ch, '😀');
        assert!(!cells[0].wide, "lead cell is not a continuation");
        assert!(
            cells[0].text_presentation,
            "VS15-narrowed 😀 must request text presentation"
        );
        assert!(!cells[0].emoji_presentation);
        assert!(!cells[1].wide, "VS15 must remove the former continuation");
        assert!(!cells[1].text_presentation);
    }

    #[test]
    fn bare_emoji_base_without_vs16_is_text_presentation() {
        // Bare ❤ (no VS16) stays narrow and text — NO emoji presentation, so
        // the renderer keeps drawing the mono black-heart glyph.
        let mut term = Terminal::new(2, 8);
        term.process("\u{2764}".as_bytes());
        let cells = term.render_row(0);
        assert_eq!(cells[0].ch, '\u{2764}');
        assert!(!cells[0].wide);
        assert!(
            !cells[0].emoji_presentation,
            "bare ❤ must not request emoji presentation"
        );
    }

    #[test]
    fn cluster_row_emits_zwj_skin_keycap_not_vs16_or_plain() {
        let mut term = Terminal::new(2, 20);
        // family ZWJ (col 0) sp(2) skin (3) sp(5) keycap (6) sp(?) ❤️ VS16 plain 'a'
        term.process(
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} \u{1F44D}\u{1F3FD} \u{31}\u{FE0F}\u{20E3} \u{2764}\u{FE0F}a".as_bytes(),
        );
        let clusters = term.cluster_row(0);
        // family at lead col 0
        let family = clusters
            .iter()
            .find(|(c, _)| *c == 0)
            .map(|(_, s)| s.as_ref());
        assert_eq!(
            family,
            Some("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"),
            "family ZWJ cluster"
        );
        // skin-tone thumbs-up at col 3
        let skin = clusters
            .iter()
            .find(|(c, _)| *c == 3)
            .map(|(_, s)| s.as_ref());
        assert_eq!(skin, Some("\u{1F44D}\u{1F3FD}"), "skin-tone cluster");
        // keycap at col 6
        let keycap = clusters
            .iter()
            .find(|(c, _)| *c == 6)
            .map(|(_, s)| s.as_ref());
        assert_eq!(keycap, Some("\u{31}\u{FE0F}\u{20E3}"), "keycap cluster");
        // VS16 ❤️ must not be emitted (it keeps the emoji_presentation path).
        assert!(
            clusters.iter().all(|(_, s)| !s.starts_with('\u{2764}')),
            "VS16 ❤️ must not be a shaping cluster, got {clusters:?}"
        );
    }

    #[test]
    fn regional_indicator_pair_folds_into_one_flag_cluster() {
        // 🇺🇸 = regional indicator U + S. The pair must fold into one 2-cell
        // grapheme (lead col 0 wide, col 1 continuation), with S as a combining
        // mark, and surface as a flag cluster for shaping.
        let mut term = Terminal::new(2, 12);
        term.process("\u{1F1FA}\u{1F1F8}".as_bytes());
        let cells = term.render_row(0);
        assert_eq!(
            cells[0].ch, '\u{1F1FA}',
            "lead cell is regional indicator U"
        );
        assert!(!cells[0].wide, "lead is not a continuation");
        assert!(cells[1].wide, "col 1 is the wide continuation of the flag");
        // The pair occupies exactly 2 cells, not 4 (render_row trims to the
        // occupied width, so a folded pair is a length-2 row).
        assert_eq!(
            cells.len(),
            2,
            "RI pair folds into one 2-cell flag, not two glyphs"
        );

        let clusters = term.cluster_row(0);
        let flag = clusters
            .iter()
            .find(|(c, _)| *c == 0)
            .map(|(_, s)| s.as_ref());
        assert_eq!(
            flag,
            Some("\u{1F1FA}\u{1F1F8}"),
            "flag cluster surfaced for shaping"
        );
    }

    #[test]
    fn three_regional_indicators_pair_then_single() {
        // 🇺🇸🇫: GB12/GB13 — the first two pair into a flag; the third stands
        // alone in its own cell (it is not folded into the completed pair).
        let mut term = Terminal::new(2, 12);
        term.process("\u{1F1FA}\u{1F1F8}\u{1F1EB}".as_bytes());
        let cells = term.render_row(0);
        assert_eq!(cells[0].ch, '\u{1F1FA}', "pair lead U");
        assert!(cells[1].wide, "pair continuation");
        // The third RI starts a fresh cell at col 2 (wide), not folded in.
        assert_eq!(cells[2].ch, '\u{1F1EB}', "third RI stands alone");
        let clusters = term.cluster_row(0);
        assert_eq!(
            clusters.len(),
            1,
            "only the completed pair is a cluster, got {clusters:?}"
        );
        assert_eq!(clusters[0].0, 0, "the flag cluster is at the pair lead");
    }

    #[test]
    fn sgr_decorations_surface_on_render_cells() {
        let mut term = Terminal::new(2, 16);
        // SGR 4 underline, 21 double, 4:3 curly, 9 strike, 53 overline.
        term.process(
            b"\x1b[4mA\x1b[0m\x1b[21mB\x1b[0m\x1b[4:3mC\x1b[0m\x1b[9mD\x1b[0m\x1b[53mE\x1b[0m",
        );
        let cells = term.render_row(0);
        assert_eq!(
            cells[0].underline,
            UnderlineStyle::Single,
            "SGR 4 -> single"
        );
        assert_eq!(
            cells[1].underline,
            UnderlineStyle::Double,
            "SGR 21 -> double"
        );
        assert_eq!(
            cells[2].underline,
            UnderlineStyle::Curly,
            "SGR 4:3 -> curly"
        );
        assert_eq!(cells[3].underline, UnderlineStyle::None);
        assert!(cells[3].strikethrough, "SGR 9 -> strikethrough");
        assert!(cells[4].overline, "SGR 53 -> overline");
        // Plain cells carry no decoration.
        let mut plain = Terminal::new(2, 8);
        plain.process(b"x");
        let pc = plain.render_row(0);
        assert_eq!(pc[0].underline, UnderlineStyle::None);
        assert!(!pc[0].strikethrough && !pc[0].overline);
    }

    #[test]
    fn underline_color_surfaces_from_sgr58() {
        let mut term = Terminal::new(2, 8);
        // SGR 4 underline + 58;2;255;0;0 sets a red underline colour.
        term.process(b"\x1b[4;58:2::255:0:0mU\x1b[0m");
        let cells = term.render_row(0);
        assert_eq!(cells[0].underline, UnderlineStyle::Single);
        assert_eq!(
            cells[0].underline_color,
            Some([255, 0, 0]),
            "SGR 58 red underline colour"
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
        // Live resolution: an OSC 4 palette redefinition re-colors the same
        // cell on the next extraction.
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

    /// W5(f)+(e) end-to-end: `bold_is_bright = false` stops the 0–7 → 8–15
    /// promotion, and `faint_opacity` drives the SGR 2 blend — both applied
    /// through `apply_config` and visible in `render_row` output.
    #[test]
    fn style_policy_flows_through_apply_config() {
        use crate::config::TerminalConfig;
        let mut term = Terminal::new(2, 8);
        term.process(b"\x1b[1;33mB\x1b[0m \x1b[2mD\x1b[0m");
        let bright = term.color_palette().get(11);
        let base = term.color_palette().get(3);
        let cells = term.render_row(0);
        assert_eq!(
            cells[0].fg,
            [bright.r, bright.g, bright.b],
            "default promotes"
        );
        let dim_default = cells[2].fg;

        let mut tc = TerminalConfig::default();
        tc.bold_is_bright = false;
        tc.faint_opacity = 1.0; // dim retains the full fg → no visual change
        term.apply_config(&tc);
        let cells = term.render_row(0);
        assert_eq!(
            cells[0].fg,
            [base.r, base.g, base.b],
            "bold_is_bright=false keeps the base indexed colour"
        );
        let fg = term.default_foreground();
        assert_eq!(
            cells[2].fg,
            [fg.r, fg.g, fg.b],
            "faint_opacity=1.0 leaves dim text at the full fg"
        );
        assert_ne!(dim_default, cells[2].fg, "the default opacity did dim");
    }

    /// [`Terminal::presentation_fingerprint`] is a cache key for RESOLVED rows
    /// (the apron memo, the control socket's `screen since=<etag>`), so every
    /// input the resolver reads beside the grid must move it. Each arm changes
    /// one input — the style policy first, through a config reload, while the
    /// colours are still the config's own — and is shown to re-colour a
    /// resolved cell, so no arm is a no-op; writing cells is content, and
    /// leaves it alone.
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
        // A bold palette-3 cell, a dim cell, a palette-1 cell.
        term.process(b"\x1b[1;33mB\x1b[0m \x1b[2mD\x1b[0m \x1b[31mR\x1b[0m");
        let arms: &[Arm] = &[
            ("bold_is_bright (config reload)", |t, c| {
                c.bold_is_bright = !c.bold_is_bright;
                t.apply_config(c);
            }),
            ("faint_opacity (config reload)", |t, c| {
                c.faint_opacity = 0.9;
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

    #[test]
    fn combining_marks_surface_for_diacritics_not_emoji() {
        let mut term = Terminal::new(2, 12);
        // é = e + U+0301, then a ZWJ family (emoji sequence), then plain 'x'.
        term.process("e\u{0301} \u{1F468}\u{200D}\u{1F469} x".as_bytes());
        let comb = term.combining_row(0);
        // The 'e' at col 0 surfaces its acute mark.
        let m0 = comb.iter().find(|(c, _)| *c == 0).map(|(_, m)| m.as_ref());
        assert_eq!(
            m0,
            Some(['\u{0301}'].as_slice()),
            "acute mark overlaid on e"
        );
        // The emoji family is not a combining-overlay cell (cluster_row owns it).
        let family_col = 2; // after "e\u{0301} " (cols 0,1)
        assert!(
            comb.iter().all(|(c, _)| *c != family_col),
            "emoji cluster must not be a combining-overlay cell, got {comb:?}"
        );
    }

    /// A cell's whole mark sequence reaches the frame lane, in arrival order,
    /// since renderers stack each mark. The grid bounds a cell at
    /// `CellExtra::MAX_COMBINING` marks; exactly that many surface for a longer
    /// run.
    #[test]
    fn combining_row_keeps_every_stacked_mark_in_order() {
        let mut term = Terminal::new(1, 8);
        term.process("e\u{0301}\u{0302}\u{0323}x".as_bytes());
        let comb = term.combining_row(0);
        let m0 = comb.iter().find(|(c, _)| *c == 0).map(|(_, m)| m.as_ref());
        assert_eq!(
            m0,
            Some(['\u{0301}', '\u{0302}', '\u{0323}'].as_slice()),
            "acute, circumflex, dot below — all three, in arrival order"
        );
        assert_eq!(
            term.cell_frame(1, 8).combining_at(0, 0).map(<[char]>::len),
            Some(3),
            "the render frame carries the same three"
        );
        assert!(
            comb.iter().all(|(c, _)| *c != 1),
            "the plain 'x' after the marks carries none: {comb:?}"
        );

        let mut zalgo = Terminal::new(1, 8);
        let mut text = String::from("e");
        text.extend(std::iter::repeat_n('\u{0301}', 20));
        zalgo.process(text.as_bytes());
        let m = zalgo.combining_row(0);
        assert_eq!(
            m.iter().find(|(c, _)| *c == 0).map(|(_, m)| m.len()),
            Some(aterm_grid::extra::CellExtra::MAX_COMBINING),
            "a 20-mark run surfaces exactly the grid's bound"
        );
    }

    #[test]
    fn combining_row_empty_for_plain_and_vs16() {
        let mut term = Terminal::new(2, 8);
        // VS16 ❤️ has a combining selector but NO overlay mark.
        term.process("hi \u{2764}\u{FE0F}".as_bytes());
        assert!(
            term.combining_row(0).is_empty(),
            "plain text + VS16 has no overlay marks"
        );
    }

    #[test]
    fn cluster_row_empty_for_plain_text() {
        let mut term = Terminal::new(2, 8);
        term.process(b"hello");
        assert!(
            term.cluster_row(0).is_empty(),
            "plain ASCII has no emoji clusters"
        );
    }

    /// A subdivision flag (🏴, tag letters, CANCEL TAG) is one emoji sequence
    /// for the colour face to shape, like a ZWJ family: it surfaces as a
    /// cluster, never as overlay marks.
    #[test]
    fn tag_sequence_surfaces_as_a_cluster_not_an_overlay() {
        const ENGLAND: &str = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}";
        let mut term = Terminal::new(1, 8);
        term.process(format!("{ENGLAND}X").as_bytes());
        assert_eq!(
            term.cluster_row(0),
            vec![(0usize, Box::<str>::from(ENGLAND))],
            "the flag is one cluster at its lead column"
        );
        assert!(
            term.combining_row(0).is_empty(),
            "no tag reaches the overlay loop"
        );
        // The folded `cell_frame` twin classifies through the same predicate.
        let frame = term.cell_frame(1, 8);
        assert_eq!(frame.clusters[0], term.cluster_row(0));
        assert!(frame.combining[0].is_empty());
        assert_eq!(
            frame.cells[0][2].ch, 'X',
            "the flag holds two cells and X follows"
        );
    }

    /// A default-ignorable that is not a sequence marker — a lone tag after a
    /// letter, an ideographic variation selector after its ideograph — attaches
    /// to the base and is neither a cluster nor an overlay: the base draws
    /// alone. A real diacritic still overlays (the negative control).
    #[test]
    fn default_ignorable_marks_are_never_overlaid() {
        for text in ["A\u{E0001}", "\u{4E00}\u{E0100}"] {
            let mut term = Terminal::new(1, 8);
            term.process(text.as_bytes());
            assert_eq!(
                term.grid().cell_extra(0, 0).map(|e| e.combining().len()),
                Some(1),
                "{text:?}: the mark attached to the base"
            );
            assert!(term.combining_row(0).is_empty(), "{text:?}: not an overlay");
            assert!(term.cluster_row(0).is_empty(), "{text:?}: not a cluster");
            let frame = term.cell_frame(1, 8);
            assert!(frame.combining[0].is_empty() && frame.clusters[0].is_empty());
        }
        let mut term = Terminal::new(1, 8);
        term.process("e\u{0301}".as_bytes());
        assert_eq!(
            term.combining_row(0),
            vec![(0usize, Box::<[char]>::from(['\u{0301}']))],
            "a real diacritic still overlays"
        );
    }

    #[test]
    fn wide_cjk_is_not_emoji_presentation() {
        // A naturally-wide CJK char is wide but not emoji-capable, so it must
        // not be mistaken for a VS16 emoji.
        let mut term = Terminal::new(2, 8);
        term.process("\u{65E5}".as_bytes()); // 日
        let cells = term.render_row(0);
        assert_eq!(cells[0].ch, '\u{65E5}');
        assert!(
            !cells[0].wide,
            "lead cell of a wide glyph is not the continuation"
        );
        assert!(
            !cells[0].emoji_presentation,
            "wide CJK must not request emoji presentation"
        );
    }

    #[test]
    fn render_row_indexed_fg_red() {
        let mut term = Terminal::new(2, 8);
        // SGR 31 = red foreground.
        term.process(b"\x1b[31mR\x1b[0m");
        let cells = term.render_row(0);
        assert_eq!(cells[0].ch, 'R');
        let [r, g, b] = cells[0].fg;
        assert!(
            r > g && r > b,
            "expected red-dominant fg, got {:?}",
            cells[0].fg
        );
    }

    #[test]
    fn render_row_indexed_bg_green() {
        let mut term = Terminal::new(2, 8);
        // SGR 42 = green background.
        term.process(b"\x1b[42mG\x1b[0m");
        let cells = term.render_row(0);
        assert_eq!(cells[0].ch, 'G');
        let [r, g, b] = cells[0].bg;
        assert!(
            g > r && g > b,
            "expected green-dominant bg, got {:?}",
            cells[0].bg
        );
    }

    #[test]
    fn render_row_truecolor_fg() {
        let mut term = Terminal::new(2, 8);
        // SGR 38;2;10;20;200 = a blue-ish truecolor fg.
        term.process(b"\x1b[38;2;10;20;200mX\x1b[0m");
        let cells = term.render_row(0);
        assert_eq!(cells[0].ch, 'X');
        assert_eq!(cells[0].fg, [10, 20, 200]);
    }

    #[test]
    fn render_row_protected_text_is_visible() {
        // DECSCA (ESC [ 1 " q) sets the PROTECTED flag, which shares bit 10 with
        // WIDE_CONTINUATION. Protected characters must still render their glyph.
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

    /// A rendition belongs to the character, not the column: under SGR 7 the
    /// highlight must be continuous across both halves of a CJK glyph, since
    /// the rasterizer spills the glyph's right half into the spacer.
    #[test]
    fn render_row_inverse_wide_char_highlights_both_halves() {
        let mut term = Terminal::new(2, 8);
        term.process("\x1b[7m\u{6F22}\x1b[0mZ".as_bytes());
        let cells = term.render_row(0);

        assert_eq!(cells[0].ch, '\u{6F22}');
        assert!(cells[1].wide, "col 1 is the right half of one character");
        assert_eq!(
            cells[1].bg, cells[0].bg,
            "the highlight band must not stop at the column seam"
        );
        assert_eq!(
            cells[1].fg, cells[0].fg,
            "the spilled right half must be drawn in the SAME swapped ink as the left"
        );
        assert_ne!(
            cells[1].fg, cells[1].bg,
            "fg == bg in the spacer is exactly the collision that erased the glyph"
        );
        // The negative control: the un-highlighted neighbour is not inverted, so
        // this test cannot pass by the whole row happening to share one colour.
        assert_ne!(cells[0].bg, cells[2].bg, "col 2 is outside the SGR 7 run");
    }

    /// The same law with explicit colours, where the identity is plainest: under
    /// `31;44` the lead resolves fg=blue on bg=red, and an un-inverted spacer
    /// resolved bg=blue — the lead's own foreground.
    #[test]
    fn render_row_inverse_wide_char_swaps_explicit_colors_in_both_halves() {
        let mut term = Terminal::new(2, 8);
        term.process("\x1b[31;44;7m\u{6F22}\x1b[0m".as_bytes());
        let cells = term.render_row(0);

        assert_eq!(cells[1].bg, cells[0].bg, "spacer bg == lead bg (red)");
        assert_eq!(cells[1].fg, cells[0].fg, "spacer fg == lead fg (blue)");
        assert_ne!(
            cells[1].bg, cells[0].fg,
            "the spacer must not keep the UN-swapped background"
        );
    }

    /// The rules (SGR 4/9/53) draw across a cell, so a spacer without them leaves
    /// a visible gap under every right half.
    #[test]
    fn render_row_wide_char_rules_span_both_halves() {
        let mut term = Terminal::new(2, 8);
        term.process("\x1b[4;9;53m\u{6F22}\x1b[0mZ".as_bytes());
        let cells = term.render_row(0);

        assert_eq!(cells[0].underline, UnderlineStyle::Single);
        assert_eq!(
            cells[1].underline,
            UnderlineStyle::Single,
            "the underline must not gap at the column seam"
        );
        assert!(cells[0].strikethrough && cells[1].strikethrough);
        assert!(cells[0].overline && cells[1].overline);
        assert_eq!(
            cells[2].underline,
            UnderlineStyle::None,
            "negative control: the rules stop when the run does"
        );
    }

    /// VS16 widens a narrow emoji to two columns through
    /// `widen_previous_cell_for_vs16`, which builds its own spacer.
    #[test]
    fn render_row_inverse_vs16_emoji_highlights_both_halves() {
        let mut term = Terminal::new(2, 8);
        // U+231A WATCH + VS16.
        term.process("\x1b[7m\u{231A}\u{FE0F}\x1b[0m".as_bytes());
        let cells = term.render_row(0);

        assert!(cells[1].wide, "VS16 made the watch two columns wide");
        assert_eq!(
            cells[1].bg, cells[0].bg,
            "the VS16 widening path obeys the same law as the CJK write path"
        );
        assert_eq!(cells[1].fg, cells[0].fg);
    }

    #[test]
    fn render_row_wide_continuation_is_blanked() {
        // A real wide char (中, U+4E2D) occupies a wide lead cell plus a
        // WIDE_CONTINUATION spacer. The lead keeps the glyph; the spacer renders
        // blank and is flagged `wide`.
        let mut term = Terminal::new(2, 8);
        term.process("中X".as_bytes());
        let cells = term.render_row(0);
        assert_eq!(cells[0].ch, '中');
        assert!(!cells[0].wide, "the wide LEAD is not a continuation");
        assert_eq!(cells[1].ch, ' ', "the continuation spacer renders blank");
        assert!(cells[1].wide, "the continuation spacer is flagged wide");
        assert_eq!(
            cells[2].ch, 'X',
            "the next glyph follows the 2-cell wide char"
        );
    }

    /// The apron at offset `d` is the bottom viewport row at offset `d - 1`,
    /// for a shallow scroll (a live row), at the `d == rows` seam, and deeper
    /// than the frame (a history row); there is no apron at the live bottom.
    /// The caret rides on it exactly when the cursor's projected row is the
    /// apron.
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
            // The caret: after the last `\r\n` the cursor sits on live row 5 col 0,
            // which projects onto viewport row `5 + d` — the apron row exactly at
            // d == 1, off the frame past it.
            assert_eq!(
                at_d.apron_row.cursor_col,
                (d == 1).then_some(0),
                "d={d}: the caret rides the apron only when its projected row is it"
            );
        }
        // Non-vacuity: a shallow apron carries real text (it is a written line).
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
        // A themeable host opts in to OSC 4 set (it fails closed by default).
        term.set_allow_palette_reconfigure(true);
        let mut scratch = crate::render::RenderInput::empty();
        let _ = term.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
        assert!(scratch.apron_row.present, "precondition: an apron at d=2");
        let fresh = |t: &mut Terminal| t.cell_frame(rows, cols).apron_row;
        assert_eq!(scratch.apron_row, fresh(&mut term));
        let recolors: [(&str, &dyn Fn(&mut Terminal)); 4] = [
            // OSC 4 first: a later host palette push takes precedence over it
            // for that index, so after one it would change nothing to see.
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
        // The apron at d = 2 is live screen row rows - d = 4 (CUP row 5).
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
    /// epoch, rows, cols)`. The memo must not hand terminal B the apron
    /// terminal A filled.
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
        // 2 visible rows; write 4 lines so 2 scroll into history.
        let mut term = Terminal::new(2, 8);
        term.process(b"L0aaa\r\nL1bbb\r\nL2ccc\r\nL3ddd");

        // At offset 0 the two paths are identical.
        assert_eq!(
            text(&term.render_row(0)),
            text(&term.render_row_at_screen(0))
        );
        let live0 = text(&term.render_row_at_screen(0));
        let live1 = text(&term.render_row_at_screen(1));

        // Scroll the GUI back one line.
        term.scroll_display(1);

        // The live-frame read is unchanged by the scroll...
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
        // ...while the offset-aware render_row followed the scroll (the two
        // frames genuinely diverge).
        assert_ne!(
            text(&term.render_row(0)),
            live0,
            "offset-aware render_row moved with the scroll"
        );
    }

    /// The DEC cursor belongs to the active grid, not retained history. The
    /// frame snapshot therefore suppresses it at a non-zero viewport offset;
    /// a host copy/vi cursor may still deliberately override this field after
    /// extraction in its own history coordinate space.
    #[test]
    fn cell_frame_projects_the_dec_cursor_while_its_row_stays_on_screen() {
        let mut term = Terminal::new(2, 8);
        term.process(b"L0\r\nL1\r\nL2\r\nL3");
        let live = term.cell_frame(2, 8);
        assert_eq!(live.display_offset, 0);
        assert!(live.cursor_visible, "live DECTCEM cursor is present");
        let live_row = live.cursor_row;

        // A scroll deep enough to push the cursor's row off the bottom hides it
        // (the whole-history case — unchanged, fail-closed).
        term.scroll_display(1);
        let history = term.cell_frame(2, 8);
        assert!(history.display_offset > 0, "fixture entered history");
        assert!(
            !history.cursor_visible,
            "a cursor row scrolled off the viewport bottom stays hidden"
        );

        term.scroll_to_bottom();
        assert!(
            term.cell_frame(2, 8).cursor_visible,
            "returning live restores the ordinary DEC cursor"
        );

        // ...but a small scroll that leaves the cursor's row on screen keeps the
        // cursor drawn, projected down by the offset: at offset d it sits at
        // viewport row `cur.row + d`, still < rows.
        let mut tall = Terminal::new(6, 8);
        tall.process(b"top");
        tall.process(b"\r\n\r\n\r\n\r\n\r\nmore\r\nmore\r\nmore");
        // Put the cursor back near the top, then leave room to scroll a little.
        tall.process(b"\x1b[1;1H");
        let base = tall.cell_frame(6, 8);
        assert_eq!(base.display_offset, 0);
        assert!(base.cursor_visible);
        assert_eq!(base.cursor_row, 0, "cursor sits on the top row at offset 0");
        tall.scroll_display(2);
        let scrolled = tall.cell_frame(6, 8);
        assert!(
            scrolled.display_offset >= 2,
            "scrolled a little into history"
        );
        assert!(
            scrolled.cursor_visible,
            "the cursor's row is still on screen after a small scroll — it must show"
        );
        assert_eq!(
            scrolled.cursor_row, scrolled.display_offset as usize,
            "and it is PROJECTED down by the offset from its active-grid row 0"
        );
        assert_eq!(
            live_row, 1,
            "sanity: the 2-row fixture's live cursor is on row 1"
        );
    }

    /// The per-frame snapshot folds emoji-cluster + combining-mark extraction
    /// into `render_row_into_impl` (one extras probe per cell) instead of the
    /// old `cluster_row_into` / `combining_row_into` full-grid passes. Pin that
    /// the folded output is byte-identical to the standalone row accessors.
    #[test]
    fn cell_frame_clusters_combining_match_row_accessors() {
        let mut term = Terminal::new(3, 24);
        // ZWJ family + skin-tone + keycap (clusters), VS16 heart (presentation,
        // neither list), é diacritic (combining), and plain text.
        term.process(
            "\u{1F468}\u{200D}\u{1F469} \u{1F44D}\u{1F3FD} \u{31}\u{FE0F}\u{20E3} \u{2764}\u{FE0F} e\u{0301} hi"
                .as_bytes(),
        );
        let frame = term.cell_frame(3, 24);
        for r in 0..3 {
            assert_eq!(
                frame.clusters[r],
                term.cluster_row(r),
                "folded clusters must equal cluster_row for row {r}"
            );
            assert_eq!(
                frame.combining[r],
                term.combining_row(r),
                "folded combining must equal combining_row for row {r}"
            );
        }
        // Sanity: the row actually carries clusters and a combining mark, so the
        // parity assertions above are non-trivial.
        assert!(
            !frame.clusters[0].is_empty(),
            "test content must produce at least one emoji cluster"
        );
        assert!(
            !frame.combining[0].is_empty(),
            "test content must produce at least one combining-mark overlay"
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
            // Cells-only extraction also reads the borrowed marks for VS15,
            // without requesting owned cluster or overlay output.
            assert_eq!(term.render_row(0), live.cells[0]);
            term.grid_mut().scroll_to_bottom();
            assert_ne!(
                term.render_row(0),
                live.cells[0],
                "the history read was not live data"
            );
        }
    }

    /// Differential oracle: the damage-scoped extraction must be
    /// content-identical to a fresh full extract after every step of a
    /// mutation corpus covering each continuity clause: echo writes, SGR,
    /// cursor-only moves, wide/emoji/combining cells, scroll (base_y), viewport
    /// scroll, DECSCNM and OSC 4 recolors, alt-screen swaps, host splices,
    /// foreign `take_damage`, and resize. Equality is `RenderInput`'s
    /// content-only `PartialEq`, as the CPU renderer's damage cache uses.
    ///
    /// Counters at the bottom require both arms to have fired, so a silent
    /// degradation to `Full` (or a gate that never refuses) fails.
    #[test]
    fn damage_scoped_extraction_matches_full_extract_over_mutation_corpus() {
        use crate::render::{FrameRefill, FullRefillCause};
        let (rows, cols) = (8usize, 40usize);
        let mut term = Terminal::new(8, 40);
        let mut scoped = crate::render::RenderInput::empty();
        let mut n_scoped = 0usize;
        let mut n_full = 0usize;

        // One corpus step: fresh-full oracle first (allocation-per-call, no
        // continuity, no damage consume), then the scoped fill (which consumes
        // the session), then content equality.
        let step = |term: &mut Terminal,
                    scoped: &mut crate::render::RenderInput,
                    n_scoped: &mut usize,
                    n_full: &mut usize,
                    what: &str|
         -> FrameRefill {
            let reference = term.cell_frame(rows, cols);
            let refill = term.cell_frame_damage_scoped_into(scoped, rows, cols);
            match refill {
                FrameRefill::Full { .. } => *n_full += 1,
                FrameRefill::Scoped { .. } => *n_scoped += 1,
            }
            assert!(
                *scoped == reference,
                "scoped extraction diverged from the full extract after: {what}"
            );
            refill
        };

        // 1) First fill: gen 0 scratch -> Full by construction.
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "first fill",
        );
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::ScratchUnstamped
            },
            "a never-filled scratch must take the full arm, under its own cause"
        );

        // 2) Keystroke echo: one damaged row -> the scoped arm, refilling only it.
        term.process(b"hello");
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "echo write",
        );
        assert!(
            matches!(r, FrameRefill::Scoped { rows_refilled } if rows_refilled >= 1),
            "an echo on a settled scratch must take the scoped arm, got {r:?}"
        );

        // 3) SGR + positioned write.
        term.process(b"\x1b[3;10H\x1b[1;35mZ\x1b[0m");
        step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "SGR write",
        );

        // 4) Cursor-only move: no damage -> Scoped with zero rows, scalars restamped.
        term.process(b"\x1b[5;5H");
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "cursor-only move",
        );
        // A pure cursor move must stay on the scoped arm (the content equality
        // above already proved the restamped cursor scalars match the oracle).
        // Deliberately not pinned to `rows_refilled: 0`: whether a CUP marks
        // the cursor cell's row damaged is the engine's business — either way
        // the arm and the bytes are what this oracle guarantees.
        assert!(
            matches!(r, FrameRefill::Scoped { .. }),
            "a pure cursor move must stay on the scoped arm, got {r:?}"
        );

        // 5) Wide + emoji cluster + combining mark (cluster/combining channels).
        term.process("\u{6f22} \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} e\u{0301}".as_bytes());
        step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "wide/emoji/combining",
        );

        // 6) Scroll: newlines advance base_y -> anchor mismatch -> Full.
        term.process(b"\r\n".repeat(10).as_slice());
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "scroll (base_y)",
        );
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::BaseY
            },
            "a base_y advance must force the full arm, and say so"
        );

        // 7) Viewport scroll: display_offset != 0 -> Full on entry and while held.
        term.scroll_display(3);
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "scrolled back",
        );
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::EngineScrolled
            },
            "a scrolled-back viewport must force the full arm, attributed to the ENGINE's \
             offset (the clause that precedes the scratch's own)"
        );
        term.process(b"x"); // live-grid write while scrolled: bits are live rows, not viewport rows
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "write while scrolled",
        );
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::EngineScrolled
            },
            "offset != 0 must keep forcing the full arm"
        );
        term.scroll_to_bottom();
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "back to bottom",
        );
        // The frame that returns to the bottom: the engine is at offset 0 again,
        // but the scratch was filled while scrolled, so its retained rows use
        // the other mapping.
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::ScratchScrolled
            },
            "returning to the bottom must refuse on the SCRATCH's stale offset"
        );

        // 8) Continuity recovers after the offset round-trip.
        term.process(b"y");
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "echo after recover",
        );
        assert!(
            matches!(r, FrameRefill::Scoped { .. }),
            "continuity must recover once anchors settle, got {r:?}"
        );

        // 9) DECSCNM: full-screen recolor marks Damage::Full -> Full arm.
        term.process(b"\x1b[?5h");
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "DECSCNM",
        );
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::FullDamage
            },
            "DECSCNM marks full damage; scoped must yield, naming the damage"
        );
        term.process(b"\x1b[?5l");
        step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "DECSCNM off",
        );

        // 10) OSC 4 palette recolor: repaints painted cells -> full damage -> Full.
        //
        // `RenderCell::fg`/`bg` are final resolved RGB, so recoloring an index
        // that painted cells use changes their bytes without a glyph write.
        // OSC 4 reconfigure is fail-closed by default
        // (`allow_palette_reconfigure`), so opt in, and paint with index 2 and
        // settle the scratch first so the recolor changes retained rows.
        term.set_allow_palette_reconfigure(true);
        term.process(b"\x1b[7;1H\x1b[32mgreen-on-2\x1b[0m");
        step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "paint with palette index 2",
        );
        term.process(b"\x1b]4;2;rgb:12/34/56\x07");
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "OSC 4 recolor",
        );
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::FullDamage
            },
            "a palette recolor marks full damage"
        );

        // 11) Alt screen: swap -> Full; write inside -> Scoped; swap back -> Full.
        term.process(b"\x1b[?1049h");
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "alt enter",
        );
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::AltScreen
            },
            "an alt-screen swap must force the full arm, on the alt clause"
        );
        term.process(b"alt!");
        step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "alt write",
        );
        term.process(b"\x1b[?1049l");
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "alt leave",
        );
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::AltScreen
            },
            "leaving the alt screen must force the full arm, on the alt clause"
        );

        // 12) Host splice: mutate a content channel and bump snapshot_seq. The
        // next scoped attempt must detect the bump and take the full arm.
        scoped.cells[0].clear();
        scoped.snapshot_seq = scoped.snapshot_seq.wrapping_add(1);
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "host splice",
        );
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::HostMutation
            },
            "a seq-bumped host mutation must force the full arm (RE-3 hazard #1) and be \
             ATTRIBUTED to the host, not to the engine — the whole point of the tally is \
             that a frontend can tell its own mutators from the terminal's scrolling"
        );

        // 13) Foreign consumer: another take_damage resets the tracker under us;
        // the generation bump must force the full arm (bits would undercount).
        term.process(b"z");
        term.take_damage();
        term.process(b"w");
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "foreign take_damage",
        );
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::DamageTaken
            },
            "a foreign take_damage must break continuity (undercounting bits)"
        );

        // 14) BiDi visual reorder (feature `bidi`, as the GUI ships, with
        // `BiDiMode` defaulting to `Implicit`). `apply_bidi_reorder` is an
        // in-place, non-idempotent row permutation, so the next refill requires
        // the fill's `engine_row_order` report to be logical.
        //
        // Content equality is asserted on every frame in both feature
        // configurations (the `step` closure); only the arm assertions are
        // feature-gated.
        term.process("\x1b[2;1H\u{05d0}\u{05d1}\u{05d2} abc\x1b[7;1H".as_bytes());
        step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "RTL row write",
        );
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "frame after an RTL reorder",
        );
        #[cfg(feature = "bidi")]
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::BidiVisual
            },
            "a fill that permuted a row into visual order must cost the NEXT fill \
             its scoped arm (retained rows would be double-permuted)"
        );
        #[cfg(not(feature = "bidi"))]
        assert!(
            matches!(r, FrameRefill::Scoped { .. }),
            "with the bidi feature off nothing permutes, so continuity must hold: {r:?}"
        );

        // ...and the scoped arm comes back once no row reorders: an RTL row
        // costs the scoped arm only while it is on screen.
        term.process(b"\x1b[2;1H\x1b[2K");
        step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "RTL row erased",
        );
        term.process(b"\x1b[7;1Hx");
        let r = step(
            &mut term,
            &mut scoped,
            &mut n_scoped,
            &mut n_full,
            "echo after the RTL row is gone",
        );
        assert!(
            matches!(r, FrameRefill::Scoped { .. }),
            "continuity must recover once no row reorders, got {r:?}"
        );

        // 15) Resize: dims mismatch -> Full; then continuity re-forms at the new dims.
        term.resize(10, 40);
        let reference = term.cell_frame(10, 40);
        let r = term.cell_frame_damage_scoped_into(&mut scoped, 10, 40);
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::ScratchRows
            },
            "a resize must force the full arm on the scratch's stale dims"
        );
        assert!(
            scoped == reference,
            "post-resize full refill must match the oracle"
        );
        n_full += 1;
        term.process(b"post-resize");
        let reference = term.cell_frame(10, 40);
        let r = term.cell_frame_damage_scoped_into(&mut scoped, 10, 40);
        assert!(
            matches!(r, FrameRefill::Scoped { .. }),
            "continuity must re-form at the new dims, got {r:?}"
        );
        assert!(
            scoped == reference,
            "scoped refill at new dims must match the oracle"
        );
        n_scoped += 1;

        // Both arms must have really run.
        assert!(n_scoped >= 5, "scoped arm under-exercised: {n_scoped}");
        assert!(n_full >= 8, "full arm under-exercised: {n_full}");
    }

    /// Randomized differential fuzz over a mutation alphabet chosen to attack
    /// the continuity proof. Every step compares the scoped scratch against a
    /// fresh full extract under the content-only `PartialEq`; a stale retained
    /// row shows up as a divergence with a printable seed.
    #[test]
    fn dmg1_fuzz_scoped_vs_full_over_random_mutation_streams() {
        use crate::render::FrameRefill;
        // xorshift64*: deterministic, no dev-dependency.
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
                // 1-4 mutations between extractions, so the probe can build
                // write(row A) -> foreign take_damage -> write(row B) ->
                // extract, where the tracker names only row B although row A
                // also changed under the scratch.
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
                        // Palette recolor: changes resolved rgb of painted cells
                        // with no glyph write.
                        12 => {
                            let idx = rng.below(8);
                            let v = rng.below(255);
                            term.process(
                                format!("\x1b]4;{idx};rgb:{v:02x}/{v:02x}/{v:02x}\x07").as_bytes(),
                            );
                        }
                        // Default fg/bg recolor (OSC 10/11), likewise glyph-free.
                        13 => term.process(b"\x1b]11;rgb:00/00/40\x07"),
                        14 => term.process(b"\x1b[?1049h"),
                        15 => term.process(b"\x1b[?1049l"),
                        // Foreign damage consumer: resets the tracker under us.
                        16 => term.take_damage(),
                        // RTL text: the one op whose extraction ends in an
                        // in-place, non-idempotent row permutation. Randomized
                        // against every other op so the interleavings that
                        // attack `engine_row_order` (reorder -> foreign
                        // take_damage -> write -> extract; reorder -> erase ->
                        // extract) are actually built. Inert with the `bidi`
                        // feature off; live in the workspace build, which is
                        // where aterm-gui unions the feature in.
                        17 => term.process("\u{05d0}\u{05d1}\u{05d2} x".as_bytes()),
                        // Viewport scroll in/out.
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
        // Reach: the fuzz must really have driven both arms.
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
    /// alternately refilling one scratch. Every other continuity token matches
    /// across the panes and their counters collide numerically, so the
    /// process-unique `terminal_id` nonce alone prevents one pane serving the
    /// other's rows. Asserted by content: a leak shows pane A's text in pane
    /// B's frame.
    #[test]
    fn dmg1_shared_scratch_across_two_terminals_never_leaks_rows() {
        use crate::render::{FrameRefill, FullRefillCause};
        let (rows, cols) = (6usize, 24usize);
        let mut a = Terminal::new(rows as u16, cols as u16);
        let mut b = Terminal::new(rows as u16, cols as u16);
        a.process(b"AAAA pane one\r\nsecond A row");
        b.process(b"BBBB pane two\r\nsecond B row");

        // One scratch, alternating owners — the aliasing shape exactly.
        let mut shared = crate::render::RenderInput::empty();
        let mut leaked_full = 0usize;
        for round in 0..12 {
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
            // After the first fill the identity nonce is the only clause that
            // may refuse here, since every other token matches across panes.
            // Round 0's pane A is the exception: the shared scratch is empty on
            // the first fill, so it refuses as `scratch_unstamped`.
            let want_a = if round == 0 {
                FullRefillCause::ScratchUnstamped
            } else {
                FullRefillCause::TerminalMismatch
            };
            assert_eq!(
                ra,
                FrameRefill::Full { cause: want_a },
                "round {round}: pane A's refusal"
            );
            assert_eq!(
                rb,
                FrameRefill::Full {
                    cause: FullRefillCause::TerminalMismatch
                },
                "round {round}: pane B must refuse on the identity nonce"
            );
            leaked_full += 1;
        }
        // An alternating shared scratch must take the full arm every time (the
        // identity nonce flips on every handoff).
        assert_eq!(
            leaked_full, 12,
            "a scratch alternating between two terminals must full-refill every frame, \
             and on the IDENTITY clause specifically"
        );
    }

    /// Per-cause attribution, one clause at a time: from a settled scratch
    /// that provably takes the scoped arm, apply exactly one mutation and
    /// require that clause's own cause. (Realistic events break several
    /// clauses at once, so the corpus test cannot pin labels.)
    ///
    /// The table must name every `FullRefillCause` variant, so a clause added
    /// without a cause, or a cause without a clause, fails here.
    #[test]
    fn dmg1_every_continuity_clause_reports_its_own_cause() {
        use crate::render::{FrameRefill, FullRefillCause, RenderInput, RowOrder};

        const ROWS: usize = 8;
        const COLS: usize = 32;

        /// A terminal with scrollback and a scratch whose continuity chain is
        /// live: the next damage-scoped refill takes the scoped arm.
        fn settled() -> (Terminal, RenderInput) {
            let mut term = Terminal::new(ROWS as u16, COLS as u16);
            // Enough lines to build scrollback (so a viewport scroll is
            // possible) and to leave `base_y` somewhere other than 0.
            for i in 0..20 {
                term.process(format!("line {i}\r\n").as_bytes());
            }
            let mut scratch = RenderInput::empty();
            // Fill 1 is Full by construction (unstamped scratch); fill 2 lands
            // on a settled chain.
            let _ = term.cell_frame_damage_scoped_into(&mut scratch, ROWS, COLS);
            term.process(b"\x1b[1;1Hz");
            let r = term.cell_frame_damage_scoped_into(&mut scratch, ROWS, COLS);
            assert!(
                matches!(r, FrameRefill::Scoped { .. }),
                "the fixture must start settled on the scoped arm, got {r:?}"
            );
            (term, scratch)
        }

        // The control: the fixture must be on the scoped arm, or every case
        // below would report its cause for the wrong reason.
        {
            let (mut term, mut scratch) = settled();
            term.process(b"\x1b[2;1Hq");
            let r = term.cell_frame_damage_scoped_into(&mut scratch, ROWS, COLS);
            assert!(
                matches!(r, FrameRefill::Scoped { .. }),
                "control: an unmutated settled fixture must stay scoped, got {r:?}"
            );
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
                // Any nonzero value that is not this terminal's nonce; the
                // `max(1)` only fires on the u64::MAX wrap, which is still a
                // change.
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
                // The requested size still matches the scratch (so the clause
                // before this one holds) but no longer matches the grid.
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
                // DECSCNM: a real event that marks Damage::Full and touches no
                // earlier clause.
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

        // Completeness: every cause has a case here.
        let mut covered: Vec<FullRefillCause> = cases.iter().map(|(_, _, c)| *c).collect();
        covered.sort_unstable();
        covered.dedup();
        assert_eq!(
            covered,
            FullRefillCause::ALL.to_vec(),
            "every FullRefillCause must be produced by exactly one clause here"
        );

        // The wire labels are a metrics contract: distinct, and snake_case so a
        // text field of `cause:count` pairs stays one whitespace-free token.
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
        // The dense index must really be dense — the tally arrays index by it.
        for (i, c) in FullRefillCause::ALL.iter().enumerate() {
            assert_eq!(c.index(), i, "FullRefillCause::ALL must be in index order");
        }
    }

    /// Fixture: a terminal with history and a scratch parked in it (filled at a
    /// non-zero offset), so the next scrolled refill is a candidate for the
    /// scrolled proof. Entering history from the live bottom is asserted to
    /// keep its label on the way in.
    fn scr2_parked_in_history(
        rows: usize,
        cols: usize,
        offset: i32,
    ) -> (Terminal, crate::render::RenderInput) {
        use crate::render::{FrameRefill, FullRefillCause};
        let mut term = Terminal::new(rows as u16, cols as u16);
        // 24 lines on an 8-row grid leave 17 in scrollback: room to park,
        // to overshoot, and to clamp at both ends.
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
    /// sprites, ink, cursor overrides) into the scratch after each fill. A
    /// scrolled step over a scratch carrying them must refill scoped and leave
    /// them as the host set them.
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
    /// Byte equality with the oracle is the assertion; the arm taken is not.
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
    /// whose tracker bits say `0..2`, zero for a frame that moved nothing), and
    /// the neighbouring corpus labels keep their meaning.
    #[test]
    fn scr2_scrolled_wheel_step_refills_only_the_exposed_strip() {
        use crate::render::{FrameRefill, FullRefillCause};
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
        // A parser batch that moves only the cursor: the pin/re-pin marks a
        // bottom strip and a top strip in the tracker, but no content moved and
        // the offset is unchanged, so the strip is empty.
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
        // Compound motion: the tracker holds `0..1 | 0..2 == 0..2` for a net
        // exposure of three rows. The strip must come from the offsets.
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

        // The three corpus labels, pinned next to the arm that lives under them.
        term.process(b"x");
        assert_eq!(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "write while scrolled"),
            FrameRefill::Full {
                cause: FullRefillCause::EngineScrolled
            },
            "content moved under the parked viewport: the full arm, under the engine's offset"
        );
        term.scroll_to_bottom();
        assert_eq!(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "back to bottom"),
            FrameRefill::Full {
                cause: FullRefillCause::ScratchScrolled
            },
            "returning to the bottom refuses on the SCRATCH's stale offset"
        );
        term.scroll_display(2);
        assert_eq!(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "re-enter history"),
            FrameRefill::Full {
                cause: FullRefillCause::EngineScrolled
            },
            "entering history from the live bottom is the one full frame of a scrub"
        );
        term.scroll_display(1);
        assert_eq!(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "scrub resumes"),
            FrameRefill::Scoped { rows_refilled: 1 },
            "and the scrub is scoped again from the next delta on"
        );
    }

    /// `row_rev` is indexed by live rows; a frame filled at a non-zero offset
    /// holds viewport rows, so it may not vouch for the lane, whether filled
    /// whole or by rotating a parked scratch.
    #[test]
    fn scr2_parked_fill_publishes_no_row_revision_lane() {
        let (_term, scratch) = scr2_parked_in_history(8, 32, 3);
        assert_ne!(
            scratch.display_offset, 0,
            "precondition: filled while parked"
        );
        assert_eq!(
            scratch.row_rev_lane, 0,
            "a fill at a non-zero offset must not publish a row-revision lane indexed by live rows"
        );
    }

    /// Scrolled oracle: byte equality with a fresh full extraction across a
    /// scrub: a compound move whose net exceeds the viewport while the tracker
    /// stays partial, the clamp at the top of history, one-row steps back to
    /// the live bottom, re-entry in bites that clamp at the top, and output
    /// while parked (the re-pin, and a write into a visible live row). Reach is
    /// counted at the bottom.
    #[test]
    fn scr2_scrolled_scoped_extraction_matches_full_extract_across_history_ends_and_output() {
        use crate::render::{FrameRefill, FullRefillCause};
        const ROWS: usize = 8;
        const COLS: usize = 32;
        let (mut term, mut scratch) = scr2_parked_in_history(ROWS, COLS, 2);
        let mut n_scoped = 0usize;
        let mut n_full = 0usize;
        fn tally(r: FrameRefill, n_scoped: &mut usize, n_full: &mut usize) -> FrameRefill {
            match r {
                FrameRefill::Full { .. } => *n_full += 1,
                FrameRefill::Scoped { .. } => *n_scoped += 1,
            }
            r
        }

        // Three steps of three: each marks `TopRows(3)` (partial), the net
        // is 9 >= 8. The strip would be the whole viewport, so the full arm.
        term.scroll_display(3);
        term.scroll_display(3);
        term.scroll_display(3);
        let r = tally(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "compound 3+3+3"),
            &mut n_scoped,
            &mut n_full,
        );
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::EngineScrolled
            },
            "a compound move that exposes the whole viewport takes the full arm, and does not panic"
        );

        // Wrap at the TOP: 11 -> 17 is a 6-row strip; a further push clamps
        // and moves nothing.
        term.scroll_to_top();
        let r = tally(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "scroll_to_top"),
            &mut n_scoped,
            &mut n_full,
        );
        assert_eq!(r, FrameRefill::Scoped { rows_refilled: 6 });
        term.scroll_display(5);
        let r = tally(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "clamped at top"),
            &mut n_scoped,
            &mut n_full,
        );
        assert_eq!(r, FrameRefill::Scoped { rows_refilled: 0 });

        // One-row steps all the way down. The frame that lands on the live
        // bottom refuses on the scratch's stale offset, as the corpus pins.
        while term.grid().display_offset() > 0 {
            term.scroll_display(-1);
            let r = tally(
                scr2_step(&mut term, &mut scratch, ROWS, COLS, "1-row step down"),
                &mut n_scoped,
                &mut n_full,
            );
            if term.grid().display_offset() == 0 {
                assert_eq!(
                    r,
                    FrameRefill::Full {
                        cause: FullRefillCause::ScratchScrolled
                    }
                );
            } else {
                assert_eq!(r, FrameRefill::Scoped { rows_refilled: 1 });
            }
        }
        term.scroll_display(-1);
        let r = tally(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "clamped at bottom"),
            &mut n_scoped,
            &mut n_full,
        );
        assert!(
            matches!(r, FrameRefill::Scoped { .. }),
            "at the live bottom the LIVE scoped arm answers, got {r:?}"
        );

        // Back up in 3-row bites until the top clamps the last one short.
        let mut entered = false;
        loop {
            let before = term.grid().display_offset();
            term.scroll_display(3);
            let after = term.grid().display_offset();
            if after == before {
                break;
            }
            let r = tally(
                scr2_step(&mut term, &mut scratch, ROWS, COLS, "3-row bite up"),
                &mut n_scoped,
                &mut n_full,
            );
            if entered {
                assert_eq!(
                    r,
                    FrameRefill::Scoped {
                        rows_refilled: after - before
                    },
                    "a bite exposes exactly the rows the offset moved (clamped at the top)"
                );
            } else {
                assert_eq!(
                    r,
                    FrameRefill::Full {
                        cause: FullRefillCause::EngineScrolled
                    }
                );
                entered = true;
            }
        }
        assert_eq!(
            term.grid().display_offset(),
            17,
            "the loop ran to the top of history"
        );

        // Output while parked with live rows on screen: the re-pin keeps the
        // same rows in view, but a batch is content, so the full arm.
        term.scroll_display(-12);
        let r = tally(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "12-row jump down"),
            &mut n_scoped,
            &mut n_full,
        );
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::EngineScrolled
            }
        );
        let base_y_before = scratch.base_y;
        term.process(b"tail\r\n");
        assert_eq!(
            term.grid().display_offset(),
            6,
            "SCR-1 re-pinned the reading position"
        );
        let r = tally(
            scr2_step(
                &mut term,
                &mut scratch,
                ROWS,
                COLS,
                "output re-pinned the viewport",
            ),
            &mut n_scoped,
            &mut n_full,
        );
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::EngineScrolled
            },
            "a re-pin is content: the full arm"
        );
        assert_eq!(
            scratch.base_y,
            base_y_before + 1,
            "the live grid scrolled one line"
        );
        term.scroll_display(1);
        let r = tally(
            scr2_step(&mut term, &mut scratch, ROWS, COLS, "step after output"),
            &mut n_scoped,
            &mut n_full,
        );
        assert_eq!(
            r,
            FrameRefill::Scoped { rows_refilled: 1 },
            "the scrub is scoped again on the frame after the output"
        );
        term.process(b"\x1b[1;1HX");
        let r = tally(
            scr2_step(
                &mut term,
                &mut scratch,
                ROWS,
                COLS,
                "write into a visible live row",
            ),
            &mut n_scoped,
            &mut n_full,
        );
        assert_eq!(
            r,
            FrameRefill::Full {
                cause: FullRefillCause::EngineScrolled
            },
            "a write into a visible live row while parked is content: the full arm"
        );
        let r = tally(
            scr2_step(
                &mut term,
                &mut scratch,
                ROWS,
                COLS,
                "parked after the write",
            ),
            &mut n_scoped,
            &mut n_full,
        );
        assert_eq!(r, FrameRefill::Scoped { rows_refilled: 0 });

        assert!(
            n_scoped >= 20,
            "the scrolled scoped arm must carry the scrub: {n_scoped} scoped / {n_full} full"
        );
        assert!(
            n_full >= 5,
            "the full arm must still answer every content/identity break: {n_full} full"
        );
    }
}
