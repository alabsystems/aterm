// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn report_advertises(doc: &Value) -> bool {
    !doc["accounts"].as_array().unwrap().is_empty()
        || doc.get("next").is_some()
        || doc.get("active").is_some()
}

pub(crate) fn state_read_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let tmp = tmp_dir("state-read-conformance");
    let env = env_at(&tmp);
    three_accounts(&tmp, &env);
    let store = env.accounts_store();
    store.ensure().unwrap();
    let (scenario, bytes): (_, Option<&[u8]>) = match action {
        "Absent" => (1, None),
        "Valid" => (2, Some(br#"{"schema":1,"accounts":{}}"#)),
        "Torn" => (
            3,
            Some(br#"{"schema":1,"accounts":{"main":{"auth":"signed-out""#),
        ),
        "NonUtf8" => (4, Some(&[0xff, 0xfe])),
        "ForeignSchema" => (5, Some(br#"{"schema":2,"pinned":true}"#)),
        "Unreadable" => (6, None),
        _ => unreachable!(),
    };
    if action == "Unreadable" {
        std::fs::create_dir(store.state_file()).unwrap();
    } else if let Some(bytes) = bytes {
        std::fs::write(store.state_file(), bytes).unwrap();
    }
    let before = std::fs::read(store.state_file()).ok();
    let files = every_file(tmp.path());
    let accepted = store.load(env.now).is_ok();
    let mut advertised = false;
    for json in [false, true] {
        let (code, out, err) = acc_quiet(AccountsCmd::Report { json }, &env);
        assert_eq!(code, exit(if accepted { 0 } else { 1 }), "{err}");
        if json {
            let doc: Value = aterm_json::from_str(&out).unwrap();
            advertised = report_advertises(&doc);
            if !accepted {
                assert!(doc["refused"].as_str().is_some(), "{out}");
                assert!(doc["accounts"].as_array().unwrap().is_empty(), "{out}");
                assert!(doc.get("next").is_none(), "{out}");
                assert!(doc.get("active").is_none(), "{out}");
            }
        } else if !accepted {
            assert!(out.is_empty(), "{out}");
        }
        assert_eq!(every_file(tmp.path()), files, "reports are read-only");
    }
    let mut edited = false;
    let result = store.transact(env.now, &mut None, |state| {
        edited = true;
        state.pinned = !state.pinned;
        Ok(Vec::new())
    });
    assert_eq!(result.is_ok(), accepted);
    let rewritten = before != std::fs::read(store.state_file()).ok();
    if !accepted {
        assert!(!store.ledger_path().exists());
        assert!(!store.wake_path().exists());
        assert!(!store.machine_file().exists());
        assert_eq!(action == "Unreadable", store.state_file().is_dir());
    }
    let mut next = state.clone();
    for (key, value) in [
        ("scenario", scenario),
        ("accepted", i64::from(accepted)),
        ("edited", i64::from(edited)),
        ("rewritten", i64::from(rewritten)),
        ("advertised", i64::from(advertised)),
    ] {
        next.insert(key, value);
    }
    next
}

#[test]
fn account_state_reads_conform_and_false_availability_is_rejected() {
    let model = aterm_models_harness::account_state_read_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = state_read_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
            }
        }
    }
    let mut wrong = state_read_transition(&model.init_state(), "Torn");
    let leaked = aterm_json::json!({"refused": "unreadable", "accounts": [{"label": "main"}]});
    wrong.insert("advertised", i64::from(report_advertises(&leaked)));
    assert!(!model.check_invariant("NoUnknownAvailability", &wrong));
    wrong.insert("advertised", 0);
    wrong.insert("rewritten", 1);
    assert!(!model.check_invariant("NoRejectedRewrite", &wrong));
}

#[test]
fn unreadable_login_bindings_produce_a_structured_refusal_for_every_pool() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        let tmp = tmp_dir("logins-read-refusal");
        let env = env_at(&tmp);
        let path = env.accounts_roster_for(cli).unwrap();
        std::fs::write(
            &path,
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n",
        )
        .unwrap();
        let store = env.accounts_store_for(cli).unwrap();
        store.ensure().unwrap();
        let bytes = br#"{"schema":1,"labels":{"main":"#;
        std::fs::write(store.logins_file(), bytes).unwrap();
        let before = every_file(tmp.path());
        for json in [false, true] {
            let mut out = Vec::new();
            let mut err = Vec::new();
            assert_eq!(
                run_accounts_report(cli, &env, json, &mut out, &mut err),
                ExitCode::from(1)
            );
            assert!(
                String::from_utf8(err)
                    .unwrap()
                    .contains("identities are unknown")
            );
            if json {
                let doc: Value = aterm_json::from_slice(&out).unwrap();
                assert!(doc["refused"].as_str().is_some());
                assert!(doc["accounts"].as_array().unwrap().is_empty());
                assert!(doc.get("next").is_none());
            } else {
                assert!(out.is_empty());
            }
        }
        assert_eq!(every_file(tmp.path()), before);
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            run_accounts_use(cli, &env, Some("main"), &mut out, &mut err),
            ExitCode::from(1)
        );
        assert!(!store.state_file().exists(), "no selection was persisted");
        assert_eq!(std::fs::read(store.logins_file()).unwrap(), bytes);
    }
}

struct NoUsage;

struct FreshUsage;

impl AccountsHost for FreshUsage {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no auth probe")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login")
    }
    fn now(&self) -> i64 {
        0
    }
    fn usage_reader(&self) -> Option<crate::harness::accounts_pool::ReadUsage<'static>> {
        Some(&|_, _, now| {
            Some(crate::harness::codex_app_login::Usage {
                sample: accounts::Sample {
                    session_pct: Some(100),
                    at: now,
                    ..accounts::Sample::default()
                },
                wall: Some(("five_hour", Some(now + 3600))),
            })
        })
    }
}

#[test]
fn statistics_refuse_when_the_usage_snapshot_lock_is_held() {
    let tmp = tmp_dir("stats-write-refusal");
    let env = env_at(&tmp);
    std::fs::write(
        env.accounts_roster_for(Cli::Codex).unwrap(),
        "[[account]]\nlabel='main'\nclass='dedicated'\n",
    )
    .unwrap();
    let store = env.accounts_store_for(Cli::Codex).unwrap();
    store
        .transact(env.now, &mut None, |s| {
            s.active = Some("main".to_owned());
            Ok(Vec::new())
        })
        .unwrap();
    let held = store.acquire().unwrap();
    let before = every_file(tmp.path());
    for json in [false, true] {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            run_accounts_stats_of(Cli::Codex, &env, &FreshUsage, json, &mut out, &mut err),
            ExitCode::from(1)
        );
        assert!(!err.is_empty());
        if json {
            let doc: Value = aterm_json::from_slice(&out).unwrap();
            assert!(doc["rows"].as_array().unwrap().is_empty());
        } else {
            assert!(out.is_empty());
        }
    }
    assert_eq!(every_file(tmp.path()), before);
    drop(held);
}

impl AccountsHost for NoUsage {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no auth probe")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login")
    }
    fn now(&self) -> i64 {
        0
    }
    fn usage_reader(&self) -> Option<crate::harness::accounts_pool::ReadUsage<'static>> {
        Some(&|_, _, _| panic!("an unreadable pool must not start a usage probe"))
    }
}

#[test]
fn unreadable_pools_refuse_statistics_before_provider_io() {
    for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
        for damaged in ["state", "logins", "roster"] {
            let tmp = tmp_dir("stats-state-refusal");
            let env = env_at(&tmp);
            let path = env.accounts_roster_for(cli).unwrap();
            std::fs::write(
                &path,
                "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n",
            )
            .unwrap();
            let store = env.accounts_store_for(cli).unwrap();
            store.ensure().unwrap();
            let damaged = match damaged {
                "state" => store.state_file(),
                "logins" => store.logins_file(),
                _ => path,
            };
            std::fs::write(&damaged, "{").unwrap();
            let before = every_file(tmp.path());
            for json in [false, true] {
                let mut out = Vec::new();
                let mut err = Vec::new();
                assert_eq!(
                    run_accounts_stats_of(cli, &env, &NoUsage, json, &mut out, &mut err),
                    ExitCode::from(1)
                );
                assert!(!err.is_empty());
                if json {
                    let doc: Value = aterm_json::from_slice(&out).unwrap();
                    assert_eq!(doc["kind"].as_str(), Some("account-stats"));
                    assert!(doc["refused"].as_str().is_some());
                    assert!(doc["rows"].as_array().unwrap().is_empty());
                } else {
                    assert!(out.is_empty());
                }
            }
            assert_eq!(every_file(tmp.path()), before);
        }
    }
}
