// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The message band's PIXEL raster (design rulings 242–244), measured pixel
//! by pixel on a realistic window: an 80-column row of 16-pixel cells with
//! 8-pixel gutters — the ground's smoothness, the fill's crisp edge through
//! the letters, the busy row's calm inks, the glint that always reads, the
//! Fault's hull, the fade that keeps its words to the midpoint, and the
//! strain row's rail.

use aterm_core::terminal::RenderCell;
use aterm_messages::{
    Anim, BandMotion, COMET_PERIOD, Duration, EchoKind, GLINT_DELAY, GLINT_TRAVEL, Hold, Instant,
    Intent, Links, Load, Look, Message, MessageCenter, MessageLog, Meter, Outcome, Presentation,
    Restatement, Severity, WallStamp, tags,
};
use aterm_render::Theme;

use crate::chrome_band;
use crate::message_band::{
    BandGeometry, RowRaster, cell_width, delta_e, hull_distance, oklch, paint_rows_on, row_inks,
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

fn themes() -> Vec<(&'static str, Theme)> {
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

/// The window pixels column `x`'s GLYPH can draw on: its own cell.
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

/// Every word's contrast on the ground ACTUALLY under its glyph — the pixel
/// raster where the row has one, split at the fill's edge — never less than
/// `floor`; the worst ratio, and where.
fn worst_word_contrast(row: &[RenderCell], raster: Option<&RowRaster>) -> (f64, String) {
    let mut worst = (f64::INFINITY, String::new());
    for (x, cell) in row.iter().enumerate() {
        if cell.ch == ' ' {
            continue;
        }
        // A chip's own fill, or an outlined Primary's inside (ruling 249):
        // its label sits on the cell's own ground, not the meter's.
        let owned = raster.is_some_and(|r| {
            r.own
                .iter()
                .any(|&(a, b)| (usize::from(a)..usize::from(b)).contains(&x))
                || r.rings
                    .iter()
                    .any(|&(a, b, ..)| (usize::from(a)..usize::from(b)).contains(&x))
        });
        // A drawn icon between blank cells spills into them (ruling 258):
        // its ink lands on the ground of all three.
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
                    // The edge's one antialiased pixel is the line itself,
                    // and so is a pastel fill's darker edge (ruling 264).
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

/// THE GROUND IS CONTINUOUS (ruling 242): inside a comet, at pixel
/// resolution, neighbouring pixel columns differ by at most two levels in
/// any channel — no cell blocks, no seams — on every builtin scheme, through
/// a whole crossing.
#[test]
fn a_comet_is_smooth_at_pixel_resolution() {
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
            worst.0 <= 2,
            "{name}: a step of {} levels at {}",
            worst.0,
            worst.1
        );
    }
}

/// A BUSY ROW'S WORDS ARE CALM (ruling 242): every glyph's ink is the same
/// on every frame of a whole comet period — floored once against the
/// hottest tone the comet can reach — on every builtin scheme, and clears AA
/// on the pixels under it on every frame.
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

/// THE GLINT ALWAYS READS (ruling 242): its peak — at full height, or in the
/// rail band where the fill's words leave it no room — is at least ΔE 6
/// (OkLab ×100) from the fill on every builtin scheme; and every word on the
/// bar clears AA on the pixels under it through the glint's whole travel,
/// the fill's edge split through the letters.
#[test]
fn the_glint_reads_and_every_word_clears_aa_through_its_travel() {
    for (name, theme) in themes() {
        let c = chrome_band::band_colors(theme);
        let now = Instant::now();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        center.post(download(570), stamp(), now);
        center.commit_rows(now, 3);
        let p = present(&center);
        let (inks, _) = row_inks(&c, &p.rows[0], false, false);
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

/// THE FILL'S EDGE IS A CRISP LINE THROUGH THE LETTERS (ruling 242): at any
/// fill, the ground is the fill left of the edge and the track right of it
/// with ONE antialiased pixel between; the cell the edge falls in is split
/// at that pixel — the fill side's crisp ink left of it, the track's ink
/// right — and every letter clears AA on the pixels under it.
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
        let (inks, _) = row_inks(&c, &p.rows[0], false, false);
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
            // The glyph's drawn icon spills into the blank cells beside it
            // (ruling 258): an edge in either splits the icon.
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
                // …wearing the track's ink as its own right of the edge.
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

/// Run a Fault echo of `msg` and hand every frame (every 10 ms, 0–600 ms)
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
    for k in 0..=60u64 {
        let (rows, rasters, m) = frame(&c, &p, at + ms(k * 10), Look::MOVING, theme);
        each(k * 10, &p, &rows, &rasters, &m);
    }
}

/// Whether `px` lies in the linear-light solid of four inks, within the
/// triangle test's own tolerance: its barycentric weights over them all at
/// least zero, or — for a pixel byte rounding set just outside, or a flat
/// solid — within 0.01 of one of its faces ([`hull_distance`]).
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

/// A FAILURE NEVER PASSES THROUGH ANOTHER HUE (ruling 244): on every builtin
/// scheme, every ground pixel of every frame of a bar's Fault echo — the
/// cross-fade and the held wash — lies inside the linear-light hull of the
/// row's track, fill and warn: no lime between a green fill and amber, no
/// magenta between blue and ochre. A busy row's Fault puts no warn on its
/// surface at all.
#[test]
fn the_fault_echo_stays_inside_its_hull() {
    for (name, theme) in themes() {
        let c = chrome_band::band_colors(theme);
        fault_frames(theme, download(600), |t, p, _, rasters, m| {
            if m.rows[0].fade > 0 {
                return;
            }
            let (inks, _) = row_inks(&c, &p.rows[0], false, false);
            let r = rasters[0].as_ref().expect("a raster");
            for (x, px) in r.ground.iter().enumerate() {
                // A pastel fill's darker edge (ruling 264) is the fill's own
                // hue deepened: its pixels keep to the solid the edge ink
                // adds to the hull.
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
            let (inks, _) = row_inks(&c, &p.rows[0], false, false);
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

/// `FAILED` KEEPS ITS CONTRAST (ruling 244): through a busy row's Fault —
/// its comet frozen and drained, the row neutral — the ⚠ and `failed` wear
/// warn at AA on the pixels under them on every frame whose opacity is at
/// least one half, on every builtin scheme.
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

/// THE TAIL KEEPS ITS WORDS TO THE MIDPOINT (ruling 244): every echo —
/// Complete, Fault and Vanish, a bar and a busy row — fades as one layer,
/// and on every frame whose opacity is at least one half every word clears
/// AA on the pixels under it and none changes side; on every builtin scheme.
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
                    t += ms(10);
                }
            }
        }
    }
}

/// THE COMPLETE ECHO'S ONE SWEEP (ruling 244): after the fill reaches the
/// window's right edge, a single glint about four cells wide crosses the
/// whole row once, left to right, lifting the full bar by the glint's own
/// step — the same light on dark, light and Solarized grounds.
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
        let (inks, _) = row_inks(&c, &p.rows[0], false, false);
        let fill = aterm_messages::animate::glide_span(700, 1000);
        let mut prev_peak: Option<usize> = None;
        let mut seen = 0;
        // From a display frame past the wipe's landing (the echo is read on
        // the display's own cadence, ruling 245).
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

/// THE STRAIN ROW IS A RAIL (rulings 243 and 248): its words sit on the band
/// in their own inks, which do not move as the level does, and keep clear of
/// the rail; no pixel it paints is the cursor accent; and its rail — the
/// warn hue at a surface's chroma, in the lowest pixels — runs from the
/// window's left edge to the level mapped onto the whole window.
#[test]
fn a_strain_row_is_a_warn_rail_under_calm_words() {
    for (name, theme) in themes() {
        let c = chrome_band::band_colors(theme);
        let now = Instant::now();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        let row = |pm: u16| {
            Message::new(tags::SYSTEM, Severity::Info, "Typing slowed by 'yes'")
                .key(aterm_messages::STRAIN_KEY)
                .hold(Hold::Live {
                    stale_after: aterm_messages::STALE_STRAIN,
                })
                .meter(Meter {
                    load: Some(Load::Cpu),
                    ..Meter::level(pm, "6 of 8 cores")
                })
                .action(Intent::ShowTab { tab: 2, window: 1 })
        };
        let id = center.post(row(300), stamp(), now).id;
        center.commit_rows(now, 3);
        let mut first_inks: Option<Vec<[u8; 3]>> = None;
        for (k, pm) in [300u16, 900, 150, 600].into_iter().enumerate() {
            let t = now + ms(1000 * u64::try_from(k).unwrap());
            if k > 0 {
                center.restate(
                    id,
                    Restatement {
                        meter: Some(row(pm).meter),
                        ..Restatement::default()
                    },
                    t,
                );
            }
            let p = present(&center);
            let landed = t + ms(700);
            let (rows, rasters, _) = frame(&center, &p, landed, Look::MOVING, theme);
            let inks: Vec<[u8; 3]> = rows[0]
                .iter()
                .filter(|cell| cell.ch != ' ')
                .map(|cell| cell.fg)
                .collect();
            match &first_inks {
                None => first_inks = Some(inks),
                Some(f) => assert_eq!(&inks, f, "{name}: an ink moved with the level"),
            }
            // No ground and no glyph in the cursor accent (a word's ink may
            // coincide with it on a theme whose cursor is its foreground).
            if c.accent != c.warn && c.accent != c.bar_bg {
                for (x, cell) in rows[0].iter().enumerate() {
                    assert!(
                        cell.bg != c.accent
                            && (x != aterm_messages::GLYPH_COL || cell.fg != c.accent),
                        "{name}: the cursor accent on a strain row at col {x}: {cell:?}"
                    );
                }
            }
            let r = rasters[0].as_ref().expect("a rail row has a raster");
            assert!(r.ground.is_empty(), "{name}: the words sit on the band");
            assert!(r.rail.iter().flatten().all(|px| *px != c.accent));
            // The words keep clear of the rail (ruling 248): the renderer
            // lifts them and fits the rail under their lowest ink.
            assert!(r.clear_rail, "{name}: the words keep clear of the rail");
            // The rail is the warn HUE at a surface's chroma — never the pale
            // ink tone the glyph wears — and reads on the band.
            let lit_px = r.rail[0].expect("lit at the left edge");
            let ((_, warn_c, warn_h), (_, rail_c, rail_h)) = (oklch(c.warn), oklch(lit_px));
            assert!(
                rail_c + 1e-3 >= warn_c,
                "{name}: the rail {lit_px:?} (chroma {rail_c:.3}) is at least as vivid as warn \
                 {:?} ({warn_c:.3})",
                c.warn
            );
            let dh = (rail_h - warn_h + 540.0).rem_euclid(360.0) - 180.0;
            assert!(
                dh.abs() < 6.0,
                "{name}: the rail keeps warn's hue ({dh:.1}°)"
            );
            assert!(
                chrome_band::contrast(lit_px, c.bar_bg) >= 3.0 - 1e-6,
                "{name}: the rail reads on the band"
            );
            let lit = r.rail.iter().filter(|px| px.is_some()).count();
            let want = usize::from(pm) * geom().win_w / 1000;
            assert!(
                lit.abs_diff(want) <= 1,
                "{name} {pm}: the rail is {lit} px, want {want}"
            );
            assert!(r.rail[0].is_some(), "{name}: from the window's left edge");
        }
    }
}

/// The rows the capture scenes show in their moving look carry a raster;
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
    // The flat look draws no ground and no rail: its raster, where it has
    // one, carries only the row's drawn icon (ruling 251).
    let r = frame(&c, &p, now, flat, theme).1[0].clone();
    assert!(
        r.as_ref()
            .is_none_or(|r| r.ground.is_empty() && r.rail.is_empty() && r.rings.is_empty()),
        "{r:?}"
    );
}

/// Every builtin scheme with its OWN palette (ruling 250): the chrome theme
/// and the scheme's ANSI blue and cyan.
fn palettes() -> Vec<(&'static str, chrome_band::BandPalette)> {
    themes()
        .into_iter()
        .map(|(name, theme)| {
            let s = aterm_types::scheme::builtin(name).expect("a listed scheme");
            (
                name,
                chrome_band::BandPalette {
                    theme,
                    ansi: Some(chrome_band::MeterAnsi::of_scheme(&s)),
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
    palette: chrome_band::BandPalette,
    hover: Option<crate::message_band::BandHover>,
) -> (Vec<Vec<RenderCell>>, Vec<Option<RowRaster>>) {
    let m = c.motion(p, at, look);
    let (rows, _, rasters) = paint_rows_on(p, palette, hover, geom(), &m);
    (rows, rasters)
}

/// OUTLINED ON METERS (ruling 249, the owner: "Outlined on meters"): on a
/// row with a fill or a busy track, the Primary is a ring in the meter's hue
/// with the band's own ground inside and its label in the accent at AA on
/// that ground — no chip cell wears a solid accent, so the bar is the row's
/// only one — and under the pointer its ring lifts and its inside takes a
/// tint, the label still AA. A row with no meter keeps its solid chip. The
/// layout is untouched, so the chip is hit exactly where it was. On every
/// builtin scheme, moving and still.
#[test]
fn a_meter_rows_primary_is_an_accent_ring_and_a_plain_rows_is_solid() {
    // Each with a Secondary beside its Primary (ruling 260: on a meter row
    // the Secondary draws no ground — label only, split at the fill's edge).
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
                // The Secondary: no ground of its own, nothing the raster
                // leaves alone — the meter runs under its label.
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
                // A bar's track is its own tint, never the chip's ground (a
                // comet's still channel is the neutral one, the chip's tone).
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
                // Lit: the ring lifts, the inside tints, the label holds AA.
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
        // No meter: the solid chip, no ring.
        let now = Instant::now();
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        center.post(
            Message::new(tags::UPDATE, Severity::Warn, "aterm v0.93.0 is ready")
                .action(Intent::ApplyUpdate { build: 7 })
                .hold(Hold::Standing),
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

/// THE OUTLINE NEVER READS BROWN (ruling 260): over every builtin scheme the
/// outlined Primary's ring and its label — rest and lit — keep a hue of the
/// theme's accent family, never a warm hue gone dark. Catppuccin Latte's
/// rosewater, floored to AA for its label, is brown; its outline borrows the
/// theme's blue while its fill keeps the rosewater — its own pastel, with a
/// darker edge (ruling 264). Every other scheme's outline is its meter's hue.
#[test]
fn the_outlined_primary_keeps_the_accent_family_on_every_scheme() {
    use aterm_messages::palette::reads_brown;
    let mut seen = Vec::new();
    for (name, palette) in palettes() {
        let c = palette.colors();
        let borrowed = c.ring != c.meter;
        seen.push((name, borrowed));
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
    let borrowers: Vec<&str> = seen.iter().filter(|(_, b)| *b).map(|(n, _)| *n).collect();
    assert_eq!(borrowers, ["Catppuccin Latte"], "{seen:?}");
}

/// BORROW THEME BLUE/CYAN (ruling 250, the owner: "use the theme's own ANSI
/// blue or cyan when the cursor is near-grey, so the bar keeps colour and
/// life; every other theme keeps the cursor-trail colour"). Pinned: which
/// builtin schemes switch and to what — the eight whose cursor is their
/// foreground or a tint of it — and the fill a still bar actually paints.
#[test]
fn a_near_grey_cursor_borrows_blue_or_cyan_and_the_rest_keep_theirs() {
    use aterm_messages::palette::MeterHue;
    let pinned = [
        ("Default", MeterHue::Cursor),
        ("Dracula", MeterHue::Blue),
        ("Nord", MeterHue::Blue),
        ("Tokyo Night", MeterHue::Cursor),
        ("Catppuccin Mocha", MeterHue::Blue),
        ("Gruvbox Dark", MeterHue::Cyan),
        ("Solarized Dark", MeterHue::Cyan),
        ("One Dark", MeterHue::Blue),
        ("Solarized Light", MeterHue::Blue),
        ("Gruvbox Light", MeterHue::Blue),
        ("Catppuccin Latte", MeterHue::Cursor),
        ("GitHub Light", MeterHue::Cursor),
    ];
    let all = palettes();
    assert_eq!(all.len(), pinned.len(), "every builtin scheme is pinned");
    for ((name, palette), (want_name, want)) in all.into_iter().zip(pinned) {
        assert_eq!(name, want_name);
        let base = chrome_band::band_colors(palette.theme);
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
            &[base.field_bg, base.bar_bg, base.value],
        );
        assert_eq!(hue, want, "{name}");
        let c = palette.colors();
        // A pastel fill (ruling 264, Catppuccin Latte only) is the cursor as
        // the theme gives it; every other cursor fill is its accent.
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
        // The boundary stands 3:1: the fill on the band, or a pastel's edge
        // line on its track.
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
        // A bare theme (no palette known) keeps the cursor accent — or its
        // pastel, where the accent was a pastel floored brown.
        assert_eq!(chrome_band::band_colors(palette.theme).meter, cursor_fill);
        // The still bar paints that fill from the window's left edge.
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

/// THE PASTEL FILL (ruling 264 — a default the supervisor took, the owner can
/// overrule it): over every builtin scheme, Catppuccin Latte ALONE keeps its
/// cursor's own pastel rosewater for the progress fill — the 3:1 floor made it
/// a brick bar — and ends it in a darker edge line of the same rose that
/// stands 3:1 from the track, visibly darker than the fill, with the words on
/// the fill at AA. Every other scheme is what it was, byte for byte: its hue
/// floored to 3:1 on the band, its track tinted from that, no edge, and a
/// raster that draws no line.
#[test]
fn only_catppuccin_latte_keeps_a_pastel_fill_with_a_darker_edge() {
    use aterm_messages::palette::{MeterHue, oklch as ok};
    let mut pastels = Vec::new();
    for (name, palette) in palettes() {
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
                // The words on the pastel: the title over a near-whole fill
                // wears the row's crisp ink, at AA on the pastel.
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
    assert_eq!(pastels, ["Catppuccin Latte"]);
}

/// DRAWN ICONS, DROP SPINNER (ruling 251): every glyph a row can carry is
/// drawn as the band's icon through the row's raster — a plain row, a bar, a
/// moving comet (its own glyph, no braille frame), an echo's ✓ and ⚠ — and
/// the cell keeps its character, so the text grid reads what it always read.
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
    // A moving comet keeps the row's own glyph, drawn.
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
    // The echoes' ✓ and ⚠.
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
