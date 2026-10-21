// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless present-real: recording-mode honesty, the recording lifecycle's conformance to its
//! derived model, and idle-zero (finalize drops the Virtual target and timer).

use std::time::{Duration, Instant};

use super::{
    LINUX_CAPTURE, MAC_CAPTURE, MATERIAL_CAPTURE_ERROR, PresentDropAccounting, PresentTarget,
    TRANSLUCENT_CAPTURE_ERROR, VideoCancellation, VideoInputSample, VideoMode, VideoRec,
    WIN_CAPTURE, WindowId, fold_video_deadlines, platform_capture_guards,
    raw_present_capture_guard, record_controller_video_attempts, video_capture_mode,
    video_capture_opacity_requires_abort, video_capture_translucency_guard,
    video_initial_next_frame, video_post_present_should_finalize,
};

use crate::app_config::{BackgroundMaterial, RenderKnobs};
use crate::app_render::GpuRecoveryOutcome;
use crate::input::InputEvent;
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

fn test_video_dir(
    label: &str,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    crate::control_auth::ConfinedVideoDir,
) {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "aterm-{label}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    crate::control_auth::ensure_private_dir(&root).unwrap();
    let dir = crate::control_auth::confine_video_dir(&root).unwrap();
    let path = dir.path().to_path_buf();
    (root, path, dir)
}

fn controller_key_with_mods(key: Key, mods: Modifiers, event_type: KeyEventType) -> InputEvent {
    InputEvent::Key {
        key,
        mods,
        base_layout: None,
        layout_text: None,
        event_type,
    }
}

fn controller_key(key: Key, event_type: KeyEventType) -> InputEvent {
    controller_key_with_mods(key, Modifiers::empty(), event_type)
}

/// Bounded bookkeeping for pre-routing controller character attempts: press/repeat adds
/// one sample, release none, never past the cap; an attempt past the cap is counted as
/// dropped, so logged plus dropped is every attempt. Says nothing about delivery or pixels.
/// `Buggy=1` logs a Kitty release as an attempt (the negative control); the conformance
/// test's own control is the overflow dropped uncounted, the defect the count fixes.
fn video_controller_attempt_ledger_model() -> aterm_spec::derive::Model {
    use aterm_spec::ty_model;
    ty_model! {
        VideoControllerAttemptLedger {
            const Cap = 3;
            const Over = 3;
            const Buggy = 0;
            var attempts = 0;
            var logged = 0;
            var dropped = 0;
            var releases = 0;
            action RecordAttempt when (logged <= Cap - 1) {
                attempts = attempts + 1;
                logged = logged + 1;
            }
            action OverflowAttempt when (logged == Cap && dropped <= Over - 1) {
                attempts = attempts + 1;
                dropped = dropped + 1;
            }
            action IgnoreRelease when (releases <= Cap - 1) {
                releases = releases + 1;
                logged = if Buggy == 1 { logged + 1 } else { logged };
            }
            action Stay when (logged == Cap && dropped == Over && releases == Cap) {
                attempts = attempts;
            }
            invariant Bounds: attempts <= Cap + Over && logged <= Cap && dropped <= Over && releases <= Cap;
            invariant ExactAttemptLedger: logged + dropped == attempts;
        }
    }
}

#[test]
fn video_controller_attempt_ledger_model_proves_and_catches_release_duplication() {
    let model = video_controller_attempt_ledger_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

/// Tier-1 conformance of the attempt ledger to the shipping classifier: press/repeat
/// record once, release is silent, named Space is kept, multi-character Text/Paste is
/// complete, the cap truncates exactly. A Ctrl shortcut is still an attempt sample.
#[test]
fn real_controller_video_attempt_ledger_conforms_to_model() {
    let model = video_controller_attempt_ledger_model();
    let mut state = model.init_state();
    let mut log = Vec::new();

    let below = record_controller_video_attempts(
        &mut log,
        &controller_key(Key::Character('a'), KeyEventType::Press),
        10,
        3,
    );
    assert_eq!(below, 0, "below the cap nothing is dropped");
    assert!(model.fire("RecordAttempt", &mut state));
    assert_eq!(log, vec![(10, VideoInputSample::Char('a'))]);

    let below = record_controller_video_attempts(
        &mut log,
        &controller_key(Key::Character('a'), KeyEventType::Release),
        11,
        3,
    );
    assert_eq!(below, 0, "below the cap nothing is dropped");
    assert!(model.fire("IgnoreRelease", &mut state));
    assert_eq!(
        log,
        vec![(10, VideoInputSample::Char('a'))],
        "release is not a second sample"
    );

    let below = record_controller_video_attempts(
        &mut log,
        &controller_key(Key::Named(NamedKey::Space), KeyEventType::Press),
        12,
        3,
    );
    assert_eq!(below, 0, "below the cap nothing is dropped");
    assert!(model.fire("RecordAttempt", &mut state));
    assert_eq!(
        log.last(),
        Some(&(12, VideoInputSample::Char(' '))),
        "named Space is represented"
    );

    let below = record_controller_video_attempts(
        &mut log,
        &controller_key(Key::Character('r'), KeyEventType::Repeat),
        13,
        3,
    );
    assert_eq!(below, 0, "below the cap nothing is dropped");
    assert!(model.fire("RecordAttempt", &mut state));
    assert_eq!(
        log.last(),
        Some(&(13, VideoInputSample::Char('r'))),
        "repeat records one attempt"
    );
    assert_eq!(log.len() as i64, state["logged"]);
    assert!(model.check_invariant("ExactAttemptLedger", &state));

    // Past the cap the helper appends nothing and answers what it dropped: one press,
    // then a two-character text, three attempts the full ledger counts and keeps out.
    let press = record_controller_video_attempts(
        &mut log,
        &controller_key(Key::Character('x'), KeyEventType::Press),
        14,
        3,
    );
    assert!(model.fire("OverflowAttempt", &mut state));
    assert_eq!(press, 1, "a press past the cap is counted");
    let text = record_controller_video_attempts(&mut log, &InputEvent::Text("yz".into()), 15, 3);
    assert!(model.fire("OverflowAttempt", &mut state));
    assert!(model.fire("OverflowAttempt", &mut state));
    assert_eq!(text, 2, "every character past the cap is counted");
    assert_eq!(
        log.len() as i64,
        state["logged"],
        "the full ledger takes no row"
    );
    assert_eq!((press + text) as i64, state["dropped"]);
    assert!(model.check_invariant("ExactAttemptLedger", &state));
    // A release past the cap is no attempt, so it is no drop either.
    assert_eq!(
        record_controller_video_attempts(
            &mut log,
            &controller_key(Key::Character('x'), KeyEventType::Release),
            16,
            3,
        ),
        0
    );
    // Negative control: the overflow dropped uncounted (the old helper) breaks the law.
    let mut uncounted = state.clone();
    uncounted.insert("dropped", uncounted["dropped"] - 1);
    assert!(!model.check_invariant("ExactAttemptLedger", &uncounted));

    // Multi-codepoint text/paste records every character below the cap.
    let mut multi = Vec::new();
    let _ = record_controller_video_attempts(&mut multi, &InputEvent::Text("ab".into()), 20, 8);
    let _ = record_controller_video_attempts(
        &mut multi,
        &InputEvent::Paste(" c".into(), crate::input::PasteFraming::AtDrain),
        21,
        8,
    );
    assert_eq!(
        multi,
        vec![
            (20, VideoInputSample::Char('a')),
            (20, VideoInputSample::Char('b')),
            (21, VideoInputSample::Char(' ')),
            (21, VideoInputSample::Char('c'))
        ]
    );

    // Ctrl-C may be consumed or encoded as a control byte by later routing; the
    // pre-routing ledger still records the attempt, so a sample is neither a PTY write
    // receipt nor a visible glyph.
    let mut shortcut = Vec::new();
    let _ = record_controller_video_attempts(
        &mut shortcut,
        &controller_key_with_mods(Key::Character('c'), Modifiers::CTRL, KeyEventType::Press),
        30,
        3,
    );
    assert_eq!(shortcut, vec![(30, VideoInputSample::Char('c'))]);

    // Negative control: the release-as-sample mutant breaks `ExactAttemptLedger`.
    let mut bad = state.clone();
    bad.insert("logged", bad["logged"] + 1);
    assert!(!model.check_invariant("ExactAttemptLedger", &bad));
}

/// The ledger stamps only the window it records: attempts on another window (the front window
/// can change mid-take) are counted into `unlogged_inputs`, through both the socket seam and the
/// hardware key path.
#[test]
fn the_key_ledger_refuses_input_from_a_window_it_is_not_recording() {
    let mut app = super::App::headless_for_test();
    let recorded = WindowId(0);
    let other = WindowId(1);
    let (root, _path, dir) = test_video_dir("video-keys-foreign-window");
    let (reply, _rx) = std::sync::mpsc::channel();
    let began = crate::unseamed_control_inputs();
    app.video_rec = Some(VideoRec {
        window: recorded,
        deadline: Instant::now() + Duration::from_secs(3),
        started_us: 0,
        keys: true,
        key_log: Vec::new(),
        pace_ticks: Vec::new(),
        unseamed_at_begin: began,
        unlogged_other_window: 0,
        unlogged_over_cap: 0,
        mode: VideoMode::SwapchainTap,
        next_frame: None,
        presented: None,
        dir,
        handoff: None,
        cancel: VideoCancellation::new(),
        reply,
    });

    // The recorded window: both paths log on the frame clock (the socket arm with the
    // `KeySequence` that `send`/`feed` build, the hardware arm with a named key).
    app.note_video_ledger_event(recorded, &InputEvent::KeySequence(b"hi".to_vec()), 100);
    app.note_video_ledger_sample(recorded, VideoInputSample::Named(NamedKey::ArrowUp), 150);

    // A second window: three bytes plus one key, four attempts this take cannot answer.
    app.note_video_ledger_event(other, &InputEvent::KeySequence(b"abc".to_vec()), 200);
    app.note_video_ledger_sample(other, VideoInputSample::Char('z'), 250);

    let rec = app.video_rec.as_ref().expect("the recording is still live");
    assert_eq!(
        rec.key_log,
        vec![
            (100, VideoInputSample::Char('h')),
            (100, VideoInputSample::Char('i')),
            (150, VideoInputSample::Named(NamedKey::ArrowUp)),
        ],
        "only input on the RECORDED window may be stamped on its frame clock"
    );
    assert_eq!(
        rec.unlogged_other_window, 4,
        "the declined attempts must be COUNTED in the ledger's own unit, not dropped"
    );
    // …and reach the take's published figure, so a short inputs[] never looks like a quiet
    // screen. A lower bound: the control-thread counter is process-wide.
    assert!(
        super::App::video_unlogged_total(rec) >= 4,
        "the take's unlogged_inputs must include the foreign-window attempts"
    );

    // Negative control: a take without a ledger counts nothing.
    if let Some(rec) = app.video_rec.as_mut() {
        rec.keys = false;
        rec.unlogged_other_window = 0;
    }
    app.note_video_ledger_event(other, &InputEvent::KeySequence(b"abc".to_vec()), 300);
    app.note_video_ledger_sample(other, VideoInputSample::Char('z'), 350);
    let rec = app.video_rec.as_ref().expect("the recording is still live");
    assert_eq!(
        (rec.key_log.len(), rec.unlogged_other_window),
        (3, 0),
        "a take without `keys` neither logs nor counts"
    );

    let _ = app.video_rec.take();
    let _ = std::fs::remove_dir_all(root);
}

/// A full ledger counts what it drops: once `key_log` holds `VIDEO_KEY_LOG_CAP` rows, the
/// recorded window's attempts on both the socket seam and the hardware key path are counted in
/// `unlogged_over_cap` and reach the take's `unlogged_inputs`, as frames count their drops.
#[test]
fn a_full_key_ledger_counts_the_attempts_it_drops() {
    let mut app = super::App::headless_for_test();
    let recorded = WindowId(0);
    let (root, _path, dir) = test_video_dir("video-keys-over-cap");
    let (reply, _rx) = std::sync::mpsc::channel();
    app.video_rec = Some(VideoRec {
        window: recorded,
        deadline: Instant::now() + Duration::from_secs(3),
        started_us: 0,
        keys: true,
        key_log: vec![(1, VideoInputSample::Char('a')); crate::VIDEO_KEY_LOG_CAP - 1],
        pace_ticks: Vec::new(),
        unseamed_at_begin: crate::unseamed_control_inputs(),
        unlogged_other_window: 0,
        unlogged_over_cap: 0,
        mode: VideoMode::SwapchainTap,
        next_frame: None,
        presented: None,
        dir,
        handoff: None,
        cancel: VideoCancellation::new(),
        reply,
    });

    // One row is left: `hi` fills it with `h` and drops `i`; the hardware key is dropped too.
    app.note_video_ledger_event(recorded, &InputEvent::KeySequence(b"hi".to_vec()), 100);
    app.note_video_ledger_sample(recorded, VideoInputSample::Named(NamedKey::ArrowUp), 150);

    let rec = app.video_rec.as_ref().expect("the recording is still live");
    assert_eq!(
        rec.key_log.len(),
        crate::VIDEO_KEY_LOG_CAP,
        "the ledger stays bounded"
    );
    assert_eq!(
        rec.key_log.last(),
        Some(&(100, VideoInputSample::Char('h')))
    );
    assert_eq!(
        (rec.unlogged_over_cap, rec.unlogged_other_window),
        (2, 0),
        "the two attempts past the cap are counted, as the full ledger's own"
    );
    // A lower bound: the control-thread counter is process-wide.
    assert!(
        super::App::video_unlogged_total(rec) >= 2,
        "the take's unlogged_inputs must include the attempts past the cap"
    );

    let _ = app.video_rec.take();
    let _ = std::fs::remove_dir_all(root);
}

/// Mode honesty both ways: glass records through the GPU swapchain tap or refuses, never the
/// offscreen arm; only a glass-less window takes the offscreen arm.
#[test]
fn video_mode_honesty_both_ways() {
    assert_eq!(
        video_capture_mode(true, true, true),
        Ok(VideoMode::SwapchainTap),
        "a windowed GPU target records bytes submitted through WSI present"
    );
    assert_eq!(
        video_capture_mode(false, false, true),
        Ok(VideoMode::OffscreenPresentReal),
        "a glass-less window records its virtual present target"
    );
    // Glass without a GPU swapchain (CPU present, mid-rebuild) refuses.
    assert!(
        video_capture_mode(true, false, true).is_err(),
        "a windowed non-GPU target refuses; it must not fall back offscreen"
    );
    assert!(video_capture_mode(true, true, false).is_err());
    assert!(video_capture_mode(false, false, false).is_err());
    for has_surface in [false, true] {
        for gpu in [false, true] {
            assert_ne!(
                video_capture_mode(true, has_surface, gpu),
                Ok(VideoMode::OffscreenPresentReal),
                "glass NEVER takes the offscreen arm"
            );
            assert_ne!(
                video_capture_mode(false, has_surface, gpu),
                Ok(VideoMode::SwapchainTap),
                "no glass NEVER claims the swapchain tap"
            );
        }
    }
}

#[test]
fn capture_regression_translucent_raw_present_refuses_wysiwyg_claim() {
    raw_present_capture_guard(0.75, true)
        .expect_err("a raw translucent layer omits compositor-owned backdrop pixels");

    assert!(
        raw_present_capture_guard(1.0, true).is_ok(),
        "opaque client bytes are the complete client result"
    );
    assert!(
        raw_present_capture_guard(0.75, false).is_ok(),
        "headless/opaque-platform capture has no compositor backdrop to omit"
    );
    assert!(
        raw_present_capture_guard(f32::NAN, true).is_ok(),
        "the render policy treats non-finite opacity as solid"
    );

    assert!(
        video_capture_translucency_guard(VideoMode::SwapchainTap, 0.75, true).is_err(),
        "negative control: the old swapchain-only policy would omit compositor-owned pixels"
    );
    assert_eq!(
        video_capture_translucency_guard(VideoMode::OffscreenPresentReal, 0.75, true),
        Ok(VideoMode::OffscreenPresentReal),
        "headless present-real remains honestly raw and labeled"
    );
    assert_eq!(
        video_capture_translucency_guard(VideoMode::SwapchainTap, 0.75, false),
        Ok(VideoMode::SwapchainTap),
        "a platform whose client remains opaque need not refuse"
    );
}

/// Each platform's capture traits route both guards: a translucent GPU window
/// is refused where the compositor blends the client (macOS) and admitted where
/// it stays opaque (Windows, Linux); a compositor material is refused where the
/// photograph omits it (Windows) and admitted where it does not. A solid window
/// without a material passes on every platform.
#[test]
fn platform_traits_route_both_guards() {
    let solid = RenderKnobs {
        background_opacity: 1.0,
        background_material: BackgroundMaterial::None,
        ..RenderKnobs::default()
    };
    let translucent = RenderKnobs {
        background_opacity: 0.75,
        ..solid
    };
    let platforms = [
        ("macOS", MAC_CAPTURE),
        ("Windows", WIN_CAPTURE),
        ("Linux", LINUX_CAPTURE),
    ];
    for (platform, traits) in platforms {
        assert_eq!(
            platform_capture_guards(traits, &solid, true, true),
            Ok(()),
            "{platform}: a solid GPU window is what the window shows"
        );
    }

    // Translucency: only a window that really presents through the GPU
    // swapchain the compositor blends.
    assert_eq!(
        platform_capture_guards(MAC_CAPTURE, &translucent, true, true),
        Err(TRANSLUCENT_CAPTURE_ERROR),
        "macOS: the raw layer of a translucent GPU window omits the backdrop"
    );
    assert_eq!(
        platform_capture_guards(MAC_CAPTURE, &translucent, false, false),
        Ok(()),
        "macOS CPU/softbuffer is an effective solid fallback, not missing glass"
    );
    assert_eq!(
        platform_capture_guards(MAC_CAPTURE, &translucent, true, false),
        Ok(()),
        "a backend/target mismatch cannot be called a translucent GPU present"
    );
    for (platform, traits) in [("Windows", WIN_CAPTURE), ("Linux", LINUX_CAPTURE)] {
        assert_eq!(
            platform_capture_guards(traits, &translucent, true, true),
            Ok(()),
            "{platform}: the compositor does not blend the client layer"
        );
    }

    // Material: only where the platform photograph omits it.
    for material in [
        BackgroundMaterial::Hud,
        BackgroundMaterial::Sidebar,
        BackgroundMaterial::UnderWindow,
    ] {
        let knobs = RenderKnobs {
            background_material: material,
            ..solid
        };
        platform_capture_guards(WIN_CAPTURE, &knobs, true, true)
            .expect_err("PrintWindow omits the live DWM material");
        for (platform, traits) in [("macOS", MAC_CAPTURE), ("Linux", LINUX_CAPTURE)] {
            assert_eq!(
                platform_capture_guards(traits, &knobs, true, true),
                Ok(()),
                "{platform}: the photograph does not omit {material:?}"
            );
        }
    }

    // Both guards are asked: a translucent GPU window with a material is
    // refused by each platform for its own reason.
    let both = RenderKnobs {
        background_material: BackgroundMaterial::Hud,
        ..translucent
    };
    assert_eq!(
        platform_capture_guards(MAC_CAPTURE, &both, true, true),
        Err(TRANSLUCENT_CAPTURE_ERROR)
    );
    assert_eq!(
        platform_capture_guards(WIN_CAPTURE, &both, true, true),
        Err(MATERIAL_CAPTURE_ERROR)
    );
    assert_eq!(
        platform_capture_guards(LINUX_CAPTURE, &both, true, true),
        Ok(())
    );
}

#[test]
fn capture_regression_post_present_stop_fails_closed() {
    assert!(
        video_post_present_should_finalize(false, true),
        "a resize/format stop finalizes before the duration deadline"
    );
    let now = Instant::now();
    assert_eq!(
        video_initial_next_frame(VideoMode::SwapchainTap, true, now),
        Some(now),
        "paced glass arms the WaitUntil owner at begin"
    );
    assert_eq!(
        video_initial_next_frame(VideoMode::SwapchainTap, false, now),
        None
    );
    assert_eq!(
        video_initial_next_frame(VideoMode::OffscreenPresentReal, false, now),
        Some(now),
        "headless present-real always needs its timer"
    );
}

/// Tier-1 conformance: the real begin decision, export permit, cancellation CAS and
/// cleanup projected onto `VideoRecordingLifecycle`. Tier-0 lives in aterm-spec.
#[test]
fn real_video_lifecycle_conforms_to_model() {
    let model = aterm_models_misc::video_recording_lifecycle_model();
    let mut st = model.init_state();

    // The control thread reserves one directory/recording slot before the
    // event-loop begin decision.
    assert!(model.fire("Reserve", &mut st));
    assert_eq!((st["phase"], st["private_dirs"]), (1, 1));
    assert_eq!(
        video_capture_mode(false, false, true),
        Ok(VideoMode::OffscreenPresentReal)
    );
    assert!(!model.fire("BeginOnGlass", &mut st), "no glass: no tap arm");
    assert!(model.fire("BeginHeadless", &mut st));
    assert_eq!(st["mode"], 2);
    let begun = Instant::now();
    assert_eq!(
        st["timer"],
        i64::from(
            video_initial_next_frame(VideoMode::OffscreenPresentReal, false, begun).is_some()
        ),
        "the offscreen loop's timer is the deadline the shipping begin arms"
    );
    assert!(model.check_invariant("OffscreenOnlyWithoutGlass", &st));
    assert!(model.check_invariant("RecordingOwnsItsPacingTimer", &st));
    // Negative control: `video_initial_next_frame` written as
    // `pace.then_some(now)` arms nothing for an unpaced headless begin, and
    // the offscreen loop's sole redraw driver never fires.
    let pace_only = |_mode: VideoMode, pace: bool, now: Instant| pace.then_some(now);
    let mut untimed = st.clone();
    untimed.insert(
        "timer",
        i64::from(pace_only(VideoMode::OffscreenPresentReal, false, begun).is_some()),
    );
    assert!(!model.check_invariant("RecordingOwnsItsPacingTimer", &untimed));
    assert!(model.fire("Tick", &mut st));

    // Finalize transfers the sole ownership slot to the real export permit.
    let export = std::sync::Arc::new(super::VideoExportState::default());
    let cancel = VideoCancellation::new();
    let permit = export
        .try_begin(cancel.clone())
        .expect("idle real export state admits one permit");
    assert!(model.fire("BeginExport", &mut st));
    assert_eq!((st["recording_slot"], st["export_permit"]), (0, 1));
    assert!(
        export.try_begin(VideoCancellation::new()).is_none(),
        "the real permit refuses a second recording/export lifecycle"
    );
    assert!(!model.fire("Reserve", &mut st));

    // The worker's CAS authorizes publication immediately before rename.
    assert!(cancel.authorize_commit());
    assert!(model.fire("AuthorizeCommit", &mut st));
    cancel.cancel();
    assert!(
        !cancel.is_cancelled(),
        "a late real cancellation cannot revoke commit authorization"
    );
    assert!(model.fire("CancelAfterAuthorization", &mut st));
    assert_eq!(
        st["cancel_state"],
        if cancel.is_cancelled() { 1 } else { 2 },
        "the real token's verdict after the late cancel"
    );
    // Negative control: a cancel that stores VIDEO_CANCELLED instead of
    // CAS-ing it from live would read cancelled here, while the export it
    // revoked is still past its rename boundary.
    let mut revoked = st.clone();
    revoked.insert("cancel_state", 1);
    assert!(!model.check_invariant("CancelledOwnsNothing", &revoked));
    assert!(model.fire("PublishSuccess", &mut st));
    drop(permit);
    assert!(!export.is_busy());
    assert_eq!(
        (st["phase"], st["recording_slot"], st["private_dirs"]),
        (0, 0, 0)
    );

    // With glass: the real law picks the tap and the model refuses the headless arm.
    assert!(model.fire("AttachGlass", &mut st));
    assert!(model.fire("Reserve", &mut st));
    assert_eq!(
        video_capture_mode(true, true, true),
        Ok(VideoMode::SwapchainTap)
    );
    assert!(
        !model.fire("BeginHeadless", &mut st),
        "glass: no offscreen arm"
    );
    assert!(model.fire("BeginOnGlass", &mut st));
    let tapped = Instant::now();
    assert_eq!(
        st["timer"],
        i64::from(video_initial_next_frame(VideoMode::SwapchainTap, false, tapped).is_some()),
        "an unpaced tap's begin arms no deadline: its presents drive it"
    );
    assert!(model.check_invariant("TapOnlyOnGlass", &st));
    // Negative control: glass gone while the tap still copies it (a close that skipped
    // `video_abort_window_close`). The shipping close path is driven by
    // `window_close_app_drop_and_backend_rebuild_abort_recording_ownership`.
    let mut orphaned = st.clone();
    orphaned.insert("glass", 0);
    assert!(!model.check_invariant("TapOnlyOnGlass", &orphaned));
    assert!(model.fire("OwnerLost", &mut st));
    assert_eq!((st["phase"], st["mode"], st["timer"]), (0, 0, 0));

    // A paced tap: the shipping begin arms the WaitUntil owner on glass too, like the
    // model's paced begin.
    assert!(model.fire("RequestPaced", &mut st));
    assert!(model.fire("Reserve", &mut st));
    assert!(model.fire("BeginOnGlass", &mut st));
    assert_eq!(
        st["timer"],
        i64::from(video_initial_next_frame(VideoMode::SwapchainTap, true, tapped).is_some()),
        "the paced tap's timer is the deadline the shipping begin arms"
    );
    // Negative control: a begin that arms no deadline for any swapchain tap
    // starves a paced recording on its first skipped unchanged frame.
    let mut starved = st.clone();
    starved.insert("timer", 0);
    assert!(!model.check_invariant("RecordingOwnsItsPacingTimer", &starved));
    assert!(model.fire("Tick", &mut st));
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, &st),
            "paced tap violates {}",
            invariant.name
        );
    }
    assert!(model.fire("OwnerLost", &mut st));
    assert!(model.fire("RequestUnpaced", &mut st));
    assert_eq!((st["phase"], st["mode"], st["timer"]), (0, 0, 0));

    // A live transition to translucent glass atomically aborts the raw tap
    // before another frame can be recorded under its opaque admission.
    assert!(model.fire("Reserve", &mut st));
    assert!(model.fire("BeginOnGlass", &mut st));
    assert!(video_capture_opacity_requires_abort(
        VideoMode::SwapchainTap,
        0.75,
        true,
    ));
    assert!(model.fire("MakeTapTranslucent", &mut st));
    assert_eq!(st["mode"], 0, "opacity transition ends the tap");
    assert!(model.check_invariant("NoTranslucentTap", &st));

    // Cancellation winning before publication maps to cleanup plus permit
    // acknowledgement; the token can no longer authorize commit.
    assert!(model.fire("MakeOpaque", &mut st));
    assert!(model.fire("DetachGlass", &mut st));
    assert!(model.fire("Reserve", &mut st));
    assert!(model.fire("BeginHeadless", &mut st));
    let cancelled = VideoCancellation::new();
    let cancelled_permit = export
        .try_begin(cancelled.clone())
        .expect("permit was released after prior publication");
    assert!(model.fire("BeginExport", &mut st));
    cancelled.cancel();
    drop(cancelled_permit);
    assert!(!cancelled.authorize_commit());
    assert!(model.fire("CancelLive", &mut st));
    assert!(!export.is_busy());
    assert!(model.check_invariant("CancelledOwnsNothing", &st));

    // Negative controls: the forbidden windowed-offscreen fallback, and a publication
    // that skipped the CAS authorization.
    let mut bad = model.init_state();
    assert!(model.fire("AttachGlass", &mut bad));
    assert!(model.fire("Reserve", &mut bad));
    bad.insert("phase", 2);
    bad.insert("mode", 2);
    bad.insert("timer", 1);
    assert!(
        !model.check_invariant("OffscreenOnlyWithoutGlass", &bad),
        "a windowed offscreen recording is exactly what the invariant rejects"
    );
    let mut bad_publish = model.init_state();
    assert!(model.fire("Reserve", &mut bad_publish));
    assert!(model.fire("BeginHeadless", &mut bad_publish));
    assert!(model.fire("BeginExport", &mut bad_publish));
    bad_publish.insert("phase", 0);
    bad_publish.insert("private_dirs", 0);
    bad_publish.insert("export_permit", 0);
    bad_publish.insert("published", 1);
    assert!(
        !model.check_invariant("CommitAuthorizationScope", &bad_publish),
        "publication without winning the CAS is rejected"
    );
}

#[test]
fn live_opacity_transition_aborts_swapchain_recording_fail_closed() {
    let mut app = super::App::headless_for_test();
    let wid = WindowId(0);
    let now = Instant::now();
    let (root, dir_path, dir) = test_video_dir("video-opacity-abort");
    let (reply, rx) = std::sync::mpsc::channel();
    app.video_rec = Some(VideoRec {
        window: wid,
        deadline: now + Duration::from_secs(3),
        started_us: 0,
        keys: false,
        key_log: Vec::new(),
        pace_ticks: Vec::new(),
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
        unlogged_over_cap: 0,
        mode: VideoMode::SwapchainTap,
        next_frame: None,
        presented: None,
        dir,
        handoff: None,
        cancel: VideoCancellation::new(),
        reply,
    });

    assert!(app.video_abort_translucent_swapchain(0.75, true));
    assert!(
        app.video_rec.is_none(),
        "the raw tap is consumed atomically"
    );
    assert!(
        !dir_path.exists(),
        "an aborted begin never leaks its pre-created recording directory"
    );
    let _ = std::fs::remove_dir_all(root);
    assert!(
        rx.recv()
            .expect("aborted recording answers its blocked client")
            .contains("full-window artifact is unavailable for a translucent client")
    );

    // Negative controls: an opaque transition and the headless present-real
    // mode retain their recording because neither omits compositor pixels.
    for (mode, opacity, blends) in [
        (VideoMode::SwapchainTap, 1.0, true),
        (VideoMode::OffscreenPresentReal, 0.75, true),
        (VideoMode::SwapchainTap, 0.75, false),
    ] {
        let (root, _path, dir) = test_video_dir("video-opacity-negative");
        let (reply, _rx) = std::sync::mpsc::channel();
        app.video_rec = Some(VideoRec {
            window: wid,
            deadline: now + Duration::from_secs(3),
            started_us: 0,
            keys: false,
            key_log: Vec::new(),
            pace_ticks: Vec::new(),
            unseamed_at_begin: 0,
            unlogged_other_window: 0,
            unlogged_over_cap: 0,
            mode,
            next_frame: None,
            presented: None,
            dir,
            handoff: None,
            cancel: VideoCancellation::new(),
            reply,
        });
        assert!(!app.video_abort_translucent_swapchain(opacity, blends));
        assert!(app.video_rec.take().is_some());
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn video_cancel_and_index_commit_have_one_atomic_winner() {
    // Schedule A: timeout/shutdown wins first. The worker cannot later
    // authorize the index rename and must abort its unpublished directory.
    let cancel_wins = VideoCancellation::new();
    cancel_wins.cancel();
    assert!(cancel_wins.is_cancelled());
    assert!(
        !cancel_wins.authorize_commit(),
        "a cancelled request cannot publish"
    );

    // Schedule B: the worker's single CAS before rename wins; a later timeout cannot revoke
    // it, and shutdown waits for its permit acknowledgement.
    let commit_wins = VideoCancellation::new();
    assert!(commit_wins.authorize_commit());
    commit_wins.cancel();
    assert!(
        !commit_wins.is_cancelled(),
        "commit authorization is irrevocable"
    );
    assert!(
        !commit_wins.authorize_commit(),
        "the one-shot boundary cannot be authorized twice"
    );
}

#[test]
fn window_close_app_drop_and_backend_rebuild_abort_recording_ownership() {
    // Survivor close: abort synchronously before WindowState/tap removal.
    let mut app = super::App::headless_for_test();
    let survivor_session = app.next_session_id;
    let _survivor = app.insert_logical_window(super::stub_session(survivor_session), 24, 80);
    let (root, path, dir) = test_video_dir("video-window-close");
    let (reply, rx) = std::sync::mpsc::channel();
    app.video_rec = Some(VideoRec {
        window: WindowId(0),
        deadline: Instant::now() + Duration::from_secs(60),
        started_us: 0,
        keys: false,
        key_log: Vec::new(),
        pace_ticks: Vec::new(),
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
        unlogged_over_cap: 0,
        mode: VideoMode::SwapchainTap,
        next_frame: Some(Instant::now()),
        presented: None,
        dir,
        handoff: None,
        cancel: VideoCancellation::new(),
        reply,
    });
    assert_eq!(
        app.close_window_logical(WindowId(0)),
        super::CloseOutcome::Stay
    );
    assert!(app.video_rec.is_none());
    assert_eq!(fold_video_deadlines(app.video_rec.as_ref(), None), None);
    assert!(!path.exists());
    assert_eq!(
        rx.recv().unwrap(),
        "ERR video: recording aborted because its window closed\n"
    );
    let _ = std::fs::remove_dir_all(root);

    // Both recording modes are aborted by the real backend rebuild before
    // WindowGpu replacement. Virtual additionally loses its timer target.
    for mode in [VideoMode::SwapchainTap, VideoMode::OffscreenPresentReal] {
        let mut app = super::App::headless_for_test();
        if mode == VideoMode::OffscreenPresentReal {
            app.windows.get_mut(&WindowId(0)).unwrap().present = Some(PresentTarget::Virtual {
                window_gpu: aterm_gpu::WindowGpu::new(),
            });
        }
        let (root, path, dir) = test_video_dir("video-backend-rebuild");
        let (reply, rx) = std::sync::mpsc::channel();
        app.video_rec = Some(VideoRec {
            window: WindowId(0),
            deadline: Instant::now() + Duration::from_secs(60),
            started_us: 0,
            keys: false,
            key_log: Vec::new(),
            pace_ticks: Vec::new(),
            unseamed_at_begin: 0,
            unlogged_other_window: 0,
            unlogged_over_cap: 0,
            mode,
            next_frame: Some(Instant::now()),
            presented: None,
            dir,
            handoff: None,
            cancel: VideoCancellation::new(),
            reply,
        });
        assert!(app.rebuild_backend(), "test backend rebuild succeeds");
        assert!(app.video_rec.is_none());
        assert_eq!(fold_video_deadlines(app.video_rec.as_ref(), None), None);
        assert!(!path.exists());
        assert_eq!(
            rx.recv().unwrap(),
            "ERR video: recording aborted by renderer backend rebuild\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    // Drop is the last fail-safe for test/embedding exits; production calls the same seam
    // before its deliberate mem::forget/process::exit.
    let mut app = super::App::headless_for_test();
    let (root, path, dir) = test_video_dir("video-app-drop");
    let (reply, rx) = std::sync::mpsc::channel();
    app.video_rec = Some(VideoRec {
        window: WindowId(0),
        deadline: Instant::now() + Duration::from_secs(60),
        started_us: 0,
        keys: false,
        key_log: Vec::new(),
        pace_ticks: Vec::new(),
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
        unlogged_over_cap: 0,
        mode: VideoMode::OffscreenPresentReal,
        next_frame: Some(Instant::now()),
        presented: None,
        dir,
        handoff: None,
        cancel: VideoCancellation::new(),
        reply,
    });
    drop(app);
    assert!(!path.exists());
    assert_eq!(
        rx.recv().unwrap(),
        "ERR video: recording aborted by application shutdown\n"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Idle-zero: during an offscreen recording the deadline fold arms the hard stop and
/// the paced `next_frame`; after `video_finalize` the Virtual target is dropped and the
/// fold contributes nothing, so `about_to_wait` waits.
#[test]
fn finalize_drops_virtual_target_and_timer() {
    let mut app = super::App::headless_for_test();
    let wid = WindowId(0);
    // Attach the glass-less Virtual target as the `Wake::Video` headless arm does.
    app.windows
        .get_mut(&wid)
        .expect("headless window 0")
        .present = Some(PresentTarget::Virtual {
        window_gpu: aterm_gpu::WindowGpu::new(),
    });
    let now = Instant::now();
    let (root, path, dir) = test_video_dir("video-finalize");
    let (reply, rx) = std::sync::mpsc::channel();
    app.video_rec = Some(VideoRec {
        window: wid,
        deadline: now + Duration::from_secs(3),
        started_us: 0,
        keys: false,
        key_log: Vec::new(),
        pace_ticks: Vec::new(),
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
        unlogged_over_cap: 0,
        mode: VideoMode::OffscreenPresentReal,
        next_frame: Some(now),
        presented: None,
        dir,
        handoff: None,
        cancel: VideoCancellation::new(),
        reply,
    });
    // In flight the fold arms the earlier of {deadline, next_frame}: the due paced frame.
    assert_eq!(
        fold_video_deadlines(app.video_rec.as_ref(), None),
        Some(now),
        "the offscreen loop's next_frame is folded into the single WaitUntil"
    );
    // It folds against other deadlines like every arm in `about_to_wait`.
    let later = now + Duration::from_secs(9);
    assert_eq!(
        fold_video_deadlines(app.video_rec.as_ref(), Some(later)),
        Some(now)
    );

    // Tier-1 (`VideoRecordingLifecycle`): project this App's recording: `mode` from the
    // window's present target (a `Virtual` target is the offscreen arm), `timer` from the
    // pacing deadline, `private_dirs` from the directory on disk.
    let model = aterm_models_misc::video_recording_lifecycle_model();
    let project = |app: &super::App, base: &aterm_spec::interp::State| {
        let mut state = base.clone();
        let offscreen = app
            .windows
            .get(&wid)
            .is_some_and(|ws| matches!(ws.present, Some(PresentTarget::Virtual { .. })));
        state.insert("mode", if offscreen { 2 } else { 0 });
        state.insert(
            "timer",
            i64::from(
                app.video_rec
                    .as_ref()
                    .is_some_and(|rec| rec.next_frame.is_some()),
            ),
        );
        state.insert("private_dirs", i64::from(path.exists()));
        state
    };
    let mut recording = model.init_state();
    for action in ["Reserve", "BeginHeadless"] {
        assert!(model.fire(action, &mut recording), "{action}");
    }
    assert_eq!(project(&app, &recording), recording);

    app.video_finalize();
    assert!(app.video_rec.is_none(), "finalize consumes the recording");
    assert!(
        app.windows.get(&wid).is_some_and(|ws| ws.present.is_none()),
        "finalize drops the Virtual target: the window returns to pure headless"
    );
    assert_eq!(
        fold_video_deadlines(app.video_rec.as_ref(), None),
        None,
        "idle-zero: nothing armed after finalize (about_to_wait -> Wait)"
    );
    // The CPU test backend has no GPU take: the client gets the honest recording-lost
    // error, not a silent empty artifact.
    let msg = rx.recv().expect("finalize answers the pending client");
    assert!(msg.starts_with("ERR video"), "honest reply, got: {msg}");
    // A lost take is the model's `Fail`, and what the shipping finalize
    // left behind is exactly that successor's target, timer and directory.
    let lost = model.successors("Fail", &recording)[0].clone();
    assert_eq!(project(&app, &lost), lost);
    // Negative control: a finalize without `ws.present = None` keeps the
    // recording-only Virtual target once no recording exists.
    let mut retained = lost;
    retained.insert("mode", 2);
    assert!(!model.check_invariant("ModeMatchesRecordingPhase", &retained));
    let _ = std::fs::remove_dir_all(root);
}

/// The video judge runs on every `new_events` cause: under sustained `WaitCancelled` the timer
/// cause never fires, so a non-timer wake must consume every due video clock.
#[test]
fn service_video_wake_consumes_due_clocks_on_any_wake() {
    // (a) A past hard-stop deadline finalizes without a timer wake.
    let mut app = super::App::headless_for_test();
    let (root, path, dir) = test_video_dir("video-wake-judge-finalize");
    let (reply, rx) = std::sync::mpsc::channel();
    let now = Instant::now();
    app.video_rec = Some(VideoRec {
        window: WindowId(0),
        deadline: now - Duration::from_millis(1),
        started_us: 0,
        keys: false,
        key_log: Vec::new(),
        pace_ticks: Vec::new(),
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
        unlogged_over_cap: 0,
        mode: VideoMode::SwapchainTap,
        next_frame: None,
        presented: None,
        dir,
        handoff: None,
        cancel: VideoCancellation::new(),
        reply,
    });
    app.service_video_wake();
    assert!(
        app.video_rec.is_none(),
        "a due deadline must be consumed on ANY wake, not only ResumeTimeReached"
    );
    assert_eq!(
        fold_video_deadlines(app.video_rec.as_ref(), None),
        None,
        "idle-zero: the judge leaves nothing armed once it finalizes"
    );
    // As above: no GPU take, so the honest recording-lost error.
    let msg = rx.recv().expect("the blocked client is answered");
    assert!(msg.starts_with("ERR video"), "honest reply, got: {msg}");
    assert!(!path.exists());
    let _ = std::fs::remove_dir_all(root);

    // (b) A due paced `next_frame` is consumed and re-armed ahead of now, never left in
    // the past for the fold to re-arm every turn; the recording lives on.
    let mut app = super::App::headless_for_test();
    let (root, _path, dir) = test_video_dir("video-wake-judge-pace");
    let (reply, _rx) = std::sync::mpsc::channel();
    let now = Instant::now();
    app.video_rec = Some(VideoRec {
        window: WindowId(0),
        deadline: now + Duration::from_secs(60),
        started_us: 0,
        keys: false,
        key_log: Vec::new(),
        pace_ticks: Vec::new(),
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
        unlogged_over_cap: 0,
        mode: VideoMode::OffscreenPresentReal,
        next_frame: Some(now - Duration::from_millis(5)),
        presented: None,
        dir,
        handoff: None,
        cancel: VideoCancellation::new(),
        reply,
    });
    app.service_video_wake();
    let rec = app.video_rec.as_ref().expect("pacing does not finalize");
    let next = rec
        .next_frame
        .expect("the paced recording loop stays armed");
    assert!(
        next > now,
        "the past next_frame is consumed and re-armed into the future"
    );
    assert!(
        rec.pace_ticks.is_empty(),
        "this test window has no present target, so the tick's redraw neither \
         presented nor PROVED the screen unchanged: it sampled nothing and books no \
         tick — a render stall under a live loop reads as a hole"
    );
    // A tick whose redraw reached the RepaintKey early-out (the screen proved unchanged) is
    // booked; a redraw that did not move the count books nothing.
    let wid = WindowId(0);
    let before = app.windows.get(&wid).map(|ws| ws.redraws_proved_unchanged);
    app.book_video_tick_if_proved_unchanged(wid, 42, before);
    assert!(app.video_rec.as_ref().unwrap().pace_ticks.is_empty());
    app.windows
        .get_mut(&wid)
        .expect("headless window 0")
        .redraws_proved_unchanged += 1;
    app.book_video_tick_if_proved_unchanged(wid, 42, before);
    assert_eq!(
        app.video_rec.as_ref().unwrap().pace_ticks,
        vec![42],
        "a proved-unchanged tick is booked at the instant it asked"
    );
    drop(app); // the drop fail-safe answers the client; not under test here
    let _ = std::fs::remove_dir_all(root);

    // (c) Nothing due: a spurious wake changes nothing.
    let mut app = super::App::headless_for_test();
    let (root, _path, dir) = test_video_dir("video-wake-judge-idle");
    let (reply, _rx) = std::sync::mpsc::channel();
    let now = Instant::now();
    let armed_frame = now + Duration::from_secs(30);
    app.video_rec = Some(VideoRec {
        window: WindowId(0),
        deadline: now + Duration::from_secs(60),
        started_us: 0,
        keys: false,
        key_log: Vec::new(),
        pace_ticks: Vec::new(),
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
        unlogged_over_cap: 0,
        mode: VideoMode::OffscreenPresentReal,
        next_frame: Some(armed_frame),
        presented: None,
        dir,
        handoff: None,
        cancel: VideoCancellation::new(),
        reply,
    });
    app.service_video_wake();
    let rec = app.video_rec.as_ref().expect("nothing due: recording kept");
    assert_eq!(
        rec.next_frame,
        Some(armed_frame),
        "an undue paced frame is left exactly as armed"
    );
    assert!(
        rec.pace_ticks.is_empty(),
        "a wake with nothing due books no tick"
    );
    drop(app);
    let _ = std::fs::remove_dir_all(root);
}

/// GPU loss cannot leave a headless Virtual recording paced forever on the CPU backend:
/// the abort seam removes the target and its timer and answers the blocked client.
#[test]
fn device_loss_aborts_virtual_recording_without_a_timer_wedge() {
    let model = aterm_models_core::gpu_loss_recovery_model();
    let expected = model.successors("FailFallbackAfterPresent", &model.init_state())[0].clone();
    let mut app = super::App::headless_for_test();
    let wid = WindowId(0);
    app.windows
        .get_mut(&wid)
        .expect("headless window 0")
        .present = Some(PresentTarget::Virtual {
        window_gpu: aterm_gpu::WindowGpu::new(),
    });
    let now = Instant::now();
    let (root, _path, dir) = test_video_dir("video-device-loss");
    let (reply, rx) = std::sync::mpsc::channel();
    app.video_rec = Some(VideoRec {
        window: wid,
        deadline: now + Duration::from_secs(3),
        started_us: 0,
        keys: false,
        key_log: Vec::new(),
        pace_ticks: Vec::new(),
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
        unlogged_over_cap: 0,
        mode: VideoMode::OffscreenPresentReal,
        next_frame: Some(now),
        presented: None,
        dir,
        handoff: None,
        cancel: VideoCancellation::new(),
        reply,
    });

    let observed = app.complete_latched_gpu_loss_recovery(wid, None, now, |_, source, counted| {
        assert_eq!(source, wid);
        assert!(!counted);
        GpuRecoveryOutcome::BackendUnavailable
    });
    let GpuRecoveryOutcome::SourceRetry(observed) = observed else {
        panic!("failed CPU fallback must own a typed retry: {observed:?}");
    };

    assert!(app.video_rec.is_none());
    assert_eq!(
        expected["recording"],
        i64::from(u8::from(app.video_rec.is_some()))
    );
    assert!(app.windows.get(&wid).is_some_and(|ws| ws.present.is_none()));
    assert_eq!(observed.accounting, PresentDropAccounting::Count);
    assert_eq!(
        observed.reason,
        crate::metrics::PresentDropReason::CpuAcquire
    );
    assert!(!observed.parked);
    assert!(app.windows[&wid].present_retry.deadline.is_some());
    assert_eq!(fold_video_deadlines(app.video_rec.as_ref(), None), None);
    assert_eq!(
        rx.recv().expect("device loss answers the pending client"),
        "ERR video: recording aborted by GPU device loss\n"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The mode labels are the disclosure contract's exact strings — index.json
/// consumers key on them.
#[test]
fn video_mode_labels_are_the_disclosure_contract() {
    assert_eq!(VideoMode::SwapchainTap.as_str(), "swapchain-tap");
    assert_eq!(
        VideoMode::OffscreenPresentReal.as_str(),
        "offscreen-present-real"
    );
}
