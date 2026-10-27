// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;
const ACCOUNT: &str = "0f6c2b8e-1d3a-4c5e-9b7f-2a4d6e8c0b1a";
const OTHER: &str = "7d1e4a90-5b2c-4f3e-8a6d-0c9b8e7f6a5d";

struct Bed {
    _root: aterm_tempfile::TempDir,
    env: Env,
    store: accounts::Store,
    path: PathBuf,
    old: PathBuf,
    new: PathBuf,
}

impl Bed {
    fn new() -> Self {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        let config = home.join(".config/aterm/aterm.toml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, "[fleet]\nmachine='m1'\n").unwrap();
        let env = Env {
            state: root.path().join("state"),
            aterm_state: None,
            cwd: home.clone(),
            home: Some(home.clone()),
            claude_dir: Some(home.join(".claude")),
            claude_config_unusable: None,
            claude_profile: Some(home.join(".claude.json")),
            claude_custom_oauth: false,
            claude_session: None,
            claude_pid: None,
            codex_thread: None,
            agy_conversation: None,
            codex_home: None,
            now: NOW,
            tz_s: 0,
            sid: String::new(),
            sock: None,
            config: Some(config),
            claude_tmp: None,
            claude_running: None,
        };
        let path = env.accounts_roster().unwrap();
        let store = env.accounts_store().bounded_by(&[]);
        let old = root.path().join("old");
        let new = root.path().join("new");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(old.join(".credentials.json"), "opaque fixture").unwrap();
        fs::write(&path, Self::roster("original", &new)).unwrap();
        store
            .transact_logins(NOW, &mut None, |state, logins| {
                let events = accounts::note_renewed(
                    state,
                    logins,
                    "original",
                    None,
                    old.to_str().unwrap(),
                    None,
                    NOW,
                );
                state.active = Some("original".into());
                state.pinned = true;
                Ok(events)
            })
            .unwrap();
        Self {
            _root: root,
            env,
            store,
            path,
            old,
            new,
        }
    }

    fn roster(label: &str, dir: &Path) -> String {
        accounts::append_rows_for(
            Cli::Claude,
            "",
            &[Row {
                label: label.into(),
                dir: Some(dir.to_str().unwrap().into()),
                ..Row::default()
            }],
        )
        .unwrap()
    }

    fn pending(&self) {
        let witness = self.store.state_witness_file();
        fs::rename(&witness, witness.with_extension("retained")).unwrap();
        fs::create_dir(&witness).unwrap();
        assert!(
            self.store
                .transact_logins(NOW, &mut None, |state, logins| {
                    Ok(accounts::note_renewed(
                        state,
                        logins,
                        "original",
                        None,
                        self.new.to_str().unwrap(),
                        None,
                        NOW,
                    ))
                })
                .is_err()
        );
        fs::rename(&witness, witness.with_extension("obstruction")).unwrap();
        fs::rename(witness.with_extension("retained"), witness).unwrap();
    }

    fn later_commit(&self) {
        let before = fs::read_to_string(&self.path).unwrap();
        let after = Self::roster("later", &self.old);
        let held = self.store.acquire().unwrap();
        self.store
            .transact_sync_held(
                &held,
                NOW + 1,
                &mut None,
                accounts::SyncTarget {
                    path: &self.path,
                    before: Some(&before),
                    after: Some(&after),
                    mode: None,
                },
                |state, logins| {
                    state.active = Some("later".into());
                    state.pinned = false;
                    Ok(accounts::note_renewed(
                        state,
                        logins,
                        "later",
                        None,
                        self.old.to_str().unwrap(),
                        None,
                        NOW + 1,
                    ))
                },
                || Ok(()),
            )
            .unwrap();
    }

    fn html(&self, after: impl FnOnce()) -> String {
        render_dashboard_html_with(&self.env, None, Some("m1"), "127.0.0.1:8741", after)
    }
}

fn table(html: &str) -> &str {
    html.split_once("<tbody>")
        .unwrap()
        .1
        .split_once("</tbody>")
        .unwrap()
        .0
}

fn report(html: &str) -> &str {
    html.split_once("<pre>")
        .unwrap()
        .1
        .split_once("</pre>")
        .unwrap()
        .0
}

fn case(fault: i64, later: i64) -> (bool, bool) {
    let bed = Bed::new();
    let paths = [
        bed.path.clone(),
        bed.store.logins_file(),
        bed.store.state_file(),
        bed.store.dir.join("sync-commit.json"),
    ];
    let before = paths.each_ref().map(|path| fs::read(path).unwrap());
    match fault {
        1 | 6 => bed.pending(),
        2 => fs::write(&bed.path, "[[account]").unwrap(),
        3 => fs::write(bed.store.logins_file(), "{").unwrap(),
        4 => fs::write(bed.store.state_file(), "{").unwrap(),
        5 => fs::write(bed.store.dir.join("sync-commit.json"), "{").unwrap(),
        _ => {}
    }
    // The production reader has a bounded lock wait; a held pending writer must
    // still fail closed. No race or sleep is used to arrange the interleaving.
    let mut held = (fault == 6).then(|| bed.store.acquire().unwrap());
    let html = bed.html(|| {
        if fault > 1 {
            if later == 1 {
                drop(held.take());
                if fault == 6 {
                    bed.store.recover_records(&bed.path).unwrap();
                } else {
                    for (path, bytes) in paths.iter().zip(&before) {
                        fs::write(path, bytes).unwrap();
                    }
                }
            }
            return;
        }
        match later {
            1 => bed.later_commit(),
            2 => fs::write(&bed.path, "[[account]").unwrap(),
            _ => {}
        }
    });
    let rows = table(&html);
    let text = report(&html);
    let visible = rows.contains("<code>original</code>");
    let coherent = if fault < 2 {
        let condition = if fault == 1 { "not-enrolled" } else { "usable" };
        visible
            && rows.contains(&format!("<td>{condition}</td>"))
            && rows.contains("<td>pinned</td>")
            && text.contains("original")
            && text.contains(condition)
            && !rows.contains("<code>later</code>")
            && !text.contains("later")
            && !rows.contains("Pool state is unknown")
    } else {
        !visible
            && !rows.contains("/use?label=")
            && rows.contains("Pool state is unknown")
            && !text.trim().is_empty()
    };
    assert_eq!(
        fs::read_to_string(bed.old.join(".credentials.json")).unwrap(),
        "opaque fixture"
    );
    assert!(!bed.new.join(".credentials.json").exists());
    if fault > 1 && later == 1 {
        assert!(
            bed.store.read_account_records(Some(&bed.path), NOW).is_ok(),
            "a later healthy image does not replace this request's refusal"
        );
    }
    (visible, coherent)
}

pub(crate) fn dashboard_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherFault" => {
            next.insert("fault", state["fault"] + 1);
        }
        "LaterChange" => {
            next.insert("later", state["later"] + 1);
        }
        "Draw" => {
            let (visible, coherent) = case(state["fault"], state["later"]);
            next.insert("phase", 1);
            next.insert("visible", i64::from(visible));
            next.insert("coherent", i64::from(coherent));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn dashboard_retains_one_admitted_projection() {
    let model = aterm_models_harness::account_dashboard_model();
    assert_eq!(model.mutant_dials(), vec![1, 2, 3]);
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut draws = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = dashboard_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                draws += usize::from(action.name == "Draw");
            }
        }
    }
    assert_eq!(draws, 21);
    let mut bad = model.init_state();
    bad.insert("phase", 1);
    assert!(!model.check_invariant("KnownReportVisible", &bad));
    bad.insert("fault", 2);
    bad.insert("visible", 1);
    assert!(!model.check_invariant("UnknownReportHidden", &bad));
    bad.insert("coherent", 0);
    assert!(!model.check_invariant("SameCapturedProjection", &bad));
}

#[test]
fn dashboard_obeys_the_report_identity_filter_and_keeps_other_rows() {
    for many in [false, true] {
        for mismatch in [false, true] {
            let bed = Bed::new();
            fs::write(bed.env.claude_profile.as_ref().unwrap(), aterm_json::to_string(&aterm_json::json!({
                "oauthAccount": { "accountUuid": if mismatch { OTHER } else { ACCOUNT }, "emailAddress": "fixture@example.test" }
            })).unwrap()).unwrap();
            if many {
                let before = fs::read_to_string(&bed.path).unwrap();
                fs::write(
                    &bed.path,
                    accounts::append_rows_for(
                        Cli::Claude,
                        &before,
                        &[Row {
                            label: "second".into(),
                            dir: Some(bed.env.state.join("second").to_str().unwrap().into()),
                            ..Row::default()
                        }],
                    )
                    .unwrap(),
                )
                .unwrap();
            }
            bed.store
                .transact(NOW, &mut None, |state| {
                    let events = accounts::note_wall(
                        state,
                        "original",
                        "weekly",
                        Some(NOW + 3600),
                        NOW,
                        NOW,
                    );
                    state.accounts.get_mut("original").unwrap().key =
                        super::super::profile::AccountKey::claude(ACCOUNT);
                    Ok(events)
                })
                .unwrap();
            let html = bed.html(|| {
                fs::write(bed.env.claude_profile.as_ref().unwrap(), aterm_json::to_string(&aterm_json::json!({
                    "oauthAccount": { "accountUuid": if mismatch { ACCOUNT } else { OTHER }, "emailAddress": "later@example.test" }
                })).unwrap()).unwrap();
            });
            let rows = table(&html);
            let text = report(&html);
            if mismatch && !many {
                assert!(rows.contains("<td>unseen</td>"), "{rows}");
                assert!(!rows.contains("limited until"), "{rows}");
                assert!(!text.contains("limited"), "{text}");
            } else {
                assert!(rows.contains("limited until"), "{rows}");
                assert!(text.contains("limited"), "{text}");
            }
        }
    }
}

#[test]
fn dashboard_keeps_empty_and_own_login_report_semantics() {
    for own in [false, true] {
        let bed = Bed::new();
        fs::write(&bed.path, "").unwrap();
        if own {
            bed.store
                .transact(NOW, &mut None, |state| {
                    state.accounts.insert(
                        super::super::cli::ACCOUNTS_OWN_LABEL.into(),
                        accounts::Account {
                            sample: Some(accounts::Sample {
                                session_pct: Some(10),
                                session_reset: Some(NOW + 3600),
                                week_pct: None,
                                week_reset: None,
                                session_read_at: None,
                                week_read_at: None,
                                at: NOW,
                                by: String::new(),
                            }),
                            ..accounts::Account::default()
                        },
                    );
                    Ok(Vec::new())
                })
                .unwrap();
        }
        let html = bed.html(|| {});
        assert!(table(&html).is_empty());
        assert!(!html.contains("Pool state is unknown"));
        if own {
            assert!(report(&html).contains("this login"));
        }
    }
    let mut bed = Bed::new();
    bed.env.config = None;
    let html = bed.html(|| {});
    assert!(table(&html).contains("Pool state is unknown"));
    assert!(!table(&html).contains("original"));
}
