// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::derive::{Model, atpkg_full_pass_rule_model};
use aterm_update_core::pkg_check::{FULL_PASS_INTERVAL_SECS, PassOutcome, Stamps};

type State = std::collections::BTreeMap<&'static str, i64>;
/// A model tick as a wall-clock age: each side of the spacing and of the interval.
const AGE_SECS: [i64; 5] = [60, 15 * 60, 3 * 3600, 6 * 3600 + 60, 9 * 3600];
/// What a reader sees of a state: the record and the pass in flight.
const RECORD: [&str; 6] = [
    "rec",
    "pass_age",
    "ever_ok",
    "ok_age",
    "write_age",
    "running",
];

/// One reachable state per distinct record — the readers see nothing else.
fn records(model: &Model) -> Vec<State> {
    let mut order = vec![model.init_state()];
    let mut seen: std::collections::BTreeSet<State> = order.iter().cloned().collect();
    let mut at = 0;
    while at < order.len() {
        for action in model.actions.iter().map(|a| a.name) {
            let mut next = order[at].clone();
            if model.fire(action, &mut next) && seen.insert(next.clone()) {
                order.push(next);
            }
        }
        at += 1;
    }
    let mut read = std::collections::BTreeSet::new();
    order
        .into_iter()
        .filter(|state| read.insert(RECORD.map(|field| state[field])))
        .collect()
}

fn walk(model: &Model, actions: &[&str]) -> State {
    let mut state = model.init_state();
    for action in actions {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    state
}

/// `state` after the look `action`, taken with no child queued (no reader sees one).
fn looks(model: &Model, action: &str, state: &State) -> State {
    let mut next = state.clone();
    next.insert("queued", 0);
    next.insert("behind", 0);
    assert!(model.fire(action, &mut next), "{action}: {state:?}");
    next
}

fn stamp(unix: i64) -> String {
    aterm_types::rfc3339::format_rfc3339(unix.unsigned_abs())
}

/// `state`'s record laid down at `now` in a scratch store by atpkg's own writers, as its
/// conformance lays it: the success, and — `records_pass_end` — each pass's recorded end
/// (else the writer of before `last_pass`, a failed pass a bare write); a vendor row
/// written after the last pass.
fn realize(
    dir: &std::path::Path,
    state: &State,
    records_pass_end: bool,
    now: i64,
) -> atpkg::store::Layout {
    use atpkg::status::{stamp_pass_end, stamp_success};
    let layout = atpkg::store::Layout {
        prefix: dir.join("pkg"),
    };
    std::fs::create_dir_all(&layout.prefix).unwrap();
    let at = |ticks: i64| now - AGE_SECS[usize::try_from(ticks).unwrap()];
    let (rec, pass_age) = (state["rec"], state["pass_age"]);
    if state["ever_ok"] == 1 {
        let success = stamp(at(state["ok_age"]) - i64::from(rec == 2));
        stamp_success(&layout, &success).unwrap();
        if records_pass_end {
            stamp_pass_end(&layout, &success, PassOutcome::Ok).unwrap();
        }
    }
    if rec == 2 {
        if records_pass_end {
            stamp_pass_end(&layout, &stamp(at(pass_age)), PassOutcome::Failed).unwrap();
        } else {
            let mut status = atpkg::status::seed_for_rewrite(&layout).unwrap();
            status.updated_at = stamp(at(pass_age));
            atpkg::status::write(&layout, &status).unwrap();
        }
    }
    let last_write = if rec >= 1 {
        pass_age
    } else if state["ever_ok"] == 1 {
        state["ok_age"]
    } else {
        4
    };
    if state["write_age"] < last_write {
        let mut status = atpkg::status::seed_for_rewrite(&layout).unwrap();
        status.programs.insert(
            "claude".into(),
            atpkg::status::ProgramStatus {
                installed_build: Some(1),
                state: "managed 2.1.280 \u{2014} Anthropic's latest".into(),
                tree_root: String::new(),
            },
        );
        status.updated_at = stamp(at(state["write_age"]));
        atpkg::status::write(&layout, &status).unwrap();
    }
    layout
}

/// The previous stamp readers, as the negative control: attempt is `updated_at` else
/// the success; a failure is an attempt after the success; no hold.
fn legacy_stamps(layout: &atpkg::store::Layout, now: i64) -> Stamps {
    let status = atpkg::status::read(layout).unwrap_or_default();
    let unix = |stamp: &str| aterm_update_core::pkg_check::rfc3339_to_unix(stamp.trim());
    let success = unix(&status.last_success_at);
    let attempt = unix(&status.updated_at).or(success);
    let failed = match (attempt, success) {
        (Some(attempt), Some(success)) => attempt > success,
        (Some(_), None) => true,
        (None, _) => false,
    };
    Stamps {
        last_success: success,
        last_attempt: attempt,
        last_outcome: attempt.map(|_| {
            if failed {
                PassOutcome::Failed
            } else {
                PassOutcome::Ok
            }
        }),
        in_flight: atpkg::progress::pass_running(layout, now.unsigned_abs()),
        ..Stamps::default()
    }
}

fn legacy_launch_park(stamps: &Stamps, now: i64) -> Option<Duration> {
    let success = stamps.last_success?;
    if stamps.last_failed() {
        return None;
    }
    let age = u64::try_from(now.saturating_sub(success)).unwrap_or(0);
    (age < FULL_PASS_INTERVAL_SECS).then(|| Duration::from_secs(FULL_PASS_INTERVAL_SECS - age))
}

/// Tier-1 of `AtpkgFullPassRule`, the window's half: over every record the model
/// reaches at `Buggy=0`, written by atpkg's own writers, [`scheduled_pass_gate`] over
/// [`pass_stamps`] runs a pass exactly when `WindowWalk` does, and a launch exactly
/// when `WindowLaunch` does. Negative control: [`legacy_stamps`] over `Buggy=1`'s
/// records is `Buggy=1`'s window.
#[test]
fn the_windows_gate_and_launch_conform_to_the_full_pass_rule() {
    let model = atpkg_full_pass_rule_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let none = OwnPasses::default();
    let lanes = |stamps: &Stamps, park: Option<Duration>, now: i64| {
        let walk = scheduled_pass_gate(PassWhy::Walk, stamps, none, None, now).is_none();
        let launch = park.is_none()
            && scheduled_pass_gate(PassWhy::LaunchDue, stamps, none, None, now).is_none();
        (walk, launch)
    };
    let fixed = records(&model);
    assert!(fixed.len() > 20, "{}", fixed.len());
    for state in &fixed {
        // Each state at its own now: a progress heartbeat is fresh for seconds only.
        let now = unix_secs(std::time::SystemTime::now());
        let dir = aterm_tempfile::tempdir().unwrap();
        let layout = realize(dir.path(), state, true, now);
        let _sink = (state["running"] >= 1).then(|| {
            atpkg::progress::ProgressSink::create(&layout.progress_file(), "update").unwrap()
        });
        let stamps = pass_stamps(Some(&layout));
        let park = launch_update_park(false, Some(&layout), now);
        let launch_look = looks(&model, "WindowLaunch", state);
        assert_eq!(
            stamps.last_failed(),
            launch_look["verdict"] == 1,
            "{state:?}"
        );
        assert_eq!(
            lanes(&stamps, park, now),
            (
                looks(&model, "WindowWalk", state)["queued"] > 0,
                launch_look["queued"] > 0
            ),
            "{state:?} {stamps:?}"
        );
    }
    let old = records(&buggy);
    assert!(old.len() > 5, "{}", old.len());
    for state in &old {
        let now = unix_secs(std::time::SystemTime::now());
        let dir = aterm_tempfile::tempdir().unwrap();
        let layout = realize(dir.path(), state, false, now);
        let _sink = (state["running"] >= 1).then(|| {
            atpkg::progress::ProgressSink::create(&layout.progress_file(), "update").unwrap()
        });
        let stamps = legacy_stamps(&layout, now);
        assert_eq!(
            lanes(&stamps, legacy_launch_park(&stamps, now), now),
            (
                looks(&buggy, "WindowWalk", state)["queued"] > 0,
                looks(&buggy, "WindowLaunch", state)["queued"] > 0
            ),
            "{state:?} {stamps:?}"
        );
    }
    // The replayed defect: a success three hours old, the head watch's row since.
    let defect = walk(
        &model,
        &[
            "WindowLaunch",
            "TakeLockFresh",
            "PassSucceeds",
            "Tick",
            "OtherWrite",
            "Tick",
        ],
    );
    let now = unix_secs(std::time::SystemTime::now());
    let dir = aterm_tempfile::tempdir().unwrap();
    let layout = realize(dir.path(), &defect, true, now);
    let parks = launch_update_park(false, Some(&layout), now);
    assert!(
        parks.is_some(),
        "the real launch parks on the young success"
    );
    assert_eq!(looks(&model, "WindowLaunch", &defect)["queued"], 0);
    let legacy = legacy_stamps(&layout, now);
    assert!(
        lanes(&legacy, legacy_launch_park(&legacy, now), now).1,
        "the old launch read the write as a failed pass and ran one"
    );
    assert_eq!(looks(&buggy, "WindowLaunch", &defect)["queued"], 1);
}
