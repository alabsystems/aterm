// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The band's pixel raster and its inks on a real window: [`ink::paint_band`] through a comet, a
//! glint, an echo and a fault, over every builtin scheme's inks as a host derives them on the
//! theme blend ([`BarBase::Blend`]: with no ANSI hues, and with the scheme's own, which a host
//! lends as the web band does), and over the stock High Contrast palettes.

use crate::band_tests::{STOCK, row_inks, word_grounds};
use crate::ink::{
    self, AnsiHues, BandInks, BarBase, InkedCell, MeterInks, RowRaster, ThemeInks, WORD_AA,
    contrast, crisp_on_fill, delta_e, ensure_contrast, fill_ink, forced_ink, glint_step, hover_ink,
    mix3, outlined_inks,
};
use crate::paint::{Geometry, Hover, HoverTarget, Icon};
use crate::palette::{BORROW_CHROMA, MeterHue, chroma, lin, oklch, reads_brown};
use crate::{
    Anim, BandMotion, COMET_PERIOD, CapsuleRole, Duration, ECHO_FAULT_FLASH, ECHO_SWEEP, EchoKind,
    GLINT_DELAY, GLINT_TRAVEL, GLYPH_COL, Glyph, Hold, Instant, Intent, Links, Look, Message,
    MessageCenter, MessageLog, Meter, Outcome, Pace, Presentation, Severity, Tone, WallStamp, tags,
};

const COLS: usize = 80;
const CELL_W: usize = 16;
const PAD: usize = 8;

/// The flat look a forced palette draws in.
const FLAT: Look = Look {
    pace: Pace::Moving,
    graded: false,
};

fn geom() -> Geometry {
    Geometry {
        win_w: COLS * CELL_W + 2 * PAD,
        cells_x: PAD,
        cell_w: CELL_W,
    }
}

fn t0() -> Instant {
    Instant::now()
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn stamp() -> WallStamp {
    WallStamp { unix_ms: 1 }
}

fn present_at(c: &MessageCenter, cols: usize) -> Presentation {
    c.presentation(cols, &crate::text::char_width, Links::Painted)
}

fn present(c: &MessageCenter) -> Presentation {
    present_at(c, COLS)
}

/// One builtin scheme: its theme inks and its ANSI hues.
struct Scheme {
    name: &'static str,
    theme: ThemeInks,
    ansi: AnsiHues,
}

/// Every builtin scheme, the default first.
fn schemes() -> Vec<Scheme> {
    let bytes = |c: aterm_types::Rgb| [c.r, c.g, c.b];
    let names = aterm_types::scheme::builtin_names();
    assert!(names.len() > 2, "CONTROL: built-ins to sweep");
    names
        .into_iter()
        .map(|name| {
            let s = aterm_types::scheme::builtin(name).expect("a listed built-in");
            let fg = bytes(s.foreground);
            Scheme {
                name,
                theme: ThemeInks {
                    bg: bytes(s.background),
                    fg,
                    cursor: s.cursor.map_or(fg, bytes),
                },
                ansi: AnsiHues {
                    red: bytes(s.ansi[1]),
                    green: bytes(s.ansi[2]),
                    yellow: bytes(s.ansi[3]),
                    blue: bytes(s.ansi[4]),
                    cyan: bytes(s.ansi[6]),
                },
            }
        })
        .collect()
}

/// Every builtin scheme's inks on the theme blend, with no ANSI hues and with its own.
fn sweep() -> Vec<(String, BandInks)> {
    bare().into_iter().chain(hued()).collect()
}

/// Every builtin scheme's inks on the theme blend with no ANSI hues.
fn bare() -> Vec<(String, BandInks)> {
    schemes()
        .into_iter()
        .map(|s| {
            (
                s.name.to_string(),
                BandInks::derive(s.theme, None, BarBase::Blend),
            )
        })
        .collect()
}

/// Every builtin scheme's inks on the theme blend with its own ANSI hues.
fn hued() -> Vec<(String, BandInks)> {
    schemes()
        .into_iter()
        .map(|s| {
            (
                format!("{} (its ANSI hues)", s.name),
                BandInks::derive(s.theme, Some(s.ansi), BarBase::Blend),
            )
        })
        .collect()
}

/// The default scheme's inks with no ANSI hues.
fn default_inks() -> BandInks {
    BandInks::derive(schemes()[0].theme, None, BarBase::Blend)
}

type Painted = (Vec<Vec<InkedCell>>, Vec<Option<RowRaster>>);

/// `p` painted at `motion` on `c` (forced when `forced`) over `geom`.
fn paint_on(
    p: &Presentation,
    c: &BandInks,
    forced: bool,
    hover: Option<Hover>,
    geom: Geometry,
    motion: &BandMotion,
) -> Painted {
    ink::paint_band(p, hover, geom, motion, forced, c)
        .into_iter()
        .map(|r| (r.cells, r.raster))
        .unzip()
}

/// One frame of `center`'s motion over `p` at `at` in `look`, painted on the window.
fn frame(
    center: &MessageCenter,
    p: &Presentation,
    at: Instant,
    look: Look,
    c: &BandInks,
) -> (Vec<Vec<InkedCell>>, Vec<Option<RowRaster>>, BandMotion) {
    let m = center.motion(p, at, look);
    let (rows, rasters) = paint_on(p, c, false, None, geom(), &m);
    (rows, rasters, m)
}

/// [`frame`] with `hover`.
fn frame_in(
    center: &MessageCenter,
    p: &Presentation,
    at: Instant,
    look: Look,
    c: &BandInks,
    hover: Option<Hover>,
) -> Painted {
    let m = center.motion(p, at, look);
    paint_on(p, c, false, hover, geom(), &m)
}

fn glyph_px(x: usize) -> std::ops::Range<usize> {
    PAD + x * CELL_W..PAD + (x + 1) * CELL_W
}

fn download(permille: u16) -> Message {
    Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.91.0")
        .meter(Meter {
            fill_permille: Some(permille),
            stats: "31 MB / 74 MB".into(),
            ..Meter::default()
        })
        .hold(Hold::Live {
            stale_after: crate::STALE_UPDATE,
        })
}

fn busy(title: &str) -> Message {
    Message::new(tags::PACKAGES, Severity::Info, title)
        .line("detail")
        .meter(Meter::busy("~3 GB"))
        .hold(Hold::Live {
            stale_after: crate::STALE_TAILED,
        })
}

/// A center with `msg` committed at `now`.
fn one_row(msg: Message, now: Instant) -> MessageCenter {
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    c.post(msg, stamp(), now);
    c.commit_rows(now, 3);
    c
}

/// The worst contrast any word on `row` has on the window pixels under its glyph, a drawn icon
/// taking its neighbours' pixels where they are blank, and where.
fn worst_word_contrast(row: &[InkedCell], raster: Option<&RowRaster>) -> (f64, String) {
    let mut worst = (f64::INFINITY, String::new());
    for (x, cell) in row.iter().enumerate() {
        if cell.ch == ' ' {
            continue;
        }
        let owned = raster.is_some_and(|r| {
            r.own
                .iter()
                .any(|&(a, b)| (usize::from(a)..usize::from(b)).contains(&x))
                || r.rings
                    .iter()
                    .any(|&(a, b, ..)| (usize::from(a)..usize::from(b)).contains(&x))
        });
        let spills = raster.is_some_and(|r| r.icons.iter().any(|&(c, _)| usize::from(c) == x))
            && x > 0
            && row.get(x - 1).is_some_and(|n| n.ch == ' ')
            && row.get(x + 1).is_some_and(|n| n.ch == ' ');
        let pixels = if spills {
            glyph_px(x - 1).start..glyph_px(x + 1).end
        } else {
            glyph_px(x)
        };
        let mut grounds: Vec<([u8; 3], [u8; 3])> = Vec::new();
        match raster.filter(|r| !r.ground.is_empty() && !owned) {
            Some(r) => {
                for px in pixels {
                    if r.edge == Some(px as u32)
                        || r.line.is_some_and(|(a, b)| (a..b).contains(&(px as u32)))
                    {
                        continue;
                    }
                    let ink = match r.split {
                        Some((col, sx, ink, _)) if usize::from(col) == x && (px as u32) < sx => ink,
                        _ => cell.fg,
                    };
                    grounds.push((ink, r.ground[px]));
                }
            }
            None => grounds.push((cell.fg, cell.bg)),
        }
        for (ink, g) in grounds {
            let ratio = contrast(ink, g);
            if ratio < worst.0 {
                worst = (ratio, format!("col {x} {:?}: {ink:?} on {g:?}", cell.ch));
            }
        }
    }
    worst
}

/// [`worst_word_contrast`] on the unit grid, each glyph on its own cell's grounds.
fn worst_on_cells(row: &[InkedCell], raster: Option<&RowRaster>) -> (f64, String) {
    let geom = Geometry::cells_only(row.len());
    let mut worst = (f64::INFINITY, String::new());
    for (x, cell) in row.iter().enumerate() {
        if cell.ch == ' ' {
            continue;
        }
        for (ink, g) in word_grounds(row, raster, geom, x) {
            let ratio = contrast(ink, g);
            if ratio < worst.0 {
                worst = (ratio, format!("col {x} {:?}: {ink:?} on {g:?}", cell.ch));
            }
        }
    }
    worst
}

/// Distance in linear light (`0..=1` per channel) from colour `p` to the convex hull of `corners`.
fn hull_distance(p: [u8; 3], corners: [[u8; 3]; 3]) -> f64 {
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

/// Whether `px` lies in the solid the four `corners` span, in linear light, or within 0.01 of a face.
fn inside_solid(px: [u8; 3], corners: [[u8; 3]; 4]) -> bool {
    let to_lin = |c: [u8; 3]| c.map(lin);
    let [t, f, e, k] = corners;
    let near_a_face = || {
        [[t, f, e], [t, e, k], [t, f, k], [f, e, k]]
            .into_iter()
            .any(|face| hull_distance(px, face) < 0.01)
    };
    let [o, a, b, c] = corners.map(to_lin);
    let sub = |x: [f64; 3], y: [f64; 3]| [x[0] - y[0], x[1] - y[1], x[2] - y[2]];
    let (u, v, w, q) = (sub(a, o), sub(b, o), sub(c, o), sub(to_lin(px), o));
    let det = |x: [f64; 3], y: [f64; 3], z: [f64; 3]| {
        x[0].mul_add(
            y[1].mul_add(z[2], -(y[2] * z[1])),
            (-y[0]).mul_add(
                x[1].mul_add(z[2], -(x[2] * z[1])),
                z[0] * x[1].mul_add(y[2], -(x[2] * y[1])),
            ),
        )
    };
    let d = det(u, v, w);
    if d.abs() < 1e-12 {
        return near_a_face();
    }
    let (l1, l2, l3) = (det(q, v, w) / d, det(u, q, w) / d, det(u, v, q) / d);
    (l1 >= 0.0 && l2 >= 0.0 && l3 >= 0.0 && l1 + l2 + l3 <= 1.0) || near_a_face()
}

/// A busy row's comet steps by at most two levels between neighbouring window pixels, on every
/// scheme's inks with no ANSI hues. With its own hues it does not hold everywhere: Solarized
/// Light's comet steps three levels.
#[test]
fn a_comet_is_smooth_at_pixel_resolution() {
    for (name, c) in bare() {
        let now = t0();
        let center = one_row(busy("Installing ALab tools"), now);
        let p = present(&center);
        let mut worst = (0u8, String::new());
        for k in 0..30u32 {
            let at = now + COMET_PERIOD * k / 30;
            let (_, rasters, _) = frame(&center, &p, at, Look::MOVING, &c);
            let r = rasters[0].as_ref().expect("a busy row has a raster");
            for (x, pair) in r.ground.windows(2).enumerate() {
                let d = (0..3)
                    .map(|i| pair[0][i].abs_diff(pair[1][i]))
                    .max()
                    .unwrap_or(0);
                if d > worst.0 {
                    worst = (d, format!("k={k} px {x}: {:?} → {:?}", pair[0], pair[1]));
                }
            }
        }
        assert!(
            worst.0 <= 2,
            "{name}: a step of {} levels at {}",
            worst.0,
            worst.1
        );
    }
}

/// A busy row's word inks hold through a whole comet period, every word at AA on what is under it.
#[test]
fn busy_row_inks_hold_across_a_comet_period() {
    for (name, c) in sweep() {
        let now = t0();
        let center = one_row(busy("Installing ALab tools"), now);
        let p = present(&center);
        let mut first: Option<Vec<[u8; 3]>> = None;
        let mut k = 0u64;
        while k * 33 <= u64::try_from(COMET_PERIOD.as_millis()).unwrap() {
            let (rows, rasters, _) = frame(&center, &p, now + ms(k * 33), Look::MOVING, &c);
            let inks: Vec<[u8; 3]> = rows[0]
                .iter()
                .enumerate()
                .filter(|(x, cell)| cell.ch != ' ' && *x != GLYPH_COL)
                .map(|(_, cell)| cell.fg)
                .collect();
            match &first {
                None => first = Some(inks),
                Some(f) => assert_eq!(&inks, f, "{name}: an ink moved at +{} ms", k * 33),
            }
            let (ratio, at) = worst_word_contrast(&rows[0], rasters[0].as_ref());
            assert!(
                ratio >= WORD_AA - 0.02,
                "{name} +{} ms: {ratio:.2}:1 at {at}",
                k * 33
            );
            k += 1;
        }
    }
}

/// The glint peaks at least ΔE 6 off the fill on the fill's side of its edge (the track past the
/// edge is not the glint), and every word clears AA through its travel.
#[test]
fn the_glint_reads_and_every_word_clears_aa_through_its_travel() {
    for (name, c) in sweep() {
        let now = t0();
        let center = one_row(download(570), now);
        let p = present(&center);
        let (inks, _) = row_inks(&c, &p.rows[0], false);
        let mut peak = 0.0f64;
        let mut t = Duration::ZERO;
        while t <= GLINT_TRAVEL {
            let (rows, rasters, _) = frame(&center, &p, now + GLINT_DELAY + t, Look::MOVING, &c);
            let r = rasters[0].as_ref().expect("a bar has a raster");
            let fill_side = usize::try_from(r.edge.expect("a bar's fill has an edge"))
                .unwrap()
                .saturating_sub(1);
            for px in r.ground[..fill_side]
                .iter()
                .chain(r.rail.iter().take(fill_side).flatten())
            {
                peak = peak.max(delta_e(*px, inks.fill));
            }
            let (ratio, at) = worst_word_contrast(&rows[0], Some(r));
            assert!(
                ratio >= WORD_AA - 0.02,
                "{name} glint +{t:?}: {ratio:.2}:1 at {at}"
            );
            t += ms(33);
        }
        assert!(peak >= 6.0, "{name}: the glint's peak is ΔE {peak:.1}");
    }
}

/// The fill's edge is one antialiased pixel, fill before it and track after it, and the cell it
/// cuts splits there in the crisp ink (the icon's cell when the edge is under the icon).
#[test]
fn the_fill_edge_is_one_antialiased_pixel_and_splits_its_cell() {
    let c = default_inks();
    for permille in (3..=997u16).step_by(7) {
        let now = t0();
        let center = one_row(download(permille), now);
        let p = present(&center);
        let (rows, rasters, m) = frame(&center, &p, now, Look::STILL, &c);
        let r = rasters[0].as_ref().expect("a raster");
        let (inks, _) = row_inks(&c, &p.rows[0], false);
        let edge = m.rows[0].surface.edge.expect("an edge inside the row");
        let win = geom().win_w as u64;
        let e = usize::try_from(u64::from(edge) * win / u64::from(crate::ROW)).unwrap();
        let mixed: Vec<usize> = r
            .ground
            .iter()
            .enumerate()
            .filter(|(_, px)| **px != inks.fill && **px != inks.track)
            .map(|(x, _)| x)
            .collect();
        assert!(
            mixed.len() <= 1 && mixed.iter().all(|x| x.abs_diff(e) <= 1),
            "{permille}: one AA pixel at the edge {e}: {mixed:?}"
        );
        assert!(
            r.ground[..e.saturating_sub(1)]
                .iter()
                .all(|px| *px == inks.fill)
        );
        assert!(r.ground[e + 1..].iter().all(|px| *px == inks.track));
        let (ratio, at) = worst_word_contrast(&rows[0], Some(r));
        assert!(ratio >= WORD_AA - 0.02, "{permille}: {ratio:.2}:1 at {at}");
        if let Some((col, x, ink, _)) = r.split {
            let col = usize::from(col);
            let cell = if col == GLYPH_COL {
                glyph_px(col - 1).start..glyph_px(col + 1).end
            } else {
                glyph_px(col)
            };
            assert!(
                (cell.start..=cell.end).contains(&(x as usize))
                    || !(PAD..PAD + COLS * CELL_W).contains(&e),
                "{permille}: the split {x} is in its cell {cell:?}"
            );
            if (glyph_px(0).start..glyph_px(2).end).contains(&e) {
                assert_eq!(col, GLYPH_COL, "{permille}: the icon takes it");
                assert_ne!(
                    rows[0][col].fg, ink,
                    "{permille}: the icon's own ink is the track's"
                );
            }
            assert_eq!(ink, crisp_on_fill(&c, c.accent, &inks));
            assert!(
                x as usize == e || x as usize == e + 1,
                "{permille}: split at the edge pixel {e}: {x}"
            );
        }
    }
}

/// Every frame of `msg`'s Fault echo, 1.5 s after it was posted, every 30 ms for 600 ms.
fn fault_frames(
    c: &BandInks,
    msg: Message,
    mut each: impl FnMut(u64, &Presentation, &[Vec<InkedCell>], &[Option<RowRaster>], &BandMotion),
) {
    let now = t0();
    let mut center = MessageCenter::new(MessageLog::empty(), now);
    let id = center.post(msg, stamp(), now).id;
    center.commit_rows(now, 3);
    let at = now + ms(1500);
    assert!(center.resolve(id, Outcome::Warn, at));
    let p = present(&center);
    for k in 0..=20u64 {
        let (rows, rasters, m) = frame(&center, &p, at + ms(k * 30), Look::MOVING, c);
        each(k * 30, &p, &rows, &rasters, &m);
    }
}

/// A Fault echo's pixels stay in the hull of the track, the fill and warn (a pastel's edge line in
/// the solid its edge ink adds), and a busy row's on the comet's line.
#[test]
fn the_fault_echo_stays_inside_its_hull() {
    for (name, c) in sweep() {
        fault_frames(&c, download(600), |t, p, _, rasters, m| {
            if m.rows[0].fade > 0 {
                return;
            }
            let (inks, _) = row_inks(&c, &p.rows[0], false);
            let r = rasters[0].as_ref().expect("a raster");
            for (x, px) in r.ground.iter().enumerate() {
                if let (Some((a, b)), Some(edge)) = (r.line, c.meter_edge)
                    && (a..b).contains(&(x as u32))
                {
                    assert!(
                        inside_solid(*px, [inks.track, inks.fill, edge, inks.warn]),
                        "{name} +{t} ms px {x}: {px:?} leaves the edge's solid"
                    );
                    continue;
                }
                let d = hull_distance(*px, [inks.track, inks.fill, inks.warn]);
                assert!(d < 0.01, "{name} +{t} ms px {x}: {px:?} is {d:.4} outside");
            }
        });
        fault_frames(&c, busy("Installing Homebrew"), |t, p, _, rasters, m| {
            if m.rows[0].fade > 0 {
                return;
            }
            let (inks, _) = row_inks(&c, &p.rows[0], false);
            let r = rasters[0].as_ref().expect("a raster");
            for (x, px) in r.ground.iter().enumerate() {
                let d = hull_distance(*px, [inks.track, inks.fill, inks.fill]);
                assert!(
                    d < 0.01,
                    "{name} busy +{t} ms px {x}: {px:?} left the comet's line"
                );
            }
        });
    }
}

/// A busy row's Fault says `failed` in its elapsed slot, every word at AA, until it fades.
#[test]
fn failed_keeps_aa_through_a_busy_fault() {
    for (name, c) in sweep() {
        fault_frames(&c, busy("Installing Homebrew"), |t, p, rows, rasters, m| {
            if m.rows[0].fade > 127 {
                return;
            }
            let col = p.rows[0].elapsed.expect("the elapsed slot");
            let words: String = rows[0][col..col + 6].iter().map(|cell| cell.ch).collect();
            assert_eq!(words, "failed", "{name} +{t} ms");
            let (ratio, at) = worst_word_contrast(&rows[0], rasters[0].as_ref());
            assert!(
                ratio >= WORD_AA - 0.02,
                "{name} +{t} ms: {ratio:.2}:1 at {at}"
            );
        });
    }
}

/// Every echo's words stay at AA on what is under them until the echo is half faded.
#[test]
fn an_echo_tail_keeps_its_words_legible_to_the_midpoint() {
    for (name, c) in sweep() {
        for kind in [EchoKind::Complete, EchoKind::Fault, EchoKind::Vanish] {
            for msg in [download(800), busy("Installing Homebrew")] {
                let now = t0();
                let mut center = MessageCenter::new(MessageLog::empty(), now);
                let id = center.post(msg, stamp(), now).id;
                center.commit_rows(now, 3);
                let at = now + ms(1500);
                match kind {
                    EchoKind::Complete => assert!(center.resolve(id, Outcome::Ok, at)),
                    EchoKind::Fault => assert!(center.resolve(id, Outcome::Warn, at)),
                    EchoKind::Vanish => assert!(center.withdraw(id, at)),
                }
                let p = present(&center);
                let until = center.echoes()[0].until;
                let mut t = at;
                while t < until {
                    let (rows, rasters, m) = frame(&center, &p, t, Look::MOVING, &c);
                    if m.rows[0].fade <= 127 {
                        let (ratio, at_) = worst_word_contrast(&rows[0], rasters[0].as_ref());
                        assert!(
                            ratio >= WORD_AA - 0.02,
                            "{name} {kind:?} +{:?} fade {}: {ratio:.2}:1 at {at_}",
                            t - at,
                            m.rows[0].fade
                        );
                    }
                    t += crate::ANIM_FRAME;
                }
            }
        }
    }
}

/// The Complete echo's sweep is a glint no wider than seven cells that crosses the row once,
/// left to right, seen on at least ten frames.
#[test]
fn the_complete_echo_sweeps_once_across_the_row() {
    for (name, c) in sweep() {
        let now = t0();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        let id = center.post(download(700), stamp(), now).id;
        center.commit_rows(now, 3);
        let at = now + ms(1500);
        assert!(center.resolve(id, Outcome::Ok, at));
        let p = present(&center);
        let (inks, _) = row_inks(&c, &p.rows[0], false);
        let fill = crate::animate::glide_span(700, 1000);
        let mut prev_peak: Option<usize> = None;
        let mut seen = 0;
        let mut t = fill + ms(20);
        while t < fill + ECHO_SWEEP {
            let (_, rasters, m) = frame(&center, &p, at + t, Look::MOVING, &c);
            assert!(matches!(m.rows[0].anim, Anim::Echo { .. }));
            let r = rasters[0].as_ref().expect("a raster");
            let lifted: Vec<usize> = r
                .ground
                .iter()
                .chain(r.rail.iter().flatten())
                .enumerate()
                .filter(|(_, px)| **px != inks.fill)
                .map(|(x, _)| x % r.ground.len().max(1))
                .collect();
            if let (Some(&lo), Some(&hi)) = (lifted.first(), lifted.last()) {
                let mid = (lo + hi) / 2;
                assert!(
                    hi - lo <= 7 * CELL_W,
                    "{name}: the sweep is a glint, not a flood"
                );
                if let Some(p0) = prev_peak {
                    assert!(
                        mid + CELL_W >= p0,
                        "{name}: the sweep went back at +{t:?}: {lo}..{hi} after a peak at {p0}"
                    );
                }
                prev_peak = Some(mid);
                seen += 1;
            }
            t += ms(16);
        }
        assert!(seen >= 10, "{name}: the sweep was seen on {seen} frames");
    }
}

/// Only a graded look draws a raster: the flat look's cells say it all.
#[test]
fn only_graded_looks_raster() {
    let c = default_inks();
    let now = t0();
    let center = one_row(download(400), now);
    let p = present(&center);
    assert!(frame(&center, &p, now, Look::MOVING, &c).1[0].is_some());
    assert!(frame(&center, &p, now, Look::STILL, &c).1[0].is_some());
    let r = frame(&center, &p, now, FLAT, &c).1[0].clone();
    assert!(
        r.as_ref()
            .is_none_or(|r| r.ground.is_empty() && r.rail.is_empty() && r.rings.is_empty()),
        "{r:?}"
    );
}

/// On a metered row the Primary is an accent ring round exactly its cells over a band-ground
/// inside, its bold label at AA, lit by hover; the Secondary rides the meter; a plain row's
/// Primary is a solid chip. On every scheme's inks with its own ANSI hues: with none, Dracula's
/// lit ring does not lift.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one Primary on two metered rows and a plain one, at rest and lit, on every scheme"
)]
fn a_meter_rows_primary_is_an_accent_ring_and_a_plain_rows_is_solid() {
    let open_packages = || Intent::OpenSettings {
        route: "/packages".into(),
    };
    let rows_with_meter = [
        download(420)
            .action(Intent::ApplyUpdate { build: 7 })
            .action(open_packages()),
        busy("Installing Homebrew")
            .action(Intent::NewWindow)
            .action(open_packages()),
    ];
    for (name, c) in hued() {
        for msg in &rows_with_meter {
            let now = t0();
            let center = one_row(msg.clone(), now);
            let p = present(&center);
            let cap = p.rows[0]
                .capsules
                .iter()
                .find(|k| k.role == CapsuleRole::Primary)
                .expect("a Primary")
                .clone();
            let span = (
                u16::try_from(cap.col).unwrap(),
                u16::try_from(cap.col + cap.width).unwrap(),
            );
            for look in [Look::MOVING, Look::STILL] {
                let at = now + ms(1500);
                let (rows, rasters) = frame_in(&center, &p, at, look, &c, None);
                let r = rasters[0].as_ref().expect("a metered row rasters");
                assert_eq!(r.rings.len(), 1, "{name} {}: one ring", msg.title);
                let (a, b, ring, inner) = r.rings[0];
                assert_eq!((a, b), span, "{name}: the ring is the chip's cells");
                assert!(
                    !r.own.contains(&span),
                    "{name}: the meter runs on round the ring"
                );
                assert_eq!((ring, inner), (c.ring, c.bar_bg), "{name}: at rest");
                let secondary = p.rows[0]
                    .capsules
                    .iter()
                    .find(|k| k.role == CapsuleRole::Secondary)
                    .expect("a Secondary");
                let second = (
                    u16::try_from(secondary.col).unwrap(),
                    u16::try_from(secondary.col + secondary.width).unwrap(),
                );
                assert!(!r.own.contains(&second), "{name}: no chip ground kept");
                assert!(
                    r.rings.iter().all(|&(a, b, ..)| (a, b) != second),
                    "{name}: no ring"
                );
                for cell in rows[0].iter().skip(secondary.col).take(secondary.width) {
                    if !p.rows[0].busy {
                        assert_ne!(cell.bg, c.chip_ground, "{name}: no chip ground");
                    }
                    assert_ne!(cell.bg, c.bar_bg, "{name}: no band block over the bar");
                }
                assert!(contrast(ring, inner) >= 3.0, "{name}: the ring reads");
                for (x, cell) in rows[0].iter().enumerate().skip(cap.col).take(cap.width) {
                    assert_eq!(cell.bg, inner, "{name} col {x}: the ring's inside");
                    assert_ne!(cell.bg, c.meter, "{name} col {x}: no solid accent");
                    if cell.ch != ' ' {
                        let ratio = contrast(cell.fg, inner);
                        assert!(ratio >= WORD_AA, "{name} col {x}: label {ratio:.2}:1");
                        assert!(cell.bold, "{name}: the Primary's label stays bold");
                    }
                }
                let hover = Hover {
                    row: 0,
                    target: HoverTarget::Capsule(cap.action),
                };
                let (lit_rows, lit) = frame_in(&center, &p, at, look, &c, Some(hover));
                let (_, _, lring, linner) = lit[0].as_ref().unwrap().rings[0];
                assert_ne!(lring, ring, "{name}: the lit ring lifts");
                assert_ne!(linner, inner, "{name}: the lit inside tints");
                assert!(contrast(lring, linner) >= 3.0, "{name}: lit ring");
                for (x, cell) in lit_rows[0]
                    .iter()
                    .enumerate()
                    .skip(cap.col + 1)
                    .take(cap.width - 2)
                {
                    assert_eq!(cell.bg, linner, "{name}: the lit inside");
                    if cell.ch != ' ' {
                        let ratio = contrast(cell.fg, linner);
                        assert!(ratio >= WORD_AA, "{name} lit col {x}: {ratio:.2}:1");
                    }
                }
            }
        }
        let now = t0();
        let center = one_row(
            Message::new(tags::UPDATE, Severity::Warn, "aterm v0.93.0 is ready")
                .action(Intent::ApplyUpdate { build: 7 })
                .hold(Hold::STANDING),
            now,
        );
        let p = present(&center);
        let cap = p.rows[0].capsules[0].clone();
        assert_eq!(cap.role, CapsuleRole::Primary);
        let (rows, rasters) = frame_in(&center, &p, now, Look::STILL, &c, None);
        assert!(
            rasters[0].as_ref().is_none_or(|r| r.rings.is_empty()),
            "{name}: no ring without a meter"
        );
        for (x, cell) in rows[0].iter().enumerate().skip(cap.col).take(cap.width) {
            assert_ne!(cell.bg, c.bar_bg, "{name} col {x}: a solid chip");
        }
    }
}

/// The outlined Primary's ring and label never read brown, the label clears AA and the ring 3:1
/// on its inside, and a ring that is not the meter's hue is the theme's blue floored on the band.
#[test]
fn the_outlined_primary_keeps_the_accent_family_on_every_scheme() {
    for s in schemes() {
        let name = s.name;
        let c = BandInks::derive(s.theme, Some(s.ansi), BarBase::Blend);
        for lit in [false, true] {
            let o = outlined_inks(&c, lit);
            assert!(
                !reads_brown(o.ring) && !reads_brown(o.label),
                "{name} lit {lit}: ring {:?} {:?}, label {:?} {:?}",
                o.ring,
                oklch(o.ring),
                o.label,
                oklch(o.label)
            );
            assert!(contrast(o.label, o.inner) >= WORD_AA, "{name}");
            assert!(contrast(o.ring, o.inner) >= 3.0, "{name}");
        }
        if c.ring != c.meter {
            assert_eq!(
                c.ring,
                ensure_contrast(s.ansi.blue, c.bar_bg, 3.0),
                "{name}: the theme's blue"
            );
        }
    }
}

/// The meter keeps the cursor's colour unless it is near grey, when it borrows the theme's blue
/// or cyan; its fill or edge clears 3:1, and the fill's first pixel is the meter colour.
#[test]
fn a_near_grey_cursor_borrows_blue_or_cyan_and_the_rest_keep_theirs() {
    for s in schemes() {
        let name = s.name;
        let base = BandInks::derive(s.theme, None, BarBase::Blend);
        let c = BandInks::derive(s.theme, Some(s.ansi), BarBase::Blend);
        let cursor = s.theme.cursor;
        let want = MeterHue::pick(
            cursor,
            s.ansi.blue,
            s.ansi.cyan,
            &[base.field_bg, base.bar_bg, base.value],
        );
        let cursor_fill = if c.meter_edge.is_some() {
            cursor
        } else {
            base.accent
        };
        if want == MeterHue::Cursor {
            assert_eq!(c.meter, cursor_fill, "{name}: the cursor-trail colour");
        } else {
            assert!(
                chroma(c.meter) >= BORROW_CHROMA,
                "{name}: the borrowed fill has colour: {:?}",
                c.meter
            );
        }
        match c.meter_edge {
            Some(edge) => assert!(
                contrast(edge, c.meter_track) >= 3.0,
                "{name}: the edge 3:1 on the track"
            ),
            None => assert!(
                contrast(c.meter, c.bar_bg) >= 3.0,
                "{name}: 3:1 on the band"
            ),
        }
        assert_eq!(base.meter, cursor_fill, "{name}: no hues to borrow");
        let now = t0();
        let center = one_row(download(500), now);
        let p = present(&center);
        let (_, rasters) = frame_in(&center, &p, now, Look::STILL, &c, None);
        assert_eq!(
            rasters[0].as_ref().unwrap().ground[0],
            c.meter,
            "{name}: the fill"
        );
    }
}

/// Only a pastel cursor whose floor would read brown keeps its own pastel fill, with a darker edge
/// line of the same hue that stands 3:1 off the track; every other fill is its floored hue with no
/// edge line. Of the builtins and Catppuccin Latte's upstream rosewater cursor, only the latter.
#[test]
fn only_a_pastel_cursor_keeps_a_pastel_fill_with_a_darker_edge() {
    const UPSTREAM_LATTE: &str = "Catppuccin Latte, upstream rosewater cursor";
    let mut subjects: Vec<(&str, ThemeInks, AnsiHues)> = schemes()
        .into_iter()
        .map(|s| (s.name, s.theme, s.ansi))
        .collect();
    let latte = *subjects
        .iter()
        .find(|(name, ..)| *name == "Catppuccin Latte")
        .expect("Catppuccin Latte is a builtin");
    subjects.push((
        UPSTREAM_LATTE,
        ThemeInks {
            cursor: [0xdc, 0x8a, 0x78],
            ..latte.1
        },
        latte.2,
    ));
    let mut pastels = Vec::new();
    for (name, theme, ansi) in subjects {
        let c = BandInks::derive(theme, Some(ansi), BarBase::Blend);
        let cursor = theme.cursor;
        let hue = MeterHue::pick(
            cursor,
            ansi.blue,
            ansi.cyan,
            &[c.field_bg, c.bar_bg, c.value],
        )
        .of(cursor, ansi.blue, ansi.cyan);
        let floored = ensure_contrast(hue, c.bar_bg, 3.0);
        let now = t0();
        for permille in [60u16, 420, 777, 993] {
            let center = one_row(download(permille), now);
            let p = present(&center);
            let (rows, rasters) = frame_in(&center, &p, now, Look::STILL, &c, None);
            let r = rasters[0].as_ref().expect("a bar has a raster");
            assert_eq!(r.ground[0], c.meter, "{name} {permille}: the fill");
            let Some(edge) = c.meter_edge else {
                assert_eq!(r.line, None, "{name} {permille}: no edge line");
                continue;
            };
            let (a, b) = r.line.expect("the pastel's edge line");
            let line: Vec<[u8; 3]> = r.ground[a as usize..b as usize].to_vec();
            assert!(
                line.iter()
                    .any(|px| px.iter().zip(edge).all(|(x, y)| x.abs_diff(y) <= 1)),
                "{name} {permille}: the line wears the edge ink {edge:?}: {line:?}"
            );
            assert!(
                r.ground[..a as usize].iter().all(|px| *px == c.meter),
                "{name} {permille}: the fill up to its line"
            );
            let (ratio, at) = worst_word_contrast(&rows[0], Some(r));
            assert!(
                ratio >= WORD_AA - 0.02,
                "{name} {permille}: {ratio:.2}:1 at {at}"
            );
        }
        match c.meter_edge {
            None => {
                assert_eq!(c.meter, floored, "{name}: the floored hue");
                assert_eq!(
                    c.meter_track,
                    mix3(c.bar_bg, floored, ink::TRACK_TINT),
                    "{name}: the track"
                );
            }
            Some(edge) => {
                pastels.push(name);
                assert_eq!(c.meter, hue, "{name}: the theme's own pastel");
                assert!(
                    reads_brown(floored),
                    "{name}: the floor it replaces read brown: {floored:?}"
                );
                assert_eq!(c.meter_track, mix3(c.bar_bg, hue, ink::TRACK_TINT));
                assert!(
                    contrast(edge, c.meter_track) >= 3.0,
                    "{name}: the edge stands 3:1 from the track"
                );
                assert!(
                    contrast(edge, c.meter) >= 1.4,
                    "{name}: the edge reads darker than the fill: {edge:?} on {:?}",
                    c.meter
                );
                assert!(
                    (oklch(edge).2 - oklch(hue).2).abs() < 3.0,
                    "{name}: the same rose, deepened: {edge:?}"
                );
                let center = one_row(download(993), now);
                let p = present(&center);
                let (rows, _) = frame_in(&center, &p, now, Look::STILL, &c, None);
                let ink = rows[0][p.rows[0].title.0].fg;
                assert!(
                    contrast(ink, c.meter) >= WORD_AA - 0.02,
                    "{name}: the title's {ink:?} on the pastel"
                );
            }
        }
    }
    assert_eq!(pastels, [UPSTREAM_LATTE]);
}

/// Every admitted glyph keeps its character in the glyph cell, in text presentation, and the
/// raster draws it as exactly its icon there, moving or still; a busy row never spins its glyph
/// and an echo draws its outcome's icon.
#[test]
fn every_rows_glyph_is_a_drawn_icon_and_its_character_stays() {
    let c = default_inks();
    let col = u16::try_from(GLYPH_COL).unwrap();
    let icons = |r: &Option<RowRaster>| r.as_ref().map_or_else(Vec::new, |r| r.icons.clone());
    for &ch in Glyph::ALLOWED {
        let now = t0();
        let center = one_row(
            Message::new(tags::SYSTEM, Severity::Info, "glyph").glyph(Glyph::new(ch).unwrap()),
            now,
        );
        let p = present(&center);
        let icon = Icon::for_char(ch).expect("every glyph has an icon");
        for look in [Look::MOVING, Look::STILL] {
            let (rows, rasters, _) = frame(&center, &p, now, look, &c);
            let cell = &rows[0][GLYPH_COL];
            assert_eq!(cell.ch, ch, "{ch:?}: the character stays");
            assert!(cell.text_presentation, "{ch:?}: text presentation");
            assert_eq!(icons(&rasters[0]), vec![(col, icon)], "{ch:?}");
        }
    }
    let now = t0();
    let center = one_row(busy("Installing Homebrew"), now);
    let p = present(&center);
    let own = p.rows[0].glyph.1;
    for k in 0..40u64 {
        let (rows, rasters, m) = frame(&center, &p, now + ms(33 * k), Look::MOVING, &c);
        assert!(matches!(m.rows[0].anim, Anim::Comet { .. }));
        assert_eq!(rows[0][GLYPH_COL].ch, own, "frame {k}: no spinner");
        assert_eq!(
            icons(&rasters[0]),
            vec![(col, Icon::for_char(own).unwrap())],
            "frame {k}"
        );
    }
    for (outcome, want) in [(Outcome::Ok, Icon::Success), (Outcome::Warn, Icon::Warn)] {
        let now = t0();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        let id = center.post(download(700), stamp(), now).id;
        center.commit_rows(now, 3);
        assert!(center.resolve(id, outcome, now + ms(1500)));
        let p = present(&center);
        let (_, rasters, m) = frame(&center, &p, now + ms(1600), Look::MOVING, &c);
        assert!(matches!(m.rows[0].anim, Anim::Echo { .. }), "{outcome:?}");
        assert_eq!(icons(&rasters[0]), vec![(col, want)], "{outcome:?}");
    }
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
                stale_after: crate::STALE_TAILED,
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
                stale_after: crate::STALE_UPDATE,
            }),
        stamp(),
        now,
    );
    assert_eq!(center.commit_rows(now, 3), Some(2));
    center
}

/// `p` painted at `center`'s still frame on the unit grid.
fn paint_still(
    p: &Presentation,
    c: &BandInks,
    forced: bool,
    hover: Option<Hover>,
    center: &MessageCenter,
) -> Vec<Vec<InkedCell>> {
    let m = center.motion(p, t0(), Look::STILL);
    paint_on(p, c, forced, hover, Geometry::cells_only(p.cols), &m).0
}

/// Every row is exactly `cols` wide and carries its title, the meter is laid out as the whole row,
/// and no procedural meter glyph is left on the band.
#[test]
fn paint_rows_are_exactly_cols_wide_and_carry_the_words() {
    let center = two_rows();
    let p = present_at(&center, 140);
    let rows = paint_still(&p, &default_inks(), false, None, &center);
    assert_eq!(rows.len(), 2);
    let text = |row: &[InkedCell]| row.iter().map(|c| c.ch).collect::<String>();
    for row in &rows {
        assert_eq!(row.len(), 140);
    }
    assert!(text(&rows[0]).contains("Installing ALab tools"));
    assert!(text(&rows[1]).contains("aterm update v0.48.0"));
    assert_eq!(p.rows[0].meter, Some((0, 140, 427)), "the meter is the row");
    assert!(
        rows[0]
            .iter()
            .all(|cell| !(0xFDD0..=0xFDEF).contains(&u32::from(cell.ch))),
        "no procedural meter glyph is left on the band"
    );
}

/// Under High Contrast a word on the fill takes `HIGHLIGHTTEXT`, the fill is `HIGHLIGHT` whatever
/// the row's severity, and the track is `WINDOW`.
#[test]
fn high_contrast_meter_is_highlight_with_highlight_text_on_it() {
    let center = two_rows();
    for (name, palette) in STOCK {
        let c = BandInks::forced(palette);
        let p = present_at(&center, 140);
        let rows = paint_still(&p, &c, true, None, &center);
        let tcol = p.rows[0].title.0;
        let edge = usize::from(p.rows[0].meter.unwrap().2) * 140 / 1000;
        assert!(tcol < edge, "{name}: the title sits on the fill");
        assert_eq!(rows[0][tcol].bg, palette.highlight, "{name}: the fill");
        assert_eq!(rows[0][tcol].fg, c.on_accent, "{name}: HIGHLIGHTTEXT");
        assert_eq!(rows[0][139].bg, c.meter_track, "{name}: the track");
    }
}

/// Under High Contrast the capsules are `[label]`, and the hovered one keeps its brackets.
#[test]
fn high_contrast_draws_capsules_as_brackets() {
    let center = two_rows();
    for (name, palette) in STOCK {
        let c = BandInks::forced(palette);
        let p = present_at(&center, 140);
        let rows = paint_still(&p, &c, true, None, &center);
        for cap in &p.rows[0].capsules {
            assert_eq!(rows[0][cap.col].ch, '[', "{name}");
            assert_eq!(rows[0][cap.col + cap.width - 1].ch, ']', "{name}");
        }
        let first = &p.rows[0].capsules[0];
        let lit = paint_still(
            &p,
            &c,
            true,
            Some(Hover {
                row: 0,
                target: HoverTarget::Capsule(first.action),
            }),
            &center,
        );
        assert_eq!(lit[0][first.col].ch, '[', "{name}: brackets stay");
    }
}

/// A held row has no motion and asks for no frame: its motion fingerprint is 0 at every frame,
/// and the engine's deadline is `None`.
#[test]
fn a_held_row_has_no_motion_and_asks_for_no_frame() {
    let now = t0();
    let center = one_row(
        Message::new(tags::CONFIG, Severity::Warn, "Font not found").line("Nope"),
        now,
    );
    let p = present(&center);
    for k in 0..=100u32 {
        let m = center.motion(&p, now + ms(50) * k, Look::MOVING);
        assert_eq!(m.fingerprint(), 0, "a held row has no motion");
    }
    assert_eq!(
        center.motion_deadline(&p, now, Look::MOVING),
        None,
        "…and asks for no frame"
    );
}

/// Under every stock High Contrast palette, in the flat look, every frame of a moving band (the
/// comet, the bar, the echo) grounds every cell in the band, the `WINDOW` track or the
/// `HIGHLIGHT` fill, never a mix between them.
#[test]
fn high_contrast_motion_uses_only_palette_inks() {
    for (name, palette) in STOCK {
        let c = BandInks::forced(palette);
        let allowed = [c.bar_bg, c.meter_track, c.accent, palette.highlight];
        let now = t0();
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
                        stale_after: crate::STALE_UPDATE,
                    }),
                stamp(),
                now,
            )
            .id;
        center.post(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab tools")
                .meter(Meter::busy(""))
                .hold(Hold::Live {
                    stale_after: crate::STALE_ANNOUNCE,
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
        let check = |center: &MessageCenter, from: Instant, span: Duration| {
            let p = present_at(center, 120);
            let mut t = Duration::ZERO;
            while t <= span {
                let m = center.motion(&p, from + t, FLAT);
                let rows = paint_on(&p, &c, true, None, Geometry::cells_only(120), &m).0;
                for (i, row) in rows.iter().enumerate() {
                    for (x, cell) in row.iter().enumerate() {
                        assert!(
                            allowed.contains(&cell.bg),
                            "{name} row {i} col {x}: {:?} is not a palette ground",
                            cell.bg
                        );
                    }
                }
                t += ms(50);
            }
        };
        check(&center, now, Duration::from_secs(4));
        let at = now + Duration::from_secs(4);
        assert!(center.resolve(download, Outcome::Ok, at));
        check(&center, at, ms(900));
    }
}

/// Every glyph on a metered row with all three chip roles clears AA on its own cell, with the
/// fill's edge under every column and through the glint, the Complete echo and the Fault flash,
/// on every builtin scheme.
#[test]
fn every_glyph_on_a_metered_row_clears_aa_in_every_frame() {
    let cols = 120;
    let check = |name: &str, what: &str, (row, raster): (&[InkedCell], Option<&RowRaster>)| {
        let (ratio, at) = worst_on_cells(row, raster);
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
                        stale_after: crate::STALE_UPDATE,
                    }),
                stamp(),
                now,
            )
            .id
    };
    let rasters_at = |center: &MessageCenter, p: &Presentation, at: Instant, look, c: &BandInks| {
        let m = center.motion(p, at, look);
        paint_on(p, c, false, None, Geometry::cells_only(p.cols), &m)
    };
    for (name, c) in sweep() {
        for permille in (0..=1000u16).step_by(4) {
            let now = t0();
            let mut center = MessageCenter::new(MessageLog::empty(), now);
            post(&mut center, permille, now);
            center.commit_rows(now, 3);
            let p = present_at(&center, cols);
            assert_eq!(p.rows[0].capsules.len(), 3, "{name}: the chips are drawn");
            let (rows, rasters) = rasters_at(&center, &p, t0(), Look::STILL, &c);
            check(
                &name,
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
                    let at = now + ms(1000);
                    assert!(center.resolve(id, o, at));
                    let span = if o == Outcome::Ok {
                        crate::animate::glide_span(800, 1000) + ECHO_SWEEP
                    } else {
                        ECHO_FAULT_FLASH
                    };
                    (at, span)
                }
            };
            let p = present_at(&center, cols);
            let mut t = Duration::ZERO;
            while t <= span {
                let (rows, rasters) = rasters_at(&center, &p, from + t, Look::MOVING, &c);
                check(
                    &name,
                    &format!("{permille} {outcome:?} t={t:?}"),
                    (&rows[0], rasters[0].as_ref()),
                );
                t += crate::ANIM_FRAME;
            }
        }
    }
}

/// The meter's floors on every scheme, for both fills a row can wear (the accent, or warn on a
/// Warn or Error row): some ink reaches AA on the fill, the glint is one fixed step of the fill
/// away from its words' ink and never costs them contrast, and the comet's head clears 3:1 on the
/// track wherever the comet's fill does.
#[test]
fn the_meter_floors_hold_on_every_builtin_scheme() {
    for (name, c) in sweep() {
        for fill in [c.accent, c.warn] {
            let inks = MeterInks::of(&c, fill);
            let comet = MeterInks::comet(&c, fill).0;
            let head = comet.rgb(Tone::HEAD.into());
            let words = fill_ink(&c, fill);
            assert_eq!(inks.glint, glint_step(fill, words, false), "{name}");
            assert!(
                contrast(ensure_contrast(c.label, fill, WORD_AA), fill) >= WORD_AA,
                "{name}: no ink reaches AA on {fill:?}"
            );
            assert!(
                contrast(words, inks.glint) >= contrast(words, fill) - 0.02,
                "{name}: the glint of {fill:?} costs its words contrast: {:.2}:1 on the glint, \
                 {:.2}:1 on the fill",
                contrast(words, inks.glint),
                contrast(words, fill)
            );
            if contrast(comet.fill, c.meter_track) >= 3.0 {
                assert!(
                    contrast(head, c.meter_track) >= 3.0,
                    "{name}: the comet head of {fill:?} on the track: {head:?} on {:?}",
                    c.meter_track
                );
            }
        }
    }
}

/// A Fault echo cross-fades the fill's coverage to warn (premultiplied, in linear light): on every
/// builtin scheme and stock High Contrast palette, for both fills a row can wear, each step lies
/// in the track–fill–warn triangle, with the fill (or the track) and warn as its ends.
#[test]
fn the_fault_wash_stays_inside_the_track_fill_warn_hull() {
    let check = |name: &str, c: &BandInks| {
        for fill in [c.accent, c.warn] {
            let inks = MeterInks::of(c, fill);
            let corners = [inks.track, inks.fill, inks.warn];
            let at = |fill, warn| {
                inks.rgb(
                    Tone {
                        fill,
                        lift: 0,
                        warn,
                    }
                    .into(),
                )
            };
            for cover in [255u8, 128, 1] {
                for u in 0..=255u16 {
                    let w = u8::try_from((u16::from(cover) * u + 127) / 255).unwrap();
                    let rgb = at(cover - w, w);
                    let d = hull_distance(rgb, corners);
                    assert!(
                        d < 0.01,
                        "{name}: cover {cover} warn {w} → {rgb:?} is {d:.4} outside"
                    );
                }
            }
            assert_eq!(at(255, 0), inks.fill, "{name}");
            assert_eq!(at(0, 255), inks.warn, "{name}");
            assert_eq!(at(0, 0), inks.track, "{name}");
        }
    };
    for (name, c) in sweep() {
        check(&name, &c);
    }
    for (name, palette) in STOCK {
        check(name, &BandInks::forced(palette));
    }
}

/// The capsule roles' floors on every scheme: the hovered label is the value ink lifted on the
/// risen fill at AA, the band reads 3:1 on the accent, the lit Primary is the band's ink at 3:1
/// on a fill at least [`ink::PRIMARY_HOVER_LIFT`] of the way from the accent toward the theme's
/// ink and unlike the grey hover, and the hover fill moves off the band wherever a 5% step keeps
/// the value ink at AA. Under each stock High Contrast palette the meter's track is `WINDOW`, the
/// ink on the accent is `HIGHLIGHTTEXT` made to read, and both hovers are `HIGHLIGHT` with inks at AA.
#[test]
fn the_capsule_roles_clear_their_floors_on_every_scheme_and_palette() {
    for s in schemes() {
        for c in [
            BandInks::derive(s.theme, None, BarBase::Blend),
            BandInks::derive(s.theme, Some(s.ansi), BarBase::Blend),
        ] {
            let name = s.name;
            let fg = s.theme.fg;
            assert!(
                contrast(c.capsule_hover_ink, c.capsule_hover) >= WORD_AA,
                "{name}: the hovered capsule label"
            );
            assert_eq!(
                c.capsule_hover_ink,
                hover_ink(c.value, c.capsule_hover),
                "{name}: the hovered label is the value ink lifted on the risen fill"
            );
            assert!(contrast(c.bar_bg, c.accent) >= 3.0, "{name}: the accent");
            assert_eq!(c.capsule_primary_hover_ink, c.bar_bg, "{name}");
            assert!(
                contrast(c.capsule_primary_hover_ink, c.capsule_primary_hover) >= 3.0,
                "{name}: the lit Primary's ink on its fill"
            );
            if c.accent != fg {
                assert_ne!(
                    c.capsule_primary_hover, c.accent,
                    "{name}: the lit Primary moves off the resting accent"
                );
                let lifted = mix3(c.accent, fg, ink::PRIMARY_HOVER_LIFT);
                for (k, &want) in lifted.iter().enumerate() {
                    let (a, f, lit, want) = (
                        f32::from(c.accent[k]),
                        f32::from(fg[k]),
                        f32::from(c.capsule_primary_hover[k]),
                        f32::from(want),
                    );
                    let toward = (f - a).signum();
                    assert!(
                        (lit - a) * toward + 1.0 >= (want - a) * toward,
                        "{name}: channel {k} of the lit Primary ({lit}) is short of the lift \
                         from the accent ({a}) toward the ink ({f}): wanted {want}"
                    );
                }
            }
            assert_ne!(
                c.capsule_primary_hover, c.capsule_hover,
                "{name}: the lit Primary is not the grey hover"
            );
            if contrast(c.value, mix3(c.bar_bg, fg, 0.05)) >= WORD_AA {
                assert_ne!(
                    c.capsule_hover, c.bar_bg,
                    "{name}: the hover fill moves off the band"
                );
            }
        }
    }
    for (name, palette) in STOCK {
        let c = BandInks::forced(palette);
        assert_eq!(c.meter_track, palette.window, "{name}");
        assert_eq!(
            c.on_accent,
            forced_ink(palette.highlight_text, palette.highlight),
            "{name}"
        );
        for (fill, ink) in [
            (c.capsule_hover, c.capsule_hover_ink),
            (c.capsule_primary_hover, c.capsule_primary_hover_ink),
        ] {
            assert_eq!(fill, palette.highlight, "{name}");
            assert!(
                contrast(ink, fill) >= WORD_AA,
                "{name}: a hovered capsule's ink"
            );
        }
    }
}
