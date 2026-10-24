// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Rerouting: the upstream Rust names inside an aterm session.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aterm_types::rust_lane;

use crate::store::Layout;
use crate::upstream_identity::Resolved;

/// Exported by the spawn seams.
pub const REROUTE_DIR_ENV: &str = "ATERM_REROUTE_DIR";
/// Exported by the front door (`crates/aterm/src/main.rs`) to a TTY session, whether or not reroute is engaged.
pub const AGENTS_DIR_ENV: &str = "ATERM_AGENTS_DIR";
/// INTERNAL PROTOCOL, not a setting: the marker of a session with every upstream tool restored.
pub const PASSTHROUGH_ENV: &str = "__ATERM_REROUTE_PASSTHROUGH";
/// cargo's mark of its own compile.
pub const CARGO_CRATE_ENV: &str = "CARGO_CRATE_NAME";
/// Trust's cargo's second mark of its own compile, set beside [`CARGO_CRATE_ENV`] on the same processes (`targo` and the store's `cargo` are one binary).
pub const TARGO_CRATE_ENV: &str = "TARGO_CRATE_NAME";
/// The rows cargo spawns for a crate, the stubs that take a stock cargo's [`CARGO_CRATE_ENV`] as [`ANNOUNCED_ENV`].
pub const CARGO_COMPILERS: &[&str] = &["rustc", "rustdoc"];
/// INTERNAL PROTOCOL, not a setting: the marker [`exec_upstream`] stamps on every upstream copy the reroute runs.
pub const ANNOUNCED_ENV: &str = "__ATERM_REROUTE_ANNOUNCED";
/// Line 2 of every stub: the only thing that makes a file ours to rewrite or remove.
pub const STUB_MARKER: &str = "# atpkg reroute stub v1";
/// The hidden verb a stub execs: `atpkg __reroute <upstream> [args…]`.
pub const HIDDEN_VERB: &str = "__reroute";
/// The exit of a refusal: strict's (`z3`, a store-less call, a stub whose atpkg is unreachable; a strict DIRECT row whose Trust tool is missing exits 127), a non-Trust `cargo`/`rustc` inside a Trust source checkout, a `__reroute` call naming no rerouted tool, and `atpkg lane`'s.
pub const REFUSAL_EXIT: u8 = 2;
/// The directory under the manager prefix (`<prefix>/reroute`).
pub const DIR_NAME: &str = "reroute";
/// A marker file `lay` puts in every reroute directory, so a walk recognizes one under any spelling (a nested aterm's second prefix, a symlink, a trailing slash) without comparing strings.
pub const DIR_MARKER_FILE: &str = ".atpkg-reroute-dir";
const MAX_STUB_BYTES: usize = 16 * 1024;

/// One verification lane a SIGNPOST names, rendered as `<command> <args>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lane {
    pub label: &'static str,
    pub command: &'static str,
    pub note: &'static str,
}

/// A DIRECT row whose branded tool is verb-first: `lean file.lean` must reach `clean check file.lean`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceVerb {
    /// The branded subcommand a bare source file routes through.
    pub verb: &'static str,
    /// The source extension that selects it, leading dot included.
    pub ext: &'static str,
}

/// The per-row policy (`docs/DESIGN-toolchain-philosophy-2026-08-29.md` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    Direct {
        branded: &'static str,
        program: &'static str,
        source_verb: Option<SourceVerb>,
    },
    Signpost {
        branded: &'static str,
        program: &'static str,
        lanes: &'static [Lane],
    },
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

/// `cargo <verb>` spellings that run a DIRECT row's tool: one Trust spelling, no lane.
pub const CARGO_VERB_REROUTES: &[(&str, &str)] = &[("clippy", "targo tippy"), ("fmt", "targo fmt")];

/// The table.
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

/// The branded name a row resolves to (what a DIRECT row runs, what a SIGNPOST or ORACLE row names).
#[must_use]
pub const fn branded_of(row: &Row) -> &'static str {
    match row.policy {
        Policy::Direct { branded, .. }
        | Policy::Signpost { branded, .. }
        | Policy::Oracle { branded, .. } => branded,
    }
}

/// The program whose install lays a row's branded name ([`branded_of`]).
#[must_use]
pub const fn program_of(row: &Row) -> &'static str {
    match row.policy {
        Policy::Direct { program, .. }
        | Policy::Signpost { program, .. }
        | Policy::Oracle { program, .. } => program,
    }
}

/// `<prefix>/reroute`, a directory of its own: `bin/` holds only the store's shims.
#[must_use]
pub fn dir(layout: &Layout) -> PathBuf {
    layout.prefix.join(DIR_NAME)
}

/// The stub file for one upstream name.
#[must_use]
pub fn stub_path(layout: &Layout, upstream: &str) -> PathBuf {
    dir(layout).join(upstream)
}

/// Whether a marker ([`PASSTHROUGH_ENV`], [`ANNOUNCED_ENV`]) is set: non-empty and not `0`, the stub's own `sh` test and `aterm_types::control_socket::env_flag_engaged`'s rule.
#[must_use]
pub fn engaged(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// `+<toolchain>` as the first argument (rustup's own spelling), the user naming a toolchain deliberately ([`aterm_types::rust_lane::explicit_toolchain`]).
#[must_use]
pub fn explicit_toolchain(args: &[String]) -> Option<&str> {
    rust_lane::explicit_toolchain(args)
}

fn is_rust_row(row: &Row) -> bool {
    rust_lane::STOCK_TOOLS
        .iter()
        .any(|(stock, _)| *stock == row.upstream)
}

/// Where a channel is pinned for the directory a stub runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    /// The channel named, verbatim.
    pub channel: String,
    /// Where it was read: a toolchain file, or `$RUSTUP_TOOLCHAIN`.
    pub source: rust_lane::PinSource,
}

impl Pin {
    /// The pin for `cwd`, `$RUSTUP_TOOLCHAIN` (`env_toolchain`) first as rustup ranks it.
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

fn escape_clause(upstream: &str) -> String {
    let mut s = String::from("(`aterm --no-reroute` restores upstream '");
    s.push_str(upstream);
    s.push_str("'.)");
    s
}

/// The setting that silences a SIGNPOST row's announcement, as the line spells it.
pub const ANNOUNCE_SETTING: &str = "[reroute] announce = false";

const MEASURING_LINE: &str = "       `aterm help rust` shows which toolchain this directory gets; the default here is Trust.";

fn signpost_closing(rust: bool) -> String {
    let silence =
        format!("       ({ANNOUNCE_SETTING} in aterm.toml silences this — Settings ▸ Packages.)");
    if rust {
        format!("{MEASURING_LINE}\n{silence}")
    } else {
        silence
    }
}

fn pin_lines(pin: &Pin, tool: &str) -> String {
    if rust_lane::is_trust_channel(&pin.channel) {
        return String::new();
    }
    let channel = &pin.channel;
    match &pin.source {
        rust_lane::PinSource::File(file) => format!(
            "       This directory pins stock \"{channel}\", which moves only rustup's proxies, never {tool}:\n       the Trust spellings above run the Trust toolchain here too. ({})\n",
            file.display()
        ),
        rust_lane::PinSource::Env => format!(
            "       $RUSTUP_TOOLCHAIN names stock \"{channel}\", which moves only rustup's proxies, never {tool}:\n       the Trust spellings above run the Trust toolchain here too.\n"
        ),
    }
}

/// The DIRECT row's one line: what runs, then the escape.
#[must_use]
pub fn direct_announcement(upstream: &str, branded: &str, upstream_found: bool) -> String {
    let mut line = format!("aterm: running {branded} in place of '{upstream}'.");
    if upstream_found {
        line.push(' ');
        line.push_str(&escape_clause(upstream));
    }
    line
}

/// The SIGNPOST announcement, the caller's arguments filled into every lane.
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

/// A SIGNPOST row with no upstream copy on PATH.
#[must_use]
pub fn no_upstream_message(row: &Row, lanes: &[Lane], args: &[String]) -> String {
    signpost_block(row, lanes, args, Head::NotOnPath, None, None, None)
}

/// The signpost for `<upstream> +<stock channel> …` on any Rust row (`cargo +1.97.1 test`, `rustfmt +1.97.1 x.rs`).
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

const fn lanes_of(row: &Row) -> &'static [Lane] {
    match row.policy {
        Policy::Signpost { lanes, .. } => lanes,
        Policy::Direct { .. } | Policy::Oracle { .. } => &[],
    }
}

/// The refusal inside a Trust source checkout at `root`: `cargo`/`rustc` resolved to something that is not Trust, so nothing ran.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Head<'a> {
    Upstream,
    Toolchain(&'a str),
    Stock(&'a str),
    NotOnPath,
}

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
        let verb = (upstream == "cargo")
            .then(|| rust_lane::cargo_verb_index(args))
            .flatten()
            .map_or("", |i| args[i].as_str());
        match spellings.as_slice() {
            [_] if rust_lane::is_unknown_verb(&spellings) => {
                out.push_str(&head_line(verb, " (no proof claim)"));
                out.push_str(&format!(
                    "; targo is not known to have `{verb}`\n       (a cargo extension targo lacks has no Trust spelling; a cargo alias takes its expansion's lane). If it has, it is:\n"
                ));
            }
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
    if let Some(what) = resolved {
        let real = what.real.display();
        match refused_in {
            Some(_) => out.push_str(&format!(
                "       ('{upstream}' here is {real}; `aterm --no-reroute` runs it as named.)\n"
            )),
            None => out.push_str(&format!("       ('{upstream}' here is {real}.)\n")),
        }
    }
    let tool = trust_tool.split(' ').next().unwrap_or(branded);
    if let Head::Stock(_) = head {
        out.push_str(&format!(
            "       A stock channel, named here or pinned, moves only rustup's proxies, never {tool}.\n"
        ));
    }
    if let Some(pin) = pin.filter(|_| is_rust_row(row)) {
        out.push_str(&pin_lines(pin, tool));
    }
    if head == Head::NotOnPath {
        out.pop();
    } else if refused_in.is_some() {
        // A refusal is never silenced, so the setting that silences a signpost is not named.
        out.push_str(MEASURING_LINE);
    } else {
        out.push_str(&signpost_closing(is_rust_row(row)));
    }
    out
}

/// The ORACLE refusal, naming the upstream copy's own path when there is one.
#[must_use]
pub fn oracle_message(upstream: &str, upstream_path: Option<&Path>) -> String {
    let reach = match upstream_path {
        Some(path) => format!(
            "run {} to reach the real {upstream} (`aterm --no-reroute` lifts this)",
            path.display()
        ),
        None => format!("no upstream '{upstream}' is on PATH"),
    };
    format!("aterm: '{upstream}' does not run by name in an aterm session — {reach}.")
}

/// The ORACLE row's line before upstream runs: what runs, and the tool this toolchain prefers — never a refusal.
#[must_use]
pub fn oracle_notice(upstream: &str, branded: &str, fix: Option<&str>) -> String {
    format!(
        "aterm: running upstream '{upstream}'; on this toolchain the solver is '{branded}'{}.",
        fix_clause(fix)
    )
}

/// The ORACLE row with no upstream copy on PATH: nothing ran, and the preferred tool — with `fix` as in [`oracle_notice`].
#[must_use]
pub fn oracle_not_on_path_message(upstream: &str, branded: &str, fix: Option<&str>) -> String {
    format!(
        "aterm: upstream '{upstream}' is not on PATH; on this toolchain the solver is '{branded}'{}.",
        fix_clause(fix)
    )
}

fn fix_clause(fix: Option<&str>) -> String {
    fix.map_or_else(String::new, |f| format!(" ({f})"))
}

fn oracle_fix(install: &Install, branded: &str) -> Option<String> {
    match install {
        Install::Runs(_) | Install::Shim(_) | Install::Pending(_) => None,
        Install::ShimLost => Some("`aterm pkg repair` restores its shim".to_string()),
        Install::Absent => Some(format!("`aterm pkg install {branded}` installs it")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unrunnable {
    Absent,
    ShimLost,
}

impl Unrunnable {
    fn refusal(self, upstream: &str, branded: &str, program: &str) -> String {
        match self {
            Self::Absent => not_installed_message(upstream, branded, program),
            Self::ShimLost => lost_shim_message(upstream, branded),
        }
    }

    fn fallback(self, upstream: &str, branded: &str, program: &str) -> String {
        match self {
            Self::Absent => fallback_not_installed_message(upstream, branded, program),
            Self::ShimLost => fallback_lost_shim_message(upstream, branded),
        }
    }
}

/// A DIRECT row whose branded tool is not installed, upstream running instead.
#[must_use]
pub fn fallback_not_installed_message(upstream: &str, branded: &str, program: &str) -> String {
    format!(
        "aterm: running upstream '{upstream}' — {branded} is not installed (`aterm pkg install {program}` installs it)."
    )
}

/// A DIRECT row whose branded tool's program is still to come (a pending stub stands at its name, [`Install::Pending`]), upstream running instead of a wait for it.
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

/// `cargo trust <verb>` / `cargo --unverified <verb>`.
pub const EXPLICIT_LANE_VERBS: &[&str] = &["trust", "--unverified"];

/// The program `aterm pkg install` names for `targo`, the tool an explicit lane runs.
pub const EXPLICIT_LANE_PROGRAM: &str = "trust";

/// Cargo queries that take no lane: an installed `targo` answers them with no line.
pub const LANE_FREE_QUERIES: &[&str] = &["metadata", "locate-project", "version", "--version"];

/// The one line printed when `cargo trust …`/`cargo --unverified …` runs `targo`.
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

/// A branded tool its program's live build holds, with its shim gone.
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

/// Printed by a strict stub ([`crate::config::RerouteConfig::strict`]) when atpkg is unreachable.
#[must_use]
pub fn unreachable_message(upstream: &str) -> String {
    format!(
        "aterm: '{upstream}' did not run — aterm's package manager is missing (`aterm --no-reroute` restores upstream '{upstream}')"
    )
}

/// Printed by the stub itself (the default) when atpkg is unreachable and an upstream copy is on PATH: it runs, after this line.
#[must_use]
pub fn unreachable_fallback_message(upstream: &str) -> String {
    format!(
        "aterm: running upstream '{upstream}' — aterm's package manager is not reachable (`aterm pkg repair` re-lays this stub)."
    )
}

/// Printed by the stub itself (the default) when atpkg is unreachable and no upstream copy is on PATH either: nothing exists to run.
#[must_use]
pub fn unreachable_not_on_path_message(upstream: &str) -> String {
    format!(
        "aterm: '{upstream}' did not run — aterm's package manager is not reachable and no upstream '{upstream}' is on PATH."
    )
}

/// Printed by a `cargo`/`rustc` stub ([`names_what_runs`]) when atpkg is unreachable inside a Trust source checkout ([`sh_source_checkout_test`]).
#[must_use]
pub fn unreachable_in_checkout_message(upstream: &str) -> String {
    format!(
        "aterm: '{upstream}' did not run — aterm's package manager is not reachable, so what '{upstream}' resolves to is unknown, and this is a Trust source checkout, where only Trust may build. `aterm pkg repair` re-lays this stub; `aterm --no-reroute` runs upstream '{upstream}'."
    )
}

/// `atpkg __reroute` with no resolvable store (no `HOME`, an `env -i` wrapper) and an upstream copy on PATH.
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

/// `atpkg __reroute` with no resolvable store under `[reroute] strict = true`: nothing ran, and the escape.
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

/// The row's policy as one clause, `<args>` standing for the caller's arguments.
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
                s.push_str(" for build/check/test, 'targo --unverified <args>' for a verb with no verified lane (run, doc, install, …), 'targo <args>' for one that takes no lane (tree, clean, …); ");
                let verbs: Vec<String> = CARGO_VERB_REROUTES
                    .iter()
                    .map(|(verb, reroute)| format!("'{upstream} {verb}' → '{reroute}'"))
                    .collect();
                s.push_str(&verbs.join(", "));
                s.push_str("; 'cargo trust <args>' / 'cargo --unverified <args>' run targo after one stderr line, and 'cargo metadata' / 'locate-project' / 'version' / '--version' run an installed targo with no line");
            }
            s.push_str(&stock_channel("names the same Trust spelling"));
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

fn sh_skip_a_stub(indent: &str, program: &str) -> String {
    let mut s = String::from(indent);
    s.push_str(&format!(
        "if [ -r \"$__d/{program}\" ] && {{ IFS= read -r __aterm_l; IFS= read -r __aterm_l; }} 2>/dev/null < \"$__d/{program}\" && [ \"$__aterm_l\" = "
    ));
    s.push_str(&crate::stub::sh_single_quote(STUB_MARKER));
    s.push_str(" ]; then continue; fi\n");
    s
}

fn sh_upstream_walk(indent: &str, before_exec: &[String]) -> String {
    sh_path_walk(indent, "$__aterm_name", "", before_exec)
}

/// [`sh_upstream_walk`] for any `program` (a word of `sh`, placed after `$__d/`), exec'd with `args` (`sh` text, empty or ending in a space) ahead of the caller's `"$@"`.
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

const SH_WALK_DEPTH: u32 = 64;

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

/// The POSIX stub for `upstream`.
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
    let compile = if CARGO_COMPILERS.contains(&upstream) {
        format!(
            " || {{ [ -n \"${{{CARGO_CRATE_ENV}:-}}\" ] && [ -z \"${{{TARGO_CRATE_ENV}:-}}\" ]; }}"
        )
    } else {
        String::new()
    };
    let mark = format!("{ANNOUNCED_ENV}=1; export {ANNOUNCED_ENV}");
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
        s.push_str(&sh_source_checkout_test(""));
        s.push_str(&sh_stage0_names_trust(""));
        s.push_str("if [ -n \"$__aterm_trust\" ]; then\n  ");
        s.push_str(&say(unreachable_in_checkout_message(upstream)));
        s.push_str("\n  exit ");
        s.push_str(&REFUSAL_EXIT.to_string());
        s.push_str("\nfi\n");
    }
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

/// The agent programs' stub: inside an aterm session it execs the managed twin, elsewhere the next copy on PATH.
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
    s.push_str("\nif [ -n \"");
    s.push_str(&crate::hooks::agents_markers_sh());
    s.push_str("\" ] && [ -x \"$__aterm_agents/$__aterm_name\" ] && [ ! -d \"$__aterm_agents/$__aterm_name\" ]; then\n");
    s.push_str("  exec \"$__aterm_agents/$__aterm_name\" \"$@\"\n");
    s.push_str("fi\n");
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
    s.push_str(&sh_skip_a_stub("    ", "$__aterm_name"));
    s.push_str("    IFS=$__aterm_ifs; set +f\n");
    s.push_str("    exec \"$__d/$__aterm_name\" \"$@\"\n");
    s.push_str("  fi\n");
    s.push_str("done\n");
    s.push_str("IFS=$__aterm_ifs; set +f\n");
    s.push_str("if [ -x \"$__aterm_agents/$__aterm_name\" ] && [ ! -d \"$__aterm_agents/$__aterm_name\" ]; then\n");
    s.push_str("  printf '%s\\n' \"");
    s.push_str(&agents_not_found_message("$__aterm_name"));
    s.push_str("\" 1>&2\nelse\n");
    s.push_str("  printf '%s\\n' \"");
    s.push_str(&agents_not_installed_message("$__aterm_name"));
    s.push_str("\" 1>&2\nfi\nexit 127\n");
    s
}

/// The agents stub's line outside aterm when no copy is on PATH and the managed twin stands.
#[must_use]
pub fn agents_not_found_message(name: &str) -> String {
    format!("aterm: no '{name}' on PATH — run: aterm {name}")
}

/// The agents stub's line when no copy is on PATH and no managed twin stands.
#[must_use]
pub fn agents_not_installed_message(name: &str) -> String {
    format!("aterm: '{name}' is not installed — run: aterm pkg install {name}")
}

/// The agent programs that get a reroute stub.
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

/// What an agents stub reads besides PATH.
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

/// Whether the agents stub for `name` execs the managed twin under `env`.
#[must_use]
pub fn agents_stub_takes_twin(layout: &Layout, name: &str, env: StubEnv) -> bool {
    env.in_aterm
        && crate::store::ToolName::new(name).is_some_and(|tool| is_twin(&layout.agent_shim(&tool)))
}

/// Our agents stub for `name` ([`stub_path`]) when it is the first executable `name` on `path_var`, the copy a shell with that PATH runs.
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

/// `path_var` with every reroute directory ([`is_reroute_dir`], any spelling) taken out.
#[must_use]
pub fn without_reroute_dirs(path_var: Option<&OsStr>) -> Option<std::ffi::OsString> {
    let kept: Vec<PathBuf> = std::env::split_paths(path_var?)
        .filter(|dir| dir.as_os_str().is_empty() || !is_reroute_dir(dir))
        .collect();
    std::env::join_paths(kept).ok()
}

/// `path_var` as an agents stub's pass-through walks it.
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

/// Whether `path` is one of our stubs: a regular file (never a symlink), small, UTF-8, whose second line is [`STUB_MARKER`].
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

/// Whether `dir` is a reroute directory — ours, under any spelling — by its marker file.
#[must_use]
pub fn is_reroute_dir(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(DIR_MARKER_FILE)).is_ok_and(|meta| meta.is_file())
}

/// What doctor reports per row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StubState {
    Laid,
    Missing,
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

/// Lay (or refresh) every row's stub.
pub fn lay(layout: &Layout) -> io::Result<()> {
    lay_with(
        layout,
        &crate::stub::Embedder::this_process(),
        crate::config::load_reroute().strict(),
    )
}

/// [`lay`], with the atpkg on offer and the temporary directories `embedder`'s.
pub(crate) fn lay_with(
    layout: &Layout,
    embedder: &crate::stub::Embedder,
    strict: bool,
) -> io::Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    if layout.declined().is_file() {
        return Ok(());
    }
    let dir = dir(layout);
    layout.ensure_dir(&dir)?;
    let marker = dir.join(DIR_MARKER_FILE);
    if marker_needs_lay(std::fs::read(&marker).ok().as_deref()) {
        crate::lay::write_regenerable(&crate::lay::Executable::new(&marker, marker_body()))?;
    }
    let mut files = Vec::new();
    let wanted = wanted_stubs(layout, embedder, strict);
    for (name, body) in &wanted {
        let path = dir.join(name);
        match std::fs::symlink_metadata(&path) {
            Err(_) => {}
            Ok(_) if is_reroute_stub(&path) => {
                if std::fs::read(&path).is_ok_and(|have| have == body.as_bytes()) {
                    continue;
                }
            }
            Ok(_) => continue,
        }
        files.push(crate::lay::Executable::new(&path, body.as_bytes()));
    }
    files.iter().try_for_each(crate::lay::write_regenerable)?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(OsStr::to_str) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            if !wanted.iter().any(|(w, _)| *w == name) && is_reroute_stub(&path) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    let _ = crate::abrew::stub::lay_with(layout, embedder);
    Ok(())
}

/// What [`lay_with`] would write or sweep now, by name.
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

fn marker_body() -> String {
    let mut body = String::from(STUB_MARKER);
    body.push('\n');
    body
}

/// Whether [`lay`] writes the marker file, given what stands there now (`None`: absent or unreadable).
pub(crate) fn marker_needs_lay(have: Option<&[u8]>) -> bool {
    have != Some(marker_body().as_bytes())
}

/// Remove every stub that is ours; the directory goes too once it is empty.
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

/// `atpkg __reroute <upstream> [args…]`: what a stub execs once its `sh`-side escape has not fired.
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
    let unserved = std::cell::OnceCell::new();
    let machine = Machine {
        passthrough: engaged(std::env::var(PASSTHROUGH_ENV).ok().as_deref()),
        announced: engaged(std::env::var(ANNOUNCED_ENV).ok().as_deref()),
        announce: &|| {
            crate::config::cached_reroute().announce()
                && unserved
                    .get_or_init(|| unserved_here(layout, branded_of(row), program_of(row)))
                    .is_none()
        },
        strict: &|| crate::config::cached_reroute().strict(),
        upstream: &|| upstream_on_path(layout, upstream, path_var.as_deref()),
        branded: &|branded, program| installed(layout, branded, program),
        pin: &|| Pin::for_dir(cwd.as_deref(), env_toolchain.as_deref()),
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
            let copy = Managed {
                target: shim,
                env: crate::shim_env::ShimEnv::NONE,
            };
            exec_branded(layout, &copy, &args)
        }
    }
}

type Decide<'a> = dyn Fn(&[String], crate::build_cache::Tool) -> Option<Vec<String>> + 'a;

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

struct Machine<'a> {
    passthrough: bool,
    announced: bool,
    announce: &'a dyn Fn() -> bool,
    strict: &'a dyn Fn() -> bool,
    upstream: &'a dyn Fn() -> Option<PathBuf>,
    branded: &'a dyn Fn(&str, &str) -> Install,
    pin: &'a dyn Fn() -> Option<Pin>,
    resolve: &'a dyn Fn(&Path, Option<&str>) -> Resolved,
    trust_checkout: &'a dyn Fn() -> Option<PathBuf>,
}

fn names_what_runs(row: &Row) -> bool {
    matches!(row.policy, Policy::Signpost { .. }) && is_rust_row(row)
}

#[derive(Debug, PartialEq, Eq)]
enum Install {
    Runs(Managed),
    Shim(PathBuf),
    Pending(PathBuf),
    ShimLost,
    Absent,
}

/// Why `branded`, one of the tools `program` ships, cannot be had on this machine.
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

use crate::ops::{Managed, managed};

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

#[derive(Debug, PartialEq, Eq)]
enum Plan {
    Upstream {
        say: Option<String>,
        target: PathBuf,
        args: Vec<String>,
    },
    Branded {
        say: String,
        copy: Managed,
        args: Vec<String>,
    },
    Quiet {
        copy: Managed,
        args: Vec<String>,
    },
    Refuse {
        say: String,
    },
    Shim {
        shim: PathBuf,
        args: Vec<String>,
    },
    Missing {
        say: String,
    },
}

fn plan(row: &Row, args: &[String], machine: &Machine<'_>) -> Plan {
    let upstream = row.upstream;
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
    let toolchain = if is_rust_row(row) {
        explicit_toolchain(args)
    } else {
        None
    };
    let shown = if toolchain.is_some() {
        &args[1..]
    } else {
        args
    };
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
        Policy::Direct {
            branded,
            program,
            source_verb,
        } => {
            let install = (machine.branded)(branded, program);
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
            if upstream == "cargo"
                && shown
                    .first()
                    .is_some_and(|verb| EXPLICIT_LANE_VERBS.contains(&verb.as_str()))
            {
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
        Policy::Oracle { branded, .. } => {
            if (machine.strict)() {
                return Plan::Refuse {
                    say: oracle_message(upstream, (machine.upstream)().as_deref()),
                };
            }
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

/// The first upstream copy of `name` on PATH.
#[must_use]
pub fn upstream_on_path(layout: &Layout, name: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    upstream_on_path_in(Some(&layout.prefix), name, path_var)
}

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

/// `atpkg __reroute` when no store resolves (no `HOME`, an `env -i` wrapper).
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

fn exec_upstream(target: &Path, args: &[String]) -> ExitCode {
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

fn exec_branded(layout: &Layout, copy: &Managed, args: &[String]) -> ExitCode {
    let child_path =
        crate::store::append_bin_to_path(std::env::var_os("PATH").as_deref(), &layout.bin_dir());
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
