// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;
const FLEET: &str = "account = []\n# admitted local projection\n";
const ROSTER: &str = "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n";

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

    fn fleet(&self) -> PathBuf {
        self.roster
            .with_file_name(super::super::super::accounts_sync::FLEET_ROSTER_FILE)
    }

    fn publish(&self, cut: Option<usize>) -> Result<bool, String> {
        let held = self.store.acquire().unwrap();
        CUT.with(|held| held.set(cut));
        let result = self.store.transact_fleet_state_held(
            &held,
            NOW,
            &mut None,
            (&self.roster, FLEET),
            |state| {
                state.swarm_label = Some("m16".into());
                Ok(Vec::new())
            },
        );
        CUT.with(|held| held.set(None));
        result
    }

    fn publish_full(&self, cut: Option<usize>) -> Result<(), String> {
        self.publish_full_to(&self.roster, &self.roster, cut)
    }

    fn publish_full_to(
        &self,
        logical: &Path,
        target: &Path,
        cut: Option<usize>,
    ) -> Result<(), String> {
        let held = self.store.acquire().unwrap();
        let before = read(target, MAX_ROSTER_BYTES).unwrap();
        CUT.with(|held| held.set(cut));
        let result = self.store.transact_sync_held(
            &held,
            NOW,
            &mut None,
            SyncTarget {
                path: target,
                before: before.as_deref(),
                after: Some(ROSTER),
                mode: None,
                fleet: Some((logical, FLEET)),
            },
            |state, _| {
                state.swarm_label = Some("m16".into());
                Ok(Vec::new())
            },
            || Ok(()),
        );
        CUT.with(|held| held.set(None));
        result.map(|_| ())
    }
}

#[test]
fn ordinary_fleet_commit_recovers_roster_and_fleet_at_every_cut() {
    for cut in 0..=6 {
        let bed = Bed::new();
        assert_eq!(
            bed.publish_full(Some(cut)).unwrap_err(),
            "injected interruption"
        );
        assert!(bed.store.load(NOW).is_err());
        assert!(bed.store.recover_sync_within(&[], &bed.roster).unwrap());
        assert_eq!(std::fs::read_to_string(&bed.roster).unwrap(), ROSTER);
        assert_eq!(std::fs::read_to_string(bed.fleet()).unwrap(), FLEET);
        assert_eq!(
            bed.store.load(NOW).unwrap().0.swarm_label.as_deref(),
            Some("m16")
        );
        assert!(!bed.store.recover_sync_within(&[], &bed.roster).unwrap());
    }
}

#[test]
fn ordinary_fleet_only_changes_are_retained_and_repeats_are_noops() {
    let bed = Bed::new();
    bed.publish_full(None).unwrap();
    let first = std::fs::read(bed.store.dir.join(RECEIPT)).unwrap();
    std::fs::write(bed.fleet(), "account = []\n# old projection\n").unwrap();
    bed.publish_full(None).unwrap();
    let second = std::fs::read(bed.store.dir.join(RECEIPT)).unwrap();
    assert_ne!(first, second);
    assert_eq!(std::fs::read_to_string(bed.fleet()).unwrap(), FLEET);
    bed.publish_full(None).unwrap();
    assert_eq!(std::fs::read(bed.store.dir.join(RECEIPT)).unwrap(), second);
}

#[test]
fn ordinary_fleet_scope_must_belong_to_the_published_roster() {
    let bed = Bed::new();
    let other = bed.root.path().join("other/accounts.toml");
    let state = std::fs::read(bed.store.state_file()).unwrap();
    let why = bed.publish_full_to(&other, &bed.roster, None).unwrap_err();
    assert!(why.contains("another account roster"), "{why}");
    assert_eq!(std::fs::read(bed.store.state_file()).unwrap(), state);
    assert!(!bed.roster.exists());
    assert!(!bed.fleet().exists());
    assert!(!bed.store.dir.join(RECEIPT).exists());
}

#[cfg(unix)]
#[test]
fn ordinary_fleet_uses_the_logical_sibling_of_a_linked_roster() {
    let bed = Bed::new();
    let target = bed.root.path().join("elsewhere/accounts.toml");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "# previous roster\n").unwrap();
    std::fs::create_dir_all(bed.roster.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, &bed.roster).unwrap();
    bed.publish_full_to(&bed.roster, &target, Some(0))
        .unwrap_err();
    assert!(bed.store.recover_sync_within(&[], &target).is_err());
    assert!(bed.store.recover_sync_within(&[], &bed.roster).unwrap());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), ROSTER);
    assert_eq!(std::fs::read_to_string(bed.fleet()).unwrap(), FLEET);
    assert!(
        !target
            .with_file_name(super::super::super::accounts_sync::FLEET_ROSTER_FILE)
            .exists()
    );
    assert!(
        std::fs::symlink_metadata(&bed.roster)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn roster_recovery_refuses_a_retargeted_parent_even_with_identical_bytes() {
    let bed = Bed::new();
    let first = bed.root.path().join("first");
    let second = bed.root.path().join("second");
    let alias = bed.root.path().join("alias");
    for dir in [&first, &second] {
        std::fs::create_dir(dir).unwrap();
        std::fs::write(dir.join("accounts.toml"), "# previous roster\n").unwrap();
    }
    std::os::unix::fs::symlink(&first, &alias).unwrap();
    let target = alias.join("accounts.toml");
    let held = bed.store.acquire().unwrap();
    CUT.with(|cut| cut.set(Some(0)));
    let interrupted = bed.store.transact_sync_held(
        &held,
        NOW,
        &mut None,
        SyncTarget {
            path: &target,
            before: Some("# previous roster\n"),
            after: Some(ROSTER),
            mode: None,
            fleet: None,
        },
        |state, _| {
            state.swarm_label = Some("m16".into());
            Ok(Vec::new())
        },
        || Ok(()),
    );
    CUT.with(|cut| cut.set(None));
    assert_eq!(interrupted.unwrap_err(), "injected interruption");
    drop(held);
    let state = std::fs::read(bed.store.state_file()).unwrap();
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&second, &alias).unwrap();
    assert!(bed.store.recover_sync_within(&[], &target).is_err());
    assert_eq!(std::fs::read(bed.store.state_file()).unwrap(), state);
    for dir in [&first, &second] {
        assert_eq!(
            std::fs::read_to_string(dir.join("accounts.toml")).unwrap(),
            "# previous roster\n"
        );
    }
    assert!(!bed.fleet().exists());
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&first, &alias).unwrap();
    assert!(bed.store.recover_sync_within(&[], &target).unwrap());
    assert_eq!(
        std::fs::read_to_string(first.join("accounts.toml")).unwrap(),
        ROSTER
    );
    assert_eq!(
        std::fs::read_to_string(second.join("accounts.toml")).unwrap(),
        "# previous roster\n"
    );
}

/// Admission on a canonical path must not exempt its ancestors from later resolution.
#[cfg(unix)]
#[test]
fn roster_recovery_refuses_a_new_parent_link_after_canonical_admission() {
    let bed = Bed::new();
    let root = std::fs::canonicalize(bed.root.path()).unwrap();
    let first = root.join("canonical");
    let retained = root.join("retained");
    let second = root.join("second");
    for dir in [&first, &second] {
        std::fs::create_dir(dir).unwrap();
        std::fs::write(dir.join("accounts.toml"), "# previous roster\n").unwrap();
    }
    let target = first.join("accounts.toml");
    assert_eq!(
        absolute(&target).unwrap(),
        usage_target(Some(&target)).unwrap().unwrap()
    );
    let held = bed.store.acquire().unwrap();
    CUT.with(|cut| cut.set(Some(0)));
    let interrupted = bed.store.transact_sync_held(
        &held,
        NOW,
        &mut None,
        SyncTarget {
            path: &target,
            before: Some("# previous roster\n"),
            after: Some(ROSTER),
            mode: None,
            fleet: None,
        },
        |state, _| {
            state.swarm_label = Some("m16".into());
            Ok(Vec::new())
        },
        || Ok(()),
    );
    CUT.with(|cut| cut.set(None));
    assert_eq!(interrupted.unwrap_err(), "injected interruption");
    drop(held);
    let state = std::fs::read(bed.store.state_file()).unwrap();
    std::fs::rename(&first, &retained).unwrap();
    std::os::unix::fs::symlink(&second, &first).unwrap();
    assert!(bed.store.recover_sync_within(&[], &target).is_err());
    assert_eq!(std::fs::read(bed.store.state_file()).unwrap(), state);
    for dir in [&retained, &second] {
        assert_eq!(
            std::fs::read_to_string(dir.join("accounts.toml")).unwrap(),
            "# previous roster\n"
        );
    }
    std::fs::remove_file(&first).unwrap();
    std::fs::rename(&retained, &first).unwrap();
    assert!(bed.store.recover_sync_within(&[], &target).unwrap());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), ROSTER);
    assert_eq!(
        std::fs::read_to_string(second.join("accounts.toml")).unwrap(),
        "# previous roster\n"
    );
}

#[test]
fn first_roster_publication_creates_its_missing_ancestors_with_or_without_fleet() {
    for with_fleet in [false, true] {
        let bed = Bed::new();
        let target = bed.root.path().join("absent/nested/config/accounts.toml");
        assert!(!target.parent().unwrap().exists());
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
                fleet: with_fleet.then_some((target.as_path(), FLEET)),
            },
            |state, _| {
                state.swarm_label = Some("m16".into());
                Ok(Vec::new())
            },
            || Ok(()),
        );
        assert!(result.is_ok(), "with_fleet={with_fleet}: {result:?}");
        drop(held);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), ROSTER);
        assert_eq!(
            bed.store.load(NOW).unwrap().0.swarm_label.as_deref(),
            Some("m16")
        );
        assert_eq!(
            target.with_file_name("fleet-accounts.toml").exists(),
            with_fleet
        );
        assert!(!bed.store.recover_sync_within(&[], &target).unwrap());
    }
}

#[test]
fn fleet_commit_recovers_every_durable_cut_without_creating_a_roster() {
    for cut in 0..=6 {
        let bed = Bed::new();
        assert_eq!(bed.publish(Some(cut)).unwrap_err(), "injected interruption");
        assert!(
            bed.store.load(NOW).is_err(),
            "pending fleet commit fences readers"
        );
        assert!(
            bed.store.recover_state_within(&[]).is_err(),
            "no caller scope grants no fleet authority"
        );
        assert!(bed.store.recover_sync_within(&[], &bed.roster).unwrap());
        assert_eq!(std::fs::read_to_string(bed.fleet()).unwrap(), FLEET);
        assert_eq!(
            bed.store.load(NOW).unwrap().0.swarm_label.as_deref(),
            Some("m16")
        );
        assert!(!bed.roster.exists());
        assert!(!bed.store.recover_sync_within(&[], &bed.roster).unwrap());
    }
}

#[test]
fn fleet_conflict_and_wrong_scope_preserve_every_admitted_image() {
    for foreign_edit in [false, true] {
        let bed = Bed::new();
        let state = std::fs::read(bed.store.state_file()).unwrap();
        bed.publish(Some(0)).unwrap_err();
        let caller = if foreign_edit {
            std::fs::create_dir_all(bed.fleet().parent().unwrap()).unwrap();
            std::fs::write(bed.fleet(), "# independently edited\n").unwrap();
            bed.roster.clone()
        } else {
            bed.root.path().join("foreign/accounts.toml")
        };
        assert!(bed.store.recover_sync_within(&[], &caller).is_err());
        assert_eq!(std::fs::read(bed.store.state_file()).unwrap(), state);
        assert!(bed.store.sync_needs_recovery());
        if foreign_edit {
            assert_eq!(
                std::fs::read_to_string(bed.fleet()).unwrap(),
                "# independently edited\n"
            );
        } else {
            assert!(!bed.fleet().exists());
        }
    }
}

#[test]
fn a_forged_fleet_destination_does_not_grant_write_authority() {
    let bed = Bed::new();
    bed.publish(Some(0)).unwrap_err();
    let foreign = bed.root.path().join("foreign.toml");
    let mut receipt = bed.store.receipt().unwrap().unwrap();
    receipt.plan.as_mut().unwrap().fleet.as_mut().unwrap().path = foreign.to_str().unwrap().into();
    bed.store.save_receipt(&receipt).unwrap();
    let before = std::fs::read(bed.store.state_file()).unwrap();
    assert!(bed.store.recover_sync_within(&[], &bed.roster).is_err());
    assert_eq!(std::fs::read(bed.store.state_file()).unwrap(), before);
    assert!(!foreign.exists());
    assert!(!bed.fleet().exists());
}

#[test]
fn an_external_roster_cannot_be_hidden_by_empty_pool_recovery() {
    let bed = Bed::new();
    bed.publish(Some(0)).unwrap_err();
    let state = std::fs::read(bed.store.state_file()).unwrap();
    std::fs::create_dir_all(bed.roster.parent().unwrap()).unwrap();
    std::fs::write(&bed.roster, "# independently created roster\n").unwrap();
    assert!(bed.store.recover_sync_within(&[], &bed.roster).is_err());
    assert_eq!(std::fs::read(bed.store.state_file()).unwrap(), state);
    assert!(!bed.fleet().exists());
    assert_eq!(
        std::fs::read_to_string(&bed.roster).unwrap(),
        "# independently created roster\n"
    );
}

#[test]
fn a_fleet_only_change_gets_a_receipt_and_a_repeat_is_a_noop() {
    let bed = Bed::new();
    assert!(bed.publish(None).unwrap());
    let before = std::fs::read(bed.store.dir.join(RECEIPT)).unwrap();
    std::fs::write(bed.fleet(), "account = []\n# prior projection\n").unwrap();
    let state = std::fs::read(bed.store.state_file()).unwrap();
    assert!(bed.publish(None).unwrap());
    let after = std::fs::read(bed.store.dir.join(RECEIPT)).unwrap();
    assert_ne!(before, after, "a fleet-only change must be journaled");
    assert_eq!(std::fs::read(bed.store.state_file()).unwrap(), state);
    assert!(!bed.publish(None).unwrap());
    assert_eq!(std::fs::read(bed.store.dir.join(RECEIPT)).unwrap(), after);
}

#[test]
fn a_new_fleet_plan_does_not_implicitly_recover_a_pending_plan() {
    let bed = Bed::new();
    bed.publish(Some(0)).unwrap_err();
    let before = std::fs::read(bed.store.state_file()).unwrap();
    let held = bed.store.acquire().unwrap();
    let mut edited = false;
    assert!(
        bed.store
            .transact_fleet_state_held(&held, NOW, &mut None, (&bed.roster, FLEET), |_| {
                edited = true;
                Ok(Vec::new())
            },)
            .is_err()
    );
    assert!(!edited, "a stale plan may not run its state edit");
    assert_eq!(std::fs::read(bed.store.state_file()).unwrap(), before);
    assert!(!bed.fleet().exists());
    drop(held);
    assert!(bed.store.recover_sync_within(&[], &bed.roster).unwrap());
}

#[cfg(unix)]
#[test]
fn the_logical_roster_scope_keeps_its_fleet_when_the_roster_is_linked() {
    let bed = Bed::new();
    let remote = bed.root.path().join("elsewhere/accounts.toml");
    std::fs::create_dir_all(remote.parent().unwrap()).unwrap();
    std::fs::write(&remote, "# roster remains unchanged\n").unwrap();
    std::fs::create_dir_all(bed.roster.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&remote, &bed.roster).unwrap();
    bed.publish(Some(1)).unwrap_err();
    assert!(bed.store.recover_sync_within(&[], &remote).is_err());
    assert!(bed.store.recover_sync_within(&[], &bed.roster).unwrap());
    assert_eq!(std::fs::read_to_string(bed.fleet()).unwrap(), FLEET);
    assert!(
        !remote
            .with_file_name(super::super::super::accounts_sync::FLEET_ROSTER_FILE)
            .exists()
    );
    assert_eq!(
        std::fs::read_to_string(remote).unwrap(),
        "# roster remains unchanged\n"
    );
}

#[cfg(unix)]
#[test]
fn a_roster_transaction_recovers_the_fleet_using_its_logical_scope() {
    let bed = Bed::new();
    let actual = bed.root.path().join("elsewhere/accounts.toml");
    std::fs::create_dir_all(actual.parent().unwrap()).unwrap();
    std::fs::write(&actual, "# unchanged roster\n").unwrap();
    std::fs::create_dir_all(bed.roster.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&actual, &bed.roster).unwrap();
    bed.publish(Some(1)).unwrap_err();
    let mut edited = false;
    bed.store
        .transact_roster_logins_within(&[], &bed.roster, NOW, &mut None, |_, state, _| {
            assert_eq!(state.swarm_label.as_deref(), Some("m16"));
            assert_eq!(std::fs::read_to_string(bed.fleet()).unwrap(), FLEET);
            edited = true;
            Ok(Vec::new())
        })
        .unwrap();
    assert!(edited);
    assert_eq!(
        std::fs::read_to_string(actual).unwrap(),
        "# unchanged roster\n"
    );
}

#[cfg(unix)]
#[test]
fn a_fleet_link_cannot_replace_account_authority() {
    let bed = Bed::new();
    std::fs::create_dir_all(bed.fleet().parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(bed.store.state_file(), bed.fleet()).unwrap();
    let before = std::fs::read(bed.store.state_file()).unwrap();
    let why = bed.publish(None).unwrap_err();
    assert!(why.contains("aliases account authority"), "{why}");
    assert_eq!(std::fs::read(bed.store.state_file()).unwrap(), before);
    assert!(!bed.store.dir.join(RECEIPT).exists());
}

pub(crate) fn fleet_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    fleet_transition_with_scope(state, action, false)
}

fn fleet_transition_with_scope(
    state: &aterm_spec::interp::State,
    action: &str,
    ignore_wrong_scope: bool,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Ordinary" => {
            next.insert("ordinary", 1);
            return next;
        }
        "Cut" => {
            next.insert("cut", state["cut"] + 1);
            return next;
        }
        "Fault" => {
            next.insert("fault", state["fault"] + 1);
            return next;
        }
        "Recover" => {}
        _ => panic!("unknown fleet commit action"),
    }
    let bed = Bed::new();
    let cut = usize::try_from(state["cut"]).unwrap();
    let interrupted = if state["ordinary"] == 1 {
        bed.publish_full(Some(cut)).unwrap_err()
    } else {
        bed.publish(Some(cut)).unwrap_err()
    };
    assert_eq!(interrupted, "injected interruption");
    let admitted = bed.store.receipt().unwrap().unwrap().plan.unwrap();
    let fenced = bed.store.load(NOW).is_err();
    let foreign = bed.root.path().join("foreign.toml");
    let mut caller = bed.roster.clone();
    match state["fault"] {
        0 => {}
        1 => {
            std::fs::create_dir_all(bed.fleet().parent().unwrap()).unwrap();
            std::fs::write(bed.fleet(), "# outside edit\n").unwrap();
        }
        2 if !ignore_wrong_scope => {
            caller = foreign.clone();
        }
        2 => {}
        3 => {
            let mut receipt = bed.store.receipt().unwrap().unwrap();
            receipt.plan.as_mut().unwrap().fleet.as_mut().unwrap().path =
                foreign.to_str().unwrap().into();
            bed.store.save_receipt(&receipt).unwrap();
        }
        4 => {
            std::fs::create_dir_all(bed.roster.parent().unwrap()).unwrap();
            std::fs::write(&bed.roster, "# externally created roster\n").unwrap();
        }
        _ => panic!("unknown fleet fault"),
    }
    let paths = [
        bed.store.state_file(),
        bed.store.ledger_path(),
        bed.store.logins_file(),
        bed.fleet(),
        bed.roster.clone(),
        foreign.clone(),
    ];
    let images = || {
        paths
            .iter()
            .map(|p| std::fs::read(p).ok())
            .collect::<Vec<_>>()
    };
    let before = images();
    let completed = bed
        .store
        .recover_sync_within(&[], &caller)
        .is_ok_and(|changed| changed);
    let after = images();
    let expected = [
        admitted.state.after,
        admitted.ledger.after,
        admitted.logins.after,
        admitted.fleet.unwrap().image.after,
        admitted.roster.after,
        None,
    ]
    .map(|image| image.map(String::into_bytes));
    let exact = !completed || after.as_slice() == expected.as_slice();
    let once =
        !completed || (!bed.store.recover_sync_within(&[], &caller).unwrap() && images() == after);
    next.insert("phase", 1);
    next.insert("completed", i64::from(completed));
    next.insert("fenced", i64::from(fenced));
    next.insert("preserved", i64::from(completed || before == after));
    next.insert("exact", i64::from(exact));
    next.insert("once", i64::from(once));
    next.insert("target_bound", i64::from(!foreign.exists()));
    next
}

#[test]
fn fleet_commit_model_matches_real_recovery_and_rejects_scope_substitution() {
    let model = aterm_models_harness::account_fleet_commit_model();
    aterm_spec::interp::prove_and_catch(&model);
    let mut recoveries = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = fleet_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{state:?}: {actual:?}"
                );
                recoveries += usize::from(action.name == "Recover");
            }
        }
    }
    assert_eq!(recoveries, 70);
    for ordinary in 0..=1 {
        let mut wrong = model.init_state();
        wrong.insert("ordinary", ordinary);
        wrong.insert("fault", 2);
        let bypassed = fleet_transition_with_scope(&wrong, "Recover", true);
        assert!(!model.successors("Recover", &wrong).contains(&bypassed));
        assert!(!model.check_invariant("ConflictRefused", &bypassed));
    }
}
