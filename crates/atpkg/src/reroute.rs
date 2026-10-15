// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Rerouting: the upstream Rust names inside an aterm session.
//!
//! Design of record: `docs/DESIGN-toolchain-philosophy-2026-08-29.md` §4
//! ("Rerouting: announced, escapable, never silently substituting"). This module
//! is that section shipped; the implementation record is
//! `docs/DESIGN-toolchain-reroute-2026-09-07.md`. The owner's ask that day,
//! verbatim: *"aterm MUST intercept and loudly warn about using non-aterm cargo,
//! rustc, and other Trust-toolchain-replacements."* Measured the same day on the
//! dev box: `~/.cargo/bin` (rustup's proxies, upstream stable) sat at PATH
//! position 17, ahead of the managed store at 19, and the store carries no
//! `cargo`/`rustc` by design — so a bare `cargo` in a session ran upstream Rust,
//! silently. This is the layer that answers.
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
//! directory is not `bin/`. The rc hook DOES move this directory first — inside an
//! aterm session only, in the same gated arm as `agents/`, and not under
//! [`PASSTHROUGH_ENV`] (2026-09-27, `crate::hooks`): the transparent-session lane has no
//! shell integration, and there `path_helper` left it behind `/opt/homebrew/bin`, so a
//! bare `cargo` ran Homebrew's with no announcement. Outside aterm the hook removes it.
//!
//! # The policy is per row, and the table is data
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
//!   THE SPELLING IS `aterm_types::rust_lane`'s (2026-09-23): one vocabulary with
//!   `aterm pkg lane` ([`crate::lane`]) and `aterm help rust`, so a cargo verb
//!   gets the lane targo accepts for it — `build`/`check`/`test` both, `run`/`doc`/
//!   `install`… the unverified one, `metadata`/`fmt`/`tree`… none, `clippy`
//!   `targo tippy` — never a command targo refuses. And a directory whose
//!   `rust-toolchain.toml` pins a STOCK channel says so under the lanes: that pin
//!   moves only rustup's proxies, never `targo` ([`Pin`]).
//!
//!   WHAT RUNS IS NAMED BY WHAT IT IS (2026-09-29). `cargo` and `rustc` are resolved
//!   before a word is said ([`crate::upstream_identity`]): through rustup when the copy
//!   on PATH is its proxy, then by the resolved file's `-vV`. The first line says `->
//!   Trust 7d5533b2, via rustup trust (the rustup default)` or `-> STOCK rustc 1.97.1
//!   (8bab26f4f), via rustup 1.97.1-aarch64-apple-darwin (pinned by …); not Trust (no
//!   proof claim)`, and one line under the lanes names the file. Measured that day after
//!   `rustup default trust`: a bare `rustc -vV` ran the Trust seal under "running upstream
//!   'rustc' (no proof claim)", the words it printed over stock 1.97.1 — the owner: "fix
//!   that naming so that there is NO CONFUSION NOW". `tlc` is not resolved: it has no
//!   Trust build to be confused with.
//!
//!   ANNOUNCE, DO NOT PREVENT. This row used to refuse (exit
//!   [`REFUSAL_EXIT`]) on the reading that "the friction is the feature". The
//!   owner's instruction of 2026-09-08 decides otherwise, verbatim: *"We don't
//!   want to prevent agents from using the vanilla rust toolchain, but we do
//!   want … some kind of printed message when using these tools that could be
//!   suppressed with a flag"* — and his 2026-09-07 ask, which this module was
//!   built to close, says *"intercept and loudly WARN"*, not refuse. The
//!   refusal came from a review, not from either instruction. What §4 actually
//!   withholds is SILENT substitution, which is why it already let
//!   `cargo +stable build` through with a signpost of its own
//!   ([`stock_channel_signpost`]); a bare `cargo build` that announces and then
//!   runs is no more silent than that one. The suppression the owner asked for is a SETTING, `[reroute]
//!   announce = false` in aterm.toml ([`crate::config::RerouteConfig`], Settings
//!   ▸ Packages) — owner, 2026-09-22: settings, "NOT ENV VARS those are for
//!   development". `ATERM_REROUTE_QUIET` is gone, and so is `ATERM_REROUTE_STRICT`,
//!   the opposite knob that restored the refusal: a second policy lane nobody in the
//!   tree set.
//!
//!   ONE EXCEPTION, A TRUST SOURCE CHECKOUT (2026-09-29). Inside one — `x.py` beside a
//!   `src/stage0` whose `compiler_dist_channel=trust`
//!   ([`crate::upstream_identity::trust_checkout`]) — a `cargo`/`rustc` that resolves to
//!   anything but Trust (stock, or not known) does not run: exit [`REFUSAL_EXIT`], naming
//!   what it resolved to, the checkout, the Trust spelling, and how to run it anyway,
//!   `aterm --no-reroute` — [`trust_checkout_refusal`]. There a stock compiler would build
//!   part of Trust: the owner's direction, "NO STOCK RUST IN THE TOOLCHAIN FOR TRUST",
//!   decides the refusal, as `[reroute] strict` refuses z3. `[reroute] announce = false`
//!   does not silence it: a refusal is not an announcement.
//!
//!   WHAT IS JUDGED IS A NAME LOOKED UP ON PATH, wherever it is looked up: typed, spawned
//!   by a tool, or spawned beneath a command the reroute itself ran — that one carries
//!   [`ANNOUNCED_ENV`], which keeps its tree's by-name spawns quiet and still judged
//!   (2026-10-01: it carried the escape, and a stock `rustc +1.97.1` beneath an allowed
//!   Trust cargo ran unjudged), with no store resolving or with atpkg unreachable alike. A
//!   cargo started by its path (or by `rustup run`) runs `rustc` by name, and that rustc
//!   is judged. NOT JUDGED, because no name is looked up: a compiler run by its own path,
//!   one handed to cargo as `RUSTC`, `RUSTC_WRAPPER` or `build.rustc` (cargo then runs that
//!   path), and everything in a session started with `aterm --no-reroute`.
//! * **ORACLE** (`z3`): `ay`'s differential oracle. Announce, naming `ay`, then run
//!   upstream z3 — as a SIGNPOST does. It used to refuse (exit 2) on the reading that
//!   running the wrong oracle manufactures a false proof claim; the owner's ruling of
//!   2026-09-29 below covers it too, and the risk now lives in the line, which says
//!   that z3 is ay's oracle, not its stand-in. `[reroute] strict = true` refuses again,
//!   naming the upstream copy's own path (a path never meets a stub).
//!
//! # Every stub runs something (2026-09-29)
//!
//! The owner, verbatim: *"fix aterm directly to make these rules informative and concise
//! warnings for the AI, not extra blockers to slow down my work"*. So a stub never stops
//! a command it could run: a DIRECT row whose Trust tool is missing (not installed, or
//! its shim lost) runs the upstream tool the caller named, with one line naming the fix;
//! `z3` runs upstream with one line naming `ay`; a stub that cannot reach atpkg walks
//! PATH for the upstream tool in `sh`, as the escape does, with one line. Exit 127 is
//! left only where nothing exists to run. `[reroute] announce = false` silences these
//! lines like any upstream-runs line; `[reroute] strict = true`
//! ([`crate::config::RerouteConfig::strict`]) restores every refusal as it was. One
//! exception came the same day, and no setting lifts it: inside a Trust source checkout a
//! `cargo`/`rustc` that is not Trust does not run ([`trust_checkout_refusal`], above).
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
//! * `<tool> +<toolchain>` naming a non-Trust toolchain is the user naming
//!   upstream deliberately: it runs as named, after [`stock_channel_signpost`] —
//!   the Trust spelling of the same command, silenced like any signpost by the
//!   setting — because §4 withholds only *silent* substitution. That holds for the
//!   DIRECT Rust rows too since 2026-09-23: `rustfmt +1.97.1 x.rs` used to hand
//!   `+1.97.1` to trustfmt as a file name (ledger item HOST-05). `+trust…` keeps
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
//!   when it is not Trust (measured 2026-09-30: `rustup run 1.97.1 cargo build` there
//!   failed at cargo's `rustc -vV`). Its compiles carry cargo's own mark,
//!   [`CARGO_CRATE_ENV`], set on every compile it spawns for a crate, and those two stubs
//!   ([`CARGO_COMPILERS`]) take it as they take [`ANNOUNCED_ENV`]: not announced again,
//!   run as the cargo chose, and a `rustc` still judged inside a Trust source checkout —
//!   so a cached `rustc -vV` (cargo keeps one per target dir) lets no stock compile
//!   through there. Until 2026-10-02 they did not: `rustup run stable cargo test --doc`,
//!   `~/.cargo/bin/cargo doc` and `/opt/homebrew/bin/cargo doc` ran Trust's trustdoc over
//!   crates stock rustc had just built (E0514) and announced every `rustc`, while the same
//!   command typed bare, through the cargo stub, passed. Trust's own cargo marks its
//!   compile twice ([`TARGO_CRATE_ENV`]) and is answered as a typed name is: its store
//!   build has `trustdoc` and no `rustdoc` beside it, so passed upstream, the `rustdoc` it
//!   spawns by name was stock Rust's over trustc's crates (E0514) or, with no stock Rust
//!   installed, none — `<sysroot>/bin/cargo doc` exit 127 (the 2026-10-02 review).
//!   Only `aterm --no-reroute` carries a whole command tree past the stubs: what a
//!   reroute-run command spawns by name meets them too ([`ANNOUNCED_ENV`]).
//!
//! # What a stub is not
//!
//! Not a managed tool. It execs through a variable — never the literal `exec '`
//! line `platform::parse_sh_shim_target` keys on — so `resolve_shim` answers
//! `None`, `active_builds` never counts it, and doctor's broken-shim scan ignores
//! it. It is recognized, rewritten and removed ONLY by its marker line
//! ([`STUB_MARKER`]), the discipline `stub.rs` established. Windows lays nothing
//! (TARGET: a `.cmd` dialect, and a prepend applied after
//! `winpath::refresh_child_path` rewrites the child PATH).

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_types::rust_lane;

use crate::store::Layout;
use crate::upstream_identity::Resolved;

/// Exported by the spawn seams: the absolute reroute directory of THIS session,
/// so the shell integration can re-assert it first after rc files ran.
pub const REROUTE_DIR_ENV: &str = "ATERM_REROUTE_DIR";
/// Exported by the FRONT DOOR (`crates/aterm/src/main.rs`) to a TTY session: the
/// absolute managed `<prefix>/agents/` ([`Layout::agents_dir`]), ensured to exist, on
/// EVERY lane — engaged reroute or not (2026-09-18, closing the R3 residual of
/// 2026-09-16: the session derived the directory as `$ATERM_REROUTE_DIR`'s sibling,
/// so `--no-reroute` — the escape hatch for the UPSTREAM RUST NAMES — also dropped the
/// managed `claude`/`codex` front-insert). Absent or EMPTY
/// means none. Deliberately NOT `ATPKG_AGENTS`: that is the name the shell hook
/// exports ([`crate::hooks`]) and the shell integration keys its hook-sourcing on it
/// being unset, so a seam-exported `ATPKG_AGENTS` would stop a tab from ever sourcing
/// the hook. The shell integration must never read THIS variable either — it is a
/// launcher→session handoff, nothing more.
pub const AGENTS_DIR_ENV: &str = "ATERM_AGENTS_DIR";
/// INTERNAL PROTOCOL, not a setting: the marker of a session with every upstream tool
/// restored. `aterm --no-reroute` stamps it on its session (and the front door CLEARS
/// an inherited one on every other launch, so exporting it by hand does not make a
/// session), and the stub and [`run`] read it. Nothing else sets it: an upstream child
/// [`exec_upstream`] runs, and one a stub that cannot reach atpkg runs, carry
/// [`ANNOUNCED_ENV`] instead (2026-10-01). It was the user
/// knob `ATERM_NO_REROUTE` until 2026-09-23; the flag is the one spelling now (owner,
/// 2026-09-22: "NOT ENV VARS those are for development").
///
/// SPELLED AS WHAT IT IS (2026-09-23 review): the stub and the shell integration are
/// shell, and a shell reads the marker from its own environment, so an `export` in an
/// rc file AFTER the front door cleared it still reaches them — a marker a shell reads is
/// unavoidably settable. It is not SUPPORTED as a control: the double-underscore
/// spelling says internal, no page documents it, and `aterm --no-reroute` is the escape.
/// As `ATERM_REROUTE_PASSTHROUGH` it read like the deleted knob under a new name.
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
/// WHY NOT THE ESCAPE (2026-10-01 review, reproduced): [`exec_upstream`] stamped
/// [`PASSTHROUGH_ENV`], so inside a Trust checkout the Trust cargo the reroute allowed —
/// `-> Trust 7d5533b2` on its line — carried the ESCAPE to its whole tree, and a `cargo
/// demo` whose subcommand ran `rustc +1.97.1 -V` by name ran stock there with no line at
/// all, while the same `rustc +1.97.1` typed alone was refused. A cargo subcommand,
/// `cargo run -p xtask`, a build script that spawns `cargo +nightly`, and Trust's own
/// `src/tools/miri/miri-script` (`rustc +miri`, `cargo +miri`) are that shape. The escape
/// says "judge nothing"; this says "say nothing again". The `cargo`/`rustc` stubs ask
/// atpkg under it only where the directory may be a Rust source checkout — an `x.py`
/// beside a `src/stage0` up the tree ([`sh_source_checkout_test`]) — so a cargo build
/// anywhere else costs its by-name `rustc` spawns a few `stat`s, not an atpkg each.
pub const ANNOUNCED_ENV: &str = "__ATERM_REROUTE_ANNOUNCED";
/// Line 2 of every stub — the ONLY thing that makes a file ours to rewrite or
/// remove. Version-suffixed so a changed body can be told from an old one.
pub const STUB_MARKER: &str = "# atpkg reroute stub v1";
/// The hidden verb a stub execs: `atpkg __reroute <upstream> [args…]`.
pub const HIDDEN_VERB: &str = "__reroute";
/// A refusal under `[reroute] strict = true` — the ORACLE row, and the fail-closed answer
/// when atpkg itself cannot be reached — a `cargo`/`rustc` that is not Trust inside a
/// Trust source checkout ([`trust_checkout_refusal`]), and a `__reroute` usage error: a
/// usage-shaped exit — the command did not run because the caller has to choose, not
/// because anything broke. Elsewhere, by default, nothing a stub could run is refused
/// (2026-09-29).
pub const REFUSAL_EXIT: u8 = 2;
/// The directory under the manager prefix (`<prefix>/reroute`).
pub const DIR_NAME: &str = "reroute";
/// A marker FILE `lay` puts in every reroute directory. It is how a walk
/// recognizes a reroute directory under ANY spelling — a second prefix (a
/// nested aterm under another `HOME`), a symlink, a trailing slash — without
/// comparing directory strings and without reading a candidate's bytes. The
/// review of 2026-09-07 reproduced the failure this closes: the stub's escape
/// walk skipped only its own directory by string equality, so two reroute
/// spellings on PATH exec'd each other's stub forever, silently, at 100% CPU —
/// in the one path the design promises always works.
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

/// A DIRECT row whose branded tool is VERB-FIRST: the upstream name takes a
/// source file as its first argument, the branded tool takes a subcommand.
///
/// `lean file.lean` is the case this exists for. Every other DIRECT row is
/// argument-compatible with its branded tool (`clippy`→`tippy`,
/// `rustfmt`→`trustfmt`, `rustdoc`→`trustdoc`), so plain passthrough is right
/// for them; `clean` is verb-first, so passthrough produced
/// `clean file.lean` → "unrecognized subcommand 'file.lean'" and the Lean name
/// was unusable inside a session.
///
/// Keyed on the source EXTENSION, never on "the argument is not a flag": the
/// branded tool has its own subcommands (`repl`, `eval`, `lake`), and a
/// not-a-flag rule would rewrite `lean repl` into `clean check repl`. Matching
/// `.lean` cannot collide with a subcommand and leaves `lean --version`,
/// `lean repl` and an explicit `lean check f.lean` exactly as the caller wrote
/// them.
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
    /// Run `branded` with the caller's arguments; announce one line. With `branded`
    /// missing, run upstream with one line naming the fix (refused under strict).
    /// `program` is the program `aterm pkg install` names to install `branded`.
    /// `source_verb` is `Some` only when the branded tool is VERB-FIRST.
    Direct {
        branded: &'static str,
        program: &'static str,
        source_verb: Option<SourceVerb>,
    },
    /// Announce `branded` and every `lane` with the caller's arguments, then run upstream.
    /// An empty `lanes` means "one spelling, equivalence unproven" (`tlc`).
    Signpost {
        branded: &'static str,
        lanes: &'static [Lane],
    },
    /// Announce `branded`, then run upstream: the upstream tool is `branded`'s
    /// differential oracle. Under `[reroute] strict = true`, refuse, naming the upstream
    /// copy's own path (a path never meets a stub).
    Oracle { branded: &'static str },
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
/// proves nor builds. Every other verb is spelled by `aterm_types::rust_lane`
/// (2026-09-23): both lanes for `build`/`check`/`test`, the unverified one for a
/// verb with no verified lane, none for a verb that takes no lane — pinned
/// against this table by `the_table_speaks_the_shared_rust_lane_vocabulary`.
pub const CARGO_VERB_REROUTES: &[(&str, &str)] = &[("clippy", "targo tippy"), ("fmt", "targo fmt")];

/// THE table. Order is the order doctor and `aterm help reroute` list it in.
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
            lanes: &[],
        },
    },
    Row {
        upstream: "rustc",
        policy: Policy::Signpost {
            branded: "trustc",
            lanes: RUSTC_LANES,
        },
    },
    Row {
        upstream: "cargo",
        policy: Policy::Signpost {
            branded: "targo",
            lanes: CARGO_LANES,
        },
    },
    Row {
        upstream: "z3",
        policy: Policy::Oracle { branded: "ay" },
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

/// THE ONE reading of the [`PASSTHROUGH_ENV`] marker — `aterm_types`'
/// `env_flag_engaged` rule restated here so a present-but-empty value (which travels)
/// never counts: engaged iff non-empty and not `"0"`.
#[must_use]
pub fn engaged(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// `+<toolchain>` as the FIRST argument (rustup's own spelling) — the user naming
/// a toolchain deliberately. `None` for anything else.
/// ([`aterm_types::rust_lane::explicit_toolchain`], the one reading.)
#[must_use]
pub fn explicit_toolchain(args: &[String]) -> Option<&str> {
    rust_lane::explicit_toolchain(args)
}

/// Whether `row` names a stock RUST tool: the rows a rustup `+<toolchain>` means
/// something to, and the ones [`rust_lane::trust_spelling`] spells.
fn is_rust_row(row: &Row) -> bool {
    rust_lane::STOCK_TOOLS
        .iter()
        .any(|(stock, _)| *stock == row.upstream)
}

/// Where a channel is pinned for the directory a stub runs in: a toolchain FILE
/// rustup would read ([`rust_lane::find_pin`]), or `$RUSTUP_TOOLCHAIN`, which rustup
/// ranks above any file. It says what rustup's proxies resolve to, and — the fact the
/// 2026-09-23 incident turned on — that it moves nothing else: `targo`, `tippy`,
/// `trustfmt` and `trustdoc` are not rustup proxies.
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

/// The page that answers which toolchain this directory gets — named on every signpost
/// and every refusal (owner, 2026-09-08: the Trust default is to be "very strongly
/// encouraged by the aterm system itself"): a reader about to run stock cargo anyway is
/// told, in the same breath, the one command that measures it.
const MEASURING_LINE: &str = "       `aterm help rust` shows which toolchain this directory gets; the default here is Trust.";

/// The last lines of a SIGNPOST message: [`MEASURING_LINE`], and the setting that silences
/// the line, because a line printed on every build must say how to stop printing it. What
/// is about to happen (what runs) is the message's FIRST line, in each arm.
fn signpost_closing() -> String {
    format!(
        "{MEASURING_LINE}\n       ({ANNOUNCE_SETTING} in aterm.toml silences this — Settings ▸ Packages.)"
    )
}

/// The lines a STOCK pin adds under a SIGNPOST's lanes (2026-09-23) — the incident's
/// shape: an agent read `channel = "1.97.1"` and concluded stock was this repository's
/// lane, when the pin moves only rustup's proxies. A Trust pin is the default and adds
/// nothing; `$RUSTUP_TOOLCHAIN` is named as the environment, not the directory.
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

/// The SIGNPOST announcement, the caller's arguments filled into every lane — unresolved,
/// the form `tlc` prints, and the frame every Rust row's resolved form shares:
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
/// `pin` is the directory's [`Pin`], when it has one — named under the lanes — and
/// `resolved` is what the name RESOLVED to ([`crate::upstream_identity::resolve`]), which
/// [`plan`] asks for `cargo` and `rustc`: the first line then names what runs by what it
/// is, and one line under the lanes names the file (2026-09-29). Measured before that day:
/// after `rustup default trust`, a bare `rustc -vV` ran the Trust seal 7d5533b2 under
/// "running upstream 'rustc' (no proof claim)" — the same words it printed over stock
/// 1.97.1 in a pinned directory.
///
/// ```text
/// aterm: running 'rustc' -> Trust 7d5533b2, via rustup trust (the rustup default); Trust's own spelling is 'trustc':
///          trustc main.rs                      VERIFIED   — proves as it compiles
///          trustc -Ztrust-verify=off main.rs   UNVERIFIED — compiles as vanilla Rust
///        ('rustc' here is /Users//me/toolchains/trust-7d5533b2/bin/rustc.)
/// aterm: running 'rustc' -> STOCK rustc 1.97.1 (8bab26f4f), via rustup 1.97.1-aarch64-apple-darwin (pinned by /r/rust-toolchain.toml); not Trust (no proof claim); on Trust the tool is 'trustc':
/// ```
///
/// Every spelling of a Rust row is [`rust_lane::trust_spelling`]'s — the vocabulary
/// `aterm pkg lane` and `aterm help rust` share — so a cargo verb gets the lane targo
/// ACCEPTS for it: `cargo build` both, `cargo run` the unverified one only, `cargo
/// metadata`/`cargo fmt` none, `cargo clippy` `targo tippy`. Until 2026-09-23 every verb
/// but `clippy` and `fmt` got both lanes, so `cargo metadata` printed `targo trust
/// metadata` and `targo --unverified metadata` — two commands targo refuses.
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

/// A SIGNPOST row with no upstream copy on PATH: nothing ran, so the first line says
/// that, the branded lanes follow, and the closing lines (which silence an
/// announcement) do not:
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

/// The signpost for `<upstream> +<stock channel> …` on any Rust row — a STOCK toolchain
/// named on the command line (`cargo +1.97.1 test`, `rustfmt +1.97.1 x.rs`). `args` is
/// what follows the directive. Upstream then runs AS NAMED: an announcement, never a
/// prevention, since the reroute cannot tell a person's `cargo +stable` from a tool's
/// by-name spawn.
///
/// It replaced a one-line note that 33 agent logs of 2026-09-23 carried beside a `cargo
/// +1.97.1` that ran anyway, and which named no Trust command with the caller's
/// arguments — so the Trust spelling of the very command leads, both lanes where there
/// are two, and one line says a stock channel moves only rustup's proxies. The setting
/// silences it like any signpost, and it says so.
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

/// THE REFUSAL INSIDE A TRUST SOURCE CHECKOUT (2026-09-29): `cargo`/`rustc` resolved to
/// something that is not Trust ([`crate::upstream_identity::trust_checkout`] found the
/// checkout at `root`), so nothing ran. What it resolved to leads, the checkout and why
/// follow, then the Trust spelling of the very command, the file it resolved to, and how
/// to run that copy anyway: `aterm --no-reroute`. Its own path is not named: run by path,
/// a `cargo` spawns `rustc` by name, which is refused here in turn (measured 2026-09-30,
/// `error: process didn't exit successfully: `rustc -vV` (exit status: 2)`) — a promise
/// the refusal made and cargo did not keep:
///
/// ```text
/// aterm: 'cargo' did not run -> STOCK cargo 1.97.1 (c980f4866), via rustup 1.97.1-aarch64-apple-darwin (pinned by /t/rust-toolchain.toml); not Trust,
///        and /t is a Trust source checkout (its src/stage0 says compiler_dist_channel=trust), where a compiler not known to be Trust would build part of Trust; on Trust the tool is 'targo':
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
    // The first line's state: what runs — by what it IS, once resolved — or that nothing
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
                "aterm: '{typed}' did not run -> {},\n       and {} is a Trust source checkout (its src/stage0 says compiler_dist_channel=trust), where a compiler not known to be Trust would build part of Trust",
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
            // answers "no such command" (measured for `targo nextest`/`targo deny`).
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
        Install::Runs(_) | Install::Shim(_) => None,
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

/// Printed by a STRICT stub ([`crate::config::RerouteConfig::strict`]) when atpkg is
/// unreachable — fail CLOSED, with the escape named, never a silent fall-through to
/// upstream.
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
/// the directory is inside a Trust source checkout, as far as `sh` can tell
/// ([`sh_source_checkout_test`], then a `src/stage0` line naming the `trust` channel):
/// nothing can be resolved, so nothing is known to be Trust, and there that does not run
/// ([`trust_checkout_refusal`]'s rule, 2026-10-01). Exit [`REFUSAL_EXIT`].
#[must_use]
pub fn unreachable_in_checkout_message(upstream: &str) -> String {
    format!(
        "aterm: '{upstream}' did not run — aterm's package manager is not reachable, so what '{upstream}' runs here cannot be named, and this is a Trust source checkout (its src/stage0 names the trust channel), where only Trust may build. `aterm pkg repair` re-lays this stub; `aterm --no-reroute` runs upstream '{upstream}'."
    )
}

/// `atpkg __reroute` with no resolvable store (no `HOME`, an `env -i` wrapper) and an
/// upstream copy on PATH: it runs, after this line (the default since 2026-09-29) —
/// named by what it resolved to for `cargo`/`rustc` (`resolved`, [`Resolved::verdict`]),
/// as every signpost names it: `aterm: running 'rustc' -> Trust 7d5533b2, via rustup trust
/// (the rustup default) — no package store resolves here, …`.
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
/// ran, and the escape (the answer this verb gave before 2026-09-29).
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

/// The row's policy as ONE clause, `<args>` standing for the caller's arguments — the
/// sentence `aterm pkg which <upstream>` answers with. Built from the same row the stub
/// applies, so the `which` surface cannot drift from what the stub prints (design S6:
/// one "which copy runs and why" surface). The escape is named only where it reaches
/// something (`upstream_found`, [`upstream_on_path`]), as in [`direct_announcement`];
/// with no upstream copy a SIGNPOST or ORACLE row says so, as the stub's own line does
/// ([`no_upstream_message`], [`oracle_not_on_path_message`], [`oracle_message`]).
/// `strict` is `[reroute] strict` ([`crate::config::RerouteConfig::strict`]): the
/// ORACLE row refuses, and a DIRECT row names no upstream fallback.
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
            // The default since 2026-09-29: a missing Trust tool runs upstream, with a
            // line — said only where an upstream copy is there to run.
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
        Policy::Signpost { branded, lanes: [] } => format!(
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
        Policy::Oracle { branded } if strict && upstream_found => format!(
            "refused ('{branded}' is measured against it); its own path reaches the real {upstream}{escape}"
        ),
        Policy::Oracle { branded } if strict => format!(
            "refused ('{branded}' is measured against it); no upstream '{upstream}' is on PATH"
        ),
        Policy::Oracle { branded } => format!(
            "announced, naming '{branded}' (it is measured against {upstream}){then_run}{escape}"
        ),
    }
}

// ── the stub ────────────────────────────────────────────────────────────────

/// One line of both stubs' pass-through walks: `continue` past a candidate that is a
/// reroute stub, known by what it SAYS — line 2 is [`STUB_MARKER`], the rule
/// [`is_reroute_stub`] gives [`upstream_on_path`], so the shell's walk and atpkg's skip
/// the same files. Builtins only (`[`, `read`), so it runs on whatever PATH the caller
/// has. It catches a stub under ANY other name: a symlink (read through), a hard link,
/// and a COPY — `cp <prefix>/reroute/* ~/bin/` copies every stub and, the glob skipping a
/// dotfile, no [`DIR_MARKER_FILE`]. The `-ef "$0"` test this replaced (2026-09-24 review)
/// compared inodes, so a copy slipped past it, and a copy plus a link, or two copies,
/// exec'd each other forever. `-r` first, and `2>/dev/null` ahead of the `<`, so an
/// unreadable candidate costs no stray line; it is not a stub, and is exec'd as before.
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
/// executable `$__aterm_name` on `PATH` past EVERY reroute directory (by
/// [`DIR_MARKER_FILE`], under any spelling and any prefix) and past any stub under
/// another name ([`sh_skip_a_stub`]), exec'd after `before_exec` (lines of `sh`, each
/// indented here). Falls through, `IFS` and globbing restored, when there is none.
fn sh_upstream_walk(indent: &str, before_exec: &[String]) -> String {
    sh_path_walk(indent, "$__aterm_name", "", before_exec)
}

/// [`sh_upstream_walk`] for any `program` (a word of `sh`, placed after `$__d/`), exec'd
/// with `args` (`sh` text, empty or ending in a space) ahead of the caller's `"$@"`. The
/// stub's search for atpkg goes through it too, as does the pending-install stub's
/// ([`crate::stub`]): the shell's own `command -v atpkg`
/// honoured a relative or empty PATH entry, so with no live embedded atpkg a bare `cargo`
/// in a cloned tree ran that tree's `./atpkg` (2026-10-01 security review), while every
/// other walk here answers absolute entries only.
///
/// Pure shell, no subshell and no external command: it runs BEFORE anything is resolved,
/// on whatever PATH the caller has (a synthetic one in tests has no `tr`). `IFS=:` splits
/// PATH; `set -f` keeps a `*` in an entry from globbing; the parameter test says
/// "absolute" without a `case`, which macOS /bin/sh (bash 3.2) mis-parses inside `$(…)`.
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
    // By MARKER FILE, not by string equality with our own directory: a second
    // prefix's reroute dir, a symlink to ours, or ours with a trailing slash
    // is still a reroute dir, and exec'ing its stub under the escape looped
    // forever (measured in the 2026-09-07 review).
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
    // A stub under another name — a link, a hard link or a COPY in an ordinary PATH
    // directory — carries no marker beside it, and exec'ing it looped forever
    // (2026-09-24): known by what it says ([`sh_skip_a_stub`]).
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
/// PHYSICAL, AS ATPKG WALKS: by `.`, `..`, `../..`, which the kernel resolves through the
/// real parent, never through `$PWD` — a logical path through a symlink into a checkout
/// has ancestors outside it. Root is where `..` is the same directory (`-ef`); past
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
/// * every `exec` goes through a VARIABLE, never the literal `exec '` a managed
///   shim carries, so the shim parser sees no target (a stub is not an install);
/// * the escape hatch is decided IN `sh`, before atpkg is consulted, walking
///   `PATH` past EVERY reroute directory (by [`DIR_MARKER_FILE`], under any
///   spelling and any prefix — never just the one it was laid in) and past any
///   stub under another name (a link or a copy of one in `~/bin` carries no marker
///   beside it and would exec a stub forever; known by its content,
///   [`sh_skip_a_stub`]) for the first absolute-entry executable of the same
///   name — the escape works with no atpkg at all;
/// * under [`ANNOUNCED_ENV`] (a child of a command the reroute ran) — or, in a
///   [`CARGO_COMPILERS`] stub, a stock cargo's own compile, known by [`CARGO_CRATE_ENV`]
///   without [`TARGO_CRATE_ENV`] (module doc, "Escapes"), which is stamped with that
///   marker — it runs upstream by the same walk, saying nothing; except a `cargo`/`rustc`
///   stub ([`names_what_runs`]) in a directory that may be inside a Rust source checkout
///   ([`sh_source_checkout_test`]), which asks atpkg, so a stock compiler spawned by name
///   under an allowed Trust cargo, or by a stock cargo started by its path, is judged as
///   the same name typed alone (2026-10-01). Decided before atpkg, so no arm below it —
///   strict, an unreachable atpkg, no store — can tell a `rustup run` cargo's compiles
///   from a stub-started one's;
/// * with atpkg unreachable it RUNS UPSTREAM by the same walk ([`sh_upstream_walk`]),
///   after one line ([`unreachable_fallback_message`]) and with the escape's marker
///   stamped on the child, so a `cargo` it runs spawns `rustc` without a second line
///   (2026-09-29, the owner's "warnings not hard limits"); 127 and one line only when no
///   upstream copy is on PATH. A `strict` stub (`[reroute] strict = true`, carried in
///   its bytes because the stub cannot read aterm.toml) fails closed as before: the
///   escape named, exit 2, never quietly running upstream. A `cargo`/`rustc` stub inside
///   a Trust source checkout fails closed too, in either mode
///   ([`unreachable_in_checkout_message`]): nothing resolved is nothing known to be Trust.
///
/// `atpkg` is the one [`crate::stub::Embedder`] settled on; the EMPTY path names none
/// (`ATPKG=''`, which the `[ -x ]` refuses), and the stub goes straight to the first
/// absolute-entry `atpkg` on PATH (the same walk, [`sh_path_walk`]).
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
    // Upstream, as the escape reaches it — announced, and marked as every upstream the
    // reroute runs is ([`ANNOUNCED_ENV`]), so its own spawns (cargo's `rustc`) pass without
    // a line each and are still judged; never the escape, which judges nothing.
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
/// * INSIDE ATERM — any of [`crate::hooks::AGENTS_MARKERS`] non-empty, the SAME list the
///   rc hook's gate is rendered from — with `<agents_dir>/<name>` executable: exec that
///   twin, which carries the self-update interception and the managed store path.
/// * Otherwise PASS THROUGH, [`stub_body_sh`]'s escape walk: the first `<name>` on PATH
///   that is not in a reroute directory (by [`DIR_MARKER_FILE`] — any spelling, any
///   prefix), not `<agents_dir>` itself (as spelled or by `-ef`, so a symlink or a
///   trailing slash is skipped too) and not a stub under another name (a link or a copy of
///   one in `~/bin` would exec a stub forever; known by its content, [`sh_skip_a_stub`]) —
///   else exit 127 with one line. Outside
///   aterm that is the user's own copy, never the `agents/` twin (owner law, 03513b5d7:
///   *"claude managed via aterm should be in aterm only, NOT in all terminals like
///   iTerm"*) — or, with none of the user's on PATH, `<prefix>/bin/<name>`, which the rc
///   hook appends last in every shell: exactly what that shell ran with no stub.
/// * The `aterm --no-reroute` marker ([`PASSTHROUGH_ENV`]) DOES NOT APPLY HERE (manager's
///   ruling, 2026-09-23). It restores the upstream Rust names, and `aterm help pkg`
///   promises it never swaps the managed `claude`/`codex` for a foreign one — a promise
///   that must hold in EVERY generation of shell. A stub that let it through would run the
///   foreign copy in an old shell and the managed one in a new shell under the same
///   marker: the very class of bug this stub exists to close. The markers alone decide.
///
/// ONE THING IT CANNOT REACH: a shell's own command cache. zsh and bash remember where a
/// command was found the first time it ran, so a shell that ran `claude` from
/// `~/.local/bin` BEFORE this stub was laid keeps running that path until `rehash` (zsh)
/// or `hash -r` (bash) — once; from then on the name resolves to this stub, which decides
/// at every run. Measured by the 2026-09-23 review in `zsh -f -i` and bash against fakes.
///
/// Pure shell, no external command and no atpkg: it works on whatever PATH the caller has.
/// It never recurses — the twin execs its store path by absolute path, and the walk skips
/// every reroute directory and every stub under any other name. Line 2 is [`STUB_MARKER`], so every recognition, sweep and
/// shim scan treats it as the other stubs; it execs only through variables, so
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
    // A stub under another name — this one or another prefix's, linked or COPIED into an
    // ordinary PATH directory — would otherwise exec a stub again, forever.
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
/// [`crate::activate::reconcile_agents`] from that same roster) stands as an executable
/// file now. A name with no twin gets none — a stub for a program that is not managed here
/// would make `command -v claude` answer on a machine with no `claude` at all — and loses
/// the one it had on the next [`lay`].
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

/// OUR agents stub for `name` ([`stub_path`]) when it is the FIRST executable `name` on
/// `path_var` — the copy a shell with that PATH runs. Absolute entries only, as every walk
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

/// Whether `path` is one of OUR stubs: a regular file (never a symlink), small,
/// UTF-8, whose second line is [`STUB_MARKER`].
#[must_use]
pub fn is_reroute_stub(path: &Path) -> bool {
    // A symlink is never ours (the stub is laid as a regular file), and the
    // crate's bounded SAME-HANDLE reader refuses a special file and a large
    // one on the handle it reads from — no path re-open between the check and
    // the read for a swapped FIFO to block on.
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
/// each agent program's with a twin ([`agents_routes`]).
///
/// A row's stub names the atpkg `embedder` settles on over the one the stub standing there
/// names now ([`crate::stub::Embedder::embed`]): never an atpkg that is not there, and never
/// one that lasts less than the live atpkg the stub already names — a dev build or a scratch
/// copy of aterm runs this lay too, at its every launch and session spawn. `strict` is the
/// `[reroute] strict` a row's stub carries ([`stub_body_sh`]).
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
/// And every agent program's stub ([`agents_stub_body_sh`], 2026-09-23) whose `agents/`
/// twin stands ([`agents_routes`]); one whose twin has gone is swept the same way. The
/// stub names the twin's PATH, never its build, so an update of the program re-lays
/// nothing here; a twin laid after a lay — the pass's alias reconcile, a hand-run
/// `aterm pkg install` — gains its stub at the next one: every pass, every session
/// spawn and `repair` call this.
///
/// A FILE IS WRITTEN ONLY WHEN ITS BYTES DIFFER (Phase 3, 2026-09-22): a pass or a spawn
/// changes only what is wrong in content.
///
/// THE ATPKG A STUB NAMES IS NOT SIMPLY THIS PROCESS'S (2026-09-25, the end-to-end run's
/// D1): a copy of aterm started from a scratch directory re-laid all eight stubs naming
/// `<scratch>/bin/atpkg`, a file that was never there, over stubs naming the installed
/// app's. [`crate::stub::Embedder`] decides, for the stubs, the pending stubs and the
/// `agents/` twins alike.
///
/// `[reroute] strict` is read FRESH here ([`crate::config::load_reroute`]), never cached:
/// a row's stub carries it in its bytes, and the long-lived window lays at every session
/// spawn — a value cached at its launch would re-lay the old policy over a newer one.
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
    // A recorded decline (`aterm pkg uninstall --all`) is durable: the toolset
    // the stubs would signpost is the one the user removed on purpose, and
    // every spawn seam calls this — without the gate the next tab would put
    // eight refusals back first on PATH one instant after the uninstall.
    if layout.declined().is_file() {
        return Ok(());
    }
    let dir = dir(layout);
    layout.ensure_dir(&dir)?;
    // THE MARKER FIRST: a walk racing this `lay` recognizes the directory before the first
    // stub lands in it.
    let marker = dir.join(DIR_MARKER_FILE);
    if marker_needs_lay(std::fs::read(&marker).ok().as_deref()) {
        crate::lay::write_in_process(&crate::lay::Executable::new(&marker, marker_body()))?;
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
    files.iter().try_for_each(crate::lay::write_in_process)?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(OsStr::to_str) else {
                continue;
            };
            // Dotfiles are never swept: the marker file, and a concurrent
            // `lay`'s `.<name>.stub-<pid>` temp between its write and rename
            // (two `lay`s at once — the first tab and `atpkg seed` — used to
            // unlink each other's temp and warn "not laid" for nothing).
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
    Ok(())
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

/// `atpkg __reroute <upstream> [args…]` — what a stub execs once the `sh`-side
/// escape hatch has NOT fired. Applies the row's policy; the exit code is the
/// exec'd tool's, 127 when nothing exists to run, and [`REFUSAL_EXIT`] only for a
/// refusal `[reroute] strict = true` asked for, a compiler that is not Trust inside a
/// Trust source checkout (below), and a name that is not a row.
///
/// A SIGNPOST row announces (unless `[reroute] announce = false`) and then execs
/// UPSTREAM, so its exit code is the upstream tool's. So does ORACLE since 2026-09-29,
/// and a DIRECT row whose Trust tool is missing (module doc, "Every stub runs
/// something").
///
/// NO ROW REFUSES AN AGENT (decided with evidence, 2026-09-23). The stub answers a
/// NAME looked up on PATH, and it cannot tell an agent's own `cargo` from a by-name
/// spawn inside a tool that agent ran (a build script, a Makefile, a project's own
/// stock-toolchain lane — stock by design where the Trust sysroot carries only its
/// host std), so a refusal here would turn those tools red for every agent that runs
/// them. What can refuse an agent's OWN command is its text: `aterm pkg lane`
/// ([`crate::lane`]) reads one command line, and a guard the owner wires calls it
/// (an agent's `PreToolUse` hook, say). aterm installs nothing into an agent
/// (decision "B", 2026-09-23), so it wires no such guard itself.
///
/// ONE PLACE REFUSES A NAME (2026-09-29): inside a Trust source checkout, a `cargo` or
/// `rustc` that resolves to anything but Trust exits [`REFUSAL_EXIT`] naming what it
/// resolved to ([`trust_checkout_refusal`]) — there a stock compiler would build part of
/// Trust, and a by-name spawn is exactly how a stock one would get in: so a by-name spawn
/// beneath a command this ran ([`ANNOUNCED_ENV`]) is judged the same, only not announced
/// again. Everywhere else a SIGNPOST stays a signpost.
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
    let machine = Machine {
        // Belt and braces: the stub decides this first, but `aterm <upstream>` and a
        // direct `atpkg __reroute` call arrive here without it. A stock cargo's compile
        // ([`CARGO_CRATE_ENV`]) is the stub's to decide: a `rustc` stub hands one here only
        // inside what may be a source checkout, and marked [`ANNOUNCED_ENV`] first; a
        // Trust cargo's ([`TARGO_CRATE_ENV`]) arrives, and is answered as a typed name.
        passthrough: engaged(std::env::var(PASSTHROUGH_ENV).ok().as_deref()),
        // A child of a command the reroute ran: said once, judged still.
        announced: engaged(std::env::var(ANNOUNCED_ENV).ok().as_deref()),
        // ANNOUNCE, THEN RUN — the 2026-09-08 ruling. `[reroute] announce = false`
        // (the owner's "suppressed with a flag", a setting since 2026-09-23) drops
        // the line but never the run.
        announce: &|| crate::config::cached_reroute().announce(),
        // WARN, DO NOT STOP — the 2026-09-29 ruling. `[reroute] strict = true` puts the
        // refusals back for whoever wants them.
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
/// `targo`, so it follows `targo`'s rule: its verified lane is left alone. Anything
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

/// Whether `row` is one whose upstream copy is RESOLVED and named by what it is, and
/// refused inside a Trust source checkout when that is not Trust: the two compilers'
/// names, `cargo` and `rustc` — a SIGNPOST Rust row.
fn names_what_runs(row: &Row) -> bool {
    matches!(row.policy, Policy::Signpost { .. }) && is_rust_row(row)
}

/// Where a branded tool stands on this machine — the cases `atpkg run` tells apart.
#[derive(Debug, PartialEq, Eq)]
enum Install {
    /// Its shim forwards to a build: the managed copy.
    Runs(Managed),
    /// `bin/<branded>` stands but forwards to no build — a yanked build's tombstone, a
    /// pending stub. It runs, and its own line says what is true of it.
    Shim(PathBuf),
    /// Its program's live build holds it, but its shim is gone
    /// ([`crate::cli::live_without_shim_of`]).
    ShimLost,
    /// Nothing here holds it.
    Absent,
}

/// A branded tool's managed copy, as its shim would exec it.
#[derive(Debug, PartialEq, Eq)]
struct Managed {
    /// Through the store, never PATH: [`crate::ops::exec_path`]'s, not `which`'s — this
    /// execs the tool without its shim, so it takes the exec root the shim's guard would
    /// (`crate::compat` — a trust build whose tippy refuses the store's own `bin/rustc`
    /// copy lints only from there).
    target: PathBuf,
    /// What the shim would export before its `exec` (`ops::exec_env`, design S7) —
    /// `atpkg run`'s rule, so a branded program whose policy declares an environment gets
    /// it by either door.
    env: crate::shim_env::ShimEnv,
}

/// The managed copy of `branded`, `None` when it is not installed.
fn managed(layout: &Layout, branded: &str) -> Option<Managed> {
    Some(Managed {
        target: crate::ops::exec_path(layout, branded)?,
        env: crate::ops::exec_env(layout, branded),
    })
}

/// Where `branded`, one of the tools `program` ships, stands on this machine ([`Install`]).
fn installed(layout: &Layout, branded: &str, program: &str) -> Install {
    if let Some(copy) = managed(layout, branded) {
        return Install::Runs(copy);
    }
    let Some(name) = crate::store::ToolName::new(branded) else {
        return Install::Absent;
    };
    let shim = layout.shim(&name);
    if crate::stub::is_pending_stub(&shim) || crate::cli::is_tombstone_shim(layout, branded) {
        return Install::Shim(shim);
    }
    if crate::cli::live_without_shim_of(layout, program, branded).is_some() {
        return Install::ShimLost;
    }
    Install::Absent
}

/// One `__reroute` call, decided BEFORE a word is printed, so no line announces a tool
/// that is not there to run: what to say on stderr, then what to exec — or, with nothing
/// to run, why.
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
    /// Say `say` and exit [`REFUSAL_EXIT`]: the ORACLE row under `[reroute] strict`, and a
    /// `cargo`/`rustc` that is not Trust inside a Trust source checkout
    /// ([`trust_checkout_refusal`]).
    Refuse { say: String },
    /// Exec a branded tool's `shim` that forwards to no build ([`Install::Shim`]),
    /// saying nothing: the shim's own line speaks.
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
    // The managed `branded` — or, where it cannot run, why ([`Unrunnable`]).
    let run_branded = |branded: &str,
                       program: &str,
                       args: Vec<String>,
                       say: &dyn Fn() -> String|
     -> Result<Plan, Unrunnable> {
        match (machine.branded)(branded, program) {
            Install::Runs(copy) => Ok(Plan::Branded {
                say: say(),
                copy,
                args,
            }),
            Install::Shim(shim) => Ok(Plan::Shim { shim, args }),
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
    // A child of a command the reroute ran ([`ANNOUNCED_ENV`]): upstream as typed, said
    // nothing — as under the escape — but a `cargo`/`rustc` is judged in a Trust source
    // checkout as the same name typed alone (2026-10-01).
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
    // A STOCK toolchain named on any Rust row — `cargo +1.97.1 test`, `rustfmt +1.97.1
    // x.rs` — runs upstream as named, after the Trust spelling of the same command. Until
    // 2026-09-23 a DIRECT row handed `+1.97.1` to its branded tool as an argument
    // (`trustfmt: Error: file '+1.97.1' does not exist`, measured) — ledger item HOST-05.
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
        // directive.
        //
        // With the branded tool missing, the upstream tool the caller named runs, after
        // one line naming the fix (2026-09-29) — with the caller's own arguments, as
        // upstream always gets them. Nothing runs only where nothing is there to run, or
        // under `[reroute] strict`.
        Policy::Direct {
            branded,
            program,
            source_verb,
        } => run_branded(branded, program, direct_args(source_verb, shown), &|| {
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
        }),
        Policy::Signpost { branded, lanes } => {
            // `+trust…` keeps the lane question, and the first line names that toolchain
            // as what runs.
            if upstream == "cargo"
                && shown
                    .first()
                    .is_some_and(|verb| EXPLICIT_LANE_VERBS.contains(&verb.as_str()))
            {
                // Upstream cargo has no `trust` subcommand and no `--unverified` flag, so
                // with targo missing there is nothing to fall back to.
                return run_branded(branded, EXPLICIT_LANE_PROGRAM, shown.to_vec(), &|| {
                    explicit_lane_note(upstream, branded)
                })
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
        // Announce, then run upstream (2026-09-29); strict refuses, naming the path.
        Policy::Oracle { branded } => {
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

/// A SIGNPOST row's upstream copy at `target` about to run as the caller typed it (`args`;
/// `shown` is what the lanes spell, `head` what the first line says of the run, `pin` the
/// directory's pin when one is named). For `cargo`/`rustc` ([`names_what_runs`]) the copy
/// is RESOLVED first — only where a line will name it or a Trust source checkout must
/// judge it, so `[reroute] announce = false` outside a checkout costs nothing — and inside
/// a checkout anything but Trust is refused, whatever the setting: a refusal is not an
/// announcement.
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

/// THE ONE JUDGEMENT of an upstream copy about to run as `row` (`shown` and `toolchain`
/// as the caller typed them; `target` the copy on PATH), for every door that runs one — a
/// signpost, a child of a command the reroute ran, a call with no store: `Err` with the
/// refusal when it is a `cargo`/`rustc` ([`names_what_runs`]) inside a Trust source
/// checkout and not Trust ([`trust_checkout_refusal`]), else `Ok` with what it resolved
/// to. Resolved only where that is needed — `named` (a line will name it) or a checkout to
/// judge it in — so a run that says nothing outside a checkout costs nothing.
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

/// The first upstream copy of `name` on PATH: an absolute entry that is not a
/// reroute directory (by marker — ANY spelling, any prefix), not under this
/// manager prefix, holding an executable regular file that is not itself a
/// reroute stub — nor a SYMLINK to one (2026-09-24): [`is_reroute_stub`] refuses
/// every link, rightly for ownership, so `ln -s <reroute>/cargo ~/bin/cargo` read
/// as upstream here, the escape exec'd the stub again, and an oracle refusal named
/// the link as the real tool's path. A walk that skips only THIS prefix (the one
/// `vendor`'s probes share) is why a nested session under another prefix used to
/// hand the escape to the enclosing session's stub and loop.
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

/// `atpkg __reroute` when NO store resolves (no `HOME`, an `env -i` wrapper; the caller
/// has said why): nothing managed can be looked up, so the upstream copy on PATH runs,
/// after one line — or, under `[reroute] strict = true`, nothing runs ([`REFUSAL_EXIT`]),
/// the answer before 2026-09-29. 127 when no upstream copy is on PATH.
///
/// A `cargo`/`rustc` is still RESOLVED and JUDGED here ([`judge`]), with no cache: its line
/// names what runs, and inside a Trust source checkout one that is not Trust does not run
/// (2026-10-01 review, reproduced: under `env -i PATH=…` a stock `rustc` ran two levels
/// inside a Trust checkout, under the very "running upstream 'rustc'" words MP-9 retired).
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
        Plan::Branded { .. } | Plan::Shim { .. } => {
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

/// The first upstream copy (`target`, from [`upstream_on_path`]), exec'd with
/// [`ANNOUNCED_ENV`] set so its own by-name spawns are not announced again — and are still
/// judged ([`plan`]). Never with [`PASSTHROUGH_ENV`]: that is the escape, and an allowed
/// Trust cargo handing it to its tree let a stock `rustc +1.97.1` beneath it run inside a
/// Trust checkout unjudged (2026-10-01).
fn exec_upstream(target: &Path, args: &[String]) -> ExitCode {
    // Raw: TRANSPARENT, so the caller's descriptors are the program's (B27's
    // allow-list). Its Windows console is decided here (`Console::Quiet`, B28).
    let mut command = std::process::Command::new(target);
    aterm_uds::spawn::set_flags(&mut command, aterm_uds::spawn::Console::Quiet, 0);
    command.args(args).env(ANNOUNCED_ENV, "1");
    let err = crate::platform::exec_or_run(&mut command);
    eprintln!("aterm: failed to exec {}: {err}", target.display());
    ExitCode::from(127)
}

/// The arguments a DIRECT row hands its branded tool.
///
/// Identity for every row without a [`SourceVerb`]. For a verb-first row it
/// inserts the verb ONLY when the caller's first argument is a source file of
/// the declared extension, so `lean f.lean` reaches `clean check f.lean` while
/// `lean --version`, `lean repl` and an explicit `lean check f.lean` are passed
/// through untouched. Pure, so the table's behaviour is testable without
/// spawning anything.
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

/// A branded tool's managed `copy` ([`managed`]), exec'd with the managed `bin/` appended
/// for its children and its shim's environment exported — `atpkg run`'s discipline.
fn exec_branded(layout: &Layout, copy: &Managed, args: &[String]) -> ExitCode {
    let child_path =
        crate::store::append_bin_to_path(std::env::var_os("PATH").as_deref(), &layout.bin_dir());
    // Raw and console-decided, as `exec_upstream`.
    let mut command = std::process::Command::new(&copy.target);
    aterm_uds::spawn::set_flags(&mut command, aterm_uds::spawn::Console::Quiet, 0);
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
