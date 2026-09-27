// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: the round-14 engine invariants (design ruling 259) — the band's
//! overflow row never hides live progress and is ONE link that names what it
//! hides; a record about a previous run ranks a class lower than its own; and
//! the numbers on a progress row agree with each other.

use crate::center::MessageCenter;
use crate::glass::{Hit, Links, Presentation, RowKind, job_stats};
use crate::log::MessageLog;
use crate::model::{
    Amount, Decision, Hold, Intent, Message, MessageId, Meter, Severity, Unit, WallStamp, tags,
};
use crate::text::char_width;
use crate::{Duration, HOLD_ASK, Instant, STALE_STRAIN, STALE_UPDATE, STRAIN_KEY};

fn stamp() -> WallStamp {
    WallStamp { unix_ms: 1 }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn present(c: &MessageCenter, cols: usize) -> Presentation {
    c.presentation(cols, &char_width, None, Links::Painted)
}

/// A download a person started: determinate, with an amount (an ETA), live.
fn download(title: &str, done: u64) -> Message {
    let total = 74_000_000;
    Message::new(tags::UPDATE, Severity::Info, title)
        .meter(Meter {
            stats: format!("{} MB / 74 MB", done / 1_000_000),
            amount: Some(Amount {
                series: Amount::series_of(title),
                done,
                total,
                unit: Unit::Bytes,
            }),
            ..Meter::default()
        })
        .hold(Hold::Live {
            stale_after: STALE_UPDATE,
        })
}

/// The five kinds of the round-14 composition (band/95-five-kinds): a
/// misspelled setting, the download at 45 %, the strain gauge, `aterm crashed
/// last time` (a retrospective Error) and a dead screen-reader bridge (a
/// standing Error). Returns the center and the download's id.
fn five_kinds(now: Instant) -> (MessageCenter, MessageId) {
    let mut c = MessageCenter::new(MessageLog::empty(), now - ms(5000));
    let near = now - ms(4000);
    c.post(
        Message::new(tags::CONFIG, Severity::Warn, "Misspelled setting")
            .line("windw_padding \u{2192} window_padding"),
        stamp(),
        near,
    );
    let dl = c
        .post(
            download("Downloading aterm v0.91.0", 33_300_000),
            stamp(),
            near,
        )
        .id;
    c.post(
        Message::new(tags::SYSTEM, Severity::Info, "Typing slowed by yes")
            .key(STRAIN_KEY)
            .hold(Hold::Live {
                stale_after: STALE_STRAIN,
            })
            .meter(Meter::level(740, "6 of 8 cores")),
        stamp(),
        near,
    );
    c.post(
        Message::new(tags::CRASH, Severity::Error, "aterm crashed last time")
            .retrospective()
            .hold(Hold::For(Duration::from_secs(60))),
        stamp(),
        now - ms(3000),
    );
    c.post(
        Message::new(tags::A11Y, Severity::Error, "Screen reader access lost").hold(Hold::Standing),
        stamp(),
        now - ms(3000),
    );
    c.commit_rows(now, 3);
    (c, dl)
}

/// THE PROGRESS ROW IS NEVER THE ONE HIDDEN (ruling 259): five kinds at 60, 80
/// and 120 columns keep the download on the glass with its fill, the dead
/// screen-reader bridge (happening now) above it, and the overflow row a
/// single link with no capsule.
#[test]
fn five_kinds_keep_the_download_on_the_glass_at_every_width() {
    let now = Instant::now() + Duration::from_secs(10);
    let (c, dl) = five_kinds(now);
    assert_eq!(c.committed_rows(), 3);
    for cols in [60usize, 80, 120] {
        let p = present(&c, cols);
        assert_eq!(p.rows.len(), 3, "{cols}");
        let kinds: Vec<RowKind> = p.rows.iter().map(|r| r.kind).collect();
        let row = p
            .rows
            .iter()
            .find(|r| r.kind == RowKind::Message(dl))
            .unwrap_or_else(|| panic!("{cols}: the download is on the glass: {kinds:?}"));
        assert_eq!(
            row.meter.map(|(col, width, _)| (col, width)),
            Some((0, cols)),
            "{cols}: its fill is the whole row"
        );
        assert_eq!(p.rows[0].full_title, "Screen reader access lost", "{cols}");
        let overflow = &p.rows[2];
        assert_eq!(overflow.kind, RowKind::Overflow { hidden: 3 }, "{cols}");
        assert!(overflow.capsules.is_empty(), "{cols}: one link, no capsule");
        assert_eq!(overflow.title.1, "3 more \u{203a}", "{cols}");
        assert_eq!(overflow.full_title, "3 more messages", "{cols}");
        for col in [0, cols / 2, cols - 1] {
            assert_eq!(p.hit(2, col), Hit::Overflow, "{cols}/{col}");
        }
    }
}

/// A PAST RUN RANKS A CLASS LOWER (ruling 259): `aterm crashed last time` is
/// an Error about the previous run; beside a Warn about now it takes the row
/// below it, where without the flag it would take the row above.
#[test]
fn a_retrospective_error_ranks_with_the_warnings() {
    let now = Instant::now();
    for (retro, want_top) in [
        (true, "Misspelled setting"),
        (false, "aterm crashed last time"),
    ] {
        let mut c = MessageCenter::new(MessageLog::empty(), now);
        c.post(
            Message::new(tags::CONFIG, Severity::Warn, "Misspelled setting"),
            stamp(),
            now,
        );
        let mut crash = Message::new(tags::CRASH, Severity::Error, "aterm crashed last time");
        if retro {
            crash = crash.retrospective();
        }
        assert_eq!(crash.retrospective, retro);
        c.post(crash, stamp(), now + ms(1));
        c.post(
            Message::new(tags::FABRIC, Severity::Info, "a third"),
            stamp(),
            now + ms(2),
        );
        c.commit_rows(now + ms(3), 2);
        let p = present(&c, 120);
        assert_eq!(p.rows[0].full_title, want_top, "retrospective {retro}");
    }
}

/// THE OVERFLOW ROW NAMES THE PROGRESS IT HIDES (ruling 259): at two rows the
/// glass keeps one download; the other, behind the overflow row, is named
/// with its percent — `+ Downloading … 20% · 1 more ›` — and the row falls back
/// to `N more ›` where that does not fit whole (60 columns). Strings pinned.
#[test]
fn the_overflow_row_names_a_hidden_download_and_degrades_to_a_count() {
    let now = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    let first = c
        .post(
            download("Downloading aterm v0.91.0", 33_300_000),
            stamp(),
            now,
        )
        .id;
    c.post(
        download("Downloading Homebrew", 14_800_000),
        stamp(),
        now + ms(1),
    );
    c.post(
        Message::new(tags::CONFIG, Severity::Info, "a record-shaped row"),
        stamp(),
        now + ms(2),
    );
    c.commit_rows(now + ms(3), 2);
    let pins = [
        (
            120,
            '+',
            "Downloading Homebrew 20% \u{b7} 1 more \u{203a}",
            "Downloading Homebrew 20% \u{b7} 1 more messages",
        ),
        (
            80,
            '+',
            "Downloading Homebrew 20% \u{b7} 1 more \u{203a}",
            "Downloading Homebrew 20% \u{b7} 1 more messages",
        ),
        (30, '\u{2026}', "2 more \u{203a}", "2 more messages"),
    ];
    for (cols, glyph, words, spoken) in pins {
        let p = present(&c, cols);
        assert_eq!(p.rows.len(), 2, "{cols}");
        assert_eq!(p.rows[0].kind, RowKind::Message(first), "{cols}");
        assert_eq!(p.rows[1].kind, RowKind::Overflow { hidden: 2 }, "{cols}");
        assert_eq!(p.rows[1].glyph.1, glyph, "{cols}");
        assert_eq!(p.rows[1].title.1, words, "{cols}");
        assert_eq!(p.rows[1].full_title, spoken, "{cols}");
        assert!(p.rows[1].capsules.is_empty(), "{cols}");
    }
    // A 60-column band: the long form is 43 cells, it fits whole.
    let p = present(&c, 60);
    assert_eq!(
        p.rows[1].title.1,
        "Downloading Homebrew 20% \u{b7} 1 more \u{203a}"
    );
    // With the links withheld there is no arrow.
    let p = c.presentation(120, &char_width, None, Links::Withheld);
    assert_eq!(p.rows[1].title.1, "Downloading Homebrew 20% \u{b7} 1 more");
    // A longer hidden title at 60 columns does not fit whole: `N more ›`.
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    c.post(
        download("Downloading aterm v0.91.0", 33_300_000),
        stamp(),
        now,
    );
    c.post(
        download(
            "Downloading the Xcode Command Line Tools for this Mac",
            14_800_000,
        ),
        stamp(),
        now + ms(1),
    );
    c.post(
        Message::new(tags::CONFIG, Severity::Info, "a record-shaped row"),
        stamp(),
        now + ms(2),
    );
    c.commit_rows(now + ms(3), 2);
    let p = present(&c, 60);
    assert_eq!(p.rows[1].glyph.1, '\u{2026}');
    assert_eq!(p.rows[1].title.1, "2 more \u{203a}");
    assert_eq!(present(&c, 120).rows[1].glyph.1, '+');
}

/// ASKS STILL RANK FIRST (ruling 259): the reservation never pushes an ask off
/// the glass — with one slot and an ask, the download waits (and the overflow
/// row names it).
#[test]
fn an_ask_keeps_its_row_over_the_reserved_progress() {
    let now = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    let ask = c
        .post(
            Message::new(tags::A11Y, Severity::Warn, "Allow file access?")
                .action(Intent::NotNow {
                    decision: Decision::FileAccess,
                })
                .hold(Hold::Ask { for_: HOLD_ASK }),
            stamp(),
            now,
        )
        .id;
    c.post(
        download("Downloading aterm v0.91.0", 33_300_000),
        stamp(),
        now + ms(1),
    );
    c.post(
        Message::new(tags::CONFIG, Severity::Warn, "Misspelled setting"),
        stamp(),
        now + ms(2),
    );
    c.post(
        Message::new(tags::FABRIC, Severity::Info, "a record-shaped row"),
        stamp(),
        now + ms(3),
    );
    c.commit_rows(now + ms(4), 2);
    let p = present(&c, 120);
    assert_eq!(p.rows[0].kind, RowKind::Message(ask));
    assert_eq!(
        p.rows[1].title.1,
        "Downloading aterm v0.91.0 45% \u{b7} 2 more \u{203a}"
    );
    // One more row: the download takes it, ahead of the warning.
    c.commit_rows(now + ms(5), 3);
    let p = present(&c, 120);
    assert_eq!(p.rows.len(), 3);
    assert_eq!(p.rows[0].kind, RowKind::Message(ask));
    assert_eq!(p.rows[1].full_title, "Downloading aterm v0.91.0");
    assert_eq!(p.rows[2].title.1, "2 more \u{203a}");
}

/// THE NUMBERS AGREE (ruling 259): a count the row shows fills its bar — `3 of
/// 4 tabs` is 75 %, whatever fill the reporter sent — a measured level keeps
/// its gauge, a busy row stays a comet, and a size beside the percent says
/// what it is part of (`33 of 74 MB`, never a bare `74 MB`).
#[test]
fn a_rows_count_and_percent_agree_and_a_size_has_its_referent() {
    let counted = |fill: Option<u16>, stats: &str| {
        Meter {
            fill_permille: fill,
            stats: stats.into(),
            ..Meter::default()
        }
        .normalized()
        .fill_permille
    };
    assert_eq!(counted(Some(800), "3 of 4 tabs"), Some(750));
    assert_eq!(counted(Some(500), "2 of 4 tabs"), Some(500));
    assert_eq!(counted(Some(1000), "4 of 4 tabs"), Some(1000));
    assert_eq!(
        counted(Some(420), "3 of 10 programs \u{b7} ~3 GB"),
        Some(300)
    );
    // With an amount the fill is measured finer than the count, and kept
    // inside the count's span: 36 % beside `3 of 10 programs` stands, 45 %
    // is held at the fourth program's 40 %, 20 % is raised to 30 %.
    let measured = |fill: u16, stats: &str| {
        Meter {
            fill_permille: Some(fill),
            stats: stats.into(),
            amount: Some(Amount {
                series: 1,
                done: 356,
                total: 1000,
                unit: Unit::Steps,
            }),
            ..Meter::default()
        }
        .normalized()
        .fill_permille
    };
    assert_eq!(measured(356, "3 of 10 programs"), Some(356));
    assert_eq!(measured(450, "3 of 10 programs"), Some(400));
    assert_eq!(measured(200, "3 of 10 programs"), Some(300));
    assert_eq!(measured(900, "10 of 10 programs"), Some(1000));
    // Not a whole-number count: the reporter's fill stands.
    assert_eq!(counted(Some(420), "1.2M of 3.4M lines"), Some(420));
    assert_eq!(counted(Some(420), "31 MB / 74 MB"), Some(420));
    assert_eq!(
        counted(Some(420), "5 of 4 tabs"),
        Some(420),
        "past its total"
    );
    // A busy row stays a comet; a level keeps its gauge.
    let busy = Meter::busy("3 of 4 tabs").normalized();
    assert_eq!(busy.fill_permille, None);
    assert!(busy.busy);
    assert_eq!(
        Meter::level(740, "6 of 8 cores").normalized().fill_permille,
        Some(740)
    );
    // The painted percent is the count's.
    let now = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    c.post(
        Message::new(tags::SESSION, Severity::Info, "Saving the session")
            .meter(Meter {
                fill_permille: Some(800),
                stats: "3 of 4 tabs".into(),
                ..Meter::default()
            })
            .hold(Hold::Live {
                stale_after: Duration::from_secs(120),
            }),
        stamp(),
        now,
    );
    c.commit_rows(now, 1);
    let p = present(&c, 120);
    assert_eq!(p.rows[0].pct.as_ref().map(|(_, t)| t.as_str()), Some("75%"));
    assert_eq!(
        p.rows[0].stats.as_ref().map(|(_, t)| t.as_str()),
        Some("3 of 4 tabs")
    );
    // A size beside the percent: part of the job, both figures.
    assert_eq!(
        job_stats("Downloading aterm v0.91.0", "33 MB / 74 MB").as_deref(),
        Some("33 of 74 MB")
    );
    assert_eq!(
        job_stats("Downloading aterm v0.91.0", "512 KB / 1.2 GB").as_deref(),
        Some("512 KB of 1.2 GB")
    );
}

/// A CORRECTION CUT KEEPS THE FIX (ruling 261): `windw_padding →
/// window_padding` whole where it fits, else the correction alone — never
/// the typo.
#[test]
fn a_cut_correction_keeps_the_fix_never_the_typo() {
    use crate::text::shape_detail;
    let excerpt = "windw_padding \u{2192} window_padding";
    assert_eq!(shape_detail(excerpt, 40), excerpt);
    assert_eq!(shape_detail(excerpt, 29), "window_padding");
    assert_eq!(shape_detail(excerpt, 14), "window_padding");
    assert!(!shape_detail(excerpt, 10).contains("windw"));
}

/// A CARRIED ROW KEEPS WHAT IT WAS (ruling 263): across a seamless handoff
/// `aterm crashed last time` stays retrospective — a class below the Warn
/// about now, as it ranked in the parent — and a pass's `3 of 10 programs`
/// measured at 36 % keeps 36 % though the carry ships no amount (never
/// snapped back to the count's 30 %, a visible backward jump).
#[test]
fn a_carried_row_keeps_its_rank_and_its_measured_fill() {
    let now = Instant::now();
    let mut parent = MessageCenter::new(MessageLog::empty(), now);
    parent.post(
        Message::new(tags::CONFIG, Severity::Warn, "Misspelled setting"),
        stamp(),
        now,
    );
    parent.post(
        Message::new(tags::CRASH, Severity::Error, "aterm crashed last time").retrospective(),
        stamp(),
        now + ms(1),
    );
    let pass = parent
        .post(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab tools")
                .meter(Meter {
                    fill_permille: Some(356),
                    stats: "3 of 10 programs".into(),
                    amount: Some(Amount {
                        series: 1,
                        done: 356,
                        total: 1000,
                        unit: Unit::Steps,
                    }),
                    ..Meter::default()
                })
                .hold(Hold::Live {
                    stale_after: Duration::from_secs(120),
                }),
            stamp(),
            now + ms(2),
        )
        .id;
    parent.commit_rows(now + ms(3), 3);
    let carry = parent.carried();
    assert!(
        carry
            .live
            .iter()
            .any(|m| m.title == "aterm crashed last time" && m.retrospective),
        "the carry ships the flag"
    );
    let later = now + ms(10);
    let mut child = MessageCenter::new(MessageLog::empty(), later);
    child.seed_carried(&carry, stamp(), later);
    child.commit_rows(later, 3);
    let crash = child
        .live_rows()
        .find(|l| l.msg.title == "aterm crashed last time")
        .expect("the crash row is carried");
    assert!(crash.msg.retrospective, "it stays a past run's record");
    let p = present(&child, 120);
    let titles: Vec<&str> = p.rows.iter().map(|r| r.full_title.as_str()).collect();
    let pos = |t: &str| titles.iter().position(|x| *x == t).unwrap();
    assert!(
        pos("Misspelled setting") < pos("aterm crashed last time"),
        "the past run ranks below now: {titles:?}"
    );
    let meter = child.live(pass).unwrap().msg.meter.clone().unwrap();
    assert_eq!(meter.fill_permille, Some(356), "the measured fill stands");
    assert_eq!(meter.amount, None, "no amount crossed");
}
