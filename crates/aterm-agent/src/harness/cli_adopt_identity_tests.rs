// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::{Cell, RefCell};

#[test]
fn claude_adoption_accepts_an_explicit_canonical_identity() {
    assert!(
        parsed("accounts adopt-default local --email alias@example.test --identity-email owner@example.test").is_ok(),
        "Claude adoption should check an explicit canonical identity"
    );
}

struct AdoptHost<'a> {
    case: i64,
    calls: Cell<i64>,
    during_probe: Box<dyn Fn() + 'a>,
}

impl AccountsHost for AdoptHost<'_> {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, login: &str) -> Result<Captured, String> {
        assert_eq!(login, "", "adoption checks only the default login");
        assert!(
            !matches!(self.case, 11 | 12 | 16),
            "unneeded provider probe"
        );
        self.calls.set(self.calls.get() + 1);
        (self.during_probe)();
        if self.case == 10 {
            return Err("provider unavailable".into());
        }
        let email = match self.case {
            2 => " OWNER@EXAMPLE.TEST ",
            3 | 21 => "another@example.test",
            19 => "alias@example.test",
            _ => "owner@example.test",
        };
        let stdout = match self.case {
            4 => br#"{"loggedIn":true,"authMethod":"claude.ai"}"#.to_vec(),
            5 => b"not JSON".to_vec(),
            _ => format!(
                r#"{{"loggedIn":{},"authMethod":"{}","email":"{email}"}}"#,
                self.case != 6,
                if self.case == 7 {
                    "api_key"
                } else {
                    "claude.ai"
                },
            )
            .into_bytes(),
        };
        Ok(Captured {
            stdout,
            code: match self.case {
                8 => Some(1),
                9 => None,
                _ => Some(0),
            },
        })
    }
    fn unaskable(&self) -> Option<String> {
        assert_ne!(self.case, 16, "legacy adoption does not ask about a probe");
        (self.case == 11).then(|| "no user for a status check".into())
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("adoption does not sign in")
    }
    fn now(&self) -> i64 {
        1_790_000_000
    }
}

fn run_case(case: i64) -> (i64, i64, i64) {
    let tmp = tmp_dir("adopt-identity");
    let env = env_at(&tmp);
    let path = env.accounts_roster().unwrap();
    let store = env.accounts_store();
    // The wake marker a published adoption replaces (`wake_for_roster`) tells a running host of
    // the row and grants nothing, so it is no credential authority here.
    let files = [store.state_file(), store.logins_file(), store.ledger_path()];
    let before = files.each_ref().map(|path| std::fs::read(path).ok());
    let held = RefCell::new(None);
    let after_probe = RefCell::new(None);
    let host = AdoptHost {
        case,
        calls: Cell::new(0),
        during_probe: Box::new(|| {
            assert!(!store.dir.exists(), "probe holds no account writer lock");
            match case {
                13 => std::fs::write(
                    &path,
                    "[[account]]\nlabel='existing'\nclass='dedicated'\nemail='other@example.test'\n",
                )
                .unwrap(),
                14 => std::fs::write(&path, "[[broken").unwrap(),
                15 => *held.borrow_mut() = Some(store.acquire().unwrap()),
                17 | 18 => std::fs::write(
                    &path,
                    format!(
                        "[[account]]\nlabel='existing'\nclass='dedicated'\ndir='/unrelated'\nemail='{}'\n",
                        if case == 17 {
                            "other@example.test"
                        } else {
                            "owner@example.test"
                        },
                    ),
                )
                .unwrap(),
                _ => {}
            }
            *after_probe.borrow_mut() = Some(std::fs::read(&path).ok());
        }),
    };
    let identity = match case {
        12 => " --identity-email invalid",
        16 => "",
        _ => " --identity-email owner@example.test",
    };
    let email = if matches!(case, 20 | 21) {
        "owner@example.test"
    } else {
        "alias@example.test"
    };
    let line = format!("accounts adopt-default local --email {email}{identity}");
    let cmd = parsed(&line).expect("adoption command");
    let Cmd::Accounts(form) = cmd else {
        panic!("Claude command")
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_accounts(&form, &env, &host, &mut out, &mut err);
    drop(held.borrow_mut().take());
    let published = code == ExitCode::SUCCESS;
    if published {
        let roster = Roster::load(&path).unwrap();
        let row = roster.row("local").unwrap();
        assert_eq!(row.email.as_deref(), Some(email));
        assert_eq!(
            row.identity_email.as_deref(),
            (case != 16).then_some("owner@example.test")
        );
        assert!(row.dir.is_none());
        if case == 17 {
            assert!(roster.row("existing").is_some());
        }
    } else {
        assert_eq!(
            std::fs::read(&path).ok(),
            after_probe.borrow().clone().flatten(),
            "refusal changed roster: {}",
            String::from_utf8_lossy(&err)
        );
    }
    if (3..=12).contains(&case) || case == 19 || case == 21 {
        assert!(
            !store.dir.exists(),
            "failed probe initialized account storage"
        );
    }
    (
        i64::from(published),
        host.calls.get(),
        i64::from(files.each_ref().map(|path| std::fs::read(path).ok()) != before),
    )
}

pub(crate) fn adopt_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Adopt" => {
            let (published, probed, granted) = run_case(state["case"]);
            next.insert("phase", 1);
            next.insert("published", published);
            next.insert("probed", probed);
            next.insert("granted", granted);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn claude_adoption_conforms_to_identity_and_current_roster() {
    let model = aterm_models_harness::account_adopt_identity_model();
    assert_eq!(model.mutant_dials(), (1..=24).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut checked = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = adopt_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                checked += usize::from(action.name == "Adopt");
            }
        }
    }
    assert_eq!(checked, 21);
    let mut wrong = model.init_state();
    wrong.insert("case", 3);
    let mut wrong = adopt_transition(&wrong, "Adopt");
    wrong.insert("published", 1);
    assert!(!model.check_invariant("IdentityAndPolicyRequired", &wrong));
}
