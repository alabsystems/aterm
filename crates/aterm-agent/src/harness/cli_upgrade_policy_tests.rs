// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Fixture {
    root: Tmp,
    env: Env,
    store: accounts::Store,
    path: PathBuf,
}

impl Fixture {
    fn new(rows: usize) -> Self {
        let root = tmp_dir("upgrade-policy");
        let env = env_at(&root);
        let store = env.accounts_store().bounded_by(&[]);
        let path = env.accounts_roster().unwrap();
        let fixture = Self {
            root,
            env,
            store,
            path,
        };
        std::fs::write(&fixture.path, fixture.roster(rows)).unwrap();
        fixture
    }

    fn roster(&self, count: usize) -> String {
        let rows: Vec<_> = (0..count)
            .map(|index| accounts::Row {
                label: format!("row-{index}"),
                dir: Some(
                    self.root
                        .path()
                        .join(format!("login-{index}"))
                        .to_str()
                        .unwrap()
                        .to_owned(),
                ),
                ..accounts::Row::default()
            })
            .collect();
        accounts::append_rows_for(Cli::Claude, "", &rows).unwrap()
    }

    fn pending(&self, rows: usize) {
        // Establish witnesses, then interrupt after the journal is admitted and before
        // the roster image is installed. No process is killed and no credentials exist.
        self.store
            .transact(self.env.now, &mut None, |state| {
                state.active = Some("row-0".into());
                Ok(Vec::new())
            })
            .unwrap();
        let witness = self.store.state_witness_file();
        let retained = witness.with_extension("retained");
        std::fs::rename(&witness, &retained).unwrap();
        std::fs::create_dir(&witness).unwrap();
        let before = std::fs::read_to_string(&self.path).unwrap();
        let after = self.roster(rows);
        let held = self.store.acquire().unwrap();
        assert!(
            self.store
                .transact_sync_held(
                    &held,
                    self.env.now,
                    &mut None,
                    accounts::SyncTarget {
                        path: &self.path,
                        before: Some(&before),
                        after: Some(&after),
                        mode: None
                    },
                    |_, _| Ok(Vec::new()),
                    || Ok(())
                )
                .is_err()
        );
        drop(held);
        std::fs::rename(&witness, witness.with_extension("obstruction")).unwrap();
        std::fs::rename(retained, witness).unwrap();
        assert_eq!(std::fs::read_to_string(&self.path).unwrap(), before);
    }

    fn opts(&self, tap: bool) -> super::super::super::upgrade_drive::Opts {
        let policy = SupervisorConfig {
            usage_tap: tap,
            ..SupervisorConfig::default()
        };
        upgrade_opts(&self.env, "", &policy, || Some(PathBuf::from("/aterm"))).unwrap()
    }
}

#[test]
fn upgrades_recover_cardinality_before_reading_model_and_tap_policy() {
    for (before, after) in [(1, 2), (2, 1), (0, 2), (2, 0)] {
        for tap in [false, true] {
            let f = Fixture::new(before);
            f.pending(after);
            let opts = f.opts(tap);
            assert_eq!(
                opts.several_logins,
                after > 1,
                "{before} -> {after}, tap={tap}"
            );
            if let Some(armed) = opts.usage_tap {
                assert_eq!(armed.listed.len(), if after > 1 { after } else { 0 });
                assert_eq!(armed.lone_login.is_some(), after == 1);
            } else {
                assert!(!tap);
            }
            assert_eq!(std::fs::read_to_string(&f.path).unwrap(), f.roster(after));
        }
    }
}

#[test]
fn unreadable_upgrade_policy_cannot_enable_home_evidence_or_a_default_tap() {
    for tap in [false, true] {
        let f = Fixture::new(1);
        std::fs::write(&f.path, "[[account]").unwrap();
        let opts = f.opts(tap);
        assert!(opts.several_logins, "unknown is restrictive");
        assert!(
            opts.usage_tap.is_none(),
            "unknown cannot name the default login"
        );
    }
}

#[test]
fn model_reporting_recovers_even_without_a_usage_tap() {
    let f = Fixture::new(1);
    f.pending(2);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        run_upgrade_models(&f.env, None, &mut out, &mut err),
        ExitCode::SUCCESS
    );
    assert_eq!(std::fs::read_to_string(&f.path).unwrap(), f.roster(2));
    assert!(!out.is_empty());
}

#[test]
fn a_tap_cannot_be_armed_from_unknown_account_records_or_time() {
    let f = Fixture::new(1);
    std::fs::create_dir_all(&f.store.dir).unwrap();
    std::fs::write(f.store.logins_file(), "{").unwrap();
    assert!(
        super::super::super::tap::Armed::arm(
            true,
            || Some(PathBuf::from("/aterm")),
            Some(&f.path),
            &f.store,
            Some(f.env.now)
        )
        .is_none()
    );
    let clean = Fixture::new(1);
    assert!(
        super::super::super::tap::Armed::arm(
            true,
            || Some(PathBuf::from("/aterm")),
            Some(&clean.path),
            &clean.store,
            None
        )
        .is_none()
    );
}

#[test]
fn a_disabled_or_missing_tap_does_not_recover_or_resolve_an_executable() {
    let f = Fixture::new(1);
    f.pending(2);
    let paths = [f.path.clone(), f.store.dir.join("sync-commit.json")];
    let before = paths.each_ref().map(|path| std::fs::read(path).unwrap());
    assert!(
        super::super::super::tap::Armed::arm(
            false,
            || panic!("disabled tap must not resolve executable"),
            Some(&f.path),
            &f.store,
            Some(f.env.now)
        )
        .is_none()
    );
    assert!(
        super::super::super::tap::Armed::arm(
            true,
            || None,
            Some(&f.path),
            &f.store,
            Some(f.env.now)
        )
        .is_none()
    );
    assert_eq!(
        paths.each_ref().map(|path| std::fs::read(path).unwrap()),
        before
    );
}

fn policy_case(
    shape: usize,
    fault: i64,
    on: bool,
    exe: bool,
) -> (super::super::super::tap::LaunchPolicy, bool) {
    let pending = matches!(fault, 1 | 4 | 5);
    let f = Fixture::new(if pending {
        if shape == 2 { 1 } else { 2 }
    } else {
        shape
    });
    if pending {
        f.pending(shape);
    }
    if fault == 2 {
        std::fs::write(&f.path, "[[account]").unwrap();
    }
    if fault == 3 {
        std::fs::create_dir_all(&f.store.dir).unwrap();
        std::fs::write(f.store.dir.join("sync-commit.json"), "{").unwrap();
    }
    if fault == 5 {
        std::fs::write(&f.path, "# another writer\n").unwrap();
    }
    let _held = (fault == 4).then(|| f.store.acquire().unwrap());
    let paths = [
        f.path.clone(),
        f.store.state_file(),
        f.store.logins_file(),
        f.store.dir.join("sync-commit.json"),
    ];
    let bytes = || {
        paths
            .iter()
            .map(|path| std::fs::read(path).ok())
            .collect::<Vec<_>>()
    };
    let before = bytes();
    let read = || {
        super::super::super::tap::LaunchPolicy::read(
            on,
            || exe.then(|| PathBuf::from("/aterm")),
            Some(&f.path),
            &f.store,
            (fault != 6).then_some(f.env.now),
        )
    };
    let policy = read();
    let after = bytes();
    assert_eq!(read(), policy, "repeat read");
    assert_eq!(bytes(), after, "repeat does not write");
    if fault != 1 {
        assert_eq!(before, after, "non-recovery read does not write");
    } else {
        assert_eq!(std::fs::read_to_string(&f.path).unwrap(), f.roster(shape));
    }
    let matching = policy.usage_tap.as_ref().is_none_or(|armed| {
        let wanted: Vec<_> = (0..shape)
            .map(|i| {
                f.root
                    .path()
                    .join(format!("login-{i}"))
                    .to_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        armed.exe.as_path() == Path::new("/aterm")
            && match shape {
                0 => armed.lone_login.is_none() && armed.listed.is_empty(),
                1 => armed.lone_login.as_ref() == wanted.first() && armed.listed.is_empty(),
                _ => armed.lone_login.is_none() && armed.listed == wanted,
            }
    });
    (policy, matching)
}

pub(crate) fn launch_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherShape" => {
            next.insert("shape", state["shape"] + 1);
        }
        "AnotherFault" => {
            next.insert("fault", state["fault"] + 1);
        }
        "EnableTap" => {
            next.insert("on", 1);
        }
        "HaveExecutable" => {
            next.insert("exe", 1);
        }
        "Read" => {
            let (policy, matching) = policy_case(
                usize::try_from(state["shape"]).unwrap(),
                state["fault"],
                state["on"] == 1,
                state["exe"] == 1,
            );
            next.insert("phase", 1);
            next.insert("several", i64::from(policy.several_logins));
            next.insert("armed", i64::from(policy.usage_tap.is_some()));
            next.insert("matching_login", i64::from(matching));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn launch_policy_refines_recovery_and_unknown_account_rules() {
    let model = aterm_models_harness::account_launch_read_model();
    assert_eq!(model.mutant_dials(), vec![1, 2, 3, 4, 5]);
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut cases = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = launch_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                cases += usize::from(action.name == "Read");
            }
        }
    }
    assert_eq!(cases, 84);
    let mut bad = model.init_state();
    bad.insert("phase", 1);
    bad.insert("shape", 2);
    assert!(!model.check_invariant("RecoveredCardinality", &bad));
    bad.insert("fault", 2);
    assert!(!model.check_invariant("UnknownEvidenceRestricted", &bad));
    bad.insert("armed", 1);
    assert!(!model.check_invariant("TapOnlyWithKnownEnabledImage", &bad));
    bad.insert("fault", 0);
    bad.insert("on", 1);
    bad.insert("exe", 1);
    bad.insert("armed", 0);
    assert!(!model.check_invariant("KnownEnabledTapAvailable", &bad));
    bad.insert("matching_login", 0);
    assert!(!model.check_invariant("LoginComesFromSnapshot", &bad));
}

#[test]
fn a_launch_policy_is_retained_and_absent_records_create_no_storage() {
    let f = Fixture::new(2);
    let policy = super::super::super::tap::LaunchPolicy::read(
        true,
        || Some(PathBuf::from("/aterm")),
        Some(&f.path),
        &f.store,
        Some(f.env.now),
    );
    assert!(policy.several_logins);
    let directories = policy.usage_tap.as_ref().unwrap().listed.clone();
    std::fs::write(&f.path, f.roster(1)).unwrap();
    assert!(policy.several_logins);
    assert_eq!(policy.usage_tap.as_ref().unwrap().listed, directories);
    assert!(
        !f.opts(true).several_logins,
        "a new decision observes the changed roster"
    );
    assert!(!f.store.dir.exists());

    for path in [None, Some(f.root.path().join("absent.toml"))] {
        let policy = super::super::super::tap::LaunchPolicy::read(
            true,
            || Some(PathBuf::from("/aterm")),
            path.as_deref(),
            &f.store,
            Some(f.env.now),
        );
        assert!(!policy.several_logins);
        assert!(policy.usage_tap.unwrap().lone_login.is_none());
        assert!(!f.store.dir.exists());
    }
}
