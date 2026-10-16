// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::late_park_record_tests::prelaunch_record;
use super::{
    PRELAUNCH_HOLD_MAX, ParkAttempt, ParkGate, ParkGateFacts, VIDEO_PARK_WAIT_MAX,
    prelaunch_hold_cap, prelaunch_park_admitted, recording_park_refusal,
};
use crate::native_update_auto_intent::{ActivityFacts, ApplyPhase};
use crate::native_updater_service::ApplyMode;

/// A machine idle in every sense, with a take running.
fn recording(mode: ApplyMode, phase: ApplyPhase, held_for: std::time::Duration) -> ParkGate {
    prelaunch_park_admitted(
        ParkGateFacts {
            mode,
            phase,
            activity: ActivityFacts {
                quiet: true,
                hands_off_keys: true,
                output_quiet: true,
                focused: true,
                consent_warmup: false,
                harness_restored_pending: false,
            },
            masters_quiet: true,
            masters_alive: true,
            land_waits: 0,
            land_gate_relaxed: false,
            held_for,
            recording: true,
        },
        prelaunch_hold_cap(mode),
    )
}

/// The wait is typed and bounded twice over: `Land` and the verb's own
/// maximum take both end it, and an explicit apply never waits at all.
#[test]
fn a_live_take_holds_the_automatic_park_before_land_and_only_until_its_bound() {
    let early = std::time::Duration::from_secs(1);
    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        for phase in [
            ApplyPhase::PreferIdle,
            ApplyPhase::PreferOutputGap,
            ApplyPhase::KeysOnly,
        ] {
            assert_eq!(
                recording(mode, phase, early),
                ParkGate::Wait("a video recording is running"),
                "{mode:?} {phase:?}: a take that can still finish is waited for"
            );
            assert_eq!(
                recording(mode, phase, VIDEO_PARK_WAIT_MAX),
                ParkGate::Park,
                "{mode:?} {phase:?}: past the longest take the park proceeds"
            );
        }
        assert_eq!(
            recording(mode, ApplyPhase::Land, early),
            ParkGate::Park,
            "{mode:?}: Land is not held by a take"
        );
    }
    for phase in [ApplyPhase::PreferIdle, ApplyPhase::Land] {
        assert_eq!(
            recording(ApplyMode::Immediate, phase, early),
            ParkGate::Park,
            "an explicit apply parks at once"
        );
    }
    // The bound comes due inside the hold: the take never stands the
    // successor down.
    assert!(VIDEO_PARK_WAIT_MAX < PRELAUNCH_HOLD_MAX);
    // CONTROL: no take, no wait.
    assert_eq!(
        recording_park_refusal(ApplyMode::Automatic, ApplyPhase::PreferIdle, false, early),
        None
    );
}

/// A take running when the successor's park lands is answered at Commit, not at
/// the park: in between the attempt can still fail and roll back with this
/// process running (`rollback_overlap`), and the take must then still be running
/// with its client told nothing. Only the Commit arm calls
/// `App::video_answer_before_commit`.
#[test]
fn a_landed_park_keeps_a_live_take_until_commit_answers_it() {
    // The park records its landed cost in the update ledger (`keep_landed_park`);
    // held, as every ledger test does, so the dry-run seed tests never read it.
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    // Session 0 on a real PTY, so the park's descriptor reservation and
    // proof term see a live master.
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .master = master;

    let root = std::env::temp_dir().join(format!(
        "aterm-video-park-{}-{}",
        std::process::id(),
        crate::metrics::now_us()
    ));
    let _ = std::fs::remove_dir_all(&root);
    crate::control_auth::ensure_private_dir(&root).expect("private recording root");
    let dir = crate::control_auth::confine_video_dir(&root).expect("confined recording dir");
    let (reply, replies) = std::sync::mpsc::channel();
    app.video_rec = Some(crate::VideoRec {
        window: crate::WindowId(0),
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
        started_us: 0,
        keys: false,
        key_log: Vec::new(),
        trail: false,
        trail_log: Vec::new(),
        trail_seen: 0,
        trail_lost: 0,
        pace_ticks: Vec::new(),
        mode: crate::VideoMode::OffscreenPresentReal,
        next_frame: None,
        presented: None,
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
        dir,
        handoff: None,
        cancel: crate::VideoCancellation::new(),
        reply,
    });
    assert!(app.video_request_live());

    // An explicit apply's launched lane: the gate parks at once.
    let (record, _cancelled, _stood_down, _transferred) = prelaunch_record(7, ApplyMode::Immediate);
    app.update_handoff_prelaunch = Some(record);
    let parked = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
    assert!(
        matches!(parked, ParkAttempt::Parked),
        "PRECONDITION: the park landed ({parked:?})"
    );
    assert!(
        app.video_rec.is_some() && replies.try_recv().is_err(),
        "a landed park leaves the take running and its client untold"
    );

    // The proof fails: the attempt rolls back and the process runs on.
    let live = app.handoff_live_sessions();
    app.rollback_overlap(None, &live);
    app.pending_update_handoff = None;
    app.update_handoff_prelaunch = None;
    assert!(
        app.video_rec.is_some(),
        "an update that did not happen leaves the take running"
    );
    assert!(
        replies.try_recv().is_err(),
        "and its client is told nothing"
    );

    // CONTROL: a park that lands and reaches Commit answers it there.
    let (record, _cancelled, _stood_down, _transferred) =
        prelaunch_record(71, ApplyMode::Immediate);
    app.update_handoff_prelaunch = Some(record);
    let parked = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
    assert!(
        matches!(parked, ParkAttempt::Parked),
        "PRECONDITION: the re-park landed ({parked:?})"
    );
    assert!(replies.try_recv().is_err(), "still untold at the park");
    let answered = app.video_answer_before_commit(super::VIDEO_EXPORT_COMMIT_WAIT);
    assert_eq!(
        answered,
        crate::VideoCommitAnswer {
            answered: true,
            export_settled: true,
            unwritten: 0,
        }
    );
    let (body, _retention) = replies
        .try_recv()
        .expect("Commit answered the take")
        .into_parts();
    assert_eq!(body, "ERR video: recording aborted: aterm is updating\n");
    assert!(app.video_rec.is_none(), "the take is gone");
    assert!(!app.video_request_live());

    app.pending_update_handoff = None;
    app.update_handoff_prelaunch = None;
    drop(app);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
    let _ = std::fs::remove_dir_all(root);
}

/// Round eight of the update audit, leads U60 and U40 (r8d #759, #739): THE
/// RIM STARTS WHERE THE TERMINAL FREEZES. It is the one explanation of a frozen
/// terminal, and it started at apply begin: on the launched lane that is before
/// the successor's boot, its dial and the park gate's hold (up to two minutes),
/// so the rim pulsed around a terminal that still echoed, and its 125 s cap could
/// run out before the park, leaving the freeze with no notice. Now apply begin
/// shows none, and the launched lane's park starts it, with its cap counted from
/// the freeze it explains.
#[test]
fn the_upgrade_rim_starts_at_the_launched_lanes_park() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .master = master;
    // Apply begins: the automatic lane adds no row, and nothing is frozen.
    app.begin_update_installing(false);
    let (record, _cancelled, _stood_down, _transferred) = prelaunch_record(7, ApplyMode::Immediate);
    app.update_handoff_prelaunch = Some(record);
    assert!(
        app.level_up.is_none(),
        "no upgrade rim over a terminal that still echoes"
    );
    let parked = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
    assert!(
        matches!(parked, ParkAttempt::Parked),
        "PRECONDITION: the park landed ({parked:?})"
    );
    let rim = app
        .level_up
        .as_ref()
        .expect("the park starts the rim that explains the freeze");
    // Its cap outlives everything after the park: the explicit apply's proof
    // deadline, the longest of the lanes', with room for the capture.
    let proof_deadline =
        super::handoff_proof_deadline(ApplyMode::Immediate, 0, std::time::Instant::now());
    assert!(!rim.is_expired(proof_deadline + std::time::Duration::from_secs(30)));

    app.pending_update_handoff = None;
    app.update_handoff_prelaunch = None;
    drop(app);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
}

/// The review of round eight's leads U60 and U40 (r8d #759, #739): A RE-PARK
/// COUNTS THE RIM'S CAP AGAIN. A park that missed its budget left its rim
/// running, and the re-park kept it as it was: one first missed early and
/// admitted late (the gate's hold runs to two minutes) froze the terminal on a
/// cap counted from the first park, which the proof after it could outrun. The
/// rim a missed park left — here 120 s old, its 125 s cap nearly spent — runs
/// on at the re-park, continuous, its cap counted from this freeze.
#[test]
fn a_re_park_counts_the_rims_cap_from_its_own_freeze() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .master = master;
    app.begin_update_installing(false);
    let (record, _cancelled, _stood_down, _transferred) = prelaunch_record(7, ApplyMode::Immediate);
    app.update_handoff_prelaunch = Some(record);
    let first_park = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(120))
        .expect("the machine has been up two minutes");
    app.level_up = Some(crate::level_up::LevelUp::charging(7, first_park, 1.0));

    let parked = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
    assert!(
        matches!(parked, ParkAttempt::Parked),
        "PRECONDITION: the re-park landed ({parked:?})"
    );
    let rim = app.level_up.as_ref().expect("the rim runs on");
    let proof_deadline =
        super::handoff_proof_deadline(ApplyMode::Immediate, 0, std::time::Instant::now());
    assert!(
        !rim.is_expired(proof_deadline + std::time::Duration::from_secs(30)),
        "its cap counts from the freeze it explains, not from the park that missed"
    );

    app.pending_update_handoff = None;
    app.update_handoff_prelaunch = None;
    drop(app);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
}

/// The review of round eight's leads U60 and U40 (r8d #759, #739): NO RIM
/// WHILE THE GATE WAITS. A missed park resumes the readers and sends the
/// launched lane back through its gate, which can wait — output pending, a
/// person typing — until the hold's cap, two minutes, with the terminal
/// echoing; the rim the missed park left pulsed around it all that time (the
/// U60 symptom). The gate's wait takes it away, and the next park it admits
/// starts it again. CONTROL: the first try, before any park, has none to take.
#[test]
fn a_gate_wait_after_a_missed_park_takes_the_rim_away() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .master = master;
    // Output its reader has not taken: an automatic park waits for it.
    let pending = b"still printing\r\n";
    // SAFETY: `slave` is the open slave end of the pair above; the buffer is
    // a live byte slice of the length passed.
    let wrote = unsafe { libc::write(slave, pending.as_ptr().cast(), pending.len()) };
    assert_eq!(
        usize::try_from(wrote).ok(),
        Some(pending.len()),
        "PRECONDITION: output pending"
    );
    let (record, _cancelled, _stood_down, _transferred) = prelaunch_record(7, ApplyMode::Automatic);
    app.update_handoff_prelaunch = Some(record);
    // The rim a park that missed its budget left running.
    app.level_up = Some(crate::level_up::LevelUp::charging(
        7,
        std::time::Instant::now(),
        1.0,
    ));

    let waited = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
    assert!(
        matches!(waited, ParkAttempt::NotYet(_)),
        "PRECONDITION: the gate holds the park back ({waited:?})"
    );
    assert!(
        app.level_up.is_none(),
        "no rim around a terminal that still echoes"
    );
    assert!(app.pending_update_handoff.is_none(), "nothing parked");

    app.update_handoff_prelaunch = None;
    drop(app);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
}

/// A COMMIT THE WORKER TOOK FIRST NEVER ANSWERS A LIVE TAKE (the review
/// of round eight's finding 18). The Commit's messages flush runs before
/// the claim, under the dispatch fence; the video answer stays past
/// `try_begin_commit` (round six, finding 22), because everything before
/// it can still roll the attempt back with this process running. Here the
/// admission holds — the flush ran — and the worker has already moved the
/// attempt to Rejecting: the take must still be running, its client untold.
///
/// RED before the fix: the Commit's waits, the video answer among them,
/// ran before the claim, and the rollback after them lost the take.
#[test]
fn a_commit_the_worker_took_first_never_answers_a_live_take() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .master = master;
    let (root, replies) = live_take(&mut app);
    let (record, _cancelled, _stood_down, _transferred) = prelaunch_record(7, ApplyMode::Immediate);
    let nonce = record.nonce.clone();
    app.update_handoff_prelaunch = Some(record);
    let parked = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
    assert!(
        matches!(parked, ParkAttempt::Parked),
        "PRECONDITION: the park landed ({parked:?})"
    );
    let pending = app
        .pending_update_handoff
        .as_mut()
        .expect("the parked attempt");
    // The dispatch fence has had its loop time.
    pending.commit_drain_started =
        Some(std::time::Instant::now() - std::time::Duration::from_millis(100));
    assert!(
        pending.arbiter.try_begin_reject(),
        "the worker takes the attempt"
    );
    let (_commit_rd, commit_wr) = std::io::pipe().expect("a commit pipe");
    let (reject, rejected) = std::sync::mpsc::sync_channel(1);
    app.finish_proven_handoff(crate::UpdateHandoffCompletion {
        attempt_id: 7,
        nonce: Some(nonce),
        child_pid: None,
        outcome: crate::UpdateHandoffOutcome::ProofReady,
        commit_fd: Some(commit_wr.into()),
        reject: Some(reject),
        reconcile: None,
        detail: String::new(),
        input_drain_spins: 1,
        child_death: crate::ChildDeathEvidence::Unobserved,
    });
    assert_eq!(
        app.pending_update_handoff
            .as_ref()
            .and_then(|pending| pending.commit_flush),
        Some(true),
        "PRECONDITION: the admission held, so the flush before the claim ran"
    );
    assert!(app.video_rec.is_some(), "the take is still running");
    assert!(
        replies.try_recv().is_err(),
        "and its client is told nothing"
    );
    assert!(rejected.try_recv().is_ok(), "the worker is told to reject");

    app.pending_update_handoff = None;
    app.update_handoff_prelaunch = None;
    drop(app);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
    let _ = std::fs::remove_dir_all(root);
}

/// A COMMIT WHOSE ANSWERS TOOK TIME HOLDS ITS CLAIM THROUGH THE FENCE (round
/// eight of the update audit, #18). A live `video` request is answered only
/// under the claim (round six, #22), and its answer can block the main
/// thread — here a reply its client takes 50 ms to read; a key the OS queued
/// to this process meanwhile died with the `_exit` when the successor had not
/// taken the keyboard. The claim's pass now answers the take and keeps the
/// claim (the worker cannot reject a claimed attempt): no Commit is written,
/// and the attempt goes back through the drain gate, whose re-post lets the
/// run loop dispatch that input into the masters. Its next pass, once the
/// fence has had its loop time, reads the admission again and writes the
/// Commit (the test's pipe has no reader, so the write fails as an EPIPE
/// does and rolls back — never an activity refusal). A pass whose admission
/// now refuses — a quit pressed during the fence — gives the claim to the
/// rejection, as activity, with the take still answered. A row the park's
/// carry left is closed only at the write (ruling 441 (d)): the refused pass
/// keeps running with it open, its reporter's id still live.
///
/// The derived machine says the same (`NativeUpdateOverlapHandoff`): input
/// queued under the claim disables `CommitModern` until it drains.
///
/// RED before the fix: the claim's pass wrote the Commit at once after the
/// answer, re-posting nothing.
#[test]
fn a_commit_whose_answers_took_time_holds_its_claim_through_the_fence() {
    let model = aterm_spec::derive::native_update_overlap_handoff_model();
    let mut state = model.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildPaintsExactProof",
        "MainWinsCommitArbiter",
        "QueueHardwareInput",
    ] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert!(
        model.successors("CommitModern", &state).is_empty(),
        "the machine writes no Commit over input queued under the claim"
    );

    for quit_during_the_fence in [false, true] {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = crate::App::headless_for_test();
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .master = master;
        let (root, replies) = live_take(&mut app);
        // The take's client reads its answer 50 ms after it is SENT: it
        // waits for the answer first, so the 50 ms is always time the
        // claim's pass spent answering, however late the main thread
        // got there (begun when the thread is spawned instead, a machine at
        // load ~20 reaches the answer after the 50 ms has passed: the pass
        // takes under the 2 ms yield and writes the Commit at once).
        let (wire, release) = crate::control::ReplyWire::new();
        app.video_reply_wires.push(wire);
        let reader = std::thread::spawn(move || {
            let answer = replies.recv().expect("the claim answered the take");
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(release);
            answer
        });
        let (record, _cancelled, _stood_down, _transferred) =
            prelaunch_record(7, ApplyMode::Immediate);
        let nonce = record.nonce.clone();
        app.update_handoff_prelaunch = Some(record);
        // A row the park's carry leaves (in flight, not yet revealed), which
        // ends with this process at its Commit (ruling 441 (d)).
        let uncarried = app.post_message(
            aterm_messages::Message::new(
                aterm_messages::tags::PACKAGES,
                aterm_messages::Severity::Info,
                "Checking ALab tools",
            )
            .in_flight()
            .key("toolchain.pass")
            .hold(aterm_messages::Hold::Live {
                stale_after: aterm_messages::STALE_TAILED,
            })
            .reveal_after(aterm_messages::PROGRESS_GRACE),
        );
        let parked = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
        assert!(
            matches!(parked, ParkAttempt::Parked),
            "PRECONDITION: the park landed ({parked:?})"
        );
        assert!(
            !app.handoff_carried_messages.contains(&uncarried.raw()),
            "PRECONDITION: the carry left the unrevealed row"
        );
        let pending = app
            .pending_update_handoff
            .as_mut()
            .expect("the parked attempt");
        pending.commit_drain_started =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(100));
        let arbiter = pending.arbiter.clone();
        app.handoff_reposts = Some(Vec::new());
        // No reader: a Commit written fails with EPIPE, and the test runs on.
        let (commit_rd, commit_wr) = std::io::pipe().expect("a commit pipe");
        drop(commit_rd);
        let (reject, rejected) = std::sync::mpsc::sync_channel(1);
        app.finish_proven_handoff(crate::UpdateHandoffCompletion {
            attempt_id: 7,
            nonce: Some(nonce),
            child_pid: None,
            outcome: crate::UpdateHandoffOutcome::ProofReady,
            commit_fd: Some(commit_wr.into()),
            reject: Some(reject),
            reconcile: None,
            detail: String::new(),
            input_drain_spins: 1,
            child_death: crate::ChildDeathEvidence::Unobserved,
        });
        let (body, _retention) = reader
            .join()
            .expect("the client read its answer")
            .into_parts();
        assert_eq!(body, "ERR video: recording aborted: aterm is updating\n");
        assert_eq!(
            arbiter.phase(),
            crate::HandoffAttemptPhase::Committing,
            "the claim is held"
        );
        assert!(
            rejected.try_recv().is_err(),
            "nothing was rejected: no Commit was written"
        );
        assert!(
            app.messages.live(uncarried).is_some(),
            "the claim's answers close no row: the held pass may still refuse"
        );
        let pending = app.pending_update_handoff.as_mut().expect("the attempt");
        assert!(
            pending.held_commit.is_some(),
            "what the claim holds is kept"
        );
        let mut reposted = app.handoff_reposts.replace(Vec::new()).expect("re-posts");
        assert_eq!(reposted.len(), 1, "the attempt went back through the gate");
        let respin = reposted.remove(0);

        // The run loop dispatched; the fence has had its loop time.
        let pending = app.pending_update_handoff.as_mut().expect("the attempt");
        pending.commit_drain_started =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(100));
        if quit_during_the_fence {
            pending.teardown = crate::DeferredHandoffTeardown::QuitRequested;
        }
        app.finish_proven_handoff(respin);
        assert_eq!(
            arbiter.phase(),
            crate::HandoffAttemptPhase::Rejecting,
            "the claim went to the rejection"
        );
        assert!(rejected.try_recv().is_ok(), "the worker is told to reject");
        let pending = app.pending_update_handoff.as_ref().expect("the attempt");
        assert!(pending.held_commit.is_none());
        assert_eq!(
            pending.revoked_by_activity, quit_during_the_fence,
            "refused as activity only for the quit; otherwise the Commit was written \
             and its write failed"
        );
        assert_eq!(
            app.handoff_rejections,
            [if quit_during_the_fence {
                "destructive intent revoked Commit before teardown replay"
            } else {
                "attempt-bound Commit pipe closed before its atomic write"
            }],
            "the admission refused the quit; every other held pass wrote its Commit"
        );
        assert!(
            app.handoff_reposts.as_ref().is_some_and(Vec::is_empty),
            "the held pass committed or refused, never re-posted again"
        );
        assert_eq!(
            app.messages.live(uncarried).is_some(),
            quit_during_the_fence,
            "a held pass the admission refused keeps running with the row its \
             reporter still holds; one that reached its write closed it there"
        );

        app.pending_update_handoff = None;
        app.update_handoff_prelaunch = None;
        drop(app);
        aterm_pty::close_fd(slave);
        aterm_pty::close_fd(master);
        let _ = std::fs::remove_dir_all(root);
    }
}

/// A HELD CLAIM IS NOT REFUSED BY THE SUPERVISOR'S OWN STOP (the review of
/// round eight's #18). A supervised tab waits at a question box the
/// in-window supervisor escalated: its badge (`meta set attention
/// owner=supervisor …`) stands, and the park draws it into the manifest.
/// The Commit's answers took real time, so its claim is held through one
/// more input-dispatch fence, and the pass after it reads the admission
/// again — the escalations among it (`carried_claims_held`). A stopped
/// worker's last act clears the badges it raised (`release_attention`, on
/// a fresh connection the cut allows). The host used to stop at the claim,
/// so the held pass read that clearing as an escalation moved since the
/// park and refused the Commit as activity, its successor killed — at every
/// retry of the automatic lane while the box stood. The host now stops at
/// the write, after the admission's last reading: the held pass writes the
/// Commit (the test's pipe has no reader, so the write fails as an EPIPE
/// does), and the host is stopped there, then resumed by the failure.
///
/// The worker is the test's own: it parks until stopped, and its last act
/// clears the badge, as `release_attention` does; when a stop reaches it
/// before the held pass, the test waits for that clearing to land (the
/// real one lands in milliseconds, inside the fence's 30 ms at least).
///
/// RED before the fix: the claim's pass stopped the host, and the held
/// pass refused the Commit ("structural activity arrived before Commit").
#[test]
fn a_held_claim_is_not_refused_by_the_supervisors_own_stop() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .master = master;
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let sid = ctx.self_id.as_str().to_string();
    // The supervisor's escalation of the box, standing at the park.
    let owner = crate::harness_host::ATTENTION_OWNER;
    ctx.meta
        .lock()
        .expect("the meta")
        .set_attention_owned(owner, Some("claude question (in tab 1)".to_string()))
        .expect("room for the badge");
    let (stopped, cleared, started) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(0)),
    );
    let body = {
        let (stopped, cleared, started, ctx) = (
            Arc::clone(&stopped),
            Arc::clone(&cleared),
            Arc::clone(&started),
            Arc::clone(&ctx),
        );
        Arc::new(move |job: &crate::harness_host::WorkerJob| {
            let me = std::thread::current();
            let cut = Arc::clone(&stopped);
            *job.interrupt.lock().expect("the interrupter") = Some(Box::new(move || {
                cut.store(true, Ordering::SeqCst);
                me.unpark();
            }));
            started.fetch_add(1, Ordering::SeqCst);
            while !job.stop.load(Ordering::SeqCst) {
                std::thread::park();
            }
            // The loop's last act: the badge it raised goes.
            let _ = ctx
                .meta
                .lock()
                .expect("the meta")
                .set_attention_owned(owner, None);
            cleared.store(true, Ordering::SeqCst);
            crate::harness_host::BodyEnd::Stopped
        })
    };
    let host = crate::harness_host::HostHandle::for_supervised_test(vec![sid], body);
    app.harness = Some(host.clone());
    let until = |what: &str, pred: &dyn Fn() -> bool| {
        let seen = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(5),
            || pred().then_some(()),
        );
        assert!(seen.is_some(), "not within the hang detector: {what}");
    };
    until("the tab's worker runs", &|| {
        started.load(Ordering::SeqCst) >= 1
    });

    let (root, replies) = live_take(&mut app);
    // The take's client reads its answer a while after the Commit is written: the
    // Commit's answers take real time, and its claim is held. The wire is made here
    // and its reader started right before the Commit (below), so its clock cannot run
    // out while a starved main thread is still setting the park up.
    let (wire, release) = crate::control::ReplyWire::new();
    app.video_reply_wires.push(wire);
    let (record, _cancelled, _stood_down, _transferred) = prelaunch_record(7, ApplyMode::Immediate);
    let nonce = record.nonce.clone();
    app.update_handoff_prelaunch = Some(record);
    let parked = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
    assert!(
        matches!(parked, ParkAttempt::Parked),
        "PRECONDITION: the park landed ({parked:?})"
    );
    let drawn = app
        .pending_update_handoff
        .as_ref()
        .expect("the parked attempt")
        .carried_claims
        .clone();
    assert!(
        !ctx.meta
            .lock()
            .expect("the meta")
            .handoff_attention_owners()
            .is_empty()
            && app.carried_claims_held(&drawn),
        "PRECONDITION: the park drew the standing escalation"
    );
    let pending = app
        .pending_update_handoff
        .as_mut()
        .expect("the parked attempt");
    pending.commit_drain_started =
        Some(std::time::Instant::now() - std::time::Duration::from_millis(100));
    let arbiter = pending.arbiter.clone();
    app.handoff_reposts = Some(Vec::new());
    // No reader: a Commit written fails with EPIPE, and the test runs on.
    let (commit_rd, commit_wr) = std::io::pipe().expect("a commit pipe");
    drop(commit_rd);
    let (reject, rejected) = std::sync::mpsc::sync_channel(1);
    // The claim counts as held when its answers took `HANDOFF_INPUT_DRAIN_YIELD`
    // (2 ms); 250 ms leaves a loaded machine room to reach the wait first.
    let reader = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(250));
        drop(release);
    });
    app.finish_proven_handoff(crate::UpdateHandoffCompletion {
        attempt_id: 7,
        nonce: Some(nonce),
        child_pid: None,
        outcome: crate::UpdateHandoffOutcome::ProofReady,
        commit_fd: Some(commit_wr.into()),
        reject: Some(reject),
        reconcile: None,
        detail: String::new(),
        input_drain_spins: 1,
        child_death: crate::ChildDeathEvidence::Unobserved,
    });
    reader.join().expect("the client read its answer");
    assert!(replies.try_recv().is_ok(), "the claim answered the take");
    assert!(
        app.pending_update_handoff
            .as_ref()
            .is_some_and(|pending| pending.held_commit.is_some()),
        "PRECONDITION: the claim is held through one more fence"
    );
    // A stop that reached the worker by now has its last act land before
    // the held pass reads the admission, as the real one's does.
    let stopped_at_the_claim = stopped.load(Ordering::SeqCst);
    if stopped_at_the_claim {
        until("the stopped worker's last act", &|| {
            cleared.load(Ordering::SeqCst)
        });
    }
    let mut reposted = app.handoff_reposts.replace(Vec::new()).expect("re-posts");
    assert_eq!(reposted.len(), 1, "the attempt went back through the gate");
    let respin = reposted.remove(0);

    // The run loop dispatched; the fence has had its loop time.
    let pending = app.pending_update_handoff.as_mut().expect("the attempt");
    pending.commit_drain_started =
        Some(std::time::Instant::now() - std::time::Duration::from_millis(100));
    app.finish_proven_handoff(respin);
    assert_eq!(
        app.handoff_rejections,
        ["attempt-bound Commit pipe closed before its atomic write"],
        "the held pass wrote the Commit; its own stop of the supervisor moved nothing \
         it read"
    );
    assert!(
        !app.pending_update_handoff
            .as_ref()
            .expect("the attempt")
            .revoked_by_activity,
        "never refused as activity"
    );
    assert_eq!(arbiter.phase(), crate::HandoffAttemptPhase::Rejecting);
    assert!(rejected.try_recv().is_ok(), "the worker is told to reject");
    assert!(
        !stopped_at_the_claim,
        "the workers ran through the fence, as before any claim"
    );
    assert!(
        stopped.load(Ordering::SeqCst),
        "the host was stopped at the write"
    );
    until("the failed Commit resumes the host", &|| {
        started.load(Ordering::SeqCst) >= 2
    });

    host.shutdown_and_join();
    app.harness = None;
    app.pending_update_handoff = None;
    app.update_handoff_prelaunch = None;
    drop(app);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
    let _ = std::fs::remove_dir_all(root);
}

/// THE SUCCESSOR TAKES THE FRONT AT THE WRITE, NEVER AT A HELD CLAIM (the
/// second review of round eight's #18). The Commit's answers took real time
/// (a `video` reply its client reads 50 ms late), so its claim is held
/// through one more input-dispatch fence. The successor used to be
/// activated at the claim, before that fence: the keys the person typed
/// through it went to the successor's pre-Commit queue and died with it
/// when the held pass refused the Commit, and the fence's own run-loop
/// turns dispatched this window's focus loss, a DEC 1004 focus-out written
/// to the masters after the successor's focus-in. Now nothing is activated
/// before the held pass has read the admission. Refused (a quit pressed
/// during the fence), the successor its rejection kills was never
/// activated; admitted, it is activated once, in the write (the test's
/// pipe has no reader, so the write fails as an EPIPE does).
///
/// AND ONLY WHEN AN ATERM WINDOW HAS FOCUS AT THE WRITE (the lifecycle audit
/// of 2026-10-14, handoff finding 3): focus is read there, not at the park.
/// The person who switched to another app during the freeze keeps it; the
/// one who came back to aterm gets the successor in front.
///
/// The activation is recorded in place of AppKit's
/// (`App::handoff_activations`); the successor is a process of the
/// test's own, killed at the end, and the ids the write marks for it go
/// to a scratch rendezvous.
///
/// RED before the fix: the claim's pass activated the successor. RED with
/// focus sampled at the park: the successor took the front from the app the
/// person had moved to, and not for the one who had come back.
#[cfg(target_os = "macos")]
#[test]
fn a_held_claim_activates_the_successor_only_at_its_write() {
    /// The stand-in successor, killed (`SIGKILL`) and reaped however the
    /// test ends; and the scratch rendezvous, put back.
    struct StandIn(aterm_uds::spawnfd::Child);
    impl Drop for StandIn {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    struct Rendezvous;
    impl Drop for Rendezvous {
        fn drop(&mut self) {
            crate::identity_claim::set_rendezvous_override(None);
        }
    }
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let _rendezvous = crate::identity_claim::rendezvous_test_guard();
    let scratch = aterm_tempfile::tempdir().expect("a scratch rendezvous");
    crate::identity_claim::set_rendezvous_override(Some(scratch.path().to_path_buf()));
    let _put_back = Rendezvous;
    // (refused, focused at the park, focused at the write)
    for (refused, at_park, at_write) in [
        (false, true, true),
        (true, true, true),
        (false, true, false),
        (false, false, true),
    ] {
        let successor = StandIn(
            aterm_uds::spawnfd::command("/bin/sleep")
                .arg("60")
                .spawn()
                .expect("a stand-in successor"),
        );
        let pid = successor.0.id();
        let mut app = crate::App::headless_for_test();
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        app.pool
            .sessions
            .get_mut(&0)
            .expect("session 0")
            .session
            .master = master;
        let (root, replies) = live_take(&mut app);
        // The take's client reads its answer 50 ms after it is sent: the
        // Commit's answers take real time, and its claim is held.
        let (wire, release) = crate::control::ReplyWire::new();
        app.video_reply_wires.push(wire);
        let reader = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(release);
        });
        let (record, _cancelled, _stood_down, _transferred) =
            prelaunch_record(7, ApplyMode::Immediate);
        let nonce = record.nonce.clone();
        app.update_handoff_prelaunch = Some(record);
        for ws in app.windows.values_mut() {
            ws.os_window_for_test = true;
            ws.focused = at_park;
        }
        let parked = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
        assert!(
            matches!(parked, ParkAttempt::Parked),
            "PRECONDITION: the park landed ({parked:?})"
        );
        let pending = app
            .pending_update_handoff
            .as_mut()
            .expect("the parked attempt");
        pending.commit_drain_started =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(100));
        let arbiter = pending.arbiter.clone();
        app.handoff_reposts = Some(Vec::new());
        // No reader: a Commit written fails with EPIPE, and the test runs on.
        let (commit_rd, commit_wr) = std::io::pipe().expect("a commit pipe");
        drop(commit_rd);
        let (reject, rejected) = std::sync::mpsc::sync_channel(1);
        app.finish_proven_handoff(crate::UpdateHandoffCompletion {
            attempt_id: 7,
            nonce: Some(nonce),
            child_pid: Some(pid),
            outcome: crate::UpdateHandoffOutcome::ProofReady,
            commit_fd: Some(commit_wr.into()),
            reject: Some(reject),
            reconcile: None,
            detail: String::new(),
            input_drain_spins: 1,
            child_death: crate::ChildDeathEvidence::Unobserved,
        });
        reader.join().expect("the client read its answer");
        assert!(replies.try_recv().is_ok(), "the claim answered the take");
        assert_eq!(arbiter.phase(), crate::HandoffAttemptPhase::Committing);
        assert!(
            app.pending_update_handoff
                .as_ref()
                .is_some_and(|pending| pending.held_commit.is_some()),
            "PRECONDITION: the claim is held through one more fence"
        );
        assert!(
            app.handoff_activations.is_empty(),
            "a held claim is still a candidate: nothing is activated before its held \
             pass reads the admission, so the fence's keys and focus stay this window's \
             (activated: {:?})",
            app.handoff_activations
        );
        let mut reposted = app.handoff_reposts.replace(Vec::new()).expect("re-posts");
        assert_eq!(reposted.len(), 1, "the attempt went back through the gate");
        let respin = reposted.remove(0);

        // The run loop dispatched; the fence has had its loop time.
        let pending = app.pending_update_handoff.as_mut().expect("the attempt");
        pending.commit_drain_started =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(100));
        if refused {
            pending.teardown = crate::DeferredHandoffTeardown::QuitRequested;
        }
        // The person moved to another app during the fence, or back to aterm.
        for ws in app.windows.values_mut() {
            ws.focused = at_write;
        }
        app.finish_proven_handoff(respin);
        assert_eq!(arbiter.phase(), crate::HandoffAttemptPhase::Rejecting);
        assert!(rejected.try_recv().is_ok(), "the worker is told to reject");
        assert_eq!(
            app.handoff_rejections,
            [if refused {
                "destructive intent revoked Commit before teardown replay"
            } else {
                "attempt-bound Commit pipe closed before its atomic write"
            }],
            "the admission refused the quit; otherwise the held pass wrote the Commit"
        );
        let activated: &[i32] = if refused || !at_write {
            &[]
        } else {
            &[i32::try_from(pid).expect("a pid")]
        };
        assert_eq!(
            app.handoff_activations, activated,
            "activated once, in the write, when an aterm window has focus there; a held \
             claim the admission refused never activated the successor its rejection \
             kills (focused at the park: {at_park}, at the write: {at_write})"
        );

        app.pending_update_handoff = None;
        app.update_handoff_prelaunch = None;
        drop(app);
        aterm_pty::close_fd(slave);
        aterm_pty::close_fd(master);
        let _ = std::fs::remove_dir_all(root);
        drop(successor);
    }
}

/// Wired where it holds: the successor is activated on both of the
/// Commit's write arms — inside the operator's permit, right before the
/// write — past the claim and its held pass, and nowhere in what the
/// claim answers (the test above drives the arm with no operator).
#[cfg(target_os = "macos")]
#[test]
fn the_successor_is_activated_in_the_write_alone() {
    let src = include_str!("app_update_handoff.rs");
    // Spelled in halves so this test's own text is not counted.
    let call = ["self.activate_successor", "_at_write(child_pid);"].concat();
    assert_eq!(
        src.matches(call.as_str()).count(),
        2,
        "the Commit's two write arms, nowhere else"
    );
    let held = src
        .find("pending.held_commit = Some(HeldCommit {")
        .expect("the held claim");
    let mut from = held;
    for _ in 0..2 {
        let at = from + src[from..].find(call.as_str()).expect("an arm");
        let write = at
            + src[at..]
                .find("crate::seamless::commit_and_exit(commit_fd, proof)")
                .expect("its write");
        assert!(write - at < 200, "right before the write");
        from = write;
    }
    let answers = src
        .find("    fn answer_at_commit_claim(")
        .expect("what the claim answers");
    let end = answers + src[answers..].find("\n    }\n").expect("its end");
    assert!(
        !src[answers..end].contains("activate_running("),
        "the claim's answers activate nothing"
    );
}

/// A SLOW COMMIT FLUSH SENDS THE ATTEMPT THROUGH THE FENCE AGAIN (round
/// eight of the update audit, finding 18): the flush blocks the main thread
/// for up to `MESSAGES_FLUSH_COMMIT_WAIT`, and a key the OS queued then
/// would die with the `_exit`. A writer that waits out its bound restarts
/// the dispatch fence's clock and asks for the drain gate again, once per
/// attempt, touching no `video` request. CONTROLS: a hold under one fence
/// grain leaves the fence it has and one grain restarts it, decided by a
/// chosen hold (`App::settle_commit_flush`), never by how fast this machine
/// ran; and with no writer behind, the flush waits on nothing — answered, no
/// new fence.
#[test]
fn a_slow_commit_flush_restarts_the_dispatch_fence() {
    let mut app = crate::App::headless_for_test();
    let dir = aterm_tempfile::tempdir().expect("a temp dir");
    let (writer, _thread) =
        crate::messages_store::Writer::stalled(&dir.path().join("messages.log"));
    app.messages_log = Some(writer);
    let (root, replies) = live_take(&mut app);
    let drain_started = std::time::Instant::now();
    let mut pending = crate::messages_host::tests::parked();
    pending.commit_drain_started = Some(drain_started);
    app.pending_update_handoff = Some(pending);
    assert!(
        app.commit_flush_refences(),
        "a flush that waited out its bound sends the attempt through the fence"
    );
    let pending = app.pending_update_handoff.as_ref().expect("the attempt");
    assert_eq!(
        pending.commit_flush,
        Some(false),
        "the writer did not answer"
    );
    assert_eq!(
        pending.commit_drain_started, None,
        "the fence's clock starts again"
    );
    assert!(
        app.video_rec.is_some() && replies.try_recv().is_err(),
        "nothing before the claim answers a take"
    );
    assert!(!app.commit_flush_refences(), "once per attempt");

    // CONTROL: a hold under one grain keeps the fence it has; one grain
    // restarts it.
    let pending = app.pending_update_handoff.as_mut().expect("the attempt");
    pending.commit_drain_started = Some(drain_started);
    let under = super::HANDOFF_INPUT_DRAIN_YIELD - std::time::Duration::from_micros(1);
    assert!(!app.settle_commit_flush(true, under));
    let pending = app.pending_update_handoff.as_ref().expect("the attempt");
    assert_eq!(pending.commit_flush, Some(true));
    assert_eq!(pending.commit_drain_started, Some(drain_started));
    assert!(app.settle_commit_flush(true, super::HANDOFF_INPUT_DRAIN_YIELD));
    let pending = app.pending_update_handoff.as_ref().expect("the attempt");
    assert_eq!(pending.commit_drain_started, None, "one grain restarts it");

    // CONTROL: no writer behind, the flush waits on nothing: answered, and
    // the fence it has stands.
    app.messages_log = None;
    let pending = app.pending_update_handoff.as_mut().expect("the attempt");
    pending.commit_flush = None;
    pending.commit_drain_started = Some(drain_started);
    assert!(!app.commit_flush_refences(), "no writer: no new fence");
    let pending = app.pending_update_handoff.as_ref().expect("the attempt");
    assert_eq!(pending.commit_flush, Some(true), "no writer: answered");
    assert_eq!(pending.commit_drain_started, Some(drain_started));

    app.pending_update_handoff = None;
    drop(app);
    let _ = std::fs::remove_dir_all(root);
}

/// A live take on `session 0`'s window, answering on the returned channel.
fn live_take(
    app: &mut crate::App,
) -> (
    std::path::PathBuf,
    std::sync::mpsc::Receiver<crate::control::Retained<String>>,
) {
    let root = std::env::temp_dir().join(format!(
        "aterm-video-park-{}-{}",
        std::process::id(),
        crate::metrics::now_us()
    ));
    let _ = std::fs::remove_dir_all(&root);
    crate::control_auth::ensure_private_dir(&root).expect("private recording root");
    let dir = crate::control_auth::confine_video_dir(&root).expect("confined recording dir");
    let (reply, replies) = std::sync::mpsc::channel();
    app.video_rec = Some(crate::VideoRec {
        window: crate::WindowId(0),
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
        started_us: 0,
        keys: false,
        key_log: Vec::new(),
        trail: false,
        trail_log: Vec::new(),
        trail_seen: 0,
        trail_lost: 0,
        pace_ticks: Vec::new(),
        mode: crate::VideoMode::OffscreenPresentReal,
        next_frame: None,
        presented: None,
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
        dir,
        handoff: None,
        cancel: crate::VideoCancellation::new(),
        reply,
    });
    (root, replies)
}

/// A park that misses keeps the take: an explicit apply's park is admitted with a
/// take running, then its capture meets an engine held past the deadline; the
/// park misses, the readers resume, and the take keeps running, its client not
/// told of an update that did not happen. Control: the re-park lands and Commit
/// answers the take.
#[test]
fn a_park_that_misses_keeps_a_live_take() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .master = master;
    let (root, replies) = live_take(&mut app);
    let (record, _cancelled, _stood_down, _transferred) = prelaunch_record(9, ApplyMode::Immediate);
    app.update_handoff_prelaunch = Some(record);

    let term = app.pool.get(0).expect("session 0").term.clone();
    // On the real rung: a test park's default deadline would wait it out.
    let real = super::park_budget_for_test::real();
    let holder = super::dry_run_capture_tests::hold_engine_past_every_rung(&term);
    let missed = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
    holder.join().expect("the holder finished");
    drop(real);
    assert!(
        matches!(missed, ParkAttempt::Missed(_)),
        "PRECONDITION: the park was admitted and missed ({missed:?})"
    );
    assert!(
        app.video_rec.is_some(),
        "a park that rolled back leaves the take running"
    );
    assert!(
        replies.try_recv().is_err(),
        "and its client is told nothing — no update happened"
    );

    // CONTROL: the park that lands, then its Commit, answers it.
    let parked = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
    assert!(
        matches!(parked, ParkAttempt::Parked),
        "PRECONDITION: the re-park landed ({parked:?})"
    );
    assert!(
        app.video_answer_before_commit(super::VIDEO_EXPORT_COMMIT_WAIT)
            .answered
    );
    let (body, _retention) = replies
        .try_recv()
        .expect("Commit answered the take")
        .into_parts();
    assert_eq!(body, "ERR video: recording aborted: aterm is updating\n");
    assert!(app.video_rec.is_none());

    app.pending_update_handoff = None;
    app.update_handoff_prelaunch = None;
    drop(app);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
    let _ = std::fs::remove_dir_all(root);
}

/// EVERY `video` REPLY COMMIT CAUSES IS ON ITS CONNECTION BEFORE THE
/// `_exit` (round six of the update audit, finding 31). Commit aborts a
/// live take and cancels an export; the export's encode worker sees that
/// only between frames, and either answer reaches its client only when
/// the connection thread writes it, after the channel hand-off. Each
/// stand-in below is a connection thread that writes its reply 150 ms
/// after the answer arrives (and releases the request's [`ReplyWire`] as
/// the real write does, `write_control_reply_with_timeout_arm`), and the
/// export worker answers 200 ms after the cancel: Commit's step
/// (`App::video_answer_before_commit`, which the Commit arm alone calls)
/// returns only once both replies are WRITTEN, inside its bound.
///
/// The bound handed to the step is TEN MINUTES, not the shipped
/// [`super::VIDEO_EXPORT_COMMIT_WAIT`]: "returned on the writes, not at the
/// bound" is then told apart by a minute's hang detector, where a one-second
/// bound let one stall of this thread fail a correct tree.
///
/// RED before the fix: Commit went from the park's cancel straight to the
/// `_exit`; with only the export's permit waited for, the export's reply
/// was still unwritten when it returned.
///
/// [`ReplyWire`]: crate::control::ReplyWire
#[test]
fn commit_waits_until_every_video_reply_is_written() {
    const BOUND: std::time::Duration = std::time::Duration::from_secs(600);
    let mut app = crate::App::headless_for_test();
    let written = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // A connection thread: waits for its request's answer, writes it
    // 150 ms later, and releases the wire as the write does.
    let connection = |answers: std::sync::mpsc::Receiver<String>| {
        let (wire, release) = crate::control::ReplyWire::new();
        let written = std::sync::Arc::clone(&written);
        let thread = std::thread::spawn(move || {
            let answer = answers.recv().expect("the request is answered");
            std::thread::sleep(std::time::Duration::from_millis(150));
            written.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            drop(release);
            answer
        });
        (wire, thread)
    };

    // A live take, answered on its channel.
    let (root, take_replies) = live_take(&mut app);
    let (take_answers, take_answered) = std::sync::mpsc::channel();
    let forward = std::thread::spawn(move || {
        let (body, _retention) = take_replies
            .recv()
            .expect("the take is answered")
            .into_parts();
        let _ = take_answers.send(body);
    });
    let (take_wire, take_connection) = connection(take_answered);
    app.video_reply_wires.push(take_wire);

    // An export mid-frame: it answers at its next frame boundary, 200 ms
    // after the cancel, and only then drops its permit.
    let export = crate::VideoCancellation::new();
    let permit = app
        .video_export
        .try_begin(export.clone())
        .expect("an export starts");
    let (export_answers, export_answered) = std::sync::mpsc::channel();
    let worker = {
        let export = export.clone();
        std::thread::spawn(move || {
            while !export.is_cancelled() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            let _ = export_answers.send("ERR video: export failed".to_string());
            drop(permit);
        })
    };
    let (export_wire, export_connection) = connection(export_answered);
    app.video_reply_wires.push(export_wire);

    let started = std::time::Instant::now();
    let answered = app.video_answer_before_commit(BOUND);
    assert_eq!(
        written.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "both replies were written before Commit went on"
    );
    assert_eq!(
        answered,
        crate::VideoCommitAnswer {
            answered: true,
            export_settled: true,
            unwritten: 0,
        }
    );
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "returned on the writes, not at its ten-minute bound: {:?}",
        started.elapsed()
    );
    assert!(export.is_cancelled() && !app.video_request_live());
    assert!(
        app.video_reply_wires.is_empty(),
        "released wires are pruned"
    );
    forward.join().expect("the take's answer was forwarded");
    worker.join().expect("the worker finished");
    assert_eq!(
        take_connection.join().expect("the take's connection"),
        "ERR video: recording aborted: aterm is updating\n"
    );
    assert_eq!(
        export_connection.join().expect("the export's connection"),
        "ERR video: export failed"
    );

    // BOUNDED: a reply nobody ever writes costs Commit its bound, and is
    // said, not waited on forever.
    let (stuck, _never_released) = crate::control::ReplyWire::new();
    app.video_reply_wires.push(stuck);
    let started = std::time::Instant::now();
    let answered = app.video_answer_before_commit(std::time::Duration::from_millis(50));
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "a 50 ms bound ended the wait (a hang detector): {:?}",
        started.elapsed()
    );
    assert_eq!(answered.unwritten, 1);
    assert!(!answered.answered);

    // CONTROL: with nothing live the step costs nothing — not its bound.
    app.video_reply_wires.clear();
    let started = std::time::Instant::now();
    assert_eq!(
        app.video_answer_before_commit(BOUND),
        crate::VideoCommitAnswer {
            answered: false,
            export_settled: true,
            unwritten: 0,
        }
    );
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "nothing live, yet the step waited toward its ten-minute bound: {:?}",
        started.elapsed()
    );
    let _ = std::fs::remove_dir_all(root);
}

/// A `video` request arriving after the park is refused, since Commit would cut
/// it off. The event loop asks [`crate::App::video_request_refusal`] before
/// starting any take.
#[test]
fn a_video_request_after_the_park_is_refused() {
    // The park that lands keeps its cost in the update ledger.
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .master = master;
    assert_eq!(
        app.video_request_refusal(),
        None,
        "an idle, unparked process takes a recording"
    );

    let (record, _cancelled, _stood_down, _transferred) = prelaunch_record(8, ApplyMode::Immediate);
    app.update_handoff_prelaunch = Some(record);
    let parked = app.park_and_transfer_to_prelaunched_successor(std::time::Instant::now());
    assert!(
        matches!(parked, ParkAttempt::Parked),
        "PRECONDITION: the park landed ({parked:?})"
    );
    assert!(app.update_handoff_parked(), "PRECONDITION: parked");
    assert_eq!(
        app.video_request_refusal(),
        Some("aterm is updating"),
        "a take accepted now would die at Commit with no reply"
    );

    app.pending_update_handoff = None;
    app.update_handoff_prelaunch = None;
    assert_eq!(app.video_request_refusal(), None, "and taken again after");
    drop(app);
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
}
