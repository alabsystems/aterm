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
//!     ␠ G ␠ TITLE(bold) ␠·␠ excerpt (label) …  42% 35 s left  stats …  disk busy  ␠␠ ▐cap▌␠▐Details ›▌ ␠
//!     ███████████████████████████▓░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  ← a metered row's surface
//! ```
//!
//! Glyph at col 1 in `accent` (Success/Info) or `warn` (Warn/Error) — the
//! row's own, or an echo's ✓ / ⚠ (a moving comet no longer spins a braille
//! frame beside it, ruling 251) — DRAWN as one of the band's icons through
//! the row's pixel raster ([`RowRaster::icons`], ruling 251: the owner's
//! "Drawn icons, drop spinner"), the character kept in the cell for the text
//! grid, a screen reader, copy and the `messages` verb; text presentation on
//! purpose (`⚠` and `✓` have emoji forms, and
//! a colour emoji in a chrome row would be two cells wide in one); the bold
//! title in `value` or `warn`; ` · ` then the excerpt in `label`; then the
//! row's SLOTS, words on the row (ruling 136): `  NN%` or the elapsed words
//! (`for 41 s`), the ETA words (`35 s left`, `stalled` in warn, blank while
//! the estimate is hidden — ruling 241), the stats in `label`, and at the
//! tail the load words (`disk busy`, ruling 246).
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
//! TO THE PIXEL (ruling 242, amending ruling 138): each cell's record carries
//! the mean tone of its span, and a graded row ALSO carries its pixel raster
//! ([`RowRaster`] → `aterm_render::ChromeRaster`): the ground one colour per
//! window pixel column, mixed in linear light, the fill's edge one
//! antialiased pixel, a level's RAIL in the row's lowest pixels (ruling 243),
//! and the one cell the edge crosses split — its glyph drawn in the fill's
//! ink left of the edge and the track's right of it. The words' inks are
//! chosen ONCE per row (never per cell from the tone passing under), so an
//! edge, a glint, a comet or an echo moving under a word never changes it.
//! The flat look (High Contrast) keeps whole-cell palette tones.
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
//! Every word a DETERMINATE row writes over its fill wears the row's CRISP
//! ink ([`fill_ink`], ruling 222): whichever of the band's own background and
//! foreground inks reads better on the resting fill, chosen once for the
//! row, so the edge, the glint and an echo passing never change which ink a
//! word is.
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
//! rows queued behind it) and the overflow row is ONE link, its words the
//! whole row (`… 3 more ›`, or `+ Downloading … 45% · 2 more ›` when it hides
//! live progress — ruling 259):
//! each opens Settings ▸ Messages in the pressed window — at the row's own
//! entry, or at the top of the log ([`App::band_presentation`] passes
//! `Links::Painted`; Phase 1 withheld them while the page did not exist,
//! since a link with no destination is dishonest). A press on the row BODY
//! is the same `Details ›` press ([`App::press_message_body`], the one law
//! of §2.2), which is why a pointer on the body lights that chip.

use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_messages::text::char_width;
use aterm_messages::{
    ActionIndex, Anim, BandMotion, CapsuleLayout, CapsuleRole, EchoKind, FAILED_WORD, FineTone,
    Hit, Links, MARGIN, Presentation, RowKind, RowLayout, RowMotion, STALLED_WORD, Severity,
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
/// Warn/Error row, `HIGHLIGHT` under High Contrast), the glint — one fixed
/// perceptual step of that fill away from its words' ink ([`glint_step`],
/// ruling 242) — and the fault hue a Fault echo's wash hands the fill to.
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
    /// The glint (and the Complete echo's sweep, which is the same light)
    /// is the fill moved one fixed perceptual step AWAY from the ink its
    /// words wear on it ([`glint_step`], ruling 242): the words only ever
    /// gain contrast under it, so no letter flips as it passes (ruling 158's
    /// promise, kept by construction rather than by a hold).
    pub(crate) fn of(c: &BandColors, fill: [u8; 3]) -> Self {
        // The glint is ONE fixed perceptual step (ruling 242): the fill moved
        // in OkLab lightness AWAY from the ink its words wear on it, so no
        // word loses contrast — or changes side — under it; where that step
        // would be too faint to see, the row draws it in its rail band.
        Self {
            track: c.meter_track,
            fill,
            glint: glint_step(fill, fill_ink(c, fill), false),
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

/// THE CRISP INK ON A FILL (design ruling 222): the ink every word a
/// determinate row writes over its fill wears — the theme's own ink that
/// reads best on the row's RESTING fill among those on the fill's words'
/// side ([`fill_anchor`]): its background (the terminal's, `field_bg`, or
/// the band's, `bar_bg`) or its foreground (`value`). On the default ground
/// that is the terminal's dark on the neon cursor green, where the old floor
/// held a grey at exactly 4.5:1. Where no candidate sits on that side (a
/// fill at the crossover) the one nearest it does. Chosen once per row from
/// the fill's ink, never from the tone under a cell, so the edge, the glint,
/// a glide or an echo passing under a word never changes which ink it is;
/// the per-cell floor ([`floor_word`]) stays as the minimum.
pub(crate) fn fill_ink(c: &BandColors, fill: [u8; 3]) -> [u8; 3] {
    let dark = fill_anchor(fill) == BLACK;
    let lum = luminance(fill);
    let candidates = [c.field_bg, c.bar_bg, c.value];
    let on_side = |ink: &&[u8; 3]| {
        if dark {
            luminance(**ink) <= lum
        } else {
            luminance(**ink) >= lum
        }
    };
    let best = candidates.iter().filter(on_side).max_by(|a, b| {
        chrome_band::contrast(**a, fill).total_cmp(&chrome_band::contrast(**b, fill))
    });
    let nearest = || {
        let key = |ink: &&[u8; 3]| luminance(**ink);
        if dark {
            candidates.iter().min_by(|a, b| key(a).total_cmp(&key(b)))
        } else {
            candidates.iter().max_by(|a, b| key(a).total_cmp(&key(b)))
        }
    };
    best.or_else(nearest).copied().unwrap_or(c.bar_bg)
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
                // The comet runs on the NEUTRAL channel (ruling 260): the
                // determinate bar's track is tinted toward its hue, but a
                // comet's own light is the hue, and its gradient keeps the
                // two-level pixel steps it was measured to (ruling 242).
                track: c.chip_ground,
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
pub(crate) fn keep_side(ink: [u8; 3], anchor: [u8; 3], target: f64) -> [u8; 3] {
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
    // In LINEAR light (ruling 242): the held tone stays on the straight line
    // between the two, inside the hull of the row's inks.
    let (mut lo, mut hi) = (0u8, 255u8);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if chrome_band::contrast(anchor, lin_mix(bg, track, f64::from(mid) / 255.0)) >= target {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    lin_mix(bg, track, f64::from(hi) / 255.0)
}

/// `a` toward `b` by `t`/255 — integer, so every target mixes the same byte.
fn mix_u8(a: [u8; 3], b: [u8; 3], t: u8) -> [u8; 3] {
    let t = u32::from(t);
    let mix = |x: u8, y: u8| {
        u8::try_from((u32::from(x) * (255 - t) + u32::from(y) * t + 127) / 255).unwrap_or(255)
    };
    [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])]
}

/// A tone's colour against a row's inks — the track mixed toward the fill
/// by `fill`, that toward the glint by `lift`, plus `warn`'s share of warn
/// over the track — in LINEAR light, with one rounding at the end (the pixel
/// raster's own mix, [`LinInks`], ruling 242; ruling 157's single rounding).
#[cfg(test)]
pub(crate) fn tone_rgb(t: aterm_messages::Tone, k: &MeterInks) -> [u8; 3] {
    fine_rgb(t.into(), k)
}

/// [`tone_rgb`] of a [`FineTone`]: a span's mean, unrounded.
#[cfg(test)]
pub(crate) fn fine_rgb(t: FineTone, k: &MeterInks) -> [u8; 3] {
    LinInks::of(k).rgb(t)
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
/// every other piece of the row — time, load, capsules — in the column the
/// row had it. A fill is taken to 100 % first, the reading the echo
/// completes from; the echo drops the frozen stats and the load words (the
/// work ended; the slot they sat in stays — ruling 229), and keeps its
/// percent's and its capsules' cells with nothing drawn in them (ruling
/// 244).
#[cfg(test)]
pub(crate) fn assert_completes_in_place(msg: &aterm_messages::Message, finished: &str) {
    use aterm_messages::{Instant, Look, MessageCenter, MessageLog, Outcome, TITLE_COL, WallStamp};
    assert_eq!(msg.finished_title(), finished, "{}", msg.title);
    let mut msg = msg.clone();
    msg.reveal_after = None;
    if let Some(m) = msg.meter.as_mut().filter(|m| m.fill_permille.is_some()) {
        m.fill_permille = Some(1000);
        // The count read with it (ruling 259 holds a fill inside its count's
        // span): the echo drops the stats anyway.
        m.stats.clear();
    }
    // Everything but the title — and the pieces a Complete echo keeps the
    // cells of and draws nothing in (ruling 244): the percent and the
    // capsules' words.
    let but_title = |r: &RowLayout| {
        let mut r = r.clone();
        r.kind = RowKind::Overflow { hidden: 0 };
        r.title.1.clear();
        r.full_title.clear();
        r.stats = None;
        r.load = None;
        r.pct = None;
        for cap in &mut r.capsules {
            cap.text = " ".repeat(cap.text.chars().count());
            cap.full_label = "";
            cap.role = CapsuleRole::Details;
        }
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
        assert_eq!(echo.load, None, "{}@{cols}: no load words", msg.title);
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
        // The log states the outcome (ruling 259): the finished words, with
        // no in-flight frame (ruling 265), under ✓ Success.
        let rec = center.log().get(id).unwrap();
        assert_eq!(rec.title, finished, "the record's words");
        assert!(
            rec.detail.is_empty(),
            "no in-flight frame: {:?}",
            rec.detail
        );
        assert_eq!(
            (rec.severity, rec.glyph.ch()),
            (aterm_messages::Severity::Success, '\u{2713}')
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

/// One row's PIXEL raster in WINDOW pixels (design ruling 242), before the
/// splice maps it onto its frame ([`RowRaster::on_frame`]): the ground one
/// colour per window pixel column (empty for none — a rail row, the flat
/// look), the rail band's colours (`None` keeps the ground; empty for no
/// rail), whether the words lift clear of a level's rail, the columns whose
/// chips keep their own fill, and the ink split.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct RowRaster {
    /// One colour per window pixel column, `[0, win_w)`.
    pub ground: Vec<[u8; 3]>,
    /// The rail band's colour per window pixel column (`None`: the ground).
    pub rail: Vec<Option<[u8; 3]>>,
    /// A LEVEL's rail (the strain row, ruling 248): the row's words keep
    /// clear of it — the renderer lifts them into the room the face leaves
    /// above its tallest letter and fits the rail, with one clear row, under
    /// their lowest ink ([`aterm_render::ChromeRaster::clear_rail`]). A bar's
    /// glint rail never moves its words.
    pub clear_rail: bool,
    /// Cell columns `[start, end)` that keep their own fill.
    pub own: Vec<(u16, u16)>,
    /// `(col, window x, ink, ground)`: the cell the fill's edge falls in,
    /// the window pixel the edge is at, and the fill side's ink and ground.
    pub split: Option<(u16, u32, [u8; 3], [u8; 3])>,
    /// The one window pixel the fill's edge antialiases (a mix of the fill
    /// and the track), when the edge is inside the row.
    pub edge: Option<u32>,
    /// The window pixels `[start, end)` a PASTEL fill's darker edge line is
    /// drawn over (design ruling 264, [`chrome_band::BandColors::meter_edge`]):
    /// the fill's last [`edge_line_px`] pixels, antialiased at both ends.
    /// `None` on every other row.
    pub line: Option<(u32, u32)>,
    /// OUTLINED capsules (ruling 249): `(start, end, ring, inner)` — cell
    /// columns, the ring's colour and the ground inside it.
    pub rings: Vec<(u16, u16, [u8; 3], [u8; 3])>,
    /// The cells whose glyph is a DRAWN band icon (ruling 251).
    pub icons: Vec<(u16, aterm_render::BandIcon)>,
}

impl RowRaster {
    /// This raster on a frame whose column 0 starts `lo` window pixels in
    /// from the window's left edge (the leading remainder band, `cells_x −
    /// pad`) and which is `frame_w` pixels wide, for chrome row `row` whose
    /// band is `cell_h` pixels tall: the renderer's [`aterm_render::ChromeRaster`].
    /// A frame pixel past the window's own columns repeats the edge one.
    pub(crate) fn on_frame(
        &self,
        row: u16,
        lo: usize,
        frame_w: usize,
        cell_h: usize,
    ) -> aterm_render::ChromeRaster {
        let pack = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
        let at = |v: &[[u8; 3]], x: usize| v[(x + lo).min(v.len().saturating_sub(1))];
        let ground: std::sync::Arc<[u32]> = if self.ground.is_empty() {
            std::sync::Arc::from(Vec::new())
        } else {
            (0..frame_w).map(|x| pack(at(&self.ground, x))).collect()
        };
        let rail: std::sync::Arc<[u32]> = if self.rail.is_empty() {
            std::sync::Arc::from(Vec::new())
        } else {
            (0..frame_w)
                .map(|x| {
                    self.rail[(x + lo).min(self.rail.len() - 1)]
                        .map_or(aterm_render::ChromeRaster::KEEP, pack)
                })
                .collect()
        };
        let rail_h = if self.rail.is_empty() {
            0
        } else {
            u16::try_from(rail_px(cell_h)).unwrap_or(0)
        };
        aterm_render::ChromeRaster {
            row,
            ground,
            rail_h,
            clear_rail: self.clear_rail && rail_h > 0,
            rail,
            own: self.own.clone(),
            rings: self
                .rings
                .iter()
                .map(|&(start, end, ring, inner)| aterm_render::ChromeRing {
                    start,
                    end,
                    ring: pack(ring),
                    inner: pack(inner),
                    // The seam is the composed stack's ([`floor_rings`]).
                    seam: None,
                })
                .collect(),
            icons: self
                .icons
                .iter()
                .map(|&(col, icon)| aterm_render::ChromeIcon { col, icon })
                .collect(),
            split: self.split.and_then(|(col, x, ink, bg)| {
                Some(aterm_render::InkSplit {
                    col,
                    x: u32::try_from(usize::try_from(x).ok()?.checked_sub(lo)?).ok()?,
                    ink: pack(ink),
                    bg: pack(bg),
                })
            }),
        }
    }
}

/// The rail band's height on a `cell_h`-pixel row: about an eighth of it —
/// two pixels on a 1x cell, three on a 24-pixel one, never under two — in
/// the row's lowest pixels (design ruling 243). A level's rail is at most
/// this: the renderer fits it, with its words lifted clear, into the room
/// the face leaves ([`RowRaster::clear_rail`], ruling 248); a bar's glint
/// drawn there sits under its words' descenders as before.
pub(crate) fn rail_px(cell_h: usize) -> usize {
    ((cell_h + 4) / 8).max(2)
}

/// The width of a pastel fill's darker EDGE line on a window whose cells are
/// `cell_w` pixels wide (design ruling 264): about an eighth of a cell — one
/// pixel on a 1x cell, two on a Retina one — never under one.
pub(crate) fn edge_line_px(cell_w: usize) -> usize {
    ((cell_w + 4) / 8).max(1)
}

/// One painted band: its rows; for each row the `(left, right)` gutter tones
/// of its meter (`None` on an unmetered row, whose gutters keep the band's
/// own tone); and each row's pixel raster (`None` where the cells say it all).
pub(crate) type PaintedBand = (
    Vec<Vec<RenderCell>>,
    Vec<Option<([u8; 3], [u8; 3])>>,
    Vec<Option<RowRaster>>,
);

/// Paint every committed band row as one `p.cols`-wide row each, top to
/// bottom, on the chrome band's material, at ONE motion frame `motion`
/// (`MessageCenter::motion` over `p`: the moving look on a window that
/// animates, the still look everywhere else — a held bar and a busy row's
/// unlit track are drawn from it either way). Each metered or busy row's
/// surface is mapped onto the window through `geom` ([`MeterSpan`]) at
/// PIXEL resolution ([`RowRaster`], ruling 242). `hover` lights the chip
/// under the pointer (or the body's `Details ›`) on its row. No row carries
/// the hairline that closes the chrome against the terminal: the splice pads
/// and trims this cache to the committed count and puts a presence row above
/// it, so only the COMPOSED stack knows which row is last — [`seal_stack`]
/// draws it there.
pub(crate) fn paint_rows_on(
    p: &Presentation,
    palette: impl Into<chrome_band::BandPalette>,
    hover: Option<BandHover>,
    geom: BandGeometry,
    motion: &BandMotion,
) -> PaintedBand {
    let c = palette.into().colors();
    let hc = chrome_band::forced_chrome().is_some();
    let n = p.rows.len();
    let mut rows: Vec<Vec<RenderCell>> = Vec::with_capacity(n);
    let mut edges = Vec::with_capacity(n);
    let mut rasters = Vec::with_capacity(n);
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
        // enters and leaves. The pixel raster, where there is one, is drawn
        // over both (and dirties its row on its own).
        let (metered, raster) = paint_row(&mut row, p.cols, layout, rm, &c, hc, lit, geom);
        edges.push(
            metered
                .filter(|()| !row.is_empty())
                .map(|()| (row[0].bg, row[row.len() - 1].bg)),
        );
        rows.push(row);
        rasters.push(raster);
    }
    (rows, edges, rasters)
}

/// The inks a row's words wear, chosen ONCE for the row (design ruling 242)
/// — never per cell from the tone passing under it, so an edge, a glint, a
/// comet or an echo moving under a word never changes its ink.
struct Ground<'a> {
    band: [u8; 3],
    /// Each column's surface (the MEAN over its pixels, what the cell record
    /// carries), and whether it lies on the FILL's side of the row — `None`
    /// on an unmetered row.
    meter: Option<&'a [([u8; 3], bool)]>,
    hc: bool,
    /// The High Contrast ink for a word on the fill (`HIGHLIGHTTEXT`).
    hc_fill_ink: [u8; 3],
    /// How a word's ink is chosen on this row.
    plan: InkPlan,
}

/// A row's ink rule (ruling 242).
#[derive(Clone, Copy)]
enum InkPlan {
    /// The band's own inks, untouched: an unmetered row, a RAIL row (the
    /// strain gauge's words sit on the band, ruling 243).
    Band,
    /// A BUSY row: every role's ink floored ONCE toward `anchor` against the
    /// hottest tone the comet can reach (`hot`) and its `track` (ruling 152's
    /// side hold keeps every tone between them on the far side).
    Busy {
        anchor: [u8; 3],
        hot: [u8; 3],
        track: [u8; 3],
    },
    /// A DETERMINATE row: over the fill, the row's one crisp ink (ruling 222)
    /// floored against every tone the fill side can take; over the track, each
    /// role's ink floored against the track toward the band's anchor.
    Bar {
        crisp: [u8; 3],
        track: [u8; 3],
        track_anchor: [u8; 3],
    },
}

impl Ground<'_> {
    /// The background under column `x` (the cell record's).
    fn at(&self, x: usize) -> [u8; 3] {
        self.meter
            .and_then(|m| m.get(x))
            .map_or(self.band, |&(bg, _)| bg)
    }

    /// Whether column `x` lies on the fill's side of the row.
    fn on_fill(&self, x: usize) -> bool {
        self.meter
            .and_then(|m| m.get(x))
            .is_some_and(|&(_, fill)| fill)
    }

    /// `ink` as a word at column `x` wears it: the row's one ink for its
    /// side ([`InkPlan`]); under High Contrast the system's `HIGHLIGHTTEXT`
    /// on the fill and the forced ink floor elsewhere.
    fn ink(&self, x: usize, ink: [u8; 3]) -> [u8; 3] {
        if self.meter.is_none() {
            return ink;
        }
        if self.hc {
            return if self.on_fill(x) {
                self.hc_fill_ink
            } else {
                chrome_band::forced_ink(ink, self.at(x))
            };
        }
        match self.plan {
            InkPlan::Band => ink,
            InkPlan::Busy { anchor, hot, track } => floor_toward(
                floor_toward(ink, hot, anchor, WORD_AA),
                track,
                anchor,
                WORD_AA,
            ),
            InkPlan::Bar {
                crisp,
                track,
                track_anchor,
            } => {
                if self.on_fill(x) {
                    crisp
                } else {
                    floor_word(ink, track, track_anchor)
                }
            }
        }
    }

    /// `write_str` over this ground: one glyph per cell, each on the
    /// surface under it in its row's ink.
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
    // STACKED ROWS STAY APART (design ruling 260): every row above the last
    // carries a 1 px DIVIDER, the seam's ink at [`DIVIDER_ALPHA`] over the
    // band — two rows of the same ground no longer run together into one
    // slab. A rail lit in a row's lowest pixels takes its place there, as it
    // takes the seam's (the splice's rail pass).
    let colors = chrome_band::band_colors(theme);
    // Under an OS-forced palette every ink is a system colour: the divider is
    // the seam's own.
    let divider = if chrome_band::forced_chrome().is_some() {
        colors.label
    } else {
        chrome_band::mix3(colors.bar_bg, colors.label, DIVIDER_ALPHA)
    };
    for row in above {
        for cell in row.iter_mut() {
            cell.underline = UnderlineStyle::Single;
            cell.underline_color = Some(divider);
        }
    }
    chrome_band::seal_band_bottom(last, colors.label);
}

/// How strongly the divider between stacked band rows shows the seam's ink
/// over the band (design ruling 260): a hairline that separates, softer than
/// the seam that closes the stack.
pub(crate) const DIVIDER_ALPHA: f32 = 0.35;

/// GIVE EACH OUTLINED CAPSULE ITS FLOOR (ruling 254): the cells between a
/// ring's two rounded ends ([`aterm_render::chrome_ring_floor_cols`]) carry
/// a single underline in the ring's colour, on whichever row the ring sits.
/// The ring builder stands the pill on the underline's rows and leaves its
/// straight floor to these cells, so the renderer's descender ink-skip
/// carves the floor round a `p` exactly as it carves the seam. On the
/// stack's LAST row the seam hands its two end cells to the ring
/// ([`aterm_render::ChromeRing::seam`]): the builder draws the seam there
/// itself, meeting the pill's rounded foot antialiased, and the pill's
/// floor carries the line on in the ring's colour — one edge, and no seam
/// pixel crosses the pill's inside. Run after [`seal_stack`] (and after the
/// seam gives way to a lit rail), each frame the stack is composed;
/// `rasters` are the frame's chrome rasters, keyed by their row.
pub(crate) fn floor_rings(
    rows: &mut [Vec<RenderCell>],
    rasters: &mut [aterm_render::ChromeRaster],
) {
    for m in rasters {
        let Some(row) = rows.get_mut(usize::from(m.row)) else {
            continue;
        };
        for ring in &mut m.rings {
            let floor = aterm_render::chrome_ring_floor_cols(ring);
            let (start, end) = (
                usize::from(ring.start),
                usize::from(ring.end).min(row.len()),
            );
            if start >= end {
                continue;
            }
            // The seam, when every end cell carries it in one tone.
            let ends = || (start..end).filter(|c| !floor.contains(c));
            let tone = row[start].underline_color;
            let sealed = ends().all(|c| {
                row[c].underline == UnderlineStyle::Single && row[c].underline_color == tone
            });
            ring.seam = tone
                .filter(|_| sealed)
                .map(|[r, g, b]| (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b));
            if ring.seam.is_some() {
                for c in ends() {
                    row[c].underline = UnderlineStyle::None;
                    row[c].underline_color = None;
                }
            }
            let [_, r, g, b] = ring.ring.to_be_bytes();
            let fe = floor.end.min(end);
            for cell in row.get_mut(floor.start.min(fe)..fe).into_iter().flatten() {
                cell.underline = UnderlineStyle::Single;
                cell.underline_color = Some([r, g, b]);
            }
        }
    }
}

/// The ink a time slot's words wear (design §10.7): a stall, and a Fault
/// echo's `failed`, are the one thing on the row the person may need to act
/// on — warn; a remaining time (it always ends in `left`) reads in the value
/// ink; how long work has run (`for 41 s`, ruling 241) in the label.
fn slot_ink(words: &str, c: &BandColors) -> [u8; 3] {
    if words == STALLED_WORD || words == FAILED_WORD {
        c.warn
    } else if words.ends_with(" left") {
        c.value
    } else {
        c.label
    }
}

/// One row at one motion frame: the meter's surface (the engine's
/// [`aterm_messages::Surface`] mapped onto the window, [`MeterSpan`]) — at
/// PIXEL resolution where the look is graded ([`RowRaster`], ruling 242) —
/// then the glyph (the row's own or an echo's ✓ / ⚠, drawn as an icon), title,
/// excerpt, pct, the time slots, stats, the load words at the tail, and the
/// capsules over it; last an echo's fade. `Some(())` when the row has a
/// surface (its gutters then continue its painted edge cells), `None`
/// otherwise; and the row's raster.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one row's paint reads its layout, its frame, the palette, the look, the hover and the window — each a separate input the splice already holds — and lays its surface, its words and its fade in one order"
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
) -> (Option<()>, Option<RowRaster>) {
    let overflow = matches!(l.kind, RowKind::Overflow { .. });
    let alarm = matches!(l.severity, Severity::Warn | Severity::Error);
    // Success and Info share the title ink, Warn wears `warn` and Error
    // `error` (§2.6, ruling 265);
    // the overflow row is a link, not a message, and reads in `label`.
    // Under High Contrast every ink is WINDOWTEXT (module doc): the glyph
    // too — a stock palette's HIGHLIGHT is made to sit under highlight text,
    // and on Aquatic's black band it is 1.3:1.
    // The overflow row is ONE link (ruling 259): the pointer anywhere on it
    // lifts its words to the value ink — its lit state, event-driven.
    let (ink, accent) = if overflow && hover.is_some() {
        (c.value, c.value)
    } else if overflow {
        (c.label, c.label)
    } else if l.severity == Severity::Error {
        // An error's words are red, a warning's the warn ink (ruling 265).
        (c.error, c.error)
    } else if alarm {
        (c.warn, c.warn)
    } else if hc {
        (c.value, c.value)
    } else {
        (c.value, c.accent)
    };
    let surface = &rm.surface;
    // A measured LEVEL is a RAIL (ruling 243): its words sit on the band's own
    // ground, the rail in the row's lowest pixels in the warn hue — never the
    // cursor accent, never a fill a word rides.
    let rail = surface.rail && !surface.is_empty();
    // THE METER IS THE ROW: the fill's ink is the cursor accent, `warn` on a
    // Warn/Error row; under High Contrast the system's HIGHLIGHT whatever the
    // severity — the forced `warn` is WINDOWTEXT, an ink, not a surface — and
    // the words on it HIGHLIGHTTEXT.
    let fill = if hc || !alarm { c.meter } else { c.warn };
    let busy = l.track.is_some();
    let (inks, side) = row_inks(c, l, rail, hc);
    let span = MeterSpan { geom, cols };
    let win_w = span.win_w();
    // The row's pixel raster: every graded surface but a rail's is its ground,
    // pixel by pixel; the flat look (High Contrast) keeps whole-cell tones.
    let raster_ground = !surface.is_empty() && !surface.flat && !rail && cols > 0;
    // The fill's EDGE in window pixels, and the cell it falls in — the one
    // cell whose glyph is split (ruling 242).
    let edge_px = surface
        .edge
        .filter(|_| raster_ground && !busy)
        .map(|e| u64::from(e) * win_w / u64::from(aterm_messages::ROW));
    let split_col = edge_px.and_then(|e| {
        (0..cols).find(|&x| {
            let (x0, x1) = span.px(x);
            x0 < e && e < x1
        })
    });
    // A DRAWN ICON spills into the blank cells beside its own (ruling 258):
    // an edge anywhere in its three cells — on a boundary between them too —
    // crosses the icon, so the icon's cell takes the split: the fill's ink
    // left of the edge, its own (the track's) right of it.
    let (gcol, gch) = l.glyph;
    let icon_split = edge_px
        .filter(|&e| {
            gcol > 0
                && gcol + 1 < cols
                && aterm_render::BandIcon::for_char(rm.glyph.unwrap_or(gch)).is_some()
                && span.px(gcol - 1).0 < e
                && e < span.px(gcol + 1).1
        })
        .map(|_| gcol);
    // The glint in the lowest pixels, where the fill's words leave it no room
    // at full height (ruling 242).
    let glint_rail = !busy && !rail && !hc && glint_needs_rail(c, fill, &inks);
    let rail_glint_ink = glint_rail.then(|| glint_step(fill, fill_ink(c, fill), true));
    let lin = LinInks::of(&inks);
    // Each cell's record: the MEAN tone over its pixels, and its side.
    let tones: Option<Vec<([u8; 3], bool)>> =
        (!surface.is_empty() && cols > 0 && !rail).then(|| {
            (0..cols)
                .map(|x| {
                    let mut t = span.fine(surface, x);
                    if glint_rail {
                        t.lift = 0;
                    }
                    let rgb = lin.rgb(t);
                    let on = match (split_col, edge_px) {
                        _ if busy || hc => t.fill >= 128 << 8,
                        // The split icon's own ink is the track's.
                        _ if icon_split == Some(x) => false,
                        (Some(s), _) => x < s,
                        (None, Some(e)) => span.px(x).1 <= e,
                        (None, None) => t.fill >= 128 << 8,
                    };
                    match side {
                        // Mixed in linear light between two inks already on the
                        // words' side, a comet tone never leaves it: no hold.
                        Some(_) => (rgb, false),
                        None if hc => (rgb, on),
                        None => {
                            let rgb = if on && t.warn > 0 {
                                hold_side(rgb, inks.fill, fill_anchor(inks.fill), COMET_SIDE_AA)
                            } else {
                                rgb
                            };
                            (rgb, on)
                        }
                    }
                })
                .collect()
        });
    // THE ROW'S INKS, chosen once (ruling 242).
    let plan = if rail || surface.is_empty() {
        InkPlan::Band
    } else if let Some(anchor) = side {
        // Floored against the hottest tone the comet can reach — its head,
        // held on the words' side — and its track: nothing it draws between
        // them is nearer the anchor (linear light), so no word brightens or
        // dims as the head passes.
        // A still busy row draws its unlit track only: no comet to floor
        // against (the look changing is a change of state, not a flicker).
        let still = matches!(rm.anim, Anim::Track);
        InkPlan::Busy {
            anchor,
            hot: if still {
                inks.track
            } else {
                hold_side(inks.fill, inks.track, anchor, WORD_AA)
            },
            track: inks.track,
        }
    } else {
        // A stalled bar's fill is its dim slate (`Tone::STALLED`): its own
        // state, its own ink.
        let stalled = surface
            .stops
            .iter()
            .any(|s| s.tone == aterm_messages::Tone::STALLED);
        InkPlan::Bar {
            crisp: if stalled {
                crisp_on_stalled(c, fill, &inks)
            } else {
                crisp_on_fill(c, fill, &inks)
            },
            track: inks.track,
            track_anchor: words_anchor(c),
        }
    };
    let on = Ground {
        band: c.bar_bg,
        meter: tones.as_deref(),
        hc,
        hc_fill_ink: c.on_accent,
        plan,
    };
    if let Some(tones) = &tones {
        for (cell, &(bg, _)) in row.iter_mut().zip(tones) {
            *cell = chrome_band::cell(' ', c.label, bg, false, false);
        }
    }
    let (gcol, glyph) = l.glyph;
    let fault = matches!(
        rm.anim,
        Anim::Echo {
            kind: EchoKind::Fault,
            ..
        }
    );
    // The glyph cell's DRAWN icon (ruling 251): its character stays in the
    // cell; the renderer draws the icon in the cell's ink instead of a font's
    // glyph.
    let mut icon = None;
    if gcol < cols {
        // The engine's glyph for this frame: an echo's ✓ / ⚠ (a Fault echo's
        // ⚠ in warn whatever the row's severity was) — else the row's own
        // (a moving comet no longer spins a braille frame there).
        // A rail row's glyph wears the rail's warn: the strain row paints
        // nothing in the cursor accent (ruling 243).
        let g_ink = if fault || rail { c.warn } else { accent };
        let glyph = rm.glyph.unwrap_or(glyph);
        icon = aterm_render::BandIcon::for_char(glyph);
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
        on.slot(
            row,
            cols,
            (col, l.elapsed_width()),
            words,
            slot_ink(words, c),
        );
    }
    if let Some(col) = l.eta {
        let words = rm.eta.as_deref().unwrap_or("");
        on.slot(row, cols, (col, l.eta_width()), words, slot_ink(words, c));
    }
    if let Some((scol, s)) = &l.stats {
        on.write(row, cols, *scol, s, c.label, false);
    }
    // The load words at the TAIL of the word cluster, with no joint (ruling
    // 246): an empty reservation reads as track, never as a hole.
    if let Some((lcol, words)) = l.load {
        on.write(row, cols, lcol, words, c.label, false);
    }
    let mut own: Vec<(u16, u16)> = Vec::new();
    let mut rings: Vec<(u16, u16, [u8; 3], [u8; 3])> = Vec::new();
    // A Primary on a row with a meter is OUTLINED (ruling 249) wherever the
    // row is drawn to the pixel: the bar is the row's only solid accent.
    let outline = raster_ground && !hc;
    for cap in &l.capsules {
        let lit = match hover {
            Some(HoverTarget::Capsule(k)) => cap.action == k,
            Some(HoverTarget::Body) => cap.action.is_details(),
            None => false,
        };
        let span = (
            u16::try_from(cap.col).unwrap_or(u16::MAX),
            u16::try_from((cap.col + cap.width).min(cols)).unwrap_or(u16::MAX),
        );
        match paint_capsule(row, cols, cap, c, hc, lit, &on, outline) {
            Chip::Rides => {}
            Chip::Owns => own.push(span),
            Chip::Rings { ring, inner } => rings.push((span.0, span.1, ring, inner)),
        }
    }
    let owned = |x: usize| {
        own.iter()
            .any(|&(a, b)| (usize::from(a)..usize::from(b)).contains(&x))
    };
    let ringed = |x: usize| {
        rings
            .iter()
            .any(|&(a, b, ..)| (usize::from(a)..usize::from(b)).contains(&x))
    };
    // THE PIXEL RASTER (ruling 242): the ground pixel by pixel in linear
    // light, its edge antialiased over one pixel (each pixel is the MEAN of
    // the profile over it); the glint in the lowest pixels where the words
    // leave it no room at full height; a level's rail (ruling 243).
    // Which window pixels lie on the fill's side (the fade's anchors).
    let mut fill_side: Vec<bool> = Vec::new();
    let mut raster = (raster_ground || rail).then(|| {
        let mut r = RowRaster::default();
        let mut memo: Option<(FineTone, [u8; 3])> = None;
        let mut rgb_of = |t: FineTone| match memo {
            Some((k, v)) if k == t => v,
            _ => {
                let v = lin.rgb(t);
                memo = Some((t, v));
                v
            }
        };
        let n = usize::try_from(win_w).unwrap_or(0);
        if raster_ground {
            r.ground.reserve(n);
        }
        let mut rail_px_row: Vec<Option<[u8; 3]>> = Vec::new();
        for xw in 0..n as u64 {
            let t = surface.span_fine(xw, xw + 1, win_w);
            // The fill's coverage — handed to warn by a Fault's wash.
            fill_side.push(u32::from(t.fill) + u32::from(t.warn) >= 128 << 8);
            if rail {
                let cov = f64::from(t.fill) / (255.0 * 256.0);
                rail_px_row.push((t.fill > 0).then(|| lin_mix(c.bar_bg, inks.fill, cov)));
                continue;
            }
            let base = FineTone {
                lift: if glint_rail { 0 } else { t.lift },
                ..t
            };
            let mut rgb = rgb_of(base);
            if side.is_none() && t.warn > 0 && !hc && edge_px.is_none_or(|e| xw < e) {
                rgb = hold_side(rgb, inks.fill, fill_anchor(inks.fill), COMET_SIDE_AA);
            }
            r.ground.push(rgb);
            if let Some(g) = rail_glint_ink {
                let lift = f64::from(t.lift) / (255.0 * 256.0);
                rail_px_row.push((t.lift > 0).then(|| lin_mix(rgb, g, lift)));
            }
        }
        if rail || glint_rail {
            r.rail = rail_px_row;
        }
        r.clear_rail = rail;
        r.own.clone_from(&own);
        r.rings.clone_from(&rings);
        r.edge = edge_px.and_then(|e| u32::try_from(e).ok());
        if let (Some(col), Some(e), InkPlan::Bar { crisp, .. }) =
            (icon_split.or(split_col), edge_px, plan)
            && !owned(col)
            && !ringed(col)
        {
            // The pixel the edge crosses goes to whichever side covers most
            // of it: the crisp ink's line is at its pixel boundary.
            let exact = surface.edge.map_or(0, |q| u64::from(q) * win_w);
            let covered =
                exact % u64::from(aterm_messages::ROW) * 2 >= u64::from(aterm_messages::ROW);
            let x = e + u64::from(covered);
            let at = usize::try_from(e.saturating_sub(1)).unwrap_or(0);
            let under = r.ground.get(at).copied().unwrap_or(inks.fill);
            r.split = Some((
                u16::try_from(col).unwrap_or(u16::MAX),
                u32::try_from(x).unwrap_or(u32::MAX),
                crisp,
                under,
            ));
        }
        // THE PASTEL FILL'S EDGE (ruling 264): the fill's last pixels in the
        // palette's darker edge ink — the boundary a pastel cannot carry on
        // its own — drawn after the split took the fill's own ground for its
        // glyph. Each pixel trades the fill for the edge by the line's
        // coverage of it (antialiased at both ends), in linear light, scaled
        // by the fill's own strength there, so a stalled slate or a Fault's
        // wash takes the line with it.
        if let (Some(edge_ink), Some(q), true) = (
            c.meter_edge,
            surface.edge,
            fill == c.meter && !hc && !busy && raster_ground,
        ) {
            let unit = u64::from(aterm_messages::ROW);
            // A share of one pixel, `v` of `unit`.
            let share = |v: u64| {
                f64::from(u32::try_from(v).unwrap_or(u32::MAX)) / f64::from(aterm_messages::ROW)
            };
            let exact = u64::from(q) * win_w;
            let width = u64::try_from(edge_line_px(geom.cell_w)).unwrap_or(1) * unit;
            let (lo, hi) = (exact.saturating_sub(width), exact);
            if hi > lo {
                let first = lo / unit;
                let end = hi
                    .div_ceil(unit)
                    .min(u64::try_from(r.ground.len()).unwrap_or(u64::MAX));
                let (edge_lin, fill_lin) = (edge_ink.map(self::lin), inks.fill.map(self::lin));
                for p in first..end {
                    let (p0, p1) = (p * unit, (p + 1) * unit);
                    let line = p1.min(hi).saturating_sub(p0.max(lo));
                    let filled = p1.min(exact).saturating_sub(p0);
                    if line == 0 || filled == 0 {
                        continue;
                    }
                    // The fill's strength at this pixel: its coverage over
                    // the share of the pixel left of the edge.
                    let t = surface.span_fine(p, p + 1, win_w);
                    let strength =
                        (f64::from(t.fill) / (255.0 * 256.0) / share(filled)).clamp(0.0, 1.0);
                    let k = share(line) * strength;
                    let Some(px) = usize::try_from(p).ok().and_then(|p| r.ground.get_mut(p)) else {
                        continue;
                    };
                    let old = *px;
                    *px = [0, 1, 2]
                        .map(|i| enc((edge_lin[i] - fill_lin[i]).mul_add(k, self::lin(old[i]))));
                }
                r.line = Some((
                    u32::try_from(first).unwrap_or(u32::MAX),
                    u32::try_from(end).unwrap_or(u32::MAX),
                ));
            }
        }
        r
    });
    if let (Some(icon), Ok(col)) = (icon, u16::try_from(gcol)) {
        raster
            .get_or_insert_with(RowRaster::default)
            .icons
            .push((col, icon));
    }
    // An echo's fade (ruling 244): the row fades as ONE layer, in linear
    // light, toward the band — and for its first half (the words' opacity at
    // least one half) the ground under the words is held on their side and
    // the words floored on it, so they keep AA to the midpoint and never
    // change side; the second half fades that composed state to the band.
    if rm.fade > 0 {
        let alpha = 1.0 - f64::from(rm.fade) / 255.0;
        let track_anchor = words_anchor(c);
        let anchor_of = |on_fill: bool| match (side, on_fill) {
            (Some(a), _) => a,
            (None, true) => fill_anchor(fill),
            (None, false) => track_anchor,
        };
        for (x, cell) in row.iter_mut().enumerate() {
            let a = anchor_of(on.on_fill(x));
            if owned(x) {
                cell.fg = lin_mix(cell.fg, c.bar_bg, 1.0 - alpha);
                cell.bg = lin_mix(cell.bg, c.bar_bg, 1.0 - alpha);
                continue;
            }
            let (bg, fg) = faded_pair(cell.bg, cell.fg, c.bar_bg, alpha, a);
            cell.bg = bg;
            cell.fg = fg;
        }
        if let Some(r) = raster.as_mut() {
            for (xw, px) in r.ground.iter_mut().enumerate() {
                let on_fill = fill_side.get(xw).copied().unwrap_or(false);
                *px = faded_ground(*px, c.bar_bg, alpha, anchor_of(on_fill));
            }
            for px in r.rail.iter_mut().flatten() {
                *px = lin_mix(*px, c.bar_bg, 1.0 - alpha);
            }
            if let Some((_, _, ink, bg)) = r.split.as_mut() {
                let a = anchor_of(true);
                let (b, i) = faded_pair(*bg, *ink, c.bar_bg, alpha, a);
                *bg = b;
                *ink = i;
            }
        }
    }
    (tones.map(|_| ()), raster)
}

/// The inks row `l`'s surface resolves against, and — on a BUSY row — the
/// anchor its words floor toward. A busy row's surface keeps its words' side
/// (the comet, its track and its echoes); a determinate row's Fault wash
/// keeps the fill's words' side, its hue kept (ruling 222) — High Contrast
/// keeps its raw warn; a RAIL (a measured level, ruling 243) wears warn over
/// the band itself, never the cursor accent.
pub(crate) fn row_inks(
    c: &BandColors,
    l: &RowLayout,
    rail: bool,
    hc: bool,
) -> (MeterInks, Option<[u8; 3]>) {
    let alarm = matches!(l.severity, Severity::Warn | Severity::Error);
    let fill = if hc || !alarm { c.meter } else { c.warn };
    if rail {
        let warn = chrome_band::ensure_contrast(rail_warn(c.warn, hc), c.bar_bg, 3.0);
        (
            MeterInks {
                track: c.bar_bg,
                fill: warn,
                glint: warn,
                warn,
            },
            None,
        )
    } else if l.track.is_some() && !hc {
        let (inks, anchor) = MeterInks::comet(c, fill);
        (inks, Some(anchor))
    } else {
        let inks = MeterInks::of(c, fill);
        let warn = if hc {
            inks.warn
        } else {
            keep_side(inks.warn, fill_anchor(fill), COMET_SIDE_AA)
        };
        (MeterInks { warn, ..inks }, None)
    }
}

/// The level rail's warn (ruling 248): the warn HUE at a surface's chroma.
/// A theme's warn is an INK, and on a dark band a pale one (`#f1fa8c`, OkLCh
/// chroma 0.13): a three-pixel line of it under the words' descenders read
/// as an underline, not a level. The rail keeps the warn's lightness and hue
/// and lifts its chroma to [`RAIL_CHROMA`] as far as sRGB allows (`#f2fd4a`
/// there; the light bands' amber is already at the gamut's edge and stays).
/// High Contrast keeps its system ink.
fn rail_warn(warn: [u8; 3], hc: bool) -> [u8; 3] {
    if hc {
        return warn;
    }
    let (l, ch, h) = oklch(warn);
    from_oklch(l, ch.max(RAIL_CHROMA), h)
}

/// The level rail's OkLCh chroma floor (ruling 248): a vivid warn, never the
/// pale ink tone the words' glyph wears.
const RAIL_CHROMA: f64 = 0.19;

/// THE ONE INK every word wears over a determinate row's fill (rulings 222
/// and 242): the row's crisp ink ([`fill_ink`]), floored ONCE against every
/// tone the fill's side can take — the fill, its glint where it is drawn at
/// full height, a Fault's warn wash — so no edge, glint, glide or echo
/// passing under a word ever changes its ink. (A STALLED bar is a state, not
/// a motion: its dim slate takes its own ink, [`crisp_on_stalled`].)
pub(crate) fn crisp_on_fill(c: &BandColors, fill: [u8; 3], inks: &MeterInks) -> [u8; 3] {
    let anchor = fill_anchor(fill);
    let mut crisp = fill_ink(c, fill);
    let mut grounds = vec![inks.fill, inks.warn];
    if delta_e(inks.glint, fill) >= 6.0 {
        grounds.push(inks.glint);
    }
    for _ in 0..2 {
        for g in &grounds {
            crisp = floor_word(crisp, *g, anchor);
        }
    }
    crisp
}

/// The one ink over a STALLED bar's dim slate ([`crisp_on_fill`]'s
/// counterpart for the stall's state).
fn crisp_on_stalled(c: &BandColors, fill: [u8; 3], inks: &MeterInks) -> [u8; 3] {
    let stalled = LinInks::of(inks).rgb(aterm_messages::Tone::STALLED.into());
    floor_word(fill_ink(c, fill), stalled, fill_anchor(stalled))
}

/// A ground and the ink on it at layer opacity `alpha` (ruling 244): above
/// one half, the ground fades in linear light but is held on the words'
/// side of `anchor` ([`hold_side`]) and the ink, faded alike, is floored on
/// it toward `anchor` — the words keep AA and never flip; below one half,
/// that state at one half fades on toward `band` as one layer.
fn faded_pair(
    bg: [u8; 3],
    fg: [u8; 3],
    band: [u8; 3],
    alpha: f64,
    anchor: [u8; 3],
) -> ([u8; 3], [u8; 3]) {
    let a1 = alpha.max(0.5);
    let bg1 = faded_ground(bg, band, a1, anchor);
    let fg1 = floor_toward(lin_mix(fg, band, 1.0 - a1), bg1, anchor, WORD_AA);
    if alpha >= 0.5 {
        (bg1, fg1)
    } else {
        let t = 1.0 - 2.0 * alpha;
        (lin_mix(bg1, band, t), lin_mix(fg1, band, t))
    }
}

/// A ground pixel at layer opacity `alpha` (see [`faded_pair`]).
fn faded_ground(g: [u8; 3], band: [u8; 3], alpha: f64, anchor: [u8; 3]) -> [u8; 3] {
    let a1 = alpha.max(0.5);
    let held = if chrome_band::contrast(anchor, g) >= COMET_SIDE_AA {
        hold_side(lin_mix(g, band, 1.0 - a1), g, anchor, COMET_SIDE_AA)
    } else {
        lin_mix(g, band, 1.0 - a1)
    };
    if alpha >= 0.5 {
        held
    } else {
        lin_mix(held, band, 1.0 - 2.0 * alpha)
    }
}

/// `a` toward `b` by `t` (0–1) in LINEAR light.
pub(crate) fn lin_mix(a: [u8; 3], b: [u8; 3], t: f64) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|k| enc(lin(a[k]).mul_add(1.0 - t, lin(b[k]) * t)))
}

/// A row's [`MeterInks`] in linear light, for the pixel raster (ruling 242):
/// a tone is the track mixed toward the fill by `fill`, that toward the
/// glint by `lift`, plus `warn`'s share of warn over the track — in linear
/// light, so a gradient has no dark band between its ends and a Fault's
/// cross-fade stays inside the track–fill–warn hull.
struct LinInks {
    track: [f64; 3],
    fill: [f64; 3],
    glint: [f64; 3],
    warn: [f64; 3],
}

impl LinInks {
    fn of(k: &MeterInks) -> Self {
        Self {
            track: k.track.map(lin),
            fill: k.fill.map(lin),
            glint: k.glint.map(lin),
            warn: k.warn.map(lin),
        }
    }

    fn rgb(&self, t: FineTone) -> [u8; 3] {
        const FULL: f64 = 255.0 * 256.0;
        let (f, l, w) = (
            f64::from(t.fill) / FULL,
            f64::from(t.lift) / FULL,
            f64::from(t.warn) / FULL,
        );
        [0, 1, 2].map(|i| {
            let base = (self.fill[i] - self.track[i]).mul_add(f, self.track[i]);
            let base = (self.glint[i] - base).mul_add(l, base);
            enc((self.warn[i] - self.track[i]).mul_add(w, base))
        })
    }
}

/// OkLab `(L, a, b)` of an sRGB colour.
fn oklab(c: [u8; 3]) -> (f64, f64, f64) {
    let (l, ch, h) = oklch(c);
    (l, ch * h.to_radians().cos(), ch * h.to_radians().sin())
}

/// The OkLab distance between two colours, ×100 (a ΔE of 6 is a glint the
/// eye sees on any fill).
pub(crate) fn delta_e(a: [u8; 3], b: [u8; 3]) -> f64 {
    let (l0, a0, b0) = oklab(a);
    let (l1, a1, b1) = oklab(b);
    100.0 * ((l1 - l0).powi(2) + (a1 - a0).powi(2) + (b1 - b0).powi(2)).sqrt()
}

/// The glint's one perceptual STEP (ruling 242): the fill moved
/// [`GLINT_DL`] in OkLab lightness AWAY from the ink its words wear on it —
/// so the words only ever gain contrast under it and never change side —
/// with a small chroma lift. `rail`: the other way, for the rail band under
/// the words, where no ink sits.
pub(crate) fn glint_step(fill: [u8; 3], words: [u8; 3], rail: bool) -> [u8; 3] {
    let (l, ch, h) = oklch(fill);
    let away = if oklch(words).0 < l { 1.0 } else { -1.0 };
    let dir = if rail { -away } else { away };
    from_oklch(GLINT_DL.mul_add(dir, l).clamp(0.0, 1.0), ch * 1.12, h)
}

/// The glint's lightness step in OkLab (ruling 242: 0.07–0.10).
const GLINT_DL: f64 = 0.085;

/// Whether a row's glint belongs in the rail band: its full-height step
/// away from the fill's words would read under ΔE 6 (a fill already near the
/// end of the scale its words are not on).
fn glint_needs_rail(c: &BandColors, fill: [u8; 3], inks: &MeterInks) -> bool {
    let _ = c;
    delta_e(inks.glint, fill) < 6.0
}

/// What a painted chip asks of the row's pixel raster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chip {
    /// No fill of its own: it rides the surface under it like a word.
    Rides,
    /// Its own fill on a metered row: the raster leaves its cells alone.
    Owns,
    /// OUTLINED (ruling 249): the raster's ground runs under it and a ring
    /// in `ring` with `inner` inside is drawn over that.
    Rings { ring: [u8; 3], inner: [u8; 3] },
}

/// One chip over its `width` cells from `col`: the pad cells carry the fill
/// (or, under High Contrast, the brackets), the text sits between them. A
/// lit Primary keeps its ink and weight and brightens its fill; every other
/// lit chip takes the hover fill and ink.
///
/// On a METERED row ([`Ground::meter`]) a chip must not vanish into the
/// meter under it: a Secondary (whose resting fill is the track) wears the
/// band's own `bar_bg`; a Primary is OUTLINED where the row is drawn to the
/// pixel (`outline`, ruling 249 — the owner: *"Outlined on meters"*): an
/// accent ring with the band's ground inside and its label in the accent,
/// so the bar is the row's only solid accent and 0–100 % reads cleanly; the
/// cells carry the inside's ground and the label, the ring is the raster's
/// ([`Chip::Rings`]). Without the pixel raster a metered Primary wears the
/// band's ground with the accent ink (the ring's inside, unringed).
/// `Details ›` and every High Contrast chip carry no fill of their own and
/// ride the meter like words. A lit chip keeps its hover fill: the pointer
/// is on it.
#[allow(
    clippy::too_many_arguments,
    reason = "a chip's paint reads the row, its layout, the palette, the look, the hover, the ground under it and whether it may be outlined"
)]
fn paint_capsule(
    row: &mut [RenderCell],
    cols: usize,
    cap: &CapsuleLayout,
    c: &BandColors,
    hc: bool,
    lit: bool,
    on: &Ground<'_>,
    outline: bool,
) -> Chip {
    if cap.width < 2 {
        return Chip::Rides;
    }
    let last = cap.col + cap.width - 1;
    let (open, close) = if hc { ('[', ']') } else { (' ', ' ') };
    let metered = on.meter.is_some();
    // No fill of its own: every cell rides the surface under it, like a word —
    // and on a metered row a resting Secondary too (design ruling 260): a
    // block of the chip's ground cut the bar the row is (`Open log` over the
    // green at 95 %), so the label rides the fill and the track like
    // `Details ›`, its ink split at the fill's edge, in the value ink.
    let rides = !lit
        && (hc
            || cap.role == CapsuleRole::Details
            || (metered && cap.role == CapsuleRole::Secondary));
    if rides && metered {
        let ink = if hc || cap.role == CapsuleRole::Secondary {
            c.value
        } else {
            c.label
        };
        let bold = hc && cap.role == CapsuleRole::Primary;
        if cap.col < cols {
            row[cap.col] =
                chrome_band::cell(open, on.ink(cap.col, ink), on.at(cap.col), false, false);
        }
        on.write(row, cols, cap.col + 1, &cap.text, ink, bold);
        if last < cols {
            row[last] = chrome_band::cell(close, on.ink(last, ink), on.at(last), false, false);
        }
        return Chip::Rides;
    }
    let ringed = !hc && metered && cap.role == CapsuleRole::Primary;
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
    } else if ringed {
        let o = outlined_inks(c, lit);
        (o.label, o.inner, true)
    } else {
        chip_inks(c, cap.role, lit, metered)
    };
    if cap.col < cols {
        row[cap.col] = chrome_band::cell(open, fg, bg, false, false);
    }
    write_str(row, cols, cap.col + 1, &cap.text, fg, bg, bold);
    if last < cols {
        row[last] = chrome_band::cell(close, fg, bg, false, false);
    }
    match (ringed && outline, metered) {
        (true, _) => {
            let o = outlined_inks(c, lit);
            Chip::Rings {
                ring: o.ring,
                inner: o.inner,
            }
        }
        (false, true) => Chip::Owns,
        (false, false) => Chip::Rides,
    }
}

/// An OUTLINED Primary's inks (ruling 249): the ring, the ground inside it
/// and the label on that ground.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OutlinedInks {
    pub ring: [u8; 3],
    pub inner: [u8; 3],
    pub label: [u8; 3],
}

/// The inks of an outlined Primary on a metered row (ruling 249). At rest:
/// the ring is the row's accent — the meter's hue, already 3:1 on the band,
/// or the theme's blue where that hue floored for its label reads brown
/// ([`chrome_band::BandColors::ring`], ruling 260) —
/// the inside is the band's own ground, and the label is that accent moved
/// in linear light, its hue kept, by the least amount that reads at AA on the
/// inside ([`keep_side`]). Lit: the ring lifts [`chrome_band::PRIMARY_HOVER_LIFT`]
/// toward the theme's ink (the lit solid Primary's lift, ruling 160), the
/// inside takes up to an [`OUTLINE_LIT_TINT`] tint of the accent — the most
/// its label still clears AA on — and the ring holds 3:1 on it.
pub(crate) fn outlined_inks(c: &BandColors, lit: bool) -> OutlinedInks {
    // The lit tint steps back toward the band until the label can clear AA
    // on it (a near-white accent on a dark band tints the inside toward a
    // grey no ink reads on at full strength); a tint of 0 is the band.
    let (inner, label) = (0..=6u8)
        .rev()
        .map(|k| {
            let t = if lit {
                OUTLINE_LIT_TINT * f64::from(k) / 6.0
            } else {
                0.0
            };
            let inner = lin_mix(c.bar_bg, c.ring, t);
            (inner, keep_side(c.ring, inner, WORD_AA))
        })
        .find(|&(inner, label)| chrome_band::contrast(label, inner) >= WORD_AA)
        .unwrap_or((c.bar_bg, keep_side(c.ring, c.bar_bg, WORD_AA)));
    let ring = if lit {
        chrome_band::mix3(
            c.ring,
            c.primary_lift_toward,
            chrome_band::PRIMARY_HOVER_LIFT,
        )
    } else {
        c.ring
    };
    OutlinedInks {
        ring: keep_side(ring, inner, 3.0),
        inner,
        label,
    }
}

/// How far a lit outlined Primary's inside moves from the band toward the
/// accent, in linear light: a tint that says "under the pointer" and leaves
/// the bar the only solid accent on the row.
const OUTLINE_LIT_TINT: f64 = 0.18;

/// A chip's `(ink, fill, bold)` off High Contrast, every label at AA on its
/// own fill (design ruling 155) — over the band, the track or the fill,
/// resting or lit. A Primary keeps its polarity, the band's ink on the
/// accent: where that pair falls short (3.69:1 on Solarized Light, 4.28:1 on
/// GitHub Light) the ACCENT deepens in linear light, its hue kept, by the
/// least amount that clears AA ([`keep_side`]) — flooring the ink instead
/// turned Solarized's label black on blue. The lit Primary is lifted from it
/// and deepened the same way (ruling 160). A Primary on a metered row is
/// OUTLINED instead ([`outlined_inks`], ruling 249). Every other label is
/// lifted to AA by `ensure_contrast_either`.
fn chip_inks(
    c: &BandColors,
    role: CapsuleRole,
    lit: bool,
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
        (CapsuleRole::Primary, false) => (c.bar_bg, accent, true),
        (CapsuleRole::Secondary, false) if metered => (c.value, c.bar_bg, false),
        (CapsuleRole::Secondary, false) => (c.value, c.chip_ground, false),
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
    /// or the overflow row's words (the painter's `Body` arm lights the row's
    /// link capsule, and the overflow row's words). `None` off the band and on
    /// the presence row.
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

/// How far, in LINEAR light (0–1 per channel), colour `p` lies from the
/// triangle `corners` — the convex hull of three inks (ruling 244: every
/// pixel of a Fault echo is a mix of the track, the fill and warn, never a
/// fourth colour on the way between them). 0 inside it; a byte's rounding
/// is under 0.003.
#[cfg(test)]
pub(crate) fn hull_distance(p: [u8; 3], corners: [[u8; 3]; 3]) -> f64 {
    let v = |c: [u8; 3]| c.map(lin);
    let (p, a, b, c) = (v(p), v(corners[0]), v(corners[1]), v(corners[2]));
    let sub = |x: [f64; 3], y: [f64; 3]| [x[0] - y[0], x[1] - y[1], x[2] - y[2]];
    let dot = |x: [f64; 3], y: [f64; 3]| x[0] * y[0] + x[1] * y[1] + x[2] * y[2];
    let seg = |x: [f64; 3], y: [f64; 3]| {
        let d = sub(y, x);
        let len = dot(d, d);
        let t = if len > 0.0 {
            (dot(sub(p, x), d) / len).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let q = [x[0] + t * d[0], x[1] + t * d[1], x[2] + t * d[2]];
        dot(sub(p, q), sub(p, q)).sqrt()
    };
    let (e0, e1, w) = (sub(b, a), sub(c, a), sub(p, a));
    let (d00, d01, d11) = (dot(e0, e0), dot(e0, e1), dot(e1, e1));
    let det = d00.mul_add(d11, -(d01 * d01));
    if det.abs() > 1e-12 {
        let (d20, d21) = (dot(w, e0), dot(w, e1));
        let u = d11.mul_add(d20, -(d01 * d21)) / det;
        let t = d00.mul_add(d21, -(d01 * d20)) / det;
        if u >= 0.0 && t >= 0.0 && u + t <= 1.0 {
            let q = [
                a[0] + u * e0[0] + t * e1[0],
                a[1] + u * e0[1] + t * e1[1],
                a[2] + u * e0[2] + t * e1[2],
            ];
            return dot(sub(p, q), sub(p, q)).sqrt();
        }
    }
    seg(a, b).min(seg(b, c)).min(seg(a, c))
}

/// The `(ink, ground)` pairs under column `x`'s glyph as the renderer draws
/// it (ruling 242): pixel by pixel over the row's raster where it has one —
/// the fill side's ink left of the split, the cell's own right of it, the
/// edge's one antialiased pixel (the line itself) skipped, and a pastel
/// fill's darker edge line with it (ruling 264) — else the cell's own ink on
/// its own ground.
#[cfg(test)]
pub(crate) fn word_grounds(
    row: &[RenderCell],
    raster: Option<&RowRaster>,
    geom: BandGeometry,
    x: usize,
) -> Vec<([u8; 3], [u8; 3])> {
    let cell = &row[x];
    // A chip's own fill, or an outlined Primary's inside (ruling 249).
    let owned = raster.is_some_and(|r| {
        r.own
            .iter()
            .any(|&(a, b)| (usize::from(a)..usize::from(b)).contains(&x))
            || r.rings
                .iter()
                .any(|&(a, b, ..)| (usize::from(a)..usize::from(b)).contains(&x))
    });
    match raster.filter(|r| !r.ground.is_empty() && !owned) {
        Some(r) => {
            let x0 = geom.cells_x + x * geom.cell_w;
            (x0..x0 + geom.cell_w)
                .filter(|&px| {
                    r.edge != Some(px as u32)
                        && !r.line.is_some_and(|(a, b)| (a..b).contains(&(px as u32)))
                        && px < r.ground.len()
                })
                .map(|px| {
                    let ink = match r.split {
                        Some((col, sx, ink, _)) if usize::from(col) == x && (px as u32) < sx => ink,
                        _ => cell.fg,
                    };
                    (ink, r.ground[px])
                })
                .collect()
        }
        None => vec![(cell.fg, cell.bg)],
    }
}

/// The worst contrast any word on `row` has on the ground under its glyph
/// ([`word_grounds`]), and where.
#[cfg(test)]
pub(crate) fn worst_word_contrast(
    row: &[RenderCell],
    raster: Option<&RowRaster>,
    geom: BandGeometry,
) -> (f64, String) {
    let mut worst = (f64::INFINITY, String::new());
    for (x, cell) in row.iter().enumerate() {
        if cell.ch == ' ' {
            continue;
        }
        for (ink, g) in word_grounds(row, raster, geom, x) {
            let ratio = chrome_band::contrast(ink, g);
            if ratio < worst.0 {
                worst = (ratio, format!("col {x} {:?}: {ink:?} on {g:?}", cell.ch));
            }
        }
    }
    worst
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
        // Beside a percent the job's size says what it is part of (ruling
        // 259, amending 246): the count, and the bytes as part of the total.
        assert!(top.contains("3 of 10 · 512 MB of 1.2 GB"), "{top}");
        assert!(
            top.ends_with("  Packages   Details \u{203a}"),
            "the authored capsule, then the row's link: {top}"
        );
        let bottom = text_of(&rows[1]);
        assert!(bottom.contains("aterm update v0.48.0"), "{bottom}");
        assert!(bottom.contains("60%"), "{bottom}");
        assert!(
            bottom.contains("45 of 74 MB")
                && !bottom.contains("45 MB")
                && bottom.ends_with("Details \u{203a}"),
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
        // Both sit on the meter's fill (ruling 55): each wears the row's
        // CRISP ink (ruling 222) — the band's own ink that reads best on the
        // fill — floored against it.
        let fill = rows[0][MARGIN].bg;
        assert_eq!(fill, c.accent, "the glyph sits on the fill");
        let crisp = floor_word(fill_ink(&c, c.accent), fill, fill_anchor(c.accent));
        assert_eq!(rows[0][MARGIN].fg, crisp, "the glyph wears the crisp ink");
        assert_eq!(
            rows[0][p.rows[0].title.0].fg, crisp,
            "the title wears the crisp ink"
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
                    // Row 0 is METERED (ruling 55): the quiet chip draws no
                    // ground of its own (ruling 260) — it rides the meter like
                    // `Details ›`, in the value ink floored on the track under
                    // it at this fill — and `Details ›` rides it too.
                    CapsuleRole::Secondary => {
                        assert_eq!(rows[0][cap.col].bg, c.meter_track);
                        assert_eq!(
                            rows[0][cap.col + 1].fg,
                            floor_word(c.value, c.meter_track, words_anchor(&c))
                        );
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
    /// `… 5 more ›` in `label`, not bold — ONE link with no capsule of its own
    /// (ruling 259) — and a press ANYWHERE on it is that one link.
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
        assert_eq!(text.trim_end(), " \u{2026} 5 more \u{203a}", "{text}");
        assert!(p.rows[2].capsules.is_empty(), "the words are the link");
        let c = chrome_band::band_colors(Theme::default());
        assert_eq!(rows[2][MARGIN].fg, c.label);
        assert!(!rows[2][MARGIN].bold);
        assert_eq!(rows[2][p.rows[2].title.0].fg, c.label);
        assert!(!rows[2][p.rows[2].title.0].bold);
        for col in 0..80 {
            assert_eq!(p.hit(2, col), Hit::Overflow, "col {col}");
        }
        // A pointer anywhere on the row lifts its words to the value ink.
        let lit = paint_still(
            &p,
            Theme::default(),
            Some(BandHover {
                row: 2,
                target: HoverTarget::Body,
            }),
            &center,
        );
        assert_eq!(lit[2][p.rows[2].title.0].fg, c.value, "the link lights");
        assert_eq!(lit[2][p.rows[2].title.0].bg, c.bar_bg, "no chip appears");
        assert_eq!(lit[1][MARGIN].bg, c.bar_bg, "the row above does not");
    }

    /// The stack's LAST row carries the seam and every row above it the
    /// DIVIDER (ruling 260: the seam's ink at 35 % over the band), each in one
    /// tone — whatever shape the splice composed: painted rows, a padded blank
    /// row under them, or a single row.
    #[test]
    fn the_stack_is_sealed_on_its_last_row_only() {
        let theme = Theme::default();
        let colors = chrome_band::band_colors(theme);
        let ink = colors.label;
        let divider = chrome_band::mix3(colors.bar_bg, colors.label, DIVIDER_ALPHA);
        assert!(
            chrome_band::contrast(divider, colors.bar_bg) > 1.2,
            "it separates"
        );
        assert!(
            chrome_band::contrast(divider, colors.bar_bg)
                < chrome_band::contrast(ink, colors.bar_bg),
            "softer than the seam"
        );
        let ruled = |row: &[RenderCell], tone: [u8; 3]| {
            row.iter()
                .all(|c| c.underline == UnderlineStyle::Single && c.underline_color == Some(tone))
        };
        let sealed = |row: &[RenderCell]| ruled(row, ink);
        let divided = |row: &[RenderCell]| ruled(row, divider);
        let bare = |row: &[RenderCell]| row.iter().all(|c| c.underline == UnderlineStyle::None);

        let center = two_rows();
        let mut rows = paint_still(&present(&center, 100), theme, None, &center);
        assert!(rows.iter().all(|r| bare(r)), "the painter seals nothing");
        seal_stack(&mut rows, theme);
        assert!(divided(&rows[0]) && sealed(&rows[1]));

        // PADDED: the geometry still owns a third row after a dismissal. The seam
        // moves down to it; the row that used to be last takes the divider.
        rows.push(blank_band_row(100, theme));
        seal_stack(&mut rows, theme);
        assert!(divided(&rows[0]) && divided(&rows[1]) && sealed(&rows[2]));

        // A single row seals itself; an empty stack is left alone.
        let mut one = vec![blank_band_row(100, theme)];
        seal_stack(&mut one, theme);
        assert!(sealed(&one[0]));
        seal_stack(&mut [], theme);
    }

    /// An outlined capsule's FLOOR is its middle cells' underline in the
    /// ring's colour (ruling 254), on any row. On the sealed last row the
    /// seam hands the pill's two end cells to the ring, which draws it there
    /// to meet its rounded foot; on a row above, the floor is drawn with no
    /// seam beside it. The cells outside a ring are untouched.
    #[test]
    fn a_ringed_capsule_takes_the_seam_as_its_floor() {
        let theme = Theme::default();
        let ink = chrome_band::band_colors(theme).label;
        let mut rows = vec![blank_band_row(40, theme), blank_band_row(40, theme)];
        seal_stack(&mut rows, theme);
        let ring = |row: u16, start: u16, end: u16, rgb: u32| aterm_render::ChromeRaster {
            row,
            ground: std::sync::Arc::from(Vec::new()),
            rail: std::sync::Arc::from(Vec::new()),
            rail_h: 0,
            clear_rail: false,
            own: Vec::new(),
            split: None,
            rings: vec![aterm_render::ChromeRing {
                start,
                end,
                ring: rgb,
                inner: 0x0030_3135,
                seam: None,
            }],
            icons: Vec::new(),
        };
        let mut rasters = [
            ring(0, 3, 9, 0x00BD_93F9),
            ring(1, 20, 32, 0x0050_FA7B),
            ring(7, 0, 5, 0),
        ];
        floor_rings(&mut rows, &mut rasters);
        // The row above carries the DIVIDER (ruling 260): the ring meets it
        // at its ends as it meets the seam on the last row.
        let colors = chrome_band::band_colors(theme);
        let divider = chrome_band::mix3(colors.bar_bg, colors.label, DIVIDER_ALPHA);
        for (c, cell) in rows[0].iter().enumerate() {
            let (style, tone) = match c {
                3 | 8 => (UnderlineStyle::None, None),
                4..=7 => (UnderlineStyle::Single, Some([0xBD, 0x93, 0xF9])),
                _ => (UnderlineStyle::Single, Some(divider)),
            };
            assert_eq!(
                (cell.underline, cell.underline_color),
                (style, tone),
                "upper row {c}"
            );
        }
        let [r, g, b] = divider;
        assert_eq!(
            rasters[0].rings[0].seam,
            Some((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)),
            "the ring draws the divider across its ends"
        );
        let [r, g, b] = ink;
        assert_eq!(
            rasters[1].rings[0].seam,
            Some((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)),
            "the ring draws the seam across its ends"
        );
        for (c, cell) in rows[1].iter().enumerate() {
            let (style, tone) = match c {
                20 | 31 => (UnderlineStyle::None, None),
                21..=30 => (UnderlineStyle::Single, Some([0x50, 0xFA, 0x7B])),
                _ => (UnderlineStyle::Single, Some(ink)),
            };
            assert_eq!(
                (cell.underline, cell.underline_color),
                (style, tone),
                "last row {c}"
            );
        }
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
            none[meter][packages.col].bg, c.meter_track,
            "at rest the quiet chip on a METERED row draws no ground: it rides the meter (ruling 260)"
        );
        assert_eq!(
            none[ask][not_now.col].bg, c.chip_ground,
            "on an unmetered row the quiet chip keeps its ground"
        );
        assert!(
            chrome_band::contrast(c.capsule_hover, c.chip_ground) >= chrome_band::HOVER_RISE,
            "the pointer's step is plain to see (ruling 260)"
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
                    let (rows, edges, rasters) = paint_rows_on(&p, theme, None, g, &motion);
                    for (i, (row, layout)) in rows.iter().zip(&p.rows).enumerate() {
                        let (ratio, at) = worst_word_contrast(row, rasters[i].as_ref(), g);
                        assert!(
                            ratio >= WORD_AA - 0.02,
                            "{name}@{cols} row {i}: {ratio:.2}:1 at {at}"
                        );
                        let (mcol, w, _) = layout.meter.expect("both rows are metered");
                        assert_eq!((mcol, w), (0, cols), "{name}@{cols}: the meter is the row");
                        let span = MeterSpan { geom: g, cols };
                        let inks = MeterInks::of(&c, c.accent);
                        // Every cell is the meter's, a resting Secondary's
                        // too: it draws no ground of its own (ruling 260).
                        for (x, cell) in row.iter().enumerate() {
                            let want = fine_rgb(span.fine(&motion.rows[i].surface, x), &inks);
                            assert_eq!(cell.bg, want, "{name}@{cols} row {i} col {x}");
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

    /// A PRIMARY chip on a meter is OUTLINED wherever the fill is (ruling
    /// 249, the owner: "Outlined on meters"): the band's ground inside, the
    /// label in the accent at AA on it, over the fill and over the track
    /// alike — the accent chip on the accent fill would be no chip at all,
    /// and a solid one over the track read as a second bar.
    #[test]
    fn a_primary_chip_on_a_meter_is_outlined_wherever_the_fill_is() {
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
        let o = outlined_inks(&c, false);
        for fill in [1000u16, 10] {
            let center = post(fill);
            let p = present(&center, cols);
            let cap = p.rows[0]
                .capsules
                .iter()
                .find(|cap| cap.role == CapsuleRole::Primary)
                .expect("New window is consequential: a Primary");
            let rows = paint_still(&p, theme, None, &center);
            let cell = &rows[0][cap.col + 1];
            assert_eq!((cell.fg, cell.bg), (o.label, c.bar_bg), "fill {fill}");
            assert!(chrome_band::contrast(o.label, c.bar_bg) >= WORD_AA);
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
    /// overflow row's words end in its link's `›` (ruling 259), through the host's own presentation
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
            // The overflow row IS its link (ruling 259): its words end in
            // `›`, no capsule of its own.
            assert!(p.rows[2].capsules.is_empty(), "cols {cols}");
            assert!(
                text_of(&painted[2]).trim_end().ends_with('\u{203a}'),
                "cols {cols}: the overflow row's words are the link"
            );
            for (r, row) in p.rows.iter().enumerate().take(2) {
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
                assert_eq!(link.text, "Details \u{203a}", "whole, or gone");
                assert_eq!(link.role, CapsuleRole::Details);
                assert_eq!(link.full_label, "Details \u{203a}", "cols {cols} row {r}");
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
    /// The first width each form of the Universal Control pointer shows at.
    const PINNED_UC_FORMS: [(usize, &str); 5] = [
        (90, "`aterm pkg machine`\u{2026}"),
        (97, "`aterm pkg machine` prints\u{2026}"),
        (101, "`aterm pkg machine` prints the\u{2026}"),
        (107, "`aterm pkg machine` prints the revert"),
        (113, "undo: `aterm pkg machine` prints the revert"),
    ];

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
                row.title.1, "ALab tools turned off Universal Control",
                "cols {cols}: whole"
            );
            let Some((_, excerpt)) = row.detail else {
                // The title grew thirteen cells (`ALab tools turned off
                // Universal Control`, ruling 261): every threshold with it.
                assert!(cols <= 80, "the excerpt is dropped below 90 cols only");
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
        assert_eq!(shown, [90, 100, 130, 150]);
        // Where each form of the excerpt first shows, as the row widens.
        let mut firsts: Vec<(usize, String)> = Vec::new();
        for cols in 80..=140 {
            if let Some((_, d)) = app.band_presentation(cols).rows[0].detail.clone()
                && firsts.last().is_none_or(|(_, last)| *last != d)
            {
                firsts.push((cols, d));
            }
        }
        let firsts: Vec<(usize, &str)> = firsts.iter().map(|(c, d)| (*c, d.as_str())).collect();
        assert_eq!(firsts, PINNED_UC_FORMS);
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
        // The title is three cells shorter than it was (`are current`, ruling
        // 261): every threshold with it.
        assert_eq!(excerpt(77), None, "drops whole at 77 cols");
        assert_eq!(
            excerpt(107).map(|(_, d)| d),
            Some("`. ~/.aterm/shell.d/00-atp\u{2026}".to_string()),
            "at 107 the room is narrower than the command: its head, from the backtick"
        );
        assert_eq!(
            excerpt(117).map(|(_, d)| d),
            Some(command.to_string()),
            "at 117 the command is whole"
        );
        assert_eq!(
            excerpt(140).map(|(_, d)| d),
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
        assert_eq!(whole_from, Some(113));
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

    /// [`frame_at`] with each row's pixel raster (on the unit grid).
    fn frame_rasters(
        center: &MessageCenter,
        p: &Presentation,
        at: Instant,
        look: Look,
        theme: Theme,
    ) -> (Vec<Vec<RenderCell>>, Vec<Option<RowRaster>>) {
        let m = center.motion(p, at, look);
        let (rows, _, rasters) =
            paint_rows_on(p, theme, None, BandGeometry::cells_only(p.cols), &m);
        (rows, rasters)
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

    /// AN ERROR READS RED ON THE BAND (design ruling 265): its glyph and words
    /// wear the band's error ink, a warning's the warn ink — they shared the
    /// warn yellow, so only the glyph told `✕ Tests failed on main` from `⚠
    /// Misspelled setting`, while the log paints errors red. Both clear AA on
    /// the band.
    #[test]
    fn an_error_row_reads_red_and_a_warning_amber() {
        let now = t0();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        center.post(
            Message::new(tags::SYSTEM, Severity::Error, "Tests failed on main"),
            stamp(),
            now,
        );
        center.post(
            Message::new(tags::CONFIG, Severity::Warn, "Misspelled setting"),
            stamp(),
            now,
        );
        assert_eq!(center.commit_rows(now, 3), Some(2));
        let p = center.presentation(120, &aterm_messages::text::char_width, None, Links::Painted);
        let motion = center.motion(&p, now, Look::STILL);
        let light = Theme {
            fg: 0x0020_2020,
            bg: 0x00FA_FAFA,
            cursor: 0x0020_2020,
            selection: 0x00C0_C8FF,
        };
        for theme in [Theme::default(), light] {
            let c = chrome_band::band_colors(theme);
            let rows = paint_rows(&p, theme, None, &motion);
            let ink = |row: usize| rows[row][aterm_messages::TITLE_COL].fg;
            assert_eq!(p.rows[0].severity, Severity::Error);
            assert_eq!(ink(0), c.error, "the error's words");
            assert_eq!(rows[0][GLYPH_COL].fg, c.error, "the error's glyph");
            assert_eq!(ink(1), c.warn, "the warning's words");
            assert_ne!(c.error, c.warn);
            assert!(chrome_band::contrast(c.error, c.bar_bg) >= 4.5);
            let [r, g, b] = c.error;
            assert!(r > g && r > b, "red: {:?}", c.error);
        }
    }

    /// The frames a test walks: every 50 ms from `from` for `span`.
    fn frames(from: Instant, span: aterm_messages::Duration) -> impl Iterator<Item = Instant> {
        let steps = u64::try_from(span.as_millis() / 50).unwrap_or(0);
        (0..=steps).map(move |k| from + aterm_messages::Duration::from_millis(k * 50))
    }

    /// Whether column `x` of a row carries words a motion frame may change:
    /// the elapsed and ETA slots and the glyph cell.
    fn in_motion_span(l: &RowLayout, x: usize) -> bool {
        l.elapsed
            .is_some_and(|c| (c..c + l.elapsed_width()).contains(&x))
            || l.eta.is_some_and(|c| (c..c + l.eta_width()).contains(&x))
            || x == aterm_messages::GLYPH_COL
    }

    /// A MOTION FRAME MOVES ONLY THE SURFACE, THE TIME SLOTS AND THE GLYPH
    /// (design §10.7; rulings 138, 140 — re-pinned from the pill's "the
    /// overlay writes only its spans" on the merge: the meter is now the whole
    /// row, so its surface may move under every cell). Across 12 s of frames —
    /// the comet's crossings, the glint, the elapsed words
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

    /// THE BUSY ROW DRAWS A COMET, OR HOLDS STILL — on the full-width
    /// meter's geometry (main's ruling 75, re-pinned on the engine's motion
    /// by rulings 139–140). The engine lays the track out as the whole row
    /// and computes the frame; the host paints it: moving, the comet's tones
    /// over the track with the row's own glyph in its cell (the braille
    /// spinner went with ruling 251: the comet already says the work moves);
    /// still, the row's own glyph over an unlit track. Every glyph clears AA on the cell under it at every
    /// frame, the words never move, and a row that is not busy draws the same
    /// in both looks.
    #[test]
    fn a_busy_row_draws_a_comet_or_holds_still() {
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
                .all(|cell| cell.bg == c.chip_ground || cell.bg == c.bar_bg),
            "still: the unlit track (a comet's channel is neutral, ruling 260)"
        );
        // Moving: the row's own glyph (no spinner beside the comet, ruling
        // 251), and the comet somewhere on the row.
        let mut lit_frames = 0;
        let steps =
            aterm_messages::COMET_PERIOD.as_millis() / aterm_messages::ANIM_FRAME.as_millis();
        for k in 0..u32::try_from(steps).unwrap() {
            let t = now + aterm_messages::ANIM_FRAME * k;
            let (rows, _) = frame_at(&center, &p, t, Look::MOVING, theme);
            let g = rows[0][aterm_messages::GLYPH_COL].ch;
            assert_eq!(g, layout.glyph.1, "frame {k}: the row's own glyph");
            if rows[0]
                .iter()
                .any(|cell| cell.bg != c.chip_ground && cell.bg != c.bar_bg)
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
    /// and every word — and the glyph cell's icon — stays on that anchor's side of the
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
                floor_word(fill_ink(&c, c.accent), bg, fill_anchor(c.accent))
            }
        };
        let now = t0();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        let id = center.post(download(10_000_000), stamp(), now).id;
        center.commit_rows(now, 3);
        let (words, _, _, _) = slot(&center, now + aterm_messages::Duration::from_millis(4200));
        assert_eq!(
            words, "",
            "a hidden estimate leaves the slot blank (ruling 241)"
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
        // Delivered: the ✓ and the finished words only — the slot says
        // nothing (ruling 244: no `100% done`).
        assert!(center.resolve(id, aterm_messages::Outcome::Ok, t));
        let (words, _, _, _) = slot(&center, t + aterm_messages::Duration::from_millis(100));
        assert_eq!(words, "");
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
            at(4, Look::MOVING),
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
            } else if oklch(c.accent).1 > 2.0 * 0.03 {
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
            let rest = chip_inks(&c, CapsuleRole::Primary, false, false).1;
            let lit = chip_inks(&c, CapsuleRole::Primary, true, false).1;
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

    /// EVERY GLYPH ON A METERED ROW CLEARS AA ON ITS OWN CELL, IN EVERY
    /// FRAME (design ruling 199): on every builtin scheme, a row with a
    /// Primary, a Secondary and `Details ›` — its fill's edge swept under
    /// every chip cell, then through the glint, the Complete wipe and bloom
    /// and the Fault flash — never paints a glyph under 4.5:1 on the cell
    /// it sits on. The static chip test held the fill at 2 % and 100 %, so
    /// the edge cell under a chip and the moving frames were never read,
    /// and the title test allowed 0.01 under the floor: 4.49:1 passed.
    #[test]
    fn every_glyph_on_a_metered_row_clears_aa_in_every_frame() {
        use aterm_messages::{
            ANIM_FRAME, Duration, ECHO_FAULT_FLASH, ECHO_SWEEP, GLINT_DELAY, GLINT_TRAVEL, Outcome,
        };
        let cols = 120;
        // On the ground under each glyph as it is drawn (ruling 242).
        let check = |name: &str, what: &str, (row, raster): (&[RenderCell], Option<&RowRaster>)| {
            let (ratio, at) = worst_word_contrast(row, raster, BandGeometry::cells_only(row.len()));
            assert!(
                ratio >= WORD_AA - 0.02,
                "{name} {what}: {ratio:.3}:1 at {at}"
            );
        };
        let post = |center: &mut MessageCenter, permille: u16, now: Instant| {
            center
                .post(
                    Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.91.0")
                        .line("detail")
                        .action(Intent::NewWindow)
                        .action(Intent::OpenSettings {
                            route: "/packages".into(),
                        })
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
                .id
        };
        for (name, theme) in builtin_themes() {
            // The fill's edge under every column, the chips' included.
            for permille in (0..=1000u16).step_by(4) {
                let now = t0();
                let mut center = MessageCenter::new(MessageLog::empty(), now);
                post(&mut center, permille, now);
                center.commit_rows(now, 3);
                let p = present(&center, cols);
                assert_eq!(p.rows[0].capsules.len(), 3, "{name}: the chips are drawn");
                let (rows, rasters) = frame_rasters(&center, &p, t0(), Look::STILL, theme);
                check(
                    name,
                    &format!("still {permille}"),
                    (&rows[0], rasters[0].as_ref()),
                );
            }
            // The glint's travel, the Complete echo, the Fault flash.
            for (permille, outcome) in [
                (570, None),
                (960, None),
                (800, Some(Outcome::Ok)),
                (400, Some(Outcome::Warn)),
            ] {
                let now = t0();
                let mut center = MessageCenter::new(MessageLog::empty(), now);
                let id = post(&mut center, permille, now);
                center.commit_rows(now, 3);
                let (from, span) = match outcome {
                    None => (now + GLINT_DELAY, GLINT_TRAVEL),
                    Some(o) => {
                        let at = now + Duration::from_millis(1000);
                        assert!(center.resolve(id, o, at));
                        let span = if o == Outcome::Ok {
                            aterm_messages::animate::glide_span(800, 1000) + ECHO_SWEEP
                        } else {
                            ECHO_FAULT_FLASH
                        };
                        (at, span)
                    }
                };
                let p = present(&center, cols);
                let mut t = Duration::ZERO;
                while t <= span {
                    let (rows, rasters) = frame_rasters(&center, &p, from + t, Look::MOVING, theme);
                    check(
                        name,
                        &format!("{permille} {outcome:?} t={t:?}"),
                        (&rows[0], rasters[0].as_ref()),
                    );
                    t += ANIM_FRAME;
                }
            }
        }
    }

    // (Ruling 156's OkLCh warming — `the_fault_echo_warms_straight_to_the_fault_hue`
    // — is withdrawn by ruling 244: the Fault is a premultiplied cross-fade
    // that stays inside the track–fill–warn hull, pinned pixel by pixel in
    // `band_raster_tests::the_fault_echo_stays_inside_its_hull`.)

    /// THE WORDS ON A DETERMINATE BAR NEVER JUMP (design ruling 158): on
    /// every builtin scheme, through the glint's travel over a bar at 57 %,
    /// the Complete echo's wipe and bloom from 80 % and the Fault echo's
    /// flash from 40 %, every letter of the title keeps its side of the
    /// ground under it (the glint, the bloom and the Fault's warn are held
    /// there — ruling 222 withdrew the Fault's one crossing on Solarized
    /// Light), clears AA, and moves its ink by no more than the ground under
    /// it moved — the old
    /// ten-step floor jumped a whole step on a one-level change in the
    /// bloom, and the glint flipped Catppuccin Latte's letters to white.
    #[test]
    fn the_words_on_a_determinate_bar_never_jump() {
        use aterm_messages::{
            ANIM_FRAME, Duration, ECHO_FAULT_FLASH, ECHO_SWEEP, GLINT_DELAY, GLINT_TRAVEL, Outcome,
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
                        aterm_messages::animate::glide_span(800, 1000) + ECHO_SWEEP
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
            // No crossing even through a Fault flash: its warn is held on the
            // words' side (ruling 222 withdrew ruling 158's one crossing).
            let crossings = 0u32;
            let mut t = Duration::ZERO;
            while t <= span {
                let (rows, rasters) = frame_rasters(&center, &p, from + t, Look::MOVING, theme);
                let row = rows[0].clone();
                let geom = BandGeometry::cells_only(cols);
                for &x in &letters {
                    let (fg, bg) = (row[x].fg, row[x].bg);
                    for (ink, g) in word_grounds(&row, rasters[0].as_ref(), geom, x) {
                        assert!(
                            chrome_band::contrast(ink, g) >= WORD_AA - 0.02,
                            "{name} {outcome:?} t={t:?} col {x}: {ink:?} on {g:?} under AA"
                        );
                    }
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

    /// CRISP INK ON FILLS (design ruling 222): on every builtin scheme and in
    /// every frame of a determinate row's life — its glide across the words,
    /// the glint's travel, the Complete echo's wipe and bloom and the Fault
    /// echo's flash — every word cell over the fill wears the row's ONE crisp
    /// ink (the band's `bar_bg` or `value`, whichever reads better on the
    /// resting fill): it clears 4.5:1 always, 7:1 on the resting fill wherever
    /// either candidate reaches it there, and it never changes side of the
    /// ground under it — no letter flips as the edge, the glint or an echo
    /// passes.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one row's life, frame by frame, on every scheme"
    )]
    fn words_on_a_fill_wear_the_rows_crisp_ink_in_every_frame() {
        use aterm_messages::{
            ANIM_FRAME, Duration, ECHO_FAULT_FLASH, ECHO_SWEEP, GLINT_DELAY, GLINT_TRAVEL, Outcome,
        };
        let cols = 120;
        let mut sevens = 0;
        let themes = builtin_themes();
        for (name, theme) in &themes {
            let (name, theme) = (*name, *theme);
            let c = chrome_band::band_colors(theme);
            let crisp = fill_ink(&c, c.accent);
            let best = |bg: [u8; 3]| {
                [c.field_bg, c.bar_bg, c.value]
                    .map(|ink| chrome_band::contrast(ink, bg))
                    .into_iter()
                    .fold(0.0f64, f64::max)
            };
            if best(c.accent) >= WORD_AA {
                assert!(
                    (chrome_band::contrast(crisp, c.accent) - best(c.accent)).abs() < 1e-9,
                    "{name}: the crisp ink is the better candidate on the fill"
                );
            }
            if best(c.accent) >= 7.0 {
                sevens += 1;
            }
            let crisp_lighter = fill_anchor(c.accent) == WHITE;
            // (start fill, the life to run): a glide from 20 % to 95 %, the
            // glint at 95 %, a Complete echo from 95 %, a Fault echo from 95 %.
            for (life, outcome) in [
                ("glide", None),
                ("glint", None),
                ("complete", Some(Outcome::Ok)),
                ("fault", Some(Outcome::Warn)),
            ] {
                let now = t0();
                let mut center = MessageCenter::new(MessageLog::empty(), now);
                let start = if life == "glide" { 200 } else { 950 };
                let row = |fill: u16| {
                    Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.91.0")
                        .no_excerpt()
                        .meter(Meter {
                            fill_permille: Some(fill),
                            stats: "31 MB / 74 MB".into(),
                            ..Meter::default()
                        })
                        .hold(Hold::Live {
                            stale_after: aterm_messages::STALE_UPDATE,
                        })
                };
                let id = center.post(row(start), stamp(), now).id;
                center.commit_rows(now, 3);
                let (from, span) = match (life, outcome) {
                    ("glide", _) => {
                        let at = now + Duration::from_millis(500);
                        center.restate(
                            id,
                            aterm_messages::Restatement {
                                meter: Some(row(950).meter),
                                ..aterm_messages::Restatement::default()
                            },
                            at,
                        );
                        (at, Duration::from_millis(1500))
                    }
                    (_, None) => (now + GLINT_DELAY, GLINT_TRAVEL),
                    (_, Some(o)) => {
                        let at = now + Duration::from_millis(1000);
                        assert!(center.resolve(id, o, at));
                        let span = if o == Outcome::Ok {
                            aterm_messages::animate::glide_span(800, 1000) + ECHO_SWEEP
                        } else {
                            ECHO_FAULT_FLASH
                        };
                        (at, span)
                    }
                };
                let p = present(&center, cols);
                let l = &p.rows[0];
                // Every word cell on the row: glyph, title, percent, stats.
                let mut words: Vec<usize> = vec![l.glyph.0];
                let mut add = |col: usize, text: &str| {
                    for (k, ch) in text.chars().enumerate() {
                        if !ch.is_whitespace() {
                            words.push(col + k);
                        }
                    }
                };
                add(l.title.0, &l.title.1);
                if let Some((col, text)) = &l.pct {
                    add(*col, text);
                }
                if let Some((col, text)) = &l.stats {
                    add(*col, text);
                }
                let mut t = Duration::ZERO;
                while t <= span {
                    let (rows, m) = frame_at(&center, &p, from + t, Look::MOVING, theme);
                    let (_, rasters) = frame_rasters(&center, &p, from + t, Look::MOVING, theme);
                    let geom = BandGeometry::cells_only(cols);
                    let span_of = MeterSpan { geom, cols };
                    for &x in &words {
                        // Wholly on the fill: the cell the edge crosses wears
                        // the track's ink, split by the renderer (ruling 242).
                        let on_fill = m.rows[0].surface.is_empty()
                            || u32::from(span_of.fine(&m.rows[0].surface, x).fill)
                                + u32::from(span_of.fine(&m.rows[0].surface, x).warn)
                                >= 255 << 8;
                        let (fg, bg) = (rows[0][x].fg, rows[0][x].bg);
                        let at = format!("{name} {life} t={t:?} col {x}: {fg:?} on {bg:?}");
                        for (ink, g) in word_grounds(&rows[0], rasters[0].as_ref(), geom, x) {
                            let r = chrome_band::contrast(ink, g);
                            assert!(r >= WORD_AA - 0.02, "{at}: under AA ({r:.2}) on {g:?}");
                        }
                        let ratio = chrome_band::contrast(fg, bg);
                        if !on_fill || rows[0][x].ch == ' ' {
                            continue;
                        }
                        assert_eq!(
                            luminance(fg) > luminance(bg),
                            crisp_lighter,
                            "{at}: the word changed side over the fill"
                        );
                        if bg == c.accent {
                            let (inks, _) = row_inks(&c, l, false, false);
                            assert_eq!(
                                fg,
                                crisp_on_fill(&c, c.accent, &inks),
                                "{at}: the crisp ink on the fill"
                            );
                            assert!(
                                ratio >= best(bg).min(7.0) - 1e-9,
                                "{at}: {ratio:.2}:1 where a candidate reaches {:.2}",
                                best(bg)
                            );
                        }
                    }
                    t += ANIM_FRAME;
                }
            }
        }
        assert!(sevens > 0, "some scheme reaches 7:1 on its fill");
        // The default ground: the band's own dark on the neon cursor green.
        let c = chrome_band::band_colors(Theme::default());
        let crisp = chrome_band::contrast(fill_ink(&c, c.accent), c.accent);
        assert!(crisp >= 7.0, "the default ground's crisp ink: {crisp:.2}:1");
    }

    /// HIGH CONTRAST KEEPS ITS RAW FAULT FLASH (ruling 222's warn exemption):
    /// under every stock High Contrast palette, in the graded look, every
    /// flashed cell of a Fault echo over the fill is the engine's tone on the
    /// row's own inks — HIGHLIGHT warmed toward the raw warn, never pulled back
    /// toward the side the crisp ink's hold protects (its words wear
    /// HIGHLIGHTTEXT).
    #[test]
    fn high_contrast_keeps_its_raw_fault_flash() {
        use aterm_messages::{ANIM_FRAME, Duration, ECHO_FAULT_FLASH, Outcome};
        let cols = 120;
        for (name, palette) in chrome_band::hc_fixtures::STOCK {
            chrome_band::hc_fixtures::with_forced(palette, || {
                let theme = Theme::default();
                let c = chrome_band::band_colors(theme);
                let inks = MeterInks::of(&c, c.accent);
                let now = t0();
                let mut center = MessageCenter::new(MessageLog::empty(), now);
                let id = center
                    .post(
                        Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.91.0")
                            .no_excerpt()
                            .meter(Meter {
                                fill_permille: Some(950),
                                stats: "70 MB / 74 MB".into(),
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
                let at = now + Duration::from_millis(1000);
                assert!(center.resolve(id, Outcome::Warn, at));
                let p = present(&center, cols);
                let span = MeterSpan {
                    geom: BandGeometry::cells_only(cols),
                    cols,
                };
                let mut flashed = 0;
                let mut t = Duration::ZERO;
                while t <= ECHO_FAULT_FLASH {
                    let (rows, m) = frame_at(&center, &p, at + t, Look::MOVING, theme);
                    for (x, cell) in rows[0].iter().enumerate().take(cols) {
                        let tone = span.fine(&m.rows[0].surface, x);
                        if tone.fill < 128 << 8 || tone.warn == 0 {
                            continue;
                        }
                        flashed += 1;
                        assert_eq!(
                            cell.bg,
                            fine_rgb(tone, &inks),
                            "{name} t={t:?} col {x}: the raw flash"
                        );
                    }
                    t += ANIM_FRAME;
                }
                assert!(flashed > 0, "{name}: the flash crossed the fill");
            });
        }
    }

    /// A FAULT ECHO'S EDGE IS ITS COVERAGE OF THE WASH (rulings 158 and
    /// 244): the cell (or pixel) the fill only partly covers is the wash —
    /// the fill handing its coverage to warn — over the track by that
    /// coverage, in linear light, on no other hue.
    #[test]
    fn a_fault_echos_edge_cell_is_the_wash_by_its_coverage() {
        for (name, theme) in builtin_themes() {
            let c = chrome_band::band_colors(theme);
            let inks = MeterInks::of(&c, c.accent);
            for warn in [64u16, 160, 255] {
                let wash = |cover: u16| {
                    let w = cover * warn / 255;
                    fine_rgb(
                        aterm_messages::FineTone {
                            fill: (cover - w) << 8,
                            lift: 0,
                            warn: w << 8,
                        },
                        &inks,
                    )
                };
                let (full, edge) = (wash(255), wash(85));
                let expect = lin_mix(c.meter_track, full, 85.0 / 255.0);
                let off = (0..3)
                    .map(|k| edge[k].abs_diff(expect[k]))
                    .max()
                    .unwrap_or(0);
                assert!(
                    off <= 2,
                    "{name} warn {warn}: edge {edge:?}, the wash {full:?} by a third \
                     over {:?} is {expect:?}",
                    c.meter_track
                );
                assert!(
                    hull_distance(edge, [inks.track, inks.fill, inks.warn]) < 0.01,
                    "{name} warn {warn}: the edge left the hull"
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
            // The comet wears the meter's fill (a pastel on Catppuccin Latte,
            // ruling 264; the cursor accent everywhere else).
            let (inks, _) = MeterInks::comet(&c, c.meter);
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
                    let (rows, _, _) = paint_rows_on(&p, theme, None, geom, &m);
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

#[cfg(test)]
#[path = "band_raster_tests.rs"]
mod band_raster_tests;
