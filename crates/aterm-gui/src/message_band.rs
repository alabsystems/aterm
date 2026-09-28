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
//! ([`Presentation::hit`]), computes every moving thing as FRACTIONS of
//! the row ([`BandMotion`], design ruling 140 — the owner's architecture ask:
//! *"design the logic in aterm core and then keep the osx layer lightweight"*)
//! and PAINTS each row's STRUCTURE (`aterm_messages::paint::paint`, ruling 319): which
//! character goes in which cell, in which ink slot (`aterm_messages::paint::Ink`, ruling
//! 320) or chip form, which columns lie on the fill's side, the cell the
//! fill's edge splits, the cells a chip keeps for its own ground or ring, the
//! drawn icon, and every tone mapped onto the window's PIXELS
//! (`paint::MeterSpan`, through the window's [`BandGeometry`]) — and
//! RESOLVES it to colours (`aterm_messages::ink`, ruling 324): every
//! decision an RGB value makes — the contrast floors, the crisp ink, the side
//! holds, the glint's rail, the pastel edge line, the chip inks, the fade —
//! over the chrome band's material as plain RGB. This module maps the chrome
//! theme onto that material ([`chrome_band::band_colors`] — the chrome
//! palette, which also retires the OSC-11 tint drift the config band had),
//! writes the resolved cells as [`RenderCell`]s ([`paint_rows_on`]), places
//! each row's pixel raster ([`RowRaster`]) on the renderer's frame
//! ([`OnFrame`]), and maps a window pixel to a band row and column
//! ([`App::band_hit_at`]).
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
//! tone over its own window-pixel span ([`aterm_messages::paint::MeterSpan`], through the window's
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
//! of it, and a Fault echo warms it to the fault hue ([`ink::MeterInks::rgb`]). A BUSY
//! row's comet wears the same accent held on its words' side of the
//! luminance scale ([`ink::MeterInks::comet`]: on a dark band a deep, saturated
//! accent), so a word it passes brightens and dims with it and never flips
//! to the far ink.
//!
//! Every word a DETERMINATE row writes over its fill wears the row's CRISP
//! ink ([`ink::fill_ink`], ruling 222): whichever of the band's own background and
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
//! accent falls short, `ink::chip_inks`), `bar_bg` ink, bold; **Secondary** (a navigation
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
use aterm_messages::ink;
use aterm_messages::paint::Icon;
use aterm_messages::text::char_width;
use aterm_messages::{ActionIndex, BandMotion, Hit, Links, MARGIN, Presentation};
#[cfg(test)]
use aterm_messages::{
    CapsuleLayout, CapsuleRole, FineTone, RowKind, RowLayout, RowMotion, Severity,
};
use aterm_render::Theme;

#[cfg(test)]
use crate::chrome_band::BandColors;
use crate::chrome_band::{self};
use crate::settings::{blank_row, write_str};
use crate::{App, WindowId};

// The painter's structure types, under the names the host has always used
// (ruling 319): the engine owns them now.
#[cfg(test)]
pub(crate) use aterm_messages::paint::MeterSpan;
pub(crate) use aterm_messages::paint::{Geometry as BandGeometry, Hover as BandHover, HoverTarget};
// The band's repaint key (ruling 328): the engine's pure hash, its values kept.
pub(crate) use aterm_messages::paint::{BandKey, band_fp};
// The colour resolver's items, under the names the host has always used (ruling
// 324): the engine resolves the band's colours now (`aterm_messages::ink`).
pub(crate) use aterm_messages::ink::RowRaster;
#[cfg(test)]
pub(crate) use aterm_messages::ink::{
    MeterInks, WORD_AA, crisp_on_fill, delta_e, fill_ink, glint_step, lin_mix, luminance,
    outlined_inks,
};
#[cfg(test)]
pub(crate) use aterm_messages::palette::{lin, oklch};

/// The band's cell measure — the char count, because the band's writer puts
/// one `char` in one cell (see the module doc).
pub(crate) fn cell_width(s: &str) -> usize {
    char_width(s)
}

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

/// A tone's colour against a row's inks — the track mixed toward the fill
/// by `fill`, that toward the glint by `lift`, plus `warn`'s share of warn
/// over the track — in LINEAR light, with one rounding at the end (the
/// engine's [`MeterInks::rgb`], ruling 242; ruling 157's single rounding).
#[cfg(test)]
pub(crate) fn tone_rgb(t: aterm_messages::Tone, k: &MeterInks) -> [u8; 3] {
    fine_rgb(t.into(), k)
}

/// [`tone_rgb`] of a [`FineTone`]: a span's mean, unrounded.
#[cfg(test)]
pub(crate) fn fine_rgb(t: FineTone, k: &MeterInks) -> [u8; 3] {
    k.rgb(t)
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

/// A row's PIXEL raster ([`RowRaster`], the engine's, ruling 324) placed on the
/// renderer's frame.
pub(crate) trait OnFrame {
    /// This raster on a frame whose column 0 starts `lo` window pixels in
    /// from the window's left edge (the leading remainder band, `cells_x −
    /// pad`) and which is `frame_w` pixels wide, for chrome row `row` whose
    /// band is `cell_h` pixels tall: the renderer's [`aterm_render::ChromeRaster`].
    /// A frame pixel past the window's own columns repeats the edge one.
    fn on_frame(
        &self,
        row: u16,
        lo: usize,
        frame_w: usize,
        cell_h: usize,
    ) -> aterm_render::ChromeRaster;
}

impl OnFrame for RowRaster {
    fn on_frame(
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
                .map(|&(col, icon)| aterm_render::ChromeIcon {
                    col,
                    icon: band_icon(icon),
                })
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
/// surface is mapped onto the window through `geom` ([`aterm_messages::paint::MeterSpan`]) at
/// PIXEL resolution ([`RowRaster`], ruling 242). `hover` lights the chip
/// under the pointer (or the body's `Details ›`) on its row. No row carries
/// the hairline that closes the chrome against the terminal: the splice pads
/// and trims this cache to the committed count and puts a presence row above
/// it, so only the COMPOSED stack knows which row is last — [`seal_stack`]
/// draws it there. The engine paints each row's structure
/// (`aterm_messages::paint::paint`, ruling 319, reading the forced-palette
/// latch once here, as the painter always has) and resolves it on this
/// palette's colours (`aterm_messages::ink::paint_band`, ruling 324); the host
/// writes each resolved cell as a [`RenderCell`] and keeps the gutter edges.
pub(crate) fn paint_rows_on(
    p: &Presentation,
    palette: impl Into<chrome_band::BandPalette>,
    hover: Option<BandHover>,
    geom: BandGeometry,
    motion: &BandMotion,
) -> PaintedBand {
    let c = palette.into().colors();
    let hc = chrome_band::forced_chrome().is_some();
    let resolved = ink::paint_band(p, hover, geom, motion, hc, &c);
    let n = resolved.len();
    let mut rows: Vec<Vec<RenderCell>> = Vec::with_capacity(n);
    let mut edges = Vec::with_capacity(n);
    let mut rasters = Vec::with_capacity(n);
    for r in resolved {
        // The gutters continue the cells ACTUALLY painted at the row's two
        // edges — a chip the width law put at column 0 (a degenerate narrow
        // row) wears its own fill, not the meter's, and an echo's fade mixes
        // both — so a tone changes only together with its edge cell: the
        // renderer's no-epoch contract (`aterm_render::ChromeBleed::row_edges`).
        // A busy row's comet is the same surface, so its gutters light as it
        // enters and leaves. The pixel raster, where there is one, is drawn
        // over both (and dirties its row on its own).
        let row: Vec<RenderCell> = r
            .cells
            .iter()
            .map(|k| {
                let mut out = chrome_band::cell(k.ch, k.fg, k.bg, k.bold, false);
                out.text_presentation = k.text_presentation;
                out
            })
            .collect();
        edges.push((r.metered && !row.is_empty()).then(|| (row[0].bg, row[row.len() - 1].bg)));
        rows.push(row);
        rasters.push(r.raster);
    }
    (rows, edges, rasters)
}

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

/// The renderer's drawn icon for the engine's (ruling 322: one set in two
/// tables, held together by `band_icon_ids_match_the_renderers`).
pub(crate) const fn band_icon(icon: Icon) -> aterm_render::BandIcon {
    use aterm_render::BandIcon as B;
    match icon {
        Icon::Info => B::Info,
        Icon::Success => B::Success,
        Icon::Warn => B::Warn,
        Icon::Error => B::Error,
        Icon::Download => B::Download,
        Icon::Update => B::Update,
        Icon::Upload => B::Upload,
        Icon::Pause => B::Pause,
        Icon::Sparkle => B::Sparkle,
        Icon::Alert => B::Alert,
        Icon::Dot => B::Dot,
        Icon::More => B::More,
        Icon::Remove => B::Remove,
    }
}

/// The inks row `l`'s surface resolves against, and — on a BUSY row — the
/// anchor its words floor toward (the engine's [`ink::inks_for`] from the
/// row's layout): a busy row's surface keeps its words' side, a determinate
/// row's Fault wash keeps the fill's words' side, and a RAIL (a measured
/// level, ruling 243) wears warn over the band itself.
#[cfg(test)]
pub(crate) fn row_inks(
    c: &BandColors,
    l: &RowLayout,
    rail: bool,
    hc: bool,
) -> (MeterInks, Option<[u8; 3]>) {
    let alarm = matches!(l.severity, Severity::Warn | Severity::Error);
    let fill = if hc || !alarm { c.meter } else { c.warn };
    ink::inks_for(c, fill, l.track.is_some(), rail, hc)
}

/// The band row and column for a frame pixel, given the geometry — the pure
/// half of [`App::band_hit_at`]. `gx`/`gy` are frame pixels from the frame
/// origin; `frame` is the cell lattice across the frame (cells at the pad,
/// `cell_w` wide) and `top` its offset down, `ch` a cell's height; the chrome
/// rows between the strip and the grid are `presence_rows` then `band_rows`.
/// The column is the engine's arithmetic ([`BandGeometry::cell_at`], ruling
/// 328); the rows stack the strip, the presence row and the band, which are
/// this host's chrome.
#[allow(
    clippy::too_many_arguments,
    reason = "the pure hit test takes the geometry as plain numbers so its tests need no App"
)]
fn band_target_for_pixel(
    gx: usize,
    gy: usize,
    frame: BandGeometry,
    ch: usize,
    top: usize,
    strip_rows: usize,
    presence_rows: usize,
    band_rows: usize,
) -> Option<BandTarget> {
    if band_rows == 0 && presence_rows == 0 {
        return None;
    }
    let ch = ch.max(1);
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
        col: frame.cell_at(gx),
    })
}

/// The `$HOME` the band abbreviates paths under (`~/…`); the log keeps the
/// absolute path (D4).
pub(crate) fn band_home() -> Option<String> {
    aterm_types::dirs::home_dir().map(|h| h.to_string_lossy().into_owned())
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
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let cols = self.windows.get(&wid).map_or(0, |ws| usize::from(ws.cols));
        // The frame's own lattice: `cols·cw + 2·pad` wide, cells at the pad.
        let frame = BandGeometry {
            win_w: cols
                .saturating_mul(cw)
                .saturating_add(pad.saturating_mul(2)),
            cells_x: pad,
            cell_w: cw,
        };
        band_target_for_pixel(
            fx as usize,
            fy as usize,
            frame,
            ch,
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
    /// carries the words, the meter is laid out as the whole row and leaves no
    /// procedural glyph behind, the painter draws no seam (the stack's last
    /// row closes the chrome), and the glyph is text presentation. Its colour
    /// half — the fill, the track, the edge cell's mix and the crisp ink —
    /// is the engine's (`band_tests::the_meter_row_is_its_fill_and_track_and_its_words_the_crisp_ink`,
    /// ruling 328).
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
        // THE METER IS THE ROW (rulings 55, 136).
        let (mcol, w, fill) = p.rows[0].meter.expect("a meter at 140 cols");
        assert_eq!((mcol, w, fill), (0, 140, 427), "the meter is the row");
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
        assert!(rows[0][p.rows[0].title.0].bold);
    }

    /// Layout and hit share one law: every cell a capsule is painted on maps
    /// back to that capsule — the `Details ›` to the Details hit — and the
    /// gaps map to the body. Below its long width `Details ›` goes (it has
    /// no short form: a lone `›` said nothing the body press does not do).
    /// Its colour half — each role's ground and ink on the metered row — is
    /// the engine's (`band_tests::each_chip_wears_its_roles_ground_and_ink_on_a_metered_row`,
    /// ruling 328).
    #[test]
    fn capsules_are_hit_where_they_are_painted() {
        let center = two_rows();
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

    /// Under Windows High Contrast the capsules are `[label]`, and the
    /// hovered one keeps its brackets. Its colour half — no fill, every ink
    /// WINDOWTEXT, the glyph on the fill HIGHLIGHTTEXT, the hovered chip
    /// HIGHLIGHT — is the engine's
    /// (`band_tests::high_contrast_capsules_are_one_ink_on_the_band`, ruling 328).
    #[test]
    fn high_contrast_draws_capsules_as_brackets() {
        let center = two_rows();
        for (name, palette) in chrome_band::hc_fixtures::STOCK {
            chrome_band::hc_fixtures::with_forced(palette, || {
                let p = present(&center, 140);
                let rows = paint_still(&p, Theme::default(), None, &center);
                for cap in &p.rows[0].capsules {
                    assert_eq!(rows[0][cap.col].ch, '[', "{name}");
                    assert_eq!(rows[0][cap.col + cap.width - 1].ch, ']', "{name}");
                }
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
                assert_eq!(lit[0][first.col].ch, '[', "{name}: brackets stay");
            });
        }
    }

    /// The pixel → row/column map: the presence row first, then the band
    /// rows, the column from the cell lattice; off the chrome rows is `None`.
    #[test]
    fn band_target_for_pixel_maps_the_chrome_rows_in_order() {
        // Cells 8 px wide from the pad, 16 px tall.
        let (pad, top) = (4, 10);
        let lattice = |cw| BandGeometry {
            win_w: 80 * cw + 2 * pad,
            cells_x: pad,
            cell_w: cw,
        };
        let cell = lattice(8);
        // strip 1 row, presence 1 row, band 2 rows.
        let hit = |gx, gy| band_target_for_pixel(gx, gy, cell, 16, top, 1, 1, 2);
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
            band_target_for_pixel(40, top + 20, cell, 16, top, 1, 0, 0),
            None
        );
        // No strip (macOS): the band starts at the lattice top.
        assert_eq!(
            band_target_for_pixel(0, top, cell, 16, top, 0, 0, 1),
            Some(BandTarget::Row { row: 0, col: 0 })
        );
        // A zero cell size never divides by zero.
        assert_eq!(
            band_target_for_pixel(0, top, lattice(0), 0, top, 0, 0, 1),
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

/// THE ENGINE AND THE HOST'S TABLES AGREE (rulings 319-325): the engine's
/// icon mirror names exactly the renderer's icon for every `char`, and the
/// engine's band contrast is `aterm_types::Rgb::contrast` bit for bit.
#[cfg(test)]
mod paint_parity_tests {
    use super::*;

    /// Every `char` the renderer draws as a band icon is the engine's icon of
    /// that char, and no other char is an engine icon (ruling 322).
    #[test]
    fn band_icon_ids_match_the_renderers() {
        for ch in (0..=0x10_ffffu32).filter_map(char::from_u32) {
            assert_eq!(
                aterm_render::BandIcon::for_char(ch),
                Icon::for_char(ch).map(band_icon),
                "{ch:?}"
            );
        }
        for icon in aterm_render::BandIcon::ALL {
            assert_eq!(Icon::for_char(icon.ch()).map(band_icon), Some(icon));
        }
    }

    /// THE ENGINE'S BAND TESTS READ THIS HOST'S THEMES (ruling 328): the
    /// inputs `aterm_messages`' `band_tests` paint on — the default theme, the
    /// light theme its comet test uses, every builtin scheme as a theme and
    /// the four stock High Contrast palettes — digest to the number its
    /// `fixtures_are_the_hosts` pins, and this host maps each theme onto the
    /// palette those tests derive (the blend base everywhere but Linux, whose
    /// CSD base `chrome_band`'s own tests keep).
    #[test]
    fn the_engines_band_tests_read_the_hosts_themes() {
        use aterm_messages::ink::{BandInks, BarBase, ThemeInks};
        let of = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
        let inks = |t: Theme| ThemeInks {
            bg: of(t.bg),
            fg: of(t.fg),
            cursor: of(t.cursor),
        };
        let light = Theme {
            fg: 0x001F_2328,
            bg: 0x00FF_FFFF,
            cursor: 0x0009_69DA,
            selection: 0x00DD_F4FF,
        };
        let mut themes = vec![Theme::default(), light];
        let builtin: Vec<(&str, ThemeInks)> = aterm_types::scheme::builtin_names()
            .into_iter()
            .map(|name| {
                let parts = aterm_types::scheme::builtin(name)
                    .expect("a listed scheme")
                    .to_theme_parts();
                let theme = Theme {
                    fg: parts.fg,
                    bg: parts.bg,
                    cursor: parts.cursor,
                    selection: parts.selection,
                };
                themes.push(theme);
                (name, inks(theme))
            })
            .collect();
        let text = format!(
            "{:?}",
            (
                inks(Theme::default()),
                inks(light),
                &builtin[..],
                &chrome_band::hc_fixtures::STOCK[..]
            )
        );
        let digest = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
        assert_eq!(digest, 0x973b_e822_c486_d59b, "{text}");
        if cfg!(not(target_os = "linux")) {
            for theme in themes {
                assert_eq!(
                    chrome_band::band_colors(theme),
                    BandInks::derive(inks(theme), None, BarBase::Blend),
                    "{theme:?}"
                );
            }
        }
        for (_, palette) in chrome_band::hc_fixtures::STOCK {
            chrome_band::hc_fixtures::with_forced(palette, || {
                assert_eq!(
                    chrome_band::band_colors(Theme::default()),
                    BandInks::forced(palette)
                );
            });
        }
    }

    /// THE HOST WRITES WHAT THE ENGINE RESOLVED (ruling 328): every cell
    /// [`paint_rows_on`] writes is the engine's resolved cell — its character,
    /// ink, ground, weight and presentation — with no underline, over the
    /// fixture the engine's colour tests read (two metered rows at every
    /// width they use), resting and under each hover, in the still and the
    /// moving look, on the default theme and under each stock High Contrast
    /// palette; and each metered row's gutters are its edge cells' grounds.
    #[test]
    fn paint_rows_on_writes_the_engines_resolved_cells() {
        use aterm_messages::{Instant, Look, Message, MessageCenter, MessageLog, Meter, WallStamp};
        let now = Instant::now();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        let stamp = WallStamp { unix_ms: 1 };
        center.post(
            Message::new(
                aterm_messages::tags::TOOLCHAIN,
                Severity::Info,
                "Installing ALab tools",
            )
            .line("trust — extracting 120 MB / 900 MB")
            .meter(Meter {
                fill_permille: Some(427),
                stats: "3 of 10 · 512 MB / 1.2 GB".into(),
                ..Meter::default()
            })
            .hold(aterm_messages::Hold::Live {
                stale_after: aterm_messages::STALE_TAILED,
            })
            .action(aterm_messages::Intent::OpenSettings {
                route: "/packages".into(),
            }),
            stamp,
            now,
        );
        center.post(
            Message::new(
                aterm_messages::tags::UPDATE,
                Severity::Info,
                "aterm update v0.48.0",
            )
            .line("downloading…")
            .meter(Meter {
                fill_permille: Some(608),
                stats: "45 MB / 74 MB".into(),
                ..Meter::default()
            })
            .hold(aterm_messages::Hold::Live {
                stale_after: aterm_messages::STALE_UPDATE,
            }),
            stamp,
            now,
        );
        assert_eq!(center.commit_rows(now, 3), Some(2));
        let mut compared = 0usize;
        let mut check = |hc: bool| {
            let theme = Theme::default();
            for cols in [60usize, 80, 120, 140, 160] {
                let p = center.presentation(cols, &cell_width, None, Links::Painted);
                let mut hovers = vec![None];
                for (r, row) in p.rows.iter().enumerate() {
                    let row_u8 = u8::try_from(r).unwrap();
                    hovers.push(Some(BandHover {
                        row: row_u8,
                        target: HoverTarget::Body,
                    }));
                    for cap in &row.capsules {
                        hovers.push(Some(BandHover {
                            row: row_u8,
                            target: HoverTarget::Capsule(cap.action),
                        }));
                    }
                }
                for look in [Look::STILL, Look::MOVING] {
                    let m =
                        center.motion(&p, now + aterm_messages::Duration::from_millis(700), look);
                    for &hover in &hovers {
                        for g in [
                            BandGeometry::cells_only(cols),
                            BandGeometry {
                                win_w: cols * 9 + 31,
                                cells_x: 15,
                                cell_w: 9,
                            },
                        ] {
                            let c = chrome_band::band_colors(theme);
                            let want = ink::paint_band(&p, hover, g, &m, hc, &c);
                            let (rows, edges, rasters) = paint_rows_on(&p, theme, hover, g, &m);
                            assert_eq!(rows.len(), want.len());
                            for (i, (row, r)) in rows.iter().zip(&want).enumerate() {
                                assert_eq!(row.len(), r.cells.len());
                                for (cell, k) in row.iter().zip(&r.cells) {
                                    assert_eq!(
                                        (
                                            cell.ch,
                                            cell.fg,
                                            cell.bg,
                                            cell.bold,
                                            cell.text_presentation
                                        ),
                                        (k.ch, k.fg, k.bg, k.bold, k.text_presentation),
                                        "hc {hc} cols {cols} row {i} hover {hover:?}"
                                    );
                                    assert_eq!(cell.underline, UnderlineStyle::None);
                                    compared += 1;
                                }
                                assert_eq!(rasters[i], r.raster);
                                assert_eq!(
                                    edges[i],
                                    r.metered.then(|| (row[0].bg, row[row.len() - 1].bg))
                                );
                            }
                        }
                    }
                }
            }
        };
        check(false);
        for (_, palette) in chrome_band::hc_fixtures::STOCK {
            chrome_band::hc_fixtures::with_forced(palette, || check(true));
        }
        assert!(compared > 10_000, "{compared} cells compared");
    }

    /// THE BAND'S CONTRAST IS aterm-types' (ruling 324): the engine's
    /// `ink::contrast`, which every band floor reads, is
    /// `aterm_types::Rgb::contrast` bit for bit — an unfused transcription, not
    /// `aterm_messages::palette::contrast`, which fuses its luminance and is one
    /// ulp apart on a third of all pairs. Every 13th colour of the cube against
    /// black, white and mid grey, and every 257th against each builtin's band,
    /// value, meter and track (round 24 ran all 2^24 against black and white
    /// once, both orders: 0 of 33,554,432 differ).
    #[test]
    fn band_contrast_is_aterm_types_contrast() {
        let rgb = |c: [u8; 3]| aterm_types::Rgb::new(c[0], c[1], c[2]);
        let of = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
        let same = |a: [u8; 3], g: [u8; 3]| {
            assert_eq!(
                ink::contrast(a, g).to_bits(),
                rgb(a).contrast(rgb(g)).to_bits(),
                "{a:02x?} on {g:02x?}"
            );
        };
        let mut grounds: Vec<[u8; 3]> = Vec::new();
        for name in aterm_types::scheme::builtin_names() {
            let s = aterm_types::scheme::builtin(name).expect("a listed scheme");
            let parts = s.to_theme_parts();
            let c = chrome_band::BandPalette {
                theme: Theme {
                    fg: parts.fg,
                    bg: parts.bg,
                    cursor: parts.cursor,
                    selection: parts.selection,
                },
                ansi: Some(chrome_band::MeterAnsi::of_scheme(&s)),
            }
            .colors();
            grounds.extend([c.bar_bg, c.value, c.meter, c.meter_track]);
        }
        grounds.sort_unstable();
        grounds.dedup();
        for v in (0..1u32 << 24).step_by(13) {
            for g in [[0, 0, 0], [255, 255, 255], [0x80, 0x80, 0x80]] {
                same(of(v), g);
            }
        }
        for v in (0..1u32 << 24).step_by(257) {
            for &g in &grounds {
                same(of(v), g);
                same(g, of(v));
            }
        }
    }
}
