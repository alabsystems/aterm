// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::stats_units::{Gpu, Heat};

const TEXT: HeaderText<'static> = HeaderText {
    tab: "Tab 2",
    machine: "this Mac",
    over: [true; 2],
    totals: &[],
};

fn hn() -> FaceMetrics {
    FaceMetrics::helvetica_neue()
}

/// Totals forms that measure as they read: figures that never change.
fn said_totals(forms: &[&str]) -> Vec<TotalsForm> {
    forms
        .iter()
        .map(|&f| TotalsForm {
            said: f.to_owned(),
            widest: f.to_owned(),
            used: false,
        })
        .collect()
}

fn px(v: f64, s: f64) -> f64 {
    (v * s + 0.5).floor()
}

fn strip_col(strip: Strip) -> Col {
    Col::new(strip.x, strip.end() - strip.x)
}

fn full_cols(plan: &FullPlan) -> Vec<Col> {
    let mut cols = Vec::new();
    if let Some(slot) = plan.gauge {
        cols.extend([slot.at, Col::new(slot.divider, 0.0)]);
    }
    cols.extend([plan.name, plan.well]);
    cols.extend(plan.units.map(|units| units.at));
    cols.extend(plan.pct);
    cols.push(strip_col(plan.strip));
    cols.extend(plan.meter.cols());
    cols.extend(plan.details);
    cols
}

fn block_cols(block: &BlockPlan) -> Vec<Col> {
    let mut cols = vec![block.name, block.well];
    cols.extend(block.units.map(|units| units.at));
    cols.extend(block.pct);
    cols.push(strip_col(block.strip));
    cols.extend(block.meter.cols());
    cols.extend(block.details);
    cols
}

fn assert_in_order(cols: &[Col], x0: f64, x1: f64, what: &str) {
    let mut edge = x0;
    for col in cols {
        assert!(
            col.w >= 0.0 && col.x >= edge,
            "{what}: {col:?} starts before {edge}"
        );
        edge = col.end();
    }
    assert!(edge <= x1, "{what}: the columns end at {edge}, past {x1}");
}

fn assert_on_pixels(cols: &[Col], what: &str) {
    for s in [1.0, 2.0] {
        for edge in cols.iter().flat_map(|col| [col.x, col.end()]) {
            assert_eq!(edge * s, px(edge, s), "{what}: edge {edge} at {s}×");
        }
    }
}

/// A Full plan's strip and meter: the strip's (x, cols, pitch), then its bar and run.
fn full_meter(p: &FullPlan) -> ((f64, u32, f64), Option<Col>, Option<Col>) {
    (
        (p.strip.x, p.strip.cols, p.strip.pitch),
        p.meter.bar,
        p.meter.run,
    )
}

/// The strip takes the row (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §2): from the
/// strip's own start, the bar `bar_gap` past its end, the run [`RUN_GAP`] past the bar's,
/// then Details, every edge on whole device px.
#[test]
fn plan_golden_tables_full() {
    let m = hn();
    let at = |w: f64| {
        let plan = plan_full(w, &m);
        assert_eq!(plan_full((2.0 * w + 1.0) / 2.0, &m), plan);
        assert_on_pixels(&full_cols(&plan), &format!("W {w}"));
        plan
    };
    let col = |x: f64, w: f64| Some(Col::new(x, w));

    // The owner's window: six hours' strip of 429 pt, a bar of 134 and the run with each
    // row's maximum.
    let p = at(1039.0);
    assert_eq!((p.kind, p.offset), (FullKind::Wide, 0.0));
    assert_eq!((p.name, p.pct), (Col::new(97.0, 56.0), col(166.0, 30.0)));
    assert_eq!(
        full_meter(&p),
        ((203.0, 86, 5.0), col(641.0, 134.0), col(781.0, 88.0))
    );
    assert_eq!((p.strip.end(), p.strip.span_s()), (632.0, 21_600));
    assert_eq!(p.details, col(879.0, 148.0));
    assert_eq!(
        (p.gauge, p.cores, p.machine),
        (
            Some(GaugeSlot {
                at: Col::new(12.0, 68.0),
                divider: 88.0
            }),
            None,
            None
        )
    );
    assert_eq!([2.0, 1.0].map(|s| px(p.strip.x, s)), [406.0, 203.0]);
    assert_eq!([2.0, 1.0].map(|s| px(p.strip.cell, s)), [8.0, 4.0]);

    let p = at(1424.0);
    assert_eq!(
        full_meter(&p),
        ((203.0, 120, 5.0), col(811.0, 240.0), col(1057.0, 88.0))
    );
    assert_eq!(p.details, col(1155.0, 257.0));
    // Past the strip's and the bar's most, the table centres and Details take the rest.
    let p = at(2560.0);
    assert_eq!((p.offset, p.strip.x, p.strip.cols), (416.0, 619.0, 120));
    assert_eq!(p.details, col(1571.0, 560.0));

    let p = at(940.0);
    assert_eq!(
        full_meter(&p),
        ((203.0, 76, 5.0), col(591.0, 103.0), col(700.0, 88.0))
    );
    assert_eq!(p.details, col(798.0, 130.0));
    // Mid lays no Details: the strip takes their room at a finer pitch.
    let p = at(900.0);
    assert_eq!(p.kind, FullKind::Mid);
    assert_eq!(
        full_meter(&p),
        ((203.0, 103, 4.0), col(623.0, 171.0), col(800.0, 88.0))
    );
    assert_eq!(p.details, None);
    let p = at(700.0);
    assert_eq!(
        full_meter(&p),
        ((203.0, 73, 4.0), col(503.0, 91.0), col(600.0, 88.0))
    );
    // Narrowing, the run gives way first, then the bar (as long as its legend at least),
    // and the strip, which keeps its share, last.
    let p = at(550.0);
    assert_eq!(
        (p.kind, p.name, p.pct),
        (FullKind::Narrow, Col::new(12.0, 52.0), col(77.0, 30.0))
    );
    assert_eq!(full_meter(&p), ((114.0, 51, 5.0), col(375.0, 163.0), None));
    assert_eq!(p.strip.span_s(), 21_600);
    let p = at(500.0);
    assert_eq!(full_meter(&p), ((114.0, 48, 5.0), col(360.0, 128.0), None));
    assert_eq!((p.gauge, p.cores), (None, Some(Col::new(77.0, 411.0))));
    // A bar column stands beside six hours (A1 step R5): at 450 pt the narrow table's strip
    // would hold four (39 boxes of 5 pt), so each bar stands under its % and the strip holds
    // six hours.
    let p = at(450.0);
    assert_eq!((p.kind, p.meter), (FullKind::Slim, Meter::default()));
    assert_eq!(
        (p.strip.x, p.strip.cols, p.strip.span_s()),
        (103.0, 84, 21_600)
    );
    assert_eq!((p.gauge, p.cores), (None, Some(Col::new(70.0, 368.0))));
    // Where no bar as long as its legend fits, it stands under each % and the strip takes
    // the room: four hours at 350 pt.
    let p = at(350.0);
    assert_eq!((p.kind, p.meter), (FullKind::Slim, Meter::default()));
    assert_eq!(
        (p.strip.x, p.strip.cols, p.strip.span_s()),
        (103.0, 59, 14_400)
    );
    assert!(p.bar_under().is_some());
    let p = at(210.0);
    assert_eq!((p.kind, p.meter), (FullKind::Slim, Meter::default()));
    assert!(p.bar_under().is_some());
}

/// Folded's blocks as Full's rows: the strip, the bar past it and the run past that, inside
/// each block; at the owner's widths the strip is over half its block.
#[test]
fn plan_golden_tables_folded() {
    let m = hn();
    let at = |w: f64| {
        let plan = plan_folded(w, &m);
        assert_eq!(plan_folded((2.0 * w + 1.0) / 2.0, &m), plan);
        for block in &plan.blocks {
            assert_on_pixels(&block_cols(block), &format!("W {w}"));
        }
        plan
    };
    let col = |x: f64, w: f64| Some(Col::new(x, w));
    let block_meter = |b: &BlockPlan| {
        (
            (b.strip.x, b.strip.cols, b.strip.pitch),
            b.meter.bar,
            b.meter.run,
        )
    };

    let p = at(1039.0);
    assert_eq!(
        p.gauge,
        Some(FoldedGauge {
            at: Col::new(12.0, 54.0),
            divider: 75.0,
            cy: 31.0
        })
    );
    assert_eq!(
        p.blocks.map(|b| (b.kind, b.at)),
        [
            (BlockKind::Wide, Col::new(84.0, 457.0)),
            (BlockKind::Wide, Col::new(570.0, 457.0))
        ]
    );
    let [a, b] = p.blocks;
    assert_eq!((a.name, a.pct), (Col::new(84.0, 52.0), col(148.0, 30.0)));
    // Six hours' strip of 284 pt and the bar to the block's end: no run, whose room the
    // strip keeps.
    assert_eq!(block_meter(&a), ((185.0, 57, 5.0), col(476.0, 65.0), None));
    assert_eq!(
        (a.strip.end(), a.strip.span_s(), a.details),
        (469.0, 21_600, None)
    );
    assert_eq!(block_meter(&b), ((671.0, 57, 5.0), col(962.0, 65.0), None));

    let a = at(940.0).blocks[0];
    assert_eq!(a.at, Col::new(84.0, 407.0));
    assert_eq!(block_meter(&a), ((185.0, 49, 5.0), col(436.0, 55.0), None));
    let a = at(1424.0).blocks[0];
    assert_eq!(block_meter(&a), ((185.0, 87, 5.0), col(626.0, 107.0), None));
    // A run where the bar is as long as it, and Details where they leave the strip 480 pt.
    let a = at(2560.0).blocks[0];
    assert_eq!(
        block_meter(&a),
        ((185.0, 120, 5.0), col(791.0, 160.0), col(957.0, 82.0))
    );
    assert_eq!(a.details, col(1049.0, 252.0));
    // Narrower blocks keep the strip's share: a shorter bar, a finer pitch, then the bar
    // under the %. Every bar column stands beside four hours (A1 step R5): at 650 pt a mid
    // block's strip would hold six minutes (32 boxes of 4 pt), so the narrow block lays 46
    // of 3 pt.
    let a = at(750.0).blocks[0];
    assert_eq!(
        (a.kind, block_meter(&a)),
        (BlockKind::Wide, ((185.0, 33, 5.0), col(356.0, 40.0), None))
    );
    let a = at(700.0).blocks[0];
    assert_eq!(
        (a.kind, block_meter(&a), a.strip.span_s()),
        (
            BlockKind::Mid,
            ((183.0, 37, 4.0), col(337.0, 34.0), None),
            14_400
        )
    );
    let a = at(650.0).blocks[0];
    assert_eq!(
        (a.kind, block_meter(&a), a.strip.span_s()),
        (
            BlockKind::Narrow,
            ((172.0, 46, 3.0), col(316.0, 30.0), None),
            14_400
        )
    );
    let a = at(600.0).blocks[0];
    assert_eq!((a.kind, a.meter), (BlockKind::Slim, Meter::default()));
    assert_eq!(
        (a.strip.x, a.strip.cols, a.strip.span_s()),
        (172.0, 50, 14_400)
    );
    assert!(a.bar_under().is_some());
    let a = at(450.0).blocks[0];
    assert_eq!((a.kind, a.meter), (BlockKind::Slim, Meter::default()));
    assert!(a.bar_under().is_some());
    // Under 389 pt the strip beside a % would hold a minute: the % goes and the strip, in its
    // place, holds six minutes (A1 step R5).
    let p = at(350.0);
    assert_eq!(
        (
            p.gauge,
            p.blocks[0].pct,
            p.blocks[0].strip.x,
            p.blocks[0].strip.cols,
            p.blocks[0].strip.span_s()
        ),
        (None, None, 71.0, 30, 360)
    );
    let p = at(400.0);
    assert_eq!(
        (p.gauge, p.blocks[0].strip.x, p.blocks[0].strip.cols),
        (None, 100.0, 28)
    );

    // The owner's widths (2026-10-26, about 940 to 1040 pt): each strip is well over half
    // its block (six tenths, to a box), reaches back six hours, and is the widest thing on
    // its row; the bar takes the rest of the block.
    for w in (940..=1040).map(f64::from) {
        for b in at(w).blocks {
            assert_eq!(b.strip.span_s(), 21_600, "W {w}");
            assert_eq!(
                b.meter.bar.map(Col::end),
                Some(b.at.end()),
                "W {w}: the bar ends the block"
            );
            assert!(
                b.strip.width() >= 0.6 * b.at.w - b.strip.pitch,
                "W {w}: a strip of {} in a block of {}",
                b.strip.width(),
                b.at.w
            );
            assert_widest(b.strip, &b.meter, b.details, &format!("W {w}"));
        }
    }
}

#[test]
fn rows_sit_where_the_vertical_tables_say() {
    let m = hn();
    let tops = |rows: &Rows, s: f64| {
        (0..rows.count)
            .map(|r| px(rows.top(r), s))
            .collect::<Vec<_>>()
    };

    let wide = plan_full(1119.0, &m).rows;
    assert_eq!(
        (wide.header_baseline, wide.count, wide.cell, wide.baseline),
        (11.5, 7, 9.0, 8.0)
    );
    assert_eq!(
        tops(&wide, 2.0),
        [32.0, 56.0, 80.0, 104.0, 128.0, 152.0, 176.0]
    );
    assert_eq!([2.0, 1.0].map(|s| px(wide.cell, s)), [18.0, 9.0]);
    assert_eq!(
        (wide.name_pt, wide.amount_pt, wide.figure_pt, wide.bar_h),
        (10.5, 10.0, 10.0, 6.0)
    );

    let narrow = plan_full(500.0, &m).rows;
    assert_eq!(
        (narrow.count, narrow.pitch, narrow.cell, narrow.baseline),
        (8, 10.5, 8.0, 7.0)
    );
    assert_eq!(
        (narrow.name_pt, narrow.amount_pt, narrow.figure_pt),
        (10.0, 10.0, 10.0)
    );
    let slim = plan_full(250.0, &m).rows;
    assert_eq!((slim.name_pt, slim.amount_pt, slim.bar_h), (9.5, 9.5, 5.0));

    let folded = plan_folded(1119.0, &m).blocks[0].rows;
    assert_eq!(
        (folded.header_baseline, folded.count, folded.baseline),
        (9.5, 4, 7.5)
    );
    assert_eq!(tops(&folded, 2.0), [28.0, 51.0, 74.0, 97.0]);
    assert_eq!(tops(&folded, 1.0), [14.0, 26.0, 37.0, 49.0]);
    assert_eq!([2.0, 1.0].map(|s| px(folded.cell, s)), [17.0, 9.0]);
    assert_eq!(
        (
            folded.name_pt,
            folded.amount_pt,
            folded.figure_pt,
            folded.bar_h
        ),
        (10.0, 9.5, 9.5, 5.0)
    );
    assert_eq!(plan_folded(350.0, &m).blocks[0].rows.name_pt, 9.5);
}

/// The bar under a % figure stands where the now-left design's table puts it
/// (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5): from a point below the digits'
/// baseline, 2 pt tall, clear of the next row's top by a point in Folded and half a point in
/// Full narrow and slim, so it costs the deck no height.
#[test]
fn the_bar_under_the_percent_stays_inside_the_row_pitch() {
    let m = hn();
    for (what, rows, want, clear) in [
        (
            "Folded",
            plan_folded(350.0, &m).blocks[0].rows,
            (8.5, 10.5),
            1.0,
        ),
        ("Full slim", plan_full(210.0, &m).rows, (8.0, 10.0), 0.5),
        ("Full narrow", plan_full(400.0, &m).rows, (8.0, 10.0), 0.5),
        ("Full wide", plan_full(1119.0, &m).rows, (9.0, 11.0), 1.0),
    ] {
        for r in 0..rows.count {
            let (y0, y1) = rows.under(r);
            let top = rows.top(r);
            assert_eq!((y0 - top, y1 - top), want, "{what} row {r}");
            assert_eq!(rows.top(r + 1) - y1, clear, "{what} row {r}");
            assert_eq!(y0 - (top + rows.baseline), UNDER_GAP, "{what} row {r}");
        }
    }
}

/// Every row that prints a % keys its three-scope bar
/// (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5): in a column right of the strip in
/// Full wide, mid and narrow, in fwide, fmid and fnarrow, and in Full slim with room left,
/// else under the % figure (fslim's always), never both, on both faces at every width. The
/// legend names the three scopes once, whole, or not at all.
#[test]
fn every_percent_has_its_bar_at_every_width() {
    let scopes = |head: &HeaderPlan| {
        head.labels
            .iter()
            .filter(|l| matches!(l.label, Label::Scope(_)))
            .count()
    };
    let faces = [hn(), FaceMetrics::test_stack()];
    let widths: Vec<(usize, u32)> = (0..faces.len())
        .flat_map(|face| (160..=5120).map(move |w| (face, w)))
        .collect();
    // Bars under the % seen in Full and in fslim, and Full slim's column.
    let seen = aterm_spec::check::each(&widths, |&(face, w)| {
        let (m, w) = (&faces[face], f64::from(w));
        let mut seen = [0_u32; 3];
        let full = plan_full(w, m);
        if !full.ground_only && full.pct.is_some() {
            let at = format!("Full {:?} W {w}", full.kind);
            let bar = full.meter.bar;
            assert!(bar.is_some() != full.bar_under().is_some(), "{at}");
            match (full.kind, bar) {
                (FullKind::Slim, None) => seen[0] += 1,
                (FullKind::Slim, Some(_)) => seen[2] += 1,
                (_, bar) => assert!(bar.is_some(), "{at}: its column"),
            }
            let n = scopes(&header_full(&full, TEXT, None, m));
            assert!(n == 0 || n == 3, "{at}: the legend in part, {n} names");
        }
        let folded = plan_folded(w, m);
        if folded.ground_only {
            return seen;
        }
        for block in folded.blocks.iter().filter(|b| b.pct.is_some()) {
            let at = format!("Folded {:?} W {w}", block.kind);
            let bar = block.meter.bar;
            assert!(bar.is_some() != block.bar_under().is_some(), "{at}");
            match (block.kind, bar) {
                (BlockKind::Slim, bar) => {
                    assert_eq!(bar, None, "{at}: fnarrow lays slim's column first");
                    seen[1] += 1;
                }
                (_, bar) => assert!(bar.is_some(), "{at}: its column"),
            }
        }
        let n = scopes(&header_folded(&folded, TEXT, [None, None], m));
        assert!(n == 0 || n == 3, "Folded W {w}: the legend once, {n} names");
        seen
    })
    .into_iter()
    .fold([0_u32; 3], |mut all, one| {
        all.iter_mut().zip(one).for_each(|(n, more)| *n += more);
        all
    });
    assert!(seen.iter().all(|&n| n > 10), "CONTROL: {seen:?}");
}

/// Where the bars stand under the % figures, the scope key (`this Mac ■ aterm ■ Tab`) takes
/// the leftmost room over no strip that the scale and rule K's items leave, whole (the
/// machine's last word standing for "this Mac" where only that fits), or none
/// (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5): it never costs the scale a tick or
/// the key an item. Where bars stand under the % the scopes' heads (over their figures or
/// shares) usually key them, so the header here is planned over rooms of its own.
#[test]
fn the_key_of_bars_under_the_percent_comes_after_the_scale() {
    let m = hn();
    // A minute's strip, whose header holds its label and a tick but not "over max".
    let strip = plan_folded(350.0, &m).blocks[0].strip;
    let words = |s: &str| m.width(s, HEADER_PT, false);
    let need = 3.0 * SWATCH_ADVANCE + words("this Mac") + words("aterm") + words("Tab 2") + 16.0;
    let noun_need = need - words("this Mac") + words("Mac");
    let over = KeyItem::Over.width(&m) + KEY_GAP;
    let x0 = strip.end() + 20.0;
    let plan = |keys: &'static [KeyItem], room: f64, strip_room: bool, under: bool| {
        let mut head = Header::new(&m);
        head.keys = keys;
        head.time(strip);
        head.span(x0, x0 + room, strip_room);
        if under {
            head.under_key = Some(("this Mac".to_owned(), "Tab 2".to_owned()));
        }
        head.finish()
    };
    let names = |head: &HeaderPlan| -> Vec<(f64, String)> {
        head.labels
            .iter()
            .filter(|l| matches!(l.label, Label::Scope(_)))
            .map(|l| (l.x, l.text.clone()))
            .collect()
    };
    let rest = |head: &HeaderPlan| {
        let labels: Vec<HeaderLabel> = head
            .labels
            .iter()
            .filter(|l| !matches!(l.label, Label::Scope(_)))
            .cloned()
            .collect();
        (labels, head.key.clone(), head.ruler.clone())
    };
    let firsts = |head: &HeaderPlan| names(head).first().map(|n| n.0);
    let shown: &'static [KeyItem] = &[KeyItem::Over];
    for keys in [shown, &[]] {
        for room in [need - 0.5, need, over + need - 0.5, over + need, 400.0] {
            for strip_room in [false, true] {
                let with = plan(keys, room, strip_room, true);
                let at = format!("{keys:?}, {room} pt, over a strip {strip_room}");
                assert_eq!(
                    rest(&with),
                    rest(&plan(keys, room, strip_room, false)),
                    "{at}: the key costs the scale and rule K nothing"
                );
                let said = names(&with);
                assert!(said.is_empty() || said.len() == 3, "{at}: {said:?}");
                if strip_room {
                    assert_eq!(said, [], "{at}: the key stands over no strip");
                }
            }
        }
    }
    // "over max" takes the room's start (the strip's own header has none for it), and the
    // key the rest, when it holds the key's need.
    assert_eq!(
        placed(&plan(shown, 400.0, false, true), KeyItem::Over),
        Some(x0)
    );
    assert_eq!(
        firsts(&plan(shown, over + need, false, true)),
        Some(x0 + over)
    );
    let noun = |head: &HeaderPlan| names(head).first().map(|n| n.1.clone());
    assert_eq!(
        noun(&plan(shown, over + need - 0.5, false, true)).as_deref(),
        Some("Mac")
    );
    assert_eq!(
        firsts(&plan(shown, over + noun_need - 0.5, false, true)),
        None
    );
    assert_eq!(firsts(&plan(&[], need, false, true)), Some(x0));
    assert_eq!(
        noun(&plan(&[], need - 0.5, false, true)).as_deref(),
        Some("Mac")
    );
    assert_eq!(firsts(&plan(&[], noun_need - 0.5, false, true)), None);
    assert_eq!(
        names(&plan(shown, 400.0, false, true))
            .into_iter()
            .map(|n| n.1)
            .collect::<Vec<_>>(),
        ["this Mac", "aterm", "Tab 2"]
    );
}

#[test]
fn breakpoints_follow_the_face() {
    let helvetica = breakpoints(&hn());
    // A1 step R5: a kind with a bar column fits from the width its column holds at every wider
    // one beside four hours (Folded) or six (Full), and a % beside six minutes.
    assert_eq!(helvetica.full, [909.0, 565.0, 474.0, 194.0]);
    assert_eq!(helvetica.block, [289.0, 269.0, 250.0, 168.0]);
    assert_eq!(helvetica.folded, [703.0, 663.0, 625.0, 461.0]);
    assert_eq!(helvetica.folded_bare, 389.0);

    let stack = FaceMetrics::test_stack();
    assert_eq!(plan_full(1119.0, &stack).name.w, 59.0);
    assert_eq!(plan_full(580.0, &stack).name.w, 59.0);
    assert_eq!(plan_full(500.0, &stack).name.w, 56.0);
    let dejavu = breakpoints(&stack);
    assert_eq!(dejavu.full, [912.0, 572.0, 482.0, 199.0]);
    assert_eq!(dejavu.block, [293.0, 273.0, 255.0, 173.0]);
    assert_eq!(dejavu.folded, [711.0, 671.0, 635.0, 471.0]);
    assert_eq!(dejavu.folded_bare, 399.0);

    let drawn = |w: f64| {
        (
            !plan_full(w, &hn()).ground_only,
            !plan_folded(w, &hn()).ground_only,
        )
    };
    // Without the % column the strip starts BARE_STRIP_GAP past the well, so the narrowest
    // decks that draw a table are a point (Full) and four (Folded's two blocks) wider than
    // where the now marker's line stood over the well's marks (the review of 2026-10-26).
    assert_eq!(
        [164.0, 165.0, 288.0, 289.0].map(drawn),
        [(false, false), (true, false), (true, false), (true, true)]
    );

    for (m, bp) in [(&hn(), helvetica), (&stack, dejavu)] {
        for (kind, w) in FullKind::ALL.into_iter().zip(bp.full) {
            assert_eq!(plan_full(w, m).kind, kind);
        }
        for (kind, w) in FullKind::ALL.into_iter().zip(bp.full).take(3) {
            assert_ne!(plan_full(w - 1.0, m).kind, kind);
        }
        for (kind, w) in BlockKind::ALL.into_iter().zip(bp.folded) {
            let at = plan_folded(w, m);
            assert_eq!(at.blocks.map(|b| b.kind), [kind; 2]);
            assert!(at.gauge.is_some(), "{kind:?} at {w} has the gauge");
            let below = plan_folded(w - 1.0, m);
            if kind == BlockKind::Slim {
                assert_eq!((below.gauge, below.blocks[0].kind), (None, kind));
                assert!(below.blocks[0].pct.is_some());
            } else {
                assert_ne!(below.blocks[0].kind, kind);
            }
        }
        let bare = plan_folded(bp.folded_bare, m);
        assert_eq!((bare.gauge, bare.blocks[0].kind), (None, BlockKind::Slim));
        assert!(bare.blocks[0].pct.is_some());
        let below = plan_folded(bp.folded_bare - 1.0, m).blocks[0];
        assert_eq!((below.kind, below.pct), (BlockKind::Slim, None));
    }
}

/// Where the now marker's line stands in a table with `well`, `pct` and `strip`, against
/// what is beside it (`what` names the table): a point wide at most, clear of the well's
/// marks and of the % column in device px at 1× and 2×. Under slim's width it once stood
/// inside fslim's well, over the right edge of its marks (the review of 2026-10-26).
fn marker_faults(what: &str, well: Col, pct: Option<Col>, strip: Strip) -> Vec<String> {
    let x = strip.x - crate::stats_live::LINE_DX;
    let mut faults = Vec::new();
    for s in [1.0_f64, 2.0] {
        // As `Deck::live_edge` draws it.
        let lw = (0.75 * s).round().max(1.0);
        let lx = (x * s - lw / 2.0).round();
        if lx < (well.end() + 0.5) * s {
            faults.push(format!(
                "{what} {s}×: the line at {lx} px meets the well {well:?}"
            ));
        }
        if let Some(pct) = pct
            && lx < pct.end() * s
        {
            faults.push(format!("{what} {s}×: the line at {lx} px meets {pct:?}"));
        }
    }
    faults
}

/// The strip is the row's widest part (the owner, 2026-10-26: "see how the time series is
/// too showr"): wider than the bar, the figure run and Details, each.
fn assert_widest(strip: Strip, meter: &Meter, details: Option<Col>, what: &str) {
    for col in meter.cols().into_iter().chain(details) {
        assert!(
            strip.width() > col.w,
            "{what}: the strip {strip:?} is no wider than {col:?}"
        );
    }
}

/// Narrowing, the run gives way first, then the bar (to a narrower kind's least, then under
/// each %), and the strip last (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §2): at every
/// width, on both faces, a run stands only beside a bar at least as long as the run's least,
/// no width lays a run below one that lays no bar column, and every strip beside a bar keeps
/// its kind's share of the meter's room (up to its most). CONTROL: runs, bar columns and bars under the % are
/// each seen in both forms.
#[test]
fn narrowing_drops_the_run_then_the_bar_and_the_strip_keeps_its_share() {
    for m in [hn(), FaceMetrics::test_stack()] {
        // (form, width, strip, meter, the kind's share, the run's least, a bar under the %)
        let mut tables = Vec::new();
        for px2 in 320..=5_120 {
            let w = f64::from(px2) / 2.0;
            let full = plan_full(w, &m);
            if !full.ground_only {
                let spec = match full.kind {
                    FullKind::Wide => FULL_WIDE,
                    FullKind::Mid => FULL_MID,
                    FullKind::Narrow => FULL_NARROW,
                    FullKind::Slim => FULL_SLIM,
                };
                let under = full.bar_under().is_some();
                tables.push(("Full", w, full.strip, full.meter, spec, under));
            }
            let folded = plan_folded(w, &m);
            if !folded.ground_only {
                for b in folded.blocks {
                    let spec = match b.kind {
                        BlockKind::Wide => FOLD_WIDE,
                        BlockKind::Mid => FOLD_MID,
                        BlockKind::Narrow => FOLD_NARROW,
                        BlockKind::Slim => FOLD_SLIM,
                    };
                    let under = b.bar_under().is_some();
                    tables.push(("Folded", w, b.strip, b.meter, spec, under));
                }
            }
        }
        for form in ["Full", "Folded"] {
            let ours = || tables.iter().filter(move |t| t.0 == form);
            let lowest_run = ours()
                .filter(|t| t.3.run.is_some())
                .map(|t| t.1)
                .fold(f64::INFINITY, f64::min);
            let highest_under = ours()
                .filter(|t| t.3.bar.is_none())
                .map(|t| t.1)
                .fold(0.0, f64::max);
            assert!(
                highest_under < lowest_run,
                "{form}: a run at {lowest_run} pt, no bar column at {highest_under} pt"
            );
            let seen = (
                ours().filter(|t| t.3.run.is_some()).count(),
                ours().filter(|t| t.3.bar.is_some()).count(),
                ours().filter(|t| t.5).count(),
            );
            assert!(
                seen.0 > 0 && seen.1 > seen.0 && seen.2 > 0,
                "CONTROL {form}: {seen:?}"
            );
            for &(_, w, strip, meter, spec, _) in ours() {
                let Some(bar) = meter.bar else { continue };
                if meter.run.is_some() {
                    assert!(bar.w >= spec.run, "{form} {w} pt: {bar:?} beside a run");
                }
                let room = bar.end() - strip.x;
                assert!(
                    strip.width() + spec.pitch >= (spec.share * room).min(spec.strip.1),
                    "{form} {w} pt: a strip of {} short of its share of {room}",
                    strip.width()
                );
            }
        }
    }
}

/// The machine's used of total as `words::machine_totals` says it for a 10-core Mac of 32 GiB
/// and 4 TB, `mem` GiB and `disk` TB used: each form with the template it is measured by.
fn used_totals(mem: &str, disk: &str) -> Vec<TotalsForm> {
    let form = |said: String, widest: &str, used: bool| TotalsForm {
        said,
        widest: widest.to_owned(),
        used,
    };
    let totals = [
        "10 cores \u{b7} 32 GiB \u{b7} 4 TB",
        "10c \u{b7} 32G \u{b7} 4T",
    ];
    vec![
        form(
            format!("10 cores \u{b7} {mem} of 32 GiB \u{b7} {disk} of 4 TB"),
            "10 cores \u{b7} 88.8 of 32 GiB \u{b7} 8.88 of 4 TB",
            true,
        ),
        form(
            format!("10c \u{b7} {mem}/32G \u{b7} {disk}/4T"),
            "10c \u{b7} 88.8/32G \u{b7} 8.88/4T",
            true,
        ),
        form(totals[0].to_owned(), totals[0], false),
        form(totals[1].to_owned(), totals[1], false),
    ]
}

/// Every form's plan at every width from 300 pt to 2560 pt that lays a header.
fn header_plans(m: &FaceMetrics) -> Vec<(DeckForm, f64, FullPlan, FoldedPlan)> {
    let mut out = Vec::new();
    for w in (300..=2_560).map(f64::from) {
        let (full, folded) = (plan_full(w, m), plan_folded(w, m));
        for form in [DeckForm::Full, DeckForm::Folded] {
            out.push((form, w, full, folded));
        }
    }
    out
}

/// What the legend's machine name says in `form` with `totals`, and whether a run stands
/// there (where the legend takes no totals); `None` where no legend or key names it.
fn legend_machine(
    at: (DeckForm, &FullPlan, &FoldedPlan),
    totals: &[TotalsForm],
    m: &FaceMetrics,
) -> Option<String> {
    legend_machine_over(at, totals, [false; 2], m)
}

/// [`legend_machine`] with the blocks' cells drawing the over mark as `over` says.
fn legend_machine_over(
    at: (DeckForm, &FullPlan, &FoldedPlan),
    totals: &[TotalsForm],
    over: [bool; 2],
    m: &FaceMetrics,
) -> Option<String> {
    legend_heads_over(at, totals, over, m)
        .into_iter()
        .next()
        .map(|h| h.0)
}

/// The legend's names and where each ends ([`heads`]) in the header [`legend_machine_over`]
/// reads the machine's name from.
fn legend_heads_over(
    (form, full, folded): (DeckForm, &FullPlan, &FoldedPlan),
    totals: &[TotalsForm],
    over: [bool; 2],
    m: &FaceMetrics,
) -> Vec<(String, f64)> {
    let text = HeaderText {
        totals,
        over,
        ..TEXT
    };
    let head = match form {
        DeckForm::Full => header_full(full, text, None, m),
        DeckForm::Folded => header_folded(folded, text, [None, None], m),
    };
    heads(&head, m)
}

/// Which of `totals` the machine's name says (`this Mac · …`), or `None` for the name alone.
fn form_of(said: Option<&str>, totals: &[TotalsForm]) -> Option<usize> {
    let said = said?;
    totals
        .iter()
        .position(|t| said == format!("this Mac \u{b7} {}", t.said))
}

fn run_stands(form: DeckForm, full: &FullPlan, folded: &FoldedPlan) -> bool {
    match form {
        DeckForm::Full => full.meter.run.is_some(),
        DeckForm::Folded => folded.blocks[0].meter.run.is_some(),
    }
}

/// The legend says the machine's used of total in its longest form that fits
/// (`docs/DESIGN-stats-deck-flow-2026-11-02.md` §4): at every width of both forms, the form
/// it says fits alone there and says the figures of the first of the four that does (the used
/// of total, else the totals alone, else neither), each measured by its widest, so a form
/// whose words fit but whose widest does not is not said. It is that first form itself but
/// where Folded's over mark's key keeps its room over block B's strip, which takes a long
/// wording to its short one and never costs a figure (`docs/DESIGN-stats-deck-meter-
/// 2026-10-26.md` §4). CONTROL: each used-of-total form and the totals alone are each said at
/// some width; the key's room shortens a form at some Folded width; and a form measured by
/// its words alone is said where its widest does not fit.
#[test]
fn the_legend_says_used_of_total_in_its_longest_form_that_fits() {
    let m = hn();
    let totals = used_totals("14.4", "2.34");
    // What a form says: the used of total, the totals alone, or neither (`None`).
    let figures = |i: Option<usize>| i.map_or(2, |i| usize::from(!totals[i].used));
    let mut said = [0_usize; 5];
    let (mut by_words, mut keyed) = (0, 0);
    for (form, w, full, folded) in header_plans(&m) {
        let at = (form, &full, &folded);
        let chosen = form_of(legend_machine(at, &totals, &m).as_deref(), &totals);
        let fits: Vec<bool> = totals
            .iter()
            .map(|t| {
                let one = std::slice::from_ref(t);
                form_of(legend_machine(at, one, &m).as_deref(), one) == Some(0)
            })
            .collect();
        let first = fits.iter().position(|&f| f);
        assert!(
            chosen.is_none_or(|c| fits[c]),
            "{form:?} {w} pt: says {chosen:?}, which does not fit alone: {fits:?}"
        );
        assert_eq!(
            figures(chosen),
            figures(first),
            "{form:?} {w} pt: says {chosen:?} where {first:?} fits"
        );
        if chosen != first {
            assert_eq!(form, DeckForm::Folded, "{w} pt: {chosen:?} for {first:?}");
            assert_eq!(chosen, first.map(|f| f + 1), "{w} pt: its short wording");
            keyed += 1;
        }
        said[chosen.unwrap_or(4)] += 1;
        if !fits[0] {
            let lying = TotalsForm {
                said: "10c".into(),
                widest: totals[0].widest.clone(),
                used: true,
            };
            let one = std::slice::from_ref(&lying);
            assert_ne!(
                form_of(legend_machine(at, one, &m).as_deref(), one),
                Some(0),
                "{form:?} {w} pt: the words fit, the widest does not"
            );
            let honest = said_totals(&["10c"]);
            by_words += usize::from(
                form_of(legend_machine(at, &honest, &m).as_deref(), &honest) == Some(0),
            );
        }
    }
    assert!(
        said.iter().all(|&n| n > 0),
        "CONTROL: each form is said: {said:?}"
    );
    assert!(
        keyed > 0,
        "CONTROL: the over mark's key's room shortens a form at some Folded width"
    );
    assert!(
        by_words > 0,
        "CONTROL: a short form's words fit where the long one does not"
    );
}

/// A changing reading never flips the legend's form (`docs/DESIGN-stats-deck-flow-
/// 2026-11-02.md` §4): the same Mac read near empty, part used and near full, with the cells
/// of neither block, either or both drawing the over mark, says the same form at every width
/// of both forms, since each form is measured by its widest and Folded's keeps the over mark's
/// key its room over B's strip whether or not a cell draws the mark; and Folded's legend ends
/// at block B's meter whatever its words. CONTROL: the same forms measured by their words
/// alone flip at some width, and at some Folded width the legend says a shorter form than
/// fits in its room with the key's, so a legend that kept the key's room only while B draws
/// the mark would flip there.
#[test]
fn a_changing_reading_never_flips_the_legends_form() {
    let m = hn();
    let readings = [
        ("0.1", "0.01"),
        ("3.0", "0.95"),
        ("14.4", "2.34"),
        ("31.9", "3.99"),
    ];
    let overs = [[false, false], [false, true], [true, false], [true, true]];
    let measured: Vec<Vec<TotalsForm>> = readings.iter().map(|&(g, t)| used_totals(g, t)).collect();
    let by_words: Vec<Vec<TotalsForm>> = measured
        .iter()
        .map(|forms| {
            forms
                .iter()
                .map(|t| TotalsForm {
                    said: t.said.clone(),
                    widest: t.said.clone(),
                    used: t.used,
                })
                .collect()
        })
        .collect();
    let (mut flips, mut used, mut keyed) = (0, 0, 0);
    for (form, w, full, folded) in header_plans(&m) {
        let at = (form, &full, &folded);
        let said = |totals: &[TotalsForm], over: [bool; 2]| {
            form_of(legend_machine_over(at, totals, over, &m).as_deref(), totals)
        };
        // Each reading under each over mark: its legend's names, its form read off the first.
        let legends = overs
            .iter()
            .flat_map(|&over| measured.iter().map(move |t| (t, over)))
            .map(|(t, over)| {
                let legend = legend_heads_over(at, t, over, &m);
                (
                    over,
                    form_of(legend.first().map(|h| h.0.as_str()), t),
                    legend,
                )
            })
            .collect::<Vec<_>>();
        let forms: Vec<Option<usize>> = legends.iter().map(|(_, form, _)| *form).collect();
        assert!(
            forms.windows(2).all(|p| p[0] == p[1]),
            "{form:?} {w} pt: {forms:?}"
        );
        used += usize::from(forms[0].is_some_and(|i| i < 2));
        let words: Vec<Option<usize>> = by_words.iter().map(|t| said(t, [false; 2])).collect();
        flips += usize::from(words.windows(2).any(|p| p[0] != p[1]));
        if form == DeckForm::Folded
            && let (Some(_), Some(b_end)) =
                (folded.blocks[0].meter.bar, folded.blocks[1].meter.end())
        {
            let hi = b_end.min(folded.blocks[1].at.end());
            for (over, _, legend) in &legends {
                if let Some(&(_, end)) = legend.last() {
                    assert!(
                        (end - hi).abs() < 1e-9,
                        "W {w} {over:?}: the legend ends at {end}"
                    );
                }
            }
            // The longest of these forms that fits in the legend's whole room, the key's
            // included, against the one it says.
            let text = HeaderText {
                totals: &measured[2],
                ..TEXT
            };
            let tail = folded.blocks[0].meter.run.is_none();
            let lead = folded.blocks[1].strip.x + MARKER_ROOM;
            let whole = Header::new(&m)
                .legend_forms(&text, tail)
                .iter()
                .position(|f| f.2 <= hi - lead + 1e-9)
                .filter(|&i| i < measured[2].len());
            keyed += usize::from(forms[0] != whole);
        }
    }
    assert!(used > 0, "CONTROL: a used-of-total form is said");
    assert!(
        flips > 0,
        "CONTROL: measured by their words, the forms flip"
    );
    assert!(
        keyed > 0,
        "CONTROL: at some Folded width the key's room shortens the form it says"
    );
}

/// Where a run stands at the end of the bars, it says each row's maximum, so the legend says
/// the machine's name alone (`docs/DESIGN-stats-deck-meter-2026-10-26.md` §4, kept by the
/// flow record's §4): at every width of both forms where one stands, no totals follow the
/// name. CONTROL: where none stands, the legend says the totals at some width.
#[test]
fn a_standing_run_takes_the_totals_from_the_legend() {
    let m = hn();
    let totals = used_totals("14.4", "2.34");
    let (mut with_run, mut totals_said) = (0, 0);
    for (form, w, full, folded) in header_plans(&m) {
        let said = legend_machine((form, &full, &folded), &totals, &m);
        if run_stands(form, &full, &folded) {
            with_run += 1;
            assert!(
                said.as_deref().is_none_or(|s| !s.contains('\u{b7}')),
                "{form:?} {w} pt: a run stands, yet the legend says {said:?}"
            );
        } else {
            totals_said += usize::from(form_of(said.as_deref(), &totals).is_some());
        }
    }
    assert!(
        with_run > 0 && totals_said > 0,
        "CONTROL: {with_run} runs, {totals_said} totals"
    );
}

/// The legend keys the bars once and never across the Folded blocks' divider: every scope
/// name stands, with its swatch, inside one block, at every width, with and without the
/// machine's totals. CONTROL: a legend is laid at most widths.
#[test]
fn the_legend_stays_inside_one_block() {
    let m = hn();
    let totals = said_totals(&[
        "18 cores \u{b7} 128 GiB \u{b7} 2 TB",
        "18c \u{b7} 128G \u{b7} 2T",
    ]);
    let mut laid = 0;
    for w in (300..=2_560).map(f64::from) {
        let plan = plan_folded(w, &m);
        if plan.ground_only {
            continue;
        }
        for text in [
            TEXT,
            HeaderText {
                totals: &totals,
                ..TEXT
            },
        ]
        .into_iter()
        .flat_map(|text| {
            [[true, false], [false, true], [true; 2], [false; 2]]
                .map(|over| HeaderText { over, ..text })
        }) {
            let head = header_folded(&plan, text, [None, None], &m);
            let said = heads(&head, &m);
            laid += usize::from(said.len() == 3);
            for (l, (words, end)) in head
                .labels
                .iter()
                .filter(|l| matches!(l.label, Label::Scope(_)))
                .zip(&said)
            {
                assert!(
                    plan.blocks
                        .iter()
                        .any(|b| l.x >= b.at.x - 1e-9 && *end <= b.at.end() + 1e-9),
                    "W {w}: {words:?} at {}..{end} crosses a block's edge",
                    l.x
                );
            }
        }
    }
    assert!(laid > 3_000, "CONTROL: {laid} legends laid");
}

/// A state no row says keeps `now` beside each live edge's dot at the owner's widths and down
/// to Full's narrow tables: it stands over the names beside `now`, or at the strip's left
/// (the owner's hot scene said "Memory warn" over `now`, review of 2026-10-26).
#[test]
fn a_state_keeps_now_beside_the_dot() {
    let m = hn();
    let states = [
        StateLabel {
            full: "Memory warn +1",
            bare: "Memory warn",
        },
        StateLabel {
            full: "Disk full ~34 min +1",
            bare: "Disk full ~34 min",
        },
    ];
    let nows = |head: &HeaderPlan| head.labels.iter().filter(|l| l.label == Label::Now).count();
    let stated = |head: &HeaderPlan| {
        head.labels
            .iter()
            .filter(|l| matches!(l.label, Label::State(_)))
            .count()
    };
    for state in states {
        for w in [1039.0, 940.0, 700.0, 450.0, 400.0] {
            let plan = plan_full(w, &m);
            let head = header_full(&plan, TEXT, Some(state), &m);
            assert_eq!(
                nows(&head),
                1,
                "Full {w} pt, {:?}: {:?}",
                state.full,
                head.labels
            );
            // Where Details stand, they say the state.
            assert_eq!(
                stated(&head),
                usize::from(plan.details.is_none()),
                "Full {w} pt"
            );
        }
        for w in (940..=1_040).map(f64::from) {
            let plan = plan_folded(w, &m);
            let head = header_folded(&plan, TEXT, [Some(state), Some(state)], &m);
            assert_eq!(
                nows(&head),
                2,
                "Folded {w} pt, {:?}: {:?}",
                state.full,
                head.labels
            );
            assert_eq!(stated(&head), 2, "Folded {w} pt");
        }
    }
}

#[test]
fn every_width_plans_inside_the_card() {
    for m in [hn(), FaceMetrics::test_stack()] {
        let slim_beside = breakpoints(&m).folded[3];
        for w in (160..=5120).map(f64::from) {
            let full = plan_full(w, &m);
            assert_ne!(full.strip.period_s, FOLD_SLOT_S, "W {w}: a Full strip");
            assert_eq!(
                full.strip.spec,
                Strip::spec_for(full.strip.width()),
                "W {w}"
            );
            if !full.ground_only {
                // A reading a second moves its fine band 2 pt or more, half a 2 s box of 4 pt
                // or more, to the rounding of the box's place in the strip.
                assert!(full.strip.pt_per_s() >= 1.99, "Full W {w}");
                assert_widest(
                    full.strip,
                    &full.meter,
                    full.details,
                    &format!("Full W {w}"),
                );
                assert_in_order(&full_cols(&full), X0, w - X0, &format!("Full W {w}"));
                let what = format!("Full W {w}");
                let faults = marker_faults(&what, full.well, full.pct, full.strip);
                assert!(faults.is_empty(), "{faults:?}");
            }
            if full.offset > 0.0
                && let (Some(GaugeSlot { at, .. }), Some(details)) = (full.gauge, full.details)
            {
                assert!(
                    (at.x - (w - details.end())).abs() <= 1.0,
                    "W {w} is not centred"
                );
            }

            let folded = plan_folded(w, &m);
            let [a, b] = folded.blocks;
            assert!(a.at.end() < folded.divider && folded.divider < b.at.x);
            if let Some(g) = folded.gauge {
                let [x0, y0, x1, y1] = g.dial();
                assert!(
                    x0 >= X0 && x1 < g.divider && g.divider < a.at.x && a.at.x == a.name.x,
                    "Folded W {w}: the gauge left of block A: {g:?} {:?}",
                    a.at
                );
                assert!(y0 >= 4.0 && y1 <= 58.0, "Folded W {w}: {g:?} in the band");
            }
            assert_eq!(folded.gauge.is_some(), w >= slim_beside, "Folded W {w}");
            for block in &folded.blocks {
                let pitch = match block.kind {
                    BlockKind::Wide => 5.0,
                    BlockKind::Mid => 4.0,
                    BlockKind::Narrow | BlockKind::Slim => 3.0,
                };
                assert_eq!(
                    (block.strip.period_s, block.strip.pitch),
                    (FOLD_SLOT_S, pitch),
                    "Folded W {w}"
                );
                assert_eq!(block.strip.spec, Strip::spec_for(block.strip.width()));
                if !folded.ground_only {
                    // A reading a second moves its fine band 2 pt or more (the now-left
                    // design's time-true step, live §8's ruling of 2026-10-26).
                    assert!(block.strip.pt_per_s() >= 1.99, "Folded W {w}");
                    let what = format!("Folded W {w}");
                    assert_widest(block.strip, &block.meter, block.details, &what);
                }
                if !folded.ground_only {
                    assert_in_order(
                        &block_cols(block),
                        block.at.x,
                        block.at.end(),
                        &format!("Folded W {w}"),
                    );
                    let what = format!("Folded W {w}");
                    let faults = marker_faults(&what, block.well, block.pct, block.strip);
                    assert!(faults.is_empty(), "{faults:?}");
                }
            }
            if !folded.ground_only {
                assert!(a.at.x >= X0 && b.at.end() <= w - X0, "Folded W {w}");
            }
        }
    }
}

#[test]
fn deck_height_is_a_function_of_the_window() {
    use crate::stats_deck::{FOLDED_PT, StatsBars, deck_px};
    use DeckForm::{Folded, Full};
    let _: fn(DeckSizePref) -> DeckForm = deck_form_for;
    let _: fn(StatsBars, DeckForm, f64, usize) -> usize = deck_px;

    // Folded unless chosen Full: the form reads nothing of the window.
    assert_eq!(deck_form_for(DeckSizePref::Auto), Folded);
    assert_eq!(deck_form_for(DeckSizePref::Full), Full);
    assert_eq!(deck_form_for(DeckSizePref::Folded), Folded);

    // The deck paints the record's rows until A4's step N5: main's 62 and 100 pt.
    let painted = DeckRows::PAINTED;
    assert_eq!(
        (FOLDED_PT, painted.folded_pt(), painted.full_table_pt()),
        (62.0, 62.0, 100.0)
    );
    assert_eq!(painted.full_delta_px(2.0, 28), 84);
    assert_eq!(deck_px(StatsBars::Deck, Full, 2.0, 28), 208);
    // The instruments' 73.5 and 136 pt add 62.5 pt to Folded: five 28 px cells at 2×.
    let instruments = DeckRows::Instruments;
    assert_eq!(
        (instruments.folded_pt(), instruments.full_table_pt()),
        (73.5, 136.0)
    );
    assert_eq!(instruments.full_delta_px(2.0, 28), 140);
    for s in [1.0, 2.0] {
        let folded = (FOLDED_PT * s).round() as usize;
        let table = (painted.full_table_pt() * s).round() as usize;
        for cell in 10..=40 {
            let at = format!("{cell} px cells at {s}×");
            assert_eq!(deck_px(StatsBars::Deck, Folded, s, cell), folded, "{at}");
            let full = deck_px(StatsBars::Deck, Full, s, cell);
            assert_eq!((full - folded) % cell, 0, "{at}");
            assert_eq!(full - folded, painted.full_delta_px(s, cell), "{at}");
            assert!((table..table + cell).contains(&full), "{at}");
            let delta = instruments.full_delta_px(s, cell);
            let need = ((136.0 - 73.5) * s).round() as usize;
            assert!(
                delta.is_multiple_of(cell) && (need..need + cell).contains(&delta),
                "{at}"
            );
            for bars in [StatsBars::Sill, StatsBars::Off] {
                for deck in [Folded, Full] {
                    assert_eq!(deck_px(bars, deck, s, cell), 0, "{bars:?} {at}");
                }
            }
        }
    }
}

/// A4 §11's rule in Folded: the deck's height is a constant of its rows, which no reading, no
/// machine and no width changes. The instruments' band is 14 + 5 × 11.5 + 2 = 73.5 pt at
/// every whole width from 160 to 3200 pt, for every machine (some with no heat, disk, network
/// or processes reader), on either face: each block holds five rows of the same geometry,
/// block B its Machine row, and the dial, r 27 as built, is centred in the band at 36.75 pt,
/// 9.75 pt clear of its top and its foot. The plan the deck paints holds the record's four
/// rows a block in 62 pt and its dial at 31 pt, as on main. CONTROL: the dial at main's 31 pt
/// in the taller band stands 4 pt below its top and 15.5 pt above its foot.
#[test]
fn folded_is_73_5_pt_at_every_width_and_machine() {
    let band = |rows: &Rows| rows.top(rows.count) + FOLD_FOOT;
    let mut seen = 0;
    for m in [hn(), FaceMetrics::test_stack()] {
        for facts in std::iter::once(UnitFacts::NONE).chain(unit_machines()) {
            for w in (160..=3200).map(f64::from) {
                let p = plan_folded_with(w, &m, &facts);
                let what = || format!("{facts:?} at {w} pt");
                for blk in &p.blocks {
                    assert_eq!(
                        blk.rows,
                        Rows::folded(blk.kind, DeckRows::Instruments),
                        "{}",
                        what()
                    );
                    assert_eq!((blk.rows.count, band(&blk.rows)), (5, 73.5), "{}", what());
                }
                assert_eq!(
                    p.blocks.map(|blk| blk.machine.is_some()),
                    [false, true],
                    "{}",
                    what()
                );
                if let Some(g) = p.gauge {
                    let [_, y0, _, y1] = g.dial();
                    assert_eq!((y0, 73.5 - y1), (9.75, 9.75), "{}", what());
                }
                seen += 1;
            }
        }
        for w in (160..=3200).map(f64::from) {
            let p = plan_folded(w, &m);
            for blk in &p.blocks {
                assert_eq!(
                    (blk.rows.count, band(&blk.rows), blk.machine, blk.units),
                    (4, 62.0, None, None),
                    "the painted plan at {w} pt"
                );
            }
            assert!(p.gauge.is_none_or(|g| g.centre().1 == 31.0), "{w} pt");
        }
    }
    assert!(seen > 0);
    let main = FoldedGauge {
        cy: DeckRows::Resources.gauge_cy(),
        ..plan_folded_with(1039.0, &hn(), &this_mac())
            .gauge
            .expect("a gauge")
    };
    let [_, y0, _, y1] = main.dial();
    assert_eq!((y0, 73.5 - y1), (4.0, 15.5), "CONTROL");
}

/// A4 §11's rule in Full: ten rows (CPU, GPU, Memory, Disk, Heat, Network, Latency,
/// Processes, Cores, Machine) in 16 + 10 × 12 = 136 pt at every kind, narrow and slim at wide's
/// 12 pt pitch with their own cell and type, at every whole width from 160 to 3200 pt, for
/// every machine, on either face: a resize across kinds never changes the deck's height. Every
/// kind holds the Cores row and the Machine row. The plan the deck paints holds the record's
/// rows in 100 pt at every kind, as on main. CONTROL: ten rows at the record's 10.5 pt narrow
/// pitch take 121 pt, so a resize from mid to narrow would re-grid the tab.
#[test]
fn full_is_136_pt_at_every_kind() {
    let height = |rows: &Rows| rows.top(rows.count);
    let mut kinds = Vec::new();
    for m in [hn(), FaceMetrics::test_stack()] {
        for facts in std::iter::once(UnitFacts::NONE).chain(unit_machines()) {
            for w in (160..=3200).map(f64::from) {
                let p = plan_full_with(w, &m, &facts);
                let what = format!("{facts:?} at {w} pt, {:?}", p.kind);
                assert_eq!(p.rows, Rows::full(p.kind, DeckRows::Instruments), "{what}");
                assert_eq!(
                    (p.rows.count, p.rows.pitch, height(&p.rows)),
                    (10, 12.0, 136.0),
                    "{what}"
                );
                assert!(p.cores.is_some() && p.machine.is_some(), "{what}");
                if !kinds.contains(&p.kind) {
                    kinds.push(p.kind);
                }
            }
        }
        for w in (160..=3200).map(f64::from) {
            let p = plan_full(w, &m);
            assert_eq!(height(&p.rows), 100.0, "the painted plan at {w} pt");
            assert_eq!(p.machine, None, "the painted plan at {w} pt");
            assert_eq!(
                p.cores.is_some(),
                matches!(p.kind, FullKind::Narrow | FullKind::Slim),
                "the painted plan at {w} pt"
            );
        }
    }
    assert_eq!(kinds.len(), FullKind::ALL.len(), "{kinds:?}");
    for kind in FullKind::ALL {
        let rows = Rows::full(kind, DeckRows::Instruments);
        let main = Rows::full(kind, DeckRows::Resources);
        assert_eq!(
            (rows.cell, rows.name_pt),
            (main.cell, main.name_pt),
            "{kind:?}"
        );
    }
    let narrow = Rows {
        pitch: 10.5,
        ..Rows::full(FullKind::Narrow, DeckRows::Instruments)
    };
    assert_eq!(height(&narrow), 121.0, "CONTROL");
}

/// A4 §11's rows move no column: planned with no units facts, the instruments' rows lay the
/// columns the deck paints today, at every whole and half width from 160 to 3200 pt on either
/// face, Folded and Full; only the rows, the dial's centre, Full's Cores row beside the
/// gauge's slot and the Machine row differ. The name column measures the Heat and Latency
/// rows' names only for the instruments' rows ([`DeckRows::row_names`]), and "Processes"
/// stays the widest. CONTROL: a row named "Temperatures" would widen the name column at every
/// such width and kind, so the check would find the name, the well and every column past them
/// moved.
#[test]
fn the_instruments_rows_move_no_column() {
    let painted = DeckRows::PAINTED;
    let wider = |m: &FaceMetrics, rows: &Rows| name_width(0.0, rows.name_pt, m, &["Temperatures"]);
    for m in [hn(), FaceMetrics::test_stack()] {
        for w in (320..=6400).map(|half| f64::from(half) / 2.0) {
            let (p, i) = (
                plan_folded(w, &m),
                plan_folded_with(w, &m, &UnitFacts::NONE),
            );
            let as_painted = FoldedPlan {
                gauge: i.gauge.map(|g| FoldedGauge {
                    cy: painted.gauge_cy(),
                    ..g
                }),
                blocks: i.blocks.map(|blk| BlockPlan {
                    rows: Rows::folded(blk.kind, painted),
                    machine: None,
                    ..blk
                }),
                ..i
            };
            assert_eq!(as_painted, p, "Folded at {w} pt");
            for blk in p.blocks {
                assert!(
                    wider(&m, &blk.rows) > blk.name.w,
                    "CONTROL: Folded at {w} pt"
                );
            }
            let (p, i) = (plan_full(w, &m), plan_full_with(w, &m, &UnitFacts::NONE));
            let as_painted = FullPlan {
                rows: Rows::full(i.kind, painted),
                cores: i.cores.filter(|_| painted.full_cores_row(i.kind)),
                machine: None,
                ..i
            };
            assert_eq!(as_painted, p, "Full at {w} pt");
            assert!(wider(&m, &p.rows) > p.name.w, "CONTROL: Full at {w} pt");
        }
    }
}

#[test]
fn the_deck_size_setting_reads_its_own_words() {
    for pref in DeckSizePref::ALL {
        assert_eq!(DeckSizePref::parse(Some(pref.word())), pref);
        assert_eq!(DeckSizePref::from_word(pref.word()), Some(pref));
        let loud = format!(" {} ", pref.word().to_uppercase());
        assert_eq!(DeckSizePref::parse(Some(&loud)), pref);
    }
    assert_eq!(DeckSizePref::parse(None), DeckSizePref::Auto);
    assert_eq!(DeckSizePref::parse(Some("huge")), DeckSizePref::Auto);
    assert_eq!(DeckSizePref::from_word("huge"), None);
}

/// The machines the Cores row's law sweeps (A4 §7): from 4 to 256 cores, in one unnamed
/// cluster and in one to six named clusters, split as evenly as the count allows.
fn machines() -> Vec<Vec<Cluster<'static>>> {
    const NAMES: [&[&str]; 6] = [
        &["Performance"],
        &["Performance", "Efficiency"],
        &["Super", "Performance", "Efficiency"],
        &["Super", "Performance", "Mid 2", "Efficiency"],
        &["Super", "Performance", "Mid 2", "Mid 1", "Efficiency"],
        &[
            "Prime",
            "Super",
            "Performance",
            "Mid 2",
            "Mid 1",
            "Efficiency",
        ],
    ];
    let mut machines = Vec::new();
    for n in 4..=256_usize {
        machines.push(vec![Cluster {
            count: n,
            name: None,
        }]);
        for names in NAMES {
            let k = names.len();
            machines.push(
                names
                    .iter()
                    .enumerate()
                    .map(|(i, &name)| Cluster {
                        count: n / k + usize::from(i < n % k),
                        name: Some(name),
                    })
                    .filter(|cluster| cluster.count > 0)
                    .collect(),
            );
        }
    }
    machines
}

/// Where a Cores row stands: its room, cell, ladder and caption, the queue its square forms
/// take, and the form the plan chose for each machine.
#[derive(Clone, Copy, Debug)]
struct CoresRoom {
    room: f64,
    cell: f64,
    ladder: &'static [CoresMode],
    caption_pt: f64,
    queue: f64,
}

/// What breaks A4 §7's fit for `clusters` in `at` where the plan chose `mode`: a form other
/// than the first of its ladder that fits with the queue tile, or none where one fits; squares
/// wider than the room; or a count that cuts a cluster or overflows.
fn cores_fault(
    clusters: &[Cluster<'_>],
    mode: CoresMode,
    at: CoresRoom,
    m: &FaceMetrics,
) -> Option<String> {
    let width = |mode| cores_row_width(clusters, mode, at.cell, at.caption_pt, at.queue, m);
    let want = if total_cores(clusters) == 0 {
        CoresMode::CountOnly
    } else {
        at.ladder
            .iter()
            .copied()
            .find(|&mode| width(mode) <= at.room)
            .unwrap_or(CoresMode::CountOnly)
    };
    if mode != want {
        return Some(format!("{clusters:?} in {at:?}: {mode:?}, not {want:?}"));
    }
    if mode == CoresMode::CountOnly {
        let total = count_words(clusters);
        let words = cores_count_text(clusters, at.room, at.caption_pt, m);
        let whole =
            words == total || (words.starts_with(&format!("{total}: ")) && !words.ends_with('…'));
        let fits = m.width(&words, at.caption_pt, false) <= at.room + 0.01;
        return (!(whole && fits)).then(|| format!("{clusters:?} in {at:?} says {words:?}"));
    }
    (width(mode) > at.room).then(|| format!("{clusters:?} in {at:?}: {mode:?} overflows"))
}

/// A plan that holds a Cores row: Folded's block B or a Full table.
#[derive(Clone, Copy, Debug)]
enum CoresPlan {
    Block(BlockPlan),
    Full(FullPlan),
}

impl CoresPlan {
    /// Where its row stands, with the queue `deck`'s square forms take (A4 §7: the
    /// instruments' run-queue tile and its wall; the record's row draws none).
    fn at(&self, deck: DeckRows) -> CoresRoom {
        let queue = match deck {
            DeckRows::Resources => 0.0,
            DeckRows::Instruments => CORES_QUEUE,
        };
        match self {
            Self::Block(b) => CoresRoom {
                room: b.cores.w,
                cell: b.rows.cell,
                ladder: b.kind.cores_ladder(),
                caption_pt: b.kind.cores_caption_pt(),
                queue,
            },
            Self::Full(p) => CoresRoom {
                room: p.cores.map_or(0.0, |room| room.w),
                cell: p.rows.cell,
                ladder: p.kind.cores_ladder(),
                caption_pt: CORES_CAPTION_PT,
                queue,
            },
        }
    }

    /// The form the plan chooses for `clusters`.
    fn mode(&self, clusters: &[Cluster<'_>], m: &FaceMetrics) -> CoresMode {
        match self {
            Self::Block(b) => b.cores_mode(clusters, m),
            Self::Full(p) => p.cores_mode(clusters, m).expect("a Cores row"),
        }
    }
}

/// A4 §7: the Cores row draws every core or a count, never a cluster cut, for 4 to 256 cores
/// in one to six clusters ([`machines`]), in Folded's block B and in every Full kind (the
/// instruments' rows) or narrow and slim (the record's, which the deck paints), on either
/// face. Its room steps with the width within each kind, gauge and units column, so the law
/// reads each such layout at its least and its widest room over every whole width from 160
/// to 3200 pt, and at 940, 1039 and 1440: there the plan's form is the first of its ladder
/// whose squares, with the instruments' run-queue tile and its wall, fit, else a count that
/// names every cluster or the total alone; a machine with no cores read says so. In Full, at
/// every such width, the row spans the centred table: from where its units stand to the right
/// margin less the table's offset, never past the margin. CONTROLS: a form chosen without the
/// queue tile's 6 pt overflows the instruments' row; a centred wide table's room not narrowed
/// by its offset ends that offset past the right margin.
#[test]
fn the_cores_row_draws_every_core_or_a_count() {
    type Key = (&'static str, Option<usize>, bool, bool, DeckRows);
    let machines = machines();
    let mac = this_mac();
    let (mut checked, mut unqueued, mut centred) = (0, 0, 0);
    for m in [hn(), FaceMetrics::test_stack()] {
        // Each layout's least and widest room, by the plan that laid it, and the owner's.
        let mut rooms: Vec<(Key, CoresPlan, CoresPlan)> = Vec::new();
        let mut ats: Vec<(CoresPlan, DeckRows)> = Vec::new();
        for deck in [DeckRows::PAINTED, DeckRows::Instruments] {
            for w in (160..=3200).map(f64::from) {
                let folded = folded_by(w, &m, &mac, fold_spec, deck);
                let full = full_by(w, &m, &mac, full_spec, deck);
                let mut laid = Vec::new();
                if !folded.ground_only {
                    let b = folded.blocks[1];
                    let kind = BlockKind::ALL.iter().position(|&k| k == b.kind);
                    let gauge = folded.gauge.is_some();
                    let key = ("Folded", kind, gauge, b.units.is_some(), deck);
                    laid.push((key, CoresPlan::Block(b)));
                }
                if let Some(room) = full.cores.filter(|_| !full.ground_only) {
                    // The row spans the centred table: from where its units stand (the
                    // units column, else the %, else slim's well end with no %) to the
                    // right margin less the offset, never past the margin.
                    let left = full
                        .units
                        .map(|units| units.at.x)
                        .or(full.pct.map(|pct| pct.x))
                        .unwrap_or(full.well.end());
                    let right = full.w - X0;
                    assert!(
                        (room.x - left).abs() < 1e-9
                            && (room.end() - (right - full.offset)).abs() < 1e-9
                            && room.end() <= right,
                        "{deck:?} at {w} pt, {:?}: the Cores row {room:?} spans from {left} \
                         to {right} less the offset {}",
                        full.kind,
                        full.offset,
                    );
                    if full.offset > 0.0 {
                        let unnarrowed = Col::new(room.x, room.w + 2.0 * full.offset);
                        assert!(unnarrowed.end() > right, "CONTROL at {w} pt");
                        centred += 1;
                    }
                    let kind = FullKind::ALL.iter().position(|&k| k == full.kind);
                    let key = (
                        "Full",
                        kind,
                        full.gauge.is_some(),
                        full.units.is_some(),
                        deck,
                    );
                    laid.push((key, CoresPlan::Full(full)));
                }
                for (key, plan) in laid {
                    if [940.0, 1039.0, 1440.0].contains(&w) {
                        ats.push((plan, deck));
                    }
                    let room = plan.at(deck).room;
                    match rooms.iter_mut().find(|(k, _, _)| *k == key) {
                        Some((_, least, widest)) => {
                            if room < least.at(deck).room {
                                *least = plan;
                            }
                            if room > widest.at(deck).room {
                                *widest = plan;
                            }
                        }
                        None => rooms.push((key, plan, plan)),
                    }
                }
            }
        }
        ats.extend(
            rooms
                .iter()
                .flat_map(|&(key, least, widest)| [(least, key.4), (widest, key.4)]),
        );
        // One plan for each room it lays: the row reads nothing else of it.
        let mut distinct: Vec<(CoresPlan, DeckRows)> = Vec::new();
        for (plan, deck) in ats {
            let at = plan.at(deck);
            let same = |(p, d): &(CoresPlan, DeckRows)| {
                let b = p.at(*d);
                (b.room, b.cell, b.ladder, b.caption_pt, b.queue)
                    == (at.room, at.cell, at.ladder, at.caption_pt, at.queue)
            };
            if !distinct.iter().any(same) {
                distinct.push((plan, deck));
            }
        }
        let ats = distinct;
        for clusters in &machines {
            for &(plan, deck) in &ats {
                let at = plan.at(deck);
                if let Some(fault) = cores_fault(clusters, plan.mode(clusters, &m), at, &m) {
                    panic!("{fault}");
                }
                checked += 1;
                if unqueued > 0 || deck != DeckRows::Instruments {
                    continue;
                }
                let blind = cores_row_mode(
                    clusters,
                    at.room,
                    at.cell,
                    at.ladder,
                    at.caption_pt,
                    0.0,
                    &m,
                );
                if cores_fault(clusters, blind, at, &m).is_some() {
                    unqueued += 1;
                }
            }
        }
        let unread = [Cluster {
            count: 0,
            name: None,
        }];
        for clusters in [&[][..], &unread] {
            for &(plan, deck) in &ats {
                assert_eq!(plan.mode(clusters, &m), CoresMode::CountOnly);
                let at = plan.at(deck);
                let words = cores_count_text(clusters, at.room, at.caption_pt, &m);
                assert!(words.ends_with("not read"), "the row says {words:?}");
            }
        }
    }
    assert!(checked > 0);
    assert!(
        unqueued > 0,
        "CONTROL: a form chosen without the queue tile fits"
    );
    assert!(centred > 0, "CONTROL: no centred table holds a Cores row");
}

#[test]
fn an_m5_draws_its_cores_large_where_it_can() {
    let m = hn();
    let m5 = [
        Cluster {
            count: 6,
            name: Some("Super"),
        },
        Cluster {
            count: 12,
            name: Some("Performance"),
        },
    ];
    assert_eq!(
        plan_full(1119.0, &m).cores_mode(&m5, &m),
        None,
        "the gauge's slot"
    );
    assert_eq!(
        plan_full(400.0, &m).cores_mode(&m5, &m),
        Some(CoresMode::Stack)
    );
    assert_eq!(
        plan_folded(1119.0, &m).blocks[1].cores_mode(&m5, &m),
        CoresMode::Big
    );
    // The instruments' Full rows hold the Cores row at every kind, beside the gauge's slot in
    // wide and mid, and the queue tile it takes past the squares (A4 §7).
    let mac = this_mac();
    assert_eq!(
        plan_full_with(1119.0, &m, &mac).cores_mode(&m5, &m),
        Some(CoresMode::Big)
    );
    assert_eq!(
        plan_folded_with(1119.0, &m, &mac).blocks[1].cores_mode(&m5, &m),
        CoresMode::Big
    );
    assert_eq!(
        cores_count_text(&m5, 400.0, CORES_CAPTION_PT, &m),
        "18 cores: 6 Super, 12 Performance"
    );
    assert_eq!(
        cores_count_text(&m5, 60.0, CORES_CAPTION_PT, &m),
        "18 cores"
    );
}

fn placed(head: &HeaderPlan, item: KeyItem) -> Option<f64> {
    head.key
        .placed
        .iter()
        .find(|&&(i, _)| i == item)
        .map(|&(_, x)| x)
}

fn label_x(head: &HeaderPlan, label: Label) -> Option<f64> {
    head.labels.iter().find(|l| l.label == label).map(|l| l.x)
}

/// The header's scope names, each after its swatch: its words and where it ends.
fn heads(head: &HeaderPlan, m: &FaceMetrics) -> Vec<(String, f64)> {
    head.labels
        .iter()
        .filter_map(|l| match l.label {
            Label::Scope(_) => {
                let end = l.x + SWATCH_ADVANCE + m.width(&l.text, HEADER_PT, false);
                Some((l.text.clone(), end))
            }
            _ => None,
        })
        .collect()
}

/// The legend over the bars: `names` in order, each after its swatch, the first from `x`
/// and each next [`LEGEND_GAP`] past the one before.
fn assert_legend(head: &HeaderPlan, m: &FaceMetrics, x: f64, names: &[&str]) {
    let got = heads(head, m);
    let words: Vec<&str> = got.iter().map(|h| h.0.as_str()).collect();
    assert_eq!(words, names, "{:?}", head.labels);
    let mut at = x;
    for (l, (_, end)) in head
        .labels
        .iter()
        .filter(|l| matches!(l.label, Label::Scope(_)))
        .zip(&got)
    {
        assert!((l.x - at).abs() < 1e-9, "{l:?} starts at {}, not {at}", l.x);
        at = end + LEGEND_GAP;
    }
}

#[test]
fn the_key_goes_where_the_spec_says() {
    let m = hn();

    // The time strip's header carries the time label, the scale's ticks (the live edge's
    // design) and the key between them; what the ticks leave no room for goes, by weight,
    // to the next free span.
    let plan = plan_full(1119.0, &m);
    let [label, _] = time_words(plan.strip);
    assert_eq!(label, "6h ago", "a Full wide strip spans six hours");
    let head = header_full(&plan, TEXT, None, &m);
    let ticks: Vec<u32> = head.ruler.iter().map(|t| t.age_s).collect();
    assert!(
        ticks.contains(&3600) && ticks.contains(&600),
        "Full wide marks multi-scale horizon ticks: {ticks:?}"
    );
    assert!(placed(&head, KeyItem::Over).is_some());
    assert_eq!(
        (
            placed(&head, KeyItem::Ramp),
            placed(&head, KeyItem::NotRead)
        ),
        (None, None),
        "wide keys only the over mark"
    );
    // The legend once over the bars, from the bar's start, its swatches in the bar's inks;
    // the over mark's key over the strip, clear of it.
    let bar = plan.meter.bar.expect("wide's bar");
    let names = ["this Mac", "aterm", "Tab 2"];
    assert_legend(&head, &m, bar.x, &names);
    let over = placed(&head, KeyItem::Over).expect("the over mark's key");
    assert!(
        over + KeyItem::Over.width(&m) < bar.x,
        "the over key {over} before the legend"
    );
    assert_eq!(
        label_x(&head, Label::Now),
        Some(plan.strip.x - WORDS_TO_STRIP),
        "\"now\" beside the live edge's dot, clear of its beat"
    );
    assert_eq!(label_x(&head, Label::Details), None);
    // With no over mark on screen the key is empty, and the scale keeps what it had.
    let calm = header_full(
        &plan,
        HeaderText {
            over: [false; 2],
            ..TEXT
        },
        None,
        &m,
    );
    assert!(calm.key.placed.is_empty(), "{:?}", calm.key.placed);
    assert!(calm.ruler.len() >= head.ruler.len());

    // Narrow keys all three marks by rule K, the over mark over the names; the legend
    // stands over the bar, which is as long as it, from the bar's start.
    let plan = plan_full(476.0, &m);
    assert_eq!(plan.kind, FullKind::Narrow);
    let head = header_full(&plan, TEXT, None, &m);
    assert_eq!(placed(&head, KeyItem::Over), Some(plan.name.x));
    assert!(placed(&head, KeyItem::Ramp).is_some(), "{:?}", head.key);
    let bar = plan.meter.bar.expect("narrow's bar");
    let said = heads(&head, &m);
    let names: Vec<&str> = said.iter().map(|h| h.0.as_str()).collect();
    assert_eq!(names, ["Mac", "aterm", "Tab"], "{:?}", head.labels);
    assert_legend(&head, &m, bar.x, &names);
    assert!(said[2].1 <= bar.end() + 1e-9, "{said:?} past {bar:?}");
    // Where the bars stand under each %, the legend stands over the strip's header at its
    // left, past the now marker's room, and the scale keeps its label and a tick.
    let plan = plan_full(350.0, &m);
    assert!(plan.meter.bar.is_none() && plan.bar_under().is_some());
    let head = header_full(&plan, TEXT, None, &m);
    let said = heads(&head, &m);
    let names: Vec<&str> = said.iter().map(|h| h.0.as_str()).collect();
    assert_eq!(names.len(), 3, "{:?}", head.labels);
    assert_legend(&head, &m, plan.strip.x + MARKER_ROOM, &names);
    assert!(
        !head.ruler.is_empty(),
        "the scale keeps a tick: {:?}",
        head.labels
    );
    assert!(
        head.ruler
            .iter()
            .all(|t| t.left >= said[2].1 + KEY_GAP - 1e-9),
        "the ticks stand past the legend: {:?}",
        head.ruler
    );
    assert_eq!(
        label_x(&head, Label::Time).map(|x| x > said[2].1),
        Some(true)
    );

    // With marks in both blocks, Folded keys "over max" over block B's strip past its now
    // marker (block A's carries the time label, right-aligned over its old end, and the
    // scale). A single marked block is covered below.
    let plan = plan_folded(1119.0, &m);
    let head = header_folded(&plan, TEXT, [None, None], &m);
    let [a, b] = plan.blocks;
    assert_eq!(
        label_x(&head, Label::Time),
        Some(a.strip.end() - m.width("6h ago", HEADER_PT, false))
    );
    assert_eq!(
        head.labels
            .iter()
            .find(|l| l.label == Label::Time)
            .map(|l| l.text.as_str()),
        Some("6h ago")
    );
    assert!(!head.ruler.is_empty(), "Folded wide places X-axis ticks");
    assert_eq!(
        head.labels
            .iter()
            .filter(|l| l.label == Label::Now)
            .map(|l| l.x)
            .collect::<Vec<_>>(),
        [a.strip, b.strip].map(|strip| strip.x - WORDS_TO_STRIP),
        "\"now\" over each %, right-aligned clear of the dot's beat"
    );
    assert_eq!(placed(&head, KeyItem::Over), Some(b.strip.x + MARKER_ROOM));
    assert_eq!(
        (
            placed(&head, KeyItem::Ramp),
            placed(&head, KeyItem::NotRead)
        ),
        (None, None)
    );
    let calm = HeaderText {
        over: [false; 2],
        ..TEXT
    };
    assert!(
        header_folded(&plan, calm, [None, None], &m)
            .key
            .placed
            .is_empty()
    );
    // The legend once, at the deck's top right: ending at block B's bar's end, over its
    // bars and its strip's old end, clear of the over mark's key and never across the
    // blocks' divider; with the machine's totals where they fit, as no run says the rows'
    // maxima.
    let whole = |global: &str| {
        3.0 * SWATCH_ADVANCE
            + m.width(global, HEADER_PT, false)
            + m.width("aterm", HEADER_PT, false)
            + m.width("Tab 2", HEADER_PT, false)
            + 2.0 * LEGEND_GAP
    };
    let b_end = b.meter.bar.map(Col::end).expect("fwide's bar");
    assert_legend(&head, &m, b_end - whole("this Mac"), &names);
    let over = placed(&head, KeyItem::Over).expect("the over mark's key");
    assert!(over + KeyItem::Over.width(&m) + KEY_GAP <= b_end - whole("this Mac") + 1e-9);
    assert_eq!(label_x(&head, Label::Details), None);
    let totals = said_totals(&[
        "18 cores \u{b7} 128 GiB \u{b7} 2 TB",
        "18c \u{b7} 128G \u{b7} 2T",
    ]);
    for w in [1039.0, 940.0, 750.0] {
        let plan = plan_folded(w, &m);
        let [_, b] = plan.blocks;
        let b_end = b.meter.bar.map(Col::end).expect("the bar's column");
        let head = header_folded(&plan, TEXT, [None, None], &m);
        assert_legend(&head, &m, b_end - whole("this Mac"), &names);
        let with = HeaderText {
            totals: &totals,
            ..TEXT
        };
        let head = header_folded(&plan, with, [None, None], &m);
        let said = heads(&head, &m);
        let x0 = head
            .labels
            .iter()
            .find(|l| l.label == Label::Scope(Bar::Global))
            .map(|l| l.x)
            .expect("the legend");
        assert!(x0 >= b.strip.x + MARKER_ROOM, "W {w}: {:?}", head.labels);
        assert!((said[2].1 - b_end).abs() < 1e-9, "W {w}: {said:?}");
        assert!(said[0].0.starts_with("this Mac \u{b7} "), "W {w}: {said:?}");
    }
}

/// An fslim block's state takes the header: over its names, and where it needs more room
/// over the % column's too, short of the widest ring the now marker's dot beats (the ring
/// once drew over words that ended at the dot, review of 2026-10-26). Words too long even for
/// that take the strip's header as well, whole: the marker's dot and `now` yield to them (one
/// block at most), and the time scale stands over the other block's strip.
#[test]
fn an_fslim_state_takes_the_header() {
    let m = hn();
    let plan = plan_folded(450.0, &m);
    assert!(plan.blocks.iter().all(|b| b.kind == BlockKind::Slim));
    let [a, b] = plan.blocks;
    let (a_pct, b_pct) = (a.pct.expect("fslim's %"), b.pct.expect("fslim's %"));
    let short = StateLabel {
        full: "Disk full",
        bare: "Disk full",
    };
    let mid = StateLabel {
        full: "Memory warn +1",
        bare: "Memory warn",
    };
    let long = StateLabel {
        full: "Disk full ~34 min +1",
        bare: "Disk full ~34 min",
    };
    let nows = |head: &HeaderPlan| -> Vec<f64> {
        head.labels
            .iter()
            .filter(|l| l.label == Label::Now)
            .map(|l| l.x)
            .collect()
    };
    let state_of = |head: &HeaderPlan, x: f64| -> String {
        head.labels
            .iter()
            .find(|l| matches!(l.label, Label::State(_)) && l.x == x)
            .map(|l| l.text.clone())
            .expect("the block names its state")
    };
    let end_of = |x: f64, words: &str| x + MARK_ADVANCE + m.width(words, HEADER_PT, true);
    let ring = |strip: Strip| {
        use crate::stats_live::{BEAT_REACH, DOT_R, LINE_DX};
        strip.x - LINE_DX - DOT_R - BEAT_REACH
    };
    // "now" ends over its % column, a point short of the dot's widest ring.
    let now_end = |strip: Strip| strip.x - WORDS_TO_STRIP;
    for (strip, pct) in [(a.strip, a_pct), (b.strip, b_pct)] {
        let now_x = now_end(strip) - m.width("now", HEADER_PT, false);
        assert!(now_end(strip) <= ring(strip) - 1.0 + 1e-9 && now_x >= pct.x);
    }
    // A state that fits over the names leaves "now" over the % and every dot.
    let head = header_folded(&plan, TEXT, [Some(short), None], &m);
    assert_eq!(state_of(&head, a.name.x), "Disk full");
    assert_eq!(nows(&head), [now_end(a.strip), now_end(b.strip)]);
    assert!(time_on(&head, a.strip, &m), "{:?}", head.labels);
    assert_eq!(head.dotless, None);

    // One that needs the %'s header too takes it, ending where "now" would, short of the
    // dot's ring; "now" yields.
    let head = header_folded(&plan, TEXT, [Some(mid), None], &m);
    let words = state_of(&head, a.name.x);
    assert!(words.starts_with("Memory warn"), "{words:?}");
    let end = end_of(a.name.x, &words);
    assert!(
        end <= now_end(a.strip) + 1e-9 && end <= ring(a.strip) - 1.0 + 1e-9,
        "{words:?} ends at {end}"
    );
    assert!(
        end > now_end(a.strip) - m.width("now", HEADER_PT, false) - STRIP_LEAD,
        "CONTROL: {words:?} needed the %'s header"
    );
    assert_eq!(nows(&head), [now_end(b.strip)], "\"now\" yields over A");
    assert_eq!(head.dotless, None, "the dot stays");

    // A long one takes block A's strip's header too and is said whole; A's dot yields to it,
    // and the time scale stands over B's strip.
    let head = header_folded(&plan, TEXT, [Some(long), None], &m);
    let words = state_of(&head, a.name.x);
    assert_eq!(words, "Disk full ~34 min +1");
    assert!(
        end_of(a.name.x, &words) > now_end(a.strip),
        "CONTROL: it needed the strip"
    );
    assert!(end_of(a.name.x, &words) <= a.strip.end() + 1e-9);
    assert_eq!(head.dotless, Some(a.strip.x));
    assert_eq!(nows(&head), [now_end(b.strip)]);
    assert!(time_on(&head, b.strip, &m), "{:?}", head.labels);

    // Only one block spills: B's long state is fitted short of its dot.
    let head = header_folded(&plan, TEXT, [Some(long), Some(long)], &m);
    assert_eq!(state_of(&head, a.name.x), "Disk full ~34 min +1");
    let words = state_of(&head, b.name.x);
    let end = end_of(b.name.x, &words);
    assert!(words.ends_with('…'), "{words:?}");
    assert!(end <= now_end(b.strip) + 1e-9, "{words:?}");
    assert_eq!(head.dotless, Some(a.strip.x));
    assert!(nows(&head).is_empty(), "{:?}", head.labels);
    assert!(time_on(&head, b.strip, &m), "{:?}", head.labels);
    // B alone spilling keeps the scale over A.
    let head = header_folded(&plan, TEXT, [None, Some(long)], &m);
    assert_eq!(head.dotless, Some(b.strip.x));
    assert!(time_on(&head, a.strip, &m), "{:?}", head.labels);
}

/// Whether `head`'s time label stands on `strip`: over its right (old) end, right-aligned to
/// it, or beside it, starting [`STRIP_LEAD`] past that end with a tick marking it.
fn time_on(head: &HeaderPlan, strip: Strip, m: &FaceMetrics) -> bool {
    head.labels.iter().any(|l| {
        let w = m.width(&l.text, HEADER_PT, false);
        l.label == Label::Time
            && ((l.x + w - strip.end()).abs() < 1e-9 && head.time_tick.is_none()
                || (l.x - (strip.end() + STRIP_LEAD)).abs() < 1e-9
                    && head.time_tick == Some(strip.end()))
    })
}

/// Every half-point width from 160 to 5120 pt on both faces, on every core: each whole width,
/// and the half point past it where it plans otherwise (the plans read whole points,
/// [`whole_width`]). A Folded plan that differs from the width before it only in `w` is not
/// headed again, since `header_folded` reads only the blocks and `ground_only`.
#[test]
fn key_is_placed() {
    let long = StateLabel {
        full: "Disk emergency, out of file slots +3",
        bare: "Disk emergency, out of file slots",
    };
    let short = StateLabel {
        full: "Memory warn",
        bare: "Memory warn",
    };
    let check = |head: &HeaderPlan, m: &FaceMetrics, exempt: bool, what: &str| {
        let fits = head
            .key
            .spans
            .iter()
            .any(|s| s.x1 - s.x0 >= KeyItem::Over.width(m));
        assert_eq!(head.key.has(KeyItem::Over), fits, "{what}");
        assert!(exempt || fits, "{what}: no span holds over max");
        for &(item, x) in &head.key.placed {
            let inside = |s: &KeySpan| x >= s.x0 && x + item.width(m) <= s.x1;
            assert!(head.key.spans.iter().any(inside), "{what}: {item:?} at {x}");
        }
        let times = head
            .labels
            .iter()
            .filter(|l| l.label == Label::Time)
            .count();
        assert_eq!(times, 1, "{what}: one time label");
    };
    let faces = [hn(), FaceMetrics::test_stack()];
    let widths: Vec<(usize, u32)> = (0..faces.len())
        .flat_map(|face| (160..=5_120).map(move |w| (face, w)))
        .collect();
    // `header_folded` reads only the blocks and `ground_only`: plans equal but for `w` head
    // alike.
    let sans_w = |p: FoldedPlan| FoldedPlan { w: 0.0, ..p };
    aterm_spec::check::each(&widths, |&(face, w)| {
        let m = &faces[face];
        let w = f64::from(w);
        // Each whole width, and the half point past it where that plans otherwise.
        let (full, past) = (plan_full(w, m), plan_full(w + 0.5, m));
        let fulls = if past == full {
            vec![(full, w)]
        } else {
            vec![(full, w), (past, w + 0.5)]
        };
        for (full, w) in fulls.into_iter().filter(|(p, _)| !p.ground_only) {
            check(
                &header_full(&full, TEXT, None, m),
                m,
                false,
                &format!("Full W {w}"),
            );
            // A state over the names may take the key's room there.
            let said = full.details.is_none();
            check(
                &header_full(&full, TEXT, Some(long), m),
                m,
                said,
                &format!("Full W {w}, a state"),
            );
        }
        // A Folded plan equal but for `w` to the width before it was headed there.
        let (folded, past) = (plan_folded(w, m), plan_folded(w + 0.5, m));
        let mut folds = Vec::new();
        if w == 160.0 || sans_w(plan_folded(w - 1.0, m)) != sans_w(folded) {
            folds.push((folded, w));
        }
        if sans_w(past) != sans_w(folded) {
            folds.push((past, w + 0.5));
        }
        let states: [[Option<StateLabel<'_>>; 2]; 5] = [
            [None, None],
            [Some(long), None],
            [None, Some(short)],
            [Some(short), Some(long)],
            [Some(long), Some(long)],
        ];
        for (folded, w) in folds.into_iter().filter(|(p, _)| !p.ground_only) {
            for pair in states {
                let both = pair.iter().all(Option::is_some);
                let head = header_folded(&folded, TEXT, pair, m);
                check(&head, m, both, &format!("Folded W {w}"));
            }
        }
    });
}

/// Mark preference never makes an unmarked strip into a scope-legend span. State words
/// may occupy a block's names or strip header; the key still uses the free room it has.
#[test]
fn folded_over_keys_follow_the_marked_blocks_with_occupied_headers() {
    let state = StateLabel {
        full: "Disk emergency, out of file slots +3",
        bare: "Disk emergency, out of file slots",
    };
    for m in [hn(), FaceMetrics::test_stack()] {
        for w in [1119.0, 1039.0, 940.0, 750.0, 550.0, 350.0] {
            let plan = plan_folded(w, &m);
            for over in [[true, false], [false, true], [true; 2], [false; 2]] {
                for states in [
                    [None, None],
                    [Some(state), None],
                    [None, Some(state)],
                    [Some(state), Some(state)],
                ] {
                    let head = header_folded(&plan, HeaderText { over, ..TEXT }, states, &m);
                    let at = format!("W {w}, marks {over:?}, states {states:?}");
                    let width = KeyItem::Over.width(&m);
                    let x = placed(&head, KeyItem::Over);
                    let fits = head.key.spans.iter().any(|s| s.x1 - s.x0 >= width);
                    assert_eq!(x.is_some(), over.contains(&true) && fits, "{at}");
                    for span in &head.key.spans {
                        assert!(!span.over || span.strip, "{at}: {span:?}");
                        if span.strip {
                            let bi = usize::from(span.x0 >= plan.divider);
                            assert_eq!(span.over, over[bi], "{at}: {span:?}");
                        }
                    }
                    let marked_room = head
                        .key
                        .spans
                        .iter()
                        .any(|s| s.over && s.x1 - s.x0 >= width);
                    if let Some(x) = x {
                        assert!(
                            head.key.spans.iter().any(|s| {
                                x >= s.x0 && x + width <= s.x1 && (!marked_room || s.over)
                            }),
                            "{at}: over max at {x}"
                        );
                    }
                }
            }
        }
    }
}

const GIB: u64 = 1 << 30;

/// This Mac's heat chain (A4 §4): the pressure ladder, its Super and Efficiency clocks, the
/// CPU's, GPU's and Neural Engine's watts and the work per joule.
const THIS_MACS_HEAT: Heat = Heat {
    ladder: true,
    clocks: 2,
    watts: 3,
    work: true,
};

/// This Mac (A1 §1.3, A4 §4, §5): a 10-core GPU and its Neural Engine, 32 GiB, a disk
/// reader, the whole heat chain, and the network and processes readers.
fn this_mac() -> UnitFacts {
    UnitFacts {
        gpu: Some(Gpu {
            cores: Some(10),
            neural_engine: true,
        }),
        memory: Some(32 * GIB),
        disk: true,
        heat: Some(THIS_MACS_HEAT),
        network: true,
        processes: true,
    }
}

/// The machines the planner's units laws sweep: this Mac with its Neural Engine and without
/// it, a Max with a 40-core GPU, an Ultra with 80 GPU cores, a GPU with no core count and one
/// watt tile, an Intel Mac whose GPU reads 0 cores and whose CPU is of one kind, a Linux box
/// with no GPU reading whose heat is its ladder alone, a Windows box with no heat, disk,
/// network or processes reader, and machines that read one row alone.
fn unit_machines() -> Vec<UnitFacts> {
    let mac = this_mac();
    let gpu = |cores, neural_engine| {
        Some(Gpu {
            cores,
            neural_engine,
        })
    };
    let ladder = Some(Heat {
        ladder: true,
        clocks: 0,
        watts: 0,
        work: false,
    });
    vec![
        mac,
        UnitFacts {
            gpu: gpu(Some(10), false),
            heat: Some(Heat {
                watts: 2,
                ..THIS_MACS_HEAT
            }),
            ..mac
        },
        UnitFacts {
            gpu: gpu(Some(40), true),
            memory: Some(64 * GIB),
            ..mac
        },
        UnitFacts {
            gpu: gpu(Some(80), true),
            memory: Some(192 * GIB),
            ..mac
        },
        UnitFacts {
            gpu: gpu(None, false),
            memory: Some(8 * GIB),
            heat: Some(Heat {
                watts: 1,
                ..THIS_MACS_HEAT
            }),
            ..mac
        },
        UnitFacts {
            gpu: gpu(Some(0), false),
            memory: Some(16 * GIB),
            heat: Some(Heat {
                clocks: 1,
                watts: 2,
                ..THIS_MACS_HEAT
            }),
            ..mac
        },
        UnitFacts {
            gpu: None,
            memory: Some(512 * GIB),
            heat: ladder,
            ..mac
        },
        UnitFacts {
            gpu: gpu(None, false),
            memory: Some(16 * GIB),
            ..UnitFacts::NONE
        },
        UnitFacts {
            disk: true,
            ..UnitFacts::NONE
        },
        UnitFacts {
            gpu: gpu(None, true),
            ..UnitFacts::NONE
        },
        UnitFacts {
            heat: ladder,
            ..UnitFacts::NONE
        },
        UnitFacts {
            processes: true,
            ..UnitFacts::NONE
        },
    ]
}

/// How far down its row's ladder `form` stands: 0 for the widest, the ladder's length for none.
fn rung(row: UnitRow, form: Option<Form>) -> usize {
    form.and_then(|f| row.ladder().iter().position(|&g| g == f))
        .unwrap_or(row.ladder().len())
}

/// Each units row's form across a plan's units columns: Folded's blocks hold different rows.
fn forms_of(cols: &[Option<UnitsCol>]) -> Forms {
    let mut forms = [None; UnitRow::ALL.len()];
    for units in cols.iter().flatten() {
        for row in UnitRow::ALL {
            forms[row.index()] = forms[row.index()].or(units.form(row));
        }
    }
    forms
}

/// Each units row's [`rung`] for `forms`.
fn rungs(forms: Forms) -> [usize; UnitRow::ALL.len()] {
    std::array::from_fn(|i| rung(UnitRow::ALL[i], forms[i]))
}

/// One plan as A1 §2.4's monotone law reads it: its width, each units row's [`rung`], and
/// for each table (Full's one, Folded's two blocks) its strip's horizon and its kind's place
/// in its form's kinds, widest first; then what a wider deck keeps once it lays it (A1 step
/// R5): a table at all, the gauge, and each table's % and bar column.
#[derive(Clone, Debug)]
struct Widening {
    w: f64,
    rungs: [usize; UnitRow::ALL.len()],
    horizons: Vec<u32>,
    kinds: Vec<usize>,
    kept: Vec<(&'static str, bool)>,
}

fn folded_widening(p: &FoldedPlan) -> Widening {
    let [a, b] = p.blocks;
    let kind = |blk: BlockPlan| BlockKind::ALL.iter().position(|&k| k == blk.kind);
    Widening {
        w: p.w,
        rungs: rungs(forms_of(&[a.units, b.units])),
        horizons: vec![a.strip.span_s(), b.strip.span_s()],
        kinds: [a, b].map(|blk| kind(blk).expect("a kind")).to_vec(),
        kept: vec![
            ("a table", !p.ground_only),
            ("the gauge", p.gauge.is_some()),
            ("block A's %", a.pct.is_some()),
            ("block B's %", b.pct.is_some()),
            ("block A's bar column", a.meter.bar.is_some()),
            ("block B's bar column", b.meter.bar.is_some()),
        ],
    }
}

fn full_widening(p: &FullPlan) -> Widening {
    let kind = FullKind::ALL.iter().position(|&k| k == p.kind);
    Widening {
        w: p.w,
        rungs: rungs(forms_of(&[p.units])),
        horizons: vec![p.strip.span_s()],
        kinds: vec![kind.expect("a kind")],
        kept: vec![
            ("a table", !p.ground_only),
            ("the %", p.pct.is_some()),
            ("the bar column", p.meter.bar.is_some()),
        ],
    }
}

/// What breaks A1 §2.4's monotone law in `seq`, plans in widening order: a row whose form
/// steps down its ladder, a table whose strip's horizon shortens or whose kind narrows, or
/// something a table laid that goes.
fn widening_faults(seq: &[Widening]) -> Vec<String> {
    seq.windows(2)
        .filter_map(|pair| {
            let [was, now] = pair else {
                return None;
            };
            let rows = (0..UnitRow::ALL.len())
                .filter(|&i| now.rungs[i] > was.rungs[i])
                .map(|i| format!("{:?} {}→{}", UnitRow::ALL[i], was.rungs[i], now.rungs[i]));
            let pairs = |a: &[u32], b: &[u32]| -> Vec<(usize, u32, u32)> {
                a.iter()
                    .zip(b)
                    .enumerate()
                    .map(|(i, (&x, &y))| (i, x, y))
                    .collect()
            };
            let horizons = pairs(&was.horizons, &now.horizons)
                .into_iter()
                .filter(|&(_, h0, h1)| h1 < h0)
                .map(|(i, h0, h1)| format!("table {i} {h0}→{h1} s"));
            let kinds = was
                .kinds
                .iter()
                .zip(&now.kinds)
                .enumerate()
                .filter(|(_, (k0, k1))| k1 > k0)
                .map(|(i, (k0, k1))| format!("table {i} kind {k0}→{k1}"));
            let kept = was
                .kept
                .iter()
                .zip(&now.kept)
                .filter(|((_, had), (_, has))| *had && !*has)
                .map(|((what, _), _)| format!("{what} goes"));
            let all: Vec<String> = rows.chain(horizons).chain(kinds).chain(kept).collect();
            (!all.is_empty()).then(|| format!("{}→{} pt: {}", was.w, now.w, all.join(", ")))
        })
        .collect()
}

/// Main's meter measures before A1's step R5, for the monotone law's control: each kind's
/// strip least its own (Folded mid 79 pt, narrow and slim 59), the bar under each % beside
/// the same least, and a % column beside any strip.
fn mains_fold_spec(kind: BlockKind) -> MeterSpec {
    let spec = fold_spec(kind);
    let least = match kind {
        BlockKind::Wide => FOUR_HOURS,
        BlockKind::Mid => SIX_MINUTES,
        BlockKind::Narrow | BlockKind::Slim => 59.0,
    };
    MeterSpec {
        strip: (least, spec.strip.1),
        under: spec.under.map(|_| least),
        pct_strip: 0.0,
        ..spec
    }
}

/// Main's Full meter measures before A1's step R5: narrow's strip from 99 pt and slim's from
/// 79, beside a bar column or the bar under each %, and a % column beside any strip.
fn mains_full_spec(kind: FullKind) -> MeterSpec {
    let spec = full_spec(kind);
    let least = match kind {
        FullKind::Wide | FullKind::Mid => SIX_HOURS,
        FullKind::Narrow => 99.0,
        FullKind::Slim => SIX_MINUTES,
    };
    MeterSpec {
        strip: (least, spec.strip.1),
        under: spec.under.map(|_| least),
        pct_strip: 0.0,
        ..spec
    }
}

/// A1 §2.2 and §2.3's points for this Mac, and A4's (§5, §7, §8, §11): at 1039 pt block A
/// lays its units 148–194 (U 46 within a U_cap of 54), the % 197–227, a six-hour strip of 49
/// boxes 234–478 and the bar 485–541; block B its units 634–649 (U_B 15 within 17, its U_cap
/// 54), the % 652–682, a six-hour strip of 54 boxes 689–958 and the bar 965–1027, its Cores
/// row's room 634–1027, and its Machine row's name in 570–637 and its glyphs 640–682, ending
/// with the %. At 1440 pt block A lays the same units and % (U_cap 254: no form grows past
/// Column, ruling 17), a strip of 81 boxes 234–638 and the bar 645–741; block B a strip of 86
/// boxes 889–1318 and the bar 1325–1427 (b 657, so 17 caps its column), and the same
/// Machine row. The dial keeps r 27 at 36.75 pt, the band's middle. Every edge stands on
/// whole device px at 1× and 2×. The plan the deck paints is main's: no units, the record's
/// rows, block A's % at 148 and its strip of 57 boxes at 185, the dial at 31 pt.
#[test]
fn folded_1039_and_1440_plan_units_percent_strip_and_bar_at_their_points() {
    use UnitRow::{Disk, Memory, Network, Processes};
    let m = hn();
    let mac = this_mac();
    let block_a = [UnitRow::Gpu, Memory, Disk, UnitRow::Heat];
    let forms = |rows: &[UnitRow]| {
        let mut forms = [None; UnitRow::ALL.len()];
        for &row in rows {
            forms[row.index()] = Some(Form::Column);
        }
        forms
    };
    for (w, x_b, cap, cols, strip_end, bar, b_cols, b_bar) in [
        (
            1039.0,
            570.0,
            54.0,
            49,
            478.0,
            Col::new(485.0, 56.0),
            54,
            Col::new(965.0, 62.0),
        ),
        (
            1440.0,
            770.0,
            254.0,
            81,
            638.0,
            Col::new(645.0, 96.0),
            86,
            Col::new(1325.0, 102.0),
        ),
    ] {
        let p = plan_folded_with(w, &m, &mac);
        assert_eq!(
            plan_folded_with((2.0 * w + 1.0) / 2.0, &m, &mac),
            p,
            "W {w}"
        );
        assert_eq!(
            p.gauge,
            Some(FoldedGauge {
                at: Col::new(12.0, 54.0),
                divider: 75.0,
                cy: 36.75
            }),
            "W {w}"
        );
        let [a, b] = p.blocks;
        assert_eq!(
            a.units,
            Some(UnitsCol {
                at: Col::new(148.0, 46.0),
                cap,
                forms: forms(&block_a),
            }),
            "W {w}"
        );
        assert_eq!(
            (a.kind, a.name, a.well, a.pct),
            (
                BlockKind::Wide,
                Col::new(84.0, 52.0),
                Col::new(136.0, 10.0),
                Some(Col::new(197.0, 30.0))
            ),
            "W {w}"
        );
        assert_eq!(
            (a.strip.x, a.strip.cols, a.strip.end(), a.strip.span_s()),
            (234.0, cols, strip_end, 21_600),
            "W {w}"
        );
        assert_eq!(
            (a.meter.bar, a.meter.run, a.details),
            (Some(bar), None, None)
        );
        assert_eq!(bar.end(), a.at.end(), "W {w}: the bar ends the block");

        assert_eq!(
            b.units,
            Some(UnitsCol {
                at: Col::new(x_b + 64.0, 15.0),
                cap: 17.0,
                forms: forms(&[Network, Processes]),
            }),
            "W {w}"
        );
        assert_eq!(
            (b.kind, b.name, b.well, b.pct),
            (
                BlockKind::Wide,
                Col::new(x_b, 52.0),
                Col::new(x_b + 52.0, 10.0),
                Some(Col::new(x_b + 82.0, 30.0))
            ),
            "W {w}"
        );
        assert_eq!(
            (b.strip.x, b.strip.cols, b.strip.span_s()),
            (x_b + 119.0, b_cols, 21_600),
            "W {w}"
        );
        assert_eq!(
            (b.meter.bar, b.meter.run, b.details),
            (Some(b_bar), None, None),
            "W {w}"
        );
        assert_eq!(b_bar.end(), b.at.end(), "W {w}: the bar ends the block");
        assert_eq!(b.cores, Col::new(x_b + 64.0, b.at.end() - x_b - 64.0));
        assert_eq!(
            b.machine,
            Some(MachineRow {
                name: Col::new(x_b, 67.0),
                glyphs: Col::new(x_b + 70.0, 42.0),
            }),
            "W {w}"
        );
        for blk in [a, b] {
            assert_eq!(
                blk.rows,
                Rows::folded(BlockKind::Wide, DeckRows::Instruments)
            );
            assert_on_pixels(&block_cols(&blk), &format!("W {w}"));
            assert_in_order(&block_cols(&blk), blk.at.x, blk.at.end(), &format!("W {w}"));
        }
    }
    // At 1039 pt block B's U_cap is 54: only its own bound, U_B, keeps its column to 17.
    let b = plan_folded(1039.0, &m).blocks[1];
    let room = b.at.end() - b.strip.x - UNITS_TO_PCT;
    assert_eq!(
        units_cap(room, b.strip.spec, &fold_spec(b.kind)),
        Some(54.0)
    );

    let p = plan_folded(1039.0, &m);
    let a = p.blocks[0];
    assert_eq!(
        (a.units, a.pct, a.strip.x, a.strip.cols, a.meter.bar),
        (
            None,
            Some(Col::new(148.0, 30.0)),
            185.0,
            57,
            Some(Col::new(476.0, 65.0))
        )
    );
    assert_eq!(p.gauge.map(|g| g.centre()), Some((39.0, 31.0)));
    assert_eq!(
        p.blocks.map(|blk| (blk.rows, blk.units, blk.machine)),
        [(
            Rows::folded(BlockKind::Wide, DeckRows::Resources),
            None,
            None
        ); 2]
    );
    // The instruments' rows move no column: with no facts their plan is the painted one's.
    let bare = plan_folded_with(1039.0, &m, &UnitFacts::NONE);
    assert_eq!(
        bare.blocks
            .map(|blk| (blk.at, blk.name, blk.pct, blk.strip, blk.meter)),
        p.blocks
            .map(|blk| (blk.at, blk.name, blk.pct, blk.strip, blk.meter))
    );
}

/// A1 §2.4's ladder by width for this Mac, with A2 §3.5's Neural Engine tile and A4's rows
/// (§4, §5): U_cap is b − 403 for a block b pt wide, in block A and in block B, whose own
/// bound is 17, so a form U pt wide draws from W = 2U + 931 pt. Widening from 900 pt, the
/// GPU's undivided tank comes at 947 pt (957 with the tile), block B's interface tiles and
/// processes columns at 961, Disk's two tiles' place at 969, Memory's Half at 983, the heat
/// chain at 991, the GPU's Column at 993 (1003) and Memory's waffle at 1023. A Linux box's
/// heat ladder alone draws from 937. The CPU row draws none (A4 §7). At the owner's 940 pt no
/// units draw.
#[test]
fn this_macs_forms_step_up_where_the_design_says() {
    use Form::{Column, Half, Undivided};
    use UnitRow::{Disk, Memory, Network, Processes};
    let m = hn();
    let steps = |facts: &UnitFacts| {
        let mut was = [None; UnitRow::ALL.len()];
        let mut steps = Vec::new();
        for w in (900..=1100).map(f64::from) {
            let p = plan_folded_with(w, &m, facts);
            for (blk, bound) in p.blocks.iter().zip([f64::INFINITY, B_UNITS_MAX]) {
                if let Some(units) = blk.units {
                    assert_eq!(units.cap, (blk.at.w - 403.0).min(bound), "W {w}");
                }
            }
            let forms = forms_of(&p.blocks.map(|blk| blk.units));
            for (i, form) in forms.into_iter().enumerate() {
                if form != was[i] {
                    steps.push((w, UnitRow::ALL[i], form));
                    was[i] = form;
                }
            }
        }
        steps
    };
    let mac = this_mac();
    let without = UnitFacts {
        gpu: Some(Gpu {
            cores: Some(10),
            neural_engine: false,
        }),
        ..mac
    };
    let ladder = |undivided: f64, column: f64| {
        let mut want = vec![
            (undivided, UnitRow::Gpu, Some(Undivided)),
            (961.0, Network, Some(Column)),
            (961.0, Processes, Some(Column)),
            (969.0, Disk, Some(Column)),
            (983.0, Memory, Some(Half)),
            (991.0, UnitRow::Heat, Some(Column)),
            (column, UnitRow::Gpu, Some(Column)),
            (1023.0, Memory, Some(Column)),
        ];
        want.sort_by(|a, b| a.0.total_cmp(&b.0));
        want
    };
    assert_eq!(steps(&without), ladder(947.0, 993.0));
    assert_eq!(steps(&mac), ladder(957.0, 1003.0));
    let linux = UnitFacts {
        gpu: None,
        heat: Some(Heat {
            ladder: true,
            clocks: 0,
            watts: 0,
            work: false,
        }),
        ..mac
    };
    assert_eq!(
        steps(&linux).first(),
        Some(&(937.0, UnitRow::Heat, Some(Column)))
    );
    assert_eq!(
        plan_folded_with(940.0, &m, &mac)
            .blocks
            .map(|blk| blk.units),
        [None, None]
    );
}

/// What breaks A1 §2.4's units law in `blk`, a block or table laid around its units column
/// for `rows`, given `bare`, the same laid with none, and `relay`, which lays it again around
/// a column `u` pt wide: a column beside a horizon short of the top preset, or that costs the
/// table its kind, its bar column or the top preset; a form past the cap, a form on a row not
/// the table's, or a column wider than its widest form; a cap other than U_cap, or `bound`
/// where that is narrower; a U_cap that is not tight.
fn units_faults(
    bare: (FullOrBlock, f64, f64, &MeterSpec),
    blk: (FullOrBlock, Option<UnitsCol>),
    rows: &[UnitRow],
    bound: f64,
    facts: &UnitFacts,
    relay: &dyn Fn(f64) -> FullOrBlock,
) -> Vec<String> {
    let top = Strip::spec_for(f64::INFINITY);
    let (bare, room, cell, spec) = bare;
    let (laid, units) = blk;
    let Some(units) = units else {
        return Vec::new();
    };
    let held = bare.spec;
    let mut faults = Vec::new();
    if held != top {
        faults.push(format!("units beside a horizon of {held:?}"));
    }
    let keeps = |t: FullOrBlock| t.fits && t.kind == bare.kind && t.bar && t.spec == top;
    if !keeps(laid) {
        faults.push("the column costs the table six hours, its kind or its bar".to_owned());
    }
    for row in UnitRow::ALL {
        let Some(form) = units.form(row) else {
            continue;
        };
        if !rows.contains(&row) {
            faults.push(format!("{row:?} draws in a table that does not hold it"));
        }
        match facts.width(row, form, PLAN_SCALE, cell) {
            Some(fw) if fw <= units.cap => {}
            fw => faults.push(format!(
                "{row:?}'s {form:?} of {fw:?} pt past {}",
                units.cap
            )),
        }
    }
    let widest = facts.widest(&units.forms, cell);
    if units.at.w != widest {
        faults.push(format!(
            "a column of {} pt, its widest form {widest}",
            units.at.w
        ));
    }
    if units.at.w > bound {
        faults.push(format!("a column of {} pt past {bound} pt", units.at.w));
    }
    let cap = units_cap(room, held, spec);
    if cap.map(|cap| cap.min(bound)) != Some(units.cap) {
        faults.push(format!(
            "a cap of {}, not {cap:?} within {bound}",
            units.cap
        ));
    }
    if let Some(cap) = cap {
        for (u, kept) in [(units.at.w, true), (cap, true), (cap + 1.0, false)] {
            if keeps(relay(u)) != kept {
                faults.push(format!("a column of {u} pt keeps the preset: {}", !kept));
            }
        }
    }
    faults
}

/// What [`units_faults`] reads of a laid table.
#[derive(Clone, Copy, Debug)]
struct FullOrBlock {
    fits: bool,
    kind: usize,
    bar: bool,
    spec: MultiScaleSpec,
}

impl FullOrBlock {
    fn block((b, fits): (BlockPlan, bool)) -> Self {
        Self {
            fits,
            kind: BlockKind::ALL
                .iter()
                .position(|&k| k == b.kind)
                .unwrap_or(4),
            bar: b.meter.bar.is_some(),
            spec: b.strip.spec,
        }
    }

    fn full((p, fits): (FullPlan, bool)) -> Self {
        Self {
            fits,
            kind: FullKind::ALL.iter().position(|&k| k == p.kind).unwrap_or(4),
            bar: p.meter.bar.is_some(),
            spec: p.strip.spec,
        }
    }
}

/// A1 §2.4, §2.5 and ruling 11, with A4 §5: a units column never costs its strip the top
/// preset, its kind or its bar column, in Folded's block A, in its block B and in Full (step
/// R5), at any whole width from 160 to 3200 pt, for any machine, on either face. Units draw
/// only where the table's strip holds the top preset without them, and in Full only in a wide
/// or mid table; each block draws its own rows' forms, every form within its cap, and the
/// column is as wide as the widest of them. U_cap is tight: laid with a column up to U_cap
/// the table keeps the top preset, and with a column a point wider loses it. Block B moves by
/// its column, at most 17 pt (U_B), and keeps six hours from 1039 pt, as block A keeps them;
/// the gauge and the divider never move. CONTROL: at 1039 pt a 21 pt column would keep block
/// B's six hours (its U_cap is 54), and the law refuses it as past U_B.
#[test]
fn the_units_columns_keep_the_strips_preset_and_block_b_moves_17_pt_at_most() {
    let (mut drawn, mut drawn_b, mut drawn_full) = (0, 0, 0);
    let deck = DeckRows::Instruments;
    for m in [hn(), FaceMetrics::test_stack()] {
        for facts in unit_machines() {
            for w in (160..=3200).map(f64::from) {
                let bare = plan_full_with(w, &m, &UnitFacts::NONE);
                let p = plan_full_with(w, &m, &facts);
                let what = || format!("Full, {facts:?} at {w} pt");
                if p.units.is_none() {
                    assert_eq!(p, bare, "{}", what());
                    continue;
                }
                drawn_full += 1;
                assert!(
                    matches!(p.kind, FullKind::Wide | FullKind::Mid),
                    "{}",
                    what()
                );
                assert_eq!(p.kind, bare.kind, "{}", what());
                let spec = full_meter_spec(p.kind, &m, full_spec);
                let room = bare.w - X0 - (bare.strip.x - bare.offset) - UNITS_TO_PCT;
                let relay =
                    |u: f64| FullOrBlock::full(lay_full(p.w, p.kind, &m, true, u, full_spec, deck));
                let faults = units_faults(
                    (FullOrBlock::full((bare, true)), room, p.rows.cell, &spec),
                    (FullOrBlock::full((p, true)), p.units),
                    &UnitRow::ALL,
                    f64::INFINITY,
                    &facts,
                    &relay,
                );
                assert!(faults.is_empty(), "{}: {faults:?}", what());
            }
            for w in (160..=3200).map(f64::from) {
                let bare = plan_folded_with(w, &m, &UnitFacts::NONE);
                let p = plan_folded_with(w, &m, &facts);
                let what = || format!("{facts:?} at {w} pt");
                assert_eq!(
                    (p.gauge, p.divider, p.ground_only),
                    (bare.gauge, bare.divider, bare.ground_only),
                    "{}",
                    what()
                );
                let blocks = [
                    (UnitRow::BLOCK_A.as_slice(), f64::INFINITY),
                    (UnitRow::BLOCK_B.as_slice(), B_UNITS_MAX),
                ];
                for (i, (rows, bound)) in blocks.into_iter().enumerate() {
                    let (blk, blk0) = (p.blocks[i], bare.blocks[i]);
                    let Some(units) = blk.units else {
                        assert_eq!(blk, blk0, "{}", what());
                        continue;
                    };
                    if i == 0 {
                        drawn += 1;
                    } else {
                        drawn_b += 1;
                        if w >= 1039.0 {
                            assert_eq!(blk.strip.span_s(), 21_600, "{}", what());
                        }
                    }
                    // The block moves by its column, from where its % stood.
                    assert_eq!(
                        (blk.kind, blk.at, units.at.x, blk.pct.map(|pct| pct.x)),
                        (
                            blk0.kind,
                            blk0.at,
                            blk0.pct.expect("a %").x,
                            Some(units.at.end() + UNITS_TO_PCT)
                        ),
                        "{}",
                        what()
                    );
                    let room = blk0.at.end() - blk0.strip.x - UNITS_TO_PCT;
                    let spec = fold_spec(blk.kind);
                    let relay = |u: f64| {
                        FullOrBlock::block(lay_block(
                            blk.at.x, blk.at.w, blk.kind, &m, true, u, fold_spec, deck,
                        ))
                    };
                    let faults = units_faults(
                        (FullOrBlock::block((blk0, true)), room, blk.rows.cell, &spec),
                        (FullOrBlock::block((blk, true)), blk.units),
                        rows,
                        bound,
                        &facts,
                        &relay,
                    );
                    assert!(faults.is_empty(), "block {i}, {}: {faults:?}", what());
                }
            }
        }
    }
    assert!(
        drawn > 0 && drawn_b > 0 && drawn_full > 0,
        "CONTROL: a table drew no units"
    );

    let m = hn();
    let mac = this_mac();
    let p = plan_folded_with(1039.0, &m, &mac);
    let (b, b0) = (p.blocks[1], plan_folded(1039.0, &m).blocks[1]);
    let wide = lay_block(b.at.x, b.at.w, b.kind, &m, true, 21.0, fold_spec, deck);
    let column = UnitsCol {
        at: Col::new(b0.pct.expect("a %").x, 21.0),
        cap: 21.0,
        ..b.units.expect("block B's units")
    };
    let room = b0.at.end() - b0.strip.x - UNITS_TO_PCT;
    let relay = |u: f64| {
        FullOrBlock::block(lay_block(
            b.at.x, b.at.w, b.kind, &m, true, u, fold_spec, deck,
        ))
    };
    let faults = units_faults(
        (
            FullOrBlock::block((b0, true)),
            room,
            b.rows.cell,
            &fold_spec(b.kind),
        ),
        (FullOrBlock::block(wide), Some(column)),
        &UnitRow::BLOCK_B,
        B_UNITS_MAX,
        &mac,
        &relay,
    );
    assert_eq!(wide.0.strip.span_s(), 21_600, "CONTROL");
    assert!(
        faults.iter().any(|f| f.ends_with("past 17 pt"))
            && !faults.iter().any(|f| f.contains("six hours")),
        "CONTROL: {faults:?}"
    );
}

/// U_cap (A1 §2.4, step R5) leaves the meter a room its bar column holds in
/// ([`column_holds`]), for every table that draws units, on either face, and tightly: a
/// point more leaves a room where the column does not hold or the strip loses the top preset.
/// CONTROL: Full mid's measures with a bar from 160 pt lay the six-hour strip's column by
/// [`share_meter`] at 416, 417 and 418 pt of room, not at 419, and hold it only from 420. The
/// cap is none at 418 pt and 0 pt at 420; one by `share_meter` alone would be 2 pt at 418 and
/// 4 at 420, each leaving 416 pt, where [`lay_meter`] stands no bar column.
#[test]
fn the_units_cap_leaves_a_room_the_bar_column_holds_in() {
    let top = Strip::spec_for(f64::INFINITY);
    let six_hours = |r: f64, spec: &MeterSpec| {
        share_meter(r, spec)
            .is_some_and(|(cols, _, _)| Strip::spec_for(f64::from(cols) * spec.pitch - 1.0) == top)
    };
    let island = MeterSpec {
        bar: (160.0, FULL_MID.bar.1),
        ..full_meter_spec(FullKind::Mid, &hn(), full_spec)
    };
    let mut specs = vec![FOLD_WIDE, FOLD_MID, island];
    for m in [hn(), FaceMetrics::test_stack()] {
        for kind in [FullKind::Wide, FullKind::Mid] {
            specs.push(full_meter_spec(kind, &m, full_spec));
        }
    }
    for spec in &specs {
        for room in (0..1500).map(f64::from) {
            let Some(cap) = units_cap(room, top, spec) else {
                continue;
            };
            let left = room - cap;
            assert!(
                six_hours(left, spec) && column_holds(left, spec),
                "{spec:?} at {room} pt: a cap of {cap}"
            );
            assert!(
                cap + 1.0 > room
                    || !(six_hours(left - 1.0, spec) && column_holds(left - 1.0, spec)),
                "{spec:?} at {room} pt: a cap of {cap} is not tight"
            );
        }
    }

    let laid: Vec<u32> = (400..=430)
        .filter(|&r| six_hours(f64::from(r), &island))
        .collect();
    let held = (400..=430).find(|&r| column_holds(f64::from(r), &island));
    assert_eq!(
        (laid, held),
        (
            [416, 417, 418].into_iter().chain(420..=430).collect(),
            Some(420)
        ),
        "CONTROL"
    );
    assert_eq!(
        (
            units_cap(418.0, top, &island),
            units_cap(420.0, top, &island)
        ),
        (None, Some(0.0)),
        "CONTROL"
    );
}

/// A1 §2.4's monotone law at every width (step R5): as the deck widens a point at a time from
/// 160 to 3200 pt, in Folded and in Full, on either face, with no units facts and for every
/// machine, no strip's horizon shortens, no table takes a narrower kind, nothing a table laid
/// goes (a table at all, the gauge, a %, a bar column), and no row's form steps down its ladder,
/// block B's (A4 §5) with block A's. CONTROL: U_cap as A1 first wrote it, keeping whatever
/// preset block A's bare strip holds, leaves this Mac 122 pt at 924 pt, where the strip holds
/// four hours, so every block A row draws its Column there, and nothing at 925 pt, where six
/// hours leave −3; the same check finds the step down. The step downs main's meter measures
/// make on this planner below 925 pt are this law's other control
/// (`mains_meter_rules_shorten_the_horizon_as_the_deck_widens`); a planner that lays a kind
/// wherever [`share_meter`] lays its column, without [`column_holds`], fails this law, its
/// kinds narrowing at 661, 675, 687 and 701 pt.
#[test]
fn a_wider_deck_never_draws_a_shorter_horizon_or_a_smaller_unit_form() {
    let widths = || (160..=3200).map(f64::from);
    for m in [hn(), FaceMetrics::test_stack()] {
        for facts in std::iter::once(UnitFacts::NONE).chain(unit_machines()) {
            let folded: Vec<_> = widths()
                .map(|w| folded_widening(&plan_folded_with(w, &m, &facts)))
                .collect();
            let full: Vec<_> = widths()
                .map(|w| full_widening(&plan_full_with(w, &m, &facts)))
                .collect();
            for (form, seq) in [("Folded", folded), ("Full", full)] {
                let faults = widening_faults(&seq);
                assert!(faults.is_empty(), "{form}, {facts:?}: {faults:?}");
            }
        }
    }

    let m = hn();
    let mac = this_mac();
    let first_rule = |w: f64| {
        let p = plan_folded(w, &m);
        let a = p.blocks[0];
        let spec = fold_spec(a.kind);
        let room = a.at.end() - a.strip.x - UNITS_TO_PCT;
        let keeps = |u: f64| {
            share_meter(room - u, &spec).is_some_and(|(cols, _, _)| {
                Strip::spec_for(f64::from(cols) * spec.pitch - 1.0) == a.strip.spec
            })
        };
        // The widest whole column the preset leaves room for, short of none where it is
        // negative; the rule takes a column only where it is not.
        let slack = (-20..=room as i32).rev().map(f64::from).find(|&u| keeps(u));
        let cap = slack.filter(|&u| u >= 0.0);
        let forms = cap.map_or([None; UnitRow::ALL.len()], |cap| {
            mac.forms_within(cap, a.rows.cell, &UnitRow::BLOCK_A)
        });
        (
            (slack, cap),
            Widening {
                rungs: rungs(forms),
                ..folded_widening(&p)
            },
        )
    };
    let (below, above) = (first_rule(924.0), first_rule(925.0));
    assert_eq!(
        (below.0, above.0),
        ((Some(122.0), Some(122.0)), (Some(-3.0), None))
    );
    assert_eq!(
        (below.1.rungs, above.1.rungs),
        ([0, 0, 0, 0, 1, 1], [2, 2, 1, 1, 1, 1])
    );
    let faults = widening_faults(&[below.1, above.1]);
    assert_eq!(
        faults,
        ["924→925 pt: Gpu 0→2, Memory 0→2, Disk 0→1, Heat 0→1"],
        "CONTROL"
    );
}

/// The monotone law's control (A1 step R5): main's meter measures ([`mains_fold_spec`],
/// [`mains_full_spec`]), planned with this planner and [`column_holds`] in it, shorten the
/// horizon as the deck widens, and R5's `pct_strip` and strip leasts each take their own step
/// downs away; `column_holds` took main's others. On Helvetica Neue, a % column beside a strip
/// of a minute (`pct_strip` 0) shortens both blocks' strips from six minutes to one at 347 pt,
/// where a Slim block first fits its %, and at 419 pt, where the gauge comes; a bar column
/// beside main's leasts shortens them from four hours to six minutes at 601 pt, where a Narrow
/// block first fits, and Full's strip from six hours to four at 383 pt (`test_stack`: 357,
/// 429, 611 and 400). And a kind laid wherever [`share_meter`] lays its column, without
/// [`column_holds`], narrows as the deck widens: FOLD_WIDE's column fits 173, 179 and 186 pt of
/// room and not a point more, and FOLD_MID's 168, so on Helvetica Neue block A would go from
/// Mid to Narrow at 661 pt and from Wide to Mid at 675, 687 and 701, where the planner lays
/// Mid from 663 and Wide from 703 and each kind once.
#[test]
fn mains_meter_rules_shorten_the_horizon_as_the_deck_widens() {
    let fold_pct = |k: BlockKind| MeterSpec {
        pct_strip: 0.0,
        ..fold_spec(k)
    };
    let fold_least = |k: BlockKind| MeterSpec {
        pct_strip: PCT_STRIP,
        ..mains_fold_spec(k)
    };
    let full_least = |k: FullKind| MeterSpec {
        pct_strip: PCT_STRIP,
        ..mains_full_spec(k)
    };
    let widths = || (160..=1200).map(f64::from);
    let folded = |m: &FaceMetrics, specs: FoldSpecs| {
        let seq: Vec<_> = widths()
            .map(|w| folded_widening(&folded_by(w, m, &UnitFacts::NONE, specs, DeckRows::PAINTED)))
            .collect();
        widening_faults(&seq)
    };
    let full = |m: &FaceMetrics, specs: FullSpecs| {
        let seq: Vec<_> = widths()
            .map(|w| full_widening(&full_by(w, m, &UnitFacts::NONE, specs, DeckRows::PAINTED)))
            .collect();
        widening_faults(&seq)
    };
    let both = |w0: f64, w1: f64, h0: u32, h1: u32| {
        format!("{w0}→{w1} pt: table 0 {h0}→{h1} s, table 1 {h0}→{h1} s")
    };
    for (m, [pct, gauge, narrow, full_at]) in [
        (hn(), [347.0, 419.0, 601.0, 383.0]),
        (FaceMetrics::test_stack(), [357.0, 429.0, 611.0, 400.0]),
    ] {
        let pct_faults = [
            both(pct - 1.0, pct, 360, 60),
            both(gauge - 1.0, gauge, 360, 60),
        ];
        let least_faults = [both(narrow - 1.0, narrow, 14_400, 360)];
        assert_eq!(folded(&m, fold_pct), pct_faults, "CONTROL");
        assert_eq!(folded(&m, fold_least), least_faults, "CONTROL");
        let mains = [pct_faults.as_slice(), least_faults.as_slice()].concat();
        assert_eq!(folded(&m, mains_fold_spec), mains, "CONTROL");
        let full_faults = [format!(
            "{}→{full_at} pt: table 0 21600→14400 s",
            full_at - 1.0
        )];
        assert_eq!(full(&m, full_least), full_faults, "CONTROL");
        assert_eq!(full(&m, mains_full_spec), full_faults, "CONTROL");
        assert!(folded(&m, fold_spec).is_empty() && full(&m, full_spec).is_empty());
    }

    let flips = |spec: &MeterSpec| -> Vec<u32> {
        (0..1000)
            .filter(|&r| {
                share_meter(f64::from(r), spec).is_some()
                    && share_meter(f64::from(r + 1), spec).is_none()
            })
            .collect()
    };
    assert_eq!(flips(&FOLD_WIDE), [173, 179, 186], "CONTROL");
    assert_eq!(flips(&FOLD_MID), [168], "CONTROL");
    for spec in [FOLD_WIDE, FOLD_MID] {
        let last = flips(&spec).into_iter().max().expect("a flip");
        let holds = (0..1000).map(f64::from).find(|&r| column_holds(r, &spec));
        assert_eq!(holds, Some(f64::from(last) + 2.0));
    }
    // Block A's kind by `share_meter` alone: the first whose column it lays, else Slim.
    let m = hn();
    let by_share = |w: f64| {
        let a = plan_folded(w, &m).blocks[0];
        BlockKind::ALL.into_iter().position(|k| {
            let (blk, _) = lay_block(
                a.at.x,
                a.at.w,
                k,
                &m,
                true,
                0.0,
                fold_spec,
                DeckRows::PAINTED,
            );
            k == BlockKind::Slim || share_meter(a.at.end() - blk.strip.x, &fold_spec(k)).is_some()
        })
    };
    let narrowed: Vec<f64> = widths()
        .collect::<Vec<_>>()
        .windows(2)
        .filter(|pair| by_share(pair[1]) > by_share(pair[0]))
        .map(|pair| pair[1])
        .collect();
    assert_eq!(narrowed, [661.0, 675.0, 687.0, 701.0], "CONTROL");
}

/// A1 §2.5's points for this Mac in Full (step R5), with A4's (§5, §7, §8, §11): at 1039 pt
/// the units 166–212 (U 46 within a U_cap of 471, every row's form in the one column, block
/// B's rows' with block A's), the % 215–245, a six-hour strip of 81 boxes 252–656, the bar
/// 665–784, the run and Details 888–1027, which give way to the units as block A's do; at
/// 1440 pt the same units and %, a strip of 120 boxes to 851, the bar 860–1100 and Details
/// 1204–1428. Ten rows at 12 pt in 136 pt; the gauge's slot 12–80 past its divider at 88; the
/// Cores row from the units to the right margin; the Machine row's name in 97–200 and its
/// glyphs 203–245, ending with the %. Every edge stands on whole device px at 1× and 2×. The
/// plan the deck paints is main's: no units, no Cores or Machine row beside the gauge's slot.
#[test]
fn full_1039_and_1440_plan_units_percent_strip_and_bar_at_their_points() {
    let m = hn();
    let mac = this_mac();
    for (w, cap, cols, strip_end, bar, details) in [
        (
            1039.0,
            471.0,
            81,
            656.0,
            Col::new(665.0, 119.0),
            Col::new(888.0, 139.0),
        ),
        (
            1440.0,
            872.0,
            120,
            851.0,
            Col::new(860.0, 240.0),
            Col::new(1204.0, 224.0),
        ),
    ] {
        let p = plan_full_with(w, &m, &mac);
        assert_eq!(plan_full_with((2.0 * w + 1.0) / 2.0, &m, &mac), p, "W {w}");
        assert_eq!(
            (p.kind, p.offset, p.name, p.well),
            (
                FullKind::Wide,
                0.0,
                Col::new(97.0, 56.0),
                Col::new(153.0, 10.0)
            ),
            "W {w}"
        );
        assert_eq!(
            p.units,
            Some(UnitsCol {
                at: Col::new(166.0, 46.0),
                cap,
                forms: [Some(Form::Column); UnitRow::ALL.len()],
            }),
            "W {w}"
        );
        assert_eq!(p.pct, Some(Col::new(215.0, 30.0)), "W {w}");
        assert_eq!(
            (p.strip.x, p.strip.cols, p.strip.end(), p.strip.span_s()),
            (252.0, cols, strip_end, 21_600),
            "W {w}"
        );
        assert_eq!(
            (p.meter.bar, p.meter.run.map(|run| run.w), p.details),
            (Some(bar), Some(88.0), Some(details)),
            "W {w}"
        );
        assert_eq!(
            (p.gauge, p.cores, p.machine),
            (
                Some(GaugeSlot {
                    at: Col::new(12.0, 68.0),
                    divider: 88.0
                }),
                Some(Col::new(166.0, w - X0 - 166.0)),
                Some(MachineRow {
                    name: Col::new(97.0, 103.0),
                    glyphs: Col::new(203.0, 42.0),
                })
            ),
            "W {w}"
        );
        assert_eq!(p.rows, Rows::full(FullKind::Wide, DeckRows::Instruments));
        assert_on_pixels(&full_cols(&p), &format!("W {w}"));
        assert_in_order(&full_cols(&p), 0.0, w - X0, &format!("W {w}"));
        let painted = plan_full(w, &m);
        assert_eq!(
            (painted.units, painted.cores, painted.machine),
            (None, None, None)
        );
        assert_eq!(
            painted.rows,
            Rows::full(FullKind::Wide, DeckRows::Resources)
        );
        let bare = plan_full_with(w, &m, &UnitFacts::NONE);
        assert_eq!(
            (bare.name, bare.pct, bare.strip, bare.meter, bare.details),
            (
                painted.name,
                painted.pct,
                painted.strip,
                painted.meter,
                painted.details
            )
        );
    }
}

/// A1 §2.5 as built (step R5), with A4's rows: in Full a wide or mid table draws units, and a
/// narrow or slim one none, for any machine, on either face. U_cap is W − 568 pt in both on
/// Helvetica Neue, so this Mac's GPU draws its undivided tank from 581 pt (576 without the
/// Neural Engine tile), the interface tiles and the processes columns at 583, Disk at 587,
/// Memory's Half at 594, the heat chain at 598, the GPU's Column at 604 (599) and Memory's
/// waffle at 614. CONTROL: a narrow table's U_cap reaches 78 pt at 555 pt, where it would draw
/// every row's Column, and the first mid table, at 565 pt, has none: a form a narrow table
/// drew would go again as the deck widened.
#[test]
fn full_draws_units_in_wide_and_mid_tables_and_none_narrower() {
    use Form::{Column, Half, Undivided};
    use UnitRow::{Disk, Memory, Network, Processes};
    for m in [hn(), FaceMetrics::test_stack()] {
        for facts in unit_machines() {
            for w in (160..=3200).map(f64::from) {
                let p = plan_full_with(w, &m, &facts);
                if matches!(p.kind, FullKind::Narrow | FullKind::Slim) {
                    assert_eq!(p.units, None, "{facts:?} at {w} pt");
                }
            }
        }
    }
    let m = hn();
    let steps = |facts: &UnitFacts| {
        let mut was = [None; UnitRow::ALL.len()];
        let mut steps = Vec::new();
        for w in (500..=1100).map(f64::from) {
            let p = plan_full_with(w, &m, facts);
            if let Some(units) = p.units {
                assert_eq!(units.cap, w - 568.0, "W {w}");
            }
            let forms = forms_of(&[p.units]);
            for (i, form) in forms.into_iter().enumerate() {
                if form != was[i] {
                    steps.push((w, UnitRow::ALL[i], form));
                    was[i] = form;
                }
            }
        }
        steps
    };
    let mac = this_mac();
    let without = UnitFacts {
        gpu: Some(Gpu {
            cores: Some(10),
            neural_engine: false,
        }),
        ..mac
    };
    let ladder = |undivided: f64, column: f64| {
        let mut want = vec![
            (undivided, UnitRow::Gpu, Some(Undivided)),
            (583.0, Network, Some(Column)),
            (583.0, Processes, Some(Column)),
            (587.0, Disk, Some(Column)),
            (594.0, Memory, Some(Half)),
            (598.0, UnitRow::Heat, Some(Column)),
            (column, UnitRow::Gpu, Some(Column)),
            (614.0, Memory, Some(Column)),
        ];
        want.sort_by(|a, b| a.0.total_cmp(&b.0));
        want
    };
    assert_eq!(steps(&mac), ladder(581.0, 604.0));
    assert_eq!(steps(&without), ladder(576.0, 599.0));

    let deck = DeckRows::Instruments;
    let cap_at = |w: f64, kind: FullKind| {
        let (bare, fits) = lay_full(w, kind, &m, true, 0.0, full_spec, deck);
        let spec = full_meter_spec(kind, &m, full_spec);
        let room = bare.w - X0 - (bare.strip.x - bare.offset) - UNITS_TO_PCT;
        (fits, units_cap(room, bare.strip.spec, &spec))
    };
    let first_mid = (160..=3200)
        .map(f64::from)
        .find(|&w| plan_full(w, &m).kind == FullKind::Mid);
    assert_eq!(first_mid, Some(565.0));
    assert_eq!(cap_at(565.0, FullKind::Mid), (true, None), "CONTROL");
    assert_eq!(
        cap_at(555.0, FullKind::Narrow),
        (true, Some(78.0)),
        "CONTROL"
    );
    assert_eq!(
        mac.forms_within(78.0, Rows::full(FullKind::Narrow, deck).cell, &UnitRow::ALL),
        [Some(Column); UnitRow::ALL.len()],
        "CONTROL"
    );
}

/// A1 §2.4 and §2.5: the column stands where the % stood, [`UNITS_TO_PCT`] short of the % now,
/// and every row's form fits it at 1× and at 2× (where a hairline is half a point, so no form
/// is wider), right-aligned against its %: each form's left edge inside the column and on a
/// whole device px at both scales, and every column of the table in order on whole device px,
/// in each Folded block and in Full (step R5's half, A4 §5), at every whole width from 160 to
/// 3200 pt.
#[test]
fn units_fit_their_column_at_every_folded_and_full_width_at_1x_and_2x() {
    let (mut seen, mut seen_b, mut seen_full) = (0, 0, 0);
    let fit = |facts: &UnitFacts, units: UnitsCol, cell: f64, what: &str| {
        let mut laid = 0;
        for row in UnitRow::ALL {
            let Some(form) = units.form(row) else {
                continue;
            };
            for s in [1.0, 2.0] {
                let fw = facts.width(row, form, s, cell).expect("drawn");
                let x = units.at.end() - fw;
                assert!(x >= units.at.x, "{what}: {row:?} {form:?} at {s}×");
                assert_eq!(x * s, (x * s).round(), "{what}: {row:?} at {s}×");
                laid += 1;
            }
        }
        laid
    };
    for m in [hn(), FaceMetrics::test_stack()] {
        for facts in unit_machines() {
            for w in (160..=3200).map(f64::from) {
                let p = plan_full_with(w, &m, &facts);
                let Some(units) = p.units else { continue };
                let what = format!("Full, {facts:?} at {w} pt");
                let pct = p.pct.expect("a % column beside the units");
                assert_eq!(
                    (units.at.x, units.at.end() + UNITS_TO_PCT),
                    (p.well.end() + 3.0, pct.x),
                    "{what}"
                );
                assert_in_order(&full_cols(&p), 0.0, p.w - X0, &what);
                assert_on_pixels(&full_cols(&p), &what);
                seen_full += fit(&facts, units, p.rows.cell, &what);
            }
            for w in (160..=3200).map(f64::from) {
                for (i, blk) in plan_folded_with(w, &m, &facts).blocks.iter().enumerate() {
                    let Some(units) = blk.units else { continue };
                    let what = || format!("block {i}, {facts:?} at {w} pt");
                    let pct = blk.pct.expect("a % column beside the units");
                    assert_eq!(
                        (units.at.x, units.at.end() + UNITS_TO_PCT),
                        (blk.well.end() + 2.0, pct.x),
                        "{}",
                        what()
                    );
                    assert_in_order(&block_cols(blk), blk.at.x, blk.at.end(), &what());
                    assert_on_pixels(&block_cols(blk), &what());
                    let laid = fit(&facts, units, blk.rows.cell, &what());
                    if i == 0 {
                        seen += laid;
                    } else {
                        seen_b += laid;
                    }
                }
            }
        }
    }
    assert!(
        seen > 0 && seen_b > 0 && seen_full > 0,
        "CONTROL: no form was laid"
    );
}

/// A2 §3.5 (S5's planner half): the Neural Engine tile and its clear are 5 pt beside the tank,
/// at 1× and 2×, in both the GPU's forms, and the forms ladder takes them: this Mac's GPU
/// draws its undivided tank from 957 pt and its Column from 1003 (947 and 993 without the
/// tile), its form never steps down as the deck widens to 1440 pt, and U stays Memory's 46 at
/// 1039 pt. With no GPU the tile takes no space. CONTROL: a ladder that measured the tank
/// alone would draw this Mac's Column at 993 pt, where the tile and the tank take 36 pt and
/// U_cap is 31.
#[test]
fn the_forms_ladder_takes_the_tiles_width_monotonically() {
    let m = hn();
    let mac = this_mac();
    let tank = UnitFacts {
        gpu: Some(Gpu {
            cores: Some(10),
            neural_engine: false,
        }),
        ..mac
    };
    let cell = Rows::folded(BlockKind::Wide, DeckRows::Instruments).cell;
    for form in [Form::Column, Form::Undivided] {
        for s in [1.0, 2.0] {
            let with = mac.width(UnitRow::Gpu, form, s, cell).expect("a tank");
            let alone = tank.width(UnitRow::Gpu, form, s, cell).expect("a tank");
            assert_eq!(with - alone, 5.0, "{form:?} at {s}×");
        }
    }
    let gpu_form = |facts: &UnitFacts, w: f64| {
        plan_folded_with(w, &m, facts).blocks[0]
            .units
            .and_then(|units| units.form(UnitRow::Gpu))
    };
    let first = |facts: &UnitFacts, form: Form| {
        (900..=1100)
            .map(f64::from)
            .find(|&w| gpu_form(facts, w) == Some(form))
    };
    let forms = [Form::Undivided, Form::Column];
    assert_eq!(forms.map(|f| first(&mac, f)), [Some(957.0), Some(1003.0)]);
    assert_eq!(forms.map(|f| first(&tank, f)), [Some(947.0), Some(993.0)]);
    let mut last = usize::MAX;
    for w in (900..=1440).map(f64::from) {
        let r = rung(UnitRow::Gpu, gpu_form(&mac, w));
        assert!(
            r <= last,
            "{w} pt steps the GPU down to {:?}",
            gpu_form(&mac, w)
        );
        last = r;
    }
    let at_1039 = plan_folded_with(1039.0, &m, &mac).blocks[0].units;
    assert_eq!(at_1039.map(|units| units.at.w), Some(46.0));
    let gpuless = UnitFacts { gpu: None, ..mac };
    for form in [Form::Column, Form::Undivided] {
        assert_eq!(gpuless.width(UnitRow::Gpu, form, 1.0, cell), None);
    }

    let at_993 = plan_folded_with(993.0, &m, &mac).blocks[0]
        .units
        .expect("units at 993 pt");
    assert_eq!(
        (at_993.cap, at_993.form(UnitRow::Gpu)),
        (31.0, Some(Form::Undivided))
    );
    assert_eq!(gpu_form(&tank, 993.0), Some(Form::Column), "CONTROL");
    let column = mac.width(UnitRow::Gpu, Form::Column, PLAN_SCALE, cell);
    assert_eq!(column, Some(36.0), "CONTROL");
}

/// A1 §2.4 and §4.4: the front tab's device, which comes and goes with the tab, never moves
/// block A's columns or drops a row's form. Wherever block A takes the Disk form, at every
/// whole width from 160 to 3200 pt, for every machine, on either face, the column holds
/// [`DISK_UNITS`] square tiles a point apart at 1× and at 2×, home's and the second's place,
/// and the plan reads no count of the tiles a reading draws ([`UnitFacts::disk`]), so a second
/// device draws inside the column the plan laid. CONTROL: on this Mac a form as wide as the
/// tiles drawn would take Disk with home's tile alone at 949–968 pt, where U_cap leaves no
/// room for a second, and lay U at the GPU's 13 pt at 969–982 pt, where two tiles take 19: a
/// second device would there drop the row's form or move the % and the strip 6 pt.
#[test]
fn a_front_tab_on_another_device_moves_no_column() {
    use aterm_vitals::units::DISK_UNITS;
    // DISK_UNITS square tiles, each the cell's whole device px, a point apart (A1 §4.4).
    let place = |cell: f64, s: f64| {
        let n = DISK_UNITS as f64;
        n * (cell * s).ceil() / s + (n - 1.0)
    };
    let mut seen = 0;
    for m in [hn(), FaceMetrics::test_stack()] {
        for facts in unit_machines() {
            for w in (160..=3200).map(f64::from) {
                let a = plan_folded_with(w, &m, &facts).blocks[0];
                let Some(units) = a.units else { continue };
                if units.form(UnitRow::Disk) != Some(Form::Column) {
                    continue;
                }
                let cell = a.rows.cell;
                for s in [1.0, 2.0] {
                    let disk = facts.width(UnitRow::Disk, Form::Column, s, cell);
                    assert_eq!(disk, Some(place(cell, s)), "{facts:?} at {w} pt, {s}×");
                }
                assert!(
                    place(cell, PLAN_SCALE) <= units.at.w,
                    "{facts:?} at {w} pt: {units:?}"
                );
                seen += 1;
            }
        }
    }
    assert!(seen > 0, "CONTROL: no plan took the Disk form");

    let m = hn();
    let mac = this_mac();
    let cell = Rows::folded(BlockKind::Wide, DeckRows::Instruments).cell;
    let others = UnitFacts { disk: false, ..mac };
    let (home, two) = (cell.ceil(), place(cell, PLAN_SCALE));
    // The column a form of `disk` pt would lay beside block A's other rows' forms within
    // `cap`.
    let laid = |cap: f64, disk: f64| {
        let forms = others.forms_within(cap, cell, &UnitRow::BLOCK_A);
        let rest = others.widest(&forms, cell);
        if disk <= cap { rest.max(disk) } else { rest }
    };
    let (mut drops, mut moves) = (Vec::new(), Vec::new());
    for w in 900..=1100 {
        let a = plan_folded(f64::from(w), &m).blocks[0];
        let room = a.at.end() - a.strip.x - UNITS_TO_PCT;
        let Some(cap) = units_cap(room, a.strip.spec, &fold_spec(a.kind)) else {
            continue;
        };
        let units = plan_folded_with(f64::from(w), &m, &mac).blocks[0].units;
        let planned = units.map_or(0.0, |units| units.at.w);
        assert_eq!(planned, laid(cap, two), "{w} pt");
        if home <= cap && two > cap {
            drops.push(w);
        } else if laid(cap, home) != planned {
            moves.push(w);
        }
    }
    assert_eq!(drops, (949..=968).collect::<Vec<_>>(), "CONTROL");
    assert_eq!(moves, (969..=982).collect::<Vec<_>>(), "CONTROL");
}

/// A1 §2.4's as-built note, with A4 §5: each block's Details give way to its own units, as
/// [`lay_meter`] gives Details way to a row's run and its strip. Past its units column a
/// block is the block laid bare U + 3 pt narrower, its strip, meter and Details moved right by
/// the column, at every whole width from 160 to 3200 pt, for every machine, on either face, in
/// block A and now in block B; so one block may lay no Details where the other lays them.
/// Bare, both blocks lay Details at 1825–1878 pt and from 2243 on Helvetica Neue; each
/// block's windows move right by twice its column and gap, block B's (U 15) 36 pt and block
/// A's (U 46) 98. CONTROL: this Mac's block B lays Details and its block A none at 1861–1914
/// and 2279–2340 pt, and block A lays them and block B none at 1923–1976: a rule that made the
/// two blocks agree fails the check there; and block B, which A1's rule left bare, lays none
/// at 1825–1860 and 2243–2278 pt, where the bare block lays them.
#[test]
fn each_blocks_units_cost_its_own_details_their_width() {
    let (mut b_only, mut a_only, mut b_lost) = (Vec::new(), Vec::new(), Vec::new());
    let deck = DeckRows::Instruments;
    for (face, m) in [("hn", hn()), ("stack", FaceMetrics::test_stack())] {
        for (i, facts) in unit_machines().into_iter().enumerate() {
            for w in 160..=3200 {
                let p = plan_folded_with(f64::from(w), &m, &facts);
                let [a, b] = p.blocks;
                for blk in [a, b] {
                    let Some(units) = blk.units else { continue };
                    let what = || format!("{facts:?} at {w} pt on {face}");
                    let shift = units.at.w + UNITS_TO_PCT;
                    let (bare, fits) = lay_block(
                        blk.at.x,
                        blk.at.w - shift,
                        blk.kind,
                        &m,
                        true,
                        0.0,
                        fold_spec,
                        deck,
                    );
                    assert!(fits, "{}", what());
                    let moved = |col: Col| Col::new(col.x + shift, col.w);
                    assert_eq!(
                        (blk.strip, blk.meter.bar, blk.meter.run, blk.details),
                        (
                            Strip {
                                x: bare.strip.x + shift,
                                ..bare.strip
                            },
                            bare.meter.bar.map(moved),
                            bare.meter.run.map(moved),
                            bare.details.map(moved)
                        ),
                        "{}",
                        what()
                    );
                }
                if (face, i) == ("hn", 0) {
                    match (a.details.is_some(), b.details.is_some()) {
                        (false, true) => b_only.push(w),
                        (true, false) => a_only.push(w),
                        _ => {}
                    }
                    let b0 = plan_folded_with(f64::from(w), &m, &UnitFacts::NONE).blocks[1];
                    if b0.details.is_some() && b.details.is_none() {
                        b_lost.push(w);
                    }
                }
            }
        }
    }
    assert_eq!(
        b_only,
        (1861..=1914).chain(2279..=2340).collect::<Vec<_>>(),
        "CONTROL"
    );
    assert_eq!(a_only, (1923..=1976).collect::<Vec<_>>(), "CONTROL");
    assert_eq!(
        b_lost,
        (1825..=1860).chain(2243..=2278).collect::<Vec<_>>(),
        "CONTROL"
    );
}

/// Whether `said`, the Machine row's name in `room` pt for a machine whose name forms are
/// `forms` and whose OS calls it `noun`, breaks A4 §9's fit: wider than its room; not the
/// first form that fits whole; a cut of the family form that is not at a word boundary, or
/// is shorter than the longest such cut that fits; the noun where a cut fits; nothing where
/// the noun fits.
fn name_fault(
    forms: &[&str],
    noun: &str,
    room: f64,
    said: &str,
    m: &FaceMetrics,
) -> Option<String> {
    let fits = |s: &str| m.width(s, MACHINE_NAME_PT, true) <= room + 0.01;
    let at = || format!("{forms:?} in {room} pt says {said:?}");
    if !said.is_empty() && !fits(said) {
        return Some(format!("{}, too wide", at()));
    }
    if let Some(whole) = forms.iter().find(|form| fits(form)) {
        return (said != *whole).then(|| format!("{}, not {whole:?}", at()));
    }
    let family = forms.last().copied().unwrap_or_default();
    let cuts: Vec<String> = family
        .match_indices(' ')
        .map(|(i, _)| format!("{}\u{2026}", family[..i].trim_end()))
        .filter(|cut| fits(cut))
        .collect();
    if let Some(head) = said.strip_suffix('\u{2026}') {
        let at_word = family.starts_with(head) && family[head.len()..].starts_with(' ');
        return (!at_word || cuts.last().map(String::as_str) != Some(said))
            .then(|| format!("{}, not the last word boundary that fits", at()));
    }
    if !cuts.is_empty() {
        return Some(format!("{}, where {:?} fits", at(), cuts.last()));
    }
    let want = if fits(noun) { noun } else { "" };
    (said != want).then(|| format!("{}, not {want:?}", at()))
}

/// A4 §8 and §9's fit, over a Mac's name, a VirtualMac's, a ThinkPad's (its DMI name the
/// vendor and product, its family the shorter form) and an unread machine's, in every slot
/// the Machine row has at every whole width from 160 to 3200 pt, in Folded's block B and in
/// Full, with and without units, on either face: the name is the first form that fits whole,
/// else its family cut at the last word boundary that fits, with an ellipsis, never
/// mid-word, else the OS's noun where it fits, else nothing, and never wider than its slot.
/// The row's four glyphs keep their place whether or not they draw, end where its % ends (or
/// its well, with no %), short of the strip, and the name's slot ends 3 pt short of them. At
/// 1039 pt this Mac names "MacBook Air" in block B's 67 pt and in Full's 103, and at 581 pt
/// in Full's 70 beside its 13 pt units column; at 940 pt, where block B lays no units, its
/// 49 pt take the noun, as Full's narrow 54 pt and block B's slim 36 do at 500 pt.
/// CONTROL: [`FaceMetrics::fit`]'s cut of the last form cuts mid-word.
#[test]
fn the_machine_name_fits_its_slot() {
    type Fixture = (&'static [&'static str], &'static str);
    const NAMES: [Fixture; 4] = [
        (&["MacBook Air"], "Mac"),
        (&["Virtual Machine"], "Mac"),
        (
            &["Lenovo ThinkPad X1 Carbon Gen 12", "ThinkPad X1 Carbon"],
            "Computer",
        ),
        (&[], "Computer"),
    ];
    let mac = this_mac();
    let mut slots_seen = 0;
    for m in [hn(), FaceMetrics::test_stack()] {
        let mut slots: Vec<f64> = Vec::new();
        for facts in [UnitFacts::NONE, mac] {
            for w in (160..=3200).map(f64::from) {
                let b = plan_folded_with(w, &m, &facts).blocks[1];
                let full = plan_full_with(w, &m, &facts);
                let rows = [
                    (b.machine, b.name, b.well, b.pct, b.strip, b.rows.cell),
                    (
                        full.machine,
                        full.name,
                        full.well,
                        full.pct,
                        full.strip,
                        full.rows.cell,
                    ),
                ];
                for (machine, name, well, pct, strip, cell) in rows {
                    let row = machine.expect("a Machine row");
                    let what = format!("{facts:?} at {w} pt: {row:?}");
                    let glyph = cell.ceil();
                    assert_eq!(row.glyphs.w, 4.0 * glyph + 6.0, "{what}");
                    assert_eq!(row.glyphs.end(), pct.map_or(well.end(), Col::end), "{what}");
                    assert!(row.glyphs.end() <= strip.x, "{what}");
                    assert_eq!(row.name.x, name.x, "{what}");
                    assert_eq!(
                        row.name.w,
                        (row.glyphs.x - NAME_TO_GLYPHS - name.x).max(0.0),
                        "{what}"
                    );
                    if !slots.contains(&row.name.w) {
                        slots.push(row.name.w);
                    }
                }
            }
        }
        for &(forms, noun) in &NAMES {
            for &room in &slots {
                let said = machine_name(forms, noun, room, &m);
                if let Some(fault) = name_fault(forms, noun, room, &said, &m) {
                    panic!("{fault}");
                }
            }
        }
        slots_seen += slots.len();
    }
    assert!(slots_seen > 2);

    let m = hn();
    let slot = |w: f64| {
        let b = plan_folded_with(w, &m, &mac).blocks[1];
        let full = plan_full_with(w, &m, &mac);
        [b.machine, full.machine].map(|row| row.expect("a Machine row").name.w)
    };
    assert_eq!((slot(1039.0), slot(940.0)[0]), ([67.0, 103.0], 49.0));
    assert_eq!((slot(500.0), slot(581.0)[1]), ([36.0, 54.0], 70.0));
    let named = |forms: &[&str], noun: &str, room: f64| machine_name(forms, noun, room, &m);
    let [air, virtual_mac, thinkpad, unread] = NAMES;
    assert_eq!(named(air.0, air.1, 67.0), "MacBook Air");
    assert_eq!(named(air.0, air.1, 49.0), "Mac");
    assert_eq!(named(air.0, air.1, 54.0), "Mac");
    assert_eq!(named(air.0, air.1, 70.0), "MacBook Air");
    assert_eq!(named(virtual_mac.0, virtual_mac.1, 67.0), "Virtual\u{2026}");
    assert_eq!(named(thinkpad.0, thinkpad.1, 67.0), "ThinkPad\u{2026}");
    assert_eq!(named(thinkpad.0, thinkpad.1, 103.0), "ThinkPad X1 Carbon");
    assert_eq!(named(unread.0, unread.1, 67.0), "Computer");
    assert_eq!(named(unread.0, unread.1, 10.0), "");
    let cut = m.fit(air.0, 49.0, MACHINE_NAME_PT, true);
    assert!(
        name_fault(air.0, air.1, 49.0, &cut, &m).is_some(),
        "CONTROL: {cut:?}"
    );
}
