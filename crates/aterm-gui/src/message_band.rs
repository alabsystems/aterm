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
//! the row ([`BandMotion`](aterm_messages::BandMotion), design ruling 140 — the owner's architecture ask:
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
//! hands the resolved rows to the renderer's band composition
//! (`aterm_render::band`, ruling 331 — the [`RenderCell`] write, each row's
//! pixel raster ([`RowRaster`]) on the frame, the stack's seams and the
//! splice, the one table the web module runs too) through [`paint_rows_on`]
//! and `App::splice_message_band`, and maps a window pixel to a band row and
//! column ([`App::band_hit_at`]).
//! Layout and hit test share one law by construction: the painter reads the
//! same columns the hit test reads, so a capsule is hit exactly where it is
//! painted.
//!
//! # The width measure
//!
//! The band writes one `char` per cell (`settings::write_str`, the presence
//! and strip painters' writer), so the measure handed to the width law is
//! the char count (`aterm_messages::text::char_width`, the measure the engine's
//! `drive::Lay` lays out with): the layout and the writer must agree, or
//! a right-aligned capsule would be hit one cell off where it is drawn. Every
//! reporter's words are ASCII plus a few BMP glyphs; a wide-aware writer and
//! measure move together when one arrives — as they did for the presence
//! row, whose free text (a role in any script) is laid out and painted in the
//! grid's cells (`aterm_grapheme::str_grid_columns`, injected as
//! `presence::Host::columns`; a wide glyph is its lead cell and a
//! continuation, ruling 2026-10-11).
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
//! antialiased pixel, the glint in the row's lowest pixels (its rail band)
//! where at full height it would not read against the words, and the one
//! cell the edge crosses split — its glyph drawn in the fill's
//! ink left of the edge and the track's right of it. The words' inks are
//! chosen ONCE per row (never per cell from the tone passing under), so an
//! edge, a glint, a comet or an echo moving under a word never changes it.
//! The flat look (High Contrast) keeps whole-cell palette tones.
//!
//! The fill's ink is the theme's cursor accent (ruling 137, the owner's
//! "cursor trail theme"), `warn` on a Warn/Error row, and `HIGHLIGHT` under
//! High Contrast; a stalled bar dims to a slate of it, the glint is a lift
//! of it, and a Fault echo warms it to the fault hue ([`MeterInks::rgb`](aterm_messages::ink::MeterInks::rgb)). A BUSY
//! row's comet wears the same accent held on its words' side of the
//! luminance scale ([`MeterInks::comet`](aterm_messages::ink::MeterInks::comet): on a dark band a deep, saturated
//! accent), so a word it passes brightens and dims with it and never flips
//! to the far ink.
//!
//! Every word a DETERMINATE row writes over its fill wears the row's CRISP
//! ink ([`fill_ink`](aterm_messages::ink::fill_ink), ruling 222): whichever of the band's own background and
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
//! by the engine) under one [`BandMotion`](aterm_messages::BandMotion) (`MessageCenter::motion`, read on
//! the engine's 33 ms grid). A held row's still bar and a busy row's still
//! track come from the same call in the still look, so the painter has one
//! path. An animation never re-runs the width law and never re-grids: the
//! motion moves only the surface, the time slots, the glyph cell and an
//! echo's fade — which mixes every cell toward `bar_bg` and never touches
//! the seam, which the splice draws on the COMPOSED stack afterwards
//! (`aterm_render::band::seal_stack`).
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
//! padded row, or the presence row when no band row follows
//! (`aterm_render::band::seal_stack`).
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

use aterm_core::terminal::RenderCell;
#[cfg(test)]
use aterm_core::terminal::UnderlineStyle;
#[cfg(test)]
use aterm_messages::ink;
#[cfg(test)]
use aterm_messages::text::char_width;
use aterm_messages::{ActionIndex, Hit, Links, Presentation};
#[cfg(test)]
use aterm_messages::{BandMotion, CapsuleRole, FineTone, RowKind, RowLayout, Severity};
#[cfg(test)]
use aterm_render::Theme;

#[cfg(test)]
use crate::chrome_band::BandColors;
use crate::chrome_band::{self};
use crate::{App, WindowId};

// The painter's structure types, under the names the host has always used
// (ruling 319): the engine owns them now.
pub(crate) use aterm_messages::paint::{Geometry as BandGeometry, Hover as BandHover, HoverTarget};
// The band's repaint key (ruling 328): the engine's pure hash, its values kept.
// The host reads it through its window's view (`drive::View::band_fp`); the
// tests read it by name.
#[cfg(test)]
pub(crate) use aterm_messages::paint::band_fp;
// The colour resolver's items, under the names the host has always used (ruling
// 324): the engine resolves the band's colours now (`aterm_messages::ink`).
pub(crate) use aterm_messages::ink::RowRaster;
#[cfg(test)]
pub(crate) use aterm_messages::ink::{
    MeterInks, WORD_AA, crisp_on_fill, delta_e, fill_ink, glint_step, outlined_inks,
};
#[cfg(test)]
pub(crate) use aterm_messages::palette::lin;

/// The band's cell measure — the char count, because the band's writer puts
/// one `char` in one cell (see the module doc) — the tests' name for the
/// measure the drive's `Lay` hands the width law.
#[cfg(test)]
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

/// One EMPTY band row, in the same colours a painted row uses. The compose
/// pads with this when the geometry has committed more rows than the cache
/// holds, so a reserved row is never a hole the terminal's own top row shows
/// through.
pub(crate) fn blank_band_row(
    cols: usize,
    p: impl Into<chrome_band::ChromePalette>,
) -> Vec<RenderCell> {
    aterm_render::band::blank_band_row(cols, &chrome_band::band_colors(p))
}

/// Paint the PRESENCE band's one row (`crate::presence::Words`) at `cols`: the
/// engine lays the slots out and inks each cell (`aterm_messages::presence::
/// paint_row`, design ruling 348 — the six slots shed from the right in the
/// design's order, each in its own ink, the emoji-capable glyphs pinned to
/// text presentation) on this host's chrome band ([`chrome_band::band_colors`])
/// with the presence hues at the text floor ([`chrome_band::presence_inks`]);
/// the renderer's band table writes the cells (`aterm_render::band::rows`,
/// the one table the web module runs too). The row carries no closing seam of
/// its own: the splice seals it (`aterm_render::band::seal_stack`) when no
/// band row follows.
pub(crate) fn paint_presence_row(
    words: &crate::presence::Words,
    cols: usize,
    palette: impl Into<chrome_band::ChromePalette>,
) -> Vec<RenderCell> {
    let palette = palette.into();
    let c = chrome_band::band_colors(palette);
    let p = chrome_band::presence_inks(palette);
    let cells = aterm_messages::presence::paint_row::<crate::presence::Native>(words, cols, &c, &p);
    let (mut rows, _, _) = aterm_render::band::rows(vec![aterm_messages::ink::Resolved {
        cells,
        metered: false,
        raster: None,
    }]);
    rows.pop().unwrap_or_default()
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

/// One painted band: its rows, each metered row's gutter tones and each row's
/// pixel raster — the renderer's (`aterm_render::band`, ruling 331).
#[cfg(test)]
pub(crate) use aterm_render::band::PaintedBand;

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
/// it, so only the COMPOSED stack knows which row is last — `aterm_render::band::seal_stack`
/// draws it there. The engine paints each row's structure
/// (`aterm_messages::paint::paint`, ruling 319, reading the forced-palette
/// latch once here, as the painter always has) and resolves it on this
/// palette's colours (`aterm_messages::ink::paint_band`, ruling 324); the host
/// writes each resolved cell as a [`RenderCell`] and keeps the gutter edges.
/// The tests' name for the splice's paint: the splice itself paints through
/// its window's view (`aterm_messages::drive::View::paint`, ruling 336), which
/// makes the same two calls.
#[cfg(test)]
pub(crate) fn paint_rows_on(
    p: &Presentation,
    palette: impl Into<chrome_band::ChromePalette>,
    hover: Option<BandHover>,
    geom: BandGeometry,
    motion: &BandMotion,
) -> PaintedBand {
    let c = palette.into().colors();
    let hc = chrome_band::forced_chrome().is_some();
    aterm_render::band::rows(ink::paint_band(p, hover, geom, motion, hc, &c))
}

/// How strongly the divider between stacked band rows shows the seam's ink
/// over the band (design ruling 260) — the renderer's (ruling 331).
#[cfg(test)]
pub(crate) use aterm_render::band::DIVIDER_ALPHA;

/// The ink of the line that closes composed row `row` of `frame` (the stack's
/// seam on its last row, a divider above it), however the compose drew it
/// (`aterm_render::band::compose_band`): every cell's underline where the cell
/// is too short to lift the words off a rail, else the row's 1 px rail with
/// the words lifted clear of it (`aterm_render::band::seam_can_rail`), which a
/// tall cell gets — Linux's default 15 px face does. `None` where the row
/// carries neither, or carries no one tone across it.
#[cfg(test)]
pub(crate) fn closing_line(frame: &aterm_render::RenderInput, row: usize) -> Option<[u8; 3]> {
    use aterm_core::terminal::UnderlineStyle;
    let cells = frame.cells.get(row)?;
    let first = cells.first()?;
    if first.underline == UnderlineStyle::Single {
        return cells
            .iter()
            .all(|c| {
                c.underline == UnderlineStyle::Single && c.underline_color == first.underline_color
            })
            .then_some(first.underline_color)
            .flatten();
    }
    let at = u16::try_from(row).ok()?;
    let raster = frame
        .chrome_rasters
        .iter()
        .find(|m| m.row == at && m.clear_rail && m.rail_h == 1)?;
    let px = *raster.rail.first()?;
    raster
        .rail
        .iter()
        .all(|&p| p == px && p != aterm_render::ChromeRaster::KEEP)
        .then_some([(px >> 16) as u8, (px >> 8) as u8, px as u8])
}

/// The renderer's drawn icon for the engine's (ruling 322: one set in two
/// tables, held together by `band_icon_ids_match_the_renderers`) — the
/// renderer's map since ruling 331.
#[cfg(test)]
pub(crate) use aterm_render::band::band_icon;

/// The inks row `l`'s surface resolves against, and — on a BUSY row — the
/// anchor its words floor toward (the engine's [`inks_for`](aterm_messages::ink::inks_for) from the
/// row's layout): a busy row's surface keeps its words' side, and a
/// determinate row's Fault wash keeps the fill's words' side.
#[cfg(test)]
pub(crate) fn row_inks(c: &BandColors, l: &RowLayout, hc: bool) -> (MeterInks, Option<[u8; 3]>) {
    let alarm = matches!(l.severity, Severity::Warn | Severity::Error);
    let fill = if hc || !alarm { c.meter } else { c.warn };
    ink::inks_for(c, fill, l.track.is_some(), hc)
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

#[cfg(test)]
thread_local! {
    /// How many band layouts this test thread has built (each reads the
    /// home once): a motion frame that re-ran the width law shows here.
    pub(crate) static BAND_LAYOUTS_BUILT: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

/// The `$HOME` the band abbreviates paths under (`~/…`); the log keeps the
/// absolute path (D4).
pub(crate) fn band_home() -> Option<String> {
    #[cfg(test)]
    BAND_LAYOUTS_BUILT.with(|c| c.set(c.get() + 1));
    aterm_types::dirs::home_dir().map(|h| h.to_string_lossy().into_owned())
}

/// How this host lays the band out at `cols` (the engine's
/// `aterm_messages::drive::Lay`; [`App::band_lay`] holds it): paths under
/// `$HOME` printed as `~/…`, every row ending in its link ([`Links::Painted`]:
/// Settings ▸ Messages is where each one goes — module doc).
pub(crate) fn band_lay(cols: usize) -> aterm_messages::drive::Lay {
    aterm_messages::drive::Lay {
        cols,
        links: Links::Painted,
        home: band_home,
    }
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
        (self.band_lay)(cols).presentation(&self.messages)
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

    /// THE LIT CHIP FOLLOWS THE LAYOUT under a still pointer (round 44): a
    /// post, a retire, a fold or a re-grid moves the band's chips, and the
    /// hover was written by pointer motion alone — a pass's `Packages` stayed
    /// lit on a band of no row, and the next row's chip at that index painted
    /// lit where the pointer was not. Every window's hover is resolved again
    /// from its last pointer position (nothing is lit where the window holds
    /// none), and on a change its cursor is resolved again too: the band's
    /// hand, or whatever is under the pointer now. Over a NATIVE front the
    /// cursor is the motion path's native branch's (it gives the band's hand
    /// there, ruling 417 (n)), which `update_hover_cursor` leaves alone: that
    /// branch is replayed at the pointer's own pixel instead, as a closed
    /// card hands the pointer back (`conn_card_exit`), and it lights the chip
    /// and sets the hand or the arrow (round 44's review, ruling 418 (b)).
    pub(crate) fn refresh_band_hover(&mut self) {
        let wids: Vec<WindowId> = self.windows.keys().copied().collect();
        for wid in wids {
            let Some(ws) = self.windows.get(&wid) else {
                continue;
            };
            let (was, known, (x, y)) =
                (ws.band_hover, ws.pointer_position_known, ws.last_cursor_px);
            let hover = if known {
                self.band_hit_at(wid, x, y)
                    .and_then(|target| self.band_hover_for(wid, target))
            } else {
                None
            };
            if hover == was {
                continue;
            }
            if known && self.active_native_view(wid).is_some() {
                self.on_cursor_moved(wid, x, y);
                continue;
            }
            self.set_band_hover(wid, hover);
            if known {
                self.update_hover_cursor(wid);
            }
        }
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
        Hold, Instant, Intent, Look, Message, MessageCenter, MessageLog, Meter, Severity,
        WallStamp, tags,
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

    /// Every row is exactly `cols` wide and carries its title, the meter is
    /// laid out as the whole row and leaves no procedural glyph behind, and
    /// the painter draws no seam (only the composed stack knows which row is
    /// last: `aterm_render::band::seal_stack`).
    #[test]
    fn paint_rows_are_exactly_cols_wide_and_carry_the_words() {
        let center = two_rows();
        let p = present(&center, 140);
        let rows = paint_still(&p, Theme::default(), None, &center);
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert_eq!(row.len(), 140);
        }
        assert!(text_of(&rows[0]).contains("Installing ALab tools"));
        assert!(text_of(&rows[1]).contains("aterm update v0.48.0"));
        let (mcol, w, fill) = p.rows[0].meter.expect("a meter at 140 cols");
        assert_eq!((mcol, w, fill), (0, 140, 427), "the meter is the row");
        assert!(
            rows[0]
                .iter()
                .all(|cell| !(0xFDD0..=0xFDEF).contains(&u32::from(cell.ch))),
            "no procedural meter glyph is left on the band"
        );
        assert!(
            rows.iter()
                .flatten()
                .all(|cell| cell.underline == UnderlineStyle::None)
        );
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

    /// The frames a test walks: every 50 ms from `from` for `span`.
    fn frames(from: Instant, span: aterm_messages::Duration) -> impl Iterator<Item = Instant> {
        let steps = u64::try_from(span.as_millis() / 50).unwrap_or(0);
        (0..=steps).map(move |k| from + aterm_messages::Duration::from_millis(k * 50))
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
        for (name, theme) in super::band_raster_tests::themes() {
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
}

#[cfg(test)]
#[path = "band_raster_tests.rs"]
mod band_raster_tests;
