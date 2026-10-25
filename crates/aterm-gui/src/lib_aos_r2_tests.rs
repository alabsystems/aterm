// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-path tests for Rewind R2: repair off the main thread, the watcher, privacy and the new `aos` forms.

use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use aterm_effects::aos_momentum::{MomentumCode, MomentumEvent, MomentumTrace};
use aterm_session::sink::SinkWriter;

use super::aos_repair_tests::{
    KEY_GAP, WID, exported_log, momentum_app, momentum_app_on, status, stranded, type_keys,
};
use crate::App;
use crate::aos_host::{AosRefusal, PRIVACY_DETACH_OFF, Worked};
use crate::scripted_redraw_fixture::scripted_app_with_sink;

/// The keys in a log.
fn keys(log: &[MomentumEvent]) -> usize {
    log.iter()
        .filter(|e| matches!(e, MomentumEvent::Key { .. }))
        .count()
}

/// A stranded app paused for a repair at `later`, with the job's prepare made on another thread.
fn staged(
    t0: Instant,
) -> (
    App,
    u64,
    aterm_effects::aos_momentum::PreparedRepair,
    std::sync::mpsc::Receiver<Result<String, String>>,
    Instant,
) {
    let (mut app, parked) = stranded(false, t0);
    let later = parked.at + Duration::from_millis(500);
    let (tx, rx) = channel();
    let (id, token, job) = app
        .aos_repair_stage(MomentumCode::Shipped, later, tx)
        .map_err(|(_, r)| r)
        .expect("a stranded glow stages a repair");
    assert_eq!(id, WID);
    let prepared = std::thread::spawn(move || job.prepare())
        .join()
        .expect("the worker ran");
    (app, token, prepared, rx, later)
}

#[test]
fn a_repair_prepared_off_the_main_thread_commits_with_no_replay_on_it() {
    let t0 = Instant::now();
    let (mut app, token, prepared, rx, later) = staged(t0);
    assert_eq!(
        prepared.replays(),
        2,
        "both shadow replays ran on the worker"
    );
    let paused = status(&app);
    assert!(paused.paused, "the stage paused the running glow");
    assert_eq!(paused.stats.replays, 0, "the stage replayed nothing");
    assert_eq!(
        app.aos_repair_at(MomentumCode::Shipped, later)
            .map(|_| ())
            .err(),
        Some(AosRefusal::Repairing),
        "one repair at a time"
    );
    app.aos_repair_finish(WID, token + 1, Err("a stale token".into()));
    assert!(rx.try_recv().is_err(), "a stale token is ignored");
    app.aos_repair_finish(WID, token, Ok(prepared));
    let line = rx
        .try_recv()
        .expect("the finish answers")
        .expect("the repair commits");
    assert!(
        line.starts_with("window=0 repaired paused_first=true rev=1 code=momentum-glow.shipped "),
        "{line}"
    );
    assert!(line.contains(" live_verdict=fail:HotHasWake@"), "{line}");
    assert!(
        line.contains(" main_us=") && line.ends_with(" off_main=true"),
        "{line}"
    );
    let committed = status(&app);
    assert_eq!(
        (committed.stats.replays, committed.stats.publications),
        (0, 1),
        "the main thread committed and replayed nothing"
    );
    assert!(!committed.paused && committed.code == "shipped");

    let (mut inplace, parked) = stranded(false, t0);
    let line = inplace
        .control_aos(
            crate::aos_verb::Asked::Repair(MomentumCode::Shipped),
            parked.at + Duration::from_millis(500),
        )
        .expect("repairs in place");
    assert!(line.ends_with(" off_main=false"), "{line}");
    assert_eq!(
        status(&inplace).stats.replays,
        2,
        "negative control: the in-place repair replays on the main thread"
    );
}

#[test]
fn negative_control_a_head_moved_before_the_finish_is_refused_and_keeps_the_new_pause() {
    let t0 = Instant::now();
    let (mut app, token, prepared, rx, later) = staged(t0);
    app.aos_resume_now().expect("resumes");
    let at = type_keys(&mut app, later + KEY_GAP, 2);
    let (_, moved) = app.aos_pause_at(at).expect("pauses again");
    app.aos_repair_finish(WID, token, Ok(prepared));
    let refused = rx.try_recv().expect("answers").expect_err("refused");
    assert!(
        refused.contains("stale") || refused.contains("head"),
        "a head moved since the job: {refused}"
    );
    let s = status(&app);
    assert!(s.paused, "the caller's later pause is kept");
    assert_eq!(s.code, "demo-stranded-settle");
    assert_eq!(s.stats.publications, 0);
    assert_eq!(
        app.windows[&WID].fx.cursor().momentum.aos_paused_head(),
        Some(&moved)
    );

    let (mut app, token, prepared, rx, _) = staged(t0);
    app.aos_repair_finish(WID, token, Err("spawn refused".into()));
    let refused = rx.try_recv().expect("answers").expect_err("refused");
    assert!(refused.contains("host worker did not run"), "{refused}");
    assert!(
        !status(&app).paused,
        "a repair that paused the glow resumes it on a refusal"
    );
    drop(prepared);
}

#[test]
fn with_no_event_loop_proxy_the_repair_ask_answers_in_place() {
    let t0 = Instant::now();
    let (mut app, parked) = stranded(false, t0);
    assert!(app.proxy.is_none(), "fixture: a test app has no proxy");
    let (tx, rx) = channel();
    app.aos_repair_begin(
        MomentumCode::Shipped,
        parked.at + Duration::from_millis(500),
        tx,
    );
    let line = rx.try_recv().expect("answered at once").expect("repairs");
    assert!(line.ends_with(" off_main=false"), "{line}");
}

#[test]
fn the_repair_ask_prepares_on_a_host_worker_and_answers_once_the_main_thread_commits() {
    let t0 = Instant::now();
    let (mut app, parked) = stranded(false, t0);
    let (reply, answer) = channel();
    let (post, worked) = channel();
    app.aos_repair_via(
        MomentumCode::Shipped,
        parked.at + Duration::from_millis(500),
        reply,
        Some(move |w: Worked| {
            let _ = post.send(w);
        }),
    );
    assert!(answer.try_recv().is_err(), "no answer before the worker's");
    let w = worked
        .recv_timeout(Duration::from_secs(60))
        .expect("the worker posts its prepare");
    let staged = status(&app);
    assert!(staged.paused, "paused while the worker prepared");
    assert_eq!(staged.stats.replays, 0, "the main thread replayed nothing");
    app.aos_worked(w);
    let line = answer
        .try_recv()
        .expect("the commit answers")
        .expect("repairs");
    assert!(line.ends_with(" off_main=true"), "{line}");
    let committed = status(&app);
    assert_eq!(
        (committed.stats.replays, committed.stats.publications),
        (0, 1)
    );

    let (mut inplace, parked) = stranded(false, t0);
    let (reply, answer) = channel();
    inplace.aos_repair_via(
        MomentumCode::Shipped,
        parked.at + Duration::from_millis(500),
        reply,
        None::<fn(Worked)>,
    );
    let line = answer
        .try_recv()
        .expect("answered at once")
        .expect("repairs");
    assert!(line.ends_with(" off_main=false"), "{line}");
    assert_eq!(
        status(&inplace).stats.replays,
        2,
        "negative control: with no worker both replays run on the main thread"
    );
}

/// A momentum app whose session's sink the test holds.
fn held_sink_app() -> (App, Arc<SinkWriter>) {
    let sink = Arc::new(SinkWriter::new(-1));
    let app = momentum_app_on(scripted_app_with_sink(sink.clone()), false);
    (app, sink)
}

/// Types `n` keys with the tty not echoing them, then `m` echoed keys after a re-attach, and returns the keys the re-attached log holds and whether the glow stayed attached through the span.
fn no_echo_span(n: u32, m: u32) -> (App, bool, usize, usize) {
    let t0 = Instant::now();
    let (mut app, sink) = held_sink_app();
    app.redraw_window_at(WID, t0);
    app.aos_attach_at(MomentumCode::Shipped, t0)
        .expect("attaches");
    let at = type_keys(&mut app, t0 + KEY_GAP, 3);
    assert_eq!(
        keys(&exported_log(&app, t0)),
        3,
        "fixture: echoed keys are logged"
    );
    sink.set_tty_swallows_hint(true);
    let at = type_keys(&mut app, at + KEY_GAP, n);
    let stayed = status(&app).attached;
    let in_span = if stayed {
        keys(&exported_log(&app, t0))
    } else {
        0
    };
    sink.set_tty_swallows_hint(false);
    if !stayed {
        app.aos_attach_at(MomentumCode::Shipped, at)
            .expect("re-attaches");
    }
    let _ = type_keys(&mut app, at + KEY_GAP, m);
    let after = keys(&exported_log(&app, at));
    (app, stayed, in_span, after)
}

#[test]
fn no_key_of_a_no_echo_span_is_logged_and_the_detach_is_a_timeline_row() {
    let (app, stayed, _, after) = no_echo_span(4, 2);
    assert!(!stayed, "the first no-echo key detached the recording");
    assert_eq!(
        after, 2,
        "the re-attached log holds only the echoed keys after the span"
    );
    let lines = app.aos_ls_now();
    assert!(
        lines
            .iter()
            .any(|l| l == "slot=0 row=0 detached why=no-echo_key"),
        "{lines:?}"
    );
}

#[test]
fn negative_control_with_the_detach_removed_the_no_echo_keys_are_logged() {
    PRIVACY_DETACH_OFF.with(|off| off.set(true));
    let (_, stayed, in_span, _) = no_echo_span(4, 0);
    PRIVACY_DETACH_OFF.with(|off| off.set(false));
    assert!(stayed, "nothing detached the recording");
    assert_eq!(in_span, 3 + 4, "each no-echo key was logged");
}

#[test]
fn secure_keyboard_entry_refuses_an_attach_and_a_key_under_it_detaches() {
    let t0 = Instant::now();
    let mut app = momentum_app(false);
    app.redraw_window_at(WID, t0);
    app.config.secure_keyboard_entry = Some(true);
    assert_eq!(
        app.aos_attach_at(MomentumCode::Shipped, t0).err(),
        Some(AosRefusal::SecureEntry)
    );
    app.config.secure_keyboard_entry = Some(false);
    app.aos_attach_at(MomentumCode::Shipped, t0)
        .expect("attaches");
    let at = type_keys(&mut app, t0 + KEY_GAP, 2);
    app.config.secure_keyboard_entry = Some(true);
    let _ = type_keys(&mut app, at + KEY_GAP, 1);
    assert!(
        !status(&app).attached,
        "the key under secure entry detached"
    );
    assert!(
        app.aos_ls_now()
            .iter()
            .any(|l| l == "slot=0 row=0 detached why=secure_keyboard_entry")
    );
    app.config.secure_keyboard_entry = Some(false);
    app.aos_attach_at(MomentumCode::Shipped, at)
        .expect("attaches");
    app.aos_privacy_detach_all(crate::aos_host::SECURE_ENTRY);
    assert!(
        !status(&app).attached,
        "a config change to secure entry detaches all"
    );
}

#[test]
fn sparkles_paper_gold_is_not_recorded_and_putting_it_on_detaches_with_a_timeline_row() {
    let t0 = Instant::now();
    let mut gold = momentum_app(true);
    gold.config.cursor_trail_style = Some("sparkle".into());
    gold.kitty_cursor_enabled_cache = None;
    gold.recompute_sparkle();
    gold.redraw_window_at(WID, t0);
    assert!(
        gold.windows[&WID].fx.cursor().momentum.light_vivid(),
        "fixture: Sparkle on a light theme wears its paper gold"
    );
    assert_eq!(
        gold.aos_attach_at(MomentumCode::Shipped, t0).err(),
        Some(AosRefusal::VividInk)
    );
    assert!(!status(&gold).attached);

    let mut app = momentum_app(true);
    app.redraw_window_at(WID, t0);
    app.aos_attach_at(MomentumCode::Shipped, t0)
        .expect("the walk-toward-white ink attaches");
    let at = type_keys(&mut app, t0 + KEY_GAP, 2);
    app.windows
        .get_mut(&WID)
        .expect("window")
        .fx
        .cursor_mut()
        .momentum
        .set_light_vivid(true);
    assert!(!status(&app).attached, "putting the ink on detached");
    app.aos_watch();
    assert!(
        app.aos_ls_now()
            .iter()
            .any(|l| l == "slot=0 row=0 detached why=vivid_light-page_ink"),
        "{:?}",
        app.aos_ls_now()
    );
    assert_eq!(
        app.aos_attach_at(MomentumCode::Shipped, at).err(),
        Some(AosRefusal::VividInk)
    );
}

#[test]
fn the_watcher_keeps_the_demonstration_faults_segment_and_lists_it_with_no_alert() {
    let t0 = Instant::now();
    let (mut app, parked) = stranded(false, t0);
    app.aos_repair_at(
        MomentumCode::Shipped,
        parked.at + Duration::from_millis(500),
    )
    .expect("repairs");
    assert_eq!(
        app.windows[&WID].fx.cursor().momentum.aos_retired_waiting(),
        1,
        "the commit retired the live code's segment"
    );
    let before = app.aos_ls_now();
    assert!(
        before[1].contains(" waiting=1 kept=0 rows=0 "),
        "{before:?}"
    );
    app.aos_watch();
    let lines = app.aos_ls_now();
    assert_eq!(lines[0], "tasks=1");
    assert!(
        lines[1].starts_with(
            "slot=0 kind=effect task=aterm.cursor.momentum-glow code=shipped attached=true "
        ),
        "{lines:?}"
    );
    assert!(
        lines[1].contains(" waiting=0 kept=1 rows=1 dropped=0"),
        "{lines:?}"
    );
    assert!(
        lines[2].starts_with("slot=0 row=0 segment cause=committed code=demo-stranded-settle ")
            && lines[2].contains(" judged=fail:HotHasWake@")
            && lines[2].ends_with(" kept=0"),
        "{lines:?}"
    );

    let kept = app.aos_export_now(Some(0)).expect("kept fault 0 exports");
    assert_eq!(kept.kept, Some(0));
    let trace = MomentumTrace::decode(&kept.bytes).expect("decodes");
    let build = status(&app).build.expect("attached");
    let check = trace.check(t0, &build).expect("this build replays it");
    assert!(matches!(
        check.verdict,
        aterm_aos::contract::Verdict::Fail {
            law: "HotHasWake",
            ..
        }
    ));
    assert_eq!(
        app.aos_export_now(Some(7)).err(),
        Some(AosRefusal::NoKept(7))
    );
    let live = app.aos_export_now(None).expect("the live trace exports");
    assert_eq!(live.kept, None);
}

#[cfg(unix)]
#[test]
fn an_export_is_a_private_file_and_check_replays_it_and_refuses_another_builds_by_identity() {
    use std::os::unix::fs::PermissionsExt;

    use aterm_effects::aos_momentum::MomentumHost;

    use crate::aos_verb::{check_file, check_path, write_export};

    let t0 = Instant::now();
    let mut app = momentum_app(false);
    app.redraw_window_at(WID, t0);
    app.aos_attach_at(MomentumCode::Shipped, t0)
        .expect("attaches");
    let _ = type_keys(&mut app, t0 + KEY_GAP, 5);
    let exported = app.aos_export_now(None).expect("exports");
    let root = aterm_tempfile::tempdir().expect("a scratch state dir");
    let path = write_export(root.path(), "ours.trace", &exported.bytes).expect("writes");
    let mode = std::fs::metadata(&path)
        .expect("written")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "the export is private");
    let dir_mode = std::fs::metadata(path.parent().expect("dir"))
        .expect("dir")
        .permissions()
        .mode();
    assert_eq!(dir_mode & 0o777, 0o700, "under a private directory");
    assert!(
        write_export(root.path(), "ours.trace", &exported.bytes).is_err(),
        "an export never replaces a file"
    );
    let build = status(&app).build.expect("attached");
    let bare = check_path(Some(root.path()), "ours.trace").expect("a bare name");
    assert_eq!(
        std::fs::canonicalize(&bare).ok(),
        std::fs::canonicalize(&path).ok()
    );
    let line = check_file(&bare, &build).expect("this build checks its own export");
    assert!(
        line.starts_with("checked code=shipped log_epoch=1 first=0 keys=5 ")
            && line.contains(" verdict=pass ")
            && line.ends_with(" file=ours.trace"),
        "{line}"
    );

    let mut foreign = MomentumHost::default();
    foreign.aos_attach(t0, MomentumCode::Shipped, "another-build", 1);
    foreign.on_key(
        t0 + KEY_GAP,
        aterm_effects::cursor_momentum::MOMENTUM_GLOW_TAU_S,
    );
    let theirs = foreign.aos_trace().expect("exports").encode();
    let other = write_export(root.path(), "theirs.trace", &theirs).expect("writes");
    let refused = check_file(&other, &build).expect_err("another build's trace");
    assert!(
        refused.starts_with("aos check refused=identity: "),
        "{refused}"
    );
    assert!(
        check_file(&other, "another-build").is_ok(),
        "negative control: the build that wrote it checks it"
    );
    assert!(check_path(Some(root.path()), "a/b.trace").is_err());
    assert!(check_path(None, "ours.trace").is_err());
    assert_eq!(
        check_path(None, "/abs/x.trace").ok(),
        Some(std::path::PathBuf::from("/abs/x.trace"))
    );
}
