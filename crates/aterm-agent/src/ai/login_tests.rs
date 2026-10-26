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
        pool: None,
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
    no_desk: Option<PathBuf>,
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
        }
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
        next_sign_in: None,
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
        next_sign_in: None,
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

#[test]
fn claude_limited_with_next_sign_in_renews_the_successor_account() {
    let b = machine(&["claude"]);
    let store = Store::at(&b.state);
    let shim = b.pkg.agents_dir().join("claude");
    let rig = Rig::new(&[]);
    let mut f = facts(&CLAUDE, shim);
    f.accounts = Some(ClaudeAccounts {
        label: "m27-3".into(),
        listed: true,
        refused: None,
        renews: true,
        login: None,
        made: true,
        state: Some("ok"),
        renew_by: None,
        limited_until: Some(1_800_086_400),
        served_at: None,
        renewed_at: None,
        next_sign_in: Some("m27".into()),
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
    let shim = b.pkg.agents_dir().join("kimi");
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

/// GROK BUILD'S SIGN-IN IS A PERSON'S, AT THIS TERMINAL OR THROUGH ITS POOL: under an agent's
/// marker nothing runs; off as an opt-in nobody installed, it is refused with that fact; with no
/// roster its own `grok login` runs on this terminal, with neither a carried login nor its home's
/// override dropped; with a roster it is the pool's renewal of the next row (`aterm harness
/// accounts renew <label> --cli grok`). Over SSH it is `grok login --device-auth`, kept off this
/// machine's desk ([`sign_in_over_ssh_opens_no_browser_on_this_machines_desk`]). The connection
/// is the one `aterm ai login grok` names ([`crate::ai::connector::connector`]).
#[test]
fn grok_signs_in_on_this_terminal_or_through_its_pool_and_never_for_an_agent() {
    let grok = crate::ai::connector::connector("grok").expect("Grok Build is a connection");
    let b = machine(&["grok"]);
    let store = Store::at(&b.state);
    let shim = b.pkg.agents_dir().join("grok");
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
    off.fact = "not installed: an opt-in program".to_string();
    let runner = FakeRunner::quiet();
    let done = Rig::new(&[]).sign_in(&runner, &store, &facts(grok, shim.clone()), &off, &[]);
    assert_eq!(
        done,
        Done::Refused("not installed: an opt-in program".to_string())
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
        ["harness", "accounts", "renew", "grok-two", "--cli", "grok"]
    );
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
        &facts(c, b.pkg.agents_dir().join(c.id)),
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
        (&CODEX, &["login"][..]),
    ] {
        let b = machine(&[c.id]);
        let store = Store::at(&b.state);
        let shim = b.pkg.agents_dir().join(c.id);
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
