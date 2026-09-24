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
//! directory is not `bin/`.
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
//!   ANNOUNCE, DO NOT PREVENT. This row used to refuse (exit
//!   [`REFUSAL_EXIT`]) on the reading that "the friction is the feature". The
//!   owner's instruction of 2026-09-08 decides otherwise, verbatim: *"We don't
//!   want to prevent agents from using the vanilla rust toolchain, but we do
//!   want … some kind of printed message when using these tools that could be
//!   suppressed with a flag"* — and his 2026-09-07 ask, which this module was
//!   built to close, says *"intercept and loudly WARN"*, not refuse. The
//!   refusal came from a review, not from either instruction. What §4 actually
//!   withholds is SILENT substitution, which is why it already let
//!   `cargo +stable build` through with one loud line ([`passthrough_note`]);
//!   a bare `cargo build` that announces and then runs is no more silent than
//!   that one. The suppression the owner asked for is a SETTING, `[reroute]
//!   announce = false` in aterm.toml ([`crate::config::RerouteConfig`], Settings
//!   ▸ Packages) — owner, 2026-09-22: settings, "NOT ENV VARS those are for
//!   development". `ATERM_REROUTE_QUIET` is gone, and so is `ATERM_REROUTE_STRICT`,
//!   the opposite knob that restored the refusal: a second policy lane nobody in the
//!   tree set.
//! * **ORACLE** (`z3`): `ay`'s differential oracle. Refuse, naming the upstream
//!   copy's own path — running it by path reaches the real z3 (a path never meets a
//!   stub). This row is NOT covered by the announce-and-run ruling above: the
//!   owner's sentence is about "the vanilla rust toolchain", and running the wrong
//!   oracle does not merely skip a proof claim — it manufactures a false one, which is
//!   a different risk in kind. (`ATERM_Z3_IS_ORACLE`, the per-invocation consent, is
//!   gone: the path is the consent.)
//!
//! # Escapes, all of them
//!
//! * `aterm --no-reroute` starts a session with every upstream tool: it stamps the
//!   internal [`PASSTHROUGH_ENV`] marker on that session (the front door clears an
//!   inherited one otherwise, so it is protocol, not a knob). The stub itself walks
//!   PATH past its own directory and execs the first upstream copy — in `sh`, so the
//!   escape works even when atpkg is unreachable. The Rust side stamps the same marker
//!   on any upstream child it execs, so cargo's own `rustc`/`rustdoc` spawns are never
//!   re-announced or refused.
//! * `cargo +<toolchain>` naming a non-Trust toolchain is the user naming
//!   upstream deliberately: it runs, with one loud line, because §4 withholds
//!   only *silent* substitution. `+trust…` keeps the lane question and is
//!   signposted like a bare `cargo`.
//! * `rustup run <tc> cargo` never reaches these stubs (rustup resolves the
//!   toolchain's own binaries), and neither does an absolute path — which is why
//!   the compiler's own bootstrap, which drives stage0 by absolute path, stays
//!   silent. That is the intended boundary: the reroute answers a NAME looked up
//!   on PATH, nothing else.
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

use crate::store::Layout;

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
/// session), the stub and [`run`] read it, and [`exec_upstream`] stamps it on the
/// upstream child it execs so that child's own spawns pass silently. It was the user
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
/// Line 2 of every stub — the ONLY thing that makes a file ours to rewrite or
/// remove. Version-suffixed so a changed body can be told from an old one.
pub const STUB_MARKER: &str = "# atpkg reroute stub v1";
/// The hidden verb a stub execs: `atpkg __reroute <upstream> [args…]`.
pub const HIDDEN_VERB: &str = "__reroute";
/// An ORACLE refusal and the
/// fail-closed answer when atpkg itself cannot be reached: a usage-shaped exit
/// — the command did not run because the caller has to choose, not because
/// anything broke.
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
    /// Run `branded` with the caller's arguments; announce one line.
    /// `source_verb` is `Some` only when the branded tool is VERB-FIRST.
    Direct {
        branded: &'static str,
        source_verb: Option<SourceVerb>,
    },
    /// Refuse; name `branded` and every `lane` with the caller's arguments.
    /// An empty `lanes` means "one spelling, equivalence unproven" (`tlc`).
    Signpost {
        branded: &'static str,
        lanes: &'static [Lane],
    },
    /// Refuse; the upstream tool is `branded`'s differential oracle. Its own path
    /// reaches it (a path never meets a stub), and the refusal names that path.
    Oracle { branded: &'static str },
}

/// One upstream name and what a session does with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub upstream: &'static str,
    /// Which world the upstream name belongs to ("Rust", "TLA+", "SMT") — the
    /// signpost's first clause.
    pub family: &'static str,
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
/// proves nor builds. Anything else keeps both lanes.
pub const CARGO_VERB_REROUTES: &[(&str, &str)] = &[("clippy", "targo tippy"), ("fmt", "targo fmt")];

/// THE table. Order is the order doctor and `aterm help reroute` list it in.
pub const TABLE: &[Row] = &[
    Row {
        upstream: "clippy",
        family: "Rust",
        policy: Policy::Direct {
            branded: "tippy",
            source_verb: None,
        },
    },
    Row {
        upstream: "rustfmt",
        family: "Rust",
        policy: Policy::Direct {
            branded: "trustfmt",
            source_verb: None,
        },
    },
    Row {
        upstream: "rustdoc",
        family: "Rust",
        policy: Policy::Direct {
            branded: "trustdoc",
            source_verb: None,
        },
    },
    Row {
        upstream: "lean",
        family: "Lean",
        policy: Policy::Direct {
            branded: "clean",
            source_verb: Some(SourceVerb {
                verb: "check",
                ext: ".lean",
            }),
        },
    },
    Row {
        upstream: "tlc",
        family: "TLA+",
        policy: Policy::Signpost {
            branded: "ty",
            lanes: &[],
        },
    },
    Row {
        upstream: "rustc",
        family: "Rust",
        policy: Policy::Signpost {
            branded: "trustc",
            lanes: RUSTC_LANES,
        },
    },
    Row {
        upstream: "cargo",
        family: "Rust",
        policy: Policy::Signpost {
            branded: "targo",
            lanes: CARGO_LANES,
        },
    },
    Row {
        upstream: "z3",
        family: "SMT",
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
#[must_use]
pub fn explicit_toolchain(args: &[String]) -> Option<&str> {
    let first = args.first()?;
    let toolchain = first.strip_prefix('+')?;
    (!toolchain.is_empty()).then_some(toolchain)
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

/// The last lines of a SIGNPOST message: what is about to happen — upstream runs, and
/// what it produces carries no proof claim — and the setting that silences the line,
/// because a line printed on every build must say how to stop printing it.
fn signpost_closing(upstream: &str) -> String {
    // The measuring page is named on every signpost (owner, 2026-09-08: the
    // Trust default is to be "very strongly encouraged by the aterm system
    // itself"): a reader who is about to run stock cargo anyway is told, in
    // the same breath, the one command that answers which toolchain this
    // directory actually gets — instead of guessing from the two lines above.
    format!(
        "       Running upstream '{upstream}' now — nothing it produces carries a proof claim.\n       `aterm help rust` measures which toolchain THIS directory gets; the default here is Trust.\n       ({ANNOUNCE_SETTING} in aterm.toml silences this — Settings ▸ Packages.)"
    )
}

/// The DIRECT row's one line.
#[must_use]
pub fn direct_announcement(upstream: &str, family: &str, branded: &str) -> String {
    format!(
        "aterm: '{upstream}' is the {family} name; on Trust the tool is '{branded}' — running {branded}. {}",
        escape_clause(upstream)
    )
}

/// The SIGNPOST announcement, the caller's arguments filled into every lane:
///
/// ```text
/// aterm: 'cargo' is the Rust name; on Trust the tool is 'targo'. Trust will not
///        pick a verification lane for you:
///          targo trust build --release          VERIFIED   — emits a proof claim
///          targo --unverified build --release   UNVERIFIED — no proof claim
///        Running upstream 'cargo' now — nothing it produces carries a proof claim.
///        `aterm help rust` measures which toolchain THIS directory gets; the default here is Trust.
///        ([reroute] announce = false in aterm.toml silences this — Settings ▸ Packages.)
/// ```
#[must_use]
pub fn signpost_message(row: &Row, lanes: &[Lane], args: &[String]) -> String {
    let upstream = row.upstream;
    let branded = branded_of(row);
    let rest = args.join(" ");
    let mut out = String::new();
    // `cargo clippy` / `cargo fmt`: one spelling, no lane.
    if let Some((verb, reroute)) = args
        .first()
        .and_then(|verb| CARGO_VERB_REROUTES.iter().find(|(v, _)| v == verb))
        .filter(|_| upstream == "cargo")
    {
        let tail = args[1..].join(" ");
        out.push_str(&format!(
            "aterm: '{upstream} {verb}' is the {} name; on Trust the tool is '{reroute}':\n",
            row.family
        ));
        out.push_str(&format!("         {}\n", join_command(reroute, &tail)));
        out.push_str(&signpost_closing(upstream));
        return out;
    }
    if lanes.is_empty() {
        out.push_str(&format!(
            "aterm: '{upstream}' is the {} name; on this toolchain the tool is '{branded}', and\n",
            row.family
        ));
        out.push_str("       drop-in equivalence is not yet proven, so nothing is substituted:\n");
        out.push_str(&format!("         {}\n", join_command(branded, &rest)));
        out.push_str(&signpost_closing(upstream));
        return out;
    }
    out.push_str(&format!(
        "aterm: '{upstream}' is the {} name; on Trust the tool is '{branded}'. Trust will not\n",
        row.family
    ));
    out.push_str("       pick a verification lane for you:\n");
    let commands: Vec<String> = lanes
        .iter()
        .map(|lane| join_command(lane.command, &rest))
        .collect();
    let width = commands.iter().map(String::len).max().unwrap_or(0) + 3;
    let label_width = lanes.iter().map(|lane| lane.label.len()).max().unwrap_or(0);
    for (lane, command) in lanes.iter().zip(&commands) {
        out.push_str(&format!(
            "         {command:<width$}{:<label_width$} — {}\n",
            lane.label, lane.note
        ));
    }
    out.push_str(&signpost_closing(upstream));
    out
}

/// The ORACLE refusal, naming the upstream copy's own path when there is one: a path
/// never meets a stub, so it is the consent to run the real tool.
#[must_use]
pub fn oracle_message(upstream: &str, branded: &str, upstream_path: Option<&Path>) -> String {
    let reach = match upstream_path {
        Some(path) => format!("run {} to reach the real {upstream}", path.display()),
        None => format!("no upstream '{upstream}' is on PATH"),
    };
    format!(
        "aterm: '{upstream}' is the oracle '{branded}' is measured against, so a session never runs it by name: {reach} (`aterm --no-reroute` starts a session with every upstream tool)."
    )
}

/// `cargo trust <verb>` / `cargo --unverified <verb>`: the user named the lane
/// in cargo's own vocabulary (cargo and targo are one multicall binary, and
/// `cargo trust check` is documented as targo's strict driver). Nothing is
/// silent, so nothing is refused: the branded tool runs, with one line.
pub const EXPLICIT_LANE_VERBS: &[&str] = &["trust", "--unverified"];

/// The one line printed when `cargo trust …`/`cargo --unverified …` runs `targo`.
#[must_use]
pub fn explicit_lane_note(upstream: &str, branded: &str, lane: &str) -> String {
    format!(
        "aterm: '{upstream} {lane}' names the lane in the Rust spelling; on Trust the tool is '{branded}' — running {branded} {lane}. {}",
        escape_clause(upstream)
    )
}

/// The one line printed when `cargo +<tc>` names a non-Trust toolchain and runs.
#[must_use]
pub fn passthrough_note(upstream: &str, toolchain: &str) -> String {
    format!(
        "aterm: '{upstream} +{toolchain}' names an upstream toolchain explicitly — running upstream '{upstream}'; nothing it produces is verified. (The Trust lane is 'targo trust <verb>'; a session started with `aterm --no-reroute` prints no note.)"
    )
}

/// Printed by the stub itself when atpkg is unreachable — fail CLOSED, with the
/// escape named, never a silent fall-through to upstream.
#[must_use]
pub fn unreachable_message(upstream: &str) -> String {
    format!(
        "aterm: '{upstream}' is rerouted inside aterm sessions, but aterm's package manager is not reachable to say where; `aterm --no-reroute` restores upstream '{upstream}'"
    )
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
/// one "which copy runs and why" surface).
#[must_use]
pub fn policy_summary(row: &Row) -> String {
    let upstream = row.upstream;
    let escape = escape_clause(upstream);
    match row.policy {
        Policy::Direct {
            branded,
            source_verb,
        } => match source_verb {
            Some(SourceVerb { verb, ext }) => format!(
                "runs '{branded} <args>' with one stderr line ('{branded} {verb} <file>' for a bare `{ext}` file) {escape}"
            ),
            None => format!("runs '{branded} <args>' with one stderr line {escape}"),
        },
        Policy::Signpost { branded, lanes: [] } => format!(
            "announced, naming '{branded} <args>' (drop-in equivalence is not yet proven), then run upstream {escape}"
        ),
        Policy::Signpost { lanes, .. } => {
            let named: Vec<String> = lanes
                .iter()
                .map(|lane| format!("'{} <args>'", lane.command))
                .collect();
            let mut s = format!("announced, naming {}", named.join(" / "));
            if upstream == "cargo" {
                let verbs: Vec<String> = CARGO_VERB_REROUTES
                    .iter()
                    .map(|(verb, reroute)| format!("'{upstream} {verb}' → '{reroute}'"))
                    .collect();
                s.push_str("; ");
                s.push_str(&verbs.join(", "));
            }
            s.push_str(", then run upstream ");
            s.push_str(&escape);
            s
        }
        Policy::Oracle { branded } => format!(
            "refused ('{branded}' is measured against it); its own path reaches the real {upstream} {escape}"
        ),
    }
}

// ── the stub ────────────────────────────────────────────────────────────────

/// The POSIX stub for `upstream`. Its shape is load-bearing:
///
/// * line 2 is [`STUB_MARKER`] (recognition);
/// * every `exec` goes through a VARIABLE, never the literal `exec '` a managed
///   shim carries, so the shim parser sees no target (a stub is not an install);
/// * the escape hatch is decided IN `sh`, before atpkg is consulted, walking
///   `PATH` past EVERY reroute directory (by [`DIR_MARKER_FILE`], under any
///   spelling and any prefix — never just the one it was laid in) for the
///   first absolute-entry executable of the same name — the escape works with
///   no atpkg at all;
/// * with atpkg unreachable it fails closed with the escape named (exit 2),
///   never quietly running upstream.
#[must_use]
pub fn stub_body_sh(upstream: &str, atpkg: &Path, reroute_dir: &Path) -> String {
    let name = crate::stub::sh_single_quote(upstream);
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
    s.push_str("\nif [ -n \"${");
    s.push_str(PASSTHROUGH_ENV);
    s.push_str(":-}\" ] && [ \"${");
    s.push_str(PASSTHROUGH_ENV);
    s.push_str(":-}\" != \"0\" ]; then\n");
    // Pure shell, no subshell and no external command: this branch runs BEFORE
    // anything is resolved, on whatever PATH the caller has (a synthetic one in
    // tests has no `tr`). `IFS=:` splits PATH; `set -f` keeps a `*` in an entry
    // from globbing; the parameter test says "absolute" without a `case`, which
    // macOS /bin/sh (bash 3.2) mis-parses inside `$(…)`.
    s.push_str("  __aterm_ifs=$IFS; IFS=:; set -f\n");
    s.push_str("  for __d in $PATH; do\n");
    s.push_str("    [ -z \"$__d\" ] && continue\n");
    s.push_str("    [ \"${__d#/}\" = \"$__d\" ] && continue\n");
    // By MARKER FILE, not by string equality with our own directory: a second
    // prefix's reroute dir, a symlink to ours, or ours with a trailing slash
    // is still a reroute dir, and exec'ing its stub under the escape looped
    // forever (measured in the 2026-09-07 review).
    s.push_str("    [ -f \"$__d/");
    s.push_str(DIR_MARKER_FILE);
    s.push_str("\" ] && continue\n");
    s.push_str("    if [ -x \"$__d/$__aterm_name\" ] && [ ! -d \"$__d/$__aterm_name\" ]; then\n");
    s.push_str("      IFS=$__aterm_ifs; set +f\n");
    s.push_str("      exec \"$__d/$__aterm_name\" \"$@\"\n");
    s.push_str("    fi\n");
    s.push_str("  done\n");
    s.push_str("  IFS=$__aterm_ifs; set +f\n");
    s.push_str("  printf '%s\\n' \"aterm: upstream '$__aterm_name' is not on PATH\" 1>&2\n");
    s.push_str("  exit 127\nfi\n");
    s.push_str("ATPKG=");
    s.push_str(&crate::stub::sh_single_quote(&atpkg.to_string_lossy()));
    s.push_str("\nif [ -x \"$ATPKG\" ]; then exec \"$ATPKG\" ");
    s.push_str(HIDDEN_VERB);
    s.push_str(" \"$__aterm_name\" \"$@\"; fi\n");
    s.push_str("if command -v atpkg >/dev/null 2>&1; then exec atpkg ");
    s.push_str(HIDDEN_VERB);
    s.push_str(" \"$__aterm_name\" \"$@\"; fi\n");
    s.push_str("printf '%s\\n' ");
    s.push_str(&crate::stub::sh_single_quote(&unreachable_message(
        upstream,
    )));
    s.push_str(" 1>&2\nexit ");
    s.push_str(&crate::dec_u64(u64::from(REFUSAL_EXIT)));
    s.push('\n');
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
///   trailing slash is skipped too) and not this stub under another name (`-ef "$0"`: a
///   link to it in `~/bin` would exec it forever) — else exit 127 with one line. Outside
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
/// every reroute directory and this stub under any other name. Line 2 is [`STUB_MARKER`], so every recognition, sweep and
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
    // Itself under another name: a symlink or hard link to this stub in an ordinary PATH
    // directory would otherwise exec it again, forever.
    s.push_str("  [ \"$__d/$__aterm_name\" -ef \"$0\" ] && continue\n");
    s.push_str("  if [ -x \"$__d/$__aterm_name\" ] && [ ! -d \"$__d/$__aterm_name\" ]; then\n");
    s.push_str("    IFS=$__aterm_ifs; set +f\n");
    s.push_str("    exec \"$__d/$__aterm_name\" \"$@\"\n");
    s.push_str("  fi\n");
    s.push_str("done\n");
    s.push_str("IFS=$__aterm_ifs; set +f\n");
    s.push_str("printf '%s\\n' \"");
    s.push_str(&agents_not_found_message("$__aterm_name"));
    s.push_str("\" 1>&2\nexit 127\n");
    s
}

/// The agents stub's one line when nothing is left to run: no copy on PATH outside aterm's
/// own directories, and the managed twin not taken. `name` is spliced into a double-quoted
/// `sh` string (the stub passes `$__aterm_name`), so the text holds no `"`, `$`, `` ` ``
/// or `\` of its own.
#[must_use]
pub fn agents_not_found_message(name: &str) -> String {
    format!(
        "aterm: no '{name}' on PATH outside aterm's reroute and agents directories, and the managed copy is taken only inside an aterm session, when installed (aterm help reroute)"
    )
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
/// marker file. What both walks (the stub's `sh` and [`exec_upstream`]) skip.
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
fn wanted_stubs(layout: &Layout) -> Vec<(&'static str, String)> {
    let dir = dir(layout);
    let atpkg = crate::stub::embedded_atpkg_path();
    let agents = layout.agents_dir();
    TABLE
        .iter()
        .map(|row| (row.upstream, stub_body_sh(row.upstream, &atpkg, &dir)))
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
/// A FILE IS WRITTEN ONLY WHEN ITS BYTES DIFFER (Phase 3, 2026-09-22). An identical stub
/// that carries `com.apple.provenance` used to be re-laid through the untracked launchd
/// lane on every pass, and when the lane's file came back tagged too the loop never
/// converged: the marker was re-laid, and a note printed, on every pass from 2026-09-19 to
/// 2026-09-22. Clearing the tag is `aterm pkg repair`'s job ([`relay_tagged`]), the fix
/// `aterm pkg doctor` names; a pass or a spawn changes only what is wrong in content.
pub fn lay(layout: &Layout) -> io::Result<()> {
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
    // THE MARKER FIRST, and in-process: it is data the walks test with `-f`, never an
    // executable, so its tag tracks nothing and it needs no lane — and a walk racing this
    // `lay` recognizes the directory before the first stub lands in it.
    let marker = dir.join(DIR_MARKER_FILE);
    if marker_needs_lay(std::fs::read(&marker).ok().as_deref()) {
        crate::lay::write_in_process(&crate::lay::Executable::new(&marker, marker_body()))?;
    }
    // Every stub whose bytes differ, rendered first and laid in ONE job
    // ([`crate::lay::lay_executables`]: in-process, or through the untracked launchd job
    // when this process is provenance-tracked — these stubs run every upstream
    // `cargo`/`rustc` typed in a session, and a tagged one would track them all).
    let mut files = Vec::new();
    let wanted = wanted_stubs(layout);
    for (name, body) in &wanted {
        let path = dir.join(name);
        match std::fs::symlink_metadata(&path) {
            Err(_) => {} // absent: ours to claim
            // Ours already, and byte-identical: nothing to lay. A body that differs — the
            // embedded atpkg path moved with a relocation or a self-update — is rewritten.
            Ok(_) if is_reroute_stub(&path) => {
                if std::fs::read(&path).is_ok_and(|have| have == body.as_bytes()) {
                    continue;
                }
            }
            Ok(_) => continue, // foreign: never touched
        }
        files.push(crate::lay::Executable::new(&path, body.as_bytes()));
    }
    crate::lay::lay_executables(&files)?;
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
/// unreadable): only when it is absent or its bytes differ — never for a tag, which on a
/// file nothing executes tracks nothing (the reason it is written in-process at all).
pub(crate) fn marker_needs_lay(have: Option<&[u8]>) -> bool {
    have != Some(marker_body().as_bytes())
}

/// `aterm pkg repair`'s half of [`lay`]: re-lay every stub of ours that is current in
/// content but carries `com.apple.provenance`, when a lay from this process would come
/// back clean ([`crate::lay::lay_clears_provenance`]) — the one reason to rewrite
/// identical bytes. Returns how many were re-laid.
///
/// # Errors
/// The lay's own.
pub fn relay_tagged(layout: &Layout) -> io::Result<usize> {
    relay_tagged_where(layout, |_| crate::lay::lay_clears_provenance())
}

/// The passes' half: [`relay_tagged`] once per file — a stub the lane already re-laid and
/// that came back tagged is not asked again ([`crate::lay::relay_worth_trying`]), so a pass
/// clears a stub a fallback lay tagged without re-laying the same bytes for ever.
///
/// # Errors
/// The lay's own.
pub fn relay_tagged_once(layout: &Layout) -> io::Result<usize> {
    relay_tagged_where(layout, |path| crate::lay::relay_worth_trying(layout, path))
}

/// Re-lay every tagged, current stub of ours `worth` admits, then remember which came back
/// tagged ([`crate::lay::note_relayed`]).
fn relay_tagged_where(layout: &Layout, worth: impl Fn(&Path) -> bool) -> io::Result<usize> {
    if cfg!(windows) || layout.declined().is_file() {
        return Ok(0);
    }
    let dir = dir(layout);
    let tagged: Vec<crate::lay::Executable> = wanted_stubs(layout)
        .into_iter()
        .map(|(name, _)| dir.join(name))
        .filter(|path| is_reroute_stub(path) && crate::provenance::carries_provenance(path))
        .filter(|path| worth(path))
        .filter_map(|path| {
            std::fs::read(&path)
                .ok()
                .map(|body| crate::lay::Executable::new(&path, body))
        })
        .collect();
    if tagged.is_empty() {
        return Ok(0);
    }
    crate::lay::lay_executables(&tagged)?;
    let paths: Vec<std::path::PathBuf> = tagged.iter().map(|e| e.path.clone()).collect();
    crate::lay::note_relayed(layout, &paths);
    Ok(tagged.len())
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
/// exec'd tool's, [`REFUSAL_EXIT`] for a refusal, 127 when a tool could not run.
///
/// A SIGNPOST row announces (unless `[reroute] announce = false`) and then execs
/// UPSTREAM, so its exit code is the upstream tool's. ORACLE refuses.
pub fn run(layout: &Layout, upstream: &str, args: &[String]) -> ExitCode {
    let Some(row) = row_for(upstream) else {
        eprintln!("atpkg {HIDDEN_VERB}: '{upstream}' is not a rerouted name");
        return ExitCode::from(REFUSAL_EXIT);
    };
    // Belt and braces: the stub decides this first, but `aterm <upstream>` and a
    // direct `atpkg __reroute` call arrive here without it.
    if engaged(std::env::var(PASSTHROUGH_ENV).ok().as_deref()) {
        return exec_upstream(layout, upstream, args);
    }
    match row.policy {
        Policy::Direct {
            branded,
            source_verb,
        } => {
            eprintln!("{}", direct_announcement(upstream, row.family, branded));
            exec_branded(layout, upstream, branded, &direct_args(source_verb, args))
        }
        Policy::Signpost { branded, lanes } => {
            if let Some(toolchain) =
                explicit_toolchain(args).filter(|toolchain| !toolchain.starts_with("trust"))
            {
                eprintln!("{}", passthrough_note(upstream, toolchain));
                return exec_upstream(layout, upstream, args);
            }
            // `+trust…` keeps the lane question but must not reach the rendered
            // commands: the managed `targo` is not a rustup proxy and rejects
            // a `+toolchain` directive, and a fix the user cannot paste is no fix.
            let args = if explicit_toolchain(args).is_some() {
                &args[1..]
            } else {
                args
            };
            if upstream == "cargo"
                && let Some(lane) = args
                    .first()
                    .filter(|verb| EXPLICIT_LANE_VERBS.contains(&verb.as_str()))
            {
                eprintln!("{}", explicit_lane_note(upstream, branded, lane));
                return exec_branded(layout, upstream, branded, args);
            }
            // ANNOUNCE, THEN RUN — the 2026-09-08 ruling. `[reroute] announce =
            // false` (the owner's "suppressed with a flag", a setting since
            // 2026-09-23) drops the line but never the run.
            if crate::config::cached_reroute().announce() {
                eprintln!("{}", signpost_message(row, lanes, args));
            }
            exec_upstream(layout, upstream, args)
        }
        Policy::Oracle { branded } => {
            let path_var = std::env::var_os("PATH");
            let upstream_path = upstream_on_path(layout, upstream, path_var.as_deref());
            eprintln!(
                "{}",
                oracle_message(upstream, branded, upstream_path.as_deref())
            );
            ExitCode::from(REFUSAL_EXIT)
        }
    }
}

/// The first upstream copy of `name` on PATH: an absolute entry that is not a
/// reroute directory (by marker — ANY spelling, any prefix), not under this
/// manager prefix, holding an executable regular file that is not itself a
/// reroute stub. `vendor::executable_on_path` skips only THIS prefix, which is
/// why a nested session under another prefix used to hand the escape to the
/// enclosing session's stub and loop.
#[must_use]
pub fn upstream_on_path(layout: &Layout, name: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    let path_var = path_var?;
    let prefix_real =
        std::fs::canonicalize(&layout.prefix).unwrap_or_else(|_| layout.prefix.clone());
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() || !dir.is_absolute() || is_reroute_dir(&dir) {
            continue;
        }
        let under_prefix = dir.starts_with(&layout.prefix)
            || std::fs::canonicalize(&dir).is_ok_and(|real| real.starts_with(&prefix_real));
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
        if !executable || is_reroute_stub(&candidate) {
            continue;
        }
        return Some(candidate);
    }
    None
}

/// The first upstream copy (see [`upstream_on_path`]), exec'd with
/// [`PASSTHROUGH_ENV`] set so its own spawns pass silently.
fn exec_upstream(layout: &Layout, upstream: &str, args: &[String]) -> ExitCode {
    let path_var = std::env::var_os("PATH");
    let Some(target) = upstream_on_path(layout, upstream, path_var.as_deref()) else {
        eprintln!("aterm: upstream '{upstream}' is not on PATH outside the reroute directories");
        return ExitCode::from(127);
    };
    let mut command = std::process::Command::new(&target);
    command.args(args).env(PASSTHROUGH_ENV, "1");
    let err = crate::platform::exec_or_run(&mut command);
    eprintln!("aterm: failed to exec {}: {err}", target.display());
    ExitCode::from(127)
}

/// A DIRECT row: the managed copy of `branded`, through the store (never PATH),
/// with the managed `bin/` appended for its children — `atpkg run`'s discipline.
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

fn exec_branded(layout: &Layout, upstream: &str, branded: &str, args: &[String]) -> ExitCode {
    // `exec_path`, not `which`: this execs the tool without its shim, so it has to take
    // the exec root the shim's guard would (`crate::compat` — a trust build whose tippy
    // refuses the store's own `bin/rustc` copy lints only from there).
    let Some(target) = crate::ops::exec_path(layout, branded) else {
        eprintln!(
            "aterm: '{upstream}' reroutes to '{branded}', which is not installed here — opening aterm provisions the toolset (`aterm pkg install <program>` for one program); `aterm --no-reroute` restores upstream '{upstream}'."
        );
        return ExitCode::from(127);
    };
    let child_path =
        crate::store::append_bin_to_path(std::env::var_os("PATH").as_deref(), &layout.bin_dir());
    let mut command = std::process::Command::new(&target);
    command.args(args).env("PATH", child_path);
    // …and what the shim would export before its `exec` (`ops::exec_env`, design S7) —
    // `atpkg run`'s rule, so a branded program whose policy declares an environment gets
    // it by either door.
    for (name, value) in crate::ops::exec_env(layout, branded).entries() {
        command.env(name, value);
    }
    let err = crate::platform::exec_or_run(&mut command);
    eprintln!("aterm: failed to exec {}: {err}", target.display());
    ExitCode::from(127)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-reroute-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// `lean f.lean` must reach `clean check f.lean` — the branded tool is
    /// verb-first, so plain passthrough spelled `clean f.lean` and died on
    /// "unrecognized subcommand". Every other shape stays byte-identical: a
    /// flag, one of `clean`'s OWN subcommands (the reason this is keyed on the
    /// extension and not on "not a flag"), an already-explicit verb, and every
    /// DIRECT row that declares no `SourceVerb`.
    #[test]
    fn a_verb_first_direct_row_routes_a_bare_source_file_through_its_verb() {
        let lean = row_for("lean").expect("lean is a rerouted name");
        let Policy::Direct { source_verb, .. } = lean.policy else {
            panic!("lean is a DIRECT row");
        };
        let sv = source_verb.expect("lean declares a source verb");

        assert_eq!(
            direct_args(source_verb, &args(&["f.lean"])),
            args(&["check", "f.lean"])
        );
        assert_eq!(
            direct_args(source_verb, &args(&["a/b/Main.lean", "--json"])),
            args(&["check", "a/b/Main.lean", "--json"])
        );
        // Untouched: a flag, one of clean's own subcommands, an explicit verb.
        for passthrough in [
            vec!["--version"],
            vec!["repl"],
            vec!["check", "f.lean"],
            vec![],
        ] {
            assert_eq!(
                direct_args(source_verb, &args(&passthrough)),
                args(&passthrough),
                "{passthrough:?} must pass through unchanged"
            );
        }
        assert_eq!(sv.verb, "check");
        assert_eq!(sv.ext, ".lean");

        // A row with no source verb is pure identity.
        let clippy = row_for("clippy").expect("clippy is a rerouted name");
        let Policy::Direct {
            source_verb: none, ..
        } = clippy.policy
        else {
            panic!("clippy is a DIRECT row");
        };
        assert!(none.is_none());
        assert_eq!(direct_args(none, &args(&["x.rs"])), args(&["x.rs"]));
    }

    /// The table is the policy: no name twice, every branded target a name the
    /// managed `bin/` may carry, and the three names the deny-list refuses STILL
    /// refused — the reroute dir is not a loophole into `bin/`.
    #[test]
    fn table_is_unique_branded_targets_are_managed_names_and_sensitive_names_stay_sensitive() {
        let mut seen = std::collections::BTreeSet::new();
        for row in TABLE {
            assert!(seen.insert(row.upstream), "duplicate row {}", row.upstream);
            assert!(
                crate::store::ToolName::new(branded_of(row)).is_some(),
                "{} is not a name the managed bin/ may carry",
                branded_of(row)
            );
        }
        for sensitive in ["cargo", "rustc", "rustup"] {
            assert!(
                crate::store::ToolName::new(sensitive).is_none(),
                "{sensitive} left the deny-list"
            );
        }
        assert!(
            row_for("rustup").is_none(),
            "rustup is not rerouted: `rustup run trust` is the sanctioned spelling"
        );
    }

    #[test]
    fn engaged_is_non_empty_and_not_zero() {
        assert!(!engaged(None));
        assert!(!engaged(Some("")));
        assert!(!engaged(Some("0")));
        assert!(engaged(Some("1")));
        assert!(engaged(Some("yes")));
    }

    #[test]
    fn explicit_toolchain_is_only_a_leading_plus_argument() {
        assert_eq!(
            explicit_toolchain(&args(&["+stable", "build"])),
            Some("stable")
        );
        assert_eq!(explicit_toolchain(&args(&["build", "+stable"])), None);
        assert_eq!(explicit_toolchain(&args(&["+"])), None);
        assert_eq!(explicit_toolchain(&[]), None);
    }

    #[test]
    fn signpost_fills_the_callers_arguments_into_both_lanes() {
        let row = row_for("cargo").unwrap();
        let Policy::Signpost { lanes, .. } = row.policy else {
            panic!()
        };
        let text = signpost_message(row, lanes, &args(&["build", "--release"]));
        assert!(
            text.starts_with("aterm: 'cargo' is the Rust name; on Trust the tool is 'targo'."),
            "{text}"
        );
        assert!(text.contains("targo trust build --release"), "{text}");
        assert!(
            text.contains("targo --unverified build --release"),
            "{text}"
        );
        assert!(text.contains("VERIFIED   — emits a proof claim"), "{text}");
        // Every announce-and-run signpost names the page that MEASURES the
        // answer, and says the default (owner, 2026-09-08).
        assert!(
            text.contains("`aterm help rust` measures which toolchain THIS directory gets"),
            "{text}"
        );
        assert!(text.contains("the default here is Trust"), "{text}");
        assert!(text.contains("UNVERIFIED — no proof claim"), "{text}");
        // ANNOUNCE, NOT REFUSE: the default says upstream is about to run and
        // names the one setting that silences it — never an environment variable.
        assert!(
            text.contains(
                "Running upstream 'cargo' now — nothing it produces carries a proof claim."
            ),
            "{text}"
        );
        assert!(
            text.ends_with(
                "([reroute] announce = false in aterm.toml silences this — Settings ▸ Packages.)"
            ),
            "{text}"
        );
        assert!(
            !text.contains("ATERM_"),
            "no environment knob is taught: {text}"
        );
        // No arguments: the bare commands, no trailing space.
        let bare = signpost_message(row, lanes, &[]);
        assert!(bare.contains("targo trust   "), "{bare}");
        assert!(!bare.contains("targo trust  \n"), "{bare}");
    }

    /// THE 2026-09-08 RULING, AT THE DECISION POINT.
    ///
    /// The owner: *"We don't want to prevent agents from using the vanilla rust
    /// toolchain, but we do want … some kind of printed message when using
    /// these tools that could be suppressed with a flag"* — and on 2026-09-22 that a
    /// person's controls are settings, "NOT ENV VARS". So the default must RUN, and
    /// `[reroute] announce = false` silences the line WITHOUT refusing: `run` reads the
    /// setting in exactly one place, and no environment knob remains.
    #[test]
    fn a_signpost_announces_and_runs_and_a_setting_silences_it() {
        let src = include_str!("reroute.rs");
        let body = &src[src.find("pub fn run(").expect("run")..];
        let body = &body[..body.find("\n}\n").expect("run's end")];
        assert!(
            body.contains("crate::config::cached_reroute().announce()"),
            "the announcement is gated on the setting"
        );
        assert!(
            !body.contains("REFUSAL_EXIT") || body.matches("REFUSAL_EXIT").count() == 2,
            "only the unknown-name and ORACLE arms refuse: a signpost always runs"
        );
        for retired in [
            "ATERM_REROUTE_QUIET",
            "ATERM_REROUTE_STRICT",
            "ATERM_Z3_IS_ORACLE",
        ] {
            assert!(!body.contains(retired), "{retired} is no longer read");
        }
        assert!(
            crate::config::RerouteConfig::default().announce(),
            "default: announce"
        );
    }

    #[test]
    fn cargo_clippy_and_fmt_are_one_spelling_with_no_lane() {
        let row = row_for("cargo").unwrap();
        let Policy::Signpost { lanes, .. } = row.policy else {
            panic!()
        };
        let text = signpost_message(
            row,
            lanes,
            &args(&["clippy", "--workspace", "--", "-D", "warnings"]),
        );
        assert!(
            text.contains("'cargo clippy' is the Rust name; on Trust the tool is 'targo tippy'"),
            "{text}"
        );
        assert!(
            text.contains("targo tippy --workspace -- -D warnings"),
            "{text}"
        );
        assert!(!text.contains("targo trust"), "{text}");
        let text = signpost_message(row, lanes, &args(&["fmt", "--check"]));
        assert!(text.contains("targo fmt --check"), "{text}");
    }

    #[test]
    fn rustc_names_both_lanes_and_tlc_names_one_tool_without_claiming_equivalence() {
        let rustc = row_for("rustc").unwrap();
        let Policy::Signpost { lanes, .. } = rustc.policy else {
            panic!()
        };
        let text = signpost_message(rustc, lanes, &args(&["main.rs"]));
        assert!(text.contains("trustc main.rs"), "{text}");
        assert!(text.contains("trustc -Ztrust-verify=off main.rs"), "{text}");
        let tlc = row_for("tlc").unwrap();
        let Policy::Signpost { lanes, .. } = tlc.policy else {
            panic!()
        };
        let text = signpost_message(tlc, lanes, &args(&["Spec.tla"]));
        assert!(
            text.contains("drop-in equivalence is not yet proven"),
            "{text}"
        );
        assert!(text.contains("\n         ty Spec.tla\n"), "{text}");
    }

    #[test]
    fn direct_oracle_and_passthrough_are_one_line_each_and_name_the_escape() {
        for text in [
            direct_announcement("rustfmt", "Rust", "trustfmt"),
            oracle_message("z3", "ay", Some(Path::new("/opt/homebrew/bin/z3"))),
            oracle_message("z3", "ay", None),
            passthrough_note("cargo", "stable"),
            unreachable_message("cargo"),
        ] {
            assert_eq!(text.lines().count(), 1, "{text}");
            assert!(text.starts_with("aterm: "), "{text}");
            assert!(text.contains("aterm --no-reroute"), "{text}");
            assert!(
                !text.contains("ATERM_"),
                "no environment knob is taught: {text}"
            );
        }
        assert!(
            oracle_message("z3", "ay", Some(Path::new("/opt/homebrew/bin/z3")))
                .contains("run /opt/homebrew/bin/z3 to reach the real z3"),
            "the ORACLE refusal names the path that is the consent"
        );
    }

    /// A second `lay` may be in flight: its `.<name>.stub-<pid>` temp and the
    /// marker file are dotfiles, and dotfiles are never swept.
    #[test]
    fn the_sweep_never_touches_dotfiles() {
        let l = layout("dotfiles");
        lay(&l).unwrap();
        let d = dir(&l);
        std::fs::write(
            d.join(".cargo.stub-99999"),
            format!("#!/bin/sh\n{STUB_MARKER}\nexit 2\n"),
        )
        .unwrap();
        lay(&l).unwrap();
        assert!(
            d.join(".cargo.stub-99999").exists(),
            "a concurrent lay's temp was swept"
        );
        assert!(is_reroute_dir(&d), "the marker file was swept");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The stub is invisible to every managed-shim sweep and carries its marker.
    #[test]
    fn stub_body_is_marked_and_never_parses_as_a_managed_shim() {
        let body = stub_body_sh(
            "cargo",
            Path::new("/opt/aterm/atpkg"),
            Path::new("/p/reroute"),
        );
        assert_eq!(body.lines().nth(1), Some(STUB_MARKER));
        assert!(
            crate::platform::parse_sh_shim_target(&body).is_none(),
            "{body}"
        );
        assert!(!body.contains("exec '"), "{body}");
        assert!(
            body.contains("__reroute \"$__aterm_name\" \"$@\""),
            "{body}"
        );
        assert!(body.contains(PASSTHROUGH_ENV), "{body}");
        assert!(body.ends_with("exit 2\n"), "{body}");
    }

    /// The marker is written only when absent or different in content — never for a tag
    /// (Phase 3: the tag-driven re-lay through the launchd lane never converged, and the
    /// marker is data nothing executes).
    #[test]
    fn the_marker_is_written_only_when_its_bytes_differ() {
        assert!(marker_needs_lay(None), "absent: laid");
        assert!(
            !marker_needs_lay(Some(marker_body().as_bytes())),
            "identical: left"
        );
        assert!(
            marker_needs_lay(Some(b"# something else\n")),
            "different: rewritten"
        );
    }

    /// A second `lay` over a laid directory writes NOTHING: every stub and the marker keep
    /// their inodes, whatever tag they carry — the steady-state pass and every session
    /// spawn converge.
    #[cfg(unix)]
    #[test]
    fn a_second_lay_writes_nothing() {
        use std::os::unix::fs::MetadataExt as _;
        let l = layout("idempotent");
        lay(&l).unwrap();
        let d = dir(&l);
        let inodes = |d: &Path| -> Vec<(String, u64)> {
            let mut v: Vec<(String, u64)> = std::fs::read_dir(d)
                .unwrap()
                .flatten()
                .map(|e| {
                    (
                        e.file_name().to_string_lossy().into_owned(),
                        e.metadata().unwrap().ino(),
                    )
                })
                .collect();
            v.sort();
            v
        };
        let before = inodes(&d);
        assert!(
            before.iter().any(|(n, _)| n == DIR_MARKER_FILE),
            "{before:?}"
        );
        lay(&l).unwrap();
        assert_eq!(inodes(&d), before, "nothing was rewritten");
        // A stub whose body drifted is rewritten, and only it.
        let cargo = d.join("cargo");
        let body = std::fs::read_to_string(&cargo).unwrap();
        std::fs::write(&cargo, body.replace("ATPKG=", "ATPKG_OLD=")).unwrap();
        let drifted = inodes(&d);
        lay(&l).unwrap();
        let after = inodes(&d);
        for ((name, was), (_, now)) in drifted.iter().zip(&after) {
            assert_eq!(name == "cargo", was != now, "{name}");
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn lay_claims_absent_and_own_files_never_foreign_ones_and_remove_all_sweeps_only_ours() {
        let l = layout("lay");
        let d = dir(&l);
        std::fs::create_dir_all(&d).unwrap();
        // A foreign occupant and a stale stub of ours for a name that left the table.
        std::fs::write(d.join("rustc"), "#!/bin/sh\necho someone else's\n").unwrap();
        std::fs::write(
            d.join("gone"),
            format!("#!/bin/sh\n{STUB_MARKER}\nexit 2\n"),
        )
        .unwrap();
        lay(&l).unwrap();
        for row in TABLE {
            let path = d.join(row.upstream);
            if row.upstream == "rustc" {
                assert!(!is_reroute_stub(&path), "foreign file was overwritten");
            } else {
                assert!(is_reroute_stub(&path), "{} not laid", row.upstream);
                #[cfg(unix)]
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o111,
                    0o111
                );
            }
        }
        assert!(!d.join("gone").exists(), "stale stub survived reconcile");
        let states = states(&l);
        assert!(
            states
                .iter()
                .any(|(n, s)| *n == "rustc" && *s == StubState::Foreign)
        );
        assert!(
            states
                .iter()
                .any(|(n, s)| *n == "cargo" && *s == StubState::Laid)
        );
        assert!(is_reroute_dir(&d), "lay must leave the marker file");
        // Laying again is idempotent.
        lay(&l).unwrap();
        remove_all(&l);
        assert!(d.join("rustc").exists(), "foreign file swept");
        assert!(!d.join("cargo").exists());
        assert!(!is_reroute_dir(&d), "remove_all must take the marker file");
        assert!(d.exists(), "a non-empty directory (the foreign file) stays");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Run `cmd`, killing it after `secs` — an exec loop must fail this test,
    /// not hang it.
    #[cfg(unix)]
    /// The CPU time a pid has consumed, as `ps` spells it, or `None` when `ps`
    /// cannot answer. Read ONLY to describe a timeout — never to decide one.
    fn child_cpu_time(pid: u32) -> Option<String> {
        let out = std::process::Command::new("/bin/ps")
            .args(["-o", "time=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!t.is_empty()).then_some(t)
    }

    fn output_within(mut cmd: std::process::Command, secs: u64) -> std::process::Output {
        use std::io::Read as _;
        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                child
                    .stdout
                    .take()
                    .unwrap()
                    .read_to_end(&mut stdout)
                    .unwrap();
                child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_end(&mut stderr)
                    .unwrap();
                return std::process::Output {
                    status,
                    stdout,
                    stderr,
                };
            }
            if std::time::Instant::now() > deadline {
                // A BOUND ON OUR PATIENCE IS NOT A FACT ABOUT THE STUB.
                //
                // This used to panic with "an exec loop" — a diagnosis nothing
                // here had measured. The bound is 5 s by default, and the two
                // things that exhaust it are NOT alike: an exec loop replaces
                // the image in the SAME pid over and over and burns CPU, while
                // a stub the test has just written can sit at ~0% CPU for
                // seconds while macOS `syspolicyd` assesses it under load (a
                // recorded hazard on this machine). Calling the second one an
                // exec loop sends the reader hunting a bug that is not there.
                //
                // So the pid is ASKED before it is killed. `ps -o time=` is the
                // CPU the process has actually consumed; it is the one cheap
                // measurement that separates spinning from blocked.
                let burned = child_cpu_time(child.id());
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "stub did not exit within {secs}s (cpu {}). Spinning CPU means an exec \
                     loop — the defect this test is about. Near-zero CPU means it is \
                     BLOCKED, not looping: on macOS the usual cause is Gatekeeper assessing \
                     a script this test wrote moments ago, which says nothing about the \
                     reroute. Re-run this test alone before believing the first reading.",
                    burned.as_deref().unwrap_or("unknown")
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// The 2026-09-07 review's reproduction: with a SECOND reroute directory on
    /// PATH — another prefix's, a symlink to ours, ours with a trailing slash —
    /// the escape must still reach upstream, not exec the other stub forever.
    #[cfg(unix)]
    #[test]
    fn escape_walk_skips_every_reroute_directory_not_just_its_own() {
        let l = layout("second-spelling");
        let other = layout("second-spelling-other-prefix");
        lay(&l).unwrap();
        lay(&other).unwrap();
        // The upstream copy lives OUTSIDE every manager prefix, as it does on a
        // real machine (~/.cargo/bin); a walk skips anything under a prefix.
        let upstream_dir =
            std::env::temp_dir().join(format!("atpkg-reroute-upstream-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&upstream_dir);
        std::fs::create_dir_all(&upstream_dir).unwrap();
        std::fs::write(
            upstream_dir.join("cargo"),
            "#!/bin/sh\necho \"upstream: $*\"\n",
        )
        .unwrap();
        std::fs::set_permissions(
            upstream_dir.join("cargo"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let link = l.prefix.join("link-to-reroute");
        std::os::unix::fs::symlink(dir(&l), &link).unwrap();
        let mut trailing = dir(&l).into_os_string();
        trailing.push("/");
        for (label, spellings) in [
            ("second prefix", vec![dir(&other).into_os_string()]),
            ("symlink", vec![link.clone().into_os_string()]),
            ("trailing slash", vec![trailing.clone()]),
        ] {
            let mut entries = spellings.clone();
            entries.push(dir(&l).into_os_string());
            entries.push(upstream_dir.clone().into_os_string());
            let path_env = std::env::join_paths(entries).unwrap();
            let mut cmd = std::process::Command::new(dir(&l).join("cargo"));
            cmd.args(["build"])
                .env("PATH", &path_env)
                .env(PASSTHROUGH_ENV, "1");
            // 60 s, not 5: the bound's only job is to tell "wedged" from
            // "slow", and an exec loop never finishes, so a longer wait costs
            // this test nothing when it is right and removes nearly all of the
            // window in which a loaded machine can be accused of one.
            let out = output_within(cmd, 60);
            assert_eq!(
                out.status.code(),
                Some(0),
                "{label}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "upstream: build\n",
                "{label}"
            );
            // And the Rust side's walk agrees.
            assert_eq!(
                upstream_on_path(&l, "cargo", Some(&path_env)),
                Some(upstream_dir.join("cargo")),
                "{label}"
            );
        }
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&other.prefix);
        let _ = std::fs::remove_dir_all(&upstream_dir);
    }

    /// A recorded decline is durable: `lay` lays nothing, and doctor/which see
    /// `Missing`, until the decline is lifted.
    #[test]
    fn a_recorded_decline_stops_lay() {
        let l = layout("declined");
        std::fs::write(l.declined(), b"# removed on purpose\n").unwrap();
        lay(&l).unwrap();
        assert!(!dir(&l).exists(), "a declined toolset must not get stubs");
        assert!(states(&l).iter().all(|(_, s)| *s == StubState::Missing));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// `cargo trust check` named the lane already: one line, then `targo trust check`.
    #[test]
    fn an_explicit_lane_under_the_rust_name_is_run_not_refused() {
        assert!(
            EXPLICIT_LANE_VERBS.contains(&"trust") && EXPLICIT_LANE_VERBS.contains(&"--unverified")
        );
        let note = explicit_lane_note("cargo", "targo", "trust");
        assert_eq!(note.lines().count(), 1, "{note}");
        assert!(note.contains("running targo trust"), "{note}");
        assert!(note.contains("aterm --no-reroute"), "{note}");
    }

    /// The stub's own decisions, in a real `/bin/sh`: the escape hatch execs the
    /// first upstream copy past the reroute dir (no atpkg needed); otherwise the
    /// embedded atpkg gets `__reroute <name> <args>`; with nothing reachable it
    /// fails CLOSED with the escape named.
    #[cfg(unix)]
    #[test]
    fn escape_hatch_dispatch_and_fail_closed_run_in_a_real_shell() {
        let l = layout("shell");
        let d = dir(&l);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(DIR_MARKER_FILE), format!("{STUB_MARKER}\n")).unwrap();
        let upstream_dir = l.prefix.join("upstream");
        std::fs::create_dir_all(&upstream_dir).unwrap();
        let write_exec = |path: &Path, body: &str| {
            std::fs::write(path, body).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        write_exec(
            &upstream_dir.join("cargo"),
            "#!/bin/sh\necho \"upstream: $*\"\nexit 0\n",
        );
        let fake = l.prefix.join("fake-atpkg");
        write_exec(&fake, "#!/bin/sh\necho \"atpkg: $*\"\nexit 3\n");
        let path_env = format!("{}:{}", d.display(), upstream_dir.display());
        let run = |body: &str, no_reroute: Option<&str>| {
            let stub = d.join("cargo");
            write_exec(&stub, body);
            let mut cmd = std::process::Command::new(&stub);
            cmd.args(["build", "--x"])
                .env("PATH", &path_env)
                .env_remove(PASSTHROUGH_ENV);
            if let Some(v) = no_reroute {
                cmd.env(PASSTHROUGH_ENV, v);
            }
            cmd.output().unwrap()
        };
        // Escape engaged: upstream runs, the stub's own dir skipped.
        let out = run(&stub_body_sh("cargo", &fake, &d), Some("1"));
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "upstream: build --x\n"
        );
        // Escape present but empty / zero: NOT engaged.
        for v in ["", "0"] {
            let out = run(&stub_body_sh("cargo", &fake, &d), Some(v));
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "atpkg: __reroute cargo build --x\n"
            );
            assert_eq!(out.status.code(), Some(3));
        }
        // No escape: the embedded atpkg decides.
        let out = run(&stub_body_sh("cargo", &fake, &d), None);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "atpkg: __reroute cargo build --x\n"
        );
        // Embedded path dangling and no atpkg on PATH: fail closed, exit 2, escape named.
        let out = run(&stub_body_sh("cargo", Path::new("/gone/atpkg"), &d), None);
        assert_eq!(out.status.code(), Some(2));
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("not reachable") && err.contains("aterm --no-reroute"),
            "{err}"
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).is_empty(),
            "stdout must stay clean"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// A fake program at `dir/name` that prints `<who>: <args>` — never a real vendor
    /// binary (a quarantined download raises a Gatekeeper dialog; owner, 2026-09-23).
    #[cfg(unix)]
    fn fake(dir: &Path, name: &str, who: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho \"{who}: $*\"\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// An agents twin shaped like the real one: it execs a (fake) store target by
    /// absolute path.
    #[cfg(unix)]
    fn fake_twin(l: &Layout, name: &str) -> PathBuf {
        let target = fake(
            &l.prefix.join("store").join(name).join("1").join("bin"),
            name,
            &format!("managed {name}"),
        );
        let twin = l.agents_dir().join(name);
        std::fs::create_dir_all(l.agents_dir()).unwrap();
        std::fs::write(
            &twin,
            format!("#!/bin/sh\nexec '{}' \"$@\"\n", target.display()),
        )
        .unwrap();
        std::fs::set_permissions(&twin, std::fs::Permissions::from_mode(0o755)).unwrap();
        twin
    }

    /// `name` run through `stub` on `path`, with every marker and the escape CLEARED from
    /// the inherited environment (this suite may itself run inside aterm) and `env` set.
    #[cfg(unix)]
    fn run_stub(stub: &Path, path: &std::ffi::OsStr, env: &[(&str, &str)]) -> std::process::Output {
        let mut cmd = std::process::Command::new(stub);
        cmd.args(["--version", "x y"]).env("PATH", path);
        for marker in crate::hooks::AGENTS_MARKERS {
            cmd.env_remove(marker);
        }
        cmd.env_remove(PASSTHROUGH_ENV);
        for (k, v) in env {
            cmd.env(k, v);
        }
        output_within(cmd, 60)
    }

    /// THE OWNER'S CASE, 2026-09-23, IN A REAL `/bin/sh`: a session shell from 2026-09-10
    /// whose PATH is `reroute, ~/.local/bin, /opt/homebrew/bin, pkg/bin` with NO
    /// `pkg/agents` runs the managed copy the moment the stub decides — inside aterm, on
    /// any one of the four markers. Outside aterm the same stub is a pure pass-through to
    /// the user's own copy, even with `agents/` leaked onto PATH ahead of it (owner law,
    /// 03513b5d7). `claude` and `codex` alike.
    #[cfg(unix)]
    #[test]
    fn an_agents_stub_runs_the_twin_inside_aterm_and_the_users_own_copy_outside() {
        for name in crate::stub::AGENT_PROGRAMS {
            let l = layout(&format!("agents-stub-{name}"));
            fake_twin(&l, name);
            lay(&l).unwrap();
            let d = dir(&l);
            let stub = d.join(name);
            assert!(is_reroute_stub(&stub), "{name}: a twin earns a stub");
            let local = l.prefix.parent().unwrap().join(format!(
                "atpkg-reroute-agents-local-{name}-{}",
                std::process::id()
            ));
            let brew = local.with_file_name(format!(
                "atpkg-reroute-agents-brew-{name}-{}",
                std::process::id()
            ));
            fake(&local, name, &format!("local {name}"));
            fake(&brew, name, &format!("brew {name}"));
            fake(&l.bin_dir(), name, &format!("bin {name}"));
            let stale = std::env::join_paths([d.clone(), local.clone(), brew.clone(), l.bin_dir()])
                .unwrap();
            let leaked = std::env::join_paths([
                d.clone(),
                l.agents_dir(),
                local.clone(),
                brew.clone(),
                l.bin_dir(),
            ])
            .unwrap();
            let stdout = |out: &std::process::Output| {
                assert!(
                    out.status.success(),
                    "{name}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                String::from_utf8_lossy(&out.stdout).into_owned()
            };
            let managed = format!("managed {name}: --version x y\n");
            let own = format!("local {name}: --version x y\n");
            // Inside aterm, on EACH marker alone: the twin, whatever PATH puts ahead.
            for marker in crate::hooks::AGENTS_MARKERS {
                for path in [&stale, &leaked] {
                    let out = run_stub(&stub, path, &[(marker, "1")]);
                    assert_eq!(stdout(&out), managed, "{name} under {marker}");
                }
            }
            // A marker set but EMPTY is not set (`-n` on the joined word).
            let out = run_stub(&stub, &stale, &[("ATERM_CHILD", "")]);
            assert_eq!(stdout(&out), own, "{name}: an empty marker");
            // Outside aterm: the user's own copy — never the managed one, even leaked.
            for path in [&stale, &leaked] {
                assert_eq!(stdout(&run_stub(&stub, path, &[])), own, "{name} outside");
            }
            // TERM_PROGRAM is not a marker (ATERM_TERM_PROGRAM makes it user-settable).
            let out = run_stub(&stub, &stale, &[("TERM_PROGRAM", "aterm")]);
            assert_eq!(stdout(&out), own, "{name}: TERM_PROGRAM decides nothing");
            // The --no-reroute marker does not apply to these stubs (manager's ruling,
            // 2026-09-23): it is the upstream Rust names' switch, and a foreign copy under it
            // in an old shell but the managed one in a new shell is the bug this closes.
            // Inside aterm the twin, outside the user's own — whatever its value, on any PATH.
            for v in ["1", "", "0", "yes"] {
                for path in [&stale, &leaked] {
                    let out = run_stub(&stub, path, &[("ATERM_CHILD", "1"), (PASSTHROUGH_ENV, v)]);
                    assert_eq!(stdout(&out), managed, "{name}: escape {v:?} inside aterm");
                    let out = run_stub(&stub, path, &[(PASSTHROUGH_ENV, v)]);
                    assert_eq!(stdout(&out), own, "{name}: escape {v:?} outside aterm");
                }
            }
            // The twin gone: inside aterm too, a pass-through.
            std::fs::remove_file(l.agents_dir().join(name)).unwrap();
            let out = run_stub(&stub, &stale, &[("ATERM_CHILD", "1")]);
            assert_eq!(stdout(&out), own, "{name}: no twin");
            // Nothing of the user's on PATH: `bin/` answers, as it would with no stub.
            let bin_only = std::env::join_paths([d.clone(), l.bin_dir()]).unwrap();
            let out = run_stub(&stub, &bin_only, &[]);
            assert_eq!(
                stdout(&out),
                format!("bin {name}: --version x y\n"),
                "{name}"
            );
            // Nothing at all: exit 127, one line naming the help page, stdout clean.
            let nothing = std::env::join_paths([d.clone()]).unwrap();
            let out = run_stub(&stub, &nothing, &[("ATERM_CHILD", "1")]);
            assert_eq!(out.status.code(), Some(127), "{name}");
            assert!(out.stdout.is_empty(), "{name}");
            let err = String::from_utf8_lossy(&out.stderr);
            assert_eq!(
                err.trim_end(),
                agents_not_found_message(name),
                "{name}: one line"
            );
            let _ = std::fs::remove_dir_all(&local);
            let _ = std::fs::remove_dir_all(&brew);
            let _ = std::fs::remove_dir_all(&l.prefix);
        }
    }

    /// NO RECURSION: a second reroute directory on PATH — another prefix's, holding its
    /// own `claude` stub, a symlink to ours, ours with a trailing slash — `agents/` under
    /// another spelling, and a symlink to the stub FILE in an ordinary directory are all
    /// walked past, inside aterm and out, in bounded time.
    #[cfg(unix)]
    #[test]
    fn an_agents_stub_never_runs_another_stub_or_itself() {
        let l = layout("agents-no-recursion");
        let other = layout("agents-no-recursion-other");
        fake_twin(&l, "claude");
        fake_twin(&other, "claude");
        lay(&l).unwrap();
        lay(&other).unwrap();
        // The other prefix's twin gone: its stub passes through, and must not come back.
        std::fs::remove_file(other.agents_dir().join("claude")).unwrap();
        let local = l.prefix.parent().unwrap().join(format!(
            "atpkg-reroute-agents-norec-local-{}",
            std::process::id()
        ));
        fake(&local, "claude", "local claude");
        let link = l.prefix.join("link-to-reroute");
        std::os::unix::fs::symlink(dir(&l), &link).unwrap();
        let agents_link = l.prefix.join("link-to-agents");
        std::os::unix::fs::symlink(l.agents_dir(), &agents_link).unwrap();
        let mut trailing = dir(&l).into_os_string();
        trailing.push("/");
        // The stub itself under another name, in an ordinary directory (`ln -s "$(command
        // -v claude)" ~/bin/claude`): no marker there, so only `-ef "$0"` stops the loop.
        let user_bin = l.prefix.join("user-bin");
        std::fs::create_dir_all(&user_bin).unwrap();
        std::os::unix::fs::symlink(dir(&l).join("claude"), user_bin.join("claude")).unwrap();
        let path = std::env::join_paths([
            dir(&other).into_os_string(),
            link.clone().into_os_string(),
            trailing,
            agents_link.clone().into_os_string(),
            user_bin.clone().into_os_string(),
            local.clone().into_os_string(),
        ])
        .unwrap();
        let stub = dir(&l).join("claude");
        let out = run_stub(&stub, &path, &[]);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "local claude: --version x y\n",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = run_stub(&stub, &path, &[("ATERM_SESSION_ID", "s-1")]);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "managed claude: --version x y\n",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // …and the OTHER prefix's stub (its twin gone, so it passes through), entered
        // first, walks past every reroute spelling of ours too. (It skips its OWN
        // `agents/` only — our `agents/` is left off this PATH, the nested-prefix corner
        // the pass-through does not claim to cover.)
        let path = std::env::join_paths([
            dir(&other).into_os_string(),
            link.clone().into_os_string(),
            dir(&l).into_os_string(),
            local.clone().into_os_string(),
        ])
        .unwrap();
        let out = run_stub(&dir(&other).join("claude"), &path, &[("ATERM_CHILD", "1")]);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "local claude: --version x y\n",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&local);
        let _ = std::fs::remove_dir_all(&l.prefix);
        let _ = std::fs::remove_dir_all(&other.prefix);
    }

    /// `lay` lays an agent program's stub exactly while its twin stands: none without one
    /// (a stub would make `command -v claude` answer on a machine with no claude), never
    /// over a foreign file, swept when the twin goes, and a converged directory is left
    /// alone byte for byte.
    #[cfg(unix)]
    #[test]
    fn lay_follows_the_agents_twin_and_never_claims_a_foreign_name() {
        use std::os::unix::fs::MetadataExt as _;
        let l = layout("agents-lay");
        lay(&l).unwrap();
        let d = dir(&l);
        for name in crate::stub::AGENT_PROGRAMS {
            assert!(!d.join(name).exists(), "{name}: no twin, no stub");
            assert_eq!(stub_state(&l, name), StubState::Missing);
        }
        fake_twin(&l, "claude");
        lay(&l).unwrap();
        assert!(is_reroute_stub(&d.join("claude")));
        assert_eq!(
            std::fs::read_to_string(d.join("claude")).unwrap(),
            agents_stub_body_sh("claude", &l.agents_dir())
        );
        assert!(!d.join("codex").exists(), "codex has no twin");
        assert_eq!(agents_routes(&l), vec!["claude"]);
        // Converged: a second lay rewrites nothing.
        let ino = std::fs::metadata(d.join("claude")).unwrap().ino();
        lay(&l).unwrap();
        assert_eq!(std::fs::metadata(d.join("claude")).unwrap().ino(), ino);
        // A foreign `reroute/codex` is never claimed, even once codex has a twin.
        std::fs::write(d.join("codex"), "#!/bin/sh\necho mine\n").unwrap();
        fake_twin(&l, "codex");
        lay(&l).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.join("codex")).unwrap(),
            "#!/bin/sh\necho mine\n"
        );
        assert_eq!(stub_state(&l, "codex"), StubState::Foreign);
        // The twin gone: its stub is swept at the next lay; the TABLE's stay.
        std::fs::remove_file(l.agents_dir().join("claude")).unwrap();
        lay(&l).unwrap();
        assert!(!d.join("claude").exists(), "the stub followed the twin out");
        assert!(d.join("cargo").exists() && d.join("codex").exists());
        // `remove_all` takes an agents stub like any other of ours.
        fake_twin(&l, "claude");
        lay(&l).unwrap();
        remove_all(&l);
        assert!(!d.join("claude").exists());
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The stub's shape: marked on line 2 and recognized as ours, no store target for the
    /// shim parser, the marker word rendered from THE list the rc hook's gate is (so the
    /// shell-start and exec-time decisions read the same markers), neither `TERM_PROGRAM`
    /// nor the `--no-reroute` marker read, and no marker ever SET — the stub reads the markers,
    /// never writes one.
    #[test]
    fn the_agents_stub_reads_the_hooks_markers_and_is_never_a_managed_shim() {
        let body = agents_stub_body_sh("claude", Path::new("/p/agents"));
        assert_eq!(body.lines().nth(1), Some(STUB_MARKER));
        assert!(
            crate::platform::parse_sh_shim_target(&body).is_none(),
            "{body}"
        );
        assert!(!body.contains("exec '"), "{body}");
        assert!(
            body.contains(&format!(
                "if [ -n \"{}\" ]",
                crate::hooks::agents_markers_sh()
            )),
            "{body}"
        );
        assert_eq!(
            crate::hooks::AGENTS_MARKERS,
            &["ATERM_AGENTS_DIR", "ATERM_CHILD", "ATERM_SESSION_ID"]
        );
        for marker in crate::hooks::AGENTS_MARKERS {
            assert!(
                body.contains(&format!("${{{marker}-}}")),
                "{marker}: {body}"
            );
            assert!(
                !body.contains(&format!("{marker}=")),
                "{marker} is never set: {body}"
            );
        }
        let posix = crate::hooks::hook_files(Path::new("/p/bin"), Path::new("/p/agents"))
            .into_iter()
            .find(|(n, _)| n.ends_with(".zsh"))
            .unwrap()
            .1;
        assert!(
            posix.contains(&format!(
                "case \"{}\" in",
                crate::hooks::agents_markers_sh()
            )),
            "the rc hook's gate is the same word: {posix}"
        );
        assert!(!body.contains("TERM_PROGRAM"), "{body}");
        assert!(
            body.contains("'/p/agents'") && body.contains("'claude'"),
            "{body}"
        );
        assert!(body.contains(DIR_MARKER_FILE), "{body}");
        assert!(
            !body.contains(PASSTHROUGH_ENV),
            "the markers alone decide; the Rust names' escape is never read: {body}"
        );
        assert!(body.ends_with("exit 127\n"), "{body}");
    }

    /// The Rust restatement the doctor and `which` answer with agrees with the stub:
    /// markers by `-n` (an empty one is unset), nothing else read, and the PATH a
    /// pass-through walks.
    #[cfg(unix)]
    #[test]
    fn the_stubs_decision_restated_matches_the_stub() {
        let env = |pairs: &[(&str, &str)]| {
            let owned: Vec<(String, String)> = pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
            StubEnv::read(move |k| {
                owned
                    .iter()
                    .find(|(n, _)| n == k)
                    .map(|(_, v)| std::ffi::OsString::from(v))
            })
        };
        assert_eq!(env(&[]), StubEnv::default());
        assert!(!env(&[("ATERM_CHILD", "")]).in_aterm);
        assert!(!env(&[("TERM_PROGRAM", "aterm")]).in_aterm);
        for marker in crate::hooks::AGENTS_MARKERS {
            assert!(env(&[(marker, "x")]).in_aterm, "{marker}");
        }
        assert!(
            !env(&[(PASSTHROUGH_ENV, "1")]).in_aterm,
            "the escape is not a marker, and the stub reads nothing else"
        );

        let l = layout("agents-restated");
        fake_twin(&l, "claude");
        lay(&l).unwrap();
        let inside = StubEnv { in_aterm: true };
        assert!(agents_stub_takes_twin(&l, "claude", inside));
        assert!(!agents_stub_takes_twin(&l, "claude", StubEnv::default()));
        assert!(!agents_stub_takes_twin(&l, "codex", inside), "no twin");
        let local = l.prefix.join("local");
        fake(&local, "claude", "local");
        let stale = std::env::join_paths([dir(&l), local.clone(), l.bin_dir()]).unwrap();
        assert_eq!(
            agents_stub_first_on_path(&l, "claude", Some(&stale)),
            Some(dir(&l).join("claude"))
        );
        let other_first = std::env::join_paths([local.clone(), dir(&l)]).unwrap();
        assert_eq!(
            agents_stub_first_on_path(&l, "claude", Some(&other_first)),
            None
        );
        let leaked =
            std::env::join_paths([dir(&l), l.agents_dir(), local.clone(), l.bin_dir()]).unwrap();
        assert_eq!(
            pass_through_path(&l, Some(&leaked)),
            Some(std::env::join_paths([local.clone(), l.bin_dir()]).unwrap())
        );
        assert_eq!(
            without_reroute_dirs(Some(&leaked)),
            Some(std::env::join_paths([l.agents_dir(), local, l.bin_dir()]).unwrap())
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
