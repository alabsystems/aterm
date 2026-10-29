// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ai::connector::{Flow, Region};
use crate::harness::login_drive::{Drove, Outcome, Request};
use std::cell::{Cell, RefCell};

const NOW: i64 = 1_790_000_000;
const OWNER: &str = "owner@example.test";

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
            7 => raw.rows[0].class = Class::Shared,
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

fn provider(index: i64) -> Cli {
    match index {
        1 => Cli::Codex,
        2 => Cli::Grok,
        3 => Cli::Kimi,
        _ => unreachable!(),
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
            let world = World::new(provider(state["provider"]));
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
                world.raw.rows[0].dir.as_deref(),
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
        world.raw.rows[0].dir.as_deref(),
        &mut Vec::new(),
        &mut Vec::new(),
    );
    matches!(result, Renewal::WrongAccount | Renewal::NeedsPerson)
}

#[test]
fn failed_login_observations_never_publish_without_their_history() {
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        for operation in 1..=3 {
            let world = World::new(cli);
            let before = world.records();
            let host = observation_host(&world, operation, 2);
            assert!(!run_observation(&world, &host));
            host.repair();
            assert_eq!(world.records(), before, "{cli:?}, operation {operation}");
        }
    }
}

fn observation_case(cli: Cli, operation: i64, fault: i64) -> [bool; 6] {
    let world = World::new(cli);
    let before = world.records();
    let state_before = world.store.load(NOW).unwrap().0;
    let mut expected = world.store.load_logins(NOW).unwrap().0;
    let roster_before = std::fs::read(&world.path).unwrap();
    let old_login = Path::new(expected.get("main").unwrap().dir.as_ref().unwrap())
        .join(cli.connector().stores[0]);
    std::fs::create_dir_all(old_login.parent().unwrap()).unwrap();
    std::fs::write(&old_login, b"existing private credential").unwrap();
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
    let host = observation_host(&world, operation, fault);
    let accepted = run_observation(&world, &host);
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
                provider(state["provider"]),
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

#[test]
fn signin_observations_conform_to_durable_publication() {
    let model = aterm_models_harness::account_signin_observation_commit_model();
    assert_eq!(model.mutant_dials(), (1..=7).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
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
        // A conflicting edit after that read fences the admitted record and is preserved.
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
        assert!(world.store.load(NOW).is_err());
        std::fs::write(&world.path, original).unwrap();
        assert!(world.store.recover_sync_within(&[], &world.path).unwrap());
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
