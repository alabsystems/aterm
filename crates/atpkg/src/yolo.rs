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
    /// First words the flags go behind.
    pub after: &'static [&'static str],
    /// First words that leave a launch without `--yolo` as typed.
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

/// A program's leading options as its parser reads them ([`word_options`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WordOptions {
    /// Options that take the next token as their value.
    pub(crate) values: &'static [&'static str],
    /// Options whose value is optional (clap's `num_args = 0..=1`, commander's `[value]`):
    /// the next token is theirs unless it starts with `-`.
    pub(crate) optional: &'static [&'static str],
    /// Options that take no value.
    pub(crate) switches: &'static [&'static str],
    /// The program's own spellings of the posture a typed `--yolo` sets, each among the row's
    /// markers: another name of the flag's option (grok's `--dangerously-skip-permissions`, an
    /// alias of `--always-approve` its parser refuses twice), or a weaker posture its parser
    /// refuses beside the flags (kimi's `-y`, `--yes` and `--auto-approve`). An explicit
    /// `--yolo` drops each one ahead of the first word along with itself.
    pub(crate) own_yolo: &'static [&'static str],
}

/// Leading options whose arity this word scan knows.
pub(crate) fn word_options(program: &str) -> WordOptions {
    let cli = crate::vendor_direct::spec(program)
        .and_then(|row| row.aliases.iter().find(|(name, _)| *name == program))
        .map_or(program, |(_, target)| *target);
    match cli {
        "codex" => WordOptions {
            values: &[
                "--model",
                "-m",
                "--cd",
                "-C",
                "--add-dir",
                "--local-provider",
            ],
            switches: &[
                "--oss",
                "--worktree",
                "--search",
                "--no-alt-screen",
                "--strict-config",
                "--no-daemon",
            ],
            ..WordOptions::default()
        },
        "muse" => WordOptions {
            values: &["--model"],
            ..WordOptions::default()
        },
        "claude" => WordOptions {
            switches: &["-p", "--print"],
            ..WordOptions::default()
        },
        // grok's top-level `PagerArgs` (1.0.45 source, `xai-grok-pager/src/app/cli.rs`) plus
        // 1.0.67's `--add-dir`. Hidden and feature-gated options, `-v`, `--version` and `-h`
        // are left out, so a launch typing one runs as typed.
        "grok" => WordOptions {
            values: &[
                "-m",
                "--model",
                "--cwd",
                "-p",
                "--single",
                "--print",
                "--prompt-file",
                "--prompt-json",
                "--load",
                "--leader-socket",
                "--debug-file",
                "--output-format",
                "--json-schema",
                "--reasoning-effort",
                "--effort",
                "--rules",
                "--append-system-prompt",
                "--system-prompt-override",
                "--system-prompt",
                "-s",
                "--session-id",
                "--worktree-ref",
                "--ref",
                "--agent",
                "--agents",
                "--tools",
                "--max-turns",
                "--add-dir",
            ],
            optional: &["-r", "--resume", "-w", "--worktree"],
            switches: &[
                "-c",
                "--continue",
                "--no-auto-update",
                "--debug",
                "--verbatim",
                "--include-partial-messages",
                "--fork-session",
                "--restore-code",
                "--no-plan",
                "--no-subagents",
                "--disable-web-search",
                "--no-alt-screen",
                "--minimal",
                "--fullscreen",
                "--oauth",
            ],
            own_yolo: &["--dangerously-skip-permissions"],
        },
        // kimi 2.3.1's top-level options (`apps/kimi-code/src/cli/commands.ts`).
        "kimi" => WordOptions {
            values: &[
                "-m",
                "--model",
                "-p",
                "--prompt",
                "--agent",
                "--agent-file",
                "--add-dir",
                "--skills-dir",
                "--output-format",
            ],
            optional: &["-S", "--session", "-r", "--resume"],
            switches: &["-c", "--continue", "-C", "--plan"],
            own_yolo: &["-y", "--yes", "--auto-approve"],
        },
        _ => WordOptions::default(),
    }
}

/// Why the word scan stopped short of the first word ([`first_word`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    /// An option the scan does not know, or a value missing at the end.
    Unknown,
    /// A typed `--yolo`, or one of [`WordOptions::own_yolo`], sits right after a bare
    /// optional-value option and before a bare word: removing it would hand that word to the
    /// option as its value.
    Joins,
}

/// The actual argument position of the first word, past known leading values.
fn first_word(program: &str, policy: &Policy, head: &[String]) -> Result<Option<usize>, Stop> {
    let opts = word_options(program);
    let removable = |a: &str| a == WORD || opts.own_yolo.contains(&a);
    let mut value = false;
    let mut maybe = false;
    let mut gap = false;
    for (i, arg) in head.iter().enumerate() {
        let a = arg.as_str();
        if value {
            value = false;
            continue;
        }
        if maybe {
            maybe = false;
            if !a.starts_with('-') {
                // The optional value.
                continue;
            }
            gap = removable(a);
        } else if gap && !removable(a) {
            if !a.starts_with('-') {
                return Err(Stop::Joins);
            }
            gap = false;
        }
        if removable(a) || policy.flags.contains(&a) || opts.switches.contains(&a) {
            continue;
        } else if opts.values.contains(&a) {
            value = true;
        } else if opts.optional.contains(&a) {
            maybe = true;
        } else if opts
            .values
            .iter()
            .chain(opts.optional.iter())
            .any(|flag| marker_matches(flag, a))
        {
            // A long `=value` or attached short value is already consumed.
            continue;
        } else if a.starts_with('-') {
            return Err(Stop::Unknown);
        } else {
            return Ok(Some(i));
        }
    }
    if value { Err(Stop::Unknown) } else { Ok(None) }
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
/// runs as typed, and so does an unknown leading option (with a typed `--yolo` too, when the
/// policy has `after` words) or a removal that would join a bare word to an optional value
/// ([`Stop::Joins`]). Else a typed `--yolo` becomes the policy's flags, dropping the program's
/// own spellings of that posture ahead of the first word ([`Launch::Flag`]); else a marker, a
/// `skip` word or a self-update verb runs as typed; else the flags go in front, or behind an
/// `after` word ([`Launch::Default`]).
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
    match word {
        Err(Stop::Joins) => return Launch::AsTyped,
        Err(Stop::Unknown) if !explicit || !policy.after.is_empty() => return Launch::AsTyped,
        _ => {}
    }
    // The leading options an explicit `--yolo` may drop the program's own spellings from: none
    // past an option the scan does not know.
    let leading = match word {
        Ok(Some(i)) => i,
        Ok(None) => head_len,
        Err(_) => 0,
    };
    let own_yolo = word_options(program).own_yolo;
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
            } else if i < leading && own_yolo.contains(&arg.as_str()) {
                // The vendor's own spelling of the posture the flags set.
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

/// Set to `1` when an agent launch passes `--no-harness` or `--no-alab-harness`.
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
}
