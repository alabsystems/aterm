// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Progress ETA, stall detection, custom ending words, and reveal-grace deferral tests.

use crate::animate::Look;
use crate::center::{EchoKind, MessageCenter};
use crate::glass::{Links, Presentation};
use crate::log::MessageLog;
use crate::model::{Amount, Hold, Message, Meter, Severity, Unit, WallStamp, tags};
use crate::text::char_width;
use crate::waits::{SessionWait, WaitSample, WaitStep};
use crate::wire::{Applied, NoticeRequest, WireGate, apply};
use crate::{Duration, Instant, PROGRESS_GRACE, REVEAL_DEFER_MAX, STALL_AFTER, STALLED_WORD};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn present(c: &MessageCenter, cols: usize) -> Presentation {
    c.presentation(cols, &char_width, None, Links::Painted)
}

fn notice(c: &mut MessageCenter, g: &mut WireGate, line: &str, now: Instant) -> Applied {
    let req = NoticeRequest::parse(line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
    apply(c, g, req, WallStamp { unix_ms: 1_000 }, now)
}

fn sweep(c: &mut MessageCenter, now: Instant) {
    c.settle(now, true);
    c.commit_rows(now, 3);
}

fn eta_words(c: &MessageCenter, now: Instant) -> Option<String> {
    let p = present(c, 120);
    c.motion(&p, now, Look::MOVING).rows.first()?.eta.clone()
}

#[test]
fn a_percent_row_says_how_long_is_left() {
    let t0 = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), t0);
    let mut g = WireGate::default();
    let mut now = t0;
    for k in 0..=12u64 {
        now = t0 + ms(k * 1000);
        notice(
            &mut c,
            &mut g,
            &format!("progress build pct={} Building aterm", k * 5),
            now,
        );
        sweep(&mut c, now);
    }
    let p = present(&c, 120);
    assert!(p.rows[0].eta.is_some(), "a percent row has an ETA slot");
    let words = eta_words(&c, now).expect("an estimate at 60 % of a steady job");
    assert!(words.ends_with(" left"), "{words}");
    assert_eq!(words, "10 s left");
}

#[test]
fn a_count_that_stops_says_stalled_not_less_than_five_seconds() {
    let t0 = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), t0);
    let mut g = WireGate::default();
    let mut last = t0;
    for k in 0..=11u64 {
        last = t0 + ms(k * 800);
        notice(
            &mut c,
            &mut g,
            &format!("progress deploy done={k}/40 unit=steps Deploying site"),
            last,
        );
        sweep(&mut c, last);
    }
    assert!(
        eta_words(&c, last).is_some_and(|w| w.ends_with(" left") && w != "<5 s left"),
        "latched: {:?}",
        eta_words(&c, last)
    );
    let mut said = Vec::new();
    let mut probe = last;
    while probe <= last + STALL_AFTER + ms(1000) {
        c.settle(probe, true);
        if let Some(w) = eta_words(&c, probe)
            && said.last() != Some(&w)
        {
            said.push(w);
        }
        probe += ms(250);
    }
    assert!(!said.iter().any(|w| w == "<5 s left"), "{said:?}");
    assert_eq!(
        said.last().map(String::as_str),
        Some(STALLED_WORD),
        "{said:?}"
    );
}

#[test]
fn a_scripts_own_ending_words_are_its_record() {
    let t0 = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), t0);
    let mut g = WireGate::default();
    for ((key, how, words, sev), at) in [
        ("idx", "withdraw", "Indexing skipped", Severity::Info),
        ("sync", "warn", "Indexing gave up", Severity::Warn),
    ]
    .into_iter()
    .zip([t0, t0 + ms(60_000)])
    {
        notice(
            &mut c,
            &mut g,
            &format!("progress {key} tag=search busy Indexing docs"),
            at,
        );
        sweep(&mut c, at + PROGRESS_GRACE);
        let id = c.live_rows().next().expect("the script's row").id;
        notice(
            &mut c,
            &mut g,
            &format!("done {key} {how} {words}"),
            at + ms(47_000),
        );
        let rec = c.log().get(id).unwrap();
        assert_eq!(rec.title, words, "{rec:?}");
        assert!(rec.detail.is_empty(), "{rec:?}");
        assert_eq!(rec.severity, sev, "{rec:?}");
    }
    notice(
        &mut c,
        &mut g,
        "progress idx tag=search busy Indexing docs",
        t0 + ms(120_000),
    );
    let id = c.live_rows().next().unwrap().id;
    notice(&mut c, &mut g, "done idx warn", t0 + ms(130_000));
    assert_eq!(c.log().get(id).unwrap().title, "Indexing stopped");
}

fn rewrap(total: u64) -> Message {
    Message::new(tags::SESSION, Severity::Info, "Rewrapping 2.0M lines")
        .hold(Hold::Live {
            stale_after: Duration::from_secs(30),
        })
        .meter(meter(0, total))
        .reveal_after(PROGRESS_GRACE)
}

fn meter(done: u64, total: u64) -> Meter {
    Meter {
        amount: Some(Amount {
            series: 7,
            done,
            total,
            unit: Unit::Items,
        }),
        ..Meter::default()
    }
}

fn restate(c: &mut MessageCenter, id: crate::model::MessageId, done: u64, total: u64, at: Instant) {
    c.restate(
        id,
        crate::model::Restatement {
            meter: Some(Some(meter(done, total))),
            ..Default::default()
        },
        at,
    );
}

#[test]
fn work_about_to_end_at_its_grace_never_flashes() {
    let t0 = Instant::now();
    let total = 2_000_000;
    let mut c = MessageCenter::new(MessageLog::empty(), t0);
    let id = c.post(rewrap(total), WallStamp { unix_ms: 1 }, t0).id;
    let mut now = t0;
    for k in 1..=10u64 {
        now = t0 + ms(k * 250);
        restate(&mut c, id, (total * 3 / 8) * k / 5, total, now);
        sweep(&mut c, now);
        if k >= 8 {
            assert!(c.on_glass().next().is_none(), "held back at {k}: 3/4 done");
        }
    }
    assert!(c.withdraw_with(id, EchoKind::Complete, now + ms(100)));
    assert!(c.echoes().is_empty(), "never shown, so no echo");

    let mut c = MessageCenter::new(MessageLog::empty(), t0);
    let id = c.post(rewrap(total), WallStamp { unix_ms: 1 }, t0).id;
    for k in 1..=8u64 {
        let now = t0 + ms(k * 250);
        restate(&mut c, id, total * 9 / 10 * k / 8, total, now);
        sweep(&mut c, now);
    }
    sweep(&mut c, t0 + PROGRESS_GRACE + ms(500));
    assert!(c.on_glass().next().is_none(), "still projecting an end");
    let mut probe = t0 + PROGRESS_GRACE + ms(500);
    while c.on_glass().next().is_none() && probe < t0 + PROGRESS_GRACE + REVEAL_DEFER_MAX {
        probe = c
            .deadline(true)
            .unwrap_or(probe + ms(250))
            .max(probe + ms(1));
        sweep(&mut c, probe);
    }
    sweep(&mut c, t0 + PROGRESS_GRACE + REVEAL_DEFER_MAX);
    assert!(c.on_glass().next().is_some(), "revealed by the cap");
}

#[test]
fn a_session_wait_about_to_end_is_not_posted() {
    let sample = |done: u64, ms_: u64, end| WaitSample {
        done,
        total: 2_000_000,
        elapsed: ms(ms_),
        asked: true,
        end,
    };
    let unasked = |done: u64, ms_: u64| WaitSample {
        asked: false,
        ..sample(done, ms_, None)
    };
    let mut w = SessionWait::rewrap(1);
    assert_eq!(w.step(&unasked(500_000, 1_000)), WaitStep::Idle);
    assert_eq!(w.step(&sample(1_480_000, 2_900, None)), WaitStep::Idle);
    assert!(!w.posted());
    assert_eq!(
        w.sample_every(),
        crate::waits::WAIT_SAMPLE,
        "sampled at the row's pace"
    );
    assert_eq!(
        w.step(&sample(2_000_000, 3_900, Some(crate::waits::WaitEnd::Done))),
        WaitStep::Idle,
        "ends silently"
    );
    let mut early = SessionWait::rewrap(1);
    assert!(matches!(
        early.step(&sample(1_200_000, 0, None)),
        WaitStep::Post(_)
    ));
    let mut slow = SessionWait::rewrap(1);
    assert_eq!(slow.step(&unasked(100_000, 500)), WaitStep::Idle);
    assert!(matches!(
        slow.step(&sample(200_000, 1_000, None)),
        WaitStep::Post(_)
    ));
    let mut late = SessionWait::rewrap(1);
    assert_eq!(late.step(&unasked(1_900_000, 3_900)), WaitStep::Idle);
    assert!(matches!(
        late.step(&sample(1_990_000, 4_100, None)),
        WaitStep::Post(_)
    ));
}
