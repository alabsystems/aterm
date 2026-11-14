// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_types::update_standing::{UpdateStanding, WHY_WORDS, why_word};
use aterm_update_core::update_record::{Cause, Witness};

const BUILD: u64 = 100;
const CHECKED: &str = "2026-09-23T12:00:00Z";

fn healthy() -> UpdateStatus {
    let mut status = UpdateStatus::empty(true, BUILD, "up to date".to_owned());
    status.installable = true;
    status.updated_at = CHECKED.to_owned();
    status
}

fn hold(live: bool, version: &str, why: &str) -> HoldView {
    HoldView {
        build: 101,
        version: version.to_owned(),
        cause: Cause::CrashLoop,
        witness: Witness::Headless,
        why: why.to_owned(),
        tries: 2,
        until: 1_800_003_600,
        live,
        peer_lifted: false,
    }
}

/// A window-start failure of build 101's staged bytes, as status reports it.
fn start_failed(live: bool, version: &str, why: &str) -> HoldView {
    HoldView {
        cause: Cause::StartFailed,
        witness: Witness::Window,
        tries: 1,
        ..hold(live, version, why)
    }
}

fn linux(staged: Option<u64>, phase: Option<&str>, healthy: bool) -> LinuxUpdateStatus {
    LinuxUpdateStatus {
        installed_build: 101,
        installed_version: "0.92.0".to_owned(),
        staged_build: staged,
        staged_version: staged.map(|_| "0.93.0".to_owned()),
        staged_commit: None,
        trial_phase: phase.map(str::to_owned),
        trial_starts: 0,
        trial_healthy: healthy,
    }
}

/// One status per arm of the decision, with the lane report it is read with and the standing it
/// must say, each built on a healthy, checked status. `text` is put in every free-text field the
/// arm's inputs leave free (the outcome, a hold's version and reason, the staged version, the
/// lane's refusal and failure), so a sweep can show none of it is told.
fn corpus(text: &str) -> Vec<(UpdateStatus, ApplyLaneReport, UpdateStanding)> {
    let lane = ApplyLaneReport {
        last_failure: text.to_owned(),
        last_refusal_detail: text.to_owned(),
        trial_wait: text.to_owned(),
        ..ApplyLaneReport::default()
    };
    let refusing = ApplyLaneReport {
        last_refusal: format!("refused: {text}"),
        last_failure_target_build: 101,
        failures_for_target: 3,
        ..lane.clone()
    };
    let failing = ApplyLaneReport {
        last_failure_target_build: 101,
        failures_for_target: 2,
        ..lane.clone()
    };
    let with = |edit: &dyn Fn(&mut UpdateStatus)| {
        let mut status = healthy();
        status.outcome = format!("{} {text}", status.outcome);
        edit(&mut status);
        status
    };
    let staged = |status: &mut UpdateStatus| {
        status.staged_build = Some(101);
        status.staged_version = Some(text.to_owned());
    };
    vec![
        (
            with(&|s| s.enabled = false),
            lane.clone(),
            UpdateStanding::Off,
        ),
        (
            with(&|s| s.linux = Some(linux(Some(102), None, true))),
            lane.clone(),
            UpdateStanding::Downloaded,
        ),
        (
            with(&|s| s.linux = Some(linux(None, Some("Installed"), false))),
            lane.clone(),
            UpdateStanding::Installed,
        ),
        (
            with(&|s| s.installable = false),
            lane.clone(),
            UpdateStanding::Cannot,
        ),
        (
            // A live hold outranks a download of the build it holds.
            with(&|s| {
                staged(s);
                s.hold = Some(hold(true, text, text));
            }),
            refusing.clone(),
            UpdateStanding::Held,
        ),
        (
            // A live window-start failure of the staged bytes outranks the refusal of them.
            with(&|s| {
                staged(s);
                s.start_failure = Some(start_failed(true, text, text));
            }),
            refusing.clone(),
            UpdateStanding::Held,
        ),
        (
            // A standing refusal outranks the tries before it.
            with(&|s| staged(s)),
            refusing,
            UpdateStanding::Refused,
        ),
        (
            with(&|s| staged(s)),
            failing.clone(),
            UpdateStanding::NotInstalling,
        ),
        (
            with(&|s| staged(s)),
            lane.clone(),
            UpdateStanding::Downloaded,
        ),
        (
            // A superseded build's tries never mark a fresh download failed.
            with(&|s| {
                staged(s);
                s.staged_build = Some(102);
            }),
            failing,
            UpdateStanding::Downloaded,
        ),
        (
            with(&|s| s.channel_unreadable = true),
            lane.clone(),
            UpdateStanding::Unreadable,
        ),
        (
            with(&|s| {
                s.failing_persistent = true;
                s.failing_checks = 9;
                s.failing_checks_kind = "manifest".to_owned();
                s.outcome = format!("reinstall aterm: {text}");
            }),
            lane.clone(),
            UpdateStanding::TooOld,
        ),
        (
            with(&|s| {
                s.failing_persistent = true;
                s.failing_kind = "network".to_owned();
            }),
            lane.clone(),
            UpdateStanding::Failing,
        ),
        (
            with(&|s| {
                s.failing_checks = 1;
                s.failing_checks_kind = text.to_owned();
            }),
            lane.clone(),
            UpdateStanding::CheckFailed,
        ),
        (
            with(&|s| s.hold = Some(hold(false, text, text))),
            lane.clone(),
            UpdateStanding::Retrying,
        ),
        (
            with(&|s| {
                s.behind = true;
                s.offered = Some(103);
            }),
            lane.clone(),
            UpdateStanding::Behind,
        ),
        (with(&|_| {}), lane.clone(), UpdateStanding::Current),
        (
            with(&|s| s.updated_at = String::new()),
            lane,
            UpdateStanding::Unchecked,
        ),
    ]
}

/// UPDATE STANDING DECISION: each arm of the status decision says its word, first match first: a
/// live hold beside a staged build is held, a refusal beside failed tries is refused, a manifest
/// failure that prescribes a reinstall is too old, a lapsed hold is retried, a superseded build's
/// tries leave a fresh download downloaded; and every standing but `unread` (which only a status
/// that cannot be read says, [`fleet_fact`]) has a status that says it.
/// NEGATIVE CONTROL: an `arm` with the staged arm ahead of the hold arm fails "a live hold
/// outranks a download".
#[test]
fn each_arm_of_the_status_decision_says_its_standing() {
    let mut said = Vec::new();
    for (status, lane, standing) in corpus("x") {
        assert_eq!(
            status.standing(BUILD, Some(&lane)),
            standing,
            "{standing:?}: {status:?} with {lane:?}"
        );
        said.push(standing);
    }
    for standing in UpdateStanding::ALL {
        assert_eq!(
            said.contains(&standing),
            standing != UpdateStanding::Unread,
            "{standing:?} has a status that says it"
        );
    }
    let mut held = healthy();
    held.staged_build = Some(101);
    held.hold = Some(hold(true, "0.92.0", "it did not start cleanly"));
    assert_eq!(
        held.standing(BUILD, None),
        UpdateStanding::Held,
        "a live hold outranks a download"
    );
}

/// The facts the fleet is told: a hold's build, lapse, cause and try; a failed install's tries; a
/// failure's class; the newer offer; when the last check completed and whether checks are on.
/// And NOTHING of the status's free text: with every outcome, version, reason, refusal and
/// failure set to a path and an address, no arm's words carry a `/`, an `@` or the account's name.
/// NEGATIVE CONTROL: a fact whose `update_to` takes the hold's version unchecked, with
/// `UpdateFact::tokens` keeping it unchecked too, fails "no path, no address".
#[test]
fn the_fleet_is_told_the_standing_and_none_of_the_updaters_free_text() {
    let text = "/Users//someone/Library/x a@b.example";
    for (status, lane, standing) in corpus(text) {
        for checks in [Some(true), Some(false), None] {
            let fact = status.fleet_fact(BUILD, Some(&lane), checks);
            assert_eq!(fact.standing.known(), Some(standing));
            assert_eq!(fact.checks, checks);
            let words = fact.tokens();
            assert!(
                !words.contains('/') && !words.contains('@') && !words.contains("someone"),
                "no path, no address ({standing:?}): {words}"
            );
        }
    }
    let mut held = healthy();
    held.hold = Some(hold(true, "0.92.0", text));
    let fact = held.fleet_fact(BUILD, None, Some(true));
    assert_eq!(
        (
            fact.to.as_deref(),
            fact.until,
            fact.why,
            fact.tries,
            fact.checked_at
        ),
        (
            Some("0.92.0"),
            Some(1_800_003_600),
            Some("crash-loop"),
            Some(2),
            aterm_update_core::pkg_check::rfc3339_to_unix(CHECKED)
                .and_then(|at| u64::try_from(at).ok())
        )
    );
    held.hold = Some(hold(true, "", text));
    assert_eq!(
        held.fleet_fact(BUILD, None, None).to.as_deref(),
        Some("build 101"),
        "a hold with no version is named by its build"
    );
    let mut behind = healthy();
    behind.behind = true;
    behind.offered = Some(103);
    assert_eq!(
        behind.fleet_fact(BUILD, None, None).to.as_deref(),
        Some("build 103")
    );
    let mut failing = healthy();
    failing.failing_checks = 2;
    failing.failing_checks_kind = "network".to_owned();
    assert_eq!(failing.fleet_fact(BUILD, None, None).why, Some("network"));
    let mut tried = healthy();
    tried.staged_build = Some(101);
    let lane = ApplyLaneReport {
        last_failure_target_build: 101,
        failures_for_target: 2,
        ..ApplyLaneReport::default()
    };
    assert_eq!(tried.fleet_fact(BUILD, Some(&lane), None).tries, Some(2));
}

/// A WINDOW-START FAILURE OF THE STAGED BYTES IS TOLD HELD: a window here launched the staged
/// build as its successor and saw it fail to start, so no start without a window (a headless
/// boot, the node job) installs those bytes until the failure lapses, and a Mac with no window
/// open stays where it is. `aterm update status` and the fleet say so, held with the stage's
/// version, the lapse, `start-failed` and the try, where they said downloaded, refused or not
/// installing and nothing of why. A lapsed one holds nothing, and the stage is downloaded again;
/// a failure of other bytes of that build, or of a build not newer than this one, is not the
/// stage's.
/// NEGATIVE CONTROL: an `arm` without its start-failure arm fails "held from every start with no
/// window".
#[test]
fn a_window_start_failure_of_the_staged_bytes_is_told_held_until_it_lapses() {
    use aterm_update_core::update_record::{Failure, Sha, UpdateRecord};
    let at = 1_800_000_000;
    let digest = "ab".repeat(32);
    let mut record = UpdateRecord::default();
    assert!(record.start_failed(
        Failure {
            build: 101,
            version: String::new(),
            sha: Sha::parse(&digest).expect("a digest"),
            cause: Cause::StartFailed,
            witness: Witness::Window,
            why: "stalled (unlaunched)".to_owned(),
        },
        BUILD,
        at,
    ));
    let status_at = |now: u64| {
        let mut status = healthy();
        status.staged_build = Some(101);
        status.staged_version = Some("0.93.0".to_owned());
        status.start_failure =
            HoldView::start_failure_of(&record, BUILD, 101, "0.93.0", &digest.to_uppercase(), now);
        status
    };
    let fact = status_at(at + 60).fleet_fact(BUILD, None, Some(true));
    assert_eq!(
        (
            fact.standing.known(),
            fact.to.as_deref(),
            fact.until,
            fact.why,
            fact.tries
        ),
        (
            Some(UpdateStanding::Held),
            Some("0.93.0"),
            Some(at + 4 * 3600),
            Some("start-failed"),
            Some(1)
        ),
        "held from every start with no window"
    );
    assert_eq!(
        status_at(at + 4 * 3600).standing(BUILD, None),
        UpdateStanding::Downloaded,
        "a lapsed one holds nothing"
    );
    assert_eq!(
        HoldView::start_failure_of(&record, BUILD, 101, "0.93.0", &"cd".repeat(32), at),
        None,
        "other bytes of that build"
    );
    assert_eq!(
        HoldView::start_failure_of(&record, 101, 101, "0.93.0", &digest, at),
        None,
        "a build not newer than this one"
    );
}

/// THE STATUS READS THE WINDOW-START FAILURE OF ITS STAGE: `status` reads the record's
/// window-start failure beside its hold, for the publishable stage's exact bytes, so `aterm update
/// status` and the fleet on this Mac say it as a window booked it.
/// NEGATIVE CONTROL: a `status_at` that leaves `start_failure` empty fails "the stage a window saw
/// fail".
#[cfg(target_os = "macos")]
#[test]
fn the_status_reads_the_window_start_failure_of_its_stage() {
    let s = crate::paths::Staging::scratch("standing-start-failed");
    crate::node_start_tests::publish(&s, 101);
    assert_eq!(
        crate::status_at(&s, BUILD, true).standing(BUILD, None),
        UpdateStanding::Downloaded,
        "CONTROL: nothing on record"
    );
    crate::window_start_failure::record(
        &s,
        BUILD,
        crate::FailedStartBytes::Download {
            build: 101,
            dmg_sha256: &"ab".repeat(32),
        },
        "stalled (unlaunched)",
        crate::install::unix_now_secs(),
    )
    .expect("booked");
    let fact = crate::status_at(&s, BUILD, true).fleet_fact(BUILD, None, None);
    assert_eq!(
        (fact.standing.known(), fact.to.as_deref(), fact.why),
        (
            Some(UpdateStanding::Held),
            Some("0.0.101"),
            Some("start-failed")
        ),
        "the stage a window saw fail"
    );
}

/// Every hold cause the update record keeps is a word `update_why=` may say, so a peer's hold is
/// never told without its cause.
#[test]
fn every_hold_cause_is_a_why_word() {
    let causes = [
        Cause::CrashLoop,
        Cause::Unproven,
        Cause::Stage,
        Cause::LinuxRollback,
        Cause::StartFailed,
    ];
    for cause in causes {
        // A new cause fails this match until it is listed above and in `WHY_WORDS`.
        match cause {
            Cause::CrashLoop
            | Cause::Unproven
            | Cause::Stage
            | Cause::LinuxRollback
            | Cause::StartFailed => {}
        }
        assert_eq!(why_word(cause.word()), Some(cause.word()), "{cause:?}");
    }
    for class in ["network", "manifest", "pipeline", "stage", "apply"] {
        assert!(WHY_WORDS.contains(&class), "{class}");
    }
}

/// A WINDOW OLDER THAN ITS INSTALL TELLS THE INSTALL'S STANDING: a window still on build 100
/// after an update installed 101 in its place (it waits for its relaunch) tells the fleet what
/// `aterm update status` on that Mac says, which runs 101: up to date, never `behind` the 101 it
/// was offered and already has. The status and the apply lane's report are both read for the
/// installed build, the saved setting says whether checks are on, and with no bundle installed in
/// its place (a dev build) the running build is the one asked about.
/// NEGATIVE CONTROL: a `fleet_fact_of` that reads the status for the running build fails "the
/// install's standing".
#[test]
fn a_window_older_than_its_install_tells_the_installs_standing() {
    use aterm_update_core::update_record::UpdateRecord;
    use std::cell::RefCell;
    /// The status the ledgers give for the build it is asked for, each ask noted in `asked`.
    fn offered(
        record: &UpdateRecord,
        asked: &RefCell<Vec<u64>>,
    ) -> impl FnOnce(u64) -> Option<UpdateStatus> {
        move |build| {
            asked.borrow_mut().push(build);
            let mut status = healthy();
            status.current_build = build;
            status.offered = record.offered;
            (status.hold, status.behind, status.outcome) =
                crate::hold_report(record, build, status.outcome, 1_800_000_000);
            Some(status)
        }
    }
    let record = UpdateRecord {
        offered: Some(BUILD + 1),
        ..UpdateRecord::default()
    };
    let asked = RefCell::new(Vec::new());
    let lanes = RefCell::new(Vec::new());
    let fact = fleet_fact_of(
        BUILD,
        Some(BUILD + 1),
        offered(&record, &asked),
        |build| {
            lanes.borrow_mut().push(build);
            None
        },
        || Some(false),
    );
    assert_eq!(
        (fact.standing.known(), fact.to.as_deref()),
        (Some(UpdateStanding::Current), None),
        "the install's standing"
    );
    assert_eq!(fact.checks, Some(false), "the saved setting");
    assert_eq!(
        (asked.take(), lanes.take()),
        (vec![BUILD + 1], vec![BUILD + 1]),
        "both read for the installed build"
    );

    let fact = fleet_fact_of(
        BUILD,
        None,
        offered(&record, &asked),
        |_| None,
        || Some(true),
    );
    assert_eq!(
        (fact.standing.known(), fact.to.as_deref()),
        (Some(UpdateStanding::Behind), Some("build 101")),
        "with no bundle in its place, the running build's standing"
    );
    assert_eq!(asked.take(), vec![BUILD]);

    let unread = fleet_fact_of(BUILD, Some(BUILD + 1), |_| None, |_| None, || Some(true));
    assert_eq!(
        unread.standing.known(),
        Some(if cfg!(any(target_os = "macos", target_os = "linux")) {
            UpdateStanding::Unread
        } else {
            UpdateStanding::Off
        })
    );
}
