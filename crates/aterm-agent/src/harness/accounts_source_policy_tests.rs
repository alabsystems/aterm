// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State as ModelState;

const NOW: i64 = 1_790_000_000;
const ROSTER: &str = "[[account]]\nlabel='new'\nclass='dedicated'\n";
const FLEET: &str = "account=[]\n";

#[test]
fn recovery_requires_the_matching_store_writer() {
    let unrelated = Bed::new();
    let pending = Bed::new();
    assert!(pending.publish(2, 0).is_err());
    let images = pending.images();
    let held = unrelated.store.acquire().unwrap();
    assert!(
        pending
            .store
            .recover_sync_held(&held, &pending.roster)
            .is_err()
    );
    assert_eq!(pending.images(), images);
    assert!(pending.store.sync_needs_recovery());
    pending
        .store
        .recover_sync_within(&[], &pending.roster)
        .unwrap();
    assert!(pending.store.source_policy().unwrap().retired());
}

#[test]
fn witness_loss_and_corruption_refuse_even_unrelated_writes() {
    for fault in 0..4 {
        let bed = Bed::new();
        bed.publish(0, usize::MAX).unwrap();
        let witness = bed.store.source_policy_witness();
        std::fs::remove_file(&witness).unwrap();
        match fault {
            0 => {}
            1 => std::fs::write(&witness, "unexpected content").unwrap(),
            2 => std::fs::create_dir(&witness).unwrap(),
            #[cfg(unix)]
            3 => std::os::unix::fs::symlink(bed.store.source_policy_file(), &witness).unwrap(),
            _ => continue,
        }
        assert!(bed.store.source_policy().is_err());
        let before = bed.images();
        let held = bed.store.acquire().unwrap();
        assert!(
            bed.store
                .transact_state_held(&held, NOW, &mut None, |_| Ok(Vec::new()))
                .is_err()
        );
        assert_eq!(bed.images(), before);
    }
}

#[test]
fn retirement_refuses_a_missing_retained_state() {
    let bed = Bed::new();
    std::fs::remove_file(bed.store.state_file()).unwrap();
    let held = bed.store.acquire().unwrap();
    assert!(bed.store.retire_source_held(&held, NOW).is_err());
    assert!(!bed.store.source_policy_file().exists());
}

#[test]
fn retirement_requires_the_matching_store_writer_before_editing() {
    let first = Bed::new();
    let second = Bed::new();
    let held = first.store.acquire().unwrap();
    let before = second.images();
    assert!(second.store.retire_source_held(&held, NOW).is_err());
    assert!(
        second
            .store
            .transact_fleet_state_policy_held(
                &held,
                NOW,
                &mut None,
                (&second.roster, FLEET),
                (true, |_| panic!("wrong writer reached edit"))
            )
            .is_err()
    );
    assert!(
        second
            .store
            .transact_sync_policy_held(
                &held,
                NOW,
                &mut None,
                SyncTarget {
                    path: &second.roster,
                    before: None,
                    after: Some(ROSTER),
                    mode: None,
                    fleet: None,
                },
                (true, |_, _| panic!("wrong writer reached edit")),
                || panic!("wrong writer reached publication")
            )
            .is_err()
    );
    assert_eq!(second.images(), before);
}

#[test]
fn old_receipt_digest_preimages_are_unchanged() {
    let old = r#"{"schema":1,"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","plan":{"roster_path":null,"roster_mode":null,"state":{"before":null,"after":null},"logins":{"before":null,"after":null},"ledger":{"before":null,"after":null},"roster":{"before":null,"after":null}}}"#;
    let receipt: Receipt = aterm_json::from_str(old).unwrap();
    assert_eq!(aterm_json::to_string(&receipt).unwrap(), old);
    assert_eq!(
        receipt_digest(&receipt).unwrap(),
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(old.as_bytes()))
    );
}

struct Bed {
    root: aterm_tempfile::TempDir,
    store: Store,
    roster: PathBuf,
}

impl Bed {
    fn new() -> Self {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let store = Store::at(&root.path().join("state"));
        store.save(&State::default()).unwrap();
        let roster = root.path().join("config/accounts.toml");
        Self {
            root,
            store,
            roster,
        }
    }

    fn publish(&self, mode: i64, cut: usize) -> Result<(), String> {
        let held = self.store.acquire().unwrap();
        CUT.set(Some(cut));
        let result = if mode == 0 {
            self.store.retire_source_held(&held, NOW).map(|_| ())
        } else if mode == 3 {
            self.store
                .transact_fleet_state_policy_held(
                    &held,
                    NOW,
                    &mut None,
                    (&self.roster, FLEET),
                    (true, |state| {
                        state.swarm_label = Some("m17".into());
                        Ok(Vec::new())
                    }),
                )
                .map(|_| ())
        } else {
            self.store
                .transact_sync_policy_held(
                    &held,
                    NOW,
                    &mut None,
                    SyncTarget {
                        path: &self.roster,
                        before: None,
                        after: Some(ROSTER),
                        mode: None,
                        fleet: (mode == 2).then_some((self.roster.as_path(), FLEET)),
                    },
                    (true, |state, _| {
                        state.swarm_label = Some("m17".into());
                        Ok(Vec::new())
                    }),
                    || Ok(()),
                )
                .map(|_| ())
        };
        CUT.set(None);
        result
    }

    fn images(&self) -> Vec<Option<Vec<u8>>> {
        [
            self.store.state_file(),
            self.store.logins_file(),
            self.store.ledger_path(),
            self.roster.clone(),
            self.roster
                .with_file_name(super::super::super::accounts_sync::FLEET_ROSTER_FILE),
            self.store.source_policy_file(),
        ]
        .iter()
        .map(|path| std::fs::read(path).ok())
        .collect()
    }
}

pub(crate) fn transition(state: &ModelState, action: &str) -> ModelState {
    let mut next = state.clone();
    match action {
        "Mode" => {
            next.insert("mode", state["mode"] + 1);
            return next;
        }
        "Cut" => {
            next.insert("cut", state["cut"] + 1);
            return next;
        }
        "Conflict" => {
            next.insert("conflict", 1);
            return next;
        }
        "Commit" => {}
        _ => panic!("unknown retirement action"),
    }
    let bed = Bed::new();
    let source = bed.root.path().join("legacy-source");
    std::fs::write(&source, "preserve this source").unwrap();
    let result = bed.publish(state["mode"], state["cut"] as usize);
    let pending = result.is_err();
    if let Err(why) = result {
        assert_eq!(why, "injected interruption");
    }
    let fenced = !pending || bed.store.source_policy().is_err();
    let conflict = state["conflict"] != 0;
    if conflict {
        std::fs::write(
            bed.store.source_policy_file(),
            "{\"schema\":1,\"retired_at\":9}",
        )
        .unwrap();
    }
    let before_recovery = bed.images();
    let recovered = bed.store.recover_sync_within(&[], &bed.roster);
    let preserved = !conflict || bed.images() == before_recovery;
    if conflict && pending {
        assert!(recovered.is_err());
    } else {
        recovered.unwrap();
    }
    let retired = !conflict
        && bed
            .store
            .source_policy()
            .is_ok_and(|policy| policy.retired());
    let (stable, loss_refused) = if conflict {
        (true, true)
    } else {
        let first = std::fs::read(bed.store.source_policy_file()).ok();
        let held = bed.store.acquire().unwrap();
        let repeated = bed.store.retire_source_held(&held, NOW + 100);
        if RETIREMENT_MUTANT.get() != 3 {
            repeated.unwrap();
        }
        let stable = std::fs::read(bed.store.source_policy_file()).ok() == first;
        // An unrelated receipt must not forget the retirement witness.
        let unrelated = bed
            .store
            .transact_state_held(&held, NOW + 200, &mut None, |state| {
                state.swarm_label = Some("m18".into());
                Ok(Vec::new())
            });
        if RETIREMENT_MUTANT.get() != 3 {
            unrelated.unwrap();
        }
        drop(held);
        if bed.store.source_policy_file().exists() {
            std::fs::remove_file(bed.store.source_policy_file()).unwrap();
        }
        (stable, bed.store.source_policy().is_err())
    };
    assert_eq!(
        std::fs::read_to_string(source).unwrap(),
        "preserve this source"
    );
    next.insert("done", 1);
    next.insert("retired", i64::from(retired));
    next.insert("fenced", i64::from(fenced));
    next.insert("preserved", i64::from(preserved));
    next.insert("stable", i64::from(stable));
    next.insert("loss_refused", i64::from(loss_refused));
    next
}

#[test]
fn retirement_is_retained_across_every_receipt_role_and_cut() {
    let model = aterm_models_harness::account_source_retirement_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{state:?} -> {actual:?}"
                );
            }
        }
    }
    for (mutant, mode, conflict, invariant) in [
        (1, 2, 0, "CommittedRetires"),
        (2, 2, 1, "ConflictPreservesEveryImage"),
        (3, 0, 0, "LostPolicyIsNotInitial"),
        (4, 2, 0, "PendingFencesPolicy"),
        (5, 2, 0, "RepeatedRetirementIsStable"),
    ] {
        let mut state = model.init_state();
        state.insert("mode", mode);
        state.insert("conflict", conflict);
        RETIREMENT_MUTANT.set(mutant);
        let actual = transition(&state, "Commit");
        RETIREMENT_MUTANT.set(0);
        assert!(
            !model.check_invariant(invariant, &actual),
            "mutant {mutant}"
        );
    }
}

#[test]
fn malformed_policy_cannot_be_overwritten_or_clear_retirement() {
    for text in [
        "broken",
        "{\"schema\":2,\"retired_at\":3}",
        "{\"schema\":1,\"retired_at\":3,\"active\":true}",
    ] {
        let bed = Bed::new();
        std::fs::write(bed.store.source_policy_file(), text).unwrap();
        let before = bed.images();
        assert!(bed.publish(2, 0).is_err());
        assert_eq!(bed.images(), before);
        assert!(bed.store.source_policy().is_err());
    }
    let image = Image {
        before: None,
        after: None,
    };
    let bed = Bed::new();
    let mut plan = Plan {
        roster_path: None,
        roster_mode: None,
        local_logins: false,
        state: image.clone(),
        logins: image.clone(),
        ledger: image.clone(),
        roster: image.clone(),
        wake: None,
        fleet: None,
        source_policy: Some(image),
    };
    assert!(plan.validate(&bed.store, Scope::default()).is_err());
    plan.source_policy.as_mut().unwrap().after =
        Some("x".repeat(super::super::source_policy::MAX_BYTES as usize + 1));
    assert!(plan.validate(&bed.store, Scope::default()).is_err());
}

#[cfg(unix)]
#[test]
fn policy_and_witness_cannot_be_external_targets_even_when_policy_is_omitted() {
    for witness in [false, true] {
        let bed = Bed::new();
        let target = if witness {
            bed.store.source_policy_witness()
        } else {
            bed.store.source_policy_file()
        };
        let held = bed.store.acquire().unwrap();
        let result = bed.store.transact_sync_held(
            &held,
            NOW,
            &mut None,
            SyncTarget {
                path: &target,
                before: None,
                after: Some(ROSTER),
                mode: None,
                fleet: None,
            },
            |_, _| Ok(Vec::new()),
            || panic!("invalid target cannot reach auxiliary publication"),
        );
        assert!(result.unwrap_err().contains("aliases account authority"));
        assert!(!target.exists());
    }
    let bed = Bed::new();
    let elsewhere = bed.root.path().join("elsewhere");
    std::fs::write(&elsewhere, "personal file").unwrap();
    std::os::unix::fs::symlink(&elsewhere, bed.store.source_policy_file()).unwrap();
    assert!(bed.store.source_policy().is_err());
    assert!(bed.publish(2, 0).is_err());
    assert_eq!(std::fs::read_to_string(elsewhere).unwrap(), "personal file");
}
