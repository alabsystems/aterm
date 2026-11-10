// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State as ModelState;

struct Fixture {
    _root: aterm_tempfile::TempDir,
    store: Store,
    roster: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let store = Store::at(&root.path().join("state"));
        store.save(&State::default()).unwrap();
        let roster = root.path().join("accounts.toml");
        std::fs::write(&roster, "# initial\n").unwrap();
        Self {
            _root: root,
            store,
            roster,
        }
    }

    fn scope(&self) -> Scope<'_> {
        Scope {
            roster: Some(&self.roster),
            fleet: None,
        }
    }

    fn plan(&self, change: bool) -> Plan {
        let held = |path: &Path, max| {
            let before = read(path, max).unwrap();
            Image {
                after: before.clone(),
                before,
            }
        };
        let mut roster = held(&self.roster, MAX_ROSTER_BYTES);
        if change {
            roster.after = Some("# updated\n".into());
        }
        Plan {
            roster_path: Some(
                usage_target(Some(&self.roster))
                    .unwrap()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .into(),
            ),
            roster_mode: None,
            local_logins: false,
            state: held(&self.store.state_file(), MAX_STATE_BYTES),
            logins: held(&self.store.logins_file(), MAX_LOGINS_BYTES),
            ledger: held(&self.store.ledger_path(), MAX_LEDGER),
            roster,
            wake: Some(false),
            fleet: None,
            source_policy: None,
        }
    }

    fn prepare<'a>(&'a self, held: &'a Lock, change: bool, event: bool) -> PreparedPublication<'a> {
        PreparedPublication::new(&self.store, held, self.scope(), self.plan(change), event).unwrap()
    }

    fn images(&self) -> Vec<Option<String>> {
        [
            self.store.state_file(),
            self.store.logins_file(),
            self.store.ledger_path(),
            self.roster.clone(),
        ]
        .iter()
        .map(|path| read(path, MAX_RECEIPT).unwrap())
        .collect()
    }

    fn receipt(&self) -> Option<String> {
        read(&self.store.dir.join(RECEIPT), MAX_RECEIPT).unwrap()
    }

    fn event_only(&self, held: &Lock) {
        self.prepare(held, false, true)
            .persist()
            .unwrap()
            .unwrap()
            .replay()
            .unwrap();
    }
}

struct Mutation;

impl Drop for Mutation {
    fn drop(&mut self) {
        MUTANT.set(0);
    }
}

pub(crate) fn transition(state: &ModelState, action: &str) -> ModelState {
    transition_with(state, action, 0)
}

fn transition_with(state: &ModelState, action: &str, mutant: u8) -> ModelState {
    let mut next = state.clone();
    if action == "Scenario" {
        next.insert("scenario", state["scenario"] + 1);
        return next;
    }
    assert_eq!(action, "Check");
    let scenario = state["scenario"];
    let f = Fixture::new();
    let other = Fixture::new();
    let held = if scenario == 6 {
        other.store.acquire()
    } else {
        f.store.acquire()
    }
    .unwrap();
    if scenario == 9 {
        f.store.retire_source_held(&held, 1).unwrap();
    }
    let before = f.images();
    let receipt_before = f.receipt();
    let change = scenario != 7 && scenario != 8;
    MUTANT.set(mutant);
    let _mutation = Mutation;
    let mut plan = f.plan(change);
    if scenario == 9 {
        plan.source_policy = f.store.source_policy_image(true, 2).unwrap();
    }
    let prepared = PreparedPublication::new(&f.store, &held, f.scope(), plan, scenario == 8);
    next.insert("writer", i64::from(scenario != 6 || prepared.is_err()));
    next.insert("unchanged", i64::from(f.images() == before));
    if scenario == 6 {
        drop(prepared);
    } else {
        let prepared = prepared.unwrap();
        match scenario {
            1 => {
                drop(prepared);
                next.insert(
                    "outcome",
                    i64::from(f.images() == before && f.receipt() == receipt_before),
                );
            }
            2 => {
                std::fs::write(&f.roster, "# outside edit\n").unwrap();
                let edited = f.images();
                let result = prepared.persist();
                next.insert(
                    "fresh",
                    i64::from(
                        result.is_err() && f.receipt() == receipt_before && f.images() == edited,
                    ),
                );
            }
            3 => {
                f.event_only(&held);
                let intervening = f.receipt();
                let result = prepared.persist();
                next.insert(
                    "fresh",
                    i64::from(
                        result.is_err() && f.receipt() == intervening && f.images() == before,
                    ),
                );
            }
            7 => {
                let result = prepared.persist().unwrap();
                next.insert(
                    "outcome",
                    i64::from(
                        result.is_none() && f.images() == before && f.receipt() == receipt_before,
                    ),
                );
            }
            _ => {
                let pending = prepared.persist().unwrap().unwrap();
                next.insert("unchanged", i64::from(f.images() == before));
                next.insert(
                    "fenced",
                    i64::from(f.store.load(0).is_err() && f.store.load_logins(0).is_err()),
                );
                if scenario == 4 {
                    f.store.recover_sync_held(&held, &f.roster).unwrap();
                    f.event_only(&held);
                    let committed = f.receipt();
                    let images = f.images();
                    let result = pending.replay();
                    next.insert(
                        "consumed",
                        i64::from(
                            result.is_err() && f.receipt() == committed && f.images() == images,
                        ),
                    );
                } else if scenario == 9 {
                    std::fs::remove_file(f.store.source_policy_witness()).unwrap();
                    let result = pending.replay();
                    next.insert(
                        "outcome",
                        i64::from(
                            result.is_err()
                                && f.images() == before
                                && f.store.sync_needs_recovery(),
                        ),
                    );
                } else {
                    let result = if scenario == 5 {
                        drop(pending);
                        f.store.recover_sync_held(&held, &f.roster).map(|_| ())
                    } else {
                        pending.replay()
                    };
                    let expected = if change { "# updated\n" } else { "# initial\n" };
                    let succeeded = result.is_ok()
                        && std::fs::read_to_string(&f.roster).unwrap() == expected
                        && f.store.load(0).is_ok()
                        && !f.store.recover_sync_held(&held, &f.roster).unwrap();
                    next.insert("outcome", i64::from(succeeded));
                }
            }
        }
    }
    next.insert("done", 1);
    next
}

#[test]
fn owned_publication_phases_conform_to_the_model() {
    let model = aterm_models_harness::account_publication_phases_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    let mut checked = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                checked += usize::from(action.name == "Check");
            }
        }
    }
    assert_eq!(checked, 10);
    for (mutant, scenario, law) in [
        (1, 0, "NoImagesBeforeReplay"),
        (1, 0, "PendingFencesReaders"),
        (2, 6, "MatchingWriter"),
        (3, 3, "FreshInputs"),
        (4, 4, "ConsumedIntentIsFinal"),
        (5, 2, "FreshInputs"),
        (6, 0, "ExpectedOutcome"),
        (7, 9, "ExpectedOutcome"),
    ] {
        let mut state = model.init_state();
        state.insert("scenario", scenario);
        let bad = transition_with(&state, "Check", mutant);
        assert!(!model.check_invariant(law, &bad), "mutant{mutant}: {law}");
        assert!(!model.successors("Check", &state).contains(&bad));
    }
}
