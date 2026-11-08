// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A Kimi Bash box's published verdict, judged from its screen and Kimi's record, on the boxes
//! and record lines one Kimi Code 2.5.2 session drew and wrote (aterm-phase's
//! `kimi-2.5.2-box-shell-read.txt` and `kimi-2.5.2-box-shell-backspaced-push.txt`, aterm-agent's
//! `kimi_wire::fixtures`).

use std::time::{Duration, Instant};

use aterm_agent::harness::kimi_wire::fixtures::{
    HEAD, REQUEST, REQUEST_BACKSPACED, REQUEST_QUESTION, RESOLVED_APPROVED,
};
use aterm_agent::harness::kimi_wire::{Pending, Wire};
use aterm_phase::kimi::fixtures::{
    BOX_SHELL_BACKSPACED_PUSH_252, BOX_SHELL_EMPTY_LINE_252, BOX_SHELL_READ_252, BUSY_HELD_240,
};
use aterm_phase::prompt::fixtures::screen;

use super::{Judged, READ_FOR, Read, SETTLE, Seen, judge_read};
use crate::presence::{AgentPhase, AgentVerdict, Cursor, Drawn};

const PID: i32 = 4242;

fn verdict_of(program: &str, text: &str, now: Instant) -> AgentVerdict {
    crate::presence::agent_verdict(
        Some(program),
        false,
        None,
        &screen(text),
        Cursor::Unknown,
        "",
        now,
    )
}

fn detail(v: &AgentVerdict) -> Option<String> {
    match v.reading().map(|r| &r.phase) {
        Some(AgentPhase::Prompt { detail }) => detail.clone(),
        other => panic!("not a prompt: {other:?}"),
    }
}

fn pending_of(lines: &[&str]) -> Option<Vec<Pending>> {
    let mut wire = Wire::default();
    wire.advance(lines.concat().as_bytes());
    wire.pending().map(<[Pending]>::to_vec)
}

fn drawn(text: &str) -> Option<Drawn> {
    crate::presence::prompt_drawn(aterm_phase::Program::Kimi, &screen(text))
}

/// The verdict `text`'s box publishes once a read of `record`, taken at `read_at` for `pid`,
/// is judged at `now` against the box `seen` before, its program having last read its tab's
/// input at `input_ms` and its screen standing since the box was first seen; and what the
/// judge made of it.
fn judged(
    text: &str,
    record: Option<Vec<Pending>>,
    pid: i32,
    read_at: Instant,
    seen: &mut Option<Seen>,
    input_ms: u64,
    now: Instant,
) -> (Option<String>, Judged) {
    let mut v = verdict_of("kimi", text, now);
    let screen_at = seen.map_or(now, Seen::since);
    let judged = judge_read(
        &mut v,
        || drawn(text),
        || {
            Some(Read {
                pid,
                at: read_at,
                pending: record,
            })
        },
        PID,
        (input_ms, screen_at),
        seen,
        now,
    );
    (detail(&v), judged)
}

/// [`judged`] for a box first seen at `t0` under input at `input_ms`, read [`SETTLE`] later
/// for [`PID`].
fn settled_after(
    text: &str,
    record: Option<Vec<Pending>>,
    input_ms: u64,
    t0: Instant,
) -> (Option<String>, Judged) {
    let mut seen = None;
    let _ = judged(text, None, PID, t0, &mut seen, input_ms, t0);
    let late = t0 + SETTLE;
    judged(text, record, PID, late, &mut seen, input_ms, late)
}

/// [`settled_after`] a tab that never took input.
fn settled(text: &str, record: Option<Vec<Pending>>, t0: Instant) -> (Option<String>, Judged) {
    settled_after(text, record, 0, t0)
}

#[test]
fn the_screen_alone_cannot_tell_the_spoof_from_the_read() {
    let t0 = Instant::now();
    for text in [BOX_SHELL_READ_252, BOX_SHELL_BACKSPACED_PUSH_252] {
        assert_eq!(
            detail(&verdict_of("kimi", text, t0)).as_deref(),
            Some("bash:read-only"),
            "PRECONDITION: the screen's rule passes both boxes"
        );
        assert_eq!(
            drawn(text)
                .expect("a box")
                .readings
                .first()
                .map(String::as_str),
            Some("cat README.md"),
            "PRECONDITION: both boxes draw the honest read's command"
        );
    }
}

#[test]
fn a_box_is_read_only_only_where_its_record_says_what_runs_is() {
    let t0 = Instant::now();
    assert_eq!(
        settled(
            BOX_SHELL_BACKSPACED_PUSH_252,
            pending_of(&[HEAD, REQUEST_BACKSPACED]),
            t0
        ),
        (Some("bash:not-read-only".to_string()), Judged::Settled),
        "the command that runs is a push"
    );
    assert_eq!(
        settled(BOX_SHELL_READ_252, pending_of(&[HEAD, REQUEST]), t0),
        (Some("bash:read-only".to_string()), Judged::Again),
        "NEGATIVE CONTROL: the honest read's record lends its verdict"
    );
    assert_eq!(
        settled(
            BOX_SHELL_READ_252,
            pending_of(&[HEAD, REQUEST, REQUEST_QUESTION]),
            t0
        ),
        (Some("bash:read-only".to_string()), Judged::Again),
        "a question pending beside it is no approval"
    );
    assert_eq!(
        settled(
            BOX_SHELL_READ_252,
            pending_of(&[HEAD, REQUEST, RESOLVED_APPROVED]),
            t0
        ),
        (Some("bash".to_string()), Judged::Again),
        "a record with nothing pending binds no box"
    );
    assert_eq!(
        settled(BOX_SHELL_READ_252, None, t0),
        (Some("bash".to_string()), Judged::Again),
        "a record that cannot say lends nothing"
    );
}

#[test]
fn a_read_counts_only_once_the_box_has_stood_and_only_for_its_process() {
    let t0 = Instant::now();
    let record = || pending_of(&[HEAD, REQUEST]);
    let early = t0 + SETTLE - Duration::from_millis(1);
    let mut seen = None;
    assert_eq!(
        judged(BOX_SHELL_READ_252, record(), PID, t0, &mut seen, 0, t0),
        (Some("bash".to_string()), Judged::Again),
        "a read taken as the box appears may miss a request still being written"
    );
    assert_eq!(
        judged(
            BOX_SHELL_READ_252,
            record(),
            PID,
            early,
            &mut seen,
            0,
            early
        ),
        (Some("bash".to_string()), Judged::Again),
        "a read short of the settle is no read"
    );
    let late = t0 + SETTLE;
    assert_eq!(
        judged(
            BOX_SHELL_READ_252,
            record(),
            PID + 1,
            late,
            &mut seen,
            0,
            late
        ),
        (Some("bash".to_string()), Judged::Again),
        "a read for another process is no read of this one"
    );
    assert_eq!(
        judged(BOX_SHELL_READ_252, record(), PID, late, &mut seen, 0, late),
        (Some("bash:read-only".to_string()), Judged::Again),
        "NEGATIVE CONTROL: this process's read after the settle counts"
    );
    // Input under the box (it answered, and one drawn like it standing) starts its settle again.
    assert_eq!(
        judged(BOX_SHELL_READ_252, record(), PID, late, &mut seen, 1, late),
        (Some("bash".to_string()), Judged::Again),
        "a read the box has not stood for since its tab took input is no read"
    );
}

#[test]
fn a_request_that_lands_late_takes_read_only_away() {
    let t0 = Instant::now();
    let mut seen = None;
    let _ = judged(BOX_SHELL_READ_252, None, PID, t0, &mut seen, 0, t0);
    let settle = t0 + SETTLE;
    assert_eq!(
        judged(
            BOX_SHELL_READ_252,
            pending_of(&[HEAD, REQUEST]),
            PID,
            settle,
            &mut seen,
            0,
            settle
        ),
        (Some("bash:read-only".to_string()), Judged::Again),
        "read-only is read again"
    );
    let later = settle + SETTLE;
    assert_eq!(
        judged(
            BOX_SHELL_READ_252,
            pending_of(&[HEAD, REQUEST, REQUEST_BACKSPACED]),
            PID,
            later,
            &mut seen,
            0,
            later
        ),
        (Some("bash:not-read-only".to_string()), Judged::Settled),
        "a spoof's request written after the settle takes it away, for good"
    );
}

#[test]
fn a_box_is_read_no_more_once_it_has_stood_a_while() {
    let t0 = Instant::now();
    let mut seen = None;
    let record = || pending_of(&[HEAD, REQUEST, RESOLVED_APPROVED]);
    let just_before = t0 + READ_FOR - Duration::from_millis(1);
    let _ = judged(BOX_SHELL_READ_252, None, PID, t0, &mut seen, 0, t0);
    assert_eq!(
        judged(
            BOX_SHELL_READ_252,
            record(),
            PID,
            just_before,
            &mut seen,
            0,
            just_before
        ),
        (Some("bash".to_string()), Judged::Again),
        "NEGATIVE CONTROL: within READ_FOR a record that cannot say is read again"
    );
    let late = t0 + READ_FOR;
    assert_eq!(
        judged(BOX_SHELL_READ_252, record(), PID, late, &mut seen, 0, late),
        (Some("bash".to_string()), Judged::Settled),
        "past READ_FOR the verdict stays withheld and the box is read no more"
    );
    let honest = || pending_of(&[HEAD, REQUEST]);
    assert_eq!(
        judged(BOX_SHELL_READ_252, honest(), PID, late, &mut seen, 0, late),
        (Some("bash".to_string()), Judged::Settled),
        "a box settled withheld stays so, though its record comes to say"
    );
    let mut fresh = None;
    let _ = judged(BOX_SHELL_READ_252, None, PID, t0, &mut fresh, 0, t0);
    assert_eq!(
        judged(BOX_SHELL_READ_252, honest(), PID, late, &mut fresh, 0, late),
        (Some("bash:read-only".to_string()), Judged::Settled),
        "past READ_FOR read-only settles too"
    );
}

/// A box settled at `bash:not-read-only`, or at `bash` past [`READ_FOR`], keeps its verdict
/// while it stands and asks for no read: no later line makes either safer. One settled at
/// `bash:read-only` is still judged off each read, so a request that lands later can still take
/// it away.
#[test]
fn a_box_settled_short_of_read_only_keeps_its_verdict_without_a_read() {
    let t0 = Instant::now();
    let unread = || -> Option<Read> { panic!("a settled box asks for no read") };
    let judge_unread = |text: &str, seen: &mut Option<Seen>, now: Instant| {
        let mut v = verdict_of("kimi", text, now);
        let judged = judge_read(&mut v, || drawn(text), unread, PID, (0, t0), seen, now);
        (detail(&v), judged)
    };
    let mut seen = None;
    let _ = judged(
        BOX_SHELL_BACKSPACED_PUSH_252,
        None,
        PID,
        t0,
        &mut seen,
        0,
        t0,
    );
    let late = t0 + SETTLE;
    assert_eq!(
        judged(
            BOX_SHELL_BACKSPACED_PUSH_252,
            pending_of(&[HEAD, REQUEST_BACKSPACED]),
            PID,
            late,
            &mut seen,
            0,
            late
        ),
        (Some("bash:not-read-only".to_string()), Judged::Settled),
        "PRECONDITION: the push settles not-read-only"
    );
    assert_eq!(
        judge_unread(BOX_SHELL_BACKSPACED_PUSH_252, &mut seen, late + SETTLE),
        (Some("bash:not-read-only".to_string()), Judged::Settled),
        "and keeps it without a read"
    );

    let mut seen = None;
    let _ = judged(BOX_SHELL_READ_252, None, PID, t0, &mut seen, 0, t0);
    let past = t0 + READ_FOR;
    let honest = || pending_of(&[HEAD, REQUEST]);
    assert_eq!(
        judged(BOX_SHELL_READ_252, honest(), PID, past, &mut seen, 0, past),
        (Some("bash:read-only".to_string()), Judged::Settled),
        "PRECONDITION: the honest read settles read-only"
    );
    assert_eq!(
        judged(
            BOX_SHELL_READ_252,
            pending_of(&[HEAD, REQUEST, REQUEST_BACKSPACED]),
            PID,
            past,
            &mut seen,
            0,
            past
        ),
        (Some("bash:not-read-only".to_string()), Judged::Settled),
        "NEGATIVE CONTROL: settled read-only is still judged off its record's next read"
    );
}

/// A box Kimi swaps for one drawn alike with no input between them stands on a new screen: a
/// `bash:read-only` judged off a read begun before that screen is judged again one pass later,
/// off a read begun since, until [`READ_FOR`] after the screen. Meanwhile it holds for
/// [`SETTLE`], then reads `bash`; with no such read by [`READ_FOR`] it settles at `bash`, never
/// at `bash:read-only`. Every other verdict needs no such read.
#[test]
fn a_read_only_box_off_a_read_older_than_its_screen_is_read_again() {
    let t0 = Instant::now();
    let honest = || pending_of(&[HEAD, REQUEST]);
    let judged_at = |record, read_at: Instant, screen_at: Instant, seen: &mut Option<Seen>, now| {
        let mut v = verdict_of("kimi", BOX_SHELL_READ_252, now);
        let judged = judge_read(
            &mut v,
            || drawn(BOX_SHELL_READ_252),
            || {
                Some(Read {
                    pid: PID,
                    at: read_at,
                    pending: record,
                })
            },
            PID,
            (0, screen_at),
            seen,
            now,
        );
        (detail(&v), judged)
    };
    let mut seen = None;
    let _ = judged_at(None, t0, t0, &mut seen, t0);
    let read = t0 + SETTLE;
    let past = t0 + READ_FOR;
    assert_eq!(
        judged_at(honest(), read, t0, &mut seen, past),
        (Some("bash:read-only".to_string()), Judged::Settled),
        "PRECONDITION: a read begun since the screen settles past READ_FOR"
    );
    let swapped = past + SETTLE;
    assert_eq!(
        judged_at(honest(), read, swapped, &mut seen, swapped),
        (Some("bash:read-only".to_string()), Judged::Again),
        "a read begun before the screen it judges is read again, the verdict held meanwhile"
    );
    assert_eq!(
        judged_at(
            pending_of(&[HEAD, REQUEST, REQUEST_BACKSPACED]),
            swapped,
            swapped,
            &mut seen,
            swapped + crate::session_status::AGENT_MIN_INTERVAL
        ),
        (Some("bash:not-read-only".to_string()), Judged::Settled),
        "the read begun since the swap finds the box's own request"
    );
    let mut seen = None;
    let _ = judged_at(None, t0, t0, &mut seen, t0);
    let _ = judged_at(honest(), read, t0, &mut seen, past);
    assert_eq!(
        judged_at(honest(), swapped, swapped, &mut seen, swapped),
        (Some("bash:read-only".to_string()), Judged::Settled),
        "NEGATIVE CONTROL: a read begun with its screen settles"
    );
    let mut failed = None;
    let _ = judged_at(None, t0, t0, &mut failed, t0);
    assert_eq!(
        judged_at(
            pending_of(&[HEAD, REQUEST, REQUEST_BACKSPACED]),
            read,
            swapped,
            &mut failed,
            swapped
        ),
        (Some("bash:not-read-only".to_string()), Judged::Settled),
        "a not-read-only needs no read begun since its screen"
    );
    let mut lagging = None;
    let _ = judged_at(None, t0, t0, &mut lagging, t0);
    let _ = judged_at(honest(), read, t0, &mut lagging, past);
    assert_eq!(
        judged_at(honest(), read, swapped, &mut lagging, swapped + SETTLE),
        (Some("bash".to_string()), Judged::Again),
        "held for SETTLE, a read-only off a read older than its screen is withheld while the lane lags"
    );
    let stale_long = swapped + READ_FOR;
    assert_eq!(
        judged_at(honest(), read, swapped, &mut lagging, stale_long),
        (Some("bash".to_string()), Judged::Settled),
        "a lane that never reads again is not asked on and on, and settles it withheld"
    );
    assert_eq!(
        judged_at(
            honest(),
            stale_long,
            swapped,
            &mut lagging,
            stale_long + SETTLE
        ),
        (Some("bash".to_string()), Judged::Settled),
        "and the box keeps that verdict while it stands"
    );
}

#[test]
fn a_box_with_no_read_yet_is_read_again() {
    let t0 = Instant::now();
    let mut v = verdict_of("kimi", BOX_SHELL_READ_252, t0);
    let mut seen = None;
    let judged = judge_read(
        &mut v,
        || drawn(BOX_SHELL_READ_252),
        || None,
        PID,
        (0, t0),
        &mut seen,
        t0 + SETTLE,
    );
    assert_eq!(
        judged,
        Judged::Again,
        "the box is read again until its record answers"
    );
    assert_eq!(detail(&v).as_deref(), Some("bash"));
    assert_eq!(
        seen,
        Some(Seen {
            key: drawn(BOX_SHELL_READ_252).expect("a box").key,
            input_ms: 0,
            since: t0 + SETTLE,
            settled: None,
        })
    );
}

#[test]
fn every_other_verdict_is_left_as_its_screen_reads_it() {
    let t0 = Instant::now();
    let unread = || -> Option<Read> { panic!("no record is read for this verdict") };
    let undrawn = || -> Option<Drawn> { panic!("no box is read off the screen") };
    for (program, text) in [
        ("kimi", BOX_SHELL_EMPTY_LINE_252),
        ("kimi", BUSY_HELD_240),
        ("zsh", BOX_SHELL_READ_252),
    ] {
        let before = verdict_of(program, text, t0);
        let mut v = before.clone();
        let mut seen = Some(Seen {
            key: 7,
            input_ms: 0,
            since: t0,
            settled: None,
        });
        assert_eq!(
            judge_read(&mut v, undrawn, unread, PID, (0, t0), &mut seen, t0),
            Judged::Untouched,
            "{program}"
        );
        assert_eq!(v, before, "{program}: untouched");
        assert_eq!(seen, None, "{program}: the box that stood is gone");
    }
}

#[test]
fn a_request_asked_before_the_last_input_lends_nothing() {
    // The person answered the honest read's box; its answer, and the request of the box now
    // standing (drawn the same), have not reached disk, so the record still holds the read open.
    let t0 = Instant::now();
    let asked = pending_of(&[HEAD, REQUEST]).expect("reads")[0].asked;
    for text in [BOX_SHELL_BACKSPACED_PUSH_252, BOX_SHELL_READ_252] {
        assert_eq!(
            settled_after(text, pending_of(&[HEAD, REQUEST]), asked + 1, t0),
            (Some("bash".to_string()), Judged::Again),
            "a request input came after may be the one it answered"
        );
        assert_eq!(
            settled_after(text, pending_of(&[HEAD, REQUEST]), asked - 1, t0),
            (Some("bash:read-only".to_string()), Judged::Again),
            "NEGATIVE CONTROL: one asked after the last input is the box's"
        );
    }
}

#[test]
fn input_under_a_box_starts_its_settle_again() {
    let t0 = Instant::now();
    let record = || pending_of(&[HEAD, REQUEST]);
    let mut seen = None;
    let _ = judged(BOX_SHELL_READ_252, None, PID, t0, &mut seen, 0, t0);
    let late = t0 + SETTLE;
    assert_eq!(
        judged(BOX_SHELL_READ_252, record(), PID, late, &mut seen, 0, late),
        (Some("bash:read-only".to_string()), Judged::Again),
        "PRECONDITION: the box has stood and its read counts"
    );
    let typed = late + Duration::from_millis(1);
    assert_eq!(
        judged(BOX_SHELL_READ_252, record(), PID, late, &mut seen, 1, typed),
        (Some("bash".to_string()), Judged::Again),
        "a read begun before the tab took input is no read of the box after it"
    );
    assert_eq!(seen.map(Seen::since), Some(typed));
    let again = typed + SETTLE;
    assert_eq!(
        judged(
            BOX_SHELL_READ_252,
            record(),
            PID,
            again,
            &mut seen,
            1,
            again
        ),
        (Some("bash:read-only".to_string()), Judged::Again),
        "NEGATIVE CONTROL: a read the box stood for since counts"
    );
}

/// Holds `read` as `session`'s latest read of its record, as the lane would.
pub(crate) fn hold_read(session: u64, read: Read) {
    super::READS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session, read);
}

/// The record of the honest `cat README.md` box, standing.
pub(crate) fn honest_record() -> Option<Vec<Pending>> {
    pending_of(&[HEAD, REQUEST])
}

/// The record of the honest `cat README.md` box, standing, its request asked at `ms` (wall-clock
/// milliseconds since the Unix epoch) instead of when it was measured.
pub(crate) fn honest_record_asked_at(ms: u64) -> Option<Vec<Pending>> {
    let measured = "\"time\":1794115339426";
    assert_eq!(
        REQUEST.matches(measured).count(),
        1,
        "PRECONDITION: the request's one time"
    );
    let request = REQUEST.replacen(measured, &format!("\"time\":{ms}"), 1);
    pending_of(&[HEAD, &request])
}

/// `text`'s screen with `from` changed to `to` in the one row that holds it.
pub(crate) fn edited(text: &str, from: &str, to: &str) -> Vec<String> {
    let mut rows = screen(text);
    let held: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].contains(from))
        .collect();
    assert_eq!(held.len(), 1, "PRECONDITION: one row holds {from:?}");
    rows[held[0]] = rows[held[0]].replacen(from, to, 1);
    rows
}

#[test]
fn a_box_is_keyed_by_its_own_rows_not_what_repaints_around_it() {
    let key = |rows: &[String]| {
        crate::presence::prompt_drawn(aterm_phase::Program::Kimi, rows)
            .expect("a box")
            .key
    };
    let base = key(&screen(BOX_SHELL_READ_252));
    for (what, from, to) in [
        ("the footer's tip", "ctrl+o expand", "ctrl+t expand"),
        ("the transcript above", "❯ toolstart", "❯ toolstart again"),
    ] {
        assert_eq!(
            key(&edited(BOX_SHELL_READ_252, from, to)),
            base,
            "{what} does not move the box"
        );
    }
    assert_ne!(
        key(&edited(
            BOX_SHELL_READ_252,
            "Show the readme",
            "Show the readme twice"
        )),
        base,
        "NEGATIVE CONTROL: a row of the box itself does"
    );
}

#[test]
fn a_box_its_reader_cannot_read_is_withheld_and_left() {
    let t0 = Instant::now();
    let mut v = verdict_of("kimi", BOX_SHELL_READ_252, t0);
    let mut seen = Some(Seen {
        key: 7,
        input_ms: 0,
        since: t0,
        settled: None,
    });
    let unread = || -> Option<Read> { panic!("no record is read for a box not read") };
    assert_eq!(
        judge_read(&mut v, || None, unread, PID, (0, t0), &mut seen, t0),
        Judged::Settled,
        "nothing to bind, nothing to read again"
    );
    assert_eq!(detail(&v).as_deref(), Some("bash"));
    assert_eq!(seen, None);
}
