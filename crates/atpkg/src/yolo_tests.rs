// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// The roster: every vendor row spells its yolo posture in full — never the word itself as
/// a flag (claude and agy refuse it), every flag and marker safe to embed, its own flags
/// among its markers, and the exact expansions from each vendor's `--help`.
#[test]
fn every_row_spells_its_yolo_policy_in_full() {
    assert_eq!(flags_of("claude"), &["--dangerously-skip-permissions"]);
    assert_eq!(flags_of("agy"), &["--dangerously-skip-permissions"]);
    assert_eq!(
        flags_of("codex"),
        &["--dangerously-bypass-approvals-and-sandbox"]
    );
    assert_eq!(
        flags_of("muse"),
        &[
            "--disable-approval",
            "--disable-sandbox",
            "--trust-workspace",
            "--disable-request-user-input",
        ]
    );
    assert_eq!(flags_of("grok"), &["--always-approve"]);
    assert_eq!(flags_of("kimi"), &["--auto"]);
    assert_eq!(policy_of("grok").after, &["agent"]);
    assert!(policy_of("kimi").after.is_empty());
    assert_eq!(policy_of("muse").after, &["exec", "resume"]);
    assert!(policy_of("muse").skip.contains(&"skills"));
    assert_eq!(policy_of("gemini"), Policy::NONE);
    for row in crate::vendor_direct::VENDORS {
        let p = policy_of(row.program);
        assert!(!p.flags.is_empty(), "{}", row.program);
        assert!(!p.markers.is_empty(), "{}", row.program);
        for flag in p.flags {
            assert!(is_shell_safe_flag(flag), "{} {flag}", row.program);
            assert_ne!(*flag, WORD, "{}", row.program);
            assert!(p.markers.contains(flag), "{} {flag}", row.program);
        }
        for marker in p.markers {
            assert!(is_shell_safe_marker(marker), "{} {marker}", row.program);
        }
        for own in word_options(row.program).own_yolo {
            assert!(p.markers.contains(own), "{} {own}", row.program);
            assert!(!p.flags.contains(own), "{} {own}", row.program);
        }
        for word in p.after.iter().chain(p.skip.iter()) {
            assert!(
                crate::selfupdate::is_shell_safe_verb(word),
                "{} {word}",
                row.program
            );
        }
    }
    assert_eq!(policy_of("ay"), Policy::NONE);
    assert_eq!(policy_of("nosuch"), Policy::NONE);
}

/// The word rules: long flags, short flags and `=` prefixes, nothing else.
#[test]
fn the_word_rules_admit_flags_and_markers_only() {
    for good in ["--dangerously-skip-permissions", "--a", "--a1-b2"] {
        assert!(is_shell_safe_flag(good), "{good}");
    }
    for bad in [
        "", "-y", "--", "--yolo=1", "--Yolo", "--yo'lo", "--yo lo", "yolo",
    ] {
        assert!(!is_shell_safe_flag(bad), "{bad:?}");
    }
    for good in [
        "-s",
        "-a",
        "--sandbox",
        "--sandbox=",
        "--permission-mode=",
        "--allowedTools",
    ] {
        assert!(is_shell_safe_marker(good), "{good}");
    }
    for bad in [
        "",
        "-",
        "-ss",
        "--sandbox==",
        "--sand box",
        "sandbox=",
        "--=x",
    ] {
        assert!(!is_shell_safe_marker(bad), "{bad:?}");
    }
    assert!(marker_matches("--sandbox=", "--sandbox=read-only"));
    assert!(!marker_matches("--sandbox=", "--sandbox"));
    assert!(!marker_matches("--sandbox=", "--sandboxed=1"));
    assert!(marker_matches("--sandbox", "--sandbox"));
    assert!(marker_matches("--sandbox", "--sandbox=read-only"));
    assert!(!marker_matches("--sandbox", "--sandboxed=1"));
    for token in ["-s", "-sread-only", "-s=read-only"] {
        assert!(marker_matches("-s", token), "{token}");
    }
    for token in ["-a", "-anever", "-a=never"] {
        assert!(marker_matches("-a", token), "{token}");
    }
}

/// By the flag: in place, every occurrence, only ahead of `--`, never a
/// flag twice, and behind muse's session words wherever `--yolo` stood.
#[test]
fn the_flag_expands_in_place_and_never_twice() {
    assert_eq!(
        decide("claude", &argv(&["--yolo"])),
        Launch::Flag(argv(&["--dangerously-skip-permissions"]))
    );
    assert_eq!(
        decide("claude", &argv(&["--model", "opus", "--yolo", "-p", "hi"])),
        Launch::Flag(argv(&[
            "--model",
            "opus",
            "--dangerously-skip-permissions",
            "-p",
            "hi"
        ]))
    );
    assert_eq!(
        decide(
            "codex",
            &argv(&["--dangerously-bypass-approvals-and-sandbox", "--yolo"])
        ),
        Launch::Flag(argv(&["--dangerously-bypass-approvals-and-sandbox"]))
    );
    assert_eq!(
        decide("codex", &argv(&["--yolo", "--yolo", "exec", "x"])),
        Launch::Flag(argv(&[
            "--dangerously-bypass-approvals-and-sandbox",
            "exec",
            "x"
        ]))
    );
    let muse_exec = Launch::Flag(argv(&[
        "exec",
        "--disable-approval",
        "--disable-sandbox",
        "--trust-workspace",
        "--disable-request-user-input",
        "x",
    ]));
    assert_eq!(decide("muse", &argv(&["exec", "--yolo", "x"])), muse_exec);
    assert_eq!(decide("muse", &argv(&["--yolo", "exec", "x"])), muse_exec);
    assert_eq!(
        decide("muse", &argv(&["--yolo", "exec", "--yolo", "x"])),
        muse_exec
    );
    assert_eq!(
        decide("muse", &argv(&["--disable-sandbox", "--yolo"])),
        Launch::Flag(argv(&[
            "--disable-sandbox",
            "--disable-approval",
            "--trust-workspace",
            "--disable-request-user-input",
        ]))
    );
    assert_eq!(
        decide("claude", &argv(&["--yolo", "--", "--yolo"])),
        Launch::Flag(argv(&["--dangerously-skip-permissions", "--", "--yolo"]))
    );
    for args in [
        argv(&["--yolo", "--model", "exec", "exec", "x"]),
        argv(&["--model", "exec", "--yolo", "exec", "x"]),
    ] {
        let mut expected = argv(&["--model", "exec", "exec"]);
        expected.extend(flags_of("muse").iter().map(|f| (*f).to_string()));
        expected.push("x".to_string());
        assert_eq!(decide("muse", &args), Launch::Flag(expected));
    }
    assert_eq!(
        decide(
            "muse",
            &argv(&["--new-option", "value", "--yolo", "exec", "x"])
        ),
        Launch::AsTyped
    );
    assert_eq!(
        decide("codex", &argv(&["--new-option", "value", "--yolo"])),
        Launch::Flag(argv(&[
            "--new-option",
            "value",
            "--dangerously-bypass-approvals-and-sandbox"
        ]))
    );
}

/// `codex-old` is OpenAI's `codex` under its own name (its row's alias):
/// it takes codex's options, so every launch decides exactly as codex's —
/// a known option's value is stepped over to the word behind it.
#[test]
fn an_aliased_program_decides_as_the_cli_it_runs() {
    assert_eq!(word_options("codex-old"), word_options("codex"));
    for args in [
        argv(&[]),
        argv(&["exec", "fix it"]),
        argv(&["--model", "opus", "exec", "x"]),
        argv(&["-m", "opus", "resume", "thread-id"]),
        argv(&["--oss", "exec", "x"]),
        argv(&["-s", "read-only"]),
        argv(&["--yolo", "exec", "x"]),
        argv(&["resume", "thread-id"]),
    ] {
        assert_eq!(
            decide("codex-old", &args),
            decide("codex", &args),
            "{args:?}"
        );
    }
    let stepped = argv(&["--model", "opus", "exec", "x"]);
    assert!(
        matches!(decide("codex-old", &stepped), Launch::Default(_)),
        "PRECONDITION: the value is stepped over"
    );
    assert_eq!(word_options("codex-older"), WordOptions::default());
}

#[test]
fn codex_permission_configs_keep_the_typed_posture() {
    for program in ["codex", "codex-old"] {
        for args in [
            argv(&["-c", "sandbox_mode=\"read-only\""]),
            argv(&["--config", "approval_policy=\"on-request\""]),
            argv(&["--config=sandbox_mode=\"workspace-write\""]),
            argv(&["-csandbox_mode=\"read-only\""]),
            argv(&["-c=approval_policy=\"never\""]),
            argv(&["-c", " \tapproval_policy \t=\"on-request\""]),
            argv(&[
                "resume",
                "thread-id",
                "--config",
                "sandbox_mode=\"read-only\"",
            ]),
            argv(&[
                "--model",
                "opus",
                "-c",
                "approval_policy=\"never\"",
                "fork",
                "thread-id",
            ]),
            argv(&[
                "-c",
                "model=opus",
                "--config=approval_policy=\"on-request\"",
            ]),
        ] {
            assert_eq!(
                decide(program, &args),
                Launch::AsTyped,
                "{program} {args:?}"
            );
            let mut explicit = argv(&["--yolo"]);
            explicit.extend(args.iter().cloned());
            let mut expected = argv(&["--dangerously-bypass-approvals-and-sandbox"]);
            expected.extend(args);
            assert_eq!(decide(program, &explicit), Launch::Flag(expected));
        }
        for args in [
            argv(&["-c", "model=opus", "resume", "thread-id"]),
            argv(&[
                "--config=model=\"sandbox_mode=read-only\"",
                "fork",
                "thread-id",
            ]),
            argv(&["--", "--config=sandbox_mode=read-only"]),
        ] {
            let mut expected = argv(&["--dangerously-bypass-approvals-and-sandbox"]);
            expected.extend(args.iter().cloned());
            assert_eq!(decide(program, &args), Launch::Default(expected));
        }
    }
}

/// By default: injected at the front (behind muse's session words), and
/// never over a decided posture, a skipped word, or past `--`-only noise.
#[test]
fn the_default_injects_unless_the_posture_is_decided() {
    assert_eq!(
        decide("claude", &argv(&[])),
        Launch::Default(argv(&["--dangerously-skip-permissions"]))
    );
    assert_eq!(
        decide("claude", &argv(&["-p", "hello"])),
        Launch::Default(argv(&["--dangerously-skip-permissions", "-p", "hello"]))
    );
    assert_eq!(
        decide("codex", &argv(&["exec", "fix it"])),
        Launch::Default(argv(&[
            "--dangerously-bypass-approvals-and-sandbox",
            "exec",
            "fix it"
        ]))
    );
    assert_eq!(
        decide("muse", &argv(&["exec", "fix it"])),
        Launch::Default(argv(&[
            "exec",
            "--disable-approval",
            "--disable-sandbox",
            "--trust-workspace",
            "--disable-request-user-input",
            "fix it"
        ]))
    );
    assert_eq!(
        decide("muse", &argv(&["--", "--yolo"])),
        Launch::Default(argv(&[
            "--disable-approval",
            "--disable-sandbox",
            "--trust-workspace",
            "--disable-request-user-input",
            "--",
            "--yolo"
        ]))
    );
    for (program, args) in [
        ("claude", argv(&["--permission-mode", "plan"])),
        ("claude", argv(&["--permission-mode=bypassPermissions"])),
        (
            "claude",
            argv(&["--allow-dangerously-skip-permissions", "--resume", "x"]),
        ),
        ("claude", argv(&["--dangerously-skip-permissions"])),
        ("claude", argv(&["--restricted"])),
        ("claude", argv(&["--permission-prompts", "none"])),
        ("claude", argv(&["--allowedTools", "Read"])),
        ("claude", argv(&["--disallowed-tools=Bash"])),
        ("claude", argv(&["--settings", "team.json"])),
        ("claude", argv(&["--resume", "session-id"])),
        ("codex", argv(&["-s", "read-only"])),
        ("codex", argv(&["resume", "--not-so-yolo", "thread-id"])),
        ("codex", argv(&["-sread-only"])),
        ("codex", argv(&["-s=workspace-write", "exec", "x"])),
        ("codex", argv(&["--sandbox=workspace-write", "exec", "x"])),
        ("codex", argv(&["-a", "on-request"])),
        ("codex", argv(&["-a=never"])),
        (
            "codex",
            argv(&["--new-option", "value", "resume", "thread-id"]),
        ),
        (
            "codex",
            argv(&["--new-option=value", "resume", "thread-id"]),
        ),
        (
            "codex",
            argv(&["--image", "one.png", "two.png", "resume", "thread-id"]),
        ),
        ("codex", argv(&["--model"])),
        ("codex", argv(&["--model", "--", "resume", "thread-id"])),
        ("agy", argv(&["--mode", "plan"])),
        ("agy", argv(&["--sandbox"])),
        ("muse", argv(&["--approval-mode", "on-request"])),
        ("muse", argv(&["--sandbox-network", "restricted"])),
        ("muse", argv(&["--disable-write"])),
        ("muse", argv(&["-c", "approval_mode=\"on-request\""])),
        ("muse", argv(&["resume", "session-id"])),
        ("muse", argv(&["skills", "list"])),
        ("muse", argv(&["usage"])),
        ("muse", argv(&["auth", "login"])),
        ("muse", argv(&["exec", "--disable-approval", "x"])),
        ("claude", argv(&["update"])),
        ("claude", argv(&["update", "--help"])),
        ("claude", argv(&["install", "latest"])),
        ("codex", argv(&["update"])),
        ("ay", argv(&[])),
        ("nosuch", argv(&["--yolo"])),
    ] {
        assert_eq!(
            decide(program, &args),
            Launch::AsTyped,
            "{program} {args:?}"
        );
    }
    for (_, args) in [
        ("codex", argv(&["--profile", "review"])),
        ("codex", argv(&["resume", "thread-id"])),
        ("codex", argv(&["fork", "thread-id"])),
        ("codex", argv(&["--model", "opus", "resume", "thread-id"])),
        ("codex", argv(&["--model=opus", "resume", "thread-id"])),
        ("codex", argv(&["-m", "opus", "fork", "thread-id"])),
        ("codex", argv(&["-mopus", "fork", "thread-id"])),
        ("codex", argv(&["-m=opus", "resume", "thread-id"])),
        ("codex", argv(&["--cd", "/owned/path", "fork", "thread-id"])),
        ("codex", argv(&["--cd=/owned/path", "resume", "thread-id"])),
        ("codex", argv(&["-C", "/owned/path", "resume", "thread-id"])),
        ("codex", argv(&["-C/owned/path", "fork", "thread-id"])),
        (
            "codex",
            argv(&[
                "--model",
                "opus",
                "--cd",
                "/owned/path",
                "resume",
                "thread-id",
            ]),
        ),
    ] {
        let mut expected = argv(&["--dangerously-bypass-approvals-and-sandbox"]);
        expected.extend(args.iter().cloned());
        assert_eq!(decide("codex", &args), Launch::Default(expected));
    }
    for args in [
        argv(&["--model", "resume", "fix this"]),
        argv(&["--model=resume", "fix this"]),
        argv(&["--", "resume", "thread-id"]),
        argv(&["explain resume and fork"]),
    ] {
        let mut expected = argv(&["--dangerously-bypass-approvals-and-sandbox"]);
        expected.extend(args.iter().cloned());
        assert_eq!(decide("codex", &args), Launch::Default(expected));
    }
    for args in [
        argv(&["--model", "exec", "exec", "x"]),
        argv(&["--model=exec", "exec", "x"]),
    ] {
        let word = args.len() - 2;
        let mut expected = args[..=word].to_vec();
        expected.extend(flags_of("muse").iter().map(|f| (*f).to_string()));
        expected.push("x".to_string());
        assert_eq!(decide("muse", &args), Launch::Default(expected));
    }
    assert_eq!(
        decide("muse", &argv(&["--model", "opus", "resume", "session-id"])),
        Launch::AsTyped
    );
    assert_eq!(
        decide("muse", &argv(&["--yolo"])),
        Launch::Flag(argv(&[
            "--disable-approval",
            "--disable-sandbox",
            "--trust-workspace",
            "--disable-request-user-input",
        ]))
    );
}

/// The echoes: distinct labels for asked and defaulted, every word quoted.
#[test]
fn the_echo_lines_name_the_command_as_run() {
    assert_eq!(
        echo_line("claude", &argv(&["--dangerously-skip-permissions"])),
        "aterm: --yolo: running 'claude' '--dangerously-skip-permissions'"
    );
    assert_eq!(
        default_echo_line(
            "agy",
            &argv(&["--dangerously-skip-permissions", "-p", "it's $x", ""])
        ),
        "aterm: yolo (default): running 'agy' '--dangerously-skip-permissions' \
         '-p' 'it'\\''s $x' ''"
    );
    assert_eq!(
        strip_no_harness("agy", &argv(&["--no-harness", "-p", "hi"])),
        Some(argv(&["-p", "hi"]))
    );
    assert_eq!(
        strip_no_harness(
            "claude",
            &argv(&["--no-alab-harness", "--", "--no-harness"])
        ),
        Some(argv(&["--", "--no-harness"]))
    );
    assert_eq!(
        strip_no_harness("agy", &argv(&["--", "--no-harness"])),
        None
    );
    assert_eq!(strip_no_harness("ay", &argv(&["--no-harness"])), None);
}

/// grok and kimi, read from their parsers, never by running either: the flag lands in front
/// (behind grok's `agent`, whose own options follow it), a typed `--yolo` becomes the vendor
/// flag in place, and an argv that already decides permissions or names a management word
/// runs as typed.
#[test]
fn grok_and_kimi_take_their_one_flag_where_their_parsers_read_it() {
    let d = |p: &str, a: &[&str]| decide(p, &argv(a));
    assert_eq!(d("grok", &[]), Launch::Default(argv(&["--always-approve"])));
    assert_eq!(
        d("grok", &["-m", "grok-4", "fix it"]),
        Launch::Default(argv(&["--always-approve", "-m", "grok-4", "fix it"]))
    );
    assert_eq!(
        d("grok", &["--resume", "abc", "agent", "--json"]),
        Launch::Default(argv(&[
            "--resume",
            "abc",
            "agent",
            "--always-approve",
            "--json"
        ]))
    );
    assert_eq!(
        d("grok", &["agent", "-p", "hi"]),
        Launch::Default(argv(&["agent", "--always-approve", "-p", "hi"]))
    );
    assert_eq!(
        d("grok", &["--yolo", "agent", "-p", "hi"]),
        Launch::Flag(argv(&["agent", "--always-approve", "-p", "hi"]))
    );
    assert_eq!(
        d("grok", &["--cwd", "/w", "--yolo"]),
        Launch::Flag(argv(&["--cwd", "/w", "--always-approve"]))
    );
    assert_eq!(d("kimi", &[]), Launch::Default(argv(&["--auto"])));
    assert_eq!(d("kimi", &["--yolo"]), Launch::Flag(argv(&["--auto"])));
    assert_eq!(
        d("kimi", &["-m", "k2", "--yolo", "--", "--yolo"]),
        Launch::Flag(argv(&["-m", "k2", "--auto", "--", "--yolo"]))
    );
    assert_eq!(
        d("kimi", &["--agent", "coder", "hello"]),
        Launch::Default(argv(&["--auto", "--agent", "coder", "hello"]))
    );
    for decided in [
        &["--always-approve"][..],
        &["--permission-mode", "plan"],
        &["--permission-mode=default"],
        &["--sandbox=strict"],
        &["--allow", "Bash(ls)"],
        &["--disallowed-tools=Bash"],
        &["agent", "--dangerously-skip-permissions"],
        &["-m", "grok-4", "--deny", "Write"],
    ] {
        assert_eq!(d("grok", decided), Launch::AsTyped, "grok {decided:?}");
    }
    for decided in [
        &["--auto"][..],
        &["-y"],
        &["--yes"],
        &["--auto-approve"],
        &["--plan"],
        &["-p", "hi"],
        &["--prompt=hi"],
        &["-S", "abc"],
        &["--session=abc"],
        &["-r"],
        &["--resume=abc"],
        &["-c"],
        &["--continue"],
        &["-C"],
    ] {
        assert_eq!(d("kimi", decided), Launch::AsTyped, "kimi {decided:?}");
    }
    for word in [
        "login",
        "mcp",
        "models",
        "doctor",
        "update",
        "version",
        "worktree",
        "worker",
        "cursor-worker",
        "clone",
        "ssh",
        "host-server",
        "help",
    ] {
        assert_eq!(d("grok", &[word]), Launch::AsTyped, "grok {word}");
    }
    assert_eq!(
        d("grok", &["--debug", "worker", "start"]),
        Launch::AsTyped,
        "a management word behind a known switch runs as typed too"
    );
    // Every leading option of grok's, by its arity: the flag still goes in front.
    for typed in [
        &["--effort", "high", "fix it"][..],
        &["--reasoning-effort=high", "fix it"],
        &["--add-dir", "/x", "fix it"],
        &["-s", "6f1c", "fix it"],
        &["--session-id", "6f1c"],
        &["--rules", "be terse", "fix it"],
        &["--system-prompt", "x"],
        &["--tools", "read_file", "--max-turns", "3", "fix it"],
        &["--agent", "agent"],
        &["--output-format", "json", "-p", "hi"],
        &["--debug", "fix it"],
        &["--minimal"],
        &["--no-alt-screen", "--fullscreen", "--verbatim"],
        &["-c", "--fork-session"],
        &["-w"],
        &["-w", "feat", "fix it"],
        &["-w", "login"],
        &["-wfeat", "--ref", "main"],
        &["--worktree=feat"],
        &["-r"],
        &["--resume", "agent"],
        &["-r", "--no-plan"],
        // A pooled relaunch onto a row of its own (`relaunch::pool_line`).
        &[
            "--leader-socket",
            "/Users//u/.grok/leader-work-99068163.sock",
            "--resume",
            "abc",
        ],
    ] {
        let mut want = argv(&["--always-approve"]);
        want.extend(argv(typed));
        assert_eq!(d("grok", typed), Launch::Default(want), "grok {typed:?}");
    }
    assert_eq!(
        d("grok", &["-w", "feat", "agent", "-p", "hi"]),
        Launch::Default(argv(&[
            "-w",
            "feat",
            "agent",
            "--always-approve",
            "-p",
            "hi"
        ]))
    );
    assert_eq!(
        d("grok", &["-w", "--yolo"]),
        Launch::Flag(argv(&["-w", "--always-approve"]))
    );
    for typed in [
        &["-w", "--yolo", "agent", "-p", "hi"][..],
        &["-r", "--yolo", "fix it"],
        &["--worktree", "--yolo", "--yolo", "agent"],
    ] {
        assert_eq!(
            d("grok", typed),
            Launch::AsTyped,
            "{typed:?}: dropping the word would hand the next one to the optional value"
        );
    }
    assert_eq!(
        d(
            "grok",
            &[
                "--yolo",
                "--dangerously-skip-permissions",
                "agent",
                "-p",
                "x"
            ]
        ),
        Launch::Flag(argv(&["agent", "--always-approve", "-p", "x"])),
        "grok's own alias of the flag is dropped: its parser refuses one option twice"
    );
    // kimi's `-y, --yolo` (and `--yes`, `--auto-approve`) is its own weaker posture, which
    // `--auto` cannot join: a typed `--yolo` takes that option over.
    for typed in [
        &["--yolo", "-y"][..],
        &["-y", "--yolo"],
        &["--yes", "--yolo", "--auto-approve"],
        &["--yolo", "--auto", "-y"],
    ] {
        assert_eq!(
            d("kimi", typed),
            Launch::Flag(argv(&["--auto"])),
            "kimi {typed:?}"
        );
    }
    assert_eq!(
        d("kimi", &["-y", "-m", "k2", "--yolo", "--add-dir", "/w"]),
        Launch::Flag(argv(&["-m", "k2", "--auto", "--add-dir", "/w"]))
    );
    assert_eq!(
        d("kimi", &["--yolo", "upgrade", "-y"]),
        Launch::Flag(argv(&["--auto", "upgrade", "-y"])),
        "a subcommand's own -y is its own"
    );
    assert_eq!(
        d("kimi", &["--yolo", "-y", "--", "-y"]),
        Launch::Flag(argv(&["--auto", "--", "-y"]))
    );
    assert_eq!(
        d("kimi", &["--yolo", "-S", "-y", "login"]),
        Launch::AsTyped,
        "dropping -y would hand `login` to -S as a session id"
    );
    assert_eq!(
        d("kimi", &["--yolo", "-S", "abc", "-y"]),
        Launch::Flag(argv(&["--auto", "-S", "abc"]))
    );
    for word in [
        "login", "acp", "web", "provider", "export", "upgrade", "update",
    ] {
        assert_eq!(d("kimi", &[word]), Launch::AsTyped, "kimi {word}");
    }
    assert_eq!(d("grok", &["--unknown", "x"]), Launch::AsTyped);
    assert_eq!(
        d("grok", &["--unknown", "--yolo"]),
        Launch::AsTyped,
        "an after word may hide behind an option this scan does not know"
    );
    assert_eq!(
        d("kimi", &["--unknown", "--yolo"]),
        Launch::Flag(argv(&["--unknown", "--auto"]))
    );
    assert_eq!(
        d("grok", &["--model"]),
        Launch::AsTyped,
        "a value is missing"
    );
}
