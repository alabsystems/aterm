// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MESSAGE BAND — the painter, the pixel hit test and the per-window
//! hover state for the unified message system's one reserved-row surface
//! (docs/DESIGN-unified-messages-2026-09-21.md §2, §3.2). The band is a
//! stack of 0–3 full-width single-cell-height rows exactly where the status
//! bars paint: under the native titlebar on macOS (strip = 0 rows), under the
//! in-grid strip elsewhere, below the per-window presence row when one is
//! up. Rows RESERVE grid rows (D1): nothing floats, nothing overwrites
//! terminal cells, nothing covers the strip.
//!
//! # Division of labour
//!
//! The engine lays the row out in CELLS ([`Presentation`], the width law of
//! `aterm_messages::glass`), answers where a press landed
//! ([`Presentation::hit`]) and computes every moving thing as FRACTIONS of
//! the row ([`BandMotion`], design ruling 140 — the owner's architecture ask:
//! *"design the logic in aterm core and then keep the osx layer lightweight"*);
//! this module turns a layout and one motion frame into [`RenderCell`]s on
//! the chrome band's material ([`chrome_band::band_colors`] — the chrome
//! palette, which also retires the OSC-11 tint drift the config band had),
//! maps the engine's fractions onto its window's PIXELS ([`MeterSpan`]), and
//! maps a window pixel to a band row and column ([`App::band_hit_at`]).
//! Layout and hit test share one law by construction: the painter reads the
//! same columns the hit test reads, so a capsule is hit exactly where it is
//! painted.
//!
//! # The width measure
//!
//! The band writes one `char` per cell (`settings::write_str`, the presence
//! and strip painters' writer), so the measure handed to the width law is
//! the char count ([`cell_width`]): the layout and the writer must agree, or
//! a right-aligned capsule would be hit one cell off where it is drawn. Every
//! reporter's words are ASCII plus a few BMP glyphs; a wide-aware writer and
//! measure move together when one arrives.
//!
//! # Row grammar (§2.2)
//!
//! ```text
//! col 0 1 2 3 …                                                       … cols-1
//!     ␠ G ␠ TITLE(bold) ␠·␠ excerpt (label) … 42% ~35 s left · disk busy  stats  ␠␠ ▐cap▌␠▐Details ›▌ ␠
//!     ███████████████████████████▓░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  ← a metered row's surface
//! ```
//!
//! Glyph at col 1 in `accent` (Success/Info) or `warn` (Warn/Error) — a busy
//! row's braille [`aterm_messages::SPINNER`] frame while it moves, an echo's
//! ✓ / ⚠ — text presentation on purpose (`⚠` and `✓` have emoji forms, and
//! a colour emoji in a chrome row would be two cells wide in one); the bold
//! title in `value` or `warn`; ` · ` then the excerpt in `label`; then the
//! row's SLOTS, words on the row (ruling 136): ` NN%` or the elapsed clock,
//! the ETA words (`~35 s left`, `stalled` in warn, `done`), ` · ` and the
//! load words (`disk busy`), and the stats in `label`.
//!
//! # The meter is the row (ruling 55, amended by rulings 136–141)
//!
//! A metered or busy row's whole SURFACE is its meter, window edge to window
//! edge: the engine hands a [`aterm_messages::Surface`] — tones along the
//! row as fractions of it — and each cell takes, as its BACKGROUND, the mean
//! tone over its own window-pixel span ([`MeterSpan`], through the window's
//! [`BandGeometry`]). The first and last columns' spans run out to the
//! window's edges, so their tones ARE the side gutters' — the host continues
//! them through the gutters ([`paint_rows_on`]'s edge tones →
//! `aterm_render::ChromeBleed`) and the presenters through the window's
//! leftover remainder bands — so 0 % is an empty track, 100 % is the
//! window's full width, and 50 % is its middle. The owner, of the 20-cell
//! block that used to sit after the excerpt: *"the progress bar doesn't go all
//! the way across the screen (I like that it uses the cursor trail theme)"*;
//! *"make sure that 0% and 100% and similar concepts are mapped to the
//! relative size of the screen with such top progress bars"*.
//!
//! WHOLE CELLS, NEVER METER INK (ruling 138): the fill, the comet, the
//! glint, the glide and the echoes are all per-cell BACKGROUND tones, so the
//! per-cell contrast floor can never repaint them (ruling 55's reason); the
//! motion is smooth because the TONES move — the cell under the fill's edge,
//! the comet's soft gradient and the glint take `mix(track, fill, coverage)`
//! of their pixel span — not because a glyph splits a cell.
//!
//! The fill's ink is the theme's cursor accent (ruling 137, the owner's
//! "cursor trail theme"), `warn` on a Warn/Error row, and `HIGHLIGHT` under
//! High Contrast; a stalled bar dims to a slate of it, the glint is a lift
//! of it, and a Fault echo warms it to the fault hue ([`tone_rgb`]). A BUSY
//! row's comet wears the same accent held on its words' side of the
//! luminance scale ([`MeterInks::comet`]: on a dark band a deep, saturated
//! accent), so a word it passes brightens and dims with it and never flips
//! to the far ink.
//!
//! Every word on a metered row keeps its role's ink FLOORED against the
//! surface it lands on (AA, 4.5:1 — the band's own inks were floored against
//! `bar_bg` only, and the accent is the theme's cursor, which a foreground
//! may resemble; under High Contrast a word on the fill takes
//! `HIGHLIGHTTEXT`). A chip whose resting fill would vanish into the meter
//! wears the band's own surface instead: a Secondary (`meter_track` over the
//! track) always, a Primary (`accent` over the fill) whenever any of its
//! cells lies over the fill, with its ink turned to the accent. `Details ›`
//! has no fill and rides the meter like a word.
//!
//! # Motion (design §10.7, ruling 140)
//!
//! [`paint_rows_on`] paints one FRAME: the layout (time-free, fingerprinted
//! by the engine) under one [`BandMotion`] (`MessageCenter::motion`, read on
//! the engine's 33 ms grid). A held row's still bar and a busy row's still
//! track come from the same call in the still look, so the painter has one
//! path. An animation never re-runs the width law and never re-grids: the
//! motion moves only the surface, the time slots, the glyph cell and an
//! echo's fade — which mixes every cell toward `bar_bg` and never touches
//! the seam, which the splice draws on the COMPOSED stack afterwards
//! ([`seal_stack`]).
//!
//! # Capsules
//!
//! Capsules are chips ` label `: **Primary** (a CONSEQUENTIAL intent —
//! `Intent::is_consequential`: Install now, Install, a system pane, New
//! window) `accent` fill (deepened to AA under its ink where the theme's
//! accent falls short, [`chip_inks`]), `bar_bg` ink, bold; **Secondary** (a navigation
//! or a decline: Packages, Software Update, Open log, Open aterm.toml, Not
//! now) `meter_track` fill, `value` ink; **Details ›** no fill, `label`
//! ink. The role follows the intent, not its position (review 2026-09-22:
//! every toolchain row wore a loud `Packages` and the launch pass stacked
//! two). The chip under the pointer — or the row's `Details ›` while the
//! pointer is on the row BODY, since that is what a press there will do —
//! lights, whatever its role: a Primary takes `capsule_primary_hover` (the
//! accent, lit — the grey `capsule_hover` on it read as disabled, review
//! 2026-09-22), every other chip `capsule_hover`.
//! Under Windows High Contrast every ink is WINDOWTEXT and capsules are
//! drawn as `[label]` with no fill: HC separates surfaces with borders. The
//! closing underline seam belongs to the COMPOSED stack, not to any painted
//! row: the splice seals whichever chrome row ends up last — a message row, a
//! padded row, or the presence row when no band row follows ([`seal_stack`]).
//!
//! # The links (design §2.2, §8)
//!
//! Every row ends in its `Details ›` (`+N ›` on a single committed row with
//! rows queued behind it) and the overflow row is one `Messages ›` link:
//! each opens Settings ▸ Messages in the pressed window — at the row's own
//! entry, or at the top of the log ([`App::band_presentation`] passes
//! `Links::Painted`; Phase 1 withheld them while the page did not exist,
//! since a link with no destination is dishonest). A press on the row BODY
//! is the same `Details ›` press ([`App::press_message_body`], the one law
//! of §2.2), which is why a pointer on the body lights that chip.

use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_messages::text::char_width;
use aterm_messages::{
    ActionIndex, Anim, BandMotion, CapsuleLayout, CapsuleRole, DONE_WORD, ELAPSED_W, EchoKind,
    FAILED_WORD, FineTone, Hit, Links, MARGIN, Presentation, RowKind, RowLayout, RowMotion,
    STALLED_WORD, Severity,
};
use aterm_render::Theme;

use crate::chrome_band::{self, BandColors};
use crate::settings::{blank_row, write_str};
use crate::{App, WindowId};

/// The band's cell measure — the char count, because the band's writer puts
/// one `char` in one cell (see the module doc).
pub(crate) fn cell_width(s: &str) -> usize {
    char_width(s)
}

/// Where a window's band cells sit on its glass, in device px — what maps a
/// meter's fill onto the WINDOW's width rather than the grid's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BandGeometry {
    /// The window's full width (the swapchain / softbuffer surface), px.
    pub win_w: usize,
    /// Window x of column 0's left edge: the frame's left gutter plus the
    /// leading remainder band (`pad_lo` of `aterm_render::pad_split`).
    pub cells_x: usize,
    /// One cell's width, px.
    pub cell_w: usize,
}

impl BandGeometry {
    /// Unit cells and no gutters: the fill maps onto the columns alone. The
    /// geometry of a caller that has no window (the painter's unit tests).
    #[cfg(test)]
    pub(crate) fn cells_only(cols: usize) -> Self {
        Self {
            win_w: cols,
            cells_x: 0,
            cell_w: 1,
        }
    }
}

/// THE MAPPING (ruling 55, amended by ruling 138): which window pixels a
/// `cols`-wide metered row's column `x` stands for. Column `x` covers its
/// own cell, `[cells_x + x·cell_w, cells_x + (x+1)·cell_w)` — and the FIRST
/// column also the left gutter and leading remainder band before it, the
/// LAST the ones after it, out to the window's edges. So the edge cells'
/// tones ARE the gutters' ([`aterm_render::ChromeBleed::row_edges`]'s
/// no-epoch contract: a gutter's tone changes only together with its edge
/// cell), and the whole window, gutters included, is the meter: 0 % is an
/// empty track, 100 % lights the window edge to edge, and 50 % is its middle
/// — the one cell under the fill's edge takes `mix(track, fill, coverage)`
/// of its span ([`aterm_messages::Surface::span`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MeterSpan {
    /// The window the row is mapped onto.
    pub geom: BandGeometry,
    /// The row's width in columns.
    pub cols: usize,
}

impl MeterSpan {
    /// Column `x`'s window-pixel span `[x0, x1)` (see the type doc).
    #[must_use]
    pub(crate) fn px(self, x: usize) -> (u64, u64) {
        let g = self.geom;
        let cw = g.cell_w.max(1);
        let x0 = if x == 0 { 0 } else { g.cells_x + x * cw };
        let x1 = if x + 1 >= self.cols {
            g.win_w.max(g.cells_x + self.cols * cw)
        } else {
            g.cells_x + (x + 1) * cw
        };
        (x0 as u64, x1 as u64)
    }

    /// The window's width the fractions are mapped onto.
    fn win_w(self) -> u64 {
        let g = self.geom;
        g.win_w.max(g.cells_x + self.cols * g.cell_w.max(1)) as u64
    }

    /// Column `x`'s tone on `surface`, in bytes (the tests' reading; the
    /// painter mixes from [`MeterSpan::fine`]).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn tone(self, surface: &aterm_messages::Surface, x: usize) -> aterm_messages::Tone {
        let (x0, x1) = self.px(x);
        surface.span(x0, x1, self.win_w())
    }

    /// Column `x`'s tone on `surface` to 1/256 of a step — what the painter
    /// mixes from, rounding each cell's colour once (design ruling 157).
    #[must_use]
    pub(crate) fn fine(self, surface: &aterm_messages::Surface, x: usize) -> FineTone {
        let (x0, x1) = self.px(x);
        surface.span_fine(x0, x1, self.win_w())
    }
}

/// What the pointer is on within one band row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HoverTarget {
    /// The row body: a press opens Details, so the `Details ›` chip lights.
    Body,
    /// One capsule, by its action.
    Capsule(ActionIndex),
}

/// The per-window hover state the paint key carries: which row, and what on
/// it. `None` when the pointer is off the band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BandHover {
    /// The band row (0 = topmost), narrow on purpose — the band has three.
    pub row: u8,
    /// Body or capsule.
    pub target: HoverTarget,
}

/// What one window's painted band rows were built from — `(center fingerprint
/// at cols, cols, palette key, hover, window geometry, motion fingerprint)` —
/// the splice's cache key (`App::splice_message_band`), which reads the same
/// terms the RepaintKey's `band_fp` folds. The motion term is
/// [`BandMotion::fingerprint`]: 0 with nothing moving or indicating, and
/// moved only by a frame that draws something new (ruling 140 — the host's
/// busy frame counter retired with it).
pub(crate) type BandKey = (u64, usize, u64, Option<BandHover>, BandGeometry, u64);

/// Where a window pixel landed among the chrome rows between the strip and
/// the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BandTarget {
    /// The per-window presence row: swallowed, no action (design §2).
    Presence,
    /// A band row and the cell column under the pointer — the pair
    /// [`Presentation::hit`] resolves.
    Row {
        /// 0 = the topmost band row.
        row: usize,
        /// The cell column, 0 = the window's first cell.
        col: usize,
    },
}

/// The cells of a `cols`-wide presence row that carry TEXT: the row keeps one
/// margin cell each side, and `chrome band=` / the a11y detail fit the words
/// at this width — the same line the human sees, never a slot past the margin.
pub(crate) fn presence_text_cols(cols: usize) -> usize {
    cols.saturating_sub(2 * MARGIN)
}

/// One EMPTY band row, in the same colours a painted row uses. The compose
/// pads with this when the geometry has committed more rows than the cache
/// holds, so a reserved row is never a hole the terminal's own top row shows
/// through.
pub(crate) fn blank_band_row(cols: usize, theme: Theme) -> Vec<RenderCell> {
    let c = chrome_band::band_colors(theme);
    blank_row(cols, c.label, c.bar_bg, false)
}

/// Paint the PRESENCE band's one row (`crate::presence::Words`) at `cols`: the
/// six slots laid out by [`crate::presence::Words::pieces`] (which sheds from the
/// right in the design's order), on the chrome band's material, each slot in
/// its own ink — the role in `value`, a `since`/fabric figure in `label`, the
/// hand in the DRIVE hue while a peer types (the rim's own teal family, so
/// the two surfaces read as one fact — at the TEXT floor,
/// [`chrome_band::presence_inks`], since these are words), `⊘ hold` in STOP,
/// the phase in WARN while the row's tone is `Warn` and in the drive hue for
/// the two seconds after a settled turn. The trust glyph and `⚠` are
/// text-presentation on purpose (a colour emoji would be two cells wide in
/// one). The row carries no closing seam of its own: the splice seals it
/// ([`seal_stack`]) when no band row follows.
pub(crate) fn paint_presence_row(
    words: &crate::presence::Words,
    cols: usize,
    theme: Theme,
) -> Vec<RenderCell> {
    use crate::presence::SlotKind;
    use crate::presence::Tone;
    let c = chrome_band::band_colors(theme);
    let p = chrome_band::presence_inks(theme);
    let mut row = blank_row(cols, c.label, c.bar_bg, false);
    let pieces = words.pieces(presence_text_cols(cols));
    let mut col = MARGIN;
    for (i, (kind, text)) in pieces.iter().enumerate() {
        if i > 0 {
            col += if *kind == SlotKind::Since { 1 } else { 2 };
        }
        let (ink, bold) = match kind {
            SlotKind::Role => (c.value, true),
            SlotKind::Phase => match words.tone {
                Tone::Warn => (c.warn, false),
                Tone::Success => (p.drive, false),
                Tone::Info => (c.value, false),
            },
            SlotKind::Since => (c.label, false),
            SlotKind::Hand => {
                if text.starts_with('\u{2298}') {
                    (p.stop, true)
                } else if text.starts_with('\u{25c2}') || text.starts_with('\u{25b8}') {
                    (p.drive, false)
                } else {
                    (c.label, false)
                }
            }
            SlotKind::Mail => (c.value, false),
            SlotKind::Ctx => {
                if text.ends_with('\u{26a0}') {
                    (c.warn, false)
                } else {
                    (c.value, false)
                }
            }
            SlotKind::Fabric => {
                if text.starts_with('\u{2715}') {
                    (p.stop, false)
                } else if text.starts_with('~') {
                    (c.warn, false)
                } else {
                    (c.label, false)
                }
            }
        };
        write_str(&mut row, cols, col, text, ink, c.bar_bg, bold);
        // The glyphs that have an emoji form are pinned to one cell — the
        // fleet lock included: U+1F512 is emoji-presentation by default, and
        // the renderer would take its colour face two cells wide otherwise.
        for (k, ch) in text.chars().enumerate() {
            if matches!(
                ch,
                '\u{2713}' | '\u{2717}' | '\u{26a0}' | '\u{2709}' | '\u{2715}' | '\u{1f512}'
            ) && let Some(cell) = row.get_mut(col + k)
            {
                cell.text_presentation = true;
            }
        }
        col += text.chars().count();
    }
    row
}

/// The inks a row's surface resolves its [`Tone`]s against (design §10.7,
/// ruling 137): the track, the row's FILL (the cursor accent, `warn` on a
/// Warn/Error row, `HIGHLIGHT` under High Contrast), the glint — a LIFT of
/// that fill toward [`BandColors::meter_lift`], held to the 3:1 non-text
/// floor on the track — and the fault hue a Fault echo warms toward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MeterInks {
    pub track: [u8; 3],
    pub fill: [u8; 3],
    pub glint: [u8; 3],
    pub warn: [u8; 3],
}

impl MeterInks {
    /// The inks of a row whose fill wears `fill`.
    ///
    /// The glint (and the Complete echo's bloom, which rises toward it) is
    /// held on the side of the words riding the fill ([`fill_anchor`],
    /// design ruling 158): on a light theme whose accent sits near the
    /// black/white crossover (Catppuccin Latte) the lift toward `fg` took
    /// the glint past it, and every letter it passed flipped from black to
    /// white and back — the whole row at the bloom's peak. Where the 3:1
    /// floor on the track and the words' side cannot both hold, the side
    /// wins, as the comet's does ([`MeterInks::comet`]).
    pub(crate) fn of(c: &BandColors, fill: [u8; 3]) -> Self {
        let glint = chrome_band::ensure_contrast(
            chrome_band::mix3(fill, c.meter_lift, 0.55),
            c.meter_track,
            3.0,
        );
        Self {
            track: c.meter_track,
            fill,
            glint: hold_side(glint, fill, fill_anchor(fill), COMET_SIDE_AA),
            warn: c.warn,
        }
    }
}

/// The end of the scale the words on a determinate fill `fill` ride toward:
/// whichever of black and white reads better on it — one of them always
/// clears √21 ≈ 4.58:1 (design ruling 158).
pub(crate) fn fill_anchor(fill: [u8; 3]) -> [u8; 3] {
    if chrome_band::contrast(BLACK, fill) >= chrome_band::contrast(WHITE, fill) {
        BLACK
    } else {
        WHITE
    }
}

/// A determinate row's word ink on `under`: floored to [`WORD_AA`] by the
/// least move toward `near` ([`floor_toward`], continuous in the ground), or
/// toward the other end where `near` itself cannot clear it (design ruling
/// 158).
pub(crate) fn floor_word(ink: [u8; 3], under: [u8; 3], near: [u8; 3]) -> [u8; 3] {
    let anchor = if chrome_band::contrast(near, under) >= WORD_AA {
        near
    } else {
        opposite(near)
    };
    floor_toward(ink, under, anchor, WORD_AA)
}

/// The other end of the scale from `anchor`.
fn opposite(anchor: [u8; 3]) -> [u8; 3] {
    if anchor == BLACK { WHITE } else { BLACK }
}

/// How far a BUSY row's comet may move off its track: every tone it draws
/// leaves the row's words' anchor ([`words_anchor`]) at least this far from
/// it — AA with a hair of margin for the byte rounding of the mixes between.
pub(crate) const COMET_SIDE_AA: f64 = 4.6;

impl MeterInks {
    /// THE COMET KEEPS ITS WORDS' SIDE (visual review of the merged band,
    /// 2026-09-24): the inks of a BUSY row's surface — the comet, its still
    /// track, and its echoes — and the anchor its words floor toward.
    ///
    /// The comet sweeps under every word on the row. In the full accent its
    /// head crossed the luminance at which no grey but black or white still
    /// reads, so each letter it passed flipped from light ink to black and
    /// back — scattered black letters, and a dark spinner cutting the green
    /// into two blobs at the window's edge. So each of the comet's inks — the
    /// fill, the head (no lift: a lift toward white is exactly the crossing),
    /// and a Fault echo's warn — is moved in LINEAR light toward the far end
    /// from the anchor, its hue kept, by the least amount that keeps the
    /// anchor [`COMET_SIDE_AA`] from it: on a dark band a deep, saturated
    /// accent (the owner's "cursor trail theme") that the words ride in a
    /// slowly brightening light ink ([`floor_toward`]). A determinate bar is
    /// not a comet: it keeps the full accent, and its words ride it dark.
    pub(crate) fn comet(c: &BandColors, fill: [u8; 3]) -> (Self, [u8; 3]) {
        let anchor = words_anchor(c);
        let fill = keep_side(fill, anchor, COMET_SIDE_AA);
        (
            Self {
                track: c.meter_track,
                fill,
                glint: fill,
                warn: keep_side(c.warn, anchor, COMET_SIDE_AA),
            },
            anchor,
        )
    }
}

const WHITE: [u8; 3] = [255, 255, 255];
const BLACK: [u8; 3] = [0, 0, 0];

/// The end of the scale a row's words sit toward: white when the band's
/// inks are lighter than its track (a dark band), black otherwise.
fn words_anchor(c: &BandColors) -> [u8; 3] {
    if luminance(c.label) >= luminance(c.meter_track) {
        WHITE
    } else {
        BLACK
    }
}

/// An sRGB byte in linear light.
fn lin(v: u8) -> f64 {
    let c = f64::from(v) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light back to an sRGB byte.
fn enc(l: f64) -> u8 {
    let l = l.clamp(0.0, 1.0);
    let c = if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055f64.mul_add(l.powf(1.0 / 2.4), -0.055)
    };
    (c * 255.0).round().clamp(0.0, 255.0) as u8
}

/// WCAG relative luminance.
fn luminance(c: [u8; 3]) -> f64 {
    0.2126f64.mul_add(lin(c[0]), 0.7152f64.mul_add(lin(c[1]), 0.0722 * lin(c[2])))
}

/// `ink` moved in LINEAR light toward the far end from `anchor` — its
/// chromaticity kept, only its lightness moved — by the least amount that
/// leaves `anchor` at least `target`:1 on it; unchanged when it already is.
/// `anchor` is the words' pure end on a busy row, or a chip's own ink
/// ([`chip_inks`]): the far end is black from an anchor lighter than `ink`,
/// white otherwise.
fn keep_side(ink: [u8; 3], anchor: [u8; 3], target: f64) -> [u8; 3] {
    if chrome_band::contrast(anchor, ink) >= target {
        return ink;
    }
    let away = if luminance(anchor) > luminance(ink) {
        BLACK
    } else {
        WHITE
    };
    let at = |s: f64| -> [u8; 3] {
        [0, 1, 2].map(|k| enc(lin(ink[k]).mul_add(1.0 - s, lin(away[k]) * s)))
    };
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if chrome_band::contrast(anchor, at(mid)) >= target {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    at(hi)
}

/// `ink` floored to `target`:1 on `bg` by the LEAST move toward `anchor` —
/// a continuous function of the ground, so a word under a moving tone
/// brightens and dims with it and never changes side (the comet's words,
/// [`MeterInks::comet`]). `anchor` itself where even it falls short, which
/// the comet's guard ([`hold_side`]) never lets happen.
fn floor_toward(ink: [u8; 3], bg: [u8; 3], anchor: [u8; 3], target: f64) -> [u8; 3] {
    if chrome_band::contrast(ink, bg) >= target {
        return ink;
    }
    if chrome_band::contrast(anchor, bg) < target {
        return anchor;
    }
    let (mut lo, mut hi) = (0u8, 255u8);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if chrome_band::contrast(mix_u8(ink, anchor, mid), bg) >= target {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    mix_u8(ink, anchor, hi)
}

/// A ground held on its words' side: `bg` pulled back toward `track` (a
/// comet cell's track, or a determinate row's resting fill for its glint) by
/// the least amount that leaves `anchor` `target`:1 from it.
/// The comet's inks already keep that side ([`MeterInks::comet`]); an sRGB
/// mix BETWEEN two of them can still dip past it on a light band (luminance
/// is not linear in the bytes), and this is where that is caught.
fn hold_side(bg: [u8; 3], track: [u8; 3], anchor: [u8; 3], target: f64) -> [u8; 3] {
    if chrome_band::contrast(anchor, bg) >= target {
        return bg;
    }
    let (mut lo, mut hi) = (0u8, 255u8);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if chrome_band::contrast(anchor, mix_u8(bg, track, mid)) >= target {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    mix_u8(bg, track, hi)
}

/// `a` toward `b` by `t`/255 — integer, so every target mixes the same byte.
fn mix_u8(a: [u8; 3], b: [u8; 3], t: u8) -> [u8; 3] {
    let t = u32::from(t);
    let mix = |x: u8, y: u8| {
        u8::try_from((u32::from(x) * (255 - t) + u32::from(y) * t + 127) / 255).unwrap_or(255)
    };
    [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])]
}

/// A tone's colour: the track mixed toward the fill by `fill`, then toward
/// the glint by `lift` — both in exact arithmetic with ONE rounding at the
/// end (design ruling 157: two byte roundings in a row put a ±1 zigzag in
/// the step between neighbouring cells, which read as faint vertical
/// stripes along the comet's tail) — then toward warn by `warn`
/// ([`toward_fault`]). Both ends are exact: `warn` 0 is the fill and glint
/// mix, `warn` 255 exactly warn.
#[cfg(test)]
pub(crate) fn tone_rgb(t: aterm_messages::Tone, k: &MeterInks) -> [u8; 3] {
    fine_rgb(t.into(), k)
}

/// [`tone_rgb`] of a [`FineTone`]: the painter's per-cell colour, from the
/// span's mean unrounded.
pub(crate) fn fine_rgb(t: FineTone, k: &MeterInks) -> [u8; 3] {
    const FULL: f64 = 255.0 * 256.0;
    let (f, l) = (f64::from(t.fill) / FULL, f64::from(t.lift) / FULL);
    let w = f64::from(t.warn) / FULL;
    // A cell the fill only PARTLY covers, warming while its track does not
    // (a determinate Fault echo's edge): the warmth is the fill's share, so
    // the cell is the warmed fill over the track by its coverage. Warming
    // the track-and-fill mix instead sent it round the hue circle on its own
    // path — a red cell at the end of an amber bar on a blue accent (design
    // ruling 158).
    if w > 0.0 && f > 0.0 && f < 1.0 && l == 0.0 && t.warn <= t.fill {
        let warm = toward_fault(k.fill, k.warn, (w / f).min(1.0));
        return [0, 1, 2].map(|i| {
            let under = f64::from(k.track[i]);
            (f64::from(warm[i]) - under)
                .mul_add(f, under)
                .round()
                .clamp(0.0, 255.0) as u8
        });
    }
    let c = [0, 1, 2].map(|i| {
        let under = f64::from(k.track[i]);
        let c = (f64::from(k.fill[i]) - under).mul_add(f, under);
        let c = (f64::from(k.glint[i]) - c).mul_add(l, c);
        c.round().clamp(0.0, 255.0) as u8
    });
    toward_fault(c, k.warn, w.min(1.0))
}

/// The chroma below which a colour has no hue to keep (OkLab): a track, a
/// grey accent.
const HUELESS: f64 = 0.03;

/// The hues a Fault echo never crosses into, in OkLCh degrees: green, the
/// success hue, and the mint beside it (ruling 133).
const GREEN_HUES: (f64, f64) = (120.0, 175.0);

/// `from` toward the fault hue `to` by `t` (0–1) — the Fault echo's warming
/// (design ruling 156). In OkLCh, lightness and chroma move in a straight
/// line and the hue turns ONE way round the circle, so no frame is greyer
/// and lighter than both ends and none drains to the pale grey the old
/// two-leg mix passed through on its first frame. The hue takes the short
/// way unless that way crosses green (a success hue inside a failure,
/// ruling 133) from a colour that is not green itself — a blue fill then
/// warms the long way, through the reds. A hueless end takes the other's
/// hue. Out-of-gamut steps give up chroma, never lightness or hue.
fn toward_fault(from: [u8; 3], to: [u8; 3], t: f64) -> [u8; 3] {
    if t <= 0.0 {
        return from;
    }
    if t >= 1.0 {
        return to;
    }
    let (l0, c0, h0) = oklch(from);
    let (l1, c1, h1) = oklch(to);
    let h0 = if c0 < HUELESS { h1 } else { h0 };
    let h1 = if c1 < HUELESS { h0 } else { h1 };
    let turn = fault_turn(h0, h1);
    from_oklch(
        (l1 - l0).mul_add(t, l0),
        (c1 - c0).mul_add(t, c0),
        turn.mul_add(t, h0),
    )
}

/// The signed turn, in degrees, a Fault echo's hue makes from `h0` to `h1`:
/// the short way, unless it crosses [`GREEN_HUES`] from outside them.
fn fault_turn(h0: f64, h1: f64) -> f64 {
    let short = (h1 - h0 + 540.0).rem_euclid(360.0) - 180.0;
    let inside = |h: f64| h > GREEN_HUES.0 && h < GREEN_HUES.1;
    // Whether the open arc from `h0` turning `short` passes hue `x`.
    let passes = |x: f64| {
        let d = if short >= 0.0 {
            (x - h0).rem_euclid(360.0)
        } else {
            (h0 - x).rem_euclid(360.0)
        };
        d > 0.0 && d < short.abs()
    };
    if !inside(h0) && (passes(GREEN_HUES.0) || passes(GREEN_HUES.1)) {
        short - 360.0f64.copysign(short)
    } else {
        short
    }
}

/// sRGB bytes → OkLCh `(L, C, h°)`.
#[allow(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    reason = "Björn Ottosson's published OkLab matrices, digit for digit"
)]
pub(crate) fn oklch(c: [u8; 3]) -> (f64, f64, f64) {
    let [r, g, b] = c.map(lin);
    let l = 0.0514459929f64.mul_add(b, 0.4122214708f64.mul_add(r, 0.5363325363 * g));
    let m = 0.1073969566f64.mul_add(b, 0.2119034982f64.mul_add(r, 0.6806995451 * g));
    let s = 0.6299787005f64.mul_add(b, 0.0883024619f64.mul_add(r, 0.2817188376 * g));
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    let ll = (-0.0040720468f64).mul_add(s, 0.2104542553f64.mul_add(l, 0.7936177850 * m));
    let a = 0.4505937099f64.mul_add(s, 1.9779984951f64.mul_add(l, -2.4285922050 * m));
    let bb = (-0.8086757660f64).mul_add(s, 0.0259040371f64.mul_add(l, 0.7827717662 * m));
    (ll, a.hypot(bb), bb.atan2(a).to_degrees().rem_euclid(360.0))
}

/// OkLCh → sRGB bytes, giving up chroma (never lightness or hue) until the
/// colour is inside the sRGB gamut.
#[allow(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    reason = "Björn Ottosson's published OkLab matrices, digit for digit"
)]
fn from_oklch(l: f64, c: f64, h: f64) -> [u8; 3] {
    let linear = |c: f64| -> [f64; 3] {
        let (a, b) = (c * h.to_radians().cos(), c * h.to_radians().sin());
        let lp = 0.2158037573f64.mul_add(b, 0.3963377774f64.mul_add(a, l));
        let mp = (-0.0638541728f64).mul_add(b, (-0.1055613458f64).mul_add(a, l));
        let sp = (-1.2914855480f64).mul_add(b, (-0.0894841775f64).mul_add(a, l));
        let (lp, mp, sp) = (lp.powi(3), mp.powi(3), sp.powi(3));
        [
            0.2309699292f64.mul_add(sp, 4.0767416621f64.mul_add(lp, -3.3077115913 * mp)),
            (-0.3413193965f64).mul_add(sp, (-1.2684380046f64).mul_add(lp, 2.6097574011 * mp)),
            1.7076147010f64.mul_add(sp, (-0.0041960863f64).mul_add(lp, -0.7034186147 * mp)),
        ]
    };
    let fits = |v: [f64; 3]| v.iter().all(|x| (-1e-6..=1.0 + 1e-6).contains(x));
    let mut rgb = linear(c);
    if !fits(rgb) {
        let (mut lo, mut hi) = (0.0f64, c);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.0;
            if fits(linear(mid)) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        rgb = linear(lo);
    }
    rgb.map(enc)
}

/// THE COMPLETE ECHO SAYS THE FINISHED FORM (design ruling 154), for one
/// reporter's live-work row: `msg` finishes as `finished`, and at 60, 80, 120
/// and 160 columns its Complete echo paints those words (whole where they fit
/// the cells the title may take, else elided in place) after the ✓, with
/// every other piece of the row — pct, time, load, capsules — in the column
/// the row had it. A fill is taken to 100 % first, the reading the echo
/// completes from; the echo drops the frozen stats.
#[cfg(test)]
pub(crate) fn assert_completes_in_place(msg: &aterm_messages::Message, finished: &str) {
    use aterm_messages::{Instant, Look, MessageCenter, MessageLog, Outcome, TITLE_COL, WallStamp};
    assert_eq!(msg.finished_title(), finished, "{}", msg.title);
    let mut msg = msg.clone();
    msg.reveal_after = None;
    if let Some(m) = msg.meter.as_mut().filter(|m| m.fill_permille.is_some()) {
        m.fill_permille = Some(1000);
    }
    let but_title = |r: &RowLayout| {
        let mut r = r.clone();
        r.kind = RowKind::Overflow { hidden: 0 };
        r.title.1.clear();
        r.full_title.clear();
        r.stats = None;
        r
    };
    for cols in [60usize, 80, 120, 160] {
        let now = Instant::now();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        let id = center.post(msg.clone(), WallStamp { unix_ms: 1 }, now).id;
        center.commit_rows(now, 3);
        let present = |c: &MessageCenter| c.presentation(cols, &cell_width, None, Links::Painted);
        let live = present(&center).rows[0].clone();
        let at = now + aterm_messages::Duration::from_secs(20);
        assert!(center.resolve(id, Outcome::Ok, at), "{}", msg.title);
        let p = present(&center);
        let echo = &p.rows[0];
        assert!(
            matches!(echo.kind, RowKind::Echo(_)),
            "{}@{cols}",
            msg.title
        );
        assert_eq!(
            but_title(echo),
            but_title(&live),
            "{}@{cols}: nothing but the title moved",
            msg.title
        );
        let laid = cell_width(&live.title.1);
        assert!(cell_width(&echo.title.1) >= laid, "{}@{cols}", msg.title);
        let words = echo.title.1.trim_end();
        if cell_width(finished) <= laid {
            assert_eq!(words, finished, "{}@{cols}: whole", msg.title);
        } else {
            let stem = words.trim_end_matches('\u{2026}');
            assert!(
                finished.starts_with(stem),
                "{}@{cols}: {words:?}",
                msg.title
            );
        }
        // The painted row says it after the ✓.
        let motion = center.motion(
            &p,
            at + aterm_messages::Duration::from_millis(100),
            Look::STILL,
        );
        let rows = paint_rows(&p, Theme::default(), None, &motion);
        let painted: String = rows[0][TITLE_COL..].iter().map(|c| c.ch).collect();
        assert!(
            painted.starts_with(words),
            "{}@{cols}: {painted:?}",
            msg.title
        );
        assert_eq!(rows[0][aterm_messages::GLYPH_COL].ch, '\u{2713}');
        assert_eq!(
            center.log().get(id).unwrap().title,
            msg.title,
            "the record's words"
        );
    }
}

/// [`paint_rows_on`] with the fill mapped onto the columns alone
/// ([`BandGeometry::cells_only`]) — the painter's unit tests, which have no
/// window.
#[cfg(test)]
pub(crate) fn paint_rows(
    p: &Presentation,
    theme: Theme,
    hover: Option<BandHover>,
    motion: &BandMotion,
) -> Vec<Vec<RenderCell>> {
    paint_rows_on(p, theme, hover, BandGeometry::cells_only(p.cols), motion).0
}

/// One painted band: its rows, and for each row the `(left, right)` gutter
/// tones of its meter (`None` on an unmetered row, whose gutters keep the
/// band's own tone).
pub(crate) type PaintedBand = (Vec<Vec<RenderCell>>, Vec<Option<([u8; 3], [u8; 3])>>);

/// Paint every committed band row as one `p.cols`-wide row each, top to
/// bottom, on the chrome band's material, at ONE motion frame `motion`
/// (`MessageCenter::motion` over `p`: the moving look on a window that
/// animates, the still look everywhere else — a held bar and a busy row's
/// unlit track are drawn from it either way). Each metered or busy row's
/// surface is mapped onto the window through `geom` ([`MeterSpan`]). `hover`
/// lights the chip under the pointer (or the body's `Details ›`) on its row.
/// No row carries the hairline that closes the chrome against the terminal:
/// the splice pads and trims this cache to the committed count and puts a
/// presence row above it, so only the COMPOSED stack knows which row is last
/// — [`seal_stack`] draws it there.
pub(crate) fn paint_rows_on(
    p: &Presentation,
    theme: Theme,
    hover: Option<BandHover>,
    geom: BandGeometry,
    motion: &BandMotion,
) -> PaintedBand {
    let c = chrome_band::band_colors(theme);
    let hc = chrome_band::forced_chrome().is_some();
    let n = p.rows.len();
    let mut rows: Vec<Vec<RenderCell>> = Vec::with_capacity(n);
    let mut edges = Vec::with_capacity(n);
    let still = RowMotion::default();
    for (i, layout) in p.rows.iter().enumerate() {
        let mut row = blank_row(p.cols, c.label, c.bar_bg, false);
        let lit = hover.filter(|h| usize::from(h.row) == i).map(|h| h.target);
        let rm = motion.rows.get(i).unwrap_or(&still);
        // The gutters continue the cells ACTUALLY painted at the row's two
        // edges — a chip the width law put at column 0 (a degenerate narrow
        // row) wears its own fill, not the meter's, and an echo's fade mixes
        // both — so a tone changes only together with its edge cell: the
        // renderer's no-epoch contract (`aterm_render::ChromeBleed::row_edges`).
        // A busy row's comet is the same surface, so its gutters light as it
        // enters and leaves.
        let metered = paint_row(&mut row, p.cols, layout, rm, &c, hc, lit, geom);
        edges.push(
            metered
                .filter(|()| !row.is_empty())
                .map(|()| (row[0].bg, row[row.len() - 1].bg)),
        );
        rows.push(row);
    }
    (rows, edges)
}

/// The ground a row's words are written on: the band itself, or — on a
/// metered row — each column's meter tone.
struct Ground<'a> {
    band: [u8; 3],
    /// Each column's surface, and whether it lies over the FILL (at least
    /// half lit) — `None` on an unmetered row.
    meter: Option<&'a [([u8; 3], bool)]>,
    hc: bool,
    /// The High Contrast ink for a word on the fill (`HIGHLIGHTTEXT`).
    hc_fill_ink: [u8; 3],
    /// On a BUSY row, the anchor its words floor toward, continuously
    /// ([`MeterInks::comet`]); `None` on every other row.
    side: Option<[u8; 3]>,
    /// On a DETERMINATE row, the anchors its words floor toward over the
    /// fill ([`fill_anchor`]) and over the track ([`words_anchor`]).
    sides: Option<([u8; 3], [u8; 3])>,
}

impl Ground<'_> {
    /// The background under column `x`.
    fn at(&self, x: usize) -> [u8; 3] {
        self.meter
            .and_then(|m| m.get(x))
            .map_or(self.band, |&(bg, _)| bg)
    }

    /// Whether column `x` lies over the meter's fill.
    fn on_fill(&self, x: usize) -> bool {
        self.meter
            .and_then(|m| m.get(x))
            .is_some_and(|&(_, fill)| fill)
    }

    /// `ink` as a word at column `x` wears it: untouched on the band (the
    /// band's inks are already floored there); on the meter floored to AA
    /// against the cell's own surface, or under High Contrast the system's
    /// `HIGHLIGHTTEXT` on the fill and the forced ink floor on the track.
    ///
    /// On a BUSY row the floor moves toward the row's one anchor only
    /// ([`floor_toward`]), and the comet never leaves that side
    /// ([`MeterInks::comet`]), so a letter's ink is a continuous function of
    /// the tone passing under it and never changes side. On a DETERMINATE
    /// bar the floor is just as continuous, toward the fill's anchor over the
    /// fill and the track's over the track (design ruling 158) — the far one
    /// only where the near one cannot clear AA (the cell under the fill's
    /// edge, a Fault echo's warn on a theme whose warn wants the other ink).
    /// The old ten-step nudge put a staircase in the words wherever the
    /// ground moved smoothly: one frame of lighter ink at the Complete
    /// bloom's peak, `failed` jumping from warn to pale mid-echo. (A
    /// canonical-inks policy — two inks per role, pure black or white
    /// between — was tried on the merged band's visual review, 2026-09-24,
    /// and added ink jumps instead of removing them.)
    fn ink(&self, x: usize, ink: [u8; 3]) -> [u8; 3] {
        let under = self.at(x);
        if self.meter.is_none() || under == self.band {
            ink
        } else if self.hc && self.on_fill(x) {
            self.hc_fill_ink
        } else if self.hc {
            chrome_band::forced_ink(ink, under)
        } else if let Some(anchor) = self.side {
            floor_toward(ink, under, anchor, WORD_AA)
        } else if let Some((over_fill, over_track)) = self.sides {
            let near = if self.on_fill(x) {
                over_fill
            } else {
                over_track
            };
            floor_word(ink, under, near)
        } else {
            chrome_band::ensure_contrast_either(ink, under, WORD_AA)
        }
    }

    /// `write_str` over this ground: one glyph per cell, each on the
    /// surface under it in its floored ink.
    fn write(
        &self,
        row: &mut [RenderCell],
        cols: usize,
        col: usize,
        s: &str,
        ink: [u8; 3],
        bold: bool,
    ) {
        for (k, ch) in s.chars().enumerate() {
            let x = col + k;
            if x >= cols {
                break;
            }
            row[x] = chrome_band::cell(ch, self.ink(x, ink), self.at(x), bold, false);
        }
    }

    /// Words in a `width`-cell slot from `col`, left-aligned, the rest of the
    /// slot cleared to the ground — a time slot's words change length from
    /// frame to frame, and the cells they leave must not keep old glyphs.
    fn slot(
        &self,
        row: &mut [RenderCell],
        cols: usize,
        (col, width): (usize, usize),
        words: &str,
        ink: [u8; 3],
    ) {
        let mut chars = words.chars();
        let end = (col + width).min(cols).max(col);
        for (x, cell) in row.iter_mut().enumerate().take(end).skip(col) {
            let ch = chars.next().unwrap_or(' ');
            *cell = chrome_band::cell(ch, self.ink(x, ink), self.at(x), false, false);
        }
    }
}

/// The contrast every word on a metered row is held to against the surface
/// under it: WCAG AA, the floor the band's own inks meet on `bar_bg`.
const WORD_AA: f64 = 4.5;

/// CLOSE THE COMPOSED CHROME STACK against the terminal: the LAST row carries the
/// seam ([`chrome_band::seal_band_bottom`]), every row above it carries none.
///
/// THE SEAM BELONGS TO THE STACK, NOT TO THE PAINTER'S LAST ROW. [`paint_rows`]
/// used to stamp it there, and the splice then does two things the painter never
/// sees: it PADS the cache with [`blank_band_row`] when the geometry has committed
/// more rows than are on the glass (every dismissal or retirement, for the
/// `SHRINK_QUIET` 1.5 s before the count follows — and for as long as a handoff
/// freeze holds it), and it TRIMS the cache when the count is below it (the
/// handoff successor). Padded, the rule sat MID-band with an unruled blank row
/// under it touching the terminal; trimmed, the row that carried it was cut off
/// and the band had no edge at all. And the presence row's seam "when no band row
/// follows" was promised in three comments and never drawn. Sealing the stack
/// after it is composed covers the painted, padded, trimmed and presence-only
/// shapes with one rule. No band or presence cell uses underline for anything
/// else, so clearing it above the last row erases nothing but a stale seam.
pub(crate) fn seal_stack(rows: &mut [Vec<RenderCell>], theme: Theme) {
    let Some((last, above)) = rows.split_last_mut() else {
        return;
    };
    for row in above {
        for cell in row.iter_mut() {
            cell.underline = UnderlineStyle::None;
            cell.underline_color = None;
        }
    }
    chrome_band::seal_band_bottom(last, chrome_band::band_colors(theme).label);
}

/// The ink a time slot's words wear (design §10.7): a stall, and a Fault
/// echo's `failed`, are the one thing on the row the person may need to act
/// on — warn; a Complete echo's `done` wears the ink its ✓ does; a remaining
/// time (it always ends in `left`) reads in the value ink; a CLOCK — the
/// elapsed time, and what a determinate row's ETA slot says while its
/// estimate is hidden — in the label.
fn slot_ink(words: &str, c: &BandColors, hc: bool) -> [u8; 3] {
    if words == STALLED_WORD || words == FAILED_WORD {
        c.warn
    } else if words == DONE_WORD {
        if hc { c.value } else { c.accent }
    } else if words.ends_with(" left") {
        c.value
    } else {
        c.label
    }
}

/// One row at one motion frame: the meter's surface (the engine's
/// [`aterm_messages::Surface`] mapped onto the window, [`MeterSpan`]), then
/// the glyph (a busy row's spinner frame, an echo's ✓ / ⚠), title, excerpt,
/// pct, the time slots, the load words, stats, and the capsules over it;
/// last an echo's fade. `Some(())` when the row has a surface (its gutters
/// then continue its painted edge cells), `None` otherwise.
#[allow(
    clippy::too_many_arguments,
    reason = "one row's paint reads its layout, its frame, the palette, the look, the hover and the window — each a separate input the splice already holds"
)]
fn paint_row(
    row: &mut [RenderCell],
    cols: usize,
    l: &RowLayout,
    rm: &RowMotion,
    c: &BandColors,
    hc: bool,
    hover: Option<HoverTarget>,
    geom: BandGeometry,
) -> Option<()> {
    let overflow = matches!(l.kind, RowKind::Overflow { .. });
    let alarm = matches!(l.severity, Severity::Warn | Severity::Error);
    // Success and Info share the title ink and Warn/Error share `warn` (§2.6);
    // the overflow row is a link, not a message, and reads in `label`.
    // Under High Contrast every ink is WINDOWTEXT (module doc): the glyph
    // too — a stock palette's HIGHLIGHT is made to sit under highlight text,
    // and on Aquatic's black band it is 1.3:1.
    let (ink, accent) = if overflow {
        (c.label, c.label)
    } else if alarm {
        (c.warn, c.warn)
    } else if hc {
        (c.value, c.value)
    } else {
        (c.value, c.accent)
    };
    // THE METER IS THE ROW: lay its surface under every column first, each
    // column the mean tone of its window-pixel span. The fill's ink is the
    // cursor accent, `warn` on a Warn/Error row; under High Contrast the
    // system's HIGHLIGHT whatever the severity — the forced `warn` is
    // WINDOWTEXT, an ink, not a surface — and the words on it HIGHLIGHTTEXT.
    let fill = if hc || !alarm { c.accent } else { c.warn };
    // A BUSY row's surface keeps its words' side (the comet, its track and
    // its echoes): never a fill a chip must dodge, and every word floored
    // toward the one anchor.
    let (inks, side) = if l.track.is_some() && !hc {
        let (inks, anchor) = MeterInks::comet(c, fill);
        (inks, Some(anchor))
    } else {
        (MeterInks::of(c, fill), None)
    };
    let span = MeterSpan { geom, cols };
    let tones: Option<Vec<([u8; 3], bool)>> = (!rm.surface.is_empty() && cols > 0).then(|| {
        (0..cols)
            .map(|x| {
                let t = span.fine(&rm.surface, x);
                let rgb = fine_rgb(t, &inks);
                match side {
                    Some(anchor) => (hold_side(rgb, inks.track, anchor, WORD_AA), false),
                    None => (rgb, t.fill >= 128 << 8),
                }
            })
            .collect()
    });
    let on = Ground {
        band: c.bar_bg,
        meter: tones.as_deref(),
        hc,
        hc_fill_ink: c.on_accent,
        side,
        sides: (side.is_none() && !hc).then(|| (fill_anchor(inks.fill), words_anchor(c))),
    };
    if let Some(tones) = &tones {
        for (cell, &(bg, _)) in row.iter_mut().zip(tones) {
            *cell = chrome_band::cell(' ', c.label, bg, false, false);
        }
    }
    let (gcol, glyph) = l.glyph;
    if gcol < cols {
        // The engine's glyph for this frame: a moving busy row's spinner, an
        // echo's ✓ / ⚠ (a Fault echo's ⚠ in warn whatever the row's
        // severity was) — else the row's own.
        let fault = matches!(
            rm.anim,
            Anim::Echo {
                kind: EchoKind::Fault,
                ..
            }
        );
        let g_ink = if fault { c.warn } else { accent };
        let glyph = rm.glyph.unwrap_or(glyph);
        let mut g = chrome_band::cell(glyph, on.ink(gcol, g_ink), on.at(gcol), !overflow, false);
        g.text_presentation = true;
        row[gcol] = g;
    }
    let (tcol, title) = &l.title;
    on.write(row, cols, *tcol, title, ink, !overflow);
    if let Some((dcol, d)) = &l.detail {
        // The ` · ` joint occupies the three cells before the excerpt.
        on.write(
            row,
            cols,
            dcol.saturating_sub(2),
            "\u{00b7}",
            c.label,
            false,
        );
        on.write(row, cols, *dcol, d, c.label, false);
    }
    if let Some((pcol, p)) = &l.pct {
        on.write(row, cols, *pcol, p, ink, false);
    }
    if let Some(col) = l.elapsed {
        let words = rm.readout.as_deref().unwrap_or("");
        on.slot(row, cols, (col, ELAPSED_W), words, slot_ink(words, c, hc));
    }
    if let Some(col) = l.eta {
        let words = rm.eta.as_deref().unwrap_or("");
        on.slot(
            row,
            cols,
            (col, l.eta_width()),
            words,
            slot_ink(words, c, hc),
        );
    }
    if let Some((lcol, words)) = l.load {
        on.write(
            row,
            cols,
            lcol.saturating_sub(2),
            "\u{00b7}",
            c.label,
            false,
        );
        on.write(row, cols, lcol, words, c.label, false);
    }
    if let Some((scol, s)) = &l.stats {
        on.write(row, cols, *scol, s, c.label, false);
    }
    for cap in &l.capsules {
        let lit = match hover {
            Some(HoverTarget::Capsule(k)) => cap.action == k,
            Some(HoverTarget::Body) => cap.action.is_details(),
            None => false,
        };
        paint_capsule(row, cols, cap, c, hc, lit, &on);
    }
    // An echo's fade: every cell's ink and ground toward the band (never the
    // underline — the splice seals the stack after this).
    if rm.fade > 0 {
        for cell in row.iter_mut() {
            cell.fg = mix_u8(cell.fg, c.bar_bg, rm.fade);
            cell.bg = mix_u8(cell.bg, c.bar_bg, rm.fade);
        }
    }
    tones.map(|_| ())
}

/// One chip over its `width` cells from `col`: the pad cells carry the fill
/// (or, under High Contrast, the brackets), the text sits between them. A
/// lit Primary keeps its ink and weight and brightens its fill; every other
/// lit chip takes the hover fill and ink.
///
/// On a METERED row ([`Ground::meter`]) a chip must not vanish into the
/// meter under it: a Secondary (whose resting fill is the track) wears the
/// band's own `bar_bg`; a Primary (whose fill is the accent) keeps it over
/// the track but turns to `bar_bg` with an accent ink the moment any of its
/// cells lies over the fill; `Details ›` and every High Contrast chip carry
/// no fill of their own and ride the meter like words. A lit chip keeps its
/// hover fill: the pointer is on it.
fn paint_capsule(
    row: &mut [RenderCell],
    cols: usize,
    cap: &CapsuleLayout,
    c: &BandColors,
    hc: bool,
    lit: bool,
    on: &Ground<'_>,
) {
    if cap.width < 2 {
        return;
    }
    let last = cap.col + cap.width - 1;
    let (open, close) = if hc { ('[', ']') } else { (' ', ' ') };
    let metered = on.meter.is_some();
    // No fill of its own: every cell rides the surface under it, like a word.
    let rides = !lit && (hc || cap.role == CapsuleRole::Details);
    if rides && metered {
        let ink = if hc { c.value } else { c.label };
        let bold = hc && cap.role == CapsuleRole::Primary;
        if cap.col < cols {
            row[cap.col] =
                chrome_band::cell(open, on.ink(cap.col, ink), on.at(cap.col), false, false);
        }
        on.write(row, cols, cap.col + 1, &cap.text, ink, bold);
        if last < cols {
            row[last] = chrome_band::cell(close, on.ink(last, ink), on.at(last), false, false);
        }
        return;
    }
    let over_fill = (cap.col..=last).any(|x| on.on_fill(x));
    let (fg, bg, bold) = if hc {
        if lit {
            match cap.role {
                CapsuleRole::Primary => {
                    (c.capsule_primary_hover_ink, c.capsule_primary_hover, true)
                }
                CapsuleRole::Secondary | CapsuleRole::Details => {
                    (c.capsule_hover_ink, c.capsule_hover, false)
                }
            }
        } else {
            (c.value, c.bar_bg, cap.role == CapsuleRole::Primary)
        }
    } else {
        chip_inks(c, cap.role, lit, metered && over_fill, metered)
    };
    if cap.col < cols {
        row[cap.col] = chrome_band::cell(open, fg, bg, false, false);
    }
    write_str(row, cols, cap.col + 1, &cap.text, fg, bg, bold);
    if last < cols {
        row[last] = chrome_band::cell(close, fg, bg, false, false);
    }
}

/// A chip's `(ink, fill, bold)` off High Contrast, every label at AA on its
/// own fill (design ruling 155) — over the band, the track or the fill,
/// resting or lit. A Primary keeps its polarity, the band's ink on the
/// accent: where that pair falls short (3.69:1 on Solarized Light, 4.28:1 on
/// GitHub Light) the ACCENT deepens in linear light, its hue kept, by the
/// least amount that clears AA ([`keep_side`]) — flooring the ink instead
/// turned Solarized's label black on blue. The Primary that turns to the
/// band over a fill wears that same deepened accent as its ink, and the lit
/// Primary is lifted from it and deepened the same way (ruling 160). Every
/// other label is lifted to AA by `ensure_contrast_either`.
fn chip_inks(
    c: &BandColors,
    role: CapsuleRole,
    lit: bool,
    over_fill: bool,
    metered: bool,
) -> ([u8; 3], [u8; 3], bool) {
    let accent = keep_side(c.accent, c.bar_bg, WORD_AA);
    let (fg, bg, bold) = match (role, lit) {
        // Lifted from the accent the chip WEARS, so a deepened rest keeps a
        // whole lift between it and its lit form (ruling 160; Catppuccin
        // Latte's rest and lit were 1.01:1 apart when the lit fill was
        // lifted from the raw accent and deepened on its own).
        (CapsuleRole::Primary, true) => {
            let ink = c.capsule_primary_hover_ink;
            let lit = chrome_band::mix3(
                accent,
                c.primary_lift_toward,
                chrome_band::PRIMARY_HOVER_LIFT,
            );
            (ink, keep_side(lit, ink, WORD_AA), true)
        }
        (CapsuleRole::Secondary | CapsuleRole::Details, true) => {
            (c.capsule_hover_ink, c.capsule_hover, false)
        }
        (CapsuleRole::Primary, false) if over_fill => (accent, c.bar_bg, true),
        (CapsuleRole::Primary, false) => (c.bar_bg, accent, true),
        (CapsuleRole::Secondary, false) if metered => (c.value, c.bar_bg, false),
        (CapsuleRole::Secondary, false) => (c.value, c.meter_track, false),
        (CapsuleRole::Details, false) => (c.label, c.bar_bg, false),
    };
    (
        chrome_band::ensure_contrast_either(fg, bg, WORD_AA),
        bg,
        bold,
    )
}

/// The band row and column for a frame pixel, given the geometry — the pure
/// half of [`App::band_hit_at`]. `gx`/`gy` are frame pixels from the frame
/// origin; `pad` and `top` are the cell lattice's offsets; the chrome rows
/// between the strip and the grid are `presence_rows` then `band_rows`.
#[allow(
    clippy::too_many_arguments,
    reason = "the pure hit test takes the geometry as plain numbers so its tests need no App"
)]
fn band_target_for_pixel(
    gx: usize,
    gy: usize,
    (cw, ch): (usize, usize),
    pad: usize,
    top: usize,
    strip_rows: usize,
    presence_rows: usize,
    band_rows: usize,
) -> Option<BandTarget> {
    if band_rows == 0 && presence_rows == 0 {
        return None;
    }
    let (cw, ch) = (cw.max(1), ch.max(1));
    let gy = gy.checked_sub(top)?;
    let strip_px = strip_rows * ch;
    let presence_px = presence_rows * ch;
    let band_px = band_rows * ch;
    if gy < strip_px || gy >= strip_px + presence_px + band_px {
        return None;
    }
    if gy < strip_px + presence_px {
        return Some(BandTarget::Presence);
    }
    Some(BandTarget::Row {
        row: (gy - strip_px - presence_px) / ch,
        col: gx.saturating_sub(pad) / cw,
    })
}

/// The `$HOME` the band abbreviates paths under (`~/…`); the log keeps the
/// absolute path (D4).
pub(crate) fn band_home() -> Option<String> {
    aterm_types::dirs::home_dir().map(|h| h.to_string_lossy().into_owned())
}

/// The RepaintKey's band term: the center's fingerprint at `cols` ⊕ the
/// window's hover ⊕ the window geometry a full-width meter (or a busy row's
/// comet) is mapped onto (ruling 55: a resize that keeps the column count still
/// moves its lit cells) ⊕ the motion frame's fingerprint
/// ([`BandMotion::fingerprint`], ruling 140). **Exactly `0` with no row
/// committed** (the center's own FL-1 term; the hover is `None` and the motion
/// 0 then by construction, and the geometry is not folded), so an idle key is
/// byte-identical to the no-band path; else nonzero, and moved by a hover
/// change so the lit chip re-presents, and by a motion frame that draws
/// something new — the key and the splice's cache key read the same motion
/// term (design §3.2). The motion term is folded only when nonzero, so a
/// motion-free band's key does not move with the clock.
pub(crate) fn band_fp(
    center_fp: u64,
    hover: Option<BandHover>,
    geom: BandGeometry,
    motion_fp: u64,
) -> u64 {
    if center_fp == 0 {
        return 0;
    }
    let center_fp = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        center_fp.hash(&mut h);
        geom.hash(&mut h);
        h.finish()
    };
    let term = match hover {
        None => 0u64,
        Some(BandHover { row, target }) => {
            let t = match target {
                HoverTarget::Body => 0x100,
                HoverTarget::Capsule(k) => 0x200 | u64::from(k.0),
            };
            (u64::from(row) << 16) | t | 1
        }
    };
    let fp = center_fp ^ term.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let fp = if motion_fp == 0 {
        fp
    } else {
        fp ^ motion_fp
            .wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            .rotate_left(17)
    };
    fp | 1
}

impl App {
    /// Where window `wid`'s band cells sit on its glass
    /// ([`BandGeometry`]): the window's width, column 0's window x (the
    /// frame's left gutter plus the leading remainder band — the same
    /// `frame_origin` the presenters place the frame at), and the cell width,
    /// all device px. A window with no surface yet (headless, pre-attach)
    /// answers the frame itself: `cols·cw + 2·pad` wide, cells at `pad`.
    pub(crate) fn band_geometry(&self, wid: WindowId) -> BandGeometry {
        let (cw, _) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let cols = self.windows.get(&wid).map_or(0, |ws| usize::from(ws.cols));
        let frame_w = cols
            .saturating_mul(cw)
            .saturating_add(pad.saturating_mul(2));
        let win_w = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.win_px)
            .map_or(frame_w, |s| s.width as usize);
        let (ox, _) = self.frame_origin(wid);
        let cells_x = usize::try_from(ox.saturating_add(pad as i64)).unwrap_or(0);
        BandGeometry {
            win_w,
            cells_x,
            cell_w: cw.max(1),
        }
    }

    /// The band's layout at `cols` for window `wid`'s painter — the width law
    /// over the committed rows, paths under `$HOME` printed as `~/…`, every
    /// row ending in its link ([`Links::Painted`]: Settings ▸ Messages is
    /// where each one goes — module doc).
    pub(crate) fn band_presentation(&self, cols: usize) -> Presentation {
        let home = band_home();
        self.messages
            .presentation(cols, &cell_width, home.as_deref(), Links::Painted)
    }

    /// If window pixel `(x, y)` lands on one of window `wid`'s chrome rows
    /// between the strip and the grid — the presence row, or a band row —
    /// which, and at which cell column. `None` when nothing is up there or the
    /// point is elsewhere. The band is chrome: a press on it never reaches the
    /// terminal underneath — there is no terminal underneath, the grid starts
    /// below it.
    pub(crate) fn band_hit_at(&self, wid: WindowId, x: f64, y: f64) -> Option<BandTarget> {
        let presence_rows = self.windows.get(&wid).map_or(0, |ws| ws.presence.rows);
        let (fx, fy) = self.window_to_frame(wid, x, y);
        band_target_for_pixel(
            fx as usize,
            fy as usize,
            self.win_cell_size(wid),
            self.win_pad(wid),
            self.win_pad_top(wid) + self.win_head(wid),
            usize::from(self.tab_strip_rows),
            usize::from(presence_rows),
            usize::from(self.message_band_rows),
        )
    }

    /// What a press at `target` in window `wid` lands on, through the width
    /// law at this window's width — the same layout the painter drew.
    fn band_hit(&self, wid: WindowId, target: BandTarget) -> Hit {
        let BandTarget::Row { row, col } = target else {
            return Hit::Nothing;
        };
        let Some(cols) = self.windows.get(&wid).map(|ws| usize::from(ws.cols)) else {
            return Hit::Nothing;
        };
        self.band_presentation(cols).hit(row, col)
    }

    /// A press on window `wid`'s chrome rows between the strip and the grid
    /// (design §3.4): the presence row is swallowed with no action; a capsule
    /// performs its intent (the engine's `act` logs the press and returns
    /// what to do); the `Details ›` and the row BODY are the same press
    /// ([`App::press_message_body`]: Settings ▸ Messages at that entry, in
    /// this window, the row marked seen); the overflow row is one link to
    /// the same page, top of the log. The press is consumed either way —
    /// there is no terminal under the band.
    pub(crate) fn press_band(&mut self, wid: WindowId, target: BandTarget) {
        match self.band_hit(wid, target) {
            Hit::Capsule(id, k) => {
                let intent = self.messages.act(id, k, std::time::Instant::now());
                // The press is on record (`Acted`) whatever the intent does.
                self.sync_messages();
                if let Some(intent) = intent {
                    let _ = self.perform_intent(wid, id, intent);
                }
            }
            Hit::Details(id) | Hit::Body(id) => self.press_message_body(wid, id),
            Hit::Overflow => {
                let _ = self.open_messages_entry(wid, None);
            }
            Hit::Nothing => {}
        }
    }

    /// Whether a press at `target` would DO something — the pointer is a hand
    /// there and nowhere else on the band.
    pub(crate) fn band_press_acts(&self, wid: WindowId, target: BandTarget) -> bool {
        matches!(
            self.band_hit(wid, target),
            Hit::Capsule(..) | Hit::Details(_) | Hit::Body(_) | Hit::Overflow
        )
    }

    /// The chip to light for a pointer at `target`: the capsule under it
    /// (the `Details ›` included), or — on the row BODY, and anywhere on the
    /// overflow row — the link a press there follows: the row's `Details ›`,
    /// the overflow row's `Messages ›` (the painter's `Body` arm lights the
    /// row's link capsule). `None` off the band and on the presence row.
    pub(crate) fn band_hover_for(&self, wid: WindowId, target: BandTarget) -> Option<BandHover> {
        let BandTarget::Row { row, .. } = target else {
            return None;
        };
        let row = u8::try_from(row).ok()?;
        let target = match self.band_hit(wid, target) {
            Hit::Capsule(_, k) => HoverTarget::Capsule(k),
            Hit::Details(_) => HoverTarget::Capsule(ActionIndex::DETAILS),
            Hit::Body(_) | Hit::Overflow => HoverTarget::Body,
            Hit::Nothing => return None,
        };
        Some(BandHover { row, target })
    }

    /// Track which band chip the pointer is over, on every `CursorMoved` —
    /// including motion that later paths consume, because leaving the band
    /// must clear the lit chip as surely as entering it lights one. Cheap
    /// and change-gated like the strip's tracker: a sweep along one chip
    /// costs nothing, and the redraw is what re-runs the splice, whose cache
    /// key carries the hover.
    pub(crate) fn track_band_hover(&mut self, wid: WindowId, x: f64, y: f64) {
        let hover = self
            .band_hit_at(wid, x, y)
            .and_then(|target| self.band_hover_for(wid, target));
        self.set_band_hover(wid, hover);
    }

    /// Write the band hover, requesting a redraw only on a CHANGE.
    pub(crate) fn set_band_hover(&mut self, wid: WindowId, hover: Option<BandHover>) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if ws.band_hover == hover {
            return;
        }
        ws.band_hover = hover;
        if let Some(window) = &ws.os_window {
            window.request_redraw();
        }
    }
}
/// Whether the colours `seq` — a Fault echo's frames from the row's last
/// one to the fault hue — warm STRAIGHT to it (design ruling 156): the
/// OkLCh lightness moves one way between its ends; no frame is greyer
/// than both ends by more than the gamut forces; the hue turns one way
/// and never back; and no frame enters green unless the row started
/// there. `None` when it does, else what failed.
#[cfg(test)]
pub(crate) fn warms_straight(seq: &[[u8; 3]]) -> Option<String> {
    let (Some(&first), Some(&last)) = (seq.first(), seq.last()) else {
        return None;
    };
    let (l0, c0, h0) = oklch(first);
    let (l1, c1, _) = oklch(last);
    let lch: Vec<_> = seq.iter().map(|&c| oklch(c)).collect();
    let tol = 0.012;
    for (k, pair) in lch.windows(2).enumerate() {
        let step = pair[1].0 - pair[0].0;
        if step * (l1 - l0) < -tol * tol && step.abs() > tol {
            return Some(format!("frame {k}: the lightness turned back: {seq:?}"));
        }
    }
    let floor = c0.min(c1);
    for (k, &(l, c, h)) in lch.iter().enumerate() {
        if l > l0.max(l1) + tol && c < floor - tol {
            return Some(format!("frame {k} is lighter and greyer than both ends"));
        }
        if c < floor * 0.45 - tol {
            return Some(format!(
                "frame {k} drained to grey: chroma {c:.3} of {floor:.3}: {seq:?}"
            ));
        }
        let green = h > GREEN_HUES.0 + 4.0 && h < GREEN_HUES.1 - 4.0;
        let started = h0 > GREEN_HUES.0 - 4.0 && h0 < GREEN_HUES.1 + 4.0;
        if c > HUELESS * 1.5 && green && !started && c0 > HUELESS {
            return Some(format!("frame {k} passed through green ({h:.0}°): {seq:?}"));
        }
    }
    // The hue's turn from the first hued frame: one sign, never back.
    let hued: Vec<f64> = lch
        .iter()
        .filter(|&&(_, c, _)| c > HUELESS * 1.5)
        .map(|&(_, _, h)| h)
        .collect();
    let mut sign = 0.0f64;
    for pair in hued.windows(2) {
        let d = (pair[1] - pair[0] + 540.0).rem_euclid(360.0) - 180.0;
        if d.abs() < 3.0 {
            continue;
        }
        if sign != 0.0 && d.signum() != sign {
            return Some(format!("the hue turned back ({d:.0}°): {seq:?}"));
        }
        sign = d.signum();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_messages::{
        GLYPH_COL, Hit, Hold, Instant, Intent, Look, Message, MessageCenter, MessageLog, Meter,
        Severity, Tone, WallStamp, tags,
    };

    fn t0() -> Instant {
        Instant::now()
    }

    fn stamp() -> WallStamp {
        WallStamp { unix_ms: 1 }
    }

    fn text_of(row: &[RenderCell]) -> String {
        row.iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// A center with the toolchain meter row and a downloading update row
    /// committed, the fixture the status bars' painter test used.
    fn two_rows() -> MessageCenter {
        let now = t0();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        center.post(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab tools")
                .line("trust — extracting 120 MB / 900 MB")
                .meter(Meter {
                    fill_permille: Some(427),
                    stats: "3 of 10 · 512 MB / 1.2 GB".into(),
                    ..Meter::default()
                })
                .hold(Hold::Live {
                    stale_after: aterm_messages::STALE_TAILED,
                })
                .action(Intent::OpenSettings {
                    route: "/packages".into(),
                }),
            stamp(),
            now,
        );
        center.post(
            Message::new(tags::UPDATE, Severity::Info, "aterm update v0.48.0")
                .line("downloading…")
                .meter(Meter {
                    fill_permille: Some(608),
                    stats: "45 MB / 74 MB".into(),
                    ..Meter::default()
                })
                .hold(Hold::Live {
                    stale_after: aterm_messages::STALE_UPDATE,
                }),
            stamp(),
            now,
        );
        assert_eq!(center.commit_rows(now, 3), Some(2));
        center
    }

    /// The host's own presentation: every row ending in its link.
    fn present(center: &MessageCenter, cols: usize) -> Presentation {
        center.presentation(cols, &cell_width, None, Links::Painted)
    }

    /// The band painted at `center`'s STILL frame over `p` on the unit grid —
    /// the time-free picture most of these tests read (a held bar at its
    /// data, a busy row's unlit track).
    fn paint_still(
        p: &Presentation,
        theme: Theme,
        hover: Option<BandHover>,
        center: &MessageCenter,
    ) -> Vec<Vec<RenderCell>> {
        paint_rows(p, theme, hover, &center.motion(p, t0(), Look::STILL))
    }

    /// The port of the bars' painter test: every row is exactly `cols` wide and
    /// carries the words, the meter is drawn as background cells, only the
    /// last row closes the chrome with the seam, and the glyph is text
    /// presentation.
    #[test]
    fn paint_rows_are_exactly_cols_wide_and_carry_the_words() {
        let center = two_rows();
        let p = present(&center, 140);
        let rows = paint_still(&p, Theme::default(), None, &center);
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert_eq!(row.len(), 140);
        }
        let top = text_of(&rows[0]);
        assert!(top.contains("Installing ALab tools"), "{top}");
        assert!(top.contains("\u{00b7} trust \u{2014} extracting"), "{top}");
        assert!(top.contains("42%"), "{top}");
        assert!(top.contains("3 of 10 · 512 MB / 1.2 GB"), "{top}");
        assert!(
            top.ends_with("  Packages   Details \u{203a}"),
            "the authored capsule, then the row's link: {top}"
        );
        let bottom = text_of(&rows[1]);
        assert!(bottom.contains("aterm update v0.48.0"), "{bottom}");
        assert!(bottom.contains("60%"), "{bottom}");
        assert!(
            bottom.contains("45 MB / 74 MB") && bottom.ends_with("Details \u{203a}"),
            "a capsule-less row carries its stats and ends in its link: {bottom}"
        );
        let c = chrome_band::band_colors(Theme::default());
        // THE METER IS THE ROW (rulings 55, 136): the toolchain row's 42.7 %
        // lights the cells left of its edge in the full accent, the cells
        // right of it are the track, and the ONE cell under the edge takes
        // its coverage — a whole-cell background tone, never meter ink.
        let (mcol, w, fill) = p.rows[0].meter.expect("a meter at 140 cols");
        assert_eq!((mcol, w, fill), (0, 140, 427), "the meter is the row");
        let edge = 140 * 427 / 1000;
        assert!(rows[0][..edge].iter().all(|cell| cell.bg == c.accent));
        assert!(
            rows[0][edge + 1..]
                .iter()
                .all(|cell| cell.bg == c.meter_track || cell.bg == c.bar_bg),
            "past the edge: the track (a Secondary chip wears the band)"
        );
        let inks = MeterInks::of(&c, c.accent);
        let partial = rows[0][edge].bg;
        assert!(
            (1..255u8).any(|f| tone_rgb(
                Tone {
                    fill: f,
                    lift: 0,
                    warn: 0
                },
                &inks
            ) == partial),
            "the edge cell is a mix of track and fill: {partial:?}"
        );
        assert!(
            rows[0]
                .iter()
                .all(|cell| !(0xFDD0..=0xFDEF).contains(&u32::from(cell.ch))),
            "no procedural meter glyph is left on the band"
        );
        // The painter draws no seam at all: only the composed stack knows which
        // row is last ([`seal_stack`]).
        assert!(
            rows.iter()
                .flatten()
                .all(|cell| cell.underline == UnderlineStyle::None)
        );
        assert!(rows[0][MARGIN].text_presentation);
        assert!(rows[0][MARGIN].bold);
        // Both sit on the meter's fill (ruling 55): each wears its role's ink,
        // floored against the fill.
        let fill = rows[0][MARGIN].bg;
        assert_eq!(fill, c.accent, "the glyph sits on the fill");
        assert_eq!(
            rows[0][MARGIN].fg,
            floor_word(c.accent, fill, fill_anchor(c.accent)),
            "an Info glyph is accent, floored on the fill"
        );
        assert_eq!(
            rows[0][p.rows[0].title.0].fg,
            floor_word(c.value, fill, fill_anchor(c.accent)),
            "the title is value, floored on the fill"
        );
        assert!(rows[0][p.rows[0].title.0].bold);
    }

    /// Layout and hit share one law: every cell a capsule is painted on maps
    /// back to that capsule — the `Details ›` to the Details hit — and the
    /// gaps map to the body. Below its long width `Details ›` goes (it has
    /// no short form: a lone `›` said nothing the body press does not do).
    #[test]
    fn capsules_are_hit_where_they_are_painted() {
        let center = two_rows();
        let c = chrome_band::band_colors(Theme::default());
        for cols in [60, 80, 120, 160] {
            let p = present(&center, cols);
            let rows = paint_still(&p, Theme::default(), None, &center);
            let id = center.on_glass().next().unwrap().id;
            let layout = &p.rows[0];
            let link = layout
                .capsules
                .last()
                .is_some_and(|c| c.action.is_details());
            assert_eq!(
                layout.capsules.len(),
                1 + usize::from(link),
                "cols {cols}: Packages, then the row's Details \u{203a} when it fits"
            );
            // The meter costs the words nothing (ruling 136, main's width
            // law), so the link the pill used to crowd out is last from 80
            // up. At 60 this moving row's PAINTED excerpt is an action
            // excerpt (ruling 153): `Details ›` pays for it, never the
            // excerpt for the link.
            assert!(
                link || (cols < 80 && layout.detail.is_some()),
                "cols {cols}: the link is last"
            );
            assert!(
                layout.capsules.iter().all(|c| c.text != "\u{203a}"),
                "cols {cols}: never a lone \u{203a}"
            );
            if !link {
                assert_eq!(
                    p.hit(0, layout.title.0),
                    Hit::Body(id),
                    "the body press performs Details"
                );
            }
            for cap in &layout.capsules {
                let painted: String = rows[0][cap.col + 1..cap.col + cap.width - 1]
                    .iter()
                    .map(|cell| cell.ch)
                    .collect();
                assert_eq!(painted, cap.text, "cols {cols}: painted where laid out");
                let expected = if cap.action.is_details() {
                    Hit::Details(id)
                } else {
                    Hit::Capsule(id, cap.action)
                };
                for col in cap.col..cap.col + cap.width {
                    assert_eq!(p.hit(0, col), expected, "cols {cols} col {col}");
                }
                // `Packages` is a navigation: the quiet chip, never the
                // accent (ruling 18). A Primary would wear the accent;
                // Details no fill.
                assert_eq!(
                    cap.role,
                    if cap.action.is_details() {
                        CapsuleRole::Details
                    } else {
                        CapsuleRole::Secondary
                    },
                    "cols {cols}"
                );
                match cap.role {
                    CapsuleRole::Primary => {
                        assert_eq!(rows[0][cap.col].bg, c.accent);
                        assert_eq!(rows[0][cap.col + 1].fg, c.bar_bg);
                        assert!(rows[0][cap.col + 1].bold);
                    }
                    // Row 0 is METERED (ruling 55): the quiet chip wears the
                    // band's own surface so it cannot vanish into the track,
                    // and `Details ›` rides the meter — the track under it at
                    // this fill, its ink floored there.
                    CapsuleRole::Secondary => {
                        assert_eq!(rows[0][cap.col].bg, c.bar_bg);
                        assert_eq!(rows[0][cap.col + 1].fg, c.value);
                        assert!(!rows[0][cap.col + 1].bold);
                    }
                    CapsuleRole::Details => {
                        assert_eq!(rows[0][cap.col].bg, c.meter_track);
                        assert_eq!(
                            rows[0][cap.col + 1].fg,
                            floor_word(c.label, c.meter_track, words_anchor(&c))
                        );
                    }
                }
            }
            let first = layout.capsules.first().unwrap().col;
            assert_eq!(p.hit(0, first - 1), Hit::Body(id), "cols {cols}");
            assert_eq!(p.hit(0, layout.title.0), Hit::Body(id), "cols {cols}");
        }
    }

    /// Seven held rows on a three-row band: rows 0–1 are messages, row 2 is
    /// `… 5 more messages` in `label`, not bold, ending in its `Messages ›`
    /// link, and a press ANYWHERE on it is that one link (the chip and the
    /// words are the same target — the row is a link, not a message).
    #[test]
    fn the_overflow_row_is_one_details_link() {
        let now = t0();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        for i in 0..7 {
            center.post(
                Message::new(tags::CONFIG, Severity::Warn, format!("warning {i}")),
                stamp(),
                now,
            );
        }
        assert_eq!(center.commit_rows(now, 3), Some(3));
        let p = present(&center, 80);
        assert_eq!(p.rows.len(), 3);
        assert!(matches!(p.rows[2].kind, RowKind::Overflow { hidden: 5 }));
        let rows = paint_still(&p, Theme::default(), None, &center);
        let text = text_of(&rows[2]);
        assert!(text.starts_with(" … 5 more messages"), "{text}");
        assert!(text.ends_with(" Messages \u{203a}"), "{text}");
        assert_eq!(p.rows[2].capsules.len(), 1, "one link");
        let link = &p.rows[2].capsules[0];
        assert!(link.action.is_details());
        assert_eq!(link.role, CapsuleRole::Details);
        assert_eq!(link.full_label, "Messages \u{203a}");
        let c = chrome_band::band_colors(Theme::default());
        assert_eq!(rows[2][MARGIN].fg, c.label);
        assert!(!rows[2][MARGIN].bold);
        assert_eq!(rows[2][p.rows[2].title.0].fg, c.label);
        assert!(!rows[2][p.rows[2].title.0].bold);
        for col in 0..80 {
            assert_eq!(p.hit(2, col), Hit::Overflow, "col {col}");
        }
        // A pointer anywhere on the row lights its link.
        let lit = paint_still(
            &p,
            Theme::default(),
            Some(BandHover {
                row: 2,
                target: HoverTarget::Body,
            }),
            &center,
        );
        assert_eq!(lit[2][link.col].bg, c.capsule_hover, "the link lights");
        assert_eq!(lit[1][MARGIN].bg, c.bar_bg, "the row above does not");
    }

    /// The stack's LAST row carries the seam, every other row none, in one tone —
    /// whatever shape the splice composed: painted rows, a padded blank row under
    /// them, or a single row.
    #[test]
    fn the_stack_is_sealed_on_its_last_row_only() {
        let theme = Theme::default();
        let ink = chrome_band::band_colors(theme).label;
        let sealed = |row: &[RenderCell]| {
            row.iter()
                .all(|c| c.underline == UnderlineStyle::Single && c.underline_color == Some(ink))
        };
        let bare = |row: &[RenderCell]| row.iter().all(|c| c.underline == UnderlineStyle::None);

        let center = two_rows();
        let mut rows = paint_still(&present(&center, 100), theme, None, &center);
        assert!(rows.iter().all(|r| bare(r)), "the painter seals nothing");
        seal_stack(&mut rows, theme);
        assert!(bare(&rows[0]) && sealed(&rows[1]));

        // PADDED: the geometry still owns a third row after a dismissal. The seam
        // moves down to it and leaves the row that used to be last.
        rows.push(blank_band_row(100, theme));
        seal_stack(&mut rows, theme);
        assert!(bare(&rows[0]) && bare(&rows[1]) && sealed(&rows[2]));

        // A single row seals itself; an empty stack is left alone.
        let mut one = vec![blank_band_row(100, theme)];
        seal_stack(&mut one, theme);
        assert!(sealed(&one[0]));
        seal_stack(&mut [], theme);
    }

    /// Hover lights exactly one chip, on the hovered row only, whatever its
    /// role: a lit PRIMARY keeps its ink and weight and brightens its fill
    /// (never the grey hover that read as disabled, review 2026-09-22); a
    /// lit Secondary — the toolchain row's `Packages` is one now (ruling
    /// 18) — takes the hover fill; a BODY hover lights that row's
    /// `Details ›` and nothing else — it says what a press there will do.
    #[test]
    fn hover_lights_only_the_hovered_capsule_or_the_bodys_details() {
        let now = t0();
        let mut center = two_rows();
        // A two-capsule row beneath: Open Settings (a system pane —
        // consequential, Primary), Not now (a decline — Secondary).
        center.post(
            Message::new(tags::PRIVACY, Severity::Info, "File access not confirmed")
                .action(Intent::OpenSystemPane {
                    pane: "full-disk-access".into(),
                })
                .action(Intent::NotNow {
                    decision: aterm_messages::Decision::FileAccess,
                }),
            stamp(),
            now,
        );
        assert_eq!(center.commit_rows(now, 3), Some(3));
        let p = present(&center, 140);
        let c = chrome_band::band_colors(Theme::default());
        // The ask ranks first: the FDA row is row 0, the meter row below it
        // (every row ends in its Details \u{203a}: three chips, two, one).
        let ask = p
            .rows
            .iter()
            .position(|r| r.capsules.len() == 3)
            .expect("the two-capsule row");
        let meter = p
            .rows
            .iter()
            .position(|r| r.capsules.len() == 2)
            .expect("the Packages row");
        assert_eq!((ask, meter), (0, 1));
        assert_eq!(
            p.rows[2].capsules.len(),
            1,
            "the update row: its link alone"
        );
        let packages = &p.rows[meter].capsules[0];
        assert_eq!(
            packages.role,
            CapsuleRole::Secondary,
            "Packages is a navigation: the quiet chip"
        );
        let (open, not_now) = (&p.rows[ask].capsules[0], &p.rows[ask].capsules[1]);
        assert_eq!(
            open.role,
            CapsuleRole::Primary,
            "a system pane is consequential: the accent chip"
        );
        assert_eq!(not_now.role, CapsuleRole::Secondary);
        let lit_primary = |rows: &[Vec<RenderCell>], row: usize, cap: &CapsuleLayout| {
            rows[row][cap.col].bg == c.capsule_primary_hover
                && rows[row][cap.col + 1].fg == c.capsule_primary_hover_ink
                && rows[row][cap.col + 1].bold
        };
        let lit_grey = |rows: &[Vec<RenderCell>], row: usize, cap: &CapsuleLayout| {
            rows[row][cap.col].bg == c.capsule_hover
                && rows[row][cap.col + 1].fg == c.capsule_hover_ink
        };
        let none = paint_still(&p, Theme::default(), None, &center);
        assert!(!lit_primary(&none, ask, open) && !lit_grey(&none, meter, packages));
        assert_eq!(
            none[ask][open.col].bg, c.accent,
            "at rest the Primary wears the accent"
        );
        assert_eq!(
            none[meter][packages.col].bg, c.bar_bg,
            "at rest the quiet chip on a METERED row wears the band, never the track it would vanish into"
        );
        // The pointer on the Primary: lit in the accent's family.
        let on_primary = paint_still(
            &p,
            Theme::default(),
            Some(BandHover {
                row: ask as u8,
                target: HoverTarget::Capsule(open.action),
            }),
            &center,
        );
        assert!(
            lit_primary(&on_primary, ask, open),
            "lit, in the accent's family"
        );
        assert!(
            !lit_grey(&on_primary, ask, open),
            "never the grey that reads as disabled"
        );
        assert_ne!(
            c.capsule_primary_hover, c.accent,
            "the lit fill is a visible step off the resting accent"
        );
        assert!(
            !lit_grey(&on_primary, ask, not_now) && !lit_grey(&on_primary, meter, packages),
            "the other chips are untouched"
        );
        // The pointer on the toolchain row's `Packages`: the grey hover, and
        // the ask row untouched.
        let on_packages = paint_still(
            &p,
            Theme::default(),
            Some(BandHover {
                row: meter as u8,
                target: HoverTarget::Capsule(packages.action),
            }),
            &center,
        );
        assert!(
            lit_grey(&on_packages, meter, packages),
            "a Secondary takes the hover fill"
        );
        assert!(
            !lit_primary(&on_packages, meter, packages),
            "a quiet chip never lights in the accent"
        );
        assert!(
            !lit_primary(&on_packages, ask, open) && !lit_grey(&on_packages, ask, not_now),
            "the ask row is untouched"
        );
        let on_secondary = paint_still(
            &p,
            Theme::default(),
            Some(BandHover {
                row: ask as u8,
                target: HoverTarget::Capsule(not_now.action),
            }),
            &center,
        );
        assert!(
            lit_grey(&on_secondary, ask, not_now),
            "a Secondary takes the hover fill"
        );
        assert!(
            !lit_primary(&on_secondary, ask, open) && !lit_grey(&on_secondary, meter, packages)
        );
        let on_body = paint_still(
            &p,
            Theme::default(),
            Some(BandHover {
                row: meter as u8,
                target: HoverTarget::Body,
            }),
            &center,
        );
        let details = p.rows[meter]
            .capsules
            .iter()
            .find(|cap| cap.action.is_details())
            .expect("the meter row's Details \u{203a}");
        assert!(
            lit_grey(&on_body, meter, details),
            "a body hover lights the row's Details \u{203a}"
        );
        assert!(
            !lit_grey(&on_body, meter, packages),
            "…and not the authored chip beside it"
        );
        assert!(
            !lit_primary(&on_body, ask, open) && !lit_grey(&on_body, ask, not_now),
            "the other row is untouched"
        );
        assert_ne!(
            c.capsule_hover, c.bar_bg,
            "the hover fill is a visible step"
        );
    }

    /// The band paints on the chrome palette — every cell's ground is the
    /// band tone, never the terminal background the theme carries.
    #[test]
    fn rows_use_the_chrome_palette_not_the_osc11_tint() {
        let center = two_rows();
        let theme = Theme {
            bg: 0x0010_2030,
            fg: 0x00E0_E0E0,
            cursor: 0x00FF_8800,
            selection: 0x0044_4444,
        };
        let c = chrome_band::band_colors(theme);
        let bg = [0x10, 0x20, 0x30];
        assert_ne!(
            c.bar_bg, bg,
            "the band is a step off the theme, not the theme"
        );
        let p = present(&center, 100);
        let rows = paint_still(&p, theme, None, &center);
        // A meter cell is the track, the fill, or the one edge cell's mix of
        // the two (its coverage, ruling 138) — never anything else.
        let inks = MeterInks::of(&c, c.accent);
        let on_meter = |bg: [u8; 3]| {
            (0..=255u8).any(|f| {
                tone_rgb(
                    Tone {
                        fill: f,
                        lift: 0,
                        warn: 0,
                    },
                    &inks,
                ) == bg
            })
        };
        for row in &rows {
            for cell in row {
                assert_ne!(cell.bg, bg, "no cell shows the OSC-11 ground");
                assert!(
                    [
                        c.bar_bg,
                        c.accent,
                        c.meter_track,
                        c.capsule_hover,
                        c.capsule_primary_hover
                    ]
                    .contains(&cell.bg)
                        || on_meter(cell.bg),
                    "{:?} is not a band material",
                    cell.bg
                );
            }
        }
        assert_eq!(blank_band_row(10, theme)[0].bg, c.bar_bg);
    }

    /// Every admitted glyph lands in the glyph cell, text presentation on, so
    /// the raster path never takes its colour-emoji face; the raster proof
    /// that each paints a non-blank cell headless is the capture test's.
    #[test]
    fn every_admitted_glyph_is_text_presentation_in_the_glyph_cell() {
        let now = t0();
        for ch in aterm_messages::Glyph::ALLOWED {
            let mut center = MessageCenter::new(MessageLog::empty(), now);
            center.post(
                Message::new(tags::SYSTEM, Severity::Info, "glyph")
                    .glyph(aterm_messages::Glyph::new(*ch).unwrap()),
                stamp(),
                now,
            );
            center.commit_rows(now, 3);
            let p = present(&center, 40);
            let rows = paint_still(&p, Theme::default(), None, &center);
            let cell = &rows[0][aterm_messages::GLYPH_COL];
            assert_eq!(cell.ch, *ch);
            assert!(cell.text_presentation, "{ch:?}");
            assert!(!cell.emoji_presentation, "{ch:?}");
        }
    }

    /// Each column's lit coverage of a bar at `fill` on `g`: the fill channel
    /// of its window-pixel span's mean tone (0 track … 255 full).
    fn coverage(fill: u16, cols: usize, g: BandGeometry) -> Vec<u8> {
        coverage_in(fill, cols, g, true)
    }

    /// [`coverage`] in the FLAT look (High Contrast): whole inks only.
    fn coverage_flat(fill: u16, cols: usize, g: BandGeometry) -> Vec<u8> {
        coverage_in(fill, cols, g, false)
    }

    fn coverage_in(fill: u16, cols: usize, g: BandGeometry, graded: bool) -> Vec<u8> {
        let surface = aterm_messages::animate::bar(fill, None, graded);
        let span = MeterSpan { geom: g, cols };
        (0..cols).map(|x| span.tone(&surface, x).fill).collect()
    }

    /// THE HONEST ENDS IN THE FLAT LOOK (ruling 55; review 2026-09-24).
    /// Under High Contrast every cell is whole — the fill or the track — and
    /// the first and last columns carry the gutters, so their CENTRES would
    /// leave a started pass an empty track (1 ‰ on an 80-column window) and
    /// light the whole window at 99.x %. Main's `lit.clamp(1, cols − 1)`
    /// holds in every look: any started fill lights the first column, only
    /// 1000 ‰ the last, and the lit columns are one run from the left.
    #[test]
    fn the_flat_meter_keeps_the_honest_ends() {
        for (g, cols) in [
            (
                BandGeometry {
                    win_w: 1296,
                    cells_x: 8,
                    cell_w: 16,
                },
                80,
            ),
            (
                BandGeometry {
                    win_w: 2000,
                    cells_x: 31,
                    cell_w: 17,
                },
                114,
            ),
        ] {
            assert!(coverage_flat(0, cols, g).iter().all(|&f| f == 0), "0 %");
            assert!(
                coverage_flat(1000, cols, g).iter().all(|&f| f == 255),
                "100 %: the whole window"
            );
            for fill in 1..1000u16 {
                let cov = coverage_flat(fill, cols, g);
                assert!(
                    cov.iter().all(|&f| f == 0 || f == 255),
                    "{fill}: whole inks"
                );
                assert_eq!(cov[0], 255, "{fill}: a started pass shows");
                assert_eq!(cov[cols - 1], 0, "{fill}: only 100 % fills the last");
                assert!(
                    cov.windows(2).all(|p| p[0] >= p[1]),
                    "{fill}: one run from the left"
                );
            }
        }
    }

    /// THE METER IS THE ROW, the MAPPING (ruling 55, 2026-09-23 — owner:
    /// *"make sure that 0% and 100% and similar concepts are mapped to the
    /// relative size of the screen with such top progress bars"* — amended by
    /// ruling 138: whole cells, smooth by TONE coverage). On a real 2000 px
    /// Retina window (pad 24, 17 px cells, 114 columns, a 14 px remainder
    /// split 7/7, so column 0 at x = 31): 0 ‰ lights nothing, 1000 ‰ every
    /// column fully — the first and last columns' spans run out to the
    /// window's edges, so through the gutter tones the window edge to edge —
    /// a started pass lights the first column by its coverage, an unfinished
    /// one never fills the last; at every fill the lit window pixels
    /// (coverage × span) add up to `fill · W` — 50 % at the window's exact
    /// middle — with at most ONE fractional cell, and no column ever runs
    /// backwards as the fill grows.
    #[test]
    fn the_meter_maps_the_fill_onto_the_window_edge_to_edge() {
        let g = BandGeometry {
            win_w: 2000,
            cells_x: 31,
            cell_w: 17,
        };
        let cols = 114;
        let span = MeterSpan { geom: g, cols };
        assert_eq!(span.px(0), (0, 48), "column 0 carries the left gutter");
        assert_eq!(
            span.px(cols - 1),
            (31 + 113 * 17, 2000),
            "the last, the right"
        );
        assert!(
            coverage(0, cols, g).iter().all(|&f| f == 0),
            "0 % lights nothing"
        );
        assert!(
            coverage(1000, cols, g).iter().all(|&f| f == 255),
            "100 % lights every column, edge to edge"
        );
        let started = coverage(1, cols, g);
        assert!(
            started[0] > 0 && started[1] == 0,
            "a started pass shows: {:?}",
            &started[..2]
        );
        let unfinished = coverage(999, cols, g);
        assert!(
            unfinished[cols - 1] < 255,
            "only a finished pass fills the last column"
        );
        assert!(unfinished[cols - 2] == 255);
        let half = coverage(500, cols, g);
        // Column 57 starts at x = 31 + 57·17 = 1000: the window's middle.
        assert!(half[..57].iter().all(|&f| f == 255) && half[57..].iter().all(|&f| f == 0));
        let mut prev = vec![0u8; cols];
        for fill in 0..=1000u16 {
            let cov = coverage(fill, cols, g);
            let lit: f64 = cov
                .iter()
                .enumerate()
                .map(|(x, &f)| {
                    let (x0, x1) = span.px(x);
                    f64::from(f) / 255.0 * (x1 - x0) as f64
                })
                .sum();
            let want = 2000.0 * f64::from(fill) / 1000.0;
            assert!(
                (lit - want).abs() <= 0.5,
                "fill {fill}: {lit:.2} px lit, the fill is at {want} px"
            );
            assert!(
                cov.iter().filter(|&&f| f > 0 && f < 255).count() <= 1,
                "fill {fill}: one edge cell at most"
            );
            assert!(
                cov.iter().zip(&prev).all(|(now, was)| now >= was),
                "fill {fill}: the meter ran backwards"
            );
            prev = cov;
        }
        // Degenerate widths answer without a panic and inside the row.
        for cols in 1..4 {
            for fill in [0u16, 1, 499, 500, 999, 1000] {
                assert_eq!(coverage(fill, cols, g).len(), cols);
            }
        }
        // THE WINDOW, NOT THE GRID: with the grid's own mapping a 10 % fill on
        // a 20-column grid inside a 400 px window with 100 px gutters would
        // light 2 columns (40 px from the grid's edge, 140 px from the
        // window's); mapped onto the window, 10 % of 400 px is 40 px — still
        // inside the left gutter, which column 0's span carries.
        let wide_gutters = BandGeometry {
            win_w: 400,
            cells_x: 100,
            cell_w: 10,
        };
        let cov = coverage(100, 20, wide_gutters);
        assert!(cov[0] > 0 && cov[0] < 255 && cov[1..].iter().all(|&f| f == 0));
        let cov = coverage(500, 20, wide_gutters);
        assert!(
            cov[..10].iter().all(|&f| f == 255) && cov[10..].iter().all(|&f| f == 0),
            "50 % of the window is the window's middle, not the grid's"
        );
    }

    /// THE METER IS THE ROW, the PAINT: every cell of a metered row is the
    /// tone of its window-pixel span of the engine's surface ([`MeterSpan`],
    /// the fill, the track, or the edge cell's coverage) — except
    /// a Secondary chip, which wears the band's own surface so it never
    /// vanishes into the track; every glyph on it clears AA against the cell
    /// it sits on, under the default theme, one whose cursor IS its
    /// foreground, and a light one with a pale cursor; and the row's gutter
    /// tones are exactly its first and last cells' backgrounds (the
    /// renderer's no-epoch bleed relies on that).
    #[test]
    fn a_metered_row_is_all_meter_and_every_word_reads_on_it() {
        let dark = Theme::default();
        let themes = [
            ("default", dark),
            (
                "cursor is fg",
                Theme {
                    cursor: dark.fg,
                    ..dark
                },
            ),
            (
                "light, pale cursor",
                Theme {
                    fg: 0x0020_2020,
                    bg: 0x00FA_FAFA,
                    cursor: 0x00E6_E6E6,
                    ..dark
                },
            ),
        ];
        let center = two_rows();
        for (name, theme) in themes {
            let c = chrome_band::band_colors(theme);
            // 10 and 24 are the degenerate and short-capsule layouts: a chip
            // the width law put at column 0 must still be what the left gutter
            // continues (review, 2026-09-23).
            for cols in [10usize, 24, 60, 80, 140, 200] {
                let p = present(&center, cols);
                let motion = center.motion(&p, t0(), Look::STILL);
                // The unit grid, and a real window with gutters and a
                // remainder band either side.
                for g in [
                    BandGeometry::cells_only(cols),
                    BandGeometry {
                        win_w: cols * 9 + 31,
                        cells_x: 15,
                        cell_w: 9,
                    },
                ] {
                    let (rows, edges) = paint_rows_on(&p, theme, None, g, &motion);
                    for (i, (row, layout)) in rows.iter().zip(&p.rows).enumerate() {
                        let (mcol, w, _) = layout.meter.expect("both rows are metered");
                        assert_eq!((mcol, w), (0, cols), "{name}@{cols}: the meter is the row");
                        let span = MeterSpan { geom: g, cols };
                        let inks = MeterInks::of(&c, c.accent);
                        for (x, cell) in row.iter().enumerate() {
                            let secondary = layout.capsules.iter().any(|cap| {
                                cap.role == CapsuleRole::Secondary
                                    && (cap.col..cap.col + cap.width).contains(&x)
                            });
                            let want = if secondary {
                                c.bar_bg
                            } else {
                                fine_rgb(span.fine(&motion.rows[i].surface, x), &inks)
                            };
                            assert_eq!(cell.bg, want, "{name}@{cols} row {i} col {x}");
                            if cell.ch != ' ' {
                                let ratio = chrome_band::contrast(cell.fg, cell.bg);
                                assert!(
                                    ratio >= WORD_AA - 1e-9,
                                    "{name}@{cols} row {i} col {x} {:?}: {ratio:.2}:1",
                                    cell.ch
                                );
                            }
                        }
                        assert_eq!(
                            edges[i],
                            Some((row[0].bg, row[cols - 1].bg)),
                            "{name}@{cols} row {i}: the gutters continue the edge cells"
                        );
                    }
                }
            }
        }
    }

    /// A PRIMARY chip over the fill turns to the band's surface with an
    /// accent ink — the accent chip on the accent fill would be no chip at
    /// all; over the track it keeps its resting accent. An unmetered row's
    /// chips are untouched.
    #[test]
    fn a_primary_chip_over_the_fill_stays_a_chip() {
        let now = t0();
        let post = |fill: u16| {
            let mut center = MessageCenter::new(MessageLog::empty(), now);
            center.post(
                Message::new(tags::UPDATE, Severity::Info, "aterm update v0.99.0")
                    .meter(Meter {
                        fill_permille: Some(fill),
                        ..Meter::default()
                    })
                    .hold(Hold::Live {
                        stale_after: aterm_messages::STALE_UPDATE,
                    })
                    .action(Intent::NewWindow),
                stamp(),
                now,
            );
            assert_eq!(center.commit_rows(now, 3), Some(1));
            center
        };
        let theme = Theme::default();
        let c = chrome_band::band_colors(theme);
        let cols = 100;
        for (fill, over) in [(1000u16, true), (10, false)] {
            let center = post(fill);
            let p = present(&center, cols);
            let cap = p.rows[0]
                .capsules
                .iter()
                .find(|cap| cap.role == CapsuleRole::Primary)
                .expect("New window is consequential: a Primary");
            let rows = paint_still(&p, theme, None, &center);
            let cell = &rows[0][cap.col + 1];
            if over {
                assert_eq!((cell.fg, cell.bg), (c.accent, c.bar_bg), "over the fill");
            } else {
                assert_eq!((cell.fg, cell.bg), (c.bar_bg, c.accent), "over the track");
            }
        }
    }

    /// Under High Contrast a word on the fill takes `HIGHLIGHTTEXT`, the fill
    /// is `HIGHLIGHT` whatever the row's severity, and the track is `WINDOW`.
    #[test]
    fn high_contrast_meter_is_highlight_with_highlight_text_on_it() {
        let center = two_rows();
        for (name, palette) in chrome_band::hc_fixtures::STOCK {
            chrome_band::hc_fixtures::with_forced(palette, || {
                let p = present(&center, 140);
                let c = chrome_band::band_colors(Theme::default());
                let rows = paint_still(&p, Theme::default(), None, &center);
                let tcol = p.rows[0].title.0;
                let edge = usize::from(p.rows[0].meter.unwrap().2) * 140 / 1000;
                assert!(tcol < edge, "{name}: the title sits on the fill");
                assert_eq!(rows[0][tcol].bg, palette.highlight, "{name}: the fill");
                assert_eq!(rows[0][tcol].fg, c.on_accent, "{name}: HIGHLIGHTTEXT");
                assert_eq!(rows[0][139].bg, c.meter_track, "{name}: the track");
            });
        }
    }

    /// Under Windows High Contrast the capsules are `[label]` on the band
    /// with no fill, every ink WINDOWTEXT, and the hovered one HIGHLIGHT.
    #[test]
    fn high_contrast_draws_capsules_as_brackets() {
        let center = two_rows();
        for (name, palette) in chrome_band::hc_fixtures::STOCK {
            chrome_band::hc_fixtures::with_forced(palette, || {
                let p = present(&center, 140);
                let c = chrome_band::band_colors(Theme::default());
                let rows = paint_still(&p, Theme::default(), None, &center);
                for cap in &p.rows[0].capsules {
                    assert_eq!(rows[0][cap.col].ch, '[', "{name}");
                    assert_eq!(rows[0][cap.col + cap.width - 1].ch, ']', "{name}");
                    for cell in &rows[0][cap.col..cap.col + cap.width] {
                        assert_eq!(cell.bg, c.bar_bg, "{name}: no fill");
                        assert_eq!(cell.fg, c.value, "{name}: one ink");
                    }
                }
                // The glyph sits on the metered row's fill: HIGHLIGHTTEXT, the
                // pairing every word on HIGHLIGHT takes (ruling 137); off the
                // meter it is WINDOWTEXT.
                assert_eq!(
                    rows[0][GLYPH_COL].fg, c.on_accent,
                    "{name}: the glyph on the fill is HIGHLIGHTTEXT"
                );
                let first = &p.rows[0].capsules[0];
                let lit = paint_still(
                    &p,
                    Theme::default(),
                    Some(BandHover {
                        row: 0,
                        target: HoverTarget::Capsule(first.action),
                    }),
                    &center,
                );
                assert_eq!(lit[0][first.col].bg, palette.highlight, "{name}");
                assert_eq!(lit[0][first.col].ch, '[', "{name}: brackets stay");
            });
        }
    }

    /// The pixel → row/column map: the presence row first, then the band
    /// rows, the column from the cell lattice; off the chrome rows is `None`.
    #[test]
    fn band_target_for_pixel_maps_the_chrome_rows_in_order() {
        let cell = (8, 16);
        let (pad, top) = (4, 10);
        // strip 1 row, presence 1 row, band 2 rows.
        let hit = |gx, gy| band_target_for_pixel(gx, gy, cell, pad, top, 1, 1, 2);
        assert_eq!(hit(40, top + 5), None, "the strip is not the band");
        assert_eq!(hit(40, top + 16), Some(BandTarget::Presence));
        assert_eq!(hit(40, top + 31), Some(BandTarget::Presence));
        assert_eq!(
            hit(40, top + 32),
            Some(BandTarget::Row {
                row: 0,
                col: (40 - pad) / 8
            })
        );
        assert_eq!(hit(4, top + 63), Some(BandTarget::Row { row: 1, col: 0 }));
        assert_eq!(hit(40, top + 64), None, "the grid starts below the band");
        assert_eq!(hit(40, 3), None, "above the lattice");
        // No band and no presence row: nothing to hit.
        assert_eq!(
            band_target_for_pixel(40, top + 20, cell, pad, top, 1, 0, 0),
            None
        );
        // No strip (macOS): the band starts at the lattice top.
        assert_eq!(
            band_target_for_pixel(0, top, cell, pad, top, 0, 0, 1),
            Some(BandTarget::Row { row: 0, col: 0 })
        );
        // A zero cell size never divides by zero.
        assert_eq!(
            band_target_for_pixel(0, top, (0, 0), pad, top, 0, 0, 1),
            Some(BandTarget::Row { row: 0, col: 0 })
        );
    }

    /// THE APP'S OWN HIT TEST, IN PIXELS (design §7.3, `capsules_are_hit_
    /// where_they_are_painted` at the App level): through the window's real
    /// cell lattice — its pad, its strip, its cell size — every pixel of a
    /// painted capsule maps to that capsule, a press there acts and the hover
    /// lights that chip (the `Details ›` included); the body acts too — it
    /// is the Details press — and lights the `Details ›`; the overflow row
    /// is one link; one row below the band the pointer is the grid's.
    #[test]
    fn band_capsules_are_hit_at_the_pixels_they_are_painted_at() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // The strip the Windows/Linux default ships, so the band sits BELOW a
        // chrome row rather than at the lattice top.
        app.tab_strip_rows = 1;
        let id = app.post_message(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab tools")
                .line("trust — extracting")
                .action(Intent::OpenSettings {
                    route: "/packages".into(),
                })
                .hold(Hold::Live {
                    stale_after: aterm_messages::STALE_TAILED,
                }),
        );
        assert_eq!(app.message_band_rows, 1);
        let cols = usize::from(app.windows[&wid].cols);
        let layout = app.band_presentation(cols).rows[0].clone();
        assert!(matches!(layout.kind, RowKind::Message(m) if m == id));
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64
            + f64::from(app.tab_strip_rows) * ch as f64;
        let at = |col: usize| (pad + (col as f64 + 0.5) * cw as f64, top + ch as f64 * 0.5);
        assert_eq!(layout.capsules.len(), 2, "Packages, then Details \u{203a}");
        for cap in &layout.capsules {
            let lit = HoverTarget::Capsule(cap.action);
            for col in cap.col..cap.col + cap.width {
                let (x, y) = at(col);
                let target = BandTarget::Row { row: 0, col };
                assert_eq!(app.band_hit_at(wid, x, y), Some(target), "col {col}");
                assert!(app.band_press_acts(wid, target), "col {col}");
                assert_eq!(
                    app.band_hover_for(wid, target),
                    Some(BandHover {
                        row: 0,
                        target: lit
                    }),
                    "col {col}"
                );
            }
        }
        // The body: the title cell.
        let (tx, ty) = at(layout.title.0);
        let body = BandTarget::Row {
            row: 0,
            col: layout.title.0,
        };
        assert_eq!(app.band_hit_at(wid, tx, ty), Some(body));
        assert!(app.band_press_acts(wid, body));
        assert_eq!(
            app.band_hover_for(wid, body),
            Some(BandHover {
                row: 0,
                target: HoverTarget::Body
            }),
            "the body's press is the Details press, so its Details \u{203a} is the lit chip"
        );
        // The strip above and the grid below are not the band.
        assert_eq!(app.band_hit_at(wid, tx, ty - ch as f64), None);
        assert_eq!(app.band_hit_at(wid, tx, ty + ch as f64), None);
        // A capsule-less lane row: the same body law — the press opens its
        // entry and the `Details ›` it carries lights.
        app.clear_messages_for_test();
        app.post_message(
            Message::new(tags::UPDATE, Severity::Info, "aterm update v0.48.0")
                .line("downloading…")
                .hold(Hold::Live {
                    stale_after: aterm_messages::STALE_UPDATE,
                }),
        );
        let layout = app.band_presentation(cols).rows[0].clone();
        assert_eq!(layout.capsules.len(), 1, "its Details \u{203a} alone");
        let body = BandTarget::Row {
            row: 0,
            col: layout.title.0,
        };
        assert!(app.band_press_acts(wid, body));
        assert_eq!(
            app.band_hover_for(wid, body),
            Some(BandHover {
                row: 0,
                target: HoverTarget::Body
            })
        );
        // The overflow row: one link, anywhere on it.
        for i in 0..4 {
            app.post_message(Message::new(tags::CONFIG, Severity::Warn, format!("w{i}")));
        }
        assert_eq!(app.message_band_rows, aterm_messages::MAX_ROWS);
        let overflow = BandTarget::Row { row: 2, col: 4 };
        assert!(app.band_press_acts(wid, overflow));
        assert_eq!(
            app.band_hover_for(wid, overflow),
            Some(BandHover {
                row: 2,
                target: HoverTarget::Body
            }),
            "the overflow row's link lights from anywhere on it"
        );
        let (ox, oy) = (tx, ty + 2.0 * ch as f64);
        assert_eq!(
            app.band_hit_at(wid, ox, oy),
            Some(BandTarget::Row {
                row: 2,
                col: layout.title.0
            })
        );
    }

    /// DESIGN §2.2, THE END STATE: every message row ends in its `Details ›`
    /// — the row with an authored capsule and the row without — and the
    /// overflow row in its `Messages ›`, through the host's own presentation
    /// at every width; the link is the LAST capsule and its full label is
    /// the page's name for a screen reader. A message row too narrow for
    /// `Details ›` whole lets it GO rather than paint a lone `›` (review
    /// round 2, 2026-09-23): its body press performs Details.
    #[test]
    fn every_row_ends_in_its_link() {
        let mut app = App::headless_for_test();
        // The first-run toolchain row — the one row the silent lane still
        // raises — and a live update row, neither with an authored capsule.
        app.post_message(crate::toolchain_words::announced(
            "installing 10 ALab program(s) over the network (about 3 GB on disk)",
        ));
        app.post_message(
            Message::new(tags::UPDATE, Severity::Info, "aterm update v0.48.0")
                .line("downloading…")
                .hold(Hold::Live {
                    stale_after: aterm_messages::STALE_UPDATE,
                }),
        );
        for i in 0..4 {
            app.post_message(Message::new(tags::CONFIG, Severity::Warn, format!("w{i}")));
        }
        assert_eq!(app.message_band_rows, aterm_messages::MAX_ROWS);
        for cols in [40usize, 60, 80, 120, 160] {
            let p = app.band_presentation(cols);
            assert_eq!(p.rows.len(), 3, "cols {cols}");
            assert!(matches!(p.rows[2].kind, RowKind::Overflow { .. }));
            let painted = paint_still(&p, Theme::default(), None, &app.messages);
            for (r, row) in p.rows.iter().enumerate() {
                let Some(link) = row.capsules.last() else {
                    assert!(
                        r < 2 && cols <= 60,
                        "cols {cols} row {r}: only a narrow message row lets its link go"
                    );
                    let text = text_of(&painted[r]);
                    assert!(
                        !text.trim_end().ends_with('\u{203a}'),
                        "cols {cols} row {r}: never a lone \u{203a}: {text}"
                    );
                    if let RowKind::Message(id) = row.kind {
                        assert_eq!(p.hit(r, row.title.0), Hit::Body(id), "cols {cols}");
                    }
                    continue;
                };
                assert!(link.action.is_details(), "cols {cols} row {r}: last");
                if r < 2 {
                    assert_eq!(link.text, "Details \u{203a}", "whole, or gone");
                }
                assert_eq!(link.role, CapsuleRole::Details);
                assert_eq!(
                    link.full_label,
                    if r == 2 {
                        "Messages \u{203a}"
                    } else {
                        "Details \u{203a}"
                    },
                    "cols {cols} row {r}"
                );
                assert!(
                    row.capsules
                        .iter()
                        .filter(|c| c.action.is_details())
                        .count()
                        == 1,
                    "cols {cols} row {r}: one link"
                );
                let text = text_of(&painted[r]);
                assert!(
                    text.ends_with('\u{203a}'),
                    "cols {cols} row {r}: the link closes the row: {text}"
                );
            }
        }
    }

    /// The port of status_bars.rs:4991 onto the band: the Universal Control
    /// row's undo pointer keeps `` `aterm pkg doctor` `` WHOLE at every width
    /// that shows an excerpt at all, and the excerpt is either that pointer
    /// (whole, or cut on a word with the command intact) or nothing — never
    /// a mid-word stub (`prints the reve…`, review 2026-09-22). The bars had
    /// no capsule and showed the pointer from 60 cols; with `Packages` and
    /// the row's `Details ›` on it the excerpt needs 80 (design §2.3, the
    /// end state: the links are painted). Since the silent lane (the rulings
    /// on the origin/main merge, 2026-09-23) the machine-settings words are a
    /// record and never on glass; the pin stays as the width law's (§1.5),
    /// exercised on the one undo-bearing sentence the lane has, posted here
    /// at the hold the row had.
    #[test]
    fn the_universal_control_pointer_keeps_its_command_at_every_width() {
        let mut app = App::headless_for_test();
        for msg in crate::toolchain_words::machine_settings("universal-control disabled") {
            app.post_message(msg.hold(Hold::For(aterm_messages::HOLD_WARN)));
        }
        let mut shown = Vec::new();
        for cols in [60usize, 70, 80, 90, 100, 130, 150] {
            let row = app.band_presentation(cols).rows[0].clone();
            assert_eq!(
                row.title.1, "Universal Control disabled",
                "cols {cols}: whole"
            );
            let Some((_, excerpt)) = row.detail else {
                assert!(cols <= 70, "the excerpt is dropped below 80 cols only");
                continue;
            };
            shown.push(cols);
            assert!(
                excerpt.contains("`aterm pkg machine`"),
                "cols {cols}: the command survives whole: {excerpt}"
            );
            // What is shown is a run of the pointer's own words, cut (if at
            // all) at a word boundary: the pointer whole, or its tail from
            // the command with the ellipsis after a whole word.
            let pointer = "undo: `aterm pkg machine` prints the revert";
            let words = excerpt.trim_end_matches('\u{2026}');
            let at = pointer
                .find(words)
                .unwrap_or_else(|| panic!("cols {cols}: not the pointer's words: {excerpt}"));
            let rest = &pointer[at + words.len()..];
            assert!(
                rest.is_empty() || rest.starts_with(' '),
                "cols {cols}: cut inside a word: {excerpt}"
            );
        }
        assert_eq!(shown, [80, 90, 100, 130, 150]);
        let at_80 = app.band_presentation(80).rows[0].detail.clone().unwrap().1;
        assert_eq!(at_80, "`aterm pkg machine`\u{2026}");
        let at_90 = app.band_presentation(90).rows[0].detail.clone().unwrap().1;
        assert_eq!(at_90, "`aterm pkg machine` prints the\u{2026}");
        let at_100 = app.band_presentation(100).rows[0].detail.clone().unwrap().1;
        assert_eq!(at_100, "undo: `aterm pkg machine` prints the revert");
    }

    /// THE REMEDY SURVIVES THE CUT (upstream status_bars.rs
    /// `the_frozen_tab_note_keeps_its_command_at_120_cols_and_drops_whole_at_80`,
    /// re-cut onto the band). The width law is every row's; upstream pinned
    /// it on the managed words, which were a row until 2026-09-22 and are a
    /// record now (the silent lane), so they are posted here at the hold the
    /// row had. With a frozen tab the detail IS the remedy (2026-09-23: "1 tab
    /// from before this update picks them up with `…`", no claim on every
    /// tab and no builds); what survives a cut is the hook line, whole
    /// wherever the room holds it, and below that its head from the
    /// backtick, so the path is still recognisable. The band's capsules
    /// (`Packages`, `Details ›`, design §2.3) take the room the bars' detail
    /// had: none at 80 cols (the bars' pin), the whole command at 120, and the
    /// count with the whole command from 135. A count never stands without its
    /// clause (the stub-head rule, text.rs
    /// `a_stub_head_gives_way_to_the_command_whole`).
    #[test]
    fn the_frozen_tab_note_keeps_its_command_whole_wherever_it_fits() {
        let command = "`. ~/.aterm/shell.d/00-atpkg.zsh`";
        let msg = crate::toolchain_words::managed_current(
            "claude 2.1.273 (build 2026091601); codex 0.154.0 (build 2026091001)",
            1,
            true,
            crate::toolchain_words::HookDialect::Zsh,
        )
        .expect("the managed words");
        let title = msg.title.clone();
        assert!(msg.detail[0].ends_with(command), "{:?}", msg.detail);
        let mut app = App::headless_for_test();
        app.post_message(msg.hold(Hold::For(aterm_messages::HOLD_WARN)));
        let excerpt = |cols: usize| app.band_presentation(cols).rows[0].detail.clone();
        assert_eq!(excerpt(80), None, "drops whole at 80 cols");
        assert_eq!(
            excerpt(110).map(|(_, d)| d),
            Some("`. ~/.aterm/shell.d/00-atp\u{2026}".to_string()),
            "at 110 the room is narrower than the command: its head, from the backtick"
        );
        assert_eq!(
            excerpt(120).map(|(_, d)| d),
            Some(command.to_string()),
            "at 120 the command is whole"
        );
        assert_eq!(
            excerpt(143).map(|(_, d)| d),
            Some(format!("1 tab from before this\u{2026} {command}")),
            "the count and the whole command"
        );
        let mut whole_from = None;
        for cols in 40..220 {
            let row = app.band_presentation(cols).rows[0].clone();
            let Some((col, d)) = row.detail else {
                assert!(
                    whole_from.is_none(),
                    "cols {cols}: a wider row lost its excerpt"
                );
                continue;
            };
            assert_eq!(row.title.1, title, "cols {cols}: the title is whole first");
            assert!(
                col + d.chars().count() <= cols,
                "cols {cols}: the excerpt fits: {d}"
            );
            if d.contains(command) {
                whole_from.get_or_insert(cols);
            } else {
                assert!(
                    whole_from.is_none(),
                    "cols {cols}: once whole, the command stays whole: {d}"
                );
                assert!(
                    d.starts_with("`. ~/.aterm/shell.d") && d.ends_with('\u{2026}'),
                    "cols {cols}: the command's head, from the backtick: {d}"
                );
            }
            assert!(
                !d.contains("2026091601"),
                "cols {cols}: the builds are what a person can do without: {d}"
            );
            if d.contains("1 tab") {
                assert!(
                    d.contains("1 tab from before"),
                    "cols {cols}: a count never stands without its clause: {d}"
                );
            }
        }
        assert_eq!(whole_from, Some(116));
        // The shaper alone, at the caps the row reaches: sentence first, then
        // the command's own words, then the command's head.
        let long = "what `claude` and `codex` run in every aterm tab \u{00b7} 1 tab from before \
                    this update picks them up with `. ~/.aterm/shell.d/00-atpkg.zsh`";
        let shape = aterm_messages::text::shape_detail;
        assert_eq!(
            shape(long, 90),
            format!("1 tab from before this update picks them up with {command}")
        );
        assert_eq!(
            shape(long, 70),
            format!("1 tab from before this update picks\u{2026} {command}")
        );
        let floor = shape(long, 20);
        assert!(
            floor.starts_with("`. ~/.aterm/shell.d") && floor.ends_with('\u{2026}'),
            "{floor}"
        );
    }

    // ---- Motion (design §10.7, §10.12; rulings 136–142) ------------------

    /// One frame of `center`'s motion over `p` at `at` in `look`, painted on
    /// the unit grid.
    fn frame_at(
        center: &MessageCenter,
        p: &Presentation,
        at: Instant,
        look: Look,
        theme: Theme,
    ) -> (Vec<Vec<RenderCell>>, BandMotion) {
        let m = center.motion(p, at, look);
        (paint_rows(p, theme, None, &m), m)
    }

    /// A band in motion: a determinate download, BUSY work with no fraction
    /// (`Meter::busy`, ruling 139 — the engine decides motion from the
    /// meter's state, never from the hold), and a held warning below them —
    /// all three committed.
    fn moving_rows(now: Instant) -> (MessageCenter, aterm_messages::MessageId) {
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        let download = center
            .post(
                Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.91.0")
                    .meter(Meter {
                        fill_permille: Some(427),
                        stats: "31 MB / 74 MB".into(),
                        ..Meter::default()
                    })
                    .hold(Hold::Live {
                        stale_after: aterm_messages::STALE_UPDATE,
                    }),
                stamp(),
                now,
            )
            .id;
        center.post(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab tools")
                .meter(Meter::busy(""))
                .hold(Hold::Live {
                    stale_after: aterm_messages::STALE_ANNOUNCE,
                }),
            stamp(),
            now,
        );
        center.post(
            Message::new(tags::CONFIG, Severity::Warn, "Font not found").line("Nope"),
            stamp(),
            now,
        );
        assert_eq!(center.commit_rows(now, 3), Some(3));
        (center, download)
    }

    /// The frames a test walks: every 50 ms from `from` for `span`.
    fn frames(from: Instant, span: aterm_messages::Duration) -> impl Iterator<Item = Instant> {
        let steps = u64::try_from(span.as_millis() / 50).unwrap_or(0);
        (0..=steps).map(move |k| from + aterm_messages::Duration::from_millis(k * 50))
    }

    /// Whether column `x` of a row carries words a motion frame may change:
    /// the elapsed and ETA slots and the glyph cell.
    fn in_motion_span(l: &RowLayout, x: usize) -> bool {
        l.elapsed.is_some_and(|c| (c..c + ELAPSED_W).contains(&x))
            || l.eta.is_some_and(|c| (c..c + l.eta_width()).contains(&x))
            || x == aterm_messages::GLYPH_COL
    }

    /// A MOTION FRAME MOVES ONLY THE SURFACE, THE TIME SLOTS AND THE GLYPH
    /// (design §10.7; rulings 138, 140 — re-pinned from the pill's "the
    /// overlay writes only its spans" on the merge: the meter is now the whole
    /// row, so its surface may move under every cell). Across 12 s of frames —
    /// the comet's crossings, the spinner, the glint, the elapsed words
    /// arriving — and then a Complete echo, every CHARACTER a frame changes
    /// is in a time slot or the glyph cell, except on a row that is fading;
    /// no frame draws a seam (the splice seals the composed stack after it);
    /// every glyph on a moving surface clears AA on its own cell; and the
    /// held row has no motion and paints exactly its still form.
    #[test]
    fn a_motion_frame_moves_only_the_surface_the_slots_and_the_glyph() {
        let theme = Theme::default();
        let now = t0();
        let (mut center, download) = moving_rows(now);
        let check = |center: &MessageCenter, from: Instant, span| {
            let p = present(center, 120);
            let (still, _) = frame_at(center, &p, from, Look::STILL, theme);
            let mut moved = 0;
            for t in frames(from, span) {
                let (rows, m) = frame_at(center, &p, t, Look::MOVING, theme);
                assert_eq!(m.rows.len(), p.rows.len());
                for (i, (row, before)) in rows.iter().zip(&still).enumerate() {
                    assert_eq!(row.len(), before.len());
                    for (x, (a, b)) in row.iter().zip(before).enumerate() {
                        assert_eq!(a.underline, UnderlineStyle::None, "row {i} col {x}");
                        if a.bg != b.bg {
                            moved += 1;
                        }
                        if a.ch != b.ch {
                            assert!(
                                m.rows[i].fade > 0 || in_motion_span(&p.rows[i], x),
                                "row {i} col {x}: {:?} became {:?} outside the time slots at {t:?}",
                                b.ch,
                                a.ch
                            );
                        }
                        if a.ch != ' ' && m.rows[i].fade == 0 && p.rows[i].meter.is_some() {
                            let ratio = chrome_band::contrast(a.fg, a.bg);
                            assert!(ratio >= WORD_AA - 1e-9, "row {i} col {x}: {ratio:.2}:1");
                        }
                    }
                }
                // A row with no motion paints its still form, and the held row
                // has none.
                for (i, l) in p.rows.iter().enumerate() {
                    if l.full_title == "Font not found" {
                        assert_eq!(m.rows[i], RowMotion::default(), "a held row has no motion");
                    }
                    if m.rows[i] == RowMotion::default() {
                        assert_eq!(rows[i], still[i], "row {i} has no motion");
                    }
                }
            }
            moved
        };
        assert!(
            check(&center, now, aterm_messages::Duration::from_secs(12)) > 0,
            "the frames move something"
        );
        let resolved_at = now + aterm_messages::Duration::from_secs(12);
        assert!(center.resolve(download, aterm_messages::Outcome::Ok, resolved_at));
        assert!(!center.echoes().is_empty(), "the resolve left an echo");
        check(
            &center,
            resolved_at,
            aterm_messages::Duration::from_millis(900),
        );
    }

    /// A HELD ROW HAS NO MOTION AND ASKS FOR NO FRAME (FL-1 on the row): its
    /// motion fingerprint is 0 at every frame, and the engine's deadline is
    /// `None`.
    #[test]
    fn a_held_row_has_no_motion_and_asks_for_no_frame() {
        let now = t0();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        center.post(
            Message::new(tags::CONFIG, Severity::Warn, "Font not found").line("Nope"),
            stamp(),
            now,
        );
        center.commit_rows(now, 3);
        let p = present(&center, 80);
        for t in frames(now, aterm_messages::Duration::from_secs(5)) {
            let m = center.motion(&p, t, Look::MOVING);
            assert_eq!(m.fingerprint(), 0, "a held row has no motion");
        }
        assert_eq!(
            center.motion_deadline(&p, now, Look::MOVING),
            None,
            "…and asks for no frame"
        );
    }

    /// A busy row alone, committed at `now`.
    fn busy_center(now: Instant) -> MessageCenter {
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        center.post(
            Message::new(tags::UPDATE, Severity::Info, "Checking aterm v0.92.0")
                .meter(Meter::busy(""))
                .hold(Hold::Live {
                    stale_after: aterm_messages::STALE_UPDATE,
                }),
            stamp(),
            now,
        );
        assert_eq!(center.commit_rows(now, 3), Some(1));
        center
    }

    /// THE BUSY ROW DRAWS A SPINNER AND A COMET, OR HOLDS STILL — on the
    /// full-width meter's geometry (main's ruling 75, re-pinned on the
    /// engine's motion by rulings 139–140). The engine lays the track out as
    /// the whole row and computes the frame; the host paints it: moving, the
    /// braille spinner in the glyph cell — every one of its ten frames over a
    /// comet period, a step every [`aterm_messages::SPIN_FRAMES`] frames —
    /// and the comet's tones over the track; still, the row's own glyph over
    /// an unlit track. Every glyph clears AA on the cell under it at every
    /// frame, the words never move, and a row that is not busy draws the same
    /// in both looks.
    #[test]
    fn a_busy_row_draws_a_spinner_and_a_comet_or_holds_still() {
        let theme = Theme::default();
        let c = chrome_band::band_colors(theme);
        let now = t0();
        let center = busy_center(now);
        let cols = 110;
        let p = present(&center, cols);
        let layout = &p.rows[0];
        assert!(layout.busy);
        assert_eq!(layout.track, Some((0, cols)), "the track is the whole row");
        assert_eq!(layout.meter, None, "a busy row has no fill");
        let words = |row: &[RenderCell]| -> String {
            row.iter()
                .enumerate()
                .filter(|(x, _)| !in_motion_span(layout, *x))
                .map(|(_, cell)| cell.ch)
                .collect()
        };
        // Still: the row's own glyph over the unlit track.
        let (still, _) = frame_at(&center, &p, now, Look::STILL, theme);
        assert_eq!(still[0][aterm_messages::GLYPH_COL].ch, layout.glyph.1);
        assert!(
            still[0]
                .iter()
                .all(|cell| cell.bg == c.meter_track || cell.bg == c.bar_bg),
            "still: the unlit track"
        );
        // Moving: a spinner frame, and the comet somewhere on the row.
        let mut spins = std::collections::BTreeSet::new();
        let mut lit_frames = 0;
        let steps =
            aterm_messages::COMET_PERIOD.as_millis() / aterm_messages::ANIM_FRAME.as_millis();
        for k in 0..u32::try_from(steps).unwrap() {
            let t = now + aterm_messages::ANIM_FRAME * k;
            let (rows, _) = frame_at(&center, &p, t, Look::MOVING, theme);
            let g = rows[0][aterm_messages::GLYPH_COL].ch;
            assert!(aterm_messages::SPINNER.contains(&g), "frame {k}: {g:?}");
            spins.insert(g);
            if rows[0]
                .iter()
                .any(|cell| cell.bg != c.meter_track && cell.bg != c.bar_bg)
            {
                lit_frames += 1;
            }
            assert_eq!(
                words(&rows[0]),
                words(&still[0]),
                "frame {k}: the words hold"
            );
            for (x, cell) in rows[0].iter().enumerate() {
                if cell.ch != ' ' {
                    let ratio = chrome_band::contrast(cell.fg, cell.bg);
                    assert!(ratio >= WORD_AA - 1e-9, "frame {k} col {x}: {ratio:.2}:1");
                }
            }
        }
        assert_eq!(
            spins.len(),
            aterm_messages::SPINNER.len(),
            "every spinner frame"
        );
        assert!(
            lit_frames * 10 >= u32::try_from(steps).unwrap() * 9,
            "the comet is on the row nearly every frame: {lit_frames} of {steps}"
        );
        // A row that is NOT busy — a held one — draws the same in both looks.
        let mut held = MessageCenter::new(MessageLog::empty(), now);
        held.post(
            Message::new(tags::CONFIG, Severity::Warn, "Font not found"),
            stamp(),
            now,
        );
        held.commit_rows(now, 3);
        let hp = present(&held, cols);
        assert_eq!(
            frame_at(&held, &hp, now, Look::MOVING, theme).0,
            frame_at(&held, &hp, now, Look::STILL, theme).0
        );
    }

    /// THE COMET NEVER FLIPS A WORD'S INK (visual review of the merged band,
    /// 2026-09-24): over one comet period, on a dark band and a light one,
    /// every tone the comet draws leaves the row's words' anchor AA from it,
    /// and every word — and the spinner — stays on that anchor's side of the
    /// ground under it, at least as far toward the anchor as it rests on the
    /// track. The full accent under light ink flipped each letter it crossed
    /// to black and back; the dark spinner split the entering comet in two.
    #[test]
    fn the_comet_never_flips_a_words_ink() {
        let light = Theme {
            fg: 0x001F_2328,
            bg: 0x00FF_FFFF,
            cursor: 0x0009_69DA,
            selection: 0x00DD_F4FF,
        };
        for theme in [Theme::default(), light] {
            let c = chrome_band::band_colors(theme);
            let anchor = words_anchor(&c);
            let toward = |ink: [u8; 3]| {
                if anchor == WHITE {
                    luminance(ink)
                } else {
                    -luminance(ink)
                }
            };
            let now = t0();
            let center = busy_center(now);
            let cols = 110;
            let p = present(&center, cols);
            let (still, _) = frame_at(&center, &p, now, Look::STILL, theme);
            let steps =
                aterm_messages::COMET_PERIOD.as_millis() / aterm_messages::ANIM_FRAME.as_millis();
            let mut lit = 0;
            for k in 0..u32::try_from(steps).unwrap() {
                let t = now + aterm_messages::ANIM_FRAME * k;
                let (rows, _) = frame_at(&center, &p, t, Look::MOVING, theme);
                for (x, (cell, rest)) in rows[0].iter().zip(&still[0]).enumerate() {
                    assert!(
                        chrome_band::contrast(anchor, cell.bg) >= WORD_AA - 1e-9,
                        "frame {k} col {x}: the comet left its words' side: {:?}",
                        cell.bg
                    );
                    if cell.bg != rest.bg {
                        lit += 1;
                    }
                    if cell.ch == ' ' {
                        continue;
                    }
                    assert!(
                        toward(cell.fg) > toward(cell.bg),
                        "frame {k} col {x} {:?}: the ink flipped side",
                        cell.ch
                    );
                    assert!(
                        chrome_band::contrast(cell.fg, cell.bg) >= WORD_AA - 1e-9,
                        "frame {k} col {x} {:?}",
                        cell.ch
                    );
                    if x != aterm_messages::GLYPH_COL {
                        assert!(
                            toward(cell.fg) >= toward(rest.fg) - 1e-9,
                            "frame {k} col {x} {:?}: the ink moved away from its anchor",
                            cell.ch
                        );
                    }
                }
            }
            assert!(lit > 0, "the comet drew");
        }
    }

    /// THE COMET SWEEPS THE WINDOW EDGE TO EDGE, SOFTLY (rulings 55, 138):
    /// mapped onto real 2000 px and 1000 px windows through [`MeterSpan`], the comet
    /// lights the first column (the left gutter's) as it enters and the last
    /// (the right gutter's) as it leaves; while it is wholly inside, it is
    /// about a fifth of the WINDOW wide ([`aterm_messages::COMET_PERMILLE`]);
    /// and it has no straight edge anywhere — no two neighbouring cells step
    /// from full to empty.
    #[test]
    fn the_comet_sweeps_the_window_edge_to_edge() {
        for (win_w, cols, cells_x, cell_w) in
            [(2000usize, 114usize, 31usize, 17usize), (1000, 80, 20, 12)]
        {
            sweep_edge_to_edge(
                BandGeometry {
                    win_w,
                    cells_x,
                    cell_w,
                },
                cols,
            );
        }
    }

    /// [`the_comet_sweeps_the_window_edge_to_edge`] on one window.
    fn sweep_edge_to_edge(g: BandGeometry, cols: usize) {
        let span = MeterSpan { geom: g, cols };
        let (mut first, mut last) = (false, false);
        let period = aterm_messages::COMET_PERIOD;
        let steps = period.as_millis() / aterm_messages::ANIM_FRAME.as_millis();
        for k in 0..u32::try_from(steps).unwrap() {
            let since = aterm_messages::ANIM_FRAME * k;
            let surface = aterm_messages::animate::comet(since, true);
            let fill: Vec<u8> = (0..cols).map(|x| span.tone(&surface, x).fill).collect();
            first |= fill[0] > 0;
            last |= fill[cols - 1] > 0;
            for (x, pair) in fill.windows(2).enumerate() {
                assert!(
                    pair[0].abs_diff(pair[1]) <= 160,
                    "frame {k}: a hard edge at col {x}: {pair:?}"
                );
            }
            if fill[0] == 0 && fill[cols - 1] == 0 && fill.iter().any(|&f| f > 0) {
                let lit = fill.iter().filter(|&&f| f > 0).count();
                let fifth = cols as f64 * f64::from(aterm_messages::COMET_PERMILLE) / 1000.0;
                assert!(
                    (lit as f64) >= fifth * 0.8 && (lit as f64) <= fifth * 1.4,
                    "frame {k}: {lit} cells lit, a fifth of the window is {fifth:.1}"
                );
            }
        }
        assert!(first && last, "it enters and leaves through the gutters");
    }

    /// THE COMET ENTERS WITHOUT A STRAIGHT EDGE (review round 3, 2026-09-24,
    /// re-pinned on whole-cell tones by ruling 138): through its first frames
    /// the first column — the left gutter's — takes the entering head's
    /// COVERAGE, rising through intermediate tones frame by frame rather than
    /// switching on, and it never lights before the head has entered.
    #[test]
    fn the_comet_enters_without_a_straight_edge() {
        for (win_w, cols, cells_x, cell_w) in
            [(2000usize, 114usize, 31usize, 17usize), (1000, 80, 20, 12)]
        {
            let g = BandGeometry {
                win_w,
                cells_x,
                cell_w,
            };
            let span = MeterSpan { geom: g, cols };
            for base in [0u32, 1, 2] {
                let mut seen = Vec::new();
                for k in 0..24u32 {
                    let since =
                        aterm_messages::COMET_PERIOD * base + aterm_messages::ANIM_FRAME * k;
                    let surface = aterm_messages::animate::comet(since, true);
                    seen.push(span.tone(&surface, 0).fill);
                }
                // The head is a peak, not a plateau: the first column's mean
                // rises while the head crosses it and falls as the tail
                // follows, never reaching the full fill (the column is wider
                // than the head). Entering is the run up to that peak.
                let first = seen.iter().position(|&f| f > 0).unwrap_or(seen.len());
                let peak = (first..seen.len())
                    .max_by_key(|&k| seen[k])
                    .unwrap_or(first);
                let rising = &seen[first..=peak.min(seen.len() - 1)];
                assert!(
                    rising.len() >= 3 && rising[0] < 128,
                    "{win_w} px crossing {base}: the first column switched on: {seen:?}"
                );
                assert!(
                    rising.windows(2).all(|w| w[1] >= w[0]),
                    "{win_w} px crossing {base}: the entering head ran backwards: {seen:?}"
                );
                assert!(
                    seen[peak..].windows(2).all(|w| w[1] <= w[0]),
                    "{win_w} px crossing {base}: the tail left the first column unevenly: {seen:?}"
                );
            }
        }
    }

    /// THE TIME SLOT'S WORDS WEAR THEIR MEANING'S INK (review round 3,
    /// 2026-09-24): a determinate row whose estimate is hidden shows its
    /// elapsed CLOCK in the ETA slot in the label ink, a latched estimate
    /// (`… left`) in the value ink, and its Complete echo says `done` in the
    /// ink its ✓ wears — the accent's family, never warn (`stalled` and
    /// `failed` keep warn). On the full-row meter every one of those inks is
    /// floored against the cell under it (ruling 55's AA floor), so the echo's
    /// `done` and ✓, both on the completing fill, wear the same floored ink.
    #[test]
    fn the_time_slot_words_wear_their_meanings_ink() {
        let theme = Theme::default();
        let c = chrome_band::band_colors(theme);
        let download = |done: u64| {
            Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.91.0")
                .meter(Meter {
                    fill_permille: None,
                    stats: "31 MB / 74 MB".into(),
                    amount: Some(aterm_messages::Amount {
                        series: aterm_messages::Amount::series_of("aterm 0.91.0"),
                        done,
                        total: 74_000_000,
                        unit: aterm_messages::Unit::Bytes,
                    }),
                    ..Meter::default()
                })
                .hold(Hold::Live {
                    stale_after: aterm_messages::STALE_UPDATE,
                })
                .key("update.progress")
        };
        let slot = |center: &MessageCenter, at: Instant| {
            let p = present(center, 120);
            let col = p.rows[0].eta.expect("the ETA slot at 120");
            let (rows, _) = frame_at(center, &p, at, Look::MOVING, theme);
            let words: String = rows[0][col..col + p.rows[0].eta_width()]
                .iter()
                .map(|cell| cell.ch)
                .collect();
            (
                words.trim_end().to_string(),
                rows[0][col].fg,
                rows[0][col].bg,
                rows[0][aterm_messages::GLYPH_COL].fg,
            )
        };
        let floored = |ink: [u8; 3], bg: [u8; 3]| {
            if bg == c.bar_bg {
                ink
            } else if bg == c.meter_track {
                floor_word(ink, bg, words_anchor(&c))
            } else {
                floor_word(ink, bg, fill_anchor(c.accent))
            }
        };
        let now = t0();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        let id = center.post(download(10_000_000), stamp(), now).id;
        center.commit_rows(now, 3);
        let (words, ink, bg, _) = slot(&center, now + aterm_messages::Duration::from_millis(4200));
        assert_eq!(
            (words.as_str(), ink),
            ("0:04", floored(c.label, bg)),
            "the clock, hidden ETA"
        );
        // Fed steadily, the estimate latches: `… left` in the value ink.
        let mut t = now;
        for k in 1..=16u64 {
            t = now + aterm_messages::Duration::from_millis(500 * k);
            center.restate(
                id,
                aterm_messages::Restatement {
                    meter: Some(download(10_000_000 + 2_000_000 * k).meter),
                    ..aterm_messages::Restatement::default()
                },
                t,
            );
        }
        let (words, ink, bg, _) = slot(&center, t);
        assert!(words.ends_with(" left"), "latched: {words:?}");
        assert_eq!(ink, floored(c.value, bg));
        // Delivered: `done` under the ✓, in the ✓'s ink.
        assert!(center.resolve(id, aterm_messages::Outcome::Ok, t));
        let (words, ink, bg, check) = slot(&center, t + aterm_messages::Duration::from_millis(100));
        assert_eq!(words, DONE_WORD);
        assert_eq!(ink, floored(c.accent, bg), "done wears the accent, floored");
        assert_eq!(ink, check, "done wears the check's ink");
        assert_ne!(ink, c.warn);
    }

    /// HIGH CONTRAST MOTION USES ONLY PALETTE INKS (design §10.9, ruling 137):
    /// under every stock High Contrast palette, in the flat look, every frame
    /// of a moving band — the comet, the bar, the echo — paints every cell's
    /// ground in the band, the WINDOW track or the HIGHLIGHT fill, never a
    /// mix between them, and the fill is HIGHLIGHT on the Warn row too.
    #[test]
    fn high_contrast_motion_uses_only_palette_inks() {
        let flat = Look {
            pace: aterm_messages::Pace::Moving,
            graded: false,
        };
        for (name, palette) in chrome_band::hc_fixtures::STOCK {
            chrome_band::hc_fixtures::with_forced(palette, || {
                let theme = Theme::default();
                let c = chrome_band::band_colors(theme);
                let allowed = [c.bar_bg, c.meter_track, c.accent, palette.highlight];
                let now = t0();
                let (mut center, download) = moving_rows(now);
                let check = |center: &MessageCenter, from: Instant, span| {
                    let p = present(center, 120);
                    for t in frames(from, span) {
                        let (rows, _) = frame_at(center, &p, t, flat, theme);
                        for (i, row) in rows.iter().enumerate() {
                            for (x, cell) in row.iter().enumerate() {
                                assert!(
                                    allowed.contains(&cell.bg),
                                    "{name} row {i} col {x}: {:?} is not a palette ground",
                                    cell.bg
                                );
                            }
                        }
                    }
                };
                check(&center, now, aterm_messages::Duration::from_secs(4));
                let at = now + aterm_messages::Duration::from_secs(4);
                assert!(center.resolve(download, aterm_messages::Outcome::Ok, at));
                check(&center, at, aterm_messages::Duration::from_millis(900));
            });
        }
    }

    /// THE BAND'S REPAINT KEY (FL-1; rulings 55, 140): exactly `0` with no row
    /// committed, whatever the hover, the geometry or the motion term;
    /// otherwise nonzero and moved by each of them — a hover change, a resize
    /// that keeps the column count (the meter is mapped onto the WINDOW), and
    /// a motion frame that draws something new — and the same inputs are the
    /// same key. A band of held rows only has a motion fingerprint of exactly
    /// 0 in both looks; a busy row's STILL frame does not move with time, and
    /// its MOVING frames do.
    #[test]
    fn band_fp_is_zero_with_no_row_and_moves_only_with_what_it_draws() {
        let g = BandGeometry::cells_only(100);
        let wide = BandGeometry {
            win_w: 1000,
            cells_x: 20,
            cell_w: 9,
        };
        let hovers = [
            None,
            Some(BandHover {
                row: 0,
                target: HoverTarget::Body,
            }),
            Some(BandHover {
                row: 2,
                target: HoverTarget::Capsule(aterm_messages::ActionIndex(1)),
            }),
        ];
        for hover in hovers {
            for geom in [g, wide] {
                for motion in [0u64, 0x1234] {
                    assert_eq!(band_fp(0, hover, geom, motion), 0, "no row, no key");
                }
            }
        }
        let fp = 0x1234_5678_u64;
        let key = band_fp(fp, None, g, 0);
        assert_ne!(key, 0);
        assert_eq!(
            key,
            band_fp(fp, None, g, 0),
            "the same inputs, the same key"
        );
        assert_ne!(key, band_fp(fp, hovers[1], g, 0), "a hover moves it");
        assert_ne!(key, band_fp(fp, None, wide, 0), "the window moves it");
        assert_ne!(key, band_fp(fp, None, g, 0x1234), "a frame moves it");
        assert_ne!(band_fp(fp, None, g, 0x1234), band_fp(fp, None, g, 0x5678));
        // Held rows only: no motion term, in either look.
        let now = t0();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        center.post(
            Message::new(tags::CONFIG, Severity::Warn, "Font not found").line("Nope"),
            stamp(),
            now,
        );
        center.commit_rows(now, 3);
        let p = present(&center, 100);
        for look in [Look::MOVING, Look::STILL] {
            assert_eq!(center.motion(&p, now, look).fingerprint(), 0);
        }
        // A busy row: still, the frame holds; moving, it moves.
        let center = busy_center(now);
        let p = present(&center, 100);
        let at = |k: u32, look| {
            center
                .motion(&p, now + aterm_messages::ANIM_FRAME * k, look)
                .fingerprint()
        };
        assert_ne!(at(0, Look::STILL), 0, "the still track is drawn");
        assert_eq!(
            at(0, Look::STILL),
            at(90, Look::STILL),
            "still: no motion term moves"
        );
        assert_ne!(
            at(0, Look::MOVING),
            at(aterm_messages::SPIN_FRAMES, Look::MOVING),
            "moving: the frames move the key"
        );
    }

    /// Every builtin scheme as a theme, `Default` first.
    fn builtin_themes() -> Vec<(&'static str, Theme)> {
        aterm_types::scheme::builtin_names()
            .into_iter()
            .map(|name| {
                let parts = aterm_types::scheme::builtin(name)
                    .expect("a listed scheme")
                    .to_theme_parts();
                (
                    name,
                    Theme {
                        fg: parts.fg,
                        bg: parts.bg,
                        cursor: parts.cursor,
                        selection: parts.selection,
                    },
                )
            })
            .collect()
    }

    /// EVERY CHIP LABEL CLEARS AA ON ITS OWN CELL (design ruling 155): on
    /// every builtin scheme, a Primary, a Secondary and `Details ›` — resting
    /// and lit — on the plain band, over a meter's track and over its fill,
    /// and on a busy row, each label glyph clears 4.5:1 against the cell it
    /// sits on. The Primary measured 3.69:1 on Solarized Light and 4.28:1 on
    /// GitHub Light before; it keeps its polarity (the band's ink on a
    /// deepened accent), so no label turned black on a blue chip.
    #[test]
    fn every_chip_label_clears_aa_on_every_builtin_theme() {
        let now = t0();
        let actions = |m: Message| {
            m.action(Intent::NewWindow).action(Intent::OpenSettings {
                route: "/packages".into(),
            })
        };
        let live = |meter: Meter| {
            actions(
                Message::new(tags::UPDATE, Severity::Info, "Work")
                    .meter(meter)
                    .hold(Hold::Live {
                        stale_after: aterm_messages::STALE_UPDATE,
                    }),
            )
        };
        let filled = |p: u16| {
            live(Meter {
                fill_permille: Some(p),
                ..Meter::default()
            })
        };
        let rows = [
            (
                "band",
                actions(Message::new(tags::CONFIG, Severity::Warn, "Held")),
            ),
            ("track", filled(20)),
            ("fill", filled(1000)),
            ("busy", live(Meter::busy(""))),
        ];
        for (name, theme) in builtin_themes() {
            let c = chrome_band::band_colors(theme);
            let primary = keep_side(c.accent, c.bar_bg, WORD_AA);
            if chrome_band::contrast(c.accent, c.bar_bg) >= WORD_AA {
                assert_eq!(primary, c.accent, "{name}: an accent at AA is untouched");
            } else if oklch(c.accent).1 > 2.0 * HUELESS {
                let (h0, h1) = (oklch(c.accent).2, oklch(primary).2);
                let turn = (h1 - h0 + 540.0).rem_euclid(360.0) - 180.0;
                assert!(
                    turn.abs() < 3.0,
                    "{name}: the deepened accent keeps its hue"
                );
            }
            // …and the LIT Primary is a whole lift off the rest it wears
            // (ruling 20, kept by ruling 160): at least `PRIMARY_HOVER_LIFT`
            // of the way from the worn accent to the theme's ink on every
            // channel, up to rounding — the AA deepening only carries it on.
            let rest = chip_inks(&c, CapsuleRole::Primary, false, false, false).1;
            let lit = chip_inks(&c, CapsuleRole::Primary, true, false, false).1;
            assert_eq!(
                rest, primary,
                "{name}: the resting Primary wears the AA accent"
            );
            if c.accent != c.primary_lift_toward {
                let want =
                    chrome_band::mix3(rest, c.primary_lift_toward, chrome_band::PRIMARY_HOVER_LIFT);
                for k in 0..3 {
                    let (a, f) = (f64::from(rest[k]), f64::from(c.primary_lift_toward[k]));
                    let toward = (f - a).signum();
                    assert!(
                        (f64::from(lit[k]) - a) * toward + 1.0 >= (f64::from(want[k]) - a) * toward,
                        "{name}: channel {k} of the lit Primary {lit:?} is short of a whole \
                         lift from the rest {rest:?} toward {:?}",
                        c.primary_lift_toward
                    );
                }
            }
            for (ground, msg) in &rows {
                let mut center = MessageCenter::new(MessageLog::empty(), now);
                center.post(msg.clone(), stamp(), now);
                center.commit_rows(now, 3);
                let p = present(&center, 120);
                let caps = &p.rows[0].capsules;
                assert_eq!(caps.len(), 3, "{name} {ground}: {caps:?}");
                let hovers = std::iter::once(None).chain(caps.iter().map(|cap| {
                    Some(BandHover {
                        row: 0,
                        target: HoverTarget::Capsule(cap.action),
                    })
                }));
                for hover in hovers {
                    let painted = paint_still(&p, theme, hover, &center);
                    for cap in caps {
                        for cell in &painted[0][cap.col..cap.col + cap.width] {
                            if cell.ch == ' ' {
                                continue;
                            }
                            let ratio = chrome_band::contrast(cell.fg, cell.bg);
                            assert!(
                                ratio >= WORD_AA - 1e-9,
                                "{name} {ground} {:?} {:?} hover {hover:?}: {:?} on {:?} is \
                                 {ratio:.2}:1",
                                cap.role,
                                cell.ch,
                                cell.fg,
                                cell.bg
                            );
                        }
                    }
                }
            }
        }
    }

    /// THE FAULT ECHO WARMS STRAIGHT TO THE FAULT HUE (design ruling 156):
    /// on every builtin scheme and every stock High Contrast palette, for a
    /// determinate bar and a busy row, in the moving, still and flat looks,
    /// the cell's colour goes from the row's last frame toward the fault hue
    /// with no pale grey between — the old two-leg mix drained to its own
    /// grey on the first frame — and ends on it.
    #[test]
    fn the_fault_echo_warms_straight_to_the_fault_hue() {
        let flat = Look {
            pace: aterm_messages::Pace::Moving,
            graded: false,
        };
        let run = |name: &str, theme: Theme| {
            for busy in [false, true] {
                for look in [Look::MOVING, Look::STILL, flat] {
                    let now = t0();
                    let mut center = MessageCenter::new(MessageLog::empty(), now);
                    let meter = if busy {
                        Meter::busy("")
                    } else {
                        Meter {
                            fill_permille: Some(600),
                            ..Meter::default()
                        }
                    };
                    let id = center
                        .post(
                            Message::new(tags::UPDATE, Severity::Info, "Work")
                                .meter(meter)
                                .hold(Hold::Live {
                                    stale_after: aterm_messages::STALE_UPDATE,
                                }),
                            stamp(),
                            now,
                        )
                        .id;
                    center.commit_rows(now, 3);
                    // A cell on the fill (or, busy, where the comet is not).
                    let cols = 120;
                    let x = if busy { 110 } else { 40 };
                    let at = now + aterm_messages::Duration::from_millis(1500);
                    let p = present(&center, cols);
                    let mut seq = vec![frame_at(&center, &p, at, look, theme).0[0][x].bg];
                    assert!(center.resolve(id, aterm_messages::Outcome::Warn, at));
                    let p = present(&center, cols);
                    let flash = aterm_messages::ECHO_FAULT_FLASH;
                    let mut t = aterm_messages::Duration::ZERO;
                    while t <= flash {
                        seq.push(frame_at(&center, &p, at + t, look, theme).0[0][x].bg);
                        t += aterm_messages::Duration::from_millis(11);
                    }
                    if let Some(why) = warms_straight(&seq) {
                        panic!("{name} busy={busy} {look:?}: {why}");
                    }
                }
            }
        };
        for (name, theme) in builtin_themes() {
            run(name, theme);
        }
        for (name, palette) in chrome_band::hc_fixtures::STOCK {
            chrome_band::hc_fixtures::with_forced(palette, || run(name, Theme::default()));
        }
    }

    /// THE WORDS ON A DETERMINATE BAR NEVER JUMP (design ruling 158): on
    /// every builtin scheme, through the glint's travel over a bar at 57 %,
    /// the Complete echo's wipe and bloom from 80 % and the Fault echo's
    /// flash from 40 %, every letter of the title keeps its side of the
    /// ground under it (the glint and the bloom are held there), clears AA,
    /// and moves its ink by no more than the ground under it moved. A Fault
    /// echo may cross ONCE, where the fault hue wants the other ink (Solarized
    /// Light's slate to its amber: neither black nor white reads on both) —
    /// the old
    /// ten-step floor jumped a whole step on a one-level change in the
    /// bloom, and the glint flipped Catppuccin Latte's letters to white.
    #[test]
    fn the_words_on_a_determinate_bar_never_jump() {
        use aterm_messages::{
            ANIM_FRAME, Duration, ECHO_FAULT_FLASH, ECHO_FILL, ECHO_GLOW, GLINT_DELAY,
            GLINT_TRAVEL, Outcome,
        };
        let cols = 120;
        let run = |name: &str, theme: Theme, permille: u16, outcome: Option<Outcome>| {
            let now = t0();
            let mut center = MessageCenter::new(MessageLog::empty(), now);
            let id = center
                .post(
                    Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.91.0")
                        .no_excerpt()
                        .meter(Meter {
                            fill_permille: Some(permille),
                            ..Meter::default()
                        })
                        .hold(Hold::Live {
                            stale_after: aterm_messages::STALE_UPDATE,
                        }),
                    stamp(),
                    now,
                )
                .id;
            center.commit_rows(now, 3);
            let (from, span) = match outcome {
                None => (now + GLINT_DELAY, GLINT_TRAVEL),
                Some(o) => {
                    let at = now + Duration::from_millis(1000);
                    assert!(center.resolve(id, o, at));
                    let span = if o == Outcome::Ok {
                        ECHO_FILL + ECHO_GLOW
                    } else {
                        ECHO_FAULT_FLASH
                    };
                    (at, span)
                }
            };
            let p = present(&center, cols);
            let (tcol, title) = p.rows[0].title.clone();
            let letters: Vec<usize> = title
                .chars()
                .enumerate()
                .filter(|(_, ch)| !ch.is_whitespace())
                .map(|(k, _)| tcol + k)
                .collect();
            let mut last: Option<Vec<RenderCell>> = None;
            let mut crossed = vec![0u32; cols];
            let crossings = u32::from(outcome == Some(Outcome::Warn));
            let mut t = Duration::ZERO;
            while t <= span {
                let row = frame_at(&center, &p, from + t, Look::MOVING, theme).0[0].clone();
                for &x in &letters {
                    let (fg, bg) = (row[x].fg, row[x].bg);
                    assert!(
                        chrome_band::contrast(fg, bg) >= WORD_AA - 0.01,
                        "{name} {outcome:?} t={t:?} col {x}: {fg:?} on {bg:?} under AA"
                    );
                    let Some(prev) = &last else { continue };
                    let (pfg, pbg) = (prev[x].fg, prev[x].bg);
                    let lighter = |a: [u8; 3], b: [u8; 3]| luminance(a) > luminance(b);
                    if lighter(fg, bg) != lighter(pfg, pbg) {
                        crossed[x] += 1;
                        assert!(
                            crossed[x] <= crossings,
                            "{name} {outcome:?} t={t:?} col {x}: the letter changed side, \
                             {pfg:?} on {pbg:?} -> {fg:?} on {bg:?}"
                        );
                        continue;
                    }
                    let moved = |a: [u8; 3], b: [u8; 3]| {
                        (0..3).map(|k| a[k].abs_diff(b[k])).max().unwrap_or(0)
                    };
                    assert!(
                        moved(fg, pfg) <= moved(bg, pbg).saturating_mul(2) + 2,
                        "{name} {outcome:?} t={t:?} col {x}: the ink jumped \
                         {pfg:?} -> {fg:?} while its ground moved {pbg:?} -> {bg:?}"
                    );
                }
                last = Some(row);
                t += ANIM_FRAME;
            }
        };
        for (name, theme) in builtin_themes() {
            run(name, theme, 570, None);
            run(name, theme, 800, Some(Outcome::Ok));
            run(name, theme, 400, Some(Outcome::Warn));
        }
    }

    /// A FAULT ECHO'S EDGE CELL IS ITS COVERAGE (design ruling 158): the cell
    /// the fill only partly covers is the WARMED fill over the track by that
    /// coverage, on the warmed fill's hue — not the track-and-fill mix sent
    /// round the hue circle on its own path (a red cell at the end of an
    /// amber bar on GitHub Light's blue accent).
    #[test]
    fn a_fault_echos_edge_cell_is_the_warmed_fill_by_its_coverage() {
        for (name, theme) in builtin_themes() {
            let c = chrome_band::band_colors(theme);
            let inks = MeterInks::of(&c, c.accent);
            for warn in [64u16, 160, 255] {
                let full = fine_rgb(
                    aterm_messages::FineTone {
                        fill: 255 << 8,
                        lift: 0,
                        warn: warn << 8,
                    },
                    &inks,
                );
                // A third of the cell lit: fill and warn both at a third.
                let edge = fine_rgb(
                    aterm_messages::FineTone {
                        fill: (255 << 8) / 3,
                        lift: 0,
                        warn: (warn << 8) / 3,
                    },
                    &inks,
                );
                let expect = [0, 1, 2].map(|k| {
                    let under = f64::from(c.meter_track[k]);
                    (f64::from(full[k]) - under)
                        .mul_add(1.0 / 3.0, under)
                        .round() as u8
                });
                let off = (0..3)
                    .map(|k| edge[k].abs_diff(expect[k]))
                    .max()
                    .unwrap_or(0);
                assert!(
                    off <= 2,
                    "{name} warn {warn}: edge {edge:?}, the warmed fill {full:?} by a third \
                     over {:?} is {expect:?}",
                    c.meter_track
                );
            }
        }
    }

    /// THE COMET'S TAIL IS A SMOOTH MONOTONE FALL-OFF (design ruling 157):
    /// on every builtin scheme, at 60/80/120/160 columns on whole-cell and on
    /// non-integral windows (gutters and a remainder band), every frame of a
    /// crossing rises cell by cell to one head and falls cell by cell past
    /// it — in every channel — with no neighbouring step over a bound (the
    /// tail's coverage a fifth of the fill, the lead's half), and no zigzag
    /// in the steps along the tail (a step never dips more than one level
    /// below both its neighbours). Eight chords to the tail's curve and a
    /// byte-rounded tone per cell put steps in pairs and alternated them by a
    /// level — the faint vertical stripes at capture scale.
    #[test]
    fn the_comet_tail_is_a_smooth_monotone_fall_off() {
        let now = t0();
        let center = busy_center(now);
        let steps =
            aterm_messages::COMET_PERIOD.as_millis() / aterm_messages::ANIM_FRAME.as_millis();
        let mut worst_zig = 0i32;
        let mut worst = [0.0f64; 2];
        for (name, theme) in builtin_themes() {
            let c = chrome_band::band_colors(theme);
            let (inks, _) = MeterInks::comet(&c, c.accent);
            for (cols, pad, extra) in [
                (60usize, 0usize, 0usize),
                (80, 0, 0),
                (120, 0, 0),
                (160, 0, 0),
                (80, 10, 7),
                (120, 8, 11),
                (60, 13, 5),
            ] {
                let cell_w = 12;
                let geom = BandGeometry {
                    win_w: 2 * pad + cols * cell_w + extra,
                    cells_x: pad + extra / 2,
                    cell_w,
                };
                let p = present(&center, cols);
                let span = MeterSpan { geom, cols };
                for k in 0..u32::try_from(steps).unwrap() {
                    let t = now + aterm_messages::ANIM_FRAME * k;
                    let m = center.motion(&p, t, Look::MOVING);
                    // The coverage steps between neighbouring cells, off the
                    // gutter columns: the tail's rise and the lead's fall.
                    let cover: Vec<f64> = (1..cols - 1)
                        .map(|x| {
                            let f = span.fine(&m.rows[0].surface, x);
                            let l = f64::from(f.lift) / 65_280.0;
                            let f = f64::from(f.fill) / 65_280.0;
                            f + (1.0 - f) * l
                        })
                        .collect();
                    let top = (0..cover.len())
                        .max_by(|&a, &b| cover[a].total_cmp(&cover[b]))
                        .unwrap_or(0);
                    for (x, w) in cover.windows(2).enumerate() {
                        let side = usize::from(x >= top);
                        worst[side] = worst[side].max((w[1] - w[0]).abs());
                    }
                    let (rows, _) = paint_rows_on(&p, theme, None, geom, &m);
                    let bg: Vec<[u8; 3]> = rows[0].iter().map(|cell| cell.bg).collect();
                    for ch in 0..3 {
                        let toward = |v: u8| {
                            let (t0, f) = (i32::from(inks.track[ch]), i32::from(inks.fill[ch]));
                            (i32::from(v) - t0) * (f - t0).signum()
                        };
                        let v: Vec<i32> = bg.iter().map(|c| toward(c[ch])).collect();
                        let Some(peak) = (0..cols).max_by_key(|&x| v[x]) else {
                            continue;
                        };
                        let rise = &v[..=peak];
                        let fall = &v[peak..];
                        assert!(
                            rise.windows(2).all(|w| w[1] >= w[0])
                                && fall.windows(2).all(|w| w[1] <= w[0]),
                            "{name} {cols}+{pad}/{extra} frame {k} channel {ch}: not one \
                             rise and one fall: {v:?}"
                        );

                        // The edge columns stand for their gutters too
                        // (wider spans, [`MeterSpan`]): their steps are the
                        // gutters', not the tail's texture.
                        let inner = &v[1..cols - 1];
                        let top = peak.clamp(1, cols - 2) - 1;
                        let d: Vec<i32> = inner[..=top].windows(2).map(|w| w[1] - w[0]).collect();
                        for (x, w) in d.windows(3).enumerate() {
                            let zig = w[0].min(w[2]) - w[1];
                            worst_zig = worst_zig.max(zig);
                            assert!(
                                zig <= 1,
                                "{name} {cols}+{pad}/{extra} frame {k} channel {ch} col {x}: \
                                 the steps zigzag {w:?}: {v:?}"
                            );
                        }
                    }
                }
            }
        }
        // Neither a stripe nor an edge: the tail's rise never steps more than
        // a fifth of the fill between neighbouring cells (60 columns is the
        // worst, 0.185), and the soft lead's fall never more than half.
        assert!(worst_zig <= 1);
        assert!(
            worst[0] <= 0.20,
            "the tail steps {:.3} between cells",
            worst[0]
        );
        assert!(
            worst[1] <= 0.50,
            "the lead steps {:.3} between cells",
            worst[1]
        );
    }
}
