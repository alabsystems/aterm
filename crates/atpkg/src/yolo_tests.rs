// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// The roster: every vendor row that keeps a yolo policy spells its posture in full — never
/// the word itself as a flag (claude and agy refuse it), every flag and marker safe to embed,
/// its own flags among its markers, and the exact expansions from each vendor's `--help`. kimi,
/// grok and muse keep none: their harnesses' `run` sets the default (HX-09b, HX-10d, HX-13a).
#[test]
fn every_row_spells_its_yolo_policy_in_full() {
    assert_eq!(flags_of("claude"), &["--dangerously-skip-permissions"]);
    assert_eq!(flags_of("agy"), &["--dangerously-skip-permissions"]);
    assert_eq!(
        flags_of("codex"),
        &["--dangerously-bypass-approvals-and-sandbox"]
    );
    let kept: Vec<&str> = crate::vendor_direct::VENDORS
        .iter()
        .filter(|row| policy_of(row.program) != Policy::NONE)
        .map(|row| row.program)
        .collect();
    assert_eq!(kept, ["claude", "codex", "agy", "codex-old"]);
    for row in crate::vendor_direct::VENDORS {
        let p = policy_of(row.program);
        if !kept.contains(&row.program) {
            continue;
        }
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
/// flag twice.
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
    assert_eq!(
        decide("claude", &argv(&["--yolo", "--", "--yolo"])),
        Launch::Flag(argv(&["--dangerously-skip-permissions", "--", "--yolo"]))
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

/// By default: injected at the front, and never over a decided posture, a
/// self-update verb, or past `--`-only noise.
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
