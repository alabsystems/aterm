// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `--yolo` alias, and yolo by default.

/// The word the alias claims, exactly as typed.
pub const WORD: &str = "--yolo";

/// What a by-the-flag echo starts with.
pub const ECHO_PREFIX: &str = "aterm: --yolo: running ";

/// What a by-default echo starts with.
pub const DEFAULT_ECHO_PREFIX: &str = "aterm: yolo (default): running ";

/// One program's yolo policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// The vendor flags the posture expands to.
    pub flags: &'static [&'static str],
    /// Tokens that already decide the posture: a launch holding one and no `--yolo` runs as typed.
    pub markers: &'static [&'static str],
    /// First words the injection goes behind (rule 3).
    pub after: &'static [&'static str],
    /// First words left as typed (rule 3).
    pub skip: &'static [&'static str],
}

impl Policy {
    /// No policy: no block, no expansion, no default.
    pub const NONE: Self = Self {
        flags: &[],
        markers: &[],
        after: &[],
        skip: &[],
    };
}

/// `program`'s yolo policy.
#[must_use]
pub fn policy_of(program: &str) -> Policy {
    crate::vendor_direct::spec(program).map_or(Policy::NONE, |row| Policy {
        flags: row.yolo_flags,
        markers: row.yolo_markers,
        after: row.yolo_after,
        skip: row.yolo_skip,
    })
}

/// The flags `--yolo` expands to for `program` ([`Policy::flags`]).
#[must_use]
pub fn flags_of(program: &str) -> &'static [&'static str] {
    policy_of(program).flags
}

/// Whether `f` may be embedded in the twin's `sh` block as an expansion flag.
#[must_use]
pub fn is_shell_safe_flag(f: &str) -> bool {
    f.strip_prefix("--")
        .is_some_and(crate::selfupdate::is_shell_safe_verb)
}

/// Whether `m` may be embedded in the block as a posture marker.
#[must_use]
pub fn is_shell_safe_marker(m: &str) -> bool {
    let word = m.strip_suffix('=').unwrap_or(m);
    let bytes = word.as_bytes();
    if bytes.len() == 2 && bytes[0] == b'-' {
        return bytes[1].is_ascii_alphabetic();
    }
    word.strip_prefix("--").is_some_and(|name| {
        name.as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic())
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

/// Whether `token` is decided by `marker`, in every spelling the vendors'
/// parsers give the same flag: a long flag also matches its `--flag=value`
/// form, and a short flag also matches its attached forms (`-sread-only`,
/// `-a=never`: clap takes the rest of the token as the value).
fn marker_matches(marker: &str, token: &str) -> bool {
    if let Some(word) = marker.strip_suffix('=') {
        return token.len() > word.len()
            && token.starts_with(word)
            && token[word.len()..].starts_with('=');
    }
    let bytes = marker.as_bytes();
    if bytes.len() == 2 && bytes[0] == b'-' && bytes[1] != b'-' {
        return token.starts_with(marker);
    }
    token == marker
        || token
            .strip_prefix(marker)
            .is_some_and(|rest| rest.starts_with('='))
}

/// Leading options whose arity this word scan knows.
pub(crate) fn word_options(program: &str) -> (&'static [&'static str], &'static [&'static str]) {
    let cli = crate::vendor_direct::spec(program)
        .and_then(|row| row.aliases.iter().find(|(name, _)| *name == program))
        .map_or(program, |(_, target)| *target);
    match cli {
        "codex" => (
            &[
                "--model",
                "-m",
                "--cd",
                "-C",
                "--add-dir",
                "--local-provider",
            ],
            &[
                "--oss",
                "--worktree",
                "--search",
                "--no-alt-screen",
                "--strict-config",
                "--no-daemon",
            ],
        ),
        "muse" => (&["--model"], &[]),
        "claude" => (&[], &["-p", "--print"]),
        _ => (&[], &[]),
    }
}

/// The actual argument position of the first word, past known leading values.
fn first_word(program: &str, policy: &Policy, head: &[String]) -> Result<Option<usize>, ()> {
    let (values, switches) = word_options(program);
    let mut value = false;
    for (i, arg) in head.iter().enumerate() {
        if value {
            value = false;
        } else if arg == WORD
            || policy.flags.contains(&arg.as_str())
            || switches.contains(&arg.as_str())
        {
            continue;
        } else if values.contains(&arg.as_str()) {
            value = true;
        } else if values.iter().any(|flag| marker_matches(flag, arg)) {
            // A long `=value` or attached short value is already consumed.
            continue;
        } else if arg.starts_with('-') {
            return Err(());
        } else {
            return Ok(Some(i));
        }
    }
    if value { Err(()) } else { Ok(None) }
}

/// What a launch runs as ([`decide`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// The argv as typed, nothing printed.
    AsTyped,
    /// `--yolo` was typed.
    Flag(Vec<String>),
    /// Nothing decided the posture.
    Default(Vec<String>),
}

/// Decide `program args…`, reading the words ahead of a literal `--`. A program with no policy
/// runs as typed. An unknown leading option leaves the argv as typed (with a typed `--yolo` too,
/// when the policy has `after` words); else a typed `--yolo` becomes the policy's flags
/// ([`Launch::Flag`]); else a marker, a `skip` word or a self-update verb leaves it as typed;
/// else the flags go in front, or behind an `after` word ([`Launch::Default`]).
#[must_use]
pub fn decide(program: &str, args: &[String]) -> Launch {
    let policy = policy_of(program);
    if policy.flags.is_empty() || policy.markers.is_empty() {
        return Launch::AsTyped;
    }
    let head_len = args.iter().take_while(|a| a.as_str() != "--").count();
    let head = &args[..head_len];
    let word = first_word(program, &policy, head);
    let explicit = head.iter().any(|a| a == WORD);
    if word.is_err() && (!explicit || !policy.after.is_empty()) {
        return Launch::AsTyped;
    }
    let word = word.ok().flatten();
    let after = word.is_some_and(|i| policy.after.contains(&head[i].as_str()));
    if explicit {
        let mut present: Vec<&str> = head.iter().map(String::as_str).collect();
        let fresh = |out: &mut Vec<String>, present: &mut Vec<&str>| {
            for flag in policy.flags {
                if !present.contains(flag) {
                    present.push(flag);
                    out.push((*flag).to_string());
                }
            }
        };
        let mut out: Vec<String> = Vec::with_capacity(args.len() + policy.flags.len());
        let mut pending = false;
        for (i, arg) in head.iter().enumerate() {
            if arg == WORD {
                if after && word.is_some_and(|w| i < w) {
                    pending = true;
                } else {
                    fresh(&mut out, &mut present);
                }
            } else {
                out.push(arg.clone());
                if pending && word == Some(i) {
                    pending = false;
                    fresh(&mut out, &mut present);
                }
            }
        }
        out.extend(args[head_len..].iter().cloned());
        return Launch::Flag(out);
    }
    if head
        .iter()
        .any(|a| policy.markers.iter().any(|m| marker_matches(m, a)))
    {
        return Launch::AsTyped;
    }
    if let Some(i) = word {
        // The self-update verbs skip too, on every lane: an update shape is
        // decided by that intercept first (`claude update --help` forwards to
        // the vendor), and the default must not land on the forward.
        if policy.skip.contains(&head[i].as_str())
            || crate::selfupdate::verbs_of(program).contains(&head[i].as_str())
        {
            return Launch::AsTyped;
        }
        if after {
            let mut out: Vec<String> = Vec::with_capacity(args.len() + policy.flags.len());
            out.extend(head[..=i].iter().cloned());
            out.extend(policy.flags.iter().map(|f| (*f).to_string()));
            out.extend(args[i + 1..].iter().cloned());
            return Launch::Default(out);
        }
    }
    let mut out: Vec<String> = policy.flags.iter().map(|f| (*f).to_string()).collect();
    out.extend(args.iter().cloned());
    Launch::Default(out)
}

fn line(prefix: &str, program: &str, args: &[String]) -> String {
    let mut s = String::from(prefix);
    s.push_str(&crate::platform::sh_quote_str(program));
    for arg in args {
        s.push(' ');
        s.push_str(&crate::platform::sh_quote_str(arg));
    }
    s
}

/// The one stderr line a by-the-flag launch prints (no trailing newline).
#[must_use]
pub fn echo_line(program: &str, args: &[String]) -> String {
    line(ECHO_PREFIX, program, args)
}

/// [`echo_line`] for a by-default launch ([`DEFAULT_ECHO_PREFIX`]).
#[must_use]
pub fn default_echo_line(program: &str, args: &[String]) -> String {
    line(DEFAULT_ECHO_PREFIX, program, args)
}

/// Environment variable set when `--no-harness` or `--no-alab-harness` is passed on an agent launch.
pub const NO_HARNESS_ENV: &str = "ATERM_NO_HARNESS";

/// Strip `--no-harness` and `--no-alab-harness` ahead of a literal `--` for an
/// agent program, returning `Some(stripped)` when at least one was removed.
#[must_use]
pub fn strip_no_harness(program: &str, args: &[String]) -> Option<Vec<String>> {
    if !crate::stub::is_agent_program(program) {
        return None;
    }
    let end = args.iter().position(|a| a == "--").unwrap_or(args.len());
    if !args[..end]
        .iter()
        .any(|a| matches!(a.as_str(), "--no-harness" | "--no-alab-harness"))
    {
        return None;
    }
    let mut out = Vec::with_capacity(args.len());
    for (i, arg) in args.iter().enumerate() {
        if i < end && matches!(arg.as_str(), "--no-harness" | "--no-alab-harness") {
            continue;
        }
        out.push(arg.clone());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_string()).collect()
    }

    /// The roster: every vendor row spells its yolo posture in full — never the
    /// word itself as a flag (that would hand claude and agy a flag they
    /// refuse), every flag and marker safe to embed, its own flags among its
    /// markers, and the exact expansions measured from each vendor's `--help`
    /// (2026-10-19).
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
        assert_eq!(word_options("codex-older"), (&[][..], &[][..]));
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
            ("codex", argv(&["-sread-only"])),
            ("codex", argv(&["-s=workspace-write", "exec", "x"])),
            ("codex", argv(&["--sandbox=workspace-write", "exec", "x"])),
            ("codex", argv(&["-a", "on-request"])),
            ("codex", argv(&["-a=never"])),
            ("codex", argv(&["-c", "sandbox_mode=\"read-only\""])),
            ("codex", argv(&["--config=approval_policy=\"never\""])),
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
}
