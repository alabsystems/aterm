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
//!   that one. [`QUIET_ENV`] is the flag the owner asked for, and
//!   [`STRICT_ENV`] restores the refusal for anyone who wants the friction.
//! * **ORACLE** (`z3`): `ay`'s differential oracle. Refuse; [`Z3_ORACLE_ENV`]
//!   reaches the real z3. This row is NOT covered by the announce-and-run
//!   ruling above: the owner's sentence is about "the vanilla rust toolchain",
//!   and running the wrong oracle does not merely skip a proof claim — it
//!   manufactures a false one, which is a different risk in kind.
//!
//! # Escapes, all of them
//!
//! * [`NO_REROUTE_ENV`] engaged (`aterm --no-reroute` sets it for a session)
//!   restores every upstream tool. The stub itself walks PATH past its own
//!   directory and execs the first upstream copy — in `sh`, so the escape works
//!   even when atpkg is unreachable. The Rust side sets the same variable on any
//!   upstream child it execs, so cargo's own `rustc`/`rustdoc` spawns are never
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
/// The one escape hatch (philosophy §4): engaged ⇒ every upstream tool is
/// restored. Also what an escaped upstream child inherits, so its own spawns
/// pass silently.
pub const NO_REROUTE_ENV: &str = "ATERM_NO_REROUTE";
/// The ORACLE row's own key: engaged ⇒ the real z3 runs.
pub const Z3_ORACLE_ENV: &str = "ATERM_Z3_IS_ORACLE";
/// The flag the owner asked for on 2026-09-08: engaged ⇒ a SIGNPOST row runs
/// upstream with NO announcement. It silences a line; it changes nothing else,
/// and it is deliberately not honoured for a DIRECT row (whose announcement is
/// the "never silently substituting" guarantee itself) nor when [`STRICT_ENV`]
/// is engaged (a refusal with no reason given is not a refusal, it is a bug).
pub const QUIET_ENV: &str = "ATERM_REROUTE_QUIET";
/// The opposite knob: engaged ⇒ a SIGNPOST row REFUSES (exit [`REFUSAL_EXIT`])
/// instead of running upstream, which is philosophy §4's original letter and
/// what shipped between 2026-09-07 and this change. Kept because the friction
/// reading is defensible and one variable is a cheap way to hold both.
pub const STRICT_ENV: &str = "ATERM_REROUTE_STRICT";
/// Line 2 of every stub — the ONLY thing that makes a file ours to rewrite or
/// remove. Version-suffixed so a changed body can be told from an old one.
pub const STUB_MARKER: &str = "# atpkg reroute stub v1";
/// The hidden verb a stub execs: `atpkg __reroute <upstream> [args…]`.
pub const HIDDEN_VERB: &str = "__reroute";
/// An ORACLE refusal, a SIGNPOST refusal under [`STRICT_ENV`], and the
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

/// The per-row policy — philosophy §4's table, one variant per column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Run `branded` with the caller's arguments; announce one line.
    Direct { branded: &'static str },
    /// Refuse; name `branded` and every `lane` with the caller's arguments.
    /// An empty `lanes` means "one spelling, equivalence unproven" (`tlc`).
    Signpost {
        branded: &'static str,
        lanes: &'static [Lane],
    },
    /// Refuse; the upstream tool is `branded`'s differential oracle. `env`
    /// engaged reaches the upstream tool.
    Oracle {
        branded: &'static str,
        env: &'static str,
    },
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
        policy: Policy::Direct { branded: "tippy" },
    },
    Row {
        upstream: "rustfmt",
        family: "Rust",
        policy: Policy::Direct {
            branded: "trustfmt",
        },
    },
    Row {
        upstream: "rustdoc",
        family: "Rust",
        policy: Policy::Direct {
            branded: "trustdoc",
        },
    },
    Row {
        upstream: "lean",
        family: "Lean",
        policy: Policy::Direct { branded: "clean" },
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
        policy: Policy::Oracle {
            branded: "ay",
            env: Z3_ORACLE_ENV,
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
        Policy::Direct { branded }
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

/// THE ONE reading of a boolean escape variable — `aterm_types`'
/// `env_flag_engaged` rule restated here so a present-but-empty `ATERM_NO_REROUTE=`
/// (which travels) never counts as a veto: engaged iff non-empty and not `"0"`.
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
    let mut s = String::from("(");
    s.push_str(NO_REROUTE_ENV);
    s.push_str("=1 restores upstream '");
    s.push_str(upstream);
    s.push_str("'.)");
    s
}

/// What a SIGNPOST row does once the two knobs are read: whether it speaks, and
/// whether it refuses. One function so the 2026-09-08 ruling lives in exactly
/// one place and can be pinned without a subprocess.
///
/// * neither knob — announce, then run upstream (the default, and the ruling);
/// * [`QUIET_ENV`] — run upstream, say nothing (the flag the owner asked for);
/// * [`STRICT_ENV`] — announce, then refuse (philosophy §4's original letter);
/// * both — announce, then refuse. QUIET never silences a refusal, because an
///   exit 2 with no reason printed is indistinguishable from a broken tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignpostAction {
    /// Print [`signpost_message`] on stderr.
    pub announce: bool,
    /// Exit [`REFUSAL_EXIT`] instead of exec'ing upstream.
    pub refuse: bool,
}

/// [`SignpostAction`] for the two knobs. Pure; `run` reads the environment and
/// then does exactly what this says.
#[must_use]
pub fn signpost_action(strict: bool, quiet: bool) -> SignpostAction {
    SignpostAction {
        announce: strict || !quiet,
        refuse: strict,
    }
}

/// The last line (or two) of a SIGNPOST message.
///
/// Under [`STRICT_ENV`] it is the old refusal's "re-run" instruction and the
/// escape. Otherwise it says what is about to happen — upstream runs, and what
/// it produces carries no proof claim — and names both flags, because a line
/// printed on every build must say how to stop printing it.
fn signpost_closing(upstream: &str, strict: bool, rerun: &str) -> String {
    if strict {
        return format!("       {rerun}  {}", escape_clause(upstream));
    }
    // The measuring page is named on every signpost (owner, 2026-09-08: the
    // Trust default is to be "very strongly encouraged by the aterm system
    // itself"): a reader who is about to run stock cargo anyway is told, in
    // the same breath, the one command that answers which toolchain this
    // directory actually gets — instead of guessing from the two lines above.
    format!(
        "       Running upstream '{upstream}' now — nothing it produces carries a proof claim.\n       `aterm help rust` measures which toolchain THIS directory gets; the default here is Trust.\n       ({QUIET_ENV}=1 silences this; {STRICT_ENV}=1 refuses instead of running.)"
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
///        (ATERM_REROUTE_QUIET=1 silences this; ATERM_REROUTE_STRICT=1 refuses instead of running.)
/// ```
///
/// `strict` ([`STRICT_ENV`]) swaps the last line back to the refusal's "re-run
/// naming the lane" and the escape clause; the diagnosis above it is the same
/// either way, because the diagnosis was never the part in dispute.
#[must_use]
pub fn signpost_message(row: &Row, lanes: &[Lane], args: &[String], strict: bool) -> String {
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
        out.push_str(&signpost_closing(upstream, strict, "Re-run as shown."));
        return out;
    }
    if lanes.is_empty() {
        out.push_str(&format!(
            "aterm: '{upstream}' is the {} name; on this toolchain the tool is '{branded}', and\n",
            row.family
        ));
        out.push_str("       drop-in equivalence is not yet proven, so nothing is substituted:\n");
        out.push_str(&format!("         {}\n", join_command(branded, &rest)));
        out.push_str(&signpost_closing(
            upstream,
            strict,
            "Re-run naming the tool.",
        ));
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
    out.push_str(&signpost_closing(
        upstream,
        strict,
        "Re-run naming the lane.",
    ));
    out
}

/// The ORACLE refusal.
#[must_use]
pub fn oracle_message(upstream: &str, branded: &str, env: &str) -> String {
    format!(
        "aterm: '{upstream}' is the oracle '{branded}' is measured against, so a session never runs it silently: {env}=1 reaches the real {upstream} ({NO_REROUTE_ENV}=1 restores every upstream tool)."
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
        "aterm: '{upstream} +{toolchain}' names an upstream toolchain explicitly — running upstream '{upstream}'; nothing it produces is verified. (The Trust lane is 'targo trust <verb>'; {NO_REROUTE_ENV}=1 silences this note.)"
    )
}

/// Printed by the stub itself when atpkg is unreachable — fail CLOSED, with the
/// escape named, never a silent fall-through to upstream.
#[must_use]
pub fn unreachable_message(upstream: &str) -> String {
    format!(
        "aterm: '{upstream}' is rerouted inside aterm sessions, but aterm's package manager is not reachable to say where; {NO_REROUTE_ENV}=1 restores upstream '{upstream}'"
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
        Policy::Direct { branded } => {
            format!("runs '{branded} <args>' with one stderr line {escape}")
        }
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
        Policy::Oracle { branded, env } => format!(
            "refused ('{branded}' is measured against it); {env}=1 reaches the real {upstream} {escape}"
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
    s.push_str(NO_REROUTE_ENV);
    s.push_str(":-}\" ] && [ \"${");
    s.push_str(NO_REROUTE_ENV);
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
        .map(|row| {
            let path = stub_path(layout, row.upstream);
            let state = match std::fs::symlink_metadata(&path) {
                Err(_) => StubState::Missing,
                Ok(_) if is_reroute_stub(&path) => StubState::Laid,
                Ok(_) => StubState::Foreign,
            };
            (row.upstream, state)
        })
        .collect()
}

/// Lay (or refresh) every row's stub — at seed, after each install pass, and on
/// `repair`, so the embedded atpkg path survives relocation and self-update.
/// Never over a foreign file; a stub whose row left the table is swept
/// (marker-gated). Windows lays nothing.
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
    // The marker file first, so a walk racing this `lay` already recognizes
    // the directory before the first stub lands in it.
    let marker = dir.join(DIR_MARKER_FILE);
    if !marker.is_file() {
        crate::stub::write_executable_atomic(&marker, &format!("{STUB_MARKER}\n"))?;
    }
    let atpkg = crate::stub::embedded_atpkg_path();
    for row in TABLE {
        let path = dir.join(row.upstream);
        match std::fs::symlink_metadata(&path) {
            Err(_) => {}                          // absent: ours to claim
            Ok(_) if is_reroute_stub(&path) => {} // ours: rewrite refreshes the path
            Ok(_) => continue,                    // foreign: never touched
        }
        crate::stub::write_executable_atomic(&path, &stub_body_sh(row.upstream, &atpkg, &dir))?;
    }
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
            if row_for(name).is_none() && is_reroute_stub(&path) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    Ok(())
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
/// A SIGNPOST row announces and then execs UPSTREAM, so its exit code is the
/// upstream tool's — it only refuses under [`STRICT_ENV`]. ORACLE still
/// refuses.
pub fn run(layout: &Layout, upstream: &str, args: &[String]) -> ExitCode {
    let Some(row) = row_for(upstream) else {
        eprintln!("atpkg {HIDDEN_VERB}: '{upstream}' is not a rerouted name");
        return ExitCode::from(REFUSAL_EXIT);
    };
    // Belt and braces: the stub decides this first, but `aterm <upstream>` and a
    // direct `atpkg __reroute` call arrive here without it.
    if engaged(std::env::var(NO_REROUTE_ENV).ok().as_deref()) {
        return exec_upstream(layout, upstream, args);
    }
    match row.policy {
        Policy::Direct { branded } => {
            eprintln!("{}", direct_announcement(upstream, row.family, branded));
            exec_branded(layout, upstream, branded, args)
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
            // ANNOUNCE, THEN RUN — the 2026-09-08 ruling. `strict` restores
            // the refusal; `quiet` drops the line but never the run. A strict
            // refusal is always spoken: an exit 2 with no reason is a bug.
            let strict = engaged(std::env::var(STRICT_ENV).ok().as_deref());
            let quiet = engaged(std::env::var(QUIET_ENV).ok().as_deref());
            let action = signpost_action(strict, quiet);
            if action.announce {
                eprintln!("{}", signpost_message(row, lanes, args, strict));
            }
            if action.refuse {
                return ExitCode::from(REFUSAL_EXIT);
            }
            exec_upstream(layout, upstream, args)
        }
        Policy::Oracle { branded, env } => {
            if engaged(std::env::var(env).ok().as_deref()) {
                return exec_upstream(layout, upstream, args);
            }
            eprintln!("{}", oracle_message(upstream, branded, env));
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
/// [`NO_REROUTE_ENV`] set so its own spawns pass silently.
fn exec_upstream(layout: &Layout, upstream: &str, args: &[String]) -> ExitCode {
    let path_var = std::env::var_os("PATH");
    let Some(target) = upstream_on_path(layout, upstream, path_var.as_deref()) else {
        eprintln!("aterm: upstream '{upstream}' is not on PATH outside the reroute directories");
        return ExitCode::from(127);
    };
    let mut command = std::process::Command::new(&target);
    command.args(args).env(NO_REROUTE_ENV, "1");
    let err = crate::platform::exec_or_run(&mut command);
    eprintln!("aterm: failed to exec {}: {err}", target.display());
    ExitCode::from(127)
}

/// A DIRECT row: the managed copy of `branded`, through the store (never PATH),
/// with the managed `bin/` appended for its children — `atpkg run`'s discipline.
fn exec_branded(layout: &Layout, upstream: &str, branded: &str, args: &[String]) -> ExitCode {
    let Some(target) = crate::which(layout, branded) else {
        eprintln!(
            "aterm: '{upstream}' reroutes to '{branded}', which is not installed here — opening aterm provisions the toolset (`aterm pkg install <program>` for one program); {NO_REROUTE_ENV}=1 restores upstream '{upstream}'."
        );
        return ExitCode::from(127);
    };
    let child_path =
        crate::store::append_bin_to_path(std::env::var_os("PATH").as_deref(), &layout.bin_dir());
    let mut command = std::process::Command::new(&target);
    command.args(args).env("PATH", child_path);
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
        let text = signpost_message(row, lanes, &args(&["build", "--release"]), false);
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
        // answer, and says the default (owner, 2026-09-08). The strict/refusal
        // closing is a different sentence and is pinned separately below.
        assert!(
            text.contains("`aterm help rust` measures which toolchain THIS directory gets"),
            "{text}"
        );
        assert!(text.contains("the default here is Trust"), "{text}");
        assert!(text.contains("UNVERIFIED — no proof claim"), "{text}");
        // ANNOUNCE, NOT REFUSE: the default says upstream is about to run and
        // names both flags. The refusal's wording survives only under strict.
        assert!(
            text.contains(
                "Running upstream 'cargo' now — nothing it produces carries a proof claim."
            ),
            "{text}"
        );
        assert!(
            text.ends_with("(ATERM_REROUTE_QUIET=1 silences this; ATERM_REROUTE_STRICT=1 refuses instead of running.)"),
            "{text}"
        );
        let strict = signpost_message(row, lanes, &args(&["build", "--release"]), true);
        assert!(
            strict.ends_with("(ATERM_NO_REROUTE=1 restores upstream 'cargo'.)"),
            "{strict}"
        );
        assert!(!strict.contains("Running upstream"), "{strict}");
        // The diagnosis above the last line is the same either way.
        assert!(strict.contains("targo trust build --release"), "{strict}");
        // No arguments: the bare commands, no trailing space.
        let bare = signpost_message(row, lanes, &[], false);
        assert!(bare.contains("targo trust   "), "{bare}");
        assert!(!bare.contains("targo trust  \n"), "{bare}");
    }

    /// THE 2026-09-08 RULING, AT THE DECISION POINT.
    ///
    /// The owner: *"We don't want to prevent agents from using the vanilla rust
    /// toolchain, but we do want … some kind of printed message when using
    /// these tools that could be suppressed with a flag"*. So the default must
    /// RUN, the quiet flag must silence WITHOUT refusing, and the strict knob —
    /// kept for the friction reading this replaced — must refuse while still
    /// saying why.
    #[test]
    fn a_signpost_announces_and_runs_and_only_strict_refuses() {
        assert_eq!(
            signpost_action(false, false),
            SignpostAction {
                announce: true,
                refuse: false
            },
            "the default announces and RUNS — refusing is what the owner ruled out"
        );
        assert_eq!(
            signpost_action(false, true),
            SignpostAction {
                announce: false,
                refuse: false
            },
            "the quiet flag silences the line; it must never withhold the run"
        );
        assert_eq!(
            signpost_action(true, false),
            SignpostAction {
                announce: true,
                refuse: true
            },
        );
        assert_eq!(
            signpost_action(true, true),
            SignpostAction {
                announce: true,
                refuse: true
            },
            "a refusal is always spoken: exit 2 with no reason is a bug, not a policy"
        );
        // The two knobs are distinct names and neither is the escape hatch.
        assert_ne!(QUIET_ENV, STRICT_ENV);
        assert_ne!(QUIET_ENV, NO_REROUTE_ENV);
        assert_ne!(STRICT_ENV, NO_REROUTE_ENV);
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
            false,
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
        let text = signpost_message(row, lanes, &args(&["fmt", "--check"]), false);
        assert!(text.contains("targo fmt --check"), "{text}");
    }

    #[test]
    fn rustc_names_both_lanes_and_tlc_names_one_tool_without_claiming_equivalence() {
        let rustc = row_for("rustc").unwrap();
        let Policy::Signpost { lanes, .. } = rustc.policy else {
            panic!()
        };
        let text = signpost_message(rustc, lanes, &args(&["main.rs"]), false);
        assert!(text.contains("trustc main.rs"), "{text}");
        assert!(text.contains("trustc -Ztrust-verify=off main.rs"), "{text}");
        let tlc = row_for("tlc").unwrap();
        let Policy::Signpost { lanes, .. } = tlc.policy else {
            panic!()
        };
        let text = signpost_message(tlc, lanes, &args(&["Spec.tla"]), false);
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
            oracle_message("z3", "ay", Z3_ORACLE_ENV),
            passthrough_note("cargo", "stable"),
            unreachable_message("cargo"),
        ] {
            assert_eq!(text.lines().count(), 1, "{text}");
            assert!(text.starts_with("aterm: "), "{text}");
            assert!(text.contains(NO_REROUTE_ENV), "{text}");
        }
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
        assert!(body.contains(NO_REROUTE_ENV), "{body}");
        assert!(body.ends_with("exit 2\n"), "{body}");
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
                let _ = child.kill();
                panic!("stub did not exit within {secs}s — an exec loop");
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
                .env(NO_REROUTE_ENV, "1");
            let out = output_within(cmd, 5);
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
        assert!(note.contains(NO_REROUTE_ENV), "{note}");
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
                .env_remove(NO_REROUTE_ENV);
            if let Some(v) = no_reroute {
                cmd.env(NO_REROUTE_ENV, v);
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
            err.contains("not reachable") && err.contains(NO_REROUTE_ENV),
            "{err}"
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).is_empty(),
            "stdout must stay clean"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
