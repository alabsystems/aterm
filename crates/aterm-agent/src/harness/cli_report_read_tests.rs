// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct NoProvider;

impl AccountsHost for NoProvider {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no provider IO")
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
}

struct Fixture {
    _root: Tmp,
    env: Env,
    cli: Cli,
    store: accounts::Store,
    roster: PathBuf,
    text: String,
    old: String,
    new: String,
}

impl Fixture {
    fn new(cli: Cli, present: bool) -> Self {
        let root = tmp_dir("report-consumer");
        let env = env_at(&root);
        let store = env.accounts_store_for(cli).unwrap();
        let roster = env.accounts_roster_for(cli).unwrap();
        let text = accounts::append_rows_for(
            cli,
            "",
            &[accounts::Row {
                label: "main".into(),
                ..accounts::Row::default()
            }],
        )
        .unwrap();
        if present {
            std::fs::write(&roster, &text).unwrap();
        }
        let old = root.path().join("old").to_str().unwrap().to_owned();
        let new = root.path().join("new").to_str().unwrap().to_owned();
        for dir in [&old, &new] {
            let credential = PathBuf::from(dir).join(cli.connector().stores[0]);
            std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
            std::fs::write(credential, "opaque credential").unwrap();
        }
        store
            .transact_logins(env.now, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state, logins, "main", None, &old, None, env.now,
                ))
            })
            .unwrap();
        Self {
            _root: root,
            env,
            cli,
            store,
            roster,
            text,
            old,
            new,
        }
    }

    fn pause(&self, publish: impl FnOnce() -> Result<Vec<accounts::Event>, String>) {
        let witness = self.store.state_witness_file();
        let retained = witness.with_extension("retained");
        std::fs::rename(&witness, &retained).unwrap();
        std::fs::create_dir(&witness).unwrap();
        assert!(publish().is_err());
        assert!(
            self.store.load(self.env.now).is_err(),
            "pending receipt was admitted"
        );
        std::fs::rename(&witness, witness.with_extension("obstruction")).unwrap();
        std::fs::rename(&retained, &witness).unwrap();
    }

    fn pending_login(&self) {
        self.pause(|| {
            self.store
                .transact_logins(self.env.now, &mut None, |state, logins| {
                    Ok(accounts::note_renewed(
                        state,
                        logins,
                        "main",
                        None,
                        &self.new,
                        None,
                        self.env.now,
                    ))
                })
        });
    }

    fn pending_initial_roster(&self) {
        assert!(!self.roster.exists());
        self.pause(|| {
            let held = self.store.acquire().unwrap();
            self.store.transact_sync_held(
                &held,
                self.env.now,
                &mut None,
                accounts::SyncTarget {
                    path: &self.roster,
                    before: None,
                    after: Some(&self.text),
                    mode: None,
                },
                |state, logins| {
                    Ok(accounts::note_login_end(
                        state,
                        logins,
                        "main",
                        self.env.now,
                    ))
                },
                || Ok(()),
            )
        });
    }

    fn credentials_unchanged(&self) {
        for dir in [&self.old, &self.new] {
            assert_eq!(
                std::fs::read_to_string(PathBuf::from(dir).join(self.cli.connector().stores[0]))
                    .unwrap(),
                "opaque credential"
            );
        }
    }
}

#[test]
fn other_provider_stats_recover_before_reading_account_records() {
    for cli in [Cli::Codex, Cli::Kimi, Cli::Grok] {
        let f = Fixture::new(cli, true);
        f.pending_login();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            run_accounts_stats_of((cli, Launch::default()), &f.env, true, &mut out, &mut err),
            ExitCode::SUCCESS,
            "{cli:?}: {}",
            String::from_utf8_lossy(&err)
        );
        let doc: Value = aterm_json::from_slice(&out).unwrap();
        assert_eq!(doc["rows"][0]["label"], Value::from("main"));
        assert!(doc.get("refused").is_none(), "{doc}");
        assert_eq!(
            f.store
                .load_logins(f.env.now)
                .unwrap()
                .0
                .get("main")
                .unwrap()
                .dir
                .as_ref(),
            Some(&f.new)
        );
        f.credentials_unchanged();
    }
}

#[test]
fn all_provider_inventory_recovers_an_initially_absent_roster() {
    let f = Fixture::new(Cli::Codex, false);
    f.pending_initial_roster();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        run_accounts_all(true, &f.env, &NoProvider, &mut out, &mut err),
        ExitCode::SUCCESS,
        "{}",
        String::from_utf8_lossy(&err)
    );
    let doc: Value = aterm_json::from_slice(&out).unwrap();
    assert_eq!(doc["pools"][0]["cli"], Value::from("codex"), "{doc}");
    assert_eq!(
        doc["pools"][0]["report"]["accounts"][0]["label"],
        Value::from("main")
    );
    assert!(f.roster.is_file());
    f.credentials_unchanged();
}

#[test]
fn limits_hint_recovers_pending_account_records() {
    let f = Fixture::new(Cli::Claude, true);
    f.pending_login();
    assert!(limits_accounts_line(&f.env, "@s-test").is_some());
    assert_eq!(
        f.store
            .load_logins(f.env.now)
            .unwrap()
            .0
            .get("main")
            .unwrap()
            .dir
            .as_ref(),
        Some(&f.new)
    );
    f.credentials_unchanged();
}

pub(crate) fn attribution_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    let field = match action {
        "SnapshotMany" => Some("snapshot_many"),
        "DiskMany" => Some("disk_many"),
        "OtherProfile" => Some("mismatch"),
        "Read" => None,
        _ => unreachable!(),
    };
    if let Some(field) = field {
        next.insert(field, 1);
        return next;
    }
    let root = tmp_dir("report-attribution");
    let env = env_at(&root);
    let roster = |many: bool| {
        let one = accounts::append_row("", "main", Class::Preferred, None).unwrap();
        Roster::parse(&if many {
            accounts::append_row(
                &one,
                "second",
                Class::Preferred,
                Some(&crate::harness::host_path::abs("/second")),
            )
            .unwrap()
        } else {
            one
        })
        .unwrap()
    };
    let disk = roster(state["disk_many"] == 1);
    let disk_text = accounts::append_rows_for(Cli::Claude, "", &disk.rows).unwrap();
    std::fs::write(env.accounts_roster().unwrap(), disk_text).unwrap();
    let profile = stats_profile().replace(
        STATS_UUID,
        if state["mismatch"] == 1 {
            STATS_OTHER
        } else {
            STATS_UUID
        },
    );
    std::fs::create_dir_all(env.home.as_ref().unwrap()).unwrap();
    std::fs::write(env.home.as_ref().unwrap().join(".claude.json"), profile).unwrap();
    let mut recorded = accounts::State::default();
    recorded.accounts.insert(
        "main".into(),
        accounts::Account {
            key: profile::AccountKey::claude(STATS_UUID),
            ..accounts::Account::default()
        },
    );
    let captured = roster(state["snapshot_many"] == 1);
    let admitted = HeldBy::for_roster(Cli::Claude, &env, &captured).admit(recorded);
    next.insert("kept", i64::from(admitted.accounts.contains_key("main")));
    next.insert("phase", 1);
    next
}

#[test]
fn report_attribution_uses_captured_policy_and_filters_other_identities() {
    let model = aterm_models_harness::account_report_attribution_model();
    assert_eq!(model.mutant_dials(), vec![1, 2, 3]);
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = attribution_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} => {next:?}"
                );
            }
        }
    }
    let mut bad = model.init_state();
    bad.insert("phase", 1);
    bad.insert("kept", 0);
    assert!(!model.check_invariant("CapturedPoolOrSameIdentityKeeps", &bad));
    bad.insert("mismatch", 1);
    bad.insert("kept", 1);
    assert!(!model.check_invariant("SinglePoolOtherIdentityDrops", &bad));
}

#[test]
fn report_preserves_nonfatal_machine_identity_diagnostics() {
    let f = Fixture::new(Cli::Claude, true);
    std::fs::write(f.store.machine_file(), "invalid").unwrap();
    let (_, out, err) = acc_quiet(AccountsCmd::Report { json: true }, &f.env);
    assert!(err.contains("no machine id could be read"), "{err}");
    let doc: Value = aterm_json::from_str(&out).unwrap();
    assert!(doc["refused"].is_null());
    assert_eq!(
        std::fs::read_to_string(f.store.machine_file()).unwrap(),
        "invalid"
    );
}

/// Combined reports age a held journal at the command's clock, leaving its sample time and
/// readings intact. Reusing `seen.at` keeps the passed wall and quota in both report forms.
#[test]
fn combined_reports_age_cached_journal_quota_at_the_command_clock() {
    use crate::harness::alab_seen::{self, AccountSeen, Figure, Seen, WindowSeen};
    const KEY: &str = "ac-0123456789abcdef";
    let f = Fixture::new(Cli::Claude, true);
    let roster = accounts::append_rows_for(
        Cli::Claude,
        "",
        &[accounts::Row {
            label: "main".into(),
            dir: Some(f.old.clone()),
            ..Default::default()
        }],
    )
    .unwrap();
    std::fs::write(&f.roster, roster).unwrap();
    f.store
        .transact(f.env.now, &mut None, |state| {
            state.accounts.entry("main".into()).or_default().pk = Some(KEY.into());
            Ok(Vec::new())
        })
        .unwrap();
    let end = f.env.now + 100;
    let window = |name: &str, reset, wall_until| WindowSeen {
        window: name.into(),
        sub: None,
        figure: Some(Figure::Used {
            pct_x10: 1000,
            reset,
        }),
        read: Some((f.env.now, "m17".into(), "statusline".into())),
        wall_until,
    };
    let seen = Seen {
        at: f.env.now,
        accounts: [(
            KEY.into(),
            AccountSeen {
                windows: vec![
                    window("5h", Some(end), Some(Some(end))),
                    window("7d", Some(end + 1_000), Some(Some(end + 1_000))),
                    window("credits", None, Some(None)),
                ],
                ..Default::default()
            },
        )]
        .into(),
    };
    let original = seen.clone();
    alab_seen::with_seen(seen, || {
        for now in [end - 1, end, end + 1] {
            let mut env = f.env.clone();
            env.now = now;
            for json in [false, true] {
                let (mut out, mut err) = (Vec::new(), Vec::new());
                let report = write_accounts_report(
                    (Cli::Claude, Launch::default()),
                    &env,
                    json,
                    &mut out,
                    &mut err,
                );
                assert!(report.local.is_some(), "{}", String::from_utf8_lossy(&err));
                let lines = if json {
                    let doc: Value = aterm_json::from_slice(&out).unwrap();
                    let windows = &doc["journal"]["main"]["windows"];
                    let five = &windows[0];
                    assert_eq!(five["read"]["t"].as_i64(), Some(original.at));
                    assert_eq!(five["read"]["age_s"].as_i64(), Some(now - original.at));
                    if now < end {
                        assert_eq!(five["state"].as_str(), Some("current"));
                        assert_eq!(five["used_pct_x10"].as_u64(), Some(1000));
                        assert_eq!(five["wall_until"].as_i64(), Some(end));
                    } else {
                        assert_eq!(five["state"].as_str(), Some("reset-unread"));
                        assert!(five["used_pct_x10"].is_null());
                        assert!(five["wall_until"].is_null());
                        assert_eq!(five["reset"].as_i64(), Some(end));
                    }
                    assert_eq!(windows[1]["state"].as_str(), Some("current"));
                    assert_eq!(windows[2]["state"].as_str(), Some("reset-unknown"));
                    windows
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|w| w["line"].as_str().unwrap().to_owned())
                        .collect::<Vec<_>>()
                } else {
                    let text = String::from_utf8(out).unwrap();
                    let journal = text.split_once("\nlab journal").unwrap().1;
                    ["main: 5h", "main: 7d", "main: credits"]
                        .into_iter()
                        .map(|name| {
                            journal
                                .lines()
                                .find(|line| line.contains(name))
                                .unwrap()
                                .to_owned()
                        })
                        .collect()
                };
                if now < end {
                    assert!(lines[0].contains("100% used") && lines[0].contains("at its limit"));
                } else {
                    assert!(lines[0].contains("not read since"));
                    assert!(!lines[0].contains('%') && !lines[0].contains("at its limit"));
                }
                assert!(lines[1].contains("100% used") && lines[1].contains("at its limit until"));
                assert!(lines[2].contains("reset unknown") && lines[2].contains("at its limit"));
            }
        }
        assert_eq!(alab_seen::current(false).unwrap().as_ref(), &original);
    });
    f.credentials_unchanged();
}

#[test]
fn a_captured_report_does_not_reread_changed_policy_or_discard_its_refusal() {
    let f = Fixture::new(Cli::Codex, true);
    let records = f
        .store
        .read_account_records(Some(&f.roster), f.env.now)
        .unwrap();
    std::fs::write(&f.roster, "[[account]").unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        run_accounts_report_snapshot(
            (Cli::Codex, Launch::default()),
            &f.env,
            true,
            &f.roster,
            Ok(records),
            &mut out,
            &mut err
        ),
        ExitCode::SUCCESS
    );
    let doc: Value = aterm_json::from_slice(&out).unwrap();
    assert_eq!(doc["accounts"][0]["label"], Value::from("main"));
    let refusal = f.store.read_account_records(Some(&f.roster), f.env.now);
    assert!(refusal.is_err());
    std::fs::write(&f.roster, &f.text).unwrap();
    out.clear();
    err.clear();
    assert_eq!(
        run_accounts_report_snapshot(
            (Cli::Codex, Launch::default()),
            &f.env,
            true,
            &f.roster,
            refusal,
            &mut out,
            &mut err
        ),
        ExitCode::from(1)
    );
    let doc: Value = aterm_json::from_slice(&out).unwrap();
    assert!(doc["accounts"].as_array().unwrap().is_empty());
    assert!(doc["refused"].as_str().is_some());
}

#[test]
fn all_inventory_reports_absent_damaged_stores_and_each_warning_once() {
    let f = Fixture::new(Cli::Codex, false);
    std::fs::write(f.store.logins_file(), "{").unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        run_accounts_all(true, &f.env, &NoProvider, &mut out, &mut err),
        ExitCode::from(1)
    );
    let doc: Value = aterm_json::from_slice(&out).unwrap();
    assert_eq!(doc["pools"][0]["cli"], Value::from("codex"));
    assert!(doc["pools"][0]["report"]["refused"].as_str().is_some());
    assert!(
        doc["pools"][0]["report"]["accounts"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    out.clear();
    err.clear();
    assert_eq!(
        run_accounts_all(false, &f.env, &NoProvider, &mut out, &mut err),
        ExitCode::from(1)
    );
    assert!(out.is_empty(), "no empty section header on refusal");

    let f = Fixture::new(Cli::Codex, true);
    std::fs::write(f.store.machine_file(), "invalid").unwrap();
    out.clear();
    err.clear();
    assert_eq!(
        run_accounts_all(true, &f.env, &NoProvider, &mut out, &mut err),
        ExitCode::SUCCESS
    );
    assert_eq!(
        String::from_utf8(err)
            .unwrap()
            .matches("no machine id could be read")
            .count(),
        1
    );
}

#[test]
fn empty_report_text_uses_captured_roster_presence() {
    for present in [false, true] {
        let f = Fixture::new(Cli::Codex, false);
        if present {
            std::fs::write(&f.roster, "").unwrap();
        }
        let records = f
            .store
            .read_account_records(Some(&f.roster), f.env.now)
            .unwrap();
        if present {
            std::fs::rename(&f.roster, f.roster.with_extension("retained")).unwrap();
        } else {
            std::fs::write(&f.roster, "").unwrap();
        }
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            run_accounts_report_snapshot(
                (Cli::Codex, Launch::default()),
                &f.env,
                false,
                &f.roster,
                Ok(records),
                &mut out,
                &mut err
            ),
            ExitCode::SUCCESS
        );
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains(if present {
                "(no rows)"
            } else {
                "(no such file)"
            }),
            "{text}"
        );
    }
}

#[test]
fn missing_report_paths_and_provider_homes_have_json_refusals() {
    for cli in Cli::ALL {
        let f = Fixture::new(cli, true);
        let mut env = f.env.clone();
        env.config = None;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            run_accounts_report((cli, Launch::default()), &env, true, &mut out, &mut err),
            ExitCode::from(1)
        );
        let doc: Value = aterm_json::from_slice(&out).unwrap();
        assert!(doc["refused"].as_str().is_some());
        assert!(doc["roster"].is_null());
        if cli == Cli::Claude {
            continue;
        }
        out.clear();
        err.clear();
        assert_eq!(
            run_accounts_stats_of((cli, Launch::default()), &env, true, &mut out, &mut err),
            ExitCode::from(1)
        );
        let doc: Value = aterm_json::from_slice(&out).unwrap();
        assert!(doc["refused"].as_str().unwrap().contains("config"));
        env = f.env.clone();
        env.home = None;
        out.clear();
        err.clear();
        assert_eq!(
            run_accounts_report((cli, Launch::default()), &env, true, &mut out, &mut err),
            ExitCode::from(1)
        );
        let doc: Value = aterm_json::from_slice(&out).unwrap();
        assert!(doc["refused"].as_str().is_some());
        assert!(doc["accounts"].as_array().unwrap().is_empty());
        out.clear();
        err.clear();
        assert_eq!(
            run_accounts_stats_of((cli, Launch::default()), &env, true, &mut out, &mut err),
            ExitCode::from(1)
        );
        let doc: Value = aterm_json::from_slice(&out).unwrap();
        assert!(doc["refused"].as_str().is_some());
        assert!(doc["rows"].as_array().unwrap().is_empty());
    }
}
