// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

use super::{
    PROGRAM_NAME_CONFIRM, PROGRAM_NAMED_RECHECK_FLOOR, PROGRAM_RECHECK, SessionSlot,
    StatusObserver, StatusPolicy,
};
use crate::presence::Cursor;
use crate::{App, WindowId};

/// A session's retirement reaches the Claude footer's resolver, which
/// holds its usage fold: a closed tab's fold goes with it, not at the end
/// of its watch.
#[test]
fn retiring_a_session_retires_its_footer_state() {
    let mut observer = StatusObserver::new(StatusPolicy::default(), Duration::from_millis(0));
    crate::claude_footer::STOPPED_SESSIONS.with(|r| r.borrow_mut().clear());
    observer.retire(4_321);
    assert_eq!(
        crate::claude_footer::STOPPED_SESSIONS.with(|r| r.borrow().clone()),
        vec![4_321]
    );
}

/// The background resolver can answer after the usual one-interval
/// follow-up has already gone quiet. Its completion wake restores one
/// status read at the observation floor, including on a static screen.
#[test]
fn a_late_program_answer_owns_one_observation_wake() {
    let t0 = Instant::now();
    let floor = Duration::from_millis(250);
    let id = 7;
    let pgid = 42;
    let generation = crate::control::ScreenGen { epoch: 0, seq: 1 };
    let policy = StatusPolicy::default();
    let mut observer = StatusObserver::new(policy, floor);
    observer.sessions.insert(
        id,
        SessionSlot {
            next_due: t0 + floor,
            ..SessionSlot::new(policy, t0)
        },
    );
    observer.agent_observe(
        id,
        generation,
        Some(vec!["Claude Code composer".into()]),
        Cursor::Unknown,
        Some("claude".into()),
        pgid,
        false,
        t0,
    );
    observer.sessions.get_mut(&id).unwrap().next_due = t0 + floor * 2;
    observer.agent_observe(
        id,
        generation,
        None,
        Cursor::Unknown,
        Some("claude".into()),
        pgid,
        false,
        t0 + floor,
    );
    assert_eq!(observer.next_wake(), None, "the ordinary follow-up retired");

    observer.note_program_answered(id);
    observer.note_program_answered(id);
    assert_eq!(observer.next_wake(), Some(t0 + floor * 2));
    assert!(observer.agent_zone_wanted(
        id,
        generation,
        Cursor::Unknown,
        &Some("codex".into()),
        t0 + floor * 2,
    ));
    observer.agent_observe(
        id,
        generation,
        Some(vec!["Codex prompt".into()]),
        Cursor::Unknown,
        Some("codex".into()),
        pgid,
        false,
        t0 + floor * 2,
    );
    assert_eq!(observer.next_wake(), None);
    observer.retire(id);
    observer.note_program_answered(id);
    assert_eq!(observer.next_wake(), None);
}

/// Claude Code 2.1.280's rm circuit-breaker box, measured 2026-09-23 in a
/// private headless aterm (120 columns; rows 1-24 of the capture).
const RM_BOX: &[&str] = &[
    "",
    "  Get to finished work sooner with Opus 5.5. Switch anytime with /model.",
    "  1 more notice hidden",
    "",
    "\u{276f} Use the Bash tool to run exactly this one line, verbatim, as one call: S=$PWD/tmp; for p in a b; do set -- $p; rm -rf",
    "  $S/$1; done",
    "",
    "  Removing directories tmp/a and tmp/b",
    "  \u{23bf}  $ S=$PWD/tmp; for p in a b; do set -- $p; rm -rf $S/$1; done",
    "",
    "\u{2500}",
    " Bash command",
    "",
    "   S=$PWD/tmp; for p in a b; do set -- $p; rm -rf $S/$1; done",
    "   Remove directories tmp/a and tmp/b",
    "",
    " \u{2502} Dangerous rm operation on possibly-empty variable path: $S/$1 in `rm -rf $S/$1` (bind $1 and rewrite its $S as",
    " \u{2502} \"${S:?}\" or use a literal path)",
    "",
    " Do you want to proceed?",
    " \u{276f} 1. Yes",
    "   2. No",
    "",
    " Esc to cancel \u{00b7} Tab to amend",
];

/// A busy Claude screen: a transcript row, the spinner, the composer
/// between its two rules, the footer (the shape `aterm_phase` reads).
fn busy_screen(secs: u32) -> Vec<String> {
    let rule = "\u{2500}".repeat(120);
    let mut rows = vec![String::new(); 24];
    rows[0] = "\u{23fa} Working on it.".into();
    rows[19] = format!("\u{2736} Deliberating\u{2026} ({secs}s \u{00b7} thinking)");
    rows[20] = rule.clone();
    rows[21] = "\u{276f} ".into();
    rows[22] = rule;
    rows[23] = "  \u{23f5}\u{23f5} bypass permissions on \u{00b7} esc to interrupt".into();
    rows
}

/// The rm box with its top rule at full width, and a transcript row above
/// it that keeps TICKING (a live monitor line) — the audit's INT-1 shape.
fn box_screen(tick: u32) -> Vec<String> {
    let mut rows: Vec<String> = RM_BOX.iter().map(|r| (*r).to_string()).collect();
    rows[10] = "\u{2500}".repeat(120);
    rows[0] = format!("\u{23fa} Monitor tick {tick}");
    rows
}

fn paint(app: &App, sid: u64, rows: &[String]) {
    let pooled = app.pool.get(sid).expect("pooled");
    let mut t = crate::term_lock(&pooled.term);
    if t.cols() != 120 {
        t.resize(24, 120);
    }
    let mut bytes = String::from("\x1b[H\x1b[2J");
    for (i, row) in rows.iter().enumerate() {
        bytes.push_str(&format!("\x1b[{};1H{row}", i + 1));
    }
    t.process(bytes.as_bytes());
}

fn published(app: &App, sid: u64) -> crate::session_timeline::AgentPublication {
    app.pool
        .get(sid)
        .expect("pooled")
        .ctx
        .timeline
        .lock()
        .unwrap()
        .agent()
        .clone()
}

fn app_with_stub() -> (App, u64) {
    let mut app = App::headless_for_test();
    let sid = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(sid));
    (app, sid)
}

/// INT-1: a box raised while a row keeps ticking must reach `agent=` at
/// the next sweep, although the shell status FSM's revision — the old
/// classifier gate — never moves. The negative control is the revision
/// itself: it stays put across the busy→prompt move, so a verdict gated on
/// it (today's `level=quiet` with a box up) would never have been read.
#[test]
fn a_box_under_a_ticking_row_is_published_though_the_status_revision_never_moves() {
    let (mut app, sid) = app_with_stub();
    let t0 = Instant::now();
    let step = Duration::from_millis(300);
    let mut t = t0;
    for secs in 0..3 {
        paint(&app, sid, &busy_screen(secs));
        let _ = app.observe_session_statuses(t);
        t += step;
    }
    let busy = published(&app, sid);
    assert_eq!(busy.word, "busy", "the composer frame identified Claude");
    let revision = app.session_status.revision(sid);

    paint(&app, sid, &box_screen(1));
    let changed = app.observe_session_statuses(t);
    for id in &changed {
        app.refresh_presence_session(*id);
    }
    t += step;
    let prompt = published(&app, sid);
    assert_eq!(prompt.word, "prompt", "the box is read at the next sweep");
    assert_eq!(
        prompt.detail.as_deref(),
        Some("bash:not-read-only"),
        "the box's kind and the read-only verdict, never the command"
    );
    assert_eq!(prompt.rev, busy.rev + 1);
    assert!(changed.contains(&sid), "the caller refreshes presence");
    assert_eq!(
        app.session_status.revision(sid),
        revision,
        "NEGATIVE CONTROL: the status revision did not move, so the old \
         revision-gated classifier would still read busy"
    );
    let record = app.session_status_record(sid).expect("live");
    assert!(
        record.contains(" agent=prompt agent_detail=bash:not-read-only agent_rev="),
        "{record}"
    );
    assert!(record.contains(" level=attention story="), "{record}");
    assert!(record.contains(" why=prompt "), "{record}");
    // The move is on the timeline, so the events digest pushes it.
    let events: Vec<String> = app
        .pool
        .get(sid)
        .unwrap()
        .ctx
        .timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|e| e.kind == "agent-change")
        .map(|e| e.payload.clone())
        .collect();
    let stamp = prompt.stamp.expect("a published verdict carries its read");
    assert_eq!(
        events.last().map(String::as_str),
        Some(
            format!(
                "prompt rev={} gen={} fp={:016x}",
                prompt.rev, stamp.generation, stamp.fp
            )
            .as_str()
        )
    );

    // The row keeps ticking: the zone changed, so it is re-read — and the
    // verdict holds, so nothing is re-published.
    let calls = crate::presence::classifier_calls();
    paint(&app, sid, &box_screen(2));
    let _ = app.observe_session_statuses(t);
    t += step;
    assert_eq!(crate::presence::classifier_calls() - calls, 1);
    assert_eq!(
        published(&app, sid).rev,
        prompt.rev,
        "same verdict, same rev"
    );
    // An unchanged screen costs no classification at all.
    let _ = app.observe_session_statuses(t);
    t += step;
    let _ = app.observe_session_statuses(t);
    assert_eq!(
        crate::presence::classifier_calls() - calls,
        1,
        "no content move, no read"
    );
}

/// THE VERDICT'S STAMP (review major): `agent_gen=`/`agent_fp=` name the
/// screen the verdict was read from, byte-equal to what `status gen=`/
/// `hash=` printed for that screen. A screen that changes after the sweep
/// moves `status gen=` but NOT `agent_gen=` — the NEGATIVE CONTROL, and the
/// reason a press decided from the verdict fences on `agent_gen`: a fence
/// on the fresh `status gen=` would hold over a box nothing classified.
/// A re-read whose zone is unchanged re-confirms the verdict: the stamp
/// moves, the rev does not.
#[test]
fn the_verdict_carries_the_stamp_of_the_screen_it_was_read_from() {
    let field = |record: &str, key: &str| {
        record
            .split(' ')
            .find_map(|kv| kv.strip_prefix(key))
            .map(str::to_string)
            .unwrap_or_else(|| panic!("{key} in {record}"))
    };
    let (mut app, sid) = app_with_stub();
    let mut t = Instant::now();
    let step = Duration::from_millis(300);
    paint(&app, sid, &busy_screen(0));
    let _ = app.observe_session_statuses(t);
    t += step;
    paint(&app, sid, &box_screen(1));
    let _ = app.observe_session_statuses(t);
    t += step;
    let record = app.session_status_record(sid).expect("live");
    assert!(record.contains(" agent=prompt "), "{record}");
    assert_eq!(
        field(&record, "agent_gen="),
        field(&record, "gen="),
        "{record}"
    );
    assert_eq!(
        field(&record, "agent_fp="),
        field(&record, "hash="),
        "{record}"
    );
    let rev = published(&app, sid).rev;

    // The screen moves with no sweep in between: `status` reads the new
    // screen, the verdict still names the one it was read from.
    paint(&app, sid, &box_screen(2));
    let record = app.session_status_record(sid).expect("live");
    assert_ne!(
        field(&record, "agent_gen="),
        field(&record, "gen="),
        "{record}"
    );
    assert_ne!(
        field(&record, "agent_fp="),
        field(&record, "hash="),
        "{record}"
    );

    // The sweep re-reads it (the ticking row, same verdict): the stamp
    // catches up, the rev stays.
    let _ = app.observe_session_statuses(t);
    let record = app.session_status_record(sid).expect("live");
    assert_eq!(
        field(&record, "agent_gen="),
        field(&record, "gen="),
        "{record}"
    );
    assert_eq!(
        field(&record, "agent_fp="),
        field(&record, "hash="),
        "{record}"
    );
    assert_eq!(published(&app, sid).rev, rev, "same verdict, same rev");
}

/// A zone re-read inside [`super::AGENT_MIN_INTERVAL`] is deferred, and
/// the deferral arms a follow-up wake instead of being lost.
#[test]
fn the_rate_floor_defers_a_read_and_arms_a_followup() {
    let g = |seq| crate::control::ScreenGen { epoch: 0, seq };
    let mut obs =
        super::StatusObserver::new(super::StatusPolicy::default(), Duration::from_millis(50));
    let t0 = Instant::now();
    assert!(obs.agent_zone_wanted(1, g(1), Cursor::Unknown, &None, t0));
    let rows = vec!["$ ls".to_string()];
    let _ = obs.agent_observe(1, g(1), Some(rows), Cursor::Unknown, None, -1, false, t0);
    assert!(
        !obs.agent_zone_wanted(1, g(1), Cursor::Unknown, &None, t0),
        "nothing moved"
    );
    let t1 = t0 + Duration::from_millis(100);
    assert!(
        !obs.agent_zone_wanted(1, g(2), Cursor::Unknown, &None, t1),
        "a read 100 ms after the last is deferred"
    );
    let _ = obs.agent_observe(1, g(2), None, Cursor::Unknown, None, -1, false, t1);
    assert!(obs.agents[&1].followup, "and a follow-up is armed");
    assert!(obs.agent_zone_wanted(
        1,
        g(2),
        Cursor::Unknown,
        &None,
        t0 + super::AGENT_MIN_INTERVAL
    ));
    // A resolution in flight keeps the follow-up armed over an unmoved
    // screen, so its answer is read one interval later.
    let t2 = t0 + super::AGENT_MIN_INTERVAL;
    let _ = obs.agent_observe(
        1,
        g(2),
        Some(vec!["$ ls".into()]),
        Cursor::Unknown,
        None,
        7,
        true,
        t2,
    );
    assert!(obs.agents[&1].followup);
    assert!(
        obs.agent_zone_wanted(
            1,
            g(2),
            Cursor::Unknown,
            &Some("sleep".into()),
            t2 + super::AGENT_MIN_INTERVAL
        ),
        "the resolved program re-reads an unchanged zone"
    );
}

/// A redraw can advance the screen generation without changing the live
/// zone or its verdict. At a 50 ms status interval, each such redraw used
/// to repeat the full-screen read because only classification charged the
/// 250 ms floor. The changed ready frame is deferred, then published at
/// the next due read rather than lost.
#[test]
fn unchanged_zone_reads_charge_the_agent_rate_floor() {
    use aterm_phase::prompt::fixtures::{INLINE_REPL_READY, screen};
    let ready = screen(INLINE_REPL_READY);
    let top = ready
        .iter()
        .position(|row| row.starts_with('─'))
        .expect("the prompt box's top rule");
    let mut busy = ready.clone();
    busy[top - 1] = "✶ Deliberating… (3s · thinking)".to_string();
    let claude = Some("claude".to_string());
    let mut obs =
        super::StatusObserver::new(super::StatusPolicy::default(), Duration::from_millis(50));
    let g = |seq| crate::control::ScreenGen { epoch: 0, seq };
    let t0 = Instant::now();
    assert!(obs.agent_zone_wanted(1, g(1), Cursor::Unknown, &claude, t0));
    let first = obs.agent_observe(
        1,
        g(1),
        Some(busy.clone()),
        Cursor::Unknown,
        claude.clone(),
        100,
        false,
        t0,
    );
    assert_eq!(first.publish.map(|p| p.0), Some("busy"));

    let redrawn_at = t0 + super::AGENT_MIN_INTERVAL;
    assert!(obs.agent_zone_wanted(1, g(2), Cursor::Unknown, &claude, redrawn_at));
    let same = obs.agent_observe(
        1,
        g(2),
        Some(busy),
        Cursor::Unknown,
        claude.clone(),
        100,
        false,
        redrawn_at,
    );
    assert!(same.publish.is_none(), "same zone needs no new verdict");

    let early = redrawn_at + Duration::from_millis(50);
    assert!(
        !obs.agent_zone_wanted(1, g(3), Cursor::Unknown, &claude, early),
        "a second full-screen read 50 ms after an unchanged read is too soon"
    );
    let deferred = obs.agent_observe(
        1,
        g(3),
        None,
        Cursor::Unknown,
        claude.clone(),
        100,
        false,
        early,
    );
    assert!(deferred.publish.is_none());
    assert!(obs.agents[&1].followup, "the changed frame remains owed");

    let due = redrawn_at + super::AGENT_MIN_INTERVAL;
    assert!(obs.agent_zone_wanted(1, g(3), Cursor::Unknown, &claude, due));
    let changed = obs.agent_observe(
        1,
        g(3),
        Some(ready),
        Cursor::Unknown,
        claude,
        100,
        false,
        due,
    );
    assert_eq!(changed.publish.map(|p| p.0), Some("idle"));
}

/// A newly resolved foreground program keeps the pre-existing
/// classification clock: a same-zone read must not postpone its verdict.
#[test]
fn a_program_change_is_not_delayed_by_an_unchanged_zone_read() {
    let g = |seq| crate::control::ScreenGen { epoch: 0, seq };
    let mut obs =
        super::StatusObserver::new(super::StatusPolicy::default(), Duration::from_millis(50));
    let old = Some("claude".to_string());
    let new = Some("codex".to_string());
    let rows = vec!["$ ls".to_string()];
    let t0 = Instant::now();
    assert!(obs.agent_zone_wanted(1, g(1), Cursor::Unknown, &old, t0));
    let _ = obs.agent_observe(
        1,
        g(1),
        Some(rows.clone()),
        Cursor::Unknown,
        old.clone(),
        100,
        false,
        t0,
    );
    let redrawn_at = t0 + super::AGENT_MIN_INTERVAL;
    assert!(obs.agent_zone_wanted(1, g(2), Cursor::Unknown, &old, redrawn_at));
    let same = obs.agent_observe(
        1,
        g(2),
        Some(rows.clone()),
        Cursor::Unknown,
        old,
        100,
        false,
        redrawn_at,
    );
    assert!(same.publish.is_none());
    let transitioned_at = redrawn_at + Duration::from_millis(50);
    assert!(
        obs.agent_zone_wanted(1, g(2), Cursor::Unknown, &new, transitioned_at),
        "the new program may be classified despite the recent same-zone read"
    );
    let _ = obs.agent_observe(
        1,
        g(2),
        Some(rows),
        Cursor::Unknown,
        new.clone(),
        100,
        false,
        transitioned_at,
    );
    assert_eq!(obs.agents[&1].program_seen, Some(new));
}

/// The foreground group is discovered after the screen-read decision.
/// When an unchanged read just charged that floor, the departing agent's
/// shell handback gets one immediate follow-up instead of waiting for the
/// next 250 ms status tick.
#[test]
fn group_change_after_unchanged_read_gets_immediate_followup() {
    let g = |seq| crate::control::ScreenGen { epoch: 0, seq };
    let frame = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_OFFER);
    let mut exited = frame.clone();
    exited.push("$ ".to_string());
    let claude = Some("claude".to_string());
    let mut obs =
        super::StatusObserver::new(super::StatusPolicy::default(), Duration::from_millis(50));
    let evidence = super::Evidence {
        shell: None,
        completed_block: None,
        lifecycle: None,
        foreground_job: None,
        activity: super::ActivitySample {
            alt_screen: false,
            content_seq: 1,
            last_output: None,
        },
    };
    let t0 = Instant::now();
    let _ = obs.observe(1, &evidence, t0);
    assert!(obs.agent_zone_wanted(1, g(1), Cursor::Unknown, &claude, t0));
    let first = obs.agent_observe(
        1,
        g(1),
        Some(frame.clone()),
        Cursor::Unknown,
        claude.clone(),
        100,
        false,
        t0,
    );
    assert_eq!(
        first.publish.and_then(|p| p.3),
        Some(aterm_phase::Program::Claude)
    );

    let redrawn_at = t0 + super::AGENT_MIN_INTERVAL;
    let _ = obs.observe(1, &evidence, redrawn_at);
    assert!(obs.agent_zone_wanted(1, g(2), Cursor::Unknown, &claude, redrawn_at));
    let same = obs.agent_observe(
        1,
        g(2),
        Some(frame),
        Cursor::Unknown,
        claude.clone(),
        100,
        false,
        redrawn_at,
    );
    assert!(same.publish.is_none());

    let group_moved_at = redrawn_at + Duration::from_millis(50);
    obs.note_output(1, group_moved_at);
    let _ = obs.observe(1, &evidence, group_moved_at);
    assert!(!obs.agent_zone_wanted(1, g(3), Cursor::Unknown, &claude, group_moved_at));
    let deferred = obs.agent_observe(
        1,
        g(3),
        None,
        Cursor::Unknown,
        claude,
        200,
        false,
        group_moved_at,
    );
    assert!(deferred.publish.is_none());
    assert!(obs.due(1, group_moved_at), "the new group is due now");
    assert_eq!(obs.next_wake(), Some(group_moved_at));

    assert!(obs.agent_zone_wanted(1, g(3), Cursor::Unknown, &None, group_moved_at));
    let handback = obs.agent_observe(
        1,
        g(3),
        Some(exited),
        Cursor::Unknown,
        None,
        200,
        false,
        group_moved_at,
    );
    assert_eq!(handback.publish.map(|p| p.0), Some("-"));
}

/// A BLOCK READ CUT AT ITS CAP OWES ONE MORE LOOK WITHOUT DELAYING A
/// HANDBACK'S: the session log's block read stopped at its cap in the same
/// sweep that saw the foreground group hand an exited agent back to the
/// shell. [`super::StatusObserver::owe_look`] keeps the handback's one
/// immediate follow-up immediate (the sweep-level owed look is
/// `a_burst_past_the_block_reads_cap_is_read_at_the_next_owed_look`).
/// NEGATIVE CONTROL: owing it as output
/// ([`super::StatusObserver::note_output`]) pushes the handback's look one
/// interval on.
#[test]
fn a_capped_block_read_owes_a_look_without_delaying_a_handback() {
    let interval = Duration::from_millis(50);
    // The sweep that saw the group move after an unchanged read (as in
    // `group_change_after_unchanged_read_gets_immediate_followup`), then
    // `owe`; the wait until the follow-up it leaves.
    let handback_wait = |owe: &dyn Fn(&mut super::StatusObserver, Instant)| {
        let g = |seq| crate::control::ScreenGen { epoch: 0, seq };
        let frame = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_OFFER);
        let claude = Some("claude".to_string());
        let mut obs = super::StatusObserver::new(super::StatusPolicy::default(), interval);
        let evidence = super::Evidence {
            shell: None,
            completed_block: None,
            lifecycle: None,
            foreground_job: None,
            activity: super::ActivitySample {
                alt_screen: false,
                content_seq: 1,
                last_output: None,
            },
        };
        let t0 = Instant::now();
        let _ = obs.observe(1, &evidence, t0);
        assert!(obs.agent_zone_wanted(1, g(1), Cursor::Unknown, &claude, t0));
        let _ = obs.agent_observe(
            1,
            g(1),
            Some(frame.clone()),
            Cursor::Unknown,
            claude.clone(),
            100,
            false,
            t0,
        );
        let redrawn_at = t0 + super::AGENT_MIN_INTERVAL;
        let _ = obs.observe(1, &evidence, redrawn_at);
        assert!(obs.agent_zone_wanted(1, g(2), Cursor::Unknown, &claude, redrawn_at));
        let _ = obs.agent_observe(
            1,
            g(2),
            Some(frame),
            Cursor::Unknown,
            claude.clone(),
            100,
            false,
            redrawn_at,
        );
        let moved_at = redrawn_at + interval;
        let _ = obs.observe(1, &evidence, moved_at);
        let _ = obs.agent_zone_wanted(1, g(3), Cursor::Unknown, &claude, moved_at);
        let _ = obs.agent_observe(1, g(3), None, Cursor::Unknown, claude, 200, false, moved_at);
        owe(&mut obs, moved_at);
        obs.next_wake()
            .map(|w| w.saturating_duration_since(moved_at))
    };
    assert_eq!(
        handback_wait(&|_, _| {}),
        Some(Duration::ZERO),
        "PRECONDITION: the handback's follow-up is immediate"
    );
    assert_eq!(
        handback_wait(&|obs, _| obs.owe_look(1)),
        Some(Duration::ZERO),
        "the capped read's look leaves the handback's immediate"
    );
    assert_eq!(
        handback_wait(&|obs, at| obs.note_output(1, at)),
        Some(interval),
        "NEGATIVE CONTROL: owed as output, it waits an interval"
    );
}

/// A BURST PAST THE BLOCK READ'S CAP IS READ AT THE NEXT OWED LOOK, THROUGH
/// THE REAL SWEEP: fifty commands finished between two sweeps; the first
/// logs the oldest [`super::BLOCK_READ_MAX`] and owes the session a look no
/// later than one observation interval on, at which the second logs the
/// rest. Every command is a run, in order.
#[test]
fn a_burst_past_the_block_reads_cap_is_read_at_the_next_owed_look() {
    let prompt = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\";
    let mut app = App::headless_for_test();
    let front = app.front_terminal(WindowId(0)).expect("a front terminal");
    let (session, term) = (front.session, std::sync::Arc::clone(&front.term));
    crate::term_lock(&term).process(prompt);
    let t0 = Instant::now();
    let _ = app.observe_session_statuses(t0);
    let mut burst = Vec::new();
    for i in 0..50 {
        burst.extend_from_slice(
            format!("true {i}\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\").as_bytes(),
        );
        burst.extend_from_slice(prompt);
    }
    crate::term_lock(&term).process(&burst);
    let runs = |app: &App| {
        let timeline = std::sync::Arc::clone(&app.pool.get(session).expect("pooled").ctx.timeline);
        let g = timeline.lock().unwrap();
        g.slog()
            .log()
            .window()
            .filter(|r| r.op == aterm_slog::Op::Close && r.k == aterm_slog::Kind::Cmd)
            .count()
    };
    let t1 = t0 + Duration::from_secs(1);
    let _ = app.observe_session_statuses(t1);
    assert_eq!(runs(&app), super::BLOCK_READ_MAX, "the cap's worth");
    let wake = app
        .session_status
        .next_wake()
        .expect("the rest owes a look");
    assert!(
        wake <= t1 + app.session_status.min_interval,
        "owed within one interval: {:?}",
        wake.saturating_duration_since(t1)
    );
    let _ = app.observe_session_statuses(wake);
    assert_eq!(runs(&app), 50, "every command is a run");
}

/// THE SWEEP'S CAPPED BLOCK READ OWES ITS LOOK WITHOUT DELAYING A GROUP
/// MOVE'S, THROUGH THE REAL SWEEP, with no process and no PTY: the group in
/// front is the injected probe's (`SessionFactory::fg_probe`), asked about a
/// master no file holds. Seventy commands finished between two sweeps, so
/// each of the next two block reads stops at [`super::BLOCK_READ_MAX`] with
/// commands left. The second of those sweeps also sees the group move over an
/// unchanged screen, which owes ONE immediate look
/// ([`super::StatusObserver::agent_observe`]'s handback); the capped read's
/// owed look ([`super::StatusObserver::owe_look`] at the sweep's call site)
/// leaves it immediate. NEGATIVE CONTROL (the call site, by mutation): owing
/// the capped read as output (`note_output(id, now)` in its place) pushes
/// that look an observation interval on, and this test fails.
#[test]
fn the_sweeps_capped_read_leaves_a_group_moves_look_immediate() {
    thread_local! {
        static FRONT: std::cell::Cell<i32> = const { std::cell::Cell::new(-1) };
    }
    /// The injected probe: the group this test put in front.
    fn front(_master: i32) -> i32 {
        FRONT.with(std::cell::Cell::get)
    }
    // Groups no process holds: past every pid the kernel issues.
    const SHELL: i32 = 2_000_000_001;
    const JOB: i32 = 2_000_000_002;
    let prompt = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\";
    let mut app = App::headless_for_test();
    app.session_factory.fg_probe = front;
    let shown = app.front_terminal(WindowId(0)).expect("a front terminal");
    let (session, term) = (shown.session, std::sync::Arc::clone(&shown.term));
    // The sweep asks the probe only for a session with a master: an fd
    // number no file holds (its shell pid stays the stub's, so teardown
    // signals nothing).
    app.pool
        .sessions
        .get_mut(&session)
        .expect("pooled")
        .session
        .master = i32::MAX;
    FRONT.with(|f| f.set(SHELL));
    let step = app
        .session_status
        .min_interval
        .max(super::AGENT_MIN_INTERVAL);
    crate::term_lock(&term).process(prompt);
    let t0 = Instant::now();
    let _ = app.observe_session_statuses(t0);
    let mut burst = Vec::new();
    for i in 0..70 {
        burst.extend_from_slice(
            format!("true {i}\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\").as_bytes(),
        );
        burst.extend_from_slice(prompt);
    }
    crate::term_lock(&term).process(&burst);
    // The look that reads the new screen and the first cap's worth of blocks.
    let t1 = t0 + step;
    let _ = app.observe_session_statuses(t1);
    // The shell hands its terminal to a job; the screen does not move.
    FRONT.with(|f| f.set(JOB));
    let t2 = t1 + step;
    let _ = app.observe_session_statuses(t2);
    let wake = app.session_status.next_wake().expect("a look is owed");
    let timeline = std::sync::Arc::clone(&app.pool.get(session).expect("pooled").ctx.timeline);
    let closed = timeline
        .lock()
        .unwrap()
        .slog()
        .log()
        .window()
        .filter(|r| r.op == aterm_slog::Op::Close && r.k == aterm_slog::Kind::Cmd)
        .count();
    assert_eq!(
        closed,
        2 * super::BLOCK_READ_MAX,
        "PRECONDITION: both reads were cut at the cap"
    );
    assert!(
        wake <= t2,
        "the group move's look is immediate: owed {:?} after the sweep",
        wake.saturating_duration_since(t2)
    );
}

/// THE FINAL FRAME OF A BURST THAT LANDED BETWEEN LOOKS (2026-09-24,
/// live: a box answered and a static busy screen drawn still read
/// `agent=prompt` after the host had cleared its badge; an exited agent's
/// prompt left `program=claude` standing). A look that saw nothing move
/// arms no follow-up, and output inside the interval runs no look: the
/// output wake's [`super::StatusObserver::note_output`] owes the session a
/// look one interval after the burst (never before its next due instant)
/// — and so does a look the terminal lock refused
/// ([`super::StatusObserver::note_skipped`]). NEGATIVE CONTROL: before the
/// burst nothing is owed that soon.
#[test]
fn output_between_looks_is_owed_a_look_within_one_interval_of_it() {
    let g = |seq| crate::control::ScreenGen { epoch: 0, seq };
    let interval = Duration::from_millis(250);
    let mut obs = super::StatusObserver::new(super::StatusPolicy::default(), interval);
    let ev = super::Evidence {
        shell: None,
        completed_block: None,
        lifecycle: None,
        foreground_job: None,
        activity: super::ActivitySample {
            alt_screen: false,
            content_seq: 1,
            last_output: None,
        },
    };
    let rows = vec!["$ ls".to_string()];
    let t0 = Instant::now();
    let _ = obs.observe(1, &ev, t0);
    assert!(obs.agent_zone_wanted(1, g(1), Cursor::Unknown, &None, t0));
    let _ = obs.agent_observe(1, g(1), Some(rows), Cursor::Unknown, None, -1, false, t0);
    // A look at the same screen: nothing moved, nothing owed by it.
    let t1 = t0 + interval;
    let _ = obs.observe(1, &ev, t1);
    assert!(!obs.agent_zone_wanted(1, g(1), Cursor::Unknown, &None, t1));
    let _ = obs.agent_observe(1, g(1), None, Cursor::Unknown, None, -1, false, t1);
    let due = obs.sessions[&1].next_due;
    assert!(
        obs.next_wake().is_none_or(|w| w > due),
        "NEGATIVE CONTROL: nothing owes a look at {due:?} yet"
    );
    // A burst lands 10 ms later: not due, so no look — but one is owed,
    // one interval after it.
    let burst = t1 + Duration::from_millis(10);
    obs.note_output(1, burst);
    let owed = burst + interval;
    assert!(
        owed >= due,
        "fixture: the burst's interval ends past the floor"
    );
    assert_eq!(obs.next_wake(), Some(owed), "a look once the burst paused");
    // That look reads the new frame, and owes nothing more once it holds.
    let t2 = owed;
    let _ = obs.observe(1, &ev, t2);
    assert!(obs.agent_zone_wanted(1, g(2), Cursor::Unknown, &None, t2));
    let _ = obs.agent_observe(
        1,
        g(2),
        Some(vec!["$ ls".into(), "x".into()]),
        Cursor::Unknown,
        None,
        -1,
        false,
        t2,
    );
    let t3 = t2 + interval;
    let _ = obs.observe(1, &ev, t3);
    let _ = obs.agent_observe(1, g(2), None, Cursor::Unknown, None, -1, false, t3);
    let due = obs.sessions[&1].next_due;
    assert!(obs.next_wake().is_none_or(|w| w > due), "settled again");
    // A look the lock refused owes the next one as well.
    obs.note_skipped(1, t3);
    assert_eq!(obs.next_wake(), Some(obs.sessions[&1].next_due));
}

/// OUTPUT THAT KEEPS ARRIVING IS NOT A POLL (the spin matrix's claude row,
/// aterm-conformance `tests/spin.rs`, after 2026-09-24's owed look
/// landed: 39 timer wakes in 10 s against a quiet loop's 5). A program
/// that repaints without moving the grid ~6x/s — a Claude Code spinner —
/// wakes the loop by its own output, and each output wake's sweep looks
/// whenever the session is due; an owed look pinned to the slot's next due
/// instant fired between those wakes, 4x/s, forever. The owed look waits
/// for the output to PAUSE: every burst pushes it one interval on, so no
/// timer precedes the next burst, and the last burst is still read within
/// one interval of it. RED before: the owed wake was the next due instant.
#[test]
fn output_that_keeps_arriving_owes_no_look_before_it_pauses() {
    let g = |seq| crate::control::ScreenGen { epoch: 0, seq };
    let interval = Duration::from_millis(250);
    let gap = Duration::from_millis(167);
    let mut obs = super::StatusObserver::new(super::StatusPolicy::default(), interval);
    let ev = super::Evidence {
        shell: None,
        completed_block: None,
        lifecycle: None,
        foreground_job: None,
        activity: super::ActivitySample {
            alt_screen: false,
            content_seq: 1,
            last_output: None,
        },
    };
    // Settle first: the status FSM's first sample serves a dwell, and the
    // first look (a screen never read) owes its own follow-up. Past both,
    // the only deadline left to judge is the one output owes.
    let start = Instant::now();
    let _ = obs.observe(1, &ev, start);
    let _ = obs.agent_observe(
        1,
        g(1),
        Some(vec!["* Thinking".into()]),
        Cursor::Unknown,
        None,
        -1,
        false,
        start,
    );
    let t0 = start + Duration::from_secs(5);
    let _ = obs.observe(1, &ev, t0);
    let _ = obs.agent_observe(1, g(1), None, Cursor::Unknown, None, -1, false, t0);
    assert_eq!(
        obs.next_wake(),
        None,
        "fixture: a settled, unmoved session owes nothing"
    );
    // Twelve output wakes ~6/s over an unmoved screen, in the output
    // wake's order: `note_output`, then the sweep, which looks only when
    // the session is due (the rate gate) and answers for the burst.
    let mut at = t0;
    let mut looks = 0;
    for _ in 0..12 {
        at += gap;
        obs.note_output(1, at);
        if obs.sessions[&1].next_due <= at {
            let _ = obs.observe(1, &ev, at);
            let _ = obs.agent_observe(1, g(1), None, Cursor::Unknown, None, -1, false, at);
            looks += 1;
        }
        let next_burst = at + gap;
        assert!(
            obs.next_wake().is_none_or(|w| w > next_burst),
            "no timer fires before the next burst: {:?} vs {next_burst:?}",
            obs.next_wake()
        );
    }
    assert!(
        looks >= 5,
        "fixture: the output wakes' own sweeps looked ({looks})"
    );
    // The LAST burst lands between looks, and the output pauses: it is
    // read within one interval of it.
    let last = at + gap;
    obs.note_output(1, last);
    assert!(
        obs.sessions[&1].next_due > last,
        "fixture: not due at the burst"
    );
    assert_eq!(obs.next_wake(), Some(last + interval));
}

/// THE CHANGE GATE HASHES THE ZONE THE VERDICT READS (the review of
/// 2026-09-26): the sweep re-derives the verdict only when its zone's
/// hash moved, and that hash covered the grid's last 40 rows. Claude
/// Code's inline renderer draws the REPL at the top of a tall pane, so a
/// turn that ends ABOVE those rows — the spinner row over the prompt box
/// replaced, every row from the box down unchanged — moved no hash, and
/// the verdict stayed `busy`. The gate hashes the live zone
/// (`presence::live_zone`): the idle REPL is classified and published.
/// NEGATIVE CONTROL: the same screen again under a new generation moves
/// no hash, and nothing is re-derived.
#[test]
fn a_change_above_the_last_40_rows_of_an_inline_repl_is_classified() {
    use aterm_phase::prompt::fixtures::{INLINE_REPL_READY, screen};
    let ready = screen(INLINE_REPL_READY);
    assert_eq!(ready.len(), 50);
    let top = ready
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the top rule");
    let mut busy = ready.clone();
    busy[top - 1] = "✶ Deliberating… (3s · thinking)".to_string();
    assert!(top - 1 < ready.len() - 40, "above the grid's last 40 rows");
    assert_eq!(busy[ready.len() - 40..], ready[ready.len() - 40..]);
    let claude = Some("claude".to_string());
    let mut obs =
        super::StatusObserver::new(super::StatusPolicy::default(), Duration::from_millis(50));
    let t0 = Instant::now();
    let mut look = |seq: u64, rows: &[String], after: u64| {
        let at = t0 + Duration::from_secs(after);
        let generation = crate::control::ScreenGen { epoch: 0, seq };
        assert!(obs.agent_zone_wanted(1, generation, Cursor::Unknown, &claude, at));
        obs.agent_observe(
            1,
            generation,
            Some(rows.to_vec()),
            Cursor::Unknown,
            claude.clone(),
            100,
            false,
            at,
        )
        .publish
        .map(|p| p.0)
    };
    assert_eq!(look(1, &busy, 0), Some("busy"));
    assert_eq!(look(2, &ready, 1), Some("idle"), "the turn's end");
    assert_eq!(look(3, &ready, 2), None, "the control: nothing moved");
}

/// THE CURSOR MOVING ALONE IS READ (2026-09-27): Claude Code's `idle` is
/// the prompt box that holds the terminal's cursor, and the cursor moves
/// without the content seq — a frame drawn whole, then the cursor stepped
/// into its box. The gate reads the zone again when the cursor moved
/// under an unchanged generation, and publishes `idle` once it is in the
/// box. NEGATIVE CONTROL: the same generation and cursor again read
/// nothing.
#[test]
fn a_cursor_that_steps_into_the_box_is_read() {
    use aterm_phase::prompt::fixtures::{INLINE_REPL_READY, cursor, screen};
    let ready = screen(INLINE_REPL_READY);
    let (caret, _) = cursor(INLINE_REPL_READY).expect("measured");
    let claude = Some("claude".to_string());
    let mut obs =
        super::StatusObserver::new(super::StatusPolicy::default(), Duration::from_millis(50));
    let t0 = Instant::now();
    let generation = crate::control::ScreenGen { epoch: 0, seq: 7 };
    let mut look = |cursor: Cursor, after: u64| {
        let at = t0 + Duration::from_secs(after);
        obs.agent_zone_wanted(1, generation, cursor, &claude, at)
            .then(|| {
                obs.agent_observe(
                    1,
                    generation,
                    Some(ready.clone()),
                    cursor,
                    claude.clone(),
                    100,
                    false,
                    at,
                )
                .publish
                .map(|p| p.0)
            })
            .flatten()
    };
    let under = Cursor::At(Some(caret + 3));
    let in_box = Cursor::At(Some(caret));
    assert_eq!(look(under, 0), Some("unknown"), "the cursor under the box");
    assert_eq!(look(in_box, 1), Some("idle"), "the cursor stepped in");
    assert_eq!(look(in_box, 2), None, "the control: nothing moved");
}

/// Tier-1 for `ClaudeIdleAtComposer` (the live e2e's NEW-1, 2026-09-26,
/// and its review's same-tab relaunch): over EVERY reachable state of the
/// model, the shipping observer — one `agent_observe` of the screen the
/// state names, under `program=claude`, with the terminal CURSOR measured
/// on it — publishes the verdict the model's `Look` publishes there:
/// `prompt` for the trust dialog, `idle` only for the NEW REPL drawn
/// whole, neither for the shell's rows (before the dialog, after its
/// press, or with no dialog) nor for the half-drawn REPL — in a new tab,
/// and with the previous run's prompt box on the screen above the new
/// launch line (`stale`). So `await agent idle` answers exactly where the
/// model's `Type` is enabled, and the model proves a prompt typed there
/// is never lost. The screens are the frames Claude Code 2.1.283 drew
/// (aterm-phase's fixtures, each with its measured cursor; the one dialog
/// with none measured is 2.1.280's, read with the cursor on none of its
/// rows), each model screen looked at through EVERY measured frame of it:
/// the fullscreen renderer (`LAUNCH_*`, `SHELL_MODE*`: `!` typed, the
/// caret `!`, the REPL up and taking keys), the inline one in a new tab
/// (`INLINE_*`: on the main grid under the launch line, 150x50, blank
/// rows below — its whole REPL above the grid's last 40 rows), and the
/// inline one relaunched in the same tab (`INLINE_RELAUNCH_*`). NEGATIVE
/// CONTROLS: the reader before 2026-09-26 (`Buggy=1`) publishes idle on
/// the shell's rows and the half-drawn REPL, the reader of 2026-09-26
/// (`Buggy=2`, the box by its frame alone) on the relaunch's shell rows
/// under the previous run's box — and the real observer disagrees with
/// each there, and only there. RED: read by its frame alone (the cursor
/// not given, the rule this branch had), every frame publishes what
/// `Buggy=2` publishes: `idle` on that old box.
#[test]
fn claude_is_published_idle_exactly_where_the_model_types() {
    use aterm_phase::prompt::fixtures::{
        INLINE_RELAUNCH_BEFORE_REPL, INLINE_RELAUNCH_REPL_HALF_DRAWN, INLINE_RELAUNCH_REPL_READY,
        INLINE_RELAUNCH_TRUST, INLINE_REPL_HALF_DRAWN, INLINE_REPL_READY, INLINE_TRUST,
        LAUNCH_BEFORE_REPL, LAUNCH_REPL_HALF_DRAWN, LAUNCH_REPL_READY, SHELL_MODE,
        SHELL_MODE_DRAFT, TRUST, cursor, screen,
    };
    let model = aterm_models_harness::claude_idle_at_composer_model();
    let old = aterm_spec::interp::with_buggy(&model, 1);
    let framed = aterm_spec::interp::with_buggy(&model, 2);
    // `screens[stale][screen]`: every measured frame of each.
    let screens: [[Vec<&str>; 4]; 2] = [
        [
            vec![LAUNCH_BEFORE_REPL],
            vec![TRUST, INLINE_TRUST],
            vec![LAUNCH_REPL_HALF_DRAWN, INLINE_REPL_HALF_DRAWN],
            vec![
                LAUNCH_REPL_READY,
                SHELL_MODE,
                SHELL_MODE_DRAFT,
                INLINE_REPL_READY,
            ],
        ],
        [
            vec![INLINE_RELAUNCH_BEFORE_REPL],
            vec![INLINE_RELAUNCH_TRUST],
            vec![INLINE_RELAUNCH_REPL_HALF_DRAWN],
            vec![INLINE_RELAUNCH_REPL_READY],
        ],
    ];
    let project = |word: &str| match word {
        "idle" => 1,
        "prompt" => 2,
        _ => 0,
    };
    let claude = Some("claude".to_string());
    let publish = |text: &str, cursor: Cursor| {
        let mut obs =
            super::StatusObserver::new(super::StatusPolicy::default(), Duration::from_millis(50));
        let t0 = Instant::now();
        let generation = crate::control::ScreenGen { epoch: 0, seq: 1 };
        assert!(obs.agent_zone_wanted(1, generation, cursor, &claude, t0));
        let step = obs.agent_observe(
            1,
            generation,
            Some(screen(text)),
            cursor,
            claude.clone(),
            100,
            false,
            t0,
        );
        step.publish.expect("the zone was classified").0
    };
    let measured = |text: &str| Cursor::At(cursor(text).map(|(row, _)| row));
    // Every reachable state, breadth first.
    let mut seen = vec![model.init_state()];
    let mut next = 0;
    let mut looked = std::collections::BTreeSet::new();
    let mut disagreed = [
        std::collections::BTreeSet::new(),
        std::collections::BTreeSet::new(),
    ];
    while next < seen.len() {
        let state = seen[next].clone();
        next += 1;
        if model.action_enabled("Look", &state) {
            let at = usize::try_from(state["screen"]).expect("a screen");
            let stale = usize::try_from(state["stale"]).expect("stale");
            let modeled = model.successors("Look", &state).remove(0);
            for (frame, text) in screens[stale][at].iter().enumerate() {
                let word = publish(text, measured(text));
                assert_eq!(
                    project(word),
                    modeled["published"],
                    "stale {stale} screen {at} frame {frame}: the observer published `{word}`"
                );
                assert_eq!(
                    model.action_enabled("Type", &modeled),
                    word == "idle",
                    "stale {stale} screen {at} frame {frame}"
                );
                looked.insert((stale, at, frame));
                for (bug, buggy) in [&old, &framed].into_iter().enumerate() {
                    let theirs = buggy.successors("Look", &state).remove(0)["published"];
                    if theirs != project(word) {
                        assert_eq!(theirs, 1, "Buggy={}: it said idle", bug + 1);
                        disagreed[bug].insert((stale, at));
                    }
                }
                // RED: the frame alone, the cursor not given.
                let by_frame = publish(text, Cursor::Unknown);
                let theirs = framed.successors("Look", &state).remove(0)["published"];
                assert_eq!(
                    project(by_frame),
                    theirs,
                    "stale {stale} screen {at} frame {frame}: the frame rule is Buggy=2"
                );
            }
        }
        for action in &model.actions {
            for s in model.successors(action.name, &state) {
                if !seen.contains(&s) {
                    seen.push(s);
                }
            }
        }
    }
    assert_eq!(
        looked.len(),
        screens.iter().flatten().map(Vec::len).sum::<usize>(),
        "every frame of every screen was looked at: {looked:?}"
    );
    assert_eq!(
        disagreed[0].iter().copied().collect::<Vec<_>>(),
        [(0, 0), (0, 2), (1, 0), (1, 2)],
        "NEGATIVE CONTROL: the old reader, refuted before the REPL"
    );
    assert_eq!(
        disagreed[1].iter().copied().collect::<Vec<_>>(),
        [(1, 0)],
        "NEGATIVE CONTROL: the box by its frame alone, refuted on the relaunch"
    );
}

/// THE DEPARTED AGENT'S LAST FRAME (2026-09-24): the look that first sees
/// a new foreground group read `program=` from the timeline before this
/// sweep's `note_foreground_group` forgot it — the name of the group that
/// LEFT. Judged under it, an exited Claude Code's last frame published
/// Claude's reader again, and the in-GUI host went on seeing the agent
/// (the relaunch on exit missed exits that way). The zone of a moved
/// group is judged under no name: `agent=-`, no reader. NEGATIVE CONTROL:
/// the same rows in the SAME group are the agent's.
#[test]
fn a_group_that_left_is_never_judged_under_its_name() {
    let g = |seq| crate::control::ScreenGen { epoch: 0, seq };
    let frame = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::END_OFFER);
    let mut exited = frame.clone();
    exited.push("$ ".to_string());
    let claude = Some("claude".to_string());
    let reader = |step: &super::AgentStep| step.publish.as_ref().map(|p| (p.0, p.3));
    for group_after in [200, 100] {
        let mut obs =
            super::StatusObserver::new(super::StatusPolicy::default(), Duration::from_millis(50));
        let t0 = Instant::now();
        assert!(obs.agent_zone_wanted(1, g(1), Cursor::Unknown, &claude, t0));
        let step = obs.agent_observe(
            1,
            g(1),
            Some(frame.clone()),
            Cursor::Unknown,
            claude.clone(),
            100,
            true,
            t0,
        );
        assert_eq!(
            reader(&step).and_then(|(_, r)| r),
            Some(aterm_phase::Program::Claude),
            "the agent, named"
        );
        let t1 = t0 + super::AGENT_MIN_INTERVAL;
        assert!(obs.agent_zone_wanted(1, g(2), Cursor::Unknown, &claude, t1));
        let step = obs.agent_observe(
            1,
            g(2),
            Some(exited.clone()),
            Cursor::Unknown,
            claude.clone(),
            group_after,
            true,
            t1,
        );
        let got = reader(&step);
        if group_after == 100 {
            assert_eq!(
                got.and_then(|(_, r)| r),
                Some(aterm_phase::Program::Claude),
                "NEGATIVE CONTROL: the same group is still the agent"
            );
        } else {
            assert_eq!(
                got,
                Some(("-", None)),
                "the departed group's frame is no agent's"
            );
        }
    }
}

/// THE GATE HASHES THE ZONE THE VERDICT READS (the live run of
/// 2026-09-26: Claude Code 2.1.283 in a fresh 149x62 pane). The pane
/// idle at its top, then the folder-trust dialog drawn there: the last
/// 40 rows of the SCREEN are blank on both, so a gate that hashed them
/// passed over the dialog, and `agent=idle` stood while the hosted loop
/// slept on it. The gate hashes [`crate::presence::live_zone`], the rows
/// the verdict reads: the dialog is classified and published a trust
/// prompt. NEGATIVE CONTROL: the dialog read again, nothing in the zone
/// moved, is not classified again. The cursor is where Claude Code parks
/// it: the idle prompt box's caret row, then the dialog's focused option
/// (the measured row 17).
#[test]
fn a_dialog_above_a_blank_foot_passes_the_classifier_gate() {
    use aterm_phase::prompt::fixtures::{TRUST_FRESH_PANE, composer, rows, screen};
    let g = |seq| crate::control::ScreenGen { epoch: 0, seq };
    let claude = Some("claude".to_string());
    let word = |step: &super::AgentStep| step.publish.as_ref().map(|p| (p.0, p.1.clone()));
    let mut idle = rows(&["\u{23fa} Done.", ""]);
    idle.extend(composer("  ? for shortcuts"));
    idle.resize(62, String::new());
    let trust = screen(TRUST_FRESH_PANE);
    assert_eq!(trust.len(), 62);
    assert_eq!(idle[22..], trust[22..], "the same blank foot");
    let mut obs =
        super::StatusObserver::new(super::StatusPolicy::default(), Duration::from_millis(50));
    let t0 = Instant::now();
    let caret = Cursor::At(idle.iter().position(|r| r.starts_with('❯')));
    let focus = Cursor::At(trust.iter().position(|r| r == " ❯ No, exit"));
    assert_eq!(focus, Cursor::At(Some(17)), "the measured cursor");
    let step = obs.agent_observe(1, g(1), Some(idle), caret, claude.clone(), 100, false, t0);
    assert_eq!(word(&step), Some(("idle", None)));
    let t1 = t0 + super::AGENT_MIN_INTERVAL;
    let step = obs.agent_observe(
        1,
        g(2),
        Some(trust.clone()),
        focus,
        claude.clone(),
        100,
        false,
        t1,
    );
    assert!(step.reading_changed, "{:?}", word(&step));
    assert_eq!(word(&step), Some(("prompt", Some("trust".to_string()))));
    let t2 = t1 + super::AGENT_MIN_INTERVAL;
    let step = obs.agent_observe(1, g(3), Some(trust), focus, claude, 100, false, t2);
    assert_eq!(word(&step), None, "the zone did not move");
}

/// `history_lost=` (2026-09-26): the history lines this session's update
/// handoffs could not carry, summed — what its adoption brought on its
/// record plus every import that failed here — and `0` for a session that
/// lost none. The update's total rides one band row as well
/// (`App::settle_handoff_history`), never the log alone. NEGATIVE CONTROL:
/// an update that carried every line posts no row.
#[cfg(unix)] // App::settle_handoff_history is the unix handoff's
#[test]
fn the_status_record_counts_the_history_an_update_could_not_carry() {
    use crate::handoff_history::{ImportReport, record_reports};
    let (mut app, sid) = app_with_stub();
    let record = app.session_status_record(sid).expect("live");
    assert!(
        record.contains(
            " history_lost=0 journal=off journal_off=keeper-off integration_rev=- born=cold render=ok \
             resizes=0 flaps=0 land_at=- task=- pause=- task_state=- task_rev=0 \
             log_seq=1 log_ep=1 run=- turn=- step=- need=- need_since_ms=- gen="
        ),
        "{record}"
    );
    let whole = [ImportReport {
        session: sid,
        imported: 5000,
        dropped: 0,
        withheld: 0,
        held: 0,
        held_pane: false,
        failed_lines: 0,
        links_dropped: 0,
        failed: None,
        cleared: false,
    }];
    record_reports(&app.store, &whole);
    app.settle_handoff_history(&whole);
    assert!(
        !app.has_live_message("Couldn't carry all scrollback"),
        "nothing was lost: no row"
    );
    // The adoption's carried count, then an import that failed here.
    app.store.write().unwrap().add_history_lost(sid, 1200);
    let failed = [ImportReport {
        session: sid,
        imported: 0,
        dropped: 1200,
        withheld: 0,
        held: 0,
        held_pane: false,
        failed_lines: 34,
        links_dropped: 0,
        failed: Some("the sidecar arrived with a sha other than its stamp".into()),
        cleared: false,
    }];
    record_reports(&app.store, &failed);
    app.settle_handoff_history(&failed);
    let record = app.session_status_record(sid).expect("live");
    assert!(
        record.contains(
            " history_lost=1234 journal=off journal_off=keeper-off integration_rev=- born=cold render=ok \
             resizes=0 flaps=0 land_at=- task=- pause=- task_state=- task_rev=0 \
             log_seq=1 log_ep=1 run=- turn=- step=- need=- need_since_ms=- gen="
        ),
        "{record}"
    );
    assert!(app.has_live_message("Couldn't carry all scrollback"));
}

/// THE REGISTRY'S FROZEN MARK IS LOWERED BY A MEASUREMENT, NEVER RAISED
/// (gap audit 2026-09-24: a tab healed by the live upgrade's relaunch line
/// read `path=frozen` through seven handoffs, because nothing ever lowered
/// the mark; review of 2026-09-25: a one-off `PATH=` override then RAISED
/// it, and the band counted that tab "from before this update"). A live
/// reading lowers it — so the handoff's carry and the managed record's
/// count lose the tab. A frozen reading from one job, and then from a
/// second, leaves an unmarked tab unmarked: one reads `unconfirmed`, two
/// read `frozen` as measured. NEGATIVE CONTROL: with no reading, the
/// carried mark stands through a sweep.
#[test]
fn a_measured_path_lowers_the_carried_frozen_mark_and_never_raises_it() {
    use crate::session_status::program::PathVerdict;
    let (mut app, sid) = app_with_stub();
    app.store.write().unwrap().mark_frozen_path(sid);
    let t0 = Instant::now();
    let _ = app.observe_session_statuses(t0);
    assert!(
        app.store.read().unwrap().has_frozen_path(sid),
        "no reading: the mark stands"
    );
    let record = app.session_status_record(sid).expect("live");
    assert!(record.contains(" path=frozen "), "{record}");
    assert!(record.contains(" path_evidence=carried "), "{record}");
    let measure = |app: &App, verdict: PathVerdict| {
        let pooled = app.pool.get(sid).unwrap();
        let mut tl = pooled.ctx.timeline.lock().unwrap();
        tl.note_foreground_group(4242);
        tl.set_program(4242, Some("claude".into()));
        tl.set_leader(4242, Some("foreign"), Some(verdict));
    };
    measure(&app, PathVerdict::Live);
    let _ = app.observe_session_statuses(t0 + Duration::from_millis(300));
    assert!(
        !app.store.read().unwrap().has_frozen_path(sid),
        "a live reading lowers the mark"
    );
    let record = app.session_status_record(sid).expect("live");
    assert!(record.contains(" path=live program=claude "), "{record}");
    assert!(record.contains(" path_evidence=measured:"), "{record}");
    assert!(record.contains(" copy=foreign upgrade=- "), "{record}");
    // One job's frozen reading (a `PATH=/usr/bin:/bin tool` override
    // reads exactly so) is not the shell's: not raised, and `path=` stays.
    measure(&app, PathVerdict::Frozen);
    let _ = app.observe_session_statuses(t0 + Duration::from_millis(600));
    assert!(
        !app.store.read().unwrap().has_frozen_path(sid),
        "one job's frozen reading raises nothing"
    );
    let record = app.session_status_record(sid).expect("live");
    assert!(record.contains(" path=live program=claude "), "{record}");
    assert!(record.contains(" path_evidence=unconfirmed:"), "{record}");
    // A SECOND job agreeing settles it as the shell's: `path=frozen`,
    // measured — and still no mark (it is not "from before this update").
    {
        let pooled = app.pool.get(sid).unwrap();
        let mut tl = pooled.ctx.timeline.lock().unwrap();
        tl.note_foreground_group(4343);
        tl.set_program(4343, Some("claude".into()));
        tl.set_leader(4343, Some("foreign"), Some(PathVerdict::Frozen));
    }
    let _ = app.observe_session_statuses(t0 + Duration::from_millis(900));
    assert!(!app.store.read().unwrap().has_frozen_path(sid));
    let record = app.session_status_record(sid).expect("live");
    assert!(record.contains(" path=frozen program=claude "), "{record}");
    assert!(record.contains(" path_evidence=measured:"), "{record}");
}

/// Tier-1 for `ProgramResolutionRetry`: a failed process-table lookup on a
/// motionless Claude screen still owns a wake and another resolution.
/// This drives the shipping observer's request bookkeeping, timer and
/// guard; the model's old-code mutant is the negative control.
#[test]
fn an_unnamed_static_group_retries_promptly_then_backs_off_without_spinning() {
    let model = aterm_models_harness::program_resolution_retry_model();
    let mut modeled = model.init_state();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut old = buggy.init_state();
    let t0 = Instant::now();
    let floor = Duration::from_millis(100);
    let id = 7;
    let pgid = 42;
    let generation = crate::control::ScreenGen { epoch: 0, seq: 1 };
    let policy = StatusPolicy::default();
    let mut observer = StatusObserver::new(policy, floor);
    observer.sessions.insert(
        id,
        SessionSlot {
            next_due: t0 + floor,
            ..SessionSlot::new(policy, t0)
        },
    );

    // Record the first group request before the zone read to exercise
    // the request helper independently. Production creates the watch
    // earlier via `agent_zone_wanted`; either order retains its deadline.
    assert!(observer.program_due(id, true, None, generation, t0));
    observer.note_program_request(id, pgid, t0);
    assert!(model.fire("Start", &mut modeled));
    assert!(buggy.fire("Start", &mut old));
    assert_eq!(observer.agents[&id].resolved_at, Some(t0));
    assert_eq!(
        i64::from(observer.agents[&id].resolve_attempts),
        modeled["attempts"]
    );
    assert!(!buggy.check_invariant("UnnamedGroupOwnsRetry", &old));
    observer.agent_observe(
        id,
        generation,
        Some(vec!["Claude Code approval box".into()]),
        Cursor::Unknown,
        None,
        pgid,
        true,
        t0,
    );
    assert_eq!(observer.next_wake(), Some(t0 + floor));

    // The one follow-up read sees the same frame. Its flag clears, but
    // the unresolved name keeps an independently bounded deadline.
    observer.sessions.get_mut(&id).unwrap().next_due = t0 + floor * 2;
    observer.agent_observe(
        id,
        generation,
        None,
        Cursor::Unknown,
        None,
        pgid,
        false,
        t0 + floor,
    );
    assert_eq!(observer.next_wake(), Some(t0 + Duration::from_millis(250)));
    assert!(!observer.program_due(id, false, None, generation, t0 + Duration::from_millis(249)));
    let first_retry = t0 + Duration::from_millis(250);
    assert!(model.fire("Elapse", &mut modeled));
    assert!(model.fire("Decide", &mut modeled));
    assert!(buggy.fire("Elapse", &mut old));
    assert!(buggy.fire("Decide", &mut old));
    assert_eq!(modeled["retry_due"], 1);
    assert_eq!(old["retry_due"], 0, "old same-generation guard strands it");
    assert_eq!(
        observer.program_due(id, false, None, generation, first_retry),
        modeled["retry_due"] == 1
    );
    observer.note_program_request(id, pgid, first_retry);
    assert!(model.fire("Retry", &mut modeled));
    assert_eq!(
        i64::from(observer.agents[&id].resolve_attempts),
        modeled["attempts"]
    );

    // The second miss waits one second, then the third and every later
    // miss waits five seconds. Each resolution's single follow-up is
    // charged to the ordinary observation floor.
    observer.sessions.get_mut(&id).unwrap().next_due = first_retry + floor;
    observer.agent_observe(
        id,
        generation,
        None,
        Cursor::Unknown,
        None,
        pgid,
        true,
        first_retry,
    );
    assert_eq!(observer.next_wake(), Some(first_retry + floor));
    observer.sessions.get_mut(&id).unwrap().next_due = first_retry + floor * 2;
    observer.agent_observe(
        id,
        generation,
        None,
        Cursor::Unknown,
        None,
        pgid,
        false,
        first_retry + floor,
    );
    let second_retry = first_retry + Duration::from_secs(1);
    assert_eq!(observer.next_wake(), Some(second_retry));
    assert!(!observer.program_due(
        id,
        false,
        None,
        generation,
        second_retry - Duration::from_millis(1)
    ));
    assert!(model.fire("Elapse", &mut modeled));
    assert!(model.fire("Decide", &mut modeled));
    assert_eq!(
        observer.program_due(id, false, None, generation, second_retry),
        modeled["retry_due"] == 1
    );
    observer.note_program_request(id, pgid, second_retry);
    assert!(model.fire("Retry", &mut modeled));
    assert_eq!(observer.agents[&id].resolve_attempts, 3);
    observer.sessions.get_mut(&id).unwrap().next_due = second_retry + floor;
    observer.agent_observe(
        id,
        generation,
        None,
        Cursor::Unknown,
        None,
        pgid,
        true,
        second_retry,
    );
    observer.sessions.get_mut(&id).unwrap().next_due = second_retry + floor * 2;
    observer.agent_observe(
        id,
        generation,
        None,
        Cursor::Unknown,
        None,
        pgid,
        false,
        second_retry + floor,
    );
    assert_eq!(observer.next_wake(), Some(second_retry + PROGRAM_RECHECK));

    // A contended terminal can skip a deadline. The same past timer is
    // clamped to the charged next observation, never rearmed immediately.
    let late = second_retry + PROGRAM_RECHECK + Duration::from_secs(1);
    observer.note_skipped(id, late);
    assert_eq!(observer.next_wake(), Some(late + floor));

    // A later successful name on the same static frame identifies Claude
    // and disarms every unresolved retry.
    let named_at = late + floor;
    let named = observer.agent_observe(
        id,
        generation,
        Some(vec!["Claude Code approval box".into()]),
        Cursor::Unknown,
        Some("claude".into()),
        pgid,
        false,
        named_at,
    );
    assert_eq!(
        named.publish.and_then(|(_, _, _, reader)| reader),
        Some(aterm_phase::Program::Claude)
    );
    assert!(model.fire("Resolve", &mut modeled));
    assert_eq!(
        i64::from(observer.agents[&id].resolve_attempts),
        modeled["attempts"]
    );
    assert_eq!(observer.next_wake(), None);
    assert!(!observer.program_due(
        id,
        false,
        Some("claude"),
        generation,
        named_at + PROGRAM_RECHECK * 2
    ));

    // The same group can exec again. Once a name was known, its next
    // transient miss starts at the fast 250 ms retry instead of inheriting
    // the old five-second backoff. A departed or retired group owns none.
    let newer = crate::control::ScreenGen { epoch: 0, seq: 2 };
    let exec_at = named_at + PROGRAM_RECHECK * 2;
    assert!(observer.program_due(id, false, Some("claude"), newer, exec_at));
    observer.note_program_request(id, pgid, exec_at);
    observer.agent_observe(
        id,
        newer,
        Some(vec!["still named".into()]),
        Cursor::Unknown,
        Some("claude".into()),
        pgid,
        true,
        exec_at,
    );
    observer.agent_observe(
        id,
        newer,
        Some(vec!["unnamed".into()]),
        Cursor::Unknown,
        None,
        pgid,
        false,
        exec_at,
    );
    observer.sessions.get_mut(&id).unwrap().next_due = exec_at + floor;
    assert_eq!(
        observer.next_wake(),
        Some(exec_at + Duration::from_millis(250))
    );
    observer.agent_observe(
        id,
        newer,
        None,
        Cursor::Unknown,
        None,
        -1,
        false,
        exec_at + floor,
    );
    assert_eq!(observer.next_wake(), None);
    observer.retire(id);
    assert_eq!(observer.next_wake(), None);
}

/// Tier-1 for the named-shell side of `ProgramResolutionRetry`. A first
/// lookup can catch `sh` before its same-PGID exec into Claude. One short
/// confirmation may still see `sh`; a later single Claude frame must arm
/// another bounded lookup even though that frame then stays still.
#[test]
fn a_named_shell_rechecks_once_after_a_later_static_claude_frame() {
    let model = aterm_models_harness::program_resolution_retry_model();
    let mut modeled = model.init_state();
    let t0 = Instant::now();
    let floor = Duration::from_millis(100);
    let id = 8;
    let pgid = 43;
    let first = crate::control::ScreenGen { epoch: 0, seq: 1 };
    let later = crate::control::ScreenGen { epoch: 0, seq: 2 };
    let policy = StatusPolicy::default();
    let mut observer = StatusObserver::new(policy, floor);
    observer.sessions.insert(
        id,
        SessionSlot {
            next_due: t0 + floor,
            ..SessionSlot::new(policy, t0)
        },
    );

    observer.note_program_request(id, pgid, t0);
    assert!(model.fire("Start", &mut modeled));
    observer.agent_observe(
        id,
        first,
        Some(vec!["launching".into()]),
        Cursor::Unknown,
        Some("claude".into()),
        pgid,
        true,
        t0,
    );
    assert!(
        !observer.agents[&id].name_confirmed,
        "the group-change sweep's old Claude name is not this group's answer"
    );
    // The first later sweep sees the transient shell name, not the stale
    // name sampled before `note_foreground_group` on the launch sweep.
    let shell_at = t0 + floor;
    observer.agent_observe(
        id,
        first,
        Some(vec!["sh launching Claude".into()]),
        Cursor::Unknown,
        Some("sh".into()),
        pgid,
        false,
        shell_at,
    );
    assert!(model.fire("ResolveShell", &mut modeled));
    let first_confirm = shell_at + PROGRAM_NAME_CONFIRM;
    assert_eq!(modeled["confirm_armed"], 1);
    assert_eq!(observer.next_wake(), Some(first_confirm));
    assert!(!observer.program_due(
        id,
        false,
        Some("sh"),
        first,
        first_confirm - Duration::from_millis(1)
    ));
    assert!(model.fire("Elapse", &mut modeled));
    assert_eq!(
        observer.program_due(id, false, Some("sh"), first, first_confirm),
        model.action_enabled("ConfirmShell", &modeled)
    );
    observer.note_program_request(id, pgid, first_confirm);
    assert!(model.fire("ConfirmShell", &mut modeled));
    observer.agent_observe(
        id,
        first,
        None,
        Cursor::Unknown,
        Some("sh".into()),
        pgid,
        true,
        first_confirm,
    );
    observer.sessions.get_mut(&id).unwrap().next_due = first_confirm + floor;
    observer.agent_observe(
        id,
        first,
        None,
        Cursor::Unknown,
        Some("sh".into()),
        pgid,
        false,
        first_confirm + floor,
    );
    assert_eq!(modeled["confirm_armed"], 0);
    assert_eq!(observer.next_wake(), None, "stable sh stops polling");

    // Claude execs at t=3 s and draws once. At this point the 5 s
    // screen-moved guard is still closed; the deferred deadline is the
    // only way to name the now-static new program.
    let moved_at = t0 + Duration::from_secs(3);
    assert!(!observer.program_due(id, false, Some("sh"), later, moved_at));
    observer.agent_observe(
        id,
        later,
        Some(vec!["Claude Code approval box".into()]),
        Cursor::Unknown,
        Some("sh".into()),
        pgid,
        false,
        moved_at,
    );
    let before_move = modeled.clone();
    assert!(model.fire("MoveNamed", &mut modeled));
    let old_move =
        aterm_spec::interp::with_buggy(&model, 1).successors("MoveNamed", &before_move)[0].clone();
    assert_eq!(old_move["confirm_armed"], 0, "old guard loses this frame");
    observer.sessions.get_mut(&id).unwrap().next_due = moved_at + floor;
    assert_eq!(observer.next_wake(), Some(moved_at + floor));
    observer.sessions.get_mut(&id).unwrap().next_due = moved_at + floor * 2;
    observer.agent_observe(
        id,
        later,
        None,
        Cursor::Unknown,
        Some("sh".into()),
        pgid,
        false,
        moved_at + floor,
    );
    let later_confirm = first_confirm + PROGRAM_NAMED_RECHECK_FLOOR;
    assert_eq!(later_confirm, t0 + Duration::from_millis(4_600));
    assert_eq!(observer.next_wake(), Some(later_confirm));
    assert_eq!(modeled["confirm_armed"], 1);
    assert!(!observer.program_due(
        id,
        false,
        Some("sh"),
        later,
        later_confirm - Duration::from_millis(1)
    ));
    assert!(model.fire("Elapse", &mut modeled));
    assert_eq!(
        observer.program_due(id, false, Some("sh"), later, later_confirm),
        model.action_enabled("ConfirmShell", &modeled)
    );
    observer.note_program_request(id, pgid, later_confirm);
    assert!(model.fire("ConfirmShell", &mut modeled));
    observer.agent_observe(
        id,
        later,
        None,
        Cursor::Unknown,
        Some("sh".into()),
        pgid,
        true,
        later_confirm,
    );
    observer.sessions.get_mut(&id).unwrap().next_due = later_confirm + floor;
    assert_eq!(observer.next_wake(), Some(later_confirm + floor));
    let named = observer.agent_observe(
        id,
        later,
        Some(vec!["Claude Code approval box".into()]),
        Cursor::Unknown,
        Some("claude".into()),
        pgid,
        false,
        later_confirm + floor,
    );
    assert_eq!(
        named.publish.and_then(|(_, _, _, reader)| reader),
        Some(aterm_phase::Program::Claude)
    );
    assert!(model.fire("NameBecomesClaude", &mut modeled));
    assert_eq!(modeled["confirm_armed"], 0);
    assert_eq!(observer.next_wake(), None);

    // A later non-agent name can arm another deadline, but leaving the
    // foreground group cancels it even if the retired screen stays put.
    let third = crate::control::ScreenGen { epoch: 0, seq: 3 };
    observer.agent_observe(
        id,
        third,
        Some(vec!["sh again".into()]),
        Cursor::Unknown,
        Some("sh".into()),
        pgid,
        false,
        later_confirm + PROGRAM_RECHECK,
    );
    assert!(model.fire("AgentLeaves", &mut modeled));
    assert!(observer.agents[&id].deferred_name_recheck_at.is_some());
    assert_eq!(modeled["confirm_armed"], 1);
    observer.agent_observe(
        id,
        third,
        Some(vec!["prompt".into()]),
        Cursor::Unknown,
        None,
        -1,
        false,
        later_confirm + PROGRAM_RECHECK + floor,
    );
    assert!(model.fire("GroupLeaves", &mut modeled));
    assert_eq!(observer.agents[&id].deferred_name_recheck_at, None);
    assert_eq!(modeled["confirm_armed"], 0);
    assert_eq!(observer.next_wake(), None);
}
/// INT-4: a shell whose last line ends in `?` is not an agent — `agent=-`
/// and no attention — while the SAME screen under `program=claude` is read
/// as the question it is. Identity, not screen text, decides.
#[test]
fn a_shell_ending_in_a_question_mark_is_not_an_agent() {
    let (mut app, sid) = app_with_stub();
    let screen: Vec<String> = vec![
        "$ echo \"Checking whether the disk is full?\"; sleep 30".into(),
        "Checking whether the disk is full?".into(),
    ];
    paint(&app, sid, &screen);
    let t0 = Instant::now();
    let changed = app.observe_session_statuses(t0);
    for id in changed {
        app.refresh_presence_session(id);
    }
    assert_eq!(published(&app, sid).word, "-");
    let record = app.session_status_record(sid).expect("live");
    assert!(
        record.contains(" agent=- agent_detail=- agent_rev=0 "),
        "{record}"
    );
    assert!(!record.contains("level=attention"), "{record}");
    assert!(record.contains(" why=- "), "{record}");

    // The same screen, the foreground program named `claude`: an agent,
    // asking. (The stub has no PTY, so the program is published by hand
    // exactly as the resolver would.)
    {
        let pooled = app.pool.get(sid).unwrap();
        let mut tl = pooled.ctx.timeline.lock().unwrap();
        assert!(tl.note_foreground_group(4242));
        tl.set_program(4242, Some("claude".into()));
    }
    let changed = app.observe_session_statuses(t0 + Duration::from_millis(300));
    for id in changed {
        app.refresh_presence_session(id);
    }
    let p = published(&app, sid);
    assert_eq!(p.program.as_deref(), Some("claude"));
    assert_eq!(
        p.word, "question",
        "a program change re-reads an unchanged zone"
    );
    let record = app.session_status_record(sid).expect("live");
    assert!(
        record.contains(" program=claude agent=question "),
        "{record}"
    );
    assert!(record.contains(" why=question "), "{record}");
}

/// ACT-4: an adopted, checkpointed session whose handoff CARRIED its
/// shell-integration nonce still takes its shell's marks — fed `633;E;claude
/// --resume` then `133;C`, it reads `detail=claude` — and one whose
/// handoff did not stays `integration=degraded` with its marks dropped
/// (the requirement is never cleared as a fallback).
#[test]
fn an_adopted_session_with_its_carried_nonce_reads_its_command() {
    use aterm_core::terminal::{ShellIntegrationPosture, Terminal};
    let nonce = [0x3Cu8; 32];
    let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let mut outgoing = Terminal::new(24, 80);
    outgoing.authorize_shell_integration(nonce);
    outgoing.set_require_shell_integration_nonce(true);
    outgoing.process(format!("\x1b]133;A;id={hex}\x07$ ").as_bytes());
    let carried = outgoing.checkpoint_carry(0).expect("Ground");

    let feed = format!(
        "\x1b]133;D;0;id={hex}\x07\x1b]133;A;id={hex}\x07$ \x1b]633;E;claude --resume;id={hex}\x07\
         \x1b]133;B;id={hex}\x07claude --resume\n\x1b]133;C;id={hex}\x07"
    );
    let mut adopted = Terminal::new(24, 80);
    adopted.restore_checkpoint(&carried);
    crate::spawn::authorize_adopted_shell_nonce(&mut adopted, &carried, 1);
    assert_eq!(
        adopted.shell_integration_posture(),
        ShellIntegrationPosture::On
    );
    adopted.process(feed.as_bytes());
    assert_eq!(super::executing_detail(&adopted).as_deref(), Some("claude"));

    // NEGATIVE CONTROL: the same adopt without the carried nonce (a parent
    // that predates the carry) drops every mark and says so.
    let mut uncarried = carried.clone();
    uncarried.shell_integration_nonce = None;
    let mut blind = Terminal::new(24, 80);
    blind.restore_checkpoint(&uncarried);
    crate::spawn::authorize_adopted_shell_nonce(&mut blind, &uncarried, 2);
    assert_eq!(
        blind.shell_integration_posture(),
        ShellIntegrationPosture::Degraded
    );
    assert!(blind.is_require_shell_integration_nonce(), "never cleared");
    blind.process(feed.as_bytes());
    assert_eq!(super::executing_detail(&blind), None);
}

/// The herald's posts on this thread since the last call, drained.
fn take_posts() -> Vec<crate::status_item::HeraldNotice> {
    crate::app_tabs::HERALD_POSTS.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

/// One sweep plus the presence fan-out the event loop runs after it.
fn sweep(app: &mut App, t: Instant) {
    for id in app.observe_session_statuses(t) {
        app.refresh_presence_session(id);
    }
}

fn looking_at(app: &App, sessions: &[u64]) {
    let mut set = app.notify_suppress.lock().unwrap();
    set.clear();
    set.extend(sessions.iter().copied());
}

/// THE E2E PROBE OF 2026-09-25 (W1): a trust dialog drawn at the top of
/// a 45-row pane had its title above the live zone's 40 rows, so the
/// server published `agent_detail=other` and the menu bar said "other
/// approval" — and it said so from the raw verdict before the window's
/// supervisor had taken the session's claim, then again from the
/// supervisor's own escalation: two notifications for one box. The zone
/// that cuts a box is read again whole (the verdict names `trust`), and a
/// session the host supervises raises no verdict row, claim or not — its
/// box is the host's to answer or escalate. NEGATIVE CONTROL: with no host
/// supervising, the verdict's row is there.
#[test]
fn a_box_the_zone_cuts_is_read_whole_and_a_hosted_agent_raises_no_row() {
    let (mut app, sid) = app_with_stub();
    app.headless = false;
    // The desktop notice is what these count (`desktop_alerts` is opt-in).
    app.config.desktop_alerts = Some(true);
    let fixture = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::TRUST);
    let dialog = &fixture[8..=23];
    assert!(dialog[1].contains("Accessing workspace"), "{dialog:?}");
    {
        let pooled = app.pool.get(sid).expect("pooled");
        let mut t = crate::term_lock(&pooled.term);
        t.resize(45, 120);
        let mut bytes = String::from("\x1b[H\x1b[2J");
        for (i, row) in dialog.iter().enumerate() {
            bytes.push_str(&format!("\x1b[{};1H{row}", i + 1));
        }
        t.process(bytes.as_bytes());
    }
    let _ = take_posts();
    looking_at(&app, &[]);
    sweep(&mut app, Instant::now());
    let agent = published(&app, sid);
    assert_eq!(agent.word, "prompt", "{agent:?}");
    assert!(
        agent
            .detail
            .as_deref()
            .is_some_and(|d| d.starts_with("trust")),
        "{agent:?}"
    );
    let (hosted, bare) = {
        // Copied out of the store's guard, as `App::publish_need` does.
        let handle = app.store.read().unwrap().by_local(sid).cloned();
        let handle = handle.as_ref().expect("the session");
        let now_us = crate::metrics::now_us();
        (
            app.need_facts_with(handle, |_| true, &Default::default(), now_us, 0),
            app.need_facts_with(handle, |_| false, &Default::default(), now_us, 0),
        )
    };
    assert!(hosted.supervised, "{hosted:?}");
    assert_eq!(crate::needs::of(&hosted).need, None);
    // NEGATIVE CONTROL: no host supervising — the verdict's need.
    assert!(!bare.supervised);
    assert!(crate::needs::of(&bare).need.is_some());
}

/// ANOTHER HOLDER'S LIVE CLAIM IS NAMED APART FROM THIS HOST'S
/// (`NeedFacts::other_supervisor`, the walls another holder's `meta set
/// supervisor` claim retries, a manager session's or codex-alab's own):
/// its claim makes the session supervised and names the loop. Either claim
/// is a live claim (`NeedFacts::claim_live`, what the supervisor loop's
/// `kind=wait` waits a wall out under). NEGATIVE CONTROLS: no claim names
/// none and is no live claim, and this host's own claim names no other
/// supervisor, its answer to a wall being `wall_answer`'s, never presumed.
#[test]
fn another_holders_claim_is_named_apart_from_this_hosts() {
    let (app, sid) = app_with_stub();
    let ctx = app.pool.get(sid).expect("pooled").ctx.clone();
    let read = |app: &App| {
        let handle = app.store.read().unwrap().by_local(sid).cloned();
        let handle = handle.as_ref().expect("the session");
        let f = app.need_facts_with(
            handle,
            |_| false,
            &Default::default(),
            crate::metrics::now_us(),
            0,
        );
        (f.supervised, f.other_supervisor, f.claim_live)
    };
    assert_eq!(read(&app), (false, false, false), "no claim");
    let now_us = crate::metrics::now_us();
    let host = crate::harness_host::holder();
    assert_eq!(
        crate::session_timeline::claim_supervisor(&ctx, &host, None, None, now_us),
        Ok(true)
    );
    assert_eq!(read(&app), (false, false, true), "this host's own claim");
    assert!(crate::session_timeline::release_supervisor_held_by(
        &ctx,
        &host,
        now_us,
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(
        crate::session_timeline::claim_supervisor(&ctx, "drive-watch", None, None, now_us),
        Ok(true)
    );
    assert_eq!(read(&app), (true, true, true), "another holder's claim");
}

/// ONE PLACE FOR ANDREW, end to end on the measured rm box: the sweep
/// publishes `agent=prompt`, and the presence fan-out raises exactly one
/// menu row naming the tab, the kind and the command, and exactly one
/// notification. Re-classifying the same box (its ticking row moves the
/// zone hash) posts nothing more; a new box while the human is looking
/// at the tab posts nothing but still lists the row; a headless instance
/// posts nothing at all.
#[test]
fn a_box_raises_one_menu_row_and_one_notification_per_transition() {
    let (mut app, sid) = app_with_stub();
    app.headless = false;
    // The desktop notice is what these count (`desktop_alerts` is opt-in).
    app.config.desktop_alerts = Some(true);
    {
        use crate::session_timeline::{MetaEdit, MetaField, write_session_meta};
        let ctx = app.pool.get(sid).expect("pooled").ctx.clone();
        write_session_meta(
            &ctx,
            MetaField::Title,
            MetaEdit::Set("build"),
            aterm_slog::Actor::Ctl,
        )
        .expect("title");
    }
    let _ = take_posts();
    looking_at(&app, &[]);
    let step = Duration::from_millis(300);
    let mut t = Instant::now();
    for secs in 0..2 {
        paint(&app, sid, &busy_screen(secs));
        sweep(&mut app, t);
        t += step;
    }
    assert!(take_posts().is_empty(), "a busy agent needs nobody");
    assert!(app.status_glance().needs.is_empty());

    paint(&app, sid, &box_screen(1));
    sweep(&mut app, t);
    t += step;
    let posts = take_posts();
    assert_eq!(posts.len(), 1, "exactly one notification: {posts:?}");
    assert_eq!(posts[0].session, sid);
    assert!(
        posts[0]
            .body
            .contains("bash S=$PWD/tmp; for p in a b; do set -- $p; rm -rf"),
        "the body names the command: {}",
        posts[0].body
    );
    let glance = app.status_glance();
    assert_eq!(glance.need_count(), 1, "{:?}", glance.needs);
    assert_eq!(
        glance.local_need_sessions(),
        vec![sid],
        "a click on the row focuses this tab"
    );
    assert_eq!(glance.needs[0].label, "build", "the row names its tab");
    assert!(
        posts[0].body.starts_with("build: bash "),
        "{}",
        posts[0].body
    );
    let rev = published(&app, sid).rev;

    // The same box re-read: the ticking row moves the zone hash, so the
    // sweep classifies it again — the same verdict, the same rev.
    paint(&app, sid, &box_screen(2));
    sweep(&mut app, t);
    t += step;
    assert_eq!(
        published(&app, sid).rev,
        rev,
        "the same box is the same rev"
    );
    app.refresh_presence_session(sid);
    assert!(take_posts().is_empty(), "no second notification");
    assert_eq!(
        app.presence.herald.last_quiet(),
        Some(crate::status_item::HeraldQuiet::Same)
    );
    assert_eq!(app.status_glance().need_count(), 1);

    // The box is answered, and a NEW one comes up while the human is
    // looking at this tab in the active app: listed, not announced.
    paint(&app, sid, &busy_screen(9));
    sweep(&mut app, t);
    t += step;
    assert!(app.status_glance().needs.is_empty(), "row gone");
    looking_at(&app, &[sid]);
    paint(&app, sid, &box_screen(3));
    sweep(&mut app, t);
    assert!(published(&app, sid).rev > rev, "a new transition");
    assert!(take_posts().is_empty(), "the human is looking at it");
    assert_eq!(
        app.presence.herald.last_quiet(),
        Some(crate::status_item::HeraldQuiet::Looking)
    );
    assert_eq!(app.status_glance().need_count(), 1);

    // NEGATIVE CONTROL: the same first transition in a headless instance
    // reaches the published verdict and posts nothing.
    let (mut headless, hsid) = app_with_stub();
    assert!(headless.headless);
    looking_at(&headless, &[]);
    let mut t = Instant::now();
    paint(&headless, hsid, &busy_screen(0));
    sweep(&mut headless, t);
    t += step;
    paint(&headless, hsid, &box_screen(1));
    sweep(&mut headless, t);
    assert_eq!(published(&headless, hsid).word, "prompt");
    assert!(take_posts().is_empty(), "headless never notifies");
}

/// A SUPERVISOR THAT DIES (review major): its `ttl=` claim lapses with a
/// box up and nothing writes — yet the human must hear. The presence
/// timer wakes at the expiry, removes the claim with a `meta-change
/// field=supervisor value=-` record (the `EVENT meta` push), and the box
/// posts exactly one notice. NEGATIVE CONTROL: while the claim is live the
/// same box posts nothing, and a tick before the expiry changes nothing.
#[test]
fn a_lapsed_supervisor_claim_hands_its_box_to_the_human() {
    let (mut app, sid) = app_with_stub();
    app.headless = false;
    // The desktop notice is what these count (`desktop_alerts` is opt-in).
    app.config.desktop_alerts = Some(true);
    let ctx = app.pool.get(sid).expect("pooled").ctx.clone();
    let _ = take_posts();
    looking_at(&app, &[]);
    // Long enough that the negative controls below — a presence refresh,
    // two debug-build paints and sweeps — finish inside it on a loaded
    // machine: at 120 ms they could outlast the claim and read a correct
    // lapse as a notice posted while supervised. The lapse is then waited
    // for, not slept for.
    let ttl = Duration::from_secs(2);
    let now_us = crate::metrics::now_us();
    assert_eq!(
        crate::session_timeline::claim_supervisor(
            &ctx,
            "sup",
            None,
            Some(now_us + ttl.as_micros() as u64),
            now_us
        ),
        Ok(true)
    );
    // The `meta set` arm's `Wake::MetaChanged` refresh.
    app.refresh_presence_session(sid);
    let step = Duration::from_millis(300);
    let mut t = Instant::now();
    paint(&app, sid, &busy_screen(0));
    sweep(&mut app, t);
    t += step;
    paint(&app, sid, &box_screen(1));
    sweep(&mut app, t);
    assert_eq!(published(&app, sid).word, "prompt");
    assert!(take_posts().is_empty(), "NEGATIVE CONTROL: supervised");
    let deadline = app.presence_deadline(Instant::now()).expect("armed");
    assert!(deadline <= Instant::now() + ttl + Duration::from_millis(5));
    let _ = app.presence_tick(Instant::now());
    assert!(take_posts().is_empty(), "NEGATIVE CONTROL: not lapsed yet");

    let lapsed_by = Instant::now() + ttl + aterm_spec::wait::MUST_HAPPEN;
    let posts = loop {
        std::thread::sleep(Duration::from_millis(20));
        let _ = app.presence_tick(Instant::now());
        let posts = take_posts();
        if !posts.is_empty() {
            break posts;
        }
        assert!(
            Instant::now() < lapsed_by,
            "the lapsed claim never handed its box to the human"
        );
    };
    assert_eq!(posts.len(), 1, "exactly one notice: {posts:?}");
    assert_eq!(posts[0].session, sid);
    let meta_events: Vec<String> = ctx
        .timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|e| e.kind == "meta-change")
        .map(|e| e.payload.clone())
        .collect();
    assert_eq!(
        meta_events.last().map(String::as_str),
        Some("field=supervisor value=-"),
        "{meta_events:?}"
    );
    assert!(ctx.meta.lock().unwrap().supervisor_expiry().is_none());
    let _ = app.presence_tick(Instant::now());
    assert!(take_posts().is_empty(), "announced once");
}

/// A RE-GRID IS NO MOVE OF THE PROGRAM'S (`AgentPublication::text_moves` and
/// `verdict_moves`, the held deadline's screen evidence; the reviews of its
/// third cut): a worker's held stall raises the supervisor's attention, the
/// presence row it bears is born and re-grids the tab one row shorter, and
/// Claude Code repaints for the new size — the screen's text changes at the
/// sweep's read of the resized grid and again at its read of the repaint,
/// and the program did nothing, so neither change is counted, nor the row's
/// fold back after it. Counted, the stall's own row would end the stall
/// (`working why=moved`), fold, and stall it again two minutes later, a fresh
/// `stalled` to the manager's `await task` each time. A WIDTH change (the
/// window dragged narrower, a pane split, the font zoomed) reflows the box,
/// and the read of the reflowed grid parses it otherwise until the repaint:
/// the verdict moves and moves back, `agent_rev` with it, and that is no move
/// either. NEGATIVE CONTROLS: the stamp moved at every read (a count of every
/// text change would have counted each) and the rev at the width's (a count
/// of the rev would have counted both); the program's own change at the same
/// grid past the settle is a text move, and a verdict it moves to — within a
/// re-grid's settle and kept, or past it — is a verdict move.
#[test]
fn a_regrid_and_the_programs_repaint_are_no_text_move() {
    let (mut app, sid) = app_with_stub();
    let mut t = Instant::now();
    let step = Duration::from_millis(300);
    let sweep = |app: &mut App, t: &mut Instant| {
        let _ = app.observe_session_statuses(*t);
        *t += step;
    };
    // The program's repaint for the grid it now has: the screen cleared and
    // the box drawn again at its foot (the transcript rows above it are not),
    // its rule as wide as the grid and a bordered row too long for it
    // word-wrapped inside its border.
    let repaint = |app: &App| {
        let pooled = app.pool.get(sid).expect("pooled");
        let mut term = crate::term_lock(&pooled.term);
        let cols = usize::from(term.cols());
        let border = " \u{2502} ";
        let mut frame = Vec::new();
        for row in &box_screen(1)[10..] {
            if row.starts_with('\u{2500}') {
                frame.push("\u{2500}".repeat(cols));
                continue;
            }
            let Some(text) = row
                .strip_prefix(border)
                .filter(|_| row.chars().count() > cols)
            else {
                frame.push(row.clone());
                continue;
            };
            let mut line = String::new();
            for word in text.split(' ') {
                if !line.is_empty() && 3 + line.chars().count() + 1 + word.chars().count() > cols {
                    frame.push(format!("{border}{line}"));
                    line.clear();
                }
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(word);
            }
            frame.push(format!("{border}{line}"));
        }
        let top = usize::from(term.rows()) - frame.len();
        let mut bytes = String::from("\x1b[H\x1b[2J");
        for (i, row) in frame.iter().enumerate() {
            bytes.push_str(&format!("\x1b[{};1H{row}", top + i + 1));
        }
        term.process(bytes.as_bytes());
    };
    let regrid = |app: &App, rows: u16, cols: u16| {
        let pooled = app.pool.get(sid).expect("pooled");
        crate::term_lock(&pooled.term).resize(rows, cols);
    };
    let fp = |app: &App| published(app, sid).stamp.expect("read").fp;
    paint(&app, sid, &busy_screen(0));
    sweep(&mut app, &mut t);
    paint(&app, sid, &box_screen(1));
    sweep(&mut app, &mut t);
    let held = published(&app, sid);
    assert_eq!(held.word, "prompt");

    let settle = crate::session_timeline::REGRID_SETTLE;
    // The row is born (23 rows), then folds past the settle (24); then the
    // window is dragged narrower and back: each time the tab re-gridded,
    // read, then the program's repaint, read. Widening wraps nothing back,
    // so there the repaint alone moves the text.
    for (rows, cols, reflows) in [
        (23, 120, true),
        (24, 120, true),
        (24, 100, true),
        (24, 120, false),
    ] {
        let before = fp(&app);
        let rev = published(&app, sid).rev;
        regrid(&app, rows, cols);
        sweep(&mut app, &mut t);
        let resized = fp(&app);
        assert_eq!(
            resized != before,
            reflows,
            "{rows}x{cols}: the resize moved the text"
        );
        repaint(&app);
        sweep(&mut app, &mut t);
        assert_ne!(
            fp(&app),
            resized,
            "{rows}x{cols}: the repaint moved it again"
        );
        let now = published(&app, sid);
        assert_eq!(
            (now.text_moves, now.verdict_moves, now.word),
            (held.text_moves, held.verdict_moves, "prompt"),
            "{rows}x{cols}: a re-grid and its repaint are no move"
        );
        assert_eq!(now.detail, held.detail, "{rows}x{cols}: repainted");
        if cols == 100 {
            assert_eq!(
                now.rev,
                rev + 2,
                "the reflowed read moved the verdict and back"
            );
        }
        t += settle;
    }

    // NEGATIVE CONTROLS: the program's own change at the same grid, once the
    // settle is past, is a move of its text; the verdict it keeps is none.
    let before = fp(&app);
    paint(&app, sid, &box_screen(2));
    sweep(&mut app, &mut t);
    assert_ne!(fp(&app), before);
    let moved = published(&app, sid);
    assert_eq!(moved.text_moves, held.text_moves + 1, "the program moved");
    assert_eq!(moved.verdict_moves, held.verdict_moves, "its verdict stood");
    // A verdict the program moves to within a re-grid's settle, and keeps,
    // counts at the first read of the screen past the settle (`paint` draws
    // at 120 columns again: the same burst; the paint after the settle moves
    // the text, which is what has the sweep read it).
    regrid(&app, 24, 100);
    sweep(&mut app, &mut t);
    paint(&app, sid, &busy_screen(1));
    sweep(&mut app, &mut t);
    let within = published(&app, sid);
    assert_eq!(
        (within.word, within.verdict_moves),
        ("busy", held.verdict_moves),
        "within the settle"
    );
    t += settle;
    paint(&app, sid, &busy_screen(2));
    sweep(&mut app, &mut t);
    assert_eq!(
        published(&app, sid).verdict_moves,
        held.verdict_moves + 1,
        "rested: the verdict is not the one before the re-grid"
    );
    // And one it moves to past the settle counts at once.
    paint(&app, sid, &box_screen(3));
    sweep(&mut app, &mut t);
    let back = published(&app, sid);
    assert_eq!(
        (back.word, back.verdict_moves),
        ("prompt", held.verdict_moves + 2)
    );
}
