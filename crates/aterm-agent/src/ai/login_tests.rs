// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::PathBuf;

use super::*;
use crate::ai::connector::{CLAUDE, CODEX};
use crate::ai::facts::{ClaudeAccounts, Facts, Managed};
use crate::ai::state::{Mark, Row, State};
use crate::ai::store::Record;
use crate::ai::testing::{FakeRunner, machine, ran};

fn facts(c: &'static Connector, shim: PathBuf) -> Facts {
    Facts {
        connector: c,
        kept: None,
        managed: Some(Managed {
            path: shim,
            version: None,
        }),
        foreign: None,
        home: Ok(PathBuf::from("/home/me/.x")),
        home_var: None,
        home_exists: true,
        login_file: None,
        hidden_store: false,
        profile_signed_in: false,
        plan: None,
        accounts: None,
        limited_until: None,
        record: Record::default(),
        record_scope: (c.id == "codex").then_some("file"),
        credential_store_error: None,
        env_credentials: Vec::new(),
        pool: None,
        review_source: None,
    }
}

fn row(c: &'static Connector, state: State) -> Row {
    Row {
        connector: c,
        wanted: true,
        asked: true,
        state,
        mark: Mark::Fail,
        fact: String::new(),
        fix: None,
        mend: None,
        actor: None,
        version: None,
        plan: None,
        used: None,
        renew_by: None,
        limited_until: None,
    }
}

struct Rig {
    vars: Vec<(String, String)>,
    no_desk: Option<PathBuf>,
    /// Whether stdin is a terminal: a person is here unless an agent's marker is set too.
    tty: bool,
}

impl Rig {
    fn new(extra: &[(&str, &str)]) -> Rig {
        let mut vars = vec![
            ("HOME".to_string(), "/home/me".to_string()),
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ("OPENAI_API_KEY".to_string(), "sk-test".to_string()),
            ("CLAUDE_PID".to_string(), "1".to_string()),
        ];
        vars.extend(
            extra
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
        );
        Rig {
            vars,
            no_desk: Some(PathBuf::from(crate::harness::login_drive::SANDBOX_EXEC)),
            tty: true,
        }
    }

    /// The same rig with no terminal on stdin.
    fn no_tty(mut self) -> Rig {
        self.tty = false;
        self
    }

    /// The same rig with `sandbox` as its no-desk sandbox ([`Host::no_desk`]).
    #[cfg(target_os = "macos")]
    fn sandbox(mut self, sandbox: Option<PathBuf>) -> Rig {
        self.no_desk = sandbox;
        self
    }

    fn sign_in(
        &self,
        runner: &FakeRunner,
        store: &Store,
        f: &Facts,
        r: &Row,
        answers: &[&str],
    ) -> Done {
        let vars = self.vars.clone();
        let var = move |k: &str| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let given: std::cell::RefCell<Vec<String>> =
            std::cell::RefCell::new(answers.iter().map(|s| (*s).to_string()).collect());
        let ask = |_: &str| {
            let mut g = given.borrow_mut();
            (!g.is_empty()).then(|| g.remove(0))
        };
        let now = || 1_800_000_000;
        let host = Host {
            runner,
            var: &var,
            vars: &self.vars,
            aterm: Path::new("/app/aterm"),
            store,
            now: &now,
            ask: &ask,
            no_desk: self.no_desk.as_deref(),
            attended: self.tty && agent_marker(&var).is_none(),
        };
        sign_in(&host, f, r, &mut Vec::new())
    }
}

/// AN AGENT NEVER STARTS A PERSON'S SIGN-IN: a CLI's own login (Codex's here) is refused under
/// every agent's marker and nothing runs.
#[test]
fn an_agent_never_starts_a_clis_own_sign_in() {
    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    let runner = FakeRunner::quiet();
    for marker in AGENT_MARKERS {
        let done = Rig::new(&[(marker, "1")]).sign_in(
            &runner,
            &store,
            &facts(&CODEX, b.twin("codex")),
            &row(&CODEX, State::SignedOut),
            &[],
        );
        assert!(matches!(done, Done::Refused(_)), "{marker}: {done:?}");
    }
    assert!(runner.calls().is_empty(), "nothing ran under an agent");
}

/// UNDER AN AGENT ATERM'S OWN SIGN-INS RUN (owner ruling R-13): accounts v2's renewal of a
/// Claude row with an email and a route, and a pool's renewal of its next row, are aterm's, so
/// each runs under every agent's marker, with stdin closed (never on the terminal, where a
/// renewal may open a browser for a person); what only a person gives, the email Claude's
/// adoption of the default login asks, is refused and asks nothing.
#[test]
fn under_an_agent_aterms_own_renewals_run_and_claudes_adoption_does_not() {
    use crate::ai::connector::KIMI;
    let b = machine(&["claude", "kimi"]);
    let store = Store::at(&b.state);
    for marker in AGENT_MARKERS {
        let rig = Rig::new(&[(marker, "1")]);
        let mut renewing = facts(&CLAUDE, b.twin("claude"));
        renewing.accounts = Some(listed_view(true, None));
        let runner = FakeRunner::quiet();
        let done = rig.sign_in(
            &runner,
            &store,
            &renewing,
            &row(&CLAUDE, State::SignedOut),
            &[],
        );
        assert_eq!(done, Done::SignedIn(None), "{marker}");
        assert_eq!(
            runner
                .calls()
                .iter()
                .map(|c| (c.terminal, c.args.join(" ")))
                .collect::<Vec<_>>(),
            [(false, "harness accounts renew work".to_string())],
            "{marker}"
        );

        let mut pooled = facts(&KIMI, b.twin("kimi"));
        pooled.pool = Some(crate::ai::facts::CliAccounts {
            roster: PathBuf::from("/home/me/.config/aterm/accounts-kimi.toml"),
            next: Some("kimi-one".to_string()),
            ..crate::ai::facts::CliAccounts::default()
        });
        let runner = FakeRunner::quiet();
        let done = rig.sign_in(&runner, &store, &pooled, &row(&KIMI, State::SignedOut), &[]);
        assert_eq!(done, Done::SignedIn(None), "{marker}");
        assert_eq!(
            runner.calls()[0].args,
            ["harness", "accounts", "renew", "kimi-one", "--cli", "kimi"],
            "{marker}"
        );
        assert!(!runner.calls()[0].terminal, "{marker}");

        let runner = FakeRunner::quiet();
        let done = rig.sign_in(
            &runner,
            &store,
            &facts(&CLAUDE, b.twin("claude")),
            &row(&CLAUDE, State::SignedOut),
            &["me@example.com"],
        );
        assert!(
            matches!(done, Done::Refused(ref why) if why.contains("email")),
            "{marker}: {done:?}"
        );
        assert!(runner.calls().is_empty(), "{marker}: the adoption ran");
    }
}

#[test]
fn codexs_login_runs_only_when_codex_is_signed_out_because_it_revokes_first() {
    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    let shim = b.twin("codex");
    let rig = Rig::new(&[]);
    for state in [
        State::Ok,
        State::Idle,
        State::Unproven,
        State::Due(1),
        State::Limited(1),
    ] {
        let runner = FakeRunner::quiet();
        let done = rig.sign_in(
            &runner,
            &store,
            &facts(&CODEX, shim.clone()),
            &row(&CODEX, state),
            &[],
        );
        assert!(
            matches!(done, Done::Refused(ref why) if why.contains("revokes")),
            "{state:?}: {done:?}"
        );
        assert!(
            runner.calls().is_empty(),
            "{state:?}: a working login was left alone"
        );
    }
    let runner = FakeRunner::with(|call| {
        Ok(if call.args == ["login", "status"] {
            ran(0, "", "Logged in using ChatGPT\n")
        } else {
            ran(0, "", "")
        })
    });
    let done = rig.sign_in(
        &runner,
        &store,
        &facts(&CODEX, shim),
        &row(&CODEX, State::SignedOut),
        &[],
    );
    assert_eq!(done, Done::SignedIn(Some("chatgpt")));
    let calls = runner.calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].terminal && calls[0].args == ["login"]);
    assert_eq!(calls[1].args, ["login", "status"]);
    for call in &calls {
        assert!(
            call.env
                .iter()
                .all(|(k, _)| k != "OPENAI_API_KEY" && k != "CLAUDE_PID"),
            "{:?}",
            call.env
        );
        assert!(
            call.cwd
                .as_ref()
                .is_some_and(|d| d.starts_with(store.dir())),
            "a run directory"
        );
    }
    assert_eq!(
        store
            .load()
            .expect("load")
            .of_scope("codex", Some("file"))
            .used_how,
        Some(How::Login)
    );
}

#[test]
fn a_status_read_drops_what_follows_its_prefix() {
    let Login::Run {
        after: Some(status),
        ..
    } = &CODEX.login
    else {
        panic!("codex reads its status")
    };
    let key = Ok(ran(
        0,
        "",
        "Logged in using an API key - sk-proj-abc***xyz\n",
    ));
    assert_eq!(read_status(status, &key), Said::In("api-key"));
    assert_eq!(
        read_status(status, &Ok(ran(1, "", "Not logged in\n"))),
        Said::Out
    );
    assert!(matches!(
        read_status(status, &Ok(ran(1, "", "Error checking login status: x\n"))),
        Said::Unread(_)
    ));
}

#[test]
fn claude_signs_in_only_through_accounts_v2s_renewal() {
    let b = machine(&["claude"]);
    let store = Store::at(&b.state);
    let shim = b.twin("claude");
    let rig = Rig::new(&[]);
    let runner = FakeRunner::quiet();
    let done = rig.sign_in(
        &runner,
        &store,
        &facts(&CLAUDE, shim.clone()),
        &row(&CLAUDE, State::SignedOut),
        &["me@example.com"],
    );
    assert_eq!(done, Done::SignedIn(None));
    let calls = runner.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].program, "aterm");
    assert_eq!(
        calls[0].args,
        [
            "harness",
            "accounts",
            "adopt-default",
            "main",
            "--email",
            "me@example.com"
        ]
    );
    assert_eq!(calls[1].args, ["harness", "accounts", "renew", "main"]);
    assert!(calls.iter().all(|c| c.program != "claude"));
    let mut f = facts(&CLAUDE, shim.clone());
    f.accounts = Some(ClaudeAccounts {
        pool_picks: false,
        label: "m7".into(),
        store: None,
        roster: None,
        listed: false,
        refused: None,
        unread: None,
        launch_refused: None,
        sign_in_refused: None,
        renews: true,
        login: None,
        made: true,
        state: Some("due"),
        renew_by: None,
        limited_until: None,
        served_at: None,
        renewed_at: None,
        signed_out_at: None,
        next_sign_in: None,
        tried: None,
        unattended: true,
        off: None,
    });
    let runner = FakeRunner::quiet();
    assert_eq!(
        rig.sign_in(&runner, &store, &f, &row(&CLAUDE, State::Due(1)), &[]),
        Done::SignedIn(None)
    );
    assert_eq!(
        runner
            .calls()
            .iter()
            .map(|c| c.args.clone())
            .collect::<Vec<_>>(),
        [vec!["harness", "accounts", "renew", "m7"]]
    );
    let runner = FakeRunner::quiet();
    let done = rig.sign_in(
        &runner,
        &store,
        &facts(&CLAUDE, shim),
        &row(&CLAUDE, State::SignedOut),
        &[""],
    );
    assert!(matches!(done, Done::Refused(_)));
    assert!(runner.calls().is_empty());
}

fn listed_view(renews: bool, refused: Option<&str>) -> ClaudeAccounts {
    ClaudeAccounts {
        pool_picks: false,
        store: None,
        roster: None,
        label: "work".into(),
        listed: true,
        refused: refused.map(str::to_string),
        unread: None,
        launch_refused: None,
        sign_in_refused: None,
        renews,
        login: None,
        made: false,
        state: None,
        renew_by: None,
        limited_until: None,
        served_at: None,
        renewed_at: None,
        signed_out_at: None,
        next_sign_in: None,
        tried: None,
        unattended: true,
        off: None,
    }
}

#[test]
fn claude_never_adopts_over_a_listed_row_or_a_refused_roster() {
    let b = machine(&["claude"]);
    let store = Store::at(&b.state);
    let shim = b.twin("claude");
    let rig = Rig::new(&[]);
    for (view, says) in [
        (listed_view(false, None), "with no email"),
        (
            listed_view(false, Some("line 3: not TOML")),
            "accounts.toml is refused",
        ),
    ] {
        let mut f = facts(&CLAUDE, shim.clone());
        f.accounts = Some(view);
        let runner = FakeRunner::quiet();
        let done = rig.sign_in(
            &runner,
            &store,
            &f,
            &row(&CLAUDE, State::SignedOut),
            &["me@example.com"],
        );
        assert!(
            matches!(done, Done::Refused(ref why) if why.contains(says)),
            "{done:?}"
        );
        assert!(
            runner.calls().is_empty(),
            "nothing ran: {:?}",
            runner.calls()
        );
    }
    let mut with_successor = listed_view(false, None);
    with_successor.next_sign_in = Some("m27".into());
    let mut f = facts(&CLAUDE, shim);
    f.accounts = Some(with_successor);
    let runner = FakeRunner::quiet();
    assert_eq!(
        rig.sign_in(&runner, &store, &f, &row(&CLAUDE, State::Limited(100)), &[]),
        Done::SignedIn(None),
        "when next_sign_in is present, login renews the successor even if the default row has no email"
    );
    assert_eq!(
        runner
            .calls()
            .iter()
            .map(|c| c.args.clone())
            .collect::<Vec<_>>(),
        [vec!["harness", "accounts", "renew", "m27"]]
    );
}

#[test]
fn a_failed_adopt_stops_before_the_renewal() {
    let b = machine(&["claude"]);
    let store = Store::at(&b.state);
    let runner = FakeRunner::quiet().exiting(|call| {
        if call.args.get(2).is_some_and(|a| a == "adopt-default") {
            1
        } else {
            0
        }
    });
    let done = Rig::new(&[]).sign_in(
        &runner,
        &store,
        &facts(&CLAUDE, b.twin("claude")),
        &row(&CLAUDE, State::SignedOut),
        &["me@example.com"],
    );
    assert!(
        matches!(done, Done::Failed(ref why) if why.contains("adopt-default")),
        "{done:?}"
    );
    assert_eq!(runner.calls().len(), 1, "no renewal after a refused adopt");
}

#[test]
fn a_sign_in_under_another_home_records_nothing() {
    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    let runner = FakeRunner::with(|call| {
        Ok(if call.args == ["login", "status"] {
            ran(1, "", "Not logged in\n")
        } else {
            ran(0, "", "")
        })
    });
    let other = b.dir.path().join("identity-codex");
    let rig = Rig::new(&[("CODEX_HOME", other.to_str().expect("utf8"))]);
    let mut explicit = facts(&CODEX, b.twin("codex"));
    explicit.home_var = Some(other.display().to_string());
    explicit.home = Ok(other);
    let done = rig.sign_in(
        &runner,
        &store,
        &explicit,
        &row(&CODEX, State::SignedOut),
        &[],
    );
    assert_eq!(done, Done::SignedOut);
    assert_eq!(
        store.load().expect("load").of_scope("codex", Some("file")),
        Record::default()
    );
    let done = Rig::new(&[]).sign_in(
        &runner,
        &store,
        &facts(&CODEX, b.twin("codex")),
        &row(&CODEX, State::SignedOut),
        &[],
    );
    assert_eq!(done, Done::SignedOut);
    assert!(
        store
            .load()
            .expect("load")
            .of_scope("codex", Some("file"))
            .ended()
    );
}

#[test]
fn codex_sign_in_health_belongs_to_its_observed_backend() {
    for initial in ["file", "keyring", "auto", "ephemeral"] {
        for current in ["file", "keyring", "auto", "ephemeral"] {
            for switch_at in [0, 1, 2] {
                for signed_in in [false, true] {
                    let b = machine(&["codex"]);
                    let store = Store::at(&b.state);
                    store
                        .note("codex", How::Login, Outcome::Worked, 10)
                        .unwrap();
                    let legacy = store.load().unwrap().of("codex");
                    let mut f = facts(&CODEX, b.twin("codex"));
                    let home = b.home.join(".codex");
                    std::fs::create_dir_all(&home).unwrap();
                    let config = home.join("config.toml");
                    std::fs::write(
                        &config,
                        format!("cli_auth_credentials_store = {initial:?}\n"),
                    )
                    .unwrap();
                    f.home = Ok(home);
                    f.record_scope = Some(initial);
                    f.hidden_store = initial != "file";
                    let terminal_config = config.clone();
                    let status_config = config.clone();
                    let runner = FakeRunner::with(move |_| {
                        if switch_at == 2 {
                            std::fs::write(
                                &status_config,
                                format!("cli_auth_credentials_store = {current:?}\n"),
                            )
                            .unwrap();
                        }
                        Ok(if signed_in {
                            ran(0, "", "Logged in using ChatGPT\n")
                        } else {
                            ran(1, "", "Not logged in\n")
                        })
                    })
                    .exiting(move |_| {
                        if switch_at == 1 {
                            std::fs::write(
                                &terminal_config,
                                format!("cli_auth_credentials_store = {current:?}\n"),
                            )
                            .unwrap();
                        }
                        0
                    });
                    if switch_at == 0 {
                        std::fs::write(
                            &config,
                            format!("cli_auth_credentials_store = {current:?}\n"),
                        )
                        .unwrap();
                    }
                    let done = Rig::new(&[]).sign_in(
                        &runner,
                        &store,
                        &f,
                        &row(&CODEX, State::SignedOut),
                        &[],
                    );
                    let after = store.load().unwrap();
                    assert_eq!(after.of("codex"), legacy);
                    if initial == current {
                        assert_eq!(runner.calls().len(), 2);
                        assert_eq!(
                            done,
                            if signed_in {
                                Done::SignedIn(Some("chatgpt"))
                            } else {
                                Done::SignedOut
                            }
                        );
                        let record = after.of_scope("codex", Some(initial));
                        assert_eq!(record.ended(), !signed_in);
                        assert_eq!(record.used_at.is_some(), signed_in);
                        assert_eq!(after.by_scope["codex"].len(), 1);
                    } else {
                        assert!(
                            after.by_scope.is_empty(),
                            "{initial} -> {current}, phase {switch_at}"
                        );
                        assert_eq!(runner.calls().len(), switch_at);
                        if switch_at == 0 {
                            assert!(matches!(done, Done::Refused(_)));
                        } else {
                            assert_eq!(done, Done::NotChecked);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn an_unknown_codex_backend_cannot_start_a_sign_in() {
    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    let mut f = facts(&CODEX, b.twin("codex"));
    let home = b.home.join(".codex");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        "cli_auth_credentials_store = 'future'\n",
    )
    .unwrap();
    f.home = Ok(home);
    let runner = FakeRunner::quiet();
    assert!(matches!(
        Rig::new(&[]).sign_in(&runner, &store, &f, &row(&CODEX, State::SignedOut), &[]),
        Done::Refused(_)
    ));
    assert!(runner.calls().is_empty());
    assert_eq!(
        store.load().unwrap(),
        super::super::store::Records::default()
    );
}

#[test]
fn a_file_roster_cannot_redirect_or_hold_a_hidden_backend_sign_in() {
    use crate::ai::facts::CliAccounts;
    for backend in ["keyring", "auto", "ephemeral"] {
        for refused in [false, true] {
            let b = machine(&["codex"]);
            let store = Store::at(&b.state);
            let mut f = facts(&CODEX, b.twin("codex"));
            let home = b.home.join(".codex");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::write(
                home.join("config.toml"),
                format!("cli_auth_credentials_store = {backend:?}\n"),
            )
            .unwrap();
            f.home = Ok(home);
            f.record_scope = Some(backend);
            f.hidden_store = true;
            f.record.apply(
                How::Review,
                Outcome::Ended(Ended::ReviewAuth),
                1_800_000_000,
            );
            f.pool = Some(CliAccounts {
                refused: refused.then(|| "a broken inactive file roster".into()),
                state: Some("expired"),
                next: Some("inactive-file-row".into()),
                off: Some("an inactive pool switch".into()),
                bot_check_at: Some(1_800_000_000),
                sign_in_refused: Some("an inactive file-account refusal".into()),
                ..CliAccounts::default()
            });
            let row = crate::ai::state::assess(&f, true, 1_800_000_000);
            assert_eq!(row.state, State::SignedOut);
            assert_eq!(row.actor, Some(crate::ai::state::Actor::Person));
            assert_eq!(row.fix.as_deref(), Some("aterm ai login codex"));
            assert!(refusal(&f, &row).is_none());
            assert!(crate::ai::state::held(&f, 1_800_000_000).is_none());
            let runner = FakeRunner::with(|_| Ok(ran(0, "", "Logged in using ChatGPT\n")));
            assert_eq!(
                Rig::new(&[]).sign_in(&runner, &store, &f, &row, &[]),
                Done::SignedIn(Some("chatgpt"))
            );
            assert!(runner.calls().iter().all(|call| call.program == "codex"));
            assert_eq!(runner.calls().len(), 2);
            assert_eq!(
                store
                    .load()
                    .unwrap()
                    .of_scope("codex", Some(backend))
                    .used_how,
                Some(How::Login)
            );
        }
    }
}

#[test]
fn a_login_status_cannot_replace_a_newer_health_observation() {
    for during_status in [false, true] {
        let b = machine(&["codex"]);
        let store = Store::at(&b.state);
        let terminal_store = store.clone();
        let status_store = store.clone();
        let runner = FakeRunner::with(move |_| {
            if during_status {
                status_store
                    .note_scope(
                        "codex",
                        Some("file"),
                        How::Review,
                        Outcome::Ended(Ended::ReviewAuth),
                        200,
                    )
                    .unwrap();
            }
            Ok(ran(0, "", "Logged in using ChatGPT\n"))
        })
        .exiting(move |_| {
            if !during_status {
                terminal_store
                    .note_scope(
                        "codex",
                        Some("file"),
                        How::Review,
                        Outcome::Ended(Ended::ReviewAuth),
                        100,
                    )
                    .unwrap();
            }
            0
        });
        let f = facts(&CODEX, b.twin("codex"));
        assert_eq!(
            Rig::new(&[]).sign_in(&runner, &store, &f, &row(&CODEX, State::SignedOut), &[]),
            Done::SignedIn(Some("chatgpt"))
        );
        let after = store.load().unwrap().of_scope("codex", Some("file"));
        assert_eq!(after.ended(), during_status);
        assert_eq!(after.used_at.is_some(), !during_status);
        assert_eq!(after.used_how, (!during_status).then_some(How::Login));
    }
}

#[test]
fn explicit_codex_login_refuses_unknown_or_changed_storage() {
    for during_login in [false, true] {
        let b = machine(&["codex"]);
        let store = Store::at(&b.state);
        let explicit = b.dir.path().join("explicit-codex");
        std::fs::create_dir_all(&explicit).unwrap();
        let config = explicit.join("config.toml");
        std::fs::write(
            &config,
            if during_login {
                "cli_auth_credentials_store = 'file'"
            } else {
                "not TOML"
            },
        )
        .unwrap();
        let mut f = facts(&CODEX, b.twin("codex"));
        f.home_var = Some(explicit.display().to_string());
        f.home = Ok(explicit.clone());
        let runner = FakeRunner::quiet().exiting(move |_| {
            std::fs::write(&config, "not TOML").unwrap();
            0
        });
        let rig = Rig::new(&[("CODEX_HOME", explicit.to_str().unwrap())]);
        let result = rig.sign_in(&runner, &store, &f, &row(&CODEX, State::SignedOut), &[]);
        if during_login {
            assert_eq!(result, Done::NotChecked);
            assert_eq!(runner.calls().len(), 1);
        } else {
            assert!(matches!(result, Done::Refused(_)));
            assert!(runner.calls().is_empty());
        }
        assert_eq!(store.load().unwrap(), crate::ai::store::Records::default());
    }
}

#[test]
fn claude_under_a_home_of_its_own_is_never_signed_in_here() {
    let b = machine(&["claude"]);
    let store = Store::at(&b.state);
    let runner = FakeRunner::quiet();
    let rig = Rig::new(&[("CLAUDE_CONFIG_DIR", "/me/.config/claude")]);
    let mut f = facts(&CLAUDE, b.twin("claude"));
    f.home_var = Some("/me/.config/claude".to_string());
    let done = rig.sign_in(
        &runner,
        &store,
        &f,
        &row(&CLAUDE, State::SignedOut),
        &["me@example.com"],
    );
    assert!(
        matches!(done, Done::Refused(ref why) if why.contains("CLAUDE_CONFIG_DIR")),
        "{done:?}"
    );
    assert!(runner.calls().is_empty());
}

#[test]
fn claude_limited_with_next_sign_in_renews_the_successor_account() {
    let b = machine(&["claude"]);
    let store = Store::at(&b.state);
    let shim = b.twin("claude");
    let rig = Rig::new(&[]);
    let mut f = facts(&CLAUDE, shim);
    f.accounts = Some(ClaudeAccounts {
        pool_picks: false,
        label: "m27-3".into(),
        store: None,
        roster: None,
        listed: true,
        refused: None,
        unread: None,
        launch_refused: None,
        sign_in_refused: None,
        renews: true,
        login: None,
        made: true,
        state: Some("ok"),
        renew_by: None,
        limited_until: Some(1_800_086_400),
        served_at: None,
        renewed_at: None,
        signed_out_at: None,
        next_sign_in: Some("m27".into()),
        tried: None,
        unattended: true,
        off: None,
    });
    let runner = FakeRunner::quiet();
    assert_eq!(
        rig.sign_in(
            &runner,
            &store,
            &f,
            &row(&CLAUDE, State::Due(1_800_086_400)),
            &[]
        ),
        Done::SignedIn(None)
    );
    assert_eq!(
        runner
            .calls()
            .iter()
            .map(|c| c.args.clone())
            .collect::<Vec<_>>(),
        [vec!["harness", "accounts", "renew", "m27"]]
    );
}

#[test]
fn a_cli_with_its_own_roster_signs_in_through_its_renewal_and_one_without_runs_its_login() {
    use crate::ai::connector::KIMI;
    let b = machine(&["kimi"]);
    let store = Store::at(&b.state);
    let shim = b.twin("kimi");
    let rig = Rig::new(&[]);
    let mut pooled = facts(&KIMI, shim.clone());
    pooled.pool = Some(crate::ai::facts::CliAccounts {
        roster: PathBuf::from("/home/me/.config/aterm/accounts-kimi.toml"),
        refused: None,
        next: Some("kimi-one".to_string()),
        ..crate::ai::facts::CliAccounts::default()
    });
    let runner = FakeRunner::quiet();
    let done = rig.sign_in(&runner, &store, &pooled, &row(&KIMI, State::SignedOut), &[]);
    assert_eq!(done, Done::SignedIn(None));
    let calls = runner.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls[0].terminal && calls[0].program == "aterm",
        "{:?}",
        calls[0]
    );
    assert_eq!(
        calls[0].args,
        ["harness", "accounts", "renew", "kimi-one", "--cli", "kimi"]
    );

    let mut empty = pooled.clone();
    empty.pool = Some(crate::ai::facts::CliAccounts {
        next: None,
        ..pooled.pool.clone().expect("pool")
    });
    let runner = FakeRunner::quiet();
    let done = rig.sign_in(&runner, &store, &empty, &row(&KIMI, State::SignedOut), &[]);
    assert!(
        matches!(done, Done::Refused(ref why) if why.contains("lists no email")),
        "{done:?}"
    );
    assert!(runner.calls().is_empty());

    let runner = FakeRunner::quiet();
    let done = rig.sign_in(
        &runner,
        &store,
        &facts(&KIMI, shim),
        &row(&KIMI, State::SignedOut),
        &[],
    );
    assert_eq!(
        done,
        Done::NotChecked,
        "no roster: Kimi's own login, as before"
    );
    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].terminal && calls[0].program == "kimi" && calls[0].args == ["login"]);
}

/// ON A CODEX BUILD THAT KEEPS NO ROW'S LOGIN ITS OWN, `aterm ai login codex` SIGNS THE ROW ON
/// THIS MACHINE'S OWN LOGIN IN BY CODEX'S OWN LOGIN IN THE DEFAULT HOME, never by the pool's
/// staged sign-in, which would bind it to a login of its own that build never runs. With `main`
/// signed out and `work` listed with no login made here yet (as `accounts add` leaves it), the
/// facts name `main` next, `codex login` runs, the pool records `main` signed in again, and
/// `main` keeps no `dir` and serves the next launch on that build. With `main`'s login working
/// but its renewal due, it runs nothing and says why (Codex's own login revokes the login it
/// replaces), offering no fix, also where `main` is the roster's only row. CONTROL: a build that
/// keeps the row's login signs `work` in through the pool.
#[test]
fn a_codex_default_row_on_a_build_that_ignores_codex_auth_path_signs_in_by_codex_login() {
    use crate::ai::facts::{Machine, gather};
    use crate::ai::store::Records;
    use crate::harness::accounts::{self, Cli};
    use crate::harness::codex_app_login::Usage;
    let b = machine(&["codex"]);
    let launcher = b.twin("codex");
    let lay = |reads: bool| {
        let exe = crate::harness::codex_alab::lay_package(
            &b.dir
                .path()
                .join(if reads { "store/new" } else { "store/old" }),
            reads,
        );
        std::fs::write(&launcher, crate::harness::codex_alab::twin_text(&exe)).expect("launcher");
    };
    lay(false);
    let now = 1_800_000_000_i64;
    let config = b.dir.path().join("config");
    std::fs::create_dir_all(&config).expect("config");
    std::fs::create_dir_all(b.home.join(".codex")).expect("default home");
    let token = crate::harness::codex_app_login::tests::make_chatgpt_jwt(
        Some("own@example.com"),
        "plus",
        now + 3600,
    );
    std::fs::write(
        b.home.join(".codex/auth.json"),
        aterm_json::json!({"tokens":{"access_token":token}}).to_string(),
    )
    .expect("default login");
    let work = b.home.join(".aterm/logins/codex/work");
    std::fs::create_dir_all(&work).expect("work's empty login folder");
    let main_row =
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"own@example.com\"\n";
    let both = format!(
        "{main_row}\n[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {:?}\n\
         email = \"work@example.com\"\n",
        work.display().to_string()
    );
    let roster = config.join("accounts-codex.toml");
    std::fs::write(&roster, &both).expect("codex roster");
    let pool_store = accounts::Store::at_for(&b.state, Cli::Codex, &b.home);
    crate::ai::testing::current_codex_auth(
        &pool_store,
        &roster,
        "main",
        accounts::Auth::SignedOut,
        now,
    );
    let home = b.home.display().to_string();
    let var = |k: &str| (k == "HOME").then(|| home.clone());
    let at = |now: i64| Machine {
        now,
        home: Some(b.home.clone()),
        var: &var,
        pkg: Some(b.pkg.clone()),
        exclude: Vec::new(),
        records: Records::default(),
        harness_state: Some(b.state.clone()),
        roster: Some(config.join("accounts.toml")),
        accounts_mode: None,
        config: None,
    };
    let store = Store::at(&b.state);
    let rig = Rig::new(&[]);

    let f = gather(&at(now), &CODEX);
    let pool = f.pool.as_ref().expect("codex pool");
    assert!(pool.unread && pool.default_only, "{pool:?}");
    assert_eq!(pool.next.as_deref(), Some("main"), "never `work`");
    let r = crate::ai::state::assess(&f, true, now);
    assert_eq!(r.state, State::SignedOut, "{}", r.fact);
    assert_eq!(r.fix.as_deref(), Some("aterm ai login codex"), "{}", r.fact);
    assert_eq!(pool.due, ["main"], "a signed-out row is due");
    assert!(r.fact.contains("signs that login in again"), "{}", r.fact);
    assert!(
        !r.fact.contains("once that login has ended"),
        "its login has ended already: {}",
        r.fact
    );
    let runner = FakeRunner::with(|call| {
        Ok(if call.args == ["login", "status"] {
            ran(0, "", "Logged in using ChatGPT\n")
        } else {
            ran(0, "", "")
        })
    });
    assert_eq!(
        rig.sign_in(&runner, &store, &f, &r, &[]),
        Done::SignedIn(Some("chatgpt"))
    );
    let calls = runner.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[0].terminal && calls[0].args == ["login"], "{calls:?}");
    assert!(
        calls
            .iter()
            .all(|call| call.args.first().map(String::as_str) != Some("harness")),
        "no pool sign-in: {calls:?}"
    );
    let (state, _) = pool_store.load(now).expect("pool state");
    assert_ne!(
        state.accounts.get("main").map(|a| a.auth),
        Some(accounts::Auth::SignedOut),
        "the pool records `main` signed in again"
    );
    let (logins, _) = pool_store.load_logins(now).expect("pool logins");
    assert_eq!(logins.get("main").and_then(|b| b.dir.clone()), None);
    assert_eq!(logins.sign_in_receipts.get("main"), Some(&1));
    let seat = crate::harness::accounts_pool::Seat {
        config_dir: &config,
        state: &b.state,
        home: &b.home,
        now: now + 1,
    };
    let read = |_: Cli, _: &Path, _: Option<&str>, at: i64| -> Option<Usage> {
        Some(Usage {
            sample: accounts::Sample {
                session_pct: Some(0),
                at,
                ..accounts::Sample::default()
            },
            walls: Vec::new(),
        })
    };
    let pick =
        crate::harness::accounts_pool::launch_env_for(&seat, Cli::Codex, &read, Some(&launcher))
            .expect("the launch serves")
            .map(|pick| (pick.label, pick.value));
    assert_eq!(
        pick,
        Some(("main".to_string(), None)),
        "`main` on the default login"
    );

    // Its login works, and its renewal is due: nothing runs.
    pool_store
        .transact_logins(now + 2, &mut None, |_, logins| {
            accounts::note_login_notice(logins, "main", 2, now + 2);
            Ok(Vec::new())
        })
        .expect("a notice that `main`'s login ends in two days");
    let f = gather(&at(now + 3), &CODEX);
    let pool = f.pool.as_ref().expect("codex pool");
    assert_eq!(pool.due, ["main"]);
    assert_eq!(pool.next, None);
    let r = crate::ai::state::assess(&f, true, now + 3);
    assert!(
        matches!(r.state, State::Due(_)),
        "{:?}: {}",
        r.state,
        r.fact
    );
    assert_eq!(r.fix, None, "no fix that would refuse: {}", r.fact);
    assert!(r.fact.contains("once that login has ended"), "{}", r.fact);
    let runner = FakeRunner::quiet();
    let done = rig.sign_in(&runner, &store, &f, &r, &[]);
    assert!(
        matches!(&done, Done::Refused(why) if why.contains("once that login has ended")),
        "{done:?}"
    );
    assert!(runner.calls().is_empty());
    // With `main` the roster's only row, that build serves it without serving it "alone", and
    // its renewal there is still Codex's own: nothing runs.
    std::fs::write(&roster, main_row).expect("a roster of `main` alone");
    let f = gather(&at(now + 3), &CODEX);
    let pool = f.pool.as_ref().expect("codex pool");
    assert!(pool.unread && !pool.default_only, "{pool:?}");
    assert_eq!(
        (pool.due.as_slice(), pool.next.as_deref()),
        (&["main".to_string()][..], None)
    );
    let r = crate::ai::state::assess(&f, true, now + 3);
    assert!(
        matches!(r.state, State::Due(_)),
        "{:?}: {}",
        r.state,
        r.fact
    );
    assert_eq!(r.fix, None, "no fix that would refuse: {}", r.fact);
    let runner = FakeRunner::quiet();
    let done = rig.sign_in(&runner, &store, &f, &r, &[]);
    assert!(
        matches!(&done, Done::Refused(why) if why.contains("once that login has ended")),
        "{done:?}"
    );
    assert!(runner.calls().is_empty());
    std::fs::write(&roster, &both).expect("codex roster");

    lay(true);
    let f = gather(&at(now + 3), &CODEX);
    let pool = f.pool.as_ref().expect("codex pool");
    assert!(!pool.unread, "CONTROL");
    assert_eq!(pool.next.as_deref(), Some("work"));
    let runner = FakeRunner::quiet();
    let r = crate::ai::state::assess(&f, true, now + 3);
    assert_eq!(
        rig.sign_in(&runner, &store, &f, &r, &[]),
        Done::SignedIn(None)
    );
    assert_eq!(
        runner.calls()[0].args,
        ["harness", "accounts", "renew", "work", "--cli", "codex"]
    );
}

#[test]
fn a_managed_default_login_records_failure_without_overwriting_a_newer_sign_in() {
    use crate::ai::facts::CliAccounts;
    use crate::harness::accounts::{self, Auth, Cli};
    use crate::harness::codex_alab::DefaultRow;
    let now = 1_800_000_000;
    for superseded in [false, true] {
        let b = machine(&["codex"]);
        let store = Store::at(&b.state);
        let account_store = accounts::Store::at_for(&b.state, Cli::Codex, &b.home);
        let roster = b.dir.path().join("accounts-codex.toml");
        std::fs::write(
            &roster,
            accounts::append_rows_for(
                Cli::Codex,
                "",
                &[accounts::Row {
                    label: "main".into(),
                    ..accounts::Row::default()
                }],
            )
            .unwrap(),
        )
        .unwrap();
        let mut f = facts(&CODEX, b.twin("codex"));
        f.home = Ok(b.home.join(".codex"));
        f.pool = Some(CliAccounts {
            store: Some(account_store.clone()),
            roster,
            unread: true,
            next: Some("main".into()),
            default_label: Some("main".into()),
            default_row: Some(DefaultRow::SignedOut {
                label: "main".into(),
                stored: false,
            }),
            serving_label: Some("main".into()),
            serving_login: Some(("CODEX_HOME", None)),
            ..CliAccounts::default()
        });
        account_store
            .transact_logins(now, &mut None, |_, logins| {
                accounts::note_served_login(logins, "main", now);
                Ok(Vec::new())
            })
            .unwrap();
        let current_store = account_store.clone();
        let current_roster = f.pool.as_ref().unwrap().roster.clone();
        let login_home = b.home.join(".codex");
        let runner = FakeRunner::with(move |_| {
            if superseded {
                current_store
                    .transact_logins(now, &mut None, |state, logins| {
                        let raw = accounts::Roster::load_for(Cli::Codex, &current_roster).unwrap();
                        let row = raw.row("main").unwrap();
                        let context = current_store.capture_auth_context(row).unwrap();
                        accounts::stage_checked_sign_in(state, row, logins, context, now);
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            Ok(ran(1, "", "Not logged in\n"))
        })
        .exiting(move |_| {
            std::fs::create_dir_all(&login_home).unwrap();
            let token = crate::harness::codex_app_login::tests::make_jwt(&aterm_json::json!({
                "email": "fixture@example.test",
                "https://api.openai.com/auth": {"chatgpt_user_id": "fixture-user"}
            }));
            std::fs::write(
                login_home.join("auth.json"),
                aterm_json::to_vec(&aterm_json::json!({
                    "tokens": {"id_token": token, "access_token": token}
                }))
                .unwrap(),
            )
            .unwrap();
            0
        });
        assert_eq!(
            Rig::new(&[]).sign_in(&runner, &store, &f, &row(&CODEX, State::SignedOut), &[]),
            Done::SignedOut
        );
        assert_eq!(runner.calls().len(), 2);
        assert_eq!(
            account_store.load(now).unwrap().0.accounts["main"].auth,
            if superseded {
                Auth::Ok
            } else {
                Auth::SignedOut
            }
        );
        assert_eq!(
            store.load().unwrap().of_scope("codex", Some("file")),
            Record::default()
        );
    }
}

fn unbound_default_login(
    b: &crate::ai::testing::Box3,
    email: Option<&str>,
) -> (Facts, crate::harness::accounts::Store) {
    use crate::ai::facts::CliAccounts;
    use crate::harness::accounts::{self, Cli};
    use crate::harness::codex_alab::DefaultRow;
    let store = accounts::Store::at_for(&b.state, Cli::Codex, &b.home);
    let roster = b.dir.path().join("accounts-codex.toml");
    std::fs::write(
        &roster,
        accounts::append_rows_for(
            Cli::Codex,
            "",
            &[accounts::Row {
                label: "main".into(),
                email: email.map(str::to_owned),
                ..accounts::Row::default()
            }],
        )
        .unwrap(),
    )
    .unwrap();
    let mut f = facts(&CODEX, b.twin("codex"));
    f.home = Ok(b.home.join(".codex"));
    f.pool = Some(CliAccounts {
        store: Some(store.clone()),
        roster,
        unread: true,
        next: Some("main".into()),
        default_label: Some("main".into()),
        default_row: Some(DefaultRow::SignedOut {
            label: "main".into(),
            stored: false,
        }),
        serving_label: Some("main".into()),
        serving_login: Some(("CODEX_HOME", None)),
        ..CliAccounts::default()
    });
    (f, store)
}

#[test]
fn a_first_default_sign_in_checks_identity_and_supersedes_older_reviews() {
    use crate::harness::accounts::{self, Auth, Cli, Mode};
    use crate::harness::accounts_sync::{capture_current_review, record_current_review};
    let now = 1_800_000_000;
    for initial in [None, Some(Auth::Ok), Some(Auth::SignedOut)] {
        for matching in [false, true] {
            let b = machine(&["codex"]);
            let store = Store::at(&b.state);
            let (f, account_store) = unbound_default_login(&b, Some("owner@example.com"));
            if let Some(auth) = initial {
                account_store
                    .transact(now, &mut None, |state| {
                        Ok(accounts::note_auth(state, "main", auth, now - 1))
                    })
                    .unwrap();
            }
            let observed = capture_current_review(
                &account_store,
                Cli::Codex,
                Some(&f.pool.as_ref().unwrap().roster),
                Mode::Early,
                None,
                now,
            )
            .unwrap()
            .witness;
            let home = b.home.join(".codex");
            let login_home = home.clone();
            let runner = FakeRunner::with(|_| Ok(ran(0, "", "Logged in using ChatGPT\n"))).exiting(
                move |_| {
                    std::fs::create_dir_all(&login_home).unwrap();
                    let email = if matching {
                        "owner@example.com"
                    } else {
                        "other@example.com"
                    };
                    let token = crate::harness::codex_app_login::tests::make_chatgpt_jwt(
                        Some(email),
                        "plus",
                        now + 3600,
                    );
                    std::fs::write(
                        login_home.join("auth.json"),
                        aterm_json::json!({"tokens":{"access_token":token}}).to_string(),
                    )
                    .unwrap();
                    0
                },
            );
            let done =
                Rig::new(&[]).sign_in(&runner, &store, &f, &row(&CODEX, State::SignedOut), &[]);
            if matching {
                assert_eq!(done, Done::SignedIn(Some("chatgpt")));
            } else {
                assert!(matches!(done, Done::Failed(_)), "{done:?}");
            }
            assert_eq!(runner.calls().len(), 2);
            assert_eq!(
                account_store.load(now).unwrap().0.accounts["main"].auth,
                if matching { Auth::Ok } else { Auth::SignedOut }
            );
            assert_eq!(
                account_store
                    .load_logins(now)
                    .unwrap()
                    .0
                    .sign_in_receipts
                    .get("main")
                    .copied(),
                matching.then_some(1)
            );
            assert_eq!(
                store.load().unwrap().of_scope("codex", Some("file")),
                Record::default()
            );
            if matching {
                assert!(
                    record_current_review(
                        &account_store,
                        Cli::Codex,
                        &observed,
                        &home,
                        Outcome::Ended(Ended::ReviewAuth),
                        now
                    )
                    .is_err()
                );
                assert_eq!(
                    account_store.load(now).unwrap().0.accounts["main"].auth,
                    Auth::Ok
                );
            }
        }
    }
}

#[test]
fn a_default_sign_in_requires_its_account_witness_before_running() {
    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    let (f, account_store) = unbound_default_login(&b, None);
    std::fs::create_dir_all(account_store.state_file().parent().unwrap()).unwrap();
    std::fs::write(account_store.state_file(), "damaged state").unwrap();
    let runner = FakeRunner::quiet();
    let done = Rig::new(&[]).sign_in(&runner, &store, &f, &row(&CODEX, State::SignedOut), &[]);
    assert!(matches!(done, Done::Refused(_)), "{done:?}");
    assert!(runner.calls().is_empty());
}

/// ON A CODEX BUILD THAT SERVES THIS MACHINE'S OWN LOGIN'S ROW ALONE, `aterm ai login` SIGNS IN
/// NOTHING WHILE THAT ROW CAN SERVE, never the pool's next row (`work`), which that build would
/// not run.
#[test]
fn a_codex_pool_that_serves_the_default_row_alone_signs_in_nothing_while_it_serves() {
    use crate::harness::codex_alab::DefaultRow;
    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    let rig = Rig::new(&[]);
    let mut pooled = facts(&CODEX, b.twin("codex"));
    pooled.pool = Some(crate::ai::facts::CliAccounts {
        roster: PathBuf::from("/home/me/.config/aterm/accounts-codex.toml"),
        next: None,
        default_label: Some("main".to_string()),
        default_row: Some(DefaultRow::Serves("main".to_string())),
        unread: true,
        default_only: true,
        ..crate::ai::facts::CliAccounts::default()
    });
    let runner = FakeRunner::quiet();
    let done = rig.sign_in(
        &runner,
        &store,
        &pooled,
        &row(&CODEX, State::SignedOut),
        &[],
    );
    let refused = |why: &str| why.contains("`main`") && why.contains("needs no sign-in");
    assert!(
        matches!(&done, Done::Refused(why) if refused(why)),
        "{done:?}"
    );
    assert!(runner.calls().is_empty());
}

/// ON SUCH A BUILD A SIGNED-OUT ROW ON THIS MACHINE'S OWN LOGIN THAT ANOTHER MAC'S WARM SET HOLDS
/// IS NO SIGN-IN TARGET HERE: the pool names no row next, the row offers no fix, its fact says
/// why and never that Codex's own sign-in runs "once that login has ended" (it has), and
/// `aterm ai login codex` runs nothing, neither the pool's sign-in of `main` (which
/// `sign_in_plan` refuses on that build) nor Codex's own. CONTROL: with no `held_by` the same
/// row is named next and Codex's own sign-in is its fix.
#[test]
fn a_held_signed_out_default_row_on_a_build_that_ignores_codex_auth_path_signs_in_nothing() {
    use crate::ai::facts::{Machine, gather};
    use crate::ai::store::Records;
    use crate::harness::accounts::{self, Cli};
    use crate::harness::codex_alab::DefaultRow;
    let b = machine(&["codex"]);
    let launcher = b.twin("codex");
    let exe = crate::harness::codex_alab::lay_package(&b.dir.path().join("store/old"), false);
    std::fs::write(&launcher, crate::harness::codex_alab::twin_text(&exe)).expect("launcher");
    let config = b.dir.path().join("config");
    std::fs::create_dir_all(&config).expect("config");
    std::fs::create_dir_all(b.home.join(".codex")).expect("default home");
    std::fs::write(b.home.join(".codex/auth.json"), "{}").expect("default login");
    let work = b.home.join(".aterm/logins/codex/work");
    std::fs::create_dir_all(&work).expect("work");
    std::fs::write(work.join("auth.json"), "{}").expect("work login");
    let roster = |held: &str| {
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"own@example.com\"\n\
             {held}\n[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {:?}\n\
             email = \"work@example.com\"\n",
            work.display().to_string()
        )
    };
    let roster_file = config.join("accounts-codex.toml");
    std::fs::write(&roster_file, roster("held_by = \"m2\"\n")).expect("codex roster");
    let now = 1_800_000_000_i64;
    let pool_store = accounts::Store::at_for(&b.state, Cli::Codex, &b.home);
    crate::ai::testing::current_codex_auth(
        &pool_store,
        &roster_file,
        "main",
        accounts::Auth::SignedOut,
        now,
    );
    let home = b.home.display().to_string();
    let var = |k: &str| (k == "HOME").then(|| home.clone());
    let m = Machine {
        now,
        home: Some(b.home.clone()),
        var: &var,
        pkg: Some(b.pkg.clone()),
        exclude: Vec::new(),
        records: Records::default(),
        harness_state: Some(b.state.clone()),
        roster: Some(config.join("accounts.toml")),
        accounts_mode: None,
        config: None,
    };
    let store = Store::at(&b.state);
    let rig = Rig::new(&[]);

    let f = gather(&m, &CODEX);
    let pool = f.pool.as_ref().expect("codex pool");
    assert!(pool.unread && pool.default_only, "{pool:?}");
    assert!(
        matches!(&pool.default_row, Some(DefaultRow::Other { label, why })
            if label == "main" && why.contains("held for another Mac's warm set")),
        "{:?}",
        pool.default_row
    );
    assert_eq!(pool.next, None, "neither `main` nor `work`");
    assert!(!default_home_sign_in(pool));
    let r = crate::ai::state::assess(&f, true, now);
    assert_eq!(r.state, State::SignedOut, "{}", r.fact);
    assert_eq!(r.fix, None, "no fix that would refuse: {}", r.fact);
    assert!(
        r.fact.contains("held for another Mac's warm set"),
        "{}",
        r.fact
    );
    assert!(!r.fact.contains("once that login has ended"), "{}", r.fact);
    let runner = FakeRunner::quiet();
    let done = rig.sign_in(&runner, &store, &f, &r, &[]);
    assert!(
        matches!(&done, Done::Refused(why) if why.contains("held for another Mac's warm set")),
        "{done:?}"
    );
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());

    std::fs::write(&roster_file, roster("")).expect("CONTROL: `main` warm here");
    let f = gather(&m, &CODEX);
    let pool = f.pool.as_ref().expect("codex pool");
    assert_eq!(pool.next.as_deref(), Some("main"), "CONTROL");
    assert!(default_home_sign_in(pool), "CONTROL");
    let r = crate::ai::state::assess(&f, true, now);
    assert_eq!(r.fix.as_deref(), Some("aterm ai login codex"), "CONTROL");
}

/// GROK BUILD'S SIGN-IN IS A PERSON'S, AT THIS TERMINAL OR THROUGH ITS POOL: under an agent's
/// marker nothing runs; off as excluded, it is refused with that fact; with no
/// roster its own `grok login` runs on this terminal, with neither a carried login nor its home's
/// override dropped; with a roster it is the pool's sign-in of the next row on this terminal
/// (`aterm harness accounts login <label> --cli grok`), never its headless renewal, which stops
/// at accounts.x.ai's block page (CONTROL: Kimi's pool renews, in
/// `a_cli_with_its_own_roster_signs_in_through_its_renewal_and_one_without_runs_its_login`).
/// Over SSH it is `grok login --device-auth`, kept off this
/// machine's desk ([`sign_in_over_ssh_opens_no_browser_on_this_machines_desk`]). The connection
/// is the one `aterm ai login grok` names ([`crate::ai::connector::connector`]).
#[test]
fn grok_signs_in_on_this_terminal_or_through_its_pool_and_never_for_an_agent() {
    let grok = crate::ai::connector::connector("grok").expect("Grok Build is a connection");
    let b = machine(&["grok"]);
    let store = Store::at(&b.state);
    let shim = b.twin("grok");
    for marker in AGENT_MARKERS {
        let runner = FakeRunner::quiet();
        let done = Rig::new(&[(marker, "1")]).sign_in(
            &runner,
            &store,
            &facts(grok, shim.clone()),
            &row(grok, State::SignedOut),
            &[],
        );
        assert!(matches!(done, Done::Refused(_)), "{marker}: {done:?}");
        assert!(runner.calls().is_empty(), "{marker}: nothing ran");
    }
    let mut off = row(grok, State::Off);
    off.fact = "[packages] exclude names it".to_string();
    let runner = FakeRunner::quiet();
    let done = Rig::new(&[]).sign_in(&runner, &store, &facts(grok, shim.clone()), &off, &[]);
    assert_eq!(
        done,
        Done::Refused("[packages] exclude names it".to_string())
    );
    assert!(runner.calls().is_empty());
    let runner = FakeRunner::quiet();
    let done = Rig::new(&[
        ("GROK_AUTH", "{\"token\":\"t\"}"),
        ("XAI_API_KEY", "xai-test"),
        ("GROK_HOME", "/home/me/.grok-own"),
    ])
    .sign_in(
        &runner,
        &store,
        &facts(grok, shim.clone()),
        &row(grok, State::SignedOut),
        &[],
    );
    assert_eq!(done, Done::NotChecked);
    let calls = runner.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls[0].terminal && calls[0].program == "grok",
        "{:?}",
        calls[0]
    );
    assert_eq!(calls[0].args, ["login"]);
    assert!(
        calls[0]
            .env
            .iter()
            .all(|(k, _)| !["GROK_AUTH", "XAI_API_KEY", "CLAUDE_PID"].contains(&k.as_str())),
        "{:?}",
        calls[0].env
    );
    assert!(
        calls[0]
            .env
            .iter()
            .any(|(k, v)| k == "GROK_HOME" && v == "/home/me/.grok-own"),
        "{:?}",
        calls[0].env
    );
    assert!(
        calls[0]
            .env
            .iter()
            .any(|(k, v)| k == "PATH" && v == "/usr/bin:/bin"),
        "at the desk, this process's PATH: {:?}",
        calls[0].env
    );
    let mut pooled = facts(grok, shim);
    pooled.pool = Some(crate::ai::facts::CliAccounts {
        roster: PathBuf::from("/home/me/.config/aterm/accounts-grok.toml"),
        refused: None,
        next: Some("grok-two".to_string()),
        ..crate::ai::facts::CliAccounts::default()
    });
    let runner = FakeRunner::quiet();
    let done = Rig::new(&[]).sign_in(&runner, &store, &pooled, &row(grok, State::SignedOut), &[]);
    assert_eq!(done, Done::SignedIn(None));
    let calls = runner.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls[0].terminal && calls[0].program == "aterm",
        "{:?}",
        calls[0]
    );
    assert_eq!(
        calls[0].args,
        ["harness", "accounts", "login", "grok-two", "--cli", "grok"]
    );
    // A Grok roster with no row to sign in asks for a row, never an email `accounts login`
    // does not need.
    let mut empty = pooled.clone();
    empty.pool = Some(crate::ai::facts::CliAccounts {
        next: None,
        ..pooled.pool.clone().expect("pool")
    });
    let runner = FakeRunner::quiet();
    let done = Rig::new(&[]).sign_in(&runner, &store, &empty, &row(grok, State::SignedOut), &[]);
    let refused = |why: &str| why.contains("lists no row") && !why.contains("--email");
    assert!(
        matches!(done, Done::Refused(ref why) if refused(why)),
        "{done:?}"
    );
    assert!(runner.calls().is_empty());
    // The pool's `accounts login` is the person's approval too: under an agent's marker it is
    // refused and nothing runs.
    for marker in AGENT_MARKERS {
        let runner = FakeRunner::quiet();
        let done = Rig::new(&[(marker, "1")]).sign_in(
            &runner,
            &store,
            &pooled,
            &row(grok, State::SignedOut),
            &[],
        );
        assert!(
            matches!(done, Done::Refused(ref why) if why.contains("accounts login grok-two")),
            "{marker}: {done:?}"
        );
        assert!(runner.calls().is_empty(), "{marker}: nothing ran");
    }
}

/// Kimi's SSH sign-in keeps an opener-free PATH and enforces the macOS no-window sandbox.
#[test]
fn kimi_signs_in_over_ssh_with_its_own_login_and_no_opener_on_its_path() {
    let kimi = crate::ai::connector::connector("kimi").expect("Kimi Code is a connection");
    let b = machine(&["kimi"]);
    let store = Store::at(&b.state);
    let shim = b.twin("kimi");
    let ssh = ("SSH_CONNECTION", "10.0.0.2 52000 10.0.0.3 22");
    for (rig, over_ssh) in [(Rig::new(&[ssh]), true), (Rig::new(&[]), false)] {
        let runner = FakeRunner::quiet();
        let done = rig.sign_in(
            &runner,
            &store,
            &facts(kimi, shim.clone()),
            &row(kimi, State::SignedOut),
            &[],
        );
        assert_eq!(done, Done::NotChecked, "kimi has no status read");
        let calls = runner.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].terminal);
        if cfg!(target_os = "macos") && over_ssh {
            assert_eq!(calls[0].program, "sandbox-exec");
            assert_eq!(
                calls[0].args,
                vec![
                    "-p".to_string(),
                    crate::harness::login_drive::NO_DESK.to_string(),
                    shim.display().to_string(),
                    "login".to_string(),
                ]
            );
        } else {
            assert_eq!(calls[0].program, "kimi");
            assert_eq!(calls[0].args, ["login"]);
        }
        let path = calls[0]
            .env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| PathBuf::from(v))
            .expect("a PATH");
        if over_ssh {
            assert!(
                path.starts_with(store.dir()),
                "the opener-free PATH, not this process's: {path:?}"
            );
        } else {
            assert_eq!(
                path,
                Path::new("/usr/bin:/bin"),
                "CONTROL: this process's PATH"
            );
        }
    }
}

/// The terminal call a signed-out `c` makes from `rig`, and what the sign-in came to.
#[cfg(target_os = "macos")]
fn one_terminal_call(
    rig: &Rig,
    c: &'static Connector,
    b: &crate::ai::testing::Box3,
    store: &Store,
) -> (Done, Vec<crate::ai::testing::Call>) {
    let runner = FakeRunner::quiet();
    let done = rig.sign_in(
        &runner,
        store,
        &facts(c, b.twin(c.id)),
        &row(c, State::SignedOut),
        &[],
    );
    (done, runner.calls())
}

/// OVER SSH A PERSON'S `aterm ai login` (and `aterm ai doctor`, and `doctor --on`/`--fleet`,
/// which run it on each machine over `ssh -t`) OPENS NO BROWSER ON THIS MACHINE'S DESK, which
/// nobody is at: Grok Build's `grok login --device-auth` and Codex's `codex login`, each of
/// which opens the default browser through LaunchServices whatever its `PATH` (design §5 (g)),
/// run under the no-desk sandbox with the opener-free `PATH`, the CLI printing its URL for the
/// person to open where they are. Where the sandbox is missing or cannot apply, nothing runs
/// and the refusal says why. CONTROL: at the desk each runs plainly with this process's `PATH`.
#[cfg(target_os = "macos")]
#[test]
fn sign_in_over_ssh_opens_no_browser_on_this_machines_desk() {
    use crate::harness::login_drive::{NO_DESK, SANDBOX_EXEC, sandbox_refusal};
    assert_eq!(sandbox_refusal(Path::new(SANDBOX_EXEC)), None, "CONTROL");
    let ssh = ("SSH_CONNECTION", "10.0.0.2 52000 10.0.0.3 22");
    let grok = crate::ai::connector::connector("grok").expect("a connection");
    for (c, words) in [
        (grok, &["login", "--device-auth"][..]),
        (
            crate::ai::connector::connector("kimi").unwrap(),
            &["login"][..],
        ),
        (&CODEX, &["login"][..]),
    ] {
        let b = machine(&[c.id]);
        let store = Store::at(&b.state);
        let shim = b.twin(c.id);
        let (done, calls) = one_terminal_call(&Rig::new(&[ssh]), c, &b, &store);
        assert!(
            matches!(done, Done::NotChecked | Done::SignedOut),
            "{}: {done:?}",
            c.id
        );
        let terminal: Vec<_> = calls.iter().filter(|call| call.terminal).collect();
        assert_eq!(terminal.len(), 1, "{}: {calls:?}", c.id);
        assert_eq!(terminal[0].program, "sandbox-exec", "{}", c.id);
        let mut want = vec![
            "-p".to_string(),
            NO_DESK.to_string(),
            shim.display().to_string(),
        ];
        want.extend(words.iter().map(|w| (*w).to_string()));
        assert_eq!(terminal[0].args, want, "{}", c.id);
        let path = terminal[0]
            .env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| PathBuf::from(v))
            .expect("a PATH");
        assert!(
            path.starts_with(store.dir()) && path != Path::new("/usr/bin:/bin"),
            "{}: the opener-free PATH, not this process's: {path:?}",
            c.id
        );

        let missing = b.dir.path().join("no-sandbox-exec");
        for (sandbox, says) in [
            (None, "is not here"),
            (Some(missing), "is not here"),
            (Some(PathBuf::from("/usr/bin/false")), "cannot run here"),
        ] {
            let (done, calls) =
                one_terminal_call(&Rig::new(&[ssh]).sandbox(sandbox.clone()), c, &b, &store);
            assert!(
                matches!(done, Done::Refused(ref why)
                    if why.starts_with("over SSH, ")
                        && why.contains("would open this machine's default browser")
                        && why.contains(says)),
                "{} {sandbox:?}: {done:?}",
                c.id
            );
            assert!(calls.is_empty(), "{} {sandbox:?}: nothing ran", c.id);
        }

        let (_, calls) = one_terminal_call(&Rig::new(&[]), c, &b, &store);
        let terminal: Vec<_> = calls.iter().filter(|call| call.terminal).collect();
        assert_eq!(terminal.len(), 1, "CONTROL {}: {calls:?}", c.id);
        assert_eq!(terminal[0].program, c.id, "CONTROL: at the desk, plainly");
        assert_eq!(terminal[0].args, ["login"], "CONTROL {}", c.id);
        assert!(
            terminal[0]
                .env
                .iter()
                .any(|(k, v)| k == "PATH" && v == "/usr/bin:/bin"),
            "CONTROL {}: this process's PATH",
            c.id
        );
    }
}

/// An edit a case makes to the listed accounts view.
type EditView<'a> = &'a dyn Fn(&mut ClaudeAccounts);

/// WITH NOBODY AT THIS RUN ATERM STARTS ONLY A SIGN-IN THAT RUNS UNATTENDED (findings of
/// 2026-10-28 on the m2 state): a Claude row no route signs in unattended is refused, naming
/// the step only a person gives, and nothing runs, under an agent's marker with a terminal and
/// with no terminal alike; so is a row while a switch keeps aterm's sign-ins off, and a row the
/// window's upkeep holds after a failed sign-in on record. A renewal that ran and exited non-zero
/// (as `accounts renew <label>` does when it ran nothing) is a failure, never a sign-in.
/// CONTROL: a person at the terminal runs the renewal there.
#[test]
fn with_nobody_here_aterm_starts_only_a_sign_in_that_runs_unattended() {
    let b = machine(&["claude"]);
    let store = Store::at(&b.state);
    let shim = b.twin("claude");
    let with = |edit: &dyn Fn(&mut ClaudeAccounts)| {
        let mut f = facts(&CLAUDE, shim.clone());
        let mut view = listed_view(true, None);
        edit(&mut view);
        f.accounts = Some(view);
        f
    };
    let nobody = [Rig::new(&[("CLAUDECODE", "1")]), Rig::new(&[]).no_tty()];
    let cases: [(&str, EditView<'_>, &str); 3] = [
        (
            "no route",
            &|v| v.unattended = false,
            "aterm harness accounts browser work",
        ),
        (
            "sign-ins off",
            &|v| v.off = Some("`[harness] logins = \"off\"`".into()),
            "aterm ai login claude",
        ),
        (
            "held",
            &|v| {
                v.tried = Some(crate::ai::facts::Tried {
                    outcome: "failed",
                    routes: "profile: the sign-in did not finish; profile(ready)".into(),
                    since: 1_800_000_000 - 600,
                });
            },
            "upkeep",
        ),
    ];
    for (case, edit, names) in cases {
        let f = with(edit);
        for rig in &nobody {
            let runner = FakeRunner::quiet();
            let done = rig.sign_in(&runner, &store, &f, &row(&CLAUDE, State::SignedOut), &[]);
            assert!(
                matches!(&done, Done::Refused(why) if why.contains(names)),
                "{case}: {done:?}"
            );
            assert!(runner.calls().is_empty(), "{case}: {:?}", runner.calls());
        }
        let runner = FakeRunner::quiet();
        let done = Rig::new(&[]).sign_in(&runner, &store, &f, &row(&CLAUDE, State::SignedOut), &[]);
        assert_eq!(done, Done::SignedIn(None), "CONTROL {case}");
        assert!(
            runner.calls().iter().all(|c| c.terminal),
            "CONTROL {case}: {:?}",
            runner.calls()
        );
    }

    let ran_nothing = FakeRunner::with(|_| Ok(ran(1, "due here: work\n", "not run\n")));
    let done = Rig::new(&[]).no_tty().sign_in(
        &ran_nothing,
        &store,
        &with(&|_| {}),
        &row(&CLAUDE, State::SignedOut),
        &[],
    );
    assert!(matches!(done, Done::Failed(_)), "{done:?}");
    let ran_nothing = FakeRunner::quiet().exiting(|_| 1);
    let done = Rig::new(&[]).sign_in(
        &ran_nothing,
        &store,
        &with(&|_| {}),
        &row(&CLAUDE, State::SignedOut),
        &[],
    );
    assert!(matches!(done, Done::Failed(_)), "{done:?}");
}

/// WITH NOBODY AT THIS RUN A POOL'S RENEWAL WAITS ON WHAT HOLDS IT: Cloudflare's bot check on
/// record within the upkeep's hold, or a stop for a person (whose step the refusal names);
/// nothing runs. CONTROL: past the hold, the renewal runs with stdin closed.
#[test]
fn with_nobody_here_a_pools_renewal_waits_on_a_bot_check_or_a_persons_step() {
    use crate::ai::connector::KIMI;
    let b = machine(&["kimi"]);
    let store = Store::at(&b.state);
    let hold = crate::harness::accounts_sync::PERSON_BACKOFF_SECS;
    let mut pooled = facts(&KIMI, b.twin("kimi"));
    let pool = crate::ai::facts::CliAccounts {
        roster: PathBuf::from("/home/me/.config/aterm/accounts-kimi.toml"),
        next: Some("kimi-one".to_string()),
        ..crate::ai::facts::CliAccounts::default()
    };
    let agent = Rig::new(&[("CODEX_THREAD_ID", "t")]);
    for (case, edit, names) in [
        (
            "bot check",
            crate::ai::facts::CliAccounts {
                bot_check_at: Some(1_800_000_000 - 60),
                ..pool.clone()
            },
            "bot check",
        ),
        (
            "a person's step",
            crate::ai::facts::CliAccounts {
                person_task: Some(crate::harness::accounts::SignInAction::Browser),
                ..pool.clone()
            },
            "aterm harness accounts browser kimi-one --cli kimi",
        ),
    ] {
        pooled.pool = Some(edit);
        let runner = FakeRunner::quiet();
        let done = agent.sign_in(&runner, &store, &pooled, &row(&KIMI, State::SignedOut), &[]);
        assert!(
            matches!(&done, Done::Refused(why) if why.contains(names)),
            "{case}: {done:?}"
        );
        assert!(runner.calls().is_empty(), "{case}");
    }
    pooled.pool = Some(crate::ai::facts::CliAccounts {
        bot_check_at: Some(1_800_000_000 - hold),
        ..pool
    });
    let runner = FakeRunner::quiet();
    let done = agent.sign_in(&runner, &store, &pooled, &row(&KIMI, State::SignedOut), &[]);
    assert_eq!(done, Done::SignedIn(None), "CONTROL");
    assert!(!runner.calls()[0].terminal, "CONTROL");
}

/// A SIGN-IN THAT EXITS NONZERO SIGNED NOTHING IN, WHATEVER A STATUS READ AFTER IT FINDS: Codex's
/// login that fails and leaves an older login says that exit, and records no sign-in; one the status
/// shows gone says the exit too. CONTROL: exit 0 with the status signed in is a sign-in.
#[test]
fn a_sign_in_that_exits_nonzero_signs_nothing_in() {
    for status in ["Logged in using ChatGPT\n", "Not logged in\n"] {
        let b = machine(&["codex"]);
        let store = Store::at(&b.state);
        let shim = b.pkg.agents_dir().join("codex");
        let rig = Rig::new(&[]);
        let answer = status.to_string();
        let runner = FakeRunner::with(move |_| {
            Ok(ran(i32::from(!answer.starts_with("Logged")), "", &answer))
        })
        .exiting(|_| 3);
        let done = rig.sign_in(
            &runner,
            &store,
            &facts(&CODEX, shim),
            &row(&CODEX, State::SignedOut),
            &[],
        );
        assert_eq!(done, Done::Failed("codex exited 3".to_string()), "{status}");
        assert_eq!(
            store
                .load()
                .expect("load")
                .of_scope("codex", Some("file"))
                .used_how,
            None,
            "{status}: no sign-in is on record"
        );
    }
    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    let runner = FakeRunner::with(|_| Ok(ran(0, "", "Logged in using ChatGPT\n")));
    let done = Rig::new(&[]).sign_in(
        &runner,
        &store,
        &facts(&CODEX, b.pkg.agents_dir().join("codex")),
        &row(&CODEX, State::SignedOut),
        &[],
    );
    assert_eq!(done, Done::SignedIn(Some("chatgpt")), "CONTROL");
}

/// A SIGN-IN THAT IS THE CLI ITSELF ENDS WHEN THE PERSON QUITS IT, AND A QUIT'S EXIT SAYS NOTHING OF
/// THE SIGN-IN: `agy` is opened, signed in and quit, which can exit nonzero, and with no status to
/// read the result is that nothing reads whether it worked. CONTROL: a `login` subcommand with no
/// status to read (Muse's) that exits nonzero did not sign in.
#[test]
fn a_sign_in_that_is_the_cli_itself_says_nothing_by_its_exit() {
    use crate::ai::connector::{AGY, MUSE};
    for (c, want) in [
        (&AGY, Done::NotChecked),
        (&MUSE, Done::Failed("muse exited 130".to_string())),
    ] {
        let b = machine(&[c.id]);
        let store = Store::at(&b.state);
        let shim = b.pkg.agents_dir().join(c.id);
        let runner = FakeRunner::with(|_| Ok(ran(0, "", ""))).exiting(|_| 130);
        let done = Rig::new(&[]).sign_in(
            &runner,
            &store,
            &facts(c, shim),
            &row(c, State::SignedOut),
            &[],
        );
        assert_eq!(done, want, "{}", c.id);
    }
}
