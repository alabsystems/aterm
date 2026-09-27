// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: the round-12 build list's engine invariants (design rulings
//! 241–248) — the time slot's one style per width, the distance-scaled glide
//! read on the display's cadence, the echoes' words, the strain row's rail
//! and plain words, and the words pass (the job's size said once, the
//! primary load, the load slot at the tail, a count never without its
//! unit).

use crate::animate::{Look, Pace, ROW};
use crate::center::{EchoKind, MessageCenter, Outcome};
use crate::glass::{Links, Presentation, RowKind, RowLayout, job_stats};
use crate::log::MessageLog;
use crate::model::{
    Amount, Hold, Intent, Load, Message, Meter, Restatement, Severity, Unit, WallStamp, tags,
};
use crate::text::char_width;
use crate::{Duration, ELAPSED_AFTER, Instant, STALE_TAILED, STALE_UPDATE};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn stamp() -> WallStamp {
    WallStamp { unix_ms: 1 }
}

fn fresh(now: Instant) -> MessageCenter {
    MessageCenter::new(MessageLog::empty(), now)
}

fn present(c: &MessageCenter, cols: usize) -> Presentation {
    c.presentation(cols, &char_width, None, Links::Painted)
}

const TOTAL: u64 = 74_000_000;

fn bytes_meter(done: u64) -> Meter {
    Meter {
        fill_permille: None,
        stats: format!("{} MB / 74 MB", done / 1_000_000),
        amount: Some(Amount {
            series: Amount::series_of("aterm 0.92.0"),
            done,
            total: TOTAL,
            unit: Unit::Bytes,
        }),
        ..Meter::default()
    }
}

fn download(done: u64) -> Message {
    Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.92.0")
        .meter(bytes_meter(done))
        .hold(Hold::Live {
            stale_after: STALE_UPDATE,
        })
        .key("update.progress")
}

fn busy(title: &str, stats: &str) -> Message {
    Message::new(tags::PACKAGES, Severity::Info, title)
        .meter(Meter::busy(stats))
        .hold(Hold::Live {
            stale_after: STALE_TAILED,
        })
}

/// What a time slot's words are: a clock (never allowed), the long or the
/// short ETA, the long or the short elapsed words, or a state word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Style {
    Clock,
    EtaLong,
    EtaShort,
    RanLong,
    RanShort,
    Word,
}

fn style(w: &str) -> Style {
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    if let Some((a, b)) = w.split_once(':')
        && digits(a)
        && b.len() == 2
        && digits(b)
    {
        return Style::Clock;
    }
    let unit_long = |u: &str| matches!(u, "s" | "min" | "h");
    let unit_short = |n: &str| {
        let split = n.find(|c: char| !c.is_ascii_digit()).unwrap_or(n.len());
        let (num, unit) = n.split_at(split);
        digits(num) && matches!(unit, "s" | "m" | "h")
    };
    if let Some(rest) = w.strip_suffix(" left") {
        if rest == "<5 s" {
            return Style::EtaLong;
        }
        if rest == "<5s" {
            return Style::EtaShort;
        }
        return match rest.split_once(' ') {
            Some((n, u)) if digits(n) && unit_long(u) => Style::EtaLong,
            None if unit_short(rest) => Style::EtaShort,
            _ => panic!("an unknown ETA form: {w:?}"),
        };
    }
    if let Some(rest) = w.strip_prefix("for ") {
        return match rest.split_once(' ') {
            Some((n, u)) if digits(n) && unit_long(u) => Style::RanLong,
            None if unit_short(rest) => Style::RanShort,
            _ => panic!("an unknown elapsed form: {w:?}"),
        };
    }
    assert!(
        matches!(w, "stalled" | "failed"),
        "an unknown time word: {w:?}"
    );
    Style::Word
}

/// THE TIME SLOT HAS ONE STYLE PER WIDTH (design ruling 241): over every
/// row that has a time slot — a download with its ETA latched, the same
/// download before it latches, a stalled one, a busy row past ten seconds, a
/// measured level, and their Fault and Vanish echoes — at 60, 80, 120 and
/// 160 columns and in both looks, the slot's words never read as a bare
/// `m:ss` clock; a slot laid out LONG says `30 s left` / `for 41 s`, one laid
/// out SHORT says `30s left` / `for 41s` — the declared sacrifice form, and
/// nothing else; a determinate row whose estimate is hidden leaves its slot
/// blank; and an ETA never carries a `~`.
#[test]
fn the_time_slot_never_shows_a_bare_clock_and_keeps_one_style_per_width() {
    let mut seen = std::collections::BTreeSet::new();
    let scenes: Vec<(&str, Box<dyn Fn(Instant) -> (MessageCenter, Instant)>)> = vec![
        (
            "download, latched",
            Box::new(|now| {
                let mut c = fresh(now);
                let id = c.post(download(1_000_000), stamp(), now).id;
                c.commit_rows(now, 3);
                let mut t = now;
                for k in 1..=16u64 {
                    t = now + ms(500 * k);
                    c.restate(
                        id,
                        Restatement {
                            meter: Some(Some(bytes_meter(1_000_000 + 900_000 * k))),
                            ..Restatement::default()
                        },
                        t,
                    );
                }
                (c, t + ms(40))
            }),
        ),
        (
            "download, one read",
            Box::new(|now| {
                let mut c = fresh(now);
                c.post(download(31_000_000), stamp(), now);
                c.commit_rows(now, 3);
                (c, now + ms(7_000))
            }),
        ),
        (
            "download, stalled",
            Box::new(|now| {
                let mut c = fresh(now);
                let id = c.post(download(1_000_000), stamp(), now).id;
                c.commit_rows(now, 3);
                c.restate(
                    id,
                    Restatement {
                        meter: Some(Some(bytes_meter(2_000_000))),
                        ..Restatement::default()
                    },
                    now + ms(1000),
                );
                (c, now + ms(30_000))
            }),
        ),
        (
            "busy, 41 s",
            Box::new(|now| {
                let mut c = fresh(now);
                c.post(busy("Checking ALab tools", ""), stamp(), now);
                c.commit_rows(now, 3);
                (c, now + ms(41_300))
            }),
        ),
        (
            "busy, 3 min",
            Box::new(|now| {
                let mut c = fresh(now);
                c.post(busy("Waiting for another ALab update", ""), stamp(), now);
                c.commit_rows(now, 3);
                (c, now + ms(200_000))
            }),
        ),
        (
            "level, 30 s",
            Box::new(|now| {
                let mut c = fresh(now);
                c.post(
                    Message::new(tags::SYSTEM, Severity::Info, "Typing slowed by 'yes'")
                        .key(crate::STRAIN_KEY)
                        .hold(Hold::Live {
                            stale_after: crate::STALE_STRAIN,
                        })
                        .meter(Meter::level(700, "6 of 8 cores")),
                    stamp(),
                    now,
                );
                c.commit_rows(now, 3);
                (c, now + ms(30_000))
            }),
        ),
        (
            "busy, long title",
            Box::new(|now| {
                let mut c = fresh(now);
                c.post(
                    busy("Waiting for another ALab tools update", "10 programs"),
                    stamp(),
                    now,
                );
                c.commit_rows(now, 3);
                (c, now + ms(95_000))
            }),
        ),
        (
            "download, long title, latched",
            Box::new(|now| {
                let mut c = fresh(now);
                let msg = Message::new(
                    tags::UPDATE,
                    Severity::Info,
                    "Downloading the aterm v0.92.0 update",
                )
                .meter(bytes_meter(1_000_000))
                .hold(Hold::Live {
                    stale_after: STALE_UPDATE,
                })
                .action(Intent::StopPaste { session: 1 });
                let id = c.post(msg, stamp(), now).id;
                c.commit_rows(now, 3);
                let mut t = now;
                for k in 1..=16u64 {
                    t = now + ms(500 * k);
                    c.restate(
                        id,
                        Restatement {
                            meter: Some(Some(bytes_meter(1_000_000 + 900_000 * k))),
                            ..Restatement::default()
                        },
                        t,
                    );
                }
                (c, t + ms(40))
            }),
        ),
        (
            "busy fault echo",
            Box::new(|now| {
                let mut c = fresh(now);
                let id = c.post(busy("Installing Homebrew", ""), stamp(), now).id;
                c.commit_rows(now, 3);
                assert!(c.resolve(id, Outcome::Warn, now + ms(20_000)));
                (c, now + ms(20_100))
            }),
        ),
        (
            "busy vanish echo",
            Box::new(|now| {
                let mut c = fresh(now);
                let id = c.post(busy("Installing Homebrew", ""), stamp(), now).id;
                c.commit_rows(now, 3);
                assert!(c.withdraw(id, now + ms(20_000)));
                (c, now + ms(20_100))
            }),
        ),
    ];
    for (name, scene) in &scenes {
        // The four widths the captures are shot at, and — so every declared
        // short form is met — every width from 40 to 200 for the rows long
        // enough to need one.
        let widths: Vec<usize> = if name.contains("long title") {
            (40..=200).collect()
        } else {
            vec![60, 80, 120, 160]
        };
        for cols in widths {
            for look in [Look::MOVING, Look::STILL] {
                let now = Instant::now();
                let (c, at) = scene(now);
                let p = present(&c, cols);
                let m = c.motion(&p, at, look);
                for (row, rm) in p.rows.iter().zip(&m.rows) {
                    let check = |words: &Option<String>, short: bool, eta: bool| {
                        let Some(w) = words else { return };
                        let st = style(w);
                        assert_ne!(st, Style::Clock, "{name}@{cols}: a bare clock {w:?}");
                        assert!(!w.contains('~'), "{name}@{cols}: {w:?}");
                        let want = match (eta, short) {
                            (true, false) => Style::EtaLong,
                            (true, true) => Style::EtaShort,
                            (false, false) => Style::RanLong,
                            (false, true) => Style::RanShort,
                        };
                        assert!(
                            st == want || st == Style::Word,
                            "{name}@{cols} {look:?}: {w:?} is {st:?} in a {want:?} slot"
                        );
                    };
                    if row.eta.is_some() {
                        check(&rm.eta, row.eta_short, true);
                    }
                    if row.elapsed.is_some() {
                        check(&rm.readout, row.elapsed_short, false);
                    }
                    for w in rm.eta.iter().chain(&rm.readout) {
                        seen.insert(format!("{cols}: {w} ({:?})", style(w)));
                    }
                    if *name == "download, one read" {
                        assert!(row.eta.is_some() || cols < 80, "{name}@{cols}: reserved");
                        assert_eq!(rm.eta, None, "{name}@{cols}: blank until it latches");
                    }
                }
            }
        }
    }
    // The scenes did reach every style they claim.
    let all: String = seen.iter().cloned().collect::<Vec<_>>().join("\n");
    for needle in [
        "for 41 s",
        "for 3 min",
        "for 29 s",
        " left",
        "stalled",
        "failed",
        "(RanShort)",
        "(EtaShort)",
    ] {
        assert!(all.contains(needle), "no {needle:?} in:\n{all}");
    }
}

/// THE GLIDE SCALES WITH ITS DISTANCE AND IS READ ON THE DISPLAY'S CADENCE
/// (design ruling 245): at 80 columns and 60 Hz, a jump of up to fifty
/// points moves the edge at most 2.5 cells from one display frame to the
/// next, monotonically toward the data, and lands within the 600 ms cap; the
/// deadline asks the display's frames while it flies and — once it lands on
/// a row that asks nothing else — no wake at all.
#[test]
fn a_glide_moves_at_most_two_and_a_half_cells_a_frame_and_then_sleeps() {
    let cols = 80usize;
    let cell = f64::from(ROW) / cols as f64;
    // FLAT: no glint, so a landed bar with no amount has nothing else to
    // draw and must arm nothing.
    let flat = Look {
        pace: Pace::Moving,
        graded: false,
    };
    for (from, to) in [
        (0u16, 500u16),
        (500, 0),
        (100, 600),
        (300, 350),
        (420, 421),
        (999, 499),
        (0, 1000),
    ] {
        let now = Instant::now();
        let mut c = fresh(now);
        let row = |p: u16| {
            Message::new(tags::PACKAGES, Severity::Info, "Indexing the photo library")
                .meter(Meter {
                    fill_permille: Some(p),
                    ..Meter::default()
                })
                .hold(Hold::Live {
                    stale_after: STALE_TAILED,
                })
        };
        let id = c.post(row(from), stamp(), now).id;
        c.commit_rows(now, 3);
        let t0 = now + ms(1000);
        c.restate(
            id,
            Restatement {
                meter: Some(row(to).meter),
                ..Restatement::default()
            },
            t0,
        );
        let p = present(&c, cols);
        let edge = |t: Instant| {
            let s = &c.motion(&p, t, flat).rows[0].surface;
            match s.edge {
                Some(e) => f64::from(e),
                None if s.stops.iter().any(|x| x.tone.fill > 0 && x.at > 0) => f64::from(ROW),
                None => 0.0,
            }
        };
        let span = crate::animate::glide_span(from, to);
        assert!(span <= crate::FILL_GLIDE_MAX);
        // Walk the display's frames (60 Hz) through the glide.
        let mut t = c.fine_instant(t0);
        let mut prev = edge(t);
        let end = t0 + span + c.refresh() * 2;
        let dir = f64::from(i32::from(to) - i32::from(from)).signum();
        while t < end {
            t += c.refresh();
            let e = edge(c.fine_instant(t));
            let moved = (e - prev) * dir;
            assert!(moved >= 0.0, "{from}→{to}: the edge went back at {t:?}");
            if from.abs_diff(to) <= 500 {
                assert!(
                    moved / cell <= 2.5,
                    "{from}→{to}: {:.2} cells in one frame",
                    moved / cell
                );
            }
            prev = e;
        }
        let want = f64::from(to) * f64::from(ROW) / 1000.0;
        assert!((prev - want).abs() <= 1.0, "{from}→{to}: landed at {prev}");
        // While it flies the deadline is a display frame, not the grid's.
        let mid = t0 + span / 3;
        if from.abs_diff(to) >= 50
            && let Some(d) = c.motion_deadline(&p, mid, flat)
        {
            assert!(
                d <= c.fine_instant(mid) + c.refresh() * 2,
                "{from}→{to}: a display frame while it flies: {:?}",
                d - mid
            );
        }
        // Landed: nothing else moves on this row, so nothing wakes.
        let landed = t0 + span + ms(100);
        assert_eq!(
            c.motion_deadline(&p, landed, flat),
            None,
            "{from}→{to}: an idle wake after the glide landed"
        );
    }
}

/// ✓ AND THE FINISHED WORDS ONLY (ruling 244): no echo, of any kind, in any
/// look and at any width, paints `100%` beside `done` — the words it draws
/// are its title, `failed`, or the frozen elapsed words.
#[test]
fn no_echo_says_100_percent_done() {
    for kind in [EchoKind::Complete, EchoKind::Fault, EchoKind::Vanish] {
        for cols in [60usize, 80, 120, 160] {
            for look in [Look::MOVING, Look::STILL] {
                let now = Instant::now();
                let mut c = fresh(now);
                let dl = c.post(download(70_000_000), stamp(), now).id;
                let paste = busy("Pasting 4.2 MB", "").finished_as("Pasted 4.2 MB");
                let b = c.post(paste, stamp(), now).id;
                c.commit_rows(now, 3);
                let at = now + ms(12_000);
                for id in [dl, b] {
                    match kind {
                        EchoKind::Complete => assert!(c.resolve(id, Outcome::Ok, at)),
                        EchoKind::Fault => assert!(c.resolve(id, Outcome::Warn, at)),
                        EchoKind::Vanish => assert!(c.withdraw(id, at)),
                    }
                }
                let p = present(&c, cols);
                for t in [0u64, 100, 400, 900] {
                    let m = c.motion(&p, at + ms(t), look);
                    for (row, rm) in p.rows.iter().zip(&m.rows) {
                        assert!(matches!(row.kind, RowKind::Echo(_)));
                        let words = format!(
                            "{} {} {} {}",
                            row.title.1,
                            row.pct.as_ref().map_or("", |p| p.1.as_str()),
                            rm.eta.as_deref().unwrap_or(""),
                            rm.readout.as_deref().unwrap_or("")
                        );
                        assert!(
                            !words.contains("done") && !words.contains("100%"),
                            "{kind:?}@{cols}: {words:?}"
                        );
                        if kind == EchoKind::Complete {
                            assert!(row.pct.is_none() && rm.eta.is_none() && rm.readout.is_none());
                        }
                    }
                }
            }
        }
    }
}

/// THE STRAIN ROW IS A RAIL WITH PLAIN WORDS (ruling 243): its surface is a
/// rail at every level, glides both ways at the display's cadence, and its
/// layout has no load slot; and the rail's geometry is the level mapped onto
/// the whole row (0 % its left edge, 100 % its right).
#[test]
fn the_strain_row_is_a_rail_across_the_whole_window() {
    let now = Instant::now();
    let mut c = fresh(now);
    let row = |pm: u16| {
        Message::new(tags::SYSTEM, Severity::Info, "Typing slowed by 'yes'")
            .key(crate::STRAIN_KEY)
            .hold(Hold::Live {
                stale_after: crate::STALE_STRAIN,
            })
            .meter(Meter {
                load: Some(Load::Cpu),
                ..Meter::level(pm, "6 of 8 cores")
            })
            .loads([Load::Cpu])
            .action(Intent::ShowTab { tab: 2, window: 1 })
    };
    let id = c.post(row(250), stamp(), now).id;
    c.commit_rows(now, 3);
    for cols in [60usize, 80, 120] {
        let p = present(&c, cols);
        let l = &p.rows[0];
        assert!(l.load_slot.is_none() && l.load.is_none(), "@{cols}");
        assert!(
            l.capsules.iter().any(|cap| cap.full_label == "Show tab 2"),
            "@{cols}: the navigation to the tab the title names"
        );
        for look in [Look::MOVING, Look::STILL] {
            let s = &c.motion(&p, now + ms(500), look).rows[0].surface;
            assert!(s.rail, "@{cols} {look:?}");
            let e = s.edge.expect("a level inside the row has an edge");
            assert!(
                (i64::from(e) - i64::from(ROW) / 4).abs() <= 1,
                "25 % of the row"
            );
        }
    }
    c.restate(
        id,
        Restatement {
            meter: Some(row(900).meter),
            ..Restatement::default()
        },
        now + ms(1000),
    );
    let p = present(&c, 80);
    let mid = c.motion(&p, now + ms(1200), Look::MOVING).rows[0]
        .surface
        .clone();
    assert!(mid.rail);
    let e = f64::from(mid.edge.unwrap()) / f64::from(ROW);
    assert!(e > 0.25 && e < 0.9, "gliding: {e}");
    let before = now + ms(10_100);
    assert!(ELAPSED_AFTER <= ms(10_000));
    let rm = &c.motion(&p, before, Look::MOVING).rows[0];
    assert_eq!(rm.readout.as_deref(), Some("for 10 s"));
}

/// THE JOB'S SIZE IS SAID ONCE (ruling 246): beside a percent the stats slot
/// paints only the size of the whole job — a pair `A / B` its total, a count
/// its one grammar — hidden until the first unit arrives, and gone where the
/// title already states it; and across the band's own rows no row states
/// the same total twice.
#[test]
fn the_jobs_size_is_said_once_and_only_when_it_is_news() {
    assert_eq!(
        job_stats("Downloading aterm v0.92.0", "45 MB / 74 MB").as_deref(),
        Some("45 of 74 MB"),
        "a size says what it is part of (ruling 259)"
    );
    assert_eq!(job_stats("Downloading aterm v0.92.0", "0 B / 200 MB"), None);
    assert_eq!(
        job_stats("Downloading aterm v0.92.0", "   0 B / 4.2 MB"),
        None
    );
    assert_eq!(job_stats("Pasting 4.2 MB", "1.1 MB / 4.2 MB"), None);
    // The title says it only as WHOLE words: `12 GB` is not `2 GB`, and a
    // wire script's title is free text (review round 12).
    assert_eq!(
        job_stats("Uploading 12 GB backup", "1 GB / 2 GB").as_deref(),
        Some("1 of 2 GB")
    );
    assert_eq!(
        job_stats("Uploading 14.2 MB", "1 MB / 4.2 MB").as_deref(),
        Some("1 of 4.2 MB")
    );
    assert_eq!(job_stats("Uploading the 2 GB backup", "1 GB / 2 GB"), None);
    assert_eq!(job_stats("Copying 2 GB", "1 GB / 2 GB"), None);
    assert_eq!(
        job_stats("Copying 2 GBs", "1 GB / 2 GB").as_deref(),
        Some("1 of 2 GB")
    );
    assert_eq!(
        job_stats("Rewrapping 3.4M lines", "1.2M of 3.4M lines"),
        None
    );
    assert_eq!(
        job_stats("Installing ALab tools", "3 of 10 programs").as_deref(),
        Some("3 of 10 programs")
    );
    assert_eq!(job_stats("Installing ALab tools", "0 of 10 programs"), None);
    assert_eq!(
        job_stats("Saving the session", "3 of 4 tabs \u{b7} 12 MB / 40 MB").as_deref(),
        Some("3 of 4 tabs \u{b7} 12 of 40 MB")
    );
    // Over the band's own rows, at every width: the total never twice.
    let rows: Vec<Message> = vec![
        crate::waits::paste_row(3, 1, 1_100_000, 4_200_000, Duration::ZERO),
        crate::waits::rewrap_row(3, 1_200_000, 3_400_000, Duration::ZERO),
        download(45_000_000),
    ];
    for msg in rows {
        for cols in [60usize, 80, 120, 160] {
            let now = Instant::now();
            let mut c = fresh(now);
            let _ = c.post(msg.clone(), stamp(), now);
            let _ = c.settle(now, true);
            c.commit_rows(now, 3);
            let p = present(&c, cols);
            let l = &p.rows[0];
            if let Some((_, stats)) = &l.stats {
                for piece in stats.split(crate::PIECE_SEP) {
                    let total = piece.rsplit_once(" / ").map_or(piece, |(_, t)| t);
                    assert!(
                        !l.title.1.contains(total),
                        "{}@{cols}: {total:?} said twice",
                        msg.title
                    );
                    assert!(
                        !piece.contains(" / "),
                        "{}@{cols}: bytes done painted",
                        msg.title
                    );
                }
            }
        }
    }
}

fn pass(load: Option<Load>) -> Message {
    Message::new(tags::PACKAGES, Severity::Info, "Installing ALab tools")
        .meter(Meter {
            fill_permille: Some(350),
            stats: "3 of 10 programs".into(),
            load,
            ..Meter::default()
        })
        .hold(Hold::Live {
            stale_after: STALE_TAILED,
        })
        .loads([Load::Network, Load::Disk, Load::Cpu])
        .primary_load(Load::Network)
}

/// THE WORK'S OWN LOAD SAYS NOTHING THE TITLE DOES NOT (ruling 246): a
/// download row's `network busy` shows only while other work is measurably
/// slowed; a load outside the work's own (its extraction making the disk
/// busy) shows as before; the slot is reserved either way, at the TAIL of
/// the words — never a mid-row hole, never led by a `·`.
#[test]
fn a_downloads_network_words_show_only_while_other_work_is_slowed() {
    for cols in [80usize, 120, 160] {
        let now = Instant::now();
        let mut c = fresh(now);
        let id = c.post(pass(Some(Load::Network)), stamp(), now).id;
        c.commit_rows(now, 3);
        let l = present(&c, cols).rows[0].clone();
        assert_eq!(l.load, None, "@{cols}: the primary load is the title's");
        let slot = l.load_slot.expect("reserved");
        let caps = l.capsules.first().map_or(cols - 1, |cap| cap.col - 2);
        assert_eq!(
            slot.0 + slot.1,
            caps,
            "@{cols}: at the tail, before the capsules"
        );
        // Slowed: the words show, in the same cells.
        c.set_slowing(true);
        let slowed = present(&c, cols).rows[0].clone();
        assert_eq!(slowed.load.map(|(_, w)| w), Some("network busy"), "@{cols}");
        assert_eq!(slowed.load_slot, l.load_slot, "@{cols}: nothing moves");
        c.set_slowing(false);
        // Another load than the work's own shows at once (it was declared
        // with the post's load, or after LOAD_AFTER).
        c.restate(
            id,
            Restatement {
                meter: Some(pass(Some(Load::Disk)).meter),
                ..Restatement::default()
            },
            now + ms(100),
        );
        let _ = c.settle(now + ms(100), true);
        let disk = present(&c, cols).rows[0].clone();
        assert_eq!(disk.load.map(|(_, w)| w), Some("disk busy"), "@{cols}");
        assert_eq!(disk.load_slot, l.load_slot, "@{cols}: nothing moves");
        let painted = crate::glass::tests::render(&disk, cols);
        let (col, words) = disk.load.unwrap();
        let before: String = painted.chars().take(col).collect();
        assert!(
            !before.trim_end().ends_with('\u{b7}'),
            "@{cols}: no `·` leads the load words: {painted:?}"
        );
        assert_eq!(
            painted
                .chars()
                .skip(col)
                .take(words.len())
                .collect::<String>(),
            words
        );
    }
}

/// THE PERCENT KEEPS THE STANDARD TWO-SPACE GAP (ruling 246): `100%` and
/// `42%` alike stand two cells clear of the title, right-aligned in four.
#[test]
fn the_percent_keeps_a_two_space_gap() {
    for p in [0u16, 50, 420, 1000] {
        let now = Instant::now();
        let mut c = fresh(now);
        c.post(
            Message::new(tags::PACKAGES, Severity::Info, "Indexing").meter(Meter {
                fill_permille: Some(p),
                ..Meter::default()
            }),
            stamp(),
            now,
        );
        c.commit_rows(now, 3);
        let l: RowLayout = present(&c, 80).rows[0].clone();
        let (col, text) = l.pct.clone().unwrap();
        let title_end = l.title.0 + char_width(&l.title.1);
        assert_eq!(col + char_width(&text), title_end + 2 + 4, "{p}: {text:?}");
        assert!(col >= title_end + 2, "{p}: {text:?}");
    }
}

/// A TITLE WITH NO PAST TENSE STILL SAYS IT FINISHED (ruling 247): a wire
/// script's free text (`notice done backup ok` on `Uploading the backup`)
/// has no declared finished words and no participle the table reads, so its
/// Complete echo keeps its title and says `done` in the time slot — never a
/// bare `✓ Uploading the backup` that reads as still going. A row with no
/// time slot keeps its `100%`. A past-tense title still says nothing there
/// (ruling 244).
#[test]
fn a_complete_echo_without_a_past_tense_says_done() {
    // The 72/73 captures' row: `done=<n>/<total> unit=bytes`, so an ETA slot.
    let bar = |title: &str| {
        Message::new(tags::PACKAGES, Severity::Info, title)
            .meter(bytes_meter(30_000_000))
            .hold(Hold::Live {
                stale_after: STALE_UPDATE,
            })
    };
    for cols in [60usize, 80, 120] {
        for look in [Look::MOVING, Look::STILL] {
            let now = Instant::now();
            let mut c = fresh(now);
            let free = c.post(bar("Uploading the backup"), stamp(), now).id;
            let past = c.post(bar("Downloading the backup"), stamp(), now).id;
            let busy_free = c.post(busy("Syncing photos", ""), stamp(), now).id;
            c.commit_rows(now, 3);
            // Long enough that each row has a time slot.
            let at = now + ms(12_000);
            let p0 = present(&c, cols);
            for l in &p0.rows {
                assert!(
                    l.eta.is_some() || l.elapsed.is_some(),
                    "@{cols}: {:?} has no time slot",
                    l.title.1
                );
            }
            for id in [free, past, busy_free] {
                assert!(c.resolve(id, Outcome::Ok, at));
            }
            let p = present(&c, cols);
            for t in [0u64, 400, 900] {
                let m = c.motion(&p, at + ms(t), look);
                for (row, rm) in p.rows.iter().zip(&m.rows) {
                    let said = rm.eta.as_deref().or(rm.readout.as_deref());
                    assert!(row.pct.is_none(), "@{cols}: {:?}", row.title.1);
                    if row.full_title.starts_with("Downloaded") {
                        assert_eq!(said, None, "@{cols}: past tense says nothing more");
                    } else {
                        assert!(
                            row.title.1.starts_with("Uploading")
                                || row.title.1.starts_with("Syncing"),
                            "@{cols}: {:?}",
                            row.title.1
                        );
                        assert_eq!(said, Some(crate::DONE_WORD), "@{cols}: {:?}", row.title.1);
                    }
                }
            }
        }
    }
    // No time slot at all (a bare `pct=`): the `100%` is the word left.
    let now = Instant::now();
    let mut c = fresh(now);
    let pct = Message::new(tags::PACKAGES, Severity::Info, "Uploading the backup")
        .meter(Meter {
            fill_permille: Some(420),
            ..Meter::default()
        })
        .hold(Hold::Live {
            stale_after: STALE_UPDATE,
        });
    let id = c.post(pct, stamp(), now).id;
    c.commit_rows(now, 3);
    let p0 = present(&c, 80);
    assert!(p0.rows[0].eta.is_none() && p0.rows[0].elapsed.is_none());
    assert!(c.resolve(id, Outcome::Ok, now + ms(12_000)));
    let p = present(&c, 80);
    assert!(matches!(p.rows[0].kind, RowKind::Echo(_)));
    assert_eq!(p.rows[0].pct.as_ref().map(|p| p.1.trim()), Some("100%"));
}

/// A COUNT NEVER STANDS WITHOUT ITS UNIT (ruling 248): at 80 columns the
/// strain row's stats read `6/8`, a fraction that did not say what it
/// counts. Where the width law shortens a count it keeps its unit word
/// (`6/8 cores`, `3/10 programs`, `23/24 GB`); where even that does not fit
/// the stats go whole. Pinned at 60/80/120 for every row that carries a
/// count, and swept over every width 40–200: no painted stats is a bare
/// fraction.
#[test]
fn a_count_keeps_its_unit_at_every_width() {
    let strain = |title: &str, severity: Severity, load: Load, named: bool, stats: &str| {
        let msg = Message::new(tags::SYSTEM, severity, title)
            .key(crate::STRAIN_KEY)
            .hold(Hold::Live {
                stale_after: crate::STALE_STRAIN,
            })
            .loads([load])
            .meter(Meter {
                load: named.then_some(load),
                ..Meter::level(740, stats)
            })
            .no_excerpt();
        if named {
            msg.action(Intent::ShowTab { tab: 2, window: 1 })
        } else {
            msg
        }
    };
    let rows: [(&str, Message, [Option<&str>; 3]); 4] = [
        (
            "strain, cpu",
            strain(
                "Typing slowed by 'yes'",
                Severity::Info,
                Load::Cpu,
                true,
                "6 of 8 cores",
            ),
            // At 60 the capsules and the elapsed slot leave no room for
            // `6/8 cores`: the stats go, never as `6/8`. At 80 the title
            // without its `(tab 2)` (ruling 259) leaves room for them whole.
            [None, Some("6 of 8 cores"), Some("6 of 8 cores")],
        ),
        (
            "strain, memory",
            strain(
                "Typing slowed by low memory",
                Severity::Warn,
                Load::Memory,
                false,
                "23 of 24 GB used",
            ),
            [None, Some("23 of 24 GB used"), Some("23 of 24 GB used")],
        ),
        (
            "strain, heat",
            strain(
                "Typing slowed by heat",
                Severity::Info,
                Load::Cpu,
                false,
                "throttled \u{b7} 6 of 8 cores",
            ),
            [
                Some("6/8 cores"),
                Some("throttled \u{b7} 6 of 8 cores"),
                Some("throttled \u{b7} 6 of 8 cores"),
            ],
        ),
        (
            "pass",
            pass(None),
            [
                Some("3/10 programs"),
                Some("3 of 10 programs"),
                Some("3 of 10 programs"),
            ],
        ),
    ];
    let mut failures = Vec::new();
    for (name, msg, want) in rows {
        let now = Instant::now();
        let mut c = fresh(now);
        c.post(msg, stamp(), now);
        c.commit_rows(now, 3);
        for (cols, want) in [60usize, 80, 120].into_iter().zip(want) {
            let got = present(&c, cols).rows[0].stats.clone().map(|s| s.1);
            if got.as_deref() != want {
                failures.push(format!("{name}@{cols}: {got:?}"));
            }
        }
        for cols in 40..=200 {
            if let Some((_, s)) = present(&c, cols).rows[0].stats.clone() {
                assert!(
                    s.chars().any(char::is_alphabetic),
                    "{name}@{cols}: a bare fraction {s:?}"
                );
            }
        }
    }
    assert!(failures.is_empty(), "re-pin:\n{}", failures.join("\n"));
}
