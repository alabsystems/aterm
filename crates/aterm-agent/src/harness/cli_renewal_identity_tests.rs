// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::{Cell, RefCell};

const MAIL: &str = "owner@example.test";

struct Approval;

impl shared_renewal::Authority for Approval {
    fn prepare(&self, _: &accounts::Row) -> Result<shared_renewal::Grant, String> {
        Ok(shared_renewal::Grant {
            source: source(),
            decision: [3; 32],
            generation: 1,
            configuration: [4; 32],
            identity: MAIL.into(),
            expected_principal: profile::principal_id("provider-uuid"),
            required_browser_profile: None,
        })
    }

    fn current(&self, _: &shared_renewal::Grant, _: &accounts::Row) -> Result<(), String> {
        Ok(())
    }
}

fn source() -> accounts::shared::Source {
    accounts::shared::Source {
        network: [1; 32],
        account: [2; 32],
    }
}

struct Host {
    store: accounts::Store,
    now: i64,
    replacement: Vec<u8>,
    legacy: bool,
    homes: RefCell<Vec<PathBuf>>,
    before: RefCell<Option<Option<accounts::Binding>>>,
    before_state: RefCell<Option<accounts::State>>,
    checks: Cell<usize>,
    observations: Cell<usize>,
}

impl AccountsHost for Host {
    fn stdin_is_terminal(&self) -> bool {
        true
    }

    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("renewal checks only its staging home")
    }

    fn unaskable(&self) -> Option<String> {
        None
    }

    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no plain login")
    }

    fn now(&self) -> i64 {
        self.now
    }

    fn renew_flow(&self, home: &str, _: &str, _: &RenewBrowser) -> Result<i32, String> {
        let home = PathBuf::from(home);
        std::fs::write(home.join(".claude.json"), profile_bytes(1)).unwrap();
        self.homes.borrow_mut().push(home);
        Ok(0)
    }

    fn status_in_home(&self, home: &str) -> Result<Captured, String> {
        self.checks.set(self.checks.get() + 1);
        *self.before.borrow_mut() = Some(
            self.store
                .load_logins(self.now)
                .unwrap()
                .0
                .get("account")
                .cloned(),
        );
        *self.before_state.borrow_mut() = Some(self.store.load(self.now).unwrap().0);
        let status = Captured {
            code: Some(0),
            stdout: format!(
                r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"{MAIL}","subscriptionType":"max"}}"#
            ).into_bytes(),
        };
        // The provider status is already determined when another local writer
        // replaces the effective profile. Completion must inspect that profile.
        if self.legacy {
            std::fs::write(Path::new(home).join(".claude.json"), profile_bytes(2)).unwrap();
        }
        let path = Path::new(home).join(if self.legacy {
            ".config.json"
        } else {
            ".claude.json"
        });
        std::fs::write(path, &self.replacement).unwrap();
        Ok(status)
    }

    fn claude_identity_in_home(&self, home: &str, email: &str) -> profile::IdentityObservation {
        assert_eq!(
            self.store
                .transact_within(&[], self.now, &mut None, |_| {
                    Err("identity was observed outside the writer".into())
                })
                .unwrap_err(),
            accounts::LOCK_HELD,
            "the identity observation must run under the account writer"
        );
        self.observations.set(self.observations.get() + 1);
        profile::identity_in_home(Path::new(home), email)
    }
}

fn profile_bytes(kind: i64) -> Vec<u8> {
    match kind {
        0 => b"opaque fixture".to_vec(),
        1 => format!(r#"{{"oauthAccount":{{"emailAddress":"{MAIL}","accountUuid":"provider-uuid"}}}}"#).into_bytes(),
        2 => br#"{"oauthAccount":{"emailAddress":"different@example.test","accountUuid":"provider-uuid"}}"#.to_vec(),
        3 => br#"{"oauthAccount":{"accountUuid":"provider-uuid"}}"#.to_vec(),
        4 => format!(r#"{{"oauthAccount":{{"emailAddress":"{MAIL}"}}}}"#).into_bytes(),
        5 => format!(r#"{{"oauthAccount":{{"emailAddress":"{MAIL}","accountUuid":"reserved-uuid"}}}}"#).into_bytes(),
        _ => unreachable!(),
    }
}

fn renewal_identity_case(
    mode: i64,
    kind: i64,
    prior: bool,
    variant: u8,
    replay: bool,
) -> [(&'static str, i64); 8] {
    let tmp = tmp_dir("renewal-identity");
    let env = env_at(&tmp);
    let store = env.accounts_store();
    let row = accounts::Row {
        label: "account".into(),
        email: Some(
            if variant & 1 != 0 {
                "delivery-alias@example.test"
            } else {
                MAIL
            }
            .into(),
        ),
        identity_email: (variant & 1 != 0).then(|| MAIL.into()),
        ..accounts::Row::default()
    };
    std::fs::write(
        env.accounts_roster().unwrap(),
        accounts::append_rows_for(Cli::Claude, "", &[row]).unwrap(),
    )
    .unwrap();
    if prior {
        let previous = tmp.path().join("previous");
        std::fs::create_dir_all(&previous).unwrap();
        std::fs::write(previous.join(".claude.json"), profile_bytes(1)).unwrap();
        store
            .transact_logins(env.now - 1, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "account",
                    None,
                    previous.to_str().unwrap(),
                    None,
                    env.now - 1,
                ))
            })
            .unwrap();
    }
    if mode == 1 {
        let reserved = store.claim_staging_dir("reserved").unwrap();
        store
            .transact_logins(env.now, &mut None, |_, logins| {
                logins.shared.begin(
                    accounts::shared::Intent {
                        source: source(),
                        decision: [3; 32],
                        generation: 1,
                        configuration: [4; 32],
                        request: [5; 16],
                        label: "reserved".into(),
                        identity: "reserved@example.test".into(),
                        staging: reserved.to_str().unwrap().into(),
                        before_generation: None,
                        begun_at: env.now,
                    },
                    profile::principal_id("reserved-uuid"),
                )?;
                Ok(Vec::new())
            })
            .unwrap();
    }
    let host = Host {
        store: store.clone(),
        now: env.now + 1,
        replacement: profile_bytes(kind),
        legacy: variant & 2 != 0,
        homes: RefCell::new(Vec::new()),
        before: RefCell::new(None),
        before_state: RefCell::new(None),
        checks: Cell::new(0),
        observations: Cell::new(0),
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = if mode == 2 {
        shared_renewal::run(
            &env, &host, "account", [1; 16], &Approval, &mut out, &mut err,
        )
    } else {
        let Cmd::Accounts(command) = parsed("accounts renew account").unwrap() else {
            unreachable!()
        };
        run_accounts(&command, &env, &host, &mut out, &mut err)
    };
    if replay {
        assert_eq!(mode, 2);
        let retained = store.load_logins(host.now).unwrap().0;
        for request in [[1; 16], [2; 16]] {
            assert_ne!(
                shared_renewal::run(
                    &env, &host, "account", request, &Approval, &mut out, &mut err,
                ),
                ExitCode::SUCCESS,
                "a consumed grant cannot authorize another provider attempt"
            );
            assert_eq!(store.load_logins(host.now).unwrap().0, retained);
        }
    }
    assert_eq!(host.checks.get(), 1, "{}", String::from_utf8_lossy(&err));
    assert_eq!(
        host.observations.get(),
        1,
        "one current profile observation"
    );
    let after = store.load_logins(host.now).unwrap().0;
    if code != ExitCode::SUCCESS {
        assert_eq!(
            store.load(host.now).unwrap().0.accounts,
            host.before_state.borrow().as_ref().unwrap().accounts,
            "a refused completion leaves account observations intact"
        );
    }
    if prior {
        assert_eq!(
            std::fs::read(tmp.path().join("previous/.claude.json")).unwrap(),
            profile_bytes(1)
        );
    }
    let home = host.homes.borrow()[0].clone();
    let profile = home.join(if host.legacy {
        ".config.json"
    } else {
        ".claude.json"
    });
    let attempt = after.shared.latest(source());
    [
        ("published", i64::from(code == ExitCode::SUCCESS)),
        (
            "bound",
            i64::from(
                after
                    .get("account")
                    .is_some_and(|binding| binding.dir.as_deref() == home.to_str()),
            ),
        ),
        (
            "unchanged",
            i64::from(after.get("account").cloned() == host.before.borrow().clone().unwrap()),
        ),
        (
            "preserved",
            i64::from(std::fs::read(&profile).ok().as_ref() == Some(&host.replacement)),
        ),
        (
            "checkpoint",
            i64::from(home.join(accounts::RENEWAL_CHECKPOINT_FILE).exists()),
        ),
        ("spent", i64::from(mode == 2 && attempt.is_some())),
        (
            "unknown",
            i64::from(
                mode == 2
                    && attempt.is_some_and(|a| a.outcome == accounts::shared::Outcome::Unknown),
            ),
        ),
        ("calls", host.homes.borrow().len() as i64),
    ]
}

pub(crate) fn renewal_identity_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Mode" => {
            next.insert("mode", state["mode"] + 1);
        }
        "Credential" => {
            next.insert("credential", state["credential"] + 1);
        }
        "Prior" => {
            next.insert("prior", 1);
        }
        "Complete" | "Replay" => {
            let observed = renewal_identity_case(
                state["mode"],
                state["credential"],
                state["prior"] == 1,
                0,
                action == "Replay",
            );
            for variant in 1..4 {
                assert_eq!(
                    renewal_identity_case(
                        state["mode"],
                        state["credential"],
                        state["prior"] == 1,
                        variant,
                        action == "Replay",
                    ),
                    observed,
                    "{state:?}, variant={variant}, action={action}"
                );
            }
            next.insert("phase", if action == "Replay" { 2 } else { 1 });
            for (name, value) in observed {
                next.insert(name, value);
            }
        }
        _ => panic!("unknown renewal identity action"),
    }
    next
}

#[test]
fn explicit_renewal_identity_conforms_and_preserves_consumed_grants() {
    let model = aterm_models_harness::renewal_identity_model();
    assert_eq!(model.mutant_dials(), (1..=12).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 84);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = renewal_identity_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name,
                );
            }
        }
    }
    let mut before = model.init_state();
    before.insert("mode", 2);
    before.insert("credential", 2);
    let refused = model.successors("Complete", &before).pop().unwrap();
    for (field, value, law) in [
        ("published", 1, "Publication"),
        ("bound", 1, "Binding"),
        ("unchanged", 0, "Refusal"),
        ("preserved", 0, "Evidence"),
        ("checkpoint", 1, "Checkpoint"),
        ("spent", 0, "Consumption"),
        ("unknown", 0, "FailedOutcome"),
        ("calls", 2, "ProviderEntry"),
    ] {
        let mut bad = refused.clone();
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad), "{law}");
    }
}
