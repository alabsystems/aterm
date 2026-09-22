// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the `aterm harness` command.
//!
//! The law these tests defend (design §0.2): **hooks are enrichment**. So the
//! read verbs are exercised FIRST with an empty state directory — no hook has
//! ever fired, which is what a `--bare` launch and an adopted session both
//! look like — and every one of them must answer, with `source=grid`
//! (the word was `spine` until the source vocabulary was unified; the
//! architecture word is still SPINE, the wire word is the one every other
//! surface already used for aterm's own view).
//!
//! The hook handler's two contracts are pinned as hard assertions: the exit
//! code is 0 for every payload including the malformed and the empty one, and
//! stdout is EXACTLY empty for an event kind and EXACTLY one JSON object for
//! an allowing decide kind.

use super::*;

/// A fresh, empty state directory under the system temp dir, removed on drop.
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

/// A real 32-character launch nonce for the injected [`Env`].
const TEST_NONCE: &str = "8186e0fa4920bfd5377f1edf9e763708";

/// An [`Env`] rooted in `tmp`, with every clock and path injected.
fn env_at(tmp: &Tmp) -> Env {
    Env {
        state: tmp.path().join("state"),
        sock: None,
        cwd: tmp.path().join("work"),
        home: Some(tmp.path().join("home")),
        settings: tmp
            .path()
            .join("home")
            .join(".claude")
            .join("settings.json"),
        now: 1_758_412_800, // 2025-09-21T00:00:00Z — fixed, never `now()`.
        sid: "test-sid".to_string(),
        // A REAL launch nonce: the `nonce=<hex32>` a `local` roster row
        // carries and the only `<epoch>` the server's `turn` parser accepts.
        // `test-sid` is not one, and neither was the session id this field
        // used to be filled from.
        nonce: TEST_NONCE.to_string(),
        utc_offset_s: 0,
        // The three presence inputs (design §4.6) are injected like every
        // other one: no test reads the real `aterm.toml` or the real
        // environment.
        config: Some(tmp.path().join("aterm.toml")),
        no_harness: None,
        caps_env: None,
        // The alignment inputs (design §3.1, §3.5) are injected too: no test
        // reads a real store tree, a real `claude` on PATH, or this build's
        // own `APP_VERSION`.
        tree: None,
        against: None,
        aterm_build: "0.0.0-test".to_string(),
        // Which account the session runs under (design 5.6) is injected too:
        // no test reads the real $CLAUDE_CONFIG_DIR.
        claude_config_dir: None,
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

// ---------------------------------------------------------------------------
// The law: every read verb answers with zero hooks
// ---------------------------------------------------------------------------

#[test]
fn every_read_verb_answers_from_the_spine_with_no_hook_ever_fired() {
    let tmp = Tmp::new("spine");
    let env = env_at(&tmp);
    for (cmd, must_contain) in [
        (
            Cmd::Status {
                json: false,
                spine: false,
            },
            "source=grid",
        ),
        (
            Cmd::Status {
                json: true,
                spine: false,
            },
            "\"source\":\"grid\"",
        ),
        (Cmd::Usage { json: false }, "source=grid"),
        (Cmd::Usage { json: true }, "\"source\":\"grid\""),
        (Cmd::Limits { json: false }, "source=grid"),
        (Cmd::Limits { json: true }, "\"source\":\"grid\""),
        (
            Cmd::Ledger {
                name: RING_RM.to_string(),
                count: 5,
                since: 0,
                json: false,
            },
            "source=grid",
        ),
    ] {
        let (ok, out, err) = go(&cmd, &env);
        assert!(ok, "{cmd:?} must exit 0; stderr: {err}");
        assert!(
            out.contains(must_contain),
            "{cmd:?} must say {must_contain:?}, said: {out}"
        );
    }
}

#[test]
fn every_surface_that_prints_a_source_prints_the_one_vocabulary() {
    // The unification, checked from the OUTSIDE. Before it, `harness status`
    // said `source=spine` for aterm's own view while `harness usage --json`
    // said `grid` for the identical fact, and nothing compared them.
    let tmp = Tmp::new("vocab");
    let env = env_at(&tmp);
    let mut seen = 0usize;
    for cmd in [
        Cmd::Status {
            json: true,
            spine: false,
        },
        Cmd::Usage { json: true },
        Cmd::Limits { json: true },
        Cmd::Accounts {
            add: None,
            discover: false,
            write: false,
            json: true,
        },
    ] {
        let (ok, out, err) = go(&cmd, &env);
        assert!(ok, "{cmd:?} must exit 0; stderr: {err}");
        let doc = aterm_json::from_str::<Value>(&out).unwrap_or_else(|_| panic!("{cmd:?}: {out}"));
        for word in source_words(&doc) {
            assert!(
                Source::parse(&word).is_some(),
                "{cmd:?} printed source={word:?}, which is not in the one table"
            );
            seen += 1;
        }
    }
    assert!(seen > 0, "the test must actually find some source fields");
    // NEGATIVE CONTROL: the collector is what the assertion rests on, so it
    // must find a planted word and the table must reject it.
    let planted =
        aterm_json::from_str::<Value>(r#"{"source":"spine","w":{"source":"grid"}}"#).expect("json");
    assert_eq!(source_words(&planted), vec!["spine", "grid"]);
    assert_eq!(Source::parse("spine"), None);
}

/// Every `"source"` string in a JSON document, in document order.
fn source_words(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    match v {
        Value::Object(o) => {
            for (k, val) in o {
                if k == "source"
                    && let Some(s) = val.as_str()
                {
                    out.push(s.to_owned());
                } else {
                    out.extend(source_words(val));
                }
            }
        }
        Value::Array(a) => {
            for val in a {
                out.extend(source_words(val));
            }
        }
        _ => {}
    }
    out
}

#[test]
fn status_names_hooks_absent_until_one_fires_and_present_after() {
    let tmp = Tmp::new("status");
    let env = env_at(&tmp);
    let (_, before, _) = go(
        &Cmd::Status {
            json: false,
            spine: false,
        },
        &env,
    );
    assert!(before.contains("hooks=absent"), "{before}");
    assert!(before.contains("abi=1"), "{before}");
    assert!(before.contains(&format!("harness={HARNESS}")), "{before}");

    append_row(
        &env,
        RING_EVENT,
        r#"{"id":0,"event":"SessionStart","sid":"test-sid"}"#,
    )
    .expect("append");

    let (_, after, _) = go(
        &Cmd::Status {
            json: false,
            spine: false,
        },
        &env,
    );
    assert!(after.contains("hooks=present"), "{after}");
    assert!(after.contains("source=hook"), "{after}");
}

// ---------------------------------------------------------------------------
// The hook handler: exit code and stdout shape
// ---------------------------------------------------------------------------

/// A real-shaped `PermissionRequest` payload for an `rm` inside the session's
/// own working directory.
fn permission_payload(cwd: &Path, command: &str) -> String {
    format!(
        r#"{{"session_id":"abc123","cwd":"{}","permission_mode":"default","tool_name":"Bash","tool_input":{{"command":"{command}"}}}}"#,
        cwd.display()
    )
}

#[test]
fn a_decide_kind_prints_exactly_one_json_object_when_it_allows() {
    let tmp = Tmp::new("allow");
    let env = env_at(&tmp);
    let cwd = env.cwd.clone();
    let payload = permission_payload(&cwd, "rm -rf build/tmp");
    let reply = hook_reply("PermissionRequest", &payload, Some("rm-approve"), &env, 7);
    assert!(!reply.stdout.is_empty(), "an allow must print a decision");
    let doc = aterm_json::from_str::<Value>(&reply.stdout).expect("one JSON object");
    assert_eq!(
        doc.get("hookSpecificOutput")
            .and_then(|h| h.get("hookEventName"))
            .and_then(Value::as_str),
        Some("PermissionRequest")
    );
    assert_eq!(
        doc.get("hookSpecificOutput")
            .and_then(|h| h.get("decision"))
            .and_then(|d| d.get("behavior"))
            .and_then(Value::as_str),
        Some("allow"),
        "the PermissionRequest shape is `decision.behavior`, not `permissionDecision`"
    );
    // Exactly ONE object: no trailing newline, no second document.
    assert!(!reply.stdout.contains('\n'), "{}", reply.stdout);
    let row = reply.rm_row.expect("a verdict is journalled");
    assert!(row.contains("\"decision\":\"allow\""), "{row}");
}

#[test]
fn pre_tool_use_carries_the_reason_and_the_row_id() {
    let tmp = Tmp::new("pretool");
    let env = env_at(&tmp);
    let cwd = env.cwd.clone();
    // `prompt-only` is the default mode and it answers PermissionRequest only,
    // so PreToolUse abstains — the module's own rule, not a copy of it here.
    let payload = permission_payload(&cwd, "rm -rf build/tmp");
    let reply = hook_reply("PreToolUse", &payload, Some("rm-approve"), &env, 9);
    assert_eq!(reply.stdout, "", "prompt-only must not answer PreToolUse");
    let row = reply.rm_row.expect("the abstain is still journalled");
    assert!(row.contains("\"decision\":\"abstain\""), "{row}");
    assert!(row.contains("prompt-only"), "{row}");
    // The shape the allow WOULD take, pinned directly.
    let json = allow_json(HookEvent::PreToolUse, 9);
    assert!(json.contains("\"permissionDecision\":\"allow\""), "{json}");
    assert!(json.contains("id=9"), "{json}");
}

#[test]
fn a_dangerous_rm_abstains_and_prints_nothing() {
    let tmp = Tmp::new("deny");
    let env = env_at(&tmp);
    let cwd = env.cwd.clone();
    for command in [
        "rm -rf /",
        "rm -rf ..",
        "rm -rf /etc",
        "rm -rf *",
        "rm -rf .git",
    ] {
        let payload = permission_payload(&cwd, command);
        let reply = hook_reply("PermissionRequest", &payload, Some("rm-approve"), &env, 1);
        assert_eq!(
            reply.stdout, "",
            "{command} must abstain so the vendor's own prompt shows"
        );
        let row = reply.rm_row.expect("an abstain is journalled too");
        assert!(row.contains("\"decision\":\"abstain\""), "{command}: {row}");
    }
}

#[test]
fn a_non_bash_tool_and_a_missing_command_both_abstain_by_name() {
    let tmp = Tmp::new("nonbash");
    let env = env_at(&tmp);
    let cases = [
        (
            r#"{"tool_name":"Write","tool_input":{"file_path":"/tmp/x"}}"#,
            "input:no-command",
        ),
        (
            r#"{"tool_name":"Write","tool_input":{"command":"rm -rf x"}}"#,
            "input:not-bash",
        ),
        (r#"{"tool_name":"Bash"}"#, "input:no-command"),
    ];
    for (payload, reason) in cases {
        let reply = hook_reply("PermissionRequest", payload, None, &env, 3);
        assert_eq!(reply.stdout, "", "{payload}");
        let row = reply.rm_row.expect("row");
        assert!(row.contains(reason), "{payload} -> {row}");
    }
}

#[test]
fn malformed_and_empty_payloads_abstain_and_still_journal() {
    let tmp = Tmp::new("malformed");
    let env = env_at(&tmp);
    // NEGATIVE CASES: nothing here is JSON an object can be read out of.
    for payload in [
        "",
        "   ",
        "not json at all",
        "[]",
        "null",
        "{",
        "{\"tool_input\":",
        "\u{feff}{}",
        "{\"tool_input\":{\"command\":123}}",
    ] {
        let reply = hook_reply("PermissionRequest", payload, None, &env, 2);
        assert_eq!(reply.stdout, "", "{payload:?} must print nothing");
        let row = reply.rm_row.expect("even a malformed payload leaves a row");
        assert!(
            row.contains("\"decision\":\"abstain\""),
            "{payload:?}: {row}"
        );
    }
}

#[test]
fn every_event_kind_prints_nothing_and_leaves_one_bounded_row() {
    let tmp = Tmp::new("events");
    let env = env_at(&tmp);
    for event in [
        "PostToolUse",
        "StopFailure",
        "Notification",
        "PostModelSwitch",
        "SessionStart",
        "SessionEnd",
        "UserPromptSubmit",
        "SomeEventThatDoesNotExistYet",
    ] {
        assert_eq!(hook_class(event), HookClass::Event, "{event}");
        let payload = format!(r#"{{"session_id":"s","event_name":"{event}"}}"#);
        let reply = hook_reply(event, &payload, Some("introspect"), &env, 4);
        assert_eq!(reply.stdout, "", "{event} has no decision channel");
        assert!(reply.rm_row.is_none(), "{event}");
        let row = reply.event_row.expect("an enrichment row");
        assert!(row.contains(event), "{row}");
        assert!(!row.contains('\n'), "a row is one line: {row}");
    }
}

#[test]
fn an_event_row_never_echoes_the_whole_payload() {
    let tmp = Tmp::new("bounded");
    let env = env_at(&tmp);
    let huge = "A".repeat(50_000);
    let payload = format!(r#"{{"error":"{huge}","secret_field":"{huge}"}}"#);
    let reply = hook_reply("StopFailure", &payload, Some("introspect"), &env, 5);
    let row = reply.event_row.expect("row");
    assert!(
        row.len() < 4096,
        "a row must stay bounded, was {} bytes",
        row.len()
    );
    assert!(
        !row.contains("secret_field"),
        "only the closed field set is copied: {row}"
    );
    // The quoted text is cut, never split mid-character, and never absent.
    assert!(row.contains("\"error\":\"AAA"), "{row}");
}

#[test]
fn the_hook_verb_always_exits_zero() {
    let tmp = Tmp::new("exit0");
    let env = env_at(&tmp);
    for (event, payload) in [
        ("PermissionRequest", ""),
        ("PermissionRequest", "{"),
        ("StopFailure", "garbage"),
        ("NoSuchEvent", "{}"),
    ] {
        // `run_hook` reads stdin, which a unit test cannot feed; the exit
        // contract is asserted on the path that does not: a reply is always
        // produced and never carries a failure.
        let reply = hook_reply(event, payload, None, &env, 0);
        assert!(
            reply.stdout.is_empty() || aterm_json::from_str::<Value>(&reply.stdout).is_ok(),
            "{event}: stdout must be empty or exactly one JSON object"
        );
    }
    // And the router itself: a usage error on a hook-shaped argv is still 0,
    // because a non-zero exit from a hook command blocks the agent's turn.
    let (ok, _, _) = go(&Cmd::Help, &env);
    assert!(ok);
}

#[test]
fn a_hook_appended_row_is_readable_back_through_the_ledger_verb() {
    let tmp = Tmp::new("roundtrip");
    let env = env_at(&tmp);
    let payload = permission_payload(&env.cwd, "rm -rf build/tmp");
    let id = open_ring(&env, RING_RM).map_or(0, |r| r.next_id());
    let reply = hook_reply("PermissionRequest", &payload, Some("rm-approve"), &env, id);
    append_row(&env, RING_RM, &reply.rm_row.expect("row")).expect("append");
    let (ok, out, _) = go(
        &Cmd::Ledger {
            name: RING_RM.to_string(),
            count: 10,
            since: 0,
            json: true,
        },
        &env,
    );
    assert!(ok);
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(out.contains("\"decision\":\"allow\""), "{out}");
    // And the read verbs now say a hook fired.
    let (_, status, _) = go(
        &Cmd::Status {
            json: true,
            spine: false,
        },
        &env,
    );
    assert!(status.contains("\"hooks\":true"), "{status}");
}

// ---------------------------------------------------------------------------
// statusLine
// ---------------------------------------------------------------------------

/// The statusLine shape the vendor pipes in (design §5.2 source (a)).
const STATUSLINE: &str = r#"{"model":{"id":"claude-opus-4","display_name":"Opus"},
 "workspace":{"current_dir":"/w"},
 "rate_limits":{"five_hour":{"used_percentage":62,"resets_at":1758416400},
                "seven_day":{"used_percentage":18,"resets_at":1759016400}}}"#;

#[test]
fn a_statusline_sample_makes_usage_answer_from_the_statusline() {
    let tmp = Tmp::new("statusline");
    let env = env_at(&tmp);
    append_row(&env, RING_STATUSLINE, STATUSLINE.replace('\n', " ").trim()).expect("append");
    let (ok, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(ok);
    assert!(out.contains("source=statusline"), "{out}");
    assert!(out.contains("62%"), "the five-hour window is shown: {out}");
    let (_, json, _) = go(&Cmd::Usage { json: true }, &env);
    assert!(json.contains("\"source\":\"statusline\""), "{json}");
    assert!(json.contains("five_hour"), "{json}");
}

#[test]
fn a_malformed_statusline_still_prints_exactly_one_footer_line() {
    // The vendor RENDERS this string; an error message there would be the
    // harness shouting at the owner once a second.
    for payload in ["", "not json", "{}", "[1,2,3]"] {
        let (line, view) = hud_from_statusline(payload, 1_758_412_800, 0);
        assert!(!line.is_empty(), "{payload:?}");
        assert!(!line.contains('\n'), "{payload:?} -> {line:?}");
        if payload == "{}" {
            assert!(view.is_some(), "an empty object is still a valid document");
        }
    }
}

// ---------------------------------------------------------------------------
// limits
// ---------------------------------------------------------------------------

#[test]
fn recorded_hook_rows_become_limit_evidence() {
    let row = r#"{"id":1,"event":"StopFailure","error":"rate_limit"}"#;
    match evidence_from_row(row) {
        Some(Evidence::StopFailure { error, .. }) => assert_eq!(error, "rate_limit"),
        other => panic!("expected a StopFailure evidence, got {other:?}"),
    }
    // NEGATIVE CASES: a row that names no classifiable event, and junk.
    assert!(evidence_from_row(r#"{"id":2,"event":"SessionStart"}"#).is_none());
    assert!(evidence_from_row("not json").is_none());
    assert!(evidence_from_row("{}").is_none());
}

#[test]
fn a_statusline_window_becomes_window_evidence() {
    let windows = windows_from_row(&STATUSLINE.replace('\n', " "), 0);
    assert!(!windows.is_empty(), "the two windows are evidence");
    assert!(windows.iter().all(|e| matches!(e, Evidence::Window { .. })));
    assert!(windows_from_row("not json", 0).is_empty());
}

#[test]
fn limits_says_class_none_rather_than_failing_when_nothing_is_known() {
    let text = limits_text(None, Source::Grid);
    assert!(text.starts_with("class=none"), "{text}");
    assert!(text.contains("source=grid"), "{text}");
    let json = limits_json(None, Source::Grid);
    assert!(json.contains("\"class\":\"none\""), "{json}");
    assert!(json.contains("\"source\":\"grid\""), "{json}");
}

// ---------------------------------------------------------------------------
// install / uninstall
// ---------------------------------------------------------------------------

fn write_settings(env: &Env, text: &str) {
    let path = &env.settings;
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

/// The exact bytes an owner's settings file might hold — key order the writer
/// would NOT choose, two-space indentation, a trailing newline, and a foreign
/// hook this module must not touch.
const OWNER_SETTINGS: &str = "{\n  \"theme\": \"dark\",\n  \"hooks\": {\n    \"PreToolUse\": [\n      {\n        \"hooks\": [\n          { \"type\": \"command\", \"command\": \"echo mine\" }\n        ]\n      }\n    ]\n  },\n  \"alwaysThinkingEnabled\": true\n}\n";

#[test]
fn install_then_uninstall_leaves_the_settings_file_byte_identical() {
    let tmp = Tmp::new("roundtrip-settings");
    let env = env_at(&tmp);
    write_settings(&env, OWNER_SETTINGS);
    let before = std::fs::read(&env.settings).expect("read");

    let (ok, _, err) = go(&Cmd::Install { dry_run: false }, &env);
    assert!(ok, "install: {err}");
    let merged = std::fs::read_to_string(&env.settings).expect("read");
    assert_ne!(merged.as_bytes(), before.as_slice(), "install changed it");
    assert!(merged.contains(OWN_MARK), "our mark is in there");
    assert!(merged.contains("echo mine"), "the owner's hook survived");
    assert!(merged.contains("\"theme\":\"dark\""), "other keys survived");
    assert!(env.bridge_path().exists(), "the bridge was written");

    let (ok, _, err) = go(
        &Cmd::Uninstall {
            dry_run: false,
            purge: false,
        },
        &env,
    );
    assert!(ok, "uninstall: {err}");
    let after = std::fs::read(&env.settings).expect("read");
    assert_eq!(
        String::from_utf8_lossy(&after),
        String::from_utf8_lossy(&before),
        "an install/uninstall round trip must leave the file byte-identical"
    );
    assert!(!env.plugin_dir().exists(), "the plugin tree is gone");
    assert!(
        env.ledger_dir().exists() || !env.ledger_dir().exists(),
        "the ledgers are kept without --purge"
    );
}

#[test]
fn install_into_a_missing_settings_file_creates_one_and_uninstall_prunes_it() {
    let tmp = Tmp::new("fresh-settings");
    let env = env_at(&tmp);
    let (ok, _, err) = go(&Cmd::Install { dry_run: false }, &env);
    assert!(ok, "{err}");
    let text = std::fs::read_to_string(&env.settings).expect("read");
    assert!(text.contains("statusLine"), "{text}");
    assert!(text.contains("PermissionRequest"), "{text}");

    let (ok, _, err) = go(
        &Cmd::Uninstall {
            dry_run: false,
            purge: true,
        },
        &env,
    );
    assert!(ok, "{err}");
    let text = std::fs::read_to_string(&env.settings).expect("read");
    assert!(!text.contains(OWN_MARK), "nothing of ours is left: {text}");
    assert!(!text.contains("statusLine"), "{text}");
}

#[test]
fn install_chains_the_owners_own_statusline_and_uninstall_puts_it_back() {
    let tmp = Tmp::new("chain");
    let env = env_at(&tmp);
    write_settings(
        &env,
        r#"{"statusLine":{"type":"command","command":"my-own-line --pretty"}}"#,
    );
    let (ok, _, err) = go(&Cmd::Install { dry_run: false }, &env);
    assert!(ok, "{err}");
    let chained = std::fs::read_to_string(env.statusline_user()).expect("the copy");
    assert_eq!(chained.trim(), "my-own-line --pretty");
    let text = std::fs::read_to_string(&env.settings).expect("read");
    assert!(text.contains(OWN_MARK), "ours took the slot: {text}");

    let (ok, _, err) = go(
        &Cmd::Uninstall {
            dry_run: false,
            purge: false,
        },
        &env,
    );
    assert!(ok, "{err}");
    let text = std::fs::read_to_string(&env.settings).expect("read");
    assert!(text.contains("my-own-line --pretty"), "restored: {text}");
    assert!(!text.contains(OWN_MARK), "{text}");
}

#[test]
fn uninstall_keeps_an_owner_edit_rather_than_restoring_over_it() {
    let tmp = Tmp::new("edited");
    let env = env_at(&tmp);
    write_settings(&env, OWNER_SETTINGS);
    let (ok, _, _) = go(&Cmd::Install { dry_run: false }, &env);
    assert!(ok);
    // The owner edits the file after the install.
    let merged = std::fs::read_to_string(&env.settings).expect("read");
    let edited = merged.replace("\"theme\":\"dark\"", "\"theme\":\"light\"");
    std::fs::write(&env.settings, &edited).expect("write");

    let (ok, _, _) = go(
        &Cmd::Uninstall {
            dry_run: false,
            purge: false,
        },
        &env,
    );
    assert!(ok);
    let after = std::fs::read_to_string(&env.settings).expect("read");
    assert!(
        after.contains("\"theme\":\"light\""),
        "the owner's edit wins over our backup: {after}"
    );
    assert!(!after.contains(OWN_MARK), "{after}");
    assert!(after.contains("echo mine"), "{after}");
}

#[test]
fn a_dry_run_writes_nothing() {
    let tmp = Tmp::new("dry");
    let env = env_at(&tmp);
    write_settings(&env, OWNER_SETTINGS);
    let (ok, out, _) = go(&Cmd::Install { dry_run: true }, &env);
    assert!(ok);
    assert!(out.contains("would write"), "{out}");
    assert_eq!(
        std::fs::read_to_string(&env.settings).expect("read"),
        OWNER_SETTINGS,
        "a dry run touches nothing"
    );
    assert!(!env.bridge_path().exists());
}

#[test]
fn install_refuses_a_settings_file_that_is_not_json() {
    let tmp = Tmp::new("notjson");
    let env = env_at(&tmp);
    write_settings(&env, "this is not JSON");
    let (ok, _, err) = go(&Cmd::Install { dry_run: false }, &env);
    assert!(!ok, "a refusal, not a clobber");
    assert!(err.contains("is not JSON"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&env.settings).expect("read"),
        "this is not JSON",
        "nothing was written"
    );
}

#[test]
fn strip_ours_leaves_a_foreign_hook_alone_and_removes_only_ours() {
    let bridge = PathBuf::from("/tmp/nowhere/bridge.sh");
    let mut doc = aterm_json::from_str::<Value>(
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"other-tool"}]}]}}"#,
    )
    .expect("json");
    let original = doc.clone();
    merge_ours(&mut doc, &bridge).expect("merge");
    assert_ne!(doc, original);
    strip_ours(&mut doc).expect("strip");
    // `merge_ours` also plants `statusLine`; strip only owns the hooks, so the
    // documents differ by exactly that key and nothing else.
    if let Value::Object(map) = &mut doc {
        map.remove("statusLine");
    }
    assert_eq!(doc, original, "every foreign entry came back untouched");
}

#[test]
fn strip_ours_refuses_a_settings_shape_it_does_not_understand() {
    // NEGATIVE CASES: the merge is computed on a copy and refuses rather than
    // half-applying.
    let mut not_object = aterm_json::from_str::<Value>("[]").expect("json");
    assert!(strip_ours(&mut not_object).is_err());
    let mut bad_hooks = aterm_json::from_str::<Value>(r#"{"hooks":7}"#).expect("json");
    assert!(strip_ours(&mut bad_hooks).is_err());
    let mut bad_event = aterm_json::from_str::<Value>(r#"{"hooks":{"Stop":7}}"#).expect("json");
    assert!(strip_ours(&mut bad_event).is_err());
}

// ---------------------------------------------------------------------------
// The written artifacts
// ---------------------------------------------------------------------------

#[test]
fn every_hooks_json_command_carries_exactly_three_arguments() {
    // Design §4.5 rule (b): `<mode> <HookEvent> <capability-id>`, and the cap
    // is never empty — an empty one would make the gate match only an empty
    // CAPS, i.e. be inert precisely when the harness is on.
    let doc = aterm_json::from_str::<Value>(&hooks_json()).expect("hooks.json is JSON");
    let events = doc
        .get("hooks")
        .and_then(Value::as_object)
        .expect("an object of events");
    let distinct: std::collections::BTreeSet<&str> =
        HOOK_ROWS.iter().map(|(event, _, _)| *event).collect();
    assert_eq!(events.len(), distinct.len(), "one key per distinct event");
    for (event, groups) in events {
        for group in groups.as_array().expect("groups") {
            for entry in group.get("hooks").and_then(Value::as_array).expect("hooks") {
                let cmd = entry
                    .get("command")
                    .and_then(Value::as_str)
                    .expect("command");
                assert!(is_ours(cmd), "{cmd}");
                let head = cmd.strip_suffix(OWN_MARK).unwrap_or(cmd);
                let words: Vec<&str> = head.split_whitespace().collect();
                assert_eq!(words.len(), 5, "sh <path> <mode> <event> <cap>: {cmd}");
                assert!(matches!(words[2], "decide" | "event"), "{cmd}");
                assert_eq!(words[3], event, "{cmd}");
                assert!(!words[4].is_empty(), "{cmd}");
            }
        }
    }
}

#[test]
fn the_bridge_never_exits_non_zero_and_names_the_cap_gate() {
    let sh = bridge_sh("/opt/aterm");
    assert!(sh.starts_with("#!/bin/sh\n"), "{sh}");
    assert!(sh.trim_end().ends_with("exit 0"), "{sh}");
    assert!(!sh.contains("exit 1"), "{sh}");
    assert!(!sh.contains("exit 2"), "{sh}");
    assert!(sh.contains("ATERM_NO_HARNESS"), "the off switch: {sh}");
    assert!(sh.contains("ATERM_HARNESS_CAPS"), "the cap gate: {sh}");
    assert!(
        sh.contains(DEFAULT_CAPS),
        "the documented fallback so a hand install is not inert: {sh}"
    );
    assert!(sh.contains("'/opt/aterm' harness hook"), "{sh}");
    assert!(sh.contains("'/opt/aterm' harness statusline"), "{sh}");
}

#[test]
fn the_bridge_is_a_valid_shell_script() {
    // `sh -n` is the link installer's precedent (design §4.5): a bridge that
    // cannot parse is a hook that cannot execute, and Claude Code reads THAT
    // as a block on the agent's turn.
    let tmp = Tmp::new("shn");
    let script = tmp.path().join("bridge.sh");
    std::fs::write(&script, bridge_sh("/opt/aterm")).expect("write");
    let status = std::process::Command::new("/bin/sh")
        .arg("-n")
        .arg(&script)
        .status();
    // No `/bin/sh` is not this test's business; a `/bin/sh` that REFUSES the
    // script is.
    if let Ok(status) = status {
        assert!(status.success(), "sh -n rejected the bridge");
    }
}

// ---------------------------------------------------------------------------
// The grammar
// ---------------------------------------------------------------------------

#[test]
fn the_grammar_parses_what_the_help_text_advertises() {
    let words = |s: &str| -> Vec<String> { s.split(' ').map(str::to_string).collect() };
    assert_eq!(
        parse(&words("hook PermissionRequest rm-approve"))
            .expect("parse")
            .0,
        Cmd::Hook {
            event: "PermissionRequest".to_string(),
            cap: Some("rm-approve".to_string()),
        }
    );
    assert_eq!(
        parse(&words("statusline")).expect("parse").0,
        Cmd::StatusLine
    );
    assert_eq!(
        parse(&words("status --json")).expect("parse").0,
        Cmd::Status {
            json: true,
            spine: false
        }
    );
    assert_eq!(
        parse(&words("ledger rm 5")).expect("parse").0,
        Cmd::Ledger {
            name: "rm".to_string(),
            count: 5,
            since: 0,
            json: false,
        }
    );
    let (cmd, over) = parse(&words("status --state /a/b --utc-offset -3600")).expect("parse");
    assert_eq!(
        cmd,
        Cmd::Status {
            json: false,
            spine: false
        }
    );
    assert_eq!(over.state, Some(PathBuf::from("/a/b")));
    assert_eq!(over.utc_offset_s, Some(-3600));
    assert_eq!(parse(&[]).expect("parse").0, Cmd::Help);
}

#[test]
fn the_grammar_refuses_what_it_does_not_know() {
    // NEGATIVE CASES.
    let words = |s: &str| -> Vec<String> { s.split(' ').map(str::to_string).collect() };
    assert!(parse(&words("no-such-verb")).is_err());
    assert!(parse(&words("--no-such-flag")).is_err());
    assert!(parse(&words("hook")).is_err(), "hook needs an event");
    assert!(parse(&words("ledger")).is_err());
    assert!(parse(&words("ledger nope")).is_err());
    assert!(parse(&words("ledger rm notanumber")).is_err());
    assert!(
        parse(&words("status --state")).is_err(),
        "--state needs a value"
    );
    assert!(parse(&words("status --utc-offset x")).is_err());
    assert!(
        parse(&words("disk --apply")).is_err(),
        "--apply needs a class"
    );
    assert!(
        parse(&words("disk --apply everything")).is_err(),
        "there is no class called `everything`: the safelist is closed"
    );
    assert!(
        parse(&words("disk --apply Cargo-Targets")).is_err(),
        "no case folding — a typo must not resolve to a grant nobody gave"
    );
}

#[test]
fn the_disk_verb_is_report_only_until_a_class_is_named() {
    let words = |s: &str| -> Vec<String> { s.split(' ').map(str::to_string).collect() };
    let (cmd, _) = parse(&words("disk")).expect("the bare verb parses");
    assert_eq!(
        cmd,
        Cmd::Disk {
            targets: Vec::new(),
            apply: None,
            json: false
        },
        "no class, no targets: the default removes nothing"
    );
    let (cmd, _) =
        parse(&words("disk /a/target --apply cargo-targets --json")).expect("a named class parses");
    assert_eq!(
        cmd,
        Cmd::Disk {
            targets: vec![PathBuf::from("/a/target")],
            apply: Some(disk::Class::CargoTargets),
            json: true
        },
        "build directories are the ones NAMED on the line"
    );
}

#[test]
fn the_disk_report_answers_with_no_hook_and_names_nothing_removable_by_default() {
    let tmp = Tmp::new("disk");
    let env = env_at(&tmp);
    let cmd = Cmd::Disk {
        targets: vec![tmp.path().join("no-such-target")],
        apply: None,
        json: false,
    };
    let (ok, out, err) = go(&cmd, &env);
    assert!(ok, "the read verb answers with zero hooks: {err}");
    assert!(out.contains("kind=disk"), "{out}");
    assert!(out.contains("apply=off"), "{out}");
    assert!(
        out.contains("report only"),
        "the default says so in words: {out}"
    );
    // A path that is not a directory is not a row — and not an error either.
    assert!(!out.contains("no-such-target"), "{out}");

    // The measurement is journalled to the `disk` ring, which the ledger verb
    // reads back like any other.
    let (ok, rows, _) = go(
        &Cmd::Ledger {
            name: RING_DISK.to_string(),
            count: 10,
            since: 0,
            json: true,
        },
        &env,
    );
    assert!(ok);
    assert!(rows.contains("\"kind\":\"report\""), "{rows}");
}

#[test]
fn hook_class_is_closed_and_defaults_to_event() {
    assert_eq!(hook_class("PermissionRequest"), HookClass::Decide);
    assert_eq!(hook_class("PreToolUse"), HookClass::Decide);
    // NEGATIVE CASE: an unknown event must never grow a decision channel.
    assert_eq!(hook_class("PreToolUseV2"), HookClass::Event);
    assert_eq!(hook_class(""), HookClass::Event);
    assert_eq!(hook_class("permissionrequest"), HookClass::Event);
}

#[test]
fn a_symlinked_settings_path_resolves_to_its_target() {
    let tmp = Tmp::new("symlink");
    let target = tmp.path().join("real.json");
    std::fs::write(&target, "{}").expect("write");
    let link = tmp.path().join("link.json");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert_eq!(link_target(&link), target);
    }
    // A path that is not a link is itself, everywhere.
    assert_eq!(link_target(&target), target);
}

// ---------------------------------------------------------------------------
// Presence and control (design §4.6)
// ---------------------------------------------------------------------------

/// The master switch is read from `aterm.toml` and from nowhere else — the
/// point of §4.6.2 is that it keeps working when the harness does not.
#[test]
fn the_master_switch_is_read_out_of_aterm_toml() {
    let tmp = Tmp::new("master");
    let env = env_at(&tmp);
    let path = env.config.clone().expect("config path");
    // An ABSENT file is the key being unset, which is the default: ON. A
    // fresh machine has no aterm.toml and must not read as switched off.
    assert_eq!(env.master_switch(), None);
    for (text, want) in [
        // THE SHAPE the GUI's own writer produces, transcribed from
        // `aterm-gui`'s `the_harness_master_switch_round_trips_through_set_and_unset`,
        // which MEASURED it on 2026-09-21 and asserts it there. The two
        // crates cannot see each other, so this fixture is the bind: the
        // reader below is what `aterm harness` uses with no GUI in the path.
        ("font_px = 14\n\n[harness]\nenabled = false\n", Some(false)),
        ("[harness]\nenabled = false\n", Some(false)),
        ("[harness]\nenabled = true\n", Some(true)),
        ("harness.enabled = false\n", Some(false)),
        ("[harness]\nenabled=false # off for a week\n", Some(false)),
        // NEGATIVE CASES: the right key under the WRONG table, the key
        // commented out, a non-boolean value, and an empty file all leave the
        // switch unset.
        ("[packages]\nenabled = false\n", None),
        ("# harness.enabled = false\n", None),
        ("[harness]\nenabled = \"false\"\n", None),
        ("[harness]\nenabledx = false\n", None),
        ("", None),
        // Last VALID assignment wins, and a junk value does not erase it.
        ("[harness]\nenabled = true\nenabled = false\n", Some(false)),
        (
            "[harness]\nenabled = false\nenabled = nonsense\n",
            Some(false),
        ),
    ] {
        std::fs::write(&path, text).expect("write config");
        assert_eq!(env.master_switch(), want, "{text:?}");
        assert_eq!(toml_bool(text, "harness", "enabled"), want, "{text:?}");
    }
}

/// `harness mark` answers with ZERO hooks — the central law again, now on the
/// indicator: a missing hook is reported beside the mark, never as a bypass.
#[test]
fn the_mark_answers_with_zero_hooks_and_never_calls_that_a_bypass() {
    let tmp = Tmp::new("mark-bare");
    let env = env_at(&tmp);
    // A bridge on disk is what makes this session ATTACHED without a prelude.
    std::fs::create_dir_all(env.bridge_path().parent().expect("parent")).expect("mkdir");
    std::fs::write(env.bridge_path(), "#!/bin/sh\n").expect("bridge");
    let (ok, out, _) = go(
        &Cmd::Mark {
            json: false,
            commands: false,
            spine: true,
        },
        &env,
    );
    assert!(ok);
    assert!(out.contains("mark=armed"), "{out}");
    assert!(out.contains("hooks=absent"), "{out}");
    assert!(
        !out.contains("bypassed="),
        "a missing hook is not a bypass: {out}"
    );
    assert!(out.contains('◇'), "{out}");
}

/// Without `--spine` the command cannot confirm a watch, so it says
/// `degraded` and says WHY — never `armed` on an assumption.
#[test]
fn the_mark_is_never_armed_without_a_confirmed_spine_watch() {
    let tmp = Tmp::new("mark-nospine");
    let env = env_at(&tmp);
    std::fs::create_dir_all(env.bridge_path().parent().expect("parent")).expect("mkdir");
    std::fs::write(env.bridge_path(), "#!/bin/sh\n").expect("bridge");
    let (ok, out, _) = go(
        &Cmd::Mark {
            json: false,
            commands: false,
            spine: false,
        },
        &env,
    );
    assert!(ok);
    assert!(out.contains("mark=degraded"), "{out}");
    assert!(out.contains("degraded=spine-down"), "{out}");
    assert!(out.contains("holds no watch"), "{out}");
}

/// BOTH off switches reach the mark through the real command: the durable one
/// in aterm.toml and the per-session environment bypass.
#[test]
fn both_off_switches_read_bypassed_through_the_command() {
    let tmp = Tmp::new("mark-off");
    let mut env = env_at(&tmp);
    std::fs::create_dir_all(env.bridge_path().parent().expect("parent")).expect("mkdir");
    std::fs::write(env.bridge_path(), "#!/bin/sh\n").expect("bridge");
    let ask = |env: &Env| {
        go(
            &Cmd::Mark {
                json: false,
                commands: false,
                spine: true,
            },
            env,
        )
        .1
    };
    std::fs::write(
        env.config.clone().expect("cfg"),
        "[harness]\nenabled = false\n",
    )
    .expect("write");
    let out = ask(&env);
    assert!(out.contains("bypassed=config"), "{out}");
    assert!(out.contains('◌'), "{out}");
    // The per-session bypass, with the config switch back on.
    std::fs::write(
        env.config.clone().expect("cfg"),
        "[harness]\nenabled = true\n",
    )
    .expect("write");
    env.no_harness = Some("1".to_string());
    let out = ask(&env);
    assert!(out.contains("bypassed=env"), "{out}");
    // NEGATIVE CASES: an EMPTY or "0" value is not a bypass — an inherited
    // empty variable must not silently kill the harness.
    for v in ["", "0"] {
        env.no_harness = Some(v.to_string());
        let out = ask(&env);
        assert!(out.contains("mark=armed"), "{v:?}: {out}");
    }
}

/// Nothing attached is `no-prelude`, and the prelude's own variable attaches.
#[test]
fn nothing_attached_is_no_prelude_and_the_prelude_variable_attaches() {
    let tmp = Tmp::new("attach");
    let mut env = env_at(&tmp);
    assert_eq!(env.attach(), Attach::None);
    let (_, out, _) = go(
        &Cmd::Mark {
            json: false,
            commands: false,
            spine: true,
        },
        &env,
    );
    assert!(out.contains("bypassed=no-prelude"), "{out}");
    env.caps_env = Some(DEFAULT_CAPS.to_string());
    assert_eq!(env.attach(), Attach::Prelude);
    // NEGATIVE CASE: an EMPTY capability export proves no prelude ran.
    env.caps_env = Some(String::new());
    assert_eq!(env.attach(), Attach::None);
}

/// `--commands` prints exactly the three control-protocol lines a host sends,
/// and this command opens no socket to do it.
#[test]
fn mark_commands_are_three_sendable_control_lines() {
    let tmp = Tmp::new("mark-cmds");
    let env = env_at(&tmp);
    let (ok, out, _) = go(
        &Cmd::Mark {
            json: false,
            commands: true,
            spine: true,
        },
        &env,
    );
    assert!(ok);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert!(lines[0].starts_with("meta set icon "), "{out}");
    assert!(lines[1].starts_with("meta set description "), "{out}");
    assert!(lines[2].starts_with("appnotice toolchain "), "{out}");
}

/// `status` carries the mark beside the source, and the JSON form nests it.
#[test]
fn status_carries_the_mark_beside_the_source() {
    let tmp = Tmp::new("status-mark");
    let env = env_at(&tmp);
    let (_, line, _) = go(
        &Cmd::Status {
            json: false,
            spine: true,
        },
        &env,
    );
    assert!(line.contains("source=grid"), "{line}");
    assert!(line.contains("mark=bypassed"), "{line}");
    assert_eq!(line.lines().count(), 1, "{line}");
    let (_, json, _) = go(
        &Cmd::Status {
            json: true,
            spine: true,
        },
        &env,
    );
    let doc = aterm_json::from_str::<Value>(&json).expect("json");
    assert_eq!(doc.get("schema").and_then(|v| v.as_u64()), Some(1));
    let presence = doc.get("presence").expect("presence");
    assert_eq!(
        presence.get("mark").and_then(|v| v.as_str()),
        Some("bypassed")
    );
    assert_eq!(
        presence.get("bypassed").and_then(|v| v.as_str()),
        Some("no-prelude")
    );
}

/// `enable`/`disable` round-trip through the on-disk store, and every answer
/// names the master switch's home — because it is not here.
#[test]
fn enable_and_disable_round_trip_and_name_the_master_switch_elsewhere() {
    let tmp = Tmp::new("switch");
    let env = env_at(&tmp);
    let (ok, out, _) = go(&Cmd::Disable { caps: Vec::new() }, &env);
    assert!(ok);
    assert!(out.contains("disabled every capability"), "{out}");
    assert!(out.contains("live: -"), "{out}");
    assert!(out.contains("harness.enabled"), "{out}");
    assert!(out.contains("aterm.toml"), "{out}");
    assert_eq!(env.caps_off().len(), caps().len());
    // A single capability, in BOTH accepted spellings.
    let (ok, out, _) = go(
        &Cmd::Enable {
            caps: vec!["limits".to_string()],
        },
        &env,
    );
    assert!(ok);
    assert!(out.contains("live: limits"), "{out}");
    assert!(!env.caps_off().contains(&"limits".to_string()));
    let (cmd, _) = parse(&["disable".to_string(), "cap=limits".to_string()]).expect("parse");
    assert_eq!(
        cmd,
        Cmd::Disable {
            caps: vec!["limits".to_string()]
        }
    );
    let (bare, _) = parse(&["disable".to_string(), "limits".to_string()]).expect("parse");
    assert_eq!(bare, cmd, "cap=<c> and the bare name are the same request");
    // Enabling everything empties the store, and the verbs SAY when nothing
    // changed rather than pretending to have acted.
    let (ok, _, _) = go(&Cmd::Enable { caps: Vec::new() }, &env);
    assert!(ok);
    assert!(env.caps_off().is_empty());
    let (ok, out, _) = go(&Cmd::Enable { caps: Vec::new() }, &env);
    assert!(ok);
    assert!(out.contains("nothing changed"), "{out}");
}

/// A capability that is off degrades the mark, end to end: the switch on disk
/// is what the indicator reports.
#[test]
fn a_disabled_capability_degrades_the_mark_end_to_end() {
    let tmp = Tmp::new("switch-mark");
    let env = env_at(&tmp);
    std::fs::create_dir_all(env.bridge_path().parent().expect("parent")).expect("mkdir");
    std::fs::write(env.bridge_path(), "#!/bin/sh\n").expect("bridge");
    let (ok, _, _) = go(
        &Cmd::Disable {
            caps: vec!["usage-hud".to_string()],
        },
        &env,
    );
    assert!(ok);
    let (_, out, _) = go(
        &Cmd::Mark {
            json: false,
            commands: false,
            spine: true,
        },
        &env,
    );
    assert!(out.contains("mark=degraded"), "{out}");
    assert!(out.contains("degraded=caps-reduced"), "{out}");
    assert!(out.contains("caps_live=3/4"), "{out}");
}

/// NEGATIVE CASE: an unknown capability is refused with an exit of 1, nothing
/// is written, and the answer names the real set.
#[test]
fn an_unknown_capability_is_refused_and_writes_nothing() {
    let tmp = Tmp::new("switch-bad");
    let env = env_at(&tmp);
    let (ok, out, err) = go(
        &Cmd::Disable {
            caps: vec!["usage-hood".to_string()],
        },
        &env,
    );
    assert!(!ok, "a refusal it can explain exits 1");
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("usage-hood"), "{err}");
    assert!(err.contains("usage-hud"), "{err}");
    assert!(env.caps_off().is_empty());
    assert!(!mark::caps_path(&env.state).exists());
}

/// The `rm` approval's reason field is produced by the ONE attribution
/// helper, so the line inside the vendor's transcript names the harness and
/// the ledger row that explains it.
#[test]
fn the_rm_approval_reason_is_the_shared_attribution() {
    let json = allow_json(HookEvent::PreToolUse, 812);
    let doc = aterm_json::from_str::<Value>(&json).expect("json");
    let reason = doc
        .get("hookSpecificOutput")
        .and_then(|o| o.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .expect("reason");
    assert_eq!(reason, mark::attribution(Voice::Field, RULE_RM, 812));
    assert_eq!(reason, "aterm harness rm policy id=812");
}

/// The bridge and the mark must read `$ATERM_NO_HARNESS` the SAME way. A
/// bridge that exits on an inherited empty variable while the mark renders
/// `armed` is exactly the two-surfaces-disagree failure the indicator exists
/// to prevent, so the script's guard is asserted to be the shared rule's
/// shape and driven through a real `sh`.
#[test]
fn the_bridge_and_the_mark_read_the_bypass_the_same_way() {
    let script = bridge_sh("/nonexistent/aterm");
    assert!(
        script.contains(r#"case "${ATERM_NO_HARNESS:-}" in ""|0) ;; *) exit 0 ;; esac"#),
        "{script}"
    );
    #[cfg(unix)]
    {
        let tmp = Tmp::new("bridge-bypass");
        let path = tmp.path().join("bridge.sh");
        // A probe body: the guard, then a marker the guard suppresses.
        let guard = r#"case "${ATERM_NO_HARNESS:-}" in ""|0) ;; *) exit 0 ;; esac
printf ran"#;
        std::fs::write(&path, guard).expect("write");
        for (value, engaged) in [
            (None, false),
            (Some(""), false),
            (Some("0"), false),
            (Some("1"), true),
            (Some("no"), true),
            (Some("00"), true),
        ] {
            let mut cmd = std::process::Command::new("/bin/sh");
            cmd.arg(&path);
            match value {
                Some(v) => cmd.env("ATERM_NO_HARNESS", v),
                None => cmd.env_remove("ATERM_NO_HARNESS"),
            };
            let out = cmd.output().expect("sh");
            let ran = out.stdout == b"ran";
            assert_eq!(
                ran, !engaged,
                "ATERM_NO_HARNESS={value:?}: the bridge ran={ran}"
            );
            assert_eq!(
                mark::env_engaged(value),
                engaged,
                "ATERM_NO_HARNESS={value:?}: the mark's reading"
            );
        }
    }
}

/// A DISABLED harness: every read verb still answers, and they SAY the harness
/// is off rather than going quiet. "The harness did nothing" and "the harness
/// was not there" are different answers (design §4.6.3), and the second one
/// has to be readable without asking a second question.
#[test]
fn a_disabled_harness_still_answers_and_every_verb_says_it_is_off() {
    let tmp = Tmp::new("disabled");
    let env = env_at(&tmp);
    std::fs::create_dir_all(env.bridge_path().parent().expect("parent")).expect("mkdir");
    std::fs::write(env.bridge_path(), "#!/bin/sh\n").expect("bridge");
    std::fs::write(
        env.config.clone().expect("cfg"),
        "[harness]\nenabled = false\n",
    )
    .expect("write config");

    for (cmd, must) in [
        (
            Cmd::Status {
                json: false,
                spine: true,
            },
            "bypassed=config",
        ),
        (
            Cmd::Mark {
                json: false,
                commands: false,
                spine: true,
            },
            "harness.enabled = false in aterm.toml",
        ),
    ] {
        let (ok, out, err) = go(&cmd, &env);
        assert!(ok, "{cmd:?} must still answer; stderr: {err}");
        assert!(out.contains(must), "{cmd:?} must say {must:?}, said: {out}");
    }
    // The READ verbs are untouched by the switch: they report what the
    // ledgers hold, and going silent would be the worst answer of the three.
    for cmd in [Cmd::Usage { json: true }, Cmd::Limits { json: true }] {
        let (ok, out, _) = go(&cmd, &env);
        assert!(ok, "{cmd:?}");
        assert!(out.contains("\"source\":\"grid\""), "{cmd:?}: {out}");
    }
    // The switch is NOT read by the bridge, and that is design §4.6.2: turning
    // it off re-renders the launcher twin so the NEXT launch is plain, while a
    // session already running keeps the hooks it launched with until it exits.
    // An in-flight session whose hooks died mid-turn is the failure that rule
    // exists to avoid.
    assert!(
        !bridge_sh("/nonexistent/aterm").contains("harness.enabled"),
        "the bridge must not read the durable switch"
    );
}

// ---------------------------------------------------------------------------
// The watch verbs (design §5.4, §5.7, §5.8.7)
// ---------------------------------------------------------------------------

/// A `StopFailure` row and a statusLine row that together name an exhausted
/// five-hour window — the pair §5.8.2 asks for, from the two sources a real
/// session has.
fn seed_exhausted(env: &Env) {
    append_row(
        env,
        RING_EVENT,
        r#"{"id":0,"event":"StopFailure","error":"rate_limit","sid":"test-sid"}"#,
    )
    .expect("append");
    append_row(
        env,
        RING_STATUSLINE,
        r#"{"rate_limits":{"five_hour":{"used_percentage":100,"resets_at":1758414600},"seven_day":{"used_percentage":41,"resets_at":1758999999}}}"#,
    )
    .expect("append");
}

#[test]
fn the_watch_verbs_answer_with_no_hook_ever_fired_and_say_which_state_needs_one() {
    let tmp = Tmp::new("watch-spine");
    let env = env_at(&tmp);
    for (cmd, must_contain) in [
        (Cmd::Liveness { json: false }, "hooks=absent"),
        (Cmd::Liveness { json: true }, "\"hooks\":\"absent\""),
        (
            Cmd::Nudge {
                sid: String::new(),
                level: watch::Level::Warn,
                json: false,
                commands: false,
                spine: false,
            },
            "verdict=",
        ),
    ] {
        let (ok, out, err) = go(&cmd, &env);
        assert!(ok, "{cmd:?} must exit 0; stderr: {err}");
        assert!(
            out.contains(must_contain),
            "{cmd:?} must say {must_contain:?}, said: {out}"
        );
    }
    // The ONE state that is hook-only names itself rather than reading false.
    let (_, text, _) = go(&Cmd::Liveness { json: false }, &env);
    assert!(
        text.contains("stopped-short: unavailable (hooks=absent)"),
        "{text}"
    );
    let (_, json, _) = go(&Cmd::Liveness { json: true }, &env);
    assert!(
        json.contains("\"stopped_short\":\"unavailable (hooks=absent)\""),
        "{json}"
    );
    // And it never claims a watch it does not hold.
    assert!(json.contains("\"spine\":false"), "{json}");
}

#[test]
fn recover_reads_the_class_from_the_ledger_and_refuses_one_asserted_on_the_command_line() {
    let tmp = Tmp::new("recover");
    let env = env_at(&tmp);
    seed_exhausted(&env);
    let (_, limits_out, _) = go(&Cmd::Limits { json: false }, &env);
    assert!(
        limits_out.contains("class=session-5h-limit"),
        "{limits_out}"
    );

    let (ok, out, _) = go(
        &Cmd::Recover {
            what: "session-5h-limit".to_string(),
            target: None,
            json: false,
            commands: false,
            spine: false,
        },
        &env,
    );
    assert!(ok, "{out}");
    assert!(out.contains("verdict=planned"), "{out}");
    assert!(out.contains("cap=limits"), "{out}");
    // Nothing was SENT: this command opens no control socket.
    assert!(out.contains("nothing was sent"), "{out}");

    // NEGATIVE CASE: a class the evidence does not name is refused, and the
    // refusal says where a class comes from.
    let (ok, _, err) = go(
        &Cmd::Recover {
            what: "auth".to_string(),
            target: None,
            json: false,
            commands: false,
            spine: false,
        },
        &env,
    );
    assert!(!ok);
    assert!(err.contains("the ledger names session-5h-limit"), "{err}");

    // And with no evidence at all, an ACTION has nothing to recover from.
    let bare = Tmp::new("recover-bare");
    let (ok, _, err) = go(
        &Cmd::Recover {
            what: "switch-model".to_string(),
            target: None,
            json: false,
            commands: false,
            spine: false,
        },
        &env_at(&bare),
    );
    assert!(!ok);
    assert!(err.contains("no limit class is live"), "{err}");
}

#[test]
fn recover_commands_prints_the_fence_and_never_a_bare_enter() {
    let tmp = Tmp::new("recover-commands");
    let env = env_at(&tmp);
    seed_exhausted(&env);
    let (ok, out, err) = go(
        &Cmd::Recover {
            what: "switch-model".to_string(),
            target: Some("opus".to_string()),
            json: false,
            commands: true,
            // The caller ASSERTS the spine. Without it the lines below are
            // withheld — pinned by the negative control at the end of this
            // test — because this command reads no grid, so the approval-box
            // fence would be at its unmeasured default.
            spine: true,
        },
        &env,
    );
    assert!(ok, "{err}");
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2, "a turn and its guarded press: {lines:?}");
    assert!(
        lines[0].starts_with(&format!("turn id={TEST_NONCE}:")),
        "{lines:?}"
    );
    // The `<epoch>` is checked against the SERVER's reader, never against a
    // literal this file also wrote: `pty_idem::parse_key` answers
    // `ERR usage: …` as the whole reply for anything else, before it types.
    let epoch = lines[0]
        .trim_start_matches("turn id=")
        .split(':')
        .next()
        .expect("an epoch");
    assert!(
        aterm_session::LaunchNonce::from_hex(epoch).is_some(),
        "{epoch:?} is not a launch nonce: {lines:?}"
    );
    assert!(lines[0].contains("submit=none"), "{lines:?}");
    assert!(lines[0].ends_with("/model opus"), "{lines:?}");
    assert!(lines[1].starts_with("key if="), "{lines:?}");
    assert!(lines[1].ends_with(" enter"), "{lines:?}");
    assert_ne!(lines[1], "key enter", "never a bare Enter");
    // Never `y`, in any shape, on any line: answering an approval is the
    // vendor's channel.
    for l in &lines {
        assert!(!l.ends_with(" y"), "{l}");
    }
}

#[test]
fn a_hand_asked_rung_runs_at_the_level_it_names_and_the_warn_rung_types_nothing() {
    let tmp = Tmp::new("nudge");
    let env = env_at(&tmp);
    // The warn rung: L1 only, and it sends `meta set`/`appnotice` lines.
    let (ok, out, _) = go(
        &Cmd::Nudge {
            sid: String::new(),
            level: watch::Level::Warn,
            json: false,
            commands: true,
            spine: false,
        },
        &env,
    );
    assert!(ok);
    for l in out.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
        assert!(
            l.starts_with("meta set ") || l.starts_with("appnotice "),
            "rung 1 types nothing: {l}"
        );
    }
    // The guards under an L1 rung are still assumptions, and the output says
    // so on every socket-free plan.
    assert!(out.contains("no spine was read"), "{out}");
    // The escape rung asked for at its own level is planned; asked for
    // through a watcher pinned lower it is refused by level. The CLI raises
    // the level for the rung the operator NAMED, so the refusal that remains
    // is the budget's, which is the honest one for a hand-asked interrupt.
    let (ok, out, _) = go(
        &Cmd::Nudge {
            sid: String::new(),
            level: watch::Level::Escape,
            json: true,
            commands: false,
            spine: false,
        },
        &env,
    );
    assert!(ok);
    assert!(out.contains("\"kind\":\"nudge\""), "{out}");
    assert!(out.contains("\"verdict\":\"planned\""), "{out}");

    // A sid that is not this session is NAMED rather than silently ignored.
    let (_, out, _) = go(
        &Cmd::Nudge {
            sid: "s-other".to_string(),
            level: watch::Level::Warn,
            json: false,
            commands: false,
            spine: false,
        },
        &env,
    );
    assert!(out.contains("s-other is not this session"), "{out}");
}

#[test]
fn a_bypassed_harness_refuses_every_planned_act() {
    let tmp = Tmp::new("bypassed");
    let mut env = env_at(&tmp);
    env.no_harness = Some("1".to_string());
    seed_exhausted(&env);
    let (ok, out, _) = go(
        &Cmd::Recover {
            what: "session-5h-limit".to_string(),
            target: None,
            json: false,
            commands: false,
            spine: false,
        },
        &env,
    );
    assert!(ok);
    assert!(out.contains("verdict=refused:bypassed"), "{out}");
}

#[test]
fn the_watch_grammar_refuses_what_it_does_not_know() {
    let words = |s: &str| -> Vec<String> { s.split(' ').map(str::to_string).collect() };
    // NEGATIVE CASES.
    assert!(
        parse(&words("recover")).is_err(),
        "recover needs an operand"
    );
    assert!(parse(&words("recover no-such-class")).is_err());
    assert!(parse(&words("nudge --level loud")).is_err());
    assert!(parse(&words("nudge --level")).is_err());
    assert!(parse(&words("recover auth --target")).is_err());
    // And the ones it does know.
    assert!(matches!(
        parse(&words("liveness --json")).expect("parses").0,
        Cmd::Liveness { json: true }
    ));
    assert!(matches!(
        parse(&words("recover switch-model --commands"))
            .expect("parses")
            .0,
        Cmd::Recover { commands: true, .. }
    ));
    assert!(matches!(
        parse(&words("nudge --level escape")).expect("parses").0,
        Cmd::Nudge {
            level: watch::Level::Escape,
            ..
        }
    ));
    // A bare `nudge` is the warn rung — the one that types nothing.
    assert!(matches!(
        parse(&words("nudge")).expect("parses").0,
        Cmd::Nudge {
            level: watch::Level::Warn,
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// `align` and `caps` (design §3.2, §3.5, §3.6)
// ---------------------------------------------------------------------------

/// A contract whose two capabilities stand on the two probes written beside it.
const TREE_CONTRACT: &str = r#"
abi = 1
wraps_any = ["claude"]
capabilities = ["usage-hud", "introspect"]

[[tool]]
bin = "no-such-tool-anywhere"
probe = "version"
min = ""
caps = ["usage-hud.sheet"]

[[capability]]
id = "usage-hud"
needs = ["literal.statusLine"]

[[capability]]
id = "introspect"
needs = ["help.settings"]
"#;

#[cfg(unix)]
fn write_exe(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("the parent is created");
    }
    std::fs::write(path, body).expect("the script is written");
    let mut perms = std::fs::metadata(path).expect("it exists").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("it is executable");
}

/// Lay a harness TREE under `tmp` and a fake installed program beside it.
/// `settings_status` is what the `help.settings` probe prints.
#[cfg(unix)]
fn lay_tree(tmp: &Tmp, contract: &str, settings_status: &str) -> (PathBuf, PathBuf) {
    let tree = tmp
        .path()
        .join("store")
        .join("claude-harness")
        .join("2026092101");
    let probes = tree.join("probes");
    write_exe(
        &probes.join("literal.statusLine"),
        "#!/bin/sh\nprintf '{\"status\":\"absent\",\"detail\":\"no statusLine literal\"}'\n",
    );
    write_exe(
        &probes.join("help.settings"),
        &format!("#!/bin/sh\nprintf '{{\"status\":\"{settings_status}\"}}'\n"),
    );
    std::fs::write(tree.join("harness.toml"), contract).expect("the contract is written");
    let claude = tmp.path().join("bin").join("claude");
    write_exe(&claude, "#!/bin/sh\necho '2.1.274 (Claude Code)'\n");
    (tree, claude)
}

#[cfg(unix)]
fn aligned_env(tmp: &Tmp, tree: &Path, claude: &Path) -> Env {
    let mut env = env_at(tmp);
    env.tree = Some(tree.to_path_buf());
    env.against = Some(claude.to_path_buf());
    env
}

#[cfg(unix)]
#[test]
fn align_probes_a_real_tree_and_degrades_exactly_what_failed() {
    let tmp = Tmp::new("align-real");
    let (tree, claude) = lay_tree(&tmp, TREE_CONTRACT, "ok");
    let env = aligned_env(&tmp, &tree, &claude);

    let (ok, stdout, stderr) = go(
        &Cmd::Align {
            json: true,
            write: false,
            signed: None,
        },
        &env,
    );
    assert!(ok, "align exits 0 on a tree it could read: {stderr}");
    let doc: Value = aterm_json::from_str(stdout.trim()).expect("the output is JSON");
    let Value::Object(o) = doc else {
        panic!("the output is an object");
    };
    assert_eq!(
        o.get("verdict").and_then(Value::as_str),
        Some("degraded(1/2):usage-hud"),
        "the absent statusLine literal takes down usage-hud and nothing else"
    );
    assert_eq!(o.get("program").and_then(Value::as_str), Some("claude"));
    assert_eq!(
        o.get("program_build").and_then(Value::as_str),
        Some("2.1.274"),
        "the program build is MEASURED off the binary, never assumed"
    );
    assert_eq!(
        o.get("aterm").and_then(Value::as_str),
        Some("0.0.0-test"),
        "the aterm token is injected, never read from this build"
    );
    assert_eq!(
        o.get("harness_build").and_then(Value::as_str),
        Some("2026092101")
    );
    assert_eq!(o.get("attested").and_then(Value::as_str), Some("local"));

    // The human form names the same facts, and `caps` names the probe that
    // decided each one.
    let (_, stdout, _) = go(
        &Cmd::Align {
            json: false,
            write: false,
            signed: None,
        },
        &env,
    );
    assert!(
        stdout.contains("verdict   degraded(1/2):usage-hud"),
        "{stdout}"
    );
    assert!(stdout.contains("1 ok, 2 absent, 0 error"), "{stdout}");
    let (_, stdout, _) = go(
        &Cmd::Caps {
            json: false,
            signed: None,
        },
        &env,
    );
    assert!(
        stdout.contains("usage-hud        OFF  literal.statusLine: absent — no statusLine literal"),
        "{stdout}"
    );
    assert!(stdout.contains("introspect       on"), "{stdout}");
    assert!(
        stdout.contains("tool no-such-tool-anywhere"),
        "a [[tool]] row is listed beside the capabilities: {stdout}"
    );
    // NEGATIVE CONTROL: the sub-capability the absent tool gates is off, and
    // its PARENT is not off BECAUSE of it.
    let (_, json, _) = go(
        &Cmd::Caps {
            json: true,
            signed: None,
        },
        &env,
    );
    assert!(json.contains("\"blamed\":\"literal.statusLine\""), "{json}");
    assert!(
        !json.contains("\"blamed\":\"tool.no-such-tool-anywhere\""),
        "{json}"
    );
}

#[cfg(unix)]
#[test]
fn align_with_every_probe_ok_is_aligned_and_writes_one_sidecar_row() {
    let tmp = Tmp::new("align-write");
    let (tree, claude) = lay_tree(&tmp, TREE_CONTRACT, "ok");
    // Make the statusLine probe pass too.
    write_exe(
        &tree.join("probes").join("literal.statusLine"),
        "#!/bin/sh\nprintf '{\"status\":\"ok\"}'\n",
    );
    let env = aligned_env(&tmp, &tree, &claude);

    let (ok, stdout, _) = go(
        &Cmd::Align {
            json: false,
            write: true,
            signed: None,
        },
        &env,
    );
    assert!(ok);
    assert!(stdout.contains("verdict   aligned(2/2)"), "{stdout}");
    assert!(stdout.contains("wrote     "), "{stdout}");

    let sidecar = align::sidecar_path(&env.state, "2026092101");
    let text = std::fs::read_to_string(&sidecar).expect("the sidecar was written");
    let rows = align::read_sidecar(&text);
    assert_eq!(rows.len(), 1, "{text:?}");
    assert_eq!(rows[0].wire, "aligned(2/2)");
    assert_eq!(rows[0].at, env.now);

    // A second pass APPENDS; the newest row wins, and the key is the tuple.
    let (_, _, _) = go(
        &Cmd::Align {
            json: false,
            write: true,
            signed: None,
        },
        &env,
    );
    let text = std::fs::read_to_string(&sidecar).expect("the sidecar is still there");
    assert_eq!(align::read_sidecar(&text).len(), 2);
}

#[cfg(unix)]
#[test]
fn a_signed_entry_narrows_a_local_verdict_and_never_widens_one() {
    let tmp = Tmp::new("align-signed");
    let (tree, claude) = lay_tree(&tmp, TREE_CONTRACT, "ok");
    write_exe(
        &tree.join("probes").join("literal.statusLine"),
        "#!/bin/sh\nprintf '{\"status\":\"ok\"}'\n",
    );
    // A contract that DECLARES the probe set it ships, so a signed row can be
    // about this tree at all.
    let digest = align::probe_set_digest(&tree.join("probes")).expect("the probes digest");
    let contract =
        TREE_CONTRACT.replacen("abi = 1", &format!("abi = 1\nprobe_set = \"{digest}\""), 1);
    std::fs::write(tree.join("harness.toml"), &contract).expect("rewritten");
    let env = aligned_env(&tmp, &tree, &claude);
    let short = align::probe_set_short(&digest);

    // Locally aligned; the lane says usage-hud is off. The intersection is
    // degraded, and the disagreement is reported rather than smoothed over.
    let entry = format!("claude@2.1.274=degraded:usage-hud#{short}");
    let (ok, stdout, _) = go(
        &Cmd::Align {
            json: false,
            write: false,
            signed: Some(entry),
        },
        &env,
    );
    assert!(ok);
    assert!(
        stdout.contains("verdict   degraded(1/2):usage-hud"),
        "{stdout}"
    );
    assert!(stdout.contains("attested  both"), "{stdout}");
    assert!(stdout.contains("signed and local disagree"), "{stdout}");

    // NEGATIVE: a signed `aligned` cannot raise a local degradation.
    write_exe(
        &tree.join("probes").join("literal.statusLine"),
        "#!/bin/sh\nprintf '{\"status\":\"absent\"}'\n",
    );
    let digest = align::probe_set_digest(&tree.join("probes")).expect("the probes digest");
    let contract =
        TREE_CONTRACT.replacen("abi = 1", &format!("abi = 1\nprobe_set = \"{digest}\""), 1);
    std::fs::write(tree.join("harness.toml"), &contract).expect("rewritten");
    let short = align::probe_set_short(&digest);
    let entry = format!("claude@2.1.274=aligned#{short}");
    let (_, stdout, _) = go(
        &Cmd::Align {
            json: false,
            write: false,
            signed: Some(entry),
        },
        &env,
    );
    assert!(
        stdout.contains("verdict   degraded(1/2):usage-hud"),
        "local lowers a signed aligned: {stdout}"
    );

    // NEGATIVE: a signed entry about ANOTHER tuple is named and not adopted.
    let entry = format!("claude@9.9.9=unsupported:nope#{short}");
    let (_, stdout, _) = go(
        &Cmd::Align {
            json: false,
            write: false,
            signed: Some(entry),
        },
        &env,
    );
    assert!(stdout.contains("not about this tuple"), "{stdout}");
    assert!(stdout.contains("verdict   degraded(1/2)"), "{stdout}");
}

#[cfg(unix)]
#[test]
fn a_probe_set_that_does_not_match_the_tree_is_unsupported_and_renders_nothing() {
    let tmp = Tmp::new("align-digest");
    let (tree, claude) = lay_tree(&tmp, TREE_CONTRACT, "ok");
    let contract = TREE_CONTRACT.replacen(
        "abi = 1",
        &format!("abi = 1\nprobe_set = \"sha256:{}\"", "0".repeat(64)),
        1,
    );
    std::fs::write(tree.join("harness.toml"), contract).expect("rewritten");
    let env = aligned_env(&tmp, &tree, &claude);
    let (ok, stdout, _) = go(
        &Cmd::Align {
            json: false,
            write: false,
            signed: None,
        },
        &env,
    );
    assert!(ok, "a mismatched digest is a verdict, not a crash");
    assert!(
        stdout.contains("verdict   unsupported:probe_set"),
        "{stdout}"
    );
    assert!(stdout.contains("does not match probes/"), "{stdout}");
    // And nothing renders.
    let (_, json, _) = go(
        &Cmd::Align {
            json: true,
            write: false,
            signed: None,
        },
        &env,
    );
    assert!(json.contains("\"caps_ok\":0"), "{json}");
}

#[test]
fn align_with_no_tree_is_pending_with_a_note_and_still_exits_zero() {
    let tmp = Tmp::new("align-none");
    let env = env_at(&tmp);
    let (ok, stdout, stderr) = go(
        &Cmd::Align {
            json: false,
            write: false,
            signed: None,
        },
        &env,
    );
    assert!(ok, "no tree is not an error: {stderr}");
    assert!(stdout.contains("verdict   pending"), "{stdout}");
    assert!(
        stdout.contains("no harness tree"),
        "a pending verdict must say why: {stdout}"
    );
    // A `pending` verdict is never CACHED: a cached one would stop the next
    // pass measuring.
    let (_, stdout, stderr) = go(
        &Cmd::Align {
            json: false,
            write: true,
            signed: None,
        },
        &env,
    );
    assert!(!stdout.contains("wrote "), "{stdout}");
    assert!(stderr.contains("nothing was measured"), "{stderr}");
    assert!(
        !align::sidecar_path(&env.state, "dev").exists(),
        "no sidecar is written for a pending verdict"
    );
    // `caps` answers too, with the note and no capability rows.
    let (ok, stdout, _) = go(
        &Cmd::Caps {
            json: false,
            signed: None,
        },
        &env,
    );
    assert!(ok);
    assert!(stdout.contains("note no harness tree"), "{stdout}");
}

#[cfg(unix)]
#[test]
fn a_refused_contract_exits_one_names_the_line_and_renders_nothing() {
    let tmp = Tmp::new("align-refused");
    let (tree, claude) = lay_tree(&tmp, TREE_CONTRACT, "ok");
    std::fs::write(
        tree.join("harness.toml"),
        "abi = 1\nwraps_any = [\"claude\"]\ncapabilities = [\"usage-hud\"]\n\n[shim]\nargs = [\"--dangerously-skip-permissions\"]\n\n[[capability]]\nid = \"usage-hud\"\n",
    )
    .expect("rewritten");
    let env = aligned_env(&tmp, &tree, &claude);
    let (ok, stdout, stderr) = go(
        &Cmd::Align {
            json: false,
            write: false,
            signed: None,
        },
        &env,
    );
    assert!(!ok, "a refused contract is an explainable refusal");
    assert!(
        stderr.contains("closed vocabulary"),
        "the refusal names why: {stderr}"
    );
    assert!(stdout.contains("verdict   pending"), "{stdout}");
    let (ok, _, stderr) = go(
        &Cmd::Caps {
            json: false,
            signed: None,
        },
        &env,
    );
    assert!(!ok);
    assert!(stderr.contains("aterm harness caps:"), "{stderr}");
}

#[test]
fn the_new_flags_parse_and_reach_the_overrides() {
    let words = |s: &str| -> Vec<String> { s.split(' ').map(str::to_string).collect() };
    assert!(matches!(
        parse(&words("align --json --write")).expect("parses").0,
        Cmd::Align {
            json: true,
            write: true,
            signed: None
        }
    ));
    let (cmd, over) = parse(&words(
        "align --tree /opt/tree --against /usr/bin/claude --aterm-build 0.86.0 --signed claude@1=aligned#a1b2c3d4",
    ))
    .expect("parses");
    assert_eq!(over.tree.as_deref(), Some(Path::new("/opt/tree")));
    assert_eq!(over.against.as_deref(), Some(Path::new("/usr/bin/claude")));
    assert_eq!(over.aterm_build.as_deref(), Some("0.86.0"));
    assert!(matches!(
        cmd,
        Cmd::Align {
            signed: Some(ref e),
            ..
        } if e == "claude@1=aligned#a1b2c3d4"
    ));
    assert!(matches!(
        parse(&words("caps --json")).expect("parses").0,
        Cmd::Caps { json: true, .. }
    ));
    // NEGATIVE: every new flag needs its value.
    for missing in [
        "align --tree",
        "align --against",
        "align --aterm-build",
        "align --signed",
    ] {
        assert!(parse(&words(missing)).is_err(), "{missing}");
    }
}

// -- the spine-side usage source, the bounded chain, the printed assumption -----

#[test]
fn the_project_directory_name_reproduces_the_two_measured_ones() {
    // MEASURED on this machine, 2026-09-21, by listing
    // `~/.claude/projects`: these two names are what the vendor wrote (the
    // home-directory prefix is anonymised; the fold is the same for any path).
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

#[test]
fn usage_with_zero_hooks_folds_the_transcript_and_says_so() {
    // THE CENTRAL LAW. `--bare` removes hooks, plugins and the statusLine in
    // one flag; `~/.claude/projects/**/*.jsonl` is a filesystem fact it does
    // not touch. Before this, `harness usage` read the statusLine ring and
    // nothing else, and answered a row of zeros labelled `source=grid`.
    let tmp = Tmp::new("usage-transcript");
    let env = env_at(&tmp);
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
    // NOTHING named a session, so the answer is the WEAKER transcript word
    // and it says the newest file is what it read.
    assert!(out.contains("source=transcript-newest"), "{out}");
    assert!(out.contains("session-a.jsonl"), "the path is named: {out}");
    assert!(
        out.contains("2 assistant rows"),
        "what was folded is stated: {out}"
    );
    assert!(
        out.contains("NOTHING named a session"),
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

    // NEGATIVE CONTROL: a statusLine sample outranks it for the SOURCE word,
    // because the vendor's own windows are the higher authority and the
    // transcript carries no window at all — but the spend is still folded,
    // because spend exists nowhere else.
    append_row(&env, RING_STATUSLINE, STATUSLINE.replace('\n', " ").trim()).expect("append");
    let (_, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(out.contains("source=statusline"), "{out}");
    assert!(
        out.contains("session-a.jsonl"),
        "spend is still folded: {out}"
    );
}

/// GAP 2, the transcript ambiguity. Two Claude Code sessions in ONE working
/// directory write into one project directory, so "the newest `.jsonl`" can
/// be the other session's. A statusLine payload names `session_id`, and that
/// name wins over the clock.
#[test]
fn usage_folds_the_transcript_the_statusline_names_not_the_newest_one() {
    let tmp = Tmp::new("usage-session-id");
    let env = env_at(&tmp);
    let dir = env
        .transcripts_root()
        .expect("home is injected")
        .join(project_dir_name(&env.cwd));
    std::fs::create_dir_all(&dir).expect("project dir");
    let row = |id: &str, input: u64| {
        format!(
            r#"{{"type":"assistant","message":{{"id":"{id}","model":"claude-fable-5-1","usage":{{"input_tokens":{input},"output_tokens":1}}}}}}"#
        )
    };
    // OURS is written first, so it is the OLDER file.
    std::fs::write(
        dir.join("ours.jsonl"),
        format!(
            "{}
",
            row("m1", 11)
        ),
    )
    .expect("ours");
    std::fs::write(
        dir.join("theirs.jsonl"),
        format!(
            "{}
",
            row("m2", 9999)
        ),
    )
    .expect("theirs");
    // Make the mtime order unambiguous whatever the filesystem's resolution:
    // the OTHER session's file is the newest, which is the trap.
    assert!(
        std::fs::metadata(dir.join("theirs.jsonl"))
            .and_then(|m| m.modified())
            .expect("mtime")
            >= std::fs::metadata(dir.join("ours.jsonl"))
                .and_then(|m| m.modified())
                .expect("mtime"),
        "the fixture must make the other session's transcript the newest"
    );

    // Without a name: the newest wins, and the output says so.
    let (_, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(out.contains("theirs.jsonl"), "{out}");
    assert!(out.contains("source=transcript-newest"), "{out}");

    // With the statusLine naming OUR session, the name wins over the clock.
    let statusline = r#"{"session_id":"ours","model":{"id":"claude-fable-5-1"}}"#;
    append_row(&env, RING_STATUSLINE, statusline).expect("append");
    let (_, json, _) = go(&Cmd::Usage { json: true }, &env);
    assert!(json.contains("ours.jsonl"), "{json}");
    assert!(!json.contains("theirs.jsonl"), "{json}");
    assert!(
        json.contains("\"transcript_pick\":\"statusline-session\""),
        "{json}"
    );
    assert!(
        json.contains("\"in\":11"),
        "the OTHER session's 9999 input tokens are not folded in: {json}"
    );
}

/// The same law from the other named source: a hook row's `session_id`.
/// With no statusLine at all, the newest hook row still names the session.
#[test]
fn usage_folds_the_transcript_a_hook_row_names() {
    let tmp = Tmp::new("usage-hook-session");
    let env = env_at(&tmp);
    let dir = env
        .transcripts_root()
        .expect("home is injected")
        .join(project_dir_name(&env.cwd));
    std::fs::create_dir_all(&dir).expect("project dir");
    let row = |id: &str, input: u64| {
        format!(
            r#"{{"type":"assistant","message":{{"id":"{id}","model":"claude-fable-5-1","usage":{{"input_tokens":{input},"output_tokens":1}}}}}}"#
        )
    };
    std::fs::write(
        dir.join("mine.jsonl"),
        format!(
            "{}
",
            row("m1", 42)
        ),
    )
    .expect("mine");
    std::fs::write(
        dir.join("other.jsonl"),
        format!(
            "{}
",
            row("m2", 7777)
        ),
    )
    .expect("other");

    append_row(
        &env,
        RING_EVENT,
        r#"{"id":1,"event":"Notification","session_id":"mine"}"#,
    )
    .expect("append");
    let (_, json, _) = go(&Cmd::Usage { json: true }, &env);
    assert!(json.contains("mine.jsonl"), "{json}");
    assert!(
        json.contains("\"transcript_pick\":\"hook-session\""),
        "{json}"
    );
    assert!(json.contains("\"source\":\"transcript\""), "{json}");
}

/// A session id is joined onto a directory, so it is a PATH FENCE. A payload
/// that names a traversal, an absolute path or a dotfile names nothing, and
/// the fold falls back to the newest file rather than reading what the
/// payload chose. Rows are data, never instructions (design 4.4).
#[test]
fn a_session_id_that_could_escape_the_project_directory_names_nothing() {
    for hostile in [
        "../../../etc/passwd",
        "/etc/passwd",
        "..",
        ".",
        ".hidden",
        "",
        "has space",
        "semi;colon",
        "new\nline",
    ] {
        assert_eq!(
            admissible_session_id(hostile),
            None,
            "{hostile:?} must not become a file name"
        );
    }
    // The measured shape — a UUID — and the plain names the tests above use.
    for ok in ["5f3a9c21-0b7e-4c66-9f10-2a8e4d1b7c33", "ours", "a_b.c-1"] {
        assert_eq!(admissible_session_id(ok), Some(ok));
    }
    assert_eq!(admissible_session_id(&"a".repeat(MAX_SESSION_ID + 1)), None);
    assert_eq!(
        admissible_session_id(&"a".repeat(MAX_SESSION_ID)).map(str::len),
        Some(MAX_SESSION_ID)
    );

    // End to end: a hostile `session_id` in the ring is ignored, and the
    // answer degrades to the weaker, LABELLED newest-file pick.
    let tmp = Tmp::new("usage-hostile-session");
    let env = env_at(&tmp);
    let dir = env
        .transcripts_root()
        .expect("home is injected")
        .join(project_dir_name(&env.cwd));
    std::fs::create_dir_all(&dir).expect("project dir");
    std::fs::write(
        dir.join("real.jsonl"),
        "{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"model\":\"m\",\"usage\":{\"input_tokens\":1}}}\n",
    )
    .expect("transcript");
    append_row(
        &env,
        RING_STATUSLINE,
        r#"{"session_id":"../../../../../../etc/passwd"}"#,
    )
    .expect("append");
    let (_, json, _) = go(&Cmd::Usage { json: true }, &env);
    assert!(json.contains("real.jsonl"), "{json}");
    assert!(!json.contains("passwd"), "{json}");
    assert!(
        json.contains("\"transcript_pick\":\"newest-mtime\""),
        "{json}"
    );
}

#[test]
fn usage_with_no_hook_and_no_transcript_says_which_one_is_missing() {
    let tmp = Tmp::new("usage-nothing");
    let env = env_at(&tmp);
    let (ok, out, _) = go(&Cmd::Usage { json: false }, &env);
    assert!(ok);
    assert!(out.contains("source=grid"), "{out}");
    assert!(
        out.contains("no statusLine sample and no transcript"),
        "the empty answer names both missing sources: {out}"
    );
}

#[test]
fn a_chained_statusline_is_folded_to_exactly_one_line() {
    // `run_statusline`'s doc says ALWAYS one line, because the vendor renders
    // whatever this prints in its footer. A three-line user command printed
    // three lines before this.
    assert_eq!(statusline_one_line("a\nb\nc\n"), "a\n");
    assert_eq!(statusline_one_line("\n\n  second  \nthird"), "  second\n");
    assert_eq!(statusline_one_line("with\ta tab"), "with a tab\n");
    // NEGATIVE CONTROL for the shared predicate: `U+2028` LINE SEPARATOR is
    // Zl, not Cc, so `char::is_control()` is false for it — and this surface
    // asked exactly that question until 2026-09-22, which let a vendor
    // statusLine put a second line in the footer.
    assert!(!'\u{2028}'.is_control());
    assert_eq!(statusline_one_line("a\u{2028}b"), "a b\n");
    assert_eq!(statusline_one_line("a\u{2029}b"), "a b\n");
    assert_eq!(statusline_one_line("   \n"), "");
    assert_eq!(statusline_one_line(""), "");
    let long = "x".repeat(STATUSLINE_LINE_CAP * 2);
    let got = statusline_one_line(&long);
    assert!(got.len() <= STATUSLINE_LINE_CAP + 1, "{}", got.len());

    // And the real chain, through a real /bin/sh: three lines in, one out,
    // with the payload on its stdin.
    let got = chain_statusline("cat; printf 'b\\nc\\n'", "a-payload\n").expect("it answers");
    assert_eq!(got, "a-payload\n", "{got:?}");
    assert_eq!(got.matches('\n').count(), 1);
    // Silence is `None`, so the caller prints the harness's own line rather
    // than a blank footer.
    assert_eq!(chain_statusline("true", "x"), None);
    // A command that prints its line and exits non-zero keeps its line: the
    // exit code is not part of the vendor's statusLine contract.
    assert_eq!(
        chain_statusline("printf 'mine\\n'; exit 3", "x").as_deref(),
        Some("mine\n")
    );
}

#[test]
fn status_prints_whether_the_spine_was_asserted_or_unread() {
    // §4.6's *never `armed` on an assumption* is a CALLER obligation, and
    // `--assume-spine` is where a caller discharges it by saying so. Nothing
    // in this process opens a socket, so what keeps the sentence true is that
    // the assertion is PRINTED beside the mark it produced.
    let tmp = Tmp::new("status-assume");
    let env = env_at(&tmp);
    let (_, line, _) = go(
        &Cmd::Status {
            json: false,
            spine: true,
        },
        &env,
    );
    assert!(line.contains("spine=asserted"), "{line}");
    assert_eq!(line.lines().count(), 1, "status stays ONE line: {line}");
    let (_, line, _) = go(
        &Cmd::Status {
            json: false,
            spine: false,
        },
        &env,
    );
    assert!(line.contains("spine=unread"), "{line}");
    let (_, json, _) = go(
        &Cmd::Status {
            json: true,
            spine: true,
        },
        &env,
    );
    let doc = aterm_json::from_str::<Value>(&json).expect("json");
    assert_eq!(
        doc.get("spine_asserted").and_then(|v| v.as_bool()),
        Some(true)
    );
    // And the mark command says it in words.
    let (_, out, _) = go(
        &Cmd::Mark {
            json: false,
            commands: false,
            spine: true,
        },
        &env,
    );
    assert!(
        out.contains("cannot check one") && out.contains("assertion"),
        "the assumption is printed: {out}"
    );
}

#[test]
fn the_flag_is_spelled_for_what_it_does() {
    // Renamed from `--spine`: the old spelling claimed a reading.
    let (cmd, _) = parse(&["status".to_string(), "--assume-spine".to_string()]).expect("parses");
    assert!(matches!(cmd, Cmd::Status { spine: true, .. }), "{cmd:?}");
    assert!(USAGE.contains("--assume-spine"));
    assert!(
        !USAGE.contains(" --spine "),
        "the old spelling is gone from the help"
    );
}

// ---------------------------------------------------------------------------
// The account roster (design §5.6, §5.8.10) — GAP 1
// ---------------------------------------------------------------------------

/// A config directory with a `.claude.json` carrying only the non-secret keys
/// this harness reads, plus a token-shaped key it must not.
fn config_dir(env: &Env, name: &str, five: f64, seven: f64, signed_in: bool) -> PathBuf {
    let dir = env.home.as_ref().expect("home").join(name);
    std::fs::create_dir_all(&dir).expect("config dir");
    let account = if signed_in {
        r#""oauthAccount":{"organizationName":"Acme","seatTier":"pro","accessToken":"NEVER-READ"},"#
    } else {
        ""
    };
    std::fs::write(
        dir.join(".claude.json"),
        format!(
            "{{{account}\"cachedUsageUtilization\":{{\"fetchedAtMs\":1758412740000,\
             \"utilization\":{{\"five_hour\":{five},\"seven_day\":{seven}}}}}}}"
        ),
    )
    .expect("write .claude.json");
    dir
}

fn write_roster(env: &Env, text: &str) {
    std::fs::create_dir_all(&env.state).expect("state dir");
    std::fs::write(env.accounts_path(), text).expect("write accounts.toml");
}

#[test]
fn the_accounts_verb_answers_with_no_roster_and_names_how_to_get_one() {
    let tmp = Tmp::new("accounts-empty");
    let env = env_at(&tmp);
    let (ok, out, _) = go(
        &Cmd::Accounts {
            add: None,
            discover: false,
            write: false,
            json: false,
        },
        &env,
    );
    assert!(ok, "an absent roster is not an error");
    assert!(out.contains("rotation OFF"), "{out}");
    assert!(out.contains("accounts discover"), "{out}");
    assert!(
        out.contains("reads no credential"),
        "the credential line is printed: {out}"
    );
}

#[test]
fn the_accounts_verb_names_the_next_account_and_why_every_other_is_out() {
    let tmp = Tmp::new("accounts-list");
    let env = env_at(&tmp);
    // `work` is the session's own account (CLAUDE_CONFIG_DIR is unset, so the
    // default `<home>/.claude` is what is active) and it is at its limit.
    let work = config_dir(&env, ".claude", 100.0, 40.0, true);
    let alt1 = config_dir(&env, ".claude-alt1", 100.0, 2.0, true);
    let alt2 = config_dir(&env, ".claude-alt2", 12.0, 30.0, true);
    write_roster(
        &env,
        &format!(
            "[accounts]\nenabled = true\n\
             [[account]]\nlabel = \"work\"\ndir = {:?}\n\
             [[account]]\nlabel = \"alt-1\"\ndir = {:?}\n\
             [[account]]\nlabel = \"alt-2\"\ndir = {:?}\n",
            work.display().to_string(),
            alt1.display().to_string(),
            alt2.display().to_string()
        ),
    );

    let (ok, out, _) = go(
        &Cmd::Accounts {
            add: None,
            discover: false,
            write: false,
            json: false,
        },
        &env,
    );
    assert!(ok);
    assert!(out.contains("rotation on"), "{out}");
    assert!(out.contains("not a candidate: active"), "{out}");
    assert!(
        out.contains("not a candidate: five-hour-exhausted"),
        "{out}"
    );
    assert!(out.contains("NEXT"), "{out}");
    // The lowest seven-day figure belongs to an EXHAUSTED account, so the
    // §5.6 rule picks the other one — which is the whole point of the rule.
    let next_line = out
        .lines()
        .find(|l| l.contains("NEXT"))
        .expect("one NEXT line");
    assert!(next_line.contains("alt-2"), "{out}");

    let (_, json, _) = go(
        &Cmd::Accounts {
            add: None,
            discover: false,
            write: false,
            json: true,
        },
        &env,
    );
    assert!(json.contains("\"next\":\"alt-2\""), "{json}");
    assert!(json.contains("\"active\":\"work\""), "{json}");
    assert!(
        json.contains("\"why_not\":\"five-hour-exhausted\""),
        "{json}"
    );
    assert!(json.contains("\"tier\":\"pro\""), "{json}");
    assert!(json.contains("\"auth\":\"signed-in\""), "{json}");
    // THE CREDENTIAL LINE: nothing token-shaped reaches the output.
    assert!(!json.contains("NEVER-READ"), "{json}");
    assert!(!json.contains("accessToken"), "{json}");
}

#[test]
fn the_roster_fills_the_watch_config_that_nothing_used_to_write() {
    // GAP 1's actual defect: `WatchConfig::accounts_enabled` and
    // `account_dir` had no writer, so `plan_limits` answered
    // `refused:unresolved` for ever while the table row read as live.
    let tmp = Tmp::new("accounts-wiring");
    let env = env_at(&tmp);
    let alt = config_dir(&env, ".claude-alt1", 3.0, 7.0, true);

    // With no roster the two fields keep their shipped defaults: OFF, and no
    // directory — so a rotation is still refused, and now for a reason.
    let mut cfg = watch::WatchConfig::default();
    apply_accounts(&env, &mut cfg);
    assert!(!cfg.accounts_enabled);
    assert_eq!(cfg.account_dir, None);

    write_roster(
        &env,
        &format!(
            "[accounts]\nenabled = true\n[[account]]\nlabel = \"alt-1\"\ndir = {:?}\n",
            alt.display().to_string()
        ),
    );
    let mut cfg = watch::WatchConfig::default();
    apply_accounts(&env, &mut cfg);
    assert!(cfg.accounts_enabled, "the roster's own consent reaches it");
    assert_eq!(cfg.account_label, "alt-1");
    assert_eq!(cfg.account_dir, Some(alt.display().to_string()));

    // NEGATIVE CONTROL: consent is the ROSTER's word, never implied by a row
    // existing. The same file with `enabled = false` still resolves the
    // label — the engine's own guard is what skips the action.
    write_roster(
        &env,
        &format!(
            "[accounts]\nenabled = false\n[[account]]\nlabel = \"alt-1\"\ndir = {:?}\n",
            alt.display().to_string()
        ),
    );
    let mut cfg = watch::WatchConfig::default();
    apply_accounts(&env, &mut cfg);
    assert!(!cfg.accounts_enabled);

    // NEGATIVE CONTROL: a roster that does not admit writes NOTHING, so a
    // broken file can never turn rotation on.
    write_roster(&env, "[[account]]\nlabel = \"a b\"\ndir = \"/x\"\n");
    let mut cfg = watch::WatchConfig {
        accounts_enabled: false,
        ..watch::WatchConfig::default()
    };
    apply_accounts(&env, &mut cfg);
    assert!(!cfg.accounts_enabled);
    assert_eq!(cfg.account_dir, None);
    let (ok, _, err) = go(
        &Cmd::Accounts {
            add: None,
            discover: false,
            write: false,
            json: false,
        },
        &env,
    );
    assert!(!ok, "a roster that does not admit is an error at the verb");
    assert!(err.contains("accounts.toml"), "{err}");
}

#[test]
fn discovery_proposes_rows_writes_nothing_and_write_leaves_rotation_off() {
    let tmp = Tmp::new("accounts-discover");
    let env = env_at(&tmp);
    config_dir(&env, ".claude", 10.0, 20.0, true);
    config_dir(&env, ".claude-alt1", 1.0, 2.0, true);
    config_dir(&env, ".claude-bare", 0.0, 0.0, false);

    let (ok, out, _) = go(
        &Cmd::Accounts {
            add: None,
            discover: true,
            write: false,
            json: false,
        },
        &env,
    );
    assert!(ok);
    assert!(out.contains("NO credential"), "{out}");
    assert!(out.contains("[[account]]"), "the rows are printed: {out}");
    assert!(out.contains("nothing was written"), "{out}");
    assert!(
        out.contains("claude-bare") && out.contains("unauthenticated"),
        "a dir with no oauthAccount is listed, not hidden: {out}"
    );
    assert!(
        !env.accounts_path().exists(),
        "discovery without --write writes no roster"
    );

    // --write appends, and what it writes has rotation OFF.
    let (ok, out, _) = go(
        &Cmd::Accounts {
            add: None,
            discover: true,
            write: true,
            json: false,
        },
        &env,
    );
    assert!(ok, "{out}");
    let text = std::fs::read_to_string(env.accounts_path()).expect("the roster was written");
    assert!(text.contains("enabled = false"), "{text}");
    let roster = accounts::Roster::load(&env.accounts_path()).expect("it re-admits");
    assert!(!roster.enabled, "consent is never written by discovery");
    assert_eq!(
        roster.rows.len(),
        2,
        "only the signed-in directories become rows: {text}"
    );
    // A second --write proposes nothing new, so it appends nothing.
    let before = text.len();
    let (_, _, _) = go(
        &Cmd::Accounts {
            add: None,
            discover: true,
            write: true,
            json: false,
        },
        &env,
    );
    assert_eq!(
        std::fs::read_to_string(env.accounts_path())
            .expect("roster")
            .len(),
        before,
        "a row already in the roster is not appended twice"
    );

    let (_, json, _) = go(
        &Cmd::Accounts {
            add: None,
            discover: true,
            write: false,
            json: true,
        },
        &env,
    );
    assert!(json.contains("\"kind\":\"accounts-discover\""), "{json}");
    assert!(json.contains("\"already\":true"), "{json}");
    assert!(!json.contains("NEVER-READ"), "{json}");
}

#[test]
fn the_accounts_grammar_refuses_what_it_does_not_know() {
    let words = |s: &str| -> Vec<String> { s.split(' ').map(str::to_string).collect() };
    let (cmd, _) = parse(&words("accounts")).expect("the bare verb parses");
    assert_eq!(
        cmd,
        Cmd::Accounts {
            add: None,
            discover: false,
            write: false,
            json: false
        }
    );
    let (cmd, _) = parse(&words("accounts discover --write")).expect("parses");
    assert_eq!(
        cmd,
        Cmd::Accounts {
            add: None,
            discover: true,
            write: true,
            json: false
        }
    );
    // NEGATIVE CASES.
    assert!(parse(&words("accounts switch")).is_err(), "no such operand");
    assert!(
        parse(&words("accounts --write")).is_err(),
        "--write belongs to discover: the roster is the owner's file"
    );
}

/// F-1, at the front door: without a launch nonce there is nothing to key a
/// `turn` by, so the typed half of the recovery table REFUSES rather than
/// printing a line the server rejects at parse.
#[test]
fn recover_without_a_launch_nonce_refuses_rather_than_printing_a_dead_turn() {
    let tmp = Tmp::new("recover-nonce");
    let mut env = env_at(&tmp);
    env.nonce = String::new();
    seed_exhausted(&env);
    let (ok, out, err) = go(
        &Cmd::Recover {
            what: "switch-model".to_string(),
            target: Some("opus".to_string()),
            json: false,
            commands: true,
            spine: false,
        },
        &env,
    );
    assert!(ok, "{err}");
    assert!(out.contains("refused:unresolved"), "{out}");
    assert!(!out.contains("turn id="), "{out}");

    // The same with the SESSION ID in the field — the spelling the shipped
    // filler used, and not a launch nonce.
    let mut sid_env = env_at(&tmp);
    sid_env.nonce = "s-1e918c4662a1b7b8bd43".to_string();
    let (_, out2, _) = go(
        &Cmd::Recover {
            what: "switch-model".to_string(),
            target: Some("opus".to_string()),
            json: false,
            commands: true,
            spine: false,
        },
        &sid_env,
    );
    assert!(out2.contains("refused:unresolved"), "{out2}");

    // NEGATIVE CONTROL: the injected real nonce prints the fence.
    let (_, out3, _) = go(
        &Cmd::Recover {
            what: "switch-model".to_string(),
            target: Some("opus".to_string()),
            json: false,
            commands: true,
            spine: false,
        },
        &env_at(&tmp),
    );
    assert!(out3.contains("turn id="), "{out3}");
}

/// The `--nonce` flag is validated at PARSE time against the server's own
/// reader, so a wrong one is refused with the roster field named rather than
/// carried into a `turn` that cannot parse.
#[test]
fn the_nonce_flag_is_validated_at_parse_time_against_the_servers_reader() {
    let err = parse(&[
        "status".to_string(),
        "--nonce".to_string(),
        "test-sid".to_string(),
    ])
    .expect_err("a session id is not a launch nonce");
    assert!(err.contains("nonce=<hex32>"), "{err}");
    assert!(
        parse(&[
            "status".to_string(),
            "--nonce".to_string(),
            TEST_NONCE.to_string(),
        ])
        .is_ok()
    );
}

/// Efficiency finding 2, at the verb. `harness mark` is the presence surface
/// design §4.6.1 expects a host to re-assert on every change, and it asked
/// for exact row counts to answer three booleans. `Readout::from_presence`
/// must reach the SAME source, hooks verdict and mark as `Readout::new` —
/// and must leave `rows` empty rather than reporting zeros it never read.
#[test]
fn the_mark_reads_presence_not_counts_and_reaches_the_same_verdict() {
    let tmp = Tmp::new("presence");
    let env = env_at(&tmp);

    // With nothing written: the spine, from either constructor.
    let (counts, seq) = read_counts(&env);
    let (present, pseq) = read_presence(&env);
    assert_eq!(seq, pseq);
    let a = Readout::new(counts, seq);
    let b = Readout::from_presence(&present, pseq);
    assert_eq!(a.source, b.source);
    assert_eq!(a.source, Source::Grid);
    assert_eq!(a.hooks_present, b.hooks_present);
    assert!(b.rows.is_empty(), "counts it never read are absent, not 0");
    assert_eq!(
        presence_of(&a, &env, false).json(),
        presence_of(&b, &env, false).json()
    );

    // After a hook row, and after a statusLine row: the ladder's two other
    // rungs, both reached identically.
    let payload = permission_payload(&env.cwd, "rm -rf build/tmp");
    let id = open_ring(&env, RING_RM).map_or(0, |r| r.next_id());
    let reply = hook_reply("PermissionRequest", &payload, Some("rm-approve"), &env, id);
    append_row(&env, RING_RM, &reply.rm_row.expect("row")).expect("append");
    for expect in [Source::Hook, Source::StatusLine] {
        if expect == Source::StatusLine {
            append_row(&env, RING_STATUSLINE, "{\"line\":\"x\"}").expect("append");
        }
        let (counts, seq) = read_counts(&env);
        let (present, pseq) = read_presence(&env);
        let a = Readout::new(counts, seq);
        let b = Readout::from_presence(&present, pseq);
        assert_eq!(a.source, expect, "{a:?}");
        assert_eq!(a.source, b.source);
        assert!(a.hooks_present && b.hooks_present);
        assert_eq!(
            presence_of(&a, &env, true).json(),
            presence_of(&b, &env, true).json()
        );
    }
}

/// Plan item 4c / wrapper §6.1: two installers write `~/.claude/settings.json`
/// and each strips only its own entries, so a merge computed from bytes
/// another writer has since replaced must never be renamed over theirs. The
/// guarded write re-reads just before the rename: a change leaves the other
/// writer's bytes in place, removes the temporary, and answers `false` so
/// `install`/`uninstall` read and merge again; an unchanged file is replaced.
#[test]
fn a_settings_write_never_lands_over_bytes_another_writer_put_there() {
    let tmp = Tmp::new("settings-reread");
    let path = tmp.path().join("settings.json");
    std::fs::write(&path, "{\"a\":1}\n").expect("seed");
    let original = std::fs::read_to_string(&path).ok();

    // Another writer (the primer's aterm-link block) lands between our read
    // and our rename.
    let wrote = write_atomic_if(&path, "{\"ours\":1}\n", 0o600, || {
        std::fs::write(&path, "{\"a\":1,\"link\":2}\n").expect("the other writer");
        settings_unchanged(&path, original.as_deref())
    })
    .expect("no io error");
    assert!(!wrote, "a changed file is not replaced");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "{\"a\":1,\"link\":2}\n",
        "the other writer's bytes stand"
    );
    let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
        .expect("dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "the temporary is removed: {leftovers:?}"
    );

    // The negative control: nothing moved, so the write lands.
    let original = std::fs::read_to_string(&path).ok();
    let wrote = write_atomic_if(&path, "{\"ours\":1}\n", 0o600, || {
        settings_unchanged(&path, original.as_deref())
    })
    .expect("no io error");
    assert!(wrote);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "{\"ours\":1}\n"
    );

    // An absent file read as absent is unchanged; one that appeared is not.
    let fresh = tmp.path().join("fresh.json");
    assert!(settings_unchanged(&fresh, None));
    std::fs::write(&fresh, "{}").expect("appear");
    assert!(!settings_unchanged(&fresh, None));
    assert_eq!(SETTINGS_TRIES, 3);
}

// ---------------------------------------------------------------------------
// The MANUAL PATHS of design §5.7 — the verbs a human drives by hand
// ---------------------------------------------------------------------------

#[test]
fn the_grammar_carries_every_manual_verb_and_refuses_the_near_misses() {
    let cases: &[(&[&str], Cmd)] = &[
        (
            &["switch", "model", "opus"],
            Cmd::Switch {
                kind: watch::SwitchKind::Model,
                target: "opus".to_string(),
                json: false,
            },
        ),
        (
            &["switch", "account", "alt-1", "--json"],
            Cmd::Switch {
                kind: watch::SwitchKind::Account,
                target: "alt-1".to_string(),
                json: true,
            },
        ),
        (
            &["watch", "--passes", "3"],
            Cmd::Watch {
                passes: Some(3),
                json: false,
            },
        ),
        (
            &["watch"],
            Cmd::Watch {
                passes: None,
                json: false,
            },
        ),
        (
            &["config", "get"],
            Cmd::Config {
                key: String::new(),
                value: None,
                json: false,
            },
        ),
        (
            &["config", "get", "cap.limits.level"],
            Cmd::Config {
                key: "cap.limits.level".to_string(),
                value: None,
                json: false,
            },
        ),
        (
            // The value is the REST of the line, so an array that the shell
            // split into words is still one value.
            &[
                "config",
                "set",
                "cap.limits.actions.auth",
                "relogin",
                "escalate",
            ],
            Cmd::Config {
                key: "cap.limits.actions.auth".to_string(),
                value: Some("relogin escalate".to_string()),
                json: false,
            },
        ),
        (
            &["accounts", "add", "alt-1", "/tmp/alt-1"],
            Cmd::Accounts {
                add: Some(("alt-1".to_string(), PathBuf::from("/tmp/alt-1"))),
                discover: false,
                write: false,
                json: false,
            },
        ),
    ];
    for (args, want) in cases {
        let argv: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        let (got, _over) = parse(&argv).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        assert_eq!(&got, want, "{args:?}");
    }
    // NEGATIVE CONTROLS: every operand that must be present, and every word
    // outside a closed set.
    for bad in [
        vec!["switch"],
        vec!["switch", "cluster", "x"],
        vec!["switch", "model"],
        vec!["config"],
        vec!["config", "unset", "cap.limits.level"],
        vec!["config", "set", "cap.limits.level"],
        vec!["accounts", "add"],
        vec!["accounts", "add", "alt-1"],
        vec!["accounts", "sync"],
        vec!["watch", "--passes", "soon"],
    ] {
        let argv: Vec<String> = bad.iter().map(|a| (*a).to_string()).collect();
        assert!(parse(&argv).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn config_get_lists_every_key_and_set_writes_one_and_re_reads_it() {
    let tmp = Tmp::new("config");
    let env = env_at(&tmp);
    let (ok, out, err) = go(
        &Cmd::Config {
            key: String::new(),
            value: None,
            json: false,
        },
        &env,
    );
    assert!(ok, "{err}");
    for key in config::key_names() {
        assert!(out.contains(&key), "{key} is missing from `config get`");
    }
    // A write lands, re-reads and survives a fresh load.
    let (ok, out, err) = go(
        &Cmd::Config {
            key: "cap.limits.level".to_string(),
            value: Some("4".to_string()),
            json: false,
        },
        &env,
    );
    assert!(ok, "{err}");
    assert!(out.contains("cap.limits.level = 4"), "{out}");
    let back = config::HarnessConfig::load(&env.harness_config_path()).expect("re-read");
    assert_eq!(back.limits.level, 4);
    // And the watcher every read verb builds now USES it, which is the whole
    // point of the file: before this, `config set` would have written a value
    // nothing read.
    let (w, _source, _hooks) = seeded_watcher(&env);
    assert_eq!(w.limits_config().level, 4);
}

#[test]
fn config_set_refuses_the_five_eight_seven_tables_and_leaves_the_file_alone() {
    let tmp = Tmp::new("config-refuse");
    let env = env_at(&tmp);
    // Establish a file first, so "unchanged" is a real assertion about bytes.
    let (ok, _out, err) = go(
        &Cmd::Config {
            key: "cap.limits.level".to_string(),
            value: Some("4".to_string()),
            json: false,
        },
        &env,
    );
    assert!(ok, "{err}");
    let before = std::fs::read_to_string(env.harness_config_path()).expect("the file");
    for (key, value) in [
        (
            "cap.limits.actions.network-offline",
            "switch-model,escalate",
        ),
        ("cap.limits.actions.unknown", "switch-account,escalate"),
        ("cap.limits.actions.unknown", "retry,escalate"),
        (
            "cap.limits.actions.model-bucket-limit",
            "switch-model,escalate",
        ),
        ("cap.limits.actions.auth", "wait,escalate"),
        ("cap.limits.actions.spend-billing", "retry,escalate"),
        ("cap.limits.no_such_key", "1"),
    ] {
        let (ok, _out, err) = go(
            &Cmd::Config {
                key: key.to_string(),
                value: Some(value.to_string()),
                json: false,
            },
            &env,
        );
        assert!(!ok, "{key} = {value} must be refused");
        assert!(err.contains(key), "{key}: {err}");
        let now = std::fs::read_to_string(env.harness_config_path()).expect("the file");
        assert_eq!(now, before, "a refused set rewrote {key}");
    }
    // POSITIVE CONTROL: the reorder design §5.8.7 names as the whole point of
    // the verb does land.
    let (ok, out, err) = go(
        &Cmd::Config {
            key: "cap.limits.actions.session-5h-limit".to_string(),
            value: Some("[\"wait\",\"escalate\"]".to_string()),
            json: false,
        },
        &env,
    );
    assert!(ok, "{err}");
    assert!(out.contains("\"wait\""), "{out}");
    assert_ne!(
        std::fs::read_to_string(env.harness_config_path()).expect("the file"),
        before
    );
}

#[test]
fn accounts_add_writes_one_row_leaves_rotation_off_and_refuses_a_duplicate() {
    let tmp = Tmp::new("accounts-add");
    let env = env_at(&tmp);
    let dir = tmp.path().join("home").join(".claude-alt");
    std::fs::create_dir_all(&dir).expect("the config dir");
    let add = |label: &str, dir: &Path| Cmd::Accounts {
        add: Some((label.to_string(), dir.to_path_buf())),
        discover: false,
        write: false,
        json: false,
    };
    let (ok, out, err) = go(&add("alt-1", &dir), &env);
    assert!(ok, "{err}");
    assert!(out.contains("rotation OFF"), "{out}");
    let roster = accounts::Roster::load(&env.accounts_path()).expect("the roster admits");
    assert_eq!(roster.rows.len(), 1);
    assert_eq!(roster.rows[0].label, "alt-1");
    assert_eq!(roster.rows[0].kind, accounts::Kind::ConfigDir);
    assert!(
        !roster.enabled,
        "adding a row must never be read as consent to rotate"
    );
    // NEGATIVE CONTROLS: the same label, the same directory, a relative
    // path, a label outside the row grammar, and a directory that is not one.
    let second = tmp.path().join("home").join(".claude-two");
    std::fs::create_dir_all(&second).expect("a second dir");
    for (label, d) in [
        ("alt-1", second.clone()),
        ("alt-2", dir.clone()),
        ("alt-3", PathBuf::from("relative/path")),
        ("alt 4", second.clone()),
        ("alt-5", tmp.path().join("home").join("nothing-here")),
    ] {
        let (ok, _out, err) = go(&add(label, &d), &env);
        assert!(!ok, "{label} {} must be refused", d.display());
        assert!(!err.is_empty(), "{label}: a refusal with no reason");
    }
    let roster = accounts::Roster::load(&env.accounts_path()).expect("still admits");
    assert_eq!(roster.rows.len(), 1, "a refused add wrote a row");
    // POSITIVE CONTROL: a second, valid row does land.
    let (ok, _out, err) = go(&add("alt-2", &second), &env);
    assert!(ok, "{err}");
    assert_eq!(
        accounts::Roster::load(&env.accounts_path())
            .expect("admits")
            .rows
            .len(),
        2
    );
}

#[test]
fn switch_and_watch_refuse_unresolved_when_no_control_socket_answers() {
    let tmp = Tmp::new("no-socket");
    let mut env = env_at(&tmp);
    // A path nothing is bound to: the two ACTING verbs must say so rather
    // than report a decision as though they had acted.
    env.sock = Some(tmp.path().join("nothing.sock").display().to_string());
    let (ok, out, err) = go(
        &Cmd::Switch {
            kind: watch::SwitchKind::Model,
            target: "opus".to_string(),
            json: false,
        },
        &env,
    );
    assert!(!ok, "a switch with no socket must not exit 0");
    assert!(out.contains("verdict=refused:unresolved"), "{out}{err}");
    assert!(out.contains("id=0"), "{out}");
    let (ok, _out, err) = go(
        &Cmd::Watch {
            passes: Some(1),
            json: false,
        },
        &env,
    );
    assert!(!ok, "a watch with no socket must not exit 0");
    assert!(err.contains("aterm harness:"), "{err}");
}

#[test]
fn a_switch_refuses_before_it_acts_on_every_gate_the_loop_has() {
    // The DECISION half, driven directly: `run_switch_now` needs a socket,
    // but `Watcher::switch` is the thing that decides and it is pure.
    let cfg = watch::WatchConfig {
        nonce: TEST_NONCE.to_string(),
        accounts_enabled: true,
        account_label: "alt-1".to_string(),
        account_dir: Some("/tmp/alt-1".to_string()),
        ..watch::WatchConfig::default()
    };
    let limits_cfg = limits::LimitsConfig {
        level: 4,
        ..limits::LimitsConfig::default()
    };
    let w = Watcher::new("s-1", cfg, limits_cfg);
    let live = HostGuards {
        hold: false,
        busy: false,
        custody_user: false,
        generation: w.generation(),
        engaged: true,
    };
    // POSITIVE CONTROL first: a model switch plans an act under live guards.
    assert!(
        matches!(
            w.switch(watch::SwitchKind::Model, &live, 1_000),
            Plan::Act(_)
        ),
        "a model switch must plan under clean guards"
    );
    // And the ACCOUNT kind refuses by its own name under exactly the same
    // clean guards, because no control verb carries an environment into a new
    // session (`watch::Act::Relaunch` has no line). It must NOT read as
    // `refused:unresolved`: that word means "the roster named no candidate",
    // and this roster names one.
    match w.switch(watch::SwitchKind::Account, &live, 1_000) {
        Plan::Refused { why, .. } => assert_eq!(why.as_str(), "refused:unsupported"),
        other => panic!("an account rotation planned {other:?}"),
    }
    // Each gate, one at a time, with its spelling.
    let cases: &[(HostGuards, &str)] = &[
        (HostGuards { hold: true, ..live }, "refused:hold"),
        (HostGuards { busy: true, ..live }, "refused:busy"),
        (
            HostGuards {
                custody_user: true,
                ..live
            },
            "refused:custody",
        ),
        (
            HostGuards {
                engaged: false,
                ..live
            },
            "refused:bypassed",
        ),
        (
            HostGuards {
                generation: live.generation.wrapping_add(7),
                ..live
            },
            "refused:generation",
        ),
    ];
    for (host, want) in cases {
        match w.switch(watch::SwitchKind::Model, host, 1_000) {
            Plan::Refused { why, .. } => assert_eq!(why.as_str(), *want, "{host:?}"),
            other => panic!("{host:?} planned {other:?} instead of {want}"),
        }
    }
}

#[test]
fn a_switch_without_a_launch_nonce_refuses_rather_than_typing_a_line_that_dies_at_parse() {
    // The last fence: a typed act needs a real launch nonce. The default
    // config carries none.
    let limits_cfg = limits::LimitsConfig {
        level: 4,
        ..limits::LimitsConfig::default()
    };
    let w = Watcher::new("s-1", watch::WatchConfig::default(), limits_cfg);
    let live = HostGuards {
        hold: false,
        busy: false,
        custody_user: false,
        generation: w.generation(),
        engaged: true,
    };
    match w.switch(watch::SwitchKind::Model, &live, 1_000) {
        Plan::Refused { why, .. } => assert_eq!(why.as_str(), "refused:unresolved"),
        other => panic!("a nonce-less switch planned {other:?}"),
    }
    // NEGATIVE CONTROL: an account switch types nothing, so the nonce fence
    // is not what stops it — it is refused EARLIER and by a different word,
    // which is how the two failures stay distinguishable in the ledger.
    let cfg = watch::WatchConfig {
        accounts_enabled: true,
        account_dir: Some("/tmp/alt-1".to_string()),
        ..watch::WatchConfig::default()
    };
    let limits_cfg = limits::LimitsConfig {
        level: 4,
        ..limits::LimitsConfig::default()
    };
    let w = Watcher::new("s-1", cfg, limits_cfg);
    match w.switch(watch::SwitchKind::Account, &live, 1_000) {
        Plan::Refused { why, .. } => assert_eq!(why.as_str(), "refused:unsupported"),
        other => panic!("an account rotation planned {other:?}"),
    }
}

#[test]
fn the_two_actuator_ledgers_are_readable_through_the_ledger_verb() {
    let tmp = Tmp::new("actuator-ledger");
    let env = env_at(&tmp);
    for name in [RING_RECOVERY, RING_ACTUATION] {
        assert!(ring_config(name).is_some(), "{name} answers no ring");
        append_row(&env, name, "{\"kind\":\"probe\"}").expect("append");
        let (ok, out, err) = go(
            &Cmd::Ledger {
                name: name.to_string(),
                count: 5,
                since: 0,
                json: false,
            },
            &env,
        );
        assert!(ok, "{name}: {err}");
        assert!(out.contains("probe"), "{name}: {out}");
    }
    // NEGATIVE CONTROL: a ring name nothing answers to is still refused.
    let argv: Vec<String> = ["ledger", "no-such-ring"]
        .iter()
        .map(|a| (*a).to_string())
        .collect();
    assert!(parse(&argv).is_err());
}

/// `hooks=` IS ABOUT THIS SESSION, and has three answers.
///
/// The state directory is one per MACHINE, and the old test was "does the
/// event ring hold any row at all" — no sid, no bound. So one hook that fired
/// in ANOTHER session weeks ago made this session report `hooks=present`, and
/// `stopped_short_available` then claimed a capability that cannot work here;
/// a `--bare` session inherited the same false positive.
#[test]
fn hooks_are_reported_for_this_session_and_never_for_the_machine() {
    let tmp = Tmp::new("hooks-sid");
    let env = env_at(&tmp);
    // A row from a DIFFERENT session, which is what a shared state directory
    // is full of.
    append_row(
        &env,
        RING_EVENT,
        r#"{"id":1,"event":"PreToolUse","sid":"some-other-session"}"#,
    )
    .expect("append");
    let (_, out, _) = go(&Cmd::Liveness { json: false }, &env);
    assert!(
        out.contains("hooks=absent"),
        "another session's hook is not this session's channel: {out}"
    );
    assert!(out.contains("stopped-short: unavailable"), "{out}");

    // POSITIVE CONTROL: a row carrying THIS session's sid is the channel.
    append_row(
        &env,
        RING_EVENT,
        r#"{"id":2,"event":"PreToolUse","sid":"test-sid"}"#,
    )
    .expect("append");
    let (_, out, _) = go(&Cmd::Liveness { json: false }, &env);
    assert!(out.contains("hooks=present"), "{out}");
    assert!(!out.contains("stopped-short: unavailable"), "{out}");

    // THE THIRD ANSWER: a process that does not know its own sid cannot
    // attribute a row either way, and `absent` would read as a measurement.
    let mut anon = env_at(&tmp);
    anon.sid = String::new();
    let (_, out, _) = go(&Cmd::Liveness { json: true }, &anon);
    assert!(out.contains("\"hooks\":\"unknown\""), "{out}");
    // And `unknown` breaks to the safe answer for a hook-only capability.
    assert!(
        out.contains("\"stopped_short\":\"unavailable (hooks=unknown)\""),
        "{out}"
    );
}

/// THE SOCKET-FREE PLANNERS SAY THEIR GUARDS ARE ASSUMPTIONS, and withhold
/// the sendable lines of an act that TYPES.
///
/// `planning_guards` returns `hold: false, busy: false, custody_user: false`
/// and nothing measured any of them; neither verb reads the spine, so
/// `Watcher::prompt` — the approval-box fence — is at its `false` default.
/// `run_switch_now` gets this right (it reads the spine "so the approval-box
/// fence has something to fence on"); these two asserted the all-clear and
/// printed L3 lines under it.
#[test]
fn a_socket_free_planner_names_its_assumptions_and_withholds_a_typing_act() {
    let tmp = Tmp::new("guards-note");
    let env = env_at(&tmp);
    seed_exhausted(&env);
    let (ok, out, err) = go(
        &Cmd::Recover {
            what: "switch-model".to_string(),
            target: Some("opus".to_string()),
            json: false,
            commands: true,
            spine: false,
        },
        &env,
    );
    assert!(ok, "{err}");
    assert!(out.contains("no spine was read"), "{out}");
    assert!(out.contains("--commands withheld"), "{out}");
    assert!(
        !out.lines().any(|l| l.starts_with("turn id=")),
        "no SENDABLE L3 line under unmeasured guards: {out}"
    );
    // The DECISION is still printed: saying what the step would be costs
    // nothing, and withholding it would hide the table.
    assert!(out.contains("action=switch-model"), "{out}");

    // POSITIVE CONTROL: an L1 plan types nothing, so its lines are printed
    // under the same guards — the refusal is about typing, not about socket
    // freedom.
    let (_, out, _) = go(
        &Cmd::Nudge {
            sid: String::new(),
            level: watch::Level::Warn,
            json: false,
            commands: true,
            spine: false,
        },
        &env,
    );
    assert!(out.contains("no spine was read"), "{out}");
    assert!(!out.contains("--commands withheld"), "{out}");
}

/// ALL THREE `[disk]` KNOBS ARE READ, through the one TOML reader this module
/// already had.
///
/// `disk_config` read only `disk.apply` and carried a comment saying "this
/// file has no TOML number reader", while `align::read_toml_dotted` sat in
/// the same module tree returning `Val::Int` and USAGE advertised
/// `disk.target_stale_days` as if something read it.
#[test]
fn the_numeric_disk_knobs_are_read_from_aterm_toml() {
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

    // NEGATIVE CONTROLS, each breaking to the shipped default rather than to
    // a guess: a value of the wrong TYPE, a NEGATIVE stale window (which
    // would make every directory a candidate at once), and a file the
    // grammar refuses outright.
    let d = disk::Config::default();
    for text in [
        "[disk]\nwarn_free_gib = \"lots\"\ntarget_stale_days = -1\n",
        "[disk]\nwarn_free_gib = 5\ntarget_stale_days\n",
    ] {
        std::fs::write(&path, text).expect("write");
        let cfg = disk_config(&env);
        assert_eq!(cfg.target_stale_days, d.target_stale_days, "{text}");
    }
    std::fs::write(&path, "[disk]\nwarn_free_gib = \"lots\"\n").expect("write");
    assert_eq!(disk_config(&env).warn_free_gib, d.warn_free_gib);
}
