// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-path tests for AOS momentum glow attachment, fault detection, and live repair.

use std::time::{Duration, Instant};

use aterm_aos::contract::Verdict;
use aterm_aos::evidence::EvidenceGrade;
use aterm_aos::log::EventLog;
use aterm_aos::publish::PublishRefusal;
use aterm_effects::aos_momentum::{
    HOT_HAS_WAKE, MOMENTUM_MAX_EVENTS, MomentumAosStatus, MomentumCode, MomentumEvent,
    MomentumSchema, TraceRefusal,
};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

use crate::aos_host::AosRefusal;
use crate::cursor_glow::BlockFillOwner;
use crate::input::{InputEvent, Source};
use crate::scripted_redraw_fixture::{feed, scripted_app, surface};
use crate::{App, WindowId};

pub(super) const WID: WindowId = WindowId(0);
/// A 120 Hz panel: the shipped effect lane.
const PANEL: Duration = Duration::from_nanos(8_333_333);
/// A brisk typist.
pub(super) const KEY_GAP: Duration = Duration::from_millis(60);
/// Pinned-time hang detector for the effect lane loop.
const HORIZON: Duration = Duration::from_secs(60);

/// Scripted fixture with only the momentum glow enabled on a focused 120 Hz window.
pub(super) fn momentum_app(light: bool) -> App {
    let mut app = scripted_app();
    app.config.serious_mode = Some(false);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("off".into());
    app.config.cursor_momentum_glow = Some(true);
    app.config.motion = Some("full".into());
    app.config.trail_sounds = Some(false);
    app.config.cursor_blink = Some(false);
    app.kitty_cursor_enabled_cache = None;
    app.recompute_sparkle();
    {
        let ws = app.windows.get_mut(&WID).expect("window");
        ws.focused = true;
        ws.frame_interval = Some(PANEL);
    }
    if light {
        feed(&app, b"\x1b[?5h");
    }
    feed(&app, b"$ ");
    app
}

/// One printable key as the keyboard sends it.
fn glyph(c: char) -> InputEvent {
    InputEvent::Key {
        key: Key::Character(c),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

/// Dispatches `n` echoed keys `KEY_GAP` apart and returns the last key's instant.
pub(super) fn type_keys(app: &mut App, from: Instant, n: u32) -> Instant {
    let mut at = from;
    for k in 0..n {
        at = from + KEY_GAP * k;
        crate::app_input::pin_input_clock(Some(at));
        let _ = app.input(WID, glyph('a'), Source::Human);
        feed(app, b"a");
        app.redraw_window_at(WID, at);
    }
    crate::app_input::pin_input_clock(None);
    at
}

/// Where the effect lane parked.
pub(super) struct Parked {
    /// The instant of the plan that parked it.
    pub(super) at: Instant,
    /// Deadlines it woke at.
    pub(super) wakes: usize,
    /// Of those, the ones planned while the glow was settling (its settle wakes).
    pub(super) settle_wakes: usize,
    /// Whether the glow was settling at some plan.
    pub(super) saw_settling: bool,
    /// Whether the last wake's redraw reached the surface.
    pub(super) last_presented: bool,
}

/// Runs the terminal effect lane from `from` until no further wake is planned.
pub(super) fn run_lane(app: &mut App, from: Instant) -> Parked {
    let mut now = from;
    let mut parked = Parked {
        at: from,
        wakes: 0,
        settle_wakes: 0,
        saw_settling: false,
        last_presented: false,
    };
    loop {
        let ws = app.windows.get_mut(&WID).expect("window");
        let settling = ws.fx.cursor().momentum.settling();
        parked.saw_settling |= settling;
        let Some(deadline) = ws.plan_terminal_effect_lane(now, false, true) else {
            parked.at = now;
            return parked;
        };
        assert!(deadline > now, "a deadline must advance");
        assert!(
            deadline.saturating_duration_since(from) < HORIZON,
            "the lane never parked: {} wakes",
            parked.wakes
        );
        assert!(
            ws.service_due_terminal_effect_tick(deadline),
            "the armed clock is due at its deadline"
        );
        parked.settle_wakes += usize::from(settling);
        let commits = surface(app).commits();
        app.redraw_window_at(WID, deadline);
        parked.last_presented = surface(app).commits() > commits;
        parked.wakes += 1;
        now = deadline;
    }
}

/// Count of momentum glow quads in the active presentation stream.
pub(super) fn presented_glow(app: &App, light: bool) -> usize {
    let input = &app.windows[&WID].input_scratch;
    if light {
        input.glow_under.len()
    } else {
        input.cursor_glow_add.len()
    }
}

pub(super) fn status(app: &App) -> MomentumAosStatus {
    app.windows[&WID].fx.cursor().momentum.aos_status()
}

/// The exported log since the latest checkpoint, decoded back.
pub(super) fn exported_log(app: &App, epoch: Instant) -> Vec<MomentumEvent> {
    let trace = app.windows[&WID]
        .fx
        .cursor()
        .momentum
        .aos_trace()
        .expect("attached");
    let ancestor = aterm_aos::checkpoint::Checkpoint::decode(&trace.checkpoint)
        .expect("the exported checkpoint decodes")
        .digest();
    let (log, _) = EventLog::decode(
        &MomentumSchema::new(epoch),
        &trace.log,
        &trace.identity,
        &ancestor,
        MOMENTUM_MAX_EVENTS,
    )
    .expect("the exported log decodes");
    log.inputs().to_vec()
}

/// Attaches `DemoStrandedSettle`, types keys, and runs the lane until the glow is stranded.
fn stranded(light: bool, t0: Instant) -> (App, Parked) {
    let mut app = momentum_app(light);
    app.redraw_window_at(WID, t0);
    let (id, attached) = app
        .aos_attach_at(MomentumCode::DemoStrandedSettle, t0)
        .expect("the front window attaches");
    assert_eq!(id, WID);
    assert!(attached.attached && attached.code == "demo-stranded-settle");
    let last_key = type_keys(&mut app, t0 + KEY_GAP, 24);
    assert!(
        status(&app).hot,
        "light={light}: normal key dispatch lit the attached glow"
    );
    let parked = run_lane(&mut app, last_key);
    let ws = &app.windows[&WID];
    let momentum = &ws.fx.cursor().momentum;
    assert!(
        parked.wakes > 0,
        "light={light}: the lane ran the frame train"
    );
    assert!(
        momentum.settling() && status(&app).hot,
        "light={light}: STRANDED — the lane parked while the glow is still hot"
    );
    assert_eq!(ws.next_trail_tick, None, "light={light}: no wake is armed");
    assert!(
        presented_glow(&app, light) > 0,
        "light={light}: the presented frame still carries the halo"
    );
    assert_eq!(
        ws.block_fill.map(|b| b.owner),
        Some(BlockFillOwner::Momentum),
        "light={light}: and the momentum tint on the caret"
    );
    assert_eq!(
        status(&app).stats.refused,
        0,
        "every pinned instant admitted"
    );
    (app, parked)
}

/// Repairs a stranded glow through the host path and verifies clearing and subsequent keys.
#[test]
fn a_stranded_glow_is_repaired_through_the_host_and_fresh_keys_clear_with_no_reset_or_replay() {
    for light in [false, true] {
        let t0 = Instant::now();
        let (mut app, parked) = stranded(light, t0);

        let later = parked.at + Duration::from_millis(500);
        assert_eq!(
            app.windows
                .get_mut(&WID)
                .expect("window")
                .plan_terminal_effect_lane(later, false, true),
            None,
            "light={light}: nothing re-arms a stranded glow"
        );
        let before = status(&app);
        assert_eq!(before.revision, 0);

        let repaired = app
            .aos_repair_at(MomentumCode::Shipped, later)
            .expect("the shipped code repairs the stranded glow");
        assert_eq!(repaired.window, WID);
        assert!(repaired.paused_first, "a running glow is paused first");
        let w = &repaired.commit.witness;
        assert!(
            matches!(
                w.live_verdict(),
                Verdict::Fail {
                    law: HOT_HAS_WAKE,
                    ..
                }
            ),
            "light={light}: the live code fails the corrected contract: {:?}",
            w.live_verdict()
        );
        assert_eq!(w.candidate_verdict(), &Verdict::Pass);
        assert!(w.same_code_reproduced());
        assert_eq!(w.grade(), EvidenceGrade::Tested);
        assert_eq!(w.live_code().name, "momentum-glow.demo-stranded-settle");
        assert_eq!(w.candidate_code().name, "momentum-glow.shipped");
        assert!(
            repaired.commit.revision.0 > before.revision,
            "a new revision"
        );
        assert!(
            repaired.commit.delivered.is_empty(),
            "nothing queued within the call"
        );
        let committed = status(&app);
        assert_eq!(committed.code, "shipped");
        assert_eq!(committed.revision, repaired.commit.revision.0);
        assert!(!committed.paused);
        assert_eq!(committed.stats.publications, 1);
        assert!(
            committed.hot,
            "the accepted state is the glow as it was: still hot"
        );

        let commits = surface(&mut app).commits();
        let cleared = run_lane(&mut app, later);
        assert!(
            cleared.settle_wakes > 0,
            "light={light}: the settling glow armed its wake"
        );
        assert!(
            cleared.last_presented && surface(&mut app).commits() > commits,
            "light={light}: the clearing frame reached the surface"
        );
        let ws = &app.windows[&WID];
        assert_eq!(
            presented_glow(&app, light),
            0,
            "light={light}: the clearing frame carries no halo"
        );
        assert_eq!(ws.input_scratch.cursor_fill_override, None);
        assert_eq!(ws.block_fill, None, "light={light}: no momentum tint");
        let cold = status(&app);
        assert!(!cold.hot && !cold.is_active && !cold.settling);
        let frame = cold.last_frame.expect("a tick was delivered");
        assert!(
            !frame.hot && frame.fill.is_none() && frame.fp == 0 && frame.out + frame.under == 0,
            "light={light}: the last delivered tick is the inert clearing frame"
        );

        let fresh_from = cleared.at + Duration::from_millis(250);
        let last_key = type_keys(&mut app, fresh_from, 12);
        assert!(status(&app).hot, "light={light}: fresh keys heat the glow");
        let commits = surface(&mut app).commits();
        let again = run_lane(&mut app, last_key);
        assert!(
            again.saw_settling && again.settle_wakes > 0,
            "light={light}: it settled and the scheduler woke it"
        );
        assert!(
            again.last_presented && surface(&mut app).commits() > commits,
            "light={light}: the clearing frame was presented again"
        );
        assert_eq!(presented_glow(&app, light), 0);
        assert_eq!(app.windows[&WID].block_fill, None);
        let after = status(&app);
        assert!(!after.hot);
        assert_eq!(after.code, "shipped");
        assert_eq!(after.revision, committed.revision);
        assert_eq!(
            (
                after.stats.replays,
                after.stats.publications,
                after.stats.rejections
            ),
            (
                committed.stats.replays,
                committed.stats.publications,
                committed.stats.rejections
            ),
            "light={light}: no further replay, publication or rejection"
        );
        assert_eq!(after.stats.refused, 0, "every pinned instant admitted");
        let log = exported_log(&app, t0);
        assert!(
            !log.iter().any(|e| matches!(e, MomentumEvent::Reset)),
            "light={light}: no reset recorded since the commit"
        );
        assert_eq!(
            log.iter()
                .filter(|e| matches!(e, MomentumEvent::Key { .. }))
                .count(),
            12,
            "light={light}: each fresh key reached the published pair once"
        );

        let trace = app.aos_trace_now().expect("attached");
        assert_eq!(trace.log_len, log.len());
        assert_eq!(trace.log_len, after.log_len);
        assert_eq!(trace.broken, None);
        assert!(trace.trace.is_some());
        assert_eq!(
            Some(trace.checkpoint.short()),
            after.checkpoint,
            "the summary's checkpoint is the live ancestor"
        );
    }
}

/// A refused repair undo-resumes a call-initiated pause while preserving a caller's pause.
#[test]
fn a_refused_repair_resumes_the_glow_it_paused_and_keeps_a_callers_pause() {
    let t0 = Instant::now();
    let (mut app, parked) = stranded(false, t0);
    let demo = MomentumCode::DemoStrandedSettle;
    let refused = app.aos_repair_at(demo, parked.at).map(|_| ());
    assert!(
        matches!(
            &refused,
            Err(AosRefusal::Publish(PublishRefusal::CandidateFailed(
                Verdict::Fail {
                    law: HOT_HAS_WAKE,
                    ..
                }
            )))
        ),
        "the fault fails its own corrected contract: {refused:?}"
    );
    let s = status(&app);
    assert!(!s.paused, "the pause this call took is undone");
    assert_eq!((s.code, s.revision), ("demo-stranded-settle", 0));
    assert_eq!(s.stats.rejections, 1);

    let (_, head) = app.aos_pause_at(parked.at).expect("a running glow pauses");
    assert!(matches!(
        app.aos_repair_at(demo, parked.at),
        Err(AosRefusal::Publish(PublishRefusal::CandidateFailed(_)))
    ));
    let s = status(&app);
    assert!(s.paused, "a caller's pause holds");
    assert_eq!(s.input_cursor, head.input_cursor);
    let resumed = app.aos_resume_now().expect("paused");
    assert!(resumed.delivered.is_empty());
    assert!(!status(&app).paused);
}

/// Invalid lifecycle transitions return their exact refusal without mutating state.
#[test]
fn each_aos_ask_names_its_refusal_and_changes_nothing() {
    let t0 = Instant::now();
    let mut app = momentum_app(false);
    app.redraw_window_at(WID, t0);
    let not_attached = AosRefusal::Publish(PublishRefusal::NotAttached);
    assert_eq!(app.aos_pause_at(t0).err(), Some(not_attached.clone()));
    assert_eq!(
        app.aos_repair_at(MomentumCode::Shipped, t0)
            .map(|_| ())
            .err(),
        Some(not_attached.clone())
    );
    assert_eq!(
        app.aos_resume_now().map(|_| ()).err(),
        Some(not_attached.clone())
    );
    assert_eq!(app.aos_detach_now().err(), Some(not_attached));
    assert_eq!(
        app.aos_trace_now().err(),
        Some(AosRefusal::Trace(TraceRefusal::NotAttached))
    );
    assert!(!status(&app).attached, "the refusals attached nothing");

    app.aos_attach_at(MomentumCode::Shipped, t0)
        .expect("attaches");
    assert_eq!(
        app.aos_resume_now().map(|_| ()).err(),
        Some(AosRefusal::Publish(PublishRefusal::NotPaused))
    );
    app.aos_pause_at(t0).expect("pauses");
    assert_eq!(
        app.aos_pause_at(t0).err(),
        Some(AosRefusal::Publish(PublishRefusal::AlreadyPaused))
    );
    let (_, detached) = app.aos_detach_now().expect("detaches");
    assert!(!detached.attached && !detached.paused);

    app.frontmost_window = None;
    assert_eq!(app.aos_status_now().err(), Some(AosRefusal::NoWindow));
}

/// `control_aos` formats each lifecycle operation and advances `log_epoch` on re-attach.
#[test]
fn the_wake_handler_answers_each_ask_as_its_line() {
    use crate::aos_verb::Asked;
    let t0 = Instant::now();
    let mut app = momentum_app(false);
    app.redraw_window_at(WID, t0);
    let line = |app: &mut App, asked: Asked| app.control_aos(asked, t0).expect("answers");
    assert!(line(&mut app, Asked::Status).starts_with("window=0 attached=false code=shipped "));
    let attached = line(&mut app, Asked::Attach(MomentumCode::Shipped));
    assert!(attached.starts_with("window=0 attached=true code=shipped "));
    assert!(attached.contains(" log_epoch=1 "), "{attached}");
    assert!(line(&mut app, Asked::Pause).starts_with("window=0 paused rev=0 "));
    let repaired = line(&mut app, Asked::Repair(MomentumCode::Shipped));
    assert!(
        repaired
            .starts_with("window=0 repaired paused_first=false rev=1 code=momentum-glow.shipped "),
        "{repaired}"
    );
    assert!(repaired.contains(" grade=tested "), "{repaired}");
    assert!(line(&mut app, Asked::Trace).starts_with("window=0 log="));
    assert!(line(&mut app, Asked::Detach).starts_with("window=0 attached=false "));
    let again = line(&mut app, Asked::Attach(MomentumCode::Shipped));
    assert!(again.contains(" log_epoch=2 "), "{again}");
    assert!(
        app.control_aos(Asked::Resume, t0).is_err(),
        "a running glow has no update to reject"
    );
}

/// `TraceHex` appends a valid, replayable hex trace bundle to the `Trace` summary line.
#[test]
fn the_trace_hex_ask_exports_the_trace_file_its_summary_counts() {
    use aterm_effects::aos_momentum::MomentumTrace;

    use crate::aos_verb::Asked;
    let t0 = Instant::now();
    let mut app = momentum_app(false);
    app.redraw_window_at(WID, t0);
    app.aos_attach_at(MomentumCode::Shipped, t0)
        .expect("attaches");
    let last = type_keys(&mut app, t0 + KEY_GAP, 6);
    let summary = app.control_aos(Asked::Trace, last).expect("answers");
    let line = app.control_aos(Asked::TraceHex, last).expect("answers");
    let rest = line
        .strip_prefix(summary.as_str())
        .unwrap_or_else(|| panic!("the trace line comes first: {line}"));
    let Some((len, hex)) = rest
        .strip_prefix(" file_bytes=")
        .and_then(|r| r.split_once(" hex="))
    else {
        panic!("file_bytes= then hex=: {rest}");
    };
    let file = aterm_codec::hex::decode_vec(hex.as_bytes()).expect("lowercase hex");
    assert_eq!(len.parse::<usize>(), Ok(file.len()));
    let trace = MomentumTrace::decode(&file).expect("a sealed momentum trace file");
    let counts = trace.read(t0).expect("its log reads whole").counts;
    assert_eq!(counts.keys, 6, "each key once");
    assert!(
        summary.ends_with(&format!(
            " sealed=true keys={} resets={} ticks={} queries={}",
            counts.keys, counts.resets, counts.ticks, counts.queries
        )),
        "the summary counts the exported log: {summary}"
    );
    let build = status(&app).build.expect("attached");
    let replayed = trace
        .replay(t0, &build)
        .expect("the file replays to the live head");
    assert_eq!(replayed.code, MomentumCode::Shipped);
    assert_eq!(replayed.counts, counts);

    app.aos_detach_now().expect("detaches");
    assert_eq!(
        app.control_aos(Asked::TraceHex, last),
        app.control_aos(Asked::Trace, last)
    );
    assert!(app.control_aos(Asked::TraceHex, last).is_err());
}

/// Turning on serious mode or disabling the glow suppresses a paused glow's held overlay.
#[test]
fn a_paused_glow_is_not_drawn_once_serious_mode_or_its_switch_turns_it_off() {
    type Turn = fn(&mut App);
    let serious: Turn = |app| {
        app.config.serious_mode = Some(true);
        app.apply_serious_mode(true);
    };
    let switch_off: Turn = |app| app.config.cursor_momentum_glow = Some(false);
    for (why, turn_off) in [
        ("serious mode on", serious),
        ("the glow's switch off", switch_off),
    ] {
        for light in [false, true] {
            let t0 = Instant::now();
            let mut app = momentum_app(light);
            app.redraw_window_at(WID, t0);
            app.aos_attach_at(MomentumCode::Shipped, t0)
                .expect("the front window attaches");
            let last = type_keys(&mut app, t0 + KEY_GAP, 12);
            assert!(status(&app).hot, "fixture: the glow is hot");
            app.aos_pause_at(last).expect("a running glow pauses");
            app.redraw_window_at(WID, last + Duration::from_millis(10));
            assert!(
                presented_glow(&app, light) > 0,
                "{why}, light={light}: fixture: the held overlay shows while paused"
            );
            assert_eq!(
                app.windows[&WID].block_fill.map(|b| b.owner),
                Some(BlockFillOwner::Momentum),
                "{why}, light={light}: fixture: the caret wears the held tint"
            );
            let logged = exported_log(&app, t0).len();
            turn_off(&mut app);
            app.recompute_sparkle();
            app.redraw_window_at(WID, last + Duration::from_millis(20));
            let ws = &app.windows[&WID];
            assert_eq!(
                (
                    ws.input_scratch.cursor_glow_add.len(),
                    ws.input_scratch.glow_under.len()
                ),
                (0, 0),
                "{why}, light={light}: no held light"
            );
            assert_eq!(
                ws.block_fill, None,
                "{why}, light={light}: no held caret fill"
            );
            assert!(
                status(&app).paused,
                "{why}, light={light}: the pause is kept"
            );
            assert_eq!(exported_log(&app, t0).len(), logged, "nothing is recorded");
        }
    }
}
