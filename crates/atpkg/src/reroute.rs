// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Rerouting: the upstream Rust names inside an aterm session.
//!
//! Design of record: `docs/DESIGN-toolchain-philosophy-2026-08-29.md` §4
//! ("Rerouting: announced, escapable, never silently substituting"); the implementation
//! record is `docs/DESIGN-toolchain-reroute-2026-09-07.md`. The managed store carries no
//! `cargo`/`rustc` by design, so without this layer a bare `cargo` in a session runs
//! whatever upstream copy PATH finds first (rustup's proxies, Homebrew's), silently.
//!
//! # What it is
//!
//! A SESSION-SCOPED directory, [`DIR_NAME`] under the manager prefix, holding one
//! tiny `/bin/sh` stub per upstream name in [`TABLE`]. The spawn seams put that
//! directory FIRST on the PATH they hand a session's shell (philosophy §3:
//! "Session PATH: prepend is legitimate … the only place precedence is taken")
//! and export its location as [`REROUTE_DIR_ENV`] so the shell integration can
//! re-assert it after the user's own rc files have run (`. ~/.cargo/env` in a
//! `.zshrc` prepends `~/.cargo/bin` AFTER the environment was injected). Nothing
//! machine-wide ever points at it: the rc hook keeps APPENDING the managed
//! `bin/`, and `cargo`/`rustc`/`rustup` stay on `store::SENSITIVE_SHIMS` — a
//! managed `bin/` never carries those names (CONTRIBUTING.md), and this
//! directory is not `bin/`. The rc hook does move this directory first inside an aterm
//! session (the gated arm it shares with `agents/`, never under [`PASSTHROUGH_ENV`],
//! `crate::hooks`), which covers the transparent-session lane: no shell integration runs
//! there, and `path_helper` would leave the directory behind `/opt/homebrew/bin`. Outside
//! aterm the hook removes it.
//!
//! # The policy is per row
//!
//! * **DIRECT** (`clippy`→`tippy`, `rustfmt`→`trustfmt`, `rustdoc`→`trustdoc`,
//!   `lean`→`clean`): the branded tool is the same operation under a new name.
//!   Run it with the caller's arguments; announce ONE line on stderr.
//! * **SIGNPOST** (`cargo`→`targo`, `rustc`→`trustc`, `tlc`→`ty`): substituting
//!   would make a choice the user did not make (the verification lane) or assert
//!   an equivalence nobody has proven. So nothing is substituted: print the
//!   branded command with the caller's own arguments filled in — both lanes
//!   where there are two — and then RUN WHAT WAS ASKED FOR, upstream.
//!
//!   The spelling is `aterm_types::rust_lane`'s: one vocabulary with `aterm pkg lane`
//!   ([`crate::lane`]) and `aterm help rust`, so a cargo verb gets the lane targo accepts
//!   for it — `build`/`check`/`test` both, `run`/`doc`/`install`… the unverified one,
//!   `metadata`/`fmt`/`tree`… none, `clippy` `targo tippy` — never a command targo
//!   refuses. A directory whose `rust-toolchain.toml` pins a STOCK channel says so under
//!   the lanes: that pin moves only rustup's proxies, never `targo` ([`Pin`]).
//!
//!   What runs is named by what it is. `cargo` and `rustc` are resolved before a word is
//!   said ([`crate::upstream_identity`]): through rustup when the copy on PATH is its
//!   proxy, then by the resolved file's `-vV`. The first line says `-> Trust 7d5533b2, via
//!   rustup trust (the rustup default)` or `-> STOCK rustc 1.97.1 (8bab26f4f), via rustup
//!   1.97.1-aarch64-apple-darwin (pinned by …); not Trust (no proof claim)`, and one line
//!   under the lanes names the file, so a Trust seal is never announced as "upstream".
//!   `tlc` is not resolved: it has no Trust build to be confused with.
//!
//!   Announce, do not prevent: §4 withholds only SILENT substitution, so a bare
//!   `cargo build` that announces and then runs is as allowed as `cargo +stable build`
//!   ([`stock_channel_signpost`]). A setting silences the announcement, `[reroute]
//!   announce = false` in aterm.toml ([`crate::config::RerouteConfig`], Settings ▸
//!   Packages); no environment variable does.
//!
//!   One exception, a Trust source checkout. Inside one — `x.py` beside a `src/stage0`
//!   whose `compiler_dist_channel=trust` ([`crate::upstream_identity::trust_checkout`]) — a
//!   `cargo`/`rustc` that resolves to anything but Trust (stock, or not known) does not
//!   run: exit [`REFUSAL_EXIT`], naming what it resolved to, the checkout, the Trust
//!   spelling, and how to run it anyway, `aterm --no-reroute` ([`trust_checkout_refusal`]).
//!   There a stock compiler would build part of Trust. `[reroute] announce = false` does
//!   not silence it: a refusal is not an announcement.
//!
//!   What is judged is a name looked up on PATH, wherever it is looked up: typed, spawned
//!   by a tool, or spawned beneath a command the reroute itself ran — that one carries
//!   [`ANNOUNCED_ENV`], which keeps its tree's by-name spawns quiet and still judged —
//!   with no store resolving or with atpkg unreachable alike. A cargo started by its path
//!   (or by `rustup run`) runs `rustc` by name, and that rustc is judged. NOT JUDGED,
//!   because no name is looked up: a compiler run by its own path, one handed to cargo as
//!   `RUSTC`, `RUSTC_WRAPPER` or `build.rustc` (cargo then runs that path), and everything
//!   in a session started with `aterm --no-reroute`.
//! * **ORACLE** (`z3`): `ay`'s differential oracle. Announce, naming `ay`, then run
//!   upstream z3 — as a SIGNPOST does; the line says z3 is ay's oracle, not its stand-in.
//!   `[reroute] strict = true` refuses it, naming the upstream copy's own path (a path
//!   never meets a stub).
//!
//! # Every stub runs something
//!
//! A stub never stops a command it could run: a DIRECT row whose Trust tool is missing
//! (not installed, or its shim lost) runs the upstream tool the caller named, with one
//! line naming the fix; `z3` runs upstream with one line naming `ay`; a stub that cannot
//! reach atpkg walks PATH for the upstream tool in `sh`, as the escape does, with one
//! line. Exit 127 is left only where nothing exists to run. `[reroute] announce = false`
//! silences these lines like any upstream-runs line; `[reroute] strict = true`
//! ([`crate::config::RerouteConfig::strict`]) refuses instead. No setting lifts the one
//! exception: inside a Trust source checkout a `cargo`/`rustc` that is not Trust does not
//! run ([`trust_checkout_refusal`], above).
//!
//! # Escapes, all of them
//!
//! * `aterm --no-reroute` starts a session with every upstream tool: it stamps the
//!   internal [`PASSTHROUGH_ENV`] marker on that session (the front door clears an
//!   inherited one otherwise, so it is protocol, not a knob). The stub itself walks
//!   PATH past its own directory and execs the first upstream copy — in `sh`, so the
//!   escape works even when atpkg is unreachable. Nothing else stamps it: an upstream
//!   child the reroute execs carries [`ANNOUNCED_ENV`] instead, so cargo's own
//!   `rustc`/`rustdoc` spawns are never re-announced, and a `cargo`/`rustc` among them is
//!   refused only where the same name typed alone would be — inside a Trust source
//!   checkout, when it is not Trust.
//! * `<tool> +<toolchain>` naming a non-Trust toolchain is the user naming upstream
//!   deliberately: it runs as named, after [`stock_channel_signpost`] — the Trust spelling
//!   of the same command, silenced like any signpost by the setting — on the DIRECT Rust
//!   rows too, whose branded tool would take `+1.97.1` for a file name. `+trust…` keeps
//!   the lane question and is signposted like a bare `cargo`; upstream receives the
//!   directive it was given, and a DIRECT row's branded tool never does ([`plan`]).
//! * `rustup run <tc> cargo` never reaches these stubs (rustup resolves the
//!   toolchain's own binaries), and neither does an absolute path — which is why
//!   the compiler's own bootstrap, which drives stage0 by absolute path (and hands
//!   cargo `RUSTC`), stays silent. That is the intended boundary: the reroute answers
//!   a NAME looked up on PATH, nothing else. A compiler named by path in a variable —
//!   `RUSTC=/path/to/rustc` for cargo — is a path, and is not judged.
//! * What such a command spawns BY NAME does meet a stub. cargo looks `rustc` and
//!   `rustdoc` up by name unless it can skip past rustup's proxies, and it skips only when
//!   the `rustc` on PATH is the size of `rustup` — never a stub's. rustup puts
//!   `~/.cargo/bin` ahead only when it is absent from PATH, and Homebrew's cargo or a
//!   toolchain's own has no proxies to skip to. Its `rustc -vV` probe carries no mark and
//!   is answered as a typed name: announced, and inside a Trust source checkout refused
//!   when it is not Trust. Its compiles carry cargo's own mark, [`CARGO_CRATE_ENV`], set
//!   on every compile it spawns for a crate, and those two stubs ([`CARGO_COMPILERS`])
//!   take it as they take [`ANNOUNCED_ENV`]: not announced again, run as the cargo chose
//!   (so a stock `cargo doc` never pairs Trust's trustdoc with crates stock rustc built,
//!   E0514), and a `rustc` still judged inside a Trust source checkout — so a cached
//!   `rustc -vV` (cargo keeps one per target dir) lets no stock compile through there.
//!   Trust's own cargo marks its compile twice ([`TARGO_CRATE_ENV`]) and is answered as a
//!   typed name is: its store build has `trustdoc` and no `rustdoc` beside it, so the
//!   `rustdoc` it spawns by name must be the DIRECT row's. Only `aterm --no-reroute`
//!   carries a whole command tree past the stubs: what a reroute-run command spawns by
//!   name meets them too ([`ANNOUNCED_ENV`]).
//!
//! # What a stub is not
//!
//! Not a managed tool: it execs through a variable, never the literal `exec '` line
//! `platform::parse_sh_shim_target` keys on, so `resolve_shim`, `active_builds` and doctor's
//! broken-shim scan never count it. It is recognized, rewritten and removed only by its
//! marker line ([`STUB_MARKER`]). Windows lays nothing yet: it needs a `.cmd` dialect and a
//! prepend applied after `winpath::refresh_child_path` rewrites the child PATH.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_types::rust_lane;

use crate::store::Layout;
use crate::upstream_identity::Resolved;

/// Exported by the spawn seams: the absolute reroute directory of this session, so the
/// shell integration can re-assert it first after rc files ran.
pub const REROUTE_DIR_ENV: &str = "ATERM_REROUTE_DIR";
/// Exported by the front door (`crates/aterm/src/main.rs`) to a TTY session on every lane,
/// engaged reroute or not: the absolute managed `<prefix>/agents/` ([`Layout::agents_dir`]),
/// ensured to exist, so `--no-reroute` keeps the managed `claude`/`codex` front-insert.
/// Absent or empty means none. A launcher→session handoff the shell integration never reads.
/// Not `ATPKG_AGENTS`: the shell hook exports that ([`crate::hooks`]), and the shell
/// integration sources the hook only while it is unset.
pub const AGENTS_DIR_ENV: &str = "ATERM_AGENTS_DIR";
/// INTERNAL PROTOCOL, not a setting: the marker of a session with every upstream tool
/// restored. `aterm --no-reroute` stamps it on its session (and the front door CLEARS
/// an inherited one on every other launch, so exporting it by hand does not make a
/// session), and the stub and [`run`] read it. Nothing else sets it: an upstream child
/// [`exec_upstream`] runs, and one a stub that cannot reach atpkg runs, carry
/// [`ANNOUNCED_ENV`] instead. `aterm --no-reroute` is the one supported spelling.
///
/// SPELLED AS WHAT IT IS: the stub and the shell integration are shell, and a shell reads
/// the marker from its own environment, so an `export` in an rc file AFTER the front door
/// cleared it still reaches them — a marker a shell reads is unavoidably settable. It is
/// not SUPPORTED as a control: the double-underscore spelling says internal, and no page
/// documents it.
pub const PASSTHROUGH_ENV: &str = "__ATERM_REROUTE_PASSTHROUGH";
/// cargo's mark of its own compile: set on every `rustc` and `rustdoc` it spawns to build
/// or document a crate (and so inherited by what those run — a proc macro's children, a
/// doctest), and on nothing else cargo starts: not a build script, a `cargo run` binary, a
/// test binary, nor its own `rustc -vV` probes (measured 2026-10-02 on cargo 1.96). A
/// [`CARGO_COMPILERS`] stub takes it as it takes [`ANNOUNCED_ENV`] — said nothing, run as
/// the cargo chose, a `rustc` judged inside a Trust source checkout — unless
/// [`TARGO_CRATE_ENV`] says the cargo is Trust's. Read, never set.
pub const CARGO_CRATE_ENV: &str = "CARGO_CRATE_NAME";
/// Trust's cargo's second mark of its own compile, beside [`CARGO_CRATE_ENV`] on exactly
/// the same processes (measured 2026-10-02 on trust 9192: `targo` and the store's `cargo`,
/// one binary); no stock cargo sets it. That compile is answered as a typed name is: the
/// store build carries `trustdoc` and no `rustdoc`, so the `rustdoc` its `cargo` spawns by
/// name is the DIRECT row's — upstream it was stock Rust's (E0514) or none at all (127).
pub const TARGO_CRATE_ENV: &str = "TARGO_CRATE_NAME";
/// The rows cargo spawns for a crate, the stubs that take a stock cargo's
/// [`CARGO_CRATE_ENV`] as [`ANNOUNCED_ENV`]: their copy is the one the cargo chose, as a
/// stub-started cargo's is. The row's announcement and a DIRECT row's Trust tool are for a
/// name a PERSON typed — one that carries no such mark — and for Trust's own cargo
/// ([`TARGO_CRATE_ENV`]).
pub const CARGO_COMPILERS: &[&str] = &["rustc", "rustdoc"];
/// INTERNAL PROTOCOL, not a setting: the marker [`exec_upstream`] stamps on every upstream
/// copy the reroute runs — a command already announced (or deliberately not, under
/// `[reroute] announce = false`). What that command spawns BY NAME is not announced again,
/// and runs upstream as typed — but it is still JUDGED: a `cargo`/`rustc` inside a Trust
/// source checkout that is not Trust does not run ([`trust_checkout_refusal`]). A stock
/// cargo's own compile ([`CARGO_CRATE_ENV`]) is taken as this marker by the
/// [`CARGO_COMPILERS`] stubs, which stamp it on the child.
///
/// WHY NOT THE ESCAPE: with [`PASSTHROUGH_ENV`] on its tree, a Trust cargo the reroute
/// allowed inside a Trust checkout would let a by-name `rustc +1.97.1` beneath it run
/// stock with no line at all, while the same `rustc +1.97.1` typed alone is refused. A
/// cargo subcommand, `cargo run -p xtask`, a build script that spawns `cargo +nightly`,
/// and Trust's own `src/tools/miri/miri-script` (`rustc +miri`, `cargo +miri`) are that
/// shape. The escape says "judge nothing"; this says "say nothing again". The
/// `cargo`/`rustc` stubs ask atpkg under it only where the directory may be a Rust source
/// checkout — an `x.py` beside a `src/stage0` up the tree ([`sh_source_checkout_test`]) —
/// so a cargo build anywhere else costs its by-name `rustc` spawns a few `stat`s, not an
/// atpkg each.
pub const ANNOUNCED_ENV: &str = "__ATERM_REROUTE_ANNOUNCED";
/// Line 2 of every stub: the only thing that makes a file ours to rewrite or remove.
/// Version-suffixed so a changed body can be told from an old one.
pub const STUB_MARKER: &str = "# atpkg reroute stub v1";
/// The hidden verb a stub execs: `atpkg __reroute <upstream> [args…]`.
pub const HIDDEN_VERB: &str = "__reroute";
/// The exit of a refusal (strict's refusals, a non-Trust `cargo`/`rustc` inside a Trust
/// source checkout, a `__reroute` usage error). Usage-shaped: the command did not run
/// because the caller has to choose, not because anything broke.
pub const REFUSAL_EXIT: u8 = 2;
/// The directory under the manager prefix (`<prefix>/reroute`).
pub const DIR_NAME: &str = "reroute";
/// A marker file `lay` puts in every reroute directory, so a walk recognizes one under any
/// spelling (a nested aterm's second prefix, a symlink, a trailing slash) without comparing
/// strings: skipping only its own directory by string, two reroute spellings on PATH would
/// exec each other's stub forever.
pub const DIR_MARKER_FILE: &str = ".atpkg-reroute-dir";
/// A stub is a few hundred bytes; anything larger is not one of ours.
const MAX_STUB_BYTES: usize = 16 * 1024;

/// One verification lane a SIGNPOST names, rendered as `<command> <args>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lane {
    pub label: &'static str,
    pub command: &'static str,
    pub note: &'static str,
}

/// A DIRECT row whose branded tool is verb-first: `lean file.lean` must reach
/// `clean check file.lean`. Keyed on the source extension, never on "the argument is not a
/// flag": the branded tool has its own subcommands (`repl`, `eval`, `lake`), and `lean repl`
/// must not become `clean check repl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceVerb {
    /// The branded subcommand a bare source file routes through.
    pub verb: &'static str,
    /// The source extension that selects it, leading dot included.
    pub ext: &'static str,
}

/// The per-row policy — philosophy §4's table, one variant per column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Run `branded` with the caller's arguments and one line; with it missing, run upstream
    /// with one line naming the fix (refused under strict). `program` is what
    /// `aterm pkg install` names to install `branded`; `source_verb` is `Some` only for a
    /// verb-first branded tool.
    Direct {
        branded: &'static str,
        program: &'static str,
        source_verb: Option<SourceVerb>,
    },
    /// Announce `branded` and every `lane` with the caller's arguments, then run upstream.
    /// An empty `lanes` means "one spelling, equivalence unproven" (`tlc`). `program` is
    /// the program whose install lays `branded`, as for a direct row.
    Signpost {
        branded: &'static str,
        program: &'static str,
        lanes: &'static [Lane],
    },
    /// Announce `branded`, then run upstream: the upstream tool is `branded`'s
    /// differential oracle. Under `[reroute] strict = true`, refuse, naming the upstream
    /// copy's own path (a path never meets a stub). `program` as for a direct row.
    Oracle {
        branded: &'static str,
        program: &'static str,
    },
}

/// One upstream name and what a session does with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub upstream: &'static str,
    pub policy: Policy,
}

const CARGO_LANES: &[Lane] = &[
    Lane {
        label: "VERIFIED",
        command: "targo trust",
        note: "emits a proof claim",
    },
    Lane {
        label: "UNVERIFIED",
        command: "targo --unverified",
        note: "no proof claim",
    },
];
const RUSTC_LANES: &[Lane] = &[
    Lane {
        label: "VERIFIED",
        command: "trustc",
        note: "proves as it compiles",
    },
    Lane {
        label: "UNVERIFIED",
        command: "trustc -Ztrust-verify=off",
        note: "compiles as vanilla Rust",
    },
];

/// `cargo <verb>` for a verb that IS a direct-row operation: one branded
/// spelling and no lane question — tippy lints and trustfmt formats, neither
/// proves nor builds. Every other verb is spelled by `aterm_types::rust_lane`:
/// both lanes for `build`/`check`/`test`, the unverified one for a
/// verb with no verified lane, none for a verb that takes no lane.
pub const CARGO_VERB_REROUTES: &[(&str, &str)] = &[("clippy", "targo tippy"), ("fmt", "targo fmt")];

/// The table. Order is the order doctor and `aterm help reroute` list it in.
pub const TABLE: &[Row] = &[
    Row {
        upstream: "clippy",
        policy: Policy::Direct {
            branded: "tippy",
            program: "trust",
            source_verb: None,
        },
    },
    Row {
        upstream: "rustfmt",
        policy: Policy::Direct {
            branded: "trustfmt",
            program: "trust",
            source_verb: None,
        },
    },
    Row {
        upstream: "rustdoc",
        policy: Policy::Direct {
            branded: "trustdoc",
            program: "trust",
            source_verb: None,
        },
    },
    Row {
        upstream: "lean",
        policy: Policy::Direct {
            branded: "clean",
            program: "clean",
            source_verb: Some(SourceVerb {
                verb: "check",
                ext: ".lean",
            }),
        },
    },
    Row {
        upstream: "tlc",
        policy: Policy::Signpost {
            branded: "ty",
            program: "ty",
            lanes: &[],
        },
    },
    Row {
        upstream: "rustc",
        policy: Policy::Signpost {
            branded: "trustc",
            program: "trust",
            lanes: RUSTC_LANES,
        },
    },
    Row {
        upstream: "cargo",
        policy: Policy::Signpost {
            branded: "targo",
            program: EXPLICIT_LANE_PROGRAM,
            lanes: CARGO_LANES,
        },
    },
    Row {
        upstream: "z3",
        policy: Policy::Oracle {
            branded: "ay",
            program: "ay",
        },
    },
];

/// The row for an upstream name, if it is rerouted at all.
#[must_use]
pub fn row_for(name: &str) -> Option<&'static Row> {
    TABLE.iter().find(|row| row.upstream == name)
}

/// The branded name a row resolves to (what a DIRECT row runs, what a SIGNPOST
/// or ORACLE row names).
#[must_use]
pub fn branded_of(row: &Row) -> &'static str {
    match row.policy {
        Policy::Direct { branded, .. }
        | Policy::Signpost { branded, .. }
        | Policy::Oracle { branded, .. } => branded,
    }
}

/// The program whose install lays a row's branded name ([`branded_of`]) — `trust` for
/// `targo`, `trustc`, `tippy`, `trustfmt` and `trustdoc`. What a pending stub for the
/// branded name reports on and waits for ([`crate::stub::program_of`]).
#[must_use]
pub fn program_of(row: &Row) -> &'static str {
    match row.policy {
        Policy::Direct { program, .. }
        | Policy::Signpost { program, .. }
        | Policy::Oracle { program, .. } => program,
    }
}

/// `<prefix>/reroute` — never `bin/`, so `ToolName`'s deny-list keeps meaning
/// exactly what it means.
#[must_use]
pub fn dir(layout: &Layout) -> PathBuf {
    layout.prefix.join(DIR_NAME)
}

/// The stub file for one upstream name.
#[must_use]
pub fn stub_path(layout: &Layout, upstream: &str) -> PathBuf {
    dir(layout).join(upstream)
}

/// The one reading of the [`PASSTHROUGH_ENV`] marker, `aterm_types`' `env_flag_engaged`
/// rule restated so a present-but-empty value (which travels) never counts: engaged iff
/// non-empty and not `"0"`.
#[must_use]
pub fn engaged(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// `+<toolchain>` as the first argument (rustup's own spelling), the user naming a
/// toolchain deliberately ([`aterm_types::rust_lane::explicit_toolchain`]).
#[must_use]
pub fn explicit_toolchain(args: &[String]) -> Option<&str> {
    rust_lane::explicit_toolchain(args)
}

/// Whether `row` names a stock Rust tool: the rows a rustup `+<toolchain>` means
/// something to, and the ones [`rust_lane::trust_spelling`] spells.
fn is_rust_row(row: &Row) -> bool {
    rust_lane::STOCK_TOOLS
        .iter()
        .any(|(stock, _)| *stock == row.upstream)
}

/// Where a channel is pinned for the directory a stub runs in: a toolchain file rustup
/// would read ([`rust_lane::find_pin`]), or `$RUSTUP_TOOLCHAIN`, which rustup ranks above
/// any file. It says what rustup's proxies resolve to and moves nothing else: `targo`,
/// `tippy`, `trustfmt` and `trustdoc` are not rustup proxies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    /// The channel named, verbatim.
    pub channel: String,
    /// Where it was read: a toolchain file, or `$RUSTUP_TOOLCHAIN`.
    pub source: rust_lane::PinSource,
}

impl Pin {
    /// The pin for `cwd`, `$RUSTUP_TOOLCHAIN` (`env_toolchain`) first as rustup ranks
    /// it. `None` when neither names a channel.
    #[must_use]
    pub fn for_dir(cwd: Option<&Path>, env_toolchain: Option<&str>) -> Option<Pin> {
        if let Some(tc) = env_toolchain.filter(|tc| !tc.is_empty()) {
            return Some(Pin {
                channel: tc.to_string(),
                source: rust_lane::PinSource::Env,
            });
        }
        let (file, channel) = rust_lane::find_pin(cwd?)?;
        Some(Pin {
            channel,
            source: rust_lane::PinSource::File(file),
        })
    }
}

// ── messages: byte-stable, stderr-only, one fix sentence ────────────────────

fn escape_clause(upstream: &str) -> String {
    let mut s = String::from("(`aterm --no-reroute` restores upstream '");
    s.push_str(upstream);
    s.push_str("'.)");
    s
}

/// The setting that silences a SIGNPOST row's announcement, as the line spells it.
pub const ANNOUNCE_SETTING: &str = "[reroute] announce = false";

/// Named on every signpost and every refusal: a reader about to run stock cargo anyway is
/// told the one command that measures which toolchain this directory gets.
const MEASURING_LINE: &str = "       `aterm help rust` shows which toolchain this directory gets; the default here is Trust.";

/// The last lines of a SIGNPOST message: [`MEASURING_LINE`], and the setting that silences
/// it, because a line printed on every build must say how to stop it.
fn signpost_closing() -> String {
    format!(
        "{MEASURING_LINE}\n       ({ANNOUNCE_SETTING} in aterm.toml silences this — Settings ▸ Packages.)"
    )
}

/// The lines a stock pin adds under a SIGNPOST's lanes: `channel = "1.97.1"` reads like the
/// repository's lane, but it moves only rustup's proxies. A Trust pin is the default and
/// adds nothing; `$RUSTUP_TOOLCHAIN` is named as the environment, not the directory.
fn pin_lines(pin: &Pin) -> String {
    if rust_lane::is_trust_channel(&pin.channel) {
        return String::new();
    }
    let channel = &pin.channel;
    match &pin.source {
        rust_lane::PinSource::File(file) => format!(
            "       This directory pins stock \"{channel}\", which moves only rustup's proxies, never targo:\n       the Trust spellings above run the Trust toolchain here too. ({})\n",
            file.display()
        ),
        rust_lane::PinSource::Env => format!(
            "       $RUSTUP_TOOLCHAIN names stock \"{channel}\", which moves only rustup's proxies, never targo:\n       the Trust spellings above run the Trust toolchain here too.\n"
        ),
    }
}

/// The DIRECT row's one line: what runs, then the escape — named only where it reaches
/// something (`upstream_found`): stock Rust ships no `clippy`, so with no upstream copy
/// on PATH an `aterm --no-reroute` session has none either.
#[must_use]
pub fn direct_announcement(upstream: &str, branded: &str, upstream_found: bool) -> String {
    let mut line = format!("aterm: running {branded}, Trust's '{upstream}'.");
    if upstream_found {
        line.push(' ');
        line.push_str(&escape_clause(upstream));
    }
    line
}

/// The SIGNPOST announcement, the caller's arguments filled into every lane. Unresolved (the
/// form `tlc` prints, and the frame every Rust row's resolved form shares):
///
/// ```text
/// aterm: running upstream 'cargo' (no proof claim); on Trust the tool is 'targo':
///          targo trust build --release          VERIFIED   — emits a proof claim
///          targo --unverified build --release   UNVERIFIED — no proof claim
///        `aterm help rust` shows which toolchain this directory gets; the default here is Trust.
///        ([reroute] announce = false in aterm.toml silences this — Settings ▸ Packages.)
/// ```
///
/// `toolchain` is the caller's `+trust…` directive, which runs as typed:
/// `aterm: running 'cargo +trust' (rustup's trust toolchain); on Trust the tool is 'targo':`.
///
/// `pin` is the directory's [`Pin`], named under the lanes. `resolved` is what the name
/// resolved to ([`crate::upstream_identity::resolve`], asked for `cargo` and `rustc`): the
/// first line names what runs by what it is, and one line under the lanes names the file.
///
/// ```text
/// aterm: running 'rustc' -> Trust 7d5533b2, via rustup trust (the rustup default); Trust's own spelling is 'trustc':
///          trustc main.rs                      VERIFIED   — proves as it compiles
///          trustc -Ztrust-verify=off main.rs   UNVERIFIED — compiles as vanilla Rust
///        ('rustc' here is /Users//me/toolchains/trust-7d5533b2/bin/rustc.)
/// aterm: running 'rustc' -> STOCK rustc 1.97.1 (8bab26f4f), via rustup 1.97.1-aarch64-apple-darwin (pinned by /r/rust-toolchain.toml); not Trust (no proof claim); on Trust the tool is 'trustc':
/// ```
///
/// Every Rust row's spelling is [`rust_lane::trust_spelling`]'s.
#[must_use]
pub fn signpost_message_in(
    row: &Row,
    lanes: &[Lane],
    args: &[String],
    toolchain: Option<&str>,
    pin: Option<&Pin>,
    resolved: Option<&Resolved>,
) -> String {
    let head = toolchain.map_or(Head::Upstream, Head::Toolchain);
    signpost_block(row, lanes, args, head, pin, resolved, None)
}

/// A SIGNPOST row with no upstream copy on PATH: nothing ran, so the first line says so,
/// and no closing lines follow the lanes:
///
/// ```text
/// aterm: upstream 'cargo' is not on PATH; on Trust the tool is 'targo':
///          targo trust build          VERIFIED   — emits a proof claim
///          targo --unverified build   UNVERIFIED — no proof claim
/// ```
#[must_use]
pub fn no_upstream_message(row: &Row, lanes: &[Lane], args: &[String]) -> String {
    signpost_block(row, lanes, args, Head::NotOnPath, None, None, None)
}

/// The signpost for `<upstream> +<stock channel> …` on any Rust row (`cargo +1.97.1 test`,
/// `rustfmt +1.97.1 x.rs`); `args` is what follows the directive. Upstream then runs as
/// named, since the reroute cannot tell a person's `cargo +stable` from a tool's by-name
/// spawn, after the Trust spelling of the same command and one line saying a stock channel
/// moves only rustup's proxies.
///
/// ```text
/// aterm: running 'cargo +1.97.1' -> STOCK cargo 1.97.1 (c980f4866), via rustup 1.97.1-aarch64-apple-darwin (named on the command line); not Trust (no proof claim); on Trust the tool is 'targo':
///          targo trust test -p x          VERIFIED   — emits a proof claim
///          targo --unverified test -p x   UNVERIFIED — no proof claim
///        ('cargo' here is /Users//me/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin/cargo.)
///        A stock channel, named here or pinned, moves only rustup's proxies, never targo.
///        `aterm help rust` shows which toolchain this directory gets; the default here is Trust.
///        ([reroute] announce = false in aterm.toml silences this — Settings ▸ Packages.)
/// ```
///
/// `resolved` is what the name resolved to ([`signpost_message_in`]) — `cargo` and `rustc`
/// only; a DIRECT row (`rustfmt +1.97.1`) says "running upstream" of the channel it named.
#[must_use]
pub fn stock_channel_signpost(
    row: &Row,
    toolchain: &str,
    args: &[String],
    resolved: Option<&Resolved>,
) -> String {
    signpost_block(
        row,
        lanes_of(row),
        args,
        Head::Stock(toolchain),
        None,
        resolved,
        None,
    )
}

/// A SIGNPOST row's lanes; none for any other row.
fn lanes_of(row: &Row) -> &'static [Lane] {
    match row.policy {
        Policy::Signpost { lanes, .. } => lanes,
        Policy::Direct { .. } | Policy::Oracle { .. } => &[],
    }
}

/// The refusal inside a Trust source checkout at `root`: `cargo`/`rustc` resolved to
/// something that is not Trust, so nothing ran. It names what it resolved to, the checkout
/// and why, the Trust spelling, the file, and `aterm --no-reroute` to run that copy anyway
/// (not its path: a `cargo` run by path still spawns `rustc` by name, refused in turn).
///
/// ```text
/// aterm: 'cargo' did not run -> STOCK cargo 1.97.1 (c980f4866), via rustup 1.97.1-aarch64-apple-darwin (pinned by /t/rust-toolchain.toml); not Trust,
///        and /t is a Trust source checkout (its src/stage0 says compiler_dist_channel=trust), where only Trust may build; on Trust the tool is 'targo':
///          targo trust build          VERIFIED   — emits a proof claim
///          targo --unverified build   UNVERIFIED — no proof claim
///        ('cargo' here is /Users//me/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin/cargo; `aterm --no-reroute` runs it as named.)
///        `aterm help rust` shows which toolchain this directory gets; the default here is Trust.
/// ```
#[must_use]
pub fn trust_checkout_refusal(
    row: &Row,
    args: &[String],
    toolchain: Option<&str>,
    resolved: &Resolved,
    root: &Path,
) -> String {
    let head = match toolchain {
        Some(tc) if rust_lane::is_trust_channel(tc) => Head::Toolchain(tc),
        Some(tc) => Head::Stock(tc),
        None => Head::Upstream,
    };
    signpost_block(
        row,
        lanes_of(row),
        args,
        head,
        None,
        Some(resolved),
        Some(root),
    )
}

/// What the first line of a SIGNPOST block says of the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Head<'a> {
    /// Upstream runs as typed.
    Upstream,
    /// Upstream runs under the caller's `+<toolchain>` — a Trust one ([`signpost_message_in`]).
    Toolchain(&'a str),
    /// Upstream runs under the caller's `+<toolchain>` — a STOCK one
    /// ([`stock_channel_signpost`]).
    Stock(&'a str),
    /// No upstream copy on PATH: nothing ran.
    NotOnPath,
}

/// [`signpost_message_in`], [`no_upstream_message`], [`stock_channel_signpost`] and
/// [`trust_checkout_refusal`] (`refused_in`, the checkout's root: nothing runs).
fn signpost_block(
    row: &Row,
    lanes: &[Lane],
    args: &[String],
    head: Head<'_>,
    pin: Option<&Pin>,
    resolved: Option<&Resolved>,
    refused_in: Option<&Path>,
) -> String {
    let upstream = row.upstream;
    let branded = branded_of(row);
    // The first line's state: what runs (by what it is, once resolved) or that nothing
    // does. `verb` is a one-spelling cargo verb's (`cargo clippy`, `cargo run`); `claim` is
    // said only of what is not known to be Trust, and never of a command that did not run.
    let head_line = |verb: &str, claim: &str| {
        let typed = match head {
            Head::Upstream | Head::NotOnPath => join_command(upstream, verb),
            Head::Stock(toolchain) | Head::Toolchain(toolchain) => {
                join_command(&format!("{upstream} +{toolchain}"), verb)
            }
        };
        match (head, resolved, refused_in) {
            (Head::NotOnPath, ..) => format!("aterm: upstream '{upstream}' is not on PATH"),
            (_, Some(what), Some(root)) => format!(
                "aterm: '{typed}' did not run -> {},\n       and {} is a Trust source checkout (its src/stage0 says compiler_dist_channel=trust), where only Trust may build",
                what.verdict(upstream),
                root.display()
            ),
            (_, Some(what), None) if what.is_trust() => {
                format!("aterm: running '{typed}' -> {}", what.verdict(upstream))
            }
            (_, Some(what), None) => {
                format!(
                    "aterm: running '{typed}' -> {}{claim}",
                    what.verdict(upstream)
                )
            }
            (Head::Upstream | Head::Stock(_), None, _) => {
                format!("aterm: running upstream '{typed}'{claim}")
            }
            (Head::Toolchain(toolchain), None, _) => {
                format!("aterm: running '{typed}' (rustup's {toolchain} toolchain)")
            }
        }
    };
    // Where the Trust tool is named after the first line: what resolved to Trust is already
    // Trust, so its lanes are Trust's own spelling, not a replacement.
    let intro = |tool: &str| {
        if resolved.is_some_and(Resolved::is_trust) && refused_in.is_none() {
            format!("; Trust's own spelling is '{tool}':\n")
        } else {
            format!("; on Trust the tool is '{tool}':\n")
        }
    };
    let mut out = String::new();
    let mut trust_tool = branded.to_string();
    if let Some(spellings) = rust_lane::trust_spelling(upstream, args) {
        // A Rust row: the shared vocabulary decides the spelling and its lane.
        let verb = (upstream == "cargo")
            .then(|| rust_lane::cargo_verb_index(args))
            .flatten()
            .map_or("", |i| args[i].as_str());
        match spellings.as_slice() {
            // A verb targo is not known to have (`cargo nextest`): the spelling is
            // conditional, and the line says so rather than naming a command that
            // answers "no such command".
            [_] if rust_lane::is_unknown_verb(&spellings) => {
                out.push_str(&head_line(verb, " (no proof claim)"));
                out.push_str(&format!(
                    "; targo is not known to have `{verb}`\n       (a cargo extension targo lacks has no Trust spelling). If it has, it is:\n"
                ));
            }
            // One spelling with no lane: `cargo clippy` → `targo tippy`, `cargo fmt`,
            // `cargo metadata`, `rustfmt` → `trustfmt`. Named by the Trust tool and, for
            // cargo, its verb — a flag is not part of a tool's name.
            [one] if one.label.is_empty() => {
                trust_tool = if upstream == "cargo" {
                    match verb {
                        "" => String::from("targo"),
                        "clippy" => String::from("targo tippy"),
                        verb => format!("targo {verb}"),
                    }
                } else {
                    one.command.split(' ').next().unwrap_or(branded).to_string()
                };
                out.push_str(&head_line(verb, ""));
                out.push_str(&intro(&trust_tool));
            }
            // One lane only: `cargo run`/`doc`/`install`… have no verified lane, which the
            // lane's own note says.
            [_] => {
                out.push_str(&head_line(verb, " (no proof claim)"));
                out.push_str(&intro(branded));
            }
            _ => {
                out.push_str(&head_line("", " (no proof claim)"));
                out.push_str(&intro(branded));
            }
        }
        out.push_str(&rust_lane::render_spellings(&spellings, "         "));
    } else if lanes.is_empty() {
        // Not a Rust row (`tlc`): one spelling, equivalence unproven.
        out.push_str(&head_line("", " (no proof claim)"));
        out.push_str(&format!("; on this toolchain the tool is '{branded}'\n"));
        out.push_str("       (drop-in equivalence is not yet proven):\n");
        out.push_str(&format!(
            "         {}\n",
            join_command(branded, &args.join(" "))
        ));
    } else {
        out.push_str(&head_line("", " (no proof claim)"));
        out.push_str(&intro(branded));
        let rest = args.join(" ");
        let rendered: Vec<rust_lane::Spelling> = lanes
            .iter()
            .map(|lane| rust_lane::Spelling {
                label: lane.label,
                command: join_command(lane.command, &rest),
                note: lane.note,
            })
            .collect();
        out.push_str(&rust_lane::render_spellings(&rendered, "         "));
    }
    // The file the name resolved to, under the lanes — and, refused, how to run it anyway.
    if let Some(what) = resolved {
        let real = what.real.display();
        match refused_in {
            Some(_) => out.push_str(&format!(
                "       ('{upstream}' here is {real}; `aterm --no-reroute` runs it as named.)\n"
            )),
            None => out.push_str(&format!("       ('{upstream}' here is {real}.)\n")),
        }
    }
    if let Head::Stock(_) = head {
        let tool = trust_tool.split(' ').next().unwrap_or(branded);
        out.push_str(&format!(
            "       A stock channel, named here or pinned, moves only rustup's proxies, never {tool}.\n"
        ));
    }
    if let Some(pin) = pin {
        out.push_str(&pin_lines(pin));
    }
    if head == Head::NotOnPath {
        out.pop();
    } else if refused_in.is_some() {
        // A refusal is never silenced, so the setting that silences a signpost is not named.
        out.push_str(MEASURING_LINE);
    } else {
        out.push_str(&signpost_closing());
    }
    out
}

/// The ORACLE refusal, naming the upstream copy's own path when there is one: a path
/// never meets a stub, so it is the consent to run the real tool. Why the name is
/// refused (the branded tool is measured against it) is `aterm pkg which`'s answer
/// ([`policy_summary`]), not this line's.
#[must_use]
pub fn oracle_message(upstream: &str, upstream_path: Option<&Path>) -> String {
    // The escape is named only where it reaches something: with no upstream copy on
    // PATH, an `aterm --no-reroute` session has none either.
    let reach = match upstream_path {
        Some(path) => format!(
            "run {} to reach the real {upstream} (`aterm --no-reroute` lifts this)",
            path.display()
        ),
        None => format!("no upstream '{upstream}' is on PATH"),
    };
    format!("aterm: '{upstream}' does not run by name in an aterm session — {reach}.")
}

/// The ORACLE row's line before upstream runs: what runs, and the tool this toolchain
/// prefers — never a refusal. `fix` is the one command that makes that tool runnable
/// here when it is not ([`oracle_fix`]), said after its name.
#[must_use]
pub fn oracle_notice(upstream: &str, branded: &str, fix: Option<&str>) -> String {
    format!(
        "aterm: running upstream '{upstream}'; on this toolchain the solver is '{branded}'{}.",
        fix_clause(fix)
    )
}

/// The ORACLE row with no upstream copy on PATH: nothing ran, and the preferred tool —
/// with `fix` as in [`oracle_notice`].
#[must_use]
pub fn oracle_not_on_path_message(upstream: &str, branded: &str, fix: Option<&str>) -> String {
    format!(
        "aterm: upstream '{upstream}' is not on PATH; on this toolchain the solver is '{branded}'{}.",
        fix_clause(fix)
    )
}

/// ` (<fix>)`, or nothing.
fn fix_clause(fix: Option<&str>) -> String {
    fix.map_or_else(String::new, |f| format!(" ({f})"))
}

/// What makes the ORACLE row's solver runnable here, as its lines say it: nothing when it
/// runs (or its shim's own line will speak), else the one command that fixes it.
fn oracle_fix(install: &Install, branded: &str) -> Option<String> {
    match install {
        Install::Runs(_) | Install::Shim(_) | Install::Pending(_) => None,
        Install::ShimLost => Some("`aterm pkg repair` restores its shim".to_string()),
        Install::Absent => Some(format!("`aterm pkg install {branded}` installs it")),
    }
}

/// Why a DIRECT row's branded tool cannot run here — the two cases that name a fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unrunnable {
    /// Nothing here holds it ([`Install::Absent`]): `aterm pkg install <program>`.
    Absent,
    /// Its program's live build holds it, its shim is gone ([`Install::ShimLost`]):
    /// `aterm pkg repair`.
    ShimLost,
}

impl Unrunnable {
    /// The line when nothing runs: the strict answer, and the answer with no upstream copy.
    fn refusal(self, upstream: &str, branded: &str, program: &str) -> String {
        match self {
            Self::Absent => not_installed_message(upstream, branded, program),
            Self::ShimLost => lost_shim_message(upstream, branded),
        }
    }

    /// The line before the upstream copy runs in the branded tool's place (the default).
    fn fallback(self, upstream: &str, branded: &str, program: &str) -> String {
        match self {
            Self::Absent => fallback_not_installed_message(upstream, branded, program),
            Self::ShimLost => fallback_lost_shim_message(upstream, branded),
        }
    }
}

/// A DIRECT row whose branded tool is not installed, upstream running instead: what runs
/// first, then the one command that installs the Trust tool.
#[must_use]
pub fn fallback_not_installed_message(upstream: &str, branded: &str, program: &str) -> String {
    format!(
        "aterm: running upstream '{upstream}' — {branded} is not installed (`aterm pkg install {program}` installs it)."
    )
}

/// A DIRECT row whose branded tool's program is still to come (a pending stub stands at its
/// name, [`Install::Pending`]), upstream running instead of a wait for it.
#[must_use]
pub fn fallback_pending_message(upstream: &str, branded: &str, program: &str) -> String {
    format!(
        "aterm: running upstream '{upstream}' — {branded} is not installed yet (it comes with {program})."
    )
}

/// A DIRECT row whose branded tool lost its shim, upstream running instead.
#[must_use]
pub fn fallback_lost_shim_message(upstream: &str, branded: &str) -> String {
    format!(
        "aterm: running upstream '{upstream}' — {branded}'s shim is missing (`aterm pkg repair` restores it)."
    )
}

/// `cargo trust <verb>` / `cargo --unverified <verb>`: the user named the lane
/// in cargo's own vocabulary (cargo and targo are one multicall binary, and
/// `cargo trust check` is documented as targo's strict driver). Nothing is
/// silent, so nothing is refused: the branded tool runs, with one line.
pub const EXPLICIT_LANE_VERBS: &[&str] = &["trust", "--unverified"];

/// The program `aterm pkg install` names for `targo`, the tool an explicit lane runs.
pub const EXPLICIT_LANE_PROGRAM: &str = "trust";

/// `cargo metadata`, `cargo locate-project`, `cargo version` and `cargo --version`: the
/// lane-free queries rust-analyzer, an editor and a build script make by name. They compile
/// nothing, so there is no lane to name, and whoever asked parses their stdout — so they run
/// the managed `targo` itself, saying nothing (`.vscode` used to work around the refusal
/// with `CARGO=targo`). Every other verb keeps its signpost or refusal; with no runnable
/// `targo` these are answered as any other verb.
pub const LANE_FREE_QUERIES: &[&str] = &["metadata", "locate-project", "version", "--version"];

/// The one line printed when `cargo trust …`/`cargo --unverified …` runs `targo`. No
/// escape: stock cargo has no such subcommand, so upstream would not run this command.
#[must_use]
pub fn explicit_lane_note(upstream: &str, branded: &str) -> String {
    format!("aterm: running {branded}, Trust's '{upstream}'.")
}

/// A branded tool that is not installed: nothing ran, and the one command that installs it.
#[must_use]
pub fn not_installed_message(upstream: &str, branded: &str, program: &str) -> String {
    format!(
        "aterm: '{upstream}' did not run — {branded} is not installed; `aterm pkg install {program}` installs it."
    )
}

/// A branded tool its program's live build holds, with its shim gone: nothing ran, and
/// `repair` re-lays the shim (`install` would answer up to date).
#[must_use]
pub fn lost_shim_message(upstream: &str, branded: &str) -> String {
    format!(
        "aterm: '{upstream}' did not run — {branded}'s shim is missing; `aterm pkg repair` restores it."
    )
}

/// No upstream copy on PATH — the same words the stub's escape walk prints.
#[must_use]
pub fn not_on_path_message(upstream: &str) -> String {
    format!("aterm: upstream '{upstream}' is not on PATH")
}

/// Printed by a strict stub ([`crate::config::RerouteConfig::strict`]) when atpkg is
/// unreachable: fail closed, with the escape named, never a silent fall-through.
#[must_use]
pub fn unreachable_message(upstream: &str) -> String {
    format!(
        "aterm: '{upstream}' did not run — aterm's package manager is missing (`aterm --no-reroute` restores upstream '{upstream}')"
    )
}

/// Printed by the stub itself (the default) when atpkg is unreachable and an upstream copy
/// is on PATH: it runs, after this line — never silently, and never a stop.
#[must_use]
pub fn unreachable_fallback_message(upstream: &str) -> String {
    format!(
        "aterm: running upstream '{upstream}' — aterm's package manager is not reachable (`aterm pkg repair` re-lays this stub)."
    )
}

/// Printed by the stub itself (the default) when atpkg is unreachable and no upstream copy
/// is on PATH either: nothing exists to run.
#[must_use]
pub fn unreachable_not_on_path_message(upstream: &str) -> String {
    format!(
        "aterm: '{upstream}' did not run — aterm's package manager is not reachable and no upstream '{upstream}' is on PATH."
    )
}

/// Printed by a `cargo`/`rustc` stub ([`names_what_runs`]) when atpkg is unreachable and
/// the directory is inside a Trust source checkout as far as `sh` can tell
/// ([`sh_source_checkout_test`], then a `src/stage0` line naming the `trust` channel):
/// nothing can be resolved, so nothing is known to be Trust, and there that does not run
/// ([`trust_checkout_refusal`]'s rule). Exit [`REFUSAL_EXIT`].
#[must_use]
pub fn unreachable_in_checkout_message(upstream: &str) -> String {
    format!(
        "aterm: '{upstream}' did not run — aterm's package manager is not reachable, so what '{upstream}' resolves to is unknown, and this is a Trust source checkout, where only Trust may build. `aterm pkg repair` re-lays this stub; `aterm --no-reroute` runs upstream '{upstream}'."
    )
}

/// `atpkg __reroute` with no resolvable store (no `HOME`, an `env -i` wrapper) and an
/// upstream copy on PATH: it runs after this line, which names what `cargo`/`rustc`
/// resolved to (`resolved`, [`Resolved::verdict`]) as every signpost does.
#[must_use]
pub fn storeless_message(upstream: &str, args: &[String], resolved: Option<&Resolved>) -> String {
    let tail = "no package store resolves here, so its Trust tool was not looked up.";
    match resolved {
        Some(what) => {
            let command = match explicit_toolchain(args) {
                Some(tc) => format!("{upstream} +{tc}"),
                None => upstream.to_string(),
            };
            format!(
                "aterm: running '{command}' -> {} — {tail}",
                what.verdict(upstream)
            )
        }
        None => format!("aterm: running upstream '{upstream}' — {tail}"),
    }
}

/// `atpkg __reroute` with no resolvable store under `[reroute] strict = true`: nothing
/// ran, and the escape.
#[must_use]
pub fn storeless_refusal(upstream: &str) -> String {
    format!("aterm: '{upstream}' did not run (`aterm --no-reroute` restores upstream '{upstream}')")
}

fn join_command(command: &str, rest: &str) -> String {
    if rest.is_empty() {
        command.to_string()
    } else {
        format!("{command} {rest}")
    }
}

/// The row's policy as one clause, `<args>` standing for the caller's arguments: what
/// `aterm pkg which <upstream>` answers, built from the same row the stub applies so the two
/// cannot drift. The escape and an upstream run are named only where an upstream copy is
/// (`upstream_found`, [`upstream_on_path`]). `strict` is `[reroute] strict`: the ORACLE row
/// refuses, and a DIRECT row names no upstream fallback.
#[must_use]
pub fn policy_summary(row: &Row, upstream_found: bool, strict: bool) -> String {
    let upstream = row.upstream;
    let escape = if upstream_found {
        format!(" {}", escape_clause(upstream))
    } else {
        String::new()
    };
    let then_run = if upstream_found {
        String::from(", then run upstream")
    } else {
        format!("; no upstream '{upstream}' is on PATH")
    };
    // A stock `+<channel>` on a Rust row runs upstream as named, after the Trust
    // spelling ([`stock_channel_signpost`]) — said only where an upstream copy is there
    // to run, like the escape.
    let stock_channel = |then: &str| {
        if upstream_found && is_rust_row(row) {
            format!("; '{upstream} +<stock channel> <args>' {then}")
        } else {
            String::new()
        }
    };
    match row.policy {
        Policy::Direct {
            branded,
            source_verb,
            ..
        } => {
            let stock = stock_channel("runs upstream as named, after the Trust spelling");
            // A missing Trust tool runs upstream with a line, said only where an
            // upstream copy is there to run.
            let fallback = if upstream_found && !strict {
                format!(" (upstream '{upstream}', with a line, while {branded} is missing)")
            } else {
                String::new()
            };
            match source_verb {
                Some(SourceVerb { verb, ext }) => format!(
                    "runs '{branded} <args>' with one stderr line ('{branded} {verb} <file>' for a bare `{ext}` file){fallback}{stock}{escape}"
                ),
                None => {
                    format!("runs '{branded} <args>' with one stderr line{fallback}{stock}{escape}")
                }
            }
        }
        Policy::Signpost {
            branded, lanes: [], ..
        } => format!(
            "announced, naming '{branded} <args>' (drop-in equivalence is not yet proven){then_run}{escape}"
        ),
        Policy::Signpost { lanes, .. } => {
            let named: Vec<String> = lanes
                .iter()
                .map(|lane| format!("'{} <args>'", lane.command))
                .collect();
            let mut s = format!("announced, naming {}", named.join(" / "));
            if upstream == "cargo" {
                // The verb decides the lane (`aterm_types::rust_lane`).
                s.push_str(" for build/check/test, 'targo --unverified <args>' for a verb with no verified lane (run, doc, install, …), 'targo <args>' for one that takes no lane (metadata, tree, …); ");
                let verbs: Vec<String> = CARGO_VERB_REROUTES
                    .iter()
                    .map(|(verb, reroute)| format!("'{upstream} {verb}' → '{reroute}'"))
                    .collect();
                s.push_str(&verbs.join(", "));
            }
            s.push_str(&stock_channel("names the same Trust spelling"));
            // The line names the resolved copy only where there is one to resolve.
            let resolves = names_what_runs(row) && upstream_found;
            if resolves {
                s.push_str(&format!(
                    "; the line names what '{upstream}' resolves to (by its `-vV`: Trust, STOCK, or not known to be Trust)"
                ));
            }
            s.push_str(&then_run);
            if resolves {
                s.push_str(" — inside a Trust source checkout, only when that is Trust");
            }
            s.push_str(&escape);
            s
        }
        Policy::Oracle { branded, .. } if strict && upstream_found => format!(
            "refused ('{branded}' is measured against it); its own path reaches the real {upstream}{escape}"
        ),
        Policy::Oracle { branded, .. } if strict => format!(
            "refused ('{branded}' is measured against it); no upstream '{upstream}' is on PATH"
        ),
        Policy::Oracle { branded, .. } => format!(
            "announced, naming '{branded}' (it is measured against {upstream}){then_run}{escape}"
        ),
    }
}

// ── the stub ────────────────────────────────────────────────────────────────

/// One line of both stubs' pass-through walks: `continue` past a candidate whose line 2 is
/// [`STUB_MARKER`] (the rule [`is_reroute_stub`] gives [`upstream_on_path`]), so the shell's
/// walk and atpkg's skip the same files. Builtins only (`[`, `read`). It catches a stub under
/// any other name (a symlink, a hard link, a copy: `cp <prefix>/reroute/* ~/bin/` skips the
/// dotfile [`DIR_MARKER_FILE`]), which an inode test misses and would exec forever. `-r`
/// first and `2>/dev/null` ahead of the `<`, so an unreadable candidate prints nothing; it is
/// not a stub, and is exec'd.
fn sh_skip_a_stub(indent: &str, program: &str) -> String {
    let mut s = String::from(indent);
    s.push_str(&format!(
        "if [ -r \"$__d/{program}\" ] && {{ IFS= read -r __aterm_l; IFS= read -r __aterm_l; }} 2>/dev/null < \"$__d/{program}\" && [ \"$__aterm_l\" = "
    ));
    s.push_str(&crate::stub::sh_single_quote(STUB_MARKER));
    s.push_str(" ]; then continue; fi\n");
    s
}

/// The walk both of a stub's upstream arms share, at `indent`: the first absolute-entry
/// executable `$__aterm_name` on `PATH` past every reroute directory (by [`DIR_MARKER_FILE`],
/// any spelling, any prefix) and every stub under another name ([`sh_skip_a_stub`]), exec'd
/// after `before_exec` (lines of `sh`). Falls through, `IFS` and globbing restored, when
/// there is none.
fn sh_upstream_walk(indent: &str, before_exec: &[String]) -> String {
    sh_path_walk(indent, "$__aterm_name", "", before_exec)
}

/// [`sh_upstream_walk`] for any `program` (a word of `sh`, placed after `$__d/`), exec'd
/// with `args` (`sh` text, empty or ending in a space) ahead of the caller's `"$@"`. The
/// stub's search for atpkg and the pending-install stub's ([`crate::stub`]) use it too:
/// unlike `command -v`, it takes absolute entries only, so a relative or empty PATH entry
/// never runs a cloned tree's `./atpkg`.
///
/// Pure shell, no subshell and no external command: it runs before anything is resolved,
/// on whatever PATH the caller has. `IFS=:` splits PATH; `set -f` keeps a `*` in an entry
/// from globbing; the parameter test says "absolute" without a `case`, which macOS /bin/sh
/// (bash 3.2) mis-parses inside `$(…)`.
pub(crate) fn sh_path_walk(
    indent: &str,
    program: &str,
    args: &str,
    before_exec: &[String],
) -> String {
    let line = |s: &mut String, depth: &str, text: &str| {
        s.push_str(indent);
        s.push_str(depth);
        s.push_str(text);
        s.push('\n');
    };
    let mut s = String::new();
    line(&mut s, "", "__aterm_ifs=$IFS; IFS=:; set -f");
    line(&mut s, "", "for __d in $PATH; do");
    line(&mut s, "  ", "[ -z \"$__d\" ] && continue");
    line(&mut s, "  ", "[ \"${__d#/}\" = \"$__d\" ] && continue");
    // By marker file, not string equality with our own directory: a second prefix's
    // reroute dir, a symlink to ours, or ours with a trailing slash is still one, and
    // exec'ing its stub under the escape would loop forever.
    line(
        &mut s,
        "  ",
        &format!("[ -f \"$__d/{DIR_MARKER_FILE}\" ] && continue"),
    );
    line(
        &mut s,
        "  ",
        &format!("if [ -x \"$__d/{program}\" ] && [ ! -d \"$__d/{program}\" ]; then"),
    );
    // A stub under another name (a link, hard link or copy in an ordinary PATH directory)
    // has no marker beside it: known by what it says ([`sh_skip_a_stub`]).
    s.push_str(&sh_skip_a_stub(&format!("{indent}    "), program));
    line(&mut s, "    ", "IFS=$__aterm_ifs; set +f");
    for text in before_exec {
        line(&mut s, "    ", text);
    }
    line(
        &mut s,
        "    ",
        &format!("exec \"$__d/{program}\" {args}\"$@\""),
    );
    line(&mut s, "  ", "fi");
    line(&mut s, "", "done");
    line(&mut s, "", "IFS=$__aterm_ifs; set +f");
    s
}

/// How many directories up [`sh_source_checkout_test`] walks before it stops and counts the
/// directory as one that may be a checkout: deeper than any real tree, so a cwd whose
/// parents cannot be read (a removed directory) is judged rather than missed.
const SH_WALK_DEPTH: u32 = 64;

/// Lines of `sh`, at `indent`, setting `__aterm_src` to the nearest directory up the tree
/// holding an `x.py` and a `src/stage0` — a Rust source checkout, Trust's or upstream's —
/// and to nothing when there is none. Only atpkg tells the two apart
/// ([`crate::upstream_identity::trust_checkout`]); this decides whether to ask it, and must
/// never miss one atpkg would find.
///
/// Physical, as atpkg walks: by `.`, `..`, `../..`, which the kernel resolves through the
/// real parent, never `$PWD` (a logical path through a symlink into a checkout has
/// ancestors outside it). Root is where `..` is the same directory (`-ef`); past
/// [`SH_WALK_DEPTH`] levels the walk stops and counts where it stopped. Builtins only, no
/// subshell: it runs before every by-name `cargo`/`rustc` a reroute-run command spawns.
fn sh_source_checkout_test(indent: &str) -> String {
    let lines = [
        "__aterm_src=; __aterm_up=.; __aterm_n=0".to_string(),
        "while :; do".to_string(),
        "  if [ -f \"$__aterm_up/x.py\" ] && [ -f \"$__aterm_up/src/stage0\" ]; then __aterm_src=$__aterm_up; break; fi".to_string(),
        "  [ \"$__aterm_up\" -ef \"$__aterm_up/..\" ] && break".to_string(),
        "  __aterm_n=$((__aterm_n + 1))".to_string(),
        format!("  if [ \"$__aterm_n\" -ge {SH_WALK_DEPTH} ]; then __aterm_src=$__aterm_up; break; fi"),
        "  __aterm_up=$__aterm_up/..".to_string(),
        "done".to_string(),
    ];
    lines
        .iter()
        .map(|line| format!("{indent}{line}\n"))
        .collect()
}

/// Lines of `sh`, at `indent`, after [`sh_source_checkout_test`]: `__aterm_trust` is set
/// when that directory's `src/stage0` has a line naming `compiler_dist_channel` and `trust`
/// — a loose reading of [`crate::upstream_identity::trust_checkout`]'s, so it refuses
/// wherever atpkg would and never lets through what atpkg would refuse.
fn sh_stage0_names_trust(indent: &str) -> String {
    let lines = [
        "__aterm_trust=",
        "if [ -n \"$__aterm_src\" ] && [ -r \"$__aterm_src/src/stage0\" ]; then",
        "  while IFS= read -r __aterm_l || [ -n \"$__aterm_l\" ]; do",
        "    case $__aterm_l in *compiler_dist_channel*=*trust*) __aterm_trust=1; break;; esac",
        "  done < \"$__aterm_src/src/stage0\"",
        "fi",
    ];
    lines
        .iter()
        .map(|line| format!("{indent}{line}\n"))
        .collect()
}

/// The POSIX stub for `upstream`. Its shape is load-bearing:
///
/// * line 2 is [`STUB_MARKER`] (recognition);
/// * every `exec` goes through a variable, never the literal `exec '` a managed shim
///   carries, so the shim parser sees no target;
/// * the escape is decided in `sh` before atpkg is consulted, by the walk past every reroute
///   directory and every stub under another name ([`sh_path_walk`]), so it works with no
///   atpkg at all;
/// * under [`ANNOUNCED_ENV`], or in a [`CARGO_COMPILERS`] stub for a stock cargo's own
///   compile ([`CARGO_CRATE_ENV`] without [`TARGO_CRATE_ENV`], then stamped with that
///   marker), it runs upstream by the same walk, silently, except that a `cargo`/`rustc`
///   stub in a directory that may be inside a Rust source checkout
///   ([`sh_source_checkout_test`]) asks atpkg, which judges it as the name typed. Decided
///   before atpkg, so no later arm can tell a `rustup run` cargo's compiles from a
///   stub-started one's;
/// * with atpkg unreachable it runs upstream by the same walk after one line
///   ([`unreachable_fallback_message`]), marking the child [`ANNOUNCED_ENV`]; 127 only when
///   no upstream copy is on PATH. A `strict` stub (carried in its bytes: the stub cannot
///   read aterm.toml) fails closed with exit [`REFUSAL_EXIT`], and a `cargo`/`rustc` stub
///   inside a Trust source checkout fails closed in either mode
///   ([`unreachable_in_checkout_message`]).
///
/// `atpkg` is [`crate::stub::Embedder`]'s choice; an empty path names none (`ATPKG=''`,
/// which `[ -x ]` refuses), and the stub goes straight to the first absolute-entry `atpkg`
/// on PATH ([`sh_path_walk`]).
#[must_use]
pub fn stub_body_sh(upstream: &str, atpkg: &Path, reroute_dir: &Path, strict: bool) -> String {
    let name = crate::stub::sh_single_quote(upstream);
    let judged = row_for(upstream).is_some_and(names_what_runs);
    let engaged =
        |marker: &str| format!("[ -n \"${{{marker}:-}}\" ] && [ \"${{{marker}:-}}\" != \"0\" ]");
    let mut s = String::from("#!/bin/sh\n");
    s.push_str(STUB_MARKER);
    s.push_str("\n# Rerouting inside an aterm session (aterm help reroute). Not a managed tool:\n");
    s.push_str("# this file resolves to no store target and is never proof of an install.\n");
    s.push_str("__aterm_reroute_dir=");
    s.push_str(&crate::stub::sh_single_quote(
        &reroute_dir.to_string_lossy(),
    ));
    s.push_str("\n__aterm_name=");
    s.push_str(&name);
    s.push('\n');
    // A stock cargo's own compile — cargo's mark without Trust's — is a child of a command
    // as an announced one is (module doc, "Escapes"), whichever way the cargo started.
    let compile = if CARGO_COMPILERS.contains(&upstream) {
        format!(
            " || {{ [ -n \"${{{CARGO_CRATE_ENV}:-}}\" ] && [ -z \"${{{TARGO_CRATE_ENV}:-}}\" ]; }}"
        )
    } else {
        String::new()
    };
    let mark = format!("{ANNOUNCED_ENV}=1; export {ANNOUNCED_ENV}");
    // The escape — and, for a name that is never judged, a child of a command the reroute
    // ran: upstream, silently, with no atpkg; that child's own spawns marked as its.
    s.push_str("if ");
    s.push_str(&engaged(PASSTHROUGH_ENV));
    if !judged {
        s.push_str(" || { ");
        s.push_str(&engaged(ANNOUNCED_ENV));
        s.push_str("; }");
        s.push_str(&compile);
    }
    s.push_str("; then\n");
    let escape_marks: &[String] = if judged {
        &[]
    } else {
        std::slice::from_ref(&mark)
    };
    s.push_str(&sh_upstream_walk("  ", escape_marks));
    s.push_str("  printf '%s\\n' \"");
    s.push_str(&not_on_path_message("$__aterm_name"));
    s.push_str("\" 1>&2\n");
    s.push_str("  exit 127\nfi\n");
    if judged {
        // A child of a command the reroute ran, or a stock cargo's compile: marked as one,
        // and silently upstream, unless the directory may be inside a Rust source checkout —
        // there atpkg judges it, quietly, as the marker tells it.
        s.push_str("if ");
        s.push_str(&engaged(ANNOUNCED_ENV));
        s.push_str(&compile);
        s.push_str("; then\n  ");
        s.push_str(&mark);
        s.push('\n');
        s.push_str(&sh_source_checkout_test("  "));
        s.push_str("  if [ -z \"$__aterm_src\" ]; then\n");
        s.push_str(&sh_upstream_walk("    ", &[]));
        s.push_str("    printf '%s\\n' \"");
        s.push_str(&not_on_path_message("$__aterm_name"));
        s.push_str("\" 1>&2\n");
        s.push_str("    exit 127\n  fi\nfi\n");
    }
    s.push_str("ATPKG=");
    s.push_str(&crate::stub::sh_single_quote(&atpkg.to_string_lossy()));
    s.push_str("\nif [ -x \"$ATPKG\" ]; then exec \"$ATPKG\" ");
    s.push_str(HIDDEN_VERB);
    s.push_str(" \"$__aterm_name\" \"$@\"; fi\n");
    s.push_str(&sh_path_walk(
        "",
        "atpkg",
        &format!("{HIDDEN_VERB} \"$__aterm_name\" "),
        &[],
    ));
    let say = |message: String| {
        format!(
            "printf '%s\\n' {} 1>&2",
            crate::stub::sh_single_quote(&message)
        )
    };
    if strict {
        s.push_str(&say(unreachable_message(upstream)));
        s.push_str("\nexit ");
        s.push_str(&REFUSAL_EXIT.to_string());
        s.push('\n');
        return s;
    }
    if judged {
        // Nothing can be resolved, so nothing is known to be Trust: inside a Trust source
        // checkout that does not run.
        s.push_str(&sh_source_checkout_test(""));
        s.push_str(&sh_stage0_names_trust(""));
        s.push_str("if [ -n \"$__aterm_trust\" ]; then\n  ");
        s.push_str(&say(unreachable_in_checkout_message(upstream)));
        s.push_str("\n  exit ");
        s.push_str(&REFUSAL_EXIT.to_string());
        s.push_str("\nfi\n");
    }
    // Upstream, as the escape reaches it: announced, and marked [`ANNOUNCED_ENV`] so its
    // own spawns (cargo's `rustc`) pass without a line each and are still judged.
    s.push_str(&sh_upstream_walk(
        "",
        &[
            say(unreachable_fallback_message(upstream)),
            format!("{ANNOUNCED_ENV}=1; export {ANNOUNCED_ENV}"),
        ],
    ));
    s.push_str(&say(unreachable_not_on_path_message(upstream)));
    s.push_str("\nexit 127\n");
    s
}

/// THE AGENT PROGRAMS' STUB (2026-09-23): which copy of `claude`/`codex` runs is decided
/// when the program RUNS, never when the shell started.
///
/// Measured that day on the owner's machine: the session shell he restarted `claude` in
/// had started on 2026-09-10 and survived every seamless self-update since; a shell's PATH
/// is fixed when it starts, and that one's was `pkg/reroute, ~/.local/bin, …,
/// /opt/homebrew/bin, …, pkg/bin` with NO `pkg/agents` — so `claude` ran the vendor's own
/// self-updating install and not the managed build the pass had just landed, while
/// `aterm pkg doctor` said SHADOWED. The rc hook of 03513b5d7 fixes the PATH of NEW shells
/// only. The one atpkg directory at the front of PATH in every generation of session shell
/// is this one — so the decision lives here, in `sh`, read at exec time:
///
/// A shell's command cache is out of reach: zsh and bash keep a path found before this stub
/// was laid until `rehash` / `hash -r`. Pure shell, no external command and no atpkg. It never
/// recurses: the twin execs its store path absolutely, and the walk skips every reroute
/// directory and stub. Line 2 is [`STUB_MARKER`], and it execs only through variables, so
/// [`crate::platform::parse_sh_shim_target`] sees no target.
#[must_use]
pub fn agents_stub_body_sh(name: &str, agents_dir: &Path) -> String {
    let mut s = String::from("#!/bin/sh\n");
    s.push_str(STUB_MARKER);
    s.push_str(
        "\n# Which copy of an agent program runs is decided HERE, when it runs (aterm help\n",
    );
    s.push_str(
        "# reroute): inside an aterm session the managed twin, anywhere else the user's own.\n",
    );
    s.push_str("# Not a managed tool: this file resolves to no store target and is never proof of an install.\n");
    s.push_str("__aterm_agents=");
    s.push_str(&crate::stub::sh_single_quote(&agents_dir.to_string_lossy()));
    s.push_str("\n__aterm_name=");
    s.push_str(&crate::stub::sh_single_quote(name));
    // The markers alone decide — never the --no-reroute marker (the doc above).
    s.push_str("\nif [ -n \"");
    s.push_str(&crate::hooks::agents_markers_sh());
    s.push_str("\" ] && [ -x \"$__aterm_agents/$__aterm_name\" ] && [ ! -d \"$__aterm_agents/$__aterm_name\" ]; then\n");
    s.push_str("  exec \"$__aterm_agents/$__aterm_name\" \"$@\"\n");
    s.push_str("fi\n");
    // The pass-through: [`stub_body_sh`]'s escape walk, plus the agents twin.
    s.push_str("__aterm_ifs=$IFS; IFS=:; set -f\n");
    s.push_str("for __d in $PATH; do\n");
    s.push_str("  [ -z \"$__d\" ] && continue\n");
    s.push_str("  [ \"${__d#/}\" = \"$__d\" ] && continue\n");
    s.push_str("  [ -f \"$__d/");
    s.push_str(DIR_MARKER_FILE);
    s.push_str("\" ] && continue\n");
    s.push_str("  [ \"$__d\" = \"$__aterm_agents\" ] && continue\n");
    s.push_str("  [ \"$__d\" -ef \"$__aterm_agents\" ] && continue\n");
    s.push_str("  if [ -x \"$__d/$__aterm_name\" ] && [ ! -d \"$__d/$__aterm_name\" ]; then\n");
    // A stub under another name (this one or another prefix's, linked or copied into an
    // ordinary PATH directory) would otherwise exec a stub again, forever.
    s.push_str(&sh_skip_a_stub("    ", "$__aterm_name"));
    s.push_str("    IFS=$__aterm_ifs; set +f\n");
    s.push_str("    exec \"$__d/$__aterm_name\" \"$@\"\n");
    s.push_str("  fi\n");
    s.push_str("done\n");
    s.push_str("IFS=$__aterm_ifs; set +f\n");
    // Nothing left to run. A twin standing here means this shell is outside aterm (inside,
    // the first arm exec'd it): `aterm <name>` reaches it. No twin: nothing is installed.
    s.push_str("if [ -x \"$__aterm_agents/$__aterm_name\" ] && [ ! -d \"$__aterm_agents/$__aterm_name\" ]; then\n");
    s.push_str("  printf '%s\\n' \"");
    s.push_str(&agents_not_found_message("$__aterm_name"));
    s.push_str("\" 1>&2\nelse\n");
    s.push_str("  printf '%s\\n' \"");
    s.push_str(&agents_not_installed_message("$__aterm_name"));
    s.push_str("\" 1>&2\nfi\nexit 127\n");
    s
}

/// The agents stub's line outside aterm when no copy is on PATH and the managed twin
/// stands. `name` is spliced into a double-quoted `sh` string (the stub passes
/// `$__aterm_name`), so the text holds no `"`, `$`, `` ` `` or `\` of its own.
#[must_use]
pub fn agents_not_found_message(name: &str) -> String {
    format!("aterm: no '{name}' on PATH — run: aterm {name}")
}

/// The agents stub's line when no copy is on PATH and no managed twin stands — the same
/// splicing rule as [`agents_not_found_message`].
#[must_use]
pub fn agents_not_installed_message(name: &str) -> String {
    format!("aterm: '{name}' is not installed — run: aterm pkg install {name}")
}

/// The agent programs that get a reroute stub: every [`crate::stub::AGENT_PROGRAMS`] name
/// whose `agents/` twin ([`Layout::agent_shim`], laid by
/// [`crate::activate::reconcile_agents`]) is an executable file now. A name with no twin gets
/// none (its stub would make `command -v claude` answer on a machine with no `claude`) and
/// loses the one it had on the next [`lay`].
#[must_use]
pub fn agents_routes(layout: &Layout) -> Vec<&'static str> {
    crate::stub::AGENT_PROGRAMS
        .iter()
        .copied()
        .filter(|name| {
            crate::store::ToolName::new(name).is_some_and(|tool| is_twin(&layout.agent_shim(&tool)))
        })
        .collect()
}

/// What an agents stub reads besides PATH — restated for the surfaces that say what
/// `claude` typed in a shell does (`aterm pkg doctor`, which runs as a child of that shell
/// and so sees what the stub would see). The markers, and nothing else: never
/// [`PASSTHROUGH_ENV`] ([`agents_stub_body_sh`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StubEnv {
    /// Any of [`crate::hooks::AGENTS_MARKERS`] set and non-empty — `-n` on the joined word.
    pub in_aterm: bool,
}

impl StubEnv {
    /// Read through `get` (a variable's raw value, `None` when unset).
    #[must_use]
    pub fn read(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> Self {
        Self {
            in_aterm: crate::hooks::AGENTS_MARKERS
                .iter()
                .any(|m| get(m).is_some_and(|v| !v.is_empty())),
        }
    }

    /// This process's environment.
    #[must_use]
    pub fn of_process() -> Self {
        Self::read(|name| std::env::var_os(name))
    }
}

/// Whether the agents stub for `name` execs the managed twin under `env` —
/// [`agents_stub_body_sh`]'s first arm: inside aterm, the twin executable.
#[must_use]
pub fn agents_stub_takes_twin(layout: &Layout, name: &str, env: StubEnv) -> bool {
    env.in_aterm
        && crate::store::ToolName::new(name).is_some_and(|tool| is_twin(&layout.agent_shim(&tool)))
}

/// Our agents stub for `name` ([`stub_path`]) when it is the first executable `name` on
/// `path_var`, the copy a shell with that PATH runs. Absolute entries only, as every walk
/// here; `None` when another copy comes first, or when nothing of ours stands there.
#[must_use]
pub fn agents_stub_first_on_path(
    layout: &Layout,
    name: &str,
    path_var: Option<&OsStr>,
) -> Option<PathBuf> {
    let ours = stub_path(layout, name);
    let ours_real = std::fs::canonicalize(&ours).ok();
    for dir in std::env::split_paths(path_var?) {
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            continue;
        }
        let candidate = dir.join(name);
        if !is_twin(&candidate) {
            continue;
        }
        let resolves_there =
            ours_real.is_some() && std::fs::canonicalize(&candidate).ok() == ours_real;
        let same = candidate == ours || resolves_there;
        return (same && is_reroute_stub(&candidate)).then_some(candidate);
    }
    None
}

/// `path_var` with every reroute directory ([`is_reroute_dir`], any spelling) taken out —
/// what a shell with that PATH resolves once the stubs are out of the way, for the
/// surfaces that name the copy an agents stub out-ranks.
#[must_use]
pub fn without_reroute_dirs(path_var: Option<&OsStr>) -> Option<std::ffi::OsString> {
    let kept: Vec<PathBuf> = std::env::split_paths(path_var?)
        .filter(|dir| dir.as_os_str().is_empty() || !is_reroute_dir(dir))
        .collect();
    std::env::join_paths(kept).ok()
}

/// `path_var` as an agents stub's pass-through walks it: without every reroute directory
/// ([`without_reroute_dirs`]) and without `layout`'s `agents/` — as spelled or as it
/// resolves ([`agents_stub_body_sh`]).
#[must_use]
pub fn pass_through_path(layout: &Layout, path_var: Option<&OsStr>) -> Option<std::ffi::OsString> {
    let stripped = without_reroute_dirs(path_var)?;
    let agents = layout.agents_dir();
    let agents_real = std::fs::canonicalize(&agents).ok();
    let kept: Vec<PathBuf> = std::env::split_paths(&stripped)
        .filter(|dir| {
            let resolves_there =
                agents_real.is_some() && std::fs::canonicalize(dir).ok() == agents_real;
            !(*dir == agents || resolves_there)
        })
        .collect();
    std::env::join_paths(kept).ok()
}

/// Whether `path` is an agents twin the stub's `-x … && ! -d …` would take: an executable
/// file (a link to one counts, as it does for `sh`).
fn is_twin(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let executable = true;
    meta.is_file() && executable
}

/// Whether `path` is one of our stubs: a regular file (never a symlink), small, UTF-8, whose
/// second line is [`STUB_MARKER`].
#[must_use]
pub fn is_reroute_stub(path: &Path) -> bool {
    // A symlink is never ours (stubs are laid as regular files), and the bounded
    // same-handle reader refuses a special or large file on the handle it reads, so a
    // swapped FIFO cannot block it.
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return false;
    }
    let Ok(content) = crate::metadata_io::read_bounded_regular_utf8(path, MAX_STUB_BYTES) else {
        return false;
    };
    content.lines().nth(1) == Some(STUB_MARKER)
}

/// Whether `dir` is a reroute directory — ours, under any spelling — by its
/// marker file. What both walks (the stub's `sh` and [`upstream_on_path`]) skip.
#[must_use]
pub fn is_reroute_dir(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(DIR_MARKER_FILE)).is_ok_and(|meta| meta.is_file())
}

/// What doctor reports per row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StubState {
    /// Our stub is in place.
    Laid,
    /// Nothing there (never laid, removed, or Windows).
    Missing,
    /// Something that is not ours occupies the name; never touched.
    Foreign,
}

/// The state of every row's stub, table order.
#[must_use]
pub fn states(layout: &Layout) -> Vec<(&'static str, StubState)> {
    TABLE
        .iter()
        .map(|row| (row.upstream, stub_state(layout, row.upstream)))
        .collect()
}

/// The state of the reroute stub named `name` — a [`TABLE`] row's or an agent program's.
#[must_use]
pub fn stub_state(layout: &Layout, name: &str) -> StubState {
    let path = stub_path(layout, name);
    match std::fs::symlink_metadata(&path) {
        Err(_) => StubState::Missing,
        Ok(_) if is_reroute_stub(&path) => StubState::Laid,
        Ok(_) => StubState::Foreign,
    }
}

/// Every stub [`lay`] wants in `dir(layout)` now, `(name, body)`: each [`TABLE`] row's, then
/// each agent program's with a twin ([`agents_routes`]). A row's stub names the atpkg
/// `embedder` settles on over the one the standing stub names
/// ([`crate::stub::Embedder::embed`]), and carries `strict` ([`stub_body_sh`]).
fn wanted_stubs(
    layout: &Layout,
    embedder: &crate::stub::Embedder,
    strict: bool,
) -> Vec<(&'static str, String)> {
    let dir = dir(layout);
    let agents = layout.agents_dir();
    TABLE
        .iter()
        .map(|row| {
            let path = dir.join(row.upstream);
            let standing = is_reroute_stub(&path)
                .then(|| crate::metadata_io::read_bounded_regular_utf8(&path, MAX_STUB_BYTES).ok())
                .flatten()
                .and_then(|body| crate::stub::sh_named_atpkg(&body, "ATPKG"));
            let atpkg = embedder.embed(standing.as_deref());
            let body = stub_body_sh(
                row.upstream,
                crate::stub::named(atpkg.as_deref()),
                &dir,
                strict,
            );
            (row.upstream, body)
        })
        .chain(
            agents_routes(layout)
                .into_iter()
                .map(|name| (name, agents_stub_body_sh(name, &agents))),
        )
        .collect()
}

/// Lay (or refresh) every row's stub — at seed, after each install pass, at every session
/// spawn, and on `repair`, so the embedded atpkg path survives relocation and self-update.
/// Never over a foreign file; a stub whose row left the table is swept (marker-gated).
/// Windows lays nothing.
///
/// The atpkg a stub names is [`crate::stub::Embedder`]'s choice, so a copy of aterm started
/// from a scratch directory never names a `<scratch>/bin/atpkg` that was never there.
/// `[reroute] strict` is read fresh ([`crate::config::load_reroute`]): a row's stub carries
/// it in its bytes, and the long-lived window lays at every session spawn, so a value cached
/// at launch would re-lay an old policy over a newer one.
pub fn lay(layout: &Layout) -> io::Result<()> {
    lay_with(
        layout,
        &crate::stub::Embedder::this_process(),
        crate::config::load_reroute().strict(),
    )
}

/// [`lay`], with the atpkg on offer and the temporary directories `embedder`'s — a test
/// stands the laying process anywhere — and `[reroute] strict` given.
pub(crate) fn lay_with(
    layout: &Layout,
    embedder: &crate::stub::Embedder,
    strict: bool,
) -> io::Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    // A recorded decline (`aterm pkg uninstall --all`) is durable: without this gate the
    // next spawn would put the stubs back first on PATH right after the uninstall.
    if layout.declined().is_file() {
        return Ok(());
    }
    let dir = dir(layout);
    layout.ensure_dir(&dir)?;
    // The marker first: a walk racing this `lay` recognizes the directory before the first
    // stub lands in it.
    let marker = dir.join(DIR_MARKER_FILE);
    if marker_needs_lay(std::fs::read(&marker).ok().as_deref()) {
        crate::lay::write_regenerable(&crate::lay::Executable::new(&marker, marker_body()))?;
    }
    // Every stub whose bytes differ.
    let mut files = Vec::new();
    let wanted = wanted_stubs(layout, embedder, strict);
    for (name, body) in &wanted {
        let path = dir.join(name);
        match std::fs::symlink_metadata(&path) {
            Err(_) => {} // absent: ours to claim
            // Ours already, and byte-identical: nothing to lay. A body that differs — the
            // atpkg it names moved with a relocation or a self-update — is rewritten.
            Ok(_) if is_reroute_stub(&path) => {
                if std::fs::read(&path).is_ok_and(|have| have == body.as_bytes()) {
                    continue;
                }
            }
            Ok(_) => continue, // foreign: never touched
        }
        files.push(crate::lay::Executable::new(&path, body.as_bytes()));
    }
    // Regenerable: every seed, pass and session spawn lays again what is missing or differs.
    files.iter().try_for_each(crate::lay::write_regenerable)?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(OsStr::to_str) else {
                continue;
            };
            // Dotfiles are never swept: the marker file, and a concurrent `lay`'s
            // `.<name>.stub-<pid>` temp between its write and rename.
            if name.starts_with('.') {
                continue;
            }
            // Ours and no longer wanted: a row that left the table, or an agent program
            // whose twin is gone (uninstalled, tombstoned) — its stub would only pass through.
            if !wanted.iter().any(|(w, _)| *w == name) && is_reroute_stub(&path) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    // abrew's `brew` stub (docs/DESIGN-abrew-2026-10-19.md §6) lives in this
    // directory under its own marker, laid after the sweep above so no lay
    // ever removes it; a failure is abrew's to report (`abrew doctor`), never
    // this lay's — the toolchain stubs just landed either way.
    let _ = crate::abrew::stub::lay_with(layout, embedder);
    Ok(())
}

/// What [`lay_with`] would write or sweep now, by name — the directory's marker, each stub
/// absent or ours with other bytes, each ours no row wants (`<name> (swept)`), and abrew's
/// `brew` — changing nothing: what `aterm pkg repair`'s `base.pkg.stubs` item reads. A
/// foreign file is never named, since no lay touches it.
pub(crate) fn lay_pending(
    layout: &Layout,
    embedder: &crate::stub::Embedder,
    strict: bool,
) -> Vec<String> {
    if cfg!(windows) || layout.declined().is_file() {
        return Vec::new();
    }
    let dir = dir(layout);
    let mut pending = Vec::new();
    if marker_needs_lay(std::fs::read(dir.join(DIR_MARKER_FILE)).ok().as_deref()) {
        pending.push(DIR_MARKER_FILE.to_owned());
    }
    let wanted = wanted_stubs(layout, embedder, strict);
    for (name, body) in &wanted {
        let path = dir.join(name);
        let lays = match std::fs::symlink_metadata(&path) {
            Err(_) => true,
            Ok(_) if is_reroute_stub(&path) => {
                !std::fs::read(&path).is_ok_and(|have| have == body.as_bytes())
            }
            Ok(_) => false,
        };
        if lays {
            pending.push((*name).to_owned());
        }
    }
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(OsStr::to_str) else {
                continue;
            };
            if !name.starts_with('.')
                && !wanted.iter().any(|(w, _)| *w == name)
                && is_reroute_stub(&path)
            {
                pending.push(format!("{name} (swept)"));
            }
        }
    }
    if crate::abrew::stub::lay_pending(layout, embedder) {
        pending.push("brew".to_owned());
    }
    pending
}

/// The marker file's bytes.
fn marker_body() -> String {
    let mut body = String::from(STUB_MARKER);
    body.push('\n');
    body
}

/// Whether [`lay`] writes the marker file, given what stands there now (`None`: absent or
/// unreadable): only when it is absent or its bytes differ.
pub(crate) fn marker_needs_lay(have: Option<&[u8]>) -> bool {
    have != Some(marker_body().as_bytes())
}

/// Remove every stub that is ours; the directory goes too once it is empty.
/// `uninstall --all` and a recorded decline.
pub fn remove_all(layout: &Layout) {
    let dir = dir(layout);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_reroute_stub(&path) {
            let _ = std::fs::remove_file(&path);
        }
    }
    let _ = std::fs::remove_file(dir.join(DIR_MARKER_FILE));
    let _ = std::fs::remove_dir(&dir);
}

// ── the hidden verb ─────────────────────────────────────────────────────────

/// `atpkg __reroute <upstream> [args…]`: what a stub execs once its `sh`-side escape has not
/// fired. Applies the row's policy; the exit code is the exec'd tool's, 127 when nothing
/// exists to run, and [`REFUSAL_EXIT`] for a strict refusal, a compiler that is not Trust
/// inside a Trust source checkout, or a name that is not a row.
///
/// No row refuses an agent: the stub cannot tell an agent's own `cargo` from a by-name spawn
/// inside a tool it ran (a build script, a Makefile, a project's stock-toolchain lane), so a
/// refusal would turn those tools red. An agent's own command is judged by its text,
/// `aterm pkg lane` ([`crate::lane`]), which a guard the owner wires (a `PreToolUse` hook)
/// can call; aterm installs nothing into an agent.
pub fn run(layout: &Layout, upstream: &str, args: &[String]) -> ExitCode {
    let Some(row) = row_for(upstream) else {
        eprintln!("atpkg {HIDDEN_VERB}: '{upstream}' is not a rerouted name");
        return ExitCode::from(REFUSAL_EXIT);
    };
    let path_var = std::env::var_os("PATH");
    let cwd = std::env::current_dir().ok();
    let env_toolchain = std::env::var("RUSTUP_TOOLCHAIN").ok();
    let rustup_home = crate::seam::rustup_home_with(
        std::env::var_os("RUSTUP_HOME").as_deref(),
        aterm_types::dirs::home_dir().as_deref(),
    );
    let identity_cache = layout.prefix.join(crate::upstream_identity::CACHE_FILE);
    // Read once, and only where a line would be said: one `stat` walk and, when the
    // branded tool does not run here, one `status.toml` read.
    let unserved = std::cell::OnceCell::new();
    let machine = Machine {
        // The stub decides the escape first, but `aterm <upstream>` and a direct
        // `atpkg __reroute` arrive here without it. A stock cargo's compile
        // ([`CARGO_CRATE_ENV`]) is the stub's to decide; a Trust cargo's
        // ([`TARGO_CRATE_ENV`]) is answered as a typed name.
        passthrough: engaged(std::env::var(PASSTHROUGH_ENV).ok().as_deref()),
        // A child of a command the reroute ran: said once, judged still.
        announced: engaged(std::env::var(ANNOUNCED_ENV).ok().as_deref()),
        // ANNOUNCE, THEN RUN — the 2026-09-08 ruling. `[reroute] announce = false`
        // (the owner's "suppressed with a flag", a setting since 2026-09-23) drops
        // the line but never the run. A row whose tool has no build for this target
        // ([`unserved_here`]) has nothing to announce: its line would name commands that
        // cannot run here, so upstream runs as typed, unannounced — and is still judged
        // inside a Trust source checkout, which is a refusal, not an announcement.
        announce: &|| {
            crate::config::cached_reroute().announce()
                && unserved
                    .get_or_init(|| unserved_here(layout, branded_of(row), program_of(row)))
                    .is_none()
        },
        // WARN, DO NOT STOP: `[reroute] strict = true` puts the refusals back for whoever
        // wants them.
        strict: &|| crate::config::cached_reroute().strict(),
        upstream: &|| upstream_on_path(layout, upstream, path_var.as_deref()),
        branded: &|branded, program| installed(layout, branded, program),
        // Read only where a signpost names it: a few `stat`s walking up.
        pin: &|| Pin::for_dir(cwd.as_deref(), env_toolchain.as_deref()),
        // Read only for `cargo`/`rustc`, and only where a line or the checkout needs it.
        resolve: &|target, toolchain| {
            crate::upstream_identity::resolve(&crate::upstream_identity::Ask {
                name: upstream,
                target,
                toolchain,
                cwd: cwd.as_deref(),
                env_toolchain: env_toolchain.as_deref(),
                rustup_home: rustup_home.as_deref(),
                cache: Some(&identity_cache),
            })
        },
        trust_checkout: &|| {
            cwd.as_deref()
                .and_then(crate::upstream_identity::trust_checkout)
        },
    };
    match plan(row, args, &machine) {
        Plan::Refuse { say } => {
            eprintln!("{say}");
            ExitCode::from(REFUSAL_EXIT)
        }
        Plan::Missing { say } => {
            eprintln!("{say}");
            ExitCode::from(127)
        }
        Plan::Upstream { say, target, args } => {
            if let Some(say) = say {
                eprintln!("{say}");
            }
            let args = if upstream == "cargo" && !machine.passthrough {
                upstream_cargo_args(
                    &target,
                    args,
                    &machine,
                    &crate::build_cache::with_build_dir_here,
                )
            } else {
                args
            };
            exec_upstream(&target, &args)
        }
        Plan::Quiet { copy, args } => exec_branded(layout, &copy, &args),
        Plan::Branded { say, copy, args } => {
            eprintln!("{say}");
            let args = if upstream == "cargo" {
                crate::build_cache::with_build_dir_here(&args, crate::build_cache::Tool::Targo)
                    .unwrap_or(args)
            } else {
                args
            };
            exec_branded(layout, &copy, &args)
        }
        Plan::Shim { shim, args } => {
            // The shim exports its own environment; its line is the one said.
            let copy = Managed {
                target: shim,
                env: crate::shim_env::ShimEnv::NONE,
            };
            exec_branded(layout, &copy, &args)
        }
    }
}

/// [`crate::build_cache::with_build_dir_here`]'s shape, injected into
/// [`upstream_cargo_args`].
type Decide<'a> = dyn Fn(&[String], crate::build_cache::Tool) -> Option<Vec<String>> + 'a;

/// An upstream `cargo`'s `args`, with the build cache's `--config` ([`crate::build_cache`])
/// when `decide` adds it and the cargo that runs reads `build.build-dir` (its `-vV`
/// release, resolved and cached as a signpost resolves it). Trust's own `cargo` is
/// `targo`, so it follows `targo`'s rule, but its `trust` lane is handed nothing here
/// (that folder goes through the environment, which only the shim sets). Anything
/// unknown keeps the line as typed.
fn upstream_cargo_args(
    target: &Path,
    args: Vec<String>,
    machine: &Machine<'_>,
    decide: &Decide<'_>,
) -> Vec<String> {
    use crate::build_cache::Tool;
    if decide(&args, Tool::Cargo).is_none() {
        return args;
    }
    match (machine.resolve)(target, explicit_toolchain(&args)).identity {
        crate::upstream_identity::Identity::Known(vv)
            if crate::build_cache::release_reads_build_dir(&vv.release) =>
        {
            let tool = if vv.trust.is_some() {
                Tool::Targo
            } else {
                Tool::Cargo
            };
            decide(&args, tool).unwrap_or(args)
        }
        _ => args,
    }
}

/// What [`plan`] reads from the machine, injected so every arm is decided — and tested —
/// without a spawn. Each probe is read only by the arm that needs it.
struct Machine<'a> {
    /// The [`PASSTHROUGH_ENV`] marker is engaged.
    passthrough: bool,
    /// The [`ANNOUNCED_ENV`] marker is engaged: a child of a command the reroute ran.
    announced: bool,
    /// `[reroute] announce` ([`crate::config::RerouteConfig`]).
    announce: &'a dyn Fn() -> bool,
    /// `[reroute] strict` ([`crate::config::RerouteConfig::strict`]).
    strict: &'a dyn Fn() -> bool,
    /// The first upstream copy on PATH ([`upstream_on_path`]).
    upstream: &'a dyn Fn() -> Option<PathBuf>,
    /// Where a branded tool, shipped by the program named second, stands ([`installed`]).
    branded: &'a dyn Fn(&str, &str) -> Install,
    /// The directory's pinned channel ([`Pin::for_dir`]), named under a signpost's lanes.
    pin: &'a dyn Fn() -> Option<Pin>,
    /// What the upstream copy at the path given runs here, under the caller's
    /// `+<toolchain>` ([`crate::upstream_identity::resolve`]) — asked of `cargo`/`rustc`.
    resolve: &'a dyn Fn(&Path, Option<&str>) -> Resolved,
    /// The root of the Trust source checkout the command runs in
    /// ([`crate::upstream_identity::trust_checkout`]).
    trust_checkout: &'a dyn Fn() -> Option<PathBuf>,
}

/// Whether `row`'s upstream copy is resolved and named by what it is, and refused inside a
/// Trust source checkout when not Trust: `cargo` and `rustc`, the SIGNPOST Rust rows.
fn names_what_runs(row: &Row) -> bool {
    matches!(row.policy, Policy::Signpost { .. }) && is_rust_row(row)
}

/// Where a branded tool stands on this machine — the cases `atpkg run` tells apart.
#[derive(Debug, PartialEq, Eq)]
enum Install {
    /// Its shim forwards to a build: the managed copy.
    Runs(Managed),
    /// `bin/<branded>` stands but forwards to no build — a yanked build's tombstone. It
    /// runs, and its own line says what is true of it.
    Shim(PathBuf),
    /// `bin/<branded>` is a PENDING STUB ([`crate::stub`]): its program is wanted and not
    /// installed yet. Run, it says the install's state and waits for the program while a
    /// pass installs it — so it is what runs only where nothing else can
    /// ([`fallback_pending_message`]).
    Pending(PathBuf),
    /// Its program's live build holds it, but its shim is gone
    /// ([`crate::cli::live_without_shim_of`]).
    ShimLost,
    /// Nothing here holds it.
    Absent,
}

/// Why `branded`, one of the tools `program` ships, cannot be had on this machine: the
/// row the last pass recorded for `program` when it says the index publishes no build for
/// this target (`unavailable on <target>: …`, [`crate::state::UNAVAILABLE_PREFIX`]) and
/// nothing here runs `branded` — no store build, no dev link, at most a pending stub that
/// answers with that same row. `None` wherever the tool runs or can still arrive (no row
/// yet, a held or failed row, a served target).
///
/// Keyed on the recorded state, never on the platform: on aarch64 Linux (index build 71
/// publishes no Trust build for any Linux triple) every `cargo` in a tab printed a six-line
/// signpost naming `targo trust build`, a command whose pending stub answers "trust is not
/// coming on its own" and whose install answers "unavailable". Where Trust is served (an
/// Apple-silicon Mac) the row never reads `unavailable on`, so nothing changes there.
#[must_use]
pub fn unserved_here(layout: &Layout, branded: &str, program: &str) -> Option<String> {
    if !matches!(
        installed(layout, branded, program),
        Install::Pending(_) | Install::Absent
    ) {
        return None;
    }
    let state = crate::status::read(layout)?.programs.remove(program)?.state;
    state
        .starts_with(crate::state::UNAVAILABLE_PREFIX)
        .then_some(state)
}

/// A branded tool's managed copy, as its shim would exec it: through the store, never
/// PATH, at the exec root the shim's guard would take (`crate::compat` — a trust build whose
/// tippy refuses the store's own `bin/rustc` copy lints only from there), with what the
/// shim would export (design S7) — `atpkg run`'s rule, so a branded program whose policy
/// declares an environment gets it by either door.
use crate::ops::{Managed, managed};

/// Where `branded`, one of the tools `program` ships, stands on this machine ([`Install`]).
fn installed(layout: &Layout, branded: &str, program: &str) -> Install {
    if let Some(copy) = managed(layout, branded) {
        return Install::Runs(copy);
    }
    let Some(name) = crate::store::ToolName::new(branded) else {
        return Install::Absent;
    };
    let shim = layout.shim(&name);
    if crate::stub::is_pending_stub(&shim) {
        return Install::Pending(shim);
    }
    if crate::cli::is_tombstone_shim(layout, branded) {
        return Install::Shim(shim);
    }
    if crate::cli::live_without_shim_of(layout, program, branded).is_some() {
        return Install::ShimLost;
    }
    Install::Absent
}

/// One `__reroute` call, decided before a word is printed, so no line announces a tool
/// that is not there to run: what to say on stderr, then what to exec, or why nothing.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    /// Say `say` (when `Some`), then exec the upstream copy at `target`.
    Upstream {
        say: Option<String>,
        target: PathBuf,
        args: Vec<String>,
    },
    /// Say `say`, then exec the managed `copy`.
    Branded {
        say: String,
        copy: Managed,
        args: Vec<String>,
    },
    /// Exec the managed `copy`, saying nothing: a [`LANE_FREE_QUERIES`] call, whose
    /// caller parses what it prints.
    Quiet { copy: Managed, args: Vec<String> },
    /// Say `say` and exit [`REFUSAL_EXIT`]: the ORACLE row under `[reroute] strict`, and a
    /// `cargo`/`rustc` that is not Trust inside a Trust source checkout
    /// ([`trust_checkout_refusal`]).
    Refuse { say: String },
    /// Exec a branded tool's `shim` that forwards to no build ([`Install::Shim`],
    /// [`Install::Pending`]), saying nothing: the shim's own line speaks.
    Shim { shim: PathBuf, args: Vec<String> },
    /// Say `say` and exit 127: the tool to run is not there.
    Missing { say: String },
}

/// The row's policy applied to `args` on `machine` — [`run`] without its effects.
fn plan(row: &Row, args: &[String], machine: &Machine<'_>) -> Plan {
    let upstream = row.upstream;
    // Upstream as typed, `say` printed first — or, with no copy on PATH, the stub's line.
    let run_upstream = |say: &dyn Fn() -> Option<String>| match (machine.upstream)() {
        Some(target) => Plan::Upstream {
            say: say(),
            target,
            args: args.to_vec(),
        },
        None => Plan::Missing {
            say: not_on_path_message(upstream),
        },
    };
    // The managed branded tool where it stands (`install`) — or, where it cannot run, why
    // ([`Unrunnable`]).
    let run_branded = |install: Install,
                       args: Vec<String>,
                       say: &dyn Fn() -> String|
     -> Result<Plan, Unrunnable> {
        match install {
            Install::Runs(copy) => Ok(Plan::Branded {
                say: say(),
                copy,
                args,
            }),
            Install::Shim(shim) | Install::Pending(shim) => Ok(Plan::Shim { shim, args }),
            Install::ShimLost => Err(Unrunnable::ShimLost),
            Install::Absent => Err(Unrunnable::Absent),
        }
    };
    if machine.passthrough {
        return run_upstream(&|| None);
    }
    // `+<toolchain>` is rustup's (first argument only), so it means something on the Rust
    // rows alone: `lean +x` hands `+x` to clean untouched.
    let toolchain = if is_rust_row(row) {
        explicit_toolchain(args)
    } else {
        None
    };
    // What the rendered commands and a branded tool see: never a rustup directive. The
    // managed `targo` is not a rustup proxy and rejects one, and a fix the user cannot
    // paste is no fix. Upstream still gets the caller's own arguments, directive included.
    let shown = if toolchain.is_some() {
        &args[1..]
    } else {
        args
    };
    // A child of a command the reroute ran ([`ANNOUNCED_ENV`]): upstream as typed, saying
    // nothing, but a `cargo`/`rustc` is judged in a Trust source checkout as if typed.
    if machine.announced {
        if !names_what_runs(row) {
            return run_upstream(&|| None);
        }
        return match (machine.upstream)() {
            Some(target) => match judge(row, shown, toolchain, &target, false, machine) {
                Err(refused) => refused,
                Ok(_) => Plan::Upstream {
                    say: None,
                    target,
                    args: args.to_vec(),
                },
            },
            None => Plan::Missing {
                say: not_on_path_message(upstream),
            },
        };
    }
    // A stock toolchain named on any Rust row (`cargo +1.97.1 test`, `rustfmt +1.97.1
    // x.rs`) runs upstream as named, after the Trust spelling of the same command; a
    // DIRECT row never hands the directive to its branded tool.
    if let Some(stock) = toolchain.filter(|tc| !rust_lane::is_trust_channel(tc)) {
        if names_what_runs(row) {
            return match (machine.upstream)() {
                Some(target) => {
                    signpost_upstream(row, args, shown, Head::Stock(stock), target, None, machine)
                }
                None => Plan::Missing {
                    say: not_on_path_message(upstream),
                },
            };
        }
        return run_upstream(&|| {
            (machine.announce)().then(|| stock_channel_signpost(row, stock, shown, None))
        });
    }
    match row.policy {
        // `+trust…` on a DIRECT row names Trust: its branded tool runs, without the
        // directive. With it missing, upstream runs after one line naming the fix, unless
        // strict or there is no upstream copy.
        Policy::Direct {
            branded,
            program,
            source_verb,
        } => {
            let install = (machine.branded)(branded, program);
            // A PENDING STUB HOLDS NO CALL HOSTAGE: the branded tool's program
            // is still to come, so the upstream copy the caller named runs NOW, after one
            // line saying so — never a wait of a minute or more for `trust` behind an
            // editor's `rustfmt`. The stub runs (and waits) only where nothing else can:
            // no upstream copy, or `[reroute] strict`.
            if matches!(install, Install::Pending(_))
                && !(machine.strict)()
                && let Some(target) = (machine.upstream)()
            {
                return Plan::Upstream {
                    say: (machine.announce)()
                        .then(|| fallback_pending_message(upstream, branded, program)),
                    target,
                    args: args.to_vec(),
                };
            }
            run_branded(install, direct_args(source_verb, shown), &|| {
                direct_announcement(upstream, branded, (machine.upstream)().is_some())
            })
            .unwrap_or_else(|why| {
                let refusal = || Plan::Missing {
                    say: why.refusal(upstream, branded, program),
                };
                if (machine.strict)() {
                    return refusal();
                }
                match (machine.upstream)() {
                    Some(target) => Plan::Upstream {
                        say: (machine.announce)().then(|| why.fallback(upstream, branded, program)),
                        target,
                        args: args.to_vec(),
                    },
                    None => refusal(),
                }
            })
        }
        Policy::Signpost { branded, lanes, .. } => {
            // A lane-free query runs targo itself, quietly, wherever targo runs.
            if upstream == "cargo"
                && shown
                    .first()
                    .is_some_and(|verb| LANE_FREE_QUERIES.contains(&verb.as_str()))
                && let Install::Runs(copy) = (machine.branded)(branded, EXPLICIT_LANE_PROGRAM)
            {
                return Plan::Quiet {
                    copy,
                    args: shown.to_vec(),
                };
            }
            // `+trust…` keeps the lane question, and the first line names that toolchain
            // as what runs.
            if upstream == "cargo"
                && shown
                    .first()
                    .is_some_and(|verb| EXPLICIT_LANE_VERBS.contains(&verb.as_str()))
            {
                // Upstream cargo has no `trust` subcommand and no `--unverified` flag, so
                // with targo missing there is nothing to fall back to.
                // A pending `targo` is the stub to run: it waits for `trust` and then runs.
                return run_branded(
                    (machine.branded)(branded, EXPLICIT_LANE_PROGRAM),
                    shown.to_vec(),
                    &|| explicit_lane_note(upstream, branded),
                )
                .unwrap_or_else(|why| Plan::Missing {
                    say: why.refusal(upstream, branded, EXPLICIT_LANE_PROGRAM),
                });
            }
            match (machine.upstream)() {
                // A `+trust…` directive, not the directory's pin, decides what upstream is.
                Some(target) => signpost_upstream(
                    row,
                    args,
                    shown,
                    toolchain.map_or(Head::Upstream, Head::Toolchain),
                    target,
                    toolchain.is_none().then_some(machine.pin),
                    machine,
                ),
                None => Plan::Missing {
                    say: no_upstream_message(row, lanes, shown),
                },
            }
        }
        // Announce, then run upstream; strict refuses, naming the path.
        Policy::Oracle { branded, .. } => {
            if (machine.strict)() {
                return Plan::Refuse {
                    say: oracle_message(upstream, (machine.upstream)().as_deref()),
                };
            }
            // The solver's program is its own name (`ay`).
            let fix = || oracle_fix(&(machine.branded)(branded, branded), branded);
            match (machine.upstream)() {
                Some(target) => Plan::Upstream {
                    say: (machine.announce)()
                        .then(|| oracle_notice(upstream, branded, fix().as_deref())),
                    target,
                    args: args.to_vec(),
                },
                None => Plan::Missing {
                    say: oracle_not_on_path_message(upstream, branded, fix().as_deref()),
                },
            }
        }
    }
}

/// A SIGNPOST row's upstream copy at `target` about to run as typed (`args`; `shown` is what
/// the lanes spell, `head` the first line, `pin` the directory's pin). A `cargo`/`rustc` is
/// judged first ([`judge`]), whatever `[reroute] announce` says.
fn signpost_upstream(
    row: &Row,
    args: &[String],
    shown: &[String],
    head: Head<'_>,
    target: PathBuf,
    pin: Option<&dyn Fn() -> Option<Pin>>,
    machine: &Machine<'_>,
) -> Plan {
    let toolchain = match head {
        Head::Toolchain(tc) | Head::Stock(tc) => Some(tc),
        Head::Upstream | Head::NotOnPath => None,
    };
    let announce = (machine.announce)();
    let resolved = match judge(row, shown, toolchain, &target, announce, machine) {
        Ok(resolved) => resolved,
        Err(refused) => return refused,
    };
    Plan::Upstream {
        say: announce.then(|| {
            let pin = pin.and_then(|pin| pin());
            signpost_block(
                row,
                lanes_of(row),
                shown,
                head,
                pin.as_ref(),
                resolved.as_ref(),
                None,
            )
        }),
        target,
        args: args.to_vec(),
    }
}

/// The one judgement of an upstream copy about to run as `row`, for every door that runs
/// one: `Err` with the refusal for a `cargo`/`rustc` inside a Trust source checkout that is
/// not Trust ([`trust_checkout_refusal`]), else `Ok` with what it resolved to. Resolved only
/// when `named` (a line will name it) or inside a checkout, so a silent run elsewhere costs
/// nothing.
fn judge(
    row: &Row,
    shown: &[String],
    toolchain: Option<&str>,
    target: &Path,
    named: bool,
    machine: &Machine<'_>,
) -> Result<Option<Resolved>, Plan> {
    if !names_what_runs(row) {
        return Ok(None);
    }
    let checkout = (machine.trust_checkout)();
    if !named && checkout.is_none() {
        return Ok(None);
    }
    let resolved = (machine.resolve)(target, toolchain);
    match checkout {
        Some(root) if !resolved.is_trust() => Err(Plan::Refuse {
            say: trust_checkout_refusal(row, shown, toolchain, &resolved, &root),
        }),
        _ => Ok(Some(resolved)),
    }
}

/// The first upstream copy of `name` on PATH: an executable regular file in an absolute entry
/// that is neither a reroute directory (by marker, any spelling, any prefix) nor under this
/// manager prefix, and that is not a reroute stub or a link to one. The marker, not the
/// prefix, keeps a nested session's escape off an enclosing session's stub.
#[must_use]
pub fn upstream_on_path(layout: &Layout, name: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    upstream_on_path_in(Some(&layout.prefix), name, path_var)
}

/// [`upstream_on_path`] with the manager prefix optional: `None` when no store resolves
/// ([`run_storeless`]), where every reroute directory and stub is still skipped.
fn upstream_on_path_in(
    prefix: Option<&Path>,
    name: &str,
    path_var: Option<&OsStr>,
) -> Option<PathBuf> {
    let path_var = path_var?;
    let prefix_real =
        prefix.map(|prefix| std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf()));
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() || !dir.is_absolute() || is_reroute_dir(&dir) {
            continue;
        }
        let under_prefix = prefix
            .zip(prefix_real.as_deref())
            .is_some_and(|(prefix, real)| {
                dir.starts_with(prefix)
                    || std::fs::canonicalize(&dir).is_ok_and(|dir_real| dir_real.starts_with(real))
            });
        if under_prefix {
            continue;
        }
        let candidate = dir.join(name);
        let Ok(meta) = std::fs::metadata(&candidate) else {
            continue;
        };
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt as _;
            meta.is_file() && meta.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = meta.is_file();
        let reaches_a_stub = is_reroute_stub(&candidate)
            || std::fs::canonicalize(&candidate).is_ok_and(|real| is_reroute_stub(&real));
        if !executable || reaches_a_stub {
            continue;
        }
        return Some(candidate);
    }
    None
}

/// `atpkg __reroute` when no store resolves (no `HOME`, an `env -i` wrapper): nothing
/// managed can be looked up, so the upstream copy on PATH runs after one line (127 when there
/// is none), or nothing runs under `[reroute] strict = true` ([`REFUSAL_EXIT`]). A
/// `cargo`/`rustc` is still resolved, uncached, and judged ([`judge`]).
pub fn run_storeless(upstream: &str, args: &[String]) -> ExitCode {
    let Some(row) = row_for(upstream) else {
        eprintln!("atpkg {HIDDEN_VERB}: '{upstream}' is not a rerouted name");
        return ExitCode::from(REFUSAL_EXIT);
    };
    let path_var = std::env::var_os("PATH");
    let cwd = std::env::current_dir().ok();
    let env_toolchain = std::env::var("RUSTUP_TOOLCHAIN").ok();
    let rustup_home = crate::seam::rustup_home_with(
        std::env::var_os("RUSTUP_HOME").as_deref(),
        aterm_types::dirs::home_dir().as_deref(),
    );
    let reroute = crate::config::cached_reroute();
    let machine = Machine {
        passthrough: engaged(std::env::var(PASSTHROUGH_ENV).ok().as_deref()),
        announced: engaged(std::env::var(ANNOUNCED_ENV).ok().as_deref()),
        announce: &|| reroute.announce(),
        strict: &|| reroute.strict(),
        upstream: &|| upstream_on_path_in(None, upstream, path_var.as_deref()),
        // Nothing managed is looked up without a store.
        branded: &|_, _| Install::Absent,
        pin: &|| None,
        resolve: &|target, toolchain| {
            crate::upstream_identity::resolve(&crate::upstream_identity::Ask {
                name: upstream,
                target,
                toolchain,
                cwd: cwd.as_deref(),
                env_toolchain: env_toolchain.as_deref(),
                rustup_home: rustup_home.as_deref(),
                // No prefix to keep it under: every probe runs.
                cache: None,
            })
        },
        trust_checkout: &|| {
            cwd.as_deref()
                .and_then(crate::upstream_identity::trust_checkout)
        },
    };
    match storeless_plan(row, args, &machine) {
        Plan::Upstream { say, target, args } => {
            if let Some(say) = say {
                eprintln!("{say}");
            }
            exec_upstream(&target, &args)
        }
        Plan::Refuse { say } => {
            eprintln!("{say}");
            ExitCode::from(REFUSAL_EXIT)
        }
        Plan::Missing { say } => {
            eprintln!("{say}");
            ExitCode::from(127)
        }
        Plan::Branded { .. } | Plan::Quiet { .. } | Plan::Shim { .. } => {
            unreachable!("no store: nothing branded is planned")
        }
    }
}

/// [`run_storeless`] without its effects: the escape and a child of a command the reroute
/// ran as in [`plan`], strict's refusal, then the upstream copy — judged, and named on its
/// line when it is a `cargo`/`rustc`.
fn storeless_plan(row: &Row, args: &[String], machine: &Machine<'_>) -> Plan {
    let upstream = row.upstream;
    if !machine.passthrough && (machine.strict)() {
        return Plan::Refuse {
            say: storeless_refusal(upstream),
        };
    }
    let Some(target) = (machine.upstream)() else {
        return Plan::Missing {
            say: not_on_path_message(upstream),
        };
    };
    if machine.passthrough {
        return Plan::Upstream {
            say: None,
            target,
            args: args.to_vec(),
        };
    }
    let toolchain = is_rust_row(row).then(|| explicit_toolchain(args)).flatten();
    let shown = if toolchain.is_some() {
        &args[1..]
    } else {
        args
    };
    let announce = !machine.announced && (machine.announce)();
    match judge(row, shown, toolchain, &target, announce, machine) {
        Err(refused) => refused,
        Ok(resolved) => Plan::Upstream {
            say: announce.then(|| storeless_message(upstream, args, resolved.as_ref())),
            target,
            args: args.to_vec(),
        },
    }
}

/// Exec the upstream copy `target` with [`ANNOUNCED_ENV`] set, so its by-name spawns are
/// judged but not announced again ([`plan`]). Never with [`PASSTHROUGH_ENV`]: that is the
/// escape, and carried by a Trust cargo's tree it lets a stock `rustc +<channel>` beneath it
/// run unjudged inside a Trust checkout.
fn exec_upstream(target: &Path, args: &[String]) -> ExitCode {
    // Raw: transparent, so the caller's descriptors are the program's. Its Windows
    // console is decided here.
    #[expect(
        clippy::disallowed_types,
        reason = "a pass-through by exec: its caller's descriptors are the program's"
    )]
    let mut command = std::process::Command::new(target);
    aterm_uds::spawn::set_flags(&mut command, 0);
    command.args(args).env(ANNOUNCED_ENV, "1");
    let err = crate::platform::exec_or_run(&mut command);
    eprintln!("aterm: failed to exec {}: {err}", target.display());
    ExitCode::from(127)
}

/// The arguments a DIRECT row hands its branded tool: identity, except that a verb-first
/// row ([`SourceVerb`]) gets its verb inserted when the first argument is a source file of
/// its extension (`lean f.lean` → `clean check f.lean`; `lean --version`, `lean repl` and
/// `lean check f.lean` pass untouched).
#[must_use]
fn direct_args(source_verb: Option<SourceVerb>, args: &[String]) -> Vec<String> {
    match source_verb {
        Some(SourceVerb { verb, ext })
            if args.first().is_some_and(|first| first.ends_with(ext)) =>
        {
            let mut out = Vec::with_capacity(args.len() + 1);
            out.push(verb.to_string());
            out.extend_from_slice(args);
            out
        }
        _ => args.to_vec(),
    }
}

/// A branded tool's managed `copy` ([`crate::ops::managed`]), exec'd with the managed `bin/` appended
/// for its children and its shim's environment exported — `atpkg run`'s discipline.
fn exec_branded(layout: &Layout, copy: &Managed, args: &[String]) -> ExitCode {
    let child_path =
        crate::store::append_bin_to_path(std::env::var_os("PATH").as_deref(), &layout.bin_dir());
    // Raw and console-decided, as `exec_upstream`.
    #[expect(
        clippy::disallowed_types,
        reason = "a pass-through by exec: its caller's descriptors are the program's"
    )]
    let mut command = std::process::Command::new(&copy.target);
    aterm_uds::spawn::set_flags(&mut command, 0);
    command.args(args).env("PATH", child_path);
    for (name, value) in copy.env.entries() {
        command.env(name, value);
    }
    let err = crate::platform::exec_or_run(&mut command);
    eprintln!("aterm: failed to exec {}: {err}", copy.target.display());
    ExitCode::from(127)
}

#[cfg(test)]
#[path = "reroute_tests.rs"]
mod tests;
