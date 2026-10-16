// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The self-update intercept: a vendor's own self-update verb typed on the managed name
//! (`claude update`, `claude upgrade`, `claude install [latest] [--force]`, `codex update`)
//! is answered by aterm's package manager, the only thing that can move the copy that name
//! runs. Design and measurements: `docs/DESIGN-vendor-self-update-2026-09-19.md`.
//!
//! * The twin's block ([`crate::platform::sh_selfupdate_prelude`]) matches the first token
//!   exactly (`case "$1" in update|upgrade|install) … exec "$__atpkg" __selfupdate 'claude'
//!   '<prefix>' -- "$@"`): `claude -p update` is a prompt, and commander's optional-valued
//!   `--debug [filter]` swallows the `install` of `claude --debug install`, so scanning
//!   past `$1` would hijack sessions; the accepted miss is that `claude --bare update` runs
//!   the vendor's updater. The verbs come from the vendor row
//!   ([`VendorSpec::self_update_verbs`]), validated shell-safe at render; no line is a
//!   literal `exec '` or `export `, so readers of the twin's target and env are unaffected.
//!   When the co-located `atpkg` is not executable the arm falls through to the twin's own
//!   `exec`: never a stranded tool.
//! * `bin/` carries the same block (`crate::activate::reconcile_agents` lays the primary
//!   with it): outside aterm the rc hook puts only `bin/` on `PATH` (`crate::hooks`), and
//!   the vendor's updater run there would rewrite `~/.claude.json` for a native install.
//!   The verb's forward `exec`s `bin/<program>` under the same pid, and [`REENTRY_VAR`]
//!   then runs the store build instead of handing over again.
//! * The hidden verb (`cli::cmd_selfupdate`) re-checks the argv with [`classify`]. Help
//!   forwards to the vendor after one stderr note. An honoured shape prints one announce
//!   line and runs the standard update as a foreground child (`update <program>
//!   --wait-lock` [`WAIT_LOCK_SECS`], `ATPKG_SPAWNER_PID` set, stdio inherited), so the real
//!   dispatch edge runs and the child prints what a typed `aterm pkg update claude` does,
//!   plus the wait lane's markers (`cli::SEED_BUSY_MARKER`; on a terminal a spinner,
//!   `cli::lock_wait_spinner`). A held store is waited for, visibly; Ctrl-C ends the wait.
//!   Every other shape is declined on stderr with exit 2: never run silently, never handed
//!   to the vendor's updater, no bypass and no environment knob. The child's stdout line is
//!   the verdict; this module adds one stderr line only when the child did not decide
//!   (exit 1, a signal, exit 75).
//!
//! "Current" is the vendor's head (`docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md`
//! §1.7): the child runs the vendor-direct lane ([`crate::vendor_direct`]). The lines name
//! versions, never a store build id.
//!
//! Accepted exposure: a twin laid against an older co-located `atpkg` (the app downgraded)
//! answers `__selfupdate` with `unknown verb`, exit 2, until that atpkg re-lays it; a newer
//! twin against an older table forwards verbatim (`row_for` answers `None`). For agy, whose
//! updater rewrites the store build in place, the store seals a single-binary build's
//! `bin/` ([`crate::store::seal_bin`]), so its `update` refuses rather than mutate the tree.
//!
//! Scope: the agent programs ([`crate::vendor_direct::VENDORS`]). `gh` has no self-update
//! verb, `brew` manages what it installs, and none has an `agents/` twin. On Windows the
//! `.cmd` twin carries no block ([`crate::platform::twin_prelude`]): a `"%~1"=="update"`
//! line expands user text, and a `"` in an argument can end the batch with the tool never
//! run.
//!
//! Every function here is pure and builds no exit code: the relay and the verb's usage line
//! live in `cli.rs`, the file the exit-code scan in [`crate::lock`]'s tests reads. So every
//! line and verdict is pinned on every platform.

use std::ffi::OsStr;
use std::io;
use std::path::Path;

use crate::vendor_direct::VendorSpec;

/// The hidden verb the twin's block execs: `atpkg __selfupdate <program> [<abs prefix>]
/// -- [args…]`, the grammar of [`crate::landing::HandOver`]. Unlisted and dispatched before
/// the store lock, like `__landing`: the child it spawns takes the lock, and the verb
/// itself never mutates the store.
pub const HIDDEN_VERB: &str = "__selfupdate";

/// The block's re-entry guard: the block hands over as `__ATPKG_SELFUPDATE_PID=$$ exec
/// "$__atpkg" __selfupdate …`, and a block that finds this variable equal to its own `$$`
/// runs the store build instead of handing over again. The verb's forward (help, a
/// pass-through) `exec`s `bin/<program>`, which carries the block too, so without the guard
/// `claude update --help` hands over to itself forever. Hand-over and forward are both
/// `exec(2)`, so the re-entered shim has the same pid; a `claude update` typed anywhere else
/// has a pid no environment names, so the variable is no bypass a person could leave set.
pub const REENTRY_VAR: &str = "__ATPKG_SELFUPDATE_PID";

/// The child's `--wait-lock` bound in seconds: thirty minutes, the bound every scheduled
/// pass waits ([`aterm_update_core::pkg_check::PASS_WAIT_LOCK_SECS`]). A typed
/// `claude update` asks for the update, so it waits for a pass already running one, never
/// silently, and Ctrl-C ends it. No environment knob shortens it; the in-process test
/// passes its own bound through `cli::selfupdate_check`'s parameter.
pub const WAIT_LOCK_SECS: u64 = aterm_update_core::pkg_check::PASS_WAIT_LOCK_SECS;

/// `program`'s vendor row, or `None` for a non-agent program: a twin laid by a newer client
/// against this older table forwards to the tool verbatim.
#[must_use]
pub fn row_for(program: &str) -> Option<&'static VendorSpec> {
    crate::vendor_direct::spec(program)
}

/// The verbs `program`'s twin intercepts: empty for an unknown program or a row with none,
/// and an empty list renders no block ([`crate::activate::reconcile_agents`]).
#[must_use]
pub fn verbs_of(program: &str) -> &'static [&'static str] {
    row_for(program).map_or(&[], |r| r.self_update_verbs)
}

/// `^[a-z][a-z0-9-]*$` and not `esac`: a word inert inside a `sh` `case` pattern and a `#`
/// comment line. `esac` is the one such word that is not (POSIX grammar rule 4 reserves it
/// where a pattern list starts: `esac)` is a parse error in sh, bash, dash, zsh and ksh, and
/// would kill the twin on every run); `in`, `case`, `if`, `then`, `fi`, `do`, `done` and
/// `while` parse as ordinary pattern words. The renderer refuses the whole block over one
/// failing verb; the program name is held to the same rule (it lands in the note line).
#[must_use]
pub fn is_shell_safe_verb(v: &str) -> bool {
    let mut chars = v.chars();
    v != "esac"
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// What the verb does with the user's argv, decided by [`classify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict<'a> {
    /// `-h`/`--help` before any literal `--`: one stderr note, then the vendor's own
    /// help through `bin/<program>`, verbatim. Help never mutates.
    Help,
    /// Not an intercepted shape at all (`args[0]` is not one of the row's verbs, or there
    /// are no args): `bin/<program>` runs the arguments verbatim, nothing printed.
    PassThrough,
    /// A shape aterm can honour: announce, then the standard `update <program>`.
    Check {
        /// The verb typed (`update`, `upgrade`, `install`).
        verb: &'a str,
        /// The words as typed, verb first: `update`, `install latest`,
        /// `install --force latest`.
        words: String,
    },
    /// A shape aterm cannot honour: one stderr line, exit 2, nothing run.
    Declined(Decline<'a>),
}

/// Why a shape is declined — each its own sentence ([`declined_line`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decline<'a> {
    /// `install stable`: aterm installs from the vendor's `latest` and no other channel.
    NotAChannel { verb: &'a str, target: &'a str },
    /// `install 2.1.200` / `install v2.1.200`: a version of the user's choosing cannot be
    /// honoured (`aterm pkg rollback` and `pin` can).
    Version { verb: &'a str, target: &'a str },
    /// Anything else after the verb (`update now`, `install --foo`, a repeated word).
    Shape { verb: &'a str, rest: &'a [String] },
}

/// Classify the user's argv (after the twin's `--`) against `row`. The `sh` side already
/// matched `$1`; this re-checks, because a hand-typed `atpkg __selfupdate claude -- …`
/// arrives here too.
///
/// The rules, in order:
/// 1. any `-h` or `--help` before a literal `--` ⇒ [`Verdict::Help`] (a bare `help` is not:
///    commander reads it as an operand);
/// 2. `args[0]` not one of `row.self_update_verbs`, or no args ⇒ [`Verdict::PassThrough`];
/// 3. a verb other than `install` with no further token ⇒ `Check`; with any ⇒
///    `Declined(Shape)`;
/// 4. `install` whose operands are drawn from {`latest`, `--force`}, each at most once ⇒
///    `Check` with the words as typed; else the first operand not starting with `-`
///    decides: `stable` ⇒ `Declined(NotAChannel)`, a version (a leading digit, or `v` +
///    digit) ⇒ `Declined(Version)`, anything else or none ⇒ `Declined(Shape)`.
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

/// The child's argv: `update <program> --wait-lock <bound>`, the window's shape
/// (`aterm-gui`'s `run_atpkg_pass`) minus its `--progress-file`. The bound is always
/// there, so a held store is always waited for; the edge strips `--wait-lock` only for
/// `seed`, `update` and `install`.
#[must_use]
pub fn child_args(program: &str, bound: u64) -> Vec<String> {
    vec![
        String::from("update"),
        program.to_string(),
        String::from("--wait-lock"),
        bound.to_string(),
    ]
}

/// [`child_args`] for the executable at `binary`: verbatim when its file stem is `atpkg`
/// (the thin bin, or the bundle's argv0 alias: `current_exe()` through a symlink keeps the
/// symlink's name), else behind `pkg`, because the one binary's front door reads a bare
/// `update` as aterm's own app-update lane (`aterm_cli::Verb::Update`) and refuses
/// `update claude`.
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

/// The announce line, stderr, before the child: who updates the program and from whom,
/// what you have (`have`, [`crate::vendor_direct::have_words`] of the active build, read
/// offline), and that a check runs now. Nothing about the download (the child draws a
/// meter) and no claim of being latest (a pin or rollback may hold it; the child's
/// verdict says so). The words typed are not echoed: they are the line above it.
#[must_use]
pub fn announce_line(row: &VendorSpec, have: &str) -> String {
    let p = row.program;
    let (product, vendor) = (row.product, row.vendor);
    format!(
        "atpkg: aterm updates {product} from {vendor} — you have {have}; checking now: `aterm \
         pkg update {p}`"
    )
}

/// The declined line, stderr, exit 2, nothing run: one sentence per [`Decline`], user text
/// through [`echo_words`]. No sentence names a bypass: the vendor's installer is never run
/// through the managed name, whose copy it does not install.
#[must_use]
pub fn declined_line(row: &VendorSpec, d: &Decline<'_>) -> String {
    let p = row.program;
    let (product, vendor) = (row.product, row.vendor);
    // State first (`not run`), then the one action (`{verb} latest`, the direct swap for a
    // refused channel or version), then at most one caveat: the pin, or rollback for a
    // person who typed a version.
    match d {
        Decline::NotAChannel { verb, target } => format!(
            "atpkg: `{p} {verb} {target}` not run — aterm updates {product} from {vendor} and \
             installs no `{target}`; `{p} {verb} latest` checks {vendor}'s latest now (`aterm \
             pkg pin {p}` holds the version you have)"
        ),
        Decline::Version { verb, target } => format!(
            "atpkg: `{p} {verb} {}` not run — aterm updates {product} from {vendor} and takes \
             no version number; `{p} {verb} latest` checks {vendor}'s latest now (`aterm pkg \
             rollback {p}` returns to the version before)",
            echo_words(std::slice::from_ref(&(*target).to_string()))
        ),
        Decline::Shape { verb, rest } => {
            let mut typed = vec![(*verb).to_string()];
            typed.extend(rest.iter().cloned());
            format!(
                "atpkg: `{p} {}` not run — not a shape aterm's updater takes; `{p} {verb}` \
                 alone checks {vendor}'s latest now",
                echo_words(&typed)
            )
        }
    }
}

/// The help note, stderr, ahead of the vendor's own help, so an agent learns what the verb
/// does under aterm before the vendor's text describes the vendor's updater.
#[must_use]
pub fn help_line(row: &VendorSpec) -> String {
    let p = row.program;
    format!(
        "atpkg: here `{p} {}` runs `aterm pkg update {p}` (`aterm help pkg`) — the vendor's \
         own help follows",
        lead_verb(row)
    )
}

/// The disabled line, stderr, exit 1, nothing run: the manager is off because the build
/// pins no root key. The vendor's updater is not handed the verb either.
#[must_use]
pub fn disabled_line(row: &VendorSpec, build: Option<u64>) -> String {
    let p = row.program;
    format!(
        "atpkg: this aterm build updates nothing (it pins no release key) — {p} {} stays; a \
         released aterm updates it",
        build_words(build)
    )
}

/// The epilogue for a child that exited non-zero on its own (offline, a flow error, a lock
/// I/O error, a panic): not every such exit names the version it keeps, so this line does,
/// and the retry.
#[must_use]
pub fn incomplete_line(row: &VendorSpec, build: Option<u64>) -> String {
    let p = row.program;
    format!(
        "atpkg: {p} {} stays; `{p} {}` tries again",
        build_words(build),
        lead_verb(row)
    )
}

/// The epilogue for a child a signal stopped: it printed no verdict, so this line says
/// what stays, and the retry.
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

/// The epilogue for exit 75, stderr, after the child's lock sentence: another pass held the
/// store for the whole `bound` and is doing the update; the build stays until it finishes.
#[must_use]
pub fn contended_line(
    row: &VendorSpec,
    build: Option<u64>,
    bound: u64,
    holder_is_person: bool,
) -> String {
    let p = row.program;
    // On a person's terminal the child prints nothing on a timed-out wait, so this line is
    // the whole report. The holder record marks a person only for a typed verb without
    // `--wait-lock`, so an unmarked holder is named neutrally.
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

/// A wait bound as a person reads it: `N s` under two minutes, else `N min` with any
/// remainder spelled out (`2 min 30 s`), so the line never rounds the wait.
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

/// The prefix cross-check refusal, stderr, exit 2, nothing run: the twin's operand names
/// one store and `store::resolve_configured` here another (a different `HOME` or
/// `aterm.toml`), and a child `update` would move the wrong one.
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

/// The spawn failure, stderr, exit 126: `cmd_landing`'s `could not run` shape plus the
/// typed spelling of the same check.
#[must_use]
pub fn could_not_run_line(atpkg: &Path, program: &str, err: &io::Error) -> String {
    format!(
        "atpkg: could not run {} update {program}: {err} — `aterm pkg update {program}` does \
         the same check",
        atpkg.display()
    )
}

/// The `which` surface's trailing sentence for a rostered agent program: `` `claude
/// update` here runs `aterm pkg update claude` ``, separate from `ShimEnv::fix_line`, which
/// is `None` for a program that exports no env. `cli::which_line` appends it only where it
/// is true: the laid `agents/` twin is current and carries the hand-over
/// (`cli::agent_twin_intercepts`, so never the `.cmd` twin) and `agents/` is on this
/// shell's `PATH`. A shell with only `bin/` gets no sentence, an under-claim.
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

    /// Each row's verbs are `case`-pattern-safe words, unique within the row, and so is its
    /// program (the note line holds it).
    #[test]
    fn every_self_update_verb_is_a_shell_word() {
        for row in crate::vendor_direct::VENDORS {
            let mut seen = std::collections::BTreeSet::new();
            for v in row.self_update_verbs {
                assert!(is_shell_safe_verb(v), "{}: {v:?}", row.program);
                assert!(seen.insert(*v), "{}: {v} twice", row.program);
            }
            assert!(
                is_shell_safe_verb(row.program),
                "the note line holds the program"
            );
        }
        assert_eq!(verbs_of("claude"), &["update", "upgrade", "install"]);
        assert_eq!(verbs_of("codex"), &["update"]);
        assert_eq!(verbs_of("ay"), &[] as &[&str], "a non-member has no verbs");
        assert_eq!(row_for("ay"), None);
        // The word rule, spelled out.
        for ok in ["update", "upgrade", "install", "self-update", "v2"] {
            assert!(is_shell_safe_verb(ok), "{ok}");
        }
        // `esac` is the one regex-admitted word that breaks a `case` pattern.
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
        assert_eq!(HIDDEN_VERB, "__selfupdate");
        assert_eq!(
            WAIT_LOCK_SECS, 1800,
            "thirty minutes — the window's own bound, and no knob"
        );
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
        // codex: one verb; the others pass through untouched.
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
        // Without the `-h` the same argv checks.
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

    /// Every line, byte-exact, for claude and codex. Each starts `atpkg: ` (the `which`
    /// sentence, appended to a `which` row, does not), names the program only through its
    /// row, names no environment variable, and escapes and caps a hostile target.
    #[test]
    fn every_line_is_exact() {
        let c = claude();
        let x = codex();
        let v2_1_280 = crate::vendor_direct::Version::parse("2.1.280")
            .unwrap()
            .build_id();
        // The announce line: what you have, then the check running now and its verb; the
        // words typed are not echoed, and every render stays under 180 bytes.
        assert_eq!(
            announce_line(c, "2.1.280"),
            "atpkg: aterm updates Claude Code from Anthropic — you have 2.1.280; checking now: \
             `aterm pkg update claude`"
        );
        assert_eq!(
            announce_line(c, "build 2026091902"),
            "atpkg: aterm updates Claude Code from Anthropic — you have build 2026091902; \
             checking now: `aterm pkg update claude`"
        );
        assert_eq!(
            announce_line(c, "(no version recorded)"),
            "atpkg: aterm updates Claude Code from Anthropic — you have (no version recorded); \
             checking now: `aterm pkg update claude`"
        );
        assert_eq!(
            announce_line(x, "0.156.0"),
            "atpkg: aterm updates Codex CLI from OpenAI — you have 0.156.0; checking now: \
             `aterm pkg update codex`"
        );
        // `have_words` over an empty layout (no program stamp): a vendor-direct build id
        // decodes to its version, a legacy index build is `build N`, none is
        // `(no version recorded)`.
        let empty = crate::store::Layout {
            prefix: std::env::temp_dir().join(format!("atpkg-announce-{}", std::process::id())),
        };
        assert_eq!(
            crate::vendor_direct::have_words(&empty, "claude", Some(v2_1_280)),
            "2.1.280"
        );
        assert_eq!(
            crate::vendor_direct::have_words(&empty, "claude", Some(2_026_091_902)),
            "build 2026091902"
        );
        assert_eq!(
            crate::vendor_direct::have_words(&empty, "claude", None),
            "(no version recorded)"
        );
        for line in [
            announce_line(c, "2.1.280"),
            announce_line(c, "build 2026091902"),
            announce_line(c, "(no version recorded)"),
            announce_line(x, "(no version recorded)"),
        ] {
            assert!(line.len() < 180, "{}: {line}", line.len());
            assert!(
                !line.contains('`') || !line.contains("` — "),
                "no typed words echoed: {line}"
            );
        }
        assert_eq!(
            help_line(c),
            "atpkg: here `claude update` runs `aterm pkg update claude` (`aterm help pkg`) — \
             the vendor's own help follows"
        );
        assert_eq!(
            help_line(x),
            "atpkg: here `codex update` runs `aterm pkg update codex` (`aterm help pkg`) — the \
             vendor's own help follows"
        );
        assert_eq!(
            disabled_line(c, Some(v2_1_280)),
            "atpkg: this aterm build updates nothing (it pins no release key) — claude 2.1.280 \
             stays; a released aterm updates it"
        );
        assert_eq!(
            disabled_line(x, None),
            "atpkg: this aterm build updates nothing (it pins no release key) — codex (no \
             version recorded) stays; a released aterm updates it"
        );
        assert_eq!(
            declined_line(
                c,
                &Decline::NotAChannel {
                    verb: "install",
                    target: "stable"
                }
            ),
            "atpkg: `claude install stable` not run — aterm updates Claude Code from Anthropic \
             and installs no `stable`; `claude install latest` checks Anthropic's latest now \
             (`aterm pkg pin claude` holds the version you have)"
        );
        assert_eq!(
            declined_line(
                c,
                &Decline::Version {
                    verb: "install",
                    target: "2.1.200"
                }
            ),
            "atpkg: `claude install 2.1.200` not run — aterm updates Claude Code from Anthropic \
             and takes no version number; `claude install latest` checks Anthropic's latest \
             now (`aterm pkg rollback claude` returns to the version before)"
        );
        let rest = a(&["--foo"]);
        assert_eq!(
            declined_line(
                c,
                &Decline::Shape {
                    verb: "update",
                    rest: &rest
                }
            ),
            "atpkg: `claude update --foo` not run — not a shape aterm's updater takes; `claude \
             update` alone checks Anthropic's latest now"
        );
        let rest = a(&["now"]);
        assert_eq!(
            declined_line(
                x,
                &Decline::Shape {
                    verb: "update",
                    rest: &rest
                }
            ),
            "atpkg: `codex update now` not run — not a shape aterm's updater takes; `codex \
             update` alone checks OpenAI's latest now"
        );
        assert_eq!(
            incomplete_line(c, Some(v2_1_280)),
            "atpkg: claude 2.1.280 stays; `claude update` tries again"
        );
        assert_eq!(
            incomplete_line(x, None),
            "atpkg: codex (no version recorded) stays; `codex update` tries again"
        );
        assert_eq!(
            stopped_line(c, Some(v2_1_280)),
            "atpkg: the claude check was stopped before it finished — claude 2.1.280 stays; \
             `claude update` tries again"
        );
        // A legacy index build, not yet replaced, keeps its number.
        assert_eq!(
            stopped_line(c, Some(2_026_091_902)),
            "atpkg: the claude check was stopped before it finished — claude build 2026091902 \
             stays; `claude update` tries again"
        );
        assert_eq!(
            contended_line(c, Some(v2_1_280), WAIT_LOCK_SECS, false),
            "atpkg: another update is still running after 30 min — claude 2.1.280 stays; \
             `claude update` again when it finishes"
        );
        assert_eq!(
            contended_line(x, None, 1, true),
            "atpkg: another `aterm pkg` command is still running after 1 s — codex (no version \
             recorded) stays; `codex update` again when it finishes"
        );
        // No line over 230 bytes, and none names the mechanism.
        for line in [
            declined_line(
                c,
                &Decline::Version {
                    verb: "install",
                    target: "2.1.200",
                },
            ),
            disabled_line(c, Some(v2_1_280)),
            incomplete_line(c, Some(v2_1_280)),
            stopped_line(c, Some(2_026_091_902)),
            contended_line(c, Some(v2_1_280), WAIT_LOCK_SECS, true),
            help_line(c),
        ] {
            assert!(line.len() <= 230, "{}: {line}", line.len());
            assert!(
                !line.contains("package manager") && !line.contains("moving packages"),
                "no mechanism words: {line}"
            );
        }
        // The bound as a person reads it, the remainder never rounded.
        for (secs, words) in [
            (1, "1 s"),
            (60, "60 s"),
            (119, "119 s"),
            (120, "2 min"),
            (150, "2 min 30 s"),
            (1800, "30 min"),
        ] {
            assert_eq!(wait_words(secs), words, "{secs}");
        }
        assert_eq!(
            prefix_mismatch_line(
                c,
                Path::new("/Users//u/Library/Application Support/aterm/pkg"),
                Path::new("/opt/aterm/pkg")
            ),
            "atpkg: this claude lives under /Users//u/Library/Application Support/aterm/pkg \
             but atpkg here resolves /opt/aterm/pkg — run `aterm pkg update claude` from a \
             shell whose HOME (or aterm.toml) names that store"
        );
        let err = io::Error::new(io::ErrorKind::NotFound, "No such file or directory");
        assert_eq!(
            could_not_run_line(
                Path::new("/Applications/aterm.app/Contents/MacOS/atpkg"),
                "claude",
                &err
            ),
            "atpkg: could not run /Applications/aterm.app/Contents/MacOS/atpkg update claude: \
             No such file or directory — `aterm pkg update claude` does the same check"
        );
        assert_eq!(
            which_sentence(c),
            "`claude update` here runs `aterm pkg update claude`"
        );
        assert_eq!(
            which_sentence(x),
            "`codex update` here runs `aterm pkg update codex`"
        );
        // Hostile echoes: a newline and quotes are `{:?}`-escaped; 200 bytes are capped
        // at 64 plus `…`; the plain shape stays plain.
        let hostile = "2.1\n\"x\"";
        let line = declined_line(
            c,
            &Decline::Version {
                verb: "install",
                target: hostile,
            },
        );
        assert!(
            line.contains("`claude install \"2.1\\n\\\"x\\\"\"`"),
            "{line}"
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
        assert!(
            line.contains(&format!("`claude install {}…`", "9".repeat(64))),
            "{line}"
        );
        assert!(!line.contains(&"9".repeat(65)), "{line}");
        let rest = a(&["--foo", "a b", "ünïcode"]);
        let line = declined_line(
            c,
            &Decline::Shape {
                verb: "update",
                rest: &rest,
            },
        );
        assert!(
            line.starts_with(
                "atpkg: `claude update --foo \"a b\" \"ünïcode\"` not run — not a shape"
            ),
            "{line}"
        );
        assert_eq!(echo_words(&a(&["plain", "--flag=1"])), "plain --flag=1");
        assert_eq!(echo_words(&[]), "");
        // The cap lands on a char boundary, never inside a multi-byte sequence.
        let capped = echo_words(&a(&[&"é".repeat(40)]));
        assert!(capped.ends_with('…'), "{capped}");
        assert!(capped.len() <= ECHO_CAP + '…'.len_utf8(), "{capped}");
        // Every stderr line starts with the prefix; none names a program by literal, and
        // none names an environment variable.
        let rest = a(&["now"]);
        for line in [
            announce_line(x, "(no version recorded)"),
            help_line(x),
            disabled_line(x, None),
            incomplete_line(x, None),
            stopped_line(x, None),
            contended_line(x, None, 5, false),
            prefix_mismatch_line(x, Path::new("/a"), Path::new("/b")),
            could_not_run_line(Path::new("/a"), "codex", &err),
            declined_line(
                x,
                &Decline::Shape {
                    verb: "update",
                    rest: &rest,
                },
            ),
            declined_line(
                x,
                &Decline::NotAChannel {
                    verb: "install",
                    target: "stable",
                },
            ),
            declined_line(
                x,
                &Decline::Version {
                    verb: "install",
                    target: "1.2.3",
                },
            ),
        ] {
            assert!(line.starts_with("atpkg: "), "{line}");
            assert!(
                !line.contains("claude"),
                "codex's line names no other program: {line}"
            );
            assert!(
                !line.contains("ATPKG_") && !line.contains("=1"),
                "no line names an environment variable or a knob: {line}"
            );
        }
        assert!(contended_line(x, None, 5, false).contains("after 5 s "));
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
        // An `atpkg`-named executable takes the argv verbatim; any other takes it behind
        // `pkg`, because the front door reads a bare `update` as aterm's app-update lane.
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
