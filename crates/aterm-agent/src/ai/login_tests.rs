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
        home_exists: true,
        login_file: None,
        hidden_store: false,
        profile_signed_in: false,
        plan: None,
        accounts: None,
        limited_until: None,
        record: Record::default(),
        env_credentials: Vec::new(),
    }
}

fn row(c: &'static Connector, state: State) -> Row {
    Row {
        connector: c,
        wanted: true,
        state,
        mark: Mark::Fail,
        fact: String::new(),
        fix: None,
        version: None,
        plan: None,
        used: None,
        renew_by: None,
        limited_until: None,
    }
}

struct Rig {
    vars: Vec<(String, String)>,
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
        Rig { vars }
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
        };
        sign_in(&host, f, r, &mut Vec::new())
    }
}

#[test]
fn an_agent_never_starts_a_sign_in() {
    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    let runner = FakeRunner::quiet();
    for marker in AGENT_MARKERS {
        let done = Rig::new(&[(marker, "1")]).sign_in(
            &runner,
            &store,
            &facts(&CODEX, b.pkg.agents_dir().join("codex")),
            &row(&CODEX, State::SignedOut),
            &[],
        );
        assert!(matches!(done, Done::Refused(_)), "{marker}: {done:?}");
    }
    assert!(runner.calls().is_empty(), "nothing ran under an agent");
}

#[test]
fn codexs_login_runs_only_when_codex_is_signed_out_because_it_revokes_first() {
    let b = machine(&["codex"]);
    let store = Store::at(&b.state);
    let shim = b.pkg.agents_dir().join("codex");
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
    // The control: signed out, it runs, then reads its status on the login it made.
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
        store.load().expect("load").of("codex").used_how,
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
    let shim = b.pkg.agents_dir().join("claude");
    let rig = Rig::new(&[]);
    // No renewable row: the email is asked for, the own login listed, then renewed.
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
    // Nothing of Claude's own ran: no `claude auth login`, no `auth status`.
    assert!(calls.iter().all(|c| c.program != "claude"));
    // A renewable row renews at once, under its label.
    let mut f = facts(&CLAUDE, shim.clone());
    f.accounts = Some(ClaudeAccounts {
        label: "m7".into(),
        listed: false,
        refused: None,
        renews: true,
        login: None,
        made: true,
        state: Some("due"),
        renew_by: None,
        limited_until: None,
        served_at: None,
        renewed_at: None,
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
    // No email given: nothing runs.
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
        label: "work".into(),
        listed: true,
        refused: refused.map(str::to_string),
        renews,
        login: None,
        made: false,
        state: None,
        renew_by: None,
        limited_until: None,
        served_at: None,
        renewed_at: None,
    }
}

#[test]
fn claude_never_adopts_over_a_listed_row_or_a_refused_roster() {
    let b = machine(&["claude"]);
    let store = Store::at(&b.state);
    let shim = b.pkg.agents_dir().join("claude");
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
        &facts(&CLAUDE, b.pkg.agents_dir().join("claude")),
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
    let done = rig.sign_in(
        &runner,
        &store,
        &facts(&CODEX, b.pkg.agents_dir().join("codex")),
        &row(&CODEX, State::SignedOut),
        &[],
    );
    assert_eq!(done, Done::SignedOut);
    assert_eq!(store.load().expect("load").of("codex"), Record::default());
    // The control: on the default home the same answer is recorded.
    let done = Rig::new(&[]).sign_in(
        &runner,
        &store,
        &facts(&CODEX, b.pkg.agents_dir().join("codex")),
        &row(&CODEX, State::SignedOut),
        &[],
    );
    assert_eq!(done, Done::SignedOut);
    assert!(store.load().expect("load").of("codex").ended());
}

#[test]
fn claude_under_a_home_of_its_own_is_never_signed_in_here() {
    let b = machine(&["claude"]);
    let store = Store::at(&b.state);
    let runner = FakeRunner::quiet();
    let rig = Rig::new(&[("CLAUDE_CONFIG_DIR", "/me/.config/claude")]);
    let done = rig.sign_in(
        &runner,
        &store,
        &facts(&CLAUDE, b.pkg.agents_dir().join("claude")),
        &row(&CLAUDE, State::SignedOut),
        &["me@example.com"],
    );
    assert!(
        matches!(done, Done::Refused(ref why) if why.contains("CLAUDE_CONFIG_DIR")),
        "{done:?}"
    );
    assert!(runner.calls().is_empty());
}
