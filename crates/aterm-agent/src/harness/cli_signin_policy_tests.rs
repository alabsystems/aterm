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
        if mode == 1 {
            run_accounts_login_for(
                self.world.cli,
                &self.world.env,
                self,
                "main",
                &mut out,
                &mut err,
            );
        } else {
            run_accounts_renew_for(
                self.world.cli,
                &self.world.env,
                self,
                (&["main".into()], false, None),
                &mut out,
                &mut err,
            );
        }
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
            assert_eq!(result, Renewal::Failed, "{cli:?}, case {case}");
            assert!(!host.called.get());
            assert_eq!(world.records(), before);
            assert!(!world.store.logins_dir().join("main+1").exists());
        }
    }
}
