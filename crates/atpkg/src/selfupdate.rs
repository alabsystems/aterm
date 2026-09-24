// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE SELF-UPDATE INTERCEPT (2026-09-19): a vendor's own self-update verb typed on the
//! MANAGED name — `claude update`, `claude upgrade`, `claude install [latest] [--force]`,
//! `codex update` — is answered by aterm's package manager, which is the only thing that
//! can move the copy that name runs.
//!
//! Owner, 2026-09-19, verbatim: *"when executing `claude install latest` or similar for
//! codex and other aterm atpkg managed programs, aterm reports that these packages are
//! automatically managed by atpkg to keep to the latest version (and then does a check to
//! make sure that they are updated and then actually updates) via the standard pkg
//! manager."*
//!
//! # What was true before (measured 2026-09-19)
//!
//! * The managed `claude` (2.1.278) carries `install [options] [target]` — target
//!   `stable`, `latest` or a version, option `--force` — and `update|upgrade` (a
//!   commander alias: `claude upgrade --help` prints `Usage: claude update|upgrade`).
//!   Its shim exports only `DISABLE_AUTOUPDATER=1`, which stops the BACKGROUND check and
//!   nothing else: `DISABLE_AUTOUPDATER=1 claude update` printed `Claude Code is up to
//!   date (2.1.278)` and exited 0, and `agents/claude update` — the twin, the copy every
//!   shell runs — printed the same. So with upstream ahead of the signed pin the vendor's
//!   updater would have written `~/.local/share/claude/versions/<v>` and repointed
//!   `~/.local/bin/claude`: a copy this name never runs (`docs/TOOLCHAIN-PACKAGE-MANAGER.md`
//!   §17.12), wasted bandwidth, and a `claude doctor` complaint — the vendor's verb did
//!   the opposite of what the person typing it meant.
//! * `codex` has exactly one such verb, `update` (`codex-rs/cli/src/main.rs`,
//!   `Subcommand::Update`, no alias, no flags, read 2026-09-19). On the store copy it
//!   bails — the path matches no install method it knows (`InstallMethod::Other`) — with
//!   a sentence pointing at the vendor's own channels. It was not run on this machine
//!   (`codex` hangs off a TTY here), so its exact stderr is inferred from source, never
//!   quoted by a test.
//!
//! # The mechanism
//!
//! Two halves, the shape of the retired landing hand-over ([`crate::landing`]):
//!
//! * **The twin's block** ([`crate::platform::sh_selfupdate_prelude`]), the whole of the
//!   twin's prelude: `case "$1" in update|upgrade|install) … exec "$__atpkg"
//!   __selfupdate 'claude' '<prefix>' -- "$@"; …; esac`. THE FIRST TOKEN, EXACTLY, and
//!   nothing else: `claude -p update` is a prompt, and `claude --debug install` is not an
//!   install at all — commander's `--debug [filter]` takes an OPTIONAL value and swallows
//!   `install` as the filter (measured: it prints the top-level usage). Any scan past `$1`
//!   would hijack a session whose first word happens to be `update`, which is far worse
//!   than the recorded false negative: `claude --bare update` runs the vendor's updater
//!   as before this existed. The verbs are DATA from [`ROSTER`], validated shell-safe at
//!   render; the program and prefix are single-quoted; `"$@"` is verbatim; no line of
//!   the block is a literal `exec '` or an `export `, so every reader of the twin's
//!   target and env answers as before. THE EXEC IS THE GUARANTEE: when the embedded
//!   co-located `atpkg` is not executable the arm falls through to the twin's own
//!   exports and store `exec`, and the vendor's verb runs as it did before (review,
//!   2026-09-16), never a stranded tool.
//! * **The hidden verb** (`cli::cmd_selfupdate`): re-checks the argv in Rust with
//!   [`classify`] — help never mutates (`-h`/`--help` forwards to the vendor after one
//!   stderr note), a shape aterm can honour prints ONE announce line and runs the
//!   STANDARD update as a foreground child (`<embedded atpkg> update <program>
//!   --wait-lock 1800` — [`WAIT_LOCK_SECS`], the window's own bound — with
//!   `ATPKG_SPAWNER_PID` set and stdio inherited) so the real dispatch edge — the store
//!   lock, the orphan watch — is what runs and
//!   the child's own lines are byte-identical to a typed `aterm pkg update claude` —
//!   plus, because the child is a `--wait-lock` caller, the wait lane's stdout markers
//!   a typed verb never prints (measured 2026-09-19 under a held lock: `atpkg:
//!   lock-waiting: …` after the 2 s grace, `atpkg: lock-acquired: …` when the holder
//!   lets go, `atpkg: seed-busy: …` at the 75 ending; `cli::SEED_BUSY_MARKER` records
//!   the exception). A store another pass holds is WAITED FOR, the whole 30 minutes —
//!   a typed `claude update` asks FOR the update, so it waits for a pass already running
//!   it, never silently (those markers), and Ctrl-C ends the wait. Every other shape
//!   (`install stable`, `install 2.1.200`, an unknown token) is DECLINED on stderr with
//!   exit 2 — never run silently, and never handed to the vendor's own updater, which
//!   installs a copy this name never runs; there is no bypass and no environment knob
//!   (owner, 2026-09-19: *"remove all these env flags so that aterm works correctly by
//!   default"*). The child's own stdout line is the verdict an agent reads (`atpkg:
//!   claude 2.1.278 → 2.1.280 (Anthropic latest)` / `atpkg: claude 2.1.280 is Anthropic's
//!   latest` / `held by local pin`); this module adds exactly one stderr line only when
//!   the child did not decide (exit 1 → the version you have stays, how to retry; exit
//!   75 → another pass held the store for the whole wait and is the one moving packages).
//!
//! "Current" is THE VENDOR'S HEAD (`docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md`
//! §1.7): the child runs the vendor-direct lane ([`crate::vendor_direct`]), which reads
//! the vendor's `latest` pointer, verifies the build on this client under the vendor's
//! own anchors, and installs it — no ALab index sits in between. The lines name
//! versions (the `.vendor` record's), never a store build id.
//!
//! # Exposure accepted, like `__landing`'s
//!
//! A twin laid by THIS client against an OLDER co-located `atpkg` (the app downgraded
//! before its first pass re-lays the twin) answers `__selfupdate` with `unknown verb`
//! exit 2 — loud, non-mutating, only for a self-update verb (a plain `claude` never
//! touches the arm), and self-healing: that older atpkg's first `reconcile_agents` re-lays
//! the twin without the block. The reverse — a NEWER twin's roster against this verb's
//! older one — forwards to the tool verbatim (`row_for` answers `None`).
//!
//! # Scope
//!
//! [`crate::stub::AGENT_PROGRAMS`] only, one [`Row`] each, pinned by a test in both
//! directions. Out by argument: `gh` has no self-update verb (`gh upgrade` is unknown;
//! `gh extension upgrade` moves extensions), `emacs` none, `brew update`/`upgrade` IS the
//! sanctioned manager of an OS-installed, system-satisfied member, and the Command Line
//! Tools are Apple's `softwareupdate`'s. None of them has an `agents/` twin to intercept
//! in anyway. A future vendor member joins by one row.
//!
//! # Windows: TARGET, rendered empty
//!
//! The `.cmd` twin is byte-unchanged ([`crate::platform::cmd_selfupdate_prelude`] answers
//! `""` on every input, with the intended text in its doc): a `"%~1"=="update"` line
//! expands user text on a batch line, and an argument carrying a `"` can end the batch
//! with the tool never run — the strand the Unix rule forbids — on a platform no machine
//! of this repo runs. The Rust half is platform-neutral, so landing the segment later is
//! a renderer change.
//!
//! Every function here is PURE — no process, no I/O, no exit-code value of its own (the
//! exit-code registry scan in [`crate::lock`] reads `cli.rs`, where the relay is literal
//! arms) and no usage-line literal (the help-surfaces gate discovers files by that word;
//! the verb's usage line lives in `cli.rs` beside `cmd_landing`'s) — so every line and
//! every verdict is pinned on every platform.

use std::ffi::OsStr;
use std::io;
use std::path::Path;

/// The hidden verb the twin's block execs: `atpkg __selfupdate <program> [<abs prefix>]
/// -- [args…]` — the grammar of [`crate::landing::HandOver`], reused verbatim. Unlisted,
/// dispatched before the verb match and before the store lock, like `__landing`: the
/// CHILD it spawns takes the lock through the real dispatch edge; the verb itself never
/// mutates the store.
pub const HIDDEN_VERB: &str = "__selfupdate";

/// The child's `--wait-lock` bound, in seconds: THIRTY MINUTES, a constant — the one
/// every scheduled pass waits ([`aterm_update_core::pkg_check::PASS_WAIT_LOCK_SECS`]: the
/// window's `ATPKG_WAIT_LOCK_SECS` and the terminal session's pass bind it too, and
/// `no_environment_knob_reaches_the_intercept` reads the window's source to pin that). A
/// typed `claude update` asks FOR the update, so it waits for a pass already running it —
/// the window loop's, a landing, a typed `aterm pkg` verb — and the wait is never silent:
/// the child prints `lock-waiting:` after the 2 s grace and `lock-acquired:` when it gets
/// the store, and Ctrl-C ends it. There is no environment knob for it (owner, 2026-09-19: *"remove all
/// these env flags so that aterm works correctly by default"*); an in-process test
/// drives the 75 ending with a 1 s bound through `cli::selfupdate_check`'s parameter.
pub const WAIT_LOCK_SECS: u64 = aterm_update_core::pkg_check::PASS_WAIT_LOCK_SECS;

/// One rostered agent program: the verbs its twin intercepts and the measurement the row
/// rests on. Its lines name the product and vendor from [`crate::vendor_direct::VENDORS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// The program, as [`crate::stub::AGENT_PROGRAMS`] spells it.
    pub program: &'static str,
    /// The vendor's self-update verbs, exactly as typed as `$1`; each must pass
    /// [`is_shell_safe_verb`] (it lands in a `case` pattern). EMPTY means "this member
    /// has no self-updater and its twin carries no block" — an explicit decision, never
    /// a missing row (the coherence test forces a row per agent program).
    pub verbs: &'static [&'static str],
    /// Where the verbs come from — the measurement, dated, so a reader of the table can
    /// tell a fact from a guess.
    pub measured: &'static str,
}

/// The roster. Programs in [`crate::stub::AGENT_PROGRAMS`]' order, one row each — pinned
/// both ways by `the_roster_is_the_agent_programs_and_every_verb_is_a_shell_word`.
pub const ROSTER: &[Row] = &[
    Row {
        program: "claude",
        verbs: &["update", "upgrade", "install"],
        measured: "claude 2.1.278 `--help`, 2026-09-19: `install [options] [target]` \
                   (stable|latest|<version>; --force), `update|upgrade`; \
                   DISABLE_AUTOUPDATER=1 leaves `claude update` live (measured exit 0, \
                   `up to date`)",
    },
    Row {
        program: "codex",
        verbs: &["update"],
        measured: "openai/codex codex-rs/cli/src/main.rs `Subcommand::Update` (no alias, \
                   no flags), read 2026-09-19; not run on this box (codex hangs off a TTY)",
    },
];

/// The row for `program`, or `None` for a program that is not rostered — a twin laid by
/// a NEWER client against this older roster forwards to the tool verbatim.
#[must_use]
pub fn row_for(program: &str) -> Option<&'static Row> {
    ROSTER.iter().find(|r| r.program == program)
}

/// The verbs `program`'s twin intercepts; EMPTY for an unknown program and for a member
/// whose row says it has none — what [`crate::activate::reconcile_agents`] hands the
/// renderer, which renders no block for an empty list.
#[must_use]
pub fn verbs_of(program: &str) -> &'static [&'static str] {
    row_for(program).map_or(&[], |r| r.verbs)
}

/// `^[a-z][a-z0-9-]*$` and not `esac` — a word that is inert inside a `sh` `case`
/// pattern (no glob character, no `|`, no `)`, no quote, no space) and inside a `#`
/// comment line. `esac` is the ONE word the regex admits that is not inert there (POSIX
/// shell grammar rule 4 reserves it where a pattern list starts): measured 2026-09-19,
/// `case "$1" in\n  esac)` is a parse error in /bin/sh, bash, dash, zsh and ksh, so the
/// whole twin would die at line 3 on EVERY invocation — the stranded tool the renderer's
/// fail-closed rule exists to forbid — while `in`, `case`, `if`, `then`, `fi`, `do`,
/// `done` and `while` all parse and match as ordinary pattern words (the same run), so
/// they are not refused. The renderer refuses a whole block over one verb that fails
/// this, fail-closed; the program name is held to the same rule because it lands in the
/// note line too.
#[must_use]
pub fn is_shell_safe_verb(v: &str) -> bool {
    let mut chars = v.chars();
    v != "esac"
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// What the verb does with the user's argv (after the twin's `--`), decided by
/// [`classify`] and nothing else.
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
        /// The words the announce line echoes, as typed, verb first: `update`,
        /// `install latest`, `install --force latest`.
        words: String,
    },
    /// A shape aterm cannot honour: one stderr line, exit 2, nothing run.
    Declined(Decline<'a>),
}

/// Why a shape is declined — each its own sentence ([`declined_line`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decline<'a> {
    /// `install stable`: aterm updates the program from the vendor's `latest` and installs
    /// no other channel.
    NotAChannel { verb: &'a str, target: &'a str },
    /// `install 2.1.200` / `install v2.1.200`: aterm updates from the vendor's latest; a
    /// version of the user's choosing cannot be honoured (`aterm pkg rollback` and `pin`
    /// can).
    Version { verb: &'a str, target: &'a str },
    /// Anything else after the verb (`update now`, `install --foo`, a repeated word):
    /// not a shape the updater answers, and not one to hand to the vendor's installer
    /// silently either.
    Shape { verb: &'a str, rest: &'a [String] },
}

/// Classify the user's argv (AFTER the twin's `--`) against `row` — pure, on every
/// platform. The `sh` side already guaranteed `$1` is one of the row's verbs; this
/// re-checks, because a hand-typed `atpkg __selfupdate claude -- …` arrives here too.
///
/// The rules, in order:
/// 1. any `-h` or `--help` among the arguments BEFORE a literal `--` ⇒ [`Verdict::Help`]
///    (a bare word `help` is NOT help: commander reads it as an operand);
/// 2. `args[0]` not one of `row.verbs`, or no args ⇒ [`Verdict::PassThrough`];
/// 3. `update` / `upgrade` with no further token ⇒ `Check`; with any ⇒ `Declined(Shape)`;
/// 4. `install` whose operands are drawn only from the closed set {`latest`, `--force`},
///    each at most once ⇒ `Check` with the words as typed (a bare `install` is a check of
///    the vendor's latest; Claude's own default target for a bare `install` was NOT
///    measured); else
///    the first operand not starting with `-` decides: `stable` ⇒ `Declined(NotAChannel)`,
///    a version (a leading digit, or `v` + digit) ⇒ `Declined(Version)`, anything else or
///    none ⇒ `Declined(Shape)`.
///
/// Nothing here names a program: the verbs come from the row, and the `install` grammar
/// belongs to the verb `install` wherever a row carries it.
#[must_use]
pub fn classify<'a>(row: &Row, args: &'a [String]) -> Verdict<'a> {
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
    if !row.verbs.contains(&verb) {
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

/// The child's argv: `update <program> --wait-lock <bound>` — the window's shape
/// (`aterm-gui`'s `run_atpkg_pass`) minus `--progress-file`, which is the window's
/// byte-parsed channel and never a terminal's. `bound` is [`WAIT_LOCK_SECS`] in
/// production and a test's own seconds in a test; the pair is ALWAYS there, so a held
/// store is always waited for. The edge strips the flag only when `args[0]` is
/// `seed|update|install`, which `update` is.
#[must_use]
pub fn child_args(program: &str, bound: u64) -> Vec<String> {
    vec![
        String::from("update"),
        program.to_string(),
        String::from("--wait-lock"),
        crate::dec_u64(bound),
    ]
}

/// [`child_args`] as the executable at `binary` must be handed them: verbatim when its
/// file stem is `atpkg` — the thin bin, the bundle's `atpkg` argv0 alias (the one binary
/// dispatches on argv0, and `current_exe()` through a symlink keeps the symlink's name,
/// measured 2026-09-19) — and behind the `pkg` verb otherwise. The one shipped binary is
/// `aterm`, and its front door reads a bare `update` as aterm's OWN app-update lane
/// (`aterm_cli::Verb::Update`), never as a package verb: measured on the installed
/// bundle, `aterm update claude --wait-lock 60` answers `aterm: unknown update
/// sub-command "claude"` and that lane's own operands (`status|check`), exit 2 — so a
/// sibling-less `aterm` (a dev build copied out of `target/`, a bundle missing the alias
/// symlink) whose verb fell back to `current_exe()` announced a check that never ran and
/// relayed that usage refusal as the update's status (review finding). Any other name
/// is the front door too, so `pkg` is the rule and `atpkg` the one exception.
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

/// The longest echo of user-typed text any line here carries, in bytes; the rest is
/// `…`. A 200-byte "version" is a hostile or mistyped argument either way, and a TTY line
/// is not the place to reproduce it.
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

/// The row's canonical verb spelling for the lines that name ONE verb (`claude update`
/// here runs …): the first verb of the row — `update` for both members today.
fn lead_verb(row: &Row) -> &'static str {
    row.verbs.first().copied().unwrap_or("update")
}

/// The version the store's active build is (`2.1.280`; a legacy index build `build N`),
/// or `(no version recorded)` when the layout has no active build of the program — offline
/// words (`ops::active_builds` reads the `bin/` shims).
fn build_words(build: Option<u64>) -> String {
    build.map_or_else(
        || String::from("(no version recorded)"),
        crate::vendor_direct::build_words,
    )
}

/// `(product, vendor)` for the row's program — `("Claude Code", "Anthropic")` — from the
/// vendor table; the program itself and `its vendor` for a row the table lacks (the
/// roster and the table are the same programs, pinned by a test).
fn keeper(row: &Row) -> (&'static str, &'static str) {
    crate::vendor_direct::spec(row.program)
        .map_or((row.program, "its vendor"), |s| (s.product, s.vendor))
}

/// The announce line, stderr, before the child: `` atpkg: aterm updates Claude Code from
/// Anthropic — you have 2.1.278; checking now: `aterm pkg update claude` (a new version
/// downloads silently, ~1 min) `` — facts in the order a person needs them (owner,
/// 2026-09-22: *"more concise and with facts, like the current version, that this triggers
/// a check"*): who updates it and from whom, what you HAVE (`have` is
/// [`crate::vendor_direct::have_words`] of the active build, read offline before the
/// child: the version of a vendor-direct build, the version an earlier pass recorded for
/// a legacy index build, else `build N`; `(no version recorded)` with none), that a check
/// is running NOW and the verb that runs it, and the one thing that would otherwise
/// misread — a download that is SILENT on a terminal (no progress sink without
/// `--progress-file`; 200 MB must not read as a hang). No claim that the running copy is
/// the latest: a pin or a rollback may hold it, and the child's verdict line says so.
/// The words typed are not echoed: they are the line above this one in every transcript.
#[must_use]
pub fn announce_line(row: &Row, have: &str) -> String {
    let p = row.program;
    let (product, vendor) = keeper(row);
    format!(
        "atpkg: aterm updates {product} from {vendor} — you have {have}; checking now: `aterm \
         pkg update {p}` (a new version downloads silently, ~1 min)"
    )
}

/// The declined line, stderr, exit 2, nothing run — one sentence per [`Decline`]. The
/// target and the argv are echoed through the escape-and-cap rule ([`echo_words`]). No
/// sentence names a bypass: the vendor's own installer is never run through the managed
/// name, because the copy it installs is one this name never runs — and there is no
/// environment knob that would (owner, 2026-09-19).
#[must_use]
pub fn declined_line(row: &Row, d: &Decline<'_>) -> String {
    let p = row.program;
    let (product, vendor) = keeper(row);
    match d {
        Decline::NotAChannel { verb, target } => format!(
            "atpkg: `{p} {verb} {target}` — aterm updates {product} from {vendor} and installs \
             no `{target}`; `{p} {verb}` (or `{p} {verb} latest`) checks {vendor}'s latest \
             now, and `aterm pkg pin {p}` holds the version you have"
        ),
        Decline::Version { verb, target } => format!(
            "atpkg: `{p} {verb} {}` — aterm updates {product} from {vendor} and cannot install \
             a version of your choosing; `{p} {verb}` (or `{p} {verb} latest`) checks \
             {vendor}'s latest now, `aterm pkg rollback {p}` returns to the version before, \
             and `aterm pkg pin {p}` holds the version you have",
            echo_words(std::slice::from_ref(&(*target).to_string()))
        ),
        Decline::Shape { verb, rest } => {
            let mut typed = vec![(*verb).to_string()];
            typed.extend(rest.iter().cloned());
            format!(
                "atpkg: `{p} {}` is not a shape aterm's updater answers — `{p} {verb}` checks \
                 {vendor}'s latest now",
                echo_words(&typed)
            )
        }
    }
}

/// The help note, stderr, then the vendor's own help: `` atpkg: on this copy `claude
/// update` is answered by `aterm pkg update claude` (aterm help pkg) — the vendor's own
/// help follows `` — so an agent reading the output learns what the verb does under
/// aterm before the vendor's text describes the vendor's updater.
#[must_use]
pub fn help_line(row: &Row) -> String {
    let p = row.program;
    format!(
        "atpkg: on this copy `{p} {}` is answered by `aterm pkg update {p}` (aterm help pkg) \
         — the vendor's own help follows",
        lead_verb(row)
    )
}

/// The disabled line, stderr, exit 1, nothing run: the manager is off (a build that pins
/// no root key), so nothing is checked — and the vendor's updater
/// is NOT handed the verb either, because the copy it would install is one this name
/// never runs; `aterm pkg doctor` says why the manager is off.
#[must_use]
pub fn disabled_line(row: &Row, build: Option<u64>) -> String {
    let p = row.program;
    format!(
        "atpkg: the aterm package manager is disabled here — nothing checked; {p} {} stays \
         in place; `aterm pkg doctor` says why",
        build_words(build)
    )
}

/// The epilogue for a child that exited 1 (offline, disabled inside the child, any flow
/// error), stderr, after the child's own failure line: the version you have stays, and how
/// to retry. (The typed lane's offline sentence — `no signature-valid index at/above the
/// floor` — is pre-existing and reads as a trust problem; this line is what says what
/// actually happened to the copy.)
#[must_use]
pub fn incomplete_line(row: &Row, build: Option<u64>) -> String {
    let p = row.program;
    format!(
        "atpkg: the {p} check did not complete (see above) — {p} {} stays in place; `aterm \
         pkg update {p}` retries it and `aterm pkg doctor` explains",
        build_words(build)
    )
}

/// The epilogue for exit 75, stderr, after the child's lock sentence: another pass — the
/// window's own update, or a typed verb — held the store for the whole `bound`
/// ([`wait_words`]: `30 min` for the production [`WAIT_LOCK_SECS`]) and IS the one
/// moving packages; the build stays until it finishes. What turns "refusing to mutate
/// the store concurrently" into "the update you asked for is already running".
#[must_use]
pub fn contended_line(row: &Row, build: Option<u64>, bound: u64) -> String {
    let p = row.program;
    format!(
        "atpkg: another atpkg pass held the store for the whole {} wait — that pass (the \
         window's own update, or a typed `aterm pkg` verb) is the one moving packages; {p} \
         {} stays in place until it finishes, then `{p} {}` again",
        wait_words(bound),
        build_words(build),
        lead_verb(row)
    )
}

/// A wait bound as a person reads it: `N s` under two minutes, else `N min` — `30 min`
/// for [`WAIT_LOCK_SECS`], `1 s` for the in-process test's bound — with a remainder
/// spelled out (`2 min 30 s`), so the line never rounds what the child actually waited.
fn wait_words(secs: u64) -> String {
    if secs < 120 {
        let mut s = crate::dec_u64(secs);
        s.push_str(" s");
        return s;
    }
    let mut s = crate::dec_u64(secs / 60);
    s.push_str(" min");
    if !secs.is_multiple_of(60) {
        s.push(' ');
        s.push_str(&crate::dec_u64(secs % 60));
        s.push_str(" s");
    }
    s
}

/// The prefix cross-check refusal, stderr, exit 2, nothing run: the twin's operand names
/// one store, `store::resolve_configured` in this environment names another (a shell
/// whose `HOME` or `aterm.toml` differs from the one the twin was laid for) — and a
/// child `update` would move THAT store, not the one this twin runs.
#[must_use]
pub fn prefix_mismatch_line(row: &Row, operand: &Path, resolved: &Path) -> String {
    let p = row.program;
    format!(
        "atpkg: this {p} lives under {} but atpkg here resolves {} — run `aterm pkg update \
         {p}` from a shell whose HOME (or aterm.toml) names that store",
        operand.display(),
        resolved.display()
    )
}

/// The spawn failure, stderr, exit 126 — the `could not run` shape `cmd_landing` prints
/// for a failed exec, plus the typed spelling of the same check.
#[must_use]
pub fn could_not_run_line(atpkg: &Path, program: &str, err: &io::Error) -> String {
    format!(
        "atpkg: could not run {} update {program}: {err} — `aterm pkg update {program}` does \
         the same check",
        atpkg.display()
    )
}

/// The `which` surface's trailing sentence for a rostered agent program: `` `claude
/// update` here runs `aterm pkg update claude` `` — appended for every rostered member
/// whether or not its shim exports an env (codex exports none, so `ShimEnv::fix_line`
/// is `None` there and could not carry it). "Here" is shell-local, and `cli::which_line`
/// appends it only where it is true (review, 2026-09-19): the laid `agents/` twin is
/// current AND carries the hand-over (`cli::agent_twin_intercepts` — so never the `.cmd`
/// twin, never a twin from before the block) AND `agents/` is on THIS shell's `PATH`; a
/// shell whose `PATH` holds only `bin/` runs `bin/<program>`, which carries no block, and
/// `claude update` there is the vendor's updater.
#[must_use]
pub fn which_sentence(row: &Row) -> String {
    let p = row.program;
    format!("`{p} {}` here runs `aterm pkg update {p}`", lead_verb(row))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    fn claude() -> &'static Row {
        row_for("claude").unwrap()
    }

    fn codex() -> &'static Row {
        row_for("codex").unwrap()
    }

    /// THE ROSTER IS THE AGENT PROGRAMS, both directions, in the same order — a new
    /// agent member must decide here whether it has a self-updater (a row with
    /// `verbs: &[]` says "no" out loud). Every verb is a `case`-pattern-safe word, unique
    /// within its row, and every row carries a dated measurement.
    #[test]
    fn the_roster_is_the_agent_programs_and_every_verb_is_a_shell_word() {
        let rostered: Vec<&str> = ROSTER.iter().map(|r| r.program).collect();
        assert_eq!(
            rostered,
            crate::stub::AGENT_PROGRAMS,
            "same members, same order"
        );
        for row in ROSTER {
            assert!(
                crate::stub::is_agent_program(row.program),
                "{} is not an agent program",
                row.program
            );
            assert!(
                !row.verbs.is_empty(),
                "{}: verbs (today both have one)",
                row.program
            );
            let mut seen = std::collections::BTreeSet::new();
            for v in row.verbs {
                assert!(is_shell_safe_verb(v), "{}: {v:?}", row.program);
                assert!(seen.insert(*v), "{}: {v} twice", row.program);
            }
            assert!(
                is_shell_safe_verb(row.program),
                "the note line holds the program"
            );
            assert!(
                row.measured.contains("2026-09-19"),
                "{}: dated",
                row.program
            );
            assert!(
                crate::vendor_direct::spec(row.program).is_some(),
                "{}: its lines name the vendor table's product and vendor",
                row.program
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
        // `esac` is the one regex-admitted word that breaks a `case` pattern (measured
        // 2026-09-19: a parse error in sh, bash, dash, zsh and ksh, the tool never run).
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

    /// THE CLASSIFICATION TABLE, one assertion per rule of [`classify`]'s doc, negative
    /// controls included: the first token exactly (`--bare update`, `-p update`,
    /// `-- update` and the word `help` all pass through), help before every shape
    /// (`upgrade -x --help`), a `--help` behind the user's own `--` is data (and
    /// `update` takes no operands, so it is declined), the closed `install` set each at
    /// most once, `stable` a channel, a digit or `v`+digit a version, anything else a
    /// shape — and codex, whose row carries only `update`.
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
        // A NON-VACUITY control on the help rule: without the `-h` the same argv checks.
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

    /// EVERY LINE, BYTE-EXACT, for claude and codex: the announce for the three typed
    /// shapes, the three declines, help, disabled, incomplete, contended (the production
    /// bound 1800 rendered `30 min`, a test's 1 rendered `1 s`, build Some and None),
    /// prefix mismatch, could-not-run, and the `which` sentence. Every line starts
    /// `atpkg: ` (the `which` sentence is the one that does not: it is appended to a
    /// `which` row, which carries no prefix), names the program only through its row,
    /// names NO environment variable (owner, 2026-09-19: no env knobs; the first cut's
    /// declined and disabled lines advertised an escape hatch and its contended line a
    /// wait knob, and both are gone), and a hostile target — a newline, quotes, 200
    /// bytes — is escaped and capped before it reaches a terminal.
    #[test]
    fn every_line_is_exact() {
        let c = claude();
        let x = codex();
        let v2_1_280 = crate::vendor_direct::Version::parse("2.1.280")
            .unwrap()
            .build_id();
        // The announce line: what you HAVE, from the active build alone — the version of
        // a vendor-direct build, `build N` for a legacy index build, `(no version
        // recorded)` with none — then the check running now and its verb; the words
        // typed are not echoed. Under 180 bytes in every render (the 2026-09-19 line
        // was 315, the 2026-09-22 one 200).
        assert_eq!(
            announce_line(c, "2.1.280"),
            "atpkg: aterm updates Claude Code from Anthropic — you have 2.1.280; checking now: \
             `aterm pkg update claude` (a new version downloads silently, ~1 min)"
        );
        assert_eq!(
            announce_line(c, "build 2026091902"),
            "atpkg: aterm updates Claude Code from Anthropic — you have build 2026091902; \
             checking now: `aterm pkg update claude` (a new version downloads silently, ~1 min)"
        );
        assert_eq!(
            announce_line(c, "(no version recorded)"),
            "atpkg: aterm updates Claude Code from Anthropic — you have (no version recorded); \
             checking now: `aterm pkg update claude` (a new version downloads silently, ~1 min)"
        );
        assert_eq!(
            announce_line(x, "0.156.0"),
            "atpkg: aterm updates Codex CLI from OpenAI — you have 0.156.0; checking now: \
             `aterm pkg update codex` (a new version downloads silently, ~1 min)"
        );
        // The words are the vendor-direct module's, offline: a vendor-direct build id
        // decodes to its version, a legacy index build renders as `build N` unless an
        // earlier pass recorded its version in the program stamp, none is `(no version
        // recorded)` — `have_words` over an empty layout, so no stamp.
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
            "atpkg: on this copy `claude update` is answered by `aterm pkg update claude` \
             (aterm help pkg) — the vendor's own help follows"
        );
        assert_eq!(
            help_line(x),
            "atpkg: on this copy `codex update` is answered by `aterm pkg update codex` \
             (aterm help pkg) — the vendor's own help follows"
        );
        assert_eq!(
            disabled_line(c, Some(v2_1_280)),
            "atpkg: the aterm package manager is disabled here — nothing checked; claude \
             2.1.280 stays in place; `aterm pkg doctor` says why"
        );
        assert_eq!(
            disabled_line(x, None),
            "atpkg: the aterm package manager is disabled here — nothing checked; codex (no \
             version recorded) stays in place; `aterm pkg doctor` says why"
        );
        assert_eq!(
            declined_line(
                c,
                &Decline::NotAChannel {
                    verb: "install",
                    target: "stable"
                }
            ),
            "atpkg: `claude install stable` — aterm updates Claude Code from Anthropic and \
             installs no `stable`; `claude install` (or `claude install latest`) checks \
             Anthropic's latest now, and `aterm pkg pin claude` holds the version you have"
        );
        assert_eq!(
            declined_line(
                c,
                &Decline::Version {
                    verb: "install",
                    target: "2.1.200"
                }
            ),
            "atpkg: `claude install 2.1.200` — aterm updates Claude Code from Anthropic and \
             cannot install a version of your choosing; `claude install` (or `claude install \
             latest`) checks Anthropic's latest now, `aterm pkg rollback claude` returns to \
             the version before, and `aterm pkg pin claude` holds the version you have"
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
            "atpkg: `claude update --foo` is not a shape aterm's updater answers — `claude \
             update` checks Anthropic's latest now"
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
            "atpkg: `codex update now` is not a shape aterm's updater answers — `codex \
             update` checks OpenAI's latest now"
        );
        assert_eq!(
            incomplete_line(c, Some(v2_1_280)),
            "atpkg: the claude check did not complete (see above) — claude 2.1.280 stays in \
             place; `aterm pkg update claude` retries it and `aterm pkg doctor` explains"
        );
        // A legacy index build, not yet replaced, keeps its number.
        assert_eq!(
            incomplete_line(c, Some(2_026_091_902)),
            "atpkg: the claude check did not complete (see above) — claude build 2026091902 \
             stays in place; `aterm pkg update claude` retries it and `aterm pkg doctor` \
             explains"
        );
        assert_eq!(
            incomplete_line(x, None),
            "atpkg: the codex check did not complete (see above) — codex (no version \
             recorded) stays in place; `aterm pkg update codex` retries it and `aterm pkg \
             doctor` explains"
        );
        assert_eq!(
            contended_line(c, Some(v2_1_280), WAIT_LOCK_SECS),
            "atpkg: another atpkg pass held the store for the whole 30 min wait — that pass \
             (the window's own update, or a typed `aterm pkg` verb) is the one moving \
             packages; claude 2.1.280 stays in place until it finishes, then `claude update` \
             again"
        );
        assert_eq!(
            contended_line(x, None, 1),
            "atpkg: another atpkg pass held the store for the whole 1 s wait — that pass (the \
             window's own update, or a typed `aterm pkg` verb) is the one moving packages; \
             codex (no version recorded) stays in place until it finishes, then `codex \
             update` again"
        );
        // The bound as a person reads it: seconds under two minutes, else minutes, a
        // remainder spelled out — never rounded.
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
            line.starts_with("atpkg: `claude update --foo \"a b\" \"ünïcode\"` is not a shape"),
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
            contended_line(x, None, 5),
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
        assert!(contended_line(x, None, 5).contains("whole 5 s wait"));
    }

    /// The child's argv: the standard update with the FIXED bound — `update <program>
    /// --wait-lock 1800` in production ([`WAIT_LOCK_SECS`]), the pair always present (a
    /// held store is always waited for; there is no `0` and no knob), never the window's
    /// `--progress-file` — and the executable decides the leading verb.
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
        // The executable decides the leading verb: an `atpkg`-named one (the thin bin,
        // the bundle's argv0 alias, `.exe` or not) takes the argv verbatim; the one
        // binary under its own name — or any other — takes it behind `pkg`, because the
        // front door reads a bare `update` as aterm's app-update lane (measured: exit 2,
        // `unknown update sub-command "claude"`).
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

    /// NO ENVIRONMENT KNOB REACHES THE INTERCEPT (owner, 2026-09-19: *"remove all these
    /// env flags so that aterm works correctly by default"*). The first cut shipped two —
    /// an escape hatch that ran the vendor's own updater through the managed name, and a
    /// wait-bound override — and both are gone: the production half of this file, the
    /// whole of `cli.rs` (its verb region, `cmd_selfupdate` through
    /// `selfupdate_child_binary`, reads no environment variable of its own), the twin
    /// renderers in `platform/mod.rs` and the manual's self-update bullet name neither
    /// token; this file reads no environment at all; the manual says a busy store is
    /// waited for, audibly, and how the wait ends; and the bound is the window's own —
    /// `aterm-gui`'s `ATPKG_WAIT_LOCK_SECS`, read off that crate's source because atpkg
    /// cannot depend on it — so a person's `claude update` waits exactly as long as the
    /// pass it waits for would. The token names are spelled by concatenation so this
    /// test's own text is not a hit.
    #[test]
    fn no_environment_knob_reaches_the_intercept() {
        let escape = ["ATPKG_VENDOR", "_SELF_UPDATE"].concat();
        let wait = ["ATPKG_SELF", "_UPDATE_WAIT_SECS"].concat();
        let env_read = ["env::", "var"].concat();
        let env_read_os = ["var", "_os"].concat();
        let whole = include_str!("selfupdate.rs");
        let (production, _) = whole
            .split_once("#[cfg(test)]")
            .expect("the test module's gate");
        for token in [&escape, &wait] {
            assert!(!production.contains(token), "selfupdate.rs names {token}");
        }
        assert!(
            !whole.contains(&env_read) && !whole.contains(&env_read_os),
            "selfupdate.rs reads the environment"
        );
        let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let read = |rel: &str| {
            std::fs::read_to_string(crate_root.join(rel))
                .unwrap_or_else(|e| panic!("{rel} beside this crate: {e}"))
        };
        let cli = read("src/cli.rs");
        for token in [&escape, &wait] {
            assert!(!cli.contains(token), "cli.rs names {token}");
        }
        let start = cli.find("fn cmd_selfupdate(").expect("the verb");
        let end = cli[start..]
            .find("fn is_executable_regular_file(")
            .expect("the verb region's end")
            + start;
        let region = &cli[start..end];
        assert!(
            !region.contains(&env_read) && !region.contains(&env_read_os),
            "the verb region reads an environment variable of its own"
        );
        assert!(
            region.contains("crate::selfupdate::WAIT_LOCK_SECS"),
            "production passes the fixed bound"
        );
        let platform = read("src/platform/mod.rs");
        for token in [&escape, &wait] {
            assert!(!platform.contains(token), "platform/mod.rs names {token}");
        }
        // The manual's bullet (`aterm help pkg`), read off the sibling crate's source.
        let manual = read("../aterm-cli/src/manual.rs");
        let at = manual
            .find("SELF-UPDATE VERBS ON THE MANAGED NAME")
            .expect("the manual's self-update bullet");
        let bullet = &manual[at..];
        let bullet = &bullet[..bullet.find("\n  * ").unwrap_or(bullet.len())];
        for token in [&escape, &wait] {
            assert!(!bullet.contains(token), "the manual names {token}");
        }
        for said in [
            "lock-waiting:",
            "lock-acquired:",
            "Ctrl-C",
            "30 minutes",
            "aterm pkg pin",
            "aterm pkg doctor",
            "refused",
        ] {
            assert!(bullet.contains(said), "the manual's bullet says {said:?}");
        }
        // The bound is the window's own: `aterm-gui` binds the one every scheduled pass
        // waits (`pkg_check::PASS_WAIT_LOCK_SECS`, Phase 3), and so does this child.
        let gui = read("../aterm-gui/src/lib.rs");
        let decl = gui
            .lines()
            .find(|l| l.contains("const ATPKG_WAIT_LOCK_SECS: u64 = "))
            .expect("aterm-gui declares ATPKG_WAIT_LOCK_SECS");
        let value = decl
            .split(" = ")
            .nth(1)
            .and_then(|v| v.trim().strip_suffix(';'))
            .expect("a binding");
        assert_eq!(
            value.trim(),
            "aterm_update_core::pkg_check::PASS_WAIT_LOCK_SECS",
            "{decl}"
        );
        assert_eq!(
            WAIT_LOCK_SECS,
            aterm_update_core::pkg_check::PASS_WAIT_LOCK_SECS
        );
        assert_eq!(WAIT_LOCK_SECS, 30 * 60);
    }

    /// THIS MODULE IS PURE, by its source: no exit-code value (the exit-code registry scan
    /// in `lock.rs` reads `cli.rs`, where every relay is a literal arm or a named
    /// constant), no usage-line literal and no `USAGE`/`HELP`-named item (the help-surfaces
    /// gate would otherwise discover this file and demand a roster row; the usage line
    /// lives in `cli.rs` like `cmd_landing`'s), and no `#[cfg(test)]` item ahead of this
    /// test module (the shape the registry scan's split rule requires of every file it
    /// might one day read).
    #[test]
    fn the_module_builds_no_exit_code_and_carries_no_help_surface() {
        let src = include_str!("selfupdate.rs");
        let gate = "#[cfg(test)]";
        let (production, tests) = src.split_once(gate).expect("the test module's gate");
        assert!(
            tests.starts_with("\nmod tests {"),
            "the first #[cfg(test)] is this module, nothing earlier"
        );
        assert!(!production.contains("mod tests {"));
        let exit_code = ["Exit", "Code"].concat();
        assert!(!production.contains(&exit_code), "no {exit_code} here");
        let usage = ["usage", ":"].concat();
        assert!(!production.contains(&usage), "no {usage} literal here");
        for named in ["USAGE", "HELP"] {
            assert!(
                !production
                    .lines()
                    .any(|l| (l.contains("const ") || l.contains("static ")) && l.contains(named)),
                "no {named}-named item"
            );
        }
        assert!(
            !production.contains("std::process::exit"),
            "the verb relays the child's code; this module decides nothing about exits"
        );
    }
}
