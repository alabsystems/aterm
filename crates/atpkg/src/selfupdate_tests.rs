// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn a(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

fn claude() -> &'static VendorSpec {
    row_for("claude").unwrap()
}

fn codex() -> &'static VendorSpec {
    row_for("codex").unwrap()
}

/// A verb lands in the twin's `case` pattern, so only a shell-safe word qualifies; a
/// program with no vendor row has no verbs.
#[test]
fn the_shell_word_rule_refuses_what_breaks_a_case_pattern() {
    assert_eq!(verbs_of("ay"), &[] as &[&str], "a non-member has no verbs");
    assert_eq!(row_for("ay"), None);
    for ok in ["update", "upgrade", "install", "self-update", "v2"] {
        assert!(is_shell_safe_verb(ok), "{ok}");
    }
    for bad in [
        "", "Update", "up|date)", "up date", "*", "-x", "2go", "cl'aude", "a\n", "esac",
    ] {
        assert!(!is_shell_safe_verb(bad), "{bad:?}");
    }
    for reserved_but_inert in ["in", "case", "if", "then", "fi", "do", "done", "while"] {
        assert!(
            is_shell_safe_verb(reserved_but_inert),
            "{reserved_but_inert}: parses and matches as an ordinary pattern word"
        );
    }
}

/// One assertion per rule of [`classify`]'s doc, negative controls included, and codex,
/// whose row carries only `update`.
#[test]
fn classify_is_the_first_token_exactly_and_help_never_checks() {
    let c = claude();
    for pass in [
        &[][..],
        &["--bare", "update"],
        &["-p", "update"],
        &["--", "update"],
        &["help", "update"],
        &["doctor"],
    ] {
        assert_eq!(classify(c, &a(pass)), Verdict::PassThrough, "{pass:?}");
    }
    for help in [
        &["update", "--help"][..],
        &["install", "-h"],
        &["upgrade", "-x", "--help"],
        &["install", "latest", "--help"],
        &["-h", "update"],
    ] {
        assert_eq!(classify(c, &a(help)), Verdict::Help, "{help:?}");
    }
    let shape = |verb: &'static str, rest: &[&str]| {
        let rest = a(rest);
        let mut args = vec![verb.to_string()];
        args.extend(rest.iter().cloned());
        assert_eq!(
            classify(c, &args),
            Verdict::Declined(Decline::Shape { verb, rest: &rest }),
            "{args:?}"
        );
    };
    shape("update", &["--", "--help"]);
    shape("update", &["now"]);
    shape("upgrade", &["latest"]);
    shape("install", &["latest", "latest"]);
    shape("install", &["--force", "--force"]);
    shape("install", &["--foo"]);
    shape("install", &["latest", "--foo"]);
    for (typed, words) in [
        (&["update"][..], "update"),
        (&["upgrade"], "upgrade"),
        (&["install"], "install"),
        (&["install", "latest"], "install latest"),
        (&["install", "--force"], "install --force"),
        (&["install", "latest", "--force"], "install latest --force"),
        (&["install", "--force", "latest"], "install --force latest"),
    ] {
        assert_eq!(
            classify(c, &a(typed)),
            Verdict::Check {
                verb: typed[0],
                words: words.to_string()
            },
            "{typed:?}"
        );
    }
    for stable in [
        &["install", "stable"][..],
        &["install", "--force", "stable"],
    ] {
        assert_eq!(
            classify(c, &a(stable)),
            Verdict::Declined(Decline::NotAChannel {
                verb: "install",
                target: "stable"
            }),
            "{stable:?}"
        );
    }
    for (typed, target) in [
        (&["install", "2.1.200"][..], "2.1.200"),
        (&["install", "v2.1.200"], "v2.1.200"),
        (&["install", "--force", "2.1.200"], "2.1.200"),
        (&["install", "2.1.200", "--force"], "2.1.200"),
    ] {
        assert_eq!(
            classify(c, &a(typed)),
            Verdict::Declined(Decline::Version {
                verb: "install",
                target
            }),
            "{typed:?}"
        );
    }
    let x = codex();
    assert_eq!(
        classify(x, &a(&["update"])),
        Verdict::Check {
            verb: "update",
            words: "update".into()
        }
    );
    assert_eq!(classify(x, &a(&["upgrade"])), Verdict::PassThrough);
    assert_eq!(classify(x, &a(&["install"])), Verdict::PassThrough);
    assert_eq!(
        classify(x, &a(&["install", "latest"])),
        Verdict::PassThrough
    );
    assert_eq!(classify(x, &a(&["update", "--help"])), Verdict::Help);
    let rest = a(&["now"]);
    assert_eq!(
        classify(x, &a(&["update", "now"])),
        Verdict::Declined(Decline::Shape {
            verb: "update",
            rest: &rest
        })
    );
    assert!(matches!(
        classify(c, &a(&["install", "latest"])),
        Verdict::Check { .. }
    ));
    assert!(looks_like_version("2.1.200"));
    assert!(looks_like_version("v2"));
    assert!(!looks_like_version("v"));
    assert!(!looks_like_version("latest"));
    assert!(!looks_like_version(""));
}

/// grok's one verb and kimi's two: a bare verb is the managed check; anything after it
/// (grok's `update --check`, kimi's `upgrade -y`) is a shape aterm declines rather than
/// guesses; grok's `upgrade`, which is not grok's verb, passes through to grok.
#[test]
fn grok_and_kimi_check_on_their_bare_verbs_only() {
    let (grok, kimi) = (row_for("grok").unwrap(), row_for("kimi").unwrap());
    assert_eq!(verbs_of("grok"), &["update"]);
    assert_eq!(verbs_of("kimi"), &["upgrade", "update"]);
    for (row, verb) in [(grok, "update"), (kimi, "upgrade"), (kimi, "update")] {
        assert_eq!(
            classify(row, &a(&[verb])),
            Verdict::Check {
                verb,
                words: verb.to_string()
            },
            "{} {verb}",
            row.program
        );
    }
    for (row, typed) in [
        (grok, &["update", "--check"][..]),
        (grok, &["update", "1.0.70"]),
        (kimi, &["upgrade", "-y"]),
        (kimi, &["update", "--force"]),
        (kimi, &["upgrade", "latest"]),
    ] {
        assert!(
            matches!(
                classify(row, &a(typed)),
                Verdict::Declined(Decline::Shape { .. })
            ),
            "{} {typed:?}",
            row.program
        );
    }
    for (row, typed) in [
        (grok, &["upgrade"][..]),
        (grok, &["--resume", "update"]),
        (kimi, &["install"]),
    ] {
        assert_eq!(classify(row, &a(typed)), Verdict::PassThrough, "{typed:?}");
    }
    assert_eq!(classify(kimi, &a(&["upgrade", "--help"])), Verdict::Help);
}

/// A hostile target — a newline, quotes, 200 bytes — is escaped and capped before it
/// reaches a terminal, and the cap lands on a char boundary.
#[test]
fn a_hostile_echo_is_escaped_and_capped() {
    let c = claude();
    let line = declined_line(
        c,
        &Decline::Version {
            verb: "install",
            target: "2.1\n\"x\"",
        },
    );
    assert!(!line.contains('\n'), "{line}");
    let long = "9".repeat(200);
    let line = declined_line(
        c,
        &Decline::Version {
            verb: "install",
            target: &long,
        },
    );
    assert!(line.contains(&"9".repeat(64)), "{line}");
    assert!(!line.contains(&"9".repeat(65)), "{line}");
    let capped = echo_words(&a(&[&"é".repeat(40)]));
    assert!(capped.ends_with('…'), "{capped}");
    assert!(capped.len() <= ECHO_CAP + '…'.len_utf8(), "{capped}");
}

/// The child's argv: the standard update with the fixed bound, never the window's
/// `--progress-file`, and the executable decides the leading verb.
#[test]
fn the_child_argv_is_the_standard_update_with_the_fixed_wait() {
    assert_eq!(
        child_args("claude", WAIT_LOCK_SECS),
        a(&["update", "claude", "--wait-lock", "1800"])
    );
    assert_eq!(
        child_args("codex", 1),
        a(&["update", "codex", "--wait-lock", "1"]),
        "a test's own bound rides the same pair"
    );
    assert!(
        !child_args("claude", WAIT_LOCK_SECS)
            .iter()
            .any(|s| s.contains("progress")),
        "never the window's --progress-file"
    );
    for atpkg in [
        "/Applications/aterm.app/Contents/MacOS/atpkg",
        "/x/target/debug/atpkg",
        "atpkg.exe",
        "atpkg",
    ] {
        assert_eq!(
            child_argv(Path::new(atpkg), "claude", WAIT_LOCK_SECS),
            a(&["update", "claude", "--wait-lock", "1800"]),
            "{atpkg}"
        );
    }
    for front_door in [
        "/Applications/aterm.app/Contents/MacOS/aterm",
        "/x/target/debug/aterm",
        "/x/nosib/aterm-cli",
        "aterm.exe",
    ] {
        assert_eq!(
            child_argv(Path::new(front_door), "claude", WAIT_LOCK_SECS),
            a(&["pkg", "update", "claude", "--wait-lock", "1800"]),
            "{front_door}"
        );
    }
    assert_eq!(
        child_argv(Path::new("/x/aterm"), "codex", 1),
        a(&["pkg", "update", "codex", "--wait-lock", "1"])
    );
}
