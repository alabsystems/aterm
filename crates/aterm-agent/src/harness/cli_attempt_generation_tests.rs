// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::{Cell, RefCell};

const NOW: i64 = 1_790_000_000;
const OWNER: &str = "owner@example.test";

fn records(store: &accounts::Store) -> [Option<Vec<u8>>; 4] {
    [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ]
    .map(|path| std::fs::read(path).ok())
}

struct AttemptHost<'a> {
    cli: Cli,
    case: i64,
    store: &'a accounts::Store,
    raw: &'a Roster,
    old_dir: &'a str,
    changed: Cell<bool>,
    after: RefCell<Option<[Option<Vec<u8>>; 4]>>,
    staging: RefCell<Option<PathBuf>>,
}

impl AttemptHost<'_> {
    fn authenticate(&self, staging: &Path) {
        *self.staging.borrow_mut() = Some(staging.to_owned());
        let login = staging.join(self.cli.connector().stores[0]);
        std::fs::create_dir_all(login.parent().unwrap()).unwrap();
        std::fs::write(login, b"isolated new login").unwrap();
        assert!(!self.changed.replace(true));
        let before = self.store.load_logins(NOW).unwrap().0;
        let before_row = accounts::bound(self.raw, &before)
            .row("main")
            .unwrap()
            .clone();
        match self.case {
            2 | 3 | 6 | 7 => {
                self.store
                    .transact_logins(NOW, &mut None, |state, logins| {
                        let was = logins.get("main").unwrap().was.clone();
                        let events = accounts::note_renewed(
                            state,
                            logins,
                            "main",
                            None,
                            self.old_dir,
                            None,
                            if self.case == 3 { NOW - 20 } else { NOW - 10 },
                        );
                        logins.labels.get_mut("main").unwrap().was = was;
                        Ok(events)
                    })
                    .unwrap();
            }
            4 => {
                for (from, to) in [("main", "away"), ("away", "main")] {
                    self.store
                        .transact_logins(NOW, &mut None, |state, logins| {
                            accounts::move_account(state, logins, from, to)?;
                            Ok(Vec::new())
                        })
                        .unwrap();
                }
            }
            5 => {
                self.store
                    .transact_logins(NOW, &mut None, |state, logins| {
                        Ok(accounts::note_renewed(
                            state,
                            logins,
                            "other",
                            None,
                            self.store.login_dir("other").unwrap().to_str().unwrap(),
                            None,
                            NOW,
                        ))
                    })
                    .unwrap();
            }
            8 => {
                self.store
                    .transact_logins(NOW, &mut None, |_, logins| {
                        logins
                            .labels
                            .insert("main".into(), accounts::Binding::default());
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            9 => {
                self.store
                    .transact_logins(NOW, &mut None, |_, logins| {
                        accounts::note_served_login(logins, "main", NOW);
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            10 => {
                let witness = self.store.state_witness_file();
                std::fs::rename(&witness, witness.with_extension("retained")).unwrap();
                std::fs::create_dir(witness).unwrap();
            }
            _ => {}
        }
        assert_eq!(
            accounts::bound(self.raw, &self.store.load_logins(NOW).unwrap().0).row("main"),
            Some(&before_row),
            "the previous row-only check would pass"
        );
        *self.after.borrow_mut() = Some(records(self.store));
    }

    fn status(&self) -> Captured {
        Captured {
            stdout: format!(r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"{}","subscriptionType":"max"}}"#,
                if self.case == 6 { "another@example.test" } else { OWNER }).into_bytes(),
            code: Some(0),
        }
    }
}

#[test]
fn completed_cli_signins_recover_all_records_without_another_provider_login() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        let tmp = tmp_dir("signin-commit-recovery");
        let mut env = env_at(&tmp);
        env.now = NOW;
        let store = env.accounts_store_for(cli).unwrap();
        let old_dir = store.login_dir("main").unwrap();
        let old_dir = old_dir.to_str().unwrap();
        let raw = Roster {
            rows: vec![accounts::Row {
                label: "main".into(),
                dir: Some(old_dir.into()),
                email: Some(OWNER.into()),
                vendor: accounts::Vendor {
                    region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
                    ..Default::default()
                },
                ..Default::default()
            }],
        };
        let roster_path = env.accounts_roster_for(cli).unwrap();
        std::fs::write(
            &roster_path,
            accounts::append_rows_for(cli, "", &raw.rows).unwrap(),
        )
        .unwrap();
        store
            .transact_logins(NOW - 10, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "main",
                    None,
                    old_dir,
                    None,
                    NOW - 10,
                ))
            })
            .unwrap();
        let before = store.load_logins(NOW).unwrap().0;
        let host = AttemptHost {
            cli,
            case: 10,
            store: &store,
            raw: &raw,
            old_dir,
            changed: Cell::new(false),
            after: RefCell::new(None),
            staging: RefCell::new(None),
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _exit = if cli == Cli::Claude {
            run_accounts_renew(
                &env,
                &host,
                (&["main".into()], false, None),
                &mut out,
                &mut err,
            )
        } else {
            run_accounts_renew_for(
                cli,
                &env,
                &host,
                (&["main".into()], false, None),
                &mut out,
                &mut err,
            )
        };
        let staging = host.staging.borrow().clone().expect("provider flow ran");
        assert!(
            store.load(NOW).is_err(),
            "{cli:?}: {}",
            String::from_utf8_lossy(&err)
        );
        assert!(store.load_logins(NOW).is_err());
        let witness = store.state_witness_file();
        std::fs::rename(&witness, witness.with_extension("blocked")).unwrap();
        std::fs::rename(witness.with_extension("retained"), &witness).unwrap();
        assert_eq!(
            run_accounts_report((cli, Launch::default()), &env, true, &mut out, &mut err),
            ExitCode::SUCCESS
        );
        let after = store.load_logins(NOW).unwrap().0;
        assert_eq!(after.get("main").unwrap().dir.as_deref(), staging.to_str());
        assert_eq!(after.generation_clock, before.generation_clock + 1);
        assert!(staging.join(accounts::RENEWAL_CHECKPOINT_FILE).is_file());
        assert!(staging.join(cli.connector().stores[0]).is_file());
        assert_eq!(
            store
                .tail(100)
                .unwrap()
                .iter()
                .filter(|event| event.kind == accounts::Kind::Renewed)
                .count(),
            2
        );
    }
}

impl AccountsHost for AttemptHost<'_> {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn now(&self) -> i64 {
        NOW
    }
    fn login(&self, dir: Option<&str>) -> Result<i32, String> {
        self.authenticate(Path::new(dir.unwrap()));
        Ok(if self.case == 7 { 1 } else { 0 })
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        Ok(self.status())
    }
    fn renew_flow(&self, dir: &str, _: &str, _: &RenewBrowser) -> Result<i32, String> {
        self.login(Some(dir))
    }
    fn status_in_home(&self, _: &str) -> Result<Captured, String> {
        Ok(self.status())
    }
    fn vendor_login(&self, req: &crate::harness::login_drive::Request<'_>) -> Result<i32, String> {
        self.authenticate(req.staging);
        Ok(if self.case == 7 { 1 } else { 0 })
    }
    fn vendor_sign_in(
        &self,
        req: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        self.authenticate(req.staging);
        crate::harness::login_drive::Drove {
            outcome: if self.case == 7 {
                crate::harness::login_drive::Outcome::NeedsSignIn("fixture".into())
            } else {
                crate::harness::login_drive::Outcome::SignedIn {
                    email: Some(
                        if self.case == 6 {
                            "another@example.test"
                        } else {
                            OWNER
                        }
                        .into(),
                    ),
                    page: false,
                }
            },
            host: None,
        }
    }
    fn codex_account(&self, _: &Path) -> Option<String> {
        Some(
            if self.case == 6 {
                "another@example.test"
            } else {
                OWNER
            }
            .into(),
        )
    }
}

pub(crate) fn attempt_transition(
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
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Choose" => {
            next.insert("phase", 1);
        }
        "Commit" => {
            let cli = match state["provider"] {
                1 => Cli::Claude,
                2 => Cli::Codex,
                3 => Cli::Grok,
                4 => Cli::Kimi,
                _ => unreachable!(),
            };
            let tmp = tmp_dir("attempt-generation");
            let mut env = env_at(&tmp);
            env.now = NOW;
            let store = env.accounts_store_for(cli).unwrap();
            let old_dir = store.login_dir("main").unwrap();
            let old_dir = old_dir.to_str().unwrap();
            let raw = Roster {
                rows: vec![accounts::Row {
                    label: "main".into(),
                    dir: Some(old_dir.into()),
                    email: (!(cli == Cli::Claude && state["mode"] == 2)).then(|| OWNER.into()),
                    vendor: accounts::Vendor {
                        region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
                        ..Default::default()
                    },
                    ..Default::default()
                }],
            };
            let roster_path = env.accounts_roster_for(cli).unwrap();
            std::fs::write(
                &roster_path,
                accounts::append_rows_for(cli, "", &raw.rows).unwrap(),
            )
            .unwrap();
            if state["case"] != 8 {
                store
                    .transact_logins(NOW - 10, &mut None, |state, logins| {
                        Ok(accounts::note_renewed(
                            state,
                            logins,
                            "main",
                            None,
                            old_dir,
                            None,
                            NOW - 10,
                        ))
                    })
                    .unwrap();
            }
            let host = AttemptHost {
                cli,
                case: state["case"],
                store: &store,
                raw: &raw,
                old_dir,
                changed: Cell::new(false),
                after: RefCell::new(None),
                staging: RefCell::new(None),
            };
            let mut out = Vec::new();
            let mut err = Vec::new();
            let _exit = if state["mode"] == 2 {
                if cli == Cli::Claude {
                    run_accounts_login(&env, &host, "main", &mut out, &mut err)
                } else {
                    run_accounts_login_for(cli, &env, &host, "main", &mut out, &mut err)
                }
            } else if cli == Cli::Claude {
                run_accounts_renew(
                    &env,
                    &host,
                    (&["main".into()], false, None),
                    &mut out,
                    &mut err,
                )
            } else {
                run_accounts_renew_for(
                    cli,
                    &env,
                    &host,
                    (&["main".into()], false, None),
                    &mut out,
                    &mut err,
                )
            };
            let staging = host
                .staging
                .borrow()
                .clone()
                .unwrap_or_else(|| panic!("flow never ran: {}", String::from_utf8_lossy(&err)));
            let published = store
                .load_logins(NOW)
                .unwrap()
                .0
                .get("main")
                .and_then(|binding| binding.dir.as_deref())
                .is_some_and(|dir| Path::new(dir) == staging);
            let preserved = records(&store) == host.after.borrow().clone().unwrap();
            if !published && state["case"] != 7 {
                assert!(
                    staging.join(cli.connector().stores[0]).is_file(),
                    "completed login retained"
                );
                assert!(
                    !staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists(),
                    "no stale recovery proof"
                );
            }
            next.insert("phase", 2);
            next.insert("published", i64::from(published));
            next.insert("preserved", i64::from(preserved));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn queued_vendor_renewal_rechecks_its_original_parent_before_running_authentication() {
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        let tmp = tmp_dir("queued-parent");
        let mut env = env_at(&tmp);
        env.now = NOW;
        let store = env.accounts_store_for(cli).unwrap();
        let path = store.login_dir("main").unwrap();
        let path = path.to_str().unwrap();
        let raw = Roster {
            rows: vec![accounts::Row {
                label: "main".into(),
                dir: Some(path.into()),
                email: Some(OWNER.into()),
                vendor: accounts::Vendor {
                    region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
                    ..Default::default()
                },
                ..Default::default()
            }],
        };
        let roster_path = env.accounts_roster_for(cli).unwrap();
        std::fs::write(
            &roster_path,
            accounts::append_rows_for(cli, "", &raw.rows).unwrap(),
        )
        .unwrap();
        let renew = || {
            store
                .transact_logins(NOW, &mut None, |state, logins| {
                    Ok(accounts::note_renewed(
                        state,
                        logins,
                        "main",
                        Some(path),
                        path,
                        None,
                        NOW,
                    ))
                })
                .unwrap()
        };
        renew();
        let captured = store.load_logins(NOW).unwrap().0;
        let row = accounts::bound(&raw, &captured)
            .row("main")
            .unwrap()
            .clone();
        renew();
        let before = records(&store);
        let host = AttemptHost {
            cli,
            case: 1,
            store: &store,
            raw: &raw,
            old_dir: path,
            changed: Cell::new(false),
            after: RefCell::new(None),
            staging: RefCell::new(None),
        };
        assert_eq!(
            renew_one_for(
                cli,
                NOW,
                &host,
                &store,
                (&row, &roster_path, &captured),
                Some(path),
                &mut Vec::new(),
                &mut Vec::new()
            ),
            Renewal::Deferred
        );
        assert!(!host.changed.get(), "no provider authentication starts");
        assert_eq!(records(&store), before);
    }
}

#[test]
fn login_and_renewal_generation_admission_conform_for_every_provider() {
    let model = aterm_models_harness::account_attempt_generation_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = attempt_transition(&state, action.name);
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
    let mut forged = attempt_transition(&state, "Commit");
    forged.insert("published", 1);
    assert!(!model.check_invariant("StaleAttemptRefused", &forged));
}
