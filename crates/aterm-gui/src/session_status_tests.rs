// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::{Arc, Mutex};

use crate::session_timeline::SessionTimeline;

/// `seq=<n|-> hash=<hex16|->`, last on the record and in that order — the
/// pair a stamped report opens with, and the one
/// `history` can be held against.
fn assert_stamp_rides_last(record: &str) {
    let stamp = record
        .rsplit_once(" seq=")
        .expect("the record ends with the stamp")
        .1;
    let (seq, hash) = stamp.split_once(" hash=").expect("seq= then hash=");
    assert!(
        seq == "-" || seq.chars().all(|c| c.is_ascii_digit()),
        "seq={seq:?} in {record}"
    );
    assert!(
        hash == "-" || (hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit())),
        "hash={hash:?} in {record}"
    );
}
use super::*;

const QUIET: Duration = Duration::from_millis(5_000);
const DWELL: Duration = Duration::from_millis(750);

fn policy() -> StatusPolicy {
    StatusPolicy {
        quiet_after: QUIET,
        dwell: DWELL,
    }
}

#[test]
fn stale_claude_sample_cannot_restart_a_stopped_footer_watch() {
    let now = Instant::now();
    let mut observer = StatusObserver::new(policy(), Duration::from_millis(50));
    let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
    {
        let mut current = timeline.lock().unwrap();
        current.note_foreground_group(42);
        current.set_program(42, Some("claude".into()));
        current.set_program(42, Some("zsh".into()));
    }
    // The UI sampled Claude before the worker published zsh and sent its
    // stop. A later request from that stale sample would revive the watch.
    observer.request_footer_if_due(7, &timeline, Some("claude"), 42, now);
    assert!(!observer.footer_asked.contains_key(&7));

    {
        let mut current = timeline.lock().unwrap();
        current.note_foreground_group(43);
        current.set_program(43, Some("claude".into()));
    }
    observer.request_footer_if_due(7, &timeline, Some("claude"), 42, now);
    assert!(!observer.footer_asked.contains_key(&7));
}

fn blank(seq: u64) -> ActivitySample {
    ActivitySample {
        alt_screen: false,
        content_seq: seq,
        last_input: None,
        last_output: None,
    }
}

/// The block that completed with `exit_code`, as [`completed_block`] reads it.
fn done(id: u64, exit_code: i32) -> Option<CompletedBlock> {
    Some(CompletedBlock {
        id,
        exit_code: Some(exit_code),
    })
}

fn evidence(activity: ActivitySample) -> Evidence {
    Evidence {
        shell: None,
        completed_block: None,
        lifecycle: None,
        foreground_job: None,
        activity,
    }
}

/// Drive a candidate past the dwell gate: observe, wait, observe again.
fn settle(fsm: &mut StatusFsm, ev: &Evidence, at: Instant) -> Instant {
    fsm.observe(ev, at);
    let later = at + DWELL;
    fsm.observe(ev, later);
    later
}

#[test]
fn shell_prompt_outranks_background_output() {
    // The `tail -f &` case: content is moving, but the shell is at a prompt.
    // Screen movement must NOT promote the session to Running.
    let t0 = Instant::now();
    let mut fsm = StatusFsm::new(policy(), t0);
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Prompt);

    fsm.observe(&ev, t0);
    ev.activity.content_seq = 2;
    let t1 = settle(&mut fsm, &ev, t0 + Duration::from_millis(10));

    assert_eq!(fsm.status().phase, Phase::Idle);
    assert_eq!(fsm.status().confidence, Confidence::Strong);
    assert!(fsm.status().since <= t1);
}

/// The complete phase vocabulary, pinned against its wire spellings. This is
/// the `status` verb's contract: a phase the classifier cannot produce has
/// no business being in the enum, and a consumer matching these tokens must
/// be able to trust the list is exhaustive.
#[test]
fn every_phase_is_reachable_and_has_a_wire_spelling() {
    let spellings = [
        (Phase::Unknown, "unknown"),
        (Phase::Starting, "starting"),
        (Phase::Idle, "idle"),
        (Phase::Running, "running"),
        (Phase::Quiet, "quiet"),
        (Phase::Exited, "exited"),
    ];
    for (phase, wire) in spellings {
        assert_eq!(phase.as_str(), wire);
    }

    // Each one, produced by the classifier from real evidence — no phase in
    // the vocabulary is scaffolding.
    let t0 = Instant::now();
    let mut fsm = StatusFsm::new(policy(), t0);
    assert_eq!(fsm.status().phase, Phase::Starting, "seeded");

    // No evidence reports unknown rather than guessing.
    let mut ev = evidence(blank(1));
    settle(&mut fsm, &ev, t0);
    assert_eq!(fsm.status().phase, Phase::Unknown, "no evidence");
    assert_eq!(fsm.status().confidence, Confidence::Unknown);
    assert_eq!(fsm.status().reasons, vec![Reason::NoEvidence]);

    ev.foreground_job = Some(false);
    let t1 = settle(&mut fsm, &ev, t0 + Duration::from_millis(10));
    assert_eq!(fsm.status().phase, Phase::Idle);

    ev.foreground_job = Some(true);
    ev.activity.content_seq = 2;
    let t2 = settle(&mut fsm, &ev, t1 + Duration::from_millis(10));
    assert_eq!(fsm.status().phase, Phase::Running);

    // Silence is Quiet, and only heuristically: a pager, a REPL, a password
    // prompt and a `sleep` are indistinguishable from here (the indicator
    // table pins that Quiet never asks for the user).
    ev.activity.last_output = None;
    let t3 = settle(&mut fsm, &ev, t2 + QUIET * 2);
    assert_eq!(fsm.status().phase, Phase::Quiet);
    assert_eq!(fsm.status().confidence, Confidence::Heuristic);

    ev.lifecycle = Some(Lifecycle::Exited { exit_code: Some(3) });
    fsm.observe(&ev, t3 + Duration::from_millis(10));
    assert_eq!(fsm.status().phase, Phase::Exited);
    assert_eq!(fsm.status().last_outcome, Outcome::Failure { exit_code: 3 });
}

#[test]
fn alt_screen_transition_is_a_resync_not_activity() {
    // Entering the alternate screen restarts content_gen. A naive
    // comparison would read that jump as a burst of work.
    let t0 = Instant::now();
    let mut fsm = StatusFsm::new(policy(), t0);
    let mut ev = evidence(ActivitySample {
        alt_screen: false,
        content_seq: 900,
        last_input: None,
        last_output: None,
    });
    ev.foreground_job = Some(true);
    fsm.observe(&ev, t0);

    ev.activity = ActivitySample {
        alt_screen: true,
        content_seq: 3,
        last_input: None,
        last_output: None,
    };
    settle(&mut fsm, &ev, t0 + Duration::from_millis(10));

    assert_eq!(
        fsm.status().phase,
        Phase::Quiet,
        "an alt-screen sequence restart must not count as movement"
    );
}

#[test]
fn a_new_unit_of_work_clears_the_previous_outcome() {
    let t0 = Instant::now();
    let mut fsm = StatusFsm::new(policy(), t0);
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(2) });
    let t1 = settle(&mut fsm, &ev, t0);
    // Completion keeps the outcome while the phase goes idle.
    assert_eq!(
        fsm.status().phase,
        Phase::Idle,
        "finished work is not a phase"
    );
    assert_eq!(fsm.status().last_outcome, Outcome::Failure { exit_code: 2 });
    assert!(fsm.status().last_outcome.is_failure());

    ev.shell = Some(ShellEvidence::Executing);
    ev.activity.content_seq = 2;
    settle(&mut fsm, &ev, t1 + Duration::from_millis(10));

    assert_eq!(fsm.status().phase, Phase::Running);
    assert_eq!(fsm.status().last_outcome, Outcome::None);
}

#[test]
fn dwell_suppresses_flapping_but_exit_publishes_immediately() {
    let t0 = Instant::now();
    let mut fsm = StatusFsm::new(policy(), t0);
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(false);
    settle(&mut fsm, &ev, t0);
    assert_eq!(fsm.status().phase, Phase::Idle);

    // A single contrary observation inside the dwell window changes nothing.
    ev.foreground_job = Some(true);
    ev.activity.content_seq = 2;
    assert!(!fsm.observe(&ev, t0 + Duration::from_millis(50)));
    assert_eq!(fsm.status().phase, Phase::Idle);

    // An exact-confidence transition bypasses dwell entirely.
    ev.lifecycle = Some(Lifecycle::Exited { exit_code: Some(0) });
    assert!(fsm.observe(&ev, t0 + Duration::from_millis(60)));
    assert_eq!(fsm.status().phase, Phase::Exited);
    assert_eq!(fsm.status().last_outcome, Outcome::Success);
    assert_eq!(fsm.status().confidence, Confidence::Exact);
}

/// THE `Executing` ARM NAMES THE CLOCK THAT IS ALIVE
/// (`docs/EFFECTS-AND-WAKE-FOLLOWUPS-2026-08-24.md` item 21). On a
/// shell-integrated session, output that mutates nothing must read
/// `output_activity` — the same `movement_reason` the foreground-job arm
/// uses — or `ctl status` cannot say whether the movement clock has gone
/// stale (the in-band witness the spin gate's fixture relies on). RED
/// before: the arm hardcoded `ContentActivity` whenever anything moved.
#[test]
fn executing_names_output_only_movement_as_output_activity() {
    let t0 = Instant::now();
    let mut fsm = StatusFsm::new(policy(), t0);
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Executing);
    ev.activity.last_output = Some(t0);
    let t1 = settle(&mut fsm, &ev, t0);
    assert_eq!(fsm.status().phase, Phase::Running);
    assert_eq!(
        fsm.status().reasons,
        vec![Reason::ShellBlock, Reason::OutputActivity],
        "bytes arrived and the grid never moved"
    );

    // Control: the grid moves, and the record says so.
    ev.activity.content_seq = 2;
    ev.activity.last_output = Some(t1);
    settle(&mut fsm, &ev, t1 + Duration::from_millis(10));
    assert_eq!(fsm.status().phase, Phase::Running);
    assert_eq!(
        fsm.status().reasons,
        vec![Reason::ShellBlock, Reason::ContentActivity],
        "a moved grid is content activity"
    );
}

#[test]
fn executing_without_a_foreground_job_is_flagged_as_conflicting() {
    let t0 = Instant::now();
    let mut fsm = StatusFsm::new(policy(), t0);
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Executing);
    ev.foreground_job = Some(false);

    settle(&mut fsm, &ev, t0);

    assert!(
        fsm.status().conflict,
        "contradictory evidence must be visible"
    );
}

fn published(phase: Phase, last_outcome: Outcome) -> Status {
    Status {
        phase,
        last_outcome,
        since: Instant::now(),
        detail: None,
        confidence: Confidence::Strong,
        reasons: Vec::new(),
        conflict: false,
    }
}

#[test]
fn indicators_map_work_and_failure_without_claiming_unsaved_state() {
    use crate::tab_model::TabIndicators;

    let cases = [
        (Phase::Running, Outcome::None, (true, false)),
        // An exited session with NO collectable status raises nothing: an
        // unwitnessed adopted shell, or a master that went unreadable, is unknown — not
        // a failure worth marking a tab for.
        (Phase::Exited, Outcome::None, (false, false)),
        // A finished failure keeps the tab marked while the shell honestly
        // sits back at a prompt.
        (
            Phase::Idle,
            Outcome::Failure { exit_code: 1 },
            (false, true),
        ),
        (Phase::Exited, Outcome::Signal { signal: 9 }, (false, true)),
        // Running AND failed: the previous unit's result is still the thing
        // worth showing, and neither bit hides the other.
        (
            Phase::Running,
            Outcome::Failure { exit_code: 2 },
            (true, true),
        ),
        (Phase::Idle, Outcome::Success, (false, false)),
        // Quiet is work in flight that has simply printed nothing recently
        // (it is only reachable with a known foreground job), so the dot
        // must STAY lit — darking it mid-link reads as "finished".
        (Phase::Quiet, Outcome::None, (true, false)),
        (Phase::Starting, Outcome::None, (false, false)),
        (Phase::Unknown, Outcome::None, (false, false)),
    ];
    for (phase, outcome, (busy, attention)) in cases {
        assert_eq!(
            status_indicators(&published(phase, outcome)),
            TabIndicators {
                dirty: false,
                busy,
                // A terminal NEVER writes the out-of-band bit: that
                // separation is what lets the classifier's own bit be
                // recomputed rather than latched.
                attention: false,
                status_attention: attention,
            },
            "{phase:?} with {outcome:?}"
        );
    }
}

/// The latch defect: the classifier is edge-driven by PTY output, but the
/// transition OUT of Running is time-gated. A build that finishes and prints
/// nothing more would keep its busy dot lit forever unless the observer can
/// tell the event loop it still owes a wake.
#[test]
fn a_finished_pane_owes_a_wake_so_its_busy_dot_can_retire() {
    let t0 = Instant::now();
    let mut observer = StatusObserver::new(policy(), Duration::from_millis(0));
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    // The first sample only ADOPTS the activity identity (it cannot know
    // whether anything moved), so movement needs a second observation and
    // publication needs a third once dwell is served.
    observer.observe(1, &ev, t0);
    let t1 = t0 + Duration::from_millis(10);
    ev.activity.content_seq = 2;
    observer.observe(1, &ev, t1);
    let t2 = t1 + DWELL;
    ev.activity.content_seq = 3;
    observer.observe(1, &ev, t2);
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Running));
    assert!(
        status_indicators(observer.status(1).expect("published")).busy,
        "a running pane lights the dot"
    );

    // Nothing more will ever be written to this PTY. The event loop must
    // still be told to come back, or the dot never goes out.
    let owed = observer.next_wake().expect("a Running pane owes a wake");
    assert!(owed > t0, "the owed wake is in the future, not a spin");

    // At that wake the pane ages out of Running with no new bytes at all.
    let later = t2 + QUIET * 2;
    observer.observe(1, &ev, later);
    observer.observe(1, &ev, later + DWELL);
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Quiet));

    // A pane at rest owes nothing, so an idle machine parks instead of
    // spinning on a deadline it does not need.
    let mut resting = evidence(blank(9));
    resting.foreground_job = Some(false);
    observer.observe(2, &resting, later);
    observer.observe(2, &resting, later + DWELL);
    assert_eq!(observer.status(2).map(|s| s.phase), Some(Phase::Idle));
    assert!(
        !status_indicators(observer.status(2).expect("published")).busy,
        "an idle pane shows no dot"
    );
}

/// End to end on the real fan-out: a BACKGROUND pane's transition must reach
/// the tab its window renders. The status path is leaf-addressed precisely
/// because a focus-addressed one would see nothing here.
#[test]
fn a_background_pane_going_busy_marks_its_tab() {
    use crate::tab_model::TabIndicators;

    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    // The new pane takes focus, so session 0 becomes the background leaf.
    let focused = app.split_active_stub_tab(wid);
    assert_ne!(focused, 0, "the split spawns a second session");

    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    ev.activity.last_output = Some(t0);
    assert!(!app.session_status.observe(0, &ev, t0), "dwell holds first");
    let t1 = t0 + DWELL;
    ev.activity.last_output = Some(t1);
    assert!(app.session_status.observe(0, &ev, t1), "Running publishes");

    app.refresh_session_status_chrome(0);

    let tab = app.windows[&wid]
        .tab_set
        .active()
        .expect("active tab")
        .presentation
        .indicators;
    assert_eq!(
        tab,
        TabIndicators {
            dirty: false,
            busy: true,
            attention: false,
            status_attention: false,
        }
    );
    assert!(
        app.tab_strip_metadata(wid).first().expect("one tab").busy,
        "the bits both strip renderers read must carry it"
    );
}

/// THE ATTENTION LATCH. `refresh_tab_status_indicators` used to OR the
/// STORED bit back in to preserve the two out-of-band native writers — and
/// so ORed back its OWN previous contribution, marking a tab for the life of
/// the process after one failed command. On a tab mixing terminal and native
/// leaves the stale terminal bit could then never be cleared.
#[test]
fn a_cleared_failure_releases_the_tab_while_a_native_sibling_keeps_its_own_mark() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    // A MIXED tab: the terminal leaf (session 0) plus a native sibling.
    let native = app
        .view_store
        .insert_native(crate::tab_model::AppInstanceId::from_stored(7))
        .expect("native view identity space");
    let split = app
        .windows
        .get_mut(&wid)
        .and_then(|window| window.tab_set.active_mut())
        .is_some_and(|tab| tab.split_focused(crate::tab_model::SplitAxis::Vertical, native));
    assert!(split, "mixed terminal + native tab");
    let tab_id = app.windows[&wid].tab_set.active().expect("active tab").id;

    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(1) });
    settle_observer(&mut app.session_status, 0, &ev, t0);
    app.refresh_tab_status_indicators(wid, tab_id);
    assert!(
        attention_of(&app, wid, tab_id),
        "a failed command marks the tab"
    );

    // The native sibling raises its own attention OUT OF BAND (a failed
    // document shutdown / an update announcement): not derivable from the
    // runtime presentation, so the status path must never erase it — and
    // note it does so while the classifier's own bit is ALSO set, which one
    // shared bool could not have told apart.
    app.windows
        .get_mut(&wid)
        .and_then(|window| window.tab_set.active_mut())
        .expect("active tab")
        .presentation
        .indicators
        .attention = true;

    // A new unit of work clears the terminal's outcome.
    ev.shell = Some(ShellEvidence::Executing);
    ev.activity.content_seq = 2;
    ev.activity.last_output = Some(t0 + Duration::from_millis(10));
    let t1 = settle_observer(
        &mut app.session_status,
        0,
        &ev,
        t0 + Duration::from_millis(10),
    );
    assert_eq!(
        app.session_status.status(0).map(|s| s.last_outcome),
        Some(Outcome::None),
        "the classifier itself has let go of the failure"
    );
    app.refresh_tab_status_indicators(wid, tab_id);
    assert!(
        attention_of(&app, wid, tab_id),
        "the NATIVE sibling's out-of-band mark survives"
    );

    // Now the native owner clears its own bit. With the latch gone, nothing
    // is left claiming attention.
    app.windows
        .get_mut(&wid)
        .and_then(|window| window.tab_set.active_mut())
        .expect("active tab")
        .presentation
        .indicators
        .attention = false;
    app.refresh_tab_status_indicators(wid, tab_id);
    assert!(
        !attention_of(&app, wid, tab_id),
        "the terminal's own bit must be clearable, not self-sustaining"
    );

    // And the same on a PURE-terminal tab, which the latch broke too.
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(2) });
    let t2 = settle_observer(
        &mut app.session_status,
        0,
        &ev,
        t1 + Duration::from_millis(10),
    );
    app.refresh_tab_status_indicators(wid, tab_id);
    assert!(attention_of(&app, wid, tab_id));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
    settle_observer(
        &mut app.session_status,
        0,
        &ev,
        t2 + Duration::from_millis(10),
    );
    app.refresh_tab_status_indicators(wid, tab_id);
    assert!(
        !attention_of(&app, wid, tab_id),
        "a passing command retires the previous failure's mark"
    );
}

/// What the CHROME shows: the fold both renderers and the introspection
/// serializer read, not either owner's field alone.
fn attention_of(app: &crate::App, wid: crate::WindowId, tab_id: crate::tab_model::TabId) -> bool {
    app.windows[&wid]
        .tab_set
        .get(tab_id)
        .expect("tab")
        .presentation
        .indicators
        .wants_attention()
}

/// [`settle`] for the observer, which owns the dwell clock through its slots.
fn settle_observer(
    observer: &mut StatusObserver,
    session: u64,
    ev: &Evidence,
    at: Instant,
) -> Instant {
    observer.observe(session, ev, at);
    let later = at + DWELL;
    observer.observe(session, ev, later);
    later
}

/// THE SWEEP RECORDS ITS TRANSITIONS on the session's timeline (RFC §7,
/// built 2026-09-27): a command that fails through the real terminal leaves
/// one `status-change` event carrying the failure, which the `events` digest
/// pushes as `EVENT <local> status …`. Before, the record was polled only
/// and the timeline carried no status transition at all.
///
/// Fed the bytes a real integration writes: the precmd's `133;D` is followed
/// AT ONCE by the next prompt's `133;A` (and its `133;B`), which
/// archives the finished block before the sweep can read it as current.
/// Read from the current block only, this published `outcome=none` for
/// every command (review finding, 2026-09-27; the test then fed no
/// trailing A, a sequence no real shell leaves).
#[test]
fn the_status_sweep_records_one_status_change_per_published_transition() {
    let mut app = crate::App::headless_for_test();
    let front = app
        .front_terminal(crate::WindowId(0))
        .expect("front terminal");
    let (session, term) = (front.session, front.term.clone());
    let timeline = app.pool.get(session).expect("pooled").ctx.timeline.clone();
    let status_events =
        |timeline: &std::sync::Arc<std::sync::Mutex<crate::session_timeline::SessionTimeline>>| {
            timeline
                .lock()
                .unwrap()
                .since(None)
                .filter(|e| e.kind == STATUS_CHANGE)
                .map(|e| e.payload.clone())
                .collect::<Vec<_>>()
        };
    // A command ran and failed: OSC 133 A, B, C, then the precmd's D with
    // exit 7 and the next prompt's A and B, in one batch.
    const NEXT_PROMPT: &[u8] = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\";
    let run = |exit: &str| {
        let mut bytes = format!("\x1b]133;C\x1b\\\x1b]133;D;{exit}\x1b\\").into_bytes();
        bytes.extend_from_slice(NEXT_PROMPT);
        term.lock().unwrap().process(&bytes);
    };
    term.lock().unwrap().process(NEXT_PROMPT);
    run("7");
    let t0 = Instant::now();
    let _ = app.observe_session_statuses(t0);
    let _ = app.observe_session_statuses(t0 + Duration::from_secs(1));
    let events = status_events(&timeline);
    assert_eq!(
        events,
        vec!["phase=idle outcome=failure exit_code=7 signal=- confidence=strong".to_string()],
        "one event for the one published transition"
    );
    // Nothing moved: another sweep records nothing.
    let _ = app.observe_session_statuses(t0 + Duration::from_secs(2));
    assert_eq!(
        status_events(&timeline).len(),
        1,
        "no event without a transition"
    );
    assert_eq!(
        crate::subscribe::timeline_wire_kind(STATUS_CHANGE),
        Some("status"),
        "the events digest pushes it"
    );

    // The failure marks the tab until it is seen; a SECOND failing command
    // that ran inside one sweep interval (no work observed between) is no
    // status change, and only its block tells it apart — through the same
    // trailing prompt, which is what re-arms the mark in the product.
    let attention = |app: &crate::App| {
        app.session_status
            .indicators(session)
            .expect("published")
            .status_attention
    };
    assert!(attention(&app), "an unseen failure marks the tab");
    assert!(app.session_status.note_seen(session), "looking quiets it");
    let _ = app.observe_session_statuses(t0 + Duration::from_secs(3));
    assert!(!attention(&app), "the same failure stays seen");
    run("7");
    let _ = app.observe_session_statuses(t0 + Duration::from_secs(4));
    assert!(attention(&app), "a new failing block re-arms the mark");
    assert_eq!(
        status_events(&timeline).len(),
        1,
        "the second failure is not a status change"
    );

    // A command that succeeds reports that, through the same prompt.
    run("0");
    let _ = app.observe_session_statuses(t0 + Duration::from_secs(5));
    assert_eq!(
        status_events(&timeline).last().map(String::as_str),
        Some("phase=idle outcome=success exit_code=- signal=- confidence=strong"),
    );
    assert!(!attention(&app), "a success carries no mark");
}

/// A PROMPT REPORTS ONLY THE BLOCK IT CLOSED ([`completed_block`]): the
/// finished command behind a prompt, the current block while it is
/// `Complete`, nothing while a command runs, and nothing when the prompt
/// closed a block that never completed — an empty Enter, or a command whose
/// shell was replaced — so an older command's result never comes back.
#[test]
fn a_prompt_reports_the_block_it_closed_and_no_older_one() {
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    let prompt = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\";
    term.process(prompt);
    assert_eq!(
        completed_block(&term),
        None,
        "a first prompt closed nothing"
    );

    term.process(b"\x1b]133;C\x1b\\");
    assert_eq!(
        completed_block(&term),
        None,
        "a running command reports nothing"
    );
    term.process(b"\x1b]133;D;3\x1b\\");
    let failed = completed_block(&term).expect("the current block completed");
    assert_eq!(failed.exit_code, Some(3));
    term.process(prompt);
    assert_eq!(
        completed_block(&term),
        Some(failed),
        "the prompt reports the block it closed"
    );
    assert_eq!(shell_evidence(&term), Some(ShellEvidence::Entering));

    // An empty Enter: A with no C or D closes a block that ran nothing.
    term.process(prompt);
    assert_eq!(
        completed_block(&term),
        None,
        "a prompt that closed an empty block reports no older result"
    );

    // A command whose shell never reported its end (`exec zsh`): the next
    // prompt closes an Executing block.
    term.process(b"\x1b]133;C\x1b\\");
    term.process(prompt);
    assert_eq!(completed_block(&term), None);
}

/// THE "SEEN" ACKNOWLEDGEMENT (RFC §17, 2026-09-27). A failure marks its
/// tab until the user looks at the session; looking quiets the mark while the
/// record keeps the failure; a NEW failure re-arms it — including a second
/// failing command the sweep saw no work between (only its completed block
/// differs), which publishes no status change at all.
#[test]
fn a_seen_failure_quiets_its_tab_and_a_new_failure_re_arms_it() {
    let t0 = Instant::now();
    let mut observer = StatusObserver::new(policy(), Duration::from_millis(250));
    let attention = |o: &StatusObserver| o.indicators(1).expect("published").status_attention;
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(1) });
    ev.completed_block = done(10, 1);
    let t1 = settle_observer(&mut observer, 1, &ev, t0);
    assert!(attention(&observer), "an unseen failure marks the tab");

    assert!(observer.note_seen(1), "looking quiets a live mark");
    assert!(
        !attention(&observer),
        "a seen failure no longer marks the tab"
    );
    assert!(
        observer
            .status(1)
            .expect("published")
            .last_outcome
            .is_failure(),
        "the RECORD keeps the failure; only the mark went quiet"
    );
    assert!(!observer.note_seen(1), "looking again quiets nothing");

    // A second failing command with NO work observed between: the same
    // phase and outcome, so the classifier publishes nothing — only the
    // completed block moved.
    ev.completed_block = done(11, 1);
    assert!(
        !observer.observe(1, &ev, t1 + Duration::from_millis(300)),
        "the second failure is not a status change"
    );
    assert!(attention(&observer), "a new failure re-arms the mark");

    // The same block observed again is the SAME failure.
    observer.note_seen(1);
    observer.observe(1, &ev, t1 + Duration::from_millis(600));
    assert!(
        !attention(&observer),
        "re-reading one failure does not re-arm"
    );

    // Work, then another failure: the ordinary edge.
    ev.shell = Some(ShellEvidence::Executing);
    ev.completed_block = None;
    ev.activity.content_seq = 2;
    ev.activity.last_output = Some(t1 + Duration::from_millis(900));
    let t2 = settle_observer(&mut observer, 1, &ev, t1 + Duration::from_millis(900));
    assert!(
        !attention(&observer),
        "a running session carries no failure"
    );
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(2) });
    ev.completed_block = done(12, 2);
    settle_observer(&mut observer, 1, &ev, t2 + Duration::from_millis(10));
    assert!(attention(&observer), "the next failure marks the tab again");
}

/// A session WITHOUT shell integration has one failure edge, its lifecycle
/// exit, and no block to tell failures apart: the published edge is the
/// defined fallback, so the mark arms once and a look quiets it.
#[test]
fn a_session_without_shell_integration_arms_its_mark_on_the_published_edge() {
    let t0 = Instant::now();
    let mut observer = StatusObserver::new(policy(), Duration::from_millis(250));
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    observer.observe(1, &ev, t0);
    let exit = Lifecycle::Exited { exit_code: Some(3) };
    observer.note_exit(1, exit, &ev, t0 + Duration::from_millis(10));
    assert!(observer.indicators(1).expect("published").status_attention);
    assert!(observer.note_seen(1));
    observer.observe(1, &ev, t0 + Duration::from_millis(400));
    assert!(
        !observer.indicators(1).expect("published").status_attention,
        "the held exit is one failure, seen once"
    );
}

/// THE FLAPPING POLICY (RFC §15). Rapid heuristic transitions must not flood
/// the bounded timeline: a `status-change` event is taken only when the
/// published phase or outcome moved, and dwell bounds how often that can
/// happen — here a pane that alternates output and silence faster than
/// dwell for ten seconds yields at most one event per dwell interval.
#[test]
fn rapid_heuristic_transitions_append_at_most_one_status_event_per_dwell() {
    let fast = StatusPolicy {
        quiet_after: Duration::from_millis(100),
        dwell: DWELL,
    };
    let t0 = Instant::now();
    let mut observer = StatusObserver::new(fast, Duration::from_millis(0));
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    let mut events = Vec::new();
    let span = Duration::from_secs(10);
    let step = Duration::from_millis(50);
    let mut at = t0;
    let mut seq = 1;
    while at < t0 + span {
        // Output for 150 ms, then 150 ms of silence: Running and Quiet
        // candidates alternate every few samples.
        if (at - t0).as_millis() % 300 < 150 {
            seq += 1;
            ev.activity.content_seq = seq;
            ev.activity.last_output = Some(at);
        }
        if observer.observe(1, &ev, at)
            && let Some(payload) = observer.take_status_event(1)
        {
            events.push(payload);
        }
        at += step;
    }
    let bound = (span.as_millis() / DWELL.as_millis()) as usize + 1;
    assert!(
        events.len() <= bound,
        "{} status events in {span:?} (bound {bound}): {events:?}",
        events.len()
    );
    // Not vacuous: a transition that survives dwell IS an event.
    let mut steady = evidence(blank(1));
    steady.foreground_job = Some(true);
    steady.shell = Some(ShellEvidence::Complete { exit_code: Some(1) });
    steady.completed_block = done(4, 1);
    settle_observer(&mut observer, 1, &steady, at);
    let payload = observer
        .take_status_event(1)
        .expect("a surviving transition");
    assert_eq!(
        payload,
        "phase=idle outcome=failure exit_code=1 signal=- confidence=strong"
    );
    assert_eq!(
        observer.take_status_event(1),
        None,
        "one event per transition"
    );
}

/// A PTY exit publishes `Exited` immediately and STAYS there. Under `--hold`
/// the pane outlives its shell and keeps being swept; with the child gone
/// `tcgetpgrp` fails, which reads as "no foreground job" — and would have
/// published `Idle`, which chrome renders as "ready". A dead pane claiming
/// to be a shell at a prompt is the exact lie the sticky lifecycle prevents.
#[test]
fn an_exited_pane_stays_exited_instead_of_reverting_to_ready() {
    let mut app = crate::App::headless_for_test();
    app.hold = true;
    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    ev.activity.last_output = Some(t0);
    settle_observer(&mut app.session_status, 0, &ev, t0);
    assert_eq!(
        app.session_status.status(0).map(|s| s.phase),
        Some(Phase::Running)
    );

    // The stub session's pid is not a real child, so the collector honestly
    // answers "unknown" rather than inventing a code.
    app.note_session_exit(0);
    assert_eq!(
        app.session_status.status(0).map(|s| s.phase),
        Some(Phase::Exited),
        "exit is Exact and bypasses dwell"
    );
    assert_eq!(
        app.session_status.status(0).map(|s| s.last_outcome),
        Some(Outcome::None),
        "an uncollectable status is unknown, NEVER success"
    );

    // The `--hold` sweep: the child is gone, so `tcgetpgrp` reports no
    // foreground job. Without the sticky fact this republished `Idle`.
    let mut after = evidence(blank(2));
    after.foreground_job = Some(false);
    let t1 = t0 + DWELL * 4;
    settle_observer(&mut app.session_status, 0, &after, t1);
    assert_eq!(
        app.session_status.status(0).map(|s| s.phase),
        Some(Phase::Exited),
        "a dead pane must not claim to be a shell at a prompt"
    );
    assert_eq!(
        app.session_status_text(0).as_deref(),
        Some("exited"),
        "and chrome must not read 'ready'"
    );
}

/// A collected code becomes the session's outcome, and a failure marks the
/// tab. This is what `Lifecycle` is FOR — the enum stopped being scaffolding
/// when the exit edge started feeding it.
#[test]
fn a_collected_exit_code_becomes_the_outcome() {
    let t0 = Instant::now();
    let cases = [
        (
            Lifecycle::Exited { exit_code: Some(0) },
            Outcome::Success,
            false,
        ),
        (
            Lifecycle::Exited { exit_code: Some(7) },
            Outcome::Failure { exit_code: 7 },
            true,
        ),
        (Lifecycle::Exited { exit_code: None }, Outcome::None, false),
        (
            Lifecycle::Signalled { signal: 9 },
            Outcome::Signal { signal: 9 },
            true,
        ),
    ];
    for (lifecycle, outcome, attention) in cases {
        let mut observer = StatusObserver::new(policy(), Duration::from_millis(0));
        let ev = evidence(blank(1));
        assert!(observer.note_exit(1, lifecycle, &ev, t0), "{lifecycle:?}");
        let status = observer.status(1).expect("published");
        assert_eq!(status.phase, Phase::Exited, "{lifecycle:?}");
        assert_eq!(status.last_outcome, outcome, "{lifecycle:?}");
        assert_eq!(status.confidence, Confidence::Exact, "{lifecycle:?}");
        assert_eq!(status.reasons, vec![Reason::LifecycleExit], "{lifecycle:?}");
        assert_eq!(
            status_indicators(status).status_attention,
            attention,
            "{lifecycle:?}"
        );
    }
}

/// A live policy edit must reach sessions that ALREADY exist. The FSM holds
/// its own copy of the policy, so updating only the observer would apply a
/// Settings change to future sessions and silently leave every open tab on
/// the old numbers.
#[test]
fn a_policy_edit_reaches_live_sessions_and_reopens_the_deadline() {
    let t0 = Instant::now();
    let mut observer = StatusObserver::new(policy(), Duration::from_millis(250));
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    settle_observer(&mut observer, 1, &ev, t0);

    // A candidate serving its dwell owes a wake at first_seen + dwell.
    ev.foreground_job = Some(false);
    let t1 = t0 + DWELL;
    observer.observe(1, &ev, t1);
    let before = observer
        .next_wake()
        .expect("a pending candidate owes a wake");
    assert_eq!(before, t1 + DWELL);

    assert!(
        observer.reconfigure(
            StatusPolicy {
                quiet_after: QUIET,
                dwell: Duration::from_millis(10),
            },
            Duration::from_millis(10),
            t1,
        ),
        "a real change reports that it moved"
    );
    let after = observer.next_wake().expect("still owed");
    assert_eq!(
        after,
        t1 + Duration::from_millis(10),
        "the LIVE session's dwell moved, not just the observer's field"
    );
    assert!(
        observer.due(1, t1 + Duration::from_millis(10)),
        "the shortened interval takes effect now, not after one old interval"
    );
    assert!(
        !observer.reconfigure(
            StatusPolicy {
                quiet_after: QUIET,
                dwell: Duration::from_millis(10),
            },
            Duration::from_millis(10),
            t1,
        ),
        "a no-op edit costs no fan-out"
    );
}

/// `tab_status = false` is a REAL off: no classification at all, and the
/// records already on screen are retired rather than frozen.
#[test]
fn turning_tab_status_off_stops_classifying_and_retires_the_records() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let tab_id = app.windows[&wid].tab_set.active().expect("active tab").id;
    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(1) });
    settle_observer(&mut app.session_status, 0, &ev, t0);
    app.refresh_tab_status_indicators(wid, tab_id);
    assert!(attention_of(&app, wid, tab_id));

    app.config.tab_status = Some(false);
    app.reconfigure_session_status();
    assert!(
        app.session_status.status(0).is_none(),
        "a stale phase must not sit on a tab after the classifier stops"
    );
    assert!(
        !attention_of(&app, wid, tab_id),
        "and the chrome settles in the same pass"
    );
    assert!(
        app.observe_session_statuses(t0 + DWELL * 4).is_empty(),
        "the sweep returns before it touches the pool"
    );
    app.note_session_exit(0);
    assert!(
        app.session_status.status(0).is_none(),
        "not even the exit edge classifies while the subsystem is off"
    );
}

/// `tab_status_badge = false` keeps the record and only quiets the chrome —
/// the two switches are independent on purpose.
#[test]
fn the_badge_switch_quiets_chrome_without_stopping_classification() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let tab_id = app.windows[&wid].tab_set.active().expect("active tab").id;
    app.config.tab_status_badge = Some(false);

    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(1) });
    settle_observer(&mut app.session_status, 0, &ev, t0);
    app.refresh_tab_status_indicators(wid, tab_id);

    assert!(!attention_of(&app, wid, tab_id), "no mark on the tab");
    assert_eq!(
        app.session_status.status(0).map(|s| s.last_outcome),
        Some(Outcome::Failure { exit_code: 1 }),
        "the record is still classified and still readable"
    );
    assert_eq!(
        app.session_status_text(0).as_deref(),
        Some("failed (exit 1)")
    );
}

/// THE INPUT FIELDS (2026-09-24): `input= input_bytes= input_wait_ms=
/// fg_rss_mb=` ride BOTH arms of the record, right after `integration=` and
/// right before `supervisor=`, and the stamp still rides last. A session
/// with no tty (every headless stub) has no reading: all four are `-`.
#[test]
fn the_status_record_carries_the_input_fields_before_supervisor() {
    let mut app = crate::App::headless_for_test();
    let fields = " input=- input_bytes=- input_wait_ms=- fg_rss_mb=- supervisor=";
    let unobserved = app.session_status_record(0).expect("live session");
    assert!(unobserved.contains(" observed=false "), "{unobserved}");
    let observed = {
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
        settle_observer(&mut app.session_status, 0, &ev, Instant::now());
        app.session_status_record(0).expect("live session")
    };
    assert!(observed.contains(" observed=true "), "{observed}");
    for record in [unobserved, observed] {
        assert!(record.contains(fields), "{record}");
        let integration = record.find(" integration=").expect("integration=");
        let input = record.find(" input=").expect("input=");
        assert!(
            !record[integration + 1..input].contains(' '),
            "input= follows integration=: {record}"
        );
        assert_stamp_rides_last(&record);
    }
}

/// The two ADDITIVE fabric fields (§11.2). They answer the question a driver
/// asks right after `ERR halted`: is this session held, and is there a bridge
/// alive to lift it? Both arrive on BOTH arms of the record — the unobserved
/// arm too, because a session nobody has classified yet can still be halted —
/// and neither bumps `schema=`, which is what "additive" means here.
#[test]
fn the_status_record_carries_hold_and_fabric_inbox_hold() {
    crate::fabric::with_link_reset(the_status_record_carries_hold_and_fabric_body);
}

fn the_status_record_carries_hold_and_fabric_body() {
    let mut app = crate::App::headless_for_test();
    let record = app.session_status_record(0).expect("live session");
    assert!(record.contains(" observed=false "), "the unobserved arm");
    assert!(record.contains(" hold=0 "), "{record}");
    // `hold_advisory=` (2026-09-29) right after `hold=`: no bridge ever
    // lost here, so none.
    assert!(
        record.contains(" hold=0 hold_advisory=- fabric="),
        "{record}"
    );
    assert!(record.contains(" fabric=absent "), "{record}");
    // The round-13 tail: no bridge, so no ack and no age — then `identity=`
    // (session identities) — and after it the round-19 tail: no wake yet, so
    // no hand, a quiet level and no story.
    assert!(
        record.contains(" fabric=absent fabric_rtt_ms=- fabric_link_age_ms=- identity=- "),
        "{record}"
    );
    // Round 22 appended the SCREEN'S STAMP after the round-19 tail, the same
    // additive way, so the tail is no longer the end of the record.
    assert!(
        record.contains(" hand=- level=quiet story=0 why=- path=live program="),
        "{record}"
    );
    // The person's stamp (2026-09-25) and its count (2026-09-27): no
    // person has keyed session 0, and the fields ride after
    // `supervisor=`; the owner's columns
    // follow it, then the history this session's handoffs lost (none:
    // it never crossed one, 2026-09-26), the integration its shell runs
    // (none signed: `-`, 2026-09-26), the resize ledger's verdict and
    // counts (never resized: `ok`, 2026-09-28), then the screen's stamp.
    assert!(
        record.contains(
            " supervisor=- human_ms=- human_seq=0 path_evidence=- copy=- upgrade=- history_lost=0 \
             integration_rev=- render=ok resizes=0 flaps=0 gen="
        ),
        "{record}"
    );
    assert_stamp_rides_last(&record);
    assert!(
        record.starts_with("schema=1 "),
        "additive, not a new schema"
    );

    // Held: the flag flips on the OBSERVED arm too, from the same leaf read.
    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
    settle_observer(&mut app.session_status, 0, &ev, t0);
    let ctx = app.pool.get(0).expect("live session").ctx.clone();
    crate::fabric::apply_hold_for_test(
        &ctx,
        Some(crate::fabric::Hold {
            reason: "main%20broken".to_string(),
            origin: "fleet".to_string(),
        }),
    );
    let record = app.session_status_record(0).expect("live session");
    assert!(record.contains(" observed=true "), "{record}");
    assert!(record.contains(" hold=1 "), "{record}");
    assert!(!record.contains('\n'), "one line, always");
}

#[test]
fn fabric_batch_matches_single_status_fields_for_each_session() {
    let mut app = crate::App::headless_for_test();
    let second = crate::stub_session(1);
    crate::App::register_session(&app.store, &second, None);
    app.pool.insert(second);

    let assert_same = |app: &crate::App| {
        let batch = app.session_statuses_record();
        let mut lines = batch.lines();
        assert_eq!(lines.next(), Some("OK 2"), "{batch}");
        for local in [0, 1] {
            let h = app
                .store
                .read()
                .unwrap()
                .by_local(local)
                .expect("registered session")
                .clone();
            let row = lines.next().expect("one row per live session");
            let prefix = format!("{local} {} {} ", h.sid.as_str(), h.nonce.to_hex());
            let fields = row.strip_prefix(&prefix).expect("stable sid and nonce");
            let single = app.session_status_record(local).expect("live session");
            for key in ["sid=", "revision=", "hold=", "detail=", "agent="] {
                let batch_value = fields
                    .split_whitespace()
                    .find(|part| part.starts_with(key))
                    .expect("batch field present");
                let single_value = single
                    .split_whitespace()
                    .find(|part| part.starts_with(key))
                    .expect("single field present");
                assert_eq!(batch_value, single_value, "{key}: {row}");
            }
        }
        assert!(lines.next().is_none(), "framed count is exact: {batch}");
    };

    // Both sessions start unclassified. An unavailable terminal must not
    // invent an observed detail or revision for either one.
    let locked_term = app.pool.get(0).unwrap().term.clone();
    let guard = locked_term.lock().unwrap();
    assert_same(&app);
    drop(guard);

    // One session becomes classified and held; its cached detail supplies
    // the same percent-encoded fallback when its terminal is contended.
    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
    settle_observer(&mut app.session_status, 0, &ev, t0);
    assert!(
        app.session_status
            .set_detail(0, Some("Claude Code".to_string()))
    );
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    crate::fabric::apply_hold_for_test(
        &ctx,
        Some(crate::fabric::Hold {
            reason: "test".to_string(),
            origin: "fleet".to_string(),
        }),
    );
    let guard = locked_term.lock().unwrap();
    assert_same(&app);
    assert!(
        app.fabric_status_fields(0)
            .unwrap()
            .contains("detail=Claude%20Code")
    );
    drop(guard);
}

#[test]
fn fabric_batch_does_not_wait_for_a_background_timeline_writer() {
    use std::sync::mpsc;

    let app = crate::App::headless_for_test();
    let timeline = app.pool.get(0).expect("session").ctx.timeline.clone();
    {
        let mut guard = timeline.lock().unwrap();
        guard.publish_agent(
            "prompt",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
    }
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    // The writer keeps the lock until the test says the batch is back, so
    // "the batch did not wait for the writer" is an ORDER: `true` from the
    // join means the batch returned while the writer still held the lock.
    // The minute is only a hang detector for the old blocking read, which
    // parks behind this guard until the writer gives up (`false`). What the
    // order does NOT see is how long the batch took: a read that stalled a
    // bounded while (under the minute) and then deferred would pass. That it
    // does not stall at all is the code's shape — one `try_lock`, no timed
    // wait — and no clock here judges it. (The order replaces a 200 ms bound
    // on the batch, which measured the scheduler as much as the lock
    // discipline, and failed on a correct tree under load.)
    let worker = std::thread::spawn(move || {
        let _guard = timeline.lock().unwrap();
        held_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(60)).is_ok()
    });
    held_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the writer took the timeline lock");

    let batch = app.session_statuses_record();
    let _ = release_tx.send(());
    assert!(
        worker.join().unwrap(),
        "a Fabric roster read waited on the main thread for the background \
         timeline writer to let go"
    );
    assert!(batch.contains("agent_deferred=1"), "{batch}");
    assert!(
        !batch.contains("agent=-"),
        "a busy writer is not no agent: {batch}"
    );

    let settled = app.session_statuses_record();
    assert!(settled.contains("agent=prompt"), "{settled}");
}

/// IDENTITY (session identities, phase 1): the record carries the agent
/// identity the session was spawned under — the word the `sessions`
/// roster carries, from the same spawn-time field — `-` for the human's
/// own agent config. It rides BOTH arms after `fabric_link_age_ms=`,
/// additive, `schema=1` unmoved; round 19's presence tail (`hand=`,
/// `level=`, `story=`) was appended after it, the same way.
#[test]
fn the_status_record_carries_the_identity_after_the_fabric_tail() {
    crate::fabric::with_link_reset(
        the_status_record_carries_the_identity_after_the_fabric_tail_body,
    );
}

fn the_status_record_carries_the_identity_after_the_fabric_tail_body() {
    let mut app = crate::App::headless_for_test();
    let record = app.session_status_record(0).expect("live session");
    assert!(record.contains(" observed=false "), "the unobserved arm");
    assert!(
        record.contains(" fabric_link_age_ms=- identity=- hand="),
        "{record}"
    );

    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .identity = Some("worker".to_string());
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(" fabric_link_age_ms=- identity=worker hand="),
        "{record}"
    );
    assert!(
        record.starts_with("schema=1 "),
        "additive, not a new schema"
    );

    // The OBSERVED arm carries it too, from the same field.
    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
    settle_observer(&mut app.session_status, 0, &ev, t0);
    let record = app.session_status_record(0).expect("live session");
    assert!(record.contains(" observed=true "), "{record}");
    assert!(
        record.contains(" fabric_link_age_ms=- identity=worker hand="),
        "{record}"
    );
    assert!(!record.contains('\n'), "one line, always");
}

/// The `status` verb's reply, field by field. `schema=` leads so a consumer
/// can reject an unknown MAJOR from the first token.
#[test]
fn the_status_record_is_versioned_and_separates_unobserved_from_unknown() {
    // The tail it pins is `fabric=`, which is process-global instance state.
    crate::fabric::with_link_reset(
        the_status_record_is_versioned_and_separates_unobserved_from_unknown_body,
    );
}

fn the_status_record_is_versioned_and_separates_unobserved_from_unknown_body() {
    let mut app = crate::App::headless_for_test();

    // Never classified. This is NOT `phase=unknown`, which means the
    // classifier looked and found no usable evidence.
    let record = app.session_status_record(0).expect("live session");
    assert!(record.starts_with("schema=1 sid=0 "), "{record}");
    assert!(record.contains(" observed=false "), "{record}");
    assert!(record.contains(" phase=unknown "), "{record}");
    assert!(record.contains(" revision=0 "), "{record}");
    assert!(record.contains(" enabled=true"), "{record}");

    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(3) });
    settle_observer(&mut app.session_status, 0, &ev, t0);
    let record = app.session_status_record(0).expect("live session");
    assert!(record.contains(" observed=true "), "{record}");
    assert!(record.contains(" phase=idle "), "{record}");
    assert!(
        record.contains(" outcome=failure exit_code=3 signal=- "),
        "{record}"
    );
    assert!(record.contains(" confidence=strong "), "{record}");
    assert!(record.contains(" reasons=shell_block "), "{record}");
    assert!(record.contains(" conflict=false "), "{record}");
    assert!(
        !record.contains('\n'),
        "the record must stay ONE line: {record}"
    );

    // `tab_status = false` is disclosed rather than left to be inferred from
    // a wall of `unknown`.
    app.config.tab_status = Some(false);
    let record = app.session_status_record(0).expect("live session");
    // `enabled=` was the LAST field when this was written, and the assertion
    // said so with `ends_with`. Four ADDITIVE fabric fields now follow it
    // (`hold=`, `fabric=`, `fabric_rtt_ms=`, `fabric_link_age_ms=`), and
    // `identity=` (session identities) after those, and round 19's
    // `hand=`/`level=`/`story=` after that, which is exactly what the
    // record's own help promises a consumer — fields are appended and never
    // bump `schema=`. So pin the VALUE, and let the tail keep growing.
    assert!(record.contains(" enabled=false "), "{record}");
    assert!(
        record.contains(" fabric=absent fabric_rtt_ms=- fabric_link_age_ms=- identity=- "),
        "{record}"
    );
    // Round 19 appended three more (`hand=`, `level=`, `story=`), the same way.
    // Round 22 appended the SCREEN'S STAMP after the round-19 tail, the same
    // additive way, so the tail is no longer the end of the record. `path=`
    // (2026-09-22) went in between, the same way again — the stamp keeps
    // its contract (last), the tail keeps its order. 2026-09-23: `why=`
    // closes the round-19 tail, and the published `program= agent= …
    // integration=` columns sit between `path=` and the stamp.
    assert!(
        record.contains(" hand=- level=quiet story=0 why=- path=live program="),
        "{record}"
    );
    assert_stamp_rides_last(&record);

    assert!(
        app.session_status_record(9999).is_err(),
        "an unknown session is an error, not a blank record"
    );
}

/// THE TWO ADDITIVE FIELDS (design §5.2). They ride BOTH branches of the
/// record — the never-classified one as well as the classified one — so a
/// poller can never see a field appear from nowhere, and `schema=1` does
/// not move because nothing was renamed or removed.
///
/// `covered` is deliberately unreachable today: it needs Full Disk Access
/// AND `attribution=live` AND a service §7 S4 proved covered, and S4 has
/// not been run. `unknown` here is the honest answer, not a stub.
#[test]
fn the_status_record_carries_the_two_additive_consent_fields() {
    let mut app = crate::App::headless_for_test();

    let record = app.session_status_record(0).expect("live session");
    assert!(record.contains(" observed=false "), "{record}");
    assert!(
        record.contains(" reasons=- attribution=live fs_consent=unknown conflict=false "),
        "the fields sit between `reasons=` and `conflict=`: {record}"
    );
    assert!(
        record.contains(" schema=1 ") || record.starts_with("schema=1 "),
        "{record}"
    );

    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
    settle_observer(&mut app.session_status, 0, &ev, t0);
    let record = app.session_status_record(0).expect("live session");
    assert!(record.contains(" observed=true "), "{record}");
    assert!(
        record.contains(" reasons=shell_block attribution=live fs_consent=unknown conflict="),
        "{record}"
    );
    assert!(
        !record.contains("fs_consent=covered"),
        "`covered` needs a measurement nobody has taken: {record}"
    );
    assert!(!record.contains('\n'), "still ONE line: {record}");

    // An ADOPTED session says so, and still refuses to assert the TCC
    // consequence (§3.9 pt 3).
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .handoff_local_id = Some(11);
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(" attribution=adopted fs_consent=unknown "),
        "{record}"
    );
}

/// `consent_at_risk` is emitted on the CONJUNCTION and on nothing else: it
/// is silent while the cwd is unknown (every session without shell
/// integration), and it appears the moment a reported cwd lands under a
/// protected root. It is not a claim that a dialog is showing.
#[test]
fn consent_at_risk_rides_the_reasons_token_only_on_the_conjunction() {
    assert_eq!(Reason::ConsentPrompt.as_str(), "consent_at_risk");

    let mut app = crate::App::headless_for_test();
    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
    settle_observer(&mut app.session_status, 0, &ev, t0);

    let record = app.session_status_record(0).expect("live session");
    assert!(
        !record.contains("consent_at_risk"),
        "no cwd reported, so no token: {record}"
    );

    // The conjunction half is Unix-only: the OSC 7 fed below is
    // `file://localhost<cwd>` with a `/`-rooted cwd, and on Windows a
    // protected root is `C:\Users\…` — how the Windows integration spells
    // OSC 7 is a separate question this fixture cannot ask.
    #[cfg(unix)]
    {
        // HOME is process-global: the seamless tests point it at a scratch
        // dir under ENV_LOCK, and this block reads it twice (the roots here,
        // and the record's own consent check). Without the lock the two reads
        // can see different homes and the token goes missing (seen
        // 2026-10-11 in the round-fifteen land's gate: the cwd was under a
        // seamless test's `conncarry` home, the record judged it against the
        // real one).
        let _env = crate::seamless::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The protected roots are RESOLVED DATA from the consent module —
        // this file writes no protected-folder literal of its own (rule B13).
        let roots = aterm_containment::consent::protected_roots(&[]);
        let Some(root) = roots.first() else {
            return; // no $HOME resolved: nothing to assert against
        };
        let cwd = root.join("aterm-consent-proof");
        {
            let term = app.pool.get(0).expect("session 0").term.clone();
            let mut t = crate::term_lock(&term);
            t.process(format!("\x1b]7;file://localhost{}\x07", cwd.display()).as_bytes());
        }
        let record = app.session_status_record(0).expect("live session");
        assert!(
            record.contains(",consent_at_risk "),
            "a protected cwd with an uncovered fs_consent arms the token: {record}"
        );
        assert!(
            record.contains(" fs_consent=unknown "),
            "and the token never upgrades the verdict it rides on: {record}"
        );
    }
}

/// LIVE TYPING (review follow-up to frame audit #3). The audit's decay
/// classified `Entering` as plain `Idle` unconditionally, so the settled
/// verdict engaged WHILE the user was typing and the typing subject never
/// showed. Typing must publish the live marker at once: prompt entry with
/// fresh keystroke echo is the same `Idle` phase on the wire, but NOT
/// settled — and because the phase does not change, the marker rides a
/// same-phase reasons update and never waits out a dwell.
#[test]
fn typing_at_the_prompt_publishes_live_without_a_dwell() {
    let t0 = Instant::now();
    let mut fsm = StatusFsm::new(policy(), t0);
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Prompt);
    settle(&mut fsm, &ev, t0);
    assert_eq!(fsm.status().phase, Phase::Idle);
    assert!(fsm.status().settled_idle(), "a bare prompt is settled idle");

    // The first keystroke: block Entering, the echo advances the counter,
    // and the KEY ITSELF is evidence — movement alone is a background job.
    ev.shell = Some(ShellEvidence::Entering);
    ev.activity.content_seq = 2;
    let t1 = t0 + DWELL + Duration::from_millis(10);
    ev.activity.last_input = Some(t1);
    assert!(
        fsm.observe(&ev, t1),
        "the live marker publishes immediately"
    );
    assert_eq!(
        fsm.status().phase,
        Phase::Idle,
        "typing is still idle on the wire — RFC §6 permits no stronger claim"
    );
    assert!(
        !fsm.status().settled_idle(),
        "…but NOT settled, so the typing subject may show while keys land"
    );
    assert_eq!(
        fsm.status().reasons,
        vec![Reason::ShellBlock, Reason::ContentActivity],
        "the record says WHY: prompt entry plus live echo"
    );
}

/// THE REVIEW'S REGRESSION, pinned: `EnteringCommand` is the steady state
/// whenever a prompt is DISPLAYED, so a background job printing at an idle
/// prompt (`tail -f &`, a sibling build) moves the grid without anyone
/// typing. Movement alone re-stuck the "Typing a command" subject on a
/// window nobody had touched for minutes; the marker needs a keystroke.
#[test]
fn background_output_at_a_prompt_is_not_live_typing() {
    let t0 = Instant::now();
    let mut fsm = StatusFsm::new(policy(), t0);
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Entering);
    settle(&mut fsm, &ev, t0);
    assert!(
        fsm.status().settled_idle(),
        "a displayed prompt nobody typed at is settled"
    );

    // A background job prints: the counter advances, no key was pressed.
    ev.activity.content_seq = 2;
    let t1 = t0 + DWELL + Duration::from_millis(10);
    fsm.observe(&ev, t1);
    assert_eq!(fsm.status().phase, Phase::Idle);
    assert!(
        fsm.status().settled_idle(),
        "ambient output must not claim live typing"
    );
    assert_eq!(
        fsm.status().reasons,
        vec![Reason::ShellBlock],
        "no ContentActivity marker without a keystroke"
    );

    // A STALE keystroke (older than the activity window) is no better.
    ev.activity.content_seq = 3;
    ev.activity.last_input = Some(t1);
    let t2 = t1 + QUIET + Duration::from_millis(1);
    fsm.observe(&ev, t2);
    assert!(
        fsm.status().settled_idle(),
        "a keystroke aged past quiet_after cannot resurrect the subject"
    );
}

/// An ABANDONED half-typed prompt: the echo ages out over `quiet_after`,
/// the observer owes the event loop a wake for a transition no new output
/// will ever cause, and the record settles to plain `Idle` — the decay of
/// frame audit #3, with the live state intact this time.
#[test]
fn an_abandoned_prompt_settles_and_owes_the_wake_that_retires_it() {
    let t0 = Instant::now();
    let mut observer = StatusObserver::new(policy(), Duration::from_millis(0));
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Entering);
    observer.observe(1, &ev, t0);
    let t1 = t0 + Duration::from_millis(10);
    ev.activity.content_seq = 2;
    ev.activity.last_input = Some(t1);
    observer.observe(1, &ev, t1);
    let t2 = t1 + DWELL;
    ev.activity.content_seq = 3;
    // The LAST keystroke; nothing lands after it, so the stamp ages
    // out exactly like the echo it caused.
    ev.activity.last_input = Some(t2);
    observer.observe(1, &ev, t2);
    let status = observer.status(1).expect("published");
    assert_eq!(status.phase, Phase::Idle);
    assert!(!status.settled_idle(), "typing is live");

    // No more keystrokes will ever land. The event loop must still be
    // told to come back, or the typing subject never decays.
    let owed = observer
        .next_wake()
        .expect("a live typing pane owes a wake");
    assert_eq!(
        owed,
        t2 + QUIET,
        "the settle window is quiet_after past the last echo"
    );

    // At that wake the marker retires with no new bytes at all.
    assert!(observer.observe(1, &ev, owed), "the decay publishes");
    assert!(
        observer.status(1).expect("published").settled_idle(),
        "an abandoned prompt settles to plain Idle"
    );
    // Settled: nothing left owing a wake, so an idle machine parks.
    assert_eq!(observer.next_wake(), None);
}

/// A command that is typed, run, and finishes returns to a SETTLED idle at
/// the Complete mark itself: the typing marker never outlives prompt
/// entry, so the decay needs no echo window after real work.
#[test]
fn a_settled_command_returns_to_settled_idle() {
    let t0 = Instant::now();
    let mut fsm = StatusFsm::new(policy(), t0);
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Entering);
    fsm.observe(&ev, t0);
    ev.activity.content_seq = 2;
    let t1 = t0 + DWELL;
    ev.activity.last_input = Some(t1);
    fsm.observe(&ev, t1);
    assert!(!fsm.status().settled_idle(), "typing is live");

    ev.shell = Some(ShellEvidence::Executing);
    ev.activity.content_seq = 3;
    let t2 = settle(&mut fsm, &ev, t1 + Duration::from_millis(10));
    assert_eq!(fsm.status().phase, Phase::Running);

    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
    ev.activity.content_seq = 4;
    settle(&mut fsm, &ev, t2 + Duration::from_millis(10));
    assert_eq!(fsm.status().phase, Phase::Idle);
    assert!(
        fsm.status().settled_idle(),
        "Complete is not prompt entry: the marker does not survive it"
    );
    assert_eq!(fsm.status().last_outcome, Outcome::Success);
}

/// The verdict seam end to end over the REAL App wiring: while the user is
/// typing, NEITHER push site — the publish edge
/// (`refresh_session_status_chrome`) nor the per-observation reconcile
/// (`note_title_activity`, which runs on the very output wakes typing
/// produces) — may decay the typing subject; once the echo settles, both
/// flip it to the prompt description.
#[test]
fn the_typing_subject_shows_while_typing_and_decays_once_settled() {
    let mut app = crate::App::headless_for_test();
    let term = app.pool.get(0).expect("session 0").term.clone();
    // OSC 133 A + B: the block is EnteringCommand — prompt entry is open —
    // and the echo of what the user typed has moved the cursor off the
    // command-start mark (an untouched prompt reads as `Ready`, not
    // typing: `title_summary::block_activity`).
    term.lock()
        .unwrap()
        .process(b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\ls");

    // Classifier: Entering with the echo moving the grid between samples.
    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Entering);
    app.session_status.observe(0, &ev, t0);
    ev.activity.content_seq = 2;
    // The keystroke behind that echo — the marker's evidence.
    ev.activity.last_input = Some(t0 + DWELL);
    assert!(
        app.session_status.observe(0, &ev, t0 + DWELL),
        "the live typing record publishes"
    );
    assert!(
        !app.session_status
            .status(0)
            .expect("published")
            .settled_idle()
    );

    // The coordinator observes the Entering block; the reconcile pushes
    // the LIVE verdict, so the typing subject stays up.
    app.note_title_activity(0);
    assert_eq!(
        app.title_summaries.activity(0, &app.config),
        Some("Typing a command"),
        "the typing subject must show WHILE the user is typing"
    );

    // The publish edge pushes the same live verdict.
    app.refresh_session_status_chrome(0);
    assert_eq!(
        app.title_summaries.activity(0, &app.config),
        Some("Typing a command"),
        "the publish edge must not decay a LIVE typing subject"
    );

    // The echo ages out with no further bytes: the abandoned prompt
    // settles, and the same edge now decays the subject.
    assert!(
        app.session_status.observe(0, &ev, t0 + DWELL + QUIET),
        "the settle publishes"
    );
    assert!(
        app.session_status
            .status(0)
            .expect("published")
            .settled_idle()
    );
    app.refresh_session_status_chrome(0);
    assert_eq!(
        app.title_summaries.activity(0, &app.config),
        Some("Ready"),
        "an abandoned prompt decays to the prompt-state description"
    );
}

/// THE CLAUDE-CODE SHAPE (busy-rearm audit, item 1). A TUI that emits
/// output WITHOUT grid movement indefinitely — sync-bracketed identical
/// repaints, heartbeats — keeps `Running` alive through `output_recently`
/// while `last_movement` goes stale past `quiet_after`. Arming the retire
/// wake off the movement clock alone therefore produced a PAST instant on
/// every turn: the wake fired ~immediately, the observation gate refused
/// or observed no change, and the loop re-armed (~200 kHz sustained, 78%
/// CPU, input p99 335 ms — live evidence). The owed wake must be the
/// earliest instant `classify` could actually CHANGE the verdict: the
/// FRESHER of the two clocks plus `quiet_after`.
#[test]
fn output_without_movement_arms_the_retire_wake_off_the_freshest_clock() {
    let t0 = Instant::now();
    let mut observer = StatusObserver::new(policy(), Duration::from_millis(0));
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    ev.activity.last_output = Some(t0);
    observer.observe(1, &ev, t0);
    // One real grid movement, then the grid goes still while output flows.
    let t1 = t0 + DWELL;
    ev.activity.content_seq = 2;
    ev.activity.last_output = Some(t1);
    observer.observe(1, &ev, t1);
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Running));

    // Far past `quiet_after` on the MOVEMENT clock, Running is still
    // honestly sustained by raw output. The movement clock's instant
    // (t1 + QUIET) is long past; the owed wake must ride the output clock.
    let t2 = t1 + QUIET * 2;
    ev.activity.last_output = Some(t2);
    observer.observe(1, &ev, t2);
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Running));
    let owed = observer.next_wake().expect("Running owes its retirement");
    assert_eq!(
        owed,
        t2 + QUIET,
        "the wake is the earliest instant classify could change its verdict"
    );
    assert!(owed > t2, "an owed wake is never already in the past");

    // The user-visible contract is unchanged: at that wake the phase
    // retires with no new bytes at all — output sustains the busy state,
    // true silence ages it out.
    observer.observe(1, &ev, owed);
    observer.observe(1, &ev, owed + DWELL);
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Quiet));
}

/// The structural invariant (busy-rearm audit, item 2): an armed wake at
/// which observation is FORBIDDEN is a contradiction — `due()` refuses the
/// slot until `next_due`, so a deadline before it can only fire, be
/// refused, and re-arm (the dwell-edge spin: up to `min_interval` of
/// hot turns per status transition). `next_wake` must clamp every slot's
/// owed instant to its own observation gate, converting any future bug of
/// this class to at most the observation rate.
#[test]
fn an_armed_wake_never_precedes_its_own_observation_gate() {
    let t0 = Instant::now();
    let interval = Duration::from_millis(250);
    let mut observer = StatusObserver::new(policy(), interval);
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(false);
    observer.observe(1, &ev, t0);

    // Re-observe near the dwell edge: the pending candidate's raw owed
    // instant (t0 + dwell) now precedes the refreshed observation gate.
    let t1 = t0 + DWELL - Duration::from_millis(50);
    observer.observe(1, &ev, t1);
    let gate = observer.sessions.get(&1).expect("slot").next_due;
    assert_eq!(gate, t1 + interval);
    assert!(
        t0 + DWELL < gate,
        "the raw owed wake precedes the gate — the shape under test"
    );
    let owed = observer
        .next_wake()
        .expect("a pending candidate owes a wake");
    assert_eq!(owed, gate, "the armed wake is clamped to the gate");
    assert!(
        observer.due(1, owed),
        "an armed wake at which observation is forbidden is a contradiction"
    );

    // The clamped wake still publishes: dwell was served well before it.
    assert!(observer.observe(1, &ev, owed), "the transition lands");
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Idle));
}

/// The clamp's floor must ADVANCE on the sweep's other refusal. The sweep
/// skips a session whose terminal `try_lock` is held by the PTY reader, and
/// that path never reaches `observe` — so before `note_skipped` the slot's
/// `next_due` stood still, `next_wake` re-armed the same already-past
/// instant every turn, and the "at most the observation rate" bound the
/// clamp advertises did not hold under exactly the sustained-output
/// workload that contends the lock.
#[test]
fn a_refused_sample_still_advances_the_observation_gate() {
    let t0 = Instant::now();
    let interval = Duration::from_millis(250);
    let mut observer = StatusObserver::new(policy(), interval);

    // A Running session whose clocks then go stale: `owed_wake` returns
    // `last_activity + quiet_after`, which recedes into the past.
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    observer.observe(1, &ev, t0);
    // Movement is only inferable between two samples, and the candidate then
    // serves its dwell before publishing — so the third observation is what
    // makes `Running` the PUBLISHED phase (`owed_wake` reads the published
    // record, not the candidate).
    let moved_at = t0 + Duration::from_millis(10);
    ev.activity.content_seq = 2;
    observer.observe(1, &ev, moved_at);
    observer.observe(1, &ev, moved_at + DWELL);
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Running));

    // Long past the retirement instant, with the lock contended ever since.
    let now = t0 + QUIET + Duration::from_secs(30);
    assert!(
        observer.next_wake().is_some_and(|wake| wake < now),
        "the shape under test: an owed wake already deep in the past"
    );

    observer.note_skipped(1, now);
    // Every sweep ends in `note_swept`, which restores `next_due_any` to the
    // exact minimum (`note_skipped` only folds a lower bound, exactly as
    // `observe` does). Mirrored here so the O(1) gate is asserted in the
    // state production actually leaves it in.
    observer.note_swept(observer.swept_pool_epoch(), Vec::new());
    let wake = observer.next_wake().expect("the slot still owes a wake");
    assert!(
        wake >= now + interval,
        "a refused sample charges its interval, so the re-arm is in the future"
    );
    assert!(
        !observer.due(1, now),
        "and the gate that refused it is closed for that interval"
    );
    assert!(
        !observer.any_due(now),
        "the O(1) gate agrees, so the sweep itself stops running per turn"
    );
    assert!(
        observer.due(1, wake),
        "the armed wake is admissible when it fires"
    );

    // An unknown session is NOT charged: it holds no slot, so it owes no
    // deadline and cannot spin the loop, and inserting one here would bank
    // the pool epoch over a session that was never classified.
    observer.note_skipped(404, now);
    assert!(
        !observer.knows(404),
        "a refusal never invents a classification"
    );
}

/// The RFC §4 privacy rule as a table: the program's basename, an
/// allow-listed subcommand at most, never an argument — and the wrappers,
/// assignments and paths an agent's real command lines carry.
#[test]
fn command_detail_keeps_the_program_and_an_allowlisted_subcommand_only() {
    let cases: &[(&str, Option<&str>)] = &[
        ("claude --dangerously-skip-permissions", Some("claude")),
        // A prompt glyph a scrape left in front is not the program.
        ("$ sleep 30", Some("sleep")),
        ("❯ codex", Some("codex")),
        ("% ", None),
        ("codex", Some("codex")),
        ("targo --unverified test -p x", Some("targo test")),
        ("cargo build --release", Some("cargo build")),
        ("git status", Some("git status")),
        // `env -S`'s one string: the subcommand is read from its words,
        // from the closed list, and never an argument of it.
        (
            "env -S 'git status --porcelain /secret/repo'",
            Some("git status"),
        ),
        ("env -S 'git /secret/tok'", Some("git")),
        // A flag's VALUE is never a subcommand: the program stands alone.
        // The value may look exactly like a subcommand (a bare identifier);
        // only vocabulary membership tells them apart, and a directory, a
        // namespace, a context or a prefix is the very category the rule
        // keeps off the wire.
        ("git -C /secret/repo status", Some("git")),
        ("git -C aterm status", Some("git")),
        ("kubectl -n prod get pods", Some("kubectl")),
        ("docker --context customer-acme ps", Some("docker")),
        ("npm --prefix web run build", Some("npm")),
        // A word OUTSIDE the vocabulary is an argument even in the
        // subcommand slot.
        ("git customer-acme", Some("git")),
        ("cargo xtask release", Some("cargo")),
        ("ssh me@host", Some("ssh")),
        ("/usr/local/bin/python3 secret.py", Some("python3")),
        ("./run.sh --token abc", Some("run.sh")),
        // Quoting is honoured: a quoted path with a space is ONE word,
        // and a Windows launcher's `.exe` names the same program the bare
        // name does everywhere else (measured 2026-09-22: `Program`).
        (
            "\"C:\\Program Files\\Git\\usr\\bin\\less.exe\" README.md",
            Some("less"),
        ),
        // The audit's own line: pwsh's call operator (a glyph, skipped as
        // a prompt's is), then a SINGLE-quoted path with a space — and the
        // 8.3 short spelling of the same path, which never had a space.
        (
            "& 'C:\\Program Files\\Git\\usr\\bin\\less.exe' C:\\Windows\\win.ini",
            Some("less"),
        ),
        (
            "& C:\\PROGRA~1\\Git\\usr\\bin\\less.exe C:\\Windows\\win.ini",
            Some("less"),
        ),
        ("'/opt/my tools/claude' --resume", Some("claude")),
        ("C:\\Windows\\System32\\cmd.exe /c dir", Some("cmd")),
        ("C:\\tools\\build.CMD release", Some("build")),
        // An escaped space is part of the WORD, but a program's name is
        // never published with whitespace: it ends at the first one.
        ("my\\ tool --flag", Some("my")),
        ("\"git\" status", Some("git status")),
        // Only the executable extensions Windows resolves are dropped: a
        // dotted program NAME is not a path with an extension.
        ("python3.11 -m http.server", Some("python3.11")),
        ("/usr/bin/a.out", Some("a.out")),
        ("FOO=1 cmd --secret", Some("cmd")),
        ("FOO=1 BAR=two targo test", Some("targo test")),
        ("sudo -u root targo --unverified test", Some("targo test")),
        ("sudo apt install x", Some("apt")),
        ("env FOO=1 codex", Some("codex")),
        // A wrapper by full path is the same wrapper: basename first.
        ("/usr/bin/env FOO=1 codex", Some("codex")),
        (
            "/usr/bin/sudo -u root targo --unverified test",
            Some("targo test"),
        ),
        // A make/just target is user-named: only the conventional ones
        // are vocabulary.
        ("time make deploy-prod", Some("make")),
        ("make test", Some("make test")),
        ("just fmt", Some("just fmt")),
        ("aterm ctl ls", Some("aterm ctl")),
        ("atpkg install foo", Some("atpkg install")),
        ("nice -n 5 kubectl get pods -n prod", Some("kubectl get")),
        ("docker compose up", Some("docker compose")),
        ("'claude' 'resume'", Some("claude")),
        // Control removal must apply before wrapper/vocabulary matching,
        // even though ordinary words are now borrowed without a copy.
        ("clau\u{7f}de --private", Some("claude")),
        ("s\u{7f}udo -u user codex", Some("codex")),
        ("targo te\u{7f}st private", Some("targo test")),
        ("npm run build:prod", Some("npm run")),
        ("pnpm dlx create-foo", Some("pnpm dlx")),
        ("yarn build --token abc", Some("yarn build")),
        // A subcommand slot holding a path or an assignment is dropped.
        ("make ./target/x", Some("make")),
        // A COMPOUND line names the program of the segment that is RUNNING,
        // not the word it opens with: `cd x && claude` is the launch shape
        // the supervise-agent skill recommends, and it read `cd`. The last
        // of an `&&`/`;` chain; a trailing `|| …` runs only on failure, so
        // the operand before it; `exec` is a wrapper.
        ("cd ~/ay && claude", Some("claude")),
        ("cd ~/ay && exec claude", Some("claude")),
        ("exec claude --resume", Some("claude")),
        ("cd /tmp; codex", Some("codex")),
        ("make || echo failed", Some("make")),
        ("FOO=1 cd x && targo test", Some("targo test")),
        (
            "git pull && targo --unverified test -p aterm-gui",
            Some("targo test"),
        ),
        ("cd x && claude || echo failed", Some("claude")),
        ("cd x; claude;", Some("claude")),
        ("cd x &&", Some("cd")),
        ("(cd ~/ay && claude)", Some("claude")),
        ("cd x\nclaude", Some("claude")),
        // A pipeline is still named by its first command.
        ("cd x && cat log | grep err", Some("cat")),
        // An operator inside quotes or behind a backslash is an argument,
        // not a cut.
        ("claude -p \"fix a; then b\"", Some("claude")),
        ("echo 'x && y' && codex", Some("codex")),
        ("echo a \\; codex", Some("echo")),
        // A keyword opener stays the keyword: the body is not parsed, and
        // the `;` in `for …; do` is the keyword's grammar, not a list
        // operator (cutting there would answer `done`). `{` opens a group
        // the same way, so it keeps the first-word reading.
        ("for i in 1 2 3; do sleep 1; done", Some("for")),
        ("$ for i in 1 2 3; do sleep 1; done", Some("for")),
        ("while true; do claude; done", Some("while")),
        ("if [ -f x ]; then claude; fi", Some("if")),
        ("{ cd x; claude; }", Some("cd")),
        // The keyword need not open the LINE: behind a `cd x &&` or a
        // wrapper the first-word reading fell through to the running
        // segment, whose `;`-cut is the CLOSER — `done`, `fi` — and a
        // closer is never a program. The first segment whose program slot
        // holds an opener is read instead.
        ("cd x && for i in 1 2 3; do sleep 1; done", Some("for")),
        ("cd x && if true; then claude; fi", Some("if")),
        ("time for i in 1 2 3; do sleep 1; done", Some("for")),
        ("", None),
        ("   \t ", None),
        (";", None),
    ];
    // A keyword that CLOSES a compound command is never a program, so no
    // row may read one, whatever its `;`-cut would have answered.
    const KEYWORD_CLOSERS: [&str; 8] = ["done", "fi", "esac", "}", "then", "do", "else", "elif"];
    for (cmd, want) in cases {
        let got = command_detail(cmd);
        assert_eq!(got.as_deref(), *want, "{cmd:?}");
        assert!(
            !got.as_deref().is_some_and(|d| KEYWORD_CLOSERS.contains(&d)),
            "{cmd:?} reads a closer, which is never a program: {got:?}"
        );
    }
    // Bounded: a screen-scraped line can be long; the reply cannot.
    let long = format!("{} arg", "p".repeat(200));
    let detail = command_detail(&long).unwrap();
    assert_eq!(detail.chars().count(), 48);
    assert!(
        detail.capacity() <= 48 * 4,
        "long input capacity must not be retained"
    );
    let wide = format!("{} private", "終".repeat(80));
    assert_eq!(command_detail(&wide), Some("終".repeat(48)));
    // A program name is never reported with a subcommand it does not own.
    assert_eq!(command_detail("claude commit").as_deref(), Some("claude"));
}

/// The live E2E's D3 (2026-09-26): once the harness's live upgrade had
/// relaunched Claude Code from the managed store — the line it types quotes
/// every word, `'…/Library/Application Support/aterm/pkg/agents/claude'
/// '--resume' …` — the tab's `status` read `detail=Application`. Words were
/// cut at EVERY space, quoted or not, so the program slot held the first
/// half of the path. A quoted or backslash-escaped space is part of its
/// word, as the shell reads it, so the detail is the program's name.
/// CONTROL: the same program at a path with no space reads as it always
/// did, and a quoted argument with spaces is still never the detail.
#[test]
fn a_program_at_a_path_with_spaces_reads_its_name() {
    let store = "/Users//me/Library/Application Support/aterm/pkg/agents";
    let relaunch = format!(
        ". '/Users//me/.aterm/shell.d/00-atpkg.zsh'; rehash; '{store}/claude' \
         '--dangerously-skip-permissions' '--resume' 'b87ea568-277e-4ba7-bbd2-f0ab8bd14d1c'"
    );
    let cases: &[(String, &str)] = &[
        // The upgrade's relaunch line, verbatim in shape.
        (relaunch, "claude"),
        (format!("'{store}/claude' --resume x"), "claude"),
        (format!("\"{store}/claude\" --resume x"), "claude"),
        (
            format!("{} --resume x", store.replace(' ', "\\ ") + "/claude"),
            "claude",
        ),
        (format!("exec '{store}/claude'"), "claude"),
        // A quoted assignment with a space is ONE assignment, skipped.
        (format!("env 'FOO=a b' '{store}/codex' resume"), "codex"),
        // CONTROL: no space anywhere in the path.
        (
            "/Users//me/.local/share/claude/versions/2.1.281 --resume x".to_string(),
            "2.1.281",
        ),
        (
            "'/Users//me/.local/bin/claude' --resume x".to_string(),
            "claude",
        ),
        // CONTROL: an argument with spaces is an argument.
        (
            "claude -p 'fix the bug in Application Support'".to_string(),
            "claude",
        ),
    ];
    for (cmd, want) in cases {
        assert_eq!(command_detail(cmd).as_deref(), Some(*want), "{cmd:?}");
    }
    // Through the one producer the `status` record reads, by both roads: a
    // screen scrape of the typed line, and OSC 633;E's explicit line.
    let line = format!("'{store}/claude' '--resume' 'x'");
    let mut term = aterm_core::terminal::Terminal::new(24, 200);
    term.process(format!("\x1b]133;A\x07$ \x1b]133;B\x07{line}\n\x1b]133;C\x07").as_bytes());
    assert_eq!(
        executing_detail(&term).as_deref(),
        Some("claude"),
        "scraped"
    );
    term.process(b"\x1b]133;D;0\x07");
    term.process(
        format!("\x1b]133;A\x07$ \x1b]633;E;{line}\x07\x1b]133;B\x07{line}\n\x1b]133;C\x07")
            .as_bytes(),
    );
    assert_eq!(
        executing_detail(&term).as_deref(),
        Some("claude"),
        "explicit"
    );
}

/// PowerShell escapes a space with a BACKTICK: `& C:\Program` Files\Git\
/// usr\bin\less.exe C:\Windows\win.ini` ran less and read `detail=Program`
/// on 0.95.0 (measured 2026-09-27). Inside a Windows path — a drive or a
/// `\` before the backtick — the escaped space is part of the word, so the
/// detail is the program's name. CONTROL: a backtick after a bare name, the
/// one place a POSIX shell's command substitution could put it, still ends
/// the word at the space, so an argument behind it never becomes the
/// detail; a POSIX command substitution reads as before.
#[test]
fn a_pwsh_backtick_escaped_space_is_part_of_the_program_path() {
    let cases: &[(&str, &str)] = &[
        (
            r"& C:\Program` Files\Git\usr\bin\less.exe C:\Windows\win.ini",
            "less",
        ),
        (
            r"C:\Program` Files\Git\usr\bin\less C:\Windows\win.ini",
            "less",
        ),
        (r"& .\my` tools\build.cmd --target x", "build"),
        (r"& C:/Program` Files/Git/usr/bin/vim.exe notes.txt", "vim"),
        // The escaped space in the LAST component: the name ends at it.
        (r"& C:\tools\my` app.exe --token SECRET", "my"),
        // CONTROL: no Windows path in front of the backtick.
        ("claude` /secret/tok --resume x", "claude"),
        ("claude `echo /secret/tok`", "claude"),
        ("`which less` README.md", "which"),
        // CONTROL: the same program quoted, as tab completion writes it.
        (
            r"& 'C:\Program Files\Git\usr\bin\less.exe' C:\Windows\win.ini",
            "less",
        ),
    ];
    for (cmd, want) in cases {
        assert_eq!(command_detail(cmd).as_deref(), Some(*want), "{cmd:?}");
    }
    assert!(windows_path_so_far(r"C:\Program"));
    assert!(windows_path_so_far("d:/tools/my"));
    assert!(windows_path_so_far(r".\my"));
    assert!(!windows_path_so_far("claude"));
    assert!(!windows_path_so_far("/usr/local/bin/my"));
}

/// The quote that lets a spaced path be ONE word (D3, above) must never
/// carry an ARGUMENT into `detail=` — "never an argument" is the privacy
/// rule, and splitting at every space kept it by construction. A line the
/// POSIX reading gets wrong ran its program's word on to the end of the
/// line, and `detail=` published whatever followed the word's last `/`:
/// measured on the first D3 fix, `$'/usr/local/bin/don\'t' --token
/// SECRET` read `'t' --token SECRET`, the harness's own fish quoting read
/// `claude' '--resume' 'b87ea568-…` (the session id), and `don't --token
/// SECRET` read the whole line. And ending a word at the first whitespace
/// after the LAST quote that opened still leaked when the argument's
/// opening quote closed the program's apostrophe: `don't '/secret/tok'`
/// read `tok`, `/Users//o'neil/bin/claude --resume '/secret/tok'` read
/// `tok`, `it's-a-script.sh --password 'x/hunter2'` read `hunter2`. So a
/// word ends where EITHER reading (POSIX, fish) ends it, a reading that
/// leaves a quote open ends the word at the word's own first whitespace,
/// and the published name ends at its first whitespace. CONTROL: the
/// managed copy's `Application Support` path, quoted every way a shell
/// quotes it, still reads `claude`, and `don't "/secret/tok"` — whose
/// double quotes sit inside the apostrophe's open quote, so the quote left
/// open is the program's own — read `don't` under both rules.
#[test]
fn a_misquoted_line_never_publishes_an_argument() {
    let store = "Library/Application Support/aterm/pkg/agents/claude";
    let cases: &[(String, &str)] = &[
        // bash/zsh `$'…'`, where `\'` is a quote: POSIX reads the quote
        // after `t` as opening one that never closes. What is read is a
        // piece of the program's name (`don't`), never the argument.
        (r"$'/usr/local/bin/don\'t' --token SECRET".into(), "t"),
        // The harness's own fish quoting (`upgrade::quote`) of a home with
        // an apostrophe: POSIX runs the program's word into the session id.
        (
            r"'/Users//o\'neil/bin/claude' '--resume' 'b87ea568-277e-4ba7-bbd2-f0ab8bd14d1c'".into(),
            "claude",
        ),
        // …and a line POSIX misreads with every quote closed, whose last
        // `/` sits in an argument: no open quote to notice, only fish's
        // nearer end.
        (
            r"'/Users//o\'neil/bin/claude' '--resume' '/secret/tok en'".into(),
            "claude",
        ),
        // An apostrophe in a bare word opens a quote that never closes —
        // and the rest of the line may hold a `/` of its own.
        ("don't --token SECRET".into(), "don't"),
        ("don't /secret/tok".into(), "don't"),
        (
            "it's-a-script.sh --password hunter2".into(),
            "it's-a-script.sh",
        ),
        // …and the ARGUMENT's opening quote may close the program's
        // apostrophe, running the word through the argument, with the
        // line's last quote left open behind it. The word still ends at
        // its first whitespace, so nothing past the program is read.
        ("don't '/secret/tok'".into(), "don't"),
        (
            "it's-a-script.sh --password 'x/hunter2'".into(),
            "it's-a-script.sh",
        ),
        ("don't --token '/secret/tok'".into(), "don't"),
        (
            "don't '--resume' '/Users//me/secret-session'".into(),
            "don't",
        ),
        ("sudo don't '/secret/tok'".into(), "don't"),
        ("o'neil-tool --token '/secret/tok'".into(), "o'neil-tool"),
        (
            "/Users//o'neil/bin/claude --resume '/secret/tok'".into(),
            "claude",
        ),
        ("can't 'x' '/secret/tok'".into(), "can't"),
        ("it's 'a' b '/secret/tok'".into(), "it's"),
        // CONTROL: double quotes inside the apostrophe's open quote pair
        // nothing, so the quote left open is the program's own.
        ("don't \"/secret/tok\"".into(), "don't"),
        // `env -S` hands env ONE string that env splits into the program
        // and its arguments: the program is that string's first word,
        // never its basename (the third review read `server.js`, `tok`,
        // `notes.py`, `app.ts`).
        (
            "env -S \"node --inspect /Users//me/secret-project/server.js\"".into(),
            "node",
        ),
        ("env -S 'claude --resume /secret/tok'".into(), "claude"),
        (
            "/usr/bin/env -S \"python3 /Users//me/private/notes.py --token x\"".into(),
            "python3",
        ),
        (
            "env --split-string 'deno run --allow-read /Users//me/secret/app.ts'".into(),
            "deno",
        ),
        (
            "env --split-string='deno run --allow-read /Users//me/secret/app.ts'".into(),
            "deno",
        ),
        ("env -vS 'node /srv/secret/server.js'".into(), "node"),
        ("env -S'node /srv/secret/server.js'".into(), "node"),
        ("env -S '-i FOO=1 /usr/bin/node /srv/x.js'".into(), "node"),
        // CONTROL: `-u` takes a value, so `-uS` unsets `S` and is no split.
        ("env -uS /usr/bin/node /srv/x.js".into(), "node"),
        // A space in the program's OWN name is never published.
        (r"/Users//me/my\ tool --token SECRET".into(), "my"),
        ("\"/opt/my tools/run me\" --token SECRET".into(), "run"),
        // fish's quoting of the spaced store path under an apostrophe
        // home: the readings disagree, the nearer end wins. A directory's
        // name, never an argument (the cost of not knowing the dialect).
        (
            format!(r"'/Users//o\'neil/{store}' '--resume' 'SECRET'"),
            "Application",
        ),
        // CONTROL: the managed copy's path, as POSIX, fish (no apostrophe:
        // the same line), double quotes and backslashes write it.
        (format!("'/Users//me/{store}' '--resume' 'SECRET'"), "claude"),
        (
            format!(r"'/Users//o'\''neil/{store}' '--resume' 'SECRET'"),
            "claude",
        ),
        (format!("\"/Users//me/{store}\" --resume SECRET"), "claude"),
        (
            format!("/Users//me/{} --resume SECRET", store.replace(' ', r"\ ")),
            "claude",
        ),
    ];
    const ARGUMENTS: [&str; 7] = [
        "SECRET", "token", "hunter2", "b87ea568", "resume", "secret", "tok",
    ];
    for (cmd, want) in cases {
        let got = command_detail(cmd);
        assert_eq!(got.as_deref(), Some(*want), "{cmd:?}");
        let got = got.unwrap_or_default();
        assert!(
            !got.contains(char::is_whitespace),
            "{cmd:?}: a program's name is published without whitespace: {got:?}"
        );
        for argument in ARGUMENTS {
            assert!(
                !got.contains(argument),
                "{cmd:?}: {got:?} carries the argument {argument:?}"
            );
        }
    }
}

/// The allocation-free selection must keep the old group/pop semantics,
/// especially blank groups and trailing OR alternatives. Quoting stays in
/// the unchanged splitter and is exercised by the privacy table above.
#[test]
fn borrowed_running_segment_matches_the_previous_group_selection() {
    fn previous<'a>(cmdline: &'a str, segments: &[(ListOp, &'a str)]) -> &'a str {
        let mut groups: Vec<Vec<(ListOp, &str)>> = Vec::new();
        for &(op, text) in segments {
            if op == ListOp::Seq || groups.is_empty() {
                groups.push(Vec::new());
            }
            groups.last_mut().unwrap().push((op, text));
        }
        let mut chain = groups
            .into_iter()
            .rev()
            .find(|group| group.iter().any(|(_, text)| !text.trim().is_empty()))
            .unwrap_or_default();
        while chain.len() > 1
            && chain
                .last()
                .is_some_and(|(op, text)| *op == ListOp::Or || text.trim().is_empty())
        {
            chain.pop();
        }
        chain.last().map_or(cmdline, |(_, text)| text)
    }

    let words = ["", " ", "first", "last"];
    let operators = [";", "\n", "&&", "||"];
    // Four operands and three operators: all 16,384 combinations include
    // empty first operands, entirely blank groups and mixed &&/|| tails.
    for case in 0..4usize.pow(7) {
        let mut choices = case;
        let mut cmdline = String::new();
        for segment in 0..4 {
            if segment != 0 {
                cmdline.push_str(operators[choices % 4]);
                choices /= 4;
            }
            cmdline.push_str(words[choices % 4]);
            choices /= 4;
        }
        let segments = split_list(&cmdline);
        assert_eq!(
            running_segment(&cmdline, &segments),
            previous(&cmdline, &segments),
            "{cmdline:?}",
        );
    }
}

/// `executing_detail` is the one producer: a block that is EXECUTING names
/// its (sanitized) command; a prompt, an entering block and a completed
/// block name nothing, whatever text they carry.
#[test]
fn executing_detail_reads_only_an_executing_block() {
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    assert_eq!(executing_detail(&term), None, "no shell integration");
    term.process(b"\x1b]133;A\x07$ ");
    assert_eq!(executing_detail(&term), None, "prompt only");
    term.process(b"\x1b]133;B\x07targo --unverified test -p aterm-gui");
    assert_eq!(executing_detail(&term), None, "entering, not executing");
    term.process(b"\n\x1b]133;C\x07");
    assert_eq!(
        executing_detail(&term).as_deref(),
        Some("targo test"),
        "executing: screen-scraped command, sanitized"
    );
    term.process(b"running\n\x1b]133;D;0\x07");
    assert_eq!(executing_detail(&term), None, "complete");
    // OSC 633;E's explicit command line wins over the screen scrape.
    term.process(b"\x1b]133;A\x07$ \x1b]633;E;claude --dangerously-skip-permissions\x07\x1b]133;B\x07claude --dangerously-skip-permissions\n\x1b]133;C\x07");
    assert_eq!(executing_detail(&term).as_deref(), Some("claude"));
    term.process(b"\x1b]133;D;0\x07");
    // A WIDE prompt glyph before the 133;B mark: the cut is by display
    // column, so the program keeps its first letter.
    term.process(
        "]133;A🐱 $ ]133;Bcodex resume
]133;C"
            .as_bytes(),
    );
    assert_eq!(executing_detail(&term).as_deref(), Some("codex"));
}

/// The `status` record's `detail=`: the sanitized running command while the
/// block executes, `-` otherwise; the engine's fact, so it is answered
/// before the classifier's first publish, under a pin, and never by
/// waiting on a contended terminal. The subject ladder under the same
/// contention: a contended terminal reports `unavailable` rather than
/// silently falling to a lower rung (which a poller would read as a real
/// title change), and a pin is answered from the metadata LEAF lock.
#[test]
fn the_status_record_details_the_executing_block_and_nothing_else() {
    let mut app = crate::App::headless_for_test();
    let term = app.pool.get(0).expect("session 0").term.clone();

    let record = app.session_status_record(0).expect("live session");
    assert!(record.contains(" detail=- "), "nothing running: {record}");

    term.lock()
        .unwrap()
        .process(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep xyzzy\n\x1b]133;C\x07");
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(" observed=false ") && record.contains(" detail=sleep "),
        "the engine's fact precedes the classifier's first publish: {record}"
    );

    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Executing);
    settle_observer(&mut app.session_status, 0, &ev, t0);
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(" observed=true ") && record.contains(" detail=sleep "),
        "{record}"
    );
    // The argument is a word no other field can spell. The record carries
    // wall-clock ages (`since_ms=`, `agent_since_ms=`), so a numeric
    // argument matched one of them on a loaded machine
    // (`agent_since_ms=30` against `sleep 30`).
    assert!(
        !record.contains("xyzzy"),
        "arguments never reach the wire: {record}"
    );

    // Contended: `-`, never a wait, and the subject is `unavailable`, not
    // a lower rung.
    let held = term.lock().unwrap();
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(" subject=- subject_source=unavailable ") && record.contains(" detail=- "),
        "a contended terminal is `-`, not a stall or a lower rung: {record}"
    );

    // A pin answers the subject from the leaf lock with the terminal still
    // held, and keeps the detail once the terminal is free.
    app.pool
        .get(0)
        .expect("session 0")
        .ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set("title", Some("deploy".to_string()))
        .expect("a short title is within cap");
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(" subject=deploy subject_source=pin ") && record.contains(" detail=- "),
        "the pin must not need the terminal lock: {record}"
    );
    drop(held);
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(" subject=deploy subject_source=pin ") && record.contains(" detail=sleep "),
        "{record}"
    );

    term.lock().unwrap().process(b"\x1b]133;D;0\x07");
    let record = app.session_status_record(0).expect("live session");
    assert!(record.contains(" detail=- "), "complete: {record}");
}

/// The chrome already composes `running {detail}`; with the field populated
/// it reads as the running program, and a blank detail stays plain `running`.
#[test]
fn chrome_renders_the_running_detail() {
    let status = |detail: Option<&str>| Status {
        phase: Phase::Running,
        last_outcome: Outcome::None,
        since: Instant::now(),
        detail: detail.map(str::to_string),
        confidence: Confidence::Strong,
        reasons: vec![Reason::ShellBlock],
        conflict: false,
    };
    assert_eq!(
        summary_text(&status(Some("targo test"))).as_deref(),
        Some("running targo test")
    );
    assert_eq!(summary_text(&status(Some(""))).as_deref(), Some("running"));
    assert_eq!(summary_text(&status(None)).as_deref(), Some("running"));
}

/// A guess reads as what was seen, with no confidence word; an exit says
/// its code the way a failed command does.
#[test]
fn chrome_says_what_was_observed() {
    let with = |phase, confidence, last_outcome, reason| Status {
        phase,
        last_outcome,
        since: Instant::now(),
        detail: None,
        confidence,
        reasons: vec![reason],
        conflict: false,
    };
    let status =
        |phase, confidence, last_outcome| with(phase, confidence, last_outcome, Reason::NoEvidence);
    let text = |s: Status| summary_text(&s).unwrap();
    assert_eq!(
        text(status(Phase::Quiet, Confidence::Heuristic, Outcome::None)),
        "no output lately"
    );
    assert_eq!(
        text(with(
            Phase::Running,
            Confidence::Heuristic,
            Outcome::None,
            Reason::ContentActivity
        )),
        "screen changing"
    );
    assert_eq!(
        text(with(
            Phase::Running,
            Confidence::Heuristic,
            Outcome::None,
            Reason::OutputActivity
        )),
        "output arriving"
    );
    assert_eq!(
        text(status(Phase::Starting, Confidence::Unknown, Outcome::None)),
        "starting"
    );
    assert_eq!(
        text(status(
            Phase::Exited,
            Confidence::Exact,
            Outcome::Failure { exit_code: 1 }
        )),
        "exited (exit 1)"
    );
}

/// An agent silent at its prompt or at a box is a shell's `no output
/// lately`; its published phase is what the activity line says.
#[test]
fn chrome_says_the_agent_phase_not_the_shell_guess() {
    use crate::presence::AgentPhase as P;
    let text = |p: P| agent_activity_text(&p);
    assert_eq!(text(P::Busy).as_deref(), Some("busy"));
    assert_eq!(text(P::Idle).as_deref(), Some("idle"));
    assert_eq!(text(P::Survey).as_deref(), Some("idle"));
    assert_eq!(text(P::Question).as_deref(), Some("question"));
    assert_eq!(
        text(P::Prompt {
            detail: Some("question".into())
        })
        .as_deref(),
        Some("question"),
        "the question tool's box approves nothing"
    );
    assert_eq!(
        text(P::Prompt {
            detail: Some("bash:not-read-only".into())
        })
        .as_deref(),
        Some("bash approval")
    );
    assert_eq!(
        text(P::Wall {
            kind: aterm_phase::WallKind::Auth,
            reset: None,
            until: None,
        })
        .as_deref(),
        Some("logged out")
    );
    assert_eq!(text(P::Unknown), None, "no evidence: the shell's line");
}

/// The chrome's `detail` was seeded `None` and never written, so every tab
/// read plain `running` while the wire named the program. The sweep now
/// folds `executing_detail` onto the published status through
/// [`StatusObserver::set_detail`]: an Executing block publishes the
/// sanitized program, and idle (prompt, complete, no integration) clears it.
#[test]
fn the_sweep_publishes_the_running_detail_onto_the_chrome_status() {
    let mut app = crate::App::headless_for_test();
    let term = app.pool.get(0).expect("session 0").term.clone();
    let t0 = Instant::now();

    // Idle: no executing block, so the published detail the chrome reads is
    // empty (plain `running` once running, never a stale command).
    app.observe_session_statuses(t0);
    assert_eq!(
        app.session_status.status(0).and_then(|s| s.detail.clone()),
        None,
        "idle publishes no detail"
    );

    // An executing block: the same sweep guard reads the command and folds
    // its sanitized basename onto the status `summary_text` turns into
    // `running <detail>`.
    term.lock()
        .unwrap()
        .process(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 30\n\x1b]133;C\x07");
    let t1 = t0 + Duration::from_secs(10);
    app.observe_session_statuses(t1);
    assert_eq!(
        app.session_status
            .status(0)
            .and_then(|s| s.detail.clone())
            .as_deref(),
        Some("sleep"),
        "the executing block's sanitized program reaches the chrome status"
    );

    // The block completes: back to idle, the detail clears — a done tab
    // never keeps naming the command that finished.
    term.lock().unwrap().process(b"\x1b]133;D;0\x07");
    let t2 = t1 + Duration::from_secs(10);
    app.observe_session_statuses(t2);
    assert_eq!(
        app.session_status.status(0).and_then(|s| s.detail.clone()),
        None,
        "a completed block clears the chrome detail"
    );
}
