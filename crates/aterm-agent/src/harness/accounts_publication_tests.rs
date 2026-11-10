// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::account_sync_error::{Retry, SyncError};
use aterm_spec::interp::State as ModelState;

use crate::harness::alab_roster::{ReceiptGuard, SourceGuard};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct ReceiptHost {
    scenario: i64,
    called: Arc<AtomicBool>,
    early: Arc<AtomicBool>,
    roster: PathBuf,
    directory: PathBuf,
}

impl SourceGuard for ReceiptHost {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn same_source(&self, _: &dyn SourceGuard) -> bool {
        false
    }
    fn with_admitted(
        &self,
        save: Box<dyn FnOnce() -> Result<(), crate::harness::account_sync_error::SyncError> + '_>,
    ) -> Result<(), crate::harness::account_sync_error::SyncError> {
        assert!(
            !self.directory.exists(),
            "preparation created a login directory"
        );
        match self.scenario {
            1 => return Err(SyncError::retry(Retry::Changed, "source revision changed")),
            2 => return Ok(()),
            3 => std::fs::write(&self.roster, "# independent edit\n").unwrap(),
            _ => {}
        }
        self.called.store(true, Ordering::SeqCst);
        let result = save();
        self.early.store(self.directory.exists(), Ordering::SeqCst);
        if self.scenario == 3 {
            assert!(result.is_err());
            // An ill-behaved host cannot erase the actual callback failure.
            return Ok(());
        }
        result?;
        if self.scenario == 4 {
            Err(SyncError::retry(
                Retry::Unavailable,
                "host failed after saving",
            ))
        } else {
            Ok(())
        }
    }
}

pub(crate) fn receipt_transition(state: &ModelState, action: &str) -> ModelState {
    receipt_transition_with(state, action, false)
}

fn receipt_transition_with(state: &ModelState, action: &str, bypass: bool) -> ModelState {
    let mut next = state.clone();
    if action == "Scenario" {
        next.insert("scenario", state["scenario"] + 1);
        return next;
    }
    assert_eq!(action, "Publish");
    let scenario = state["scenario"];
    let f = Fixture::new();
    let held = f.store.acquire().unwrap();
    let directory = f.store.login_dir("new").unwrap();
    let after = append_rows_for(
        Cli::Claude,
        "",
        &[Row {
            label: "new".into(),
            email: Some("new@example.test".into()),
            dir: Some(directory.to_str().unwrap().into()),
            ..Row::default()
        }],
    )
    .unwrap();
    let called = Arc::new(AtomicBool::new(false));
    let early = Arc::new(AtomicBool::new(false));
    let source = ReceiptGuard::new(ReceiptHost {
        scenario,
        called: called.clone(),
        early: early.clone(),
        roster: f.roster.clone(),
        directory: directory.clone(),
    });
    CUT.set((scenario == 5).then_some(0));
    let result = f.store.transact_sync_source_held(
        &held,
        1,
        &mut None,
        (
            SyncTarget {
                path: &f.roster,
                before: Some("# initial\n"),
                after: Some(&after),
                mode: None,
                fleet: None,
            },
            (!bypass).then_some(&source),
        ),
        (false, |_, _| Ok(Vec::new())),
    );
    CUT.set(None);
    if !bypass && matches!(scenario, 1 | 3 | 4) {
        assert_eq!(
            result.as_ref().unwrap_err().retry_reason(),
            Some(if scenario != 4 {
                Retry::Changed
            } else {
                Retry::Unavailable
            })
        );
    }
    let pending = f.store.sync_needs_recovery();
    let installed = directory.is_dir() && std::fs::read_to_string(&f.roster).unwrap() == after;
    let recovered = if pending {
        // The host and its captured source are gone. Replay uses only retained intent.
        drop(source);
        f.store.recover_sync_held(&held, &f.roster).unwrap()
            && directory.is_dir()
            && std::fs::read_to_string(&f.roster).unwrap() == after
            && !f.store.sync_needs_recovery()
    } else {
        false
    };
    next.insert("done", 1);
    next.insert("called", i64::from(called.load(Ordering::SeqCst) || bypass));
    next.insert("accepted", i64::from(result.is_ok()));
    next.insert("pending", i64::from(pending));
    next.insert("installed", i64::from(installed));
    next.insert("recovered", i64::from(recovered));
    next.insert("early", i64::from(early.load(Ordering::SeqCst)));
    next
}

#[test]
fn source_receipt_model_drives_real_publication_and_recovery() {
    let model = aterm_models_harness::account_source_receipt_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = receipt_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
            }
        }
    }
    let mut stale = model.init_state();
    stale.insert("scenario", 1);
    let wrong = receipt_transition_with(&stale, "Publish", true);
    assert!(!model.successors("Publish", &stale).contains(&wrong));
    assert!(!model.check_invariant("RefusedSourceUnchanged", &wrong));
}

#[test]
fn state_only_publication_does_not_materialize_roster_login_directories() {
    for held_preparation in [false, true] {
        let f = Fixture::new();
        let directory = f.store.login_dir("missing").unwrap();
        let text = append_rows_for(
            Cli::Claude,
            "",
            &[Row {
                label: "missing".into(),
                dir: Some(directory.to_str().unwrap().into()),
                ..Row::default()
            }],
        )
        .unwrap();
        std::fs::write(&f.roster, &text).unwrap();
        let edit = |state: &mut State, _: &mut Logins| {
            state.active = Some("missing".into());
            state.pinned = true;
            Ok(Vec::new())
        };
        if held_preparation {
            let held = f.store.acquire().unwrap();
            f.store
                .transact_sync_held(
                    &held,
                    1,
                    &mut None,
                    SyncTarget {
                        path: &f.roster,
                        before: Some(&text),
                        after: Some(&text),
                        mode: None,
                        fleet: None,
                    },
                    edit,
                    || Ok(()),
                )
                .unwrap();
        } else {
            f.store
                .transact_roster_logins_within(
                    &LOCK_LADDER,
                    &f.roster,
                    1,
                    &mut None,
                    |_, state, logins| edit(state, logins),
                )
                .unwrap();
        }
        assert!(f.store.load(1).unwrap().0.pinned);
        assert!(!directory.exists());
        assert!(!f.store.sync_needs_recovery());
        assert_eq!(std::fs::read_to_string(&f.roster).unwrap(), text);
    }
}

#[test]
fn missing_login_directory_is_a_recoverable_change_with_unchanged_images() {
    for leaf in ["missing", "missing~1", "missing+2", "custom.home"] {
        let f = Fixture::new();
        let held = f.store.acquire().unwrap();
        let directory = f.store.logins_dir().join(leaf);
        let text = append_rows_for(
            Cli::Claude,
            "",
            &[Row {
                label: "missing".into(),
                dir: Some(directory.to_str().unwrap().into()),
                ..Row::default()
            }],
        )
        .unwrap();
        std::fs::write(&f.roster, &text).unwrap();
        CUT.set(Some(0));
        let result = f.store.transact_roster_held(
            &held,
            1,
            &mut None,
            SyncTarget {
                path: &f.roster,
                before: Some(&text),
                after: Some(&text),
                mode: None,
                fleet: None,
            },
            false,
        );
        CUT.set(None);
        assert!(result.is_err());
        assert!(!directory.exists());
        assert!(f.store.sync_needs_recovery());
        assert!(f.store.recover_sync_held(&held, &f.roster).unwrap());
        assert!(directory.is_dir());
        assert_eq!(std::fs::read_to_string(&f.roster).unwrap(), text);
        // Repeating an already satisfied directory intent makes no new change.
        assert!(
            !f.store
                .transact_roster_held(
                    &held,
                    2,
                    &mut None,
                    SyncTarget {
                        path: &f.roster,
                        before: Some(&text),
                        after: Some(&text),
                        mode: None,
                        fleet: None
                    },
                    false
                )
                .unwrap()
        );
    }
}

#[test]
fn directory_intents_cannot_escape_or_create_an_unlisted_login() {
    let f = Fixture::new();
    for leaf in ["../outside", "/outside", "unlisted"] {
        let mut plan = f.plan(false);
        plan.login_dirs = vec![leaf.into()];
        assert!(plan.validate(&f.store, f.scope()).is_err());
    }
}

#[test]
fn a_roster_path_cannot_expand_directory_intent_scope() {
    let f = Fixture::new();
    for leaf in [
        "../outside",
        "/outside",
        ".",
        "..",
        "nested/child",
        "child/../other",
    ] {
        let directory = f.store.logins_dir().join(leaf);
        let mut plan = f.plan(false);
        plan.roster.after = Some(
            append_rows_for(
                Cli::Claude,
                "",
                &[Row {
                    label: "new".into(),
                    dir: Some(directory.to_str().unwrap().into()),
                    ..Row::default()
                }],
            )
            .unwrap(),
        );
        plan.login_dirs = vec![leaf.into()];
        assert!(plan.validate(&f.store, f.scope()).is_err(), "{leaf}");
    }
}

#[cfg(unix)]
#[test]
fn directory_intents_narrow_new_bindings_only_after_save_and_preserve_files() {
    use std::os::unix::fs::PermissionsExt;

    for new_binding in [false, true] {
        let f = Fixture::new();
        let held = f.store.acquire().unwrap();
        let root = f.store.logins_dir();
        let directory = f.store.login_dir("legacy").unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        for path in [&root, &directory] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let kept = directory.join("retained.txt");
        std::fs::write(&kept, "existing login bytes").unwrap();
        let row = Row {
            label: "legacy".into(),
            dir: Some(directory.to_str().unwrap().into()),
            ..Row::default()
        };
        let before = if new_binding {
            "# initial\n".into()
        } else {
            append_rows_for(Cli::Claude, "", std::slice::from_ref(&row)).unwrap()
        };
        let after = append_rows_for(
            Cli::Claude,
            "",
            &[Row {
                email: Some("legacy@example.test".into()),
                ..row
            }],
        )
        .unwrap();
        std::fs::write(&f.roster, &before).unwrap();
        CUT.set(Some(0));
        let result = f.store.transact_roster_held(
            &held,
            1,
            &mut None,
            SyncTarget {
                path: &f.roster,
                before: Some(&before),
                after: Some(&after),
                mode: None,
                fleet: None,
            },
            false,
        );
        CUT.set(None);
        assert!(result.is_err());
        assert!(f.store.sync_needs_recovery());
        for path in [&root, &directory] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        assert!(f.store.recover_sync_held(&held, &f.roster).unwrap());
        for path in [&root, &directory] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                if new_binding { 0o700 } else { 0o755 }
            );
        }
        assert_eq!(
            std::fs::read_to_string(&kept).unwrap(),
            "existing login bytes"
        );
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    }
}

#[cfg(unix)]
#[test]
fn a_linked_login_root_is_refused_before_retaining_an_intent() {
    let f = Fixture::new();
    let held = f.store.acquire().unwrap();
    let outside = f._root.path().join("outside");
    aterm_uds::private_file::create_private_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, f.store.logins_dir()).unwrap();
    let dir = f.store.login_dir("new").unwrap();
    let after = append_rows_for(
        Cli::Claude,
        "",
        &[Row {
            label: "new".into(),
            dir: Some(dir.to_str().unwrap().into()),
            ..Row::default()
        }],
    )
    .unwrap();
    assert!(
        f.store
            .transact_roster_held(
                &held,
                1,
                &mut None,
                SyncTarget {
                    path: &f.roster,
                    before: Some("# initial\n"),
                    after: Some(&after),
                    mode: None,
                    fleet: None,
                },
                false,
            )
            .is_err()
    );
    assert!(f.receipt().is_none());
    assert!(!outside.join("new").exists());
}

#[test]
fn a_file_in_place_of_a_login_directory_is_preserved_without_an_intent() {
    for file_at_root in [false, true] {
        let f = Fixture::new();
        let held = f.store.acquire().unwrap();
        let directory = f.store.login_dir("new").unwrap();
        let occupied = if file_at_root {
            f.store.logins_dir()
        } else {
            std::fs::create_dir_all(f.store.logins_dir()).unwrap();
            directory.clone()
        };
        std::fs::write(&occupied, "keep this file").unwrap();
        let after = append_rows_for(
            Cli::Claude,
            "",
            &[Row {
                label: "new".into(),
                dir: Some(directory.to_str().unwrap().into()),
                ..Row::default()
            }],
        )
        .unwrap();
        assert!(
            f.store
                .transact_roster_held(
                    &held,
                    1,
                    &mut None,
                    SyncTarget {
                        path: &f.roster,
                        before: Some("# initial\n"),
                        after: Some(&after),
                        mode: None,
                        fleet: None,
                    },
                    false,
                )
                .is_err()
        );
        assert!(f.receipt().is_none());
        assert_eq!(
            std::fs::read_to_string(&occupied).unwrap(),
            "keep this file"
        );
        assert_eq!(std::fs::read_to_string(&f.roster).unwrap(), "# initial\n");
    }
}

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
            login_dirs: Vec::new(),
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
            outbox: None,
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
