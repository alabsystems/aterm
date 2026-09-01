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
use crate::grid::{Cell, CellFlags};

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
    /// True when this (lead) cell requested EMOJI presentation: a text-default
    /// emoji base char (`is_vs16_emoji_capable`) that VS16 (U+FE0F) widened to
    /// two cells. Such a char has a monochrome glyph in the text fonts but the
    /// selector asks for the colour form, so the renderer must prefer the
    /// colour-emoji face over the (otherwise-winning) mono primary/fallback.
    /// `❤️` (U+2764 U+FE0F) is the canonical case. Bare `❤` (no VS16) stays
    /// narrow and mono. SMP emoji (🚀) are already colour via the normal path.
    pub emoji_presentation: bool,
    /// True when this lead cell carries an explicit VS15 (U+FE0E) request whose
    /// final materialized geometry is narrow. This is distinct from the Unicode
    /// default presentation: a default-emoji scalar such as 😀 normally takes
    /// the colour path, but `😀︎` must take the text/mono path and stay in
    /// one cell. A later effective VS16 re-widens the cell and clears this state
    /// by construction (`CellFlags::WIDE` is then set again).
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
    /// A CHROME-ONLY channel, and deliberately so: ECMA-48 assigns 53/55 for
    /// the overline itself and NOTHING for its colour, and the one colour
    /// extension the terminal world actually agreed on — SGR 58/59 — is the
    /// UNDERLINE's (kitty's, adopted by VTE/iTerm2/Windows Terminal). Inventing
    /// an escape for this would mint a private sequence no other terminal
    /// answers to, so nothing in the SGR path or the grid's cell storage feeds
    /// this field: [`Terminal::render_row`] always leaves it `None`. What it
    /// exists for is aterm's OWN row builders (the chrome bands), where an
    /// overline is a structural SEAM rather than a rendition — a rule that must
    /// hold one tone across a row whose cells carry different inks.
    pub overline_color: Option<[u8; 3]>,
}

/// A blank cell in the DOCUMENTED empty shape — `ch` is `' '`, not the `'\0'`
/// a derived Default would produce (the struct doc promises `' '` for empty /
/// NUL cells, and the renderers blit `ch` unconditionally). Colors are black
/// on black: a `Default` cell is a structural placeholder, and every real
/// frame path resolves colors through the live palette before a renderer sees
/// them.
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
        }
    }
}

impl Terminal {
    /// Resolve the render-ready cell represented by an UNMATERIALIZED grid
    /// column.
    ///
    /// Grid rows are sparse: a missing tail is the terminal's implicit
    /// [`Cell::EMPTY`], not a request to inherit the last stored cell's SGR.
    /// Hosts use this value when padding a snapshot row to its declared width
    /// (split composition, web/wasm snapshots, control queries). Resolving it
    /// here keeps every host on the same live palette/default-color/DECSCNM path
    /// as materialized cells.
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
            ch: ' ',
            fg: [fg.r, fg.g, fg.b],
            bg: [bg.r, bg.g, bg.b],
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
    #[allow(
        clippy::too_many_lines,
        reason = "single per-cell resolution pass (colors + all decorations) over a row"
    )]
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

    /// LIVE-frame twin of [`render_row`](Self::render_row): resolve the row at
    /// `screen_row` IGNORING `display_offset`, so a socket introspection read
    /// (`cell`/`screen`/`cells`) gets the live on-screen colours + decorations that
    /// pair with the live [`cell_grapheme`](Self::cell_grapheme) glyph — never a
    /// scrolled-back row's colours stitched onto a live glyph. Identical to
    /// `render_row` at `display_offset == 0`; diverges only while the GUI is scrolled.
    #[must_use]
    pub fn render_row_at_screen(&self, screen_row: usize) -> Vec<RenderCell> {
        let mut out = Vec::new();
        self.render_row_into_impl(screen_row, &mut out, None, true);
        out
    }

    /// Core of [`render_row_into`](Self::render_row_into), with an optional
    /// `extras_out` sink for the emoji-cluster + combining-mark classification.
    ///
    /// `render_row_into` passes `None` (cells only). The per-frame snapshot
    /// builder [`cell_frame_into`](Self::cell_frame_into) passes
    /// `Some((clusters, combining))` so the cluster / combining extraction is
    /// FOLDED into this single per-cell loop — sharing the one flag-gated
    /// [`CellExtra`](aterm_grid::CellExtra) read this loop already does for the
    /// underline colour, instead of running `cluster_row_into` /
    /// `combining_row_into` as two extra full-grid passes that each re-probe the
    /// extras map per cell. The pushed data is byte-identical to those accessors.
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
        // Row source. `screen`: the LIVE-frame (offset-INDEPENDENT) row — the socket
        // introspection reads (`cell`/`screen`/`cells`) use this so their colours /
        // decorations / wide flags pair with the LIVE `cell_grapheme` glyph, never a
        // scrolled-back row's. Otherwise the display-offset-AWARE source the GUI
        // renderer needs: a scrolled-back row supplies its MATERIALIZED cells + extras
        // (correctly-paired emoji / combining / RGB from history). The two coincide at
        // display_offset == 0.
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
        for col in 0..cols {
            let Some(cell) = view.cell(col) else {
                continue;
            };

            // Unified flag-gated overflow lookup: one coordinated probe replaces
            // the separate fg_rgb_at / bg_rgb_at / resolved_char / cell_extra calls
            // and no-ops for the common non-overflow / non-complex / no-extras cell.
            // The view keys the LIVE probe on the display-mapped screen row (== the
            // raw row when not scrolled) and swaps in materialized history extras
            // for scrolled-back rows.
            let data = view.cell_data(col, cell);

            // Style-interned cells keep their colors in the StyleTable, so the
            // raw `colors()` of the cell is a StyleId payload. Rehydrate it to
            // an inline-colored cell (+ explicit RGB) before resolving, so the
            // resolver sees real packed colors. Inline cells take the fast path.
            // DEAD-BRANCH PROBE (see `SgrStyleHandler::apply_style_change`): no
            // production path sets `CellFlags::USES_STYLE_ID`. Every writer of that
            // bit — `Row::write_char_with_style_id`, `Cell::with_style_id`,
            // `Cell::set_style_id` — is `#[cfg(test)]`/`feature = "testing"`, and
            // the SGR path no longer interns anything for them to reference. The
            // rehydration below is kept because the CELL ENCODING still reserves
            // the bit (and a future "route non-`PackedColors` renditions through
            // the table" design would use it), but the whole suite now PROVES the
            // branch is unreachable in production instead of assuming it.
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

            // A TRUE wide continuation (the blank right half of a CJK glyph)
            // must be disambiguated from a DECSCA-protected cell: `PROTECTED`
            // and `WIDE_CONTINUATION` share bit 10, so the raw flag alone would
            // blank every protected character. A real continuation has bit 10
            // set, is not itself a WIDE main cell, and sits immediately right of
            // a WIDE cell. (Same rule as `Row::is_cell_wide_continuation`, done
            // inline here to reuse `grid_row` — render_row is a hot path.)
            let wide = cell.is_wide_continuation()
                && !cell.is_wide()
                && col > 0
                && view.cell(col - 1).is_some_and(|c| c.is_wide());
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

            // Emoji presentation: a text-default emoji base that VS16 widened to
            // 2 cells. Such a char is narrow by default, so a WIDE main cell
            // holding an emoji-capable base can ONLY have been widened by VS16
            // (`widen_previous_cell_for_vs16`). Lead cells only (`!wide`).
            let emoji_presentation =
                !wide && cell.is_wide() && super::handler::is_vs16_emoji_capable(ch);
            // VS15 text presentation is authoritative only while the resulting
            // lead remains narrow. This also handles selector replay exactly:
            // `⌚︎️` is wide again (VS16 took effect) and therefore not text,
            // while an ineffective later VS16 at the row edge leaves VS15 active.
            let text_presentation = !wide && !cell.is_wide() && marks.contains(&'\u{FE0E}');

            // Line decorations (SGR 4 family / 9 / 53).
            let cflags = eff_cell.flags();
            let underline = UnderlineStyle::from_flags(cflags);
            let strikethrough = cflags.contains(CellFlags::STRIKETHROUGH);
            let overline = cflags.contains(CellFlags::OVERLINE);
            let bold = cflags.contains(CellFlags::BOLD);
            let italic = cflags.contains(CellFlags::ITALIC);
            // Single CellExtra read (from the unified lookup above) serves the
            // SGR 58 underline colour AND the emoji-cluster / combining-mark
            // classification below. Live: the flag-gated live extra; history: the
            // materialized extra (shape-identical, so combining()/hyperlink() read
            // the same way; underline colour is not restored into scrollback).
            let extra = data.cell_extra();
            let underline_color = if underline == UnderlineStyle::None {
                None
            } else {
                extra.and_then(|e| {
                    // SGR 58:2 explicit RGB wins; SGR 58:5 stored a palette
                    // INDEX for draw-time resolution (W5g) — resolve it here
                    // against the LIVE palette (this extraction runs per frame,
                    // so an OSC 4 palette change re-colors the underline on the
                    // next frame, on both the CPU and GPU backends).
                    e.underline_color().or_else(|| {
                        e.underline_color_index().map(|i| {
                            let c = palette.get(i);
                            [c.r, c.g, c.b]
                        })
                    })
                })
            };
            // A HYPERLINK NOBODY CAN SEE IS A PHISHING VECTOR, not a quiet
            // convenience. OSC 8 lets the visible text and the destination be
            // unrelated — `google.com` addressed to evil.example is one escape
            // sequence — and a linked cell carried no mark whatsoever: same ink,
            // same weight, same everything as the prose around it, while
            // ctrl-click opened it through the desktop launcher with no preview
            // and no prompt. Measured on 0.61.0: `ctl cell` reported
            // `link=https://evil.example/steal` on cells rendering "google.com"
            // with attrs `none`.
            //
            // Underline is the mark every terminal and every browser already
            // shares for "this is a link", and it is what makes the text ask to
            // be inspected before it is trusted. A decoration the PROGRAM chose
            // outranks it — this fills only the undecorated case, so an author
            // who styled their own link keeps their styling — and the colour is
            // deliberately left `None` so the line takes the text's own ink
            // rather than inventing a link colour the theme never picked.
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
            });

            // Emoji-cluster / combining-mark extraction, folded in from
            // `cluster_row_into` / `combining_row_into` (byte-identical at
            // display_offset == 0) so the per-frame snapshot probes the extras
            // once per cell instead of in two extra full-grid passes. Only
            // requested by `cell_frame_into`. `marks()` reads the live combining
            // slice, or reconstructs a materialized cell's cluster tail.
            if let Some((clusters, combining_out)) = extras_out.as_mut() {
                if marks.iter().copied().any(is_emoji_sequence_marker) {
                    // Multi-codepoint EMOJI sequence (ZWJ / skin-tone / keycap /
                    // regional-indicator pair): surface the whole grapheme for
                    // shaping. Skip a NUL base, matching `cluster_row_into`.
                    if raw != '\0' {
                        let mut s = String::with_capacity(2 + marks.len());
                        s.push(raw);
                        s.extend(marks.iter().copied());
                        clusters.push((col as usize, s.into_boxed_str()));
                    }
                } else if !marks.is_empty() {
                    // Plain combining diacritics: overlay every mark except the
                    // VS15/VS16 presentation selectors, matching `combining_row_into`.
                    let overlay: Box<[char]> = marks
                        .iter()
                        .copied()
                        .filter(|&c| c != '\u{FE0E}' && c != '\u{FE0F}')
                        .collect();
                    if !overlay.is_empty() {
                        combining_out.push((col as usize, overlay));
                    }
                }
            }
        }
    }

    /// Emoji grapheme-cluster strings for the visible `row`, sparse: one
    /// `(col, cluster)` per cell whose combining marks form a multi-codepoint
    /// EMOJI sequence — a ZWJ sequence (👨‍👩‍👧), a skin-tone modifier (👍🏽), or
    /// an enclosing keycap (1️⃣). The renderer shapes each cluster to a single
    /// colour glyph; without this it would only see the base codepoint and draw
    /// just the first component.
    ///
    /// Deliberately EXCLUDES pure VS15/VS16 clusters (e.g. ❤️) — those keep the
    /// presentation-selector path ([`RenderCell::emoji_presentation`]), which is
    /// already CPU/GPU-consistent. `col` is the wide lead cell (the base char's
    /// column), matching where the renderer blits the glyph.
    #[must_use]
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

    /// Combining MARKS to overlay per cell of the visible `row`, sparse: one
    /// `(col, marks)` for each cell carrying combining diacritics (é = e + U+0301,
    /// ñ = n + U+0303, …). The renderer blits each mark's glyph over the base so
    /// the accent shows; without this only the base code point is drawn.
    ///
    /// Excludes cells handled elsewhere: emoji sequences (a sequence marker is
    /// present — [`cluster_row`](Self::cluster_row) shapes those) and the bare
    /// VS15/VS16 selectors ([`RenderCell::emoji_presentation`]). Marks are kept
    /// in arrival order so stacked diacritics layer correctly.
    #[must_use]
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
            // Overlay every combining char except the presentation selectors,
            // which only widen/narrow the base (no glyph of their own).
            let marks: Box<[char]> = combining
                .iter()
                .copied()
                .filter(|&c| c != '\u{FE0E}' && c != '\u{FE0F}')
                .collect();
            if marks.is_empty() {
                continue;
            }
            out.push((col as usize, marks));
        }
    }

    /// Inline-image placements for the visible `row`, sparse: one `(col,
    /// ImageRef)` for every cell covered by an iTerm2 OSC 1337 `File=` image. The
    /// renderer decodes each image once (keyed by the `Arc` inside the ref) and
    /// blits the cell's tile; a covered cell skips its glyph (its background still
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
            // SCROLLED-BACK row: the picture is no longer in the live extras map
            // — it left with its row and now rides the history line's image
            // spans, which the materializer expands back into per-cell
            // `ImageRef`s. Read it there, or a scrolled-back image is a hole.
            //
            // Kitty Unicode PLACEHOLDER cells are deliberately not resolved for
            // a history row (same rule as the batch fill, which this is the
            // parity oracle for): `placeholder_image_ref` reads the LIVE grid's
            // `resolved_char`, which is not this row's.
            if let aterm_grid::VisibleRowView::History { mat } = grid.visible_row_view(visible_row)
            {
                let cols = grid.cols();
                for (col, extra) in mat.extras_iter() {
                    if col < cols
                        && let Some(image) = extra.image()
                    {
                        out.push((col as usize, image.clone()));
                    }
                }
                out.sort_unstable_by_key(|&(col, _)| col);
            }
            return;
        }
        // Fast path: with no extras anywhere there are no image cells, so the
        // common case (plain text) pays a single bool check.
        if grid.extras().is_empty() {
            return;
        }
        // The live extras map is keyed by the SCREEN row; at a scrolled viewport
        // the live rows sit `display_offset` slots lower on screen. Identity
        // when not scrolled.
        let screen_row = visible_row.saturating_sub(u16::try_from(offset).unwrap_or(u16::MAX));
        // Scan the FULL grid width, not `grid_row.len()`: an image cell carries
        // only an extra (no glyph), so the row may not be materialized to full
        // width — `Row::len()` can be 0 while the image extras live in the extras
        // map. `cell_extra` reads that map directly, independent of materialization.
        if screen_row >= grid.rows() {
            return;
        }
        // A Kitty Unicode placeholder can only resolve to a stored image, and
        // `placeholder_image_ref` ends in `self.transient.kitty_images.get(&id)?`
        // — so with no images transmitted it is provably `None` for every cell.
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
            if let Some(image) = extra.image() {
                out.push((col as usize, image.clone()));
            } else if has_kitty
                && let Some(iref) = self.placeholder_image_ref(screen_row, col, extra)
            {
                // Kitty Unicode placeholder cell: synthesize an ImageRef so it rides
                // the same (pixel-tested) render path as a direct placement.
                out.push((col as usize, iref));
            }
        }
    }

    /// Every visible row's inline-image placements, in ONE pass over the extras
    /// map: the frame-level twin of [`images_row`](Self::images_row), allocating
    /// a fresh `Vec` per row exactly as [`cell_frame`](Self::cell_frame) wraps
    /// [`cell_frame_into`](Self::cell_frame_into).
    ///
    /// Same rows, same ascending-column order within a row, same `ImageRef`s as
    /// calling `images_row` for every row in `0..rows` (pinned by
    /// `images_frame_into_matches_per_row`) — for the cost of the extras that
    /// exist rather than `rows x cols` hash probes. This is the reader for an
    /// off-frame whole-screen gather (the control socket's styled frame); the
    /// windowed frontend takes the scratch-reusing fill through
    /// [`cell_frame_into`](Self::cell_frame_into) instead of allocating here.
    #[must_use]
    pub fn images_frame(&self, rows: usize) -> Vec<Vec<(usize, aterm_grid::ImageRef)>> {
        let mut images = Vec::new();
        self.images_frame_into(&mut images, rows);
        images
    }

    /// Batch equivalent of calling [`images_row_into`](Self::images_row_into)
    /// for every row in `0..rows`, driven by ONE pass over the extras map
    /// instead of `rows x cols` per-cell probes. The per-row scan's only
    /// early-out is the map being EMPTY, so a single hyperlink, colored
    /// underline, or the image itself anywhere on screen made every frame pay
    /// ~rows*cols hash probes under the terminal lock; iterating the map is
    /// proportional to the extras actually present instead.
    ///
    /// Output is byte-identical to the per-row accessor (see
    /// `images_frame_into_matches_per_row`): entries are bounds-checked
    /// against the CURRENT grid — stale rows/cols surviving a shrink are
    /// dropped, exactly as the per-row `0..cols` / `row < rows()` scan drops
    /// them implicitly — and each row is sorted by column afterwards (map
    /// iteration order is arbitrary; the per-row scan emits ascending
    /// columns, and per-frame consumers index into the row Vecs).
    ///
    /// Scratch-reuse contract matches the other `cell_frame_into` containers:
    /// the outer Vec is resized keeping existing inner Vecs, and each inner
    /// Vec is `clear()`ed in place so its capacity survives across frames.
    fn images_frame_into(&self, images: &mut Vec<Vec<(usize, aterm_grid::ImageRef)>>, rows: usize) {
        images.resize_with(rows, Vec::new);
        for row in images.iter_mut() {
            row.clear();
        }
        let grid = self.grid();
        let offset = grid.display_offset();
        // SCROLLED-BACK rows first: viewport rows `0..display_offset` are
        // history, and their pictures are no longer in the live extras map —
        // they left with their rows and now ride the history lines' image spans.
        // Each row's materialization is memoized (`viewport_row_cache`), and
        // `extras_iter` costs the entries that exist rather than `cols` probes.
        //
        // Kitty Unicode PLACEHOLDER cells are deliberately not resolved here:
        // `placeholder_image_ref` reads the live grid's `resolved_char`, which is
        // not the history row's, and the placeholder protocol has its own
        // history story. Direct placements — OSC 1337 and sixel — are what this
        // path restores.
        for (r, images_row) in images.iter_mut().enumerate().take(rows.min(offset)) {
            let Ok(visible_row) = u16::try_from(r) else {
                break;
            };
            if let aterm_grid::VisibleRowView::History { mat } = grid.visible_row_view(visible_row)
            {
                let cols = grid.cols();
                for (col, extra) in mat.extras_iter() {
                    if col < cols
                        && let Some(image) = extra.image()
                    {
                        images_row.push((usize::from(col), image.clone()));
                    }
                }
            }
        }
        // Fast path: with no extras anywhere there are no LIVE image cells, so
        // the common case (plain text) pays a single bool check.
        if grid.extras().is_empty() {
            for row in images.iter_mut() {
                row.sort_unstable_by_key(|&(col, _)| col);
            }
            return;
        }
        // `iter()` yields external coordinates with stale (scrolled-off)
        // entries already filtered; clamp the SCREEN row to the grid's extent
        // and the VIEWPORT row to the caller's `rows` (they can differ during a
        // resize, and by `display_offset` while scrolled back). The two bounds
        // coincide when the viewport is at the bottom.
        let max_screen_row = usize::from(grid.rows());
        // With no transmitted Kitty images, `placeholder_image_ref` is provably
        // `None` for every entry (its only `Some` return threads through
        // `self.transient.kitty_images.get(&image_id)?`), so the whole
        // placeholder branch — a `resolved_char` row lookup + cell read PER
        // non-image extra, i.e. per hyperlink, combining mark and underline
        // colour on screen — is dead work. Hoist the emptiness test out of the
        // loop: it is a pure `&self` read and `transient` cannot change while
        // this borrow is live, so the emitted rows are byte-identical.
        let has_kitty = !self.transient.kitty_images.is_empty();
        for (coord, extra) in grid.extras().iter() {
            #[cfg(test)]
            image_probe_meter::charge();
            // The map is keyed by SCREEN row; at a scrolled viewport a live row
            // sits `display_offset` slots lower on screen (identity at 0).
            let Some(r) = usize::from(coord.row).checked_add(offset) else {
                continue;
            };
            if usize::from(coord.row) >= max_screen_row || r >= rows || coord.col >= grid.cols() {
                continue;
            }
            if let Some(image) = extra.image() {
                images[r].push((usize::from(coord.col), image.clone()));
            } else if has_kitty
                && let Some(iref) = self.placeholder_image_ref(coord.row, coord.col, extra)
            {
                // Kitty Unicode placeholder cell: synthesize an ImageRef so it
                // rides the same (pixel-tested) render path as a direct placement.
                images[r].push((usize::from(coord.col), iref));
            }
        }
        for row in images.iter_mut() {
            row.sort_unstable_by_key(|&(col, _)| col);
        }
    }

    /// If the cell at (`row`,`col`) is a Kitty Unicode placeholder (U+10EEEE),
    /// decode its diacritics (row, col, image-id-high) + fg-color (image-id-low)
    /// and return an [`ImageRef`](aterm_grid::ImageRef) into the stored image. The
    /// pixel-exact sub-tile blit is the renderer's existing ImageRef job, so a
    /// virtual placement reuses the proven direct-placement compositor. Returns
    /// `None` for any non-placeholder cell or an unknown image id.
    fn placeholder_image_ref(
        &self,
        row: u16,
        col: u16,
        extra: &aterm_grid::CellExtra,
    ) -> Option<aterm_grid::ImageRef> {
        use super::kitty_placeholder::{PLACEHOLDER, diacritic_value};
        // The placeholder is non-BMP, so it always resolves via the overflow table.
        if self.grid().resolved_char(row, col) != Some(PLACEHOLDER) {
            return None;
        }
        let comb = extra.combining();
        let row_val = comb.first().and_then(|&c| diacritic_value(c)).unwrap_or(0);
        let col_val = comb.get(1).and_then(|&c| diacritic_value(c)).unwrap_or(0);
        let id_high = comb.get(2).and_then(|&c| diacritic_value(c)).unwrap_or(0) & 0xFF;
        let image_id = (id_high << 24) | self.cell_fg_image_id(row, col);
        let image = self.transient.kitty_images.get(&image_id)?.clone();
        Some(aterm_grid::ImageRef {
            image,
            cell_row: u16::try_from(row_val).unwrap_or(0),
            cell_col: u16::try_from(col_val).unwrap_or(0),
        })
    }

    /// The low 24 bits of a Kitty image id, encoded in a cell's foreground color:
    /// an RGB fg is `(r<<16)|(g<<8)|b`; an indexed fg is the palette index; a
    /// default fg is 0 (matching kitty's `colorToId`).
    fn cell_fg_image_id(&self, row: u16, col: u16) -> u32 {
        if let Some([r, g, b]) = self.grid().fg_rgb_at(row, col) {
            return (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
        }
        if let Some(cell) = self.grid().cell(row, col) {
            let colors = cell.colors();
            if colors.fg_is_indexed() {
                return u32::from(colors.fg_index());
            }
        }
        0
    }

    /// Build the engine's render SNAPSHOT for one frame (`read_image`, REARCH A-3):
    /// a plain-owned [`RenderInput`](crate::render::RenderInput) a renderer can
    /// paint WITHOUT any `&Terminal` borrow, allocating a fresh value each call.
    ///
    /// This is the engine-side replacement for the renderer's old reach-in
    /// (`aterm_render::Renderer::extract`): the engine now EMITS the snapshot, so
    /// `aterm-render` / `aterm-gpu` consume only the value and never touch core
    /// internals. The per-frame, allocation-reusing path is
    /// [`cell_frame_into`](Self::cell_frame_into); this wrapper allocates then
    /// delegates, so the two produce byte-identical snapshots. Live default
    /// background and cursor colors are resolved here with the cells, including
    /// OSC resets, dynamic cursor fallback, and DECSCNM.
    ///
    /// `&mut self` because the snapshot is stamped with
    /// [`damage_epoch`](Self::damage_epoch) (which latches), not because the fill
    /// mutates the grid.
    #[must_use]
    pub fn cell_frame(&mut self, rows: usize, cols: usize) -> crate::render::RenderInput {
        let mut scratch = crate::render::RenderInput::empty();
        self.cell_frame_into(&mut scratch, rows, cols);
        scratch
    }

    /// Like [`cell_frame`](Self::cell_frame), but REFILLS a caller-owned `scratch`
    /// [`RenderInput`](crate::render::RenderInput) in place instead of allocating a
    /// fresh one each frame — the per-frame hot path the windowed frontend calls on
    /// a kept scratch UNDER the `Terminal` lock.
    ///
    /// The three per-row container Vecs of Vecs (`cells`, `clusters`, `combining`)
    /// are resized to `rows` REUSING their existing inner per-row Vecs in place
    /// (truncating if shorter, pushing fresh empty Vecs if longer), then each row's
    /// inner Vec is `clear()`ed + refilled by the matching `*_row_into` accessor. So
    /// when the grid dimensions are stable (the common case: same window, frame
    /// after frame) NEITHER the outer Vecs NOR the inner per-row Vecs reallocate.
    /// `line_sizes` is `.clear()`ed (its elements are `Copy`, no inner allocation);
    /// pane-local `line_size_spans` and `default_bg_spans` rows are cleared in
    /// place because an engine snapshot is never a composed split frame.
    /// The data is byte-for-byte identical to what [`cell_frame`](Self::cell_frame)
    /// produces.
    ///
    /// Per-frame allocation of the four containers AND the per-row inner Vecs is
    /// elided. What still allocates is the owned cluster/mark CONTENT (`Box<str>`
    /// per emoji cluster, `Box<[char]>` per combining cell) the `*_row_into`
    /// accessors push — per-cluster owned data, only present for emoji/diacritic
    /// cells. Plain ASCII rows push none of those, so they are allocation-free in
    /// steady state.
    ///
    /// IMPORTANT: do NOT `.clear()` the outer container Vecs — that drops the inner
    /// per-row Vecs, throwing away their grown capacity and forcing a fresh
    /// allocation per row next frame. Resize-in-place is what preserves the inner
    /// buffers.
    ///
    /// The snapshot is stamped with [`damage_epoch`](Self::damage_epoch) as its
    /// [`snapshot_seq`](crate::render::RenderInput::snapshot_seq): the monotone
    /// version of the engine state this frame reflects. Because the whole snapshot
    /// is filled under the one lock the caller holds, that seq is internally
    /// consistent (no torn read) and a later `damage_epoch()` lets the caller detect
    /// staleness. This builds on the EXISTING epoch (O(1); already read for the
    /// frontend's coarse present early-out) — no new counter and no extra damage
    /// scan.
    pub fn cell_frame_into(
        &mut self,
        scratch: &mut crate::render::RenderInput,
        rows: usize,
        cols: usize,
    ) {
        self.cell_frame_fill(scratch, rows, cols, None);
    }

    /// Damage-scoped variant of [`cell_frame_into`](Self::cell_frame_into) that
    /// also CONSUMES the damage session (DMG-1: the damage carrier crossing the
    /// engine boundary).
    ///
    /// The grid's `DamageTracker` already knows, cell-granularly, which rows
    /// changed since the last [`take_damage`](Self::take_damage) — but the
    /// historical pipeline discarded that at this boundary and re-resolved
    /// EVERY visible cell per frame. This entry point refills ONLY the
    /// tracker's damaged rows into the caller-persistent `scratch` when — and
    /// only when — the scratch is provably a byte-identical baseline for the
    /// undamaged rows, and falls back to the full refill otherwise. Either way
    /// it then calls `take_damage()` (fill-and-consume, replacing the caller's
    /// historical `cell_frame_into(..); take_damage();` pair) and restamps the
    /// scratch's continuity tokens, so the next frame can chain.
    ///
    /// The `Full` arm names the clause that refused
    /// ([`FullRefillCause`](crate::render::FullRefillCause)). A caller that can
    /// only see THAT the chain broke cannot act on it: a forgotten host seq
    /// bump, a scratch shared across panes, and a workload that simply scrolls
    /// all read as the same number and have three different answers (fix the
    /// mutator, give the pane its own scratch, do nothing — scrolling is
    /// honestly Full). The cause is a fieldless `#[repr(usize)]` enum returned
    /// in a register, so this is free on the per-presented-frame path: nothing
    /// is formatted or allocated, and the Scoped arm is untouched. The
    /// discriminant width is measured, not stylistic — see
    /// [`FullRefillCause`](crate::render::FullRefillCause), where a `u8` is
    /// shown to cost the SCOPED arm an ABI cliff.
    ///
    /// The continuity proof, clause by clause (each one closes a documented
    /// unsoundness from the raster audit's RE-3 skip):
    /// - `terminal_id` match (nonzero): the scratch was last engine-filled by
    ///   THIS terminal — a compositor's scratch shared across same-dims panes
    ///   can never leak one pane's retained rows into another (per-terminal
    ///   `damage_epoch` values collide numerically; the identity nonce cannot).
    /// - `extract_gen` match: no consumer took damage since the scratch's fill,
    ///   so the tracker's bits are a SUPERSET of the rows that changed under
    ///   this scratch (a reset-then-accumulate window would undercount).
    /// - `snapshot_seq == engine_fill_seq`: no post-extraction HOST mutator
    ///   (stream fade, prediction ghosts, strip splice — all of which follow
    ///   the existing bump-`snapshot_seq` discipline) wrote content channels
    ///   since the engine filled the scratch.
    /// - dims + `cells.len()` match: resize-in-place preserved every row.
    /// - `display_offset == 0` on BOTH sides: tracker rows are LIVE-grid rows;
    ///   at offset 0 they coincide with viewport rows. A scrolled-back
    ///   viewport (or a transition) re-maps rows and takes the full arm.
    /// - `base_y` / `absolute_row_revision` match: no scroll or protected-
    ///   footer insertion shifted retained rows out from under their indices
    ///   (scroll damage marks only the EXPOSED strip — #6072 — so bits alone
    ///   cannot describe a shift).
    /// - alt bit match, and `!Damage::Full`.
    /// - `engine_row_order == RowOrder::Logical`: the reorder pass permutes rows in
    ///   place and is not idempotent, so a partial refill may only retain rows
    ///   that are still in LOGICAL order — i.e. rows the previous fill did not
    ///   permute. A frame that really did reorder an RTL row costs the NEXT
    ///   frame its scoped arm; a pure-LTR frame (every row identity, the
    ///   overwhelming majority even in a `bidi` build) keeps it. Mode/direction
    ///   changes need no clause of their own: each setter calls
    ///   `invalidate_bidi_all`, which marks FULL damage.
    ///
    /// Row granularity ONLY: the per-row column bounds the tracker maintains
    /// are deliberately not consulted — ligatures and clusters couple columns
    /// within a row, so column-scoped extraction is a correctness project, not
    /// an optimization (raster-audit hazard #2).
    ///
    /// Cost: `O(damaged rows × cols + rows)` instead of `O(rows × cols)` per
    /// frame — the `O(rows)` floor is the unconditional scalar/metadata restamp
    /// (cursor, selection, colors, `line_sizes`) plus the mask scan; the images
    /// pass stays a full extras-map walk (O(placed images), already cheap, and
    /// image placement need not mark row damage to stay fresh).
    ///
    /// Equality with the full path is pinned by the in-crate differential
    /// oracle `damage_scoped_extraction_matches_full_extract_over_mutation_corpus`
    /// (content-only `PartialEq` — the exact comparison the CPU renderer's
    /// damage cache uses).
    pub fn cell_frame_damage_scoped_into(
        &mut self,
        scratch: &mut crate::render::RenderInput,
        rows: usize,
        cols: usize,
    ) -> crate::render::FrameRefill {
        let Some(cause) = self.damage_scoped_refill_refusal(scratch, rows, cols) else {
            // Copy the tracker's row bits into an owned mask BEFORE the fill:
            // the tracker read borrows `&self`, the fill needs `&mut self`.
            // O(rows/64) words; `damaged_rows` skips clear words via
            // trailing_zeros, so an idle-grid frame costs the iterator setup.
            let mut mask = vec![0u64; rows.div_ceil(64)];
            let mut rows_refilled = 0usize;
            for r in self.grid().damage().damaged_rows(self.rows()) {
                let r = usize::from(r);
                if r < rows {
                    mask[r / 64] |= 1u64 << (r % 64);
                    rows_refilled += 1;
                }
            }
            self.cell_frame_fill(scratch, rows, cols, Some(&mask));
            self.take_damage();
            // The fill stamped the PRE-take generation; `take_damage` just
            // bumped it. Restamp: THIS scratch is the consumer that closed the
            // session, so it remains the valid baseline for the next frame.
            scratch.extract_gen = self.extract_gen;
            return crate::render::FrameRefill::Scoped { rows_refilled };
        };
        // Any continuity break lands here: always sound, costs exactly the
        // pre-carrier status quo, and re-establishes a valid baseline. The
        // refusing clause rides out with the arm so the caller can tally WHICH
        // one it was without re-deriving anything.
        //
        // NOT outlined into a cold `#[inline(never)]` callee. That was tried,
        // to keep `cause` (live across the two calls below) out of the hot
        // function's callee-saved set: it made no measurable difference —
        // `keystroke_tick/scoped_extract/24x80` read +0.49% outlined against
        // +0.40% inline, both inside that arm's own identical-binary control
        // envelope of +/-0.6% — so the simpler shape stays.
        self.cell_frame_fill(scratch, rows, cols, None);
        self.take_damage();
        scratch.extract_gen = self.extract_gen;
        crate::render::FrameRefill::Full { cause }
    }

    /// The damage-scoped continuity check — every clause is justified on
    /// [`cell_frame_damage_scoped_into`](Self::cell_frame_damage_scoped_into).
    /// Pure read; conservative by construction (any refusal merely buys the
    /// full refill).
    ///
    /// `None` means every clause held and the scoped arm is legal.
    /// `Some(cause)` names the FIRST clause that refused.
    ///
    /// SHAPE NOTE (this reports the proof, it does not edit it). This was a
    /// `-> bool` whose clauses were one `&&` chain. `&&` is left-to-right and
    /// short-circuiting, and every operand here is a pure `&self` read with no
    /// interior mutability, so rewriting the chain as the same clauses in the
    /// same order, each returning its own cause, is a semantics-preserving
    /// transcription: the set of `(scratch, rows, cols)` that pass is
    /// unchanged, clause for clause and order for order. Two conjuncts were
    /// SPLIT (`terminal_id != 0 && terminal_id == identity`, and the four
    /// dimension comparisons) — splitting a conjunction into consecutive
    /// refusals cannot change the verdict, only the label it refuses under, and
    /// the labels are the point: "this scratch was never filled" and "this
    /// scratch belongs to another terminal" are different bugs.
    ///
    /// Nothing here may be reordered for a nicer attribution. A clause's
    /// position IS part of what its cause means (a scrolled frame that also
    /// took full damage reports the scroll), and the corpus below pins those
    /// labels, so a reorder that looked cosmetic would show up as a test
    /// failure rather than as silently relabelled telemetry.
    fn damage_scoped_refill_refusal(
        &self,
        scratch: &crate::render::RenderInput,
        rows: usize,
        cols: usize,
    ) -> Option<crate::render::FullRefillCause> {
        use crate::render::FullRefillCause as Cause;
        // BiDi reorder is an in-place, non-idempotent row permutation applied
        // at fill time, so retained rows may only be kept while they are still
        // in LOGICAL order. That is precisely what the carrier's
        // `engine_row_order` records about the LAST engine fill — not
        // whether the feature is compiled in, and not whether the runtime mode
        // is on.
        //
        // The clause this replaces vetoed on `bidi_mode != Disabled`. It read
        // as conservative, but `BiDiMode`'s `#[default]` is `Implicit` and
        // `aterm-gui` enables the `bidi` feature, so it vetoed EVERY frame of
        // the shipping app: the damage-scoped arm could never fire where it
        // matters, and only the workspace-unified feature build of the oracle's
        // reach guards ("an echo on a settled scratch must take the scoped
        // arm") could see it. A mode/direction change cannot slip past this
        // token either — every setter calls `invalidate_bidi_all`, which marks
        // FULL damage and is caught by the `!is_full()` clause below.
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
        // No host row PREPEND is still in force. A spliced scratch holds
        // engine row `r` at index `r + row_shift`, so retaining "row r"
        // would serve a DIFFERENT row's content — the exact stale-row
        // defect this whole carrier exists to make impossible. The
        // frontend inverts its tab-strip prepend
        // (`RenderInput::undo_host_row_prepend`) before handing the scratch
        // back; anything it could not invert arrives here still shifted and
        // takes the full arm. The `rows`/`cells.len()` clauses below
        // already reject the common shape, but they reject it by arithmetic
        // coincidence — this clause rejects it by name, and now reports it
        // by name too.
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

    /// The one shared fill body behind [`cell_frame_into`](Self::cell_frame_into)
    /// (`refill_mask: None` — the historical unconditional walk, byte-identical)
    /// and the damage-scoped arm (`Some(mask)` — only marked rows re-resolve;
    /// the caller has already proven every unmarked row byte-identical in the
    /// scratch). EVERYTHING except the per-row cell loop runs identically on
    /// both arms: images, `line_sizes`, span hygiene, cursor/selection/color
    /// scalars, seq + carrier stamps, BiDi — so the scoped arm can never skew a
    /// scalar or a non-cell channel.
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

        // Resize the outer Vec-of-Vecs to `rows`, KEEPING the existing inner
        // per-row Vecs (their grown capacity), then refill each in place via the
        // `*_row_into` accessor (which clears + repushes). `resize_with` truncates
        // when `rows` shrank (dropping the surplus inner Vecs) and appends fresh
        // empty Vecs when `rows` grew; the `0..len` already-present rows keep their
        // buffers untouched until the per-row `*_into` clear+refill below.
        // One combined pass fills cells + emoji-clusters + combining marks per
        // row, so the extras map is probed once per cell (inside
        // `render_row_into_impl`) instead of in three independent full-grid
        // scans. `cells[r]` / `clusters[r]` / `combining[r]` are disjoint fields,
        // so the three per-row borrows coexist.
        scratch.cells.resize_with(rows, Vec::new);
        scratch.clusters.resize_with(rows, Vec::new);
        scratch.combining.resize_with(rows, Vec::new);
        for r in 0..rows {
            // Damage-scoped arm (DMG-1): rows the tracker did not mark are
            // byte-identical in the scratch (the caller's continuity proof),
            // so their re-resolve is skipped — including the cluster/combining
            // refill, whose per-row Vecs likewise still hold this row's data.
            // Row granularity only; the col bounds are deliberately unused.
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

        // Images stay a SEPARATE pass: an image/placeholder cell carries only an
        // extra (no glyph), so the row may be unmaterialized and `render_row_into`
        // (which iterates `grid_row.len()`) would miss it. The batch fill walks
        // the extras map ONCE for the whole frame (instead of rows x cols probes
        // whenever ANY extra exists) and is byte-identical to the per-row
        // `images_row_into` — do NOT fold it into the cell pass.
        self.images_frame_into(&mut scratch.images, rows);

        // A single terminal's frame is UNIFORM per row, so it never carries
        // line-size runs. Clearing is load-bearing, not hygiene: this scratch is
        // reused across frames, and a window that was split a moment ago left
        // per-pane runs here. Without this, dropping back to one pane would keep
        // scaling columns by a pane that no longer exists.
        scratch.line_size_spans.clear();
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
        // …and its per-pane list, for the identical reason and with a stronger
        // consequence: a NON-EMPTY `selections` makes the renderer read the list
        // INSTEAD of the scalar selection this extraction is about to stamp, so
        // a composed frame's panes left in a reused scratch would replace this
        // terminal's highlight wholesale.
        scratch.selections.clear();

        let cur = self.cursor();
        scratch.cursor_col = cur.col as usize;
        let display_offset = self.grid().display_offset();
        // The DEC cursor is anchored in the ACTIVE grid; the viewport row it
        // occupies is its active-grid row pushed DOWN by the scrollback offset
        // (older history scrolls in at the top, so live content — the cursor's
        // row included — slides toward the bottom). Project it there and show
        // it only while that projected row is still on screen — matching
        // xterm/kitty, where a small scroll-back that leaves the cursor row
        // visible keeps the cursor drawn. A larger scroll pushes the projected
        // row off the bottom (`>= rows`) and it fails closed, exactly as the
        // whole-history case did before (this replaces the blanket
        // `display_offset == 0` hide). Host copy/vi modes still override this
        // snapshot after extraction with their own history-space cursor; the
        // effects-layer history blackout is independent of this projection.
        //
        // Both halves come from the accessor pair that IS this projection
        // ([`Terminal::projected_cursor_row`] / [`Terminal::cursor_row_on_screen`]),
        // so a host that has to know where a pane's caret is in the frame it is
        // painting asks the extraction's own rule rather than a second copy of
        // the arithmetic that can drift away from it.
        scratch.cursor_row = self.projected_cursor_row();
        scratch.cursor_visible = self.cursor_row_on_screen(rows).is_some();
        scratch.cursor_style = self.cursor_style();
        scratch.display_offset = display_offset as i32;
        // Capture base_y (absolute row of the top visible line) under the SAME lock as
        // the cells, so a host that re-anchors absolute rows into this frame (⌘F find
        // highlight) uses a value consistent with the exact grid just extracted.
        scratch.base_y = i64::try_from(self.grid().base_y()).unwrap_or(i64::MAX);
        // Capture the protected-footer insertion revision at the same extraction
        // boundary. A host can therefore fail closed when retained absolute-row
        // geometry no longer describes this exact frame.
        scratch.absolute_row_revision = self.absolute_row_revision();
        // `clone_from` reuses the destination's existing allocation where the
        // selection's owned data permits, instead of dropping + reallocating.
        scratch.selection.clone_from(self.text_selection());
        let pack = |color: aterm_types::Rgb| {
            (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
        };
        scratch.selection_bg = self
            .selection_background()
            .map_or(crate::render::COLOR_UNSET, pack);
        scratch.selection_fg = self
            .selection_foreground()
            .map_or(crate::render::COLOR_DYNAMIC, pack);
        // These colors describe the terminal snapshot itself, not host
        // presentation policy. Stamp them at the same extraction boundary as
        // the sparse cells so every direct `cell_frame*` consumer sees one
        // coherent OSC/DECSCNM state. In particular, an unmaterialized row tail
        // is `Cell::EMPTY`: its effective background follows OSC 11 and swaps
        // with the live foreground under DECSCNM. A dynamic cursor (OSC 21
        // `cursor=`) follows the live OSC 10 foreground.
        let reverse_video = self.modes().reverse_video();
        // ONE blank cell feeds BOTH stamps, so whenever both fire they are
        // coherent by construction: `implicit_blank_render_cell` runs
        // `Cell::EMPTY` through `resolve_colors(.., reverse_video)`, and
        // DECSCNM swaps the pair together. But the two AUTHORITY tests are
        // SEPARATE, and must be.
        //
        // Folding the foreground's authority into the background's gate is a
        // real regression (found in review): a terminal configured with OSC 10
        // ALONE would then stamp its VT-spec black as an authoritative
        // background, and the renderer's host-theme fallback for the padding
        // band and base clear would be lost on a frame where OSC 11 was never
        // sent. Each half answers only for itself; DECSCNM arms both because it
        // genuinely makes both authoritative.
        let implicit_blank = self.implicit_blank_render_cell();
        if self.color.frame_background_authoritative || reverse_video {
            scratch.default_bg = (u32::from(implicit_blank.bg[0]) << 16)
                | (u32::from(implicit_blank.bg[1]) << 8)
                | u32::from(implicit_blank.bg[2]);
        } else {
            // Preserve the standalone-renderer compatibility contract for a
            // pristine, unconfigured `Terminal::new`: its VT-spec black is not
            // a host theme decision. Any host/OSC background or DECSCNM state
            // above makes the terminal authoritative.
            scratch.default_bg = crate::render::COLOR_UNSET;
        }
        if self.color.frame_foreground_authoritative || reverse_video {
            scratch.default_fg = (u32::from(implicit_blank.fg[0]) << 16)
                | (u32::from(implicit_blank.fg[1]) << 8)
                | u32::from(implicit_blank.fg[2]);
        } else {
            scratch.default_fg = crate::render::COLOR_UNSET;
        }
        if self.color.frame_cursor_authoritative
            || (self.color.cursor_color.is_none() && self.color.frame_foreground_authoritative)
        {
            scratch.cursor_color = pack(
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

        // DMG-1 damage carrier: extraction-continuity tokens. Stamped by EVERY
        // engine fill (full or scoped) so any scratch can later prove — or
        // fail — scoped-refill eligibility. `engine_fill_seq` snapshots the
        // seq the ENGINE stamped; a host mutator that bumps `snapshot_seq`
        // afterwards (the established fade/ghost/splice discipline) thereby
        // self-reports and forces the next fill to the full arm.
        scratch.terminal_id = self.extract_identity;
        scratch.extract_gen = self.extract_gen;
        scratch.engine_alt = self.is_alternate_screen();
        scratch.engine_fill_seq = scratch.snapshot_seq;
        // D-2 SPLICE: an engine fill is, by definition, an UNSHIFTED engine
        // fill — every host row prepend this scratch may have carried is gone
        // (the frontend either inverted it or the resize above dropped it), so
        // the shift count returns to zero and the provenance token is re-armed
        // beside `engine_fill_seq`. From here only a blessed prepend
        // (`note_host_row_prepend`) can keep the two in step; every other host
        // cell write bumps `snapshot_seq` alone and thereby disowns the lane.
        scratch.row_shift = 0;
        scratch.shifted_fill_seq = scratch.snapshot_seq;
        // K2: an engine fill is the one thing a COMPOSITE is not — these cells
        // came from one terminal's grid, not from a compositor's pane
        // rectangles — so a retention ledger describing a previous composed
        // frame in this reused buffer is disowned here, unconditionally.
        scratch.composed_fill_seq = 0;

        // D-2 PER-ROW REVISION LANE. The grid already knows which rows changed;
        // publish that fact instead of letting every consumer re-derive it by
        // comparing every cell of every row against a full copy of the previous
        // frame. `row_revisions` folds the CURRENT damage session first, so the
        // stamps describe the exact grid state the cells above were read from.
        //
        // Copied per row rather than aliased because the snapshot outlives the
        // lock. `rows` is the SNAPSHOT's row count, which a host may later grow
        // (a spliced tab strip); the lane is stamped at the engine's row count
        // and any host that re-shapes the rows leaves the length mismatched,
        // which the consumer reads as "do not trust" — fail closed.
        scratch.row_rev.clear();
        {
            let revisions = self.grid_mut().row_revisions();
            scratch.row_rev.extend_from_slice(revisions);
        }
        scratch.row_rev.resize(rows, 0);
        // The engine fill vouches for the lane — EXCEPT on a `Damage::Full`
        // session, which publishes no lane at all.
        //
        // WHY FULL IS EXCLUDED: `Damage::Full` keeps no tracker and therefore no
        // mark clock, so the fold cannot tell a repeated read of one full session
        // from a session that took another write in between. Its only sound
        // answer is to re-stamp every row on every fold — which would make two
        // extracts of an IDENTICAL full-damage screen (a standalone renderer that
        // never consumes damage does exactly this) compare as a whole-screen
        // repaint instead of the gate hit they are. Publishing no lane hands
        // those frames back to the exact whole-grid compare, which gets both the
        // gate hit AND the change right. Full-damage frames repaint everything
        // anyway, so the compare they pay for is not a cost this lane was ever
        // going to save.
        scratch.row_rev_lane = if self.grid().damage().is_full() {
            0
        } else {
            self.extract_identity
        };

        // BiDi visual reordering (feature `bidi`): permute each row into visual
        // order so RTL runs display correctly on BOTH renderers and in the
        // `image` capture. No-op for pure-LTR frames and when the feature is off
        // (byte-identical). See terminal/bidi_reorder.rs::apply_bidi_reorder.
        // DMG-1: the reorder pass is mask-aware (retained rows are provably
        // still the identity permutation — see `apply_bidi_reorder`), and it
        // REPORTS whether it permuted anything so the next refill's continuity
        // check can require logical order. Feature-off builds stamp `false`,
        // which is exactly true: no permutation exists to undo.
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

/// A combining char that marks its cell as a multi-codepoint EMOJI sequence:
/// ZWJ (U+200D, family/role sequences), an emoji skin-tone modifier
/// (U+1F3FB–U+1F3FF), COMBINING ENCLOSING KEYCAP (U+20E3), or a regional
/// indicator (U+1F1E6–U+1F1FF, the second half of a flag pair the writer folds
/// into one cell). VS15/VS16 are presentation selectors, not sequence markers,
/// and are excluded on purpose.
#[inline]
fn is_emoji_sequence_marker(c: char) -> bool {
    matches!(c as u32, 0x200D | 0x20E3 | 0x1F3FB..=0x1F3FF | 0x1F1E6..=0x1F1FF)
}

/// Deterministic cost meter for the two inline-image readers, in the tree's
/// established shape (`aterm_grid::test_counters`): a complexity claim about a
/// screen read is asserted as an OPERATION COUNT, not as wall-clock, which no
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

    /// A LINK HAS TO LOOK LIKE ONE. OSC 8 addresses arbitrary text to an
    /// arbitrary URI, ctrl-click opens it with no preview, and before this the
    /// linked cells rendered byte-identically to the prose around them.
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

        // A decoration the program CHOSE outranks the link's: an author who
        // styled their own link keeps their styling, curl and all.
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
    fn cell_frame_stamps_absolute_row_revision() {
        let mut term = Terminal::new(5, 10);
        let before = term.cell_frame(5, 10);
        assert_eq!(before.absolute_row_revision, 0);

        // Insert one history row before two protected footer rows. This is the
        // non-uniform coordinate change the frame stamp exists to fence. The
        // displaced top row must be WRITTEN — never-written rows scroll
        // history-free (no archival, no splice, no revision bump).
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

    /// The batch `images_frame_into` fill must be byte-identical to the per-row
    /// `images_row` reference for every row — covering a direct iTerm2
    /// placement, a Kitty Unicode placeholder, non-image extras (hyperlink /
    /// colored underline) that must NOT emit entries, scrolled state (extras
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
        // nonzero row_offset AND stale scrolled-off entries the batch path
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

    /// The same batch-vs-per-row parity, with the viewport SCROLLED BACK over
    /// the image. The two readers take different routes for a history row — the
    /// batch loop walks the materialized row's sparse extras, the per-row
    /// accessor probes it — and a divergence there is a picture that appears in
    /// one code path and not the other.
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

    /// THE COST CLAIM, as an operation count (wall-clock cannot be asserted; the
    /// probe count can). The per-row reader's ONLY early-out is the extras map
    /// being empty, so a screen carrying one hyperlink and no picture at all — an
    /// `ls --hyperlink` listing, a link in a TUI status line — pays a `cell_extra`
    /// probe for every cell on it, on every whole-screen gather. The batch reader
    /// pays one lookup per extra that actually exists.
    ///
    /// Both states are asserted, because the interesting half is the second one:
    /// the emptiness gate really does rescue a plain-text screen (0 probes either
    /// way), and it really does NOT rescue the hyperlink screen.
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

        // ONE hyperlink, zero images — the case the gate does not rescue.
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

    /// A bare `Terminal::new()` defaults to the SINGLE-SOURCE constants in aterm-types
    /// (pins the constructor boundary; transient_state used 229 while
    /// `TerminalConfig::default` used 255 before they were unified — see the color
    /// audit) (N2).
    #[test]
    fn terminal_new_defaults_are_single_source() {
        let term = Terminal::new(2, 4);
        assert_eq!(term.default_foreground(), aterm_types::DEFAULT_FOREGROUND);
        assert_eq!(term.default_background(), aterm_types::DEFAULT_BACKGROUND);
    }

    /// OSC 110 (reset default fg) restores the CONFIGURED (themed) default — never a
    /// transient OSC-10 value nor the spec default. This is the reset-to-configured
    /// semantics behind the single-source fix (S10).
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
        // OSC 110 resets → back to the CONFIGURED themed value, not magenta/spec.
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
        // widens it to 2 cells AND requests colour presentation.
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
        // VS16 ❤️ must NOT be emitted (it keeps the emoji_presentation path).
        assert!(
            clusters.iter().all(|(_, s)| !s.starts_with('\u{2764}')),
            "VS16 ❤️ must not be a shaping cluster, got {clusters:?}"
        );
    }

    #[test]
    fn regional_indicator_pair_folds_into_one_flag_cluster() {
        // 🇺🇸 = regional indicator U + S. The pair must fold into ONE 2-cell
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
        // alone in its own cell (it is NOT folded into the completed pair).
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

    /// W5(g): SGR 58:5:n (INDEXED underline colour) resolves against the LIVE
    /// palette at extraction time — it was previously parsed + stored (with a
    /// comment promising draw-time resolution) but never read, so an indexed
    /// underline silently fell back to the glyph fg on both backends.
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
        // LIVE resolution: an OSC 4 palette redefinition re-colors the SAME
        // cell on the next extraction (the whole point of storing the index).
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
        // The emoji family is NOT a combining-overlay cell (cluster_row owns it).
        let family_col = 2; // after "e\u{0301} " (cols 0,1)
        assert!(
            comb.iter().all(|(c, _)| *c != family_col),
            "emoji cluster must not be a combining-overlay cell, got {comb:?}"
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

    #[test]
    fn wide_cjk_is_not_emoji_presentation() {
        // A naturally-wide CJK char is wide but NOT emoji-capable, so it must
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
        // WIDE_CONTINUATION. Protected characters must still render their glyph
        // — they are NOT wide-continuation spacers. Regression for the bit-10
        // collision that blanked every DECSCA-protected cell.
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

    #[test]
    fn render_row_wide_continuation_is_blanked() {
        // A real wide char (中, U+4E2D) occupies a WIDE lead cell + a
        // WIDE_CONTINUATION spacer. The lead keeps the glyph; the spacer renders
        // blank and is flagged `wide`. (Counterpart to the protected-cell case.)
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

    /// `render_row_at_screen` is the LIVE-frame twin of `render_row`: identical at
    /// `display_offset == 0`, but IGNORES a GUI scroll-back — so a socket `cell`/
    /// `screen`/`cells` read never pairs a scrolled-back row's colours/attrs with the
    /// live glyph (the round-4 scroll-frame defect).
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

        // The LIVE-frame read is unchanged by the scroll...
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
        // ...while the offset-AWARE render_row followed the scroll (proving the two
        // frames genuinely diverge, so the socket reads picking the live one matters).
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

        // …but a SMALL scroll that leaves the cursor's row on screen keeps the
        // cursor drawn, PROJECTED down by the offset (audit-2 item 16). A tall
        // terminal with the cursor near the top makes this observable: at
        // offset d the cursor sits at viewport row `cur.row + d`, still < rows.
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
        // Sanity: the row actually carries clusters AND a combining mark, so the
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

    /// DMG-1 differential oracle: the damage-scoped extraction must be
    /// CONTENT-identical to a fresh full extract after every step of a
    /// mutation corpus that covers each continuity clause — echo writes, SGR,
    /// cursor-only moves, wide/emoji/combining cells, scroll (base_y), viewport
    /// scroll (display_offset), DECSCNM + OSC-4 recolors (full-damage marks),
    /// alt-screen swaps, host splices (the bump-`snapshot_seq` discipline),
    /// foreign `take_damage`, and resize. Content equality uses `RenderInput`'s
    /// hand-written `PartialEq` — the EXACT comparison the CPU renderer's
    /// damage cache trusts, with carrier metadata excluded by design.
    ///
    /// TWO-SIDED REACH: the corpus must exercise BOTH arms — a run in which the
    /// scoped arm never fired (silent degradation to `Full` every frame) or the
    /// full arm never fired (a gate that cannot say no) fails the counters at
    /// the bottom, so this test also fences the fast path's existence, not just
    /// its correctness.
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
        // Deliberately NOT pinned to `rows_refilled: 0`: whether a CUP marks
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

        // 7) Viewport scroll: display_offset != 0 -> Full on entry AND while held.
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
        // The frame that RETURNS to the bottom: the engine is at offset 0 again,
        // but the scratch was filled while scrolled, so its retained rows are
        // viewport rows from the other mapping. Previously unasserted — the pair
        // of offset clauses is the sharpest case of an attribution that a single
        // `full` counter cannot express, because the two frames look identical
        // in it and have opposite explanations.
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
        // `RenderCell::fg`/`bg` are FINAL RESOLVED RGB (resolved against the
        // live palette at extraction time), so a recolor of an index that
        // painted cells reference changes their extracted BYTES while writing
        // no glyph — the exact "content changed without a grid write" shape the
        // scoped arm must never retain rows through. Two things this step needs
        // to actually test that, both of which the corpus originally missed:
        //
        //  a) OSC 4 palette reconfigure is FAIL-CLOSED by default (#7937
        //     F01-3: `allow_palette_reconfigure`). Without the opt-in the
        //     escape returns before touching the palette OR marking damage, so
        //     the step was a silent no-op that proved nothing.
        //  b) Nothing in the corpus was painted with the recolored index, so
        //     even a genuinely-missed recolor could not have diverged the
        //     oracle. Paint with index 2 FIRST, and settle the scratch, so the
        //     recolor below is a real byte-changing event on retained rows.
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

        // 12) HOST SPLICE: mutate a content channel + bump snapshot_seq (the
        // shipping fade/ghost/splice discipline). The next scoped attempt MUST
        // detect the bump and take the full arm — this is the RE-3 "host
        // splices leave scoped rows stale" hazard, closed.
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

        // 13) FOREIGN CONSUMER: another take_damage resets the tracker under us;
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

        // 14) BIDI VISUAL REORDER (feature `bidi` — which the shipping GUI
        // compiles in, and `BiDiMode`'s `#[default]` is `Implicit`, so this is
        // the app's real configuration). `apply_bidi_reorder` is an IN-PLACE,
        // NON-IDEMPOTENT row permutation, so the carrier may only retain rows
        // that are still in LOGICAL order. The fill reports whether it permuted
        // anything (`engine_row_order`) and the next refill's continuity
        // check requires that report to be false.
        //
        // Content equality is asserted on every frame below in BOTH feature
        // configurations (the `step` closure does it); only the ARM assertions
        // are feature-gated, because with the feature off no permutation exists
        // to undo. This leg exists because the clause it replaced — a blanket
        // `bidi_mode != Disabled` veto — was sound but DEAD: in a `bidi` build
        // it vetoed every frame, and the reach guards at the bottom of this test
        // were the only thing that could see it.
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

        // ...and the scoped arm must COME BACK once no row reorders any more.
        // Pinned so a future "once RTL, always full" regression is visible: the
        // price of an RTL row is paid while it is on screen, not for the rest of
        // the session.
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

        // TWO-SIDED REACH GUARDS: both arms must have really run.
        assert!(n_scoped >= 5, "scoped arm under-exercised: {n_scoped}");
        assert!(n_full >= 8, "full arm under-exercised: {n_full}");
    }

    /// DMG-1 ADVERSARIAL PROBE (campaign): randomized differential fuzz over a
    /// mutation alphabet chosen to attack the continuity proof, not to exercise
    /// it. Every step compares the scoped scratch against a FRESH full extract
    /// under the content-only `PartialEq` the CPU renderer's damage cache uses.
    /// Any stale retained row shows up as a divergence with a printable seed.
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
                // 1-4 mutations BETWEEN extractions. This is what makes the
                // probe able to build the killer interleaving the continuity
                // proof exists for: write(row A) -> foreign take_damage
                // (clears the bits) -> write(row B) -> extract. The tracker
                // now names only row B, while row A ALSO changed under the
                // scratch — a one-op-per-extract loop can never produce it.
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
                        // Palette recolor: changes RESOLVED rgb of painted cells
                        // with NO glyph write — the sharpest attack on the proof.
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
                        // RTL text: the ONE op whose extraction ends in an
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

    /// DMG-1 ALIASING PROBE (campaign): the split compositor's SHARED
    /// `pane_scratch` — two SAME-DIMS terminals alternately refilling ONE
    /// scratch. Every other continuity token (dims, anchors, offset, alt bit,
    /// seq) matches across the two panes, and their per-terminal
    /// `damage_epoch`/generation counters collide NUMERICALLY (both count from
    /// the same origin), so the process-unique `terminal_id` nonce is the ONLY
    /// thing standing between this workload and one pane serving the other's
    /// retained rows. Asserted by content, not by arm: a leak shows up as
    /// pane A's text appearing in pane B's frame.
    #[test]
    fn dmg1_shared_scratch_across_two_terminals_never_leaks_rows() {
        use crate::render::{FrameRefill, FullRefillCause};
        let (rows, cols) = (6usize, 24usize);
        let mut a = Terminal::new(rows as u16, cols as u16);
        let mut b = Terminal::new(rows as u16, cols as u16);
        a.process(b"AAAA pane one\r\nsecond A row");
        b.process(b"BBBB pane two\r\nsecond B row");

        // ONE scratch, alternating owners — the aliasing shape exactly.
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
            // ATTRIBUTED, not merely counted: after the very first fill the
            // identity nonce is the ONLY clause that may refuse here — every
            // other token matches across the two panes by construction — so a
            // run that fell back for some other reason would be proving
            // something else entirely, and would leave this probe passing while
            // no longer testing the nonce at all.
            //
            // Round 0's pane A is the exception, and the attribution is what
            // makes it visible: the shared scratch is EMPTY on the first fill,
            // so it refuses as `scratch_unstamped` (never engine-filled) rather
            // than on the nonce, which cannot discriminate what was never
            // stamped. Pinned rather than papered over, because it is exactly
            // the distinction the tally exists to draw: a window's first frame
            // is unavoidably Full, a hand-off's is a pane sharing a buffer.
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
        // Reach: an ALTERNATING shared scratch must take the FULL arm every
        // time (the identity nonce flips on every handoff). If this ever reads
        // as scoped, the nonce stopped discriminating and the assertions above
        // are the only thing left — fail loudly here instead.
        assert_eq!(
            leaked_full, 12,
            "a scratch alternating between two terminals must full-refill every frame, \
             and on the IDENTITY clause specifically"
        );
    }

    /// PER-CAUSE ATTRIBUTION, ONE CLAUSE AT A TIME (the follow-up a78dd8a1
    /// deferred: "per-cause Full-arm attribution needs the engine's validity
    /// check to report its failing clause").
    ///
    /// WHY IT IS SHAPED LIKE THIS. The corpus oracle above already drives most
    /// of these causes, but it drives them through REALISTIC events, and a
    /// realistic event usually breaks several clauses at once (a scroll moves
    /// `base_y` AND the row revision AND marks damage). That makes it a fine
    /// test of the arm and a poor test of the LABEL: an attribution that
    /// returned one stuck constant would pass most of it. So this test starts
    /// from a settled scratch that provably takes the SCOPED arm, applies
    /// EXACTLY ONE mutation, and requires that clause's own cause back. Every
    /// case is therefore a controlled experiment with a verified control, which
    /// is the only way a per-clause claim can mean anything.
    ///
    /// It is also the completeness gate: the table must name every variant of
    /// `FullRefillCause`, so a clause added to the predicate without a cause —
    /// or a cause added without a clause that can produce it — fails here
    /// instead of quietly reporting as somebody else's.
    #[test]
    fn dmg1_every_continuity_clause_reports_its_own_cause() {
        use crate::render::{FrameRefill, FullRefillCause, RenderInput, RowOrder};

        const ROWS: usize = 8;
        const COLS: usize = 32;

        /// A terminal with scrollback and a scratch whose continuity chain is
        /// LIVE: the very next damage-scoped refill takes the scoped arm. Every
        /// case below starts from a fresh one of these, so the single mutation
        /// it applies is the only difference from a frame that would have been
        /// scoped.
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

        // THE CONTROL. Without this the whole table is worthless: if the
        // fixture had drifted off the scoped arm, every case below would report
        // its cause for a reason that has nothing to do with its mutation.
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
                // The requested size still matches the SCRATCH (so the clause
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

        // COMPLETENESS. A clause added to the predicate with a new cause and no
        // case here would otherwise ship as an unproven label.
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
}
