// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The message band's pixel raster.

use aterm_core::terminal::RenderCell;
use aterm_messages::{
    Anim, BandMotion, COMET_PERIOD, Duration, EchoKind, GLINT_DELAY, GLINT_TRAVEL, Hold, Instant,
    Intent, Links, Look, Message, MessageCenter, MessageLog, Meter, Outcome, Presentation,
    Severity, WallStamp, tags,
};
use aterm_render::Theme;

use crate::chrome_band;
use crate::message_band::{
    BandGeometry, RowRaster, cell_width, delta_e, hull_distance, paint_rows_on, row_inks,
};

const COLS: usize = 80;
const CELL_W: usize = 16;
const PAD: usize = 8;

fn geom() -> BandGeometry {
    BandGeometry {
        win_w: COLS * CELL_W + 2 * PAD,
        cells_x: PAD,
        cell_w: CELL_W,
    }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn stamp() -> WallStamp {
    WallStamp { unix_ms: 1 }
}

fn present(c: &MessageCenter) -> Presentation {
    c.presentation(COLS, &cell_width, None, Links::Painted)
}

/// Every builtin scheme as a theme, `Default` first.
pub(super) fn themes() -> Vec<(&'static str, Theme)> {
    aterm_types::scheme::builtin_names()
        .into_iter()
        .map(|name| {
            (
                name,
                Theme::from(&aterm_types::scheme::builtin(name).expect("a listed scheme")),
            )
        })
        .collect()
}

/// One frame of the band at `at` in `look`, painted on the window.
fn frame(
    c: &MessageCenter,
    p: &Presentation,
    at: Instant,
    look: Look,
    theme: Theme,
) -> (Vec<Vec<RenderCell>>, Vec<Option<RowRaster>>, BandMotion) {
    let m = c.motion(p, at, look);
    let (rows, _, rasters) = paint_rows_on(p, theme, None, geom(), &m);
    (rows, rasters, m)
}

/// The window pixels column `x`'s glyph can draw on: its own cell.
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
            stale_after: aterm_messages::STALE_UPDATE,
        })
}

fn busy(title: &str) -> Message {
    Message::new(tags::PACKAGES, Severity::Info, title)
        .line("detail")
        .meter(Meter::busy("~3 GB"))
        .hold(Hold::Live {
            stale_after: aterm_messages::STALE_TAILED,
        })
}

/// where.
fn worst_word_contrast(row: &[RenderCell], raster: Option<&RowRaster>) -> (f64, String) {
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
            let ratio = chrome_band::contrast(ink, g);
            if ratio < worst.0 {
                worst = (ratio, format!("col {x} {:?}: {ink:?} on {g:?}", cell.ch));
            }
        }
    }
    worst
}

/// seam, and checked there at that.
#[test]
fn a_comet_is_smooth_at_pixel_resolution() {
    let blend = chrome_band::PLATFORM_BAR_BASE == aterm_messages::ink::BarBase::Blend;
    smooth_on_this_threads_base(if blend { 2 } else { 3 });
    if !blend {
        chrome_band::lay_bands_on_the_theme_blend_for_test();
        smooth_on_this_threads_base(2);
    }
}

/// derives on: no two neighbouring pixel columns more than `levels` apart.
fn smooth_on_this_threads_base(levels: u8) {
    for (name, theme) in themes() {
        let now = Instant::now();
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        c.post(busy("Installing ALab tools"), stamp(), now);
        c.commit_rows(now, 3);
        let p = present(&c);
        let mut worst = (0u8, String::new());
        for k in 0..30u64 {
            let at = now + COMET_PERIOD * u32::try_from(k).unwrap() / 30;
            let (_, rasters, _) = frame(&c, &p, at, Look::MOVING, theme);
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
            worst.0 <= levels,
            "{name}: a step of {} levels at {}",
            worst.0,
            worst.1
        );
    }
}

/// reaches), on every builtin scheme, and clears AA on the pixels under it.
#[test]
fn busy_row_inks_hold_across_a_comet_period() {
    for (name, theme) in themes() {
        let now = Instant::now();
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        c.post(busy("Installing ALab tools"), stamp(), now);
        c.commit_rows(now, 3);
        let p = present(&c);
        let mut first: Option<Vec<[u8; 3]>> = None;
        let mut k = 0u64;
        while k * 33 <= u64::try_from(COMET_PERIOD.as_millis()).unwrap() {
            let at = now + ms(k * 33);
            let (rows, rasters, _) = frame(&c, &p, at, Look::MOVING, theme);
            let inks: Vec<[u8; 3]> = rows[0]
                .iter()
                .enumerate()
                .filter(|(x, cell)| cell.ch != ' ' && *x != aterm_messages::GLYPH_COL)
                .map(|(_, cell)| cell.fg)
                .collect();
            match &first {
                None => first = Some(inks),
                Some(f) => assert_eq!(&inks, f, "{name}: an ink moved at +{} ms", k * 33),
            }
            let (ratio, at_) = worst_word_contrast(&rows[0], rasters[0].as_ref());
            assert!(
                ratio >= 4.5 - 0.02,
                "{name} +{} ms: {ratio:.2}:1 at {at_}",
                k * 33
            );
            k += 1;
        }
    }
}

/// letters.
#[test]
fn the_glint_reads_and_every_word_clears_aa_through_its_travel() {
    for (name, theme) in themes() {
        let c = chrome_band::band_colors(theme);
        let now = Instant::now();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        center.post(download(570), stamp(), now);
        center.commit_rows(now, 3);
        let p = present(&center);
        let (inks, _) = row_inks(&c, &p.rows[0], false);
        let mut peak = 0.0f64;
        let mut t = Duration::ZERO;
        while t <= GLINT_TRAVEL {
            let at = now + GLINT_DELAY + t;
            let (rows, rasters, _) = frame(&center, &p, at, Look::MOVING, theme);
            let r = rasters[0].as_ref().expect("a bar has a raster");
            for px in r.ground.iter().chain(r.rail.iter().flatten()) {
                peak = peak.max(delta_e(*px, inks.fill));
            }
            let (ratio, at_) = worst_word_contrast(&rows[0], Some(r));
            assert!(
                ratio >= 4.5 - 0.02,
                "{name} glint +{t:?}: {ratio:.2}:1 at {at_}"
            );
            t += ms(33);
        }
        assert!(peak >= 6.0, "{name}: the glint's peak is ΔE {peak:.1}");
    }
}

/// ink left, track ink right), and every letter clears AA on its pixels.
#[test]
fn the_fill_edge_is_one_antialiased_pixel_and_splits_its_cell() {
    let theme = Theme::default();
    let c = chrome_band::band_colors(theme);
    for permille in (3..=997u16).step_by(7) {
        let now = Instant::now();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        center.post(download(permille), stamp(), now);
        center.commit_rows(now, 3);
        let p = present(&center);
        let (rows, rasters, m) = frame(&center, &p, now, Look::STILL, theme);
        let r = rasters[0].as_ref().expect("a raster");
        let (inks, _) = row_inks(&c, &p.rows[0], false);
        let edge = m.rows[0].surface.edge.expect("an edge inside the row");
        let win = geom().win_w as u64;
        let e = u64::from(edge) * win / u64::from(aterm_messages::ROW);
        let e = usize::try_from(e).unwrap();
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
        let (ratio, at_) = worst_word_contrast(&rows[0], Some(r));
        assert!(ratio >= 4.5 - 0.02, "{permille}: {ratio:.2}:1 at {at_}");
        if let Some((col, x, ink, _)) = r.split {
            let col = usize::from(col);
            let cell = if col == aterm_messages::GLYPH_COL {
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
                assert_eq!(
                    col,
                    aterm_messages::GLYPH_COL,
                    "{permille}: the icon takes it"
                );
                assert_ne!(
                    rows[0][col].fg, ink,
                    "{permille}: the icon's own ink is the track's"
                );
            }
            assert_eq!(ink, crate::message_band::crisp_on_fill(&c, c.accent, &inks));
            assert!(
                x as usize == e || x as usize == e + 1,
                "{permille}: split at the edge pixel {e}: {x}"
            );
        }
    }
}

/// to `each`.
fn fault_frames(
    theme: Theme,
    msg: Message,
    mut each: impl FnMut(u64, &Presentation, &[Vec<RenderCell>], &[Option<RowRaster>], &BandMotion),
) {
    let now = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    let id = c.post(msg, stamp(), now).id;
    c.commit_rows(now, 3);
    let at = now + ms(1500);
    assert!(c.resolve(id, Outcome::Warn, at));
    let p = present(&c);
    for k in 0..=20u64 {
        let (rows, rasters, m) = frame(&c, &p, at + ms(k * 30), Look::MOVING, theme);
        each(k * 30, &p, &rows, &rasters, &m);
    }
}

/// ([`hull_distance`]).
fn inside_solid(px: [u8; 3], corners: [[u8; 3]; 4]) -> bool {
    let lin = |c: [u8; 3]| {
        c.map(|v| {
            let v = f64::from(v) / 255.0;
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        })
    };
    let [t, f, e, k] = corners;
    let near_a_face = || {
        [[t, f, e], [t, e, k], [t, f, k], [f, e, k]]
            .into_iter()
            .any(|face| hull_distance(px, face) < 0.01)
    };
    let [o, a, b, c] = corners.map(lin);
    let sub = |x: [f64; 3], y: [f64; 3]| [x[0] - y[0], x[1] - y[1], x[2] - y[2]];
    let (u, v, w, q) = (sub(a, o), sub(b, o), sub(c, o), sub(lin(px), o));
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

/// busy row's Fault puts no warn on its surface.
#[test]
fn the_fault_echo_stays_inside_its_hull() {
    for (name, theme) in themes() {
        let c = chrome_band::band_colors(theme);
        fault_frames(theme, download(600), |t, p, _, rasters, m| {
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
        fault_frames(theme, busy("Installing Homebrew"), |t, p, _, rasters, m| {
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

/// every frame at opacity ≥ 1/2, on every builtin scheme.
#[test]
fn failed_keeps_aa_through_a_busy_fault() {
    for (name, theme) in themes() {
        let c = chrome_band::band_colors(theme);
        fault_frames(
            theme,
            busy("Installing Homebrew"),
            |t, p, rows, rasters, m| {
                if m.rows[0].fade > 127 {
                    return;
                }
                let l = &p.rows[0];
                let col = l.elapsed.expect("the elapsed slot");
                let words: String = rows[0][col..col + 6].iter().map(|cell| cell.ch).collect();
                assert_eq!(words, "failed", "{name} +{t} ms");
                let (ratio, at_) = worst_word_contrast(&rows[0], rasters[0].as_ref());
                assert!(ratio >= 4.5 - 0.02, "{name} +{t} ms: {ratio:.2}:1 at {at_}");
                let _ = c.warn;
            },
        );
    }
}

/// builtin scheme.
#[test]
fn an_echo_tail_keeps_its_words_legible_to_the_midpoint() {
    for (name, theme) in themes() {
        for kind in [EchoKind::Complete, EchoKind::Fault, EchoKind::Vanish] {
            for msg in [download(800), busy("Installing Homebrew")] {
                let now = Instant::now();
                let mut c = MessageCenter::new(MessageLog::empty(), now);
                let id = c.post(msg, stamp(), now).id;
                c.commit_rows(now, 3);
                let at = now + ms(1500);
                match kind {
                    EchoKind::Complete => assert!(c.resolve(id, Outcome::Ok, at)),
                    EchoKind::Fault => assert!(c.resolve(id, Outcome::Warn, at)),
                    EchoKind::Vanish => assert!(c.withdraw(id, at)),
                }
                let p = present(&c);
                let until = c.echoes()[0].until;
                let mut t = at;
                while t < until {
                    let (rows, rasters, m) = frame(&c, &p, t, Look::MOVING, theme);
                    if m.rows[0].fade <= 127 {
                        let (ratio, at_) = worst_word_contrast(&rows[0], rasters[0].as_ref());
                        assert!(
                            ratio >= 4.5 - 0.02,
                            "{name} {kind:?} +{:?} fade {}: {ratio:.2}:1 at {at_}",
                            t - at,
                            m.rows[0].fade
                        );
                    }
                    t += aterm_messages::ANIM_FRAME;
                }
            }
        }
    }
}

/// the full bar by the glint's own step, on every builtin scheme.
#[test]
fn the_complete_echo_sweeps_once_across_the_row() {
    for (name, theme) in themes() {
        let c = chrome_band::band_colors(theme);
        let now = Instant::now();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        let id = center.post(download(700), stamp(), now).id;
        center.commit_rows(now, 3);
        let at = now + ms(1500);
        assert!(center.resolve(id, Outcome::Ok, at));
        let p = present(&center);
        let (inks, _) = row_inks(&c, &p.rows[0], false);
        let fill = aterm_messages::animate::glide_span(700, 1000);
        let mut prev_peak: Option<usize> = None;
        let mut seen = 0;
        let mut t = fill + ms(20);
        while t < fill + aterm_messages::ECHO_SWEEP {
            let (_, rasters, m) = frame(&center, &p, at + t, Look::MOVING, theme);
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

/// a Still frame of a bar does too; a flat (High Contrast) one does not.
#[test]
fn only_graded_looks_raster() {
    let now = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    c.post(download(400), stamp(), now);
    c.commit_rows(now, 3);
    let p = present(&c);
    let theme = Theme::default();
    assert!(frame(&c, &p, now, Look::MOVING, theme).1[0].is_some());
    assert!(frame(&c, &p, now, Look::STILL, theme).1[0].is_some());
    let flat = Look {
        pace: aterm_messages::Pace::Moving,
        graded: false,
    };
    let r = frame(&c, &p, now, flat, theme).1[0].clone();
    assert!(
        r.as_ref()
            .is_none_or(|r| r.ground.is_empty() && r.rail.is_empty() && r.rings.is_empty()),
        "{r:?}"
    );
}

/// scheme's ANSI blue and cyan.
fn palettes() -> Vec<(&'static str, chrome_band::ChromePalette)> {
    themes()
        .into_iter()
        .map(|(name, theme)| {
            let s = aterm_types::scheme::builtin(name).expect("a listed scheme");
            (
                name,
                chrome_band::ChromePalette {
                    theme,
                    ansi: Some(chrome_band::ansi_of_scheme(&s)),
                    authored: aterm_messages::tokens::AuthoredChrome::NONE,
                },
            )
        })
        .collect()
}

/// One frame painted from a palette, with an optional hover.
fn frame_in(
    c: &MessageCenter,
    p: &Presentation,
    at: Instant,
    look: Look,
    palette: chrome_band::ChromePalette,
    hover: Option<crate::message_band::BandHover>,
) -> (Vec<Vec<RenderCell>>, Vec<Option<RowRaster>>) {
    let m = c.motion(p, at, look);
    let (rows, _, rasters) = paint_rows_on(p, palette, hover, geom(), &m);
    (rows, rasters)
}

/// area are unchanged. On every builtin scheme, moving and still.
#[test]
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
    for (name, palette) in palettes() {
        let c = palette.colors();
        for msg in &rows_with_meter {
            let now = Instant::now();
            let mut center = MessageCenter::new(MessageLog::empty(), now);
            center.post(msg.clone(), stamp(), now);
            center.commit_rows(now, 3);
            let p = present(&center);
            let cap = p.rows[0]
                .capsules
                .iter()
                .find(|k| k.role == aterm_messages::CapsuleRole::Primary)
                .expect("a Primary")
                .clone();
            let span = (
                u16::try_from(cap.col).unwrap(),
                u16::try_from(cap.col + cap.width).unwrap(),
            );
            for look in [Look::MOVING, Look::STILL] {
                let at = now + ms(1500);
                let (rows, rasters) = frame_in(&center, &p, at, look, palette, None);
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
                    .find(|k| k.role == aterm_messages::CapsuleRole::Secondary)
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
                assert!(
                    chrome_band::contrast(ring, inner) >= 3.0,
                    "{name}: the ring reads"
                );
                for (x, cell) in rows[0].iter().enumerate().skip(cap.col).take(cap.width) {
                    assert_eq!(cell.bg, inner, "{name} col {x}: the ring's inside");
                    assert_ne!(cell.bg, c.meter, "{name} col {x}: no solid accent");
                    if cell.ch != ' ' {
                        let ratio = chrome_band::contrast(cell.fg, inner);
                        assert!(ratio >= 4.5, "{name} col {x}: label {ratio:.2}:1");
                        assert!(cell.bold, "{name}: the Primary's label stays bold");
                    }
                }
                let hover = crate::message_band::BandHover {
                    row: 0,
                    target: crate::message_band::HoverTarget::Capsule(cap.action),
                };
                let (lit_rows, lit) = frame_in(&center, &p, at, look, palette, Some(hover));
                let (_, _, lring, linner) = lit[0].as_ref().unwrap().rings[0];
                assert_ne!(lring, ring, "{name}: the lit ring lifts");
                assert_ne!(linner, inner, "{name}: the lit inside tints");
                assert!(
                    chrome_band::contrast(lring, linner) >= 3.0,
                    "{name}: lit ring"
                );
                for (x, cell) in lit_rows[0]
                    .iter()
                    .enumerate()
                    .skip(cap.col + 1)
                    .take(cap.width - 2)
                {
                    assert_eq!(cell.bg, linner, "{name}: the lit inside");
                    if cell.ch != ' ' {
                        let ratio = chrome_band::contrast(cell.fg, linner);
                        assert!(ratio >= 4.5, "{name} lit col {x}: {ratio:.2}:1");
                    }
                }
            }
        }
        let now = Instant::now();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        center.post(
            Message::new(tags::UPDATE, Severity::Warn, "aterm v0.93.0 is ready")
                .action(Intent::ApplyUpdate { build: 7 })
                .hold(Hold::STANDING),
            stamp(),
            now,
        );
        center.commit_rows(now, 3);
        let p = present(&center);
        let cap = p.rows[0].capsules[0].clone();
        assert_eq!(cap.role, aterm_messages::CapsuleRole::Primary);
        let (rows, rasters) = frame_in(&center, &p, now, Look::STILL, palette, None);
        assert!(
            rasters[0].as_ref().is_none_or(|r| r.rings.is_empty()),
            "{name}: no ring without a meter"
        );
        for (x, cell) in rows[0].iter().enumerate().skip(cap.col).take(cap.width) {
            assert_ne!(cell.bg, c.bar_bg, "{name} col {x}: a solid chip");
        }
    }
}

/// outline is its meter's hue.
#[test]
fn the_outlined_primary_keeps_the_accent_family_on_every_scheme() {
    use aterm_messages::palette::reads_brown;
    for (name, palette) in palettes() {
        let c = palette.colors();
        let borrowed = c.ring != c.meter;
        for lit in [false, true] {
            let o = crate::message_band::outlined_inks(&c, lit);
            assert!(
                !reads_brown(o.ring) && !reads_brown(o.label),
                "{name} lit {lit}: ring {:?} {:?}, label {:?} {:?}",
                o.ring,
                aterm_messages::palette::oklch(o.ring),
                o.label,
                aterm_messages::palette::oklch(o.label)
            );
            assert!(chrome_band::contrast(o.label, o.inner) >= 4.5, "{name}");
            assert!(chrome_band::contrast(o.ring, o.inner) >= 3.0, "{name}");
        }
        if borrowed {
            let blue = palette.ansi.unwrap().blue;
            assert_eq!(
                c.ring,
                chrome_band::ensure_contrast(blue, c.bar_bg, 3.0),
                "{name}: the theme's blue"
            );
        }
    }
}

/// has colour, its boundary stands 3:1, and a still bar paints that fill.
#[test]
fn a_near_grey_cursor_borrows_blue_or_cyan_and_the_rest_keep_theirs() {
    use aterm_messages::palette::MeterHue;
    for (name, palette) in palettes() {
        let base = chrome_band::band_colors(palette.theme);
        let ansi = palette.ansi.unwrap();
        let cursor = [
            ((palette.theme.cursor >> 16) & 0xff) as u8,
            ((palette.theme.cursor >> 8) & 0xff) as u8,
            (palette.theme.cursor & 0xff) as u8,
        ];
        let want = MeterHue::pick(
            cursor,
            ansi.blue,
            ansi.cyan,
            &[base.field_bg, base.bar_bg, base.value],
        );
        let c = palette.colors();
        let cursor_fill = if c.meter_edge.is_some() {
            cursor
        } else {
            base.accent
        };
        if want == MeterHue::Cursor {
            assert_eq!(c.meter, cursor_fill, "{name}: the cursor-trail colour");
        } else {
            assert!(
                aterm_messages::palette::chroma(c.meter) >= aterm_messages::palette::BORROW_CHROMA,
                "{name}: the borrowed fill has colour: {:?}",
                c.meter
            );
        }
        match c.meter_edge {
            Some(edge) => assert!(
                chrome_band::contrast(edge, c.meter_track) >= 3.0,
                "{name}: the edge 3:1 on the track"
            ),
            None => assert!(
                chrome_band::contrast(c.meter, c.bar_bg) >= 3.0,
                "{name}: 3:1 on the band"
            ),
        }
        assert_eq!(chrome_band::band_colors(palette.theme).meter, cursor_fill);
        let now = Instant::now();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        center.post(download(500), stamp(), now);
        center.commit_rows(now, 3);
        let p = present(&center);
        let (_, rasters) = frame_in(&center, &p, now, Look::STILL, palette, None);
        assert_eq!(
            rasters[0].as_ref().unwrap().ground[0],
            c.meter,
            "{name}: the fill"
        );
    }
}

/// line in the raster.
#[test]
fn only_a_pastel_cursor_keeps_a_pastel_fill_with_a_darker_edge() {
    use aterm_messages::palette::{MeterHue, oklch as ok};
    const UPSTREAM_LATTE: &str = "Catppuccin Latte, upstream rosewater cursor";
    let mut subjects = palettes();
    let latte = subjects
        .iter()
        .find(|(name, _)| *name == "Catppuccin Latte")
        .map(|(_, palette)| *palette)
        .expect("Catppuccin Latte is a builtin");
    subjects.push((
        UPSTREAM_LATTE,
        chrome_band::ChromePalette {
            theme: aterm_render::Theme {
                cursor: 0x00DC_8A78,
                ..latte.theme
            },
            ..latte
        },
    ));
    let mut pastels = Vec::new();
    for (name, palette) in subjects {
        let c = palette.colors();
        let ansi = palette.ansi.unwrap();
        let cursor = [
            ((palette.theme.cursor >> 16) & 0xff) as u8,
            ((palette.theme.cursor >> 8) & 0xff) as u8,
            (palette.theme.cursor & 0xff) as u8,
        ];
        let hue = MeterHue::pick(
            cursor,
            ansi.blue,
            ansi.cyan,
            &[c.field_bg, c.bar_bg, c.value],
        )
        .of(cursor, ansi.blue, ansi.cyan);
        let floored = chrome_band::ensure_contrast(hue, c.bar_bg, 3.0);
        let now = Instant::now();
        for permille in [60u16, 420, 777, 993] {
            let mut center = MessageCenter::new(MessageLog::empty(), now);
            center.post(download(permille), stamp(), now);
            center.commit_rows(now, 3);
            let p = present(&center);
            let (rows, rasters) = frame_in(&center, &p, now, Look::STILL, palette, None);
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
            let (ratio, at_) = worst_word_contrast(&rows[0], Some(r));
            assert!(
                ratio >= 4.5 - 0.02,
                "{name} {permille}: {ratio:.2}:1 at {at_}"
            );
        }
        match c.meter_edge {
            None => {
                assert_eq!(c.meter, floored, "{name}: the floored hue, as before");
                assert_eq!(
                    c.meter_track,
                    chrome_band::mix3(c.bar_bg, floored, chrome_band::TRACK_TINT),
                    "{name}: the track, as before"
                );
            }
            Some(edge) => {
                pastels.push(name);
                assert_eq!(c.meter, hue, "{name}: the theme's own pastel");
                assert!(
                    aterm_messages::palette::reads_brown(floored),
                    "{name}: the floor it replaces read brown: {floored:?}"
                );
                assert_eq!(
                    c.meter_track,
                    chrome_band::mix3(c.bar_bg, hue, chrome_band::TRACK_TINT)
                );
                assert!(
                    chrome_band::contrast(edge, c.meter_track) >= 3.0,
                    "{name}: the edge stands 3:1 from the track"
                );
                assert!(
                    chrome_band::contrast(edge, c.meter) >= 1.4,
                    "{name}: the edge reads darker than the fill: {edge:?} on {:?}",
                    c.meter
                );
                assert!(
                    (ok(edge).2 - ok(hue).2).abs() < 3.0,
                    "{name}: the same rose, deepened: {edge:?}"
                );
                let now = Instant::now();
                let mut center = MessageCenter::new(MessageLog::empty(), now);
                center.post(download(993), stamp(), now);
                center.commit_rows(now, 3);
                let p = present(&center);
                let (rows, _) = frame_in(&center, &p, now, Look::STILL, palette, None);
                let (tcol, _) = &p.rows[0].title;
                let ink = rows[0][*tcol].fg;
                assert!(
                    chrome_band::contrast(ink, c.meter) >= 4.5 - 0.02,
                    "{name}: the title's {ink:?} on the pastel"
                );
            }
        }
    }
    assert_eq!(pastels, [UPSTREAM_LATTE]);
}

/// text grid.
#[test]
fn every_rows_glyph_is_a_drawn_icon_and_its_character_stays() {
    let theme = Theme::default();
    let col = aterm_messages::GLYPH_COL;
    let icon_of = |r: &Option<RowRaster>| -> Vec<(u16, aterm_render::BandIcon)> {
        r.as_ref().map_or_else(Vec::new, |r| {
            r.icons
                .iter()
                .map(|&(c, i)| (c, crate::message_band::band_icon(i)))
                .collect()
        })
    };
    for &ch in aterm_messages::Glyph::ALLOWED {
        let now = Instant::now();
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        c.post(
            Message::new(tags::SYSTEM, Severity::Info, "glyph")
                .glyph(aterm_messages::Glyph::new(ch).unwrap()),
            stamp(),
            now,
        );
        c.commit_rows(now, 3);
        let p = present(&c);
        for look in [Look::MOVING, Look::STILL] {
            let (rows, rasters, _) = frame(&c, &p, now, look, theme);
            assert_eq!(rows[0][col].ch, ch, "{ch:?}: the character stays");
            let icon = aterm_render::BandIcon::for_char(ch).expect("every glyph has an icon");
            assert_eq!(aterm_render::BandIcon::for_char(icon.ch()), Some(icon));
            assert_eq!(
                icon_of(&rasters[0]),
                vec![(u16::try_from(col).unwrap(), icon)],
                "{ch:?}"
            );
        }
    }
    let now = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    c.post(busy("Installing Homebrew"), stamp(), now);
    c.commit_rows(now, 3);
    let p = present(&c);
    let own = p.rows[0].glyph.1;
    for k in 0..40u64 {
        let (rows, rasters, m) = frame(&c, &p, now + ms(33 * k), Look::MOVING, theme);
        assert!(matches!(m.rows[0].anim, Anim::Comet { .. }));
        assert_eq!(rows[0][col].ch, own, "frame {k}: no spinner");
        assert_eq!(
            icon_of(&rasters[0]),
            vec![(
                u16::try_from(col).unwrap(),
                aterm_render::BandIcon::for_char(own).unwrap()
            )],
            "frame {k}"
        );
    }
    for (outcome, want) in [
        (Outcome::Ok, aterm_render::BandIcon::Success),
        (Outcome::Warn, aterm_render::BandIcon::Warn),
    ] {
        let now = Instant::now();
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        let id = c.post(download(700), stamp(), now).id;
        c.commit_rows(now, 3);
        assert!(c.resolve(id, outcome, now + ms(1500)));
        let p = present(&c);
        let (_, rasters, m) = frame(&c, &p, now + ms(1600), Look::MOVING, theme);
        assert!(matches!(m.rows[0].anim, Anim::Echo { .. }), "{outcome:?}");
        assert_eq!(
            icon_of(&rasters[0]),
            vec![(u16::try_from(col).unwrap(), want)],
            "{outcome:?}"
        );
    }
}
