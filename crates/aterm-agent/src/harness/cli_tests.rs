// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the `aterm harness` command: the four read views, the retired
//! hook-bridge tombstones, and the refusal of the verbs deleted with the
//! second harness stack.
//!
//! Every path and clock is injected through [`Env`]; the one verb that opens
//! a control connection (`limits`) is driven through a scripted [`Ctl`].

use aterm_phase::prompt::fixtures::{composer, rows};

use super::*;
use crate::supervise::run::CtlReply;

/// A fresh, empty directory under the system temp dir, removed on drop.
struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
        let mut dir = std::env::temp_dir();
        let unique = format!(
            "aterm-harness-cli-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        dir.push(unique);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Tmp(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An [`Env`] rooted in `tmp`, with every clock and path injected.
fn env_at(tmp: &Tmp) -> Env {
    Env {
        state: tmp.path().join("state"),
        aterm_state: Some(tmp.path().join("aterm-state")),
        cwd: tmp.path().join("work"),
        home: Some(tmp.path().join("home")),
        claude_dir: None,
        claude_config_unusable: None,
        claude_session: None,
        claude_pid: None,
        now: 1_758_412_800, // 2025-09-21T00:00:00Z — fixed, never `now()`.
        sid: "s-test".to_string(),
        sock: None,
        config: Some(tmp.path().join("aterm.toml")),
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
        ("upgrade models", Cmd::UpgradeModels { set: None }),
        (
            "upgrade models set claude-opus-5-5,claude-fable-5-1",
            Cmd::UpgradeModels {
                set: Some("claude-opus-5-5,claude-fable-5-1".to_string()),
            },
        ),
        ("--help", Cmd::Help),
    ];
    for (line, want) in cases {
        let (got, _) = parse(&words(line)).unwrap_or_else(|e| panic!("{line}: {e}"));
        assert_eq!(got, want, "{line}");
    }
    assert_eq!(parse(&[]).expect("empty").0, Cmd::Help);
    let (_, over) = parse(&words("usage --state /s --config /c.toml --sock /a.sock"))
        .expect("the environment flags parse");
    assert_eq!(over.state, Some(PathBuf::from("/s")));
    assert_eq!(over.config, Some(PathBuf::from("/c.toml")));
    assert_eq!(over.sock.as_deref(), Some("/a.sock"));
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
        // Retired 2026-09-27 with the windows it placed on the clock.
        "usage --utc-offset 3600",
        "usage --state",
        "upgrade abc",
        // The loop is gone: the window's host takes the steps, on a push.
        "upgrade --every 60",
    ] {
        assert!(parse(&words(line)).is_err(), "{line} should be refused");
    }
}

/// THE DELETED VERBS are refused BY NAME, and the refusal says where the
/// capability went. Negative control: the four kept verbs still parse, so the
/// refusal is the deleted set and not the front door.
#[test]
fn the_deleted_verbs_are_refused_by_name_and_point_at_the_engine() {
    for verb in DELETED {
        let e = parse(&words(verb)).expect_err(verb);
        assert!(e.contains("is gone"), "{verb}: {e}");
        // `config` also held main's live-upgrade switch: its refusal says
        // where that went, and no other deleted verb's does.
        assert_eq!(
            e.contains("`upgrade` in that table"),
            *verb == "config",
            "{verb}: {e}"
        );
        assert!(e.contains("aterm drive"), "{verb}: {e}");
        assert!(
            USAGE.contains(&format!("`{verb}`")),
            "the usage names {verb} as deleted"
        );
    }
    for kept in ["usage", "limits", "disk", "ledger", "upgrade"] {
        assert!(parse(&words(kept)).is_ok(), "{kept}");
        assert!(!DELETED.contains(&kept));
    }
    // The flags only the deleted verbs took are gone from the grammar too.
    for flag in [
        "--commands",
        "--assume-spine",
        "--nonce",
        "--settings",
        "--tree",
        "--passes",
        "--since",
        "--write",
    ] {
        assert!(
            parse(&words(&format!("usage {flag}"))).is_err(),
            "{flag} is still accepted"
        );
        assert!(!USAGE.contains(flag), "the usage still offers {flag}");
    }
}

// ---------------------------------------------------------------------------
// install / uninstall / hook / statusline — retired (decision "B")
// ---------------------------------------------------------------------------

/// `install` and `uninstall` refuse with the one decision-"B" sentence, exit
/// 2, and write nothing — whatever flags precede the verb. `hook` and
/// `statusline` are silent successes, the answer an old install's bridge
/// script needs so it can never block a turn. Negative control: a read verb
/// on the same env still answers normally.
#[test]
fn the_retired_verbs_refuse_or_answer_silently_and_write_nothing() {
    let tmp = Tmp::new("retired");
    let env = env_at(&tmp);
    for line in ["install", "uninstall", "--state /nowhere install"] {
        let (cmd, _) = parse(&words(line)).expect("parses to the refusal");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(&cmd, &env, &mut out, &mut err);
        assert_eq!(
            format!("{code:?}"),
            format!("{:?}", ExitCode::from(2)),
            "{line}"
        );
        let err = String::from_utf8_lossy(&err);
        assert!(err.contains(RETIRED), "{line}: {err}");
        assert!(out.is_empty(), "{line}");
    }
    for line in ["hook PermissionRequest rm-approve", "hook", "statusline"] {
        let (cmd, _) = parse(&words(line)).expect("parses to the tombstone");
        let (ok, out, err) = go(&cmd, &env);
        assert!(ok, "{line}: a hook must never exit non-zero");
        assert!(out.is_empty() && err.is_empty(), "{line}: {out:?} {err:?}");
    }
    assert!(!env.state.exists(), "no state was written");

    let (ok, _, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(ok, "a read verb still answers");
}

/// The front door's own answer, stdin injected. From a pipe (the vendor's
/// shape) `hook` and `statusline` read the payload to the end, print nothing
/// and exit 0. Typed at a terminal they read NOTHING — a reader that panics
/// on use proves it — and say in one stderr line that the verb is retired,
/// still exit 0. Negative control: `install` refuses (exit 2) on either
/// stdin, so the silence is the two tombstones and not every retired verb.
#[test]
fn the_retired_hook_verbs_drain_a_pipe_and_never_wait_on_a_terminal() {
    struct NeverRead;
    impl io::Read for NeverRead {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("a terminal stdin must not be read");
        }
    }
    for verb in ["hook", "statusline"] {
        // Over the 1 MiB the live hook once read: a `PostToolUse` for a Write
        // carries the whole file, and a drain that stopped short would close
        // the pipe on the vendor's write.
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
    for tty in [false, true] {
        let mut err = Vec::new();
        let code = answer_retired("install", tty, &mut NeverRead, &mut err);
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(2)));
        assert!(String::from_utf8_lossy(&err).contains(RETIRED));
    }
    for gone in [
        "harness hook",
        "harness statusline",
        "harness install",
        "harness uninstall",
    ] {
        assert!(!USAGE.contains(gone), "the usage still offers `{gone}`");
    }
}

// ---------------------------------------------------------------------------
// usage
// ---------------------------------------------------------------------------

#[test]
fn the_project_directory_name_reproduces_the_two_measured_ones() {
    // MEASURED 2026-09-21 by listing `~/.claude/projects` (the home prefix is
    // anonymised; the fold is the same for any path).
    assert_eq!(
        project_dir_name(Path::new("/home/me/repo")),
        "-home-me-repo"
    );
    assert_eq!(
        project_dir_name(Path::new(
            "/home/me/repo/.claude/worktrees/kitty-motion-wave"
        )),
        "-home-me-repo--claude-worktrees-kitty-motion-wave"
    );
}

/// `usage` folds the NEWEST transcript in this directory's project directory
/// and says that is what it did — the rung that names a session arrived on
/// the retired hook bridge. Negative control first: no transcript is
/// `source=none` and says so, never a row of zeros dressed as a reading.
#[test]
fn usage_folds_the_newest_transcript_and_says_what_that_costs() {
    let tmp = Tmp::new("usage");
    let env = env_at(&tmp);
    let (ok, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(ok);
    assert!(out.contains("source=none"), "{out}");
    assert!(out.contains("no transcript for this directory"), "{out}");

    let dir = env
        .transcripts_root()
        .expect("home is injected")
        .join(project_dir_name(&env.cwd));
    std::fs::create_dir_all(&dir).expect("project dir");
    let row = |id: &str, model: &str, input: u64, output: u64| {
        format!(
            r#"{{"type":"assistant","message":{{"id":"{id}","model":"{model}","usage":{{"input_tokens":{input},"output_tokens":{output}}}}}}}"#
        )
    };
    std::fs::write(
        dir.join("session-a.jsonl"),
        format!(
            "{}\n{}\nnot json at all\n",
            row("m1", "claude-fable-5-1", 100, 20),
            row("m2", "claude-fable-5-1", 7, 3)
        ),
    )
    .expect("transcript");

    let (ok, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(ok);
    // The HUD line, whole: the model the newest row names, and no window —
    // nothing here reads one (the statusLine reader went 2026-09-27; every
    // live `harness usage` printed `?%` before it did too).
    assert_eq!(
        out.lines().next(),
        Some("fable ?%/5h · ?%/7d · acct account · resets ? · none  [source=transcript-newest]"),
        "{out}"
    );
    assert!(out.contains("source=transcript-newest"), "{out}");
    assert!(out.contains("session-a.jsonl"), "the path is named: {out}");
    assert!(out.contains("2 assistant rows"), "{out}");
    assert!(
        out.contains("may be the other one's"),
        "the ambiguity is printed, not hidden: {out}"
    );
    let (_, json, _) = go(&Cmd::Usage { json: true }, &env);
    assert!(json.contains("\"source\":\"transcript-newest\""), "{json}");
    assert!(
        json.contains("\"transcript_pick\":\"newest-mtime\""),
        "{json}"
    );
    assert!(json.contains("claude-fable-5-1"), "{json}");
    assert!(json.contains("107"), "the input tokens are summed: {json}");
    // Schema 1's `windows` stays an empty object, and the model is named.
    assert!(json.contains("\"windows\":{}"), "{json}");
    assert!(json.contains("\"model\":\"claude-fable-5-1\""), "{json}");
}

/// A `CLAUDE_CONFIG_DIR` set to a RELATIVE path names a directory relative
/// to Claude Code's own working directory, which this command cannot know:
/// `usage` folds nothing and says why, rather than folding `~/.claude`'s
/// newest transcript — another configuration's sessions. Control: unset, the
/// same `~/.claude` transcript folds.
#[test]
fn usage_with_a_relative_claude_config_dir_folds_nothing_and_says_so() {
    let tmp = Tmp::new("usage-relative-config");
    let mut env = env_at(&tmp);
    let dir = env
        .transcripts_root()
        .expect("home is injected")
        .join(project_dir_name(&env.cwd));
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
    assert!(
        out.contains("CLAUDE_CONFIG_DIR is \"cfg\", not an absolute path"),
        "{out}"
    );
    let (_, json, _) = go(&Cmd::Usage { json: true }, &env);
    let doc: Value = aterm_json::from_str(&json).expect("json");
    assert_eq!(doc["transcript_pick"].as_str(), Some("none"), "{json}");
    assert_eq!(
        doc["claude_config_dir_unusable"].as_str(),
        Some("cfg"),
        "{json}"
    );
}

/// Run from a Claude Code session, `usage` folds THAT session's transcript —
/// the one `CLAUDE_CODE_SESSION_ID` names — and its subagents', never the
/// newer file another session in the same directory wrote. Named but not yet
/// written is nothing, not the neighbour's file. `CLAUDE_PID`'s sessions
/// file names it too, and `CLAUDE_CONFIG_DIR` is where it is looked for.
/// Negative control: without a name, the newest (the neighbour's) is what
/// folds, labelled so. The JSON only ADDS keys to schema 1.
#[test]
fn usage_folds_the_session_claude_code_names_not_the_newest() {
    let tmp = Tmp::new("usage-named");
    let mut env = env_at(&tmp);
    // A Claude directory of its own (`CLAUDE_CONFIG_DIR`), not `~/.claude`.
    env.claude_dir = Some(tmp.path().join("claude-config"));
    let dir = env
        .transcripts_root()
        .expect("a claude dir")
        .join(project_dir_name(&env.cwd));
    assert!(dir.starts_with(tmp.path().join("claude-config")), "{dir:?}");
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
    // The neighbour wrote LAST: newest-by-mtime would pick it.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(
        dir.join("theirs-2.jsonl"),
        row("t1", "claude-opus-5-5", 99_999),
    )
    .expect("theirs");

    env.claude_session = Some("mine-1".to_owned());
    let (ok, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(ok);
    assert!(out.contains("source=transcript]"), "{out}");
    assert!(out.contains("mine-1.jsonl"), "the path is named: {out}");
    assert!(out.contains("+1 subagent transcripts"), "{out}");
    assert!(
        out.contains("opus 1 in 1.2k out"),
        "this session's opus: {out}"
    );
    assert!(
        out.contains("haiku 1 in 40 out"),
        "its subagent's haiku, at its final figures: {out}"
    );
    let token_lines: String = out
        .lines()
        .take_while(|l| !l.starts_with("SPEND folded from"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !token_lines.contains("99"),
        "never the neighbour's tokens: {out}"
    );
    assert!(out.contains("CLAUDE_CODE_SESSION_ID"), "{out}");
    let (_, json, _) = go(&Cmd::Usage { json: true }, &env);
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
    let haiku = &doc["usage"]["accounts"][0]["spend"]["claude-haiku-4-5"];
    assert_eq!(haiku["out"].as_u64(), Some(40), "{json}");

    // A subagent transcript that exists and cannot be read is NOT in the
    // totals, and the output says so.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let locked = dir.join("mine-1").join("subagents").join("agent-b.jsonl");
        std::fs::write(&locked, row("s2", "claude-haiku-4-5", 7)).expect("subagent");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        let (_, out, _) = go(&Cmd::Usage { json: false }, &env);
        let (_, json, _) = go(&Cmd::Usage { json: true }, &env);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        std::fs::remove_file(&locked).expect("unlock");
        assert!(out.contains("+1 unread"), "{out}");
        assert!(
            out.contains("1 subagent transcripts are NOT in these totals"),
            "{out}"
        );
        assert!(
            out.contains("past the 2048 files one session's fold follows"),
            "{out}"
        );
        assert!(!out.contains("PREFIX"), "the cap did not stop it: {out}");
        assert!(json.contains("\"complete\":false"), "{json}");
        assert!(json.contains("\"subagent_transcripts_unread\":1"), "{json}");
    }

    env.claude_session = Some("not-written-yet".to_owned());
    let (ok, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(ok);
    assert!(out.contains("source=none"), "{out}");
    assert!(
        out.contains("not-written-yet has written no transcript"),
        "{out}"
    );

    // A build that names only the PROCESS (`CLAUDE_PID`).
    env.claude_session = None;
    let claude = env.claude_dir().expect("a claude dir");
    std::fs::create_dir_all(claude.join("sessions")).expect("sessions dir");
    std::fs::write(
        claude.join("sessions").join("777.json"),
        format!(
            r#"{{"pid":777,"sessionId":"mine-1","cwd":"{}"}}"#,
            env.cwd.display()
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
    let (_, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(out.contains("named by CLAUDE_PID"), "{out}");
    env.claude_pid = None;

    let (_, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(
        out.contains("theirs-2.jsonl"),
        "the control: newest wins: {out}"
    );
    assert!(out.contains("source=transcript-newest"), "{out}");
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

/// The wall is the engine's own reading (`aterm_phase::wall`) and its reset
/// is placed by the supervisor's clock — nothing is re-parsed here. NEGATIVE
/// CONTROL: the same frame without the notice shows no wall.
#[test]
fn the_view_is_the_engines_wall_and_the_painted_windows() {
    let now = 1_758_412_800;
    let zone_none = |_: &str| -> Option<i64> { None };
    let v = limits_view(&limited_screen(), now, 0, zone_none);
    let wall = v.wall.as_ref().expect("a wall is read");
    assert_eq!(wall.kind, aterm_phase::WallKind::UsageSession);
    assert_eq!(v.resets_at, Some(now + 3 * 3600));
    assert!(v.windows.is_empty(), "{v:?}");
    let idle = limits_view(&idle_screen(), now, 0, zone_none);
    assert_eq!(idle.wall, None);
    assert!(idle.windows.is_empty());
}

/// Claude Code's critical-memory banner (2026-09-24) is the engine's own
/// wall, `wall=memory`, and no limit: a restart ends it, not a wait, so it
/// has no reset. Its words say `usage` and `continue`, and neither the wall
/// table (it places the banner by position, never by phrase) nor the painted
/// `/usage` reader read them as a usage window.
#[test]
fn the_memory_banner_is_its_own_wall_and_no_limit() {
    use aterm_phase::prompt::fixtures::{MEMORY_BANNER_IDLE, screen};
    let zone_none = |_: &str| -> Option<i64> { None };
    let v = limits_view(&screen(MEMORY_BANNER_IDLE), 1_758_412_800, 0, zone_none);
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
    let text = format!(
        "{} (140.4GB) \u{2014} restart and resume with claude --continue",
        aterm_phase::anchor_text("wall.memory")
    );
    assert_eq!(aterm_phase::classify_wall(&text), None, "never by phrase");
}

/// `limits` makes ONE screen read of the named session and prints its wall
/// at `source=grid`. NEGATIVE CONTROLS: an idle screen is `wall=none`, and a
/// read that fails is exit 1 naming the session — never `wall=none`, which
/// would read as "measured, nothing wrong".
#[test]
fn limits_reads_the_sessions_screen_once_and_names_its_wall() {
    let tmp = Tmp::new("limits");
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
    assert_eq!(ctl.asked.len(), 1, "one read: {:?}", ctl.asked);
    assert!(
        ctl.asked[0].starts_with("@s-worker text --json"),
        "{:?}",
        ctl.asked
    );

    // No sid on the line: this session's, from the injected env.
    let mut ctl = scripted(text_json(&limited_screen()), 0, "");
    let (_, json, _) = run_with(&mut ctl, None, true);
    assert!(ctl.asked[0].starts_with("@s-test text"), "{:?}", ctl.asked);
    assert!(json.contains("\"schema\":2"), "{json}");
    assert!(json.contains("\"kind\":\"usage-session\""), "{json}");
    assert!(json.contains("\"windows\":[]"), "{json}");
    assert!(json.contains("\"source\":\"grid\""), "{json}");

    let mut ctl = scripted(text_json(&idle_screen()), 0, "");
    let (code, out, _) = run_with(&mut ctl, Some("@s-worker"), false);
    assert_eq!(code, ok);
    assert!(out.starts_with("wall=none source=grid"), "{out}");

    let mut ctl = scripted(String::new(), 1, "ERR no such session @s-gone");
    let (code, out, err) = run_with(&mut ctl, Some("@s-gone"), false);
    assert_eq!(code, format!("{:?}", ExitCode::from(1)));
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("@s-gone"), "{err}");
}

/// **NO SESSION, NO READ.** Outside an aterm session (`$ATERM_PARENT_SESSION_ID`
/// empty) and with no `@<sid>` on the line, `limits` used to send an
/// unaddressed read — classifying whichever session the socket defaults to —
/// and print `wall=… source=grid` naming nobody. It refuses, exit 2, and
/// reads nothing. NEGATIVE CONTROL: the same env with a sid on the line reads
/// that session.
#[test]
fn limits_with_no_session_to_read_refuses_and_reads_nothing() {
    let tmp = Tmp::new("limits-nosid");
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
        ctl.asked[0].starts_with("@s-named text --json"),
        "{:?}",
        ctl.asked
    );
}

// ---------------------------------------------------------------------------
// disk
// ---------------------------------------------------------------------------

/// The report answers, removes nothing by default, and is journalled to the
/// disk ledger `ledger disk` reads back.
#[test]
fn the_disk_report_names_nothing_removable_by_default_and_is_journalled() {
    let tmp = Tmp::new("disk");
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
    assert!(out.contains("report only"), "{out}");
    assert!(
        !out.contains("report only: report only"),
        "said twice: {out}"
    );
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

/// **THE DISK JOURNAL IS BOUNDED.** A journal past the bound is cut back to
/// its newest rows before the run appends, so a verb that exists to act on a
/// full disk never grows its own file for ever. NEGATIVE CONTROL: a journal
/// under the bound is left byte-for-byte alone.
#[test]
fn the_disk_journal_is_cut_back_to_its_newest_rows_past_the_bound() {
    let tmp = Tmp::new("disk-bound");
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

    // NEGATIVE CONTROL: under the bound nothing is cut, and the pure cut
    // says so.
    let before = std::fs::read(&path).unwrap();
    assert!(!bound_ledger(&path, DISK_LEDGER_MAX_BYTES, DISK_LEDGER_KEEP_ROWS).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    // A missing journal is not an error.
    assert!(!bound_ledger(&tmp.path().join("none.jsonl"), 1, 1).unwrap());
    // The help states the bound the code applies.
    assert!(USAGE.contains(&format!("newest {DISK_LEDGER_KEEP_ROWS} rows")));
    assert!(USAGE.contains(&format!("{} MiB", DISK_LEDGER_MAX_BYTES / (1024 * 1024))));
}

/// `now` far enough past the files a test just wrote that every profile in
/// them is idle.
fn idle_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap())
        .unwrap()
        + (disk::DEFAULT_TARGET_STALE_DAYS + 1) * 86_400
}

/// The real remover the window's tick runs, at `now`, on any device.
fn real_remover(now: i64) -> impl FnMut(&disk::Row) -> io::Result<disk::Removed> {
    disk::remover(disk::Judge {
        now,
        threshold_days: disk::DEFAULT_TARGET_STALE_DAYS,
        device: None,
    })
}

/// THE HOST'S TICK, ON A BUILD DIRECTORY LAID OUT AS CARGO LAYS ONE: at or
/// above the automatic floor it reads nothing and writes nothing; below it
/// the idle profile's `incremental/` — and nothing else of the build
/// directory — is reclaimed, and the report, the intent row (BEFORE the
/// removal) and the outcome row (with its witness, the bytes actually
/// released and the unit that went) land in the disk ledger, in that order.
/// `[disk] auto_free_gib` moves the floor; `0` turns it off.
///
/// The fixture plants NO root `.cargo-lock` (cargo never writes one); the
/// witness that dated a build directory by one could never pass here.
#[test]
fn the_disk_tick_reclaims_stale_targets_below_the_floor_and_nothing_above() {
    let tmp = Tmp::new("disk-tick");
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
        transcripts: None,
        now,
    };
    let plenty = disk_tick(
        &look(Some(cfg.auto_free_gib * gib)),
        cfg,
        &mut |row| panic!("removed {} above the floor", row.path.display()),
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

    let DiskTick::Reclaimed(done) = disk_tick(
        &look(Some(gib)),
        cfg,
        &mut real_remover(now),
        &mut disk::tests::no_pressure,
    ) else {
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

/// `aterm harness disk --help` states the idle window's shipped default, one
/// day, where it defines idle and again where it says what the tick takes
/// under the floor: least recently used first, the stop (the floor plus
/// [`disk::PRESSURE_MARGIN_GIB`], or the counted bytes), the minutes every
/// reclaim leaves alone ([`disk::PRESSURE_MIN_IDLE_S`]), the headline's
/// upper bound, and what the window still means.
#[test]
fn the_disk_help_states_the_one_day_window() {
    let flat = USAGE.split_whitespace().collect::<Vec<_>>().join(" ");
    let days = disk::DEFAULT_TARGET_STALE_DAYS;
    for said in [
        format!("`disk.target_stale_days` (default {days} day:"),
        format!("within `disk.target_stale_days` ({days} day), measuring free space again"),
        // The rule under pressure, and what the window still means.
        "LEAST RECENTLY USED FIRST: every idle profile's cache, then, one profile at a time \
         and oldest first"
            .to_owned(),
        format!(
            "stopping once it is back at the floor plus {} GiB",
            disk::PRESSURE_MARGIN_GIB
        ),
        // The second stop: the counted bytes, for a figure that does not
        // rise (review follow-up, 2026-09-28).
        "or once the bytes those removals released cover what free space was short".to_owned(),
        format!(
            "No reclaim, the tick's or this verb's, takes a profile written into within the last {} min",
            disk::PRESSURE_MIN_IDLE_S / 60
        ),
        "not even at `target_stale_days = 0`".to_owned(),
        "`under_pressure_up_to=` is what every one of them would free, an upper bound".to_owned(),
        "no longer keeps a recently built checkout's cache once the volume is under the floor"
            .to_owned(),
    ] {
        assert!(flat.contains(&said), "missing {said:?}:\n{USAGE}");
    }
}

/// Set the mtime of the directory at `path` to `at` (unix seconds).
fn set_mtime(path: &Path, at: i64) {
    let when = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(at).expect("after the epoch"));
    std::fs::File::open(path)
        .and_then(|f| f.set_modified(when))
        .expect("set mtime");
}

/// A build directory at `<tmp>/<name>` holding `debug/`, built just now, and
/// `release/`, whose compile outputs were last written two days before `now`.
fn two_day_old_release(tmp: &Tmp, name: &str, now: i64) -> PathBuf {
    let target = disk::tests::lay_target(&tmp.path().join(name));
    disk::tests::lay_profile(&target.join("release"));
    for c in ["deps", ".fingerprint", "build"] {
        set_mtime(&target.join("release").join(c), now - 2 * 86_400);
    }
    target
}

/// One tick below the floor over `target`, under `cfg` and on the real
/// remover judging by `cfg`'s own window, as the window's host runs it, free
/// space measured again through `measure`.
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
        transcripts: None,
        now,
    };
    let mut remover = disk::remover(disk::Judge {
        now,
        threshold_days: cfg.target_stale_days,
        device: None,
    });
    let DiskTick::Reclaimed(done) = disk_tick(&look, cfg, &mut remover, measure) else {
        panic!("below the floor, the tick applies");
    };
    done
}

/// THE SHIPPED WINDOW IS ONE DAY, AND IT REACHES THE CACHES AN AGENT LEFT.
/// The tick reads `[disk]` with no file (the default the window runs with),
/// on the real clock: one build directory holds a profile no compile has
/// written into for two days (`release/`) and one built just now
/// (`debug/`). Below the floor the idle one's `incremental/` goes and the
/// fresh one's stays. At the old default, 14 days, the two-day profile was
/// not idle and the tick took nothing: the agents' build directories are
/// written into daily (measured 2026-09-27), so it never freed a byte.
/// An explicit `target_stale_days = 14` is still read as written: the
/// two-day profile is not idle, so it is kept once free space measured
/// again is back — and, since 2026-09-28, taken (least recently used first)
/// while the volume is still short. The profile built just now is kept
/// either way: it is inside the ten minutes every build keeps.
#[test]
fn the_default_window_reclaims_a_profile_idle_two_days_and_keeps_one_built_today() {
    let tmp = Tmp::new("disk-window");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap())
        .unwrap();
    let target = two_day_old_release(&tmp, "repo/target", now);
    let cfg = disk_config_at(None);
    assert_eq!(cfg.target_stale_days, 1, "the shipped idle window");
    let done = tick_below_floor(&tmp, &target, now, cfg, &mut disk::tests::no_pressure);
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

    // An explicit setting is read as written: at 14 the same two-day
    // profile is not idle. Free space measured again is back above the
    // floor: the pressure pass stops before it, and nothing goes.
    let tmp = Tmp::new("disk-window-explicit");
    let target = two_day_old_release(&tmp, "repo/target", now);
    let config = tmp.path().join("aterm.toml");
    std::fs::write(&config, "[disk]\ntarget_stale_days = 14\n").expect("write");
    let cfg = disk_config_at(Some(&config));
    assert_eq!(cfg.target_stale_days, 14);
    let done = tick_below_floor(&tmp, &target, now, cfg, &mut || Some(u64::MAX));
    assert!(done.removed.is_empty(), "{done:?}");
    assert!(
        done.stopped.as_ref().is_some_and(|s| s.left == 1),
        "{done:?}"
    );
    assert!(target.join("release/incremental").exists());
    assert!(target.join("debug/incremental").exists());
    // Still short: the two-day profile goes, the one built just now stays.
    let done = tick_below_floor(&tmp, &target, now, cfg, &mut || Some(1));
    assert_eq!(done.removed.len(), 1, "{done:?}");
    assert!(done.removed[0].1.units[0].ends_with("release/incremental"));
    assert!(!target.join("release/incremental").exists());
    assert!(target.join("debug/incremental").exists());
}

/// THE HOST'S TICK UNDER PRESSURE, END TO END: every profile in the agents'
/// build directories is hours old (the machine measured 2026-09-27/28, where
/// the idle window alone took nothing). Below the floor the least recently
/// used profile gives up its `incremental/`; free space measured again is
/// back above the floor plus the margin, so the pass stops, and the disk
/// ledger holds the report, the intent, the outcome (its witness and the
/// bytes released) and the stop, in that order.
#[cfg(unix)]
#[test]
fn the_disk_tick_under_pressure_takes_the_oldest_profile_and_stops_when_free() {
    let tmp = Tmp::new("disk-pressure-tick");
    let state = tmp.path().join("state");
    let (a, b, now) = disk::pressure_tests::three_recent(tmp.path());
    let gib = 1024 * 1024 * 1024;
    let cfg = disk::Config::default();
    let targets = [a.clone(), b.clone()];
    let look = DiskLook {
        state: &state,
        volume: tmp.path(),
        free: Some(gib),
        targets: &targets,
        transcripts: None,
        now,
    };
    let mut reads = Vec::new();
    let mut measure = || {
        let free = if reads.is_empty() { gib } else { 20 * gib };
        reads.push(free);
        Some(free)
    };
    let DiskTick::Reclaimed(done) = disk_tick(&look, cfg, &mut real_remover(now), &mut measure)
    else {
        panic!("below the floor, the tick applies");
    };
    assert_eq!(
        reads.len(),
        2,
        "measured before each pressure row it reached"
    );
    assert_eq!(done.removed.len(), 1, "{done:?}");
    assert_eq!(done.removed[0].0.path, b.join("debug"));
    assert!(!b.join("debug/incremental").exists());
    assert!(a.join("debug/incremental").exists());
    assert!(a.join("release/incremental").exists());
    let ledger = std::fs::read_to_string(state.join(DISK_LEDGER)).expect("the ledger");
    let rows: Vec<&str> = ledger.lines().collect();
    assert_eq!(rows.len(), 4, "{ledger}");
    assert!(rows[0].contains("\"kind\":\"report\""), "{}", rows[0]);
    assert!(rows[1].contains("\"kind\":\"removing\""), "{}", rows[1]);
    assert!(rows[2].contains("\"kind\":\"removed\""), "{}", rows[2]);
    assert!(rows[2].contains("least-recently-used"), "{}", rows[2]);
    assert!(
        rows[2].contains(&format!("\"freed_bytes\":{}", done.freed_bytes)),
        "{}",
        rows[2]
    );
    assert!(rows[3].contains("\"kind\":\"stopped\""), "{}", rows[3]);
    assert!(rows[3].contains("\"left\":2"), "{}", rows[3]);
}

/// THE OWNER'S VERB SAYS WHAT IT WOULD TAKE UNDER PRESSURE, AND TAKES IT.
/// With the floor set above the scratch volume's free space (so `df` reads
/// it as short), `aterm harness disk <dirs>` lists every recent profile as
/// a least-recently-used row, OLDEST FIRST — not in path order — and says
/// the pass measures again before each; with `disk.apply = true`,
/// `--apply cargo-targets` takes them in that order while the volume stays
/// short, and journals each.
#[cfg(unix)]
#[test]
fn the_verb_says_what_it_would_take_under_pressure_in_order() {
    let tmp = Tmp::new("disk-pressure-verb");
    let (a, b, now) = disk::pressure_tests::three_recent(tmp.path());
    let env = Env {
        now,
        ..env_at(&tmp)
    };
    std::fs::create_dir_all(&env.cwd).expect("the working directory");
    let config = env.config.clone().expect("config");
    std::fs::write(&config, "[disk]\nauto_free_gib = 1000000000\n").expect("write");
    let report_only = Cmd::Disk {
        targets: vec![a.clone(), b.clone()],
        apply: None,
        json: false,
    };
    let (ok, out, err) = go(&report_only, &env);
    assert!(ok, "{err}");
    let at = |p: &Path| {
        out.find(&format!("{} — ", p.display()))
            .unwrap_or_else(|| panic!("{} is not a row:\n{out}", p.display()))
    };
    let (b_debug, a_debug, a_release) = (
        at(&b.join("debug")),
        at(&a.join("debug")),
        at(&a.join("release")),
    );
    assert!(
        b_debug < a_debug && a_debug < a_release,
        "oldest first:\n{out}"
    );
    assert!(
        out.contains("least recently used profiles give up incremental/ first"),
        "{out}"
    );
    assert!(
        out.contains("under pressure: the least-recently-used rows above go in that order"),
        "{out}"
    );
    for p in [a.join("debug"), a.join("release"), b.join("debug")] {
        assert!(p.join("incremental").exists(), "a report removes nothing");
    }

    std::fs::write(
        &config,
        "[disk]\napply = true\nauto_free_gib = 1000000000\n",
    )
    .expect("write");
    let apply = Cmd::Disk {
        targets: vec![a.clone(), b.clone()],
        apply: Some(disk::Class::CargoTargets),
        json: false,
    };
    let (ok, out, err) = go(&apply, &env);
    assert!(ok, "{err}");
    assert!(out.contains("removed=3"), "{out}");
    for p in [a.join("debug"), a.join("release"), b.join("debug")] {
        assert!(!p.join("incremental").exists(), "{}:\n{out}", p.display());
        assert!(p.join("deps/liblay.rlib").exists());
    }
    let ledger = std::fs::read_to_string(env.disk_ledger()).expect("ledger");
    let intents: Vec<&str> = ledger
        .lines()
        .filter(|l| l.contains("\"kind\":\"removing\""))
        .collect();
    assert_eq!(intents.len(), 3, "{ledger}");
    assert!(
        intents[0].contains(&b.join("debug").display().to_string()),
        "{}",
        intents[0]
    );
}

/// A PASS CUT SHORT STILL LEAVES ITS RECORD: the intent row is journalled
/// BEFORE a removal starts, so a remover that dies mid-pass (a panic here; a
/// quit or an update in the window) leaves the report and the intent on
/// record, naming the build directory it was working in.
#[test]
fn a_tick_cut_short_mid_removal_leaves_the_intent_in_the_journal() {
    let tmp = Tmp::new("disk-cut");
    let state = tmp.path().join("state");
    let target = disk::tests::lay_target(&tmp.path().join("repo/target"));
    let targets = [target.clone()];
    let look = DiskLook {
        state: &state,
        volume: tmp.path(),
        free: Some(1),
        targets: &targets,
        transcripts: None,
        now: idle_now(),
    };
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        disk_tick(
            &look,
            disk::Config::default(),
            &mut |_| panic!("the window quit mid-removal"),
            &mut disk::tests::no_pressure,
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

/// A CHECKOUT'S `target -> target.noindex` LINK STILL RESOLVES AFTER A TICK:
/// the window names `target.noindex` (a real directory; the link is not a
/// candidate), and the reclaim never removes the directory the link needs —
/// cargo can build through the link again, and the tag that keeps the twin
/// out of backups is still there.
#[cfg(unix)]
#[test]
fn a_target_noindex_twin_survives_the_tick_and_its_link_resolves() {
    let tmp = Tmp::new("disk-twin");
    let state = tmp.path().join("state");
    let repo = tmp.path().join("repo");
    let twin = disk::tests::lay_target(&repo.join("target.noindex"));
    std::os::unix::fs::symlink("target.noindex", repo.join("target")).expect("the link");
    let targets = [twin.clone()];
    let now = idle_now();
    let look = DiskLook {
        state: &state,
        volume: tmp.path(),
        free: Some(1),
        targets: &targets,
        transcripts: None,
        now,
    };
    let DiskTick::Reclaimed(done) = disk_tick(
        &look,
        disk::Config::default(),
        &mut real_remover(now),
        &mut disk::tests::no_pressure,
    ) else {
        panic!("below the floor, the tick runs");
    };
    assert_eq!(done.removed.len(), 1, "{done:?}");
    assert!(
        std::fs::metadata(repo.join("target")).is_ok_and(|m| m.is_dir()),
        "the link resolves to a real directory"
    );
    assert!(repo.join("target/CACHEDIR.TAG").exists());
    assert!(repo.join("target/debug/deps/liblay.rlib").exists());
    assert!(!repo.join("target/debug/incremental").exists());
}

/// THE TICK FREES THE VOLUME IT MEASURED. The free figure is the home
/// volume's, but the targets come from every agent's working directory: a
/// build directory on another volume (an external disk, a second APFS
/// volume) is not looked at when home runs short, since reclaiming it frees
/// nothing there. The fixture measures `/dev` (devfs / devtmpfs: a volume of
/// its own on macOS and Linux) against an idle build directory in the
/// scratch dir; the control is the same look measuring the scratch dir's own
/// volume.
#[cfg(unix)]
#[test]
fn the_disk_tick_leaves_a_target_on_another_volume_alone() {
    let tmp = Tmp::new("disk-volume");
    let state = tmp.path().join("state");
    let target = disk::tests::lay_target(&tmp.path().join("repo/target"));
    let now = idle_now();
    let elsewhere = Path::new("/dev");
    assert_ne!(
        volume_id(elsewhere),
        volume_id(tmp.path()),
        "the fixture needs two volumes"
    );
    let targets = [target.clone()];
    let look = |volume| DiskLook {
        state: &state,
        volume,
        free: Some(1),
        targets: &targets,
        transcripts: None,
        now,
    };
    let DiskTick::Reclaimed(done) = disk_tick(
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
    let DiskTick::Reclaimed(done) = disk_tick(
        &look(tmp.path()),
        disk::Config::default(),
        &mut real_remover(now),
        &mut disk::tests::no_pressure,
    ) else {
        panic!("below the floor, the tick runs");
    };
    assert_eq!(done.removed.len(), 1, "{done:?}");
    assert!(!target.join("debug/incremental").exists());
    assert!(target.join("debug/deps").exists());
}

/// THE OWNER'S VERB USES THE SAME UNIT: `aterm harness disk <dir> --apply
/// cargo-targets`, with `disk.apply = true`, reclaims the idle profile's
/// `incremental/` and nothing else, prints what went and what it freed, and
/// journals the intent and the outcome.
#[test]
fn the_verb_applies_cargo_targets_with_the_same_unit() {
    let tmp = Tmp::new("disk-verb");
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
#[test]
fn the_tick_removes_nothing_whose_intent_it_cannot_journal() {
    let tmp = Tmp::new("disk-unjournalled");
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
        transcripts: None,
        now,
    };
    let DiskTick::Reclaimed(done) = disk_tick(
        &look,
        disk::Config::default(),
        &mut real_remover(now),
        &mut disk::tests::no_pressure,
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

/// THE VERB, TOO: with the disk journal unopenable (a directory sits at its
/// path), `--apply cargo-targets` removes nothing and says why.
#[test]
fn the_verb_removes_nothing_whose_intent_it_cannot_journal() {
    let tmp = Tmp::new("disk-verb-unjournalled");
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
    assert!(out.contains("could not be journalled"), "{out}");
    assert!(target.join("debug/incremental").exists(), "the cache stays");
}

/// `--json` CARRIES WHAT EACH ROW KEPT AND WHY, as the text form does: a
/// profile a build holds (skipped) and an entry that could not be deleted
/// (trouble) are in the disk-apply object, beside what went and what it
/// freed — a caller reading the JSON sees a partial reclaim as partial.
/// Unix-only: it holds a profile's cargo lock and makes an entry undeletable
/// by mode bits, and the reclaim it drives refuses on every other OS.
#[cfg(unix)]
#[test]
fn the_verb_json_carries_what_each_row_went_kept_and_why() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = Tmp::new("disk-verb-json");
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

/// THE FREED FIGURE IS SAID TO BE A COUNT, NOT A MEASUREMENT: the verb
/// prints "freed about", and its help says what the count is — the blocks
/// of each unlinked file that was its last name, an APFS clone's shared
/// blocks included, a file it could not open left out.
#[test]
fn the_verb_says_what_its_freed_figure_counts() {
    assert!(USAGE.contains("APFS clone"), "{USAGE}");
    let tmp = Tmp::new("disk-verb-about");
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
        targets: vec![target],
        apply: Some(disk::Class::CargoTargets),
        json: false,
    };
    let (ok, out, err) = go(&cmd, &env);
    assert!(ok, "{err}");
    assert!(out.contains("; freed about "), "{out}");
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
    let tmp = Tmp::new("disk-knobs");
    let env = env_at(&tmp);
    let path = env.config.clone().expect("a config path");
    std::fs::write(
        &path,
        "[disk]\napply = false\nwarn_free_gib = 5\ntarget_stale_days = 30\n",
    )
    .expect("write");
    let cfg = disk_config(&env);
    assert_eq!(cfg.warn_free_gib, 5);
    assert_eq!(cfg.target_stale_days, 30);
    assert!(!cfg.apply);
    assert_eq!(
        cfg.auto_free_gib,
        disk::DEFAULT_AUTO_FREE_GIB,
        "the default floor"
    );
    std::fs::write(&path, "[disk]\nauto_free_gib = 0\n").expect("write");
    assert_eq!(disk_config(&env).auto_free_gib, 0, "the floor switched off");
    // A floor below zero is never crossed: `-1` written to stop the removal
    // stops it (it read as the default, 10, and left the removal ON).
    std::fs::write(&path, "[disk]\nauto_free_gib = -3\n").expect("write");
    assert_eq!(
        disk_config(&env).auto_free_gib,
        0,
        "a negative floor is off, never the default"
    );
    assert!(!disk_config(&env).below_auto_floor(Some(0)));
    // A value of the wrong type is the default, not a guess.
    std::fs::write(&path, "[disk]\nauto_free_gib = \"off\"\n").expect("write");
    assert_eq!(disk_config(&env).auto_free_gib, disk::DEFAULT_AUTO_FREE_GIB);
    std::fs::write(
        &path,
        "[disk]\napply = false\nwarn_free_gib = 5\ntarget_stale_days = 30\n",
    )
    .expect("write");
    // The dotted spelling, in a file TOML reads.
    std::fs::write(&path, "disk.apply = true\n").expect("write");
    assert!(disk_config(&env).apply);
    // A numeric knob's last assignment wins (its line reader, [`toml_int`]);
    // the same file is not TOML, so it grants no removal consent.
    std::fs::write(
        &path,
        "[disk]\napply = true\ntarget_stale_days = 3 # a comment\ntarget_stale_days = 4\n",
    )
    .expect("write");
    let cfg = disk_config(&env);
    assert!(!cfg.apply);
    assert_eq!(cfg.target_stale_days, 4);

    // NEGATIVE CONTROLS, each breaking to the shipped default rather than to
    // a guess: a value of the wrong TYPE, a NEGATIVE stale window (which
    // would make every directory a candidate at once), a key with no value,
    // and the same key under ANOTHER table.
    let d = disk::Config::default();
    for text in [
        "[disk]\nwarn_free_gib = \"lots\"\ntarget_stale_days = -1\napply = yes\n",
        "[disk]\ntarget_stale_days\n",
        "[harness]\ntarget_stale_days = 9\nwarn_free_gib = 9\napply = true\n",
    ] {
        std::fs::write(&path, text).expect("write");
        let cfg = disk_config(&env);
        assert_eq!(cfg.target_stale_days, d.target_stale_days, "{text}");
        assert_eq!(cfg.warn_free_gib, d.warn_free_gib, "{text}");
        assert_eq!(cfg.apply, disk::DEFAULT_APPLY, "{text}");
    }
    // A key said twice is not TOML: the boolean reader answers only a
    // `false` out of such a file, never consent (main, 2026-09-24); the
    // integer reader keeps its last valid assignment.
    assert_eq!(
        toml_bool("[disk]\napply = true\napply = maybe\n", "disk", "apply"),
        None
    );
    assert_eq!(toml_int("[disk]\nn = 2\nn = x\n", "disk", "n"), Some(2));
}

// ---------------------------------------------------------------------------
// The boolean reader and the hand-run upgrade (ported from main, 2026-09-24)
// ---------------------------------------------------------------------------

/// EVERY spelling TOML — and so the GUI's own parser, which seeds Settings ▸
/// Harness — reads as `harness.enabled = false` reads OFF here too, and one
/// TOML reads as some OTHER key does not.
///
/// The GUI's Settings writer keeps whatever spelling the file already has, so
/// `harness = { enabled = true }` switched off in Settings is written back
/// as `harness = { enabled = false }`. The line reader this used to be
/// answered `None` — ON — for that, for a quoted key and for a spaced dotted
/// key: the switch read OFF on the glass while the harness read it ON (main's
/// audit 2026-09-24, measured against `aterm_toml::from_str::<Value>`, which
/// reads all three `false`). Ported with main's key: on this branch the
/// boolean reader answers `disk.apply`, and the `[harness]` switches are the
/// supervisor policy's own reader (`crate::supervise::config`'s tests).
#[test]
fn the_boolean_reader_reads_every_spelling_the_gui_parser_reads() {
    for text in [
        "harness = { enabled = false }\n",
        "harness = {enabled=false}\n",
        "harness = { other = 1, enabled = false }\n",
        "harness . enabled = false\n",
        "[harness]\n\"enabled\" = false\n",
        "[\"harness\"]\nenabled = false\n",
        "'harness'.'enabled' = false\n",
        // The right table, then an inline table of the same name under
        // ANOTHER header: that one is `profile.harness.enabled`.
        "[harness]\nenabled = false\n[profile]\nharness = { enabled = true }\n",
    ] {
        assert_eq!(
            toml_bool(text, "harness", "enabled"),
            Some(false),
            "{text:?}"
        );
    }
    assert_eq!(
        toml_bool("harness = { enabled = true }\n", "harness", "enabled"),
        Some(true)
    );
    // NOT the switch: a sub-table, an array of tables, a near-miss key, a
    // string, a nested inline table, and a misplaced `true` — which TOML
    // files under `[profile]`, and which is not a `false`, so it cannot turn
    // anything off. (A misplaced `false` does:
    // `a_kill_switch_written_below_another_header_still_reads_off`.)
    for text in [
        "[harness.sub]\nenabled = false\n",
        "[[harness]]\nenabled = false\n",
        "harness = { enabledx = false }\n",
        "harness = { enabled = \"false\" }\n",
        "harness = { sub = { enabled = false } }\n",
        "[profile]\nharness.enabled = true\n",
    ] {
        assert_eq!(toml_bool(text, "harness", "enabled"), None, "{text:?}");
    }
}

/// A file the TOML parser REFUSES — a typo three tables away — still reads
/// a `false` the owner wrote, and it reads nothing else. Every spelling the
/// parser path admits is salvaged, one `false` beats any `true`, and a
/// refused file never answers `true`: `false` is the safe side, so a refused
/// `disk.apply = true` is not consent to remove anything.
#[test]
fn a_file_the_parser_refuses_can_only_read_off() {
    const TYPO: &str = "font_px = \n";
    for tail in [
        "[harness]\nenabled = false\n",
        "harness = { enabled = false }\n",
        "harness . \"enabled\" = false # off\n",
        "[harness]\nenabled = true\nenabled = false\n",
        "[harness]\nenabled = false\nenabled = true\n",
        // Misplaced below another header, as in the parsed reading.
        "[profile]\nharness = { enabled = false }\n",
        "[theme]\nname = \"x\"\nharness.enabled = false\n",
        "[[keybind]]\nharness.enabled = false\n",
    ] {
        let text = format!("{TYPO}{tail}");
        assert!(
            aterm_toml::from_str::<aterm_toml::Value>(&text).is_err(),
            "the fixture must be one the parser refuses: {text:?}"
        );
        assert_eq!(
            toml_bool(&text, "harness", "enabled"),
            Some(false),
            "{text:?}"
        );
    }
    for tail in [
        "[harness]\nenabled = true\n",
        "harness = { enabled = true }\n",
        "[profile]\nharness = { enabled = true }\n",
        "[[harness]]\nenabled = false\n",
        "[harness]\nnote = \"enabled = false\"\n",
        "\"harness.enabled\" = false\n",
        "# harness.enabled = false\n",
    ] {
        let text = format!("{TYPO}{tail}");
        assert_eq!(toml_bool(&text, "harness", "enabled"), None, "{text:?}");
    }
    // A byte-order mark in front of a refused file does not hide its header.
    let text = format!("\u{feff}[harness]\nenabled = false\n{TYPO}");
    assert_eq!(toml_bool(&text, "harness", "enabled"), Some(false));
    let text = format!("{TYPO}[disk]\napply = true\n");
    assert_eq!(toml_bool(&text, "disk", "apply"), None, "{text:?}");
}

/// `harness.enabled = false` APPENDED to a file whose last header is some
/// other table still switches the harness off, and the verb that acts under
/// the switch says where it found it.
///
/// That line is what an owner appends, and TOML files it under the last
/// header (`theme.harness.enabled`), so a root-key-only reader reads the
/// switch unset: ON. A kill switch may not stop working because of where in
/// the file it was written (main's review of 2026-09-24): the policy's reader
/// limits on it and names it (`crate::supervise::config`'s tests), and a
/// refused `upgrade` prints that note on stderr — and says nothing of it when
/// the switch sits where TOML reads it. The boolean reader keeps the same
/// rule for `disk.apply`: a misplaced `false` is no consent, a misplaced
/// `true` is not consent either.
#[test]
fn a_kill_switch_written_below_another_header_still_reads_off() {
    for text in [
        "font_px = 14\n[theme]\nname = \"x\"\nharness.enabled = false\n",
        "[harness]\nenabled = true\n[theme]\nharness.enabled = false\n",
        "[theme.dark]\nharness = { enabled = false }\n",
        "[[keybind]]\nkey = \"a\"\nharness.enabled = false\n",
    ] {
        assert_eq!(
            toml_bool(text, "harness", "enabled"),
            Some(false),
            "{text:?}"
        );
    }
    assert_eq!(
        toml_bool("[other]\ndisk.apply = false\n", "disk", "apply"),
        Some(false)
    );
    assert_eq!(
        toml_bool("[other]\ndisk.apply = true\n", "disk", "apply"),
        None
    );

    let tmp = Tmp::new("master-misplaced");
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
    assert!(
        err.contains("TOML files as `theme.harness.enabled`"),
        "{err}"
    );
    assert!(err.contains("limits all the same"), "{err}");
    // NEGATIVE CONTROL: the switch where TOML reads it is off, and unremarked.
    std::fs::write(&config, "[harness]\nenabled = false\n").expect("write config");
    assert!(!SupervisorConfig::from_path(Some(&config)).0.enabled);
    let (_, out, err) = go(&upgrade_cmd(true, false), &env);
    assert!(out.contains(UPGRADE_BYPASSED), "{out}");
    assert!(!err.contains("TOML files as"), "{err}");
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
/// window's host does: one `refused:bypassed` line, no lock taken, exit 1. Until 2026-09-24 it never read the switch — measured
/// then: exit 0, `sweep.lock` created, and against a fake socket it typed a
/// relaunch line into a tab.
#[test]
fn a_hand_run_upgrade_stands_down_while_the_master_switch_is_off() {
    let tmp = Tmp::new("upgrade-master");
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
    assert!(err.contains("`[harness] enabled` reads off"), "{err}");
    assert!(!lock.exists(), "the refused sweep took the lock");
    // The JSON form carries the same verdict.
    let (_, out, _) = go_code(&upgrade_cmd(false, true), &env);
    assert!(out.contains("\"step\":\"refused:bypassed\""), "{out}");

    // A dry run acts on nothing: it says a real run would not act, and answers.
    let (ok, out, err) = go(&upgrade_cmd(true, false), &env);
    assert!(ok, "{err}");
    assert!(out.contains("step=refused:bypassed"), "{out}");
    assert!(err.contains("with the switch on"), "{err}");
    assert!(!lock.exists(), "a dry run takes no lock");

    // POSITIVE CONTROL: the same scratch home with the switch on sweeps.
    std::fs::write(&config, "[harness]\nenabled = true\n").expect("write config");
    let (ok, out, err) = go(&upgrade_cmd(false, false), &env);
    assert!(ok, "{out}{err}");
    assert!(out.is_empty(), "no session, nothing to report: {out}");
    assert!(lock.exists(), "a sweep that ran takes the lock");
}

/// A refused `upgrade` NAMES the restarts it strands. An earlier sweep that
/// already sent SIGTERM left `<state>/upgrade/<session>.json` between the
/// exit and the relaunch; with the switch off nothing carries it on — this
/// sweep or the window's host — so the conversation stays stopped, and until
/// review on 2026-09-24 the refusal did not say so. Only the restarts a sweep
/// told this tab would resume are named, and a finished one never is.
#[test]
fn a_refused_upgrade_names_the_restarts_it_leaves_in_flight() {
    const EXITING: &str = "03396a15-856e-4f1b-8174-ae9a3e4b369f";
    const RELAUNCHED: &str = "1b32f22d-0000-4000-8000-000000000002";
    const DONE: &str = "b8d973ad-0000-4000-8000-000000000003";
    let tmp = Tmp::new("upgrade-stranded");
    let env = env_at(&tmp);
    std::fs::create_dir_all(tmp.path().join("home/.claude/sessions")).expect("home");
    let config = env.config.clone().expect("config path");
    std::fs::write(&config, "[harness]\nenabled = false\n").expect("write config");
    let dir = env.state.join("upgrade");
    std::fs::create_dir_all(&dir).expect("state");
    for (session, phase, tab) in [
        (EXITING, "exiting", "s-aaaa"),
        (RELAUNCHED, "relaunched", "s-bbbb"),
        (DONE, "done", "s-aaaa"),
    ] {
        std::fs::write(
            dir.join(format!("{session}.json")),
            format!("{{\"phase\":\"{phase}\",\"at\":1,\"tab\":\"{tab}\",\"pid\":1}}"),
        )
        .expect("write state");
    }

    let (_, _, err) = go_code(&upgrade_cmd(false, false), &env);
    assert!(err.contains("upgrade refused"), "{err}");
    assert!(err.contains("the 2 restarts"), "{err}");
    assert!(err.contains(EXITING) && err.contains(RELAUNCHED), "{err}");
    assert!(
        !err.contains(DONE),
        "a finished restart is not stranded: {err}"
    );
    // Told one tab, it names that tab's restart alone.
    let one = Cmd::Upgrade {
        sid: "s-aaaa".to_string(),
        dry_run: false,
        json: false,
    };
    let (_, _, err) = go_code(&one, &env);
    assert!(
        err.contains(&format!("the restart of session {EXITING}")),
        "{err}"
    );
    assert!(!err.contains(RELAUNCHED), "{err}");
    // NEGATIVE CONTROL: nothing in flight, nothing named.
    std::fs::remove_file(dir.join(format!("{EXITING}.json"))).expect("rm");
    std::fs::remove_file(dir.join(format!("{RELAUNCHED}.json"))).expect("rm");
    let (_, _, err) = go_code(&upgrade_cmd(false, false), &env);
    assert!(err.contains("upgrade refused"), "{err}");
    assert!(!err.contains("restart"), "{err}");
}

/// A one-shot `upgrade` whose sweep never RAN does not exit 0: it decided
/// nothing about any session. The lock held by another actor — the
/// window's own host, at a session's step — exits 75, atpkg's "try again
/// later"; a state directory that cannot hold the lock exits 1. Measured
/// before the fix: `step=busy:…` and exit 0 for all three.
#[test]
fn a_hand_run_upgrade_that_could_not_sweep_does_not_exit_zero() {
    let tmp = Tmp::new("upgrade-busy");
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
    held.try_lock().expect("the test holds the sweep lock");
    let (code, out, _) = go_code(&upgrade_cmd(false, false), &env);
    assert!(out.contains("step=busy:another-sweep"), "{out}");
    assert_eq!(
        code,
        format!("{:?}", ExitCode::from(atpkg::lock::CONTENDED_EXIT)),
        "{out}"
    );
    assert_eq!(atpkg::lock::CONTENDED_EXIT, 75, "USAGE names the number");
    // POSITIVE CONTROL: the lock released, the same sweep runs and exits 0.
    // Released by `LOCK_UN`, as the sweep's own guard releases it — the sweep
    // takes ONE try (`upgrade_drive::sweep_lock`), and a lock released by the
    // close alone stays held while any test thread's child has yet to exec.
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

/// `ledger` reads the ENGINE's approval ledger — the file the supervisor's
/// approval loop writes — for one session, newest last. NEGATIVE CONTROLS:
/// another session's rows never appear, and a session with no ledger says so
/// rather than printing nothing.
#[test]
fn the_ledger_verb_reads_the_supervisors_approval_ledger() {
    let tmp = Tmp::new("ledger");
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
    assert!(
        out.starts_with("no rows in "),
        "this session has none: {out}"
    );
    assert!(out.contains("s-test.jsonl"), "{out}");
}

/// The tail is bounded however long the file is, and a missing file is no
/// rows rather than an error.
#[test]
fn a_ledger_tail_keeps_only_the_newest_rows() {
    let tmp = Tmp::new("tail");
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
// the live upgrade: the owner's view and word
// ---------------------------------------------------------------------------

/// The owner's grammar (gap audit 2026-09-24): `ledger upgrade`, `upgrade
/// [<sid>] --status`, `upgrade <sid> --now|--defer <dur>|--skip`. Before that
/// day `ledger upgrade` answered `ledger takes disk, an @<sid> and a row
/// count, not "upgrade"` and there was no word to say at all.
#[test]
fn the_owners_upgrade_grammar_parses_and_refuses_what_it_cannot_mean() {
    let cases = [
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
    ];
    for (line, want) in cases {
        let (got, _) = parse(&words(line)).unwrap_or_else(|e| panic!("{line}: {e}"));
        assert_eq!(got, want, "{line}");
    }
    for line in [
        "upgrade --now",
        "upgrade --defer 1h",
        "upgrade s-abc --now --skip",
        "upgrade s-abc --status --now",
        "upgrade s-abc --now --dry-run",
        "upgrade s-abc --skip --every 30",
        "upgrade s-abc --defer",
        "upgrade s-abc --defer 0",
        "upgrade s-abc --defer 31d",
        "upgrade s-abc --defer soon",
        "upgrade s-abc s-def --now",
        "usage --now",
        "ledger --status",
    ] {
        assert!(parse(&words(line)).is_err(), "{line} should be refused");
    }
    // NEGATIVE CONTROL: the sweep's own grammar is untouched.
    assert!(matches!(
        parse(&words("upgrade s-abc --dry-run"))
            .expect("the sweep")
            .0,
        Cmd::Upgrade { dry_run: true, .. }
    ));
}

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
) -> std::process::Child {
    let mut child = std::process::Command::new(std::env::current_exe().expect("exe"))
        .args(["harness::upgrade_drive::tests::park_when_asked", "--exact"])
        .env("UPGRADE_DRIVE_TEST_PARK", "1")
        .env("ATERM_PARENT_SESSION_ID", tab)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the stand-in agent");
    let pid = child.id();
    let started = (0..500).any(|_| {
        let read = atpkg::caller_shell::process_args(pid)
            .is_some_and(|a| a.env_var("UPGRADE_DRIVE_TEST_PARK").is_some());
        if !read {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        read
    });
    // The kernel's start time as Claude renders `procStart`: `ps` in the C
    // locale and UTC, whitespace squashed.
    let start = std::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
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

/// A GIVE-UP ROW IN THE LEDGER VIEW NAMES WHAT HELD IT (the review of
/// 2026-09-27): `ledger upgrade` cuts each row's detail at 200 bytes, and the
/// give-up's own words ran past that before the list of what still ran under
/// the agent — the one thing that row is for — so the view showed none of
/// it: not the widowed `tail -f`, and not the `grep -m1` that made it a wait
/// that can never end. NEGATIVE CONTROL: any other row is still cut.
#[test]
fn a_give_up_row_in_the_ledger_view_names_what_held_it() {
    let held = [super::super::upgrade::Held {
        pid: 41234,
        name: "zsh".to_string(),
        age_s: 475_200,
        command: "D=/Users/…/wf_examplerun-1; tail -f -n +1 \"$D/journal.jsonl\" | /usr/bin/grep \
                  -m1 -F 'agentId' > /dev/null; echo \"closer finished\""
            .to_string(),
    }];
    let detail =
        super::super::upgrade_drive::gave_up_words(super::super::upgrade::Agent::Claude, &held);
    let row = |step: &str| {
        let mut o = Map::new();
        o.insert("t".into(), Value::from(1_790_000_000_u64));
        for (k, v) in [
            ("tab", "s-b5cf2faabac5ce5127bd"),
            ("session", "03396a15"),
            ("from", "2.1.281"),
            ("to", "2.1.283(managed)"),
            ("step", step),
            ("detail", detail.as_str()),
        ] {
            o.insert(k.into(), Value::from(v));
        }
        upgrade_ledger_line(&aterm_json::to_string(&Value::Object(o)).expect("a row"))
    };
    let line = row("gave-up");
    assert!(
        line.contains("pid 41234 (zsh, 5d12h): D=/Users/…/wf_examplerun-1;")
            && line.contains("grep -m1")
            && line.ends_with("echo \"closer finished\""),
        "{line}"
    );
    // The round's rest before the next asks is said whole too.
    assert!(line.contains("a new round asks again in 2h"), "{line}");
    // NEGATIVE CONTROL: another row's detail is cut as it was.
    let other = row("drain-expired:box");
    assert!(!other.contains("grep -m1"), "{other}");
}

/// THE WORST GIVE-UP ROW FITS ITS CUT WHOLE ([`GAVE_UP_DETAIL_BYTES`], whose
/// const assertion counts on the give-up's own words staying under 512
/// bytes): more processes than are named, each with a longest name and a
/// longest command of four-byte characters, and the count of the rest still
/// ends the row.
#[test]
fn the_worst_give_up_row_fits_its_cut_whole() {
    use super::super::upgrade::{HELD_COMMAND_CHARS, HELD_NAMED, Held};
    let lead =
        super::super::upgrade_drive::gave_up_words(super::super::upgrade::Agent::Claude, &[]);
    assert!(lead.len() < 512, "{} bytes: {lead}", lead.len());
    let held: Vec<Held> = (0..HELD_NAMED + 2)
        .map(|i| Held {
            pid: u32::MAX - u32::try_from(i).expect("small"),
            name: "𝔷".repeat(40),
            age_s: u64::from(u32::MAX),
            command: "𝔷".repeat(HELD_COMMAND_CHARS * 2),
        })
        .collect();
    let detail =
        super::super::upgrade_drive::gave_up_words(super::super::upgrade::Agent::Claude, &held);
    assert!(
        detail.len() <= GAVE_UP_DETAIL_BYTES,
        "{} bytes",
        detail.len()
    );
    assert_eq!(
        super::super::one_line(&detail, GAVE_UP_DETAIL_BYTES),
        detail
    );
    assert!(detail.ends_with("; and 2 more"), "{detail}");
}

/// `upgrade --status` PRINTS WHAT THE SWEEP RECORDED — how long behind, what
/// it waits on, the owner's word, whether it is stalled — sweeping nothing:
/// no lock is taken — for each upgrade a live Claude Code still holds behind
/// its target in its tab. The owner's word then lands under the lock and
/// shows in the next `--status`, with its ledger line in `ledger upgrade`.
/// NEGATIVE CONTROLS: the same recorded upgrade with NO live holder (review
/// of 2026-09-25: a state file outlives its conversation) is not listed; a
/// tab with nothing recorded is refused (exit 1), writing nothing.
#[cfg(unix)]
#[test]
fn the_owner_reads_the_recorded_upgrades_and_says_their_word() {
    let tmp = Tmp::new("upgrade-owner");
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
    assert_eq!(
        out, "no upgrade is recorded\n",
        "no live holder: nothing to show"
    );
    assert!(err.is_empty(), "no sessions at all is a verdict: {err}");
    let home = env.home.clone().expect("home");
    let mut agent = live_holder(&home, "03396a15", tab, "2.1.281");
    let (ok, out, err) = go(&status(""), &env);
    assert!(ok, "{out}{err}");
    assert_eq!(
        out,
        format!(
            "upgrade tab={tab} session=03396a15 from=2.1.281 to=2.1.282(managed) phase=pending \
             pending_for=8h22m wait=not-idle:busy wait_for=8h20m request=- next_round=- \
             stalled=overdue held_by=- release=- rung=land looked=- by=- point=- guard=- watch_at=due\n"
        )
    );
    assert!(
        !env.state.join("upgrade/sweep.lock").exists(),
        "a status read takes no lock"
    );
    let (ok, out, _) = go(&status("s-0000"), &env);
    assert!(ok);
    assert!(
        out.starts_with("no upgrade is recorded for tab s-0000"),
        "{out}"
    );

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
            " requested:now tab={tab} session=03396a15 2.1.281 -> 2.1.282(managed) — the owner's word"
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
    assert!(
        err.contains("no upgrade is recorded for tab s-0000"),
        "{err}"
    );
    assert!(out.is_empty(), "{out}");
    let _ = agent.kill();
    let _ = agent.wait();
}

/// THE OWNER'S WORD SAYS WHICH SWITCH IT WAITS ON (review of 2026-09-25): the
/// window takes upgrade steps only while BOTH `[harness] enabled` and
/// `[harness] upgrade` read on (the one `[harness]` reader), and the word only
/// noted the first — under `upgrade = false` it printed `request=now`, exited
/// 0 and said nothing, while nothing in the window would ever read it. Each
/// switch off is named, the word is written either way. NEGATIVE CONTROL: both
/// on, nothing is said.
#[test]
fn an_owners_word_names_the_switch_that_keeps_the_window_off() {
    let tmp = Tmp::new("upgrade-owner-switch");
    let env = env_at(&tmp);
    let tab = "s-b5cf2faabac5ce5127bd";
    let config = env.config.clone().expect("config path");
    let now = Cmd::UpgradeOwner {
        sid: tab.to_string(),
        ask: OwnerAsk::Now,
        json: false,
    };
    let hand_run = format!("the next `aterm harness upgrade {tab}`");
    for (toml, says) in [
        (
            "[harness]\nupgrade = false\n",
            vec!["`[harness] upgrade` reads off", hand_run.as_str()],
        ),
        (
            "[harness]\nenabled = false\n",
            vec!["`[harness] enabled` reads off", "nothing moves"],
        ),
        ("[harness]\nenabled = true\nupgrade = true\n", Vec::new()),
    ] {
        record_upgrade(&env, "03396a15", tab, "");
        std::fs::write(&config, toml).expect("write config");
        let (ok, out, err) = go(&now, &env);
        assert!(ok, "{toml}: {out}{err}");
        assert!(
            out.contains(" request=now "),
            "{toml}: the word lands: {out}"
        );
        if says.is_empty() {
            assert!(err.is_empty(), "{toml}: {err}");
        }
        for words in says {
            assert!(err.contains(words), "{toml}: {err}");
        }
    }
}

/// A word the busy lock keeps out exits 75 — the sweep's own "try again"
/// code — and writes nothing.
#[test]
fn an_owners_word_kept_out_by_a_sweep_exits_75_and_writes_nothing() {
    let tmp = Tmp::new("upgrade-owner-busy");
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
    assert!(err.contains("nothing was written"), "{err}");
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

    // A FINISHED upgrade is history, not "behind": a session that caught up
    // reads `pending_for=-` (and `null`), never the age of an upgrade that
    // landed long ago. Seen live 2026-09-27: `step=current pending_for=1d19h`.
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

/// `disk` reads the aterm.toml the window writes: `default_config_path` IS
/// `aterm_types::dirs::aterm_config_path` over the live environment. It was a
/// hand copy of the Unix arms, so on Windows it named `$HOME\.config\…`, or
/// nothing, while the window wrote `%APPDATA%\aterm\aterm.toml` (review,
/// 2026-09-27).
#[test]
fn default_config_path_is_the_window_config_path() {
    assert_eq!(
        default_config_path(),
        aterm_types::dirs::aterm_config_path()
    );
}
