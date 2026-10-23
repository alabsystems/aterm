// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The self-update intercept: a managed agent's own updater (`claude update`) runs
//! `aterm pkg update <program>` instead ([`crate::platform::sh_selfupdate_prelude`]).
use std::ffi::OsStr;
use std::io;
use std::path::Path;

use crate::vendor_direct::VendorSpec;

/// Hidden verb executed by the twin prelude (`atpkg __selfupdate <program> [<abs prefix>] -- [args…]`, [`crate::platform::sh_selfupdate_prelude`]).
pub const HIDDEN_VERB: &str = "__selfupdate";

/// The twin's re-entry guard: set to the shell's `$$` before it execs [`HIDDEN_VERB`], so a
/// forwarded call that re-enters the twin under the same PID runs the store build instead of
/// looping.
pub const REENTRY_VAR: &str = "__ATPKG_SELFUPDATE_PID";

/// Child `--wait-lock` bound in seconds ([`aterm_update_core::pkg_check::PASS_WAIT_LOCK_SECS`]).
pub const WAIT_LOCK_SECS: u64 = aterm_update_core::pkg_check::PASS_WAIT_LOCK_SECS;

/// Returns `program`'s vendor row, or `None` for non-agent programs.
#[must_use]
pub fn row_for(program: &str) -> Option<&'static VendorSpec> {
    crate::vendor_direct::spec(program)
}

/// Returns the verbs intercepted by `program`'s twin.
#[must_use]
pub fn verbs_of(program: &str) -> &'static [&'static str] {
    row_for(program).map_or(&[], |r| r.self_update_verbs)
}

/// Returns whether `v` is a shell-safe verb (`^[a-z][a-z0-9-]*$` and not `esac`).
#[must_use]
pub fn is_shell_safe_verb(v: &str) -> bool {
    let mut chars = v.chars();
    v != "esac"
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Action to take for the user's argv, decided by [`classify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict<'a> {
    /// Print a stderr note and forward to the vendor's own help.
    Help,
    /// Not an intercepted shape; run `bin/<program>` verbatim.
    PassThrough,
    /// Shape aterm honours by announcing and running `update <program>`.
    Check {
        /// Verb typed (`update`, `upgrade`, `install`).
        verb: &'a str,
        /// Words as typed, verb first.
        words: String,
    },
    /// Shape aterm refuses with exit 2.
    Declined(Decline<'a>),
}

/// Reason a self-update shape is declined ([`declined_line`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decline<'a> {
    /// Non-`latest` channel target (`install stable`).
    NotAChannel { verb: &'a str, target: &'a str },
    /// Explicit version target (`install 2.1.200`).
    Version { verb: &'a str, target: &'a str },
    /// Unrecognized arguments after the verb.
    Shape { verb: &'a str, rest: &'a [String] },
}

/// Classifies the user's argv (after the twin's `--`) against `row`: `-h`/`--help` before any
/// `--` is [`Verdict::Help`]; a first word outside the row's verbs passes through; a verb
/// other than `install` takes no arguments; `install` takes `latest` and `--force`, each at
/// most once; `install stable` and a version target are declined by name.
#[must_use]
pub fn classify<'a>(row: &VendorSpec, args: &'a [String]) -> Verdict<'a> {
    if args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "-h" || a == "--help")
    {
        return Verdict::Help;
    }
    let Some((verb, rest)) = args.split_first() else {
        return Verdict::PassThrough;
    };
    let verb = verb.as_str();
    if !row.self_update_verbs.contains(&verb) {
        return Verdict::PassThrough;
    }
    if verb != "install" {
        return if rest.is_empty() {
            Verdict::Check {
                verb,
                words: verb.to_string(),
            }
        } else {
            Verdict::Declined(Decline::Shape { verb, rest })
        };
    }
    let (mut latest, mut force) = (false, false);
    let honoured = rest.iter().all(|a| match a.as_str() {
        "latest" if !latest => {
            latest = true;
            true
        }
        "--force" if !force => {
            force = true;
            true
        }
        _ => false,
    });
    if honoured {
        return Verdict::Check {
            verb,
            words: args.join(" "),
        };
    }
    let target = rest
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(String::as_str);
    Verdict::Declined(match target {
        Some("stable") => Decline::NotAChannel {
            verb,
            target: "stable",
        },
        Some(t) if looks_like_version(t) => Decline::Version { verb, target: t },
        _ => Decline::Shape { verb, rest },
    })
}

/// `2.1.200`, `v2.1.200`, `2`: a leading ASCII digit, or `v` followed by one.
fn looks_like_version(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_digit() => true,
        Some('v') => chars.next().is_some_and(|c| c.is_ascii_digit()),
        _ => false,
    }
}

/// Builds the child `update <program> --wait-lock <bound>` arguments.
#[must_use]
pub fn child_args(program: &str, bound: u64) -> Vec<String> {
    vec![
        String::from("update"),
        program.to_string(),
        String::from("--wait-lock"),
        bound.to_string(),
    ]
}

/// Builds [`child_args`] prefixed with `pkg` when `binary` is `aterm` rather than `atpkg`.
#[must_use]
pub fn child_argv(binary: &Path, program: &str, bound: u64) -> Vec<String> {
    let is_atpkg = binary
        .file_stem()
        .and_then(OsStr::to_str)
        .is_some_and(|stem| stem == "atpkg");
    let mut v = if is_atpkg {
        Vec::new()
    } else {
        vec![String::from("pkg")]
    };
    v.extend(child_args(program, bound));
    v
}

/// The longest echo of user-typed text any line carries, in bytes; the rest is `…`.
const ECHO_CAP: usize = 64;

/// A user-typed token as the lines echo it: plain when every byte is printable ASCII
/// without whitespace (`2.1.200`, `--foo`), else `{:?}`-escaped (a newline, a quote, a
/// tab, a non-ASCII byte — nothing the terminal would interpret).
fn echo_token(t: &str) -> String {
    if !t.is_empty() && t.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        t.to_string()
    } else {
        format!("{t:?}")
    }
}

/// The user's tokens echoed ([`echo_token`] each), space-joined, capped at [`ECHO_CAP`]
/// bytes on a char boundary with `…`.
fn echo_words(args: &[String]) -> String {
    let joined = args
        .iter()
        .map(|a| echo_token(a))
        .collect::<Vec<_>>()
        .join(" ");
    if joined.len() <= ECHO_CAP {
        return joined;
    }
    let mut end = ECHO_CAP;
    while !joined.is_char_boundary(end) {
        end -= 1;
    }
    let mut capped = joined[..end].to_string();
    capped.push('…');
    capped
}

/// The verb the lines name: the row's first, `update` for every row that has one.
fn lead_verb(row: &VendorSpec) -> &'static str {
    row.self_update_verbs.first().copied().unwrap_or("update")
}

/// The active build's version (`2.1.280`; `build N` for a legacy index build), or
/// `(no version recorded)` with none.
fn build_words(build: Option<u64>) -> String {
    build.map_or_else(
        || String::from("(no version recorded)"),
        crate::vendor_direct::build_words,
    )
}

/// Formats the stderr announcement line before spawning the update child
/// ([`crate::vendor_direct::have_words`]).
#[must_use]
pub fn announce_line(row: &VendorSpec, have: &str) -> String {
    let p = row.program;
    let (product, vendor) = (row.product, row.vendor);
    format!(
        "atpkg: aterm updates {product} from {vendor} — you have {have}; checking now: `aterm \
         pkg update {p}`"
    )
}

/// Formats [`announce_line`] for an ALab build of `program` ([`crate::alab`], [`crate::alab::build_words`]).
#[must_use]
pub fn alab_announce_line(program: &str, have: &str) -> String {
    format!(
        "atpkg: aterm updates {program} from ALab's channel — you have {have}; checking now: \
         `aterm pkg update {program}`"
    )
}

/// Formats the stderr refusal line for a [`Decline`] via [`echo_words`].
#[must_use]
pub fn declined_line(row: &VendorSpec, d: &Decline<'_>) -> String {
    declined_from(
        row.program,
        row.product,
        row.vendor,
        &format!("{}'s latest", row.vendor),
        d,
    )
}

/// Formats [`declined_line`] for an ALab build of `program` ([`crate::alab`]).
#[must_use]
pub fn alab_declined_line(agent: &crate::alab::AlabAgent, d: &Decline<'_>) -> String {
    declined_from(
        agent.program,
        agent.product,
        "ALab's channel",
        "ALab's channel",
        d,
    )
}

/// [`declined_line`]'s sentences for `program`'s `product`, updated `from` a source whose
/// newest build a line calls `latest`.
fn declined_from(
    program: &str,
    product: &str,
    from: &str,
    latest: &str,
    d: &Decline<'_>,
) -> String {
    let p = program;
    match d {
        Decline::NotAChannel { verb, target } => format!(
            "atpkg: `{p} {verb} {target}` not run — aterm updates {product} from {from} and \
             installs no `{target}`; `{p} {verb} latest` checks {latest} now (`aterm pkg pin \
             {p}` holds the version you have)"
        ),
        Decline::Version { verb, target } => format!(
            "atpkg: `{p} {verb} {}` not run — aterm updates {product} from {from} and takes \
             no version number; `{p} {verb} latest` checks {latest} now (`aterm pkg rollback \
             {p}` returns to the version before)",
            echo_words(std::slice::from_ref(&(*target).to_string()))
        ),
        Decline::Shape { verb, rest } => {
            let mut typed = vec![(*verb).to_string()];
            typed.extend(rest.iter().cloned());
            format!(
                "atpkg: `{p} {}` not run — not a shape aterm's updater takes; `{p} {verb}` \
                 alone checks {latest} now",
                echo_words(&typed)
            )
        }
    }
}

/// Formats the stderr note printed ahead of the vendor's own `--help` output.
#[must_use]
pub fn help_line(row: &VendorSpec) -> String {
    let p = row.program;
    format!(
        "atpkg: here `{p} {}` runs `aterm pkg update {p}` (`aterm help pkg`) — the vendor's \
         own help follows",
        lead_verb(row)
    )
}

/// Formats the stderr line when the package manager is disabled.
#[must_use]
pub fn disabled_line(row: &VendorSpec, build: Option<u64>) -> String {
    let p = row.program;
    format!(
        "atpkg: this aterm build updates nothing (it pins no release key) — {p} {} stays; a \
         released aterm updates it",
        build_words(build)
    )
}

/// Formats the epilogue when the update child exits non-zero.
#[must_use]
pub fn incomplete_line(row: &VendorSpec, build: Option<u64>) -> String {
    let p = row.program;
    format!(
        "atpkg: {p} {} stays; `{p} {}` tries again",
        build_words(build),
        lead_verb(row)
    )
}

/// Formats the epilogue when a signal stops the update child.
#[must_use]
pub fn stopped_line(row: &VendorSpec, build: Option<u64>) -> String {
    let p = row.program;
    format!(
        "atpkg: the {p} check was stopped before it finished — {p} {} stays; `{p} {}` tries \
         again",
        build_words(build),
        lead_verb(row)
    )
}

/// Formats the epilogue when another pass holds the store lock past `bound` (exit 75).
#[must_use]
pub fn contended_line(
    row: &VendorSpec,
    build: Option<u64>,
    bound: u64,
    holder_is_person: bool,
) -> String {
    let p = row.program;
    let holder = if holder_is_person {
        "another `aterm pkg` command"
    } else {
        "another update"
    };
    format!(
        "atpkg: {holder} is still running after {} — {p} {} stays; `{p} {}` again when it \
         finishes",
        wait_words(bound),
        build_words(build),
        lead_verb(row)
    )
}

/// Formats a wait duration in human-readable seconds or minutes.
pub(crate) fn wait_words(secs: u64) -> String {
    if secs < 120 {
        let mut s = secs.to_string();
        s.push_str(" s");
        return s;
    }
    let mut s = (secs / 60).to_string();
    s.push_str(" min");
    if !secs.is_multiple_of(60) {
        s.push(' ');
        s.push_str(&(secs % 60).to_string());
        s.push_str(" s");
    }
    s
}

/// Formats the refusal line when the twin's prefix disagrees with the resolved store.
#[must_use]
pub fn prefix_mismatch_line(row: &VendorSpec, operand: &Path, resolved: &Path) -> String {
    let p = row.program;
    format!(
        "atpkg: this {p} lives under {} but atpkg here resolves {} — run `aterm pkg update \
         {p}` from a shell whose HOME (or aterm.toml) names that store",
        operand.display(),
        resolved.display()
    )
}

/// Formats the spawn failure line (exit 126).
#[must_use]
pub fn could_not_run_line(atpkg: &Path, program: &str, err: &io::Error) -> String {
    format!(
        "atpkg: could not run {} update {program}: {err} — `aterm pkg update {program}` does \
         the same check",
        atpkg.display()
    )
}

/// Formats the trailing `which` sentence for a rostered agent program.
#[must_use]
pub fn which_sentence(row: &VendorSpec) -> String {
    let p = row.program;
    format!("`{p} {}` here runs `aterm pkg update {p}`", lead_verb(row))
}

#[cfg(test)]
mod tests {
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
}
