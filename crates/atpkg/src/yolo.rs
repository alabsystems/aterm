// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `--yolo` alias, and yolo by default: on every managed agent name, a launch
//! runs with every approval and sandbox off — aterm's posture is that AIs have
//! full access to do the work (owner ruling, 2026-10-19). Each vendor CLI spells
//! that posture differently (claude and agy `--dangerously-skip-permissions`,
//! codex `--dangerously-bypass-approvals-and-sandbox`, muse four switches), and
//! claude and agy refuse the literal `--yolo` outright (measured 2026-10-19:
//! claude 2.1.304 "unknown option '--yolo'", agy 1.4.2 "flags provided but not
//! defined: -yolo"; codex 0.163 and muse 1.4.11 parse it natively). The policy
//! is the vendor row's ([`Policy`], from
//! [`crate::vendor_direct::VendorSpec::yolo_flags`] and its siblings), compiled,
//! never taken from the index — the rule `shim_env` follows.
//!
//! [`decide`] is the whole argv rule, ahead of a literal `--`:
//!
//! 1. An exact `--yolo` is replaced in place by the row's flags ([`Launch::Flag`]).
//!    A flag already present is never added twice — codex refuses its bypass flag
//!    repeated (exit 2, measured 0.163).
//! 2. Else, a launch whose posture is already decided — any of the row's markers
//!    ([`Policy::markers`]: its permission-mode, sandbox and approval words) —
//!    runs exactly as typed ([`Launch::AsTyped`]). That is what keeps the default
//!    from silently upgrading a launch that chose a restriction, the harness's
//!    own resume lines among them: claude's carry `--permission-mode`, codex's
//!    its bypass or sandbox words, so they pass untouched.
//! 3. Else the flags are injected by default ([`Launch::Default`]): at the front,
//!    behind a word in [`Policy::after`] (muse's `exec`/`resume` take its
//!    switches only there), and never on a word in [`Policy::skip`] (muse's
//!    non-session subcommands take none). A `--yolo` typed ahead of an
//!    [`Policy::after`] word expands behind it for the same reason.
//!
//! Every rewritten launch — by the flag or by default — prints the command as
//! run once on stderr ([`echo_line`]/[`default_echo_line`], every word
//! single-quoted); an as-typed launch prints nothing. The channels:
//!
//! * The twin's block ([`crate::platform::sh_yolo_prelude`], after the self-update
//!   block in [`crate::platform::twin_prelude`], in `agents/` and in `bin/`)
//!   applies [`decide`] to its own `"$@"` in pure `/bin/sh` before the store
//!   `exec`. No atpkg is consulted, so the default holds with no package manager
//!   reachable. The self-update block stands ahead of it, so an update verb is
//!   answered there and never reaches the default.
//! * `atpkg run` (`aterm claude --yolo`, `cli::cmd_run`) execs past the shim, so
//!   it applies [`decide`] itself — on every platform, like its self-update arm.
//!   On Windows the `.cmd` twin carries no block (a batch line cannot rewrite
//!   `%*` inertly; [`crate::platform::twin_prelude`]), so there only the `aterm
//!   <tool>` lane carries the default.
//! * The process that runs never carries `--yolo` in its argv, so what the
//!   harness reads back for a resume or a live upgrade is the vendor's own
//!   flags — no other lane learns a new word.
//!
//! The accepted claim on the token: a bare `--yolo` anywhere ahead of a literal
//! `--` is the alias — including where a prompt operand would be (`claude -p
//! --yolo` asks for yolo mode with no prompt, not for the prompt "--yolo"; spell
//! a literal one after `--`). The escape from the default is the vendor's own
//! vocabulary: type the posture you want (`--permission-mode plan`, `--sandbox
//! workspace-write`, `--approval-mode on-request`) and the launch runs as typed.
//! There is no environment knob, deliberately: a posture this broad is decided
//! per launch and echoed, never inherited silently from a parent shell.
//!
//! Scope: the managed copies only. A copy of the user's own ahead on `PATH`
//! never runs through a twin and is never rewritten
//! (`docs/DESIGN-which-copy-runs-2026-08-27.md` rule 1).

/// The word the alias claims, exactly as typed.
pub const WORD: &str = "--yolo";

/// What a by-the-flag echo starts with: `aterm: --yolo: running 'claude'
/// '--dangerously-skip-permissions' …`.
pub const ECHO_PREFIX: &str = "aterm: --yolo: running ";

/// What a by-default echo starts with: `aterm: yolo (default): running
/// 'claude' …` — its own words, so a reader can tell an asked-for launch from a
/// defaulted one at a glance.
pub const DEFAULT_ECHO_PREFIX: &str = "aterm: yolo (default): running ";

/// One program's yolo policy: the four row fields together ([`policy_of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// The vendor flags the posture expands to.
    pub flags: &'static [&'static str],
    /// Tokens that already decide the posture (module doc, rule 2); an entry
    /// ending in `=` matches by prefix.
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

/// `program`'s yolo policy: its vendor row's fields, [`Policy::NONE`] for a
/// program with no row.
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

/// Whether `f` may be embedded in the twin's `sh` block as an expansion flag: a
/// long flag of lowercase letters, digits and hyphens after `--`
/// ([`crate::selfupdate::is_shell_safe_verb`]'s word shape behind the dashes).
/// The renderer refuses the whole block over one failing flag, so a row can
/// never lay a twin whose block breaks the `exec`.
#[must_use]
pub fn is_shell_safe_flag(f: &str) -> bool {
    f.strip_prefix("--")
        .is_some_and(crate::selfupdate::is_shell_safe_verb)
}

/// Whether `m` may be embedded in the block as a posture marker: a short flag
/// (`-s`), a long flag, or either's `=` prefix spelling (`--sandbox=`).
#[must_use]
pub fn is_shell_safe_marker(m: &str) -> bool {
    let word = m.strip_suffix('=').unwrap_or(m);
    let bytes = word.as_bytes();
    (bytes.len() == 2 && bytes[0] == b'-' && bytes[1].is_ascii_lowercase())
        || is_shell_safe_flag(word)
}

/// Whether `token` is decided by `marker`: the token itself, or a `--word=`
/// prefix entry the token starts with.
fn marker_matches(marker: &str, token: &str) -> bool {
    match marker.strip_suffix('=') {
        Some(word) => {
            token.len() > word.len()
                && token.starts_with(word)
                && token[word.len()..].starts_with('=')
        }
        None => marker == token,
    }
}

/// What a launch runs as, per the module doc's three rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// The argv as typed, nothing printed.
    AsTyped,
    /// `--yolo` was typed: the rewritten argv, echoed with [`ECHO_PREFIX`].
    Flag(Vec<String>),
    /// Nothing decided the posture: the rewritten argv, echoed with
    /// [`DEFAULT_ECHO_PREFIX`].
    Default(Vec<String>),
}

/// Decide `program args…` under the module doc's rules. The `sh` block is this
/// function's twin; the platform tests hold the two to the same argv out for the
/// same argv in.
#[must_use]
pub fn decide(program: &str, args: &[String]) -> Launch {
    let policy = policy_of(program);
    if policy.flags.is_empty() || policy.markers.is_empty() {
        return Launch::AsTyped;
    }
    let head_len = args.iter().take_while(|a| a.as_str() != "--").count();
    let head = &args[..head_len];
    // The first word: the first token that does not start with `-` (a flag
    // value spelled like a subcommand is taken for the word; the only rows
    // with word lists are muse's, whose session flags' values are not its
    // subcommand words in any measured launch).
    let word = head.iter().position(|a| !a.starts_with('-'));
    let after = word.is_some_and(|i| policy.after.contains(&head[i].as_str()));
    if head.iter().any(|a| a == WORD) {
        // Replace each --yolo with the flags not already present (and not just
        // emitted); ahead of an `after` word, the flags wait behind it.
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

/// The one stderr line a by-the-flag launch prints (no trailing newline): the
/// command as run, the program's managed name first, every word single-quoted
/// by the shim's one quoting rule ([`crate::platform::sh_quote_str`]) so the
/// line is copy-paste runnable.
#[must_use]
pub fn echo_line(program: &str, args: &[String]) -> String {
    line(ECHO_PREFIX, program, args)
}

/// [`echo_line`] for a by-default launch ([`DEFAULT_ECHO_PREFIX`]).
#[must_use]
pub fn default_echo_line(program: &str, args: &[String]) -> String {
    line(DEFAULT_ECHO_PREFIX, program, args)
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
            assert!(
                p.after.iter().all(|w| !p.skip.contains(w)),
                "{}",
                row.program
            );
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
        for good in ["-s", "-a", "--sandbox", "--sandbox=", "--permission-mode="] {
            assert!(is_shell_safe_marker(good), "{good}");
        }
        for bad in [
            "",
            "-",
            "-S",
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
        assert!(!marker_matches("--sandbox", "--sandbox=read-only"));
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
        // Already present: the word drops, no second copy (codex refuses it).
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
        // muse: ahead of `exec` the expansion lands behind it, from either side.
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
        // A switch typed by hand is not repeated; the rest still land.
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
            ("codex", argv(&["-s", "read-only"])),
            ("codex", argv(&["--sandbox=workspace-write", "exec", "x"])),
            ("codex", argv(&["-a", "on-request"])),
            ("agy", argv(&["--mode", "plan"])),
            ("agy", argv(&["--sandbox"])),
            ("muse", argv(&["--approval-mode", "on-request"])),
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
    }
}
