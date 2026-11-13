// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The tests gated `cfg(unix)` here start a manual sign-in or select a row that needs one,
// which only Unix queues (its private FIFO, `accounts::manual_signin_capability`).
#![cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "some helpers serve only the tests gated `cfg(unix)`"
    )
)]

use super::*;
use crate::ai::connector::{Flow, Region};
use crate::harness::accounts_overlay::model_provider;
use crate::harness::login_drive::{Drove, Outcome, Request};
use std::cell::{Cell, RefCell};

const NOW: i64 = 1_790_000_000;
const OWNER: &str = "owner@example.test";

#[cfg(unix)]
#[test]
fn native_google_retry_recovers_its_encrypted_completion_before_starting_another_login() {
    struct Host {
        calls: Cell<usize>,
        fresh: Cell<usize>,
        keys: RefCell<Vec<[u8; 32]>>,
    }
    impl AccountsHost for Host {
        fn stdin_is_terminal(&self) -> bool {
            false
        }
        fn auth_status(&self, _: &str) -> Result<Captured, String> {
            unreachable!()
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            unreachable!()
        }
        fn now(&self) -> i64 {
            NOW
        }
        fn vendor_sign_in(&self, _: &Request<'_>) -> Drove {
            panic!("generic sign-in ran")
        }
        fn native_google_sign_in(
            &self,
            req: &Request<'_>,
            recovery: crate::harness::agy_signin::RecoveryKey,
        ) -> Result<crate::harness::agy_signin::VerifiedGoogleGrant, String> {
            self.keys.borrow_mut().push(recovery.0);
            let first = self.calls.get() == 0;
            self.calls.set(self.calls.get() + 1);
            let (result, fresh) =
                crate::harness::agy_signin::recovery_publication_fixture(req, recovery, first);
            self.fresh.set(self.fresh.get() + usize::from(fresh));
            result
        }
    }
    let world = World::new(Cli::Agy);
    let state = world.store.dir.parent().unwrap();
    aterm_secrets::fsx::write_atomic(&state.join("google-identities.toml"),
        b"[[identities]]\nid='one'\nemail='owner@example.test'\n[[bindings]]\nprovider='agy'\naccount='main'\nidentity='one'\nexpected_email='owner@example.test'\n", 0o600).unwrap();
    let host = Host {
        calls: Cell::new(0),
        fresh: Cell::new(0),
        keys: RefCell::new(Vec::new()),
    };
    let original = world.store.load_logins(NOW).unwrap().0.labels["main"]
        .dir
        .clone();
    for attempt in 0..3 {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let result =
            run_accounts_login_for(Cli::Agy, &world.env, &host, "main", &mut out, &mut err);
        assert_eq!(
            result == ExitCode::SUCCESS,
            attempt != 0,
            "{}{}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        );
        assert_eq!(host.fresh.get(), if attempt < 2 { 1 } else { 2 });
        let logins = world.store.load_logins(NOW).unwrap().0;
        if attempt == 0 {
            assert_eq!(logins.labels["main"].dir, original);
        } else {
            assert_eq!(logins.labels["main"].google_principal, Some([1; 32]));
        }
        let candidates = std::fs::read_dir(state.join("pending-native-google"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].path().join("candidate.age").is_file(),
            attempt == 0
        );
    }
    let keys = host.keys.borrow();
    assert_eq!(
        keys[0], keys[1],
        "a transient failure changed the admitted attempt scope"
    );
    assert_ne!(
        keys[1], keys[2],
        "publication did not fence the consumed completion"
    );
}

#[cfg(unix)]
#[test]
fn native_google_publication_retains_and_rechecks_its_typed_proof() {
    struct NativeHost<'a> {
        world: &'a World,
        mutation: u8,
        called: Cell<bool>,
    }
    impl AccountsHost for NativeHost<'_> {
        fn stdin_is_terminal(&self) -> bool {
            false
        }
        fn auth_status(&self, _: &str) -> Result<Captured, String> {
            unreachable!()
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            unreachable!()
        }
        fn now(&self) -> i64 {
            NOW
        }
        fn vendor_sign_in(&self, _: &Request<'_>) -> Drove {
            panic!("native Google must not use the generic sign-in driver")
        }
        fn native_google_sign_in(
            &self,
            req: &Request<'_>,
            _recovery: crate::harness::agy_signin::RecoveryKey,
        ) -> Result<crate::harness::agy_signin::VerifiedGoogleGrant, String> {
            self.called.set(true);
            let state = self.world.store.dir.parent().unwrap();
            let another = self.world.root.path().join("other-google-state");
            if self.mutation == 6 {
                aterm_secrets::fsx::ensure_private_dir(&another).unwrap();
                aterm_secrets::fsx::write_atomic(
                    &another.join("google-identities.toml"),
                    &std::fs::read(state.join("google-identities.toml")).unwrap(),
                    0o600,
                )
                .unwrap();
            }
            let proof = crate::harness::agy_signin::publication_fixture(
                if self.mutation == 6 { &another } else { state },
                req.staging,
                "main",
                OWNER,
            );
            match self.mutation {
                1 => std::fs::write(proof.path().unwrap(), b"changed-after-verification").unwrap(),
                2 => std::fs::write(state.join("google-identities.toml"), "").unwrap(),
                3 => return Err("provider refused authorization".into()),
                5 => {
                    let history = req.staging.join(".gemini/antigravity-cli/conversations");
                    std::fs::remove_file(&history).unwrap();
                    std::fs::create_dir(&history).unwrap();
                    std::fs::write(history.join("kept.db"), b"native private history").unwrap();
                }
                _ => {}
            }
            Ok(proof)
        }
    }
    for mutation in 0..=6 {
        let world = World::new(Cli::Agy);
        let state = world.store.dir.parent().unwrap();
        aterm_secrets::fsx::write_atomic(
            &state.join("google-identities.toml"),
            b"[[identities]]\nid='one'\nemail='owner@example.test'\n[[bindings]]\nprovider='agy'\naccount='main'\nidentity='one'\nexpected_email='owner@example.test'\n",
            0o600,
        ).unwrap();
        if mutation == 4 {
            world
                .store
                .transact_logins(NOW, &mut None, |_, logins| {
                    accounts::note_google_identity(logins, "main", OWNER, [2; 32]);
                    Ok(Vec::new())
                })
                .unwrap();
        }
        let host = NativeHost {
            world: &world,
            mutation,
            called: Cell::new(false),
        };
        let before = world.store.load_logins(NOW).unwrap().0;
        let original_dir = before
            .labels
            .get("main")
            .and_then(|binding| binding.dir.clone());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let result =
            run_accounts_login_for(Cli::Agy, &world.env, &host, "main", &mut out, &mut err);
        assert!(host.called.get(), "{}", String::from_utf8_lossy(&err));
        assert_eq!(
            result == ExitCode::SUCCESS,
            mutation == 0,
            "{}{}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        );
        let after = world.store.load_logins(NOW).unwrap().0;
        if mutation == 0 {
            assert_eq!(after.labels["main"].google_principal, Some([1; 32]));
            assert_ne!(
                after
                    .labels
                    .get("main")
                    .and_then(|binding| binding.dir.as_ref()),
                original_dir.as_ref()
            );
        } else {
            if mutation == 4 {
                assert_eq!(after.labels["main"].google_principal, Some([2; 32]));
            }
            assert_eq!(
                after
                    .labels
                    .get("main")
                    .and_then(|binding| binding.dir.as_ref()),
                original_dir.as_ref()
            );
        }
    }
}

struct World {
    root: Tmp,
    env: Env,
    cli: Cli,
    store: accounts::Store,
    path: PathBuf,
    raw: Roster,
}

impl World {
    fn new(cli: Cli) -> Self {
        let tmp = tmp_dir("signin-policy");
        let mut env = env_at(&tmp);
        env.now = NOW;
        let store = env.accounts_store_for(cli).unwrap();
        let path = env.accounts_roster_for(cli).unwrap();
        let raw = Roster {
            rows: vec![
                accounts::Row {
                    label: "main".into(),
                    email: Some(OWNER.into()),
                    dir: Some(tmp.path().join("raw-home").to_str().unwrap().into()),
                    vendor: accounts::Vendor {
                        region: (cli == Cli::Kimi).then_some(Region::Global),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                accounts::Row {
                    label: "other".into(),
                    email: Some("other@example.test".into()),
                    dir: Some(tmp.path().join("other").to_str().unwrap().into()),
                    vendor: accounts::Vendor {
                        region: (cli == Cli::Kimi).then_some(Region::Global),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ],
        };
        std::fs::write(
            &path,
            accounts::append_rows_for(cli, "", &raw.rows).unwrap(),
        )
        .unwrap();
        let old = store.login_dir("main").unwrap();
        store
            .transact_logins(NOW - 1, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "main",
                    raw.rows[0].dir.as_deref(),
                    old.to_str().unwrap(),
                    None,
                    NOW - 1,
                ))
            })
            .unwrap();
        Self {
            root: tmp,
            env,
            cli,
            store,
            path,
            raw,
        }
    }

    fn records(&self) -> [Option<Vec<u8>>; 4] {
        [
            self.store.state_file(),
            self.store.logins_file(),
            self.store.ledger_path(),
            self.store.wake_path(),
        ]
        .map(|path| std::fs::read(path).ok())
    }

    fn change(&self, case: i64) {
        let mut raw = self.raw.clone();
        match case {
            1 => return,
            2 => raw.rows[0].email = Some("changed@example.test".into()),
            3 => {
                raw.rows.remove(0);
            }
            4 => {
                std::fs::rename(&self.path, self.path.with_extension("kept")).unwrap();
                return;
            }
            5 => {
                std::fs::write(&self.path, "[[invalid").unwrap();
                return;
            }
            6 => {
                raw.rows[0].dir = Some(
                    self.root
                        .path()
                        .join("changed-home")
                        .to_str()
                        .unwrap()
                        .into(),
                )
            }
            7 => raw.rows[0].class = Class::Pool,
            8 => raw.rows[0].browser = Some("Profile 2".into()),
            9 => {
                if self.cli == Cli::Kimi {
                    raw.rows[0].vendor.region = Some(Region::MainlandCn);
                } else {
                    raw.rows[0].vendor.flow = Some(match raw.rows[0].flow(self.cli).unwrap() {
                        Flow::Browser => Flow::Device,
                        Flow::Device => Flow::Browser,
                    });
                }
            }
            10 => raw.rows[1].email = Some("unrelated@example.test".into()),
            _ => unreachable!(),
        }
        std::fs::write(
            &self.path,
            accounts::append_rows_for(self.cli, "", &raw.rows).unwrap(),
        )
        .unwrap();
    }
}

struct PolicyHost<'a> {
    world: &'a World,
    case: i64,
    outcome: u8,
    called: Cell<bool>,
    staging: RefCell<Option<PathBuf>>,
    after: RefCell<Option<[Option<Vec<u8>>; 4]>>,
}

impl<'a> PolicyHost<'a> {
    fn new(world: &'a World, case: i64, outcome: u8) -> Self {
        Self {
            world,
            case,
            outcome,
            called: Cell::new(false),
            staging: RefCell::new(None),
            after: RefCell::new(None),
        }
    }

    fn authenticate(&self, req: &Request<'_>) {
        assert!(!self.called.replace(true));
        let login = req.staging.join(self.world.cli.connector().stores[0]);
        std::fs::create_dir_all(login.parent().unwrap()).unwrap();
        std::fs::write(login, b"completed private fixture").unwrap();
        *self.staging.borrow_mut() = Some(req.staging.to_owned());
        self.world.change(self.case);
        *self.after.borrow_mut() = Some(self.world.records());
    }

    fn run(&self, mode: i64) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _exit = if mode == 1 {
            run_accounts_login_for(
                self.world.cli,
                &self.world.env,
                self,
                "main",
                &mut out,
                &mut err,
            )
        } else {
            run_accounts_renew_for(
                self.world.cli,
                &self.world.env,
                self,
                (&["main".into()], false, None),
                &mut out,
                &mut err,
            )
        };
        assert!(self.called.get(), "{}", String::from_utf8_lossy(&err));
    }
}

impl AccountsHost for PolicyHost<'_> {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        unreachable!()
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        unreachable!()
    }
    fn now(&self) -> i64 {
        NOW
    }
    fn vendor_login(&self, req: &Request<'_>) -> Result<i32, String> {
        self.authenticate(req);
        Ok(if self.outcome == 3 { 1 } else { 0 })
    }
    fn vendor_sign_in(&self, req: &Request<'_>) -> Drove {
        self.authenticate(req);
        Drove {
            host: None,
            outcome: match self.outcome {
                0 => Outcome::SignedIn {
                    email: Some(OWNER.into()),
                    page: false,
                },
                1 => Outcome::SignedIn {
                    email: Some("wrong@example.test".into()),
                    page: false,
                },
                2 => Outcome::NeedsSignIn("fixture".into()),
                _ => unreachable!(),
            },
        }
    }
    fn codex_account(&self, _: &Path) -> Option<String> {
        Some(OWNER.into())
    }
}

pub(crate) fn policy_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherProvider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "AnotherMode" => {
            next.insert("mode", 2);
        }
        "AnotherOutcome" => {
            next.insert(
                "outcome",
                if state["mode"] == 1 {
                    3
                } else {
                    state["outcome"] + 1
                },
            );
        }
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Choose" => {
            next.insert("phase", 1);
        }
        "Publish" => {
            let world = World::new(model_provider(state["provider"]));
            let host = PolicyHost::new(
                &world,
                state["case"],
                u8::try_from(state["outcome"]).unwrap(),
            );
            host.run(state["mode"]);
            let staging = host.staging.borrow().clone().unwrap();
            let published = world.records() != host.after.borrow().clone().unwrap();
            if state["case"] > 1 && state["case"] <= 9 {
                assert_eq!(
                    world.records(),
                    host.after.borrow().clone().unwrap(),
                    "no stale record"
                );
                assert!(!staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
            }
            next.insert("phase", 2);
            next.insert("published", i64::from(published));
            next.insert(
                "kept",
                i64::from(staging.join(world.cli.connector().stores[0]).is_file()),
            );
        }
        _ => unreachable!(),
    }
    next
}

#[cfg(unix)]
#[test]
fn vendor_login_and_renewal_policy_conform_with_independent_edits_allowed() {
    let model = aterm_models_harness::account_signin_policy_model();
    assert_eq!(
        model.mutant_dials(),
        (1..=14).collect::<Vec<_>>(),
        "the runner must discover every field, outcome and retention negative control"
    );
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                if action.name == "Publish" && !links_here(model_provider(state["provider"])) {
                    continue;
                }
                let next = policy_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut state = model.init_state();
    state.insert("phase", 1);
    state.insert("case", 2);
    let mut stale = policy_transition(&state, "Publish");
    stale.insert("published", 1);
    assert!(!model.check_invariant("CurrentTargetOnly", &stale));
}

#[test]
fn vendor_policy_is_rechecked_before_queued_authentication_starts() {
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        for case in 2..=9 {
            let world = World::new(cli);
            let logins = world.store.load_logins(NOW).unwrap().0;
            let rows = accounts::bound(&world.raw, &logins);
            world.change(case);
            let before = world.records();
            let host = PolicyHost::new(&world, 1, 0);
            let result = renew_one_for(
                cli,
                NOW,
                &host,
                &world.store,
                (rows.row("main").unwrap(), &world.path, &logins),
                (
                    world.raw.rows[0].dir.as_deref(),
                    crate::harness::sign_in_ladder::asked_fixture(cli, "main"),
                ),
                &mut Vec::new(),
                &mut Vec::new(),
            );
            assert_eq!(result, Renewal::Deferred, "{cli:?}, case {case}");
            assert!(!host.called.get());
            assert_eq!(world.records(), before);
            assert!(!world.store.logins_dir().join("main+1").exists());
        }
    }
}

struct ObservationHost<'a> {
    base: PolicyHost<'a>,
    operation: i64,
    fault: i64,
    obstruction: RefCell<Option<(PathBuf, Option<PathBuf>)>>,
}

impl ObservationHost<'_> {
    fn obstruct(&self) {
        let store = &self.base.world.store;
        let path = match self.fault {
            1 => return,
            2 => store.ledger_path(),
            3 => store.state_witness_file(),
            4 => store.logins_witness_file(),
            5 => store.wake_path(),
            _ => unreachable!(),
        };
        let retained = path.exists().then(|| {
            let retained = path.with_extension("retained");
            std::fs::rename(&path, &retained).unwrap();
            retained
        });
        std::fs::create_dir(&path).unwrap();
        *self.obstruction.borrow_mut() = Some((path, retained));
    }

    fn repair(&self) {
        if let Some((path, retained)) = self.obstruction.borrow_mut().take() {
            std::fs::rename(&path, path.with_extension("obstruction")).unwrap();
            if let Some(retained) = retained {
                std::fs::rename(retained, path).unwrap();
            }
        }
    }
}

impl AccountsHost for ObservationHost<'_> {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        unreachable!()
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        unreachable!()
    }
    fn now(&self) -> i64 {
        NOW
    }
    fn vendor_sign_in(&self, req: &Request<'_>) -> Drove {
        self.base.authenticate(req);
        self.obstruct();
        Drove {
            host: None,
            outcome: match self.operation {
                1 => Outcome::SignedIn {
                    email: Some("wrong@example.test".into()),
                    page: false,
                },
                2 => Outcome::NeedsSignIn("fixture".into()),
                3 => Outcome::NeedsYou("fixture".into()),
                5 => Outcome::Hung,
                6 | 7 => Outcome::SignedIn {
                    email: None,
                    page: false,
                },
                _ => unreachable!(),
            },
        }
    }
    fn codex_account(&self, _: &Path) -> Option<String> {
        (self.operation == 6).then(|| "wrong@example.test".into())
    }
}

fn observation_host(world: &World, operation: i64, fault: i64) -> ObservationHost<'_> {
    ObservationHost {
        base: PolicyHost::new(world, 1, 0),
        operation,
        fault,
        obstruction: RefCell::new(None),
    }
}

fn run_observation(world: &World, host: &ObservationHost<'_>) -> bool {
    let logins = world.store.load_logins(NOW).unwrap().0;
    let rows = accounts::bound(&world.raw, &logins);
    if host.operation == 4 {
        host.obstruct();
        return accounts_observe_signin(
            &world.store,
            &world.path,
            NOW,
            &mut Vec::new(),
            |_, _, _| {
                Ok(vec![
                    accounts::Event::new(NOW, accounts::Kind::SignIn, "main")
                        .because("fixture window closed session-kept"),
                ])
            },
        )
        .is_ok();
    }
    let result = renew_one_for(
        world.cli,
        NOW,
        host,
        &world.store,
        (rows.row("main").unwrap(), &world.path, &logins),
        (
            world.raw.rows[0].dir.as_deref(),
            crate::harness::sign_in_ladder::asked_fixture(world.cli, "main"),
        ),
        &mut Vec::new(),
        &mut Vec::new(),
    );
    matches!(result, Renewal::WrongAccount | Renewal::NeedsPerson)
}

#[cfg(unix)]
#[test]
fn failed_login_observations_never_publish_without_their_history() {
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        if !links_here(cli) {
            continue;
        }
        for operation in 1..=3 {
            let world = World::new(cli);
            let host = observation_host(&world, operation, 2);
            assert!(!run_observation(&world, &host));
            let admitted = host.base.after.borrow().clone().unwrap();
            host.repair();
            assert_eq!(world.records(), admitted, "{cli:?}, operation {operation}");
        }
    }
}

#[cfg(unix)]
#[test]
fn unscoped_legacy_guidance_is_history_until_its_own_fresh_observation() {
    let world = World::new(Cli::Kimi);
    let host = observation_host(&world, 2, 1);
    assert!(run_observation(&world, &host));
    assert!(host.base.called.get());
    let captured = world
        .store
        .read_account_records(Some(&world.path), NOW)
        .unwrap();
    let row = captured.raw_roster.row("main").unwrap();
    let task = accounts::current_sign_in_task(row, &captured.logins).unwrap();
    assert_eq!(task.action, accounts::SignInAction::Browser);
    let parent = accounts::RenewalParent::capture(&captured.logins, "main");
    accounts_observe_signin(
        &world.store,
        &world.path,
        NOW,
        &mut Vec::new(),
        |raw, _, logins| {
            parent.require_current(logins, "main")?;
            signin_policy_matches(&world.raw.rows[0], raw, world.raw.rows[0].dir.as_deref())?;
            accounts::admit_workflow(logins, "main")?;
            Ok(accounts::note_sign_in_attempt(
                logins,
                "main",
                accounts::AttemptOutcome::NoRoute,
                "fixture route",
                NOW,
            ))
        },
    )
    .unwrap();
    let scoped = world
        .store
        .read_account_records(Some(&world.path), NOW + 1)
        .unwrap();
    assert_eq!(
        accounts::current_sign_in_task(scoped.raw_roster.row("main").unwrap(), &scoped.logins),
        Some(task)
    );
    assert!(
        accounts::current_sign_in_attempt(scoped.raw_roster.row("main").unwrap(), &scoped.logins)
            .is_some()
    );
    let original_roster = std::fs::read(&world.path).unwrap();
    let binding = scoped.logins.get("main").unwrap().clone();
    let credential = Path::new(binding.dir.as_ref().unwrap()).join(world.cli.connector().stores[0]);
    std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
    std::fs::write(&credential, b"retained owned fixture credential").unwrap();
    let credential_before = std::fs::read(&credential).unwrap();

    // This is exactly the old wire shape, not a fresh grant of the old findings:
    // keep every task/attempt/account fact while omitting only workflow metadata.
    let mut wire: Value = aterm_json::from_str(&scoped.logins.to_json()).unwrap();
    let object = wire.as_object_mut().unwrap();
    object.remove("workflow_clock");
    object.remove("workflows");
    std::fs::write(world.store.logins_file(), wire.to_string()).unwrap();
    let mut legacy_lines = Vec::new();
    for event in world.store.tail_validated(100).unwrap() {
        let mut wire: Value = aterm_json::from_str(&event.to_json_line()).unwrap();
        wire.as_object_mut().unwrap().remove("workflow");
        legacy_lines.push(wire.to_string());
    }
    std::fs::write(
        world.store.ledger_path(),
        format!("{}\n", legacy_lines.join("\n")),
    )
    .unwrap();
    let historical = world.records();
    let prose = [
        accounts::Event::new(NOW, accounts::Kind::SignIn, "main").because("ordinary sign-in text"),
        accounts::Event::new(NOW, accounts::Kind::SignIn, "main")
            .because("kimi window done session-kept"),
    ];
    let mut decisions = Vec::new();
    for case in [1, 2, 8, 1] {
        std::fs::write(&world.path, &original_roster).unwrap();
        world.change(case);
        let records = world
            .store
            .read_account_records(Some(&world.path), NOW + 2)
            .unwrap();
        let raw = accounts::Roster::load_for(world.cli, &world.path).unwrap();
        let row = raw.row("main").unwrap();
        assert_eq!(records.logins.get("main"), Some(&binding));
        assert_eq!(
            world.records(),
            historical,
            "reading policy must retain history"
        );
        assert_eq!(std::fs::read(&credential).unwrap(), credential_before);
        decisions.push((
            case,
            accounts::current_sign_in_task(row, &records.logins).is_some(),
            accounts::current_sign_in_attempt(row, &records.logins).is_some(),
            prose
                .iter()
                .any(|event| accounts::workflow_event_matches(row, &records.logins, event)),
            crate::harness::accounts_sync::unaddressed_sign_in_task_at(
                &world.store,
                &records.logins,
                row,
            )
            .is_some(),
        ));
    }
    // A newly admitted producer can publish its own different task, but cannot
    // turn the unchanged legacy attempt or old browser prose into new evidence.
    std::fs::write(&world.path, &original_roster).unwrap();
    let fresh = observation_host(&world, 3, 1);
    assert!(run_observation(&world, &fresh));
    let records = world
        .store
        .read_account_records(Some(&world.path), NOW + 3)
        .unwrap();
    let row = world.raw.row("main").unwrap();
    let fresh_action = accounts::current_sign_in_task(row, &records.logins).map(|task| task.action);
    let old_attempt_current = accounts::current_sign_in_attempt(row, &records.logins).is_some();
    let old_prose_current = prose
        .iter()
        .any(|event| accounts::workflow_event_matches(row, &records.logins, event));
    assert_eq!(
        records.logins.get("main").unwrap().sign_in_attempt,
        binding.sign_in_attempt
    );
    assert!(
        std::fs::read(world.store.ledger_path())
            .unwrap()
            .starts_with(historical[2].as_ref().unwrap())
    );
    assert_eq!(std::fs::read(&credential).unwrap(), credential_before);
    assert_eq!(
        decisions,
        vec![
            (1, false, false, false, false),
            (2, false, false, false, false),
            (8, false, false, false, false),
            (1, false, false, false, false)
        ]
    );
    assert_eq!(fresh_action, Some(accounts::SignInAction::Login));
    assert!(
        !old_attempt_current && !old_prose_current,
        "fresh work cannot adopt legacy findings"
    );
}

#[cfg(unix)]
fn observation_case(cli: Cli, operation: i64, fault: i64) -> [bool; 6] {
    let world = World::new(cli);
    let initial = world.records();
    let state_before = world.store.load(NOW).unwrap().0;
    let mut expected = world.store.load_logins(NOW).unwrap().0;
    let roster_before = std::fs::read(&world.path).unwrap();
    let old_login = Path::new(expected.get("main").unwrap().dir.as_ref().unwrap())
        .join(cli.connector().stores[0]);
    std::fs::create_dir_all(old_login.parent().unwrap()).unwrap();
    std::fs::write(&old_login, b"existing private credential").unwrap();
    let host = observation_host(&world, operation, fault);
    let accepted = run_observation(&world, &host);
    let before = host.base.after.borrow().clone().unwrap_or(initial);
    expected = accounts::Logins::from_json(
        std::str::from_utf8(before[1].as_ref().unwrap()).unwrap(),
        NOW,
    )
    .unwrap();
    let expected_before = expected.clone();
    accounts::begin_workflow_projection_for_test(&world.raw, &mut expected);
    if operation != 4 {
        accounts::admit_workflow(&mut expected, "main").unwrap();
    }
    let kind = match operation {
        1 => {
            accounts::note_wrong_account(&mut expected, "main", NOW);
            accounts::Kind::WrongAccount
        }
        2 | 3 | 5 => {
            accounts::note_sign_in_task(
                &mut expected,
                "main",
                if operation == 2 && cli.renews_headlessly() {
                    accounts::SignInAction::Browser
                } else {
                    accounts::SignInAction::Login
                },
                NOW,
            );
            accounts::Kind::SignIn
        }
        4 => accounts::Kind::SignIn,
        _ => unreachable!(),
    };
    let mut expected_events = [accounts::Event::new(NOW, kind, "main")];
    accounts::finish_workflow_projection_for_test(
        &expected_before,
        &world.raw,
        &mut expected,
        &mut expected_events,
    )
    .unwrap();
    let pending = matches!(fault, 3 | 4);
    let readable = world.store.load(NOW).is_ok() && world.store.load_logins(NOW).is_ok();
    let mut guarded = readable != pending;
    if operation != 4 && matches!(fault, 2..=4) {
        assert!(
            host.base.staging.borrow().as_ref().unwrap().is_dir(),
            "an unacknowledged observation retains its staged login"
        );
    }
    host.repair();
    if fault == 2 {
        guarded &= world.records() == before;
        let retry = observation_host(&world, operation, 1);
        assert!(run_observation(&world, &retry));
    } else if pending {
        assert!(world.store.recover_sync_within(&[], &world.path).unwrap());
    }
    let logins = world.store.load_logins(NOW).unwrap().0;
    let state = world.store.load(NOW).unwrap().0;
    let coherent = logins == expected
        && state == state_before
        && world
            .store
            .tail(100)
            .unwrap()
            .iter()
            .filter(|event| event.kind == kind && event.t == NOW && event.account == "main")
            .count()
            == 1;
    let after = world.records();
    let receipt = std::fs::read(world.store.dir.join("sync-commit.json")).unwrap();
    let once = !world.store.recover_sync_within(&[], &world.path).unwrap()
        && world.records() == after
        && std::fs::read(world.store.dir.join("sync-commit.json")).unwrap() == receipt
        && host.base.called.get() == (operation != 4);
    let preserved = std::fs::read(&world.path).unwrap() == roster_before
        && std::fs::read(old_login).unwrap() == b"existing private credential"
        && logins.generation_clock == expected.generation_clock;
    let quiet = after[3] == before[3];
    [accepted, guarded, coherent, once, preserved, quiet]
}

#[cfg(unix)]
pub(crate) fn observation_commit_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherProvider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "AnotherOperation" => {
            next.insert("operation", state["operation"] + 1);
        }
        "AnotherFault" => {
            next.insert("fault", state["fault"] + 1);
        }
        "Observe" => {
            let observed = observation_case(
                model_provider(state["provider"]),
                state["operation"],
                state["fault"],
            );
            next.insert("phase", 1);
            for (key, value) in [
                "accepted",
                "guarded",
                "coherent",
                "once",
                "preserved",
                "quiet",
            ]
            .into_iter()
            .zip(observed)
            {
                next.insert(key, i64::from(value));
            }
        }
        _ => unreachable!(),
    }
    next
}

#[cfg(unix)]
#[test]
fn signin_observations_conform_to_durable_publication() {
    let model = aterm_models_harness::account_signin_observation_commit_model();
    assert_eq!(model.mutant_dials(), (1..=7).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                if action.name == "Observe"
                    && state["operation"] != 4
                    && !links_here(model_provider(state["provider"]))
                {
                    continue;
                }
                let next = observation_commit_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut admitted = model.init_state();
    admitted.insert("phase", 1);
    admitted.insert("fault", 3);
    admitted.insert("guarded", 0);
    assert!(!model.check_invariant("RefusalOrPendingFence", &admitted));
}

#[cfg(unix)]
#[test]
fn secondary_codex_identity_observations_recover_without_another_login() {
    for operation in [6, 7] {
        let world = World::new(Cli::Codex);
        let host = observation_host(&world, operation, 4);
        assert!(!run_observation(&world, &host));
        assert!(world.store.load_logins(NOW).is_err());
        assert!(host.base.staging.borrow().as_ref().unwrap().exists());
        host.repair();
        assert!(world.store.recover_sync_within(&[], &world.path).unwrap());
        let kind = if operation == 6 {
            accounts::Kind::WrongAccount
        } else {
            accounts::Kind::SignIn
        };
        assert_eq!(
            world
                .store
                .tail(100)
                .unwrap()
                .iter()
                .filter(|event| event.kind == kind && event.t == NOW)
                .count(),
            1
        );
        assert!(host.base.called.get());
        assert!(!world.store.recover_sync_within(&[], &world.path).unwrap());
        assert!(host.base.staging.borrow().as_ref().unwrap().is_dir());
    }
}

#[test]
fn signin_observation_policy_uses_the_admitted_roster_image() {
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        let world = World::new(cli);
        let row = world.raw.rows[0].clone();
        let original = std::fs::read(&world.path).unwrap();
        let before = world.records();
        let receipt_path = world.store.dir.join("sync-commit.json");
        let receipt_before = std::fs::read(&receipt_path).unwrap();
        world.change(2);
        let changed = std::fs::read(&world.path).unwrap();
        let record = |raw: &Roster| {
            signin_policy_matches(&row, raw, row.dir.as_deref())?;
            Ok(vec![accounts::Event::new(
                NOW,
                accounts::Kind::SignIn,
                "main",
            )])
        };
        // A second read would see the original policy, but this transaction read the edit.
        assert!(
            accounts_observe_signin(
                &world.store,
                &world.path,
                NOW,
                &mut Vec::new(),
                |raw, _, _| {
                    std::fs::write(&world.path, &original).unwrap();
                    record(raw)
                }
            )
            .is_err()
        );
        assert_eq!(world.records(), before);
        assert_eq!(std::fs::read(&receipt_path).unwrap(), receipt_before);
        // A conflicting edit after that read refuses new intent and is preserved.
        assert!(
            accounts_observe_signin(
                &world.store,
                &world.path,
                NOW,
                &mut Vec::new(),
                |raw, _, _| {
                    std::fs::write(&world.path, &changed).unwrap();
                    record(raw)
                }
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&world.path).unwrap(), changed);
        assert!(world.store.load(NOW).is_ok());
        assert_eq!(world.records(), before);
        assert_eq!(std::fs::read(&receipt_path).unwrap(), receipt_before);
        std::fs::write(&world.path, original).unwrap();
        assert!(!world.store.recover_sync_within(&[], &world.path).unwrap());
        assert!(
            world
                .store
                .tail(100)
                .unwrap()
                .iter()
                .all(|event| event.kind != accounts::Kind::SignIn)
        );
        accounts_observe_signin(
            &world.store,
            &world.path,
            NOW,
            &mut Vec::new(),
            |raw, _, _| record(raw),
        )
        .unwrap();
        assert_eq!(
            world
                .store
                .tail(100)
                .unwrap()
                .iter()
                .filter(|event| event.kind == accounts::Kind::SignIn)
                .count(),
            1
        );
    }
}
