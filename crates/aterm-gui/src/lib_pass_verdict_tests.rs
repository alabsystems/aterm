// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{PassRun, PassVerdict, SeedMarkers, classify_pass_exit};
use std::time::Duration;

/// Every verdict `classify_pass_exit` can reach, one labelled row each:
/// (label, exit code, card opened, marker seen) → verdict. Offline is its own ending,
/// short of an unanswered card.
#[test]
fn every_pass_exit_classifies_to_its_verdict() {
    let busy = Some(i32::from(atpkg::lock::CONTENDED_EXIT));
    let offline = Some(i32::from(aterm_update_core::pkg_check::PASS_OFFLINE_EXIT));
    // atpkg's exit codes, read from the crates both sides share.
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
        // An opened card is answered whatever the exit.
        ("an opened card, exit 0", Some(0), true, false, PassVerdict::AnnouncedThenDied),
        ("an opened card, exit 1", Some(1), true, false, PassVerdict::AnnouncedThenDied),
        ("an opened card, exit 75", busy, true, false, PassVerdict::AnnouncedThenDied),
        ("an opened card, signalled", None, true, false, PassVerdict::AnnouncedThenDied),
        // A marker outranks the code…
        ("a marker outranks a failing code", Some(1), false, true, PassVerdict::Answered),
        ("a marker answers an opened card", Some(0), true, true, PassVerdict::Answered),
        // …except beside exit 75: only the dispatch edge's refusal exits 75, printing
        // its own `seed-busy:` terminal, so the pass is still deferred.
        ("the contention refusal's own terminal is not a pass that ran", busy, false, true, PassVerdict::Busy),
        // Offline is offline whatever marker answered…
        ("offline, no marker", offline, false, false, PassVerdict::Offline),
        ("offline, a marker beside an opened card", offline, true, true, PassVerdict::Offline),
        // …short of an unanswered card, which is still answered.
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

/// A waited pass that then ran or died retires the lane's bar
/// (`PkgProgress { snapshot: None }`) exactly once; only a `Busy` exit leaves it for
/// the next queued child (`carry_wait_row`).
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
        // Carried from an earlier child, no wait line of its own: the same.
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

/// A markerless refusal of a seed or whole update is reported as a failure with the
/// child's stderr; a targeted `update <program>` stays a log line (its row is the
/// record). A quiet healthy launch posts nothing.
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
    // Killed mid-download (a signal: no code), no marker: a failure too.
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

/// A pass on the cached index is not a failure: exit 1, empty stderr, and this pass's
/// own record (its `pass_seq` moved) naming the cached index is said as a stale index
/// and posts nothing. Controls through the same arm: an unmoved `pass_seq`, a failed
/// live-index pass, and a cached-index pass with stderr are still failures.
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

    // Exit 1, empty stderr, this pass's record says the cached index.
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
    // No record before the spawn (a first pass): the one it ended is its own.
    assert!(
        posted_by(&PassRun {
            pass_seq_before: None,
            ..before(0)
        })
        .is_empty()
    );

    // Control: the record is the last pass's (`pass_seq` unmoved) — a refusal at the
    // dispatch edge writes nothing — so this markerless exit is a failure.
    assert_eq!(cached_index_cause(Some(8), &this_pass), None);
    assert!(
        matches!(
            posted_by(&before(8)).as_slice(),
            [Wake::PkgSeedFailed { detail }] if detail == "atpkg ended without saying what happened"
        ),
        "an unmoved record is not this pass's"
    );
    // Control: a pass on the live index that failed carries no cached clause.
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
    // Control: an ok pass is not "failed", whatever its outcome says.
    let ok = record(10, "ok", cached);
    assert_eq!(cached_index_cause(Some(9), &ok), None);
    // Control: a cached-index pass whose stderr said something is that failure.
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
    // An empty recorded cause still names one.
    let bare = record(12, "failed", "up to date (index from cache \u{2014} )");
    assert_eq!(
        cached_index_cause(Some(11), &bare).as_deref(),
        Some("the index was not reached")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A pass that stood down behind a sibling's cached-index pass is not a failure either.
/// atpkg's real stand-down (`atpkg::cli::stand_down_exit` over `stale_index_only_end`)
/// builds the child's streams, binding both halves. Control: a sibling pass that also
/// failed a member stands down on stderr and is still posted.
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
    // The sibling's pass end, as atpkg records it: seq 8, failed, the cache clause —
    // and whether it failed for its index alone.
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
    // The window's child: read seq 7 before the spawn, queued, stood down — its
    // streams and exit exactly as atpkg's stand-down decides them.
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

    // Control: the sibling's cached-index pass failed a member too — a failure, said.
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

/// A pass ran unless the child stood aside at the lock (exit 75, with or
/// without its `seed-busy:` terminal): that child wrote no rows. Every other
/// ending — quiet, answered, refused, signalled — is a pass that ran.
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

/// A lane over a `Vec<Wake>` sink, with the waiting row open as a `Busy` child
/// leaves it.
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

/// The park after a pass whose wait timed out: the backoff (30 s, then 60 s), then
/// the wedge — an hour; each posts one deferred row, and the child's answer closes the
/// row it left open.
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
    // A seed at the wedge parks the same hour and retries: a routine row, not the
    // Settings ▸ Packages badge.
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
