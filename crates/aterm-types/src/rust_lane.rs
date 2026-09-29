// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Rust on this machine means the Trust toolchain: the stock Rust names, the
//! Trust spelling of each, and the one reader that finds a stock Rust tool at
//! COMMAND POSITION in a shell command line.
//!
//! # The ruling this module carries
//!
//! Owner, 2026-09-23, after an orchestrating agent wrote `cargo +1.97.1` and
//! "clippy/fmt" into ~45 subagent prompts for two repos whose
//! `rust-toolchain.toml` pins stock `1.97.1`: on this machine Rust means the
//! Trust toolchain EVERYWHERE — `targo --unverified <cmd>` / `targo trust <cmd>`
//! (never `cargo`, never `cargo +<channel>`), `targo tippy` (never clippy),
//! `targo fmt` / `trustfmt` (never rustfmt), `trustc`, `trustdoc` — including in
//! a repo whose `rust-toolchain.toml` pins a stock channel.
//!
//! # Why the vocabulary lives here, once
//!
//! Three surfaces speak it and must never disagree about a spelling:
//!
//! * `atpkg::reroute` — the signpost a session prints when a stock name is typed;
//! * `aterm pkg lane` (`atpkg::lane`) — this reader over one command line, the
//!   refusal a guard the owner wires (an agent's `PreToolUse` hook, say) says
//!   back; aterm installs nothing into an agent (decision "B", 2026-09-23);
//! * `aterm help rust` — the page that measures which toolchain a directory gets.
//!
//! # A Trust toolchain's own compatibility names (2026-09-27)
//!
//! The reader judges a command word by its NAME, except where a caller lets it
//! look at the directory ([`verdict_in`]): there `<dir>/rustc` beside
//! `<dir>/trustc` is the Trust compiler and never a hit, and `<dir>/cargo` beside
//! `<dir>/targo` is refused with that directory's `targo` ([`COMPAT_TWINS`]).
//! The interim `~/.claude/hooks/trust-toolchain-guard.py` let `rustc`, `rustdoc`,
//! `cargo` and `rustfmt` through by path in any directory holding `trustc` or
//! `targo`. This reader carries the `rustc`/`rustdoc` half, each beside its own
//! twin, and deliberately keeps refusing the rest: `cargo` for the measured
//! reason on [`COMPAT_TWINS`], and `rustfmt` because a Trust toolchain ships no
//! compatibility `rustfmt` (measured: the installed one holds `trustfmt` only).
//! The same binaries chosen by a Trust rustup CHANNEL — `rustup run trust rustc`,
//! `rustc +trust` — are read the same way, text alone (2026-09-28).
//!
//! Every spelling [`trust_spelling`] renders RESOLVES: the design rule that a
//! hint may only name a command that runs (docs/DESIGN-agent-toolchain-guidance-
//! 2026-09-08.md §2). The reroute's signpost used to render `targo trust <verb>`
//! and `targo --unverified <verb>` for EVERY cargo verb, so `cargo metadata`
//! printed two commands targo refuses — the shape of ledger item HOST-05
//! ("`targo --unverified fmt` is rejected").
//!
//! # A stock pin moves only rustup's proxies (measured 2026-09-23)
//!
//! In a crate whose `rust-toolchain.toml` pins `channel = "1.97.1"`, with
//! rustup's proxies FIRST on PATH and no reroute directory at all, on the atpkg
//! store build 9192: `targo --unverified check -v` ran
//! `<store>/trust/9192/bin/trustc`, `targo tippy -v` ran
//! `<store>/trust/9192/bin/tippy-driver <store>/trust/9192/bin/rustc`,
//! `targo fmt -- --version` answered `trustfmt 1.9.0-trust`, and
//! `targo --unverified doc -v` ran `<store>/trust/9192/bin/trustdoc` — while the
//! proxies `cargo`, `rustc` and `rustfmt` answered 1.97.1, "overridden by
//! rust-toolchain.toml". rustup proxies the names it ships; `targo`, `tippy`,
//! `trustfmt` and `trustdoc` are not among them. (What CAN move `targo` off
//! trustc is an explicit `RUSTC`/`build.rustc` — a different question, which
//! `aterm help rust` measures.)
//!
//! # Which lane a cargo verb takes (measured on the same build, 2026-09-23)
//!
//! | class | verbs | the Trust spelling |
//! |---|---|---|
//! | both lanes | `build` `check` `test` | `targo trust <verb> …` / `targo --unverified <verb> …` (a bare one is REFUSED) |
//! | unverified only | `run` `doc` `bench` `rustc` `rustdoc` `fix` `package` `install` `publish` | `targo --unverified <verb> …` (`targo trust <verb>` is not a verb) |
//! | no lane | `clean` `metadata` `tree` `update` `fetch` `generate-lockfile` `locate-project` `pkgid` `vendor` `add` `remove` `new` `init` `version` `help` `search` `info` `config` `report` `verify-project` `read-manifest` `owner` `fmt` `tippy` | `targo <verb> …` (`--unverified` is refused: "valid only for a Targo compilation command") |
//! | the linter | `clippy` | `targo tippy …` |
//!
//! Re-measured 2026-09-24 on store build 9192 with non-`--help` invocations (a
//! `--help` answers before the lane check, so it proves nothing about a lane):
//! a BARE `package`, `install` or `publish` is refused like `build` ("refuses to
//! create an implicitly unverified artifact") while a bare `run`, `doc`,
//! `bench`, `rustc`, `rustdoc` or `fix` proceeds unverified with a warning — the
//! spelling above is the one both accept. `targo clippy` is accepted too and runs
//! the store's `tippy-driver`, the same driver `targo tippy` runs; the name to
//! type is still `targo tippy`.
//!
//! A verb in none of those rows (`cargo nextest`, `cargo deny`, …) is rendered
//! `targo <verb> …` with a note that a cargo EXTENSION targo lacks has no Trust
//! spelling: measured, `targo nextest` and `targo deny` answer "no such command".

use std::path::{Path, PathBuf};

/// The escape hatch an agent writes IN FRONT OF a command that genuinely needs
/// stock Rust: `ATERM_STOCK_REASON='<why>' cargo +1.97.1 …`. Non-empty, in the
/// same simple command; the reason is then owed in the agent's reply. It is a
/// word in the COMMAND TEXT, read by [`check`] — nothing reads it from an
/// environment, so exporting it earlier in a script does not escape anything.
pub const STOCK_REASON: &str = "ATERM_STOCK_REASON";

/// `build`, `check`, `test`: refused bare, and runnable in either lane.
pub const BOTH_LANES: &[&str] = &["build", "check", "test"];

/// Compilation verbs with no verified lane: `targo --unverified <verb>`.
pub const UNVERIFIED_ONLY: &[&str] = &[
    "run", "doc", "bench", "rustc", "rustdoc", "fix", "package", "install", "publish",
];

/// Verbs that take no lane at all (`--unverified` is refused on them).
pub const NO_LANE: &[&str] = &[
    "clean",
    "metadata",
    "tree",
    "update",
    "fetch",
    "generate-lockfile",
    "locate-project",
    "pkgid",
    "vendor",
    "add",
    "remove",
    "new",
    "init",
    "version",
    "help",
    "search",
    "info",
    "config",
    "report",
    "verify-project",
    "read-manifest",
    "owner",
    "fmt",
    "tippy",
];

/// The stock names [`trust_spelling`] and [`check`] know, each with the Trust
/// tool that replaces it. `cargo` and `rustc` carry lanes; the rest are one
/// spelling.
pub const STOCK_TOOLS: &[(&str, &str)] = &[
    ("cargo", "targo"),
    ("rustc", "trustc"),
    ("rustfmt", "trustfmt"),
    ("rustdoc", "trustdoc"),
    ("clippy", "tippy"),
    ("cargo-clippy", "targo tippy"),
    ("clippy-driver", "targo tippy"),
    ("cargo-fmt", "targo fmt"),
];

/// The note on the one spelling of a cargo verb in none of the verb classes —
/// usually a cargo EXTENSION (`nextest`, `deny`). Measured: `targo nextest` and
/// `targo deny` answer "no such command", so a surface rendering such a spelling
/// must say it is conditional ([`is_unknown_verb`]) rather than present it as
/// the Trust command.
pub const UNKNOWN_VERB_NOTE: &str =
    "if targo has this verb; a cargo extension targo lacks has no Trust spelling";

/// Whether `spellings` is the conditional one of a cargo verb targo is not known
/// to have ([`UNKNOWN_VERB_NOTE`]).
#[must_use]
pub fn is_unknown_verb(spellings: &[Spelling]) -> bool {
    spellings.iter().any(|s| s.note == UNKNOWN_VERB_NOTE)
}

/// The label a spelling carries: which verification lane it runs in, if any.
pub const VERIFIED: &str = "VERIFIED";
/// See [`VERIFIED`].
pub const UNVERIFIED: &str = "UNVERIFIED";

/// One Trust command, the caller's own arguments filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spelling {
    /// [`VERIFIED`], [`UNVERIFIED`], or empty for a command that takes no lane.
    pub label: &'static str,
    /// The command to type.
    pub command: String,
    /// What it means, in a few words.
    pub note: &'static str,
}

/// Which lane(s) a cargo verb takes. See the module table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CargoVerb {
    /// `build`/`check`/`test`.
    BothLanes,
    /// A compilation verb with no verified lane.
    UnverifiedOnly,
    /// A verb that takes no lane.
    NoLane,
    /// `clippy` — the linter is `targo tippy`.
    Clippy,
    /// `trust` / `--unverified` in the verb position: the lane is already
    /// named in cargo's own spelling (`cargo trust check`).
    NamedLane,
    /// No verb at all: flags only (`cargo --version`) or nothing.
    FlagsOnly,
    /// A verb this table does not know — usually a cargo extension.
    Unknown,
}

/// The class of `verb`.
#[must_use]
pub fn cargo_verb(verb: &str) -> CargoVerb {
    if BOTH_LANES.contains(&verb) {
        CargoVerb::BothLanes
    } else if UNVERIFIED_ONLY.contains(&verb) {
        CargoVerb::UnverifiedOnly
    } else if NO_LANE.contains(&verb) {
        CargoVerb::NoLane
    } else if verb == "clippy" {
        CargoVerb::Clippy
    } else if verb == "trust" || verb == "--unverified" {
        CargoVerb::NamedLane
    } else {
        CargoVerb::Unknown
    }
}

/// `+<toolchain>` as the FIRST argument (rustup's own spelling), or `None`.
#[must_use]
pub fn explicit_toolchain(args: &[String]) -> Option<&str> {
    let first = args.first()?;
    let toolchain = first.strip_prefix('+')?;
    (!toolchain.is_empty()).then_some(toolchain)
}

/// Whether a toolchain name is the Trust channel (`trust`, `trust-<rev>`, …).
#[must_use]
pub fn is_trust_channel(channel: &str) -> bool {
    channel.starts_with("trust")
}

/// Global cargo options that take their value as the NEXT word.
const CARGO_VALUE_FLAGS: &[&str] = &["--color", "--config", "-Z", "-C", "--explain"];

/// The index of cargo's verb in `args` (no leading `+toolchain`): the first word
/// that is not a global option or an option's value. `None` for flags only.
#[must_use]
pub fn cargo_verb_index(args: &[String]) -> Option<usize> {
    let mut i = 0;
    while let Some(word) = args.get(i) {
        if word == "--unverified" {
            return Some(i);
        }
        if !word.starts_with('-') {
            return Some(i);
        }
        i += if CARGO_VALUE_FLAGS.contains(&word.as_str()) {
            2
        } else {
            1
        };
    }
    None
}

fn join_words(words: &[String]) -> String {
    let mut s = String::new();
    for w in words {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(w);
    }
    s
}

fn command_line(tool: &str, words: &[String]) -> String {
    let mut s = String::from(tool);
    for w in words {
        s.push(' ');
        s.push_str(w);
    }
    s
}

/// The Trust spelling of `<stock> <args>`, `args` exactly as the caller typed
/// them (a leading `+<toolchain>` is dropped: no Trust tool takes a rustup
/// directive). `None` when `stock` is not one of [`STOCK_TOOLS`].
///
/// Two-lane commands come VERIFIED first, the order every signpost prints.
#[must_use]
pub fn trust_spelling(stock: &str, args: &[String]) -> Option<Vec<Spelling>> {
    let args = if explicit_toolchain(args).is_some() {
        &args[1..]
    } else {
        args
    };
    let one = |command: String, note: &'static str| {
        vec![Spelling {
            label: "",
            command,
            note,
        }]
    };
    Some(match stock {
        "cargo" => cargo_spelling(args),
        "rustc" => vec![
            Spelling {
                label: VERIFIED,
                command: command_line("trustc", args),
                note: "proves as it compiles",
            },
            Spelling {
                label: UNVERIFIED,
                command: command_line("trustc -Ztrust-verify=off", args),
                note: "compiles as vanilla Rust",
            },
        ],
        "rustfmt" => one(command_line("trustfmt", args), "the Trust formatter"),
        "rustdoc" => one(command_line("trustdoc", args), "the Trust doc tool"),
        "clippy" => one(command_line("tippy", args), "the Trust linter"),
        "cargo-clippy" | "clippy-driver" => {
            one(command_line("targo tippy", args), "the Trust linter")
        }
        "cargo-fmt" => one(command_line("targo fmt", args), "the Trust formatter"),
        _ => return None,
    })
}

/// [`trust_spelling`] for `cargo`, verb by verb (the module table).
fn cargo_spelling(args: &[String]) -> Vec<Spelling> {
    let Some(at) = cargo_verb_index(args) else {
        return vec![Spelling {
            label: "",
            command: command_line("targo", args),
            note: "takes no lane",
        }];
    };
    // The verb FIRST in the rendered command, then any global flag typed before
    // it, then the rest: `targo trust` parses its own verb before anything else.
    let verb_first = |verb: &str| -> Vec<String> {
        let mut words = vec![verb.to_string()];
        words.extend_from_slice(&args[..at]);
        words.extend_from_slice(&args[at + 1..]);
        words
    };
    let verb = args[at].as_str();
    match cargo_verb(verb) {
        CargoVerb::BothLanes => {
            let words = verb_first(verb);
            vec![
                Spelling {
                    label: VERIFIED,
                    command: command_line("targo trust", &words),
                    note: "emits a proof claim",
                },
                Spelling {
                    label: UNVERIFIED,
                    command: command_line("targo --unverified", &words),
                    note: "no proof claim",
                },
            ]
        }
        CargoVerb::UnverifiedOnly => vec![Spelling {
            label: UNVERIFIED,
            command: command_line("targo --unverified", &verb_first(verb)),
            note: "no proof claim; this verb has no verified lane",
        }],
        CargoVerb::Clippy => vec![Spelling {
            label: "",
            command: command_line("targo", &verb_first("tippy")),
            note: "the Trust linter; takes no lane",
        }],
        CargoVerb::NoLane | CargoVerb::NamedLane | CargoVerb::FlagsOnly => vec![Spelling {
            label: "",
            command: command_line("targo", args),
            note: if matches!(cargo_verb(verb), CargoVerb::NamedLane) {
                "the lane is already named"
            } else {
                "takes no lane"
            },
        }],
        CargoVerb::Unknown => vec![Spelling {
            label: "",
            command: command_line("targo", args),
            note: UNKNOWN_VERB_NOTE,
        }],
    }
}

/// The spellings as indented lines, one per lane, the commands padded to one
/// column: `  targo trust test -p x          VERIFIED   — emits a proof claim`.
#[must_use]
pub fn render_spellings(spellings: &[Spelling], indent: &str) -> String {
    let width = spellings.iter().map(|s| s.command.len()).max().unwrap_or(0) + 3;
    let label_width = spellings.iter().map(|s| s.label.len()).max().unwrap_or(0);
    let mut out = String::new();
    for s in spellings {
        out.push_str(indent);
        out.push_str(&s.command);
        if s.label.is_empty() && label_width == 0 {
            // One spelling with no lane: the command alone is the line.
            out.push('\n');
            continue;
        }
        for _ in s.command.len()..width {
            out.push(' ');
        }
        out.push_str(s.label);
        for _ in s.label.len()..label_width {
            out.push(' ');
        }
        out.push_str(" — ");
        out.push_str(s.note);
        out.push('\n');
    }
    out
}

// ── the pin ─────────────────────────────────────────────────────────────────

/// The channel a toolchain file names: TOML `channel = "<name>"` (comments
/// skipped), or the legacy one-line `rust-toolchain` form (the first non-empty,
/// non-comment line, when it is a bare name).
#[must_use]
pub fn channel_in(text: &str) -> Option<String> {
    let lines = text.lines().map(str::trim).filter(|l| !l.starts_with('#'));
    let mut first_line = None;
    for l in lines {
        if l.is_empty() {
            continue;
        }
        if first_line.is_none() {
            first_line = Some(l);
        }
        let Some(rest) = l.strip_prefix("channel") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(quote) = rest.chars().next().filter(|q| *q == '"' || *q == '\'') else {
            continue;
        };
        let rest = &rest[1..];
        let Some(end) = rest.find(quote) else {
            continue;
        };
        return Some(rest[..end].to_string());
    }
    let legacy = first_line?;
    let bare = legacy
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'));
    bare.then(|| legacy.to_string())
}

/// The toolchain file rustup would read for `start`, and the channel it names:
/// walking `start` and its parents, `rust-toolchain` before `rust-toolchain.toml`
/// in one directory (rustup's own order when both exist). A file that names no
/// channel ends the walk with `None`, as rustup stops at the first file.
///
/// Files only: a `rustup override set` directory override and
/// `$RUSTUP_TOOLCHAIN` also move rustup's proxies, and a caller that must know
/// asks rustup (`rustup show active-toolchain`).
#[must_use]
pub fn find_pin(start: &Path) -> Option<(PathBuf, String)> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        for name in ["rust-toolchain", "rust-toolchain.toml"] {
            let file = d.join(name);
            if file.is_file() {
                let text = std::fs::read_to_string(&file).ok()?;
                return channel_in(&text).map(|ch| (file, ch));
            }
        }
        dir = d.parent();
    }
    None
}

// ── the reader: a stock tool at command position ───────────────────────────

/// A stock Rust tool found at command position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The stock spelling as used — the tool, its `+<channel>` and its verb:
    /// `cargo +1.97.1 test`, `rustfmt`, `rustup run 1.97.1 rustfmt`.
    pub used: String,
    /// The Trust spelling of the same command, the caller's arguments filled in.
    pub instead: Vec<Spelling>,
    /// Set when the command word is a Trust toolchain's own compatibility
    /// `cargo` by path — `<dir>/cargo` with `<dir>/targo` beside it
    /// ([`COMPAT_TWINS`], asked through [`verdict_in`]): that directory as
    /// written, trailing `/` included. [`Hit::used`] and every spelling then
    /// carry it.
    pub toolchain_dir: Option<String>,
    /// Set when the tool was found in text nested deeper than the reader follows
    /// ([`MAX_DEPTH`]): its NAME appears there, and a guard refuses what it could
    /// not read rather than guess. [`Hit::used`] is then the bare
    /// name and [`Hit::instead`] the Trust tool with no arguments.
    pub unread: bool,
}

/// What [`verdict`] found in one command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// No stock Rust tool at command position.
    Clean,
    /// A stock Rust tool at command position, in a simple command that carries
    /// no escape: the one to refuse.
    Stock(Hit),
    /// Every stock Rust tool at command position sits in a simple command that
    /// carries a non-empty [`STOCK_REASON`]: the first of them, and the reason
    /// its author gave — what a guard lets through and journals.
    Escaped {
        /// The escaped stock command.
        hit: Hit,
        /// The `ATERM_STOCK_REASON` value, verbatim.
        reason: String,
    },
}

/// Fold one more reading into `found`. A [`Verdict::Stock`] ends the search (the
/// answer is a refusal whatever else the line holds); the FIRST
/// [`Verdict::Escaped`] is kept over [`Verdict::Clean`]. Answers whether to stop.
fn fold(found: &mut Verdict, next: Verdict) -> bool {
    match next {
        Verdict::Stock(_) => {
            *found = next;
            true
        }
        Verdict::Escaped { .. } if *found == Verdict::Clean => {
            *found = next;
            false
        }
        _ => false,
    }
}

/// The deepest nesting ([`check`] recursing into `$(…)`, backticks, `bash -c`,
/// `eval`) that is parsed. Deeper text is not parsed, and it is not waved
/// through either: a stock tool's NAME anywhere in it is a hit ([`Hit::unread`]).
/// Until 2026-09-28 such text answered `Clean` — nine nested `$( )`, `bash -c`
/// or `eval` levels around `cargo build` went through a guard that refuses one
/// (review of 2026-09-27): a guard that fails open at its own cap is a way past
/// it, and the escape hatch exists for the command that genuinely needs one.
pub const MAX_DEPTH: usize = 8;

/// The first stock Rust tool at COMMAND POSITION in `command` — a shell command
/// line as an agent's Bash tool receives it — that no escape clears, or `None`.
/// [`verdict`] is the same reading with the escaped ones reported too.
///
/// Text that is not a command is never a hit: a quoted argument
/// (`grep "cargo clippy"`), a single-quoted string, a heredoc body with a quoted
/// delimiter, a comment, a path argument (`ls ~/.cargo`, `ls ${DIR}/cargo`), a
/// `case` pattern (`cargo) …`). A command is found
/// through the places the shell runs one: after `;`, `&&`, `||`, `|`, `&`, a
/// newline, `(`, `{`, `then`/`do`/`else`/`if`/`elif`/`while`/`until`, a case
/// arm, the prefix words `time`, `env`, `nice`, `command`, `exec`, `nohup`,
/// `setsid`, `unbuffer`, `caffeinate`, `sudo`, `doas`, `timeout`, `stdbuf`,
/// `ionice`, `taskset`, `xargs`, `!` (their options skipped, value-taking ones
/// with their value), a leading redirection; inside `$(…)`, backticks (also within double quotes and in an
/// unquoted heredoc body), `bash|sh|zsh -c '…'`, `eval '…'`, and `find -exec`;
/// and as the tool `rustup run <toolchain> <tool>` runs. Assignments before a
/// command are skipped, and `ATERM_STOCK_REASON=<non-empty>` among them (the
/// escape hatch, [`STOCK_REASON`]; the last assignment of it wins, as in the
/// shell) clears that one simple command.
///
/// One exception to "never a hit": text nested more than [`MAX_DEPTH`] levels
/// deep is not parsed, so a stock tool's name anywhere in it is a hit
/// ([`Hit::unread`]) — refused rather than guessed at.
#[must_use]
pub fn check(command: &str) -> Option<Hit> {
    match verdict(command) {
        Verdict::Stock(hit) => Some(hit),
        Verdict::Clean | Verdict::Escaped { .. } => None,
    }
}

/// [`check`]'s reading, with an escaped stock command reported as
/// [`Verdict::Escaped`] rather than dropped — so a guard that lets it through
/// can still say what ran and why.
///
/// Text only: nothing on disk is consulted, so a PATH-FORM compatibility name
/// (`build/<host>/stage2/bin/rustc`) is read by its name like any other. A
/// caller that can look at the directory — a guard — reads with
/// [`verdict_in`] instead, which knows a Trust toolchain's own `rustc`.
#[must_use]
pub fn verdict(command: &str) -> Verdict {
    verdict_in(command, &|_, _| false)
}

/// [`verdict`], with `beside(dir, name)` answering whether a file `name` sits
/// in `dir` — the directory of a path-form command word AS WRITTEN, trailing
/// `/` included (`build/aarch64-apple-darwin/stage2/bin/`, `~/toolchains/t/bin/`).
/// The reader asks it only about the Trust twin of a compatibility name
/// ([`COMPAT_TWINS`]); [`file_beside`] is the answer from the filesystem.
///
/// What the answer changes: `<dir>/rustc` beside `<dir>/trustc`, and
/// `<dir>/rustdoc` beside `<dir>/trustdoc`, are that Trust toolchain's own
/// compiler and doc tool under their compatibility names — never a hit.
/// `<dir>/cargo` beside `<dir>/targo` stays a hit (see [`COMPAT_TWINS`] for
/// why), and its spellings name THAT directory's `targo`.
#[must_use]
pub fn verdict_in(command: &str, beside: &dyn Fn(&str, &str) -> bool) -> Verdict {
    check_at(command, 0, beside)
}

/// The directory probe [`verdict_in`] threads through the reader.
type Beside<'a> = &'a dyn Fn(&str, &str) -> bool;

/// A Trust toolchain's compatibility names, each with the Trust name of the
/// same tool — the file that marks the directory as a Trust toolchain's `bin/`.
/// The trust repository's CLAUDE.md: a Trust sysroot ships Trust-branded names,
/// and `cargo`, `rustc` and `rustdoc` are the only compatibility names.
///
/// Measured 2026-09-27 on the installed seal of trust 450403669
/// (`~/toolchains/trust-45040366/bin`): `rustc` is byte-identical to `trustc`
/// and answers `rustc 1.99.0-dev (450403669 2026-09-24)`; `rustdoc` is
/// byte-identical to `trustdoc`. Neither is stock, so by path beside its twin
/// neither is a hit — the page `aterm help rust` prints names that directory as
/// the gates' toolchain, and `build/<host>/stage2/bin/rustc -vV` is how the trust
/// repository reads the compiler under measurement (the INSTALLED `trustc` a
/// refusal would name instead is a different artifact:
/// docs/TOOLCHAIN_POSTURE.md in that repository).
///
/// `cargo` is byte-identical to `targo` too, yet run as `cargo` it is upstream
/// cargo's surface, not targo's: measured on the same directory,
/// `<dir>/cargo --unverified --version` answers "unexpected argument
/// '--unverified' found", and `<dir>/cargo build -v` compiles with `<dir>/rustc`
/// and names no lane. The ruling spells every build with its lane, so a path-form
/// `cargo` stays a hit — with the spellings rewritten to `<dir>/targo`, the
/// toolchain the caller chose rather than whichever `targo` is first on PATH.
pub const COMPAT_TWINS: &[(&str, &str)] = &[
    ("rustc", "trustc"),
    ("rustdoc", "trustdoc"),
    ("cargo", "targo"),
];

/// Whether a file `name` sits in `dir`, a directory as a shell command line
/// writes it ([`verdict_in`]'s probe). A leading `~/`, `$HOME/` or `${HOME}/` is
/// `home`; a relative directory is under `cwd` (and, with no `cwd`, answers
/// `false`). Any other expansion — `$VAR`, `~user`, a glob, a substitution — is
/// text this reader cannot resolve, and answers `false`: a guard that exempted a
/// path it could not read would be guessing. A `cd` earlier on the same line is
/// not followed either.
#[must_use]
pub fn file_beside(dir: &str, name: &str, cwd: Option<&Path>, home: Option<&Path>) -> bool {
    let from_home = ["~/", "$HOME/", "${HOME}/"]
        .iter()
        .find_map(|prefix| dir.strip_prefix(prefix));
    // The shell expands `~` only at the start of a word (`~user/`); inside a
    // path it is a literal character.
    let rest = from_home.unwrap_or(dir);
    if rest.starts_with('~') || rest.contains(['$', '`', '*', '?', '[']) {
        return false;
    }
    let base = match (from_home, home, cwd) {
        (Some(rest), Some(home), _) => home.join(rest),
        (None, _, _) if Path::new(dir).is_absolute() => PathBuf::from(dir),
        (None, _, Some(cwd)) => cwd.join(dir),
        (Some(_), None, _) | (None, _, None) => return false,
    };
    base.join(name).is_file()
}

/// The directory of a path-form command word, trailing `/` included
/// (`build/x/bin/rustc` → `build/x/bin/`), or `None` for a bare name.
fn dir_of(word: &str) -> Option<&str> {
    word.rfind('/').map(|at| &word[..=at])
}

fn check_at(command: &str, depth: usize, beside: Beside<'_>) -> Verdict {
    if depth > MAX_DEPTH {
        return unread(command);
    }
    let mut found = Verdict::Clean;
    let (text, heredoc_subs) = strip_heredocs(command);
    for sub in heredoc_subs {
        if fold(&mut found, check_at(&sub, depth + 1, beside)) {
            return found;
        }
    }
    let (segments, subs) = split_commands(&text);
    for segment in segments {
        if fold(&mut found, offending(&segment, depth, beside)) {
            return found;
        }
    }
    for sub in subs {
        if fold(&mut found, check_at(&sub, depth + 1, beside)) {
            return found;
        }
    }
    found
}

/// The verdict for text nested deeper than [`MAX_DEPTH`], which is not parsed: the
/// first stock tool NAME in it — a run of the characters a name is made of, so
/// `/x/cargo`, `"rustc"` and `$(cargo` all count — is a hit marked
/// [`Hit::unread`], whether or not it stands at command position (text past the
/// cap cannot say). An escape in front of an enclosing simple command clears it
/// like any other hit found inside a `bash -c` script.
fn unread(text: &str) -> Verdict {
    let Some(name) = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .find(|word| STOCK_TOOLS.iter().any(|(stock, _)| stock == word))
    else {
        return Verdict::Clean;
    };
    Verdict::Stock(Hit {
        used: name.to_string(),
        instead: trust_spelling(name, &[]).unwrap_or_default(),
        toolchain_dir: None,
        unread: true,
    })
}

/// Remove heredoc BODIES — text fed to a command on stdin, not commands. The
/// operator line stays. A body under an UNQUOTED delimiter is still expanded by
/// the shell, so its `$(…)` and backtick substitutions are returned to be read.
fn strip_heredocs(cmd: &str) -> (String, Vec<String>) {
    let lines: Vec<&str> = cmd.split('\n').collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut subs = Vec::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        out.push(line);
        let pending = heredoc_operators(line, &mut quote);
        i += 1;
        for (term, dash, quoted) in pending {
            while i < lines.len() {
                let body = lines[i];
                let candidate = if dash {
                    body.trim_start_matches('\t')
                } else {
                    body
                };
                i += 1;
                if candidate == term {
                    break;
                }
                if !quoted {
                    let (_, found) = split_commands(body);
                    subs.extend(found);
                }
            }
        }
    }
    (out.join("\n"), subs)
}

/// The heredoc operators on one line, OUTSIDE quotes (quote state carried
/// across lines in `quote`): `(terminator, <<- form, delimiter quoted)`. A
/// here-string (`<<<`) is not a heredoc.
fn heredoc_operators(line: &str, quote: &mut Option<char>) -> Vec<(String, bool, bool)> {
    let b: Vec<char> = line.chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match *quote {
            Some('\'') => {
                if c == '\'' {
                    *quote = None;
                }
                i += 1;
                continue;
            }
            Some(q) => {
                if c == '\\' {
                    i += 2;
                    continue;
                }
                if c == q {
                    *quote = None;
                }
                i += 1;
                continue;
            }
            None => {}
        }
        if c == '\\' {
            i += 2;
            continue;
        }
        if c == '\'' || c == '"' {
            *quote = Some(c);
            i += 1;
            continue;
        }
        if c == '#' && (i == 0 || b[i - 1].is_whitespace()) {
            break;
        }
        if c == '<' && b.get(i + 1) == Some(&'<') {
            if b.get(i + 2) == Some(&'<') {
                i += 3;
                continue;
            }
            let mut j = i + 2;
            let dash = b.get(j) == Some(&'-');
            if dash {
                j += 1;
            }
            while b.get(j).is_some_and(|c| *c == ' ' || *c == '\t') {
                j += 1;
            }
            // The delimiter is the next shell WORD, quote removal applied: `EOF-1`,
            // `END.txt` and `'my end'` end where they say (a `[A-Za-z0-9_]+` read
            // took `EOF` of `EOF-1`, never met it, and swallowed every later line).
            // Any quoting — `'EOF'`, `"EOF"`, `\EOF`, `E"O"F` — makes the body
            // literal.
            let (term, quoted, end) = heredoc_word(&b, j);
            if !term.is_empty() && (quoted || !term.starts_with(|c: char| c.is_ascii_digit())) {
                found.push((term, dash, quoted));
            }
            i = end.max(i + 2);
            continue;
        }
        i += 1;
    }
    found
}

/// The heredoc delimiter word starting at `b[j]`: `(delimiter, quoted, end)` —
/// the word with its quotes and backslashes removed, whether any part of it was
/// quoted, and the index just past it. It ends at unquoted whitespace or a shell
/// metacharacter.
fn heredoc_word(b: &[char], mut j: usize) -> (String, bool, usize) {
    let mut term = String::new();
    let mut quoted = false;
    while let Some(&c) = b.get(j) {
        match c {
            '\'' | '"' => {
                quoted = true;
                j += 1;
                while let Some(&inner) = b.get(j) {
                    j += 1;
                    if inner == c {
                        break;
                    }
                    term.push(inner);
                }
            }
            '\\' => {
                quoted = true;
                if let Some(&next) = b.get(j + 1) {
                    term.push(next);
                }
                j += 2;
            }
            c if c.is_whitespace() || ";&|<>()".contains(c) => break,
            c => {
                term.push(c);
                j += 1;
            }
        }
    }
    (term, quoted, j)
}

/// Split into simple-command segments, quote-aware, the way the shell reads
/// them; the text of every `$(…)` and backtick substitution (read even inside
/// double quotes, where the shell runs them too) is returned separately.
///
/// A parameter expansion `${…}` and an arithmetic `$((…))` are part of a WORD,
/// and `{`/`}` separate only where they are the shell's reserved words — alone,
/// at command position — so `ls ${DIR}/cargo` and `xargs -I {} …` stay one
/// command. A `case` PATTERN (`cargo)`, `(rustc|rustfmt)`) is not a command and
/// is dropped; the commands of its arm are read like any others.
fn split_commands(cmd: &str) -> (Vec<String>, Vec<String>) {
    let b: Vec<char> = cmd.chars().collect();
    let n = b.len();
    let mut segs = Vec::new();
    let mut subs = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    // Open `case … in … esac` constructs, and whether a pattern may start here.
    let mut case = CaseState::default();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if quote.is_none() && case.want_pattern {
            if cur.trim().is_empty() && starts_word(&b, i, "esac") {
                // `esac` where a pattern could start: the construct ends here.
                case.want_pattern = false;
            } else if c == ')' {
                cur.clear();
                case.want_pattern = false;
                i += 1;
                continue;
            } else if c == '(' && cur.trim().is_empty() {
                // The optional `(` before a pattern.
                i += 1;
                continue;
            } else if c == '|' {
                // Alternation inside a pattern, not a pipe.
                cur.push(c);
                i += 1;
                continue;
            }
        }
        if quote == Some('\'') {
            cur.push(c);
            if c == '\'' {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '\\' && i + 1 < n {
            cur.push(c);
            cur.push(b[i + 1]);
            i += 2;
            continue;
        }
        if c == '$' && b.get(i + 1) == Some(&'(') && b.get(i + 2) != Some(&'(') {
            let mut depth = 1usize;
            let mut j = i + 2;
            while j < n && depth > 0 {
                match b[j] {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            let end = if depth == 0 { j - 1 } else { j };
            subs.push(b[i + 2..end].iter().collect());
            cur.push_str("$SUB");
            i = j;
            continue;
        }
        if c == '`' {
            let close = b[i + 1..].iter().position(|x| *x == '`');
            let end = close.map_or(n, |p| i + 1 + p);
            subs.push(b[i + 1..end].iter().collect());
            cur.push_str("$SUB");
            i = end + 1;
            continue;
        }
        if quote == Some('"') {
            cur.push(c);
            if c == '"' {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            cur.push(c);
            i += 1;
            continue;
        }
        // `${…}` and `$((…))`: one word, whatever braces or parentheses it holds.
        if c == '$' && matches!(b.get(i + 1), Some('{' | '(')) {
            let open = b[i + 1];
            let end = matching_close(&b, i + 1, open, if open == '{' { '}' } else { ')' });
            // A substitution inside it still runs: `${x:-$(cargo metadata)}`.
            let inner: String = b[(i + 2).min(end)..end.saturating_sub(1).max(i + 2)]
                .iter()
                .collect();
            subs.extend(split_commands(&inner).1);
            cur.extend(&b[i..end]);
            i = end;
            continue;
        }
        if c == '#' && cur.chars().last().is_none_or(|p| " \t\n;&|(".contains(p)) {
            while i < n && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // `case WORD in`: a header, not a command; a pattern may start after it.
        if c.is_whitespace() && is_case_header(&cur) {
            cur.clear();
            case.open += 1;
            case.want_pattern = true;
            i += 1;
            continue;
        }
        // `;;`, `;&`, `;;&` end a case arm; the next pattern may start after it.
        if c == ';' && matches!(b.get(i + 1), Some(';' | '&')) {
            close_segment(&mut segs, &mut cur, &mut case);
            i += 2;
            if b.get(i) == Some(&'&') {
                i += 1;
            }
            case.want_pattern = case.open > 0;
            continue;
        }
        // `&` inside a redirection (`2>&1`, `>&2`, `<&3`, `&>out`) is not a
        // command separator: splitting there turned `cargo --version 2>&1` into
        // a command whose verb was `2>`.
        if c == '&' && (cur.ends_with('>') || cur.ends_with('<') || b.get(i + 1) == Some(&'>')) {
            cur.push(c);
            i += 1;
            continue;
        }
        // `{` and `}` are reserved words only alone at command position; anywhere
        // else (`{}`, `a{b,c}`, `x}`) they are part of a word.
        if (c == '{' || c == '}')
            && !(cur.trim().is_empty()
                && b.get(i + 1)
                    .is_none_or(|x| x.is_whitespace() || (c == '}' && ";&|)".contains(*x))))
        {
            cur.push(c);
            i += 1;
            continue;
        }
        if ";&|\n(){}".contains(c) {
            close_segment(&mut segs, &mut cur, &mut case);
            i += 1;
            continue;
        }
        cur.push(c);
        i += 1;
    }
    close_segment(&mut segs, &mut cur, &mut case);
    let segs = segs
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    (segs, subs)
}

/// [`split_commands`]' view of the `case` constructs around it.
#[derive(Default)]
struct CaseState {
    /// `case … in` headers read without their `esac`.
    open: usize,
    /// A pattern may start at the next word: after `in`, and after `;;`.
    want_pattern: bool,
}

/// End the current segment. An `esac` closes the innermost open `case`.
fn close_segment(segs: &mut Vec<String>, cur: &mut String, case: &mut CaseState) {
    let seg = std::mem::take(cur);
    if seg.split_whitespace().next() == Some("esac") {
        case.open = case.open.saturating_sub(1);
        case.want_pattern = false;
    }
    segs.push(seg);
}

/// Whether `cur` is a whole `case WORD in` header.
fn is_case_header(cur: &str) -> bool {
    let words: Vec<&str> = cur.split_whitespace().collect();
    words.len() >= 3 && words[0] == "case" && words[words.len() - 1] == "in"
}

/// Whether the word `word` starts at `b[i]` and ends there too (a separator,
/// whitespace or the end follows it).
fn starts_word(b: &[char], i: usize, word: &str) -> bool {
    let w: Vec<char> = word.chars().collect();
    b.get(i..i + w.len()).is_some_and(|s| s == w.as_slice())
        && b.get(i + w.len())
            .is_none_or(|x| x.is_whitespace() || ";&|)".contains(*x))
}

/// The index just past the `close` matching the `open` at `b[at]` (nesting
/// counted), or the end of the text when it is unbalanced.
fn matching_close(b: &[char], at: usize, open: char, close: char) -> usize {
    let mut depth = 0usize;
    let mut j = at;
    while j < b.len() {
        if b[j] == open {
            depth += 1;
        } else if b[j] == close {
            depth -= 1;
            if depth == 0 {
                return j + 1;
            }
        }
        j += 1;
    }
    b.len()
}

/// POSIX word splitting (quotes removed, backslashes resolved), as
/// `shlex.split(posix=True)`. `None` for an unterminated quote: text the
/// shell itself would refuse is not read as a command.
fn words(segment: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = segment.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(x) => cur.push(x),
                        None => return None,
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.peek() {
                            Some(&x) if x == '"' || x == '\\' || x == '$' || x == '`' => {
                                cur.push(x);
                                chars.next();
                            }
                            _ => cur.push('\\'),
                        },
                        Some(x) => cur.push(x),
                        None => return None,
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(x) = chars.next()
                    && x != '\n'
                {
                    cur.push(x);
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    Some(out)
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn is_assignment(word: &str) -> bool {
    let Some(eq) = word.find('=') else {
        return false;
    };
    let name = &word[..eq];
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Prefix words that run the command after them. The value-taking options of
/// each are listed so the command after them is found, not the option's value.
fn wrapper_options(word: &str) -> Option<&'static [&'static str]> {
    Some(match word {
        "nohup" | "builtin" | "noglob" | "then" | "do" | "else" | "elif" | "if" | "while"
        | "until" | "!" | "setsid" | "unbuffer" => &[],
        // GNU time's `-f FORMAT` and `-o FILE`; BSD time's flags take no value.
        "time" => &["-f", "-o", "--format", "--output"],
        "env" => &["-u", "-C", "-S", "--unset", "--chdir", "--split-string"],
        "nice" => &["-n", "--adjustment"],
        "exec" => &["-a"],
        "caffeinate" => &["-t", "-w"],
        "sudo" => &["-u", "-g", "-h", "-p", "-C", "-D", "-r", "-t", "-U", "-T"],
        "doas" => &["-u", "-C"],
        "timeout" => &["-s", "-k", "--signal", "--kill-after"],
        "stdbuf" => &["-i", "-o", "-e", "--input", "--output", "--error"],
        "ionice" => &[
            "-c",
            "-n",
            "-p",
            "-P",
            "-u",
            "--class",
            "--classdata",
            "--pid",
            "--pgid",
            "--uid",
        ],
        // `taskset [-a] [-c] <mask|list> <cmd>`: the mask is a positional word,
        // skipped like `timeout`'s duration.
        "taskset" => &[],
        "xargs" => &[
            "-n", "-I", "-L", "-P", "-d", "-E", "-s", "-a", "-J", "-R", "-S",
        ],
        "command" => &[],
        _ => return None,
    })
}

/// Why a segment is (or is not) a stock Rust tool. An unterminated quote is
/// text the shell itself would refuse, so it is not read as a command.
fn offending(segment: &str, depth: usize, beside: Beside<'_>) -> Verdict {
    words(segment).map_or(Verdict::Clean, |words| {
        offending_words(&words, depth, beside)
    })
}

/// How many words a leading redirection occupies: `2>/dev/null` is one, `> out`
/// is two (the operator, then its file). `None` for a word that is not one.
fn redirection_words(word: &str) -> Option<usize> {
    let op = word.trim_start_matches(|c: char| c.is_ascii_digit());
    let op = op.strip_prefix('&').unwrap_or(op);
    let body = op.trim_start_matches(['<', '>']);
    if body.len() == op.len() {
        return None;
    }
    let body = body.strip_prefix('&').unwrap_or(body);
    Some(if body.is_empty() { 2 } else { 1 })
}

fn offending_words(words: &[String], depth: usize, beside: Beside<'_>) -> Verdict {
    let mut i = 0;
    // The escape word's value, as the shell would pass it: the LAST assignment of
    // it in this simple command wins, and an empty value escapes nothing.
    let mut reason: Option<String> = None;
    while let Some(w) = words.get(i) {
        if let Some(skip) = redirection_words(w) {
            i += skip;
            continue;
        }
        if is_assignment(w) {
            if let Some(value) = w
                .strip_prefix(STOCK_REASON)
                .and_then(|r| r.strip_prefix('='))
            {
                let value = value.trim();
                reason = (!value.is_empty()).then(|| value.to_string());
            }
            i += 1;
            continue;
        }
        let Some(options) = wrapper_options(basename(w)) else {
            break;
        };
        let wrapper = basename(w);
        i += 1;
        // `command -v cargo` / `command -V cargo` ASK where cargo is; they run nothing.
        if wrapper == "command"
            && words
                .get(i)
                .is_some_and(|o| o.starts_with('-') && (o.contains('v') || o.contains('V')))
        {
            return Verdict::Clean;
        }
        while let Some(o) = words.get(i) {
            if o == "--" {
                i += 1;
                break;
            }
            if !o.starts_with('-') || o.len() < 2 {
                break;
            }
            i += if options.contains(&o.as_str()) { 2 } else { 1 };
        }
        // `timeout [opts] DURATION cmd` and `taskset [opts] MASK cmd`: the
        // positional word is not the command.
        if matches!(wrapper, "timeout" | "taskset") && words.get(i).is_some() {
            i += 1;
        }
        // `nice -5 cmd` (the old spelling) was consumed as an option above.
    }
    // The escape covers everything this ONE simple command runs — a `bash -c`
    // script or a `find -exec` included — and nothing past it.
    match (tool_verdict(words, i, depth, beside), reason) {
        (Verdict::Stock(hit), Some(reason)) => Verdict::Escaped { hit, reason },
        (found, _) => found,
    }
}

/// The verdict for the command word at `words[i]` (every prefix word already
/// skipped) and the arguments after it.
fn tool_verdict(words: &[String], i: usize, depth: usize, beside: Beside<'_>) -> Verdict {
    let Some(head_word) = words.get(i) else {
        return Verdict::Clean;
    };
    let mut tool_word = head_word.as_str();
    let mut head = basename(head_word);
    let mut rest = &words[i + 1..];
    let mut used_prefix = String::new();
    let mut run_channel: Option<&str> = None;
    if matches!(head, "bash" | "sh" | "zsh" | "dash" | "ksh") {
        return shell_command_string(rest)
            .map_or(Verdict::Clean, |script| check_at(script, depth + 1, beside));
    }
    if head == "eval" {
        return check_at(&join_words(rest), depth + 1, beside);
    }
    if head == "find" {
        return find_exec(rest, depth, beside);
    }
    if head == "rustup" {
        // `rustup run <toolchain> <tool> …` runs <tool> from that toolchain.
        if rest.len() >= 3 && rest[0] == "run" {
            used_prefix = String::from("rustup run ");
            used_prefix.push_str(&rest[1]);
            used_prefix.push(' ');
            run_channel = Some(rest[1].as_str());
            tool_word = &rest[2];
            head = basename(tool_word);
            rest = &rest[3..];
        } else {
            return Verdict::Clean;
        }
    }
    // A redirection is the shell's, not the tool's: it is neither an argument
    // nor a verb (`cargo --version 2>&1` has no verb `2>&1`).
    let mut plain_rest = Vec::with_capacity(rest.len());
    let mut j = 0;
    while let Some(w) = rest.get(j) {
        match redirection_words(w) {
            Some(skip) => j += skip,
            None => {
                plain_rest.push(w.clone());
                j += 1;
            }
        }
    }
    let rest = plain_rest.as_slice();
    let Some(mut instead) = trust_spelling(head, rest) else {
        return Verdict::Clean;
    };
    // The same binaries chosen by their rustup CHANNEL — `rustup run trust rustc`
    // (the spelling `aterm help reroute` sanctions), `rustc +trust` — are that
    // Trust toolchain's trustc and trustdoc: never a hit. Its `cargo` stays one,
    // for the reason on [`COMPAT_TWINS`].
    if is_trust_alias(head)
        && run_channel
            .or_else(|| explicit_toolchain(rest))
            .is_some_and(is_trust_channel)
    {
        return Verdict::Clean;
    }
    // A compatibility name BY PATH, its Trust twin beside it: that toolchain's
    // own binary ([`COMPAT_TWINS`]). `rustc` and `rustdoc` there are the Trust
    // tools; `cargo` there is upstream cargo's surface, spelled with its `targo`.
    let toolchain_dir = dir_of(tool_word).filter(|dir| {
        COMPAT_TWINS
            .iter()
            .any(|(compat, twin)| *compat == head && beside(dir, twin))
    });
    if toolchain_dir.is_some() && is_trust_alias(head) {
        return Verdict::Clean;
    }
    let mut used = used_prefix;
    if let Some(dir) = toolchain_dir {
        used.push_str(dir);
        for spelling in &mut instead {
            if spelling.command.starts_with("targo") {
                spelling.command.insert_str(0, dir);
            }
        }
    }
    used.push_str(head);
    if head == "cargo" {
        if let Some(tc) = explicit_toolchain(rest) {
            used.push_str(" +");
            used.push_str(tc);
        }
        let plain = if explicit_toolchain(rest).is_some() {
            &rest[1..]
        } else {
            rest
        };
        if let Some(at) = cargo_verb_index(plain) {
            used.push(' ');
            used.push_str(&plain[at]);
        }
    } else if let Some(tc) = explicit_toolchain(rest) {
        used.push_str(" +");
        used.push_str(tc);
    }
    Verdict::Stock(Hit {
        used,
        instead,
        toolchain_dir: toolchain_dir.map(str::to_string),
        unread: false,
    })
}

/// Whether `name` is a Trust toolchain's compatibility name for its OWN compiler
/// or doc tool ([`COMPAT_TWINS`] without `cargo`): run from that toolchain — by
/// path beside its twin, or by a Trust rustup channel — it is the Trust tool.
fn is_trust_alias(name: &str) -> bool {
    name != "cargo" && COMPAT_TWINS.iter().any(|(compat, _)| *compat == name)
}

/// The command string of `sh -c '…'` (any flag cluster carrying `c`, e.g.
/// `-lc`, `-ec`, or `-l -c`), or `None` when the shell runs a script file or
/// reads stdin — text this reader never sees.
fn shell_command_string(rest: &[String]) -> Option<&str> {
    let mut saw_c = false;
    let mut i = 0;
    while let Some(w) = rest.get(i) {
        if w == "--" {
            i += 1;
            break;
        }
        if (w.starts_with('-') || w.starts_with('+')) && w.len() > 1 {
            if w.starts_with("--") {
                i += 1;
                continue;
            }
            if w[1..].contains('c') {
                saw_c = true;
            }
            // `-o option` / `-O option` take the next word.
            i += if w[1..].ends_with('o') || w[1..].ends_with('O') {
                2
            } else {
                1
            };
            continue;
        }
        break;
    }
    if !saw_c {
        return None;
    }
    rest.get(i).map(String::as_str)
}

/// `find … -exec <cmd> … ;` (and `-execdir`, `-ok`, `-okdir`): the command runs.
fn find_exec(rest: &[String], depth: usize, beside: Beside<'_>) -> Verdict {
    let mut found = Verdict::Clean;
    let mut i = 0;
    while i < rest.len() {
        if matches!(rest[i].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
            let start = i + 1;
            let mut end = start;
            while end < rest.len() && rest[end] != ";" && rest[end] != "+" {
                end += 1;
            }
            if fold(
                &mut found,
                offending_words(&rest[start..end], depth, beside),
            ) {
                return found;
            }
            i = end;
        }
        i += 1;
    }
    found
}

// ── the refusal: what a guard says back ─────────────────────────────────────

/// The longest a rendered command may be in a [`refusal`]: an agent's command
/// line can be kilobytes, and the refusal names the Trust spelling, not the
/// whole payload.
pub const REFUSAL_COMMAND_CAP: usize = 240;

/// `text` as ONE line of at most [`REFUSAL_COMMAND_CAP`] bytes: every control
/// character (a newline inside a quoted argument, a tab) folded to a space, cut
/// on a character boundary with `…` when it was longer.
fn one_command_line(text: &str) -> String {
    let folded: String = text
        .chars()
        .map(|c| {
            if c.is_control() || c == '\u{2028}' || c == '\u{2029}' {
                ' '
            } else {
                c
            }
        })
        .collect();
    if folded.len() <= REFUSAL_COMMAND_CAP {
        return folded;
    }
    let mut end = REFUSAL_COMMAND_CAP;
    while !folded.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &folded[..end])
}

/// What a guard says back to the agent whose command it refused: the stock
/// spelling as used, the Trust spelling of the SAME command with the agent's own
/// arguments (every lane targo accepts for it, [`trust_spelling`]), the fact the
/// 2026-09-23 incident turned on — a stock pin moves only rustup's proxies — and
/// the escape hatch. `pin` is the directory's toolchain file and channel
/// ([`find_pin`]); a STOCK one is named, since that is the directory an agent
/// reads as "this repository builds with stock".
///
/// Bounded: each command is one line of at most [`REFUSAL_COMMAND_CAP`] bytes.
#[must_use]
pub fn refusal(hit: &Hit, pin: Option<(&Path, &str)>) -> String {
    let mut out = if hit.unread {
        format!(
            "`{}` appears in text nested more than {MAX_DEPTH} levels deep (`$( )`, backticks, \
             `bash -c`, `eval`), which this reader does not follow, so it is refused rather than \
             guessed at: flatten the command so the tool stands where it can be read.\n",
            one_command_line(&hit.used)
        )
    } else if let Some(dir) = &hit.toolchain_dir {
        // A Trust toolchain's own `cargo` by path ([`COMPAT_TWINS`]): not stock,
        // and saying so would be false — but it names no lane. A path runs no
        // rustup proxy, so a pin moves nothing here and is not named.
        format!(
            "`{}` is the compatibility `cargo` of the Trust toolchain in {}: upstream cargo's \
             surface, not targo's — it takes no lane (it refuses `--unverified`), so neither the \
             verified nor the unverified lane is named. On this machine a Rust build names its \
             lane, with that toolchain's own `targo` beside it.\n",
            one_command_line(&hit.used),
            one_command_line(dir)
        )
    } else {
        format!(
            "`{}` is stock Rust. On this machine Rust means the Trust toolchain — also in a \
             repository whose rust-toolchain.toml pins a stock channel: that pin moves only \
             rustup's proxies (cargo, rustc, rustfmt, cargo-clippy), never targo, tippy, trustfmt \
             or trustdoc.\n",
            one_command_line(&hit.used)
        )
    };
    if let Some((file, channel)) = pin
        .filter(|(_, ch)| !is_trust_channel(ch))
        .filter(|_| hit.toolchain_dir.is_none() && !hit.unread)
    {
        out.push_str(&format!(
            "This directory pins stock \"{}\" ({}); build it with the Trust spelling all the same.\n",
            one_command_line(channel),
            one_command_line(&file.display().to_string())
        ));
    }
    if is_unknown_verb(&hit.instead) {
        // A hint may only name a command that runs: say that this one may not.
        out.push_str(
            "Its verb is not one targo is known to have (measured: `targo nextest` and `targo deny` \
             answer \"no such command\"); IF targo has it, the Trust spelling is:\n",
        );
    } else if hit.unread {
        out.push_str("The Trust tool for it:\n");
    } else {
        out.push_str("Run the Trust spelling of the same command:\n");
    }
    let bounded: Vec<Spelling> = hit
        .instead
        .iter()
        .map(|s| Spelling {
            label: s.label,
            command: one_command_line(&s.command),
            note: s.note,
        })
        .collect();
    out.push_str(&render_spellings(&bounded, "  "));
    if is_unknown_verb(&hit.instead) {
        out.push_str(
            "If it answers \"no such command\", the extension has no Trust spelling yet — the case \
             the escape below exists for.\n",
        );
    }
    out.push_str(&format!(
        "If stock Rust is genuinely required, put {STOCK_REASON}='<why>' in front of the \
         command and say why in your reply. `aterm help rust` measures what this directory gets."
    ));
    out
}

#[cfg(test)]
#[path = "rust_lane_tests.rs"]
mod tests;
