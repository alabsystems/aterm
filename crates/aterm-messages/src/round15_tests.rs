// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: the round-15 engine invariants (design ruling 265), found by a
//! day in the life on one windowed instance — a percent row says how long is
//! left, a count that stops says it stalled, a script's finished words read
//! as every reporter's do, work about to end at its grace never flashes, and
//! a withdrawn row's record reads as ended.

use crate::animate::Look;
use crate::center::{EchoKind, MessageCenter};
use crate::glass::{Links, Presentation, RowKind};
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

/// Settle and commit, as the host's sweep does.
fn sweep(c: &mut MessageCenter, now: Instant) {
    c.settle(now, true);
    c.commit_rows(now, 3);
}

/// The ETA slot's words on the first row at `now`.
fn eta_words(c: &MessageCenter, now: Instant) -> Option<String> {
    let p = present(c, 120);
    c.motion(&p, now, Look::MOVING).rows.first()?.eta.clone()
}

/// THE OWNER'S "HOW LONG TO WAIT" FOR `pct=` (ruling 265): a script that says
/// only its percent at a steady 5 %/s gets an ETA slot and an estimate, as a
/// `done=` script does — the day's `Building aterm 25%` said no time at all.
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
    // 40 % left at 5 %/s: eight seconds, said in fives.
    assert_eq!(words, "10 s left");
}

/// A COUNT THAT STOPS SAYS SO (ruling 265): a `done=` steps job that stops
/// reporting at 11 of 40 no longer counts its estimate down to `<5 s left`
/// at a frozen fill and then goes blank; it says `stalled` four of its own
/// gaps (never under ten seconds) after its last step.
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

/// ONE ECHO FOR EVERY REPORTER (ruling 265): `done build ok Built aterm`
/// declares the finished words, so the echo is `✓ Built aterm` with its
/// percent blank — as `✓ ALab tools updated` — never `✓ Built aterm 100%`;
/// the record is `Built aterm` under ✓ Success with no in-flight frame, filed
/// under the script's own tag.
#[test]
fn a_scripts_finished_words_echo_like_every_reporters() {
    let t0 = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), t0);
    let mut g = WireGate::default();
    for k in 0..=20u64 {
        let now = t0 + ms(k * 1000);
        notice(
            &mut c,
            &mut g,
            &format!(
                "progress build pct={} Building aterm -- step {k} of 20",
                k * 5
            ),
            now,
        );
        sweep(&mut c, now);
    }
    let id = c.live_rows().next().unwrap().id;
    let end = t0 + ms(20_500);
    notice(&mut c, &mut g, "done build ok Built aterm", end);
    let p = present(&c, 120);
    let echo = &p.rows[0];
    assert!(matches!(echo.kind, RowKind::Echo(_)));
    assert_eq!(echo.title.1.trim_end(), "Built aterm");
    assert_eq!(echo.pct, None, "no `100%` beside the finished words");
    assert_eq!(c.echoes()[0].kind, EchoKind::Complete);
    let rec = c.log().get(id).unwrap();
    assert_eq!(rec.title, "Built aterm");
    assert!(rec.detail.is_empty(), "{:?}", rec.detail);
    assert_eq!(
        (rec.severity, rec.glyph.ch(), rec.tag.as_str()),
        (Severity::Success, '\u{2713}', "script")
    );
}

/// A WITHDRAWN ROW READS AS ENDED (ruling 265): `done index withdraw` on a
/// busy `Indexing docs` is logged `Indexing ended`, its live title the first
/// detail line, under the ℹ of its severity — never the live title that read
/// as still running.
#[test]
fn a_withdrawn_rows_record_reads_as_ended() {
    let t0 = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), t0);
    let mut g = WireGate::default();
    notice(
        &mut c,
        &mut g,
        "progress index tag=search busy Indexing docs",
        t0,
    );
    sweep(&mut c, t0 + PROGRESS_GRACE);
    let id = c.live_rows().next().unwrap().id;
    notice(&mut c, &mut g, "done index withdraw", t0 + ms(47_000));
    let rec = c.log().get(id).unwrap();
    assert_eq!(rec.title, "Indexing ended");
    assert_eq!(rec.detail, vec!["Indexing docs".to_string()]);
    assert_eq!((rec.severity, rec.glyph.ch()), (Severity::Info, '\u{2139}'));
}

/// A SCRIPT'S OWN ENDING WORDS STAND (ruling 266): `done <key> withdraw
/// <words>` and `done <key> warn <words>` say how the work ended, so the
/// record keeps them — never `Indexing ended` / `Indexing stopped` over them
/// with the script's sentence pushed into the detail. The mark still says
/// how it ended.
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
    // Without words the table still speaks (ruling 265).
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

/// A live row whose work is almost done at the end of its grace.
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

/// NO FLASH AT THE GRACE'S END (ruling 265): a row whose work projects to
/// end within REVEAL_MIN_LEFT when its grace runs out is held back and, when
/// the work does end, never reaches the glass — no row for under a second,
/// no echo. A projection that keeps promising and is wrong still reaches the
/// glass by the cap.
#[test]
fn work_about_to_end_at_its_grace_never_flashes() {
    let t0 = Instant::now();
    let total = 2_000_000;
    // A 2.6 s job: 75 % done at the grace's end.
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

    // A job that stalls at 90 % right at its grace: shown by the cap.
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

/// The session wait holds its row back the same way (ruling 265): a rewrap
/// the person asks after at 2.9 s, three quarters done, would show for under
/// a second — it is not posted, it is sampled at the row's pace, and it ends
/// silently; one asked early with most of it ahead is posted at once.
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
    // Seen first unasked (its pace's base), then asked with a second left.
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
    // Asked at its first sample: no pace yet, so it is posted (the engine's
    // reveal looks again when the grace ends).
    let mut early = SessionWait::rewrap(1);
    assert!(matches!(
        early.step(&sample(1_200_000, 0, None)),
        WaitStep::Post(_)
    ));
    // Most of it ahead: posted.
    let mut slow = SessionWait::rewrap(1);
    assert_eq!(slow.step(&unasked(100_000, 500)), WaitStep::Idle);
    assert!(matches!(
        slow.step(&sample(200_000, 1_000, None)),
        WaitStep::Post(_)
    ));
    // Past the cap it is posted whatever the projection says.
    let mut late = SessionWait::rewrap(1);
    assert_eq!(late.step(&unasked(1_900_000, 3_900)), WaitStep::Idle);
    assert!(matches!(
        late.step(&sample(1_990_000, 4_100, None)),
        WaitStep::Post(_)
    ));
}
