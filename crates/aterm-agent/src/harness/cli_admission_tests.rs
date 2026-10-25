// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::login_drive;

fn seed(store: &accounts::Store, fresh: bool, state_bad: i64, bindings_bad: bool) {
    if fresh {
        return;
    }
    store.ensure().unwrap();
    match state_bad {
        1 => std::fs::write(store.state_file(), "{").unwrap(),
        2 => std::fs::write(store.state_witness_file(), "").unwrap(),
        _ => std::fs::write(store.state_file(), accounts::State::default().to_json()).unwrap(),
    }
    std::fs::write(
        store.logins_file(),
        if bindings_bad {
            "{".to_owned()
        } else {
            accounts::Logins::default().to_json()
        },
    )
    .unwrap();
}

pub(crate) fn admission_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "Read" {
        let tmp = tmp_dir("signin-admission");
        let env = env_at(&tmp);
        let store = env.accounts_store();
        seed(
            &store,
            state["fresh"] == 1,
            state["state_bad"],
            state["bindings_bad"] == 1,
        );
        let before = every_file(tmp.path());
        let had_store = store.dir.exists();
        let accepted = store.check_signin_state(env.now).is_ok();
        assert_eq!(every_file(tmp.path()), before, "admission only reads");
        assert_eq!(
            store.dir.exists(),
            had_store,
            "fresh admission creates no store"
        );
        next.insert("phase", 2);
        next.insert("accepted", i64::from(accepted));
        return next;
    }
    next.insert("phase", 1);
    match action {
        "Fresh" => {
            next.insert("fresh", 1);
        }
        "Known" => {}
        "StateUnread" => {
            next.insert("state_bad", 1);
        }
        "BindingsUnread" => {
            next.insert("bindings_bad", 1);
        }
        "StateLost" => {
            next.insert("state_bad", 2);
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn signin_admission_conforms_and_unknown_records_cannot_be_admitted() {
    let model = aterm_models_harness::account_signin_admission_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = admission_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
            }
        }
    }
    for (action, law) in [
        ("StateUnread", "StateIsReadable"),
        ("StateLost", "StateIsReadable"),
        ("BindingsUnread", "BindingsAreReadable"),
    ] {
        let seeded = admission_transition(&model.init_state(), action);
        let mut bad = admission_transition(&seeded, "Read");
        bad.insert("accepted", 1);
        assert!(!model.check_invariant(law, &bad));
    }
}

struct NoSignIn;

impl AccountsHost for NoSignIn {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn now(&self) -> i64 {
        1_792_660_000
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no auth probe")
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login")
    }
    fn renew_flow(&self, _: &str, _: &str, _: &RenewBrowser) -> Result<i32, String> {
        panic!("no renewal")
    }
    fn vendor_login(&self, _: &login_drive::Request<'_>) -> Result<i32, String> {
        panic!("no vendor login")
    }
    fn vendor_sign_in(&self, _: &login_drive::Request<'_>) -> login_drive::Drove {
        panic!("no vendor renewal")
    }
    fn window_browser(&self) -> Option<PathBuf> {
        panic!("no browser discovery")
    }
}

#[test]
fn explicit_signins_and_windows_refuse_unknown_records_before_side_effects() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        for (state_bad, bindings_bad) in [(1, false), (0, true), (2, false)] {
            let tmp = tmp_dir("explicit-signin-refusal");
            let env = env_at(&tmp);
            let path = env.accounts_roster_for(cli).unwrap();
            let region = if cli == Cli::Kimi {
                "region='global'\n"
            } else {
                ""
            };
            std::fs::write(&path, format!("[[account]]\nlabel='main'\nclass='dedicated'\nemail='owner@example.test'\n{region}")).unwrap();
            let store = env.accounts_store_for(cli).unwrap();
            seed(&store, false, state_bad, bindings_bad);
            let before = every_file(tmp.path());
            let mut out = Vec::new();
            let mut err = Vec::new();
            let labels = ["main".to_owned()];
            let status = if cli == Cli::Claude {
                run_accounts_renew(&env, &NoSignIn, (&labels, false, None), &mut out, &mut err)
            } else {
                run_accounts_renew_for(
                    cli,
                    &env,
                    &NoSignIn,
                    (&labels, false, None),
                    &mut out,
                    &mut err,
                )
            };
            assert_eq!(status, ExitCode::from(1));
            let diagnostic = String::from_utf8(err.clone()).unwrap();
            assert!(
                diagnostic.contains("account records cannot be read"),
                "{cli:?}/{state_bad}/{bindings_bad}: {diagnostic}"
            );
            let refused_path = if bindings_bad {
                store.logins_file()
            } else {
                store.state_file()
            };
            assert!(diagnostic.contains(&refused_path.display().to_string()));
            assert_eq!(every_file(tmp.path()), before);
            assert_eq!(
                run_accounts_browser(cli, &env, &NoSignIn, "main", &mut out, &mut err),
                ExitCode::from(1)
            );
            assert_eq!(every_file(tmp.path()), before);
            if cli == Cli::Claude {
                std::fs::write(&path, "[[account]]\nlabel='main'\nclass='dedicated'\n").unwrap();
                let before = every_file(tmp.path());
                assert_eq!(
                    run_accounts_login(&env, &NoSignIn, "main", &mut out, &mut err),
                    ExitCode::from(1)
                );
                assert_eq!(every_file(tmp.path()), before);
            } else {
                assert_eq!(
                    run_accounts_login_for(cli, &env, &NoSignIn, "main", &mut out, &mut err),
                    ExitCode::from(1)
                );
                assert_eq!(every_file(tmp.path()), before);
            }
        }
    }
}
