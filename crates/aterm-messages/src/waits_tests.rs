// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::text::{glass_title_fault, title_words};

fn sample(done: u64, total: u64, ms: u64, asked: bool) -> WaitSample {
    WaitSample {
        done,
        total,
        elapsed: Duration::from_millis(ms),
        asked,
        end: None,
    }
}

#[test]
fn a_paste_the_program_does_not_read_says_what_it_waits_on() {
    let mut w = SessionWait::paste(3, 1);
    let total = 12_000_000;
    let WaitStep::Post(row) = w.step(&sample(0, total, 2_000, true)) else {
        panic!("posted");
    };
    assert!(row.detail.is_empty(), "{row:?}");
    let WaitStep::Restate(row) = w.step(&sample(0, total, 4_900, true)) else {
        panic!("restated");
    };
    assert!(row.detail.is_empty(), "under the bound: {row:?}");
    let WaitStep::Restate(row) = w.step(&sample(0, total, 39_000, true)) else {
        panic!("restated");
    };
    assert_eq!(row.detail, ["not read for 37 s"]);
    assert_eq!(row.title, "Pasting 12 MB", "the title is the job's");
    let figure_at = |row: &Message, cols: usize| {
        let now = crate::Instant::now();
        let mut c = crate::center::MessageCenter::new(crate::log::MessageLog::empty(), now);
        let _ = c.post(row.clone(), crate::model::WallStamp { unix_ms: 1_000 }, now);
        let _ = c.settle(now, true);
        c.commit_rows(now, 3);
        let p = c.presentation(cols, &crate::text::char_width, crate::glass::Links::Painted);
        p.rows
            .first()
            .and_then(|r| r.detail.clone())
            .map(|(_, d)| d)
    };
    let mut big = SessionWait::paste(3, 9);
    let _ = big.step(&sample(0, 999_000_000, 2_000, true));
    let WaitStep::Restate(wide) = big.step(&sample(0, 999_000_000, 3_601_000, true)) else {
        panic!("restated");
    };
    assert_eq!(wide.title, "Pasting 999 MB");
    assert_eq!(
        figure_at(&wide, 60).as_deref(),
        Some("not read for 59m 59s")
    );
    let WaitStep::Restate(row) = w.step(&sample(1_000_000, total, 39_250, true)) else {
        panic!("restated");
    };
    assert!(row.detail.is_empty(), "{row:?}");
    let mut steady = SessionWait::paste(3, 2);
    for i in 0..40u64 {
        let step = steady.step(&sample(i * 100_000, total, 2_000 + i * 250, true));
        if let WaitStep::Post(row) | WaitStep::Restate(row) = step {
            assert!(row.detail.is_empty(), "{i}: {row:?}");
        }
    }
}

#[test]
fn the_words_are_terse_and_tick_in_place() {
    assert_eq!(size_words(65_536), "66 KB");
    assert_eq!(size_words(812_000), "812 KB");
    assert_eq!(size_words(4_200_000), "4.2 MB");
    assert_eq!(size_words(999_700), "1.0 MB");
    assert_eq!(size_words(16 * 1024 * 1024), "17 MB");
    assert_eq!(paste_stats(1_100_000, 4_200_000), "1.1 MB / 4.2 MB");
    assert_eq!(paste_stats(812_000, 4_200_000), "812 KB / 4.2 MB");
    assert_eq!(paste_stats(0, 4_200_000), "   0 B / 4.2 MB");
    assert_eq!(lines_words(950), "950");
    assert_eq!(lines_words(12_345), "12K");
    assert_eq!(lines_words(1_234), "1.2K");
    assert_eq!(lines_words(1_200_000), "1.2M");
    assert_eq!(lines_words(3_400_000), "3.4M");
    assert_eq!(lines_words(12_000_000), "12M");
    assert_eq!(rewrap_stats(1_200_000, 3_400_000), "1.2M of 3.4M lines");
    for title in ["Pasting 4.2 MB", "Pasted 4.2 MB", REWRAPPING, REWRAPPED] {
        assert_eq!(glass_title_fault(title), None, "{title}");
        assert!(title_words(title) <= 3, "{title}");
    }
}

#[test]
fn the_paste_row_is_progress_with_one_stop() {
    let row = paste_row(7, 11, 1_100_000, 4_200_000, Duration::from_millis(500));
    assert_eq!(row.title, "Pasting 4.2 MB");
    assert_eq!(row.finished_title(), "Pasted 4.2 MB");
    assert_eq!(row.actions, [Intent::StopPaste { session: 7 }]);
    assert!(Intent::StopPaste { session: 7 }.is_consequential());
    assert!(!Intent::StopPaste { session: 7 }.closes_row());
    assert_eq!(Intent::StopPaste { session: 7 }.label(), "Stop paste");
    assert_eq!(row.reveal_after, Some(Duration::from_millis(500)));
    assert_eq!(row.key.as_deref(), Some("session.paste.7"));
    assert!(is_wait_key("session.paste.7") && is_wait_key(&wait_key(WaitKind::Rewrap, 7)));
    assert!(!is_wait_key("session.strain"));
    let meter = row.meter.expect("a meter");
    assert_eq!(meter.fill_permille, Some(262));
    assert_eq!(meter.stats, "1.1 MB / 4.2 MB");
    assert_eq!(meter.amount.map(|a| a.unit), Some(Unit::Bytes));
    assert!(!meter.busy);
    assert!(matches!(row.hold, Hold::Live { .. }));
}

#[test]
fn a_session_wait_posts_restates_and_ends_in_its_echo() {
    let mut w = SessionWait::paste(3, 1);
    assert_eq!(w.sample_every(), WAIT_SAMPLE_IDLE);
    assert_eq!(
        w.step(&sample(0, 4_000_000, 100, false)),
        WaitStep::Idle,
        "off screen"
    );
    let WaitStep::Post(row) = w.step(&sample(0, 4_000_000, 1_500, true)) else {
        panic!("posted");
    };
    assert_eq!(
        row.reveal_after,
        Some(Duration::from_millis(500)),
        "the grace counts from the paste's start"
    );
    assert_eq!(w.sample_every(), WAIT_SAMPLE);
    assert!(matches!(
        w.step(&sample(1_000_000, 4_000_000, 1_600, true)),
        WaitStep::Restate(_)
    ));
    let WaitStep::Restate(away) = w.step(&sample(2_000_000, 4_000_000, 5_000, false)) else {
        panic!("a row once up stays up");
    };
    assert_eq!(away.meter.and_then(|m| m.fill_permille), Some(500));
    let done = WaitSample {
        end: Some(WaitEnd::Done),
        ..sample(4_000_000, 4_000_000, 6_000, true)
    };
    assert_eq!(w.step(&done), WaitStep::End(EchoKind::Complete));
    assert_eq!(w.step(&done), WaitStep::Idle, "ended once");
    let mut quiet = SessionWait::rewrap(4);
    assert_eq!(
        quiet.step(&sample(10, 1_000_000, 100, false)),
        WaitStep::Idle
    );
    let gone = WaitSample {
        end: Some(WaitEnd::Done),
        ..sample(1_000_000, 1_000_000, 9_000, false)
    };
    assert_eq!(quiet.step(&gone), WaitStep::Idle);
    assert_eq!(WaitEnd::Stopped.echo(), EchoKind::Vanish);
    assert_eq!(WaitEnd::Gone.echo(), EchoKind::Vanish);
    assert_eq!(WaitEnd::Failed.echo(), EchoKind::Fault);
}

#[test]
fn the_rewrap_row_counts_lines_and_decides_nothing() {
    let mut w = SessionWait::rewrap(9);
    let WaitStep::Post(row) = w.step(&sample(1_200_000, 3_400_000, 3_000, true)) else {
        panic!("posted");
    };
    assert_eq!(
        row.title, "Rewrapping 3.4M lines",
        "the wait's size, said once"
    );
    assert_eq!(rewrap_title(0), REWRAPPING);
    assert_eq!(rewrap_title(1), "Rewrapping 1 line");
    assert_eq!(row.finished_title(), REWRAPPED);
    assert!(row.actions.is_empty());
    assert_eq!(
        crate::glass::job_stats(&row.title, "1.2M of 3.4M lines"),
        None
    );
    assert_eq!(row.reveal_after, Some(Duration::ZERO));
    let meter = row.meter.expect("a meter");
    assert_eq!(meter.stats, "1.2M of 3.4M lines");
    assert_eq!(meter.fill_permille, Some(353));
    assert_eq!(meter.amount.map(|a| a.unit), Some(Unit::Items));
}
