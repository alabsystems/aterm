// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The message band.

#[cfg(test)]
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
#[cfg(test)]
use crate::chrome_band::{self};
use crate::{App, WindowId};

pub(crate) use aterm_messages::paint::{Geometry as BandGeometry, Hover as BandHover, HoverTarget};

#[cfg(test)]
pub(crate) use aterm_messages::paint::band_fp;

pub(crate) use aterm_messages::ink::RowRaster;
#[cfg(test)]
pub(crate) use aterm_messages::ink::{
    MeterInks, WORD_AA, crisp_on_fill, delta_e, fill_ink, glint_step, outlined_inks,
};
#[cfg(test)]
pub(crate) use aterm_messages::palette::lin;

/// The band's cell measure, the char count (see the module doc).
#[cfg(test)]
pub(crate) fn cell_width(s: &str) -> usize {
    char_width(s)
}

/// Where a window pixel landed on the band rows between the strip and the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BandTarget {
    /// 0 = the topmost band row.
    pub(crate) row: usize,
    /// The cell column, 0 = the window's first cell.
    pub(crate) col: usize,
}

/// One empty band row, in the same colours a painted row uses (the tests' probe of the renderer's own pad, `aterm_render::band::blank_band_row`).
#[cfg(test)]
pub(crate) fn blank_band_row(
    cols: usize,
    p: impl Into<chrome_band::ChromePalette>,
) -> Vec<RenderCell> {
    aterm_render::band::blank_band_row(cols, &chrome_band::band_colors(p))
}

/// A tone's colour against a row's inks.
#[cfg(test)]
pub(crate) fn tone_rgb(t: aterm_messages::Tone, k: &MeterInks) -> [u8; 3] {
    fine_rgb(t.into(), k)
}

/// [`tone_rgb`] of a [`FineTone`].
#[cfg(test)]
pub(crate) fn fine_rgb(t: FineTone, k: &MeterInks) -> [u8; 3] {
    k.rgb(t)
}

/// The Complete echo says the finished form, for one reporter's live-work row.
#[cfg(test)]
pub(crate) fn assert_completes_in_place(msg: &aterm_messages::Message, finished: &str) {
    use aterm_messages::{Instant, Look, MessageCenter, MessageLog, Outcome, TITLE_COL, WallStamp};
    assert_eq!(msg.finished_title(), finished, "{}", msg.title);
    let mut msg = msg.clone();
    msg.reveal_after = None;
    if let Some(m) = msg.meter.as_mut().filter(|m| m.fill_permille.is_some()) {
        m.fill_permille = Some(1000);
        m.stats.clear();
    }
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

/// [`paint_rows_on`] with the fill mapped onto the columns alone ([`BandGeometry::cells_only`]).
#[cfg(test)]
pub(crate) fn paint_rows(
    p: &Presentation,
    theme: Theme,
    hover: Option<BandHover>,
    motion: &BandMotion,
) -> Vec<Vec<RenderCell>> {
    paint_rows_on(p, theme, hover, BandGeometry::cells_only(p.cols), motion).0
}

/// One painted band.
#[cfg(test)]
pub(crate) use aterm_render::band::PaintedBand;

/// Paint every committed band row as a `p.cols`-wide row on the chrome band material at `motion` (`MessageCenter::motion`).
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

/// How strongly the divider between stacked band rows shows the seam's ink over the band (`aterm_render::band`).
#[cfg(test)]
pub(crate) use aterm_render::band::DIVIDER_ALPHA;

/// The ink of the closing line on composed row `row` of `frame` (`aterm_render::band::compose_band`).
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

/// The renderer's drawn icon for the engine's (one set in two tables, held together by `band_icon_ids_match_the_renderers`).
#[cfg(test)]
pub(crate) use aterm_render::band::band_icon;

/// The inks row `l`'s surface resolves against, and — on a busy row — the anchor its
/// words floor toward (the engine's [`inks_for`](aterm_messages::ink::inks_for) from
/// the row's layout): a busy row's surface keeps its words' side, and a determinate
/// row's Fault wash keeps the fill's words' side.
#[cfg(test)]
pub(crate) fn row_inks(c: &BandColors, l: &RowLayout, hc: bool) -> (MeterInks, Option<[u8; 3]>) {
    let alarm = matches!(l.severity, Severity::Warn | Severity::Error);
    let fill = if hc || !alarm { c.meter } else { c.warn };
    ink::inks_for(c, fill, l.track.is_some(), hc)
}

/// The band row and column for a frame pixel, given the geometry — the pure
/// half of [`App::band_hit_at`].
fn band_target_for_pixel(
    gx: usize,
    gy: usize,
    frame: BandGeometry,
    ch: usize,
    top: usize,
    strip_rows: usize,
    band_rows: usize,
) -> Option<BandTarget> {
    if band_rows == 0 {
        return None;
    }
    let ch = ch.max(1);
    let gy = gy.checked_sub(top)?;
    let strip_px = strip_rows * ch;
    let band_px = band_rows * ch;
    if gy < strip_px || gy >= strip_px + band_px {
        return None;
    }
    Some(BandTarget {
        row: (gy - strip_px) / ch,
        col: frame.cell_at(gx),
    })
}

#[cfg(test)]
thread_local! {
    /// How many band layouts this test thread has built (each reads the home once).
    pub(crate) static BAND_LAYOUTS_BUILT: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

/// The `$HOME` the band abbreviates paths under (`~/…`).
pub(crate) fn band_home() -> Option<String> {
    #[cfg(test)]
    BAND_LAYOUTS_BUILT.with(|c| c.set(c.get() + 1));
    aterm_types::dirs::home_dir().map(|h| h.to_string_lossy().into_owned())
}

/// How this host lays the band out at `cols` (the engine's `aterm_messages::drive::Lay`, held in [`App::band_lay`]).
pub(crate) fn band_lay(cols: usize) -> aterm_messages::drive::Lay {
    aterm_messages::drive::Lay {
        cols,
        links: Links::Painted,
        home: band_home,
    }
}

impl App {
    /// Where window `wid`'s band cells sit on its glass ([`BandGeometry`]).
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

    /// The band's layout at `cols`.
    pub(crate) fn band_presentation(&self, cols: usize) -> Presentation {
        (self.band_lay)(cols).presentation(&self.messages)
    }

    /// If window pixel `(x, y)` lands on one of window `wid`'s band rows between the strip and the grid, which, and at which cell column.
    pub(crate) fn band_hit_at(&self, wid: WindowId, x: f64, y: f64) -> Option<BandTarget> {
        let (fx, fy) = self.window_to_frame(wid, x, y);
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let cols = self.windows.get(&wid).map_or(0, |ws| usize::from(ws.cols));
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
            usize::from(self.message_band_rows),
        )
    }

    /// What a press at `target` in window `wid` lands on, through the width
    /// law at this window's width — the same layout the painter drew.
    fn band_hit(&self, wid: WindowId, target: BandTarget) -> Hit {
        let BandTarget { row, col } = target;
        let Some(cols) = self.windows.get(&wid).map(|ws| usize::from(ws.cols)) else {
            return Hit::Nothing;
        };
        self.band_presentation(cols).hit(row, col)
    }

    /// A press on window `wid`'s band rows between the strip and the grid (design §3.4).
    pub(crate) fn press_band(&mut self, wid: WindowId, target: BandTarget) {
        match self.band_hit(wid, target) {
            Hit::Capsule(id, k) => {
                let intent = self.messages.act(id, k, std::time::Instant::now());
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

    /// Whether a press at `target` would do something.
    pub(crate) fn band_press_acts(&self, wid: WindowId, target: BandTarget) -> bool {
        matches!(
            self.band_hit(wid, target),
            Hit::Capsule(..) | Hit::Details(_) | Hit::Body(_) | Hit::Overflow
        )
    }

    /// The chip to light for a pointer at `target`.
    pub(crate) fn band_hover_for(&self, wid: WindowId, target: BandTarget) -> Option<BandHover> {
        let row = target.row;
        let row = u8::try_from(row).ok()?;
        let target = match self.band_hit(wid, target) {
            Hit::Capsule(_, k) => HoverTarget::Capsule(k),
            Hit::Details(_) => HoverTarget::Capsule(ActionIndex::DETAILS),
            Hit::Body(_) | Hit::Overflow => HoverTarget::Body,
            Hit::Nothing => return None,
        };
        Some(BandHover { row, target })
    }

    /// Update the hovered band chip on `CursorMoved`, clearing it when the pointer leaves the band.
    pub(crate) fn track_band_hover(&mut self, wid: WindowId, x: f64, y: f64) {
        let hover = self
            .band_hit_at(wid, x, y)
            .and_then(|target| self.band_hover_for(wid, target));
        self.set_band_hover(wid, hover);
    }

    /// The lit chip follows the layout under a still pointer.
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

    /// Write the band hover, requesting a redraw only on a change.
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

/// Distance in linear light (`0..=1` per channel) from colour `p` to the convex hull of `corners`.
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

/// The `(ink, ground)` pairs under column `x`'s glyph as the renderer draws it.
#[cfg(test)]
pub(crate) fn word_grounds(
    row: &[RenderCell],
    raster: Option<&RowRaster>,
    geom: BandGeometry,
    x: usize,
) -> Vec<([u8; 3], [u8; 3])> {
    let cell = &row[x];
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

/// The worst contrast any word on `row` has on the ground under its glyph ([`word_grounds`]), and where.
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

    /// A center with the toolchain meter row and a downloading update row committed.
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

    /// The band painted at `center`'s still frame over `p` on the unit grid: the time-free
    /// picture most of these tests read (a held bar at its data, a busy row's unlit track).
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

    /// Every admitted glyph lands in the glyph cell with text presentation, so the raster path
    /// never takes its colour-emoji face (`every_admitted_glyph_paints_a_non_blank_cell_headless`
    /// is the raster half).
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

    /// Under Windows High Contrast the capsules are `[label]`, and the hovered one keeps its
    /// brackets.
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

    /// The pixel → row/column map: the band rows under the strip, the column
    /// from the cell lattice; off the band rows is `None`.
    #[test]
    fn band_target_for_pixel_maps_the_chrome_rows_in_order() {
        let (pad, top) = (4, 10);
        let lattice = |cw| BandGeometry {
            win_w: 80 * cw + 2 * pad,
            cells_x: pad,
            cell_w: cw,
        };
        let cell = lattice(8);
        let hit = |gx, gy| band_target_for_pixel(gx, gy, cell, 16, top, 1, 2);
        assert_eq!(hit(40, top + 5), None, "the strip is not the band");
        assert_eq!(
            hit(40, top + 16),
            Some(BandTarget {
                row: 0,
                col: (40 - pad) / 8
            })
        );
        assert_eq!(hit(4, top + 47), Some(BandTarget { row: 1, col: 0 }));
        assert_eq!(hit(40, top + 48), None, "the grid starts below the band");
        assert_eq!(hit(40, 3), None, "above the lattice");
        assert_eq!(
            band_target_for_pixel(40, top + 20, cell, 16, top, 1, 0),
            None
        );
        assert_eq!(
            band_target_for_pixel(0, top, cell, 16, top, 0, 1),
            Some(BandTarget { row: 0, col: 0 })
        );
        assert_eq!(
            band_target_for_pixel(0, top, lattice(0), 0, top, 0, 1),
            Some(BandTarget { row: 0, col: 0 })
        );
    }

    /// The App's own hit test through the window's real cell lattice: every pixel of a painted
    /// capsule hits, acts and lights that chip; the body acts and lights `Details ›`; the
    /// overflow row is one link; the rows above and below are not the band.
    #[test]
    fn band_capsules_are_hit_at_the_pixels_they_are_painted_at() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
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
                let target = BandTarget { row: 0, col };
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
        let (tx, ty) = at(layout.title.0);
        let body = BandTarget {
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
        assert_eq!(app.band_hit_at(wid, tx, ty - ch as f64), None);
        assert_eq!(app.band_hit_at(wid, tx, ty + ch as f64), None);
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
        let body = BandTarget {
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
        for i in 0..4 {
            app.post_message(Message::new(tags::CONFIG, Severity::Warn, format!("w{i}")));
        }
        assert_eq!(app.message_band_rows, aterm_messages::MAX_ROWS);
        let overflow = BandTarget { row: 2, col: 4 };
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
            Some(BandTarget {
                row: 2,
                col: layout.title.0
            })
        );
    }

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

    /// A band in motion: a determinate download, busy work with no fraction (`Meter::busy`:
    /// the engine decides motion from the meter's state, never the hold), and a held warning
    /// below them, all committed.
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

    /// A held row has no motion and asks for no frame: its
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

    /// High Contrast motion uses only palette inks:
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

    /// Every glyph on a metered row with all three chip roles clears AA on its own cell, with
    /// the fill's edge under every column and through the glint, the Complete echo and the Fault
    /// flash, on every builtin scheme.
    #[test]
    fn every_glyph_on_a_metered_row_clears_aa_in_every_frame() {
        use aterm_messages::{
            ANIM_FRAME, Duration, ECHO_FAULT_FLASH, ECHO_SWEEP, GLINT_DELAY, GLINT_TRAVEL, Outcome,
        };
        let cols = 120;
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
}

#[cfg(test)]
#[path = "band_raster_tests.rs"]
mod band_raster_tests;
