// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_spec::derive::Model;
use aterm_update_core::pkg_check::{PassOutcome, Stamps};

pub(crate) type State = BTreeMap<&'static str, i64>;
pub(crate) const AGE_SECS: [i64; 5] = [60, 15 * 60, 3 * 3600, 6 * 3600 + 60, 9 * 3600];
const RECORD: [&str; 6] = [
    "rec",
    "pass_age",
    "ever_ok",
    "ok_age",
    "write_age",
    "running",
];

/// Every state `model` reaches.
pub(crate) fn reachable(model: &Model) -> Vec<State> {
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
    order
}

/// One reachable state per distinct record.
pub(crate) fn records(model: &Model) -> Vec<State> {
    let mut seen = std::collections::BTreeSet::new();
    reachable(model)
        .into_iter()
        .filter(|state| seen.insert(RECORD.map(|field| state[field])))
        .collect()
}

/// The state `actions` reach from the start, each of them enabled.
pub(crate) fn walk(model: &Model, actions: &[&str]) -> State {
    let mut state = model.init_state();
    for action in actions {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    state
}

/// `state` after the look `action` — taken with no child queued, which no reader sees.
pub(crate) fn looks(model: &Model, action: &str, state: &State) -> State {
    let mut next = State::clone(state);
    next.insert("queued", 0);
    next.insert("behind", 0);
    assert!(model.fire(action, &mut next), "{action}: {state:?}");
    next
}

/// `state`'s record laid down at `now` in a fresh store by the real writers: the success ([`stamp_success`]).
pub(crate) fn realize(
    label: &str,
    state: &State,
    records_pass_end: bool,
    now: i64,
) -> (
    crate::testdir::ScratchLayout,
    Option<crate::progress::ProgressSink>,
) {
    let layout = crate::testdir::layout(&format!("atpkg-full-pass-{label}-"));
    let at = |ticks: i64| now - AGE_SECS[usize::try_from(ticks).unwrap()];
    let stamp = |unix: i64| aterm_types::rfc3339::format_rfc3339(unix.unsigned_abs());
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
            let mut status = seed_for_rewrite(&layout).unwrap();
            status.updated_at = stamp(at(pass_age));
            write(&layout, &status).unwrap();
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
        let mut status = seed_for_rewrite(&layout).unwrap();
        status.programs.insert(
            "claude".into(),
            ProgramStatus {
                installed_build: Some(1),
                state: "managed 2.1.280 \u{2014} Anthropic's latest".into(),
                tree_root: String::new(),
            },
        );
        status.updated_at = stamp(at(state["write_age"]));
        write(&layout, &status).unwrap();
    }
    let sink = (state["running"] >= 1)
        .then(|| crate::progress::ProgressSink::create(&layout.progress_file(), "update").unwrap());
    (layout, sink)
}

/// The reader [`pass_stamps`] replaced, kept as the negative control: the attempt is `updated_at`, else the success.
pub(crate) fn legacy_stamps(layout: &Layout, now: i64) -> Stamps {
    let status = read(layout).unwrap_or_default();
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
        in_flight: crate::progress::pass_running(layout, u64::try_from(now).unwrap_or(0)),
        ..Stamps::default()
    }
}
