// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::animate::{Look, Pace};
use crate::center::{MessageCenter, Outcome};
use crate::glass::Links;
use crate::log::MessageLog;
use crate::model::{Hold, Intent, Message, Meter, WallStamp, tags};
use crate::text::char_width;
use crate::{Duration, Instant};

const FLAT: Look = Look {
    pace: Pace::Moving,
    graded: false,
};

fn stamp() -> WallStamp {
    WallStamp { unix_ms: 1 }
}

fn bar(pm: u16) -> Message {
    Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.91.0")
        .meter(Meter {
            fill_permille: Some(pm),
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

fn with_capsules(m: Message) -> Message {
    m.action(Intent::ApplyUpdate { build: 7 })
        .action(Intent::OpenSettings {
            route: "/packages".into(),
        })
}

fn center(msgs: Vec<Message>, now: Instant) -> MessageCenter {
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    for mut m in msgs {
        m.reveal_after = None;
        c.post(m, stamp(), now);
    }
    c.commit_rows(now, 3);
    c
}

fn present(c: &MessageCenter, cols: usize) -> Presentation {
    c.presentation(cols, &char_width, Links::Painted)
}

fn window(cols: usize) -> Geometry {
    Geometry {
        win_w: cols * 16 + 16,
        cells_x: 8,
        cell_w: 16,
    }
}

/// Paint `msgs` at `cols` on a real window at `now + 2 s` in `look`.
fn painted(
    msgs: Vec<Message>,
    cols: usize,
    look: Look,
    hover: Option<Hover>,
    forced: bool,
    each: impl FnOnce(&Presentation, &[RowPaint<'_>]),
) {
    let now = Instant::now();
    let c = center(msgs, now);
    let p = present(&c, cols);
    let m = c.motion(&p, now + Duration::from_secs(2), look);
    let rows = paint(&p, hover, window(cols), &m, forced);
    each(&p, &rows);
}

fn text(r: &RowPaint<'_>) -> String {
    r.cells.iter().map(|c| c.ch).collect()
}

fn fixtures() -> Vec<Vec<Message>> {
    vec![
        vec![with_capsules(bar(420))],
        vec![bar(0)],
        vec![bar(1000)],
        vec![busy("Installing Homebrew").action(Intent::NewWindow)],
        vec![Message::new(tags::CONFIG, Severity::Warn, "Misspelled setting").line("x")],
        vec![
            with_capsules(bar(333)),
            busy("Installing Homebrew"),
            Message::new(tags::CONFIG, Severity::Error, "Tests failed on main"),
            Message::new(tags::CONFIG, Severity::Info, "One more"),
        ],
    ]
}

/// Every painted row is exactly `cols` cells, and carries the layout's
/// words where the layout put them.
#[test]
fn every_row_is_exactly_cols_wide_and_carries_its_title() {
    for msgs in fixtures() {
        for cols in [60usize, 80, 120, 160] {
            for look in [Look::MOVING, Look::STILL, FLAT] {
                for forced in [false, true] {
                    painted(msgs.clone(), cols, look, None, forced, |p, rows| {
                        assert_eq!(rows.len(), p.rows.len());
                        for (r, l) in rows.iter().zip(&p.rows) {
                            assert_eq!(r.cells.len(), cols);
                            let t = text(r);
                            let (tcol, title) = &l.title;
                            let at: String =
                                t.chars().skip(*tcol).take(title.chars().count()).collect();
                            assert_eq!(&at, title, "{t:?}");
                            if let Some(cols) = &r.columns {
                                assert_eq!(cols.len(), r.cells.len());
                            }
                        }
                    });
                }
            }
        }
    }
}

/// A time slot's blank cells are cleared to its ink, and a later write
/// resets the glyph cell's text presentation.
#[test]
fn a_slot_is_cleared_to_its_width_in_its_ink() {
    let mut cells = Cells {
        cells: vec![Cell::BLANK; 20],
    };
    cells.cells[5].ch = 'x';
    cells.slot((3, 6), STALLED_WORD);
    let t: String = cells.cells.iter().map(|c| c.ch).collect();
    assert_eq!(&t[3..9], "stalle");
    cells.slot((3, 10), "35 s left");
    assert!(
        cells.cells[3..13]
            .iter()
            .all(|c| c.face == Face::Word(Ink::Value))
    );
    assert_eq!(cells.cells[12].ch, ' ');
    cells.slot((10, 4), "for 3m");
    assert!(
        cells.cells[10..14]
            .iter()
            .all(|c| c.face == Face::Word(Ink::Label))
    );
    cells.cells[0].text_presentation = true;
    cells.write(0, "a", Face::Word(Ink::Value), true);
    assert!(!cells.cells[0].text_presentation && cells.cells[0].bold);
    cells.slot((18, 6), "abcdef");
    cells.write(19, "xyz", Face::Blank, false);
    assert_eq!(cells.cells.len(), 20);
}

/// The glyph cell is text presentation with its drawn icon.
#[test]
fn the_glyph_cell_is_text_presentation_with_its_icon() {
    painted(vec![bar(420)], 80, Look::MOVING, None, false, |p, rows| {
        let (gcol, ch) = p.rows[0].glyph;
        let cell = rows[0].cells[gcol];
        assert!(cell.text_presentation && cell.bold);
        assert_eq!(cell.ch, ch);
        assert_eq!(
            rows[0].icon,
            Some((gcol as u16, Icon::for_char(ch).unwrap()))
        );
        assert_eq!(
            rows[0].cells.iter().filter(|c| c.text_presentation).count(),
            1
        );
    });
}

/// Forced: brackets, riding chips on a meter, no ring, no outline.
#[test]
fn forced_chips_are_brackets_and_never_rings() {
    for msgs in [
        vec![with_capsules(bar(420))],
        vec![with_capsules(Message::new(
            tags::CONFIG,
            Severity::Info,
            "A",
        ))],
    ] {
        for look in [Look::MOVING, FLAT] {
            painted(msgs.clone(), 80, look, None, true, |p, rows| {
                let r = &rows[0];
                assert!(r.rings.is_empty() && r.owned.is_empty(), "{r:?}");
                for cap in &p.rows[0].capsules {
                    assert_eq!(r.cells[cap.col].ch, '[');
                    assert_eq!(r.cells[cap.col + cap.width - 1].ch, ']');
                    if r.columns.is_none() {
                        assert!(matches!(
                            r.cells[cap.col].face,
                            Face::Chip(ChipFace {
                                form: ChipForm::Forced,
                                ..
                            })
                        ));
                    } else {
                        assert!(matches!(r.cells[cap.col].face, Face::Word(_)));
                    }
                }
            });
        }
    }
}

/// On a meter a resting Secondary rides, and a Primary wears the outlined
/// form — ringed only where the row is drawn to the pixel, else owning
/// its cells.
#[test]
fn a_metered_secondary_rides_and_a_primary_rings_only_where_drawn() {
    for (look, rings) in [(Look::MOVING, true), (Look::STILL, true), (FLAT, false)] {
        painted(
            vec![with_capsules(bar(420))],
            80,
            look,
            None,
            false,
            |p, rows| {
                let r = &rows[0];
                let caps = &p.rows[0].capsules;
                let primary = caps
                    .iter()
                    .find(|c| c.role == CapsuleRole::Primary)
                    .unwrap();
                let secondary = caps
                    .iter()
                    .find(|c| c.role == CapsuleRole::Secondary)
                    .unwrap();
                assert_eq!(
                    r.cells[secondary.col + 1].face,
                    Face::Word(Ink::Value),
                    "the Secondary rides"
                );
                assert_eq!(r.rings.len(), usize::from(rings), "{look:?}");
                assert_eq!(r.owned.len(), usize::from(!rings), "{look:?}");
                assert!(matches!(
                    r.cells[primary.col + 1].face,
                    Face::Chip(ChipFace {
                        form: ChipForm::Outlined,
                        metered: true,
                        ..
                    })
                ));
            },
        );
    }
    let hover = Some(Hover {
        row: 0,
        target: HoverTarget::Capsule(ActionIndex(1)),
    });
    painted(
        vec![with_capsules(bar(420))],
        80,
        Look::MOVING,
        hover,
        false,
        |p, rows| {
            let sec = p.rows[0]
                .capsules
                .iter()
                .find(|c| c.role == CapsuleRole::Secondary)
                .unwrap();
            assert!(matches!(
                rows[0].cells[sec.col + 1].face,
                Face::Chip(ChipFace { lit: true, .. })
            ));
        },
    );
}

/// The split and the icon split are one cell, absent over a chip's own
/// cells, and only on a bar drawn to the pixel.
#[test]
fn the_split_is_one_cell_never_under_a_chip() {
    let now = Instant::now();
    let mut seen = 0usize;
    for pm in (0..=1000u16).step_by(7) {
        let c = center(vec![with_capsules(bar(pm))], now);
        for cols in [60usize, 80, 120] {
            let p = present(&c, cols);
            for look in [Look::MOVING, Look::STILL, FLAT] {
                let m = c.motion(&p, now + Duration::from_secs(2), look);
                for g in [Geometry::cells_only(cols), window(cols)] {
                    let rows = paint(&p, None, g, &m, false);
                    let r = &rows[0];
                    let Some(s) = r.split else {
                        continue;
                    };
                    seen += 1;
                    assert!(r.raster_ground && r.edge_px.is_some());
                    assert!(matches!(r.kind, RowSurface::Bar { .. }));
                    let col = usize::from(s.col);
                    let within =
                        |&(a, b): &(u16, u16)| (usize::from(a)..usize::from(b)).contains(&col);
                    assert!(!r.owned.iter().any(within));
                    assert!(!r.rings.iter().any(|&(a, b, _)| within(&(a, b))));
                    let (x0, x1) = r.span.px(col);
                    let e = r.edge_px.unwrap();
                    let gcol = p.rows[0].glyph.0;
                    assert!(
                        (x0 < e && e < x1) || col == gcol,
                        "{pm} {cols}: the split is the edge's cell or the icon's"
                    );
                }
            }
        }
    }
    assert!(seen > 0, "no paint in the sweep carried a split");
}

/// A plain row has no columns; a busy row's columns are never on the
/// fill's side off a forced palette.
#[test]
fn columns_are_absent_on_plain_rows_and_busy_never_rides_the_fill() {
    painted(
        vec![Message::new(tags::CONFIG, Severity::Warn, "W")],
        80,
        Look::MOVING,
        None,
        false,
        |_, rows| {
            assert!(rows[0].columns.is_none());
            assert_eq!(rows[0].kind, RowSurface::Plain);
        },
    );
    let now = Instant::now();
    let c = center(vec![busy("Installing Homebrew")], now);
    let p = present(&c, 80);
    for k in 0..90u32 {
        let m = c.motion(&p, now + crate::ANIM_FRAME * k, Look::MOVING);
        let rows = paint(&p, None, window(80), &m, false);
        let cols = rows[0].columns.as_ref().expect("a comet's track");
        assert!(cols.iter().all(|c| !c.on_fill));
        assert!(matches!(rows[0].kind, RowSurface::Busy { still: false }));
        let forced = paint(&p, None, window(80), &m, true);
        assert!(matches!(forced[0].kind, RowSurface::Bar { .. }));
    }
    let m = c.motion(&p, now, Look::STILL);
    let rows = paint(&p, None, window(80), &m, false);
    assert!(matches!(rows[0].kind, RowSurface::Busy { still: true }));
}

/// The pastel edge line lies inside the fill's last pixels, each share in
/// 0..=1, ending at the edge.
#[test]
fn the_edge_line_ends_at_the_edge() {
    let now = Instant::now();
    for pm in [60u16, 420, 777, 993] {
        let c = center(vec![bar(pm)], now);
        let p = present(&c, 80);
        let m = c.motion(&p, now, Look::STILL);
        let rows = paint(&p, None, window(80), &m, false);
        let r = &rows[0];
        let line = r.edge_line().expect("a bar ending inside the row");
        let e = r.edge_px.unwrap();
        assert!(u64::from(line.end) >= e && u64::from(line.first) <= e);
        assert!(!line.coverage.is_empty());
        for &(px, k) in &line.coverage {
            assert!((0.0..=1.0).contains(&k), "{k}");
            assert!(px >= line.first as usize && px < line.end as usize);
        }
        assert!(
            paint(&p, None, window(80), &m, true)[0]
                .edge_line()
                .is_none()
        );
    }
}

/// The edge line is an eighth of the cell rounded half up, and at least one
/// pixel, for every cell width up to `usize::MAX`.
#[test]
fn the_edge_line_is_an_eighth_of_any_cell() {
    for cell_w in 0..=4096usize {
        assert_eq!(edge_line_px(cell_w), ((cell_w + 4) / 8).max(1), "{cell_w}");
    }
    // The widest cells, where `cell_w + 4` passes `usize::MAX`: 8k + 7 and 8k + 4 round up.
    assert_eq!(edge_line_px(usize::MAX), usize::MAX / 8 + 1);
    assert_eq!(edge_line_px(usize::MAX - 3), usize::MAX / 8 + 1);
    assert_eq!(edge_line_px(usize::MAX - 4), usize::MAX / 8);
}

/// An edge whose position in 1/ROW pixels passes `u64` draws no line.
#[test]
fn an_edge_past_u64_draws_no_line() {
    let now = Instant::now();
    let c = center(vec![bar(420)], now);
    let p = present(&c, 80);
    let m = c.motion(&p, now, Look::STILL);
    let mut r = paint(&p, None, window(80), &m, false).swap_remove(0);
    assert!(r.edge_line().is_some());
    r.span.geom.win_w = usize::MAX;
    assert!(r.edge_line().is_none());
}

/// Every glyph a message may carry has a drawn icon: the glyph cell never
/// paints a font fallback.
#[test]
fn every_band_glyph_has_a_drawn_icon() {
    for &ch in crate::model::Glyph::ALLOWED {
        assert!(Icon::for_char(ch).is_some(), "{ch:?}");
    }
}

/// An echo's fade is handed through, and a Fault echo's glyph wears warn.
#[test]
fn a_fault_echo_fades_under_a_warn_glyph() {
    let now = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    let id = c.post(bar(500), stamp(), now).id;
    c.commit_rows(now, 3);
    let at = now + Duration::from_millis(1500);
    assert!(c.resolve(id, Outcome::Warn, at));
    let p = present(&c, 80);
    let mut faded = false;
    for k in 0..20u64 {
        let m = c.motion(&p, at + Duration::from_millis(k * 40), Look::MOVING);
        let rows = paint(&p, None, window(80), &m, false);
        let g = p.rows[0].glyph.0;
        assert_eq!(rows[0].cells[g].face, Face::Word(Ink::Warn));
        faded |= rows[0].fade > 0;
    }
    assert!(faded);
}

/// Each column's lit coverage of a bar at `fill` on `g`: the fill channel
/// of its window-pixel span's mean tone (0 track … 255 full).
fn coverage(fill: u16, cols: usize, g: Geometry) -> Vec<u8> {
    coverage_in(fill, cols, g, true)
}

/// [`coverage`] in the flat look (High Contrast): whole inks only.
fn coverage_flat(fill: u16, cols: usize, g: Geometry) -> Vec<u8> {
    coverage_in(fill, cols, g, false)
}

fn coverage_in(fill: u16, cols: usize, g: Geometry, graded: bool) -> Vec<u8> {
    let surface = crate::animate::bar(fill, None, graded);
    let span = MeterSpan { geom: g, cols };
    (0..cols).map(|x| span.tone(&surface, x).fill).collect()
}

/// In the flat look (High Contrast) every cell is whole, the fill or the track, and
/// the end columns carry the gutters: any started fill lights the first column, only
/// 1000 ‰ the last, and the lit columns are one run from the left.
#[test]
fn the_flat_meter_keeps_the_honest_ends() {
    for (g, cols) in [
        (
            Geometry {
                win_w: 1296,
                cells_x: 8,
                cell_w: 16,
            },
            80,
        ),
        (
            Geometry {
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

/// The meter maps the fill onto the window edge to edge, in whole cells smoothed by
/// tone coverage.
#[test]
fn the_meter_maps_the_fill_onto_the_window_edge_to_edge() {
    let g = Geometry {
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
    for cols in 1..4 {
        for fill in [0u16, 1, 499, 500, 999, 1000] {
            assert_eq!(coverage(fill, cols, g).len(), cols);
        }
    }
    let wide_gutters = Geometry {
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

/// Mapped onto real windows, the comet lights the first column (the left gutter's) as
/// it enters and the last as it leaves; wholly inside, it is about a quarter of the
/// window wide ([`crate::COMET_PERMILLE`]); and no two neighbouring cells step from full
/// to empty.
#[test]
fn the_comet_sweeps_the_window_edge_to_edge() {
    for (win_w, cols, cells_x, cell_w) in
        [(2000usize, 114usize, 31usize, 17usize), (1000, 80, 20, 12)]
    {
        sweep_edge_to_edge(
            Geometry {
                win_w,
                cells_x,
                cell_w,
            },
            cols,
        );
    }
}

/// [`the_comet_sweeps_the_window_edge_to_edge`] on one window.
fn sweep_edge_to_edge(g: Geometry, cols: usize) {
    let span = MeterSpan { geom: g, cols };
    let (mut first, mut last) = (false, false);
    let period = crate::COMET_PERIOD;
    let steps = period.as_millis() / crate::ANIM_FRAME.as_millis();
    for k in 0..u32::try_from(steps).unwrap() {
        let since = crate::ANIM_FRAME * k;
        let surface = crate::animate::comet(since, true);
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
            let quarter = cols as f64 * f64::from(crate::COMET_PERMILLE) / 1000.0;
            assert!(
                (lit as f64) >= quarter * 0.8 && (lit as f64) <= quarter * 1.4,
                "frame {k}: {lit} cells lit, a quarter of the window is {quarter:.1}"
            );
        }
    }
    assert!(first && last, "it enters and leaves through the gutters");
}

/// On whole-cell tones the first column (the left gutter's) takes the entering head's
/// coverage, rising frame by frame rather than switching on, and never lights before
/// the head has entered.
#[test]
fn the_comet_enters_without_a_straight_edge() {
    for (win_w, cols, cells_x, cell_w) in
        [(2000usize, 114usize, 31usize, 17usize), (1000, 80, 20, 12)]
    {
        let g = Geometry {
            win_w,
            cells_x,
            cell_w,
        };
        let span = MeterSpan { geom: g, cols };
        for base in [0u32, 1, 2] {
            let mut seen = Vec::new();
            for k in 0..24u32 {
                let since = crate::COMET_PERIOD * base + crate::ANIM_FRAME * k;
                let surface = crate::animate::comet(since, true);
                seen.push(span.tone(&surface, 0).fill);
            }
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

/// The column under pixel `x`: the leading gutter is column 0, each cell's pixels map
/// to it, and a zero cell width never divides by zero.
#[test]
fn a_pixel_maps_to_the_cell_it_lies_in() {
    let g = Geometry {
        win_w: 8 * 10 + 2 * 4,
        cells_x: 4,
        cell_w: 8,
    };
    for x in 0..4 {
        assert_eq!(g.cell_at(x), 0, "px {x}: the gutter is column 0");
    }
    for col in 0..10 {
        for dx in 0..8 {
            assert_eq!(g.cell_at(4 + col * 8 + dx), col, "col {col} +{dx}");
        }
    }
    assert_eq!(
        g.cell_at(4 + 10 * 8),
        10,
        "past the last column: not clamped"
    );
    let zero = Geometry {
        win_w: 0,
        cells_x: 4,
        cell_w: 0,
    };
    assert_eq!(zero.cell_at(3), 0);
    assert_eq!(zero.cell_at(9), 5, "a zero cell is one pixel wide");
    assert_eq!(Geometry::cells_only(80).cell_at(79), 79);
}
