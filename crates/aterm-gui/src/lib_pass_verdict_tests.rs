// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{PassRun, PassVerdict, SeedMarkers, classify_pass_exit};
use std::time::Duration;

/// Verifies `classify_pass_exit` on labelled `(exit_code, card_opened, marker_seen)` rows that
/// reach each of its verdicts.
#[test]
fn every_pass_exit_classifies_to_its_verdict() {
    let busy = Some(i32::from(atpkg::lock::CONTENDED_EXIT));
    let offline = Some(i32::from(aterm_update_core::pkg_check::PASS_OFFLINE_EXIT));
    assert_eq!(busy, Some(75));
    assert_eq!(offline, Some(69));
    type Row = (&'static str, Option<i32>, bool, bool, PassVerdict);
    #[rustfmt::skip]
    let rows: [Row; 15] = [
        ("contention is Busy, never Refused (2026-09-10)", busy, false, false, PassVerdict::Busy),
        ("a healthy quiet launch raises nothing", Some(0), false, false, PassVerdict::Quiet),
        ("an io refusal (1) is still refused", Some(1), false, false, PassVerdict::Refused),
        ("an io refusal (2) is still refused", Some(2), false, false, PassVerdict::Refused),
        ("a signalled child is refused", None, false, false, PassVerdict::Refused),
        ("an opened card, exit 0", Some(0), true, false, PassVerdict::AnnouncedThenDied),
        ("an opened card, exit 1", Some(1), true, false, PassVerdict::AnnouncedThenDied),
        ("an opened card, exit 75", busy, true, false, PassVerdict::AnnouncedThenDied),
        ("an opened card, signalled", None, true, false, PassVerdict::AnnouncedThenDied),
        ("a marker outranks a failing code", Some(1), false, true, PassVerdict::Answered),
        ("a marker answers an opened card", Some(0), true, true, PassVerdict::Answered),
        ("the contention refusal's own terminal is not a pass that ran", busy, false, true, PassVerdict::Busy),
        ("offline, no marker", offline, false, false, PassVerdict::Offline),
        ("offline, a marker beside an opened card", offline, true, true, PassVerdict::Offline),
        ("offline under an opened, unanswered card", offline, true, false, PassVerdict::AnnouncedThenDied),
    ];
    for (label, code, card, marker, verdict) in rows {
        assert_eq!(classify_pass_exit(code, card, marker), verdict, "{label}");
    }
    let offline = run(offline, SeedMarkers::default(), "");
    assert!(offline.ran() && !offline.ok() && !offline.clean());
}

use super::{PassVerb, Wake, carry_wait_row, report_pass_verdict};

fn run(code: Option<i32>, seen: SeedMarkers, said: &str) -> PassRun {
    PassRun {
        seen,
        code,
        holder_advanced: false,
        said: said.to_string(),
        pass_seq_before: None,
    }
}

/// `carry_wait_row` clears the progress bar once after a pass runs or dies, and keeps it open on `PassVerdict::Busy`.
#[test]
fn a_waited_pass_that_ran_retires_its_row_and_busy_carries_it() {
    for verdict in [
        PassVerdict::Quiet,
        PassVerdict::Answered,
        PassVerdict::Refused,
        PassVerdict::AnnouncedThenDied,
        PassVerdict::Offline,
    ] {
        let mut posted = Vec::new();
        let open = carry_wait_row(false, true, verdict, |e| posted.push(e));
        assert!(!open, "{verdict:?}: the row is retired");
        assert!(
            matches!(
                posted.as_slice(),
                [Wake::PkgProgress { snapshot: None, .. }]
            ),
            "{verdict:?}: exactly the clear is posted: {posted:?}"
        );
        let mut posted = Vec::new();
        assert!(!carry_wait_row(true, false, verdict, |e| posted.push(e)));
        assert_eq!(posted.len(), 1, "{verdict:?}: {posted:?}");
    }
    let mut posted = Vec::new();
    assert!(
        carry_wait_row(false, true, PassVerdict::Busy, |e| posted.push(e)),
        "Busy keeps the row open"
    );
    assert!(posted.is_empty(), "Busy posts nothing: {posted:?}");
    let mut posted = Vec::new();
    assert!(
        !carry_wait_row(false, false, PassVerdict::Quiet, |e| posted.push(e)),
        "no row, nothing to retire"
    );
    assert!(posted.is_empty(), "{posted:?}");
}

/// Markerless refusals report `Wake::PkgSeedFailed` for whole-store passes and log-only for single-program updates.
#[test]
fn a_refused_whole_pass_is_a_failure_and_a_targeted_one_is_its_row() {
    let refused = run(
        Some(1),
        SeedMarkers::default(),
        "atpkg: prefix is not writable\n",
    );
    assert_eq!(refused.verdict(), PassVerdict::Refused);
    for (verb, failure) in [
        (PassVerb::Seed, true),
        (PassVerb::Update, true),
        (PassVerb::UpdateProgram("codex"), false),
    ] {
        assert_eq!(verb.refusal_is_a_failure(), failure, "{verb}");
        let mut posted = Vec::new();
        report_pass_verdict(verb, &refused, None, |e| posted.push(e));
        if failure {
            assert!(
                matches!(
                    posted.as_slice(),
                    [Wake::PkgSeedFailed { detail }] if detail == "atpkg: prefix is not writable"
                ),
                "{verb}: the refusal is a failure carrying the trimmed stderr: {posted:?}"
            );
        } else {
            assert!(posted.is_empty(), "{verb}: a log line only: {posted:?}");
        }
    }
    let killed = run(None, SeedMarkers::default(), "");
    assert_eq!(killed.verdict(), PassVerdict::Refused);
    let mut posted = Vec::new();
    report_pass_verdict(PassVerb::Update, &killed, None, |e| posted.push(e));
    assert!(
        matches!(
            posted.as_slice(),
            [Wake::PkgSeedFailed { detail }] if detail == "atpkg ended without saying what happened"
        ),
        "{posted:?}"
    );
    let quiet = run(Some(0), SeedMarkers::default(), "");
    assert_eq!(quiet.verdict(), PassVerdict::Quiet);
    let mut posted = Vec::new();
    report_pass_verdict(PassVerb::Seed, &quiet, None, |e| posted.push(e));
    assert!(
        posted.is_empty(),
        "a quiet launch raises nothing: {posted:?}"
    );
}

/// An exit-1 pass with empty stderr whose new status record names a cached index posts no failure.
#[test]
fn a_cached_index_pass_is_quiet_and_names_its_cause_regression_2026_09_24() {
    use super::cached_index_cause;
    let dir = std::env::temp_dir().join(format!("aterm-gui-cached-index-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let layout = atpkg::store::Layout {
        prefix: dir.join("pkg"),
    };
    std::fs::create_dir_all(&layout.prefix).unwrap();
    let record = |pass_seq: u64, last_pass: &str, outcome: &str| {
        let status = atpkg::status::Status {
            schema: 1,
            outcome: outcome.to_string(),
            last_pass: last_pass.to_string(),
            last_pass_at: "2026-09-24T20:35:33Z".to_string(),
            pass_seq,
            ..atpkg::status::Status::default()
        };
        atpkg::status::write(&layout, &status).unwrap();
        status
    };
    let cached = "up to date (index build 46) (index from cache \u{2014} GitHub rate limit \
                  hit (HTTP 403))";
    let before = |seq: u64| PassRun {
        pass_seq_before: Some(seq),
        ..run(Some(1), SeedMarkers::default(), "")
    };
    let posted_by = |run: &PassRun| {
        let mut posted = Vec::new();
        report_pass_verdict(PassVerb::Update, run, Some(&layout), |e| posted.push(e));
        posted
    };

    let this_pass = record(8, "failed", cached);
    assert_eq!(before(7).verdict(), PassVerdict::Refused);
    assert_eq!(
        cached_index_cause(Some(7), &this_pass).as_deref(),
        Some("GitHub rate limit hit (HTTP 403)"),
        "the recorded cause, the clause's closing paren dropped"
    );
    assert!(
        posted_by(&before(7)).is_empty(),
        "a stale-index pass posts no failure"
    );
    assert!(
        posted_by(&PassRun {
            pass_seq_before: None,
            ..before(0)
        })
        .is_empty()
    );

    assert_eq!(cached_index_cause(Some(8), &this_pass), None);
    assert!(
        matches!(
            posted_by(&before(8)).as_slice(),
            [Wake::PkgSeedFailed { detail }] if detail == "atpkg ended without saying what happened"
        ),
        "an unmoved record is not this pass's"
    );
    let live = record(
        9,
        "failed",
        "1 program(s) failed: codex \u{2014} see their rows",
    );
    assert_eq!(cached_index_cause(Some(8), &live), None);
    assert!(matches!(
        posted_by(&before(8)).as_slice(),
        [Wake::PkgSeedFailed { .. }]
    ));
    let ok = record(10, "ok", cached);
    assert_eq!(cached_index_cause(Some(9), &ok), None);
    record(11, "failed", cached);
    let said = PassRun {
        pass_seq_before: Some(10),
        ..run(
            Some(1),
            SeedMarkers::default(),
            "atpkg: prefix is not writable\n",
        )
    };
    assert!(
        matches!(
            posted_by(&said).as_slice(),
            [Wake::PkgSeedFailed { detail }] if detail == "atpkg: prefix is not writable"
        ),
        "stderr outranks the record"
    );
    let bare = record(12, "failed", "up to date (index from cache \u{2014} )");
    assert_eq!(
        cached_index_cause(Some(11), &bare).as_deref(),
        Some("the index was not reached")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Standing down behind a sibling pass that failed only due to a cached index posts no failure.
#[test]
fn a_stand_down_behind_a_siblings_cached_index_pass_is_quiet_regression_2026_09_26() {
    use aterm_update_core::pkg_check::PassOutcome;
    let dir = std::env::temp_dir().join(format!(
        "aterm-gui-stood-down-cached-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let layout = atpkg::store::Layout {
        prefix: dir.join("pkg"),
    };
    std::fs::create_dir_all(&layout.prefix).unwrap();
    let cached = "up to date (index build 46) (index from cache \u{2014} GitHub rate limit \
                  hit (HTTP 403))";
    let sibling_ended = |stale_only: bool| {
        let status = atpkg::status::Status {
            schema: 1,
            outcome: cached.to_string(),
            last_pass: PassOutcome::Failed.word().to_string(),
            last_pass_at: "2026-09-26T10:00:00Z".to_string(),
            pass_seq: 8,
            stale_index_only_pass_seq: if stale_only { 8 } else { 0 },
            ..atpkg::status::Status::default()
        };
        atpkg::status::write(&layout, &status).unwrap();
        status
    };
    let stood_down = |record: &atpkg::status::Status| {
        let exit = atpkg::cli::stand_down_exit(
            PassOutcome::Failed,
            3,
            &record.outcome,
            atpkg::status::stale_index_only_end(record),
        );
        let said = if exit.on_stderr {
            format!("{}\n", exit.line)
        } else {
            String::new()
        };
        PassRun {
            pass_seq_before: Some(7),
            ..run(
                Some(i32::from(exit.code)),
                SeedMarkers {
                    saw_lock_wait: true,
                    ..SeedMarkers::default()
                },
                &said,
            )
        }
    };
    let posted_by = |run: &PassRun| {
        let mut posted = Vec::new();
        report_pass_verdict(PassVerb::Update, run, Some(&layout), |e| posted.push(e));
        posted
    };

    let stale = stood_down(&sibling_ended(true));
    assert_eq!(stale.verdict(), PassVerdict::Refused, "exit 1, no terminal");
    assert!(
        posted_by(&stale).is_empty(),
        "a stand-down behind a stale-index pass posts no failure: {:?}",
        stale.said
    );

    let failed = stood_down(&sibling_ended(false));
    assert!(!failed.said.is_empty(), "the stand-down speaks on stderr");
    assert!(
        matches!(
            posted_by(&failed).as_slice(),
            [Wake::PkgSeedFailed { detail }]
                if detail.contains("the last update failed")
        ),
        "a real failure behind the wait still reaches the card"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `PassRun::ran` is false only when the child exited `75` (`CONTENDED_EXIT`).
#[test]
fn a_pass_ran_unless_the_child_stood_aside_at_the_lock() {
    let run = |code: Option<i32>, seen: SeedMarkers| PassRun {
        seen,
        code,
        holder_advanced: false,
        said: String::new(),
        pass_seq_before: None,
    };
    let stood_aside = SeedMarkers {
        saw_lock_wait: true,
        saw_terminal: true,
        saw_busy: true,
        ..SeedMarkers::default()
    };
    assert!(
        !run(Some(75), stood_aside).ran(),
        "stood aside, with its terminal"
    );
    assert!(
        !run(Some(75), SeedMarkers::default()).ran(),
        "stood aside without one: still not a pass"
    );
    assert!(run(Some(0), SeedMarkers::default()).ran(), "quiet");
    assert!(
        run(
            Some(0),
            SeedMarkers {
                saw_start: true,
                saw_terminal: true,
                ..SeedMarkers::default()
            }
        )
        .ran(),
        "answered"
    );
    assert!(
        run(Some(1), SeedMarkers::default()).ran(),
        "refused: it may have run"
    );
    assert!(
        run(None, SeedMarkers::default()).ran(),
        "signalled: likewise"
    );
    assert!(
        run(
            Some(0),
            SeedMarkers {
                saw_lock_wait: true,
                ..SeedMarkers::default()
            }
        )
        .ran(),
        "waited, then ran quietly: a pass"
    );
}

use super::{CONTENTION_WEDGE_PARK, ContentionBackoff, PkgLane};
use std::sync::{Arc, Mutex};

/// Builds a [`PkgLane`] backed by `posted` with `wait_row_open` set.
fn busy_lane(
    posted: &Arc<Mutex<Vec<Wake>>>,
) -> PkgLane<'static, impl Fn(Wake) + Clone + Send + 'static + use<>> {
    let sink = Arc::clone(posted);
    PkgLane {
        atpkg: std::path::Path::new("atpkg"),
        child_path: |_| "".into(),
        layout: None,
        private_state_root: false,
        post: move |e| sink.lock().unwrap().push(e),
        wait_row_open: true,
        backoff: ContentionBackoff::default(),
        ran_at: None,
        switch: super::LiveSwitch::new(None, super::LoopGate::On),
        ahead: false,
    }
}

/// The one deferred notice posted, and whether it stands down (the badge).
fn deferred(posted: &Arc<Mutex<Vec<Wake>>>) -> (String, bool) {
    let mut posted = posted.lock().unwrap();
    match posted.drain(..).collect::<Vec<_>>().as_slice() {
        [
            Wake::PkgLockTimedOut {
                detail,
                stands_down,
            },
        ] => (detail.clone(), *stands_down),
        other => panic!("exactly one deferred row is posted: {other:?}"),
    }
}

/// A routine deferral retries, so it is no stand-down.
fn one_routine_deferral(posted: &Arc<Mutex<Vec<Wake>>>) {
    let (detail, stands_down) = deferred(posted);
    assert!(!stands_down, "a deferral that retries is routine: {detail}");
}

/// Timed-out lock waits back off through 30s, 60s, and the 1-hour wedge while closing `wait_row_open`.
#[test]
fn a_timed_out_wait_parks_on_the_backoff_then_the_wedge() {
    let busy = run(Some(75), SeedMarkers::default(), "");
    assert_eq!(busy.verdict(), PassVerdict::Busy);
    let posted = Arc::new(Mutex::new(Vec::new()));
    let mut lane = busy_lane(&posted);
    let park = lane.park_after_timed_out_wait(PassVerb::Update, &busy);
    assert_eq!(park, Duration::from_secs(30));
    assert!(
        !lane.wait_row_open,
        "the deferred row answers the waiting row"
    );
    one_routine_deferral(&posted);
    let park = lane.park_after_timed_out_wait(PassVerb::Update, &busy);
    assert_eq!(park, Duration::from_secs(60));
    one_routine_deferral(&posted);
    let park = lane.park_after_timed_out_wait(PassVerb::Update, &busy);
    assert_eq!(park, Duration::from_secs(3600), "the wedge: an hour");
    assert!(lane.backoff.wedged());
    one_routine_deferral(&posted);
    let mut lane = busy_lane(&posted);
    lane.park_after_timed_out_wait(PassVerb::Seed, &busy);
    lane.park_after_timed_out_wait(PassVerb::Seed, &busy);
    posted.lock().unwrap().clear();
    assert_eq!(
        lane.park_after_timed_out_wait(PassVerb::Seed, &busy),
        CONTENTION_WEDGE_PARK
    );
    assert!(lane.backoff.wedged());
    assert!(
        !deferred(&posted).1,
        "a wedge that retries is routine, not a badge"
    );
}

/// The lanes' pass is told its stderr is aterm.log's ([`atpkg::notice::STDERR_IS_HOST_LOG_ENV`]):
/// [`super::report_pass_verdict`] and the lane's own lines copy that stderr into the log, so the
/// pass's notices print the log's form, an aterm.toml value by its length and a line that did not
/// parse by its position (DESIGN §3.6).
#[cfg(unix)]
#[test]
fn a_lanes_pass_is_told_its_stderr_is_the_logs() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-gui-pass-stderr-")
        .tempdir()
        .unwrap();
    let stub = dir.path().join("atpkg-fake");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\necho \"atpkg: copied=${{{}:-unset}}\" >&2\n",
            atpkg::notice::STDERR_IS_HOST_LOG_ENV
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    aterm_uds::deadline::output_until(
        &mut crate::qos::atpkg_command(&stub),
        std::time::Instant::now() + aterm_spec::wait::FIRST_EXEC,
    )
    .unwrap_or_else(|e| panic!("the stub's first exec within its hang detector: {e}"));
    let run = super::run_atpkg_pass(
        Vec::new(),
        crate::qos::atpkg_command(&stub),
        None,
        None,
        false,
        &|_: crate::Wake| {},
    )
    .expect("the stub runs");
    assert_eq!(
        run.why(),
        "atpkg: copied=1",
        "the pass whose stderr reaches aterm.log is told so"
    );
}
