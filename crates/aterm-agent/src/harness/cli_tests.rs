// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the `aterm harness` command: the four read views, the retired
//! hook-bridge tombstones, the refusal of the verbs deleted with the second
//! harness stack, and the `accounts` verbs.
//!
//! Every path and clock is injected through [`Env`]; the one verb that opens
//! a control connection (`limits`) is driven through a scripted [`Ctl`], and
//! the one `accounts` verb that runs `claude` (`login`: its flow, then one
//! `auth status` after a flow that exits 0) through a scripted
//! [`AccountsHost`], its clock after a login flow included: no verb here
//! runs a `claude`. The two runners behind that host are tested on their
//! own, each over a `/bin/sh` stand-in written under a temp dir. Every
//! account label is invented (`main`, `work-2`, `pool-a`) and every login
//! directory is under a temp dir.

use aterm_phase::Program;
use aterm_phase::prompt::fixtures::{composer, rows};
use aterm_tempfile::TempDir as Tmp;

use super::*;
use crate::harness::host_path::{self, abs};
use crate::supervise::run::CtlReply;

/// A fresh, empty directory under the system temp dir, removed on drop.
fn tmp_dir(tag: &str) -> Tmp {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-harness-cli-{tag}-"))
        .tempdir()
        .expect("temp dir")
}

/// An [`Env`] rooted in `tmp`, with every clock and path injected, including a
/// control socket no instance listens on (so `--status` never asks a live
/// window).
fn env_at(tmp: &Tmp) -> Env {
    Env {
        state: tmp.path().join("state"),
        aterm_state: Some(tmp.path().join("aterm-state")),
        cwd: tmp.path().join("work"),
        home: Some(tmp.path().join("home")),
        claude_dir: None,
        claude_config_unusable: None,
        claude_profile: None,
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        codex_home: None,
        now: 1_758_412_800, // 2025-09-21T00:00:00Z — fixed, never `now()`.
        sid: "s-test".to_string(),
        sock: Some(tmp.path().join("no.sock").to_string_lossy().into_owned()),
        config: Some(tmp.path().join("aterm.toml")),
        claude_tmp: Some(tmp.path().join("claude-tmp")),
        // The test's universe: no process names another Claude Code directory.
        claude_running: Some(Ok(disk::discover::Running::default())),
    }
}

/// Run one command and answer `(exit-is-zero, stdout, stderr)`.
fn go(cmd: &Cmd, env: &Env) -> (bool, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(cmd, env, &mut out, &mut err);
    (
        format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS),
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

fn words(s: &str) -> Vec<String> {
    s.split(' ').map(str::to_string).collect()
}

// ---------------------------------------------------------------------------
// The grammar
// ---------------------------------------------------------------------------

/// Every form the help text advertises parses to the command it names.
#[test]
fn the_grammar_parses_what_the_help_text_advertises() {
    let cases: Vec<(&str, Cmd)> = vec![
        ("usage", Cmd::Usage { json: false }),
        ("usage --json", Cmd::Usage { json: true }),
        (
            "limits",
            Cmd::Limits {
                sid: None,
                json: false,
            },
        ),
        (
            "limits @s-abc --json",
            Cmd::Limits {
                sid: Some("@s-abc".to_string()),
                json: true,
            },
        ),
        (
            "disk",
            Cmd::Disk {
                targets: Vec::new(),
                apply: None,
                json: false,
            },
        ),
        (
            "disk /a/target --apply cargo-targets --json",
            Cmd::Disk {
                targets: vec![PathBuf::from("/a/target")],
                apply: Some(disk::Class::CargoTargets),
                json: true,
            },
        ),
        (
            "ledger",
            Cmd::Ledger {
                file: LedgerFile::Approvals(None),
                count: LEDGER_DEFAULT_ROWS,
                json: false,
            },
        ),
        (
            "ledger @s-abc 5 --json",
            Cmd::Ledger {
                file: LedgerFile::Approvals(Some("@s-abc".to_string())),
                count: 5,
                json: true,
            },
        ),
        (
            "ledger disk 3",
            Cmd::Ledger {
                file: LedgerFile::Disk,
                count: 3,
                json: false,
            },
        ),
        (
            "upgrade",
            Cmd::Upgrade {
                sid: String::new(),
                dry_run: false,
                json: false,
            },
        ),
        (
            "upgrade s-abc --dry-run --json",
            Cmd::Upgrade {
                sid: "s-abc".to_string(),
                dry_run: true,
                json: true,
            },
        ),
        (
            "ledger accounts 7 --json",
            Cmd::Ledger {
                file: LedgerFile::Accounts,
                count: 7,
                json: true,
            },
        ),
        (
            "accounts",
            Cmd::Accounts(AccountsCmd::Report { json: false }),
        ),
        (
            "accounts --json",
            Cmd::Accounts(AccountsCmd::Report { json: true }),
        ),
        (
            "accounts stats",
            Cmd::Accounts(AccountsCmd::Stats { json: false }),
        ),
        (
            "accounts stats --json",
            Cmd::Accounts(AccountsCmd::Stats { json: true }),
        ),
        (
            "accounts add work-2",
            Cmd::Accounts(AccountsCmd::Add {
                label: "work-2".to_string(),
                class: Class::Dedicated,
                email: None,
                browser: None,
            }),
        ),
        (
            "accounts add pool-a --shared",
            Cmd::Accounts(AccountsCmd::Add {
                label: "pool-a".to_string(),
                class: Class::Shared,
                email: None,
                browser: None,
            }),
        ),
        (
            "accounts login work-2",
            Cmd::Accounts(AccountsCmd::Login {
                label: "work-2".to_string(),
            }),
        ),
        (
            "accounts use work-2",
            Cmd::Accounts(AccountsCmd::Use {
                label: "work-2".to_string(),
            }),
        ),
        ("accounts use --auto", Cmd::Accounts(AccountsCmd::Auto)),
        // `help` is this command's help word wherever it stands, so it is
        // never taken as a label: `add help` adds nothing.
        // The page it prints is the verb's own.
        ("accounts add help", Cmd::Help(HelpPage::Accounts)),
        ("accounts --help", Cmd::Help(HelpPage::Accounts)),
        ("upgrade models", Cmd::UpgradeModels { set: None }),
        (
            "upgrade models set claude-opus-5-5,claude-fable-5-1",
            Cmd::UpgradeModels {
                set: Some("claude-opus-5-5,claude-fable-5-1".to_string()),
            },
        ),
        // The owner's word: `ledger upgrade`, `upgrade [<sid>] --status`,
        // `upgrade <sid> --now|--defer <dur>|--skip`.
        (
            "ledger upgrade 5 --json",
            Cmd::Ledger {
                file: LedgerFile::Upgrade,
                count: 5,
                json: true,
            },
        ),
        (
            "upgrade --status",
            Cmd::UpgradeOwner {
                sid: String::new(),
                ask: OwnerAsk::Status,
                json: false,
            },
        ),
        (
            "upgrade s-abc --status --json",
            Cmd::UpgradeOwner {
                sid: "s-abc".to_string(),
                ask: OwnerAsk::Status,
                json: true,
            },
        ),
        (
            "upgrade s-abc --now",
            Cmd::UpgradeOwner {
                sid: "s-abc".to_string(),
                ask: OwnerAsk::Now,
                json: false,
            },
        ),
        (
            "upgrade s-abc --defer 6h",
            Cmd::UpgradeOwner {
                sid: "s-abc".to_string(),
                ask: OwnerAsk::Defer(6 * 3_600),
                json: false,
            },
        ),
        (
            "upgrade --skip s-abc",
            Cmd::UpgradeOwner {
                sid: "s-abc".to_string(),
                ask: OwnerAsk::Skip,
                json: false,
            },
        ),
        ("--help", Cmd::Help(HelpPage::Verbs)),
        ("disk --help", Cmd::Help(HelpPage::Disk)),
        ("upgrade s-1 --help", Cmd::Help(HelpPage::Upgrade)),
        ("upgrade --status -h", Cmd::Help(HelpPage::UpgradeStatus)),
    ];
    for (line, want) in cases {
        let (got, _) = parse(&words(line)).unwrap_or_else(|e| panic!("{line}: {e}"));
        assert_eq!(got, want, "{line}");
    }
    assert_eq!(parse(&[]).expect("empty").0, Cmd::Help(HelpPage::Verbs));
    let (_, over) = parse(&words("usage --state /s --config /c.toml --sock /a.sock"))
        .expect("the environment flags parse");
    assert_eq!(over.state, Some(PathBuf::from("/s")));
    assert_eq!(over.config, Some(PathBuf::from("/c.toml")));
    assert_eq!(over.sock.as_deref(), Some("/a.sock"));
}

/// The limit reset's lines of `accounts --help` are the code's own word, not
/// a copy kept by hand (a hand-kept page named 2.1.287 alone after 2.1.304's
/// texts were read, and none of 2.1.304's kinds): the page carries them,
/// every build a `reset.` anchor was read in is named there, and each kind's
/// verdict under the automatic-reset policy shows, flipping one kind's
/// verdict changing the lines. Negative controls: a build only other anchors
/// were read in is not named, and a `reset.` row of a build no anchor has is
/// named once the table holds it.
#[test]
fn the_accounts_page_says_what_the_limit_reset_reads_and_takes() {
    let anchors = aterm_phase::ANCHORS;
    let lines = help_reset(ResetKind::takeable, anchors);
    assert!(
        help_page(HelpPage::Accounts).contains(&lines),
        "the page carries the reset's lines"
    );
    let reset_builds: BTreeSet<&str> = anchors
        .iter()
        .filter(|a| a.id.starts_with("reset."))
        .map(|a| a.since)
        .collect();
    assert!(
        !reset_builds.is_empty(),
        "the reset's texts were read somewhere"
    );
    for build in &reset_builds {
        assert!(lines.contains(&format!("{build}'s")), "{build} is named");
    }
    let other = anchors
        .iter()
        .map(|a| a.since)
        .find(|b| !reset_builds.contains(b))
        .expect("a build only other anchors were read in");
    assert!(
        !lines.contains(&format!("{other}'s")),
        "{other} read no reset text"
    );
    let later = Anchor {
        id: "reset.test.later",
        text: "a later build's text",
        kind: aterm_phase::anchors::AnchorKind::Literal,
        since: "9.9.9",
    };
    let widened: Vec<Anchor> = anchors.iter().copied().chain([later]).collect();
    assert!(!lines.contains("9.9.9's"));
    assert!(help_reset(ResetKind::takeable, &widened).contains("9.9.9's"));
    for kind in ResetKind::ALL {
        assert!(lines.contains(kind.name()), "{kind:?} is named");
        let flipped = help_reset(|k| k.takeable() != (k == kind), anchors);
        assert_ne!(flipped, lines, "{kind:?}'s verdict shows on the page");
    }
    assert_eq!(
        help_reset(ResetKind::takeable, anchors),
        lines,
        "the same verdicts and table give the same lines"
    );
}

/// What it does not know is refused, and the hook-era ring names are gone.
#[test]
fn the_grammar_refuses_what_it_does_not_know() {
    for line in [
        "frobnicate",
        "usage extra",
        "limits s-abc",
        "limits @a @b",
        "ledger rm",
        "ledger recovery",
        "upgrade models set",
        "upgrade models frob",
        "upgrade models set a b",
        "ledger statusline",
        "disk --apply everything",
        "usage --bogus",
        "usage --utc-offset 3600",
        "usage --state",
        "upgrade abc",
        // No loop: the window's host takes the steps, on a push.
        "upgrade --every 60",
        // accounts: an unknown form, a missing or extra operand. `check` is
        // no form: no verb runs `claude auth status` under an idle login.
        "accounts frob",
        "accounts discover",
        "accounts check",
        "accounts add",
        "accounts add work-2 extra",
        "accounts login",
        "accounts use",
        // One of a label and --auto.
        "accounts use work-2 --auto",
        // --shared is `accounts add`'s and --auto is `accounts use`'s, as the
        // owner's flags are `upgrade`'s.
        "accounts --shared",
        "accounts login work-2 --shared",
        "accounts --auto",
        "accounts login work-2 --auto",
        "usage --shared",
        "upgrade --auto",
        // --json is the report's form.
        "accounts add work-2 --json",
        "accounts login work-2 --json",
        "accounts use work-2 --json",
        "ledger account",
        // --dry-run and --sock are not `accounts`': a flag the verb does
        // not take is refused, never swallowed by a verb that then writes.
        "accounts --dry-run",
        "accounts --json --dry-run",
        "accounts stats --dry-run",
        "accounts stats --shared",
        "accounts stats --sock /a.sock",
        "accounts stats main",
        "accounts add work-2 --dry-run",
        "accounts add pool-a --shared --dry-run",
        "accounts login work-2 --dry-run",
        "accounts use work-2 --dry-run",
        "accounts use --auto --dry-run",
        "accounts --sock /a.sock",
        "accounts add work-2 --sock /a.sock",
        "accounts login work-2 --sock /a.sock",
        "accounts use work-2 --sock /a.sock",
        "accounts use --auto --sock /a.sock",
        // The owner's word names one tab, one word, and a span within a month.
        "upgrade --now",
        "upgrade --defer 1h",
        "upgrade s-abc --now --skip",
        "upgrade s-abc --status --now",
        "upgrade s-abc --now --dry-run",
        // An owner's word is refused with another verb's flag, never taken
        // with the flag ignored.
        "upgrade --status --shared",
        "upgrade s-abc --skip --auto",
        "upgrade s-abc --now --email owner@example.com",
        "upgrade s-abc --defer 1h --browser Work",
        "upgrade s-abc --skip --due",
        "upgrade s-abc --now --here",
        "upgrade --status --on other-host",
        "upgrade s-abc --skip --every 30",
        "upgrade s-abc --defer",
        "upgrade s-abc --defer 31d",
        "upgrade s-abc s-def --now",
        "usage --now",
        "ledger --status",
    ] {
        assert!(parse(&words(line)).is_err(), "{line} should be refused");
    }
    // The front door's exit for it is 2, before the environment is read or
    // anything is written.
    for line in [
        "accounts use work-2 --dry-run",
        "accounts login work-2 --sock /a.sock",
    ] {
        let argv = words(line).into_iter().map(OsString::from).collect();
        assert_eq!(format!("{:?}", main_entry(argv)), exit(2), "{line}");
    }
    // NEGATIVE CONTROL: where the two flags belong they still parse.
    assert!(matches!(
        parse(&words("upgrade s-abc --dry-run"))
            .expect("the sweep")
            .0,
        Cmd::Upgrade { dry_run: true, .. }
    ));
    assert!(parse(&words("limits --sock /a.sock")).is_ok());
}

// ---------------------------------------------------------------------------
// hook / statusline: retired, silent
// ---------------------------------------------------------------------------

/// From a pipe, `hook` and `statusline` read the payload to the end, print
/// nothing and exit 0. At a terminal they read nothing (a panicking reader
/// proves it), print a one-line retirement note, and exit 0.
#[test]
fn the_retired_hook_verbs_drain_a_pipe_and_never_wait_on_a_terminal() {
    struct NeverRead;
    impl io::Read for NeverRead {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("a terminal stdin must not be read");
        }
    }
    for verb in ["hook", "statusline"] {
        // Over 1 MiB: a `PostToolUse` for a Write carries the whole file, and a
        // drain that stopped short would close the pipe on the writer.
        let len = (1 << 20) * 3 + 17;
        let mut pipe = io::Cursor::new(vec![b'x'; len]);
        let mut err = Vec::new();
        let code = answer_retired(verb, false, &mut pipe, &mut err);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
        assert_eq!(
            pipe.position(),
            len as u64,
            "{verb}: the payload is drained"
        );
        assert!(err.is_empty(), "{verb}: {}", String::from_utf8_lossy(&err));

        let mut err = Vec::new();
        let code = answer_retired(verb, true, &mut NeverRead, &mut err);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
        let err = String::from_utf8_lossy(&err);
        assert_eq!(err.lines().count(), 1, "{verb}: {err}");
        assert!(err.contains("retired"), "{verb}: {err}");
    }
}

// ---------------------------------------------------------------------------
// usage
// ---------------------------------------------------------------------------

/// A relative `CLAUDE_CONFIG_DIR` is relative to Claude Code's working
/// directory, which this command cannot know: `usage` folds nothing and says
/// why rather than folding another configuration's transcript. Control: unset,
/// the `~/.claude` transcript folds.
#[test]
fn usage_with_a_relative_claude_config_dir_folds_nothing_and_says_so() {
    let tmp = tmp_dir("usage-relative-config");
    let mut env = env_at(&tmp);
    let dir = env
        .transcripts_root()
        .expect("home is injected")
        .join(footer::project_slug(&env.cwd));
    std::fs::create_dir_all(&dir).expect("project dir");
    std::fs::write(
        dir.join("other-config.jsonl"),
        r#"{"type":"assistant","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":1,"output_tokens":5}}}
"#,
    )
    .expect("transcript");
    let (_, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(out.contains("other-config.jsonl"), "control: {out}");

    env.claude_config_unusable = Some("cfg".to_owned());
    let (ok, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(ok);
    assert!(out.contains("source=none"), "{out}");
    assert!(!out.contains("other-config.jsonl"), "{out}");
    let (_, json, _) = go(&Cmd::Usage { json: true }, &env);
    let doc: Value = aterm_json::from_str(&json).expect("json");
    assert_eq!(doc["transcript_pick"].as_str(), Some("none"), "{json}");
    assert_eq!(
        doc["claude_config_dir_unusable"].as_str(),
        Some("cfg"),
        "{json}"
    );
}

/// Run from a Claude Code session, `usage` folds that session's transcript
/// (named by `CLAUDE_CODE_SESSION_ID`, or `CLAUDE_PID`'s sessions file, under
/// `CLAUDE_CONFIG_DIR`) and its subagents', never a newer neighbour's. Named
/// but not yet written folds nothing, as no transcript at all does
/// (`source=none`, never a row of zeros). Negative control: without a name the
/// newest folds, labelled so. The JSON only adds keys to schema 1.
#[test]
fn usage_folds_the_session_claude_code_names_not_the_newest() {
    let tmp = tmp_dir("usage-named");
    let mut env = env_at(&tmp);
    // A Claude directory of its own (`CLAUDE_CONFIG_DIR`), not `~/.claude`.
    env.claude_dir = Some(tmp.path().join("claude-config"));
    let dir = env
        .transcripts_root()
        .expect("a claude dir")
        .join(footer::project_slug(&env.cwd));
    assert!(dir.starts_with(tmp.path().join("claude-config")), "{dir:?}");
    let (ok, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(ok);
    assert!(
        out.contains("source=none"),
        "no transcript, no row of zeros: {out}"
    );
    std::fs::create_dir_all(dir.join("mine-1").join("subagents")).expect("project dir");
    let row = |id: &str, model: &str, output: u64| {
        format!(
            "{{\"type\":\"assistant\",\"message\":{{\"id\":\"{id}\",\"model\":\"{model}\",\"usage\":{{\"input_tokens\":1,\"output_tokens\":{output}}}}}}}\n"
        )
    };
    std::fs::write(dir.join("mine-1.jsonl"), row("m1", "claude-opus-5-5", 1234)).expect("mine");
    // A subagent's message streamed over two rows: counted once, at its end.
    std::fs::write(
        dir.join("mine-1").join("subagents").join("agent-a.jsonl"),
        format!(
            "{}{}",
            row("s1", "claude-haiku-4-5", 4),
            row("s1", "claude-haiku-4-5", 40)
        ),
    )
    .expect("subagent");
    // The neighbour wrote last: newest-by-mtime would pick it.
    let theirs = dir.join("theirs-2.jsonl");
    std::fs::write(&theirs, row("t1", "claude-opus-5-5", 99_999)).expect("theirs");
    // Opened for writing: Windows sets a time only through a handle that may
    // write the file's attributes.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&theirs)
        .and_then(|f| {
            f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
        })
        .expect("the neighbour's mtime");

    env.claude_session = Some("mine-1".to_owned());
    let (ok, json, _) = go(&Cmd::Usage { json: true }, &env);
    assert!(ok);
    let doc: Value = aterm_json::from_str(&json).expect("json");
    assert_eq!(doc["schema"].as_u64(), Some(1));
    assert_eq!(doc["source"].as_str(), Some("transcript"), "{json}");
    assert_eq!(
        doc["transcript_pick"].as_str(),
        Some("session-id"),
        "{json}"
    );
    assert_eq!(doc["subagent_transcripts"].as_u64(), Some(1), "{json}");
    assert_eq!(doc["complete"].as_bool(), Some(true), "{json}");
    let spend = &doc["usage"]["accounts"][0]["spend"];
    assert_eq!(
        spend["claude-opus-5-5"]["out"].as_u64(),
        Some(1234),
        "this session's opus, never the neighbour's: {json}"
    );
    assert_eq!(
        spend["claude-haiku-4-5"]["out"].as_u64(),
        Some(40),
        "its subagent's haiku, at its final figures: {json}"
    );

    // A subagent transcript that exists and cannot be read is not in the
    // totals, and the output says so.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let locked = dir.join("mine-1").join("subagents").join("agent-b.jsonl");
        std::fs::write(&locked, row("s2", "claude-haiku-4-5", 7)).expect("subagent");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let (_, json, _) = go(&Cmd::Usage { json: true }, &env);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        std::fs::remove_file(&locked).expect("unlock");
        assert!(json.contains("\"complete\":false"), "{json}");
        assert!(json.contains("\"subagent_transcripts_unread\":1"), "{json}");
    }

    env.claude_session = Some("not-written-yet".to_owned());
    let (ok, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(ok);
    assert!(out.contains("source=none"), "{out}");

    // A build that names only the PROCESS (`CLAUDE_PID`).
    env.claude_session = None;
    let claude = env.claude_dir().expect("a claude dir");
    std::fs::create_dir_all(claude.join("sessions")).expect("sessions dir");
    std::fs::write(
        claude.join("sessions").join("777.json"),
        format!(
            r#"{{"pid":777,"sessionId":"mine-1","cwd":"{}"}}"#,
            host_path::json(&env.cwd)
        ),
    )
    .expect("sessions file");
    env.claude_pid = Some(777);
    assert_eq!(
        pick_transcript(&env),
        Pick::Named {
            path: dir.join("mine-1.jsonl"),
            how: "claude-pid"
        }
    );
    env.claude_pid = None;

    let (_, json, _) = go(&Cmd::Usage { json: true }, &env);
    let doc: Value = aterm_json::from_str(&json).expect("json");
    assert_eq!(doc["source"].as_str(), Some("transcript-newest"), "{json}");
    assert_eq!(
        doc["usage"]["accounts"][0]["spend"]["claude-opus-5-5"]["out"].as_u64(),
        Some(99_999),
        "the control: newest wins: {json}"
    );
}

/// A Claude Code process as the kernel would answer for it ([`ClaudeProcess`]):
/// `started` and `env` for the one pid [`FakeClaude::PID`], and how often
/// each was asked.
struct FakeClaude {
    started: Option<u64>,
    env: Option<Vec<String>>,
    asked: std::cell::Cell<(usize, usize)>,
}

impl FakeClaude {
    const PID: u32 = 4242;
    /// The second the kernel started it.
    const STARTED: u64 = 1_788_466_435;

    /// The live process, started with `env`.
    fn with(env: &[(&str, &str)]) -> FakeClaude {
        FakeClaude {
            started: Some(FakeClaude::STARTED),
            env: Some(env.iter().map(|(k, v)| format!("{k}={v}")).collect()),
            asked: std::cell::Cell::default(),
        }
    }
}

impl ClaudeProcess for FakeClaude {
    fn started(&self, pid: u32) -> Option<u64> {
        assert_eq!(pid, FakeClaude::PID, "the process CLAUDE_PID names");
        let (started, env) = self.asked.get();
        self.asked.set((started + 1, env));
        self.started
    }

    fn exec_env(&self, pid: u32) -> Option<Vec<String>> {
        assert_eq!(pid, FakeClaude::PID, "the process CLAUDE_PID names");
        let (started, env) = self.asked.get();
        self.asked.set((started, env + 1));
        self.env.clone()
    }
}

/// `usage` names its account by the roster's label for the login the Claude
/// Code it runs under was started with, where two or more are listed: the
/// HUD's `acct <label>` and the JSON's `label`, `active` true and no `dir`.
/// The login is read from the process `CLAUDE_PID` names and from nothing
/// else — [`run_usage`] has no other source for it — once, and only when a
/// roster of two or more and a named session ask for it.
///
/// NEGATIVE CONTROLS, each `acct account`: what the process's environment
/// says (not read, empty, a login no row names, a home of its own, a
/// credential, a route); what the roster says (refused, missing, empty, one
/// row: the process is not read at all); the newest-transcript pick, which
/// may be another session's (not read either); and a pid that is not proven
/// to be the Claude Code of the session folded — no registry row, a row that
/// names another session, a row for a process born at another second (the
/// pid was reused), a row or a kernel with no start to compare.
#[test]
fn usage_names_the_account_its_claude_code_runs_under() {
    let tmp = tmp_dir("usage-account");
    let mut env = env_at(&tmp);
    let dir = env
        .transcripts_root()
        .expect("home is injected")
        .join(footer::project_slug(&env.cwd));
    std::fs::create_dir_all(&dir).expect("project dir");
    std::fs::write(
        dir.join("mine-1.jsonl"),
        r#"{"type":"assistant","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":1,"output_tokens":5}}}
"#,
    )
    .expect("transcript");
    env.claude_session = Some("mine-1".to_owned());
    env.claude_pid = Some(FakeClaude::PID);
    // The registry row Claude Code keeps for its own pid.
    let sessions = env.claude_dir().expect("home is injected").join("sessions");
    std::fs::create_dir_all(&sessions).expect("sessions dir");
    let registry = sessions.join(format!("{}.json", FakeClaude::PID));
    let row = |session: &str, proc_start: Option<u64>| {
        let start = proc_start.map_or(String::new(), |s| {
            format!(r#","procStart":"{}""#, footer::lstart_utc(s))
        });
        format!(
            r#"{{"pid":{},"sessionId":"{session}","cwd":"{}"{start}}}"#,
            FakeClaude::PID,
            host_path::json(&env.cwd)
        )
    };
    let mine = row("mine-1", Some(FakeClaude::STARTED));
    std::fs::write(&registry, &mine).expect("registry row");

    let roster = env.accounts_roster().expect("a config dir");
    let work = tmp.path().join("logins").join("work-2");
    let work = work.to_str().expect("utf-8");
    let one = accounts::append_row("", "main", Class::Dedicated, None).expect("the row admits");
    let two =
        accounts::append_row(&one, "work-2", Class::Dedicated, Some(work)).expect("the row admits");
    std::fs::write(&roster, &two).expect("write");

    // The HUD line and the JSON's label, which always agree; `active` stays
    // true and no `dir` is added.
    let shown = |env: &Env, process: &FakeClaude| {
        let run = |json: bool| {
            let mut out = Vec::new();
            let code = run_usage(env, process, json, &mut out);
            assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
            String::from_utf8(out).expect("utf-8")
        };
        let hud = run(false).lines().next().unwrap_or_default().to_owned();
        let json = run(true);
        let doc: Value = aterm_json::from_str(&json).expect("json");
        let account = doc["usage"]["accounts"][0].clone();
        assert_eq!(account["active"].as_bool(), Some(true), "{json}");
        assert!(account.get("dir").is_none(), "no `dir` is added: {json}");
        let label = account["label"].as_str().expect("a label").to_owned();
        assert!(hud.contains(&format!(" acct {label} ")), "{hud} / {label}");
        label
    };
    let home = ("HOME", "/home/someone");

    // The default login is the row with no `dir`; a login names its row.
    // Each of the two runs reads the process once.
    let main = FakeClaude::with(&[home]);
    assert_eq!(shown(&env, &main), "main");
    assert_eq!(main.asked.get(), (2, 2), "one read a run");
    let on_work = FakeClaude::with(&[home, (accounts::LOGIN_ENV, work)]);
    assert_eq!(shown(&env, &on_work), "work-2");

    // NEGATIVE CONTROLS: what the process's environment says.
    let slashed = format!("{work}/");
    let not_named: [FakeClaude; 6] = [
        FakeClaude {
            env: None,
            ..FakeClaude::with(&[])
        },
        FakeClaude::with(&[]),
        FakeClaude::with(&[home, (accounts::LOGIN_ENV, slashed.as_str())]),
        FakeClaude::with(&[home, (accounts::HOME_ENV, "/elsewhere")]),
        FakeClaude::with(&[home, ("ANTHROPIC_API_KEY", "k")]),
        FakeClaude::with(&[home, ("ANTHROPIC_BASE_URL", "https://gateway.invalid")]),
    ];
    for process in &not_named {
        assert_eq!(shown(&env, process), "account", "{:?}", process.env);
    }

    // NEGATIVE CONTROLS: the pid is not proven to be this session's Claude
    // Code. The environment says `main` in each.
    let other = row("another-1", Some(FakeClaude::STARTED));
    let reused = row("mine-1", Some(FakeClaude::STARTED - 60));
    let unstamped = row("mine-1", None);
    for (what, text) in [
        ("no registry row", None),
        ("the row names another session", Some(other.as_str())),
        (
            "the row is of a process born earlier",
            Some(reused.as_str()),
        ),
        ("the row carries no start", Some(unstamped.as_str())),
    ] {
        match text {
            Some(text) => std::fs::write(&registry, text).expect("write"),
            None => std::fs::remove_file(&registry).expect("remove"),
        }
        assert_eq!(shown(&env, &main), "account", "{what}");
    }
    std::fs::write(&registry, &mine).expect("registry row");
    let no_start = FakeClaude {
        started: None,
        ..FakeClaude::with(&[home])
    };
    assert_eq!(shown(&env, &no_start), "account", "no kernel start");
    assert_eq!(shown(&env, &main), "main", "the row is back");

    // NEGATIVE CONTROLS: what the roster says. The process is not read.
    let refused = "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
                   [[account]]\nlabel = \"main\"\nclass = \"shared\"\ndir = \"/logins/x\"\n";
    let unread = FakeClaude::with(&[home]);
    for (what, text) in [
        ("refused", Some(refused)),
        ("no file", None),
        ("no rows", Some("# nothing listed yet\n")),
        ("one row", Some(one.as_str())),
    ] {
        match text {
            Some(text) => std::fs::write(&roster, text).expect("write"),
            None => std::fs::remove_file(&roster).expect("remove"),
        }
        assert_eq!(shown(&env, &unread), "account", "{what}");
    }

    // NEGATIVE CONTROLS: no pid named; and the newest transcript, which may
    // be another session's, is not named by this process's login.
    std::fs::write(&roster, &two).expect("write");
    assert_eq!(shown(&env, &main), "main", "the roster is back");
    env.claude_pid = None;
    assert_eq!(shown(&env, &unread), "account", "no CLAUDE_PID");
    env.claude_pid = Some(FakeClaude::PID);
    env.claude_session = None;
    assert_eq!(shown(&env, &main), "main", "the session the row names");
    std::fs::remove_file(&registry).expect("remove");
    assert_eq!(shown(&env, &unread), "account", "the newest pick");
    assert_eq!(unread.asked.get(), (0, 0), "never read");
}

/// A Codex home under `tmp` holding one conversation — its TUI's thread
/// (two turns on sol), a subagent forked from it (the copy, then a turn of
/// its own on luna) and a subagent of a model no public page prices — and
/// another conversation's thread beside it (a turn on gpt-5.5). Thread ids
/// in UUIDv7 form; the names of the three threads of the conversation.
fn codex_home_at(home: &Path) -> [&'static str; 3] {
    use crate::harness::codex_rollout::fixtures::{context, copied, count, message, meta};
    const ROOT: &str = "01900000-0000-7000-8000-000000000001";
    const SUB: &str = "01900000-0000-7000-8000-000000000002";
    const UNPRICED: &str = "01900000-0000-7000-8000-000000000004";
    const OTHER: &str = "01900000-0000-7000-8000-000000000003";
    let day = home.join("sessions/2026/10/15");
    std::fs::create_dir_all(&day).expect("sessions");
    let write = |thread: &str, body: String| {
        std::fs::write(
            day.join(format!("rollout-2026-10-15T10-00-00-{thread}.jsonl")),
            body,
        )
        .expect("rollout");
    };
    let b: i64 = 1_790_000_000_000;
    let parent = [
        meta(b, ROOT, ROOT, None),
        context(b + 1000, "gpt-5.6-sol", None),
        count(b + 5000, (1000, 800, 50), (1000, 800, 50)),
        count(b + 9000, (3000, 2600, 150), (2000, 1800, 100)),
    ]
    .concat();
    write(ROOT, parent.clone());
    let at = b + 60_000;
    write(
        SUB,
        [
            meta(at, SUB, ROOT, Some(ROOT)),
            copied(&parent, at + 1),
            message(at + 2500),
            context(at + 2501, "gpt-6-luna", None),
            count(at + 7000, (3500, 2600, 250), (500, 0, 100)),
        ]
        .concat(),
    );
    write(
        UNPRICED,
        [
            meta(b + 90_000, UNPRICED, ROOT, None),
            context(b + 91_000, "gpt-daybreak-blue-latest", None),
            count(b + 95_000, (70, 0, 7), (70, 0, 7)),
        ]
        .concat(),
    );
    write(
        OTHER,
        [
            meta(b, OTHER, OTHER, None),
            context(b + 1000, "gpt-5.5", None),
            count(b + 5000, (9000, 0, 900), (9000, 0, 900)),
        ]
        .concat(),
    );
    [ROOT, SUB, UNPRICED]
}

/// Run from a Codex session's own tools (`CODEX_THREAD_ID`), `usage` folds
/// its conversation's rollouts under `CODEX_HOME`: the TUI's thread and its
/// subagents, each served turn once and the fork's copied prefix not at all,
/// at the shipped table's list prices; a model the table does not price is
/// unpriced, its `usd` null; another conversation is none of it. Negative
/// controls: without the thread named, the Claude Code path folds (nothing
/// here); a Claude Code session the vendor named still comes first.
#[test]
fn usage_from_codex_tools_folds_its_conversation_at_list_prices() {
    let tmp = tmp_dir("usage-codex");
    let mut env = env_at(&tmp);
    let codex = tmp.path().join("codex-home");
    let [root, sub, _] = codex_home_at(&codex);
    env.codex_home = Some(codex.to_string_lossy().into_owned());
    env.codex_thread = Some(sub.to_owned());

    let (ok, json, err) = go(&Cmd::Usage { json: true }, &env);
    assert!(ok, "{err}");
    let doc: Value = aterm_json::from_str(&json).expect("json");
    assert_eq!(doc["source"].as_str(), Some("rollout"), "{json}");
    assert_eq!(doc["transcript_pick"].as_str(), Some("codex-thread"));
    assert_eq!(doc["codex_thread"].as_str(), Some(sub));
    assert_eq!(doc["conversation"].as_str(), Some(root));
    assert_eq!(doc["subagent_rollouts"].as_u64(), Some(2));
    assert_eq!(doc["turns"].as_u64(), Some(4), "{json}");
    assert_eq!(doc["copied"].as_u64(), Some(2), "{json}");
    assert_eq!(doc["complete"].as_bool(), Some(true));
    for key in ["rows_unparsed", "rows_oversized", "turns_untimed"] {
        assert_eq!(doc[key].as_u64(), Some(0), "{json}");
    }
    assert_eq!(doc["prices_as_of"].as_str(), Some("2026-10-15"));
    let account = &doc["usage"]["accounts"][0];
    assert_eq!(account["label"].as_str(), Some("codex"));
    assert_eq!(
        account["model"].as_str(),
        Some("gpt-6-luna"),
        "the named thread's"
    );
    let spend = &account["spend"];
    let sol = &spend["gpt-5.6-sol"];
    assert_eq!(
        (
            sol["in"].as_u64(),
            sol["cache_read"].as_u64(),
            sol["out"].as_u64()
        ),
        (Some(400), Some(2600), Some(150)),
        "{json}"
    );
    // The shipped table: sol $4 in, $0.40 cached, $20 out; luna $0.10 in,
    // $0.50 out.
    let usd = |m: &Value| m["usd"].as_f64().expect("priced");
    assert!((usd(sol) - (400.0 * 4.0 + 2600.0 * 0.4 + 150.0 * 20.0) / 1e6).abs() < 1e-12);
    assert!((usd(&spend["gpt-6-luna"]) - (500.0 * 0.1 + 100.0 * 0.5) / 1e6).abs() < 1e-12);
    assert!(spend["gpt-daybreak-blue-latest"]["usd"].is_null(), "{json}");
    assert_eq!(spend["gpt-daybreak-blue-latest"]["in"].as_u64(), Some(70));
    assert!(
        spend.get("gpt-5.5").is_none(),
        "another conversation's: {json}"
    );

    let (ok, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(ok);
    assert_eq!(
        out.lines().next(),
        Some("gpt ?%/5h · ?%/7d · acct codex · resets ? · none  [source=rollout]"),
        "{out}"
    );
    assert!(
        out.contains(
            "  gpt-5.6-sol: in 400 (cache write 0, cache read 2600), out 150, 2 turns · ≈$0.0056 (list)"
        ),
        "{out}"
    );
    assert!(
        out.contains("gpt-daybreak-blue-latest: in 70 (cache write 0, cache read 0), out 7, 1 turns · unpriced"),
        "{out}"
    );
    // The TUI's thread's rollout, though a subagent named the thread: the
    // subagents are the other two.
    let root_rollout = ["sessions", "2026", "10", "15"]
        .iter()
        .fold(codex.clone(), |dir, name| dir.join(name))
        .join(format!("rollout-2026-10-15T10-00-00-{root}.jsonl"));
    assert!(
        out.contains(&format!(
            "SPEND folded from {} (+2 subagent rollouts; 4 turns)",
            root_rollout.display()
        )),
        "{out}"
    );
    assert!(out.contains("(2 copied token counts)"), "{out}");
    assert!(out.contains("some unpriced"), "{out}");
    // With the TUI's thread's rollout gone, only the subagents fold.
    let kept = std::fs::read(&root_rollout).expect("read");
    std::fs::remove_file(&root_rollout).expect("remove");
    let (_, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(
        out.contains(&format!(
            "SPEND folded from 2 subagent rollouts (2 turns; its TUI's thread {root} has none here)"
        )),
        "{out}"
    );
    std::fs::write(&root_rollout, &kept).expect("write back");

    // Unrelated events and rate-limit-only counts lose no served turn.
    // Damaged served-count rows between valid turns must make the same
    // surviving totals partial, even when every rollout reaches EOF.
    use crate::harness::codex_rollout::fixtures::count;
    let benign = concat!(
        "{\"type\":\"future_event\",\"payload\":{\"type\":\"token_count\"}}\n",
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":null}}\n"
    );
    let oversized = format!(
        "{}{}",
        " ".repeat(usage::MAX_LINE_BYTES),
        count(1_790_000_006_000, (2000, 1600, 100), (1000, 800, 50))
    );
    let untimed = count(1_790_000_007_000, (2500, 2000, 125), (500, 400, 25))
        .replace("\"timestamp\"", "\"unrecognized_stamp\"");
    let malformed = "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{}}}\n";
    let damaged = [benign, malformed, &oversized, &untimed].concat();
    for (extra, losses) in [
        (benign, (0, 0, 0)),
        (malformed, (1, 0, 0)),
        (oversized.as_str(), (0, 1, 0)),
        (untimed.as_str(), (0, 0, 1)),
        (damaged.as_str(), (1, 1, 1)),
    ] {
        let partial = losses != (0, 0, 0);
        let mut lines: Vec<_> = std::str::from_utf8(&kept)
            .expect("rollout text")
            .lines()
            .collect();
        lines.insert(lines.len() - 1, extra.trim_end_matches('\n'));
        std::fs::write(&root_rollout, lines.join("\n") + "\n").expect("rollout with events");
        let (ok, json, err) = go(&Cmd::Usage { json: true }, &env);
        assert!(ok, "{err}");
        let doc: Value = aterm_json::from_str(&json).expect("json");
        assert_eq!(doc["complete"].as_bool(), Some(!partial), "{json}");
        for (key, count) in [
            ("rows_unparsed", losses.0),
            ("rows_oversized", losses.1),
            ("turns_untimed", losses.2),
        ] {
            assert_eq!(doc[key].as_u64(), Some(count), "{json}");
        }
        assert_eq!(doc["turns"].as_u64(), Some(4), "{json}");
        assert_eq!(
            doc["usage"]["accounts"][0]["spend"], *spend,
            "valid turns before and after the skipped records still fold: {json}"
        );
        let (ok, out, err) = go(&Cmd::Usage { json: false }, &env);
        assert!(ok, "{err}");
        assert_eq!(out.contains("these totals are partial"), partial, "{out}");
        if partial {
            for (count, label) in [
                (losses.0, "malformed rows"),
                (losses.1, "oversized rows"),
                (losses.2, "untimed turns"),
            ] {
                assert!(out.contains(&format!("{count} {label}")), "{out}");
            }
        }
    }

    // NEGATIVE CONTROL: no thread named, the Claude Code path.
    let plain = Env {
        codex_thread: None,
        ..env_at(&tmp)
    };
    let (_, out, _) = go(&Cmd::Usage { json: false }, &plain);
    assert!(out.contains("source=none"), "{out}");
    assert!(out.contains("no transcript for this directory"), "{out}");
    // NEGATIVE CONTROL: a Claude Code session the vendor named comes first.
    env.claude_session = Some("11111111-2222-4333-8444-555555555555".to_owned());
    let (_, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(out.contains("has written no transcript yet"), "{out}");
    assert!(!out.contains("source=rollout"), "{out}");
}

/// The Codex home is `CODEX_HOME`, else `$HOME/.codex`; a relative
/// `CODEX_HOME` names none this command can find, and `~/.codex` is not it:
/// nothing folds, and the run says why. Control: unset, `~/.codex` folds.
#[test]
fn usage_from_codex_tools_with_a_relative_codex_home_folds_nothing_and_says_so() {
    let tmp = tmp_dir("usage-codex-relative");
    let mut env = env_at(&tmp);
    let home = env.home.clone().expect("home is injected");
    let [_, sub, _] = codex_home_at(&home.join(".codex"));
    env.codex_thread = Some(sub.to_owned());
    let (_, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(out.contains("source=rollout"), "control: {out}");

    env.codex_home = Some("rel/codex".to_owned());
    let (ok, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(ok);
    assert!(out.contains("source=none"), "{out}");
    assert!(
        out.contains("CODEX_HOME is \"rel/codex\", not an absolute path"),
        "{out}"
    );
    let (_, json, _) = go(&Cmd::Usage { json: true }, &env);
    let doc: Value = aterm_json::from_str(&json).expect("json");
    assert_eq!(doc["source"].as_str(), Some("none"), "{json}");
    assert_eq!(doc["codex_home_unusable"].as_str(), Some("rel/codex"));
    // A thread with no rollout yet folds nothing of anyone's.
    env.codex_home = None;
    env.codex_thread = Some("01900000-0000-7000-8000-00000000000f".to_owned());
    let (_, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(out.contains("has written no rollout under"), "{out}");
}

// ---------------------------------------------------------------------------
// limits
// ---------------------------------------------------------------------------

/// A screen with Claude Code's session-limit notice above its composer.
fn limited_screen() -> Vec<String> {
    let mut screen = rows(&[
        "❯ carry on with the parser",
        "",
        "⏺ Working through it.",
        "  ⎿  You've hit your session limit · resets in 3h",
        "",
    ]);
    screen.extend(composer("  ? for shortcuts"));
    screen
}

/// The same frame with the turn simply over — no notice.
fn idle_screen() -> Vec<String> {
    let mut screen = rows(&["⏺ Merged and pushed.", ""]);
    screen.extend(composer("  ? for shortcuts"));
    screen
}

/// The `text --json` body a server sends for `rows`.
fn text_json(rows: &[String]) -> String {
    let mut o = Map::new();
    o.insert(
        "rows".to_owned(),
        Value::Array(rows.iter().map(|r| Value::from(r.clone())).collect()),
    );
    let mut cursor = Map::new();
    cursor.insert("row".to_owned(), Value::from(0u64));
    cursor.insert("col".to_owned(), Value::from(0u64));
    o.insert("cursor".to_owned(), Value::Object(cursor));
    o.insert("seq".to_owned(), Value::from(7u64));
    aterm_json::to_string(&Value::Object(o)).expect("json")
}

/// A [`Ctl`] that answers every request with one scripted reply and records
/// what it was asked.
struct Scripted {
    reply: CtlReply,
    asked: Vec<String>,
}

impl Ctl for Scripted {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        self.asked.push(args.join(" "));
        Ok(self.reply.clone())
    }
}

/// A [`Ctl`] for one tab: `status` names `program` as its foreground (`None`:
/// `program=-`), `text` sends `rows`; every request recorded.
struct TabCtl {
    program: Option<&'static str>,
    rows: Vec<String>,
    asked: Vec<String>,
}

impl Ctl for TabCtl {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        self.asked.push(args.join(" "));
        let stdout = if args.get(1) == Some(&"status") {
            format!(
                "OK schema=1 sid=s-worker program={}",
                self.program.unwrap_or("-")
            )
        } else {
            text_json(&self.rows)
        };
        Ok(CtlReply {
            code: 0,
            stdout,
            stderr: String::new(),
        })
    }
}

fn scripted(stdout: String, code: i32, stderr: &str) -> Scripted {
    Scripted {
        reply: CtlReply {
            code,
            stdout,
            stderr: stderr.to_string(),
        },
        asked: Vec::new(),
    }
}

/// Claude Code's critical-memory banner is the engine's own
/// wall, `wall=memory`, and no limit: a restart ends it, not a wait, so it
/// has no reset. Its words say `usage` and `continue`, and the painted
/// `/usage` reader does not read them as a usage window.
#[test]
fn the_memory_banner_is_its_own_wall_and_no_limit() {
    use aterm_phase::prompt::fixtures::{MEMORY_BANNER_IDLE, screen};
    let zone_none = |_: &str| -> Option<i64> { None };
    let v = limits_view(
        Program::Claude.reader(),
        &screen(MEMORY_BANNER_IDLE),
        1_758_412_800,
        0,
        &|z, _| zone_none(z),
    );
    let wall = v.wall.as_ref().expect("the banner is read");
    assert_eq!(wall.kind, aterm_phase::WallKind::Memory);
    assert!(!wall.kind.reads_limited(), "a restart ends it, not a reset");
    assert_eq!(v.resets_at, None);
    assert!(v.windows.is_empty(), "{v:?}");
    assert!(
        limits_text(&v).starts_with("wall=memory resets_at=- "),
        "{}",
        limits_text(&v)
    );
}

/// `limits` asks the named session's `status` (which program draws it), makes
/// ONE screen read and prints its wall — the engine's own reading (a screen
/// that names no program read by what it shows, here Claude Code's frame),
/// its reset placed by the supervisor's clock, nothing re-parsed — at
/// `source=grid`. NEGATIVE CONTROLS: an idle screen is `wall=none` with no
/// window, and a read that fails is exit 1 naming the session — never
/// `wall=none`, which would read as "measured, nothing wrong".
#[test]
fn limits_reads_the_sessions_screen_once_and_names_its_wall() {
    let tmp = tmp_dir("limits");
    let env = env_at(&tmp);
    let run_with = |ctl: &mut Scripted, sid: Option<&str>, json: bool| {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_limits_with(ctl, &env, sid, 0, json, &mut out, &mut err);
        (
            format!("{code:?}"),
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        )
    };
    let ok = format!("{:?}", ExitCode::SUCCESS);

    let mut ctl = scripted(text_json(&limited_screen()), 0, "");
    let (code, out, _) = run_with(&mut ctl, Some("@s-worker"), false);
    assert_eq!(code, ok);
    assert!(out.starts_with("wall=usage-session"), "{out}");
    assert!(
        out.contains("message=You've hit your session limit"),
        "{out}"
    );
    assert!(out.contains("source=grid"), "{out}");
    assert!(out.contains("resets_at=2025-09-21T03:00:00Z"), "{out}");
    assert_eq!(ctl.asked.len(), 2, "status, then one read: {:?}", ctl.asked);
    assert_eq!(ctl.asked[0], "@s-worker status");
    assert!(
        ctl.asked[1].starts_with("@s-worker text --json"),
        "{:?}",
        ctl.asked
    );

    // No sid on the line: this session's, from the injected env.
    let mut ctl = scripted(text_json(&limited_screen()), 0, "");
    let (_, json, _) = run_with(&mut ctl, None, true);
    assert!(ctl.asked[1].starts_with("@s-test text"), "{:?}", ctl.asked);
    assert!(json.contains("\"schema\":2"), "{json}");
    assert!(json.contains("\"kind\":\"usage-session\""), "{json}");
    assert!(json.contains("\"windows\":[]"), "{json}");
    assert!(json.contains("\"source\":\"grid\""), "{json}");

    let mut ctl = scripted(text_json(&idle_screen()), 0, "");
    let (code, out, _) = run_with(&mut ctl, Some("@s-worker"), false);
    assert_eq!(code, ok);
    assert!(out.starts_with("wall=none source=grid windows=- "), "{out}");

    let mut ctl = scripted(String::new(), 1, "ERR no such session @s-gone");
    let (code, out, err) = run_with(&mut ctl, Some("@s-gone"), false);
    assert_eq!(code, format!("{:?}", ExitCode::from(1)));
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("@s-gone"), "{err}");
}

/// Outside an aterm session (`$ATERM_PARENT_SESSION_ID` empty) and with no
/// `@<sid>`, `limits` refuses with exit 2 and reads nothing rather than
/// classifying whichever session the socket defaults to. Negative control: a
/// sid on the line reads that session.
#[test]
fn limits_with_no_session_to_read_refuses_and_reads_nothing() {
    let tmp = tmp_dir("limits-nosid");
    let mut env = env_at(&tmp);
    env.sid = String::new();
    let mut ctl = scripted(text_json(&limited_screen()), 0, "");
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_limits_with(&mut ctl, &env, None, 0, false, &mut out, &mut err);
    let err = String::from_utf8_lossy(&err);
    assert_eq!(
        format!("{code:?}"),
        format!("{:?}", ExitCode::from(2)),
        "{err}"
    );
    assert!(out.is_empty(), "{}", String::from_utf8_lossy(&out));
    assert!(ctl.asked.is_empty(), "read something: {:?}", ctl.asked);
    assert!(err.contains("@<sid>"), "{err}");

    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_limits_with(
        &mut ctl,
        &env,
        Some("@s-named"),
        0,
        false,
        &mut out,
        &mut err,
    );
    assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
    assert!(
        ctl.asked[1].starts_with("@s-named text --json"),
        "{:?}",
        ctl.asked
    );
}

/// `limits` reads a tab by ITS OWN PROGRAM'S reader, the one `status` names
/// in front: a Codex tab's usage wall (`■ You’ve hit your usage limit … try
/// again at …`) reads as no wall, since aterm reads no Codex screen (its Codex
/// screen lane was retired on 2026-10-16; codex-alab waits out its own
/// limits). NEGATIVE CONTROL: a Claude Code tab's wall reads as it always
/// did, by Claude Code's reader.
#[test]
fn limits_reads_each_tab_by_its_own_programs_grammar() {
    let tmp = tmp_dir("limits-program");
    let env = env_at(&tmp);
    let read = |program: Option<&'static str>, rows: Vec<String>| {
        let mut ctl = TabCtl {
            program,
            rows,
            asked: Vec::new(),
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_limits_with(
            &mut ctl,
            &env,
            Some("@s-worker"),
            0,
            false,
            &mut out,
            &mut err,
        );
        let err = String::from_utf8_lossy(&err);
        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", ExitCode::SUCCESS),
            "{err}"
        );
        assert_eq!(ctl.asked[0], "@s-worker status");
        String::from_utf8_lossy(&out).into_owned()
    };

    let wall = rows(&[
        "■ You’ve hit your usage limit. Upgrade to Pro, or try again at Oct 17th, 2026 1:14 PM.",
        "",
        "› Ask Codex to do anything",
        "",
        "  gpt-5 · ~/src",
    ]);
    let codex = read(Some("codex"), wall);
    assert!(codex.starts_with("wall=none "), "{codex}");

    // Negative control: a Claude Code tab, by its own reader as ever.
    let claude = read(Some("claude"), limited_screen());
    assert!(
        claude.starts_with("wall=usage-session resets_at=2025-09-21T03:00:00Z "),
        "{claude}"
    );
    assert!(
        claude.contains("message=You've hit your session limit"),
        "{claude}"
    );
}

// ---------------------------------------------------------------------------
// disk
// ---------------------------------------------------------------------------

/// The report answers, removes nothing by default, and is journalled to the
/// disk ledger `ledger disk` reads back.
#[test]
fn the_disk_report_names_nothing_removable_by_default_and_is_journalled() {
    let tmp = tmp_dir("disk");
    let env = env_at(&tmp);
    let cmd = Cmd::Disk {
        targets: vec![tmp.path().join("no-such-target")],
        apply: None,
        json: false,
    };
    let (ok, out, err) = go(&cmd, &env);
    assert!(ok, "{err}");
    assert!(out.contains("kind=disk"), "{out}");
    assert!(out.contains("apply=off"), "{out}");
    // A path that is not a directory is not a row — and not an error either.
    assert!(!out.contains("no-such-target"), "{out}");

    let (ok, rows, _) = go(
        &Cmd::Ledger {
            file: LedgerFile::Disk,
            count: 10,
            json: true,
        },
        &env,
    );
    assert!(ok);
    assert_eq!(rows.lines().count(), 1, "{rows}");
    assert!(rows.contains("\"kind\":\"report\""), "{rows}");
    // A second run appends; nothing is overwritten.
    let _ = go(&cmd, &env);
    let (_, rows, _) = go(
        &Cmd::Ledger {
            file: LedgerFile::Disk,
            count: 10,
            json: true,
        },
        &env,
    );
    assert_eq!(rows.lines().count(), 2, "{rows}");
}

/// THE VERB HOLDS THE SYSTEM'S ERROR BOXES OFF (wave 3's soundness review,
/// its first finding): `aterm harness disk` canonicalizes its roots and the
/// tabs' directories and walks build directories, and on Windows a query of
/// an empty card reader or optical drive otherwise puts a box on the
/// person's desktop and parks the command. Read inside the verb through its
/// seam; the thread's own mode is back once it returns. NEGATIVE CONTROL:
/// this test's thread, before and after, reads the boxes on (Windows;
/// elsewhere no such box exists and the seam answers quiet everywhere).
#[test]
fn the_disk_verb_holds_the_error_boxes_off() {
    let tmp = tmp_dir("disk-quiet");
    let env = env_at(&tmp);
    let cmd = Cmd::Disk {
        targets: vec![tmp.path().join("no-such-target")],
        apply: None,
        json: false,
    };
    DISK_ERRORS_QUIET.set(None);
    let unguarded = !cfg!(windows);
    assert_eq!(aterm_sysprobe::errors_quiet(), unguarded, "no guard before");
    let (ok, _, err) = go(&cmd, &env);
    assert!(ok, "{err}");
    assert_eq!(DISK_ERRORS_QUIET.get(), Some(true), "inside the verb");
    assert_eq!(
        aterm_sysprobe::errors_quiet(),
        unguarded,
        "the verb restored its caller's mode"
    );
}

/// The store the disk scan reads and the `bin/` clause 5 reads are atpkg's
/// own, on every platform: `<atpkg's default prefix>/store` and its sibling
/// `bin/`. On Windows that is `%LOCALAPPDATA%\aterm\pkg`, never the
/// `~\.local\share\aterm\pkg` that is not there.
#[test]
fn the_disk_scan_reads_atpkgs_own_store_and_bin() {
    let tmp = tmp_dir("disk-store");
    let home = tmp.path();
    let prefix = atpkg::store::default_prefix(home);
    assert_eq!(default_store_dir(home), prefix.join("store"));
    assert!(
        default_link_dirs(home).contains(&prefix.join("bin")),
        "{:?}",
        default_link_dirs(home)
    );
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA").map_or_else(
            || home.join("AppData").join("Local"),
            std::path::PathBuf::from,
        );
        assert_eq!(
            default_store_dir(home),
            local.join("aterm").join("pkg").join("store")
        );
        assert!(!default_store_dir(home).starts_with(home.join(".local")));
    }
    #[cfg(target_os = "linux")]
    assert_eq!(
        default_store_dir(home),
        home.join(".local/share/aterm/pkg/store")
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        default_store_dir(home),
        home.join("Library/Application Support/aterm/pkg/store")
    );
}

/// The disk journal is bounded: a journal past the bound is cut back to its
/// newest rows before the run appends. Negative control: a journal under the
/// bound is left byte-for-byte alone.
#[test]
fn the_disk_journal_is_cut_back_to_its_newest_rows_past_the_bound() {
    let tmp = tmp_dir("disk-bound");
    let env = env_at(&tmp);
    let path = env.disk_ledger();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let row = |i: usize| {
        format!(
            "{{\"kind\":\"report\",\"seq\":{i},\"pad\":\"{}\"}}",
            "x".repeat(200)
        )
    };
    let rows = usize::try_from(DISK_LEDGER_MAX_BYTES).unwrap() / 200 + 10;
    let mut text = String::new();
    for i in 0..rows {
        text.push_str(&row(i));
        text.push('\n');
    }
    std::fs::write(&path, &text).unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() > DISK_LEDGER_MAX_BYTES);

    let cmd = Cmd::Disk {
        targets: vec![],
        apply: None,
        json: false,
    };
    let (ok, _, err) = go(&cmd, &env);
    assert!(ok, "{err}");
    let after = std::fs::read_to_string(&path).unwrap();
    let n = after.lines().count();
    assert_eq!(
        n,
        DISK_LEDGER_KEEP_ROWS + 1,
        "the kept rows plus this run's report"
    );
    assert!(
        after.len() as u64 <= DISK_LEDGER_MAX_BYTES,
        "{} bytes",
        after.len()
    );
    // The NEWEST rows survive, in order, and this run's row is last.
    let first_kept = rows - DISK_LEDGER_KEEP_ROWS;
    assert!(
        after
            .lines()
            .next()
            .unwrap()
            .contains(&format!("\"seq\":{first_kept},"))
    );
    assert!(
        after
            .lines()
            .nth(DISK_LEDGER_KEEP_ROWS - 1)
            .unwrap()
            .contains(&format!("\"seq\":{},", rows - 1))
    );
    assert!(
        after
            .lines()
            .last()
            .unwrap()
            .contains("\"kind\":\"report\"")
    );
    assert!(!after.lines().last().unwrap().contains("\"seq\""));
    assert!(!path.with_file_name("disk.jsonl.tmp").exists());

    // Negative control: under the bound nothing is cut, and the pure cut says
    // so.
    let before = std::fs::read(&path).unwrap();
    assert!(!bound_ledger(&path, DISK_LEDGER_MAX_BYTES, DISK_LEDGER_KEEP_ROWS).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    // A missing journal is not an error.
    assert!(!bound_ledger(&tmp.path().join("none.jsonl"), 1, 1).unwrap());
}

/// THE CUT IS ONLY ORDERED (aterm-durable's counting seam): its temporary is
/// flushed ahead of the rename that publishes it, never fully; a journal under
/// the bound is not flushed at all.
#[test]
fn a_ledger_cut_is_ordered_ahead_of_its_rename() {
    let tmp = tmp_dir("ledger-cut-flush");
    let path = tmp.path().join("ledger.jsonl");
    std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let recording = aterm_durable::seam::Recording::start();
    assert!(!bound_ledger(&path, 1 << 20, 1).unwrap());
    assert_eq!(recording.take(), [], "under the bound: nothing written");
    assert!(bound_ledger(&path, 4, 1).unwrap());
    assert_eq!(recording.take(), [aterm_durable::Flush::Ordered]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "three\n");
}

/// `now` far enough past the files a test just wrote that every profile in
/// them is idle.
fn idle_now() -> i64 {
    i64::try_from(aterm_time::unix_secs().unwrap()).unwrap()
        + (disk::DEFAULT_TARGET_STALE_DAYS + 1) * 86_400
}

/// The real remover the window's tick runs, at `now`, on any device.
#[cfg(unix)]
fn real_remover(now: i64) -> impl FnMut(&disk::Row) -> io::Result<disk::Removed> {
    disk::remover(
        disk::Judge {
            now,
            threshold_days: disk::DEFAULT_TARGET_STALE_DAYS,
            device: None,
            volume: None,
        },
        disk::idle::Sessions::default,
    )
}

/// The host's tick on a cargo-shaped build directory: at or above the
/// warning it reads and writes nothing; below it only the idle profile's
/// `incremental/` is reclaimed (free space measured again first), and the
/// report, the intent row
/// (before the removal) and the outcome row (witness, bytes released, unit)
/// land in the disk ledger in that order. `[disk] auto_free_gib` moves the
/// floor; `0` turns it off. The fixture plants no root `.cargo-lock` (cargo
/// never writes one).
#[cfg(unix)]
#[test]
fn the_disk_tick_reclaims_stale_targets_below_the_floor_and_nothing_above() {
    let tmp = tmp_dir("disk-tick");
    let state = tmp.path().join("state");
    let target = disk::tests::lay_target(&tmp.path().join("repo/target"));
    let now = idle_now();
    let gib = 1024 * 1024 * 1024;
    let cfg = disk::Config::default();
    let targets = [target.clone()];

    let look = |free: Option<u64>| DiskLook {
        state: &state,
        volume: tmp.path(),
        free,
        targets: &targets,
        discover: None,
        transcripts: None,
        now,
        room: None,
        table: None,
        demand: None,
        session_cwds: &[],
        session_cwd_unknown: None,
        link_dirs: &[],
    };
    let plenty = disk_tick(
        &look(Some(cfg.warn_free_gib * gib)),
        cfg,
        &mut |row| panic!("removed {} above the warning", row.path.display()),
        &mut disk::tests::no_pressure,
    );
    assert_eq!(plenty, DiskTick::Plenty);
    assert!(
        !state.join(DISK_LEDGER).exists(),
        "nothing journalled above it"
    );
    let off = disk::Config {
        auto_free_gib: 0,
        ..cfg
    };
    let tick = disk_tick(
        &look(Some(1)),
        off,
        &mut |row| panic!("removed {} with the floor off", row.path.display()),
        &mut disk::tests::no_pressure,
    );
    assert_eq!(tick, DiskTick::Plenty);

    let DiskTick::Reclaimed(done, _) =
        disk_tick(&look(Some(gib)), cfg, &mut real_remover(now), &mut || {
            Some(gib)
        })
    else {
        panic!("below the floor, the tick applies");
    };
    assert_eq!(done.removed.len(), 1, "{done:?}");
    assert!(done.denials.is_empty(), "{done:?}");
    assert!(target.join("debug").exists(), "the build directory stays");
    assert!(
        !target.join("debug/incremental").exists(),
        "its idle incremental/ is gone"
    );
    for kept in [
        "CACHEDIR.TAG",
        "debug/deps/liblay.rlib",
        "debug/build",
        "debug/.fingerprint",
    ] {
        assert!(target.join(kept).exists(), "{kept} is kept");
    }
    let freed = done.removed[0].1.bytes;
    assert!(freed >= 8192, "the query cache's blocks: {done:?}");
    assert_eq!(done.freed_bytes, freed);
    let ledger = std::fs::read_to_string(state.join(DISK_LEDGER)).expect("the ledger");
    let rows: Vec<&str> = ledger.lines().collect();
    assert_eq!(rows.len(), 3, "{ledger}");
    assert!(rows[0].contains("\"trigger\":\"tick\""), "{}", rows[0]);
    assert!(rows[1].contains("\"kind\":\"removing\""), "{}", rows[1]);
    assert!(rows[2].contains("\"kind\":\"removed\""), "{}", rows[2]);
    assert!(rows[2].contains("stale-build-dir"), "{}", rows[2]);
    assert!(
        rows[2].contains(&format!("\"freed_bytes\":{freed}")),
        "{}",
        rows[2]
    );
    assert!(rows[2].contains("debug/incremental"), "{}", rows[2]);
}

/// Set the mtime of the directory at `path` to `at` (unix seconds).
#[cfg(unix)]
fn set_mtime(path: &Path, at: i64) {
    let when = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(at).expect("after the epoch"));
    std::fs::File::open(path)
        .and_then(|f| f.set_modified(when))
        .expect("set mtime");
}

/// A build directory at `<tmp>/<name>` holding `debug/`, built just now, and
/// `release/`, whose compile outputs were last written two days before `now`.
#[cfg(unix)]
fn two_day_old_release(tmp: &Tmp, name: &str, now: i64) -> PathBuf {
    let target = disk::tests::lay_target(&tmp.path().join(name));
    disk::tests::lay_profile(&target.join("release"));
    for c in ["deps", ".fingerprint", "build"] {
        set_mtime(&target.join("release").join(c), now - 2 * 86_400);
    }
    target
}

/// One tick below the floor over `target` under `cfg`, on the real remover
/// with `cfg`'s window, free space re-measured through `measure`.
#[cfg(unix)]
fn tick_below_floor(
    tmp: &Tmp,
    target: &Path,
    now: i64,
    cfg: disk::Config,
    measure: &mut disk::Measure<'_>,
) -> disk::Applied {
    let state = tmp.path().join("state");
    let targets = [target.to_path_buf()];
    let look = DiskLook {
        state: &state,
        volume: tmp.path(),
        free: Some(1024 * 1024 * 1024),
        targets: &targets,
        discover: None,
        transcripts: None,
        now,
        room: None,
        table: None,
        demand: None,
        session_cwds: &[],
        session_cwd_unknown: None,
        link_dirs: &[],
    };
    let mut remover = disk::remover(
        disk::Judge {
            now,
            threshold_days: cfg.target_stale_days,
            device: None,
            volume: None,
        },
        disk::idle::Sessions::default,
    );
    let DiskTick::Reclaimed(done, _) = disk_tick(&look, cfg, &mut remover, measure) else {
        panic!("below the floor, the tick applies");
    };
    done
}

/// The shipped idle window takes a two-day-old profile. With no config file,
/// on the real clock, one build directory holds a profile untouched for two
/// days (`release/`) and one built just now (`debug/`): below the floor the
/// idle one's `incremental/` goes and the fresh one's stays. (An explicit
/// window and the least-recently-used pass under pressure:
/// `disk_pressure_tests`.)
#[cfg(unix)]
#[test]
fn the_default_window_reclaims_a_profile_idle_two_days_and_keeps_one_built_today() {
    let tmp = tmp_dir("disk-window");
    let now = i64::try_from(aterm_time::unix_secs().unwrap()).unwrap();
    let target = two_day_old_release(&tmp, "repo/target", now);
    let cfg = disk_config_read(None).config;
    let done = tick_below_floor(&tmp, &target, now, cfg, &mut || Some(1));
    assert_eq!(done.removed.len(), 1, "{done:?}");
    assert!(done.denials.is_empty(), "{done:?}");
    assert!(
        !target.join("release/incremental").exists(),
        "the profile idle two days gives up its incremental/"
    );
    assert!(
        target
            .join("debug/incremental/lay-1x2y3z/s-abc-def/query-cache.bin")
            .exists(),
        "the profile built today keeps its cache"
    );
    for kept in [
        "release/deps/liblay.rlib",
        "release/.fingerprint",
        "release/build",
    ] {
        assert!(target.join(kept).exists(), "{kept} is kept");
    }
    let units = &done.removed[0].1.units;
    assert_eq!(units.len(), 1, "{units:?}");
    assert!(units[0].ends_with("release/incremental"), "{units:?}");
}

/// The intent row is journalled before a removal starts, so a remover that
/// dies mid-pass leaves the report and the intent on record, naming the build
/// directory.
#[cfg(unix)]
#[test]
fn a_tick_cut_short_mid_removal_leaves_the_intent_in_the_journal() {
    let tmp = tmp_dir("disk-cut");
    let state = tmp.path().join("state");
    let target = disk::tests::lay_target(&tmp.path().join("repo/target"));
    let targets = [target.clone()];
    let look = DiskLook {
        state: &state,
        volume: tmp.path(),
        free: Some(1),
        targets: &targets,
        discover: None,
        transcripts: None,
        now: idle_now(),
        room: None,
        table: None,
        demand: None,
        session_cwds: &[],
        session_cwd_unknown: None,
        link_dirs: &[],
    };
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        disk_tick(
            &look,
            disk::Config::default(),
            &mut |_| panic!("the window quit mid-removal"),
            &mut || Some(1),
        )
    }));
    assert!(died.is_err(), "the remover died");
    let ledger = std::fs::read_to_string(state.join(DISK_LEDGER)).expect("the ledger");
    let rows: Vec<&str> = ledger.lines().collect();
    assert_eq!(rows.len(), 2, "{ledger}");
    assert!(rows[0].contains("\"kind\":\"report\""), "{}", rows[0]);
    assert!(rows[1].contains("\"kind\":\"removing\""), "{}", rows[1]);
    let canonical = std::fs::canonicalize(&target).expect("canonical");
    assert!(
        rows[1].contains(&canonical.display().to_string()),
        "{}",
        rows[1]
    );
}

/// WINDOWS: ONE VOLUME, WHATEVER SPELLS IT, BY ITS SERIAL AND ITS GUID. A
/// path's volume is its serial through the one identity export and its
/// volume's GUID — the key its free-space reading carries — so a build
/// directory spelled verbatim (`\\?\C:\…`, as discovery records it) or in
/// capitals is on the volume a plain working directory measures (the drive
/// prefix this replaced read `\\?\C:` and `C:` apart and surveyed nothing
/// discovery found). A target whose serial agrees but whose GUID does not —
/// a cloned volume — is left out. NEGATIVE CONTROLS: a path that cannot be
/// read is on no volume, and neither is one whose GUID no table names.
#[cfg(windows)]
#[test]
fn a_windows_volume_is_named_by_its_serial_and_guid_in_every_spelling() {
    let tmp = tmp_dir("disk-volume-win");
    let target = disk::tests::lay_target(&tmp.path().join("repo").join("target"));
    let canonical = std::fs::canonicalize(&target).expect("canonical");
    let shouted = PathBuf::from(target.to_string_lossy().to_uppercase());
    let table = aterm_sysprobe::volumes::table();
    let id = volume_id(tmp.path(), Some(&table)).expect("a volume");
    assert_eq!(
        Some(id.0),
        aterm_sysprobe::volumes::file_id(tmp.path(), true).map(|i| i.dev)
    );
    let measured = disk::volume_of(tmp.path(), &table).expect("the volume read");
    assert!(id.1.is_some(), "{id:?}");
    assert_eq!(id.1, disk::target::VolumeGuid::of_key(&measured.key));
    assert_eq!(volume_id(&canonical, Some(&table)), Some(id));
    assert_eq!(volume_id(&shouted, Some(&table)), Some(id));
    let missing = tmp.path().join("missing");
    assert_eq!(volume_id(&missing, Some(&table)), None);
    assert_eq!(volume_id(tmp.path(), None), None, "no table, no GUID");
    let id_of = |path: &Path| volume_id(path, Some(&table));
    let kept = on_volume(
        tmp.path(),
        &[canonical.clone(), shouted.clone(), missing],
        &id_of,
    );
    assert_eq!(kept, [canonical.clone(), shouted.clone()]);
    // The canonical spelling read as on a clone: its serial, another GUID.
    let clone = disk::target::VolumeGuid::parse("{00000000-0000-0000-0000-00000000c10e}");
    let cloned = |path: &Path| {
        let (dev, guid) = id_of(path)?;
        Some((dev, if path == canonical { clone } else { guid }))
    };
    let kept = on_volume(tmp.path(), &[canonical.clone(), shouted.clone()], &cloned);
    assert_eq!(kept, [shouted]);
}

/// WINDOWS: THE TICK'S REMOVALS SEE CLOUD PLACEHOLDERS AS WHAT THEY ARE.
/// Below the floor the tick hands an idle build directory's row (its
/// `incremental/` to go) to its remover with this thread in
/// `PHCM_EXPOSE_PLACEHOLDERS` (2) — so
/// every `std` read a removal makes sees a placeholder as the reparse point
/// it is — and the thread's own mode (0, the process's default) is back
/// once the tick returns. NEGATIVE CONTROL: above the floor the tick
/// switches nothing and removes nothing.
#[cfg(windows)]
#[test]
fn the_disk_tick_removes_with_cloud_placeholders_exposed() {
    use aterm_dirfd::seam::thread_placeholder_mode;
    let tmp = tmp_dir("disk-tick-exposed");
    let state = tmp.path().join("state");
    let target = disk::tests::lay_target(&tmp.path().join("repo").join("target"));
    let table = aterm_sysprobe::volumes::table();
    let targets = [target.clone()];
    let gib = 1024 * 1024 * 1024;
    let cfg = disk::Config::default();
    let look = |free: u64| DiskLook {
        state: &state,
        volume: tmp.path(),
        free: Some(free),
        targets: &targets,
        discover: None,
        transcripts: None,
        now: idle_now(),
        room: None,
        table: Some(&table),
        demand: None,
        session_cwds: &[],
        session_cwd_unknown: None,
        link_dirs: &[],
    };
    assert_eq!(thread_placeholder_mode(), 0, "the process's default");
    let plenty = disk_tick(
        &look(cfg.warn_free_gib * gib),
        cfg,
        &mut |row| panic!("removed {} above the warning", row.path.display()),
        &mut disk::tests::no_pressure,
    );
    assert_eq!(plenty, DiskTick::Plenty);
    assert_eq!(thread_placeholder_mode(), 0);

    let mut modes = Vec::new();
    let DiskTick::Reclaimed(done, _) = disk_tick(
        &look(gib),
        cfg,
        &mut |row| {
            modes.push((row.path.clone(), thread_placeholder_mode()));
            Err(io::Error::other("a test's remover removes nothing"))
        },
        &mut || Some(gib),
    ) else {
        panic!("below the floor, the tick applies");
    };
    assert_eq!(modes.len(), 1, "{modes:?} {done:?}");
    let key = aterm_sysprobe::paths::windows_key;
    assert_eq!(key(&modes[0].0), key(&target), "{modes:?}");
    assert_eq!(modes[0].1, 2, "exposed while the tick removes: {modes:?}");
    assert_eq!(thread_placeholder_mode(), 0, "the thread's own mode back");
}

/// The tick frees only the volume it measured: a build directory on another
/// volume is not looked at when home runs short. The fixture measures `/dev`
/// (its own volume on macOS and Linux) against an idle build directory in the
/// scratch dir; the control measures the scratch dir's own volume.
#[cfg(unix)]
#[test]
fn the_disk_tick_leaves_a_target_on_another_volume_alone() {
    let tmp = tmp_dir("disk-volume");
    let state = tmp.path().join("state");
    let target = disk::tests::lay_target(&tmp.path().join("repo/target"));
    let now = idle_now();
    let elsewhere = Path::new("/dev");
    assert_ne!(
        volume_id(elsewhere, None),
        volume_id(tmp.path(), None),
        "the fixture needs two volumes"
    );
    let targets = [target.clone()];
    let look = |volume| DiskLook {
        state: &state,
        volume,
        free: Some(1),
        targets: &targets,
        discover: None,
        transcripts: None,
        now,
        room: None,
        table: None,
        demand: None,
        session_cwds: &[],
        session_cwd_unknown: None,
        link_dirs: &[],
    };
    let DiskTick::Reclaimed(done, _) = disk_tick(
        &look(elsewhere),
        disk::Config::default(),
        &mut |row| panic!("removed {} to free another volume", row.path.display()),
        &mut disk::tests::no_pressure,
    ) else {
        panic!("below the floor, the tick runs");
    };
    assert!(done.removed.is_empty(), "{done:?}");
    assert!(target.join("debug/incremental").exists());
    // CONTROL: measured on its own volume, the same idle cache goes.
    let DiskTick::Reclaimed(done, _) = disk_tick(
        &look(tmp.path()),
        disk::Config::default(),
        &mut real_remover(now),
        &mut || Some(1),
    ) else {
        panic!("below the floor, the tick runs");
    };
    assert_eq!(done.removed.len(), 1, "{done:?}");
    assert!(!target.join("debug/incremental").exists());
    assert!(target.join("debug/deps").exists());
}

/// THE OWNER'S VERB USES THE SAME UNIT: `aterm harness disk <dir> --apply
/// cargo-targets`, with `disk.apply = true`, reclaims the idle profile's
/// `incremental/` and nothing else, prints what went and what it freed —
/// "freed about": a count of the blocks it unlinked, not a measurement — and
/// journals the intent and the outcome.
#[cfg(unix)]
#[test]
fn the_verb_applies_cargo_targets_with_the_same_unit() {
    let tmp = tmp_dir("disk-verb");
    let target = disk::tests::lay_target(&tmp.path().join("repo/target"));
    let env = Env {
        now: idle_now(),
        ..env_at(&tmp)
    };
    std::fs::write(
        env.config.as_ref().expect("config"),
        "[disk]\napply = true\n",
    )
    .expect("write");
    let cmd = Cmd::Disk {
        targets: vec![target.clone()],
        apply: Some(disk::Class::CargoTargets),
        json: false,
    };
    let (ok, out, err) = go(&cmd, &env);
    assert!(ok, "{err}");
    assert!(out.contains("removed=1"), "{out}");
    assert!(out.contains("went: "), "{out}");
    assert!(out.contains("debug/incremental"), "{out}");
    assert!(!target.join("debug/incremental").exists());
    assert!(target.join("debug/deps/liblay.rlib").exists());
    assert!(target.join("CACHEDIR.TAG").exists());
    let ledger = std::fs::read_to_string(env.disk_ledger()).expect("ledger");
    let kinds: Vec<bool> = [
        "\"kind\":\"report\"",
        "\"kind\":\"removing\"",
        "\"kind\":\"removed\"",
    ]
    .iter()
    .map(|k| ledger.contains(k))
    .collect();
    assert_eq!(kinds, [true, true, true], "{ledger}");
}

/// NOTHING IS REMOVED WITHOUT ITS INTENT ON RECORD. When the disk journal
/// cannot take a row — the state directory cannot be made (here a FILE sits
/// where it should be), or the append fails on a full disk — the tick removes
/// nothing: each row it would have reclaimed is a denial saying the intent
/// could not be journalled, and the cache stays.
#[cfg(unix)]
#[test]
fn the_tick_removes_nothing_whose_intent_it_cannot_journal() {
    let tmp = tmp_dir("disk-unjournalled");
    let state = tmp.path().join("state");
    std::fs::write(&state, b"not a directory").expect("a file where the state dir goes");
    let target = disk::tests::lay_target(&tmp.path().join("repo/target"));
    let targets = [target.clone()];
    let now = idle_now();
    let look = DiskLook {
        state: &state,
        volume: tmp.path(),
        free: Some(1),
        targets: &targets,
        discover: None,
        transcripts: None,
        now,
        room: None,
        table: None,
        demand: None,
        session_cwds: &[],
        session_cwd_unknown: None,
        link_dirs: &[],
    };
    let DiskTick::Reclaimed(done, _) = disk_tick(
        &look,
        disk::Config::default(),
        &mut real_remover(now),
        &mut || Some(1),
    ) else {
        panic!("below the floor, the tick runs");
    };
    assert!(done.removed.is_empty(), "{done:?}");
    assert!(
        done.denials
            .iter()
            .any(|d| d.code() == "unjournalled" && d.describe().contains("journal")),
        "{done:?}"
    );
    assert!(target.join("debug/incremental").exists(), "the cache stays");
}

/// The verb, too: with the disk journal unopenable (a directory sits at its
/// path), `--apply cargo-targets` removes nothing and says why.
#[test]
fn the_verb_removes_nothing_whose_intent_it_cannot_journal() {
    let tmp = tmp_dir("disk-verb-unjournalled");
    let target = disk::tests::lay_target(&tmp.path().join("repo/target"));
    let env = Env {
        now: idle_now(),
        ..env_at(&tmp)
    };
    std::fs::write(
        env.config.as_ref().expect("config"),
        "[disk]\napply = true\n",
    )
    .expect("write");
    std::fs::create_dir_all(env.disk_ledger()).expect("a directory where the ledger goes");
    let cmd = Cmd::Disk {
        targets: vec![target.clone()],
        apply: Some(disk::Class::CargoTargets),
        json: false,
    };
    let (ok, out, _err) = go(&cmd, &env);
    assert!(ok);
    assert!(out.contains("removed=0"), "{out}");
    assert!(target.join("debug/incremental").exists(), "the cache stays");
}

/// `--json` carries what each row kept and why: a profile a build holds
/// (skipped) and an undeletable entry (trouble) sit beside what went and what
/// it freed, so a partial reclaim reads as partial. Unix-only: it holds a
/// cargo lock and uses mode bits, and the reclaim refuses on other OSes.
#[cfg(unix)]
#[test]
fn the_verb_json_carries_what_each_row_went_kept_and_why() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tmp_dir("disk-verb-json");
    let target = disk::tests::lay_target(&tmp.path().join("repo/target"));
    disk::tests::lay_profile(&target.join("release"));
    let target = std::fs::canonicalize(&target).expect("canonical");
    let hold = disk::target::Build::holds(&target.join("release/.cargo-lock"));
    let stuck = target.join("debug/incremental/stuck");
    std::fs::create_dir_all(&stuck).expect("stuck");
    std::fs::write(stuck.join("file"), b"kept").expect("file");
    std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let env = Env {
        now: idle_now(),
        ..env_at(&tmp)
    };
    std::fs::write(
        env.config.as_ref().expect("config"),
        "[disk]\napply = true\n",
    )
    .expect("write");
    let cmd = Cmd::Disk {
        targets: vec![target.clone()],
        apply: Some(disk::Class::CargoTargets),
        json: true,
    };
    let (ok, out, err) = go(&cmd, &env);
    std::fs::set_permissions(&stuck, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    drop(hold);
    assert!(ok, "{err}");
    let applied = out
        .lines()
        .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
        .find(|v| v.get("kind").and_then(Value::as_str) == Some("disk-apply"))
        .unwrap_or_else(|| panic!("no disk-apply object: {out}"));
    let rows = applied
        .get("rows")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("no rows: {applied:?}"));
    assert_eq!(rows.len(), 1, "{applied:?}");
    let row = &rows[0];
    let texts = |k: &str| -> Vec<String> {
        row.get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(
        row.get("path").and_then(Value::as_str),
        Some(target.display().to_string().as_str()),
        "{row:?}"
    );
    assert!(
        row.get("freed_bytes").and_then(Value::as_u64).is_some(),
        "{row:?}"
    );
    assert!(
        texts("skipped")
            .iter()
            .any(|s| s.contains("release") && s.contains("a build holds")),
        "{row:?}"
    );
    assert!(
        texts("trouble").iter().any(|s| s.contains("stuck")),
        "{row:?}"
    );
}

/// [`on_volume`] keeps the targets whose volume id is the measured one's, and
/// nothing when an id cannot be read.
#[test]
fn only_the_measured_volumes_targets_are_candidates() {
    let id_of = |p: &Path| -> Option<u32> {
        match p.to_str()? {
            s if s.starts_with("/home") => Some(1),
            s if s.starts_with("/Volumes/ext") => Some(2),
            _ => None,
        }
    };
    let targets = [
        PathBuf::from("/home/a/repo/target"),
        PathBuf::from("/Volumes/ext/repo/target"),
        PathBuf::from("/unreadable/repo/target"),
    ];
    assert_eq!(
        on_volume(Path::new("/home/a"), &targets, &id_of),
        vec![PathBuf::from("/home/a/repo/target")]
    );
    assert_eq!(
        on_volume(Path::new("/Volumes/ext"), &targets, &id_of),
        vec![PathBuf::from("/Volumes/ext/repo/target")]
    );
    assert!(on_volume(Path::new("/unreadable"), &targets, &id_of).is_empty());
}

#[test]
fn the_disk_knobs_are_read_from_aterm_toml() {
    let tmp = tmp_dir("disk-knobs");
    let env = env_at(&tmp);
    let path = env.config.clone().expect("a config path");
    let read = |text: &str| {
        std::fs::write(&path, text).expect("write");
        let reading = disk_config(&env);
        (reading.config, reading.notes)
    };
    let (cfg, notes) = read("[disk]\napply = false\nwarn_free_gib = 5\ntarget_stale_days = 30\n");
    assert_eq!(cfg.warn_free_gib, 5);
    assert_eq!(cfg.target_stale_days, 30);
    assert!(!cfg.apply);
    assert_eq!(
        cfg.auto_free_gib,
        disk::DEFAULT_AUTO_FREE_GIB,
        "the default floor"
    );
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(
        read("[disk]\nauto_free_gib = 0\n").0.auto_free_gib,
        0,
        "the floor switched off"
    );
    // A floor below zero is never crossed: `-1` written to stop the removal
    // stops it (it read as the default, 10, and left the removal on).
    let (cfg, notes) = read("[disk]\nauto_free_gib = -3\n");
    assert_eq!(
        cfg.auto_free_gib, 0,
        "a negative floor is off, never the default"
    );
    assert!(!cfg.below_auto_floor(Some(0)));
    assert!(
        notes.iter().any(|n| n.contains("disk.auto_free_gib")),
        "{notes:?}"
    );
    // The dotted spelling.
    assert!(read("disk.apply = true\n").0.apply);

    // Each read as its default and said, none turning the removal off: a
    // negative stale window, a warning threshold of another type (it removes
    // nothing), and a key `[disk]` does not have.
    let d = disk::Config::default();
    let (cfg, notes) = read(
        "[disk]\napply = true\nwarn_free_gib = \"lots\"\ntarget_stale_days = -1\nauto_free_gb = 0\n",
    );
    assert_eq!(cfg, disk::Config { apply: true, ..d });
    for key in [
        "disk.warn_free_gib",
        "disk.target_stale_days",
        "disk.auto_free_gb",
    ] {
        assert!(notes.iter().any(|n| n.contains(key)), "{key}: {notes:?}");
    }
    // The same keys under another table are not the knobs.
    let (cfg, notes) = read("[harness]\ntarget_stale_days = 9\nwarn_free_gib = 9\napply = true\n");
    assert_eq!(cfg, d);
    assert!(notes.is_empty(), "{notes:?}");
    // No file is every default, unremarked.
    std::fs::remove_file(&path).expect("remove");
    assert_eq!(
        disk_config(&env),
        DiskReading {
            config: d,
            floor_gib: Some(disk::DEFAULT_AUTO_FREE_GIB),
            notes: Vec::new(),
            off_by_fault: false,
            written: disk::Written::default(),
        }
    );
}

/// The `[disk]` knobs are read by the window's TOML parser, not a line
/// reader: an inline table, quoted keys and `_` separators read the same. The
/// line reader this replaced read none of them, so `auto_free_gib = 0` written
/// inline left the automatic removal on at the 10 GiB default, and
/// `target_stale_days = 3_0` read as one day.
#[test]
fn every_spelling_the_window_parser_reads_reads_the_same_knobs() {
    for text in [
        "disk = { apply = true, auto_free_gib = 0, target_stale_days = 30, \
         dead_build_dirs = false }\n",
        "[disk]\napply = true\n\"auto_free_gib\" = 0\ntarget_stale_days = 3_0\n\
         'dead_build_dirs' = false\n",
    ] {
        let DiskReading {
            config: cfg,
            floor_gib,
            notes,
            off_by_fault,
            written,
        } = disk_config_from(text);
        assert!(!off_by_fault, "{text:?}");
        assert_eq!(floor_gib, Some(0), "{text:?}");
        assert_eq!(
            written,
            disk::Written {
                auto_free_gib: true,
                warn_free_gib: false
            },
            "{text:?}"
        );
        assert_eq!(
            (
                cfg.apply,
                cfg.auto_free_gib,
                cfg.target_stale_days,
                cfg.dead_build_dirs
            ),
            (true, 0, 30, false),
            "{text:?}"
        );
        assert!(notes.is_empty(), "{text:?}: {notes:?}");
    }
}

/// A removal knob that cannot be read turns every removal off — `apply`, the
/// automatic floor and the whole-directory reclaim — whatever the rest of the
/// file grants, and the reading names it: a file the parser refuses (a typo
/// elsewhere, a key said twice, a key with no value), a `disk` that is not a
/// table, a removal knob of another type, and one TOML files under another
/// table. The line reader read `[theme] disk.target_stale_days = 0` as the
/// knob; the parser files it as `theme.disk.target_stale_days`, and a
/// misplaced off switch must not become the default by being ignored. A
/// misplaced warning threshold removes nothing, so it only says so. The floor
/// the file sets is unknown where the file or `auto_free_gib` itself cannot be
/// read, and read as written where another knob cannot.
#[test]
fn a_disk_knob_that_cannot_be_read_turns_every_removal_off() {
    const GRANT: &str = "[disk]\napply = true\nauto_free_gib = 20\ndead_build_dirs = true\ntarget_stale_days = 30\n";
    let grant = disk_config_from(GRANT);
    assert_eq!(
        (
            grant.config.apply,
            grant.config.auto_free_gib,
            grant.config.dead_build_dirs
        ),
        (true, 20, true),
        "the grant alone"
    );
    assert_eq!(grant.floor_gib, Some(20));
    assert!(grant.notes.is_empty(), "{:?}", grant.notes);
    assert!(!grant.off_by_fault);
    let refused = format!("{GRANT}font_px = \n");
    let default = Some(disk::DEFAULT_AUTO_FREE_GIB);
    // Each: the text, what its one note names (the line where the parser
    // stopped, or the key's whole path; for a `disk` that is not a table,
    // only that there is one note), and the floor read.
    let off = [
        // The parser refuses these: no floor is read.
        (refused.as_str(), Some("line 6"), None),
        (
            "[disk]\napply = true\ntarget_stale_days = 3\ntarget_stale_days = 4\n",
            Some("line 4"),
            None,
        ),
        ("[disk]\napply = yes\n", Some("line 2"), None),
        (
            "[disk]\napply = true\ntarget_stale_days\n",
            Some("line 3"),
            None,
        ),
        // Refused at the end of the input, which is on the last line.
        (
            "[disk]\napply = true\nauto_free_gib = [0\n",
            Some("line 3"),
            None,
        ),
        // Not a table.
        ("disk = 5\n", None, None),
        ("[[disk]]\napply = true\n", None, None),
        // A removal knob of another type: the floor is unknown only where it
        // is that knob.
        (
            "[disk]\napply = true\nauto_free_gib = \"off\"\n",
            Some("disk.auto_free_gib"),
            None,
        ),
        ("disk = { apply = \"yes\" }\n", Some("disk.apply"), default),
        (
            "[disk]\napply = true\ndead_build_dirs = \"no\"\n",
            Some("disk.dead_build_dirs"),
            default,
        ),
        (
            "[disk]\napply = true\nauto_free_gib = 20\ntarget_stale_days = 30.0\n",
            Some("disk.target_stale_days"),
            Some(20),
        ),
        // Filed by TOML under another table.
        (
            "[disk]\napply = true\nauto_free_gib = 20\n[theme]\ndisk.target_stale_days = 0\n",
            Some("theme.disk.target_stale_days"),
            Some(20),
        ),
        (
            "[disk]\napply = true\n[theme]\nname = \"x\"\ndisk.auto_free_gib = 0\n",
            Some("theme.disk.auto_free_gib"),
            None,
        ),
        (
            "[disk]\napply = true\n[theme.dark]\ndisk = { dead_build_dirs = false }\n",
            Some("theme.dark.disk.dead_build_dirs"),
            default,
        ),
        (
            "[disk]\napply = true\n[[keybind]]\nkey = \"a\"\ndisk.apply = true\n",
            Some("keybind[0].disk.apply"),
            default,
        ),
    ];
    for (text, named, floor) in off {
        let DiskReading {
            config: cfg,
            floor_gib,
            notes,
            off_by_fault,
            ..
        } = disk_config_from(text);
        assert!(off_by_fault, "{text:?}");
        assert_eq!(
            (cfg.apply, cfg.auto_free_gib, cfg.dead_build_dirs),
            (false, 0, false),
            "{text:?}"
        );
        assert!(!cfg.below_auto_floor(Some(0)), "{text:?}");
        assert_eq!(floor_gib, floor, "{text:?}");
        assert_eq!(notes.len(), 1, "{text:?}: {notes:?}");
        if let Some(named) = named {
            assert!(
                notes[0].contains(named),
                "{text:?} names {named}: {notes:?}"
            );
        }
    }
    // Misplaced, the stale window is not read as the knob.
    let misplaced = disk_config_from(
        "[disk]\napply = true\nauto_free_gib = 20\n[theme]\ndisk.target_stale_days = 0\n",
    );
    assert_eq!(
        misplaced.config.target_stale_days,
        disk::DEFAULT_TARGET_STALE_DAYS
    );

    let DiskReading {
        config: cfg,
        floor_gib,
        notes,
        off_by_fault,
        written,
    } = disk_config_from("[disk]\napply = true\n[theme]\ndisk.warn_free_gib = 5\n");
    assert_eq!(
        written,
        disk::Written::default(),
        "misplaced is not written"
    );
    assert!(
        !off_by_fault,
        "a misplaced warning threshold removes nothing"
    );
    assert_eq!(
        cfg,
        disk::Config {
            apply: true,
            ..disk::Config::default()
        }
    );
    assert_eq!(floor_gib, default);
    assert!(
        notes.iter().any(|n| n.contains("theme.disk.warn_free_gib")),
        "{notes:?}"
    );
}

/// The window's disk watch ends a `disk.low` line only on a volume at or
/// above the floor the file sets ([`DiskReading::at_or_above_floor`]). Where
/// the reading turned the removal off because the file could not be read, the
/// effective floor is `0`, which no figure is below; the floor the owner wrote
/// is unknown, so no figure is at or above it, and a short volume is never
/// said to have space back. Where the floor was read, the answer is the one
/// [`disk::Config::below_auto_floor`] gives for it, whatever else turned the
/// removal off.
#[test]
fn a_floor_the_file_does_not_say_never_reads_as_space_back() {
    const GIB: u64 = 1 << 30;
    let tmp = tmp_dir("disk-floor");
    let env = env_at(&tmp);
    let path = env.config.clone().expect("a config path");
    std::fs::create_dir_all(&path).expect("a directory where the file goes");
    let unread = [
        disk_config_from("[disk]\nauto_free_gib = 20\nfont_px = \n"),
        disk_config_from("disk = 5\n"),
        disk_config_from("[disk]\nauto_free_gib = \"off\"\n"),
        disk_config_from("[theme]\ndisk.auto_free_gib = 50\n"),
        disk_config(&env),
    ];
    for reading in &unread {
        assert_eq!(reading.floor_gib, None, "{reading:?}");
        for free in [Some(GIB), Some(500 * GIB), Some(u64::MAX), None] {
            // The control: the effective floor alone reads as space back.
            assert!(!reading.config.below_auto_floor(free), "{reading:?}");
            assert!(!reading.at_or_above_floor(free), "{reading:?} at {free:?}");
        }
    }
    std::fs::remove_dir(&path).expect("remove");

    // The floor read, the removal off for another knob: the floor decides.
    let reading = disk_config_from("[disk]\nauto_free_gib = 20\napply = \"yes\"\n");
    assert_eq!(reading.config.auto_free_gib, 0, "the removal is off");
    assert!(!reading.at_or_above_floor(Some(5 * GIB)));
    assert!(reading.at_or_above_floor(Some(20 * GIB)));
    // Read as written, the answer is the effective floor's.
    for text in [
        "",
        "[disk]\nauto_free_gib = 20\n",
        "[disk]\nauto_free_gib = 0\n",
        "[disk]\nauto_free_gib = -3\n",
        "disk = { auto_free_gib = 30, target_stale_days = 3_0 }\n",
    ] {
        let reading = disk_config_from(text);
        for free in [Some(0), Some(5 * GIB), Some(25 * GIB), Some(u64::MAX), None] {
            assert_eq!(
                reading.at_or_above_floor(free),
                !reading.config.below_auto_floor(free),
                "{text:?} at {free:?}"
            );
        }
    }
}

/// An aterm.toml that exists and cannot be read as text — not UTF-8, or not
/// a file — grants no removal, and the reading names it; the hand-run verb
/// prints each note on stderr, its `--json` stdout untouched.
#[test]
fn an_unreadable_aterm_toml_grants_no_removal_and_the_verb_says_so() {
    let tmp = tmp_dir("disk-unreadable");
    let env = env_at(&tmp);
    let path = env.config.clone().expect("a config path");
    std::fs::write(&path, b"[disk]\napply = true\n# \xff\n").expect("write");
    let DiskReading {
        config: cfg,
        floor_gib,
        notes,
        off_by_fault,
        ..
    } = disk_config(&env);
    assert!(off_by_fault);
    assert_eq!(floor_gib, None);
    assert_eq!(
        (cfg.apply, cfg.auto_free_gib, cfg.dead_build_dirs),
        (false, 0, false)
    );
    let named = path.display().to_string();
    assert!(notes.iter().any(|n| n.contains(&named)), "{notes:?}");
    std::fs::remove_file(&path).expect("remove");
    std::fs::create_dir_all(&path).expect("a directory where the file goes");
    let DiskReading {
        config: cfg, notes, ..
    } = disk_config(&env);
    assert_eq!(cfg.auto_free_gib, 0);
    assert!(notes.iter().any(|n| n.contains(&named)), "{notes:?}");
    std::fs::remove_dir(&path).expect("remove");

    std::fs::write(&path, "disk = { auto_free_gib = \"off\" }\n").expect("write");
    let cmd = Cmd::Disk {
        targets: vec![disk::tests::lay_target(&tmp.path().join("repo/target"))],
        apply: None,
        json: true,
    };
    let (ok, out, err) = go(&cmd, &env);
    assert!(ok, "{err}");
    assert!(err.contains("disk.auto_free_gib"), "{err}");
    assert!(!out.contains("disk.auto_free_gib"), "{out}");
}

// ---------------------------------------------------------------------------
// The hand-run upgrade
// ---------------------------------------------------------------------------

/// `harness.enabled = false` appended below another table's header still
/// switches the harness off, and the verb says where it found it. TOML files
/// that line under the last header (`theme.harness.enabled`), but a kill
/// switch must work wherever it is written: the policy's reader honours it and
/// names it (`supervise::config`'s `a_misplaced_harness_key_limits_but_never_grants`),
/// and a refused `upgrade` prints that note on stderr (and says nothing when
/// the switch sits where TOML reads it).
#[test]
fn a_kill_switch_written_below_another_header_still_reads_off() {
    let tmp = tmp_dir("master-misplaced");
    let env = env_at(&tmp);
    std::fs::create_dir_all(tmp.path().join("home/.claude/sessions")).expect("home");
    let config = env.config.clone().expect("config path");
    std::fs::write(
        &config,
        "font_px = 14\n[theme]\nname = \"x\"\nharness.enabled = false\n",
    )
    .expect("write config");
    assert!(!SupervisorConfig::from_path(Some(&config)).0.enabled);
    let (ok, out, err) = go(&upgrade_cmd(true, false), &env);
    assert!(ok, "{out}{err}");
    assert!(out.contains(UPGRADE_BYPASSED), "{out}");
    assert!(err.contains("theme.harness.enabled"), "{err}");
    // Negative control: the switch where TOML reads it is off, unremarked.
    std::fs::write(&config, "[harness]\nenabled = false\n").expect("write config");
    assert!(!SupervisorConfig::from_path(Some(&config)).0.enabled);
    let (_, out, err) = go(&upgrade_cmd(true, false), &env);
    assert!(out.contains(UPGRADE_BYPASSED), "{out}");
    assert!(!err.contains("theme.harness"), "{err}");
}

/// Run one command and answer `(exit code, stdout, stderr)`, the code as its
/// `Debug` form so a test can tell 1 from 75.
fn go_code(cmd: &Cmd, env: &Env) -> (String, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run(cmd, env, &mut out, &mut err);
    (
        format!("{code:?}"),
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

/// `upgrade` with no session under `env`'s scratch home: a sweep that runs
/// visits nothing and reaches no live aterm.
fn upgrade_cmd(dry_run: bool, json: bool) -> Cmd {
    Cmd::Upgrade {
        sid: String::new(),
        dry_run,
        json,
    }
}

/// A hand-run `upgrade` stands down under `[harness] enabled = false` as the
/// window's host does: one `refused:bypassed` line, no lock taken, exit 1.
#[test]
fn a_hand_run_upgrade_stands_down_while_the_master_switch_is_off() {
    let tmp = tmp_dir("upgrade-master");
    let env = env_at(&tmp);
    std::fs::create_dir_all(tmp.path().join("home/.claude/sessions")).expect("home");
    let lock = env.state.join("upgrade").join("sweep.lock");
    let config = env.config.clone().expect("config path");
    std::fs::write(&config, "[harness]\nenabled = false\n").expect("write config");

    let (code, out, err) = go_code(&upgrade_cmd(false, false), &env);
    assert_eq!(code, format!("{:?}", ExitCode::from(1)), "{out}{err}");
    assert_eq!(
        out,
        "upgrade pid=0 tab=- session=- from=- to=- step=refused:bypassed\n"
    );
    assert!(!lock.exists(), "the refused sweep took the lock");
    // The JSON form carries the same verdict.
    let (_, out, _) = go_code(&upgrade_cmd(false, true), &env);
    assert!(out.contains("\"step\":\"refused:bypassed\""), "{out}");

    // A dry run acts on nothing: it says a real run would not act, and answers.
    let (ok, out, err) = go(&upgrade_cmd(true, false), &env);
    assert!(ok, "{err}");
    assert!(out.contains("step=refused:bypassed"), "{out}");
    assert!(!lock.exists(), "a dry run takes no lock");

    // Positive control: the same scratch home with the switch on sweeps.
    std::fs::write(&config, "[harness]\nenabled = true\n").expect("write config");
    let (ok, out, err) = go(&upgrade_cmd(false, false), &env);
    assert!(ok, "{out}{err}");
    assert!(out.is_empty(), "no session, nothing to report: {out}");
    assert!(lock.exists(), "a sweep that ran takes the lock");
}

/// A hand-run `upgrade` leaves out the sessions of an agent whose OWN switch
/// is off (`[harness.claude] enabled = false`), says so on stderr, and still
/// answers: a Claude Code restart an earlier sweep left part way — its agent
/// gone, its time up, so a dry run says `would-fail:` — is not this pass's,
/// and stderr names it as stopped until the switch is back on (no lane this
/// pass runs carries it). NEGATIVE CONTROLS: with Codex's switch off
/// instead, or none, the same restart is the pass's and named stopped by
/// nothing; with the master switch off as well the pass is refused as ever.
#[cfg(unix)]
#[test]
fn a_hand_run_upgrade_leaves_out_an_agent_whose_switch_is_off() {
    const SESSION: &str = "03396a15-856e-4f1b-8174-ae9a3e4b369f";
    let tmp = tmp_dir("upgrade-agent-off");
    let env = env_at(&tmp);
    std::fs::create_dir_all(tmp.path().join("home/.claude/sessions")).expect("home");
    let config = env.config.clone().expect("config path");
    let dir = env.state.join("upgrade");
    std::fs::create_dir_all(&dir).expect("state");
    let mut gone = aterm_uds::spawnfd::command("true")
        .spawn()
        .expect("a process to outlive");
    let pid = gone.id();
    gone.wait().expect("reaped");
    std::fs::write(
        dir.join(format!("{SESSION}.json")),
        format!(r#"{{"phase":"exiting","at":1,"tab":"s-aaaa","pid":{pid}}}"#),
    )
    .expect("write state");
    let swept = |toml: &str| {
        std::fs::write(&config, toml).expect("write config");
        let (ok, out, err) = go(&upgrade_cmd(true, false), &env);
        assert!(ok, "{toml}: {out}{err}");
        (out.contains(&format!("session={SESSION} ")), err)
    };

    let (carried, err) = swept("[harness.claude]\nenabled = false\n");
    assert!(!carried, "Claude Code is off: {err}");
    assert!(
        err.contains("`[harness.claude] enabled` reads off")
            && err.contains("Claude Code's sessions are left as they are"),
        "{err}"
    );
    assert!(
        err.contains(&format!(
            "the restart of session {SESSION} an earlier sweep began stays stopped until the \
             switch is back on"
        )),
        "the stranded restart is named: {err}"
    );
    let (carried, err) = swept("[harness.codex]\nenabled = false\n");
    assert!(carried, "Codex's switch is not Claude Code's: {err}");
    assert!(err.contains("`[harness.codex] enabled` reads off"), "{err}");
    assert!(!err.contains("[harness.claude]"), "{err}");
    assert!(!err.contains("stays stopped"), "nothing stranded: {err}");
    let (carried, err) = swept("[harness]\nenabled = true\n");
    assert!(carried);
    assert!(!err.contains("reads off"), "{err}");

    std::fs::write(
        &config,
        "[harness]\nenabled = false\n[harness.claude]\nenabled = false\n",
    )
    .expect("write config");
    let (code, out, err) = go_code(&upgrade_cmd(false, false), &env);
    assert_eq!(code, format!("{:?}", ExitCode::from(1)), "{out}{err}");
    assert_eq!(
        out,
        "upgrade pid=0 tab=- session=- from=- to=- step=refused:bypassed\n"
    );
    assert!(err.contains("`[harness] enabled` reads off"), "{err}");
}

/// A one-shot `upgrade` whose sweep never ran does not exit 0: the lock held by
/// another actor (the window's host) exits 75, atpkg's "try again later"; a
/// state directory that cannot hold the lock exits 1.
#[test]
fn a_hand_run_upgrade_that_could_not_sweep_does_not_exit_zero() {
    let tmp = tmp_dir("upgrade-busy");
    let env = env_at(&tmp);
    std::fs::create_dir_all(tmp.path().join("home/.claude/sessions")).expect("home");
    let dir = env.state.join("upgrade");
    std::fs::create_dir_all(&dir).expect("state");
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("sweep.lock"))
        .expect("lock file");
    #[expect(
        clippy::disallowed_methods,
        reason = "the test's stand-in sweep: it lets go by unlock() before its drop"
    )]
    held.try_lock().expect("the test holds the sweep lock");
    let (code, out, _) = go_code(&upgrade_cmd(false, false), &env);
    assert!(out.contains("step=busy:another-sweep"), "{out}");
    assert_eq!(
        code,
        format!("{:?}", ExitCode::from(atpkg::lock::CONTENDED_EXIT)),
        "{out}"
    );
    // Positive control: the lock released, the same sweep runs and exits 0.
    // Released by `LOCK_UN` as the sweep's guard does: the sweep takes one try
    // (`upgrade_drive::sweep_lock`), and a lock released only by close stays
    // held while any test thread's child has yet to exec.
    held.unlock().expect("the test releases the sweep lock");
    drop(held);
    let (ok, out, err) = go(&upgrade_cmd(false, false), &env);
    assert!(ok, "{out}{err}");
    assert!(out.is_empty(), "{out}");

    // A lock that cannot be opened, and a state directory that cannot be
    // made, are not transient: exit 1.
    std::fs::remove_file(dir.join("sweep.lock")).expect("rm lock");
    std::fs::create_dir(dir.join("sweep.lock")).expect("a directory where the lock goes");
    let (code, out, _) = go_code(&upgrade_cmd(false, false), &env);
    assert!(out.contains("step=busy:lock-unopenable"), "{out}");
    assert_eq!(code, format!("{:?}", ExitCode::from(1)), "{out}");
    std::fs::remove_dir_all(&dir).expect("rm state");
    std::fs::write(&dir, "").expect("a file where the state directory goes");
    let (code, out, _) = go_code(&upgrade_cmd(false, false), &env);
    assert!(out.contains("step=busy:state-unwritable"), "{out}");
    assert_eq!(code, format!("{:?}", ExitCode::from(1)), "{out}");
}

// ---------------------------------------------------------------------------
// ledger
// ---------------------------------------------------------------------------

/// `ledger` reads the engine's approval ledger (written by the supervisor's
/// approval loop) for one session, newest last. Negative controls: another
/// session's rows never appear, and a session with no ledger says so.
#[test]
fn the_ledger_verb_reads_the_supervisors_approval_ledger() {
    let tmp = tmp_dir("ledger");
    let env = env_at(&tmp);
    let root = env.aterm_state.clone().expect("injected");
    let mut warn = Vec::new();
    for (sid, command) in [
        ("@s-worker", "rm -rf /private/tmp/claude-502/x"),
        ("@s-worker", "git status"),
        ("@s-other", "cat secret-of-the-other-session"),
    ] {
        let path = approvals::path_under(&root, Some(sid)).expect("path");
        let mut l = approvals::Ledger::open(Some(&path), Some(sid), &mut warn);
        l.write(
            crate::supervise::journal::unix_ms(),
            &approvals::Row {
                rule_id: "read-only@v1",
                outcome: approvals::Outcome::Approved,
                command,
                reason: "",
                box_seq: 1,
            },
            &mut warn,
        );
    }
    assert!(warn.is_empty(), "{}", String::from_utf8_lossy(&warn));

    let ledger =
        |file: LedgerFile, count: usize, json: bool| go(&Cmd::Ledger { file, count, json }, &env);
    let (ok, out, _) = ledger(LedgerFile::Approvals(Some("@s-worker".into())), 20, false);
    assert!(ok);
    assert_eq!(out.lines().count(), 2, "{out}");
    assert!(out.contains("approved read-only@v1 rm -rf"), "{out}");
    assert!(
        out.lines().last().unwrap_or("").ends_with("git status"),
        "{out}"
    );
    assert!(!out.contains("secret-of-the-other"), "{out}");

    let (_, out, _) = ledger(LedgerFile::Approvals(Some("@s-worker".into())), 1, true);
    assert_eq!(
        out.lines().count(),
        1,
        "the count is the newest rows: {out}"
    );
    assert!(out.contains("\"command\":\"git status\""), "{out}");

    let (ok, out, _) = ledger(LedgerFile::Approvals(None), 20, false);
    assert!(ok);
    assert!(out.contains("s-test.jsonl"), "{out}");
}

/// The tail is bounded however long the file is, and a missing file is no
/// rows rather than an error.
#[test]
fn a_ledger_tail_keeps_only_the_newest_rows() {
    let tmp = tmp_dir("tail");
    let path = tmp.path().join("l.jsonl");
    assert!(tail_lines(&path, 5).expect("missing is empty").is_empty());
    let body: String = (0..10_000).map(|i| format!("{{\"n\":{i}}}\n\n")).collect();
    std::fs::write(&path, body).expect("write");
    let got = tail_lines(&path, 3).expect("read");
    assert_eq!(got, vec!["{\"n\":9997}", "{\"n\":9998}", "{\"n\":9999}"]);
    assert_eq!(
        tail_lines(&path, usize::MAX).expect("read").len(),
        READ_MAX_ROWS
    );
    assert!(tail_lines(&path, 0).expect("read").is_empty());
}

// ---------------------------------------------------------------------------
// The live upgrade: the owner's view and word
// ---------------------------------------------------------------------------

#[test]
fn a_deferral_is_a_span_with_a_unit_within_a_month() {
    for (v, secs) in [
        ("90s", 90),
        ("90", 90),
        ("30m", 1_800),
        ("6h", 21_600),
        ("2d", 172_800),
        ("30d", UPGRADE_MAX_DEFER_S),
    ] {
        assert_eq!(parse_defer(v), Ok(secs), "{v}");
    }
    for bad in ["", "0", "0h", "31d", "6 h", "6hours", "-1h", "h", "1.5h"] {
        assert!(parse_defer(bad).is_err(), "{bad:?}");
    }
}

/// A recorded upgrade under `env`'s state, as the window's upgrade step leaves one.
fn record_upgrade(env: &Env, session: &str, tab: &str, extra: &str) {
    let dir = env.state.join("upgrade");
    std::fs::create_dir_all(&dir).expect("state");
    let now = u64::try_from(env.now).expect("now");
    std::fs::write(
        dir.join(format!("{session}.json")),
        format!(
            r#"{{"phase":"pending","from":"2.1.281","to":"2.1.282","source":"managed","tab":"{tab}","salt":1,"pending_since":{},"wait":"not-idle:busy","wait_since":{}{extra}}}"#,
            now - 30_120,
            now - 30_000
        ),
    )
    .expect("state file");
}

/// A live stand-in Claude Code holding `session` on `version` in `tab`: this
/// test binary parked in `harness::upgrade_drive`'s own park test, its tab in
/// its environment, and Claude's session file for it under `home` with the
/// kernel's start time — what `--status` vets a recorded upgrade against.
#[cfg(unix)]
fn live_holder(
    home: &std::path::Path,
    session: &str,
    tab: &str,
    version: &str,
) -> aterm_uds::spawnfd::Child {
    live_holder_in(home, session, Some(tab), version)
}

/// [`live_holder`] in `tab`, or — `None` — in no aterm tab at all: no tab in
/// its environment, as a conversation resumed in another terminal app runs.
#[cfg(unix)]
fn live_holder_in(
    home: &std::path::Path,
    session: &str,
    tab: Option<&str>,
    version: &str,
) -> aterm_uds::spawnfd::Child {
    let mut park = aterm_uds::spawnfd::command(std::env::current_exe().expect("exe"));
    park.args(["harness::upgrade_drive::tests::park_when_asked", "--exact"])
        .env("UPGRADE_DRIVE_TEST_PARK", "1");
    match tab {
        Some(tab) => park.env("ATERM_PARENT_SESSION_ID", tab),
        None => park.env_remove("ATERM_PARENT_SESSION_ID"),
    };
    let mut child = park
        .setsid(true)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the stand-in agent");
    let pid = child.id();
    let started = (0..3_000).any(|_| {
        let read = atpkg::caller_shell::process_args(pid)
            .is_some_and(|a| a.env_var("UPGRADE_DRIVE_TEST_PARK").is_some());
        if !read {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        read
    });
    // The kernel's start time as Claude renders `procStart`: `ps` in the C
    // locale and UTC, whitespace squashed.
    let start = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .env("LC_ALL", "C")
            .env("TZ", "UTC"),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .ok()
    .map(|o| {
        String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    })
    .unwrap_or_default();
    if !started || start.is_empty() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("the stand-in agent {pid} never started");
    }
    std::fs::create_dir_all(home.join(".claude/sessions")).expect("sessions");
    std::fs::write(
        home.join(format!(".claude/sessions/{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{session}","cwd":"/","version":"{version}","status":"busy","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
        ),
    )
    .expect("session file");
    child
}

/// The worst give-up row fits its cut whole ([`GAVE_UP_DETAIL_BYTES`]): more
/// processes than are named, each with a longest name and command of four-byte
/// characters, and the count of the rest still ends the row.
#[test]
fn the_worst_give_up_row_fits_its_cut_whole() {
    use super::super::upgrade::{HELD_COMMAND_CHARS, HELD_NAMED, Held};
    let held: Vec<Held> = (0..HELD_NAMED + 2)
        .map(|i| Held {
            pid: u32::MAX - u32::try_from(i).expect("small"),
            name: "𝔷".repeat(40),
            age_s: u64::from(u32::MAX),
            command: "𝔷".repeat(HELD_COMMAND_CHARS * 2),
        })
        .collect();
    // Both flows' words; the quiet flow's give-up is the longer.
    for quiet in [false, true] {
        let lead = super::super::upgrade_drive::gave_up_words(
            super::super::upgrade::Agent::Claude,
            &[],
            quiet,
        );
        assert!(lead.len() < 512, "{quiet}: {} bytes: {lead}", lead.len());
        let detail = super::super::upgrade_drive::gave_up_words(
            super::super::upgrade::Agent::Claude,
            &held,
            quiet,
        );
        assert!(
            detail.len() <= GAVE_UP_DETAIL_BYTES,
            "{quiet}: {} bytes",
            detail.len()
        );
        assert_eq!(
            super::super::one_line(&detail, GAVE_UP_DETAIL_BYTES),
            detail
        );
        assert!(detail.ends_with("; and 2 more"), "{quiet}: {detail}");
    }
}

/// `upgrade --status` prints what the sweep recorded (how long behind, what it
/// waits on, the owner's word, whether stalled) for each upgrade a live Claude
/// Code still holds behind its target, taking no lock. The owner's word lands
/// under the lock and shows in the next `--status` and in `ledger upgrade`.
/// Negative controls: a recorded upgrade with no live holder is not listed; a
/// tab with nothing recorded is refused (exit 1), writing nothing.
#[cfg(unix)]
#[test]
fn the_owner_reads_the_recorded_upgrades_and_says_their_word() {
    let tmp = tmp_dir("upgrade-owner");
    let env = env_at(&tmp);
    let tab = "s-b5cf2faabac5ce5127bd";
    record_upgrade(&env, "03396a15", tab, "");
    let status = |sid: &str| Cmd::UpgradeOwner {
        sid: sid.to_string(),
        ask: OwnerAsk::Status,
        json: false,
    };
    let (ok, out, err) = go(&status(""), &env);
    assert!(ok, "{out}{err}");
    assert!(
        !out.contains("upgrade tab="),
        "no live holder: nothing to show: {out}"
    );
    assert!(err.is_empty(), "no sessions at all is a verdict: {err}");
    let home = env.home.clone().expect("home");
    let mut agent = live_holder(&home, "03396a15", tab, "2.1.281");
    let (ok, out, err) = go(&status(""), &env);
    assert!(ok, "{out}{err}");
    assert_eq!(
        out.lines().next(),
        Some(
            format!(
                "upgrade tab={tab} session=03396a15 from=2.1.281 to=2.1.282(managed) \
                 phase=pending pending_for=8h22m wait=not-idle:busy wait_for=8h20m request=- \
                 next_round=- stalled=overdue held_by=- release=- rung=land looked=- by=- \
                 point=- guard=- watch_at=due cause=own-work told=no-progress"
            )
            .as_str()
        )
    );
    assert!(
        !env.state.join("upgrade/sweep.lock").exists(),
        "a status read takes no lock"
    );
    let (ok, out, _) = go(&status("s-0000"), &env);
    assert!(ok && !out.contains("upgrade tab="), "{out}");

    let now = Cmd::UpgradeOwner {
        sid: tab.to_string(),
        ask: OwnerAsk::Now,
        json: false,
    };
    let (ok, out, err) = go(&now, &env);
    assert!(ok, "{out}{err}");
    assert!(out.contains(" request=now "), "{out}");
    let (_, out, _) = go(&status(tab), &env);
    assert!(out.contains(" request=now "), "{out}");

    let (_, out, _) = go(
        &Cmd::Ledger {
            file: LedgerFile::Upgrade,
            count: 5,
            json: false,
        },
        &env,
    );
    assert!(
        out.contains(&format!(
            " requested:now tab={tab} session=03396a15 2.1.281 -> 2.1.282(managed)"
        )),
        "{out}"
    );

    let (code, out, err) = go_code(
        &Cmd::UpgradeOwner {
            sid: "s-0000".to_string(),
            ask: OwnerAsk::Skip,
            json: false,
        },
        &env,
    );
    assert_eq!(code, format!("{:?}", ExitCode::from(1)), "{out}{err}");
    assert!(out.is_empty(), "{out}");
    let _ = agent.kill();
    let _ = agent.wait();
}

/// A record no look has read shows `stalled=unwatched` and `told=unwatched` in
/// `--status` once the Mac has been awake past the wake grace. With `[harness]
/// enabled` or `upgrade` off no window watches anything, and stderr names the
/// switch as the reason. Negative controls: both switches on, nothing else is
/// said; within the wake grace the record shows its last look; with no
/// `unwatched` row no switch is named.
#[cfg(unix)]
#[test]
fn an_unwatched_upgrade_says_so_and_a_switch_off_says_why() {
    use crate::harness::upgrade_drive::pin_awake_since;
    let tmp = tmp_dir("upgrade-unwatched");
    let env = env_at(&tmp);
    let tab = "s-b5cf2faabac5ce5127bd";
    let config = env.config.clone().expect("config path");
    record_upgrade(&env, "03396a15", tab, "");
    let home = env.home.clone().expect("home");
    let mut agent = live_holder(&home, "03396a15", tab, "2.1.281");
    let status = Cmd::UpgradeOwner {
        sid: String::new(),
        ask: OwnerAsk::Status,
        json: false,
    };
    let now = u64::try_from(env.now).expect("now");
    pin_awake_since(now - 86_400);
    for (toml, says) in [
        ("[harness]\nenabled = true\nupgrade = true\n", None),
        ("[harness]\nupgrade = false\n", Some("`[harness] upgrade`")),
        ("[harness]\nenabled = false\n", Some("`[harness] enabled`")),
        (
            "[harness.claude]\nenabled = false\n",
            Some("`[harness.claude] enabled` reads off"),
        ),
        // Another agent's switch is not this Claude Code row's.
        ("[harness.codex]\nenabled = false\n", None),
    ] {
        std::fs::write(&config, toml).expect("write config");
        let (ok, out, err) = go(&status, &env);
        assert!(ok, "{toml}: {out}{err}");
        assert!(out.contains(" stalled=unwatched "), "{toml}: {out}");
        assert!(out.contains(" told=unwatched"), "{toml}: {out}");
        match says {
            None => assert!(!err.contains("`[harness]"), "{toml}: {err}"),
            Some(words) => {
                assert!(err.contains(words), "{toml}: {err}");
            }
        }
    }
    // Woken a minute ago: inside the grace, the record's last look stands.
    pin_awake_since(now - 60);
    let (_, out, err) = go(&status, &env);
    assert!(out.contains(" stalled=overdue "), "{out}");
    assert!(
        !err.contains("`[harness]"),
        "no unwatched row, no switch named: {err}"
    );
    pin_awake_since(0);
    let _ = agent.kill();
    let _ = agent.wait();
}

/// A tab held by another supervisor's claim reads so in `--status` (from the
/// instance's `sessions` `supervisor=`), not `stalled=unwatched`. Negative
/// controls: with `[harness] enabled` or `upgrade` off the instance is not
/// asked, and `unwatched` with the switch's note stands; with no instance
/// answering, see `an_unwatched_upgrade_says_so_and_a_switch_off_says_why`.
#[cfg(unix)]
#[test]
fn a_tab_another_supervisor_claims_reads_so_unless_a_switch_is_off() {
    use crate::harness::upgrade_drive::pin_awake_since;
    let tmp = tmp_dir("upgrade-claimed");
    let tab = "s-b5cf2faabac5ce5127bd";
    // A unix-socket path must fit in 103 bytes: the instance's is under /tmp.
    let short = aterm_tempfile::Builder::new()
        .prefix("at-")
        .tempdir_in("/tmp")
        .expect("socket dir");
    // The instance: its one tab claimed by an external loop.
    let (sock, asked) =
        crate::harness::fake_instance(&short.path().join("a.sock"), "0badf00d", move |line| {
            if line == "sessions" {
                format!("OK 1\n1 {tab} - live claude supervisor=aterm-supervise:42 upgrade=-")
            } else {
                "ERR usage".to_string()
            }
        });
    let env = Env {
        sock: Some(sock),
        ..env_at(&tmp)
    };
    let config = env.config.clone().expect("config path");
    record_upgrade(&env, "03396a15", tab, "");
    let mut agent = live_holder(&env.home.clone().expect("home"), "03396a15", tab, "2.1.281");
    let status = Cmd::UpgradeOwner {
        sid: String::new(),
        ask: OwnerAsk::Status,
        json: false,
    };
    let now = u64::try_from(env.now).expect("now");
    pin_awake_since(now - 86_400);
    std::fs::write(&config, "[harness]\nenabled = true\nupgrade = true\n").expect("config");
    let (ok, out, err) = go(&status, &env);
    assert!(ok, "{out}{err}");
    for field in [
        " wait=claimed wait_for=- ",
        " stalled=overdue ",
        " cause=person ",
    ] {
        assert!(out.contains(field), "{field}: {out}");
    }
    assert!(!err.contains("`[harness]"), "{err}");
    assert_eq!(*asked.lock().expect("asked"), vec!["sessions"]);
    for (toml, switch) in [
        ("[harness]\nupgrade = false\n", "`[harness] upgrade`"),
        ("[harness]\nenabled = false\n", "`[harness] enabled`"),
    ] {
        std::fs::write(&config, toml).expect("config");
        let (ok, out, err) = go(&status, &env);
        assert!(ok, "{toml}: {out}{err}");
        assert!(out.contains(" stalled=unwatched "), "{toml}: {out}");
        assert!(err.contains(switch), "{toml}: {err}");
        assert_eq!(
            asked.lock().expect("asked").len(),
            1,
            "{toml}: asked nothing"
        );
    }
    pin_awake_since(0);
    let _ = agent.kill();
    let _ = agent.wait();
}

/// A conversation resumed outside aterm (`claude --resume` in another terminal)
/// is never `unwatched`: its holder is in no tab, so no aterm host will look at
/// it, and blaming aterm would be wrong. The row shows its own stall
/// (`overdue`). Negative control: the same record held in its tab is
/// `unwatched` after a day awake.
#[cfg(unix)]
#[test]
fn a_conversation_resumed_outside_aterm_is_never_told_unwatched() {
    use crate::harness::upgrade_drive::pin_awake_since;
    let tmp = tmp_dir("upgrade-outside");
    let env = env_at(&tmp);
    let tab = "s-b5cf2faabac5ce5127bd";
    let config = env.config.clone().expect("config path");
    std::fs::write(&config, "[harness]\nenabled = true\nupgrade = true\n").expect("config");
    record_upgrade(&env, "03396a15", tab, "");
    let home = env.home.clone().expect("home");
    let status = Cmd::UpgradeOwner {
        sid: String::new(),
        ask: OwnerAsk::Status,
        json: false,
    };
    let now = u64::try_from(env.now).expect("now");
    pin_awake_since(now - 86_400);
    let mut outside = live_holder_in(&home, "03396a15", None, "2.1.281");
    let (ok, out, err) = go(&status, &env);
    assert!(ok, "{out}{err}");
    assert!(out.contains(&format!("upgrade tab={tab} ")), "shown: {out}");
    assert!(!out.contains("unwatched"), "no host watches it: {out}");
    assert!(out.contains(" stalled=overdue "), "its own stall: {out}");
    let _ = outside.kill();
    let _ = outside.wait();
    // Negative control: held in its tab, a host's to watch.
    let mut inside = live_holder(&home, "03396a15", tab, "2.1.281");
    let (ok, out, err) = go(&status, &env);
    assert!(ok, "{out}{err}");
    assert!(out.contains(" stalled=unwatched "), "{out}");
    pin_awake_since(0);
    let _ = inside.kill();
    let _ = inside.wait();
}

/// The owner's word on a tab whose agent's OWN switch is off is recorded and
/// waits, as under the master switch: stderr names that agent's switch, for a
/// Claude Code tab and a Codex tab alike. NEGATIVE CONTROLS: the other
/// agent's switch off says nothing of the tab; the master switch off is named
/// as ever, whatever an agent's own switch says.
#[test]
fn an_owners_word_on_a_tab_whose_agent_is_off_names_its_switch() {
    let tmp = tmp_dir("upgrade-owner-agent-off");
    let env = env_at(&tmp);
    let config = env.config.clone().expect("config path");
    let claude_tab = "s-b5cf2faabac5ce5127bd";
    let codex_tab = "s-c0dec0dec0dec0dec0de";
    let codex_record = format!("codex-{codex_tab}");
    let now = |tab: &str| Cmd::UpgradeOwner {
        sid: tab.to_string(),
        ask: OwnerAsk::Now,
        json: false,
    };
    let master = "`[harness] enabled` reads off";
    for (tab, toml, says) in [
        (
            claude_tab,
            "[harness.claude]\nenabled = false\n",
            Some("`[harness.claude] enabled` reads off"),
        ),
        (claude_tab, "[harness.codex]\nenabled = false\n", None),
        (
            codex_tab,
            "[harness.codex]\nenabled = false\n",
            Some("`[harness.codex] enabled` reads off"),
        ),
        (codex_tab, "[harness.claude]\nenabled = false\n", None),
        (
            codex_tab,
            "[harness]\nenabled = false\n[harness.codex]\nenabled = false\n",
            Some(master),
        ),
    ] {
        if tab == claude_tab {
            record_upgrade(&env, "03396a15", tab, "");
        } else {
            record_upgrade(&env, &codex_record, tab, r#","agent":"codex""#);
        }
        std::fs::write(&config, toml).expect("write config");
        let (ok, out, err) = go(&now(tab), &env);
        assert!(ok, "{tab} {toml}: {out}{err}");
        assert!(
            out.contains(" request=now "),
            "{tab} {toml}: the word lands: {out}"
        );
        match says {
            None => assert!(err.is_empty(), "{tab} {toml}: {err}"),
            Some(words) => {
                assert!(err.contains(words), "{tab} {toml}: {err}");
                assert!(err.contains("nothing moves"), "{tab} {toml}: {err}");
                assert_eq!(err.contains(master), words == master, "{tab} {toml}: {err}");
            }
        }
    }
}

/// A word the busy lock keeps out exits 75 — the sweep's own "try again"
/// code — and writes nothing.
#[test]
fn an_owners_word_kept_out_by_a_sweep_exits_75_and_writes_nothing() {
    let tmp = tmp_dir("upgrade-owner-busy");
    let env = env_at(&tmp);
    let tab = "s-b5cf2faabac5ce5127bd";
    record_upgrade(&env, "03396a15", tab, "");
    let before = std::fs::read_to_string(env.state.join("upgrade/03396a15.json")).expect("state");
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(env.state.join("upgrade/sweep.lock"))
        .expect("lock file");
    #[expect(
        clippy::disallowed_methods,
        reason = "the test's stand-in sweep: it lets go by unlock() before its drop"
    )]
    held.try_lock().expect("the test holds the sweep lock");
    let (code, _, err) = go_code(
        &Cmd::UpgradeOwner {
            sid: tab.to_string(),
            ask: OwnerAsk::Defer(60),
            json: false,
        },
        &env,
    );
    held.unlock().expect("release");
    assert_eq!(
        code,
        format!("{:?}", ExitCode::from(atpkg::lock::CONTENDED_EXIT)),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(env.state.join("upgrade/03396a15.json")).expect("state"),
        before
    );
}

/// A dry run's line carries how long the session has been behind and its
/// current wait has lasted, from the recorded upgrade; a report with no
/// recorded upgrade is the plain line.
#[test]
fn a_dry_run_line_says_how_long_behind_and_how_long_waiting() {
    let report = super::super::upgrade_drive::Report {
        pid: 4205,
        tab: "s-b5cf".to_string(),
        session: "03396a15".to_string(),
        from: "2.1.281".to_string(),
        to: "2.1.282(managed)".to_string(),
        step: "wait:not-idle".to_string(),
    };
    let row = super::super::upgrade_drive::Row {
        behind_since: 1_000,
        wait: "not-idle:busy".to_string(),
        wait_since: 1_120,
        ..Default::default()
    };
    let mut out = Vec::new();
    upgrade_line_with(&mut out, false, &report, Some((&row, 31_120)));
    upgrade_line_with(&mut out, false, &report, None);
    upgrade_line_with(&mut out, true, &report, Some((&row, 31_120)));
    let out = String::from_utf8(out).expect("utf8");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        "upgrade pid=4205 tab=s-b5cf session=03396a15 from=2.1.281 to=2.1.282(managed) \
         step=wait:not-idle pending_for=8h22m wait_for=8h20m"
    );
    assert!(lines[1].ends_with("step=wait:not-idle"), "{}", lines[1]);
    assert!(
        lines[2].contains(r#""pending_for_s":30120"#),
        "{}",
        lines[2]
    );
    assert!(lines[2].contains(r#""wait_for_s":30000"#), "{}", lines[2]);

    // A finished upgrade is not "behind": a session that caught up reads
    // `pending_for=-` (and `null`).
    let current = super::super::upgrade_drive::Report {
        step: "current".to_string(),
        ..report.clone()
    };
    let done = super::super::upgrade_drive::Row {
        behind_since: 1_000,
        phase: super::super::upgrade::Phase::Done,
        ..Default::default()
    };
    let mut out = Vec::new();
    upgrade_line_with(&mut out, false, &current, Some((&done, 158_000)));
    upgrade_line_with(&mut out, true, &current, Some((&done, 158_000)));
    let out = String::from_utf8(out).expect("utf8");
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0].ends_with("step=current pending_for=- wait_for=-"),
        "a caught-up session was reported behind: {}",
        lines[0]
    );
    assert!(lines[1].contains(r#""pending_for_s":null"#), "{}", lines[1]);
}

/// `harness limits` says the
/// time the supervisor will continue at. A weekly wall hit at 8 pm that says
/// `resets 9am` resets tomorrow, 13 hours off — the print is the done row's
/// clock — where the session window's rule (a five-hour window) put it 11
/// hours in the past; a SESSION window's `9am` is still today's.
#[test]
fn the_view_places_a_weekly_walls_bare_clock_after_its_print() {
    use aterm_phase::prompt::fixtures::{composer, rows};
    // 2026-09-28 20:00 PDT.
    let now = 1_790_650_800;
    let pdt = |z: &str| -> Option<i64> { (z == "America/Los_Angeles").then_some(-7 * 3600) };
    let wall = |limit: &str, done: &str| {
        let mut r = rows(&[
            "⏺ Running the suite.",
            &format!("  ⎿  {limit} · resets 9am (America/Los_Angeles)"),
            "",
            &format!("✻ Worked for 3m 2s · done {done}"),
            "",
        ]);
        r.extend(composer("  ⏵⏵ bypass permissions on"));
        r
    };
    let weekly = limits_view(
        Program::Claude.reader(),
        &wall("You've hit your weekly limit", "8:00 PM"),
        now,
        -7 * 3600,
        &|z, _| pdt(z),
    );
    assert_eq!(weekly.resets_at, Some(now + 13 * 3600));
    // A session window: the five-hour rule, today's, passed.
    let session = limits_view(
        Program::Claude.reader(),
        &wall("You've hit your session limit", "8:00 PM"),
        now,
        -7 * 3600,
        &|z, _| pdt(z),
    );
    assert_eq!(session.resets_at, Some(now - 11 * 3600));
    // No done row: no evidence of the print — the session rule, as before.
    let mut bare = wall("You've hit your weekly limit", "8:00 PM");
    bare.retain(|r| !r.contains("done"));
    assert_eq!(
        limits_view(
            Program::Claude.reader(),
            &bare,
            now,
            -7 * 3600,
            &|z, _| pdt(z)
        )
        .resets_at,
        Some(now - 11 * 3600)
    );
}

// ---------------------------------------------------------------------------
// accounts (docs/DESIGN-claude-accounts-2026-10-01.md §4, §8)
// ---------------------------------------------------------------------------

/// An exit code as a test compares it.
fn exit(code: u8) -> String {
    format!("{:?}", ExitCode::from(code))
}

/// A scripted [`AccountsHost`]. `auth status` — the check after a flow that
/// exits 0 — is answered by `status`, keyed by the login string it was
/// handed; every call is recorded, so a test can say what was run and with
/// which string. Nothing is spawned. Its clock reads `now`: 0, behind every
/// [`Env::now`], until a test moves it. A login can be asked unless
/// `unaskable` says why not. With `writes_home` a flow that starts writes
/// `.claude.json` into the home it is handed, whatever it then exits, as the
/// vendor's start does (MEASURED on 2.1.287).
struct FakeHost<'a> {
    tty: bool,
    status: &'a dyn Fn(&str) -> Result<Captured, String>,
    login: Result<i32, String>,
    writes_home: bool,
    now: i64,
    unaskable: Option<String>,
    asked: std::cell::RefCell<Vec<String>>,
    logins: std::cell::RefCell<Vec<Option<String>>>,
}

impl<'a> FakeHost<'a> {
    fn new(status: &'a dyn Fn(&str) -> Result<Captured, String>) -> FakeHost<'a> {
        FakeHost {
            tty: false,
            status,
            login: Ok(0),
            writes_home: false,
            now: 0,
            unaskable: None,
            asked: std::cell::RefCell::new(Vec::new()),
            logins: std::cell::RefCell::new(Vec::new()),
        }
    }
}

impl AccountsHost for FakeHost<'_> {
    fn stdin_is_terminal(&self) -> bool {
        self.tty
    }

    fn auth_status(&self, login: &str) -> Result<Captured, String> {
        self.asked.borrow_mut().push(login.to_string());
        (self.status)(login)
    }

    fn unaskable(&self) -> Option<String> {
        self.unaskable.clone()
    }

    fn login(&self, dir: Option<&str>) -> Result<i32, String> {
        self.logins.borrow_mut().push(dir.map(str::to_string));
        if self.writes_home
            && self.login.is_ok()
            && let Some(dir) = dir
        {
            std::fs::write(Path::new(dir).join(".claude.json"), "{}").expect("write");
        }
        self.login.clone()
    }

    fn now(&self) -> i64 {
        self.now
    }
}

/// The `auth status` of a verb that must run nothing.
fn never_asked(login: &str) -> Result<Captured, String> {
    panic!("this verb runs no `claude` (asked under {login:?})");
}

/// What `claude auth status --json` prints and exits with, MEASURED on
/// 2.1.286 for the one key read here (design §1): `loggedIn: true` and exit
/// 0, or `loggedIn: false` and exit 1.
fn said(logged_in: bool) -> Result<Captured, String> {
    Ok(Captured {
        stdout: format!(r#"{{"loggedIn":{logged_in},"authMethod":"claude.ai"}}"#).into_bytes(),
        code: Some(i32::from(!logged_in)),
    })
}

/// Run one `accounts` form against `host`: `(exit, stdout, stderr)`.
fn acc(form: AccountsCmd, env: &Env, host: &dyn AccountsHost) -> (String, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_accounts(&form, env, host, &mut out, &mut err);
    (
        format!("{code:?}"),
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    )
}

/// One `accounts` form that runs no `claude`.
fn acc_quiet(form: AccountsCmd, env: &Env) -> (String, String, String) {
    acc(form, env, &FakeHost::new(&never_asked))
}

/// What each login is ON RECORD as, written as an observation is — through
/// the store's own transaction ([`accounts::note_auth`] at `env.now`, its
/// ledger rows, the wake marker) — with no verb run.
fn on_record(env: &Env, seen: &[(&str, Auth)]) {
    let mut note = None;
    env.accounts_store()
        .transact(env.now, &mut note, |state| {
            Ok(seen
                .iter()
                .flat_map(|(label, auth)| accounts::note_auth(state, label, *auth, env.now))
                .collect())
        })
        .expect("recorded");
}

fn add(label: &str, class: Class) -> AccountsCmd {
    AccountsCmd::Add {
        label: label.to_string(),
        class,
        email: None,
        browser: None,
    }
}

/// A LOGIN MADE in the login directory `dir`: the directory, and one file in
/// it — what the vendor's flow leaves when it is run with the directory as
/// its home. A directory that holds nothing is not enrolled
/// ([`accounts::login_made`]).
fn login_made_in(dir: impl AsRef<Path>) {
    let dir = dir.as_ref();
    std::fs::create_dir_all(dir).expect("mkdir");
    std::fs::write(dir.join("a-login-file"), "{}").expect("write");
}

/// The design's own roster under `env`: `main` (this machine's login),
/// `work-2` (dedicated, a login made in its directory) and `pool-a` (shared,
/// its directory NOT made: not enrolled here). Answers the two directory
/// strings.
fn three_accounts(tmp: &Tmp, env: &Env) -> (String, String) {
    let work = tmp.path().join("logins").join("work-2");
    login_made_in(&work);
    let work = work.to_str().expect("utf-8").to_string();
    let pool = tmp.path().join("logins").join("pool-a");
    let pool = pool.to_str().expect("utf-8").to_string();
    let mut text = String::new();
    for (label, class, dir) in [
        ("main", Class::Dedicated, None),
        ("work-2", Class::Dedicated, Some(work.as_str())),
        ("pool-a", Class::Shared, Some(pool.as_str())),
    ] {
        text = accounts::append_row(&text, label, class, dir).expect("the row admits");
    }
    std::fs::write(env.accounts_roster().expect("a config dir"), text).expect("write");
    (work, pool)
}

/// Every file under `dir`, read whole.
fn every_file(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(every_file(&path));
        } else {
            let bytes = std::fs::read(&path).expect("read");
            found.push((path, bytes));
        }
    }
    found
}

/// THE PACKAGED STATE: no roster. The report says no account is listed,
/// names the file it read and the one line that lists one — and writes
/// nothing: no state directory, no roster file.
#[test]
fn with_no_roster_the_report_names_the_file_and_the_add_line_and_writes_nothing() {
    let tmp = tmp_dir("accounts-none");
    let env = env_at(&tmp);
    let roster = env.accounts_roster().expect("a config dir");
    let (code, out, err) = acc_quiet(AccountsCmd::Report { json: true }, &env);
    assert_eq!(code, exit(0), "{err}");
    assert!(err.is_empty(), "{err}");
    let doc: Value = aterm_json::from_str(out.trim()).expect("one JSON document");
    assert_eq!(doc["accounts"].as_array().map(Vec::len), Some(0), "{out}");

    // A file with no rows is the same roster: a document with no accounts,
    // not a refusal.
    std::fs::write(&roster, "# nothing listed yet\n").expect("write");
    let (code, out, _) = acc_quiet(AccountsCmd::Report { json: true }, &env);
    assert_eq!(code, exit(0));
    let doc: Value = aterm_json::from_str(out.trim()).expect("one JSON document");
    assert_eq!(doc.get("refused"), Some(&Value::Null));
    assert_eq!(
        doc.get("accounts").and_then(Value::as_array).map(Vec::len),
        Some(0)
    );

    assert!(!env.state.exists(), "the report made no state directory");
}

/// A ROSTER THAT DOES NOT ADMIT is refused whole, by name, exit 1 — by the
/// report (text and JSON, whose `refused` carries the reason), and by every
/// verb that would act on it, none of which writes anything or runs
/// anything. Negative control: the same file with the bad row fixed is read
/// and exits 0.
#[test]
fn a_refused_roster_is_said_by_name_and_nothing_acts_on_it() {
    let tmp = tmp_dir("accounts-refused");
    let env = env_at(&tmp);
    let roster = env.accounts_roster().expect("a config dir");
    let bad = format!(
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
         [[account]]\nlabel = \"main\"\nclass = \"shared\"\ndir = \"{}\"\n",
        abs("/logins/x")
    );
    std::fs::write(&roster, &bad).expect("write");

    let (code, out, err) = acc_quiet(AccountsCmd::Report { json: false }, &env);
    assert_eq!(code, exit(1));
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("REFUSED"), "{err}");
    assert!(err.contains(&roster.display().to_string()), "{err}");

    let (code, out, _) = acc_quiet(AccountsCmd::Report { json: true }, &env);
    assert_eq!(code, exit(1));
    let doc: Value = aterm_json::from_str(out.trim()).expect("one JSON document");
    assert!(
        doc.get("refused")
            .and_then(Value::as_str)
            .is_some_and(|why| why.contains("listed twice")),
        "{out}"
    );
    assert_eq!(doc.get("kind").and_then(Value::as_str), Some("accounts"));

    for form in [
        AccountsCmd::Login {
            label: "main".to_string(),
        },
        AccountsCmd::Use {
            label: "main".to_string(),
        },
        add("work-2", Class::Dedicated),
    ] {
        let (code, out, err) = acc_quiet(form.clone(), &env);
        assert_eq!(code, exit(1), "{form:?}");
        assert!(out.is_empty(), "{form:?}: {out}");
        assert!(err.contains("REFUSED"), "{form:?}: {err}");
    }
    assert_eq!(
        std::fs::read_to_string(&roster).expect("read"),
        bad,
        "a refused roster is never rewritten"
    );
    assert!(
        !env.accounts_store().state_file().exists(),
        "nothing was recorded"
    );

    // NEGATIVE CONTROL: the second row renamed, the file admits.
    std::fs::write(
        &roster,
        bad.replacen(
            "\"main\"\nclass = \"shared\"",
            "\"pool-a\"\nclass = \"shared\"",
            1,
        ),
    )
    .expect("write");
    let (code, out, err) = acc_quiet(AccountsCmd::Report { json: false }, &env);
    assert_eq!(code, exit(0), "{err}");
    assert!(out.contains("pool-a"), "{out}");
}

/// `accounts add`: the FIRST add on a machine with no roster lists this
/// machine's own login as `main` (dedicated, no dir) before the new row and
/// says so; the row's dir is the login directory it made, 0700; the line to
/// run next is printed. A second add appends one row and no second `main`;
/// `--shared` is the class. The file it writes is one the reader admits.
#[test]
fn the_first_add_lists_this_machines_own_login_then_the_new_row() {
    let tmp = tmp_dir("accounts-add");
    let env = env_at(&tmp);
    let roster = env.accounts_roster().expect("a config dir");
    let store = env.accounts_store();

    let (code, _, err) = acc_quiet(add("work-2", Class::Dedicated), &env);
    assert_eq!(code, exit(0), "{err}");
    let dir = store.login_dir("work-2").expect("a label");
    assert!(err.is_empty(), "{err}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).expect("made").permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700, "the login directory is private");
        assert_eq!(mode(&store.logins_dir()), 0o700);
        assert_eq!(mode(&roster), 0o600, "and so is the roster it wrote");
    }
    assert_eq!(std::fs::read_dir(&dir).expect("read").count(), 0, "empty");
    let read = Roster::load(&roster).expect("what add wrote admits");
    assert_eq!(
        read.rows,
        vec![
            accounts::Row {
                label: "main".to_string(),
                class: Class::Dedicated,
                dir: None,
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "work-2".to_string(),
                class: Class::Dedicated,
                dir: Some(dir.to_str().expect("utf-8").to_string()),
                ..accounts::Row::default()
            },
        ]
    );

    let (code, _, err) = acc_quiet(add("pool-a", Class::Shared), &env);
    assert_eq!(code, exit(0), "{err}");
    let read = Roster::load(&roster).expect("still admits");
    assert_eq!(
        read.rows
            .iter()
            .map(|r| r.label.as_str())
            .collect::<Vec<_>>(),
        ["main", "work-2", "pool-a"]
    );
    assert_eq!(read.row("pool-a").map(|r| r.class), Some(Class::Shared));
}

/// A ROW JUST ADDED IS LISTED, NOT SIGNED IN (the regression: `accounts add`
/// makes the login directory, and the report read the row `unseen` and named
/// it `next:` with no login behind it). With its
/// directory empty the row is `not-enrolled` in the report and its JSON, is
/// never `next:`, and its reason names the line that signs it in; `accounts
/// use` refuses it with that same line; and `accounts login` STILL RUNS on
/// it — it is what fills the directory — but a flow that exits 0 and leaves
/// nothing there made no login: the row is said `not enrolled`, exit 1, with
/// nothing asked. Once the flow has written there the row is `unseen` and
/// the report's `next:`, and a flow that exits 0 asks that login once.
///
/// NEGATIVE CONTROL: `main`, the row with no directory, is enrolled through
/// all of it — never told to log in.
#[test]
fn a_row_whose_login_directory_is_empty_is_listed_and_not_enrolled() {
    let tmp = tmp_dir("accounts-empty-dir");
    let env = env_at(&tmp);
    let (code, _, err) = acc_quiet(add("check-2", Class::Dedicated), &env);
    assert_eq!(code, exit(0), "{err}");
    let dir = env.accounts_store().login_dir("check-2").expect("a label");
    let dir_text = dir.to_str().expect("utf-8").to_string();
    assert_eq!(std::fs::read_dir(&dir).expect("made").count(), 0, "empty");
    let login_line = "aterm harness accounts login check-2";

    // The report: not enrolled, never `next:`.
    let (_, json, _) = acc_quiet(AccountsCmd::Report { json: true }, &env);
    let doc: Value = aterm_json::from_str(json.trim()).expect("json");
    let next = doc.get("next").expect("next");
    assert_eq!(next.get("to"), Some(&Value::Null), "{json}");
    let not = next.get("not").and_then(Value::as_array).expect("not");
    let word = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    assert_eq!(
        (word(&not[1], "label"), word(&not[1], "why")),
        (
            Some("check-2".to_string()),
            Some("not-enrolled".to_string())
        ),
        "the word alone, for a reader that matches it: {json}"
    );
    let rows = doc
        .get("accounts")
        .and_then(Value::as_array)
        .expect("accounts");
    assert_eq!(rows[0].get("enrolled"), Some(&Value::from(true)), "main");
    assert_eq!(rows[1].get("enrolled"), Some(&Value::from(false)), "{json}");
    assert_eq!(word(&rows[1], "condition").as_deref(), Some("not-enrolled"));

    // `use` refuses it, and says how it is signed in.
    let (code, _, err) = acc_quiet(
        AccountsCmd::Use {
            label: "check-2".to_string(),
        },
        &env,
    );
    assert_eq!(code, exit(1));
    assert!(err.contains(login_line), "{err}");

    // `login` still runs on it: the flow is what fills the directory. This
    // one exits 0 and leaves nothing there, so there is no login to ask.
    let mut tty = FakeHost::new(&never_asked);
    tty.tty = true;
    let (code, out, _) = acc(
        AccountsCmd::Login {
            label: "check-2".to_string(),
        },
        &env,
        &tty,
    );
    assert_eq!(code, exit(1));
    assert!(out.starts_with("CLAUDE_CONFIG_DIR="), "{out}");
    assert_eq!(*tty.logins.borrow(), vec![Some(dir_text.clone())]);

    // The flow wrote its home there: enrolled, and a flow that exits 0 asks
    // that login once.
    login_made_in(&dir);
    let signed_in = |_: &str| said(true);
    let mut tty = FakeHost::new(&signed_in);
    tty.tty = true;
    let (code, _, err) = acc(
        AccountsCmd::Login {
            label: "check-2".to_string(),
        },
        &env,
        &tty,
    );
    assert_eq!(code, exit(0), "{err}");
    assert_eq!(*tty.asked.borrow(), vec![dir_text]);
}

/// NEGATIVE CONTROL for the first-add rule: a roster that already has a row
/// — here one written by hand, with a dir — is not a new roster, and `add`
/// lists no `main` into it.
#[test]
fn an_add_to_a_roster_with_rows_lists_no_login_of_its_own() {
    let tmp = tmp_dir("accounts-add-existing");
    let env = env_at(&tmp);
    let roster = env.accounts_roster().expect("a config dir");
    let hand = format!(
        "# written by hand\n[[account]]\nlabel = \"pool-a\"\nclass = \"shared\"\ndir = \"{}\"\n",
        abs("/logins/pool-a")
    );
    std::fs::write(&roster, &hand).expect("write");
    let (code, _, err) = acc_quiet(add("work-2", Class::Dedicated), &env);
    assert_eq!(code, exit(0), "{err}");
    let text = std::fs::read_to_string(&roster).expect("read");
    assert!(text.starts_with(&hand), "what was there is kept: {text}");
    assert_eq!(
        Roster::load(&roster)
            .expect("admits")
            .rows
            .iter()
            .map(|r| r.label.clone())
            .collect::<Vec<_>>(),
        ["pool-a", "work-2"]
    );
}

/// `accounts add` REFUSES, exit 1, by name, and leaves the roster as it was:
/// a label already listed (the second add of one label), a label outside the
/// grammar, `main` as the first label of a new roster, and a state directory
/// that is not absolute (the row's dir would be relative).
#[test]
fn add_refuses_a_listed_or_inadmissible_label_by_name_and_writes_nothing() {
    let tmp = tmp_dir("accounts-add-refused");
    let env = env_at(&tmp);
    let roster = env.accounts_roster().expect("a config dir");

    for label in ["Work", "-x", "a/b", "", "with space"] {
        let (code, out, _) = acc_quiet(add(label, Class::Dedicated), &env);
        assert_eq!(code, exit(1), "{label:?}");
        assert!(out.is_empty(), "{label:?}: {out}");
    }
    // `help` is in that grammar and is no label: this command's help word.
    // (Typed, `accounts add help` is the help; the verb itself refuses it by
    // name, for whoever calls it another way.)
    let (code, out, _) = acc_quiet(add("help", Class::Dedicated), &env);
    assert_eq!(code, exit(1));
    assert!(out.is_empty(), "{out}");
    let (code, _, _) = acc_quiet(add("main", Class::Dedicated), &env);
    assert_eq!(code, exit(1));
    assert!(!roster.exists(), "no refusal made a roster");
    assert!(
        !env.accounts_store().logins_dir().exists(),
        "no refusal made a login directory"
    );

    let (code, _, err) = acc_quiet(add("work-2", Class::Dedicated), &env);
    assert_eq!(code, exit(0), "{err}");
    let before = std::fs::read_to_string(&roster).expect("read");
    let (code, out, _) = acc_quiet(add("work-2", Class::Shared), &env);
    assert_eq!(code, exit(1));
    assert!(out.is_empty(), "{out}");
    // `main` is listed now, so it is refused as any listed label is.
    let (code, _, _) = acc_quiet(add("main", Class::Dedicated), &env);
    assert_eq!(code, exit(1));
    assert_eq!(std::fs::read_to_string(&roster).expect("read"), before);

    let mut relative = env_at(&tmp);
    relative.state = PathBuf::from("relative-state");
    let (code, _, _) = acc_quiet(add("pool-a", Class::Shared), &relative);
    assert_eq!(code, exit(1));
    assert_eq!(std::fs::read_to_string(&roster).expect("read"), before);
    assert!(
        !Path::new("relative-state").exists(),
        "and it made nothing where the test happens to run"
    );
}

/// `accounts login`: the line is `CLAUDE_CONFIG_DIR='<dir>' claude auth
/// login` with the dir VERBATIM from the roster (a trailing slash and a quote
/// survive, the quote escaped for the shell). With stdin not a terminal the
/// line is printed and NOTHING is run, exit 0, and what is said to run is this
/// verb at a terminal (it records; the printed line alone does not); at a
/// terminal the flow is run under exactly that string. The row with no dir is
/// this machine's own login: its line is `claude auth login`, and piped it
/// runs nothing either. An unlisted label is refused by name.
#[test]
fn login_prints_the_line_and_runs_it_only_at_a_terminal() {
    let tmp = tmp_dir("accounts-login");
    let env = env_at(&tmp);
    let odd = format!("{}/it's here/", tmp.path().display());
    let text = accounts::append_row("", "main", Class::Dedicated, None)
        .and_then(|t| accounts::append_row(&t, "work-2", Class::Dedicated, Some(&odd)))
        .expect("the rows admit");
    std::fs::write(env.accounts_roster().expect("a config dir"), text).expect("write");
    let login = |label: &str| AccountsCmd::Login {
        label: label.to_string(),
    };
    let line = format!(
        "CLAUDE_CONFIG_DIR='{}' claude auth login\n",
        odd.replace('\'', r"'\''")
    );

    let piped = FakeHost::new(&never_asked);
    let (code, out, _) = acc(login("work-2"), &env, &piped);
    assert_eq!(code, exit(0));
    assert_eq!(out, line);
    assert!(piped.logins.borrow().is_empty(), "nothing was run");
    assert!(!Path::new(&odd).exists(), "and nothing was made");

    // This flow exits 0 and leaves nothing in the directory, so there is no
    // login to ask (the tests below ask one).
    let mut tty = FakeHost::new(&never_asked);
    tty.tty = true;
    let (code, out, _) = acc(login("work-2"), &env, &tty);
    assert_eq!(code, exit(1));
    assert!(out.starts_with(&line), "{out}");
    assert_eq!(
        *tty.logins.borrow(),
        vec![Some(odd.clone())],
        "the dir, verbatim"
    );
    assert!(
        Path::new(&odd).is_dir(),
        "the flow has a directory to write"
    );

    // A `claude` that cannot be started, and one that exits non-zero.
    tty.login = Err("No such file or directory (os error 2)".to_string());
    let (code, _, _) = acc(login("work-2"), &env, &tty);
    assert_eq!(code, exit(1));
    tty.login = Ok(3);
    let (code, _, _) = acc(login("work-2"), &env, &tty);
    assert_eq!(code, exit(1));

    let before = tty.logins.borrow().len();
    let (code, out, _) = acc(login("main"), &env, &piped);
    assert_eq!(code, exit(0));
    assert_eq!(out, "claude auth login\n");
    assert!(piped.logins.borrow().is_empty(), "nothing was run");
    let (code, out, _) = acc(login("nobody"), &env, &tty);
    assert_eq!(code, exit(1));
    assert!(out.is_empty(), "{out}");
    assert_eq!(tty.logins.borrow().len(), before, "neither ran anything");
}

/// A FAILED FLOW RUNS NOTHING MORE UNDER ANY LOGIN (this host's `auth
/// status` panics), exit 1: whatever login the directory holds is not brand
/// new, and `claude auth status` would spend its single-use refresh token. Over
/// `work-2`, whose directory held a login and which is recorded `ok`, and
/// over `main`, the row with no `dir`, a flow that exits non-zero — 3, or
/// Ctrl-C's 130 after the start wrote its files — and a `claude` that cannot
/// be started record NOTHING: the state file, the ledger and the wake marker
/// are byte for byte as they were.
///
/// NEGATIVE CONTROL: over a row just added, whose directory held nothing, a
/// failed flow that left nothing records nothing either (the row stays
/// `not-enrolled`), and the same failed flow once its start wrote there is
/// recorded `signed-out` — one `auth` ledger row, still with nothing asked —
/// so the row is not `unseen`: not the report's `next:`, refused by `use`.
#[test]
fn a_failed_flow_runs_nothing_and_records_only_a_first_login_signed_out() {
    let tmp = tmp_dir("accounts-login-failed");
    let env = env_at(&tmp);
    let (work, _) = three_accounts(&tmp, &env);
    on_record(&env, &[("main", Auth::Ok), ("work-2", Auth::Ok)]);
    let store = env.accounts_store();
    let login = |label: &str| AccountsCmd::Login {
        label: label.to_string(),
    };
    let files = || {
        [store.state_file(), store.ledger_path(), store.wake_path()]
            .map(|path| std::fs::read(path).expect("written"))
    };
    let before = files();
    #[cfg(unix)]
    let woken = wake_id(&store);

    let mut tty = FakeHost::new(&never_asked);
    tty.tty = true;
    for (flow, writes_home) in [
        (Ok(3), false),
        (Ok(130), true),
        (
            Err("No such file or directory (os error 2)".to_string()),
            false,
        ),
    ] {
        tty.login = flow;
        tty.writes_home = writes_home;
        for label in ["work-2", "main"] {
            let (code, out, err) = acc(login(label), &env, &tty);
            assert_eq!(code, exit(1), "{label}: {err}");
            assert_eq!(out.lines().count(), 1, "the line, and no answer: {out}");
        }
    }
    assert_eq!(tty.logins.borrow().len(), 6, "each flow was run");
    assert!(tty.asked.borrow().is_empty(), "and nothing under a login");
    assert!(
        Path::new(&work).join(".claude.json").is_file(),
        "the start wrote"
    );
    assert_eq!(files(), before, "nothing was recorded");
    #[cfg(unix)]
    assert_eq!(wake_id(&store), woken, "nobody is woken");

    // NEGATIVE CONTROL: a row just added, its directory empty.
    let (code, _, err) = acc_quiet(add("work-3", Class::Dedicated), &env);
    assert_eq!(code, exit(0), "{err}");
    let fresh = store.login_dir("work-3").expect("a label");
    tty.login = Ok(3);
    tty.writes_home = false;
    let (code, out, _) = acc(login("work-3"), &env, &tty);
    assert_eq!(code, exit(1));
    assert_eq!(out.lines().count(), 1, "the line, and no answer: {out}");
    assert_eq!(files(), before, "a flow that left nothing records nothing");

    tty.login = Ok(130);
    tty.writes_home = true;
    let (code, out, err) = acc(login("work-3"), &env, &tty);
    assert_eq!(code, exit(1), "{err}");
    assert!(
        out.starts_with(&format!(
            "CLAUDE_CONFIG_DIR='{}' claude auth login\n",
            fresh.display()
        )),
        "{out}"
    );
    assert!(fresh.join(".claude.json").is_file(), "it wrote");
    assert!(tty.asked.borrow().is_empty(), "still nothing was asked");
    let (state, _) = store.load(env.now).expect("this build's state");
    assert_eq!(state.account("work-3").auth, Auth::SignedOut);
    assert_eq!(state.account("work-2").auth, Auth::Ok, "not touched");
    let rows = store.tail(20).expect("ledger");
    assert_eq!(rows.len(), 3, "one row more: {rows:?}");
    assert_eq!(
        (rows[2].kind, rows[2].account.as_str(), rows[2].why.as_str()),
        (accounts::Kind::Auth, "work-3", "signed-out")
    );
    #[cfg(unix)]
    assert_eq!(wake_id(&store), woken, "nobody is woken");
    let (code, _, _) = acc_quiet(
        AccountsCmd::Use {
            label: "work-3".to_string(),
        },
        &env,
    );
    assert_eq!(code, exit(1));
}

/// The wake marker's file identity — what a waiting session's watcher wakes
/// on — or `None` where there is no marker.
#[cfg(unix)]
fn wake_id(store: &accounts::Store) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(store.wake_path()).ok().map(|m| m.ino())
}

/// `accounts login` AT A TERMINAL WAITS FOR THE FLOW AND RECORDS WHAT THE
/// LOGIN THEN ANSWERS: the flow exits 0, that ONE login is asked under its
/// own string, its line is printed after the flow's, and a login that works
/// is recorded — the state's word, a ledger `auth` row `ok`, the wake marker
/// replaced — exit 0. The answer is stamped by the host's clock, read after
/// the flow, where that is ahead of the command's own.
///
/// NEGATIVE CONTROL: before it the state said `signed-out` (as a login wall
/// records it), and no other login was asked or recorded.
#[test]
fn login_at_a_terminal_asks_that_one_login_and_records_that_it_works() {
    let tmp = tmp_dir("accounts-login-records");
    let env = env_at(&tmp);
    let (work, _) = three_accounts(&tmp, &env);
    let store = env.accounts_store();
    on_record(&env, &[("main", Auth::Ok), ("work-2", Auth::SignedOut)]);
    let (state, _) = store.load(env.now).expect("this build's state");
    assert_eq!(state.account("work-2").auth, Auth::SignedOut);
    assert_eq!(store.tail(20).expect("ledger").len(), 2);
    #[cfg(unix)]
    let woken = wake_id(&store);

    let signed_in = |_: &str| said(true);
    let mut tty = FakeHost::new(&signed_in);
    tty.tty = true;
    tty.now = env.now + 600;
    let (code, out, err) = acc(
        AccountsCmd::Login {
            label: "work-2".to_string(),
        },
        &env,
        &tty,
    );
    assert_eq!(code, exit(0), "{err}");
    assert!(
        out.starts_with(&format!("CLAUDE_CONFIG_DIR='{work}' claude auth login\n")),
        "{out}"
    );
    assert_eq!(*tty.logins.borrow(), vec![Some(work.clone())]);
    assert_eq!(*tty.asked.borrow(), vec![work], "that login, and no other");

    let later = env.now + 600;
    let (state, _) = store.load(later).expect("this build's state");
    assert_eq!(state.account("work-2").auth, Auth::Ok);
    assert_eq!(state.account("work-2").auth_at, Some(later));
    assert_eq!(state.account("main").auth_at, Some(env.now), "not touched");
    let rows = store.tail(20).expect("ledger");
    assert_eq!(rows.len(), 3);
    assert_eq!(
        (
            rows[2].kind,
            rows[2].account.as_str(),
            rows[2].why.as_str(),
            rows[2].t
        ),
        (accounts::Kind::Auth, "work-2", "ok", later)
    );
    #[cfg(unix)]
    assert_ne!(wake_id(&store), woken, "a login that works wakes");
}

/// A FLOW THAT EXITS 0 IS NOT A LOGIN THAT WORKS. One whose login then
/// answers signed out is exit 1, and the state says so with a ledger row
/// (one that could not be asked:
/// `a_login_that_could_not_be_asked_is_said_and_never_recorded`). It does
/// not replace the wake marker: nothing became usable. The answer is never stamped behind the
/// command's own clock: this host's reads 0, a clock set back.
///
/// NEGATIVE CONTROL: the same flow over a login that answers signed in is
/// exit 0 and wakes (the test above), and the state here said `ok` first.
#[test]
fn a_flow_that_ends_signed_out_is_recorded_so_and_exits_1() {
    let tmp = tmp_dir("accounts-login-signed-out");
    let env = env_at(&tmp);
    let (work, _) = three_accounts(&tmp, &env);
    let store = env.accounts_store();
    on_record(&env, &[("main", Auth::Ok), ("work-2", Auth::Ok)]);
    let login = AccountsCmd::Login {
        label: "work-2".to_string(),
    };
    #[cfg(unix)]
    let woken = wake_id(&store);

    let signed_out = |_: &str| said(false);
    let mut tty = FakeHost::new(&signed_out);
    tty.tty = true;
    let (code, _, err) = acc(login, &env, &tty);
    assert_eq!(code, exit(1), "{err}");
    assert_eq!(*tty.asked.borrow(), vec![work]);
    let (state, _) = store.load(env.now).expect("this build's state");
    assert_eq!(state.account("work-2").auth, Auth::SignedOut);
    assert_eq!(state.account("main").auth, Auth::Ok);
    let rows = store.tail(20).expect("ledger");
    assert_eq!(rows.len(), 3);
    assert_eq!(
        (
            rows[2].kind,
            rows[2].account.as_str(),
            rows[2].why.as_str(),
            rows[2].t
        ),
        (accounts::Kind::Auth, "work-2", "signed-out", env.now)
    );
    assert_eq!(state.account("work-2").auth_at, Some(env.now));
    #[cfg(unix)]
    assert_eq!(wake_id(&store), woken, "nobody is woken");
}

/// WHERE NO LOGIN CAN BE ASKED, THE FLOW IS NOT STARTED: with neither USER
/// nor LOGNAME set the login could not be asked afterwards (and would read
/// signed out), so the owner is not sent through a browser sign-in that
/// records nothing. Said with its reason, exit 1; the line is still printed.
///
/// NEGATIVE CONTROL: the same host, able to ask, runs the flow.
#[test]
fn a_login_nobody_could_ask_afterwards_is_not_started() {
    let tmp = tmp_dir("accounts-login-unaskable");
    let env = env_at(&tmp);
    let (work, _) = three_accounts(&tmp, &env);
    let signed_in = |_: &str| said(true);
    let mut tty = FakeHost::new(&signed_in);
    tty.tty = true;
    tty.unaskable = Some(AUTH_CHECK_NO_USER.to_string());
    for label in ["work-2", "main"] {
        let (code, out, _) = acc(
            AccountsCmd::Login {
                label: label.to_string(),
            },
            &env,
            &tty,
        );
        assert_eq!(code, exit(1), "{label}");
        assert_eq!(out.lines().count(), 1, "the line, and no answer: {out}");
    }
    assert!(tty.logins.borrow().is_empty(), "no flow was started");
    assert!(tty.asked.borrow().is_empty());
    assert!(!env.accounts_store().state_file().exists());

    tty.unaskable = None;
    let (code, _, err) = acc(
        AccountsCmd::Login {
            label: "work-2".to_string(),
        },
        &env,
        &tty,
    );
    assert_eq!(code, exit(0), "{err}");
    assert_eq!(*tty.logins.borrow(), vec![Some(work)]);
}

/// THE LOGIN FLOW'S CHILD ([`login_command`]) RUNS UNDER THE HOME IT IS
/// GIVEN, with the login variable removed; for this machine's own login the
/// home's variable is removed too — whatever this process's own environment
/// holds. Read off the command, nothing started: the runner that waits for it
/// through the terminal's Ctrl-C is the signal tests' below.
///
/// NEGATIVE CONTROL: a program that does not exist is an error, not an exit.
#[cfg(unix)]
#[test]
fn the_login_flow_runs_under_the_home_it_is_given() {
    let tmp = tmp_dir("login-command");
    let program = tmp.path().join("claude");
    let edits = |dir: Option<&str>| {
        login_command(program.as_os_str(), dir)
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        edits(None),
        [
            (accounts::HOME_ENV.to_string(), None),
            (accounts::LOGIN_ENV.to_string(), None)
        ]
    );
    assert_eq!(
        edits(Some("/logins/work-2")),
        [
            (
                accounts::HOME_ENV.to_string(),
                Some("/logins/work-2".to_string())
            ),
            (accounts::LOGIN_ENV.to_string(), None)
        ]
    );
    // NEGATIVE CONTROL: no such program.
    let missing = tmp.path().join("nowhere").join("claude");
    assert!(login_flow_of(missing.as_os_str(), None).is_err());
}

/// Process dispositions belong to one exact-test child, never the parallel
/// test runner. Its bounded runner kills/reaps only this owned process group.
#[cfg(unix)]
fn isolated_terminal_signals(test: &str) -> bool {
    const CHILD: &str = "ATERM_TEST_TERMINAL_SIGNALS";
    if std::env::var(CHILD).as_deref() == Ok(test) {
        // A parallel flow in the parent may have been ignoring the signals
        // when this child exec'd. Start this private process at the defaults.
        for sig in [libc::SIGINT, libc::SIGQUIT] {
            // SAFETY: all-zero is a valid empty action and mask.
            let mut default: libc::sigaction = unsafe { std::mem::zeroed() };
            default.sa_sigaction = libc::SIG_DFL;
            // SAFETY: changes only this exact-test process's dispositions.
            assert_eq!(
                unsafe { libc::sigaction(sig, &default, std::ptr::null_mut()) },
                0
            );
        }
        return true;
    }
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test exe"));
    cmd.args([test, "--exact", "--show-output"])
        .env(CHILD, test);
    let ran =
        crate::harness::align::capture_bounded(cmd, std::time::Duration::from_secs(60), 256 * 1024)
            .expect("the exact signal test finishes and is reaped");
    let stdout = String::from_utf8_lossy(&ran.stdout);
    assert_eq!(ran.code, Some(0), "isolated signal test:\n{stdout}");
    // A filter that names no test (a renamed module or test) exits 0 too,
    // having run nothing.
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("test result: ok. 1 passed;")),
        "isolated signal test: the child ran no test named {test}:\n{stdout}"
    );
    false
}

#[cfg(unix)]
fn terminal_signal_dispositions() -> [libc::sighandler_t; 2] {
    [libc::SIGINT, libc::SIGQUIT].map(|sig| {
        // SAFETY: all-zero is valid storage; a null act only reads it.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::sigaction(sig, std::ptr::null(), &mut action) },
            0
        );
        action.sa_sigaction
    })
}

#[cfg(unix)]
#[test]
fn terminal_flows_admit_spawns_after_the_previous_wait_restores_signals() {
    if !isolated_terminal_signals(
        "harness::cli::tests::terminal_flows_admit_spawns_after_the_previous_wait_restores_signals",
    ) {
        return;
    }
    use std::sync::mpsc;
    use std::time::Duration;
    enum Admission {
        Contended,
        Spawned([libc::sighandler_t; 2]),
    }
    let before = terminal_signal_dispositions();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let a = std::thread::spawn(move || {
        terminal_flow(
            || Ok::<_, std::io::Error>(()),
            |()| {
                entered_tx.send(terminal_signal_dispositions()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(60)).unwrap();
                Ok(())
            },
            || {},
        )
        .unwrap();
    });
    let entered = entered_rx.recv_timeout(Duration::from_secs(60));
    let (tx, rx) = mpsc::channel();
    let spawned = tx.clone();
    let b = std::thread::spawn(move || {
        terminal_flow(
            || {
                spawned
                    .send(Admission::Spawned(terminal_signal_dispositions()))
                    .unwrap();
                Ok::<_, std::io::Error>(())
            },
            |()| Ok(()),
            || tx.send(Admission::Contended).unwrap(),
        )
        .unwrap();
    });
    // Old code sends Spawned while A ignores the signals; proper admission
    // sends Contended. Either event lets the controller release A, without
    // a sleep or an assertion based on how long B failed to make progress.
    let first = rx.recv_timeout(Duration::from_secs(60));
    let _ = release_tx.send(());
    let a_done = a.join();
    let b_done = b.join();
    a_done.unwrap();
    b_done.unwrap();
    assert_eq!(entered.unwrap(), [libc::SIG_IGN; 2]);
    let dispositions = match first.unwrap() {
        Admission::Spawned(dispositions) => dispositions,
        Admission::Contended => match rx.recv_timeout(Duration::from_secs(60)).unwrap() {
            Admission::Spawned(dispositions) => dispositions,
            Admission::Contended => panic!("one admission per flow"),
        },
    };
    assert_eq!(
        dispositions, before,
        "the next child must not inherit SIG_IGN"
    );
    assert_eq!(terminal_signal_dispositions(), before, "put back");
}

#[cfg(unix)]
#[test]
fn terminal_flow_restores_signals_when_its_wait_unwinds() {
    if !isolated_terminal_signals(
        "harness::cli::tests::terminal_flow_restores_signals_when_its_wait_unwinds",
    ) {
        return;
    }
    let before = terminal_signal_dispositions();
    let failed = std::panic::catch_unwind(|| {
        terminal_flow(
            || Ok::<_, std::io::Error>(()),
            |()| -> Result<(), std::io::Error> { panic!("the wait unwinds") },
            || {},
        )
    });
    assert!(failed.is_err());
    assert_eq!(terminal_signal_dispositions(), before, "unwind put back");
    terminal_flow(
        || {
            assert_eq!(terminal_signal_dispositions(), before);
            Ok::<_, std::io::Error>(())
        },
        |()| Ok(()),
        || {},
    )
    .unwrap();
}

/// A login that COULD NOT BE ASKED after a flow that exited 0 is said with
/// the reason, recorded as nothing, and the exit is 1: `claude` not on PATH
/// (the runner's own sentence), an exit with no answer, an answer that
/// disagrees with its exit, a signal. Negative control: the same login,
/// answered, is recorded.
#[test]
fn a_login_that_could_not_be_asked_is_said_and_never_recorded() {
    let tmp = tmp_dir("accounts-login-unanswered");
    let env = env_at(&tmp);
    three_accounts(&tmp, &env);
    let store = env.accounts_store();
    let login = AccountsCmd::Login {
        label: "work-2".to_string(),
    };
    let cases: [(Result<Captured, String>, &str); 4] = [
        (
            Err("it will not run: No such file or directory (os error 2)".to_string()),
            "not on PATH",
        ),
        (
            Ok(Captured {
                stdout: b"Usage: claude auth [options]".to_vec(),
                code: Some(2),
            }),
            "an exit with no answer",
        ),
        (
            Ok(Captured {
                stdout: br#"{"loggedIn":true}"#.to_vec(),
                code: Some(1),
            }),
            "an answer its exit disagrees with",
        ),
        (
            Ok(Captured {
                stdout: Vec::new(),
                code: None,
            }),
            "ended by a signal",
        ),
    ];
    for (answer, case) in cases {
        let status = |_: &str| answer.clone();
        let mut tty = FakeHost::new(&status);
        tty.tty = true;
        let (code, out, _) = acc(login.clone(), &env, &tty);
        assert_eq!(code, exit(1), "{case}");
        assert!(out.contains("\nwork-2: could not run ("), "{case}: {out}");
        assert!(!store.state_file().exists(), "{case}: nothing recorded");
    }
    // NEGATIVE CONTROL: answered, it is recorded.
    let signed_in = |_: &str| said(true);
    let mut tty = FakeHost::new(&signed_in);
    tty.tty = true;
    let (code, _, err) = acc(login, &env, &tty);
    assert_eq!(code, exit(0), "{err}");
    let (state, _) = store.load(env.now).expect("this build's state");
    assert_eq!(state.account("work-2").auth, Auth::Ok);
}

/// THE SECRECY CONTROL. `claude auth status --json` names an email and an
/// organization. The fake host's output carries both — in a document that
/// answers, in plain text that does not parse, and inside the `loggedIn` key
/// itself — and no byte of them reaches stdout, stderr, the ledger, the state
/// file or any other file `accounts login` wrote after a flow that exited 0,
/// nor the report or the ledger view read afterwards.
///
/// NEGATIVE CONTROL: the strings ARE in what the host handed over (asserted
/// on the fake's own output), and the document that carried them was still
/// read for its one key (`main: signed in`), so their absence is the reader's
/// doing and not an empty fixture.
#[test]
fn login_keeps_no_byte_of_the_vendors_output() {
    let tmp = tmp_dir("accounts-secrecy");
    let env = env_at(&tmp);
    let (work, pool) = three_accounts(&tmp, &env);
    login_made_in(&pool);
    // Invented, and shaped like what the vendor prints.
    const SECRETS: [&str; 4] = ["zq.nobody", "example.invalid", "Invented", "Org-9"];
    let document = r#"{"loggedIn":true,"authMethod":"claude.ai","email":"zq.nobody@example.invalid","orgName":"Invented Org-9"}"#;
    let plain = "Not logged in as zq.nobody@example.invalid (Invented Org-9)";
    let in_the_key = r#"{"loggedIn":"zq.nobody@example.invalid of Invented Org-9"}"#;
    let status = |login: &str| {
        let (stdout, code) = if login.is_empty() {
            (document, 0)
        } else if login == work {
            (plain, 1)
        } else {
            (in_the_key, 0)
        };
        Ok(Captured {
            stdout: stdout.as_bytes().to_vec(),
            code: Some(code),
        })
    };
    for handed in [document, plain, in_the_key] {
        for secret in SECRETS {
            assert!(handed.contains(secret), "the fixture carries {secret:?}");
        }
    }
    let mut host = FakeHost::new(&status);
    host.tty = true;
    let (mut out, mut err) = (String::new(), String::new());
    for (label, code) in [("main", 0), ("work-2", 1), ("pool-a", 1)] {
        let (ran, said_out, said_err) = acc(
            AccountsCmd::Login {
                label: label.to_string(),
            },
            &env,
            &host,
        );
        assert_eq!(ran, exit(code), "{label}: only `main` gave an answer");
        out.push_str(&said_out);
        err.push_str(&said_err);
    }
    assert_eq!(host.asked.borrow().len(), 3, "all three were run");
    assert!(
        out.contains("main: signed in"),
        "the document was read: {out}"
    );
    assert!(out.contains("work-2: could not run"), "{out}");
    assert!(out.contains("pool-a: could not run"), "{out}");

    let (_, report, report_err) = acc_quiet(AccountsCmd::Report { json: false }, &env);
    let (_, json, _) = acc_quiet(AccountsCmd::Report { json: true }, &env);
    let (_, ledger, _) = go(
        &Cmd::Ledger {
            file: LedgerFile::Accounts,
            count: 20,
            json: false,
        },
        &env,
    );
    let (_, raw, _) = go(
        &Cmd::Ledger {
            file: LedgerFile::Accounts,
            count: 20,
            json: true,
        },
        &env,
    );
    assert!(
        ledger.contains("auth main"),
        "the answer is in the ledger: {ledger}"
    );
    let files = every_file(tmp.path());
    assert!(
        files.iter().any(|(p, _)| p.ends_with("state.json"))
            && files.iter().any(|(p, _)| p.ends_with("ledger.jsonl")),
        "the state and the ledger were written"
    );
    for secret in SECRETS {
        for (what, text) in [
            ("stdout", &out),
            ("stderr", &err),
            ("the report", &report),
            ("the report's stderr", &report_err),
            ("the JSON report", &json),
            ("the ledger view", &ledger),
            ("the raw ledger", &raw),
        ] {
            assert!(!text.contains(secret), "{secret:?} reached {what}: {text}");
        }
        for (path, bytes) in &files {
            assert!(
                !String::from_utf8_lossy(bytes).contains(secret),
                "{secret:?} reached {}",
                path.display()
            );
        }
    }
}

/// `accounts use <label>` records the owner's pin — `active`, `proven`,
/// `pinned`, a ledger `use` row, the wake marker replaced — and says that it
/// is recorded ([`ACCOUNTS_USE_RECORDED`]); the report ends on
/// [`ACCOUNTS_ROTATION`]. `use --auto` ends the pin with an `unpin` row; with
/// no pin standing it writes nothing.
#[test]
fn use_records_the_owners_pin_and_claims_no_move() {
    let tmp = tmp_dir("accounts-use");
    let env = env_at(&tmp);
    three_accounts(&tmp, &env);
    let store = env.accounts_store();
    let using = |label: &str| AccountsCmd::Use {
        label: label.to_string(),
    };

    let (code, _, _) = acc_quiet(AccountsCmd::Auto, &env);
    assert_eq!(code, exit(0));
    assert!(!store.state_file().exists(), "nothing to write");
    assert!(!store.ledger_path().exists());

    let (code, _, err) = acc_quiet(using("work-2"), &env);
    assert_eq!(code, exit(0), "{err}");
    let (state, _) = store.load(env.now).expect("this build's state");
    assert_eq!(state.active.as_deref(), Some("work-2"));
    assert!(state.proven && state.pinned);
    assert!(store.wake_path().exists());
    let rows = store.tail(20).expect("ledger");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].kind, rows[0].account.as_str(), rows[0].t),
        (accounts::Kind::Use, "work-2", env.now)
    );

    let (code, _, _) = acc_quiet(AccountsCmd::Auto, &env);
    assert_eq!(code, exit(0));
    let (state, _) = store.load(env.now).expect("this build's state");
    assert!(!state.pinned);
    assert_eq!(
        state.active.as_deref(),
        Some("work-2"),
        "still the active one"
    );
    let rows = store.tail(20).expect("ledger");
    assert_eq!(
        rows.iter()
            .map(|e| (e.kind, e.why.as_str()))
            .collect::<Vec<_>>(),
        [
            (accounts::Kind::Use, "the owner's word"),
            (accounts::Kind::Unpin, "auto"),
        ]
    );
    acc_quiet(AccountsCmd::Auto, &env);
    assert_eq!(store.tail(20).expect("ledger").len(), 2, "no pin: no row");
}

/// `accounts use` REFUSES BY NAME, exit 1, with nothing changed: a label the
/// roster does not list, one with no login directory here, and one on record
/// as signed out — each with what to run. Negative control: the signed-out
/// label is accepted once `accounts login` has signed it in.
#[test]
fn use_refuses_an_unlisted_unenrolled_or_signed_out_label_by_name() {
    let tmp = tmp_dir("accounts-use-refused");
    let env = env_at(&tmp);
    three_accounts(&tmp, &env);
    let store = env.accounts_store();
    let using = |label: &str| AccountsCmd::Use {
        label: label.to_string(),
    };
    on_record(&env, &[("main", Auth::Ok), ("work-2", Auth::SignedOut)]);
    let before = std::fs::read_to_string(store.state_file()).expect("state");
    let rows = store.tail(20).expect("ledger").len();

    for label in ["nobody", "pool-a", "work-2"] {
        let (code, out, _) = acc_quiet(using(label), &env);
        assert_eq!(code, exit(1), "{label}");
        assert!(out.is_empty(), "{label}: {out}");
    }
    assert_eq!(
        std::fs::read_to_string(store.state_file()).expect("state"),
        before,
        "a refusal changes nothing"
    );
    assert_eq!(store.tail(20).expect("ledger").len(), rows);

    // NEGATIVE CONTROL: signed in again, the same label is taken.
    let signed_in = |_: &str| said(true);
    let mut tty = FakeHost::new(&signed_in);
    tty.tty = true;
    let (code, _, err) = acc(
        AccountsCmd::Login {
            label: "work-2".to_string(),
        },
        &env,
        &tty,
    );
    assert_eq!(code, exit(0), "{err}");
    let (code, _, err) = acc_quiet(using("work-2"), &env);
    assert_eq!(code, exit(0), "{err}");
}

/// A state with something of everything, written through the store: `main`
/// sampled five minutes ago with its session window read and its week NOT
/// read, a reset offer, a session on it; `work-2` at a wall, with a session
/// that could not move; `pool-a` never observed and not enrolled here.
fn reported_state(env: &Env) {
    let now = env.now;
    let mut state = accounts::State {
        machine: "0a0b".to_string(),
        ..accounts::State::default()
    };
    accounts::note_sample(
        &mut state,
        "main",
        accounts::Sample {
            session_pct: Some(42),
            session_reset: Some(now + 2 * 3600),
            at: now - 300,
            ..accounts::Sample::default()
        },
        now,
    );
    assert!(accounts::note_offer(
        &mut state,
        "main",
        Some(aterm_phase::ResetKind::Weekly),
        1,
        Some(now + 3 * 86_400),
        now - 3600,
        now,
    ));
    accounts::note_auth(&mut state, "main", Auth::Ok, now - 60);
    accounts::note_wall(
        &mut state,
        "work-2",
        "five_hour",
        Some(now + 3600),
        now - 10,
        now,
    );
    accounts::note_session(&mut state, "s-a1", "main", None, now - 5);
    accounts::note_session(&mut state, "s-b2", "work-2", Some("jobs"), now - 5);
    state.active = Some("main".to_string());
    state.proven = true;
    env.accounts_store().save(&state).expect("save");
}

/// THE REPORT, as JSON: one line, schema 1, kind `accounts`; every key of a
/// row present whatever is known; an unknown is `null`, never `0`; an
/// instant in Unix seconds with its UTC form beside it; `dir` absent for
/// this machine's own login; an offer's count with the kind it counts, in
/// the text report too.
#[test]
fn the_json_report_has_every_key_and_null_for_what_nobody_read() {
    let tmp = tmp_dir("accounts-json");
    let env = env_at(&tmp);
    let (work, pool) = three_accounts(&tmp, &env);
    reported_state(&env);
    let now = env.now;
    let (code, out, err) = acc_quiet(AccountsCmd::Report { json: true }, &env);
    assert_eq!(code, exit(0), "{err}");
    assert_eq!(out.lines().count(), 1, "one line: {out}");
    let doc: Value = aterm_json::from_str(out.trim()).expect("JSON");
    let int = |v: &Value, key: &str| v.get(key).and_then(Value::as_i64);
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    let null = |v: &Value, key: &str| v.get(key) == Some(&Value::Null);

    assert_eq!(int(&doc, "schema"), Some(1));
    assert_eq!(text(&doc, "kind").as_deref(), Some("accounts"));
    assert_eq!(int(&doc, "as_of"), Some(now));
    assert_eq!(text(&doc, "as_of_rfc3339"), Some(rfc3339_utc(now)));
    assert_eq!(
        text(&doc, "roster"),
        Some(env.accounts_roster().expect("path").display().to_string())
    );
    assert!(null(&doc, "refused"));
    assert_eq!(text(&doc, "active").as_deref(), Some("main"));
    assert_eq!(doc.get("proven"), Some(&Value::from(true)));
    assert_eq!(doc.get("pinned"), Some(&Value::from(false)));
    assert_eq!(int(&doc, "sessions"), Some(2));
    let next = doc.get("next").expect("next");
    assert!(null(next, "to") && null(next, "wait_until") && null(next, "wait_until_rfc3339"));
    assert_eq!(
        next.get("not").and_then(Value::as_array).map(Vec::len),
        Some(3),
        "each row says why it is not next"
    );

    let rows = doc.get("accounts").and_then(Value::as_array).expect("rows");
    assert_eq!(rows.len(), 3);
    const KEYS: [&str; 22] = [
        "label",
        "class",
        "enrolled",
        "condition",
        "limited_until",
        "limited_until_rfc3339",
        "auth",
        "auth_at",
        "auth_at_rfc3339",
        "active",
        "pinned",
        "session_pct",
        "session_reset",
        "session_reset_rfc3339",
        "week_pct",
        "week_reset",
        "week_reset_rfc3339",
        "sampled_at",
        "sampled_at_rfc3339",
        "offer",
        "walls",
        "sessions",
    ];
    for row in rows {
        for key in KEYS {
            assert!(row.get(key).is_some(), "{key} is absent from {row:?}");
        }
    }
    let (main, work_2, pool_a) = (&rows[0], &rows[1], &rows[2]);

    assert_eq!(text(main, "label").as_deref(), Some("main"));
    assert!(
        main.get("dir").is_none(),
        "this machine's own login names none"
    );
    assert_eq!(text(main, "condition").as_deref(), Some("usable"));
    assert_eq!(int(main, "session_pct"), Some(42));
    assert_eq!(
        text(main, "session_reset_rfc3339"),
        Some(rfc3339_utc(now + 2 * 3600))
    );
    // The week was not read: null, in every one of its three keys.
    assert!(null(main, "week_pct") && null(main, "week_reset"));
    assert!(null(main, "week_reset_rfc3339"));
    // The offer's count is its kind's, which it names.
    let offer = main.get("offer").expect("the offer");
    assert_eq!(text(offer, "kind").as_deref(), Some("weekly"));
    assert_eq!(int(offer, "left"), Some(1));
    let (code, out, err) = acc_quiet(AccountsCmd::Report { json: false }, &env);
    assert_eq!(code, exit(0), "{err}");
    assert!(
        out.contains(&format!(
            "offer: 1 weekly reset left, use by {}",
            rfc3339_utc(now + 3 * 86_400)
        )),
        "{out}"
    );

    assert_eq!(text(work_2, "dir"), Some(work));
    assert_eq!(text(work_2, "condition").as_deref(), Some("limited"));
    assert_eq!(
        int(work_2, "limited_until"),
        Some(now + 3600 + accounts::RESET_GRACE_S)
    );
    let wall = work_2
        .get("walls")
        .and_then(|w| w.get("five_hour"))
        .expect("the wall, by the vendor's limit type");
    assert_eq!(int(wall, "until"), Some(now + 3600));

    // NEVER 0 FOR AN UNKNOWN: the account nobody observed is nulls throughout.
    assert_eq!(text(pool_a, "dir"), Some(pool));
    assert_eq!(pool_a.get("enrolled"), Some(&Value::from(false)));
    assert_eq!(text(pool_a, "condition").as_deref(), Some("not-enrolled"));
    assert_eq!(text(pool_a, "auth").as_deref(), Some("unseen"));
    for key in [
        "auth_at",
        "session_pct",
        "session_reset",
        "week_pct",
        "week_reset",
        "sampled_at",
        "sampled_at_rfc3339",
        "limited_until",
        "offer",
    ] {
        assert!(
            null(pool_a, key),
            "{key} of an unobserved account: {pool_a:?}"
        );
    }
    assert_eq!(
        pool_a
            .get("sessions")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0)
    );
}

/// THE CHECK AFTER A SIGN-IN IS NOT RUN WITH NEITHER USER NOR LOGNAME SET.
/// Claude Code then answers `loggedIn: false`, exit 1, for a login that
/// works (MEASURED, 2.1.286), and the check would record a signed-in account
/// as signed out — ending a pin on it, and keeping it out of every rotation.
/// So the question is not asked, and `accounts login` does not start the flow
/// whose login it could not ask afterwards
/// (`a_login_nobody_could_ask_afterwards_is_not_started`).
#[test]
fn the_check_after_a_sign_in_needs_user_or_logname() {
    type Vars = &'static [(&'static str, &'static str)];
    let var = |vars: Vars| {
        move |name: &str| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    };
    let names = |vars: Vars| {
        auth_check_env(&var(vars))
            .map(|pass| pass.iter().map(|(name, _)| *name).collect::<Vec<_>>())
    };
    let refused = Err(AUTH_CHECK_NO_USER.to_string());
    // The cleared environment a service or an `env -i` shell hands down.
    assert_eq!(names(&[("PATH", "/bin"), ("HOME", "/h")]), refused);
    assert_eq!(names(&[("ATERM_CHILD", "1")]), refused);
    // Set and empty names nobody.
    assert_eq!(names(&[("USER", ""), ("LOGNAME", "")]), refused);
    // NEGATIVE CONTROLS: either one is enough, and what goes through is
    // those two and the reroute markers — never anything else that is set.
    assert_eq!(names(&[("USER", "u")]), Ok(vec!["USER"]));
    assert_eq!(names(&[("LOGNAME", "u")]), Ok(vec!["LOGNAME"]));
    assert_eq!(
        names(&[
            ("USER", "u"),
            ("LOGNAME", "u"),
            ("ATERM_CHILD", "1"),
            ("CLAUDE_CONFIG_DIR", "/elsewhere"),
            ("PATH", "/bin"),
        ]),
        Ok(vec!["USER", "LOGNAME", "ATERM_CHILD"])
    );
}

/// A STATE FILE THAT IS NOT OURS IS NEVER SAVED OVER. A `state.json` of
/// another schema — a newer build's, with this older one still on the
/// machine — used to load as the empty state and be written over by the next
/// `login` or `use`: the pin, every wall and every signed-out word gone.
///
/// Every writing verb REFUSES, exit 1, naming the file, and leaves it byte
/// for byte — a login that answers signed in, and a failed first login,
/// included; the report shows an empty state beside the reason and exits 0.
/// NEGATIVE CONTROL: a file that is not JSON at all holds nothing, and the
/// same `use` replaces it, saying so.
#[test]
fn a_state_file_of_another_schema_is_refused_by_every_write_and_left_as_it_is() {
    let tmp = tmp_dir("accounts-not-ours");
    let env = env_at(&tmp);
    three_accounts(&tmp, &env);
    let store = env.accounts_store();
    store.ensure().expect("the store's directory");
    let newer = r#"{"schema":2,"active":"work-2","pinned":true,"accounts":{}}"#;
    std::fs::write(store.state_file(), newer).expect("write");
    let all_in = |_: &str| said(true);
    let mut tty = FakeHost::new(&all_in);
    tty.tty = true;

    let writes: [(AccountsCmd, &dyn AccountsHost); 3] = [
        (
            AccountsCmd::Login {
                label: "work-2".to_string(),
            },
            &tty,
        ),
        (
            AccountsCmd::Use {
                label: "work-2".to_string(),
            },
            &FakeHost::new(&never_asked),
        ),
        (AccountsCmd::Auto, &FakeHost::new(&never_asked)),
    ];
    for (form, host) in writes {
        let (code, out, err) = acc(form.clone(), &env, host);
        assert_eq!(code, exit(1), "{form:?}: {out}");
        assert!(
            err.contains(&store.state_file().display().to_string()),
            "{form:?} names the file: {err}"
        );
        assert!(!out.contains("pinned"), "{form:?} claims nothing: {out}");
        assert_eq!(
            std::fs::read_to_string(store.state_file()).expect("read"),
            newer,
            "{form:?}: the file is as the newer build left it"
        );
        assert!(!store.ledger_path().exists(), "{form:?}: no row");
        assert!(!store.wake_path().exists(), "{form:?}: nobody is woken");
    }
    // So too a failed first login over `pool-a`, whose directory was empty
    // and whose start wrote there: said NOT recorded signed out, nothing more
    // asked, the file as it was.
    let asked = tty.asked.borrow().len();
    tty.login = Ok(130);
    tty.writes_home = true;
    let (code, _, err) = acc(
        AccountsCmd::Login {
            label: "pool-a".to_string(),
        },
        &env,
        &tty,
    );
    assert_eq!(code, exit(1), "{err}");
    assert_eq!(tty.asked.borrow().len(), asked, "nothing more was asked");
    assert_eq!(
        std::fs::read_to_string(store.state_file()).expect("read"),
        newer
    );
    assert!(!store.wake_path().exists(), "nobody is woken");

    // The report reads it, changes nothing, and says what it could not use.
    for json in [false, true] {
        let (code, out, err) = acc_quiet(AccountsCmd::Report { json }, &env);
        assert_eq!(code, exit(0), "{err}");
        assert!(err.contains("schema 2"), "{err}");
        assert!(out.contains("main"), "the roster is still listed: {out}");
        // An empty state: no active account, no pin — never the newer
        // build's, half understood.
        if json {
            let doc: Value = aterm_json::from_str(out.trim()).expect("one JSON document");
            assert_eq!(doc.get("active"), Some(&Value::Null), "{out}");
            assert_eq!(doc.get("pinned"), Some(&Value::from(false)), "{out}");
        }
    }
    assert_eq!(
        std::fs::read_to_string(store.state_file()).expect("read"),
        newer
    );

    // NEGATIVE CONTROL: a torn file is replaced, with a note.
    std::fs::write(store.state_file(), &newer[..newer.len() / 2]).expect("write");
    let (code, _, err) = acc_quiet(
        AccountsCmd::Use {
            label: "work-2".to_string(),
        },
        &env,
    );
    assert_eq!(code, exit(0), "{err}");
    let (state, note) = store.load(env.now).expect("this build's state now");
    assert_eq!(note, None);
    assert_eq!(state.active.as_deref(), Some("work-2"));
}

/// `accounts add` ON A ROSTER THAT IS A SYMLINK (a dotfiles checkout) writes
/// THROUGH the link: the link survives and the file it points at gets the
/// row. Renamed over the link, the roster became a regular file the checkout
/// knew nothing of, and the linked file never got the row.
///
/// The file it points at KEEPS ITS MODE: it is replaced by a rename, and the
/// replacement used to be 0600 whatever the owner had made the file.
#[cfg(unix)]
#[test]
fn add_writes_a_symlinked_roster_through_the_link() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tmp_dir("accounts-add-symlink");
    let env = env_at(&tmp);
    let link = env.accounts_roster().expect("a config dir");
    let dotfiles = tmp.path().join("dotfiles");
    std::fs::create_dir_all(&dotfiles).expect("mkdir");
    let target = dotfiles.join("accounts.toml");
    let hand = "# kept in dotfiles\n[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n";
    std::fs::write(&target, hand).expect("write");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    let is_link = |p: &Path| p.symlink_metadata().expect("lstat").is_symlink();
    let mode = |p: &Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
    let chmod = |p: &Path, mode: u32| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).expect("chmod");
    };
    chmod(&target, 0o644);

    let (code, _, err) = acc_quiet(add("work-2", Class::Dedicated), &env);
    assert_eq!(code, exit(0), "{err}");
    assert!(is_link(&link), "the link is still a link");
    assert!(!is_link(&target));
    assert_eq!(mode(&target), 0o644, "the target keeps the mode it had");
    // …whatever that was: it is the file's own, not a constant.
    chmod(&target, 0o640);
    let (code, _, err) = acc_quiet(add("pool-a", Class::Shared), &env);
    assert_eq!(code, exit(0), "{err}");
    assert_eq!(mode(&target), 0o640);
    assert!(
        std::fs::read_to_string(&target)
            .expect("read")
            .contains("label = \"pool-a\"")
    );
    let text = std::fs::read_to_string(&target).expect("read");
    assert!(text.starts_with(hand), "what was there is kept: {text}");
    assert!(
        text.contains("label = \"work-2\""),
        "the target got the row: {text}"
    );
    assert_eq!(
        Roster::load(&link)
            .expect("read through the link")
            .rows
            .iter()
            .map(|r| r.label.clone())
            .collect::<Vec<_>>(),
        ["main", "work-2", "pool-a"]
    );
    assert_eq!(
        std::fs::read_dir(&dotfiles).expect("list").count(),
        1,
        "no temp file is left beside the target"
    );
    // NEGATIVE CONTROL: a roster that is no link is this command's to keep
    // private, whatever mode it had.
    let plain = tmp_dir("accounts-add-plain");
    let env = env_at(&plain);
    let roster = env.accounts_roster().expect("a config dir");
    std::fs::write(&roster, hand).expect("write");
    chmod(&roster, 0o644);
    let (code, _, err) = acc_quiet(add("work-2", Class::Dedicated), &env);
    assert_eq!(code, exit(0), "{err}");
    assert_eq!(mode(&roster), 0o600);

    // A link whose target cannot be resolved is refused by name, and nothing
    // is made in its place.
    let other = tmp_dir("accounts-add-dangling");
    let env = env_at(&other);
    let link = env.accounts_roster().expect("a config dir");
    std::os::unix::fs::symlink(other.path().join("gone").join("accounts.toml"), &link)
        .expect("symlink");
    let (code, out, _) = acc_quiet(add("work-2", Class::Dedicated), &env);
    assert_eq!(code, exit(1));
    assert!(out.is_empty(), "{out}");
    assert!(is_link(&link), "the link is as it was");
    assert!(!other.path().join("gone").exists());
    assert!(!env.accounts_store().logins_dir().exists());
}

/// A ROSTER THAT DEFINES `account` INLINE (`account = [ { … } ]`) admits, is
/// reported — and cannot be appended to: an `[[account]]` table cannot
/// extend an array written that way. `add` used to blame the owner's file as
/// `not valid TOML`, at a line it had itself appended. It says what is so,
/// prints the row to add by hand, and writes nothing.
#[test]
fn add_to_an_inline_roster_says_so_and_prints_the_row_to_add_by_hand() {
    let tmp = tmp_dir("accounts-add-inline");
    let env = env_at(&tmp);
    let roster = env.accounts_roster().expect("a config dir");
    let inline = "account = [ { label = \"main\", class = \"dedicated\" } ]\n";
    std::fs::write(&roster, inline).expect("write");
    let (code, out, _) = acc_quiet(AccountsCmd::Report { json: false }, &env);
    assert_eq!(code, exit(0));
    assert!(out.contains("main"), "the report reads it: {out}");

    let (code, out, err) = acc_quiet(add("work-2", Class::Shared), &env);
    assert_eq!(code, exit(1));
    assert!(out.is_empty(), "{out}");
    let dir = env.accounts_store().login_dir("work-2").expect("a label");
    let row = accounts::row_inline("work-2", Class::Shared, dir.to_str()).expect("a row");
    assert!(
        err.trim_end().ends_with(&row),
        "the row, to add by hand: {err}"
    );
    assert_eq!(std::fs::read_to_string(&roster).expect("read"), inline);
    assert!(!dir.exists(), "a refusal makes no login directory");

    // The row put where the refusal says is the account that was asked for.
    std::fs::write(&roster, inline.replace(" ]", &format!(", {row} ]"))).expect("write");
    let read = Roster::load(&roster).expect("admits");
    assert_eq!(
        read.row("work-2").map(|r| (r.class, r.dir.clone())),
        Some((Class::Shared, dir.to_str().map(str::to_owned)))
    );

    // THE EMPTY INLINE ARRAY (`account = []`) is a roster with no rows, so a
    // first `add` lists this machine's own login before the one asked for.
    // The refusal prints BOTH rows, in that order, and says whose the first
    // is; it used to print the row for `main` alone, and never the one that
    // was asked for.
    let empty = "account = []\n";
    std::fs::write(&roster, empty).expect("write");
    let (code, out, err) = acc_quiet(add("work-2", Class::Shared), &env);
    assert_eq!(code, exit(1));
    assert!(out.is_empty(), "{out}");
    let own = accounts::row_inline(ACCOUNTS_OWN_LABEL, Class::Dedicated, None).expect("a row");
    assert!(
        err.trim_end().ends_with(&format!("{own}, {row}")),
        "both rows, to add by hand: {err}"
    );
    assert_eq!(std::fs::read_to_string(&roster).expect("read"), empty);
    assert!(!dir.exists(), "a refusal makes no login directory");
    // The two put where the refusal says are the roster a first `add` makes.
    std::fs::write(&roster, format!("account = [ {own}, {row} ]\n")).expect("write");
    let read = Roster::load(&roster).expect("admits");
    assert_eq!(
        read.rows
            .iter()
            .map(|r| (r.label.as_str(), r.class, r.dir.is_some()))
            .collect::<Vec<_>>(),
        [
            (ACCOUNTS_OWN_LABEL, Class::Dedicated, false),
            ("work-2", Class::Shared, true)
        ]
    );
}

/// A SIGN-IN AFTER THE CLOCK WAS SET BACK IS STILL RECORDED. A word stamped
/// ahead of the clock used to make every later answer "older than the one
/// held": the verb printed `signed in` and recorded nothing, for as long as
/// the clock was behind the stamp, and the account stayed signed-out to
/// `use` and to every rotation. Here the host's clock reads 0 after the flow,
/// behind the command's own too. The login is `main`, the row with no dir:
/// its flow runs in the default home, asked under the empty string.
#[test]
fn a_login_is_recorded_over_a_word_stamped_ahead_of_the_clock() {
    let tmp = tmp_dir("accounts-login-clock");
    let env = env_at(&tmp);
    three_accounts(&tmp, &env);
    let store = env.accounts_store();
    let mut state = accounts::State {
        machine: "0a0b".to_string(),
        ..accounts::State::default()
    };
    // Recorded while the clock ran a day fast.
    accounts::note_auth(&mut state, "main", Auth::SignedOut, env.now + 86_400);
    store.save(&state).expect("save");

    let all_in = |_: &str| said(true);
    let mut tty = FakeHost::new(&all_in);
    tty.tty = true;
    let (code, out, err) = acc(
        AccountsCmd::Login {
            label: "main".to_string(),
        },
        &env,
        &tty,
    );
    assert_eq!(code, exit(0), "{err}");
    // The row with no dir is this machine's own login: its flow runs in the
    // default home, and that login is asked under the empty string.
    assert!(out.starts_with("claude auth login\n"), "{out}");
    assert_eq!(*tty.logins.borrow(), vec![None], "the default home");
    assert_eq!(*tty.asked.borrow(), vec![String::new()]);
    let (state, _) = store.load(env.now).expect("this build's state");
    assert_eq!(
        (state.account("main").auth, state.account("main").auth_at),
        (Auth::Ok, Some(env.now))
    );
    let (code, _, err) = acc_quiet(
        AccountsCmd::Use {
            label: "main".to_string(),
        },
        &env,
    );
    assert_eq!(code, exit(0), "no longer refused as signed-out: {err}");
}

/// THE ONE RUNNER OF `auth status --json` ([`auth_status_of`]: the check
/// after a sign-in) runs THE PROGRAM IT IS GIVEN, by its path, under the
/// login it is given, with USER handed through: a stand-in that answers
/// `loggedIn: true` only with USER set and the login variable set-and-empty
/// reads signed in for the default login and signed out for a directory. NEGATIVE CONTROLS: with neither USER nor LOGNAME it
/// is NOT RUN (the vendor then reports a login that works as signed out) —
/// the stand-in leaves no mark of having run; and a program that does not
/// exist could not run, with no word of any output.
#[cfg(unix)]
#[test]
fn the_auth_runner_runs_the_program_it_is_given_under_the_login() {
    let tmp = tmp_dir("auth-runner");
    let program = tmp.path().join("claude");
    let ran = tmp.path().join("ran");
    // `ran` is read off `$0`, so the body is the same on every run and
    // `stand_in` hands back an inode already assessed.
    aterm_tempfile::stand_in(
        &program,
        &format!(
            "#!/bin/sh\necho x >> \"${{0%/*}}/ran\"\nif [ -n \"$USER\" ] && [ \"${{{var}+set}}\" = set ] && [ -z \"${var}\" ]; \
             then echo '{{\"loggedIn\":true,\"email\":\"nobody\"}}'; exit 0; fi\necho \
             '{{\"loggedIn\":false}}'; exit 1\n",
            var = accounts::LOGIN_ENV,
        ),
    )
    .unwrap();
    let user = |name: &str| (name == "USER").then(|| OsString::from("someone"));
    // The runner under a hang detector, not the product's 20 s: a stand-in's
    // first exec on a machine can wait out the system's code-policy check.
    let auth_status =
        |program: &std::ffi::OsStr, login: &str, var: &dyn Fn(&str) -> Option<OsString>| {
            auth_status_within(program, login, var, aterm_spec::wait::MUST_HAPPEN)
        };
    let none = |_: &str| None;

    // NEGATIVE CONTROL: no USER, not run.
    assert_eq!(
        auth_status(program.as_os_str(), "", &none).err(),
        Some(AUTH_CHECK_NO_USER.to_string())
    );
    assert!(!ran.exists(), "the program was not started");

    assert_eq!(
        login_check(auth_status(program.as_os_str(), "", &user)),
        LoginCheck::SignedIn,
        "the default login, named by the empty value"
    );
    assert_eq!(
        login_check(auth_status(program.as_os_str(), "/logins/work-2", &user)),
        LoginCheck::SignedOut
    );
    assert_eq!(
        std::fs::read_to_string(&ran).unwrap().lines().count(),
        2,
        "it was this program that ran, once an ask"
    );
    // NEGATIVE CONTROL: no such program.
    let missing = tmp.path().join("nowhere").join("claude");
    assert!(matches!(
        login_check(auth_status(missing.as_os_str(), "", &user)),
        LoginCheck::CouldNotRun(_)
    ));
}

/// `harness limits` SAYS WHETHER USAGE CREDITS CARRY THE SESSION (accounts v2
/// S5): `credits=in-use` beside `wall=none` on the measured 2.1.300 credits
/// screens — credits are no wall — and `"credits":"in-use"` in the JSON; the
/// watchdog's retry row is a turn in progress, no wall, with `credits=-`.
/// NEGATIVE CONTROL: an ordinary screen says `credits=-` and
/// `"credits":null`.
#[test]
fn harness_limits_says_whether_credits_carry_the_session() {
    use aterm_phase::prompt::fixtures::{
        CREDITS_IN_USE_2_1_300, CREDITS_STARTED_FULLSCREEN_2_1_300, END_SESSION_LIMIT,
        WALL_WATCHDOG_SESSION_STATUSLINE_2_1_300, screen,
    };
    let view = |fixture: &str| {
        limits_view(
            Program::Claude.reader(),
            &screen(fixture),
            1_758_412_800,
            0,
            &|_, _| None,
        )
    };
    for credits in [CREDITS_IN_USE_2_1_300, CREDITS_STARTED_FULLSCREEN_2_1_300] {
        let v = view(credits);
        assert!(v.credits && v.wall.is_none(), "{v:?}");
        let text = limits_text(&v);
        assert!(
            text.starts_with("wall=none source=grid windows=- credits=in-use "),
            "{text}"
        );
        assert!(limits_json(&v).contains(r#""credits":"in-use""#));
    }
    let retry = limits_text(&view(WALL_WATCHDOG_SESSION_STATUSLINE_2_1_300));
    assert!(
        retry.starts_with("wall=none ") && retry.contains(" credits=- "),
        "{retry}"
    );
    let plain = view(END_SESSION_LIMIT);
    assert!(!plain.credits);
    assert!(
        limits_text(&plain).contains(" credits=- "),
        "{}",
        limits_text(&plain)
    );
    assert!(limits_json(&plain).contains(r#""credits":null"#));
}

#[test]
fn harness_limits_attributes_credits_only_to_claude_code() {
    use aterm_phase::prompt::fixtures::{
        CREDITS_IN_USE_2_1_300, CREDITS_STARTED_FULLSCREEN_2_1_300, END_SESSION_LIMIT, screen,
    };
    let view = |program: Program, rows: &[String]| {
        limits_view(program.reader(), rows, 1_758_412_800, 0, &|_, _| None)
    };
    for fixture in [CREDITS_IN_USE_2_1_300, CREDITS_STARTED_FULLSCREEN_2_1_300] {
        let rows = screen(fixture);
        let claude = view(Program::Claude, &rows);
        assert!(claude.credits);
        assert!(limits_text(&claude).contains(" credits=in-use "));
        assert!(limits_json(&claude).contains(r#""credits":"in-use""#));
        // A shell can print the same framed bytes, or they can remain on
        // screen while another program takes over. Its reader owns the read.
        for program in [Program::Codex, Program::Generic] {
            let other = view(program, &rows);
            assert!(!other.credits, "{program:?} inherited Claude's credits");
            assert!(limits_text(&other).contains(" credits=- "));
            assert!(limits_json(&other).contains(r#""credits":null"#));
        }
    }
    assert!(!view(Program::Claude, &screen(END_SESSION_LIMIT)).credits);
}

/// `harness accounts` SHOWS WHAT THE RECORD SAYS OF AN ACCOUNT'S USAGE
/// (accounts v2 S5): with no roster, the machine's own row once its usage is
/// on record (`[harness] usage_notes`) — credits in use since a time, then,
/// over, the vendor's word that they are off and its weekly warning — and
/// on a roster's row the same words before `on:`. NEGATIVE CONTROL: with
/// nothing on record the report is the packaged one, word for word, and a
/// row with no usage on record is unchanged.
#[test]
fn the_accounts_report_shows_the_record_of_usage() {
    use super::super::accounts::{Credits, CreditsOff, CreditsUse, WindowSaid};
    let tmp = tmp_dir("accounts-usage");
    let env = env_at(&tmp);
    let now = env.now;
    let (_, packaged, _) = acc_quiet(AccountsCmd::Report { json: false }, &env);
    let store = env.accounts_store();
    let write = |edit: &dyn Fn(&mut super::super::accounts::Account)| {
        let mut note = None;
        store
            .transact_within(
                &super::super::accounts::LOCK_LADDER,
                now,
                &mut note,
                |state| {
                    edit(
                        state
                            .accounts
                            .entry(ACCOUNTS_OWN_LABEL.to_string())
                            .or_default(),
                    );
                    Ok(Vec::new())
                },
            )
            .expect("written");
    };
    write(&|a| {
        a.credits = Some(Credits::InUse(CreditsUse {
            since: now - 600,
            seen: now - 60,
            until: Some(now + 3600),
            told: true,
        }));
    });
    let (code, out, _) = acc_quiet(AccountsCmd::Report { json: false }, &env);
    assert_eq!(code, exit(0));
    assert_eq!(
        out,
        format!(
            "{packaged}this machine's own account (main): credits: in use since {}\n",
            rfc3339_utc(now - 600)
        )
    );
    write(&|a| {
        a.credits = Some(Credits::Off(CreditsOff {
            reason: "org_level_disabled".to_string(),
            at: now - 60,
            until: None,
        }));
        a.warned_week = Some(WindowSaid {
            pct: Some(92),
            reset: Some(now + 3 * 86_400),
            at: now - 30,
        });
    });
    let (_, out, _) = acc_quiet(AccountsCmd::Report { json: false }, &env);
    assert!(
        out.ends_with(&format!(
            "(main): credits: off (org_level_disabled)  warned: weekly 92% (resets {})\n",
            rfc3339_utc(now + 3 * 86_400)
        )),
        "{out}"
    );
    // On a roster's row: before `on:`, and a row with none is as it was.
    let roster = env.accounts_roster().expect("a config dir");
    std::fs::write(
        &roster,
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
             [[account]]\nlabel = \"work-2\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
            abs("/logins/work-2")
        ),
    )
    .expect("write");
    let (_, out, _) = acc_quiet(AccountsCmd::Report { json: false }, &env);
    let row = |label: &str| {
        out.lines()
            .find(|l| l.starts_with(label))
            .unwrap_or_else(|| panic!("{label}: {out}"))
            .to_string()
    };
    assert!(
        row("main  ").contains("  credits: off (org_level_disabled)  warned: weekly 92% (")
            && row("main  ").ends_with("on: -"),
        "{out}"
    );
    assert!(!row("work-2").contains("credits:"), "{out}");
}

// ---------------------------------------------------------------------------
// The usage tap
// ---------------------------------------------------------------------------

/// `usage` SHOWS THE TAP'S LAST READING of its own process: the file this
/// tab and `CLAUDE_PID` name, written since that process started — the HUD's
/// figures and the reading's age, the JSON's `windows`. NEGATIVE CONTROLS:
/// another tab's file, and a file older than the process (a reused pid's),
/// leave the windows unknown.
#[test]
fn usage_shows_the_tap_reading_of_its_own_process() {
    let tmp = tmp_dir("usage-tap");
    let mut env = env_at(&tmp);
    env.sid = "s-1a2b".to_owned();
    env.claude_pid = Some(FakeClaude::PID);
    let dir = env
        .transcripts_root()
        .expect("home is injected")
        .join(footer::project_slug(&env.cwd));
    std::fs::create_dir_all(&dir).expect("project dir");
    std::fs::write(
        dir.join("mine-1.jsonl"),
        r#"{"type":"assistant","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":1,"output_tokens":5}}}
"#,
    )
    .expect("transcript");
    let rl = super::super::tap::rl_dir_in(
        &env.aterm_state
            .clone()
            .expect("a root")
            .join("harness")
            .join(HARNESS),
    );
    let tap_into = |sid: &str| {
        let vars = super::super::tap::TapVars {
            sid: Some(sid.to_owned()),
            pid: Some(FakeClaude::PID.to_string()),
            login: None,
            home: None,
        };
        let stdin = r#"{"cost":{"total_api_duration_ms":9},"rate_limits":{"five_hour":{"used_percentage":42,"resets_at":1758420000},"seven_day":{"used_percentage":61,"resets_at":1758900000}}}"#;
        let mut out = Vec::new();
        let _ = super::super::tap::run(
            &mut stdin.as_bytes(),
            false,
            &vars,
            Some(&rl),
            env.now - 12,
            &mut out,
        );
    };
    let run = |env: &Env, process: &FakeClaude, json: bool| {
        let mut out = Vec::new();
        let _ = run_usage(env, process, json, &mut out);
        String::from_utf8(out).expect("utf-8")
    };
    let process = FakeClaude::with(&[("HOME", "/home/someone")]);
    tap_into("s-9999");
    let hud = run(&env, &process, false);
    assert!(hud.contains("?%/5h · ?%/7d"), "another tab's file: {hud}");
    tap_into("s-1a2b");
    let hud = run(&env, &process, false);
    assert!(
        hud.starts_with("opus 42%/5h · 61%/7d · acct account · resets in "),
        "{hud}"
    );
    assert!(hud.contains(" · tap 12s"), "{hud}");
    let doc: Value = aterm_json::from_str(&run(&env, &process, true)).expect("json");
    let windows = &doc["usage"]["accounts"][0]["windows"];
    assert_eq!(windows["five_hour"]["used_pct"], Value::from(42u64));
    assert_eq!(windows["seven_day"]["used_pct"], Value::from(61u64));
    // A process born after the file was written: not its reading.
    let later = FakeClaude {
        started: Some(4_000_000_000),
        ..FakeClaude::with(&[("HOME", "/home/someone")])
    };
    let hud = run(&env, &later, false);
    assert!(hud.contains("?%/5h · ?%/7d"), "a reused pid's file: {hud}");
}

/// `accounts` WITH NO ROSTER SHOWS THIS LOGIN'S TAP READING where the window
/// recorded one (`main`, the roster's own row): the text adds its line under
/// "no accounts listed", the JSON lists that row with its figures. (With no
/// reading on record it is the packaged report:
/// `with_no_roster_the_report_names_the_file_and_the_add_line_and_writes_nothing`.)
#[test]
fn accounts_with_no_roster_shows_this_logins_tap_reading() {
    let tmp = tmp_dir("accounts-tap");
    let env = env_at(&tmp);
    let mut note = None;
    env.accounts_store()
        .transact(env.now, &mut note, |state| {
            Ok(accounts::note_reading(
                state,
                ACCOUNTS_OWN_LABEL,
                accounts::Sample {
                    session_pct: Some(42),
                    session_reset: Some(env.now + 3_600),
                    week_pct: Some(61),
                    week_reset: Some(env.now + 86_400),
                    at: env.now,
                    by: String::new(),
                },
                env.now,
            ))
        })
        .expect("a transaction");
    let (code, out, _) = acc_quiet(AccountsCmd::Report { json: true }, &env);
    assert_eq!(code, exit(0));
    let doc: Value = aterm_json::from_str(out.trim()).expect("one JSON document");
    let rows = doc["accounts"].as_array().expect("rows");
    assert_eq!(rows.len(), 1, "{out}");
    assert_eq!(rows[0]["label"], Value::from("main"));
    assert_eq!(rows[0]["session_pct"], Value::from(42u64));
}

// ---------------------------------------------------------------------------
// accounts stats
// ---------------------------------------------------------------------------

/// The account id the composed profiles carry (invented).
const STATS_UUID: &str = "0f6c2b8e-1d3a-4c5e-9b7f-2a4d6e8c0b1a";

/// The text planted wherever a value must never be printed.
const STATS_SENTINEL: &str = "S3NT1NEL";

/// A composed `.claude.json`: a Max 5x plan, credits on, the Fable flags at
/// the vendor's defaults, and the sentinel in every credential and identity
/// field the real file can hold.
fn stats_profile() -> String {
    let s = STATS_SENTINEL;
    aterm_json::to_string(&aterm_json::json!({
        "primaryApiKey": format!("sk-ant-{s}"),
        "mcpServers": {"tracker": {"env": {"TOKEN": format!("{s}-env")},
                                   "headers": {"Authorization": format!("Bearer {s}")}}},
        "oauthAccount": {
            "accountUuid": STATS_UUID,
            "emailAddress": format!("ana.{s}@example.com"),
            "displayName": format!("Ana {s}"),
            "organizationName": format!("{s} Example Org"),
            "organizationUuid": format!("{s}-org"),
            "organizationType": "claude_max",
            "userRateLimitTier": "default_claude_max_5x",
            "billingType": "stripe_subscription",
            "profileFetchedAt": 1_758_400_000_000_i64
        },
        "cachedExtraUsageDisabledReason": null,
        "cachedGrowthBookFeatures": {
            "tengu_saffron_credits_only_tiers": ["enterprise"],
            "tengu_saffron_lattice": {}
        }
    }))
    .expect("compose")
}

/// An [`Env`] whose Claude home is `dir` and its profile `profile`.
fn stats_env(tmp: &Tmp, dir: PathBuf, profile: PathBuf) -> Env {
    let mut env = env_at(tmp);
    env.claude_dir = Some(dir);
    env.claude_profile = Some(profile);
    env
}

/// `accounts stats` reads the default home's profile through the allowlist
/// reader: the plan, its tier, the credits word and the Fable plan answer,
/// labelled `main` and keyed by the id's hash — and not one sentinel, the
/// id or the email, in either form.
#[test]
fn accounts_stats_prints_the_profiles_plan_and_nothing_else() {
    let tmp = tmp_dir("stats");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(home.join(".claude.json"), stats_profile()).expect("profile");
    let env = stats_env(&tmp, home.join(".claude"), home.join(".claude.json"));

    let (code, out, err) = acc_quiet(AccountsCmd::Stats { json: true }, &env);
    assert_eq!(code, format!("{:?}", ExitCode::SUCCESS), "{err}");
    let doc: Value = aterm_json::from_str(out.trim()).expect("one JSON document");
    assert_eq!(doc["schema"], Value::from(1u64));
    assert_eq!(doc["kind"], Value::from("account-stats"));
    assert_eq!(doc["as_of"], Value::from(env.now));
    let account = &doc["accounts"][0];
    let key = super::profile::AccountKey::claude(STATS_UUID).expect("key");
    assert_eq!(account["key"], Value::from(key.to_string()));
    assert_eq!(account["agent"], Value::from("claude"));
    assert_eq!(account["label"], Value::from("main"));
    assert_eq!(account["plan"]["word"], Value::from("max"));
    assert_eq!(account["plan"]["tier"], Value::from("5x"));
    assert_eq!(
        account["plan"]["billing"],
        Value::from("stripe_subscription")
    );
    assert_eq!(account["plan"]["as_of"], Value::from(1_758_400_000_i64));
    assert_eq!(account["plan"]["source"], Value::from("profile"));
    assert_eq!(account["credits"]["state"], Value::from("on"));
    assert_eq!(account["fable"]["plan"], Value::from("included"));
    assert_eq!(account["fable"]["hint"], Value::from(true));
    assert_eq!(doc["missing"], Value::Array(Vec::new()));

    let (_, text, _) = acc_quiet(AccountsCmd::Stats { json: false }, &env);
    assert!(
        text.contains(
            "main \u{b7} claude \u{b7} Max 5x \u{b7} credits on\n  Fable  plan: Fable included\n"
        ),
        "{text}"
    );
    for printed in [&out, &text] {
        assert!(
            !printed.contains(STATS_SENTINEL),
            "a sentinel was printed: {printed}"
        );
        assert!(
            !printed.contains(STATS_UUID),
            "the id was printed: {printed}"
        );
        assert!(
            !printed.contains("example.com"),
            "the email was printed: {printed}"
        );
        assert!(
            !printed.contains("tengu"),
            "a vendor flag name was printed: {printed}"
        );
    }
    assert!(
        !text.contains(&key.to_string()),
        "text carries labels only; the key is JSON's"
    );
}

/// A home with no profile says why, in both forms; a home other than
/// Claude Code's default is labelled by its key (`home-<key6>`).
#[test]
fn accounts_stats_names_a_missing_profile_and_labels_another_home_by_key() {
    let tmp = tmp_dir("stats-missing");
    let home = tmp.path().join("home");
    let env = stats_env(&tmp, home.join(".claude"), home.join(".claude.json"));
    let (code, out, _) = acc_quiet(AccountsCmd::Stats { json: true }, &env);
    assert_eq!(code, format!("{:?}", ExitCode::SUCCESS));
    let doc: Value = aterm_json::from_str(out.trim()).expect("one JSON document");
    let account = &doc["accounts"][0];
    assert_eq!(account["label"], Value::from("main"));
    assert_eq!(account["missing"]["reason"], Value::from("no-profile"));
    assert_eq!(
        (&account["key"], &account["plan"]),
        (&Value::Null, &Value::Null)
    );
    assert_eq!(doc["missing"][0]["what"], Value::from("profile"));
    let (_, text, _) = acc_quiet(AccountsCmd::Stats { json: false }, &env);
    assert!(
        text.contains("main \u{b7} claude \u{b7} plan \u{2014} (no-profile)\n"),
        "{text}"
    );

    let other = tmp.path().join("srv-claude-b");
    std::fs::create_dir_all(&other).expect("dir");
    std::fs::write(other.join(".claude.json"), stats_profile()).expect("profile");
    let env = stats_env(&tmp, other.clone(), other.join(".claude.json"));
    let (_, out, _) = acc_quiet(AccountsCmd::Stats { json: true }, &env);
    let doc: Value = aterm_json::from_str(out.trim()).expect("one JSON document");
    let short = super::profile::AccountKey::claude(STATS_UUID)
        .expect("key")
        .short();
    assert_eq!(
        doc["accounts"][0]["label"],
        Value::from(format!("home-{short}"))
    );
}

/// `accounts stats` reads the profile Claude Code keeps for the home this
/// command's environment names ([`claude_profile_of`]), its plain file
/// `join(CLAUDE_CONFIG_DIR || homedir(), ".claude.json")` (READ in the
/// 2.1.301 bundle: `b()`; a file Claude Code reads in its place,
/// `accounts_stats_reads_the_file_claude_code_reads_for_the_home`), and
/// labels the login it holds. The user's own
/// `~/.claude` named by `CLAUDE_CONFIG_DIR` (how the real-Claude lab launches
/// Claude Code) keeps its own profile inside it, and its own keychain item:
/// `home-<key6>`, never the default home's `main`. An EMPTY one keeps
/// `$HOME`'s, its directory unresolved (no `CLAUDE_PID`) included, and the
/// default login: `main`. CONTROL: unset, beside `~/.claude`, `main`; a
/// relative one whose directory is unknown, and no `HOME`, name none.
#[test]
fn accounts_stats_reads_the_profile_claude_code_keeps_for_the_home_it_names() {
    let tmp = tmp_dir("stats-homes");
    let home = tmp.path().join("home");
    let own = home.join(".claude");
    std::fs::create_dir_all(&own).expect("home");
    let config = own.to_string_lossy().into_owned();
    let stats = |env: &Env| {
        let (_, out, _) = acc_quiet(AccountsCmd::Stats { json: true }, env);
        let doc: Value = aterm_json::from_str(out.trim()).expect("one JSON document");
        doc["accounts"][0].clone()
    };
    let short = super::profile::AccountKey::claude(STATS_UUID)
        .expect("key")
        .short();

    // `CLAUDE_CONFIG_DIR=$HOME/.claude`: inside it, its own login.
    let inside = claude_profile_of(Some(&home), Some(&config), Some(&own));
    assert_eq!(inside, Some(own.join(".claude.json")));
    std::fs::write(own.join(".claude.json"), stats_profile()).expect("profile");
    let mut env = stats_env(&tmp, own.clone(), own.join(".claude.json"));
    env.claude_profile = inside;
    let account = stats(&env);
    assert_eq!(account["plan"]["word"], Value::from("max"), "{account}");
    assert_eq!(account["label"], Value::from(format!("home-{short}")));
    std::fs::remove_file(own.join(".claude.json")).expect("rm");

    // `CLAUDE_CONFIG_DIR=` with no `CLAUDE_PID`: no directory, `$HOME`'s.
    let empty = claude_profile_of(Some(&home), Some(""), None);
    assert_eq!(empty, Some(home.join(".claude.json")));
    std::fs::write(home.join(".claude.json"), stats_profile()).expect("profile");
    let mut env = env_at(&tmp);
    env.claude_config_unusable = Some(String::new());
    env.claude_profile = empty;
    let account = stats(&env);
    assert_eq!(account["plan"]["word"], Value::from("max"), "{account}");
    assert_eq!(account["label"], Value::from("main"));

    // CONTROL: unset, beside `~/.claude`; nothing to name.
    assert_eq!(
        claude_profile_of(Some(&home), None, Some(&own)),
        Some(home.join(".claude.json"))
    );
    assert_eq!(
        claude_profile_of(Some(&home), Some("rel/claude"), None),
        None
    );
    assert_eq!(claude_profile_of(None, Some(""), None), None);
}

/// `accounts stats` reads the file Claude Code reads for the home
/// ([`profile::effective`]), and keeps the home's own label: a
/// `~/.claude/.config.json` that holds a byte is read over a stale
/// `~/.claude.json`, or where there is none, and `CLAUDE_CODE_CUSTOM_OAUTH_URL`
/// reads `~/.claude-custom-oauth.json`; each is still the default home,
/// `main`. CONTROL: the `.config.json` emptied and read-only, and the
/// variable unset, read `~/.claude.json`.
#[cfg(unix)]
#[test]
fn accounts_stats_reads_the_file_claude_code_reads_for_the_home() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tmp_dir("stats-effective");
    let home = tmp.path().join("home");
    let records = home.join(".claude");
    std::fs::create_dir_all(&records).expect("records");
    let legacy = records.join(".config.json");
    let pro = stats_profile().replace("claude_max", "claude_pro");
    std::fs::write(home.join(".claude.json"), &pro).expect("stale profile");
    std::fs::write(&legacy, stats_profile()).expect("legacy profile");
    let mut env = stats_env(&tmp, records.clone(), home.join(".claude.json"));
    let stats = |env: &Env| {
        let (_, out, _) = acc_quiet(AccountsCmd::Stats { json: true }, env);
        let doc: Value = aterm_json::from_str(out.trim()).expect("one JSON document");
        let account = &doc["accounts"][0];
        (account["label"].clone(), account["plan"]["word"].clone())
    };
    let main = Value::from("main");

    assert_eq!(stats(&env), (main.clone(), Value::from("max")));
    std::fs::remove_file(home.join(".claude.json")).expect("rm");
    assert_eq!(
        stats(&env),
        (main.clone(), Value::from("max")),
        "no plain file"
    );
    std::fs::write(home.join(".claude.json"), &pro).expect("stale profile");

    // CONTROL: emptied and read-only, it stands in for nothing.
    std::fs::write(&legacy, "").expect("empty");
    std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o444)).expect("chmod");
    assert_eq!(stats(&env), (main.clone(), Value::from("pro")));

    // A custom OAuth URL: its own file, the same home.
    std::fs::write(home.join(".claude-custom-oauth.json"), stats_profile()).expect("custom");
    env.claude_custom_oauth = true;
    assert_eq!(stats(&env), (main.clone(), Value::from("max")));
    // CONTROL: unset, the plain file.
    env.claude_custom_oauth = false;
    assert_eq!(stats(&env), (main, Value::from("pro")));
}

/// `accounts stats` labels a home by its ROSTER ROW where the home is
/// enrolled (design §3.1), the same name `accounts` lists the login by: the
/// row whose `dir` names the home a set `CLAUDE_CONFIG_DIR` resolved to,
/// however a trailing `/` spells it, once a login was made there; the row
/// with no `dir` for the default home. CONTROL: no roster, a refused one, a
/// row naming another directory, and a row whose directory holds no login
/// yet each leave `home-<key6>` (`home` with no key), and the default home
/// with no row is `main`.
#[test]
fn accounts_stats_labels_an_enrolled_home_by_its_roster_row() {
    let tmp = tmp_dir("stats-roster");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let work = tmp.path().join("accounts").join("work");
    std::fs::create_dir_all(&work).expect("dir");
    std::fs::write(work.join(".claude.json"), stats_profile()).expect("profile");
    let fresh = tmp.path().join("accounts").join("fresh");
    std::fs::create_dir_all(&fresh).expect("dir");
    let label = |env: &Env| {
        let (_, out, _) = acc_quiet(AccountsCmd::Stats { json: true }, env);
        let doc: Value = aterm_json::from_str(out.trim()).expect("one JSON document");
        doc["accounts"][0]["label"].clone()
    };
    let roster = |rows: &[(&str, Option<&Path>)]| {
        let mut text = String::new();
        for (label, dir) in rows {
            let dir = dir.map(|d| d.to_str().expect("utf-8"));
            text = accounts::append_row(&text, label, Class::Dedicated, dir).expect("admits");
        }
        text
    };
    let short = super::profile::AccountKey::claude(STATS_UUID)
        .expect("key")
        .short();
    let keyed = Value::from(format!("home-{short}"));
    let env = stats_env(&tmp, work.clone(), work.join(".claude.json"));
    let path = env.accounts_roster().expect("a config dir");

    // CONTROL: no roster, the home by its key.
    assert_eq!(label(&env), keyed);

    std::fs::write(&path, roster(&[("work", Some(work.as_path()))])).expect("write");
    assert_eq!(label(&env), Value::from("work"));
    let (_, text, _) = acc_quiet(AccountsCmd::Stats { json: false }, &env);
    assert!(
        text.contains("work \u{b7} claude \u{b7} Max 5x \u{b7} credits on\n"),
        "{text}"
    );
    let slashed = PathBuf::from(format!("{}/", work.display()));
    std::fs::write(&path, roster(&[("work", Some(slashed.as_path()))])).expect("write");
    assert_eq!(label(&env), Value::from("work"), "one home however spelled");

    // CONTROL: a row naming another directory; a refused roster.
    std::fs::write(&path, roster(&[("fresh", Some(fresh.as_path()))])).expect("write");
    assert_eq!(label(&env), keyed);
    // The admitted `work` row with its class taken out: refused whole, so it
    // names the home and its login yet labels nothing.
    let refused = roster(&[("work", Some(work.as_path()))]).replace("class = \"dedicated\"\n", "");
    assert!(Roster::parse(&refused).is_err(), "{refused}");
    std::fs::write(&path, &refused).expect("write");
    assert_eq!(label(&env), keyed, "a refused roster lists no row");

    // CONTROL: the row's directory holds no login yet: not enrolled.
    let env = stats_env(&tmp, fresh.clone(), fresh.join(".claude.json"));
    std::fs::write(&path, roster(&[("fresh", Some(fresh.as_path()))])).expect("write");
    assert_eq!(label(&env), Value::from("home"));

    // The default home: its row with no `dir`, and `main` with none.
    std::fs::write(home.join(".claude.json"), stats_profile()).expect("profile");
    let env = stats_env(&tmp, home.join(".claude"), home.join(".claude.json"));
    std::fs::write(
        &path,
        roster(&[("own", None), ("work", Some(work.as_path()))]),
    )
    .expect("write");
    assert_eq!(label(&env), Value::from("own"));
    std::fs::write(&path, roster(&[("work", Some(work.as_path()))])).expect("write");
    assert_eq!(label(&env), Value::from("main"));
}

/// A home that could not be resolved — a relative `CLAUDE_CONFIG_DIR` with
/// no `CLAUDE_PID`, or no absolute `HOME` ([`claude_profile_of`]: `None`) —
/// is `no-home` in both forms: no file was looked for, so it is not the
/// `no-profile` of a home whose file is missing. CONTROL: a home that
/// resolved, its file missing, is `no-profile`.
#[test]
fn accounts_stats_names_a_home_it_could_not_resolve() {
    let tmp = tmp_dir("stats-no-home");
    let mut env = env_at(&tmp);
    env.claude_config_unusable = Some("rel/claude".to_owned());
    env.claude_profile = claude_profile_of(env.home.as_deref(), Some("rel/claude"), None);
    assert_eq!(env.claude_profile, None);
    let (code, out, _) = acc_quiet(AccountsCmd::Stats { json: true }, &env);
    assert_eq!(code, format!("{:?}", ExitCode::SUCCESS));
    let doc: Value = aterm_json::from_str(out.trim()).expect("one JSON document");
    assert_eq!(
        doc["accounts"][0]["missing"]["reason"],
        Value::from("no-home")
    );
    assert_eq!(doc["missing"][0]["reason"], Value::from("no-home"));
    let (_, text, _) = acc_quiet(AccountsCmd::Stats { json: false }, &env);
    assert!(
        text.contains("home \u{b7} claude \u{b7} plan \u{2014} (no-home)\n"),
        "{text}"
    );

    // CONTROL: resolved, and no file there.
    let other = tmp.path().join("srv-claude-c");
    let env = stats_env(&tmp, other.clone(), other.join(".claude.json"));
    let (_, text, _) = acc_quiet(AccountsCmd::Stats { json: false }, &env);
    assert!(
        text.contains("home \u{b7} claude \u{b7} plan \u{2014} (no-profile)\n"),
        "{text}"
    );
}

#[path = "cli_renew_tests.rs"]
mod renew;

/// THE RESERVE BREAKS THE FULL-DISK DEADLOCK: at an emergency it is unlinked
/// FIRST and the `released` row journalled after it (the room it made is
/// what that row is written into); well above the target it is written and
/// journalled; between, nothing is written. NEGATIVE CONTROL: a link to a
/// victim in the reserve's place is refused at the emergency — journalled as
/// `refused`, the victim byte for byte.
#[cfg(any(unix, windows))]
#[test]
fn the_reserve_is_released_first_at_an_emergency_and_rewritten_when_space_is_back() {
    let tmp = tmp_dir("reserve");
    let state = tmp.path().join("state");
    let reserve = disk::reserve::path_for(&tmp.path().join("home"));
    let t = disk::Config::default().thresholds(Some(460 << 30));
    let rows = || -> Vec<String> {
        std::fs::read_to_string(state.join(DISK_LEDGER))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let v: Value = aterm_json::from_str(l).expect("a row");
                v["action"].as_str().unwrap_or_default().to_owned()
            })
            .collect()
    };
    // Between the floor and the target plus the reserve: nothing is written.
    let between = Some(t.target);
    assert_eq!(
        disk_reserve_look(&state, &reserve, t.level(between, None), between, &t, 1),
        ReserveMove::Kept
    );
    assert!(!reserve.exists());
    let plenty = Some(t.target + disk::reserve::RESERVE_BYTES);
    assert_eq!(
        disk_reserve_look(&state, &reserve, t.level(plenty, None), plenty, &t, 2),
        ReserveMove::Written
    );
    assert_eq!(
        std::fs::metadata(&reserve).expect("reserve").len(),
        disk::reserve::RESERVE_BYTES
    );
    assert_eq!(
        disk_reserve_look(&state, &reserve, t.level(plenty, None), plenty, &t, 3),
        ReserveMove::Kept,
        "written once"
    );
    let full = Some(0);
    assert_eq!(
        disk_reserve_look(&state, &reserve, t.level(full, None), full, &t, 4),
        ReserveMove::Released(disk::reserve::RESERVE_BYTES)
    );
    assert!(!reserve.exists());
    assert_eq!(rows(), ["written", "released"]);

    // The link: a symlink to a victim file on Unix; on Windows a junction to
    // a victim folder (a file symlink needs a privilege a plain account lacks).
    let victim = tmp.path().join("victim");
    #[cfg(unix)]
    let kept = {
        std::fs::write(&victim, b"keep").expect("victim");
        std::os::unix::fs::symlink(&victim, &reserve).expect("link");
        victim.clone()
    };
    #[cfg(windows)]
    let kept = {
        std::fs::create_dir_all(&victim).expect("victim");
        std::fs::write(victim.join("keep"), b"keep").expect("victim");
        atpkg::platform::atomic_symlink(&victim, &reserve).expect("junction");
        victim.join("keep")
    };
    assert!(matches!(
        disk_reserve_look(&state, &reserve, disk::Level::Emergency, full, &t, 5),
        ReserveMove::Refused(_)
    ));
    assert_eq!(std::fs::read(&kept).expect("victim"), b"keep");
    assert_eq!(rows(), ["written", "released", "refused"]);
}

/// A COMPRESSED RESERVE FOLDER IS REFUSED, AND JOURNALLED ONCE (Windows): a
/// reserve folder that compresses what is written into it (a drive compressed
/// whole) would hold a reserve short of its blocks, so every look refuses it,
/// and only the first writes its row ([`ReserveMove::StillRefused`] after). A
/// write ends the run: refused again later, it is journalled again. NEGATIVE
/// CONTROL: uncompressed, the same folder takes the reserve.
#[cfg(windows)]
#[test]
fn a_compressed_reserve_folder_is_refused_and_journalled_once() {
    let tmp = tmp_dir("reserve-refused-once");
    let state = tmp.path().join("state");
    let reserve = disk::reserve::path_for(&tmp.path().join("home"));
    let folder = reserve.parent().expect("folder").to_path_buf();
    std::fs::create_dir_all(&folder).expect("folder");
    let compact = |flag: &str| {
        let out = aterm_uds::spawnfd::command("compact")
            .args([flag, "/q"])
            .arg(&folder)
            .output()
            .expect("compact ran");
        assert!(out.status.success(), "compact {flag}");
    };
    let rows = || -> Vec<String> {
        std::fs::read_to_string(state.join(DISK_LEDGER))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let v: Value = aterm_json::from_str(l).expect("a row");
                v["action"].as_str().unwrap_or_default().to_owned()
            })
            .collect()
    };
    let t = disk::Config::default().thresholds(Some(460 << 30));
    let plenty = Some(t.target + disk::reserve::RESERVE_BYTES);
    let look = |now| disk_reserve_look(&state, &reserve, t.level(plenty, None), plenty, &t, now);
    compact("/c");
    assert!(
        matches!(look(1), ReserveMove::Refused(ref why) if why.contains("compresses")),
        "the first look"
    );
    assert_eq!(look(2), ReserveMove::StillRefused);
    assert_eq!(look(3), ReserveMove::StillRefused);
    assert_eq!(rows(), ["refused"]);
    compact("/u");
    assert_eq!(look(4), ReserveMove::Written);
    let full = Some(0);
    assert_eq!(
        disk_reserve_look(&state, &reserve, t.level(full, None), full, &t, 5),
        ReserveMove::Released(disk::reserve::RESERVE_BYTES)
    );
    compact("/c");
    assert!(matches!(look(6), ReserveMove::Refused(_)));
    assert_eq!(look(7), ReserveMove::StillRefused);
    assert_eq!(rows(), ["refused", "written", "released", "refused"]);
}

/// THE SWEEP'S LEDGER ROWS SAY WHY EACH BUILD CACHE WENT, with the figures
/// it was taken on: a gone workspace's names its idle time and the sweep's
/// window (a day while the volume is only low, not a fixed ten minutes); one
/// taken for a month unwritten names that and never says its workspace was
/// removed; an earlier sweep's half-removed stage says so. The intent row and
/// the removal row carry the same words.
#[test]
fn the_sweep_rows_say_why_each_build_cache_went() {
    use atpkg::build_cache::{RECLAIM_GONE, Unused};
    let tmp = tmp_dir("sweep-why");
    let state = tmp.path().join("state");
    let path = Path::new("/c/ws/.cargo-build");
    let cases = [
        (
            Unused::Gone {
                idle: std::time::Duration::from_secs(26 * 3_600),
                after: RECLAIM_GONE,
            },
            "its workspace is gone, and nothing wrote it for 26h (the sweep takes one unwritten 24h)",
        ),
        (
            Unused::Idle {
                idle: std::time::Duration::from_secs(31 * 86_400),
            },
            "nothing wrote it for 31d, a month or more, whatever its workspace's state",
        ),
        (
            Unused::Unfinished,
            "an earlier sweep renamed it aside and did not finish removing it",
        ),
    ];
    for (why, _) in cases {
        assert!(disk_sweep_journal(&state, path, 7, why, false, 1));
        assert!(disk_sweep_journal(&state, path, 7, why, true, 2));
    }
    let rows: Vec<Value> = std::fs::read_to_string(state.join(DISK_LEDGER))
        .expect("the ledger")
        .lines()
        .map(|l| aterm_json::from_str(l).expect("a row"))
        .collect();
    assert_eq!(rows.len(), 6);
    for (pair, (_, words)) in rows.chunks(2).zip(cases) {
        let actions: Vec<&str> = pair
            .iter()
            .map(|r| r["action"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(actions, ["removing", "removed"]);
        for r in pair {
            let text = r["text"].as_str().unwrap_or_default();
            assert!(text.contains(words), "{text}");
            assert!(!text.contains("ten minutes"), "{text}");
        }
    }
    let idle = rows[2]["text"].as_str().unwrap_or_default();
    assert!(
        !idle.contains("gone") && !idle.contains("removed workspace"),
        "{idle}"
    );
}

/// A REFUSED RESERVE IS JOURNALLED ONCE PER REASON: a folder at the reserve's
/// path refuses every look while the disk is healthy (the watch looks every
/// minute), and three looks put one `refused` row in the ledger, the later two
/// [`ReserveMove::StillRefused`] (nothing for the log either). Another reason
/// (the reserve's folder a file) is journalled; once the fault clears and the
/// reserve is written, the same refusal as before is news again.
#[test]
fn a_refused_reserve_is_journalled_once_per_reason() {
    let tmp = tmp_dir("reserve-refused");
    let state = tmp.path().join("state");
    let reserve = disk::reserve::path_for(&tmp.path().join("home"));
    let t = disk::Config::default().thresholds(Some(460 << 30));
    let plenty = Some(t.target + disk::reserve::RESERVE_BYTES);
    let level = t.level(plenty, None);
    let rows = || -> Vec<(String, String)> {
        std::fs::read_to_string(state.join(DISK_LEDGER))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let v: Value = aterm_json::from_str(l).expect("a row");
                (
                    v["action"].as_str().unwrap_or_default().to_owned(),
                    v["text"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect()
    };
    let look = |now| disk_reserve_look(&state, &reserve, level, plenty, &t, now);
    std::fs::create_dir_all(&reserve).expect("a folder in the reserve's place");
    assert!(matches!(look(1), ReserveMove::Refused(_)));
    assert_eq!(look(2), ReserveMove::StillRefused);
    assert_eq!(look(3), ReserveMove::StillRefused);
    assert_eq!(rows().len(), 1, "{:?}", rows());
    assert!(reserve.is_dir(), "left alone");

    // Another reason: the reserve's own folder is a file.
    let folder = reserve.parent().expect("its folder").to_path_buf();
    std::fs::remove_dir_all(&folder).expect("cleared");
    std::fs::write(&folder, b"not a folder").expect("a file");
    assert!(matches!(look(4), ReserveMove::Refused(_)));
    assert_eq!(look(5), ReserveMove::StillRefused);

    // Cleared and written: a refusal after it is news again.
    std::fs::remove_file(&folder).expect("cleared");
    assert_eq!(look(6), ReserveMove::Written);
    std::fs::remove_file(&reserve).expect("taken away");
    std::fs::create_dir_all(&reserve).expect("a folder again");
    assert!(matches!(look(7), ReserveMove::Refused(_)));
    let actions: Vec<String> = rows().into_iter().map(|(a, _)| a).collect();
    assert_eq!(actions, ["refused", "refused", "written", "refused"]);
    let texts = rows();
    assert_ne!(texts[0].1, texts[1].1, "two reasons");
    assert_eq!(texts[0].1, texts[3].1, "the first reason, news again");
}

/// A FAILED RESERVE WRITE IS NOT TRIED AT EVERY LOOK: a write that fails
/// partway (a quota, a failing disk) wrote up to `RESERVE_BYTES` first, so no
/// look within [`RESERVE_REWRITE_AFTER_SECS`] of it writes again, even once
/// the fault has cleared, and the first look past it writes the reserve. The
/// fault here is a folder that refuses new files (mode 0500), which fails the
/// write as a quota would. NEGATIVE CONTROL: a refusal that writes nothing (a
/// folder in the reserve's place) is judged again at the next look, which
/// writes the reserve once the folder has gone.
#[cfg(unix)]
#[test]
fn a_failed_reserve_write_waits_before_it_is_tried_again() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tmp_dir("reserve-unwritten");
    let state = tmp.path().join("state");
    let t = disk::Config::default().thresholds(Some(460 << 30));
    let plenty = Some(t.target + disk::reserve::RESERVE_BYTES);
    let level = t.level(plenty, None);
    let look = |reserve: &Path, now| disk_reserve_look(&state, reserve, level, plenty, &t, now);
    let mode = |p: &Path, m| {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).expect("its mode");
    };

    let reserve = disk::reserve::path_for(&tmp.path().join("home"));
    let folder = reserve.parent().expect("its folder").to_path_buf();
    std::fs::create_dir_all(&folder).expect("its folder");
    mode(&folder, 0o500);
    let ReserveMove::Refused(why) = look(&reserve, 100) else {
        panic!("a folder that refuses new files refuses the write")
    };
    assert!(why.contains("could not be written"), "{why}");
    mode(&folder, 0o700);
    assert_eq!(look(&reserve, 160), ReserveMove::StillRefused);
    assert_eq!(
        look(&reserve, 100 + RESERVE_REWRITE_AFTER_SECS - 1),
        ReserveMove::StillRefused
    );
    assert!(!reserve.exists(), "no write within the wait");
    assert_eq!(
        look(&reserve, 100 + RESERVE_REWRITE_AFTER_SECS),
        ReserveMove::Written
    );
    std::fs::remove_file(&reserve).expect("taken away");

    // NEGATIVE CONTROL: a refusal that wrote nothing is judged again at once.
    let other = disk::reserve::path_for(&tmp.path().join("other"));
    std::fs::create_dir_all(&other).expect("a folder in its place");
    assert!(matches!(look(&other, 100), ReserveMove::Refused(_)));
    std::fs::remove_dir(&other).expect("cleared");
    assert_eq!(look(&other, 160), ReserveMove::Written);
    std::fs::remove_file(&other).expect("taken away");
}

/// A state-preserving cut streams bounded main rows and reads predecessors
/// within the input cap; anything it cannot read whole keeps the ledger as it is.
#[test]
fn a_stateful_ledger_cut_keeps_input_it_cannot_read_whole() {
    let tmp = tmp_dir("ledger-cut-input");
    let path = tmp.path().join("ledger.jsonl");
    let side = tmp.path().join("earlier.jsonl");
    let original = "one\ntwo\nthree\n";
    std::fs::write(&path, original).expect("ledger");
    let called = std::cell::Cell::new(false);
    let cut = |rows: &mut dyn Iterator<Item = String>, _: &dyn Fn(&Path) -> Option<String>| {
        called.set(true);
        Some(vec![rows.last().expect("row")])
    };
    assert!(!bound_ledger_cut_with_cap(&path, 100, 64, &cut).unwrap());
    assert!(!called.get());
    assert!(bound_ledger_cut_with_cap(&path, 1, 64, &cut).unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "three\n");
    std::fs::write(&path, original).expect("ledger");
    std::fs::write(&side, "standing\n").expect("earlier ledger");
    let earlier = |rows: &mut dyn Iterator<Item = String>,
                   read: &dyn Fn(&Path) -> Option<String>| {
        rows.for_each(drop);
        Some(read(&side).map_or_else(Vec::new, |body| body.lines().map(str::to_string).collect()))
    };
    assert!(bound_ledger_cut_with_cap(&path, 1, 64, &earlier).unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "standing\n");
    std::fs::write(&path, original).expect("ledger");
    // The main input is streamed: one past the input cap is cut.
    assert!(bound_ledger_cut_with_cap(&path, 1, 8, &cut).unwrap());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "three\n",
        "a main input past the cap is streamed and cut"
    );
    // One oversized row refuses before it can grow the line buffer past cap + 1.
    let oversized = "one\n123456789\nthree\n";
    std::fs::write(&path, oversized).expect("oversized row");
    assert!(!bound_ledger_cut_with_cap(&path, 1, 8, &cut).unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), oversized);
    let boundary = "1234567\n";
    std::fs::write(&path, boundary).expect("row at the bound");
    assert!(bound_ledger_cut_with_cap(&path, 1, 8, &cut).unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), boundary);
    std::fs::write(&path, original).expect("ledger");
    // A cut that leaves rows unread, or keeps the file, keeps it.
    let partial = |rows: &mut dyn Iterator<Item = String>, _: &dyn Fn(&Path) -> Option<String>| {
        Some(rows.next().into_iter().collect())
    };
    assert!(!bound_ledger_cut_with_cap(&path, 1, 64, &partial).unwrap());
    let keep = |rows: &mut dyn Iterator<Item = String>, _: &dyn Fn(&Path) -> Option<String>| {
        rows.for_each(drop);
        None
    };
    assert!(!bound_ledger_cut_with_cap(&path, 1, 64, &keep).unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    // A main input that is not UTF-8 is no input read whole.
    std::fs::write(&path, b"one\n\xff\nthree\n").expect("invalid main text");
    assert!(!bound_ledger_cut_with_cap(&path, 1, 64, &cut).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), b"one\n\xff\nthree\n");
    std::fs::write(&path, original).expect("ledger");
    assert!(!bound_ledger_cut_with_cap(&path, 1, 4, &earlier).unwrap());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        original,
        "UNKNOWN_PREDECESSOR_MUST_KEEP_LEDGER"
    );
    std::fs::write(&side, [0xff]).expect("invalid predecessor text");
    assert!(!bound_ledger_cut_with_cap(&path, 1, 64, &earlier).unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    std::fs::remove_file(&side).expect("remove predecessor");
    std::fs::create_dir(&side).expect("nonregular predecessor");
    assert!(!bound_ledger_cut_with_cap(&path, 1, 64, &earlier).unwrap());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}
