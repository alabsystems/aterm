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
        now: 1_758_412_800, // 2025-09-21T00:00:00Z — fixed, never `now()`.
        sid: "s-test".to_string(),
        utc_offset_s: 0,
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
                every: None,
                json: false,
            },
        ),
        (
            "upgrade s-abc --dry-run --every 30 --json",
            Cmd::Upgrade {
                sid: "s-abc".to_string(),
                dry_run: true,
                every: Some(30),
                json: true,
            },
        ),
        ("--help", Cmd::Help),
    ];
    for (line, want) in cases {
        let (got, _) = parse(&words(line)).unwrap_or_else(|e| panic!("{line}: {e}"));
        assert_eq!(got, want, "{line}");
    }
    assert_eq!(parse(&[]).expect("empty").0, Cmd::Help);
    let (_, over) = parse(&words(
        "usage --state /s --config /c.toml --sock /a.sock --utc-offset 3600",
    ))
    .expect("the environment flags parse");
    assert_eq!(over.state, Some(PathBuf::from("/s")));
    assert_eq!(over.config, Some(PathBuf::from("/c.toml")));
    assert_eq!(over.sock.as_deref(), Some("/a.sock"));
    assert_eq!(over.utc_offset_s, Some(3600));
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
        "ledger statusline",
        "disk --apply everything",
        "usage --bogus",
        "usage --state",
        "upgrade abc",
        "upgrade --every 5",
        "upgrade --every soon",
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
        assert!(e.contains("deleted on 2026-09-23"), "{verb}: {e}");
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

/// The screen's banner is the evidence, and the reader is aterm-phase's —
/// nothing is re-parsed here. NEGATIVE CONTROL: the same frame without the
/// notice classifies as nothing.
#[test]
fn the_screen_evidence_is_the_banner_and_the_painted_windows() {
    let now = 1_758_412_800;
    let ev = evidence_from_screen(&limited_screen(), now, 0);
    assert_eq!(ev.len(), 1, "{ev:?}");
    let c = limits::classify(&ev, now).expect("a limit is read");
    assert_eq!(c.class, limits::Class::Session5hLimit);
    assert_eq!(c.resets_at, Some(now + 3 * 3600));
    assert!(evidence_from_screen(&idle_screen(), now, 0).is_empty());
}

/// `limits` makes ONE screen read of the named session and prints the
/// classifier's verdict at `source=grid`. NEGATIVE CONTROLS: an idle screen
/// is `class=none`, and a read that fails is exit 1 naming the session —
/// never `class=none`, which would read as "measured, nothing wrong".
#[test]
fn limits_reads_the_sessions_screen_once_and_classifies_it() {
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
    assert!(out.starts_with("class=session-5h-limit"), "{out}");
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
    assert!(json.contains("\"class\":\"session-5h-limit\""), "{json}");
    assert!(json.contains("\"source\":\"grid\""), "{json}");

    let mut ctl = scripted(text_json(&idle_screen()), 0, "");
    let (code, out, _) = run_with(&mut ctl, Some("@s-worker"), false);
    assert_eq!(code, ok);
    assert!(out.starts_with("class=none source=grid"), "{out}");

    let mut ctl = scripted(String::new(), 1, "ERR no such session @s-gone");
    let (code, out, err) = run_with(&mut ctl, Some("@s-gone"), false);
    assert_eq!(code, format!("{:?}", ExitCode::from(1)));
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("@s-gone"), "{err}");
}

/// **NO SESSION, NO READ.** Outside an aterm session (`$ATERM_PARENT_SESSION_ID`
/// empty) and with no `@<sid>` on the line, `limits` used to send an
/// unaddressed read — classifying whichever session the socket defaults to —
/// and print `class=… source=grid` naming nobody. It refuses, exit 2, and
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

/// A SWITCH that gates acts reads what the window's own `[harness]` parse
/// reads, and fails closed where [`toml_bool`] would answer nothing: absent is
/// the default, `true`/`false` (bare or quoted, as the window's policy reads
/// the value's text) is itself, and a value it cannot read is OFF — so is a
/// file TOML refuses, a key said twice among them, because the window starts
/// escalate-only on it — and a `false` written below another header, which a
/// kill switch may not ignore (main, 2026-09-24). Negative controls: the
/// default is honoured both ways, another table's key is not this one, and a
/// misplaced `true` is nothing.
#[test]
fn a_switch_reads_off_on_a_value_it_cannot_read() {
    for (text, default_on, want) in [
        ("", true, true),
        ("", false, false),
        ("[harness]\nupgrade = false\n", true, false),
        ("[harness]\nupgrade = true\n", false, true),
        ("harness.upgrade = false\n", true, false),
        // The spellings the line reader missed, which the window reads.
        ("harness = { upgrade = false }\n", true, false),
        ("harness = { upgrade = true }\n", false, true),
        ("harness . \"upgrade\" = false\n", true, false),
        ("[harness]\nupgrade = \"no\"\n", true, false),
        // Quoted, as the window's own [harness] parser reads it.
        ("[harness]\nupgrade = \"true\"\n", false, true),
        ("[harness]\nupgrade = \"false\"\n", true, false),
        ("[harness]\nupgrade = 0\n", true, false),
        ("harness = 5\n", true, false),
        // Not TOML: OFF, whatever it says. Last-assignment-wins read the
        // first of these ON.
        ("[harness]\nupgrade = false\nupgrade = true\n", true, false),
        ("[harness]\nupgrade = true\nupgrade = maybe\n", true, false),
        ("font_px = \n", true, false),
        ("[other]\nupgrade = false\n", true, true),
        ("[theme]\nharness.upgrade = false\n", true, false),
        (
            "[harness]\nupgrade = true\n[theme]\nharness.upgrade = false\n",
            true,
            false,
        ),
        ("[theme]\nharness.upgrade = true\n", false, false),
        ("[harness.sub]\nupgrade = false\n", true, true),
    ] {
        assert_eq!(
            toml_switch(text, "harness", "upgrade", default_on),
            want,
            "{text:?} (default {default_on})"
        );
    }
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
/// boolean reader answers `disk.apply`, and the switch itself is
/// [`toml_switch`] over the same parser
/// (`a_switch_reads_off_on_a_value_it_cannot_read`).
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
/// `disk.apply = true` is not consent to remove anything. (The switch reads a
/// refused file OFF outright: [`toml_switch`].)
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
/// header (`theme.harness.enabled`), so the GUI — and the first, root-key-only
/// revision of main's parser-first reader — read the switch unset: ON. A kill
/// switch may not stop working because of where in the file it was written
/// (main's review of 2026-09-24, measured then: line reader `Some(false)`,
/// root-key-only `None`). A misplaced `false` wins even over a root `true`; a
/// misplaced `true` is nothing. Settings still shows ON there, so a refused
/// `upgrade` names the line on stderr — and says nothing of it when the switch
/// sits where TOML reads it. Ported from main, where `status` and `mark` said
/// it too; both are deleted here.
#[test]
fn a_kill_switch_written_below_another_header_still_reads_off() {
    for (text, at) in [
        (
            "font_px = 14\n[theme]\nname = \"x\"\nharness.enabled = false\n",
            "theme.harness.enabled",
        ),
        (
            "[harness]\nenabled = true\n[theme]\nharness.enabled = false\n",
            "theme.harness.enabled",
        ),
        (
            "[theme.dark]\nharness = { enabled = false }\n",
            "theme.dark.harness.enabled",
        ),
        (
            "[[keybind]]\nkey = \"a\"\nharness.enabled = false\n",
            "keybind[0].harness.enabled",
        ),
    ] {
        assert_eq!(
            toml_bool(text, "harness", "enabled"),
            Some(false),
            "{text:?}"
        );
        assert_eq!(
            misplaced_false(text, "harness", "enabled").as_deref(),
            Some(at),
            "{text:?}"
        );
    }
    // Where TOML itself reads the switch, nothing is misplaced; a misplaced
    // `true` turns nothing off and so names nothing.
    for text in [
        "[harness]\nenabled = false\n[theme]\nharness.enabled = false\n",
        "[theme]\nharness.enabled = true\n",
    ] {
        assert_eq!(
            misplaced_false(text, "harness", "enabled"),
            None,
            "{text:?}"
        );
    }
    // `disk.apply` rides the same reader: a misplaced `false` is no consent
    // (its default anyway), a misplaced `true` is not consent either.
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
    assert!(!harness_switch(Some(&config), "enabled"));
    let (ok, out, err) = go(&upgrade_cmd(true, false), &env);
    assert!(ok, "{out}{err}");
    assert!(out.contains(UPGRADE_BYPASSED), "{out}");
    assert!(
        err.contains("TOML files as `theme.harness.enabled`"),
        "{err}"
    );
    assert!(
        err.contains("Settings ▸ Harness reads the switch as ON"),
        "{err}"
    );
    // NEGATIVE CONTROL: the switch where TOML reads it is off, and unremarked.
    std::fs::write(&config, "[harness]\nenabled = false\n").expect("write config");
    assert!(!harness_switch(Some(&config), "enabled"));
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
        every: None,
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
        every: None,
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

/// `--every` re-reads the switch before EVERY pass: a loop started with the
/// harness on stands down the tick it is turned off, says so ONCE, and sweeps
/// again when it is turned back on — it never exits, so a switch turned off
/// and on again does not end a hand-started loop.
#[test]
fn every_pass_rereads_the_master_switch() {
    let tmp = Tmp::new("upgrade-every");
    let env = env_at(&tmp);
    std::fs::create_dir_all(tmp.path().join("home/.claude/sessions")).expect("home");
    let config = env.config.clone().expect("config path");
    let opts = super::super::upgrade_drive::Opts {
        home: tmp.path().join("home"),
        state: env.state.clone(),
        sock: None,
        only_sid: None,
        dry_run: false,
    };
    let mut was_off = false;
    let mut pass = || {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = upgrade_pass(&env, &opts, false, &mut was_off, &mut out, &mut err);
        (
            format!("{code:?}"),
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        )
    };
    let ok = format!("{:?}", ExitCode::SUCCESS);
    let refused = format!("{:?}", ExitCode::from(1));
    assert_eq!(pass().0, ok);
    std::fs::write(&config, "[harness]\nenabled = false\n").expect("write config");
    let (code, out, err) = pass();
    assert_eq!(code, refused);
    assert!(out.contains("step=refused:bypassed"), "{out}");
    assert!(err.contains("upgrade refused"), "{err}");
    let (code, out, err) = pass();
    assert_eq!(code, refused);
    assert!(
        out.contains("step=refused:bypassed"),
        "every tick is a line: {out}"
    );
    assert!(err.is_empty(), "the sentence is said once per turn: {err}");
    std::fs::write(&config, "harness = { enabled = true }\n").expect("write config");
    let (code, out, _) = pass();
    assert_eq!(code, ok, "{out}");
    assert!(out.is_empty(), "{out}");
}

/// A one-shot `upgrade` whose sweep never RAN does not exit 0: it decided
/// nothing about any session. The lock held by another sweeper — the
/// window's own host, every minute by default — exits 75, atpkg's "try again
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
