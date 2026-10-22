// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg doctor` (§15): a no-network, no-mutation diagnostic over atpkg's store, ops,
//! signature and status primitives.
//!
//! Structural breakage (a broken bin shim, an active build whose store tree vanished, a
//! fish-breaking stray `.sh`, a world-writable login-sourced dir) is a problem: nonzero exit.
//! Advisory findings (bin not yet on PATH, a frozen-looking index, a foreign sysroot wiring)
//! are warnings: exit 0. A managed tool that cannot run (tippy refused by its own Trust
//! bundle), or that doctor cannot show to run, is neither: the report ends "not healthy",
//! names the fix, and still exits 0. Doctor parses no unverified index or manifest: its
//! freshness comes from atpkg's own `status.toml` and the durable [`crate::sig::Floor`].

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

// (10h) reports by aterm-primer's own table and rules, so it says only what a window start
// does.
use aterm_primer::{
    CLAUDE_COMPACT_WINDOW_KEY, CLAUDE_DEFAULTS, CLAUDE_DRAFTS_KEY, CLAUDE_EFFORT_DEFAULTS,
    CLAUDE_SURVEY_SWITCH, CLAUDE_TOP_EFFORT_KEY, claude_compact_offers_kept_in,
    claude_efforts_set_by_hand, claude_survey_rate_set_by_hand,
};

use aterm_converge::{Class, ItemReport, Outcome, Remedy};

use crate::converge::{self as c, Finding};
use crate::store::Layout;

const GIB: u64 = 1 << 30;

/// Whether a recorded program `state` is a fault this machine should act on, by the prefixes
/// the install and update paths write. Two need a word:
///
/// * `aborted: <phase>` (cli.rs, `TxnOutcome::Aborted`): a coherence-group transaction
///   killed mid-flight, recorded against the failed member only (on a group disk
///   shortfall, an arbitrary one), so the row names where it stopped.
/// * `tombstoned: pin yanked/below floor`: the shims are failing stubs
///   ([`crate::activate::install_tombstone_shim`]) that the broken-shim scan skips and
///   `active_builds` drops, so without this row doctor would call a stubbed compiler healthy.
///
/// Not a fault: `active`, `dev-linked (skipped)`, `held: …` (a tuple whose new pin is not
/// published here), and any unrecognized state (allow-by-prefix).
fn is_problem_state(state: &str) -> bool {
    state.starts_with("error:")
        || state.starts_with("unavailable:")
        || state.starts_with("blocked:")
        || state.starts_with("aborted:")
        || state.starts_with("tombstoned:")
        // A member held after a failed stage (`held: last attempt failed with …`) is not
        // installed and no unattended pass will install it. The group hold (`held: pinned
        // build …`) is benign.
        || state.starts_with(crate::state::HELD_FAILED_PREFIX)
}

/// Where the per-problem listing starts, or `None` when no verdict owns it. A declined store
/// lists nothing: the decline is the verdict, and a stale fault row describes a toolset the
/// user removed.
fn problem_listing_start(declined: bool, store_empty: bool, problems: usize) -> Option<usize> {
    if declined || problems == 0 {
        None
    } else if store_empty {
        // That verdict names its first reason inline; listing resumes after it.
        Some(1)
    } else {
        Some(0)
    }
}

/// The programs this machine wants but does not have: `(unexplained, on_purpose,
/// dev_linked)`, each alphabetical. "Wants" is [`crate::cli::wanted_programs`], the question
/// the install lanes ask. A program on the removed ledger is the user's decision, not a
/// problem; one missing with no ledger entry has no shim, row or fault for any other check
/// to see.
///
/// Offline and fail-quiet: with no cached index there is no trustworthy answer, and inventing
/// one would turn an unreachable network into phantom missing programs.
fn missing_against_index(
    layout: &Layout,
    installed: &std::collections::BTreeMap<String, u64>,
    dev_linked: &std::collections::BTreeSet<String>,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let Some(index) = crate::cli::cached_index(layout) else {
        return (Vec::new(), Vec::new(), Vec::new());
    };
    split_missing(
        &crate::cli::wanted_programs(&index, crate::config::cached()),
        installed,
        &layout.removed_programs(),
        dev_linked,
    )
}

/// Development links are explicit provisioning too, without a signed build number.
/// Inspect the actual shim destinations: a marker alone must not hide an empty,
/// deleted, non-executable, or redirected checkout from the health report.
fn live_dev_links(layout: &Layout) -> (std::collections::BTreeSet<String>, Vec<String>) {
    crate::linkmode::live_dev_links(layout)
}

/// The decision [`missing_against_index`] makes, without the I/O: which wanted programs are
/// absent, and which of those the removed ledger or a dev link accounts for.
fn split_missing(
    wanted: &std::collections::BTreeSet<String>,
    installed: &std::collections::BTreeMap<String, u64>,
    removed: &std::collections::BTreeSet<String>,
    linked: &std::collections::BTreeSet<String>,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut unexplained = Vec::new();
    let mut on_purpose = Vec::new();
    let mut dev_linked = Vec::new();
    for program in wanted {
        if installed.contains_key(program) {
            continue;
        }
        // A dev-linked program is present (its shims are in the managed bin/ and run) but
        // has no store build for `installed` to count. It outranks the removed ledger: a
        // link made after a removal is the newer decision.
        if linked.contains(program) {
            dev_linked.push(program.clone());
        } else if removed.contains(program) {
            on_purpose.push(program.clone());
        } else {
            unexplained.push(program.clone());
        }
    }
    (unexplained, on_purpose, dev_linked)
}

/// Whether one `status.toml` row is a fault [`recorded_problems`] lists: a real program (a
/// `-`-prefixed key is a stray row) in a problem state ([`is_problem_state`]). Public so the
/// window's Settings ▸ Packages badge counts the doctor's own list.
#[must_use]
pub fn is_recorded_problem(name: &str, state: &str) -> bool {
    !name.starts_with('-') && is_problem_state(state)
}

/// Every recorded fault, as `"<program>: <state>"`, alphabetical by program. Stray rows
/// (keys beginning with `-`) are not programs and are excluded.
pub(crate) fn recorded_problems(status: Option<&crate::Status>) -> Vec<String> {
    status
        .map(|s| {
            s.programs
                .iter()
                .filter(|(name, p)| is_recorded_problem(name, &p.state))
                .map(|(name, p)| format!("{name}: {}", p.state))
                .collect()
        })
        .unwrap_or_default()
}

/// The toolset-wide "no build for this architecture" verdict, on the platforms where it
/// is a state and not a fault: every one but macOS (Windows and Linux). `Some(names)` —
/// the ALab programs whose own rows say `unavailable on <target>: …` — when those rows or
/// the `*toolset*` row read that verdict here; `None` on macOS, whatever the rows say.
///
/// Why the platform split. On an Intel Mac the absence of an x86_64 build is a
/// publishing gap the report is right to call a PROBLEM and to answer with `aterm pkg
/// install --default-set`: the seal serves that machine the day it is cut. On Windows
/// (index build 44) and on Linux (index build 71) every ALab row is `prebuilt-only` with
/// no artifact for the target and only the agents are published, so `install
/// --default-set` cannot change the state and a PROBLEM line recommending it would be
/// false — `install` itself says "skipped, not failed". The `*toolset*` row is also
/// STALE by construction: it is written when a pass installs nothing on an empty store
/// and cleared only by a clean pass with something installed, so it outlives the day
/// the agents became servable here.
fn toolset_unserved_here(status: Option<&crate::Status>) -> Option<Vec<String>> {
    if cfg!(target_os = "macos") {
        return None;
    }
    let s = status?;
    // The members' own `unavailable` rows are the durable signal: the `*toolset*` row is
    // written only by a pass that installed nothing on an empty store and cleared by the
    // first clean pass that installs something, so once `claude`/`codex` land it is gone
    // while the other members stay unserved.
    let names: Vec<String> = s
        .programs
        .iter()
        .filter(|(name, p)| {
            !name.starts_with('*') && p.state.starts_with(crate::state::UNAVAILABLE_PREFIX)
        })
        .map(|(name, _)| name.clone())
        .collect();
    let toolset_row_says = s
        .programs
        .get("*toolset*")
        .is_some_and(|row| crate::state::is_unserved_toolset(&row.state));
    if names.is_empty() && !toolset_row_says {
        return None;
    }
    Some(names)
}

/// Whether no managed Trust can be had on this target: trust's own row reads `unavailable on
/// <target>` where that is a state, not a fault ([`toolset_unserved_here`]: never on macOS,
/// where the seal serves the machine and a local `trust` stays a warning).
fn trust_unserved_here(layout: &Layout) -> bool {
    toolset_unserved_here(crate::status::read(layout).as_ref())
        .is_some_and(|unserved| unserved.iter().any(|name| name == "trust"))
}

/// [`split_missing`]'s "unexplained" list with the unserved members taken out: on an
/// unserved platform each of them IS explained — by its own `unavailable on <target>` row,
/// which the note names. Identity when `unserved` is `None` (macOS).
fn drop_unserved(missing: Vec<String>, unserved: Option<&[String]>) -> Vec<String> {
    match unserved {
        Some(unserved) => missing
            .into_iter()
            .filter(|p| !unserved.contains(p))
            .collect(),
        None => missing,
    }
}

/// Whether a recorded problem (`"<program>: <state>"`) is the toolset-wide "no build for this
/// architecture" verdict ([`crate::state::is_unserved_toolset`]), the one `*toolset*` row
/// [`toolset_unserved_here`] turns into a note. Every other `*toolset*` verdict (`index
/// unreachable`, `every published build was refused`, …) is about this machine and stays a
/// problem everywhere.
fn is_unserved_toolset_problem(why: &str) -> bool {
    why.strip_prefix("*toolset*: ")
        .is_some_and(crate::state::is_unserved_toolset)
}

/// Whether a recorded problem is a member held after a failed stage
/// ([`crate::state::HELD_FAILED_PREFIX`]), which no unattended pass moves.
fn is_held_problem(why: &str) -> bool {
    why.split_once(": ")
        .is_some_and(|(_, state)| state.starts_with(crate::state::HELD_FAILED_PREFIX))
}

/// The first member the recorded problems say is held ([`is_held_problem`]), wherever it
/// sorts: a `*toolset*` row precedes every program in `BTreeMap` order, so on an empty store
/// the held member is never the first problem.
fn first_held_member(recorded_problems: &[String]) -> Option<&str> {
    recorded_problems
        .iter()
        .filter(|why| is_held_problem(why))
        .find_map(|why| why.split_once(": ").map(|(program, _)| program))
}

/// The explicit door for a member held after a failed stage: `aterm pkg install <p>`, which
/// forgets its memo and lands it, or, for a coherence-group member (`grouped`), `aterm pkg
/// update --retry`, which forgets every memo and moves or completes the tuple whole;
/// `install` would land that one member outside its tuple.
fn held_door(program: &str, grouped: bool) -> String {
    if grouped {
        String::from("aterm pkg update --retry")
    } else {
        format!("aterm pkg install {program}")
    }
}

/// Whether the cached index (offline) puts `program` in a coherence group; `false` with no
/// cached index, where nothing says it is.
fn in_coherence_group(layout: &Layout, program: &str) -> bool {
    crate::cli::cached_index(layout)
        .and_then(|index| index.program(program).map(|p| p.coherence_group.is_some()))
        .unwrap_or(false)
}

/// Whether an empty store on an unserved platform is the quiet state (the note, "none is
/// missing", healthy): only when nothing is recorded against a member the index serves here
/// and no served member is missing without a row. A network failure writes no per-program
/// row (`failed_install_state`), so an empty problem list alone does not prove every served
/// member arrived.
fn unserved_store_is_quiet(
    unserved: bool,
    recorded_problems: &[String],
    missing_unexplained: &[String],
) -> bool {
    unserved && recorded_problems.is_empty() && missing_unexplained.is_empty()
}

/// The one act for an empty store on an unserved platform: the explicit door of the
/// first member whose row records a failure or a hold (`aterm pkg install <p>` — which
/// also forgets a digest refusal), else of the first served member missing without a
/// row. `None` when the verdict is a toolset-wide fault (`*toolset*: unavailable: index
/// unreachable`), which is not about this platform: the caller's ordinary act answers it.
/// Never the whole-set install for the unserved condition itself, which cannot serve what
/// is not published.
fn unserved_platform_next(
    recorded_problems: &[String],
    missing_unexplained: &[String],
) -> Option<String> {
    let program = match recorded_problems.first() {
        Some(first) => first.split_once(": ")?.0,
        None => missing_unexplained.first()?.as_str(),
    };
    (!program.starts_with('*')).then(|| format!("aterm pkg install {program}"))
}

/// What the workspace the operator is in demands of the installed Trust toolchain, as the
/// installed `targo` decides it (`targo locate-project --workspace` parses the manifest,
/// compiles nothing, and refuses a `[trust]` policy key it does not know). A workspace
/// committed against a local Trust seal newer than any atpkg published fails on every
/// managed machine with an "unknown field" error that reads like a typo; this check names
/// the toolchain that is behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspacePolicy {
    /// The installed targo accepts the workspace's `[trust]` policy.
    Accepted,
    /// The installed targo refuses a policy key it does not know: the workspace needs a
    /// newer Trust build than atpkg has published.
    UnknownField { field: String },
    /// The installed targo refuses a policy key it has retired, naming the setting that
    /// replaced it (`replacement`, targo's words after "was"): the workspace is older than
    /// the installed Trust build, and the cure is the workspace's, not this machine's.
    RetiredKey { key: String, replacement: String },
    /// The probe could not run (no targo in the store, spawn failure); reported, never
    /// counted as a problem.
    ProbeFailed { why: String },
}

/// The workspace probe's result, bound to the directory it was taken in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePolicyProbe {
    pub workspace: PathBuf,
    pub policy: WorkspacePolicy,
}

/// rustup's `trust` channel resolving outside the atpkg store: a locally built or sealed
/// toolchain. Newer than the store's, it is a publisher's seal: anything committed against a
/// feature only it has will not build on an atpkg-managed machine until the seal is
/// published. Older or gone, it is stale, and `repair` re-points it at the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSealProbe {
    /// The rustup entry itself (`<rustup_home>/toolchains/trust`).
    pub link: PathBuf,
    pub link_target: PathBuf,
    /// `trustc -Vv`'s commit hash (first 10 characters), or "unknown".
    pub trustc: String,
    /// Set when it is gone or older than the store's build
    /// ([`crate::seam::stale_against_store`]).
    pub stale: Option<crate::seam::Stale>,
}

/// The environment-dependent probes `run` takes and `run_with` only reports, so the
/// reporting surface stays testable without spawning targo or reading rustup state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probes {
    pub workspace: Option<WorkspacePolicyProbe>,
    pub local_seal: Option<LocalSealProbe>,
    /// Whether packages update here on their own: the manager armed and `[packages] enabled`
    /// on ([`crate::config::PackagesConfig::enabled`], default on). With [`Self::excluded`],
    /// what the never-checked line may claim of the vendor programs.
    pub automatic: bool,
    /// The programs `[packages] exclude` names: never updated here, by any lane.
    pub excluded: Vec<String>,
    /// What the `[packages]` table's own spelling asks of its owner (a retired key to rename
    /// or remove, [`crate::config::PackagesConfig::config_notes`]), one `note` line each.
    pub config_notes: Vec<String>,
    /// A `[packages] prefix` this shipped build does not use, naming another store
    /// ([`crate::config::PackagesConfig::ignored_prefix`]): a `warn`, because nothing is
    /// installed unattended until the line goes.
    pub ignored_prefix: Option<PathBuf>,
    /// Retired environment opt-outs still exported to this process, `(name, the setting that
    /// replaced it)` ([`retired_opt_outs_exported`]): a `warn` each, because the person who
    /// set one believes something is off that is not.
    pub retired_env: Vec<(&'static str, &'static str)>,
    /// What an agent program's reroute stub reads besides PATH, as the shell that ran this
    /// report sees it ([`crate::reroute::StubEnv::of_process`]): (10d) says what `claude`
    /// typed there does, and the stub decides at exec time on exactly these. The default
    /// is a shell outside aterm.
    pub stub_env: crate::reroute::StubEnv,
    /// Whether this store's index source is the one the channel-head probe watches
    /// ([`crate::index_probe::probes_this_source`]): then (8) reads the probe's cached
    /// answer back and says whether a newer index is published. Default `false` — a
    /// private or repointed registry has no probe, so there is nothing to report.
    pub index_head: bool,
    /// The C toolchain's verdict ([`crate::prereq::probe`]), or `None` when this report
    /// does not probe it (the default: a fixture never meets the machine's compiler).
    pub cc: Option<crate::prereq::CcVerdict>,
    /// The Claude Code defaults the owner runs with, as `~/.claude/settings.json`
    /// leaves each — or why the window adds none to that file
    /// ([`claude_defaults_state`]) — or `None` when this report does not read that
    /// file (the default: a fixture has no Claude settings).
    pub claude_defaults: Option<Result<Vec<(&'static str, DefaultState)>, String>>,
    /// Whether a window adds those defaults at all: `agents_auto_prime`
    /// ([`crate::config::parse_agents_auto_prime`]), default true. Off, an unset key stays
    /// unset and (10h) says so instead of promising the window's write.
    pub claude_auto_prime: bool,
    /// Whether this aterm is on macOS's Developer Tools list ([`crate::devtools::facts`]),
    /// or `None` when this report does not read it (the default: a fixture reads no
    /// machine, and off macOS there is no list).
    pub devtools: Option<crate::devtools::Facts>,
    /// Rosetta 2 as read ([`crate::machine::rosetta_posture`]) and whether `[machine]
    /// rosetta` wants it, or `None` when this report does not read it (the default: a
    /// fixture reads no machine).
    pub rosetta: Option<(crate::machine::RosettaPosture, bool)>,
    /// The directories on the active Trust build's physical chain that `targo trust`
    /// refuses ([`probe_trust_chain`]), or `None` when this report does not walk it (the
    /// default: a fixture's store sits under a temp dir whose own chain is the machine's).
    pub trust_chain: Option<TrustChainProbe>,
    /// The rc the account's login shell reads ([`crate::hooks::account_login_rc`]: the
    /// account record's shell, never `$SHELL`), the one rc a pass creates when it is absent,
    /// so (6b) says what becomes of it; `None` when it reads no rc atpkg wires (the
    /// default: a fixture's home has no login shell).
    pub login_rc: Option<&'static str>,
}

/// What [`probe_trust_chain`] found on the active Trust build's chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustChainProbe {
    /// The trust build whose `bin/` and `lib/` were walked.
    pub build: u64,
    /// Each refused directory, top down, once.
    pub refused: Vec<ChainRefusal>,
}

/// A directory `targo trust` refuses on a toolchain's chain, and why
/// ([`chain_component_refusal`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainRefusal {
    pub dir: PathBuf,
    pub why: ChainWhy,
}

/// Why `targo trust` refuses a chain component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainWhy {
    /// Group- or other-writable: `mode & 0o022 != 0`, a sticky bit no exemption. `yours`:
    /// the invoking user owns it, so `chmod go-w` is theirs to run; root's (`/tmp`'s
    /// `1777`, a `0775` mount point) is not, and must never be narrowed.
    Writable { mode: u32, yours: bool },
    /// Owned by neither root nor the invoking user.
    Foreign { owner: u32, ours: u32 },
    /// A runtime-library directory below the sysroot (`lib/`, `lib/rustlib`,
    /// `lib/rustlib/<target>`, its `lib/`) that is not a plain directory: targo refuses
    /// every redirection there, wherever it points.
    Redirected,
}

/// `targo trust`'s rule for one component of the physical chain from `/` to a verified
/// toolchain's launchers (`bin/`) and runtime libraries (`lib/`), Trust's
/// `process_authority.rs`: owned by root or the invoking identity, and `mode & 0o022 == 0`
/// with no sticky exemption. `None` when targo admits it. Not mirrored: its `access(W_OK)`
/// refusal of an ACL-writable component the caller does not own.
#[must_use]
pub fn chain_component_refusal(ours: u32, owner: u32, mode: u32) -> Option<ChainWhy> {
    if owner != 0 && owner != ours {
        Some(ChainWhy::Foreign { owner, ours })
    } else if mode & 0o022 != 0 {
        Some(ChainWhy::Writable {
            mode: mode & 0o7777,
            yours: owner == ours,
        })
    } else {
        None
    }
}

/// The refused directories on the chains `targo trust` checks before it runs a verified
/// command with the toolchain at `build`, top down, each once (Trust's
/// `process_authority.rs`, `scan_verified_tool_runtime_library_paths`):
///
/// - the physical chain of `<build>/bin`, which targo resolves (a linked `bin/` is followed),
///   and whose parent is the sysroot;
/// - `<sysroot>/lib`, and `lib/rustlib/<T>/lib` for each non-file entry `<T>` of
///   `lib/rustlib`: every component below the sysroot must be a plain directory, so a link
///   (or a file) there is [`ChainWhy::Redirected`] whatever it points at, and nothing behind
///   it is judged; a runtime directory that exists is held to [`chain_component_refusal`]
///   component by component, as the sysroot's own chain is.
///
/// A missing `bin/` contributes nothing, as a missing `lib/` or `<T>/lib` does. Not
/// mirrored: the launch spelling's own symlink hops and the launcher files themselves.
#[cfg(unix)]
#[must_use]
pub fn trust_chain_refusals(build: &Path, ours: u32) -> Vec<ChainRefusal> {
    let mut walk = ChainWalk {
        ours,
        seen: std::collections::BTreeSet::new(),
        refused: Vec::new(),
    };
    let Ok(bin) = std::fs::canonicalize(build.join("bin")) else {
        return walk.refused;
    };
    let chain: Vec<&Path> = bin.ancestors().collect();
    for dir in chain.into_iter().rev() {
        walk.judge(dir);
    }
    let Some(sysroot) = bin.parent() else {
        return walk.refused;
    };
    let (lib, rustlib) = (OsStr::new("lib"), OsStr::new("rustlib"));
    walk.runtime_dir(sysroot, &[lib]);
    if walk.plain(sysroot, &[lib, rustlib]) {
        let mut targets: Vec<std::ffi::OsString> = std::fs::read_dir(sysroot.join("lib/rustlib"))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| !entry.file_type().is_ok_and(|t| t.is_file()))
            .map(|entry| entry.file_name())
            .collect();
        targets.sort();
        for target in &targets {
            walk.runtime_dir(sysroot, &[lib, rustlib, target, lib]);
        }
    }
    walk.refused
}

/// [`trust_chain_refusals`]'s state: each directory judged once, the refusals in order.
#[cfg(unix)]
struct ChainWalk {
    ours: u32,
    seen: std::collections::BTreeSet<PathBuf>,
    refused: Vec<ChainRefusal>,
}

#[cfg(unix)]
impl ChainWalk {
    fn refuse(&mut self, dir: &Path, why: ChainWhy) {
        if self.seen.insert(dir.to_path_buf()) {
            self.refused.push(ChainRefusal {
                dir: dir.to_path_buf(),
                why,
            });
        }
    }

    /// One directory of a physical chain, held to [`chain_component_refusal`] once.
    fn judge(&mut self, dir: &Path) {
        use std::os::unix::fs::MetadataExt as _;
        if self.seen.contains(dir) {
            return;
        }
        match std::fs::symlink_metadata(dir)
            .ok()
            .and_then(|m| chain_component_refusal(self.ours, m.uid(), m.mode()))
        {
            Some(why) => self.refuse(dir, why),
            None => {
                self.seen.insert(dir.to_path_buf());
            }
        }
    }

    /// Targo's `validate_plain_toolchain_directory_path`: whether every component of `rel`
    /// below `sysroot` is a plain directory. The first missing one answers `false`; the first
    /// that is anything else (a link, a file) is refused as [`ChainWhy::Redirected`] and
    /// answers `false`.
    fn plain(&mut self, sysroot: &Path, rel: &[&OsStr]) -> bool {
        let mut dir = sysroot.to_path_buf();
        for name in rel {
            dir.push(name);
            match std::fs::symlink_metadata(&dir) {
                Ok(m) if m.file_type().is_dir() => {}
                Ok(_) => {
                    self.refuse(&dir, ChainWhy::Redirected);
                    return false;
                }
                Err(_) => return false,
            }
        }
        true
    }

    /// A runtime-library directory targo puts on the loader path: when [`Self::plain`],
    /// each component below the sysroot is judged.
    fn runtime_dir(&mut self, sysroot: &Path, rel: &[&OsStr]) {
        if self.plain(sysroot, rel) {
            let mut dir = sysroot.to_path_buf();
            for name in rel {
                dir.push(name);
                self.judge(&dir);
            }
        }
    }
}

/// Walk the active Trust build's chain ([`trust_chain_refusals`]); `None` with no store
/// build of `trust` active, and off Unix, where there are no mode bits to read.
fn probe_trust_chain(layout: &Layout) -> Option<TrustChainProbe> {
    #[cfg(unix)]
    {
        let build = crate::ops::active_builds(layout).get("trust").copied()?;
        Some(TrustChainProbe {
            build,
            refused: trust_chain_refusals(
                &layout.build_dir("trust", build),
                crate::platform::our_uid(),
            ),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = layout;
        None
    }
}

/// (5f)'s row for a directory `targo trust` refuses on trust build `build`'s chain. Doctor
/// never chmods: the row names the one command that clears a writable component of the
/// user's own, and for one of root's (`/tmp`'s `1777`, which must stay as it is) names a
/// prefix of theirs instead, as `tools/install.sh`'s `check_install_path` does.
fn trust_chain_finding(build: u64, refusal: &ChainRefusal) -> Finding {
    let dir = refusal.dir.display();
    let quoted = crate::platform::sh_quote_str(&refusal.dir.to_string_lossy());
    let lead = format!("trust build {build}: `targo trust` refuses this toolchain");
    let person = |how: &str| Remedy::Person {
        pane: String::from("[packages].prefix"),
        how: how.to_owned(),
    };
    let (text, remedy) = match refusal.why {
        ChainWhy::Writable { mode, yours: true } => (
            format!(
                "{lead} — {dir} is group- or world-writable (mode {mode:o}), and targo runs no \
                 verified check whose compiler or runtime libraries sit below a directory \
                 another user could change; fix: chmod go-w {quoted}"
            ),
            Remedy::Command(format!("chmod go-w {quoted}")),
        ),
        ChainWhy::Writable { mode, yours: false } => (
            format!(
                "{lead} — {dir} is group- or world-writable (mode {mode:o}) and root's, not \
                 yours to change, and targo runs no verified check whose compiler or runtime \
                 libraries sit below a directory another user could change; fix: set \
                 [packages].prefix, or HOME, under directories only you can write"
            ),
            person("set [packages].prefix, or HOME, under directories only you can write"),
        ),
        ChainWhy::Foreign { owner, ours } => (
            format!(
                "{lead} — {dir} is owned by uid {owner}, neither root nor you (uid {ours}), and \
                 targo runs no verified check whose compiler or runtime libraries sit below a \
                 directory another user owns; fix: set [packages].prefix under directories \
                 root or you own"
            ),
            person("set [packages].prefix under directories root or you own"),
        ),
        ChainWhy::Redirected => (
            format!(
                "{lead} — {dir} is not a plain directory, and targo loads a verified \
                 toolchain's runtime libraries through no link below its sysroot; the bundle's \
                 fix: {TRUST_LIB_FIX}"
            ),
            Remedy::Person {
                pane: String::from("the trust bundle"),
                how: TRUST_LIB_FIX.to_owned(),
            },
        ),
    };
    Finding::change(
        c::of(c::TRUST_CHAIN, refusal.dir.display()),
        Class::Person,
        "a directory on the trust build's chain",
        text,
        "a directory only root or you can change",
        remedy,
    )
}

/// The fix a (5f)(g) [`ChainWhy::Redirected`] line names: the bundle's layout, which no
/// command here changes (a store build is laid as its bundle ships it).
const TRUST_LIB_FIX: &str = "ship lib/ and lib/rustlib/<target>/lib as real directories, \
                             never symbolic links";

impl Default for Probes {
    fn default() -> Self {
        Self {
            workspace: None,
            local_seal: None,
            automatic: true,
            excluded: Vec::new(),
            config_notes: Vec::new(),
            ignored_prefix: None,
            retired_env: Vec::new(),
            stub_env: crate::reroute::StubEnv::default(),
            index_head: false,
            cc: None,
            claude_defaults: None,
            claude_auto_prime: true,
            devtools: None,
            rosetta: None,
            trust_chain: None,
            login_rc: None,
        }
    }
}

/// Whether Claude Code's rating surveys are off under this `~/.claude/settings.json` text.
/// As Claude Code 2.1.284 reads it: every survey returns before it rolls when
/// `CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY` reads true (trimmed, lower-cased, one of `1`,
/// `true`, `yes`, `on`; the `env` block reaches the process live when the file is saved);
/// otherwise the session survey shows with probability `feedbackSurveyRate ?? <remote
/// default>`, and a rate of `0` turns off that kind alone. A rating card under a supervised
/// session's prompt waits for a keypress no one is there to give.
#[must_use]
pub fn claude_survey_off(settings_json: &str) -> bool {
    let Ok(v) = aterm_json::from_str::<aterm_json::Value>(settings_json) else {
        return false;
    };
    v.get("feedbackSurveyRate")
        .and_then(aterm_json::Value::as_f64)
        == Some(0.0)
        || claude_survey_env_on(&v)
}

/// Whether the `env` block holds `CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY` at a value Claude Code
/// reads as true ([`claude_env_true`]).
fn claude_survey_env_on(v: &aterm_json::Value) -> bool {
    v.get("env")
        .and_then(|e| e.get("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY"))
        .is_some_and(claude_env_true)
}

/// Claude Code's `bool` parse of an `env` value: stringified, trimmed, lower-cased, one of
/// `1 true yes on`. A JSON `true` or `1` stringifies to one of those; nothing else does.
fn claude_env_true(value: &aterm_json::Value) -> bool {
    let text = match value {
        aterm_json::Value::String(s) => s.clone(),
        other => aterm_json::to_string(other).unwrap_or_default(),
    };
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// A value aterm-primer's rules name, as JSON text on one line — the form every (10h) row
/// quotes a value in ([`DefaultState::Manual`]'s too). The primer keeps a number's literal
/// and prints an object over several lines; a row is one line.
fn one_line(text: String) -> String {
    aterm_json::from_str::<aterm_json::Value>(&text)
        .ok()
        .and_then(|v| aterm_json::to_string(&v).ok())
        .unwrap_or(text)
}

/// The hand-set efforts that hold the effort defaults back
/// ([`claude_efforts_set_by_hand`]), each as `<path> = <value>`.
fn efforts_by_hand(settings_json: &str) -> Vec<String> {
    claude_efforts_set_by_hand(settings_json)
        .into_iter()
        .map(|(path, value)| format!("{path} = {}", one_line(value)))
        .collect()
}

/// Where one default stands in a person's Claude settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultState {
    /// Set to the default (or, for the survey, turned off another way).
    Default,
    /// Set by hand to something else: kept, never overwritten.
    Manual(String),
    /// Not set, and not added: one of [`CLAUDE_EFFORT_DEFAULTS`] while a person's own
    /// effort stands, named here as `<path> = <value>` (several joined by `, `).
    HeldForEffort(String),
    /// Not set, and not added: [`CLAUDE_SURVEY_SWITCH`] while a person's own survey
    /// probability stands, named here as its JSON text. The switch silences every survey,
    /// so it would override the probability the person chose.
    HeldForRate(String),
    /// Not set, and not added: [`CLAUDE_COMPACT_WINDOW_KEY`] while `~/.claude.json` records a
    /// "Keep" answer to Claude Code's own autocompact offer, or cannot be read
    /// (`aterm_primer::claude_compact_offers_kept_in`), named here as the reason.
    HeldForOffer(String),
    /// The top-effort key ([`CLAUDE_TOP_EFFORT_KEY`]) `true` beside a hand-set effort, named
    /// as [`DefaultState::HeldForEffort`] names it: Claude Code then runs `xhigh`, never that
    /// effort. Nothing tells who wrote the key, so the window never removes it.
    OverridesEffort(String),
    /// Not set: an aterm window adds the default at its next start.
    Unset,
}

/// Each of [`CLAUDE_DEFAULTS`] as `settings_json` leaves it, by the window's own rule
/// (`aterm_primer::add_claude_defaults`): an unset key is added, except the effort defaults
/// while a person's own effort stands; a top-effort key that overrides such an effort is
/// [`DefaultState::OverridesEffort`].
///
/// # Errors
///
/// A file the window refuses and leaves as it is: the parse error (an empty file included),
/// or "the top level is not a JSON object". No key is reported for it.
pub fn claude_defaults_state(
    settings_json: &str,
) -> Result<Vec<(&'static str, DefaultState)>, String> {
    let v = aterm_json::from_str::<aterm_json::Value>(settings_json).map_err(|e| e.to_string())?;
    if v.as_object().is_none() {
        return Err("the top level is not a JSON object".to_string());
    }
    // A nested default goes into an object: the window refuses a file whose `env` is none.
    if v.get("env").is_some_and(|env| env.as_object().is_none()) {
        return Err("its \"env\" is not a JSON object".to_string());
    }
    let survey_off = claude_survey_off(settings_json);
    let rate_by_hand = claude_survey_rate_set_by_hand(settings_json).map(one_line);
    let by_hand = efforts_by_hand(settings_json);
    Ok(CLAUDE_DEFAULTS
        .iter()
        .map(|&(key, want)| {
            let state = match default_value(&v, key) {
                None if key == "feedbackSurveyRate" && survey_off => DefaultState::Default,
                None if CLAUDE_EFFORT_DEFAULTS.contains(&key) && !by_hand.is_empty() => {
                    DefaultState::HeldForEffort(by_hand.join(", "))
                }
                None if key == CLAUDE_SURVEY_SWITCH && rate_by_hand.is_some() => {
                    DefaultState::HeldForRate(rate_by_hand.clone().unwrap_or_default())
                }
                None => DefaultState::Unset,
                Some(have) => {
                    let text = aterm_json::to_string(have).unwrap_or_default();
                    // The switch is on for whatever Claude Code reads as true (`yes`,
                    // `TRUE`, ` 1 `, a JSON `true`), not only the spelling the window writes.
                    let is_default = if key == CLAUDE_SURVEY_SWITCH {
                        claude_env_true(have)
                    } else {
                        // The file's own value only: the `env` survey switch sets no
                        // rate, and a rate a person wrote is theirs, whatever the switch.
                        text == want.json_text()
                            || (key == "feedbackSurveyRate" && have.as_f64() == Some(0.0))
                    };
                    if key == CLAUDE_TOP_EFFORT_KEY && text == "true" && !by_hand.is_empty() {
                        DefaultState::OverridesEffort(by_hand.join(", "))
                    } else if is_default {
                        DefaultState::Default
                    } else {
                        DefaultState::Manual(text)
                    }
                }
            };
            (key, state)
        })
        .collect())
}

/// Whether an aterm window would write to a settings file with this text: the
/// window's own rule (`aterm_primer::add_claude_defaults`) — a key of
/// [`CLAUDE_DEFAULTS`] absent from the top level, unless it is held back (an effort
/// default by a person's own effort, the survey switch by their own probability, the
/// autocompact window while `window_held`).
fn window_adds(settings_json: &str, window_held: bool) -> bool {
    let Ok(v) = aterm_json::from_str::<aterm_json::Value>(settings_json) else {
        return false;
    };
    let held = !claude_efforts_set_by_hand(settings_json).is_empty();
    let rate_held = claude_survey_rate_set_by_hand(settings_json).is_some();
    CLAUDE_DEFAULTS.iter().any(|(key, _)| {
        default_value(&v, key).is_none()
            && !(held && CLAUDE_EFFORT_DEFAULTS.contains(key))
            && !(rate_held && *key == CLAUDE_SURVEY_SWITCH)
            && !(window_held && *key == CLAUDE_COMPACT_WINDOW_KEY)
    })
}

/// Why the window holds the autocompact window back for `home`, by its own rule
/// (`aterm_primer::claude_compact_offers_kept_in`), or `None` when nothing does.
fn compact_window_held(home: &Path) -> Option<String> {
    match claude_compact_offers_kept_in(home) {
        Ok(models) if models.is_empty() => None,
        Ok(models) => Some(format!(
            "Claude Code's autocompact offer was answered Keep ({}) in ~/.claude.json",
            models.join(", ")
        )),
        Err(why) => Some(format!(
            "~/.claude.json, where Claude Code records the answer to its autocompact offer, \
             cannot be read ({why})"
        )),
    }
}

/// Where a default's key stands in the settings: a plain key at the top level, a dotted one
/// (`env.NAME`) inside the object its head names.
fn default_value<'a>(v: &'a aterm_json::Value, key: &str) -> Option<&'a aterm_json::Value> {
    match key.split_once('.') {
        None => v.get(key),
        Some((parent, child)) => v.get(parent).and_then(|p| p.get(child)),
    }
}

/// A default as the person would write it into the file: `"feedbackSurveyRate": 0`, or
/// `"env": {"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "1"}` for a nested one.
fn default_line(key: &str, want: &str) -> String {
    match key.split_once('.') {
        None => format!("\"{key}\": {want}"),
        Some((parent, child)) => format!("\"{parent}\": {{\"{child}\": {want}}}"),
    }
}

/// What (10h) reads of the Claude settings file in `home` (`~/.claude/settings.json`):
/// [`claude_defaults_state`] over its text, the autocompact window held back as the window
/// holds it ([`compact_window_held`]), or `None` with no file (and no link). Each case where
/// every window start fails to add to the file is the `Err` the one refusal row names: a file
/// that cannot be read, a symlink to a missing target (which the window will not replace with
/// a plain file), or, while a key is left to add ([`window_adds`]), a real directory (where
/// the atomic write renames its temp file) this user cannot write, such as a read-only
/// home-manager store.
fn probe_claude_settings(home: &Path) -> Option<Result<Vec<(&'static str, DefaultState)>, String>> {
    let path = &home.join(".claude").join("settings.json");
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // NotFound through a link means the link is there and its target is not: the
            // window's own reading (`add_claude_defaults`).
            let meta = std::fs::symlink_metadata(path).ok()?;
            if !meta.file_type().is_symlink() {
                return None;
            }
            let target = std::fs::read_link(path)
                .map_or_else(|_| "?".to_string(), |t| t.display().to_string());
            return Some(Err(format!("a symlink to {target}, which does not exist")));
        }
        Err(e) => return Some(Err(format!("it cannot be read: {e}"))),
    };
    let held = compact_window_held(home);
    let states = claude_defaults_state(&text).map(|states| {
        states
            .into_iter()
            .map(|(key, state)| match (&held, state) {
                (Some(why), DefaultState::Unset) if key == CLAUDE_COMPACT_WINDOW_KEY => {
                    (key, DefaultState::HeldForOffer(why.clone()))
                }
                (_, state) => (key, state),
            })
            .collect()
    });
    if states.is_ok() && window_adds(&text, held.is_some()) {
        let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(dir) = real.parent()
            && !crate::platform::dir_writable_by_caller(dir)
        {
            let linked = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
            return Some(Err(if linked {
                format!(
                    "a symlink to {}, whose directory {} this user cannot write",
                    real.display(),
                    dir.display()
                )
            } else {
                format!(
                    "its directory {} is not writable by this user",
                    dir.display()
                )
            }));
        }
    }
    Some(states)
}

/// What replaced the network drive's `ATERM_NET_*` variables: the fleet, which a person
/// starts on one machine and joins from each other with its code.
const FLEET_SETUP: &str = "the fleet: `aterm fleet init` on one machine, `aterm fleet join` \
                           with its join code on each other";

/// Retired environment opt-outs a person may still export, each with the setting or launch
/// flag that replaced it. An inert opt-out silently stops opting out
/// (`ATERM_NO_AUTO_UPDATE=1` holds no update), so the doctor names it.
///
/// The one place a retired name is read, and only for presence: nothing acts on it.
pub const RETIRED_OPT_OUTS: &[(&str, &str)] = &[
    (
        "ATERM_NO_AUTO_UPDATE",
        "[update] enabled = false (Settings ▸ Software Update)",
    ),
    (
        "ATERM_NO_AUTO_APPLY",
        "[update] auto_apply = false (Settings ▸ Software Update)",
    ),
    (
        "ATPKG_DISABLE",
        "[packages] enabled = false (Settings ▸ Packages)",
    ),
    (
        "ATERM_REROUTE_QUIET",
        "[reroute] announce = false (Settings ▸ Packages)",
    ),
    ("ATERM_NO_REROUTE", "aterm --no-reroute"),
    ("ATERM_NO_HARNESS", "[harness] enabled = false"),
    // The window's launch flags and aterm.toml keys.
    (
        "ATERM_CPU",
        "gpu = false (Settings ▸ Performance), or aterm --window --cpu",
    ),
    (
        "ATERM_GPU",
        "the default: the GPU renderer is on unless gpu = false",
    ),
    (
        "ATERM_GPU_BACKEND",
        "gpu = false for a broken driver (each platform builds one GPU backend)",
    ),
    (
        "ATERM_METAL",
        "the default: Metal is always the macOS renderer",
    ),
    (
        "ATERM_FONT",
        "font_family (Settings ▸ Text & Fonts), or aterm --window --font",
    ),
    (
        "ATERM_FONT_PX",
        "font_px (Settings ▸ Text & Fonts), or aterm --window --font-px",
    ),
    ("ATERM_FONT_HINTING", "font_hinting in aterm.toml"),
    ("ATERM_FONT_SUBPIXEL", "font_subpixel in aterm.toml"),
    (
        "ATERM_FALLBACK_FONT",
        "fallback_fonts (Settings ▸ Text & Fonts)",
    ),
    ("ATERM_SYMBOL_FONT", "symbol_font (Settings ▸ Text & Fonts)"),
    ("ATERM_EMOJI_FONT", "emoji_font (Settings ▸ Text & Fonts)"),
    ("ATERM_STEM_GAMMA", "stem_gamma (Settings ▸ Text & Fonts)"),
    ("ATERM_FORCE_SCALE", "aterm --window --scale <f>"),
    ("ATERM_TAB_STRIP_ROWS", "tab_strip_rows (Settings ▸ Window)"),
    (
        "ATERM_NO_DARK_CHROME",
        "window_theme = \"auto\" (Settings ▸ Appearance)",
    ),
    (
        "ATERM_WINDOWING_BEHAVIOR",
        "windowing_behavior in aterm.toml",
    ),
    ("ATERM_LATENCY_TRACE", "`aterm ctl metrics percentiles`"),
    (
        "ATERM_SNAPSHOT_PATH",
        "`aterm ctl image <file>` (SIGUSR1 writes to the control directory)",
    ),
    // The session launch knobs.
    ("ATERM_HEADLESS", "aterm --headless"),
    (
        "ATERM_COLUMNS",
        "columns in aterm.toml, or aterm --window --columns <n>",
    ),
    (
        "ATERM_LINES",
        "lines in aterm.toml, or aterm --window --lines <n>",
    ),
    (
        "ATERM_CONTROL_SOCK",
        "aterm --window --control-sock <path> for the window, aterm ctl --sock <path> for a \
         client",
    ),
    ("ATERM_NO_CONTROL_SOCK", "aterm --window --no-control-sock"),
    (
        "ATERM_CONTROL_TOKEN",
        "the token file beside the socket (aterm drive --socket <path>)",
    ),
    ("ATERM_CTL", "the aterm-ctl beside aterm, or on PATH"),
    (
        "ATERM_SHELL",
        "shell in aterm.toml, or aterm --window --shell <name>",
    ),
    (
        "ATERM_EXEC",
        "aterm --window -e <command> (with --hold to keep the window)",
    ),
    ("ATERM_TERM_PROGRAM", "TERM_PROGRAM set in your shell rc"),
    ("ATERM_AI_HINT", "aterm help"),
    (
        "ATERM_ALT_ARCHIVE",
        "the default (the offscreen archive is always on)",
    ),
    (
        "ATERM_CONTAINMENT_MODE",
        "aterm --containment <mode> (or --sandbox)",
    ),
    (
        "ATERM_NO_PATH_REFRESH",
        "the default (the registry PATH is always merged)",
    ),
    (
        "ATERM_NO_SHELL_INTEGRATION",
        "aterm --window --no-shell-integration",
    ),
    // The embedded operator was removed (2026-10-15); a manager session takes
    // its place.
    (
        "ATERM_OPERATOR",
        "a manager session: `aterm ctl survey` and `spawn agent=claude`",
    ),
    (
        "ATERM_NO_OPERATOR",
        "the default (the embedded operator is removed)",
    ),
    (
        "ATERM_OPERATOR_PROFILE",
        "the default (the embedded operator is removed)",
    ),
    // The network drive these configured went too (2026-10-15): the fleet
    // replaced it.
    ("ATERM_NET_LISTEN", FLEET_SETUP),
    ("ATERM_NET_CERT", FLEET_SETUP),
    ("ATERM_NET_KEY", FLEET_SETUP),
    ("ATERM_VERBOSE", "aterm --verbose"),
    // `aterm drive`'s prompt-ready pattern: `--ready` left with the commands
    // that waited on it, and a server-side wait replaced them.
    (
        "ATERM_DRIVE_READY",
        "aterm ctl @<sid> await match <regex> (aterm drive has no --ready now)",
    ),
];

/// Which [`RETIRED_OPT_OUTS`] are exported to this process, whatever the value: an exported
/// `=0` is still a line in an rc a person should delete.
#[must_use]
pub fn retired_opt_outs_exported() -> Vec<(&'static str, &'static str)> {
    RETIRED_OPT_OUTS
        .iter()
        .copied()
        .filter(|(name, _)| std::env::var_os(name).is_some())
        .collect()
}

/// The publisher-side act that cures an `UnknownField` verdict: publish the newer
/// Trust coherence group from the seal the workspace was committed against.
pub const PUBLISH_RUSTC_GROUP: &str = "atpkg-publish publish-group rustc --seal <dir> --upload";

/// Find the nearest ancestor of `start` (inclusive) whose `Cargo.toml` carries a
/// `[trust]` table. `None` when no manifest on the way up declares one.
fn workspace_with_trust_table(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let manifest = d.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest)
            && text.lines().any(|l| l.trim() == "[trust]")
        {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// The field name out of targo's refusal, e.g. ``unknown field `compiler_timeout_secs`,
/// expected one of …`` → `compiler_timeout_secs`.
fn unknown_field_in(stderr: &str) -> Option<String> {
    let marker = "unknown field `";
    let start = stderr.find(marker)? + marker.len();
    let rest = &stderr[start..];
    let end = rest.find('`')?;
    let field = &rest[..end];
    (!field.is_empty()).then(|| field.to_string())
}

/// The key and the setting that replaced it, out of targo's refusal of a retired `[trust]`
/// key, in both spellings Trust writes it: the manifest reader's
/// (``error: `[trust]` key `compiler_timeout_secs` was retired on 2026-10-11: …``,
/// cargo-util-schemas' `TomlTrustRetiredKeys`) and `targo-trust`'s own loader's
/// (``failed to validate <path>: [trust] `compiler_timeout_secs` was retired on …``,
/// `RETIRED_CONFIG_KEYS`). The replacement is the rest of that line.
fn retired_key_in(stderr: &str) -> Option<(String, String)> {
    ["`[trust]` key `", "[trust] `"].iter().find_map(|marker| {
        let start = stderr.find(marker)? + marker.len();
        let (key, rest) = stderr[start..].split_once('`')?;
        let replacement = rest.strip_prefix(" was ")?.lines().next()?.trim();
        (!key.is_empty() && !replacement.is_empty())
            .then(|| (key.to_string(), replacement.to_string()))
    })
}

/// Ask the installed targo whether it accepts the workspace's `[trust]` policy.
/// `locate-project --workspace` parses the manifest and compiles nothing.
fn probe_workspace_policy(layout: &Layout, cwd: &Path) -> Option<WorkspacePolicyProbe> {
    let workspace = workspace_with_trust_table(cwd)?;
    let trust_build = crate::ops::active_builds(layout)
        .into_iter()
        .find(|(p, _)| p == "trust")
        .map(|(_, b)| b)?;
    let targo = layout
        .build_dir("trust", trust_build)
        .join("bin")
        .join("targo");
    if !targo.is_file() {
        return Some(WorkspacePolicyProbe {
            workspace,
            policy: WorkspacePolicy::ProbeFailed {
                why: format!("no targo at {}", targo.display()),
            },
        });
    }
    let probe = aterm_uds::spawnfd::command(&targo)
        .args(["locate-project", "--workspace"])
        .current_dir(&workspace)
        .env_remove("RUSTFLAGS")
        .output();
    let policy = match probe {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if let Some((key, replacement)) = retired_key_in(&stderr) {
                WorkspacePolicy::RetiredKey { key, replacement }
            } else if let Some(field) = unknown_field_in(&stderr) {
                WorkspacePolicy::UnknownField { field }
            } else if out.status.success() {
                WorkspacePolicy::Accepted
            } else {
                WorkspacePolicy::ProbeFailed {
                    why: stderr.lines().next().unwrap_or("targo failed").to_string(),
                }
            }
        }
        Err(e) => WorkspacePolicy::ProbeFailed { why: e.to_string() },
    };
    Some(WorkspacePolicyProbe { workspace, policy })
}

/// Where rustup's `trust` channel points, when that is not inside the atpkg store. The rustup
/// home is the one rustup resolves (`$RUSTUP_HOME`, else `<home>/.rustup`,
/// [`crate::seam::rustup_home_with`]), so a relocated home is probed, not skipped.
fn probe_local_seal(layout: &Layout, home: Option<&Path>) -> Option<LocalSealProbe> {
    probe_local_seal_at(
        layout,
        &crate::seam::rustup_home_with(std::env::var_os("RUSTUP_HOME").as_deref(), home)?,
    )
}

/// [`probe_local_seal`] under a known rustup home: what `aterm pkg repair`'s
/// `base.pkg.rustup.local` item reads, over the home its apply re-points in.
pub(crate) fn probe_local_seal_at(layout: &Layout, rustup_home: &Path) -> Option<LocalSealProbe> {
    let link = rustup_home.join("toolchains").join("trust");
    let target = std::fs::read_link(&link).ok()?;
    let target = if target.is_absolute() {
        target
    } else {
        link.parent()?.join(target)
    };
    let canonical_target = std::fs::canonicalize(&target).unwrap_or_else(|_| target.clone());
    let canonical_prefix =
        std::fs::canonicalize(&layout.prefix).unwrap_or_else(|_| layout.prefix.clone());
    if canonical_target.starts_with(&canonical_prefix) {
        return None;
    }
    let trustc = target.join("bin").join("trustc");
    let commit = output_bounded(aterm_uds::spawnfd::command(&trustc).arg("-Vv"))
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("commit-hash: ")
                    .map(|c| c.chars().take(10).collect::<String>())
            })
        })
        .unwrap_or_else(|| "unknown".to_string());
    Some(LocalSealProbe {
        stale: crate::seam::stale_against_store(layout, &target),
        link,
        link_target: target,
        trustc: commit,
    })
}

/// How much of the report a person asked for: the problems (the default), or everything
/// the doctor checked (`--verbose`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// The verdict, then only what needs attention (each in one short line with its next
    /// step), and the one next act.
    Problems,
    /// The verdict, then every row and its whole explanation.
    Everything,
}

/// Run the health surface over the real environment (home, PATH, the clock, the
/// `[packages]` config) and print it: the text rendered from the rows ([`render`]), or with
/// `json` the rows themselves in the one schema every `--json` surface prints
/// ([`aterm_converge::Report`], mode `check`). Returns `true` iff the rows pass
/// ([`crate::converge::passes`]: no structural problem; `main` maps `false` to exit 1).
#[must_use]
pub fn run(layout: &Layout, prefix: &str, detail: Detail, json: bool) -> bool {
    emit(
        rows(layout),
        prefix,
        detail,
        json,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

/// The doctor's rows over the real environment (home, PATH, the clock, the `[packages]`
/// config), read once and never applied: what [`run`] prints.
#[must_use]
pub fn rows(layout: &Layout) -> Vec<ItemReport> {
    rows_reading(layout, crate::devtools::facts())
}

/// [`rows`] without the Developer Tools read (one `sqlite3` and one `log show`, up to
/// 8 s): the packages view `aterm doctor` composes beside its own items, whose own
/// Developer Tools row has read it already.
#[must_use]
pub fn packages_rows(layout: &Layout) -> Vec<ItemReport> {
    rows_reading(layout, None)
}

/// [`rows`] with the Developer Tools facts `devtools` (`None`: no such row).
fn rows_reading(layout: &Layout, devtools: Option<crate::devtools::Facts>) -> Vec<ItemReport> {
    let home = aterm_types::dirs::home_dir();
    let path = std::env::var_os("PATH");
    let cfg_account = crate::config::cached().account().map(str::to_string);
    let cfg = crate::config::cached();
    let probes = Probes {
        workspace: std::env::current_dir()
            .ok()
            .and_then(|cwd| probe_workspace_policy(layout, &cwd)),
        local_seal: probe_local_seal(layout, home.as_deref()),
        automatic: crate::manager_enabled() && cfg.enabled(),
        excluded: cfg.exclude().to_vec(),
        config_notes: cfg.config_notes(),
        ignored_prefix: cfg.ignored_prefix.clone(),
        retired_env: retired_opt_outs_exported(),
        claude_defaults: home.as_deref().and_then(probe_claude_settings),
        claude_auto_prime: crate::config::load_agents_auto_prime(),
        stub_env: crate::reroute::StubEnv::of_process(),
        index_head: crate::index_probe::probes_this_source(),
        cc: Some(crate::prereq::probe(path.as_deref())),
        devtools,
        // One `stat`, where it applies; the switch from the same table the apply reads.
        rosetta: crate::machine::ROSETTA_APPLIES.then(|| {
            (
                crate::machine::rosetta_posture(&crate::machine::SystemRosetta),
                crate::config::cached_machine().rosetta(),
            )
        }),
        trust_chain: probe_trust_chain(layout),
        login_rc: home.as_deref().and_then(crate::hooks::account_login_rc),
    };
    report_with(
        layout,
        home.as_deref(),
        path.as_deref(),
        crate::flow::now_unix(),
        cfg_account.as_deref(),
        &probes,
    )
}

/// Print the rows and say whether they pass ([`crate::converge::passes`]): with `json` the
/// one [`aterm_converge::Report`] (mode `check`, `complete` the exit rule) on one line of
/// `out`; else the text [`render`] lays out — the verdict and the rest to `out`, the
/// structural faults to `err`. Write errors are ignored: a reader that closed the pipe
/// (`doctor | head`) costs an `EPIPE` nobody sees, never a panic.
pub(crate) fn emit(
    rows: Vec<ItemReport>,
    prefix: &str,
    detail: Detail,
    json: bool,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> bool {
    let passes = crate::converge::passes(&rows);
    if json {
        let report = aterm_converge::Report::new(aterm_converge::ReportMode::Check, passes, rows);
        return match aterm_json::to_string(&report) {
            Ok(text) => {
                let _ = writeln!(out, "{text}");
                let _ = out.flush();
                passes
            }
            Err(why) => {
                let _ = writeln!(err, "{prefix}: cannot encode the report: {why}");
                false
            }
        };
    }
    // The verdict first, then the structural faults, then the rest.
    let shown = render(&rows, prefix, detail);
    let _ = out.write_all(shown.verdict.as_bytes());
    let _ = out.flush();
    let _ = err.write_all(shown.faults.as_bytes());
    let _ = out.write_all(shown.rest.as_bytes());
    let _ = out.flush();
    passes
}

/// The longest line the default report prints — a row, its remedy included.
pub const SHORT_ROW: usize = 160;

/// The head of the toolset row of a declined store — said by default too ([`render`]): it
/// is why nothing is installed.
const DECLINED_LINE: &str = "the ALab toolset was removed on this machine";

/// The (8) row for a store no index update pass has completed on, whether or not a failed
/// one left a `status.toml` — followed by the vendor programs that update without one.
const NEVER_CHECKED: &str = "no index update pass has completed yet — run: aterm pkg update";

/// The report as [`render`] lays it out, in the order it is printed: the verdict, the
/// structural faults (stderr), then the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presented {
    pub verdict: String,
    pub faults: String,
    pub rest: String,
}

impl Presented {
    /// The three parts in the order they are printed — what a terminal shows.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn joined(&self) -> String {
        format!("{}{}{}", self.verdict, self.faults, self.rest)
    }
}

/// A row's mark in the text report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// As it should be.
    Ok,
    /// A fact: does not apply, or waits for something that ends by itself.
    Note,
    /// Advisory: something to look at, never a failure.
    Warn,
    /// A structural fault (stderr): the report fails.
    Fail,
    /// The toolset, or a dev link, is not what the machine wants: the report fails.
    Problem,
}

impl Mark {
    const fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Note => "note",
            Self::Warn => "warn",
            Self::Fail => "FAIL",
            Self::Problem => "PROBLEM",
        }
    }

    const fn flagged(self) -> bool {
        matches!(self, Self::Warn | Self::Fail | Self::Problem)
    }
}

/// A row's mark, from its outcome and whether it fails the report; `None` for a recorded
/// problem the toolset row lists, which prints indented under it.
fn mark(row: &ItemReport) -> Option<Mark> {
    if c::is(&row.id, c::TOOLSET_MEMBER) {
        return None;
    }
    Some(match row.outcome {
        Outcome::Ok | Outcome::Changed => Mark::Ok,
        Outcome::NotApplicable | Outcome::Deferred => Mark::Note,
        _ if row.fails_surface() => {
            if c::is(&row.id, c::TOOLSET) || c::is(&row.id, c::DEV_LINK) {
                Mark::Problem
            } else {
                Mark::Fail
            }
        }
        _ => Mark::Warn,
    })
}

/// A row's sentence: the value an `ok` row holds, else its explanation.
fn sentence(row: &ItemReport) -> &str {
    match row.outcome {
        Outcome::Ok | Outcome::Changed => row.to.as_deref(),
        _ => row
            .explain
            .as_ref()
            .map(|e| e.why.as_str())
            .or(row.why.as_deref()),
    }
    .unwrap_or(&row.title)
}

/// A row as `--verbose` prints it, whole.
fn line_of(row: &ItemReport, p: &str) -> String {
    match mark(row) {
        Some(mark) => format!("{p}: {} \u{2014} {}", mark.word(), sentence(row)),
        None => format!("{p}:   {}", sentence(row)),
    }
}

/// Whether the default report prints the row: what needs attention, the recorded problems
/// under the toolset row, and the toolset row whenever it is not ok (a declined store's
/// says why nothing is installed).
fn said_by_default(row: &ItemReport) -> bool {
    mark(row).is_none_or(Mark::flagged) || (row.id == c::TOOLSET && row.outcome != Outcome::Ok)
}

/// The words a short row ends with for its remedy, when it has one a person can act on. A
/// remedy whose sentence goes on after its act (`run '…/atpkg' repair — it moves this file
/// aside …`) keeps the act.
fn remedy_words(remedy: &Remedy) -> Option<String> {
    let act = |text: &str| {
        text.split(" \u{2014} ")
            .next()
            .unwrap_or(text)
            .trim()
            .to_owned()
    };
    let words = match remedy {
        Remedy::Command(command) => format!("fix: {}", act(command)),
        Remedy::Repair => String::from("fix: aterm pkg repair"),
        Remedy::Init { only } => format!("fix: aterm init --only {only}"),
        Remedy::Develop => String::from("fix: aterm init --develop"),
        Remedy::Provision { args } => format!("fix: targo --unverified verify provision {args}")
            .trim_end()
            .to_owned(),
        Remedy::Person { how, .. } => format!("fix: {}", act(how)),
        Remedy::Network => String::from("the next pass, once online"),
        Remedy::None => return None,
    };
    (words.chars().count() <= 110).then_some(words)
}

/// One row in at most [`SHORT_ROW`] characters: the row whole when it fits; a flagged row
/// cut to its first clause (the fact) and its remedy ([`remedy_words`]), the fact cut short
/// with a visible `…` if the two still do not fit; any other row cut to fit.
fn short_line(row: &ItemReport, p: &str) -> String {
    let whole = line_of(row, p);
    if whole.chars().count() <= SHORT_ROW {
        return whole;
    }
    let Some(mark) = mark(row).filter(|m| m.flagged()) else {
        let mut cut: String = whole.chars().take(SHORT_ROW - 1).collect();
        cut.push('\u{2026}');
        return cut;
    };
    let text = sentence(row);
    let fact = text[..c::first_clause_end(text)]
        .trim_end()
        .trim_end_matches('.');
    let lead = format!("{p}: {} \u{2014} ", mark.word());
    // The remedy is what a person acts on, so the fact yields room to it, never the reverse.
    let tail = row
        .explain
        .as_ref()
        .and_then(|e| remedy_words(&e.remedy))
        .map_or_else(String::new, |words| format!(" \u{2014} {words}"));
    let room = SHORT_ROW
        .saturating_sub(lead.chars().count() + tail.chars().count())
        .max(20);
    let fact: String = if fact.chars().count() > room {
        let mut cut: String = fact.chars().take(room - 1).collect();
        cut.push('\u{2026}');
        cut
    } else {
        fact.to_string()
    };
    format!("{lead}{fact}{tail}")
}

/// The verdict: `found N problem(s)` when rows fail the report; else `not healthy` while a
/// managed tool cannot run or cannot be shown to (its warn carries the remedy); else
/// `healthy` when nothing is flagged, `working` with a count when something is.
fn verdict(rows: &[ItemReport], p: &str) -> String {
    let fails = rows.iter().filter(|r| r.fails_surface()).count();
    if fails > 0 {
        return format!("{p}: found {fails} problem(s)");
    }
    let claims = |claim: &str| {
        rows.iter()
            .filter(|r| r.detail.as_deref() == Some(claim))
            .count()
    };
    let (cannot, unproven) = (claims(c::CANNOT_RUN), claims(c::UNPROVEN));
    if cannot + unproven > 0 {
        return format!(
            "{p}: not healthy \u{2014} {} (see below)",
            withheld_summary(cannot, unproven)
        );
    }
    let flagged = rows
        .iter()
        .filter(|r| mark(r).is_some_and(Mark::flagged))
        .count();
    // Not "ALab program(s)": the count includes the vendor-direct agents.
    let programs = rows
        .iter()
        .find(|r| r.id == c::TOOLSET && r.outcome == Outcome::Ok)
        .and_then(|r| r.detail.as_deref())
        .map(|n| format!("{n} program(s) installed"));
    match (flagged, programs) {
        (0, None) => format!("{p}: healthy"),
        (0, Some(programs)) => format!("{p}: healthy \u{2014} {programs} and working"),
        (n, None) => format!("{p}: working \u{2014} {n} thing(s) to look at"),
        (n, Some(programs)) => {
            format!("{p}: working \u{2014} {programs}; {n} thing(s) to look at")
        }
    }
}

/// The one next act of a failing report: a single command, never a menu (a pile of
/// problems and three suggestions teaches a first-hour user to close the terminal). It is
/// the explicit door ([`Remedy::Command`]) of the first failing row in this order: the C
/// toolchain (no amount of updating installs a compiler), the workspace (a publish), the
/// toolset (its own door: the whole set, a held member's, `--retry` or `update`), a live
/// build (`update` re-flips every diverged shim), one program's store tree. A failing C
/// toolchain outranks the rest even without a door: one that refuses with no one act to
/// name (macOS: it is installed, and its own stderr is the cause) has its remedy inline, so
/// there is no next line. A fault whose remedy is said inline (a stray `.sh`: remove it)
/// adds none either.
fn next_act(rows: &[ItemReport]) -> Option<String> {
    let failing = |id: &'static str| {
        rows.iter()
            .filter(move |r| r.fails_surface() && c::is(&r.id, id))
    };
    let door = |row: &ItemReport| match row.explain.as_ref().map(|e| &e.remedy) {
        Some(Remedy::Command(command)) => Some(command.clone()),
        _ => None,
    };
    if let Some(compiler) = failing(c::CLT).next() {
        return door(compiler);
    }
    [c::WORKSPACE, c::TOOLSET, c::LIVE, c::STORE]
        .into_iter()
        .find_map(|id| failing(id).find_map(door))
}

/// The report as a person reads it, rendered from the rows: the verdict first, then by
/// default ([`Detail::Problems`]) only what needs attention, each row in one line of at
/// most [`SHORT_ROW`] characters (the fact, and its remedy when the row names one), then
/// the one next act and where the rest is. `healthy` is said only when nothing was flagged;
/// warnings without a fault read "working", with a count. [`Detail::Everything`] prints
/// the verdict and then every row whole. The structural faults (`FAIL`) are `faults`,
/// which [`run`] prints on stderr.
#[must_use]
pub fn render(rows: &[ItemReport], p: &str, detail: Detail) -> Presented {
    let mut faults = String::new();
    let mut rest = String::new();
    let mut elided = false;
    for row in rows {
        let whole = line_of(row, p);
        let shown = match detail {
            Detail::Everything => whole.clone(),
            Detail::Problems if said_by_default(row) => short_line(row, p),
            Detail::Problems => {
                elided = true;
                continue;
            }
        };
        elided |= shown != whole;
        let into = if mark(row) == Some(Mark::Fail) {
            &mut faults
        } else {
            &mut rest
        };
        into.push_str(&shown);
        into.push('\n');
    }
    if !crate::converge::passes(rows)
        && let Some(act) = next_act(rows)
    {
        rest.push_str(&format!("{p}: next \u{2014} {act}\n"));
    }
    if detail == Detail::Problems && elided {
        rest.push_str(&format!("{p}: the full report: aterm pkg {p} --verbose\n"));
    }
    Presented {
        verdict: format!("{}\n", verdict(rows, p)),
        faults,
        rest,
    }
}

/// The packages view `aterm doctor` prints under its `packages:` row (design §5: `pkg
/// doctor` is its packages view): whether the rows read `healthy`, then the default
/// report's lines ([`render`], [`Detail::Problems`]) without their `doctor:` lead — the
/// verdict first, then the structural faults and the rest, each in one short line.
#[must_use]
pub fn packages_view(rows: &[ItemReport]) -> (bool, Vec<String>) {
    const P: &str = "doctor";
    let shown = render(rows, P, Detail::Problems);
    let lead = format!("{P}: ");
    let lines = [&shown.verdict, &shown.faults, &shown.rest]
        .into_iter()
        .flat_map(|part| part.lines())
        .map(|line| {
            line.strip_prefix(&lead)
                .unwrap_or(line)
                .trim_start()
                .to_owned()
        })
        .collect();
    (
        verdict(rows, P).starts_with(&format!("{P}: healthy")),
        lines,
    )
}

/// A typed row's words as the doctor prints them: `<level> — <text>`.
#[must_use]
pub fn spelled(row: &Row) -> String {
    format!("{} \u{2014} {}", level_word(row.level), row.text)
}

/// The testable core: `home`, the `PATH` value, `now` and the `[packages].account` config
/// override are injected so the surface can be exercised against a synthetic environment
/// without mutating the process env. Prints the report as [`render`] lays it out for
/// `detail` — the verdict and the rest to `out`, the structural faults to `err` — and
/// returns whether it passes.
#[must_use]
#[allow(clippy::too_many_arguments)] // the injected environment, plus the speaker and its streams
pub fn run_with(
    layout: &Layout,
    home: Option<&Path>,
    path_var: Option<&OsStr>,
    now: i64,
    cfg_account: Option<&str>,
    prefix: &str,
    probes: &Probes,
    detail: Detail,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> bool {
    let rows = report_with(layout, home, path_var, now, cfg_account, probes);
    emit(rows, prefix, detail, false, out, err)
}

/// The rows of the report over an injected environment ([`run_with`]): every finding of
/// [`read`] as an item, planned by the engine and never applied.
#[must_use]
pub fn report_with(
    layout: &Layout,
    home: Option<&Path>,
    path_var: Option<&OsStr>,
    now: i64,
    cfg_account: Option<&str>,
    probes: &Probes,
) -> Vec<ItemReport> {
    crate::converge::report(read(layout, home, path_var, now, cfg_account, probes))
}

/// Read the machine once: checks (0)–(11), in order, each leaving a [`Finding`] per thing it
/// found ([`crate::converge`]). Reads only: no write, no network.
#[must_use]
#[allow(clippy::too_many_lines)] // one read pass, in the order the report prints it
pub(crate) fn read(
    layout: &Layout,
    home: Option<&Path>,
    path_var: Option<&OsStr>,
    now: i64,
    cfg_account: Option<&str>,
    probes: &Probes,
) -> Vec<Finding> {
    let mut f: Vec<Finding> = Vec::new();

    // (0) Which atpkg is speaking. A machine can carry several (the app's, a dev `dist/`, a
    // `target/release`), and a stale one misreports the store, whose natural next step is
    // reinstalling something that was never broken.
    f.push(Finding::ok(
        c::ATPKG,
        Class::Install,
        "the atpkg speaking",
        format!(
            "this atpkg is {} at {}",
            aterm_types::version::app_version(),
            std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "<unknown path>".to_string())
        ),
    ));
    let app_dirs: Vec<PathBuf> = std::iter::once(PathBuf::from("/Applications"))
        .chain(home.map(|h| h.join("Applications")))
        .collect();
    if let Some(row) = report_aterm_posture(
        layout,
        &app_dirs,
        aterm_update_core::settings::update_auto_apply(),
    ) {
        // A downloaded build waits for a window: a state that ends by itself.
        f.push(match row.level {
            Level::Note => {
                Finding::waits(c::APP, Class::Install, "the aterm app's update", row.text)
            }
            _ => Finding::of_row(
                c::APP,
                Class::Install,
                "the aterm app's update",
                row,
                "",
                Remedy::None,
            ),
        });
    }

    // (1) Trust root and index source.
    f.push(Finding::ok(
        c::INDEX_SOURCE,
        Class::Person,
        "the index source",
        format!(
            "index source github.com/{}",
            crate::resolve_account(cfg_account).slug()
        ),
    ));
    if crate::manager_enabled() {
        // The root is the paper master, the anchor the app updater uses, not a
        // package-specific key: naming it answers "which trust root is this build on?"
        // without reading source.
        f.push(Finding::ok(
            c::TRUST_ROOT,
            Class::Person,
            "the trust root",
            format!(
                "paper master pinned (fingerprint {}, {} key(s))",
                crate::root_key_fingerprint(),
                crate::PKG_TRUST_ANCHORS.len()
            ),
        ));
    } else {
        f.push(Finding::change(
            c::TRUST_ROOT,
            Class::Person,
            "the trust root",
            "disabled/inert (no paper master compiled in: pins::PAPER_MASTER_PUBKEYS is empty) \
             — this build installs nothing",
            "the paper master, compiled in",
            Remedy::None,
        ));
    }
    // The table's own spelling: a retired key to rename or remove. Notes, never failures —
    // every one of them is still honoured or harmlessly ignored.
    for note in &probes.config_notes {
        f.push(Finding::note(
            c::CONFIG,
            Class::Person,
            "the [packages] table",
            note.clone(),
        ));
    }
    if let Some(ignored) = probes.ignored_prefix.as_deref() {
        f.push(Finding::change(
            c::of(c::CONFIG, "prefix"),
            Class::Person,
            "the [packages] prefix",
            crate::config::ignored_prefix_note(ignored),
            "a prefix atpkg honours",
            Remedy::None,
        ));
    }
    for (name, setting) in &probes.retired_env {
        f.push(Finding::change(
            c::of(c::RETIRED_ENV, name),
            Class::Person,
            format!("${name}"),
            format!("${name} is exported but does nothing — delete it and use {setting}"),
            "unset",
            Remedy::Person {
                pane: String::from("the shell's startup files"),
                how: format!("delete ${name} and use {setting}"),
            },
        ));
    }

    // (2) Prefix and store.
    if layout.prefix.is_dir() {
        f.push(Finding::ok(
            c::PREFIX,
            Class::Install,
            "the prefix",
            format!("prefix {}", layout.prefix.display()),
        ));
    } else {
        f.push(Finding::change(
            c::PREFIX,
            Class::Install,
            "the prefix",
            format!(
                "prefix {} does not exist yet (nothing installed)",
                layout.prefix.display()
            ),
            "laid by an install",
            Remedy::Command(String::from("aterm pkg install --default-set")),
        ));
    }

    // (3) PATH wiring.
    let bin_dir = layout.bin_dir();
    let on_path = path_var
        .map(|p| std::env::split_paths(p).any(|d| d == bin_dir))
        .unwrap_or(false);
    if on_path {
        f.push(Finding::ok(
            c::PATH,
            Class::Person,
            "the managed bin/ on PATH",
            "managed bin/ is on PATH",
        ));
    } else {
        // The path appears once, in the pasteable export. An aterm shell, and any rc file
        // carrying atpkg's block, sources ~/.aterm/shell.d, which appends it.
        let hint = manual_path_hint(&bin_dir);
        f.push(Finding::change(
            c::PATH,
            Class::Person,
            "the managed bin/ on PATH",
            format!("managed bin/ is not on PATH here; a new aterm shell adds it, or add: {hint}"),
            "on PATH",
            Remedy::Command(hint),
        ));
    }

    // (3b) Reroute (`crate::reroute`): each stub's state, then PATH order, not presence: a
    // `~/.cargo/bin` ahead of everything managed makes a bare `cargo` in a session run
    // upstream Rust silently. All advisory (warn, exit 0): outside an aterm session the
    // reroute dir is not on PATH by design (the prepend is session-scoped), and a missing
    // stub is re-laid at the next spawn. Doctor names; it never lays.
    let reroute_dir = layout.reroute_dir();
    // Every agent program whose `agents/` twin stands gets a row too: its stub routes
    // `claude` to the managed copy at exec time in a shell whose PATH predates it.
    let agent_rows = crate::reroute::agents_routes(layout)
        .into_iter()
        .map(|name| (name, crate::reroute::stub_state(layout, name)));
    for (name, state) in crate::reroute::states(layout).into_iter().chain(agent_rows) {
        let id = c::of(c::STUBS, name);
        let title = format!("the reroute stub {name}");
        f.push(match state {
            crate::reroute::StubState::Laid => {
                Finding::ok(id, Class::Relay, title, format!("reroute stub {name} laid"))
            }
            crate::reroute::StubState::Missing if cfg!(windows) => Finding::note(
                id,
                Class::Relay,
                title,
                format!("reroute stub {name} not laid: Windows lays no reroute stubs yet (TARGET)"),
            ),
            // A recorded decline lays none — nothing will, so it is a state, not a warning.
            crate::reroute::StubState::Missing if layout.declined().is_file() => Finding::note(
                id,
                Class::Relay,
                title,
                format!("reroute stub {name} not laid: {DECLINED_LINE}"),
            ),
            crate::reroute::StubState::Missing => Finding::change(
                id,
                Class::Relay,
                title,
                format!(
                    "reroute stub {name} missing; the next aterm session lays it — now: aterm \
                     pkg repair"
                ),
                "laid",
                Remedy::Repair,
            ),
            crate::reroute::StubState::Foreign => Finding::change(
                id,
                Class::Person,
                title,
                format!(
                    "{} is not ours (foreign file; never touched)",
                    crate::reroute::stub_path(layout, name).display()
                ),
                "atpkg's stub",
                Remedy::None,
            ),
        });
    }
    // abrew's `brew` stub rides the same directory under its own marker
    // (docs/DESIGN-abrew-2026-10-19.md §6): one row beside the reroute rows.
    if !cfg!(windows) {
        // A foreign file at the stub's path is the person's: never touched, so no remedy.
        let (row, class) = crate::abrew::stub::doctor_row(layout);
        let remedy = if class == Class::Relay {
            Remedy::Repair
        } else {
            Remedy::None
        };
        f.push(Finding::of_row(
            c::of(c::STUBS, "brew"),
            class,
            "the reroute stub brew (abrew)",
            row,
            "laid",
            remedy,
        ));
    }
    let path_entries: Vec<std::path::PathBuf> = path_var
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    let reroute_at = path_entries.iter().position(|d| *d == reroute_dir);
    let upstream_at = upstream_index_on_path(&path_entries, &layout.prefix, "cargo");
    let title = "the reroute dir on PATH";
    f.push(match (reroute_at, upstream_at) {
        (None, _) => Finding::note(
            c::REROUTE,
            Class::Person,
            title,
            "reroute dir not on this PATH (expected outside an aterm session)",
        ),
        (Some(i), Some(j)) if i < j => Finding::ok(
            c::REROUTE,
            Class::Person,
            title,
            format!("reroute dir precedes upstream cargo (PATH[{i}] < PATH[{j}])"),
        ),
        (Some(i), Some(j)) => Finding::change(
            c::REROUTE,
            Class::Person,
            title,
            format!(
                "upstream cargo (PATH[{j}]) is ahead of the reroute dir (PATH[{i}]) in this \
                 shell; a new aterm tab puts the reroute dir first"
            ),
            "ahead of upstream cargo",
            Remedy::None,
        ),
        (Some(i), None) => Finding::ok(
            c::REROUTE,
            Class::Person,
            title,
            format!("reroute dir on PATH (PATH[{i}]); no upstream cargo on this PATH"),
        ),
    });

    // (4) Broken shim scan of bin/: a shim whose forward target is gone (a dangling symlink,
    // or on Windows a `.cmd` forwarding to a missing exe); a tombstone has no target and is
    // never flagged. A target that cannot be checked (EACCES, privacy consent's EPERM, EIO)
    // is no evidence of a fault, and `tools/install.sh` points failed installs at this
    // report, so it is a warn naming the cause, not a broken shim.
    if let Ok(entries) = std::fs::read_dir(&bin_dir) {
        for e in entries.flatten() {
            let shim = e.path();
            let Some(target) = crate::platform::resolve_shim(&shim) else {
                continue;
            };
            let name = e.file_name().to_string_lossy().into_owned();
            let title = format!("the bin/ shim {name}");
            // A shim into the store under a name no shim may carry: an older atpkg laid it
            // before the name joined the deny-list (design S13). Its own row, never a
            // broken shim: the shim sweep removes it (`aterm pkg repair`, and the base
            // relay at the end of every install or update pass).
            if crate::activate::refused_name_shim(layout, &name, &shim).is_some() {
                f.push(Finding::change(
                    c::of(c::REFUSED_SHIM, &name),
                    Class::Relay,
                    title,
                    format!(
                        "bin shim {} forwards into the store under a name no shim may carry \
                         (a shell or another core command) \u{2014} an older atpkg laid it, and \
                         the shim sweep removes it",
                        shim.display()
                    ),
                    "absent",
                    Remedy::Repair,
                ));
                continue;
            }
            let id = c::of(c::SHIMS, &name);
            match crate::store::presence(&target) {
                crate::store::Presence::Present => {}
                crate::store::Presence::Absent => f.push(
                    Finding::change(
                        id,
                        Class::Relay,
                        title,
                        format!("broken bin shim {}", shim.display()),
                        "forwarding to a build in the store",
                        Remedy::Repair,
                    )
                    .essential(),
                ),
                crate::store::Presence::Unknown(why) => f.push(Finding::unread(
                    id,
                    Class::Relay,
                    title,
                    format!(
                        "bin shim {} could not be checked: its target {} is unreadable ({why}) \
                         — not a broken shim",
                        shim.display(),
                        target.display()
                    ),
                )),
            }
        }
    }

    let active = crate::ops::active_builds(layout);

    // (4b) The exposed names refused a shim (design S13): a signed `exposes` naming a shell,
    // another command a shim must never intercept, or no tool name at all. The landing laid
    // no shim for it and recorded it ([`crate::status::record_refused_shims`]) under its
    // build; the row says the entry of the build `store/<program>/current` names, so a
    // program whose every name was refused, and so has no shim, is still read, and a build
    // staged beside it says nothing until a flip makes it current. A note, never a change:
    // the publisher signed the name, and nothing on this machine changes it.
    let title = "the tools refused a shim";
    match crate::status::read_checked(layout) {
        Err(why) => f.push(Finding::unread(
            c::REFUSED,
            Class::Person,
            title,
            format!(
                "status.toml could not be read ({why}), so which exposed names were refused \
                 a shim is unknown"
            ),
        )),
        Ok(record) => {
            let refused: Vec<(&String, &crate::status::RefusedShims)> = record
                .as_ref()
                .map(|s| {
                    s.refused_shims
                        .keys()
                        .filter_map(|program| {
                            let build = crate::alab::current_build(layout, program)?;
                            Some((program, s.refused_for(program, build)?))
                        })
                        .collect()
                })
                .unwrap_or_default();
            for (program, r) in &refused {
                f.push(Finding::note(
                    c::of(c::REFUSED, program),
                    Class::Person,
                    format!("{program}'s tools refused a shim"),
                    refused_line(program, r),
                ));
            }
            if refused.is_empty() && !active.is_empty() {
                f.push(Finding::ok(
                    c::REFUSED,
                    Class::Person,
                    title,
                    "no landing recorded an exposed name refused a shim for an active build",
                ));
            }
        }
    }

    // (5) Active-build store integrity. Each program whose store tree is missing or
    // incomplete is its own row, whose remedy is its one structural repair (`install
    // <program>`), so the next act names one instead of a menu. No count here: (11) says
    // how many programs are active, from the tally that also counts a dev-linked program.
    let mut store_broken = false;
    for (program, build) in &active {
        let bd = layout.build_dir(program, *build);
        if !bd.is_dir() || !crate::store::build_is_complete(&bd) {
            store_broken = true;
            f.push(
                Finding::change(
                    c::of(c::STORE, program),
                    Class::Install,
                    format!("{program}'s active store tree"),
                    format!(
                        "active {} store missing/incomplete",
                        crate::vendor_direct::display_build(program, *build)
                    ),
                    "complete",
                    Remedy::Command(format!("aterm pkg install {program}")),
                )
                .essential(),
            );
        }
    }
    if !store_broken && !active.is_empty() {
        f.push(Finding::ok(
            c::STORE,
            Class::Install,
            "the active builds' store trees",
            "every active build's store tree is complete",
        ));
    }

    // (5c) Solvers the `trust` bundle pins privately. trustc resolves its SMT solver as a
    // sibling of its own executable (trust's `trust_verify.rs`): an explicit `AY_PATH` wins,
    // else the copy in the bundle's own `bin/`, with no PATH fallback. The pin is
    // deliberate: a proof records which solver produced it, and a solver that floated with
    // atpkg's installs would make proof identity float too. A managed `ay` newer than the
    // bundle's is not the one Trust uses, and nothing else says so: reported, never failed.
    if let Some(trust_build) = active
        .iter()
        .find(|(p, _)| p.as_str() == "trust")
        .map(|(_, b)| *b)
    {
        let bundle_bin = layout.build_dir("trust", trust_build).join("bin");
        for (program, managed_build) in &active {
            if program.as_str() == "trust" {
                continue;
            }
            let pinned = bundle_bin.join(program);
            if !pinned.is_file() {
                continue;
            }
            let pinned_version = probe_version(&pinned);
            let managed_version = probe_version(
                &layout
                    .build_dir(program, *managed_build)
                    .join("bin")
                    .join(program),
            );
            // Equal versions are healthy; an unanswered probe on either side is not
            // evidence of divergence, so it stays silent.
            if pinned_version == managed_version
                || pinned_version == "unknown"
                || managed_version == "unknown"
            {
                continue;
            }
            // No `AY_PATH` hint: it could point at an older solver than the bundle's.
            f.push(Finding::note(
                c::of(c::SOLVER, program),
                Class::Install,
                format!("the {program} Trust builds use"),
                format!(
                    "Trust builds use the {program} pinned inside the trust bundle \
                     ({pinned_version}), not the managed {program} {managed_version} (build \
                     {managed_build})"
                ),
            ));
        }
    }

    // (5f) tippy's compiler is a plain file, and the rustup view is a clone of the build.
    // Older bundles ship `bin/rustc` as a separately signed copy of `trustc` (an ad-hoc
    // signature embeds the file name, so the bytes differ), which their tippy refuses; atpkg
    // leaves the store alone and routes the trust shims through a per-build exec root where
    // `rustc` holds `trustc`'s bytes ([`crate::compat`]). Current bundles ship Trust's names
    // only, and the stock names live in the rustup view ([`crate::seam::refresh_view`]). The
    // check reads file facts (`lstat`, byte compares), never a run of tippy, whose
    // ancestor-directory checks refuse intermittently and would make the verdict flap.
    //
    // (a) `trustc` must be a plain file while the bundle ships tippy, which refuses a
    //     symlinked compiler.
    // (b) Each stock name in a recorded seam's view must be a clone of the store's Trust
    //     tool holding its bytes, or `cargo +trust` runs something else; `repair` fixes it.
    // (c) A build that [`crate::compat::needs_root`] is a note with a matching `Deep` root
    //     and every shim routed, else a warn with both inodes and the fix: `repair`, or for
    //     a link or file at `compat`/`compat/trust` (which repair refuses) its removal.
    // (d) A root no build needs is an advisory warn naming `aterm pkg gc`
    //     ([`crate::compat::strays`]).
    // (e) A build that needs no root gets no (c) line.
    // (f) An unreadable `bin/rustc` or `bin/trustc` proves neither: a warn naming the read.
    // (g) A directory on the build's chain that `targo trust` refuses (owned by neither root
    //     nor us, group/other-writable, or a link below the sysroot) is a warn: `chmod go-w`
    //     for a writable one of ours, a prefix of ours for root's, the bundle's fix for a link.
    //
    // Warns other than (d) withhold "healthy". "Cannot run" rows carry [`c::CANNOT_RUN`],
    // "may not run" or "cannot tell" ones [`c::UNPROVEN`], so the verdict never claims a
    // tool cannot run where its row only failed to show that it can.
    if let Some(trust_build) = active.get("trust").copied() {
        let bin = layout.build_dir("trust", trust_build).join("bin");
        let trustc = bin.join("trustc");
        let refused = match std::fs::symlink_metadata(&trustc) {
            Ok(m) if m.file_type().is_symlink() => Some("a symbolic link"),
            Ok(m) if !m.is_file() => Some("not a regular file"),
            // Absent: the bundle is missing its compiler, which the shim and
            // integrity checks above already name; nothing tippy-specific to add.
            _ => None,
        };
        if let Some(what) = refused
            && bin.join("tippy").is_file()
        {
            f.push(
                Finding::change(
                    c::TRUST_COMPILER,
                    Class::Person,
                    "the trust bundle's compiler, as tippy needs it",
                    format!(
                        "trust build {trust_build}: tippy cannot run — {} is {what}, and tippy \
                         requires the selected compiler trustc to be a plain file, so every \
                         tippy run stops at a setup error (the bundle's fix: \
                         {TRUST_COMPILER_FIX})",
                        trustc.display()
                    ),
                    "a plain file",
                    Remedy::Person {
                        pane: String::from("the trust bundle"),
                        how: TRUST_COMPILER_FIX.to_owned(),
                    },
                )
                .detail(c::CANNOT_RUN),
            );
        }
        view_stock_names_check(&mut f, layout, Some(bin.clone()));
        // (g) One tool that cannot run, a row per refused directory.
        if let Some(chain) = probes.trust_chain.as_ref()
            && chain.build == trust_build
        {
            for (n, refusal) in chain.refused.iter().enumerate() {
                let row = trust_chain_finding(trust_build, refusal);
                f.push(if n == 0 {
                    row.detail(c::CANNOT_RUN)
                } else {
                    row
                });
            }
        }
        match crate::compat::inspect(layout, trust_build) {
            Some(Ok(inspection)) => {
                let (row, claim, remedy) = exec_root_line(trust_build, &inspection);
                // A root `repair` cannot lay (a link or file in its place) is the person's.
                let class = if matches!(remedy, Remedy::Person { .. }) {
                    Class::Person
                } else {
                    Class::Relay
                };
                let found = Finding::of_row(
                    c::EXEC_ROOTS,
                    class,
                    "the trust build's exec root",
                    row,
                    "every trust shim routed through a matching exec root",
                    remedy,
                );
                f.push(match claim {
                    ToolClaim::Runs => found,
                    ToolClaim::CannotRun => found.detail(c::CANNOT_RUN),
                    ToolClaim::Unproven => found.detail(c::UNPROVEN),
                });
            }
            Some(Err(why)) => f.push(
                Finding::unread(
                    c::EXEC_ROOTS,
                    Class::Relay,
                    "the trust build's exec root",
                    exec_root_unread_line(trust_build, &why),
                )
                .remedy(Remedy::Command(String::from("aterm pkg verify trust")))
                .detail(c::UNPROVEN),
            ),
            None => {}
        }
    }
    // A dev-linked trust still has a seam when no shim in `bin/` names a store build: under
    // a link every trust shim resolves into the checkout, so `active` has no trust build
    // even when the store holds one. Its view is checked too — against the checkout, or
    // against the store's `current` build under a dev-link the seam refused.
    if !active.contains_key("trust") {
        let current = crate::seam::store_current(layout);
        let store_bin = std::fs::read_link(&current)
            .ok()
            .map(|raw| {
                if raw.is_absolute() {
                    raw
                } else {
                    current.parent().map_or(raw.clone(), |d| d.join(raw))
                }
            })
            .map(|build| build.join("bin"))
            .filter(|bin| bin.is_dir());
        view_stock_names_check(&mut f, layout, store_bin);
    }
    for (path, why) in crate::compat::strays(layout) {
        f.push(Finding::change(
            c::of(c::EXEC_ROOT_STRAY, path.display()),
            Class::Asked,
            "an exec root no build needs",
            stray_root_line(&path, why),
            "removed",
            Remedy::Command(String::from("aterm pkg gc")),
        ));
    }

    // (5d) The workspace the operator is in against the installed Trust. atpkg keeps the
    // toolchain current with what is published; it cannot stop a repository depending on a
    // Trust build never published, whose symptom is targo refusing the workspace with an
    // "unknown field" error that looks like a manifest typo. The installed targo decides,
    // and the row names what is behind: the publisher's toolchain, not this machine. The
    // other way round, a workspace still naming a key the installed Trust retired is
    // refused with the setting that replaced it, and the workspace is what is behind.
    if let Some(ws) = &probes.workspace {
        let trust_build = active
            .iter()
            .find(|(p, _)| p.as_str() == "trust")
            .map(|(_, b)| b.to_string())
            .unwrap_or_else(|| "?".to_string());
        let title = "the workspace's [trust] policy";
        let accepted = "accepted by the installed targo";
        f.push(match &ws.policy {
            WorkspacePolicy::Accepted => Finding::ok(
                c::WORKSPACE,
                Class::Install,
                title,
                format!(
                    "workspace {} declares a [trust] policy the installed trust build \
                     {trust_build} accepts",
                    ws.workspace.display()
                ),
            )
            .essential(),
            WorkspacePolicy::UnknownField { field } => Finding::change(
                c::WORKSPACE,
                Class::Install,
                title,
                format!(
                    "workspace {} declares a [trust] policy key the installed trust build \
                     {trust_build} does not know: `{field}`. Whoever committed it built on a \
                     NEWER Trust seal that atpkg has not published, so this workspace does not \
                     open with targo on any atpkg-managed machine. Cure (a rostered publisher, \
                     on the machine holding that seal): {PUBLISH_RUSTC_GROUP}; then here: aterm \
                     pkg update trust",
                    ws.workspace.display()
                ),
                accepted,
                Remedy::Command(format!(
                    "publish the newer Trust coherence group ({PUBLISH_RUSTC_GROUP} on a \
                     rostered machine holding the seal), then aterm pkg update trust"
                )),
            )
            .essential(),
            WorkspacePolicy::RetiredKey { key, replacement } => Finding::change(
                c::WORKSPACE,
                Class::Person,
                title,
                format!(
                    "workspace {} declares a [trust] policy key the installed trust build \
                     {trust_build} has retired: `{key}` was {replacement}. The workspace is \
                     older than this toolchain, so targo refuses it here. Cure (in the \
                     workspace's Cargo.toml, not on this machine): edit its [trust] table as \
                     that says",
                    ws.workspace.display()
                ),
                accepted,
                Remedy::Person {
                    pane: ws.workspace.join("Cargo.toml").display().to_string(),
                    how: format!("edit its [trust] table: `{key}` was {replacement}"),
                },
            )
            .essential(),
            WorkspacePolicy::ProbeFailed { why } => Finding::unread(
                c::WORKSPACE,
                Class::Install,
                title,
                format!(
                    "could not ask the installed targo about workspace {} ({why})",
                    ws.workspace.display()
                ),
            ),
        });
    }

    // (5e) A local toolchain behind rustup's `trust` channel. Newer than the store's (or of
    // unknown age), it is the user's to keep: the row says which compiler `cargo +trust`
    // gets and the one re-point that hands it back (the only row about this entry; (9)
    // stays silent on it). Older or gone, it is stale and `repair` re-points the channel at
    // the store, as the next unattended pass does by itself for an older live build tree
    // ([`crate::seam::live_build_tree`]); `repair` is named because it acts now. Where no
    // Trust is published for this target and the store holds none, the local toolchain is
    // the only Trust there is: a note ([`trust_unserved_here`]).
    if let Some(seal) = &probes.local_seal {
        let title = "rustup's trust channel";
        let seam = "the managed seam";
        f.push(match &seal.stale {
            Some(crate::seam::Stale::Dangling) => Finding::change(
                c::RUSTUP_LOCAL,
                Class::Relay,
                title,
                format!(
                    "rustup's trust channel names {}, which no longer exists, so `cargo +trust` \
                     and every repo pinning `channel = \"trust\"` fail with `'rustc' is not \
                     installed for the custom toolchain 'trust'`; fix: `aterm pkg repair` \
                     re-points it at the store",
                    seal.link_target.display()
                ),
                seam,
                Remedy::Repair,
            ),
            Some(crate::seam::Stale::Older { its, store }) => Finding::change(
                c::RUSTUP_LOCAL,
                Class::Asked,
                title,
                format!(
                    "rustup's trust channel resolves to a LOCAL toolchain OLDER than the atpkg \
                     store's: {} (trustc {}, {}; the store's is {}), so `cargo +trust` and \
                     every repo pinning `channel = \"trust\"` compile with a stale Trust; fix: \
                     `aterm pkg repair` re-points it at the store (the old toolchain is left \
                     where it is)",
                    seal.link_target.display(),
                    seal.trustc,
                    its,
                    store
                ),
                seam,
                Remedy::Repair,
            ),
            None => {
                // `None` also covers no store build and a dev-linked trust, where the
                // target may be gone and there is no managed toolchain to point at.
                let managed = layout.program_current("trust").join("bin").is_dir();
                let exists = seal.link_target.exists();
                if exists && !managed && trust_unserved_here(layout) {
                    // Where no Trust build is published for this target (Linux, Windows:
                    // trust's own row reads `unavailable on <target>`), a local toolchain
                    // is the only Trust this machine can have, and the route
                    // docs/LINUX-LOCAL-TOOLCHAIN.md gives: a fact, not a thing to look at.
                    Finding::note(
                        c::RUSTUP_LOCAL,
                        Class::Person,
                        title,
                        format!(
                            "rustup `trust` names a local toolchain: {} (trustc {}); no managed \
                             Trust is published for this target, so `cargo +trust` and projects \
                             pinning `channel = \"trust\"` build with it{}",
                            seal.link_target.display(),
                            seal.trustc,
                            if cfg!(target_os = "linux") {
                                " (docs/LINUX-LOCAL-TOOLCHAIN.md)"
                            } else {
                                ""
                            }
                        ),
                    )
                } else {
                    let what = if exists {
                        format!(
                            "names a local toolchain, not the managed one: {} (trustc {}); \
                             `cargo +trust` and projects pinning `channel = \"trust\"` build \
                             with it",
                            seal.link_target.display(),
                            seal.trustc
                        )
                    } else {
                        format!(
                            "names {}, which no longer exists, so `cargo +trust` and projects \
                             pinning `channel = \"trust\"` fail",
                            seal.link_target.display()
                        )
                    };
                    let (fix, remedy) = if managed {
                        let repoint = repoint_fix(layout, &seal.link);
                        (
                            format!(" — to use the managed one: {repoint}"),
                            Remedy::Command(repoint),
                        )
                    } else {
                        (String::new(), Remedy::None)
                    };
                    Finding::change(
                        c::RUSTUP_LOCAL,
                        Class::Person,
                        title,
                        format!("rustup `trust` {what}{fix}"),
                        seam,
                        remedy,
                    )
                }
            }
        });
    }

    // The toolchain update that waits, and the runs holding builds: states, never failures.
    // An unattended pass that found the Trust toolchain in use staged the new one and holds
    // its flip for quiet ([`crate::quiet`]); a release cut or an `aterm pkg lease` command
    // holds a lease on the build it resolved ([`crate::lease`]), which gc keeps. Each row
    // says what, on whom, until when, and the one command that ends the wait.
    f.extend(hold_findings(layout, now));

    // (5b) Live-build witness. `gc` reclaims a program's superseded builds only when the
    // `store/<program>/current` symlink and the derived `bin/` shim view name the same build;
    // otherwise it abstains rather than guess (a guess can delete a live tree) and builds
    // accumulate silently, so this makes it visible. A genuine disagreement is structural
    // (some tool on PATH runs a build activation does not select), and its repair is a
    // re-run of `update`, which re-flips every diverged shim; an absent witness warns.
    let live = crate::gc::live_builds(layout);
    for d in live.diverged() {
        let id = c::of(c::LIVE, &d.program);
        let title = format!("{}'s live build", d.program);
        let want = "one build, recorded current and run";
        let update = Remedy::Command(String::from("aterm pkg update"));
        f.push(match &d.reason {
            crate::gc::Diverged::LinkShimMismatch {
                link_says,
                shims_say,
            } => Finding::change(
                id,
                Class::Install,
                title,
                format!(
                    "{}: its `current` link selects {} but its bin/ shims run {} — fix: aterm \
                     pkg update {}",
                    d.program,
                    crate::vendor_direct::build_words(*link_says),
                    crate::vendor_direct::build_words(*shims_say),
                    d.program
                ),
                want,
                update,
            )
            .essential(),
            crate::gc::Diverged::ShimsDisagree { builds } => Finding::change(
                id,
                Class::Install,
                title,
                format!(
                    "{}: its bin/ shims are split across builds {} — fix: aterm pkg repair, \
                     then aterm pkg update {}",
                    d.program,
                    build_list(builds),
                    d.program
                ),
                want,
                update,
            )
            .essential(),
            crate::gc::Diverged::NoLiveWitness { shims_say } => Finding::change(
                id,
                Class::Install,
                title,
                format!(
                    "{}: {} runs but is not recorded as current, so its older builds are never \
                     removed — fix: aterm pkg update {}",
                    d.program,
                    crate::vendor_direct::build_words(*shims_say),
                    d.program
                ),
                want,
                Remedy::Command(format!("aterm pkg update {}", d.program)),
            ),
        });
    }
    f.push(Finding::ok(
        c::LIVE,
        Class::Install,
        "the live builds",
        format!("{} program(s) with a proven live build", live.len()),
    ));

    // (6) Shell hooks and fish safety.
    if let Some(home) = home {
        let aterm = home.join(".aterm");
        let shell_d = aterm.join("shell.d");
        // Probe the dialect this platform's interactive shell sources: `.ps1` (PowerShell) on
        // Windows, `.zsh` elsewhere. An install writes the whole set, so a native hook means
        // PATH wiring is in place.
        let native_hook = format!("{}.{}", crate::hooks::HOOK_BASENAME, native_hook_ext());
        f.push(if shell_d.join(&native_hook).is_file() {
            Finding::ok(
                c::HOOKS,
                Class::Relay,
                "the shell.d hooks",
                "shell.d hooks present",
            )
        } else {
            Finding::change(
                c::HOOKS,
                Class::Relay,
                "the shell.d hooks",
                "shell.d hooks not generated yet (an install writes them)",
                "present",
                Remedy::Repair,
            )
        });
        if let Ok(entries) = std::fs::read_dir(&shell_d) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.ends_with(".sh") {
                    f.push(
                        Finding::change(
                            c::of(c::FISH_SAFE, &name),
                            Class::Person,
                            format!("shell.d/{name}"),
                            format!("shell.d/{name}: a POSIX .sh breaks fish — remove it"),
                            "absent",
                            Remedy::Command(format!(
                                "rm {}",
                                crate::platform::sh_quote_str(&e.path().to_string_lossy())
                            )),
                        )
                        .essential(),
                    );
                }
            }
        }
        // (6b) rc wiring: one row per rc file that exists, and one for the login shell's rc
        // when it is absent (`hooks::absent_login_rc`: the one rc a pass creates), telling
        // wired, opted out and consent-fenced apart. `rc_wiring` reads six named files, among
        // them install.sh's two bash login profiles (`hooks::INSTALL_SH_PROFILES`), so a Mac
        // wired by install.sh is not called unwired; when none sources shell.d and no pass is
        // about to make the login rc, the note names the six (`hooks::rc_files_read`)
        // instead of calling the machine aterm-only.
        #[cfg(unix)]
        {
            let wiring = crate::hooks::rc_wiring(home);
            let absent = probes
                .login_rc
                .and_then(|rc| crate::hooks::absent_login_rc(home, rc).map(|state| (rc, state)));
            let names = |want: crate::hooks::RcState| -> Vec<String> {
                wiring
                    .iter()
                    .filter(|(_, s)| *s == want)
                    .map(|(rc, _)| format!("~/{rc}"))
                    .collect()
            };
            let wired = names(crate::hooks::RcState::Wired);
            let elsewhere = names(crate::hooks::RcState::SourcedElsewhere);
            let title = "the rc files' shell.d wiring";
            if !wired.is_empty() {
                f.push(Finding::ok(
                    c::RC,
                    Class::Relay,
                    title,
                    format!(
                        "{} sources ~/.aterm/shell.d (atpkg's block; delete it to opt out — \
                         `aterm pkg repair` puts it back)",
                        wired.join(", ")
                    ),
                ));
            }
            if !elsewhere.is_empty() {
                f.push(Finding::ok(
                    format!("{}.elsewhere", c::RC),
                    Class::Person,
                    title,
                    format!(
                        "{} sources ~/.aterm/shell.d via a line that is not atpkg's block \
                         (tools/install.sh's, or your own)",
                        elsewhere.join(", ")
                    ),
                ));
            }
            if let Some((rc, state)) = absent {
                let id = c::of(c::RC, rc);
                let title = format!("~/{rc}");
                match state {
                    crate::hooks::RcState::Unwired => f.push(Finding::change(
                        id,
                        Class::Relay,
                        title,
                        format!(
                            "~/{rc} does not exist, and your login shell reads it (the next \
                             install or update pass, or `aterm pkg repair`, creates it holding \
                             atpkg's block)"
                        ),
                        "sources ~/.aterm/shell.d",
                        Remedy::Repair,
                    )),
                    crate::hooks::RcState::OptedOut => f.push(Finding::note(
                        id,
                        Class::Asked,
                        title,
                        format!(
                            "~/{rc}: you deleted the one atpkg made for your login shell — \
                             opted out, left absent by every install and update pass (`aterm \
                             pkg repair` makes it again)"
                        ),
                    )),
                    crate::hooks::RcState::ConsentFenced => f.push(Finding::note(
                        id,
                        Class::Person,
                        title,
                        format!(
                            "~/{rc} is absent and its folder resolves under one macOS guards \
                             with a consent dialog, so no atpkg pass makes it; source \
                             ~/.aterm/shell.d from your login shell's rc yourself, or reach the \
                             tools with `aterm <tool>`"
                        ),
                    )),
                    crate::hooks::RcState::Wired | crate::hooks::RcState::SourcedElsewhere => {}
                }
            }
            for (rc, state) in &wiring {
                let id = c::of(c::RC, rc);
                let title = format!("~/{rc}");
                match state {
                    crate::hooks::RcState::Wired | crate::hooks::RcState::SourcedElsewhere => {}
                    crate::hooks::RcState::OptedOut => f.push(Finding::note(
                        id,
                        Class::Asked,
                        title,
                        format!(
                            "~/{rc}: atpkg's block was deleted — opted out, left alone by \
                             every install and update pass (`aterm pkg repair` lays it again)"
                        ),
                    )),
                    crate::hooks::RcState::ConsentFenced => f.push(Finding::note(
                        id,
                        Class::Person,
                        title,
                        format!(
                            "~/{rc} resolves under a folder macOS guards with a consent \
                             dialog, so no atpkg pass opens it — it is left unwired; source \
                             ~/.aterm/shell.d from it yourself, or reach the tools with `aterm \
                             <tool>`"
                        ),
                    )),
                    crate::hooks::RcState::Unwired => f.push(Finding::change(
                        id,
                        Class::Relay,
                        title,
                        format!(
                            "~/{rc} does not source ~/.aterm/shell.d (an install pass or `aterm \
                             pkg repair` appends atpkg's block)"
                        ),
                        "sources ~/.aterm/shell.d",
                        Remedy::Repair,
                    )),
                }
            }
            let login_rc_coming = matches!(absent, Some((_, crate::hooks::RcState::Unwired)));
            if wired.is_empty() && elsewhere.is_empty() && !login_rc_coming {
                let checked: Vec<String> = crate::hooks::rc_files_read()
                    .map(|rc| format!("~/{rc}"))
                    .collect();
                f.push(Finding::note(
                    format!("{}.none", c::RC),
                    Class::Relay,
                    title,
                    format!(
                        "none of {} sources ~/.aterm/shell.d, so a shell outside aterm that \
                         starts from only those files does not reach the managed tools; use \
                         `aterm <tool>` there, or: {}",
                        checked.join(", "),
                        manual_path_hint(&bin_dir)
                    ),
                ));
            }
        }
        // (6c) A file at a command path: a copy never updates, and either way it runs
        // instead of the installed aterm wherever `~/.local/bin` leads PATH. What replaces
        // it differs by platform (`hooks::copied_command_remedy`): on macOS the installed
        // app's own `atpkg repair` — `aterm` may be the very copy named here.
        #[cfg(unix)]
        for path in crate::hooks::copied_command_links(home) {
            let remedy = crate::hooks::copied_command_remedy(&path);
            f.push(Finding::change(
                c::of(c::COPIED_COMMANDS, path.display()),
                Class::Asked,
                format!("{}", path.display()),
                format!(
                    "{} is a file, not a link to the installed aterm, and it runs instead \
                         of it (a copy that never updates, or a build of your own) — fix: \
                         {remedy}",
                    path.display()
                ),
                "a link to the installed aterm",
                Remedy::Person {
                    pane: path.display().to_string(),
                    how: remedy,
                },
            ));
        }
        // Privacy of the login-sourced dirs (read-only: doctor never chmods).
        for dir in [&aterm, &shell_d] {
            if let Ok(m) = std::fs::symlink_metadata(dir)
                && m.file_type().is_dir()
                && !crate::platform::dir_meta_is_private(&m)
            {
                f.push(
                    Finding::change(
                        c::of(c::PRIVATE, dir.display()),
                        Class::Person,
                        format!("{}", dir.display()),
                        format!(
                            "{} is group/other-writable (login shells source it)",
                            dir.display()
                        ),
                        "writable by you alone",
                        Remedy::Command(format!(
                            "chmod go-w {}",
                            crate::platform::sh_quote_str(&dir.to_string_lossy())
                        )),
                    )
                    .essential(),
                );
            }
        }
    }

    // (7) Disk headroom.
    let title = "free space under the prefix";
    f.push(match crate::freespace::available_bytes(&layout.prefix) {
        Some(free) if free < 5 * GIB => Finding::change(
            c::DISK,
            Class::Person,
            title,
            format!(
                "only {} free (a toolchain update needs ~2.5x its artifact size)",
                crate::cost::human_bytes(free)
            ),
            "5 GiB free",
            Remedy::None,
        ),
        Some(free) => Finding::ok(
            c::DISK,
            Class::Person,
            title,
            format!("{} free", crate::cost::human_bytes(free)),
        ),
        None => Finding::unread(c::DISK, Class::Person, title, "could not query free space"),
    });

    // (7b) The Developer Tools list ([`crate::devtools`], macOS): a warning when aterm is not
    // on it, never a problem — only a person switches it, in System Settings. A report that
    // does not read the machine says nothing.
    if let Some(row) = probes.devtools.as_ref().and_then(crate::devtools::row) {
        let title = "aterm on the Developer Tools list";
        let text = format!("devtools: {}", row.text);
        f.push(match row.level {
            Level::Ok => Finding::ok(c::DEVTOOLS, Class::Person, title, text),
            Level::Note => Finding::note(c::DEVTOOLS, Class::Person, title, text),
            Level::Warn => Finding::person(
                c::DEVTOOLS,
                title,
                text,
                Remedy::Person {
                    pane: String::from("Privacy & Security \u{25b8} Developer Tools"),
                    how: String::from(
                        "switch aterm on in Privacy & Security \u{25b8} Developer Tools",
                    ),
                },
            ),
        });
    }

    // (7c) Universal Control (macOS): the machine setting the pass applies, read through
    // `defaults -currentHost` and never written here; the row carries the revert when it is
    // off and the opt-out when it is about to be.
    if cfg!(target_os = "macos") {
        let state = crate::machine::universal_control_state(&crate::machine::SystemDefaults);
        let policy = crate::config::cached_machine().universal_control();
        f.push(Finding::of_row(
            c::UNIVERSAL_CONTROL,
            Class::Preference,
            "Universal Control",
            state.doctor_row(policy),
            "disabled",
            Remedy::Command(String::from("aterm pkg machine apply")),
        ));
    }

    // (7d) Rosetta 2 (macOS on Apple silicon): read by one `stat`, installed by the apply
    // (never here); a warning only while the apply would install it, with that next step.
    if let Some(row) = probes
        .rosetta
        .and_then(|(posture, wanted)| posture.doctor_row(wanted))
    {
        f.push(Finding::of_row(
            c::ROSETTA,
            // An Apple install, fetched from Apple when macOS does not keep it on disk.
            Class::Install,
            "Rosetta 2",
            row,
            "installed",
            Remedy::Command(String::from("aterm pkg machine apply")),
        ));
    }

    // (8) Index freeze and age, from atpkg's own records. The clock is `last_success_at`,
    // stamped by a pass that resolved the index and ran to its end (member failures have
    // their own rows), never `updated_at`, which every writer moves: hence "completed pass".
    // "Never checked" is said only here; the vendor programs kept at their vendors' heads
    // update without an index pass.
    let vendors = vendor_update_clause(layout, probes);
    let age_title = "the last completed update pass";
    let update = || Remedy::Command(String::from("aterm pkg update"));
    let never_checked = || {
        Finding::change(
            c::INDEX_AGE,
            Class::Install,
            age_title,
            format!("{NEVER_CHECKED}{vendors}"),
            "a completed update pass",
            update(),
        )
    };
    if let Some(status) = crate::status::read(layout) {
        if status.last_success_at.trim().is_empty() {
            f.push(never_checked());
        } else {
            f.push(match index_age_days(&status.last_success_at, now) {
                Some(days) if days > 30 => Finding::change(
                    c::INDEX_AGE,
                    Class::Install,
                    age_title,
                    format!(
                        "{days} day(s) since the last completed update pass ({}) — this machine \
                         has been off, or the app has not run",
                        status.last_success_at
                    ),
                    "within 30 days",
                    update(),
                ),
                Some(days) => Finding::ok(
                    c::INDEX_AGE,
                    Class::Install,
                    age_title,
                    format!("{days} day(s) since the last completed update pass"),
                ),
                None => Finding::unread(
                    c::INDEX_AGE,
                    Class::Install,
                    age_title,
                    "could not parse the last-success time",
                ),
            });
            // Has the index been reached lately: a pass recorded failed or offline after the
            // last success (`last_pass` / `last_pass_at`) means every pass since ran on the
            // cached index. Read by the schedulers' own reader (`Stamps::last_failed`), so
            // doctor and the failure ladder agree on which pass failed. Only a pass that found
            // no host (`PassOutcome::Offline`) waits on the network: the retry ladder's, never
            // a failure, a note whose remedy is the next pass once online (design §3.3). A
            // pass that reached a host and failed (a refused signature, a failed resolve) is
            // no wait: a warn whose one act is `aterm pkg update`, which says why.
            let reached_days = index_age_days(&status.last_success_at, now);
            let stamps =
                aterm_update_core::pkg_check::Stamps::parse(&status.to_toml().unwrap_or_default());
            match reached_days {
                Some(days) if stamps.last_failed() && days > INDEX_UNREACHED_WARN_DAYS => {
                    let lead = format!("the signed index has not been reached for {days} day(s)");
                    let why = match unreached_cause(&status.outcome) {
                        Some(why) => format!("{lead}: {why} \u{2014} now: aterm pkg update"),
                        None => format!("{lead} \u{2014} `aterm pkg update` says why"),
                    };
                    let title = "the signed index, reached lately";
                    f.push(
                        if stamps.last_outcome
                            == Some(aterm_update_core::pkg_check::PassOutcome::Offline)
                        {
                            Finding::waits(c::INDEX_REACHED, Class::Install, title, why)
                                .remedy(Remedy::Network)
                        } else {
                            Finding::change(
                                c::INDEX_REACHED,
                                Class::Install,
                                title,
                                why,
                                format!("reached within {INDEX_UNREACHED_WARN_DAYS} day(s)"),
                                Remedy::Command(String::from("aterm pkg update")),
                            )
                        },
                    );
                }
                _ => {}
            }
            if !status.index_build_changed_at.is_empty()
                && let Some(days) = index_age_days(&status.index_build_changed_at, now)
                && days > INDEX_FROZEN_WARN_DAYS
                && reached_days.is_some_and(|d| d <= INDEX_UNREACHED_WARN_DAYS)
            {
                f.push(Finding::change(
                    c::INDEX_FROZEN,
                    Class::Install,
                    "publishing upstream",
                    format!(
                        "index build {} has not changed in {days} day(s) while the index \
                             was reachable — publishing looks frozen upstream",
                        status.last_index_build
                    ),
                    "moving",
                    Remedy::None,
                ));
            }
        }
    } else {
        f.push(never_checked());
    }
    // The build floor is printed with the generation that recorded it, because that pair is
    // the gate: a floor stamped with an older generation is re-based by the next
    // master-signed one rather than obeyed (`sig::BuildFloor`).
    let build_floor = crate::sig::BuildFloor {
        index_build: crate::sig::Floor::new(layout.floor()).current(),
        roster_seq: crate::sig::Floor::new(layout.floor_generation()).current(),
    };
    // A floor of 0 is no floor (`Floor::current` reads a missing file as 0): no index, or no
    // generation, has been accepted, and the row says that rather than an acceptance of 0.
    let floor_title = "the accepted index";
    let named = match (build_floor.index_build, build_floor.roster_seq) {
        (0, _) => {
            f.push(Finding::note(
                c::INDEX_FLOOR,
                Class::Install,
                floor_title,
                "no index accepted yet",
            ));
            None
        }
        (build, 0) => {
            f.push(Finding::ok(
                c::INDEX_FLOOR,
                Class::Install,
                floor_title,
                format!("index {build} accepted"),
            ));
            None
        }
        (build, seq) => {
            f.push(Finding::ok(
                c::INDEX_FLOOR,
                Class::Install,
                floor_title,
                format!("index {build} accepted (roster generation {seq})"),
            ));
            Some(seq)
        }
    };
    // The second durable ratchet, moving independently of the first: which generation of the
    // machine roster this store has accepted. A roster floor stuck while machines have been
    // minted or revoked means this store has not seen a publish since. Said only when it is
    // not the generation the row above already names.
    let roster_title = "the accepted roster generation";
    match (
        crate::sig::Floor::new(layout.roster_floor()).current(),
        named,
    ) {
        (0, None) => {}
        (0, Some(_)) => f.push(Finding::note(
            c::ROSTER_FLOOR,
            Class::Install,
            roster_title,
            "no roster generation recorded",
        )),
        (roster, named) if Some(roster) != named => f.push(Finding::ok(
            c::ROSTER_FLOOR,
            Class::Install,
            roster_title,
            format!("roster generation {roster} accepted"),
        )),
        _ => {}
    }
    // Whether the accepted index is still the newest: the channel-head probe writes its last
    // answer beside the floor, and this reads it back, offline.
    if probes.index_head {
        let last_pass = LastPass {
            reached_index_at: crate::status::read(layout)
                .and_then(|s| crate::flow::rfc3339_to_unix(&s.last_success_at)),
            held: crate::index_probe::held_index_builds(layout),
        };
        let (row, remedy) =
            index_head_line(&crate::index_probe::cached_answer(layout), &last_pass, now);
        f.push(Finding::of_row(
            c::INDEX_HEAD,
            Class::Install,
            "the newest index",
            row,
            "the newest index, accepted",
            remedy,
        ));
    }
    // (8c) Who watches the vendor heads: an aterm window, or with no window open one
    // terminal session, or nobody, and then a new Claude Code or Codex release waits for the
    // next update pass. Said for a vendor program kept at its vendor's head on a machine
    // whose packages update on their own.
    if let Some(row) = head_watch_line(layout, probes) {
        f.push(Finding::of_row(
            c::VENDOR_WATCH,
            Class::Install,
            "who watches the vendor heads",
            row,
            "",
            Remedy::None,
        ));
    }

    // (9) Rustup and relocatability, two questions kept apart: is there a rustup at all
    // (PATH, then `$CARGO_HOME/bin`, then `~/.cargo/bin`, which an app-spawned pass may not
    // have on its PATH), and does it answer inside the probe ceiling? Under load a present
    // rustup can miss the `--version` deadline, and the seam audit needs no rustup binary.
    let rustup = rustup_binary(home);
    let rustup_answers = rustup.as_deref().is_some_and(rustup_present);
    if rustup.is_none() {
        // A note, not a warn: the managed Trust toolchain runs as `targo`/`trustc` from the
        // store, so a missing rustup is a fact about this machine, never something to fix.
        f.push(Finding::note(
            c::RUSTUP,
            Class::Person,
            "rustup",
            "no rustup on PATH, in $CARGO_HOME/bin or in ~/.cargo/bin; the managed toolchain \
             runs without it (`targo`, `trustc`)",
        ));
    } else if !rustup_answers {
        f.push(Finding::unread(
            c::RUSTUP,
            Class::Person,
            "rustup",
            format!(
                "rustup at {} did not answer `--version` within {} s (a wedged shim, or a \
                 toolchain fetch in flight); rustup `trust` is checked below without it",
                rustup
                    .as_deref()
                    .map_or_else(String::new, |r| r.display().to_string()),
                PROBE_TIMEOUT.as_secs()
            ),
        ));
    }
    // A link (5e) named has its row and its fix there: neither the re-point below nor
    // the unlinked-channel row — a link to nothing does not resolve either — says it a
    // second time with another fix.
    let local_said = probes.local_seal.is_some();
    let seam_title = "rustup's trust entry";
    if !local_said
        && let Some(rustup_home) =
            crate::seam::rustup_home_with(std::env::var_os("RUSTUP_HOME").as_deref(), home)
        && layout.program_current("trust").join("bin").is_dir()
        && let Some((row, remedy)) = seam_line(
            &crate::seam::status(layout, &rustup_home, crate::seam::DEFAULT_SEAM),
            layout,
        )
    {
        // The entry exists and is not ours: the link resolves, so the check below would call
        // it healthy while every `cargo +trust` ignores the managed build. Doctor names the
        // one command and re-points nothing: an entry under `~/.rustup` that aterm did not
        // lay is the user's (the seam module's rule): only the link into the store that
        // `repair` re-points is a pass's own.
        let class = if remedy == Remedy::Repair {
            Class::Relay
        } else {
            Class::Person
        };
        f.push(Finding::of_row(
            c::RUSTUP_SEAM,
            class,
            seam_title,
            row,
            "the managed seam",
            remedy,
        ));
    } else if !local_said
        && rustup_answers
        && layout.program_current("trust").join("bin").is_dir()
        && !rustup_trust_channel_resolves(rustup.as_deref().unwrap_or(Path::new("rustup")))
    {
        // The managed Trust toolchain is here but rustup does not know it, so `cargo +trust`
        // fails with `'rustc' is not installed for the custom toolchain 'trust'`. `repair`
        // lays the entry or rebuilds the view (a hand `rustup toolchain link` at the store
        // would not do: it ships only Trust's names); without `<rustup_home>/toolchains` it
        // lays nothing, so the fix creates that first.
        let (fix, remedy) =
            match crate::seam::rustup_home_with(std::env::var_os("RUSTUP_HOME").as_deref(), home) {
                Some(rh) if !crate::seam::toolchains_dir(&rh).is_dir() => {
                    let fix = format!(
                        "mkdir -p '{}' && aterm pkg repair",
                        crate::seam::toolchains_dir(&rh).display()
                    );
                    (fix.clone(), Remedy::Command(fix))
                }
                _ => ("aterm pkg repair".to_owned(), Remedy::Repair),
            };
        f.push(Finding::change(
            c::RUSTUP_SEAM,
            Class::Relay,
            seam_title,
            format!(
                "rustup has no working `trust` toolchain, so `cargo +trust` and projects \
                 pinning `channel = \"trust\"` fail here (`targo` and `trustc` still work) — \
                 fix: {fix}"
            ),
            "the managed seam",
            remedy,
        ));
    }
    // A stale link the machine replaced on its own ([`replaced_seam_line`]).
    if let Some(rustup_home) =
        crate::seam::rustup_home_with(std::env::var_os("RUSTUP_HOME").as_deref(), home)
        && let Some(line) = replaced_seam_line(layout, &rustup_home)
    {
        f.push(Finding::note(
            c::RUSTUP_REPLACED,
            Class::Relay,
            "a rustup trust link the machine replaced",
            line,
        ));
    }

    // (10) The question a user came here with: do I have the toolchain? Everything above
    // audits the store's structure and passes vacuously on a machine that received nothing;
    // health is never reported over an empty store.
    let installed = crate::ops::active_builds(layout);
    let status = crate::status::read(layout);
    // A recorded key beginning with `-` is never a program (no shim can be made under one and
    // the signed index cannot name one): such a row is a stray, and counting it would fail a
    // healthy toolset.
    let strays: Vec<&String> = status
        .as_ref()
        .map(|s| s.programs.keys().filter(|k| k.starts_with('-')).collect())
        .unwrap_or_default();
    for stray in &strays {
        f.push(Finding::change(
            c::of(c::RECORD_STRAY, stray),
            Class::Install,
            "a record row",
            format!(
                "the record holds a stray row for {stray:?}, which cannot be a program name \
                     (it is a command-line flag, left by a mistyped `atpkg install`). No program \
                     is missing because of it; the next successful `aterm pkg update` clears it"
            ),
            "cleared",
            update(),
        ));
    }
    // (10a) System-satisfied members. A member the signed index marks `system = "<bin>"`
    // that the pass found on PATH outside the prefix is not managed here (its row says
    // `system: <path> — not managed by aterm`) and is neither missing nor a fault. The
    // recorded path is re-checked: a binary since gone is worth a row, because the next
    // pass installs the member through its artifact.
    if let Some(s) = status.as_ref() {
        for (program, row) in &s.programs {
            let Some(path) = crate::state::system_path(&row.state) else {
                continue;
            };
            let id = c::of(c::SYSTEM, program);
            let title = format!("{program}, satisfied by the system");
            f.push(if std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
                Finding::ok(
                    id,
                    Class::Person,
                    title,
                    format!(
                        "{program}: {} (remove that copy to have atpkg manage {program})",
                        row.state
                    ),
                )
            } else {
                Finding::change(
                    id,
                    Class::Install,
                    title,
                    format!(
                        "{program}: recorded as `{}`, but that copy is gone — the next `aterm \
                         pkg update` reinstalls the managed copy",
                        row.state
                    ),
                    "the managed copy",
                    update(),
                )
            });
        }
    }
    // (10g) Members held on their current build ([`crate::state::held_unpublished`]): the
    // group's new pin is not published here. Deferred, never a fault; said as `ok`, quoting
    // the row, so the frozen tuple is visible beside its siblings' stale-looking rows.
    if let Some(s) = status.as_ref() {
        for (program, row) in &s.programs {
            // Only the group hold. A member held after a failed stage (`held: last attempt
            // failed with …`) is not installed and no pass moves it; it is listed with the
            // problems (`is_problem_state`).
            if row.state.starts_with(crate::state::HELD_PREFIX)
                && !row.state.starts_with(crate::state::HELD_FAILED_PREFIX)
            {
                f.push(Finding::ok(
                    c::of(c::HELD, program),
                    Class::Install,
                    format!("{program}, held on its build"),
                    format!(
                        "{program}: {} (nothing to do here; the next pass moves the group once \
                         the index publishes for this target)",
                        row.state
                    ),
                ));
            }
        }
    }
    // (10d) Shadowed managed members. For every tool a managed member exposes, a foreign
    // executable of that name earlier on PATH than the managed bin/ is what runs. Probed
    // live against this process's PATH, never trusted from the record: a warning, never a
    // fault, and never fixed here (the user owns PATH).
    for (program, build) in &installed {
        if crate::linkmode::is_linked(layout, program) {
            continue;
        }
        let id = c::of(c::SHADOW, program);
        let title = format!("what PATH runs for {program}");
        let managed_first = "the managed copy, first on PATH";
        // An agent program's reroute stub decides at exec time: with `reroute/<name>` first
        // on this PATH, what runs is the stub's choice, either the managed twin inside aterm
        // (no shadow however stale this shell's PATH is; said below as an `ok` naming the
        // copy it out-ranks) or a pass-through, whose walk is the one probed
        // (`cli::shadow_in_shell`).
        let shadow = crate::ops::active_tools(layout, program, *build)
            .into_iter()
            .find_map(|tool| {
                crate::cli::shadow_in_shell(layout, tool.as_str(), path_var, probes.stub_env)
                    .map(|path| (tool, path))
            });
        if shadow.is_none()
            && let Some(row) =
                crate::cli::agent_routed_row(layout, program, *build, path_var, probes.stub_env)
        {
            f.push(Finding::ok(
                id,
                Class::Person,
                title,
                format!("{program}: {row}"),
            ));
            continue;
        }
        if let Some((tool, path)) = shadow {
            // Outside aterm, a stub that passes through to the user's own copy is the
            // design: a note, never shadowed.
            if let Some(row) = crate::cli::agent_passed_row(
                layout,
                program,
                *build,
                &path,
                path_var,
                probes.stub_env,
            ) {
                f.push(Finding::note(
                    id,
                    Class::Person,
                    title,
                    format!("{program}: {row}"),
                ));
                continue;
            }
            // An agent program whose `agents/` twin is laid and current: every tab runs
            // aterm's copy, so this is this shell's state (it has not run the hook), and the
            // remedy works in place (`cli::shell_remedy_command`: source the hook, never
            // `exec $SHELL`, which drops an aterm tab's shell integration).
            if let Some(row) =
                crate::cli::agent_shell_shadow(layout, program, *build, &path, path_var)
            {
                f.push(Finding::change(
                    id,
                    Class::Person,
                    title,
                    format!("{program}: {row}"),
                    managed_first,
                    Remedy::None,
                ));
                continue;
            }
            // The canonical state, then, when `alab-<tool>` is laid, one trailing sentence
            // naming the way to the managed copy without touching PATH (`cli::alias_fix`).
            let fix = crate::cli::alias_fix(layout, &tool, program, path_var)
                .map(|f| format!(" — {f}"))
                .unwrap_or_default();
            let managed = match crate::vendor_direct::spec(program) {
                // An ALab build ([`crate::alab`]) is aterm's copy from ALab's channel.
                Some(_) if crate::alab::build_is_alab(layout, program, *build) => {
                    String::from("the ALab build aterm manages")
                }
                Some(s) => format!("the copy aterm updates from {}", s.vendor),
                None => String::from("the pinned build"),
            };
            f.push(Finding::change(
                id,
                Class::Person,
                title,
                format!(
                    "{program}: {} (not {managed}; atpkg never edits PATH — remove or reorder \
                     that copy if you want the managed one){fix}",
                    crate::state::shadowed(*build, &path)
                ),
                managed_first,
                Remedy::Person {
                    pane: path.display().to_string(),
                    how: String::from("remove or reorder that copy if you want the managed one"),
                },
            ));
        }
    }
    // (10f) Managed members whose shim exports an environment (`shim_env`, e.g. the vendor's
    // updater off): one trailing `ok` sentence after the canonical row, withheld when a
    // foreign copy shadows the shim. A shim exporting other than its build declares
    // (`activate::shim_env_drift`) is a `warn` even when shadowed here, since the shim still
    // reaches the `agents/` twin every tab runs and any shell whose PATH finds it.
    for (program, build) in &installed {
        if crate::linkmode::is_linked(layout, program) {
            continue;
        }
        let id = c::of(c::SHIM_ENV, program);
        let title = format!("{program}'s shim environment");
        let tools = crate::ops::active_tools(layout, program, *build);
        if let Some(drift) =
            crate::activate::shim_env_drift(layout, &layout.build_dir(program, *build), &tools)
            && let Some((shim, _)) = drift.shims.first()
        {
            let have = crate::platform::shim_env_of(shim);
            let line = env_drift_line(program, *build, shim, &have, &drift.declared);
            f.push(Finding::change(
                id,
                Class::Relay,
                title,
                format!("{program}: {line}"),
                "what its build declares",
                Remedy::Repair,
            ));
            continue;
        }
        let Some(fix) = tools
            .iter()
            .find_map(|t| crate::cli::shim_env_fix(&layout.shim(t), program))
        else {
            continue;
        };
        // The same probe (10d) takes, an agent program's reroute stub included.
        if tools.iter().any(|t| {
            crate::cli::shadow_in_shell(layout, t.as_str(), path_var, probes.stub_env).is_some()
        }) {
            continue;
        }
        let row = status.as_ref().and_then(|s| s.programs.get(program));
        // A vendor-direct program moves with its vendor — its row and the fix-line say so;
        // an index program's with the ALab index.
        let line = match crate::vendor_direct::spec(program) {
            Some(spec) => format!(
                "{} — {fix}",
                crate::cli::vendor_state_of(layout, spec, *build, row)
            ),
            None => {
                // Any managed row's own words — a pin, or `rolled back from <N>` (never a
                // minted pin the index does not hold).
                let state = row
                    .filter(|r| {
                        r.installed_build == Some(*build) && crate::state::is_managed(&r.state)
                    })
                    .map_or_else(
                        || crate::state::managed(*build, build_floor.index_build),
                        |r| r.state.clone(),
                    );
                format!("{state} — {fix} (updates arrive with the ALab index)")
            }
        };
        f.push(Finding::ok(
            id,
            Class::Relay,
            title,
            format!("{program}: {line}"),
        ));
    }
    // (10h) Claude Code's defaults (top-effort mode, `xhigh` effort, surveys off, the drafts
    // card quiet, autocompaction at 400k tokens). An aterm window adds each where unset
    // (`aterm_primer::add_claude_defaults`) and never touches one set by hand; this says which
    // is which, a row per [`DefaultState`], with each key as the settings file holds it. A
    // file the window refuses is one `warn` naming why.
    if installed.iter().any(|(program, _)| *program == "claude")
        && let Some(states) = &probes.claude_defaults
    {
        let title = "Claude Code's defaults";
        let states = match states {
            Ok(states) => states.as_slice(),
            Err(why) => {
                f.push(Finding::change(
                    c::CLAUDE_DEFAULTS,
                    Class::Person,
                    title,
                    format!(
                        "claude: ~/.claude/settings.json cannot take aterm's defaults ({why}) — \
                         an aterm window leaves the file exactly as it is and adds none of them"
                    ),
                    "aterm's defaults, where unset",
                    Remedy::None,
                ));
                &[]
            }
        };
        let defaults: Vec<&str> = states
            .iter()
            .filter(|(_, s)| *s == DefaultState::Default)
            .map(|(k, _)| *k)
            .collect();
        if !defaults.is_empty() {
            // What each key set to its default means, for the keys this row names only.
            let meaning: Vec<&str> = defaults
                .iter()
                .map(|key| match *key {
                    CLAUDE_TOP_EFFORT_KEY => "the top-effort mode on",
                    "effortLevel" => "xhigh effort",
                    "feedbackSurveyRate" => "the session-quality survey off",
                    CLAUDE_SURVEY_SWITCH => "every rating survey off",
                    CLAUDE_DRAFTS_KEY => "the drafted-feedback card quiet",
                    CLAUDE_COMPACT_WINDOW_KEY => "autocompact at 400k tokens",
                    key => key,
                })
                .collect();
            f.push(Finding::ok(
                c::CLAUDE_DEFAULTS,
                Class::Preference,
                title,
                format!(
                    "claude: {} set to aterm's {} ({}) in ~/.claude/settings.json",
                    defaults.join(", "),
                    if defaults.len() == 1 {
                        "default"
                    } else {
                        "defaults"
                    },
                    meaning.join(", ")
                ),
            ));
        }
        for (key, state) in states {
            let id = c::of(c::CLAUDE_DEFAULTS, key);
            let title = format!("Claude Code's {key}");
            match state {
                DefaultState::Default => {}
                DefaultState::Manual(value) => {
                    // A hand-set switch that Claude Code does not read as true leaves the
                    // surveys on: kept, and said, with what an aterm-supervised session
                    // then does about them.
                    let on = if *key == CLAUDE_SURVEY_SWITCH {
                        " — Claude Code reads only 1, true, yes or on as off, so it will still \
                         show its rating surveys (an aterm-supervised session dismisses each \
                         with a 0 unless [harness] dismiss_surveys is false)"
                    } else {
                        ""
                    };
                    f.push(Finding::ok(
                        id,
                        Class::Person,
                        title,
                        format!(
                            "claude: {key} is {value}, set by hand — kept (aterm adds its \
                             default only where a key is unset){on}"
                        ),
                    ));
                }
                DefaultState::HeldForEffort(effort) => f.push(Finding::ok(
                    id,
                    Class::Person,
                    title,
                    format!(
                        "claude: {key} is unset and stays so — an effort is set by hand \
                         ({effort}), and aterm adds {CLAUDE_TOP_EFFORT_KEY} and xhigh effort \
                         only where no other effort was chosen"
                    ),
                )),
                DefaultState::HeldForRate(rate) => f.push(Finding::ok(
                    id,
                    Class::Person,
                    title,
                    format!(
                        "claude: {key} is unset and stays so — feedbackSurveyRate is set by \
                         hand ({rate}), and aterm adds the switch that turns EVERY rating survey \
                         off only where that probability is unset or 0"
                    ),
                )),
                DefaultState::HeldForOffer(why) => f.push(Finding::ok(
                    id,
                    Class::Person,
                    title,
                    format!(
                        "claude: {key} is unset and stays so — {why}, and aterm adds its 400k \
                         window only where no Keep answer stands (/autocompact in Claude Code \
                         sets a window for a model)"
                    ),
                )),
                DefaultState::OverridesEffort(effort) => f.push(Finding::change(
                    id,
                    Class::Person,
                    title,
                    format!(
                        "claude: \"{key}\": true in ~/.claude/settings.json overrides the effort \
                         set by hand ({effort}) — Claude Code runs xhigh while it stands — fix: \
                         remove \"{key}\" from ~/.claude/settings.json to use that effort (a \
                         ~/.claude/settings.json.bak-<unix> may hold the file from before)"
                    ),
                    "unset",
                    Remedy::Person {
                        pane: String::from("~/.claude/settings.json"),
                        how: format!("remove \"{key}\" from ~/.claude/settings.json"),
                    },
                )),
                // `agents_auto_prime = false`: no window adds it (aterm-gui's sweep), so
                // the key is the person's to set, and nothing is wrong.
                DefaultState::Unset if !probes.claude_auto_prime => f.push(Finding::ok(
                    id,
                    Class::Person,
                    title,
                    format!(
                        "claude: {key} is unset and stays so — agents_auto_prime = false in \
                         aterm.toml, so aterm adds none of its defaults"
                    ),
                )),
                DefaultState::Unset => {
                    let want = CLAUDE_DEFAULTS
                        .iter()
                        .find(|(k, _)| k == key)
                        .map_or(String::new(), |(_, v)| v.json_text());
                    let line = default_line(key, &want);
                    f.push(Finding::change(
                        id,
                        Class::Preference,
                        title,
                        format!(
                            "claude: {key} is unset in ~/.claude/settings.json — an aterm \
                             window adds {line} at its next start"
                        ),
                        line,
                        Remedy::None,
                    ));
                }
            }
        }
    }
    // Every problem, not the first one.
    let recorded_problems = recorded_problems(status.as_ref());
    // (11a) AN UNSERVED PLATFORM IS A STATE, NOT A FAULT — off macOS, see
    // `toolset_unserved_here`. The `*toolset*` row's "no build for this architecture"
    // verdict leaves the problem list (it is said below, as a note) — that one verdict and
    // no other: an unreachable index or a refused publish is a fault on every platform.
    let toolset_unserved = toolset_unserved_here(status.as_ref());
    let recorded_problems: Vec<String> = if toolset_unserved.is_some() {
        recorded_problems
            .into_iter()
            .filter(|why| !is_unserved_toolset_problem(why))
            .collect()
    } else {
        recorded_problems
    };
    let declined = layout.declined().is_file();
    // (11) Completeness against the signed index. Everything above audits what the record
    // mentions; a program the machine wants and never attempted has no shim, row or fault.
    // Only the index knows what should be here. Offline and best-effort (`cached_index`):
    // an unreachable index never invents a missing program.
    let (dev_linked, link_problems) = live_dev_links(layout);
    for why in link_problems {
        f.push(
            Finding::change(
                c::DEV_LINK,
                Class::Person,
                "a dev link",
                why,
                "a dev link that runs",
                Remedy::None,
            )
            .essential(),
        );
    }
    let active_count = installed
        .keys()
        .chain(dev_linked.iter())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let (missing_unexplained, missing_on_purpose, _indexed_dev_linked) =
        missing_against_index(layout, &installed, &dev_linked);
    // (11b) An unserved member is explained by its own row, not missing without a reason
    // (`drop_unserved`; identity on macOS).
    let missing_unexplained = drop_unserved(missing_unexplained, toolset_unserved.as_deref());
    if let Some(unserved) = &toolset_unserved {
        // Said once, as what it is: a fact about the index and this target, which no
        // install on this machine changes — the state, and no act. The members it names
        // are the ones whose own rows say so; the served members (the agents) are
        // reported like any other — their per-program rows print ABOVE this note (the
        // program loop runs first), so the note does not point "below". On Linux the one
        // local route is a dev link of a locally built program (`aterm pkg link`,
        // docs/LINUX-LOCAL-TOOLCHAIN.md), so the note names it instead of "nothing".
        // A vendor-direct agent's `unavailable on` row is about that vendor's own
        // publishing, not the ALab toolset: it is explained the same way, but named apart.
        let (vendors, alab): (Vec<&String>, Vec<&String>) = unserved
            .iter()
            .partition(|name| crate::vendor_direct::is_vendor(name));
        let named = |names: &[&String]| {
            names
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        f.push(Finding::note(
            c::TOOLSET_PLATFORM,
            Class::Install,
            "the ALab toolset on this platform",
            format!(
                "the ALab toolset is unavailable on this platform: no build is published for \
                 this target{}{}; {}",
                if alab.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", named(&alab))
                },
                if vendors.is_empty() {
                    String::new()
                } else {
                    format!(", and {} publishes none for it either", named(&vendors))
                },
                if cfg!(target_os = "linux") {
                    "a local build links in with `aterm pkg link <program> <checkout>` \
                     (docs/LINUX-LOCAL-TOOLCHAIN.md)"
                } else {
                    "nothing on this machine changes that"
                }
            ),
        ));
    }
    // The toolset row, essential: the one item whose door is the next act of an empty store.
    let toolset = "the ALab toolset";
    let want = "the ALab default set, installed";
    // The whole-set install cannot serve an unserved platform (the note above); there the
    // door is the explicit one of the served member the verdict is about — unless the
    // verdict is toolset-wide (an unreachable index), which the ordinary door answers
    // exactly as it does elsewhere.
    let unserved_door = if active_count == 0 && toolset_unserved.is_some() {
        unserved_platform_next(&recorded_problems, &missing_unexplained)
    } else {
        None
    };
    let door = toolset_door(
        &ToolsetFacts {
            unserved_door,
            empty_store: active_count == 0,
            recorded_problems: &recorded_problems,
            declined,
            missing: !missing_unexplained.is_empty(),
            removed_on_purpose: !missing_on_purpose.is_empty(),
        },
        |program| in_coherence_group(layout, program),
    );
    let problem =
        |why: String| Finding::change(c::TOOLSET, Class::Install, toolset, why, want, door.clone());
    let row = if declined {
        // Intended emptiness. Say so, so it does not read as a fault.
        Finding::note(
            c::TOOLSET,
            Class::Install,
            toolset,
            format!("{DECLINED_LINE} (`aterm pkg install --default-set` reinstalls it)"),
        )
        .remedy(Remedy::Command(String::from(
            "aterm pkg install --default-set",
        )))
    } else if active_count == 0
        && unserved_store_is_quiet(
            toolset_unserved.is_some(),
            &recorded_problems,
            &missing_unexplained,
        )
    {
        // Nothing active because nothing published for this target is missing: the note
        // above is the whole story, not a problem to count or exit 1 for. A served member
        // that failed, is held, or is missing without a row takes the branch below.
        Finding::ok(
            c::TOOLSET,
            Class::Install,
            toolset,
            "0 program(s) active — none is published for this platform, so none is missing",
        )
    } else if active_count == 0 {
        match recorded_problems.first() {
            Some(why) => problem(format!("no ALab programs are installed ({why})")),
            // An environmental failure (an unreachable index) leaves no per-program row, so
            // the aggregate outcome is the only place the reason lives; preferring it to the
            // generic hint keeps "why did nothing arrive?" answerable offline.
            None => match status
                .as_ref()
                .map(|s| s.outcome.as_str())
                .filter(|o| !o.is_empty())
            {
                Some(outcome) => problem(format!(
                    "no ALab programs are installed (last attempt: {outcome})"
                )),
                // The fix is the row's door, the next act (`aterm pkg install
                // --default-set`), said there and not twice.
                None => problem(String::from("no ALab programs are installed")),
            },
        }
    } else if !recorded_problems.is_empty() {
        // Something is installed, but the record carries live failures: a partial first run,
        // a blocked disk, a member with no build for this triple, an aborted coherence-group
        // transaction. No count here: the verdict's "found N problem(s)" counts the rows
        // that fail, and two Ns would contradict.
        problem(format!(
            "the toolset is incomplete; {active_count} program(s) active"
        ))
    } else if !missing_unexplained.is_empty() {
        // The machine wants these, does not have them, and nothing recorded a reason; the
        // branch above reports failures the record names.
        problem(format!(
            "the toolset is incomplete: {} of {} program(s) the signed index serves are not \
             installed ({})",
            missing_unexplained.len(),
            active_count + missing_unexplained.len() + missing_on_purpose.len(),
            missing_unexplained.join(", ")
        ))
    } else {
        // Not "ALab program(s)": the count includes the vendor-direct agents.
        Finding::ok(
            c::TOOLSET,
            Class::Install,
            toolset,
            format!("{active_count} program(s) active"),
        )
        .detail(active_count.to_string())
    };
    // Essential in every state, as an item is: a reader of `--json` sees the row that decides
    // the exit whether or not it fails it now (a declined store's note and an `ok` never do).
    f.push(row.essential());
    // A dev link and a deliberate removal are not faults, but they must be visible: the
    // removal ledger is a file no user reads, and its effect (no stub, no unattended
    // reinstall, a coherence group that never activates) looks like the program never
    // existed. Say each plainly, and name the way back.
    for program in &dev_linked {
        let from = crate::linkmode::linked_checkout(layout, program)
            .map(|c| c.display().to_string())
            .unwrap_or_else(|| "a checkout".to_string());
        f.push(Finding::ok(
            c::of(c::DEV_LINK, program),
            Class::Person,
            format!("{program}'s dev link"),
            format!(
                "{program}: dev-linked from {from} (not the index build; `aterm pkg unlink \
                 {program}` returns it to the index)"
            ),
        ));
    }
    for program in &missing_on_purpose {
        f.push(Finding::ok(
            c::of(c::REMOVED, program),
            Class::Person,
            format!("{program}, removed on purpose"),
            format!(
                "{program}: removed on purpose — no unattended pass reinstalls it (aterm pkg \
                 install {program} brings it back)"
            ),
        ));
    }
    if let Some(start) = problem_listing_start(declined, active_count == 0, recorded_problems.len())
    {
        for why in recorded_problems.iter().skip(start) {
            // `<program>: <state>`, as `recorded_problems` writes each.
            let program = why.split(": ").next().unwrap_or(why);
            f.push(Finding::change(
                c::of(c::TOOLSET_MEMBER, program),
                Class::Install,
                format!("{program}, a recorded problem"),
                why.clone(),
                "installed",
                Remedy::None,
            ));
        }
    }

    // (11) The C toolchain, a prerequisite probed and never shipped (`crate::prereq`), only
    // where a toolset is installed (that is what it stops from building). An answer (no
    // toolchain, or one that refuses) fails the report: "healthy" over a machine whose first
    // `targo build` dies in a build script is the vacuous pass (10) guards against. An
    // unknown is said; a target it does not apply to, a note.
    if active_count > 0
        && let Some(verdict) = probes.cc.as_ref()
    {
        let title = "the C toolchain";
        let line = verdict.line();
        f.push(match verdict {
            crate::prereq::CcVerdict::Ready { .. } => {
                Finding::ok(c::CLT, Class::Elevate, title, line).essential()
            }
            crate::prereq::CcVerdict::NotProbed => {
                Finding::note(c::CLT, Class::Elevate, title, line).essential()
            }
            crate::prereq::CcVerdict::Unanswered { .. }
            | crate::prereq::CcVerdict::Unproven { .. } => {
                Finding::unread(c::CLT, Class::Elevate, title, line)
            }
            // A compiler that refuses with no one act to name (macOS: it is installed, and
            // its own stderr is the cause) has its remedy inline: no door.
            _ => Finding::change(
                c::CLT,
                Class::Elevate,
                title,
                line,
                "a C toolchain that builds",
                verdict
                    .fix()
                    .map_or(Remedy::None, |fix| Remedy::Command(fix.to_owned())),
            )
            .essential(),
        });
    }
    f
}

/// What the toolset row's door is chosen from ([`toolset_door`]): facts the report has read.
#[derive(Debug, Default)]
struct ToolsetFacts<'a> {
    /// The served member's door on an unserved platform ([`unserved_platform_next`]).
    unserved_door: Option<String>,
    /// No managed program is active.
    empty_store: bool,
    /// `"<program>: <state>"` for each recorded problem.
    recorded_problems: &'a [String],
    /// The owner declined the toolset.
    declined: bool,
    /// Members the signed index serves are missing with no recorded reason.
    missing: bool,
    /// A member was removed on purpose (the removed ledger).
    removed_on_purpose: bool,
}

/// The toolset row's one door, the report's next act when the toolset fails it
/// ([`next_act`]): an unserved platform's member door; for an empty store the whole-set
/// install, or a held member's door (the whole-set pass honours the hold and repeats it,
/// `skip_held_member`); a held member's door, or `--retry` beside other problems (a plain
/// `update` repeats a hold); `update`, which rewrites every recorded fault row; for members
/// missing with no recorded reason the whole-set install, the explicit door whatever
/// `[packages]` consent says (design §3.2), and no pass waits on the network for them.
/// [`Remedy::None`] when no one door fits: beside a member removed on purpose, which the
/// whole-set install would bring back (its own row names `install <program>`). `grouped`
/// says whether a program is in a coherence group, which decides a held member's door
/// ([`held_door`]).
fn toolset_door(f: &ToolsetFacts<'_>, grouped: impl Fn(&str) -> bool) -> Remedy {
    toolset_command(f, grouped).map_or(Remedy::None, Remedy::Command)
}

/// [`toolset_door`]'s command, `None` where it names none.
fn toolset_command(f: &ToolsetFacts<'_>, grouped: impl Fn(&str) -> bool) -> Option<String> {
    let door = |program: &str| held_door(program, grouped(program));
    if f.unserved_door.is_some() {
        f.unserved_door.clone()
    } else if f.empty_store {
        Some(
            first_held_member(f.recorded_problems)
                .map_or_else(|| String::from("aterm pkg install --default-set"), door),
        )
    } else if f.declined {
        None
    } else if f.recorded_problems.iter().any(|why| is_held_problem(why)) {
        Some(
            match (
                f.recorded_problems.len(),
                first_held_member(f.recorded_problems),
            ) {
                (1, Some(program)) => door(program),
                _ => String::from("aterm pkg update --retry"),
            },
        )
    } else if !f.recorded_problems.is_empty() {
        Some(String::from("aterm pkg update"))
    } else if f.missing && !f.removed_on_purpose {
        Some(String::from("aterm pkg install --default-set"))
    } else {
        None
    }
}

/// The bare version token from `<bin> --version`: the second whitespace token
/// (`ay 0.13.0+build.8174.…` → `0.13.0`) without its `+…` suffix, since the question is
/// "same release?", not "same commit?". Best-effort: a binary that will not run or answers
/// in another shape reports `unknown` and the caller stays quiet; doctor never fails
/// because a probe failed.
fn probe_version(bin: &Path) -> String {
    const UNKNOWN: &str = "unknown";
    let Some(out) = output_bounded(aterm_uds::spawnfd::command(bin).arg("--version")) else {
        return UNKNOWN.to_string();
    };
    if !out.status.success() {
        return UNKNOWN.to_string();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(line) = text.lines().next() else {
        return UNKNOWN.to_string();
    };
    let Some(token) = line.split_whitespace().nth(1) else {
        return UNKNOWN.to_string();
    };
    token.split('+').next().unwrap_or(token).to_string()
}

/// What the bundle has to ship for tippy to run, as (5f) words it: tippy refuses a symlink
/// for the compiler.
const TRUST_COMPILER_FIX: &str = "ship trustc as one signed plain file, never a symbolic link";

/// The fix every (5f)(c) warn names but the blocked one: `repair` reconciles the exec roots
/// at `Deep` depth (`cli::repair_store` → `compat::reconcile`), which lays or rebuilds the
/// root and re-renders every trust shim through it, and exits 1 when it cannot. Behind a
/// link or a file at `compat` or `compat/trust` it cannot, on any run, so that warn names
/// [`crate::compat::Blocked::fix`] instead.
const EXEC_ROOT_FIX: &str = "`aterm pkg repair` lays the exec root (a copy-on-write clone — the \
                             store is never modified) and routes the shims";

/// What a doctor warn about a managed tool claims, which decides the words the closing
/// "not healthy" line counts it under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolClaim {
    /// Nothing withheld: the (5f)(c) note.
    Runs,
    /// The warn says the tool cannot run.
    CannotRun,
    /// The warn says the tool may not run, or that doctor cannot tell.
    Unproven,
}

/// The counted clause of the closing "not healthy" line: `cannot` warns that name a
/// managed tool that cannot run and `unproven` warns that name one that may not run or
/// cannot be checked, each under its own words.
fn withheld_summary(cannot: usize, unproven: usize) -> String {
    const UNPROVEN: &str = "may not run or cannot be checked";
    match (cannot, unproven) {
        (c, 0) => format!("{c} managed tool(s) cannot run"),
        (0, u) => format!("{u} managed tool(s) {UNPROVEN}"),
        (c, u) => format!("{c} managed tool(s) cannot run and {u} {UNPROVEN}"),
    }
}

/// (10f)'s drift warn, after `<program>: `: what `shim` exports (`have`) against what its
/// build declares; the consequence only when the build declares a self-update switch
/// ([`crate::shim_env::turns_updater_off`]) and the shim exports none, the one shape where
/// "on here" is certain (the mirror of [`crate::shim_env::ShimEnv::fix_line`]); and the one
/// remedy, `repair`, which re-lays each shim with its build's sidecar.
fn env_drift_line(
    program: &str,
    build: u64,
    shim: &Path,
    have: &crate::shim_env::ShimEnv,
    declared: &crate::shim_env::ShimEnv,
) -> String {
    let exports = if have.is_empty() {
        String::from("nothing")
    } else {
        have.spelled()
    };
    let switches = |env: &crate::shim_env::ShimEnv| {
        env.entries()
            .iter()
            .any(|(name, value)| crate::shim_env::turns_updater_off(name, value))
    };
    let switch_lost = switches(declared) && !switches(have);
    let consequence = match (switch_lost, crate::vendor_direct::spec(program)) {
        (false, _) => String::new(),
        (true, Some(spec)) => format!(", so {}'s own updater is on here", spec.product),
        (true, None) => String::from(", so its own updater is on here"),
    };
    format!(
        "{} exports {exports}, but {} declares {}{consequence} — fix: `aterm pkg repair` \
         re-lays it",
        shim.display(),
        crate::vendor_direct::display_build(program, build),
        declared.spelled()
    )
}

/// (4b)'s sentence for `program`, whose build `r.build` was refused a shim for each of
/// `r.names`: each name quoted and escaped (a refused name may be no tool name at all, a
/// control byte included), the uncounted rest said as a number.
fn refused_line(program: &str, r: &crate::status::RefusedShims) -> String {
    let mut names: Vec<String> = r
        .names
        .iter()
        .take(crate::status::MAX_REFUSED_RECORDED)
        .map(|name| format!("`{}`", name.escape_debug()))
        .collect();
    let shown = u64::try_from(names.len()).unwrap_or(u64::MAX);
    if r.count > shown {
        names.push(format!("{} more", r.count - shown));
    }
    let one = r.count <= 1;
    format!(
        "{} exposes {}, {} no shim may carry (a shell, another core command, or no tool name) \
         \u{2014} {} no shim here, so typed in a shell {} never {} {program}'s copy; its \
         signed manifest names {}, and nothing on this machine changes that",
        crate::vendor_direct::display_build(program, r.build),
        names.join(", "),
        if one { "a name" } else { "names" },
        if one { "it has" } else { "they have" },
        if one { "it" } else { "they" },
        if one { "runs" } else { "run" },
        if one { "it" } else { "them" },
    )
}

/// [`read`]'s rows for the toolchain flip held for quiet and the live build leases: each a
/// wait, or a warn when the leases cannot be read (then gc keeps every build it would
/// reclaim, and the held flip waits, until they can).
fn hold_findings(layout: &Layout, now: i64) -> Vec<Finding> {
    let mut rows = Vec::new();
    let held = "the held toolchain update";
    // Only a record that still describes a wait is said as one; one that outlived every
    // build it moves off is said as that, and left for a door that writes to end
    // ([`crate::quiet::held`] — doctor changes nothing).
    match crate::quiet::held(layout) {
        crate::quiet::Held::Nothing => {}
        crate::quiet::Held::Waits(deferral) => rows.push(Finding::waits(
            c::HELD_UPDATE,
            Class::Install,
            held,
            deferral.sentence(now, &crate::vendor_direct::watch::watcher(layout)),
        )),
        crate::quiet::Held::Outlived(record) => rows.push(
            Finding::note(
                c::HELD_UPDATE,
                Class::Relay,
                held,
                format!(
                    "the record of a held Trust toolchain update ({}) outlived every build it \
                     moves off, so nothing waits for it; `aterm pkg repair` ends it, as the next \
                     update pass does",
                    record.moves(),
                ),
            )
            .remedy(Remedy::Repair),
        ),
    }
    let leases = "a build lease";
    match crate::lease::live(&layout.prefix) {
        Ok(live) => {
            rows.extend(
                live.iter()
                    .filter_map(|(subject, holders)| lease_line(subject, holders))
                    .map(|line| Finding::waits(c::LEASES, Class::Person, leases, line)),
            );
        }
        Err(e) => rows.push(Finding::unread(
            c::LEASES,
            Class::Person,
            leases,
            format!(
                "the build leases under {} cannot be read ({e}): gc keeps every build it would \
                 reclaim, and a held toolchain update waits, until they can",
                crate::lease::dir(&layout.prefix).display()
            ),
        )),
    }
    rows
}

/// The words for one leased subject, or `None` for a free one. gc keeps a leased build and
/// the seam leaves a leased view as it stands, so the row says it stays as it is, in the
/// words the held-flip row uses ([`crate::lease::Subject::describe`],
/// [`crate::lease::Holders::clause`]), until the holder's run ends or, with no holder
/// known, until its lease can be read.
fn lease_line(subject: &crate::lease::Subject, holders: &crate::lease::Holders) -> Option<String> {
    let until = match holders {
        crate::lease::Holders::Held(_) => "that run ends",
        crate::lease::Holders::Unknown(_) => "its lease can be read",
        crate::lease::Holders::Free => return None,
    };
    Some(format!(
        "{} is {}; it stays as it is until {until}",
        subject.describe(),
        holders.clause()
    ))
}

/// The (5f)(c) row for an active trust build that [`crate::compat::needs_root`], what it
/// claims ([`ToolClaim`]), and its remedy. An absent root, or unrouted shims, run the store
/// path, where tippy cannot run; through a root that differs from the build it may not.
/// Behind a blocked `compat` ([`crate::compat::Blocked`], which `repair` also refuses) it
/// cannot run with no shim routed and may not with one routed earlier. A matching root
/// with every shim routed is the note.
fn exec_root_line(build: u64, i: &crate::compat::Inspection) -> (Row, ToolClaim, Remedy) {
    use crate::compat::RootState;
    let root = i.root.display();
    let copy = format!(
        "bin/rustc (inode {}) is a separate file from bin/trustc (inode {}; {} byte(s) differ)",
        i.rustc_ino, i.trustc_ino, i.differing
    );
    let n = i.shims.len();
    let warn = |text: String| Row {
        level: Level::Warn,
        text,
    };
    match (&i.state, i.unrouted.is_empty()) {
        (RootState::Blocked(blocked), _) => (
            warn(format!(
                "trust build {build}: PATH tippy {} run — {copy}, which this build's tippy \
                 refuses, and {}; fix: {}",
                if i.unrouted.len() == n {
                    "cannot"
                } else {
                    "may not"
                },
                blocked.what(),
                blocked.fix()
            )),
            if i.unrouted.len() == n {
                ToolClaim::CannotRun
            } else {
                ToolClaim::Unproven
            },
            Remedy::Person {
                pane: i.root.display().to_string(),
                how: blocked.fix().to_owned(),
            },
        ),
        (RootState::Matches, true) => (
            Row {
                level: Level::Note,
                text: format!(
                    "trust build {build}: {copy}, which its tippy refuses; its {n} trust shim(s) \
                     run each tool from {root}, where rustc holds trustc's bytes (a \
                     copy-on-write clone; the store is untouched) — removed with the build"
                ),
            },
            ToolClaim::Runs,
            Remedy::None,
        ),
        (RootState::Absent, _) => (
            warn(format!(
                "trust build {build}: PATH tippy cannot run — {copy}, which this build's tippy \
                 refuses, and no exec root stands at {root}; fix: {EXEC_ROOT_FIX}"
            )),
            ToolClaim::CannotRun,
            Remedy::Repair,
        ),
        (RootState::Differs(at), _) => (
            warn(format!(
                "trust build {build}: PATH tippy may not run — {copy}, which this build's tippy \
                 refuses, and the exec root {root} is not the store build {} file for file \
                 (first difference: {}); fix: {EXEC_ROOT_FIX}",
                i.build_dir.display(),
                at.display()
            )),
            ToolClaim::Unproven,
            Remedy::Repair,
        ),
        (RootState::Matches, false) => {
            let example = i
                .unrouted
                .iter()
                .find(|name| name.as_str() == "tippy")
                .unwrap_or(&i.unrouted[0]);
            (
                warn(format!(
                    "trust build {build}: PATH tippy cannot run through {} of {n} trust shim(s) \
                     (e.g. {example}) — {copy}, which this build's tippy refuses, and those \
                     shims do not route through the exec root {root}; fix: {EXEC_ROOT_FIX}",
                    i.unrouted.len()
                )),
                ToolClaim::CannotRun,
                Remedy::Repair,
            )
        }
    }
}

/// The (5f)(f) words for an active trust build whose need for an exec root could not be
/// read ([`crate::compat::inspect`]'s `Err`, which names the path and the error): a row that
/// withholds "healthy". No re-lay makes a store file readable, so `repair` is not offered;
/// the row points at `aterm pkg verify trust`, which checks every file of the build against
/// its signed tree.
fn exec_root_unread_line(build: u64, why: &str) -> String {
    format!(
        "trust build {build}: cannot tell whether PATH tippy can run — {why}; any exec root \
         standing for the build is left as it is; fix: make that store file readable again \
         (`aterm pkg verify trust` checks the build against its signed tree)"
    )
}

/// The (5f)(d) words for an entry under `<prefix>/compat/trust` that no build needs — an
/// advisory warn: the next `gc` removes it ([`crate::compat::sweep`] takes exactly what
/// [`crate::compat::strays`] classifies), and no tool fails over it.
fn stray_root_line(path: &Path, why: crate::compat::Stray) -> String {
    use crate::compat::Stray;
    let at = path.display();
    let what = match why {
        Stray::BuildGone(n) => format!(
            "exec root {at} outlives trust build {n}, which is no longer in the store, and its \
             clones keep that build's reclaimed blocks allocated"
        ),
        Stray::NeedsNone(n) => format!(
            "exec root {at} stands for trust build {n}, which needs none (its bin/rustc is not \
             a separate file from bin/trustc that its tippy refuses)"
        ),
        Stray::NotADirectory => format!(
            "{at} is not a directory, and atpkg lays only exec root directories under that name"
        ),
    };
    format!("{what}; fix: `aterm pkg gc` removes it")
}

/// Whether the view file `at` presents the store's Trust tool `store`: a clone of it
/// ([`crate::clone::is_clone_of`]: same length, mode and time on a different inode, so a
/// hard link does not count) holding its bytes, since the attributes alone cannot tell it
/// from a bundle's separately signed copy. Writes nothing.
fn presents(store: &Path, at: &Path) -> bool {
    crate::clone::is_clone_of(store, at) && matches!(crate::clone::same_bytes(store, at), Ok(true))
}

/// Render a divergence's contested builds for the report line — a vendor-direct build by
/// its version, never its store id.
fn build_list(builds: &[u64]) -> String {
    builds
        .iter()
        .map(|b| crate::vendor_direct::build_label(*b))
        .collect::<Vec<_>>()
        .join(", ")
}

/// How recent a "nothing newer" answer must be for (8) to say `ok`: the window re-asks the
/// next two tags every [`crate::index_probe::INTERVAL`], so an older answer means nothing is
/// asking (no window running), and its age is the honest thing to print.
const INDEX_HEAD_FRESH_SECS: i64 = 15 * 60;

/// What the last signed update pass left on disk that bears on the channel head: when a
/// pass last reached the signed index (`status.toml`'s `last_success_at`, stamped only by a
/// pass that verified an index the channel served, never by one on the §14 cache), and the
/// index builds of the signed candidates it downloaded
/// ([`crate::index_probe::held_index_builds`]). `None` in either field when that record is
/// absent or unreadable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LastPass {
    reached_index_at: Option<i64>,
    held: Option<Vec<u64>>,
}

/// (8)'s channel-head line: the probe's cached answer for the current floor
/// ([`crate::index_probe::cached_answer`]) beside what the last signed update pass left
/// ([`LastPass`]). A fact about the channel, so it never withholds "healthy".
///
/// "run: aterm pkg update" is printed only where it can work: not when a pass already
/// downloaded the newer index and refused it, or found no complete newer release on the
/// channel (update would say "already current" and doctor "not the newest" forever). The
/// last pass's downloaded candidates (the §14 cache) tell those apart offline. `update`
/// does not write the probe's stamps, so no note promises it refreshes them.
fn index_head_line(
    cached: &crate::index_probe::CachedProbe,
    last: &LastPass,
    now: i64,
) -> (Row, Remedy) {
    use crate::index_probe::RangeAnswer;
    let floor = cached.floor;
    let age = |written: std::time::SystemTime| -> i64 {
        let at = written
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        now.saturating_sub(at).max(0)
    };
    let pass = match last.reached_index_at {
        Some(at) => format!(
            "the last signed update pass reached the index {} ago",
            ago(now.saturating_sub(at).max(0))
        ),
        None => "no update pass has recorded reaching the signed index".to_string(),
    };
    // A newer index already downloaded and not landed. A pass that reaches the channel
    // caches what it served whether or not it selected it (`flow::resolve_verified_index`),
    // so this is independent of the probe: the pass held it, and the floor did not move.
    if let Some(held) = last
        .held
        .as_ref()
        .and_then(|builds| builds.iter().copied().filter(|b| *b > floor).max())
    {
        return (
            Row {
                level: Level::Warn,
                text: format!(
                    "index {floor} is not the newest: index {held} was downloaded but not \
                     accepted — its signatures did not verify here, or its pass stopped early; \
                     if `aterm pkg update` still ends on index {floor}, the release needs \
                     fixing, not this machine"
                ),
            },
            Remedy::None,
        );
    }
    let note = |text: String| {
        (
            Row {
                level: Level::Note,
                text,
            },
            Remedy::None,
        )
    };
    let update = || Remedy::Command(String::from("aterm pkg update"));
    match cached.near {
        Some((RangeAnswer::Published, seen)) => {
            let text = if last.reached_index_at.is_some() && last.held.is_some() {
                // The last pass reached the channel and held nothing above the floor: then
                // it had no complete signed release newer than it.
                format!(
                    "index {floor} is not the newest: the index probe found a newer ALab index \
                     (seen {} ago; an unverified hint), but {pass} and found no complete \
                     signed release newer than {floor} there — if the new one was up by then, \
                     its signed files are not all uploaded and no update can land it yet; \
                     otherwise run: aterm pkg update",
                    ago(age(seen))
                )
            } else {
                format!(
                    "index {floor} is not the newest: the index probe found a newer ALab index \
                     (seen {} ago; an unverified hint — no signed pass has landed it on this \
                     machine) — run: aterm pkg update",
                    ago(age(seen))
                )
            };
            (
                Row {
                    level: Level::Warn,
                    text,
                },
                update(),
            )
        }
        Some((RangeAnswer::Missing, checked)) => {
            // Said as what was checked, and when: the probe's own rule is that absence is
            // never authority. Only a running window re-asks, so a stale answer says so.
            let checked = age(checked);
            if checked <= INDEX_HEAD_FRESH_SECS {
                (
                    Row {
                        level: Level::Ok,
                        text: format!(
                            "no index newer than {floor} is published (checked {} ago)",
                            ago(checked)
                        ),
                    },
                    Remedy::None,
                )
            } else {
                note(format!(
                    "no index newer than {floor} was published when last checked, {} ago (an \
                     aterm window re-checks while open); {pass}",
                    ago(checked)
                ))
            }
        }
        Some((RangeAnswer::Failed, when)) => note(format!(
            "the last check for an index newer than {floor} got no usable answer ({} ago), so \
             whether one is published is unknown here; {pass}",
            ago(age(when))
        )),
        None => note(format!(
            "no check for an index newer than {floor} is recorded on this machine (an aterm \
             window checks while open); {pass}"
        )),
    }
}

/// A non-negative age in seconds, said in the unit a person reads it in.
fn ago(secs: i64) -> String {
    if secs < 90 {
        format!("{secs} s")
    } else if secs < 90 * 60 {
        format!("{} min", secs / 60)
    } else if secs < 48 * 3600 {
        format!("{} h", secs / 3600)
    } else {
        format!("{} day(s)", secs / 86_400)
    }
}

/// How long the signed index may go unreached before `doctor` says so: past this, every
/// pass has been a cached one — three days of cache is three days a newer pin or yank could
/// have been missed.
const INDEX_UNREACHED_WARN_DAYS: i64 = 3;
/// How long an unchanged `index_build` — with the index reachable — reads as a frozen
/// publisher rather than a quiet week: the ALab pins move on releases, the vendor rows
/// on every Claude Code and Codex release, and thirty days without either is neither.
const INDEX_FROZEN_WARN_DAYS: i64 = 30;

/// The tail of the never-checked lines (`; claude and codex update from their vendors
/// without it`), naming only [`vendor_followers`]. Empty when there are none, and the line
/// then claims nothing of them.
fn vendor_update_clause(layout: &Layout, probes: &Probes) -> String {
    let names = vendor_followers(layout, probes);
    match names.as_slice() {
        [] => String::new(),
        [one] => format!("; {one} updates from its vendor without it"),
        many => format!(
            "; {} update from their vendors without it",
            spoken_list(many, "and")
        ),
    }
}

/// `names` as a sentence lists them: `a`, `a and b`, `a, b and c` (`conj` the last joint).
fn spoken_list(names: &[&str], conj: &str) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} {conj} {last}", rest.join(", ")),
    }
}

/// The vendor programs this machine keeps at their vendors' heads without an index pass
/// ([`crate::vendor_direct::watch::follows_vendor`]: installed, not removed, held or
/// dev-linked), not in `[packages] exclude`, on a machine whose packages update on their
/// own ([`Probes::automatic`]) — none otherwise.
fn vendor_followers(layout: &Layout, probes: &Probes) -> Vec<&'static str> {
    if !probes.automatic {
        return Vec::new();
    }
    crate::vendor_direct::VENDORS
        .iter()
        .filter(|spec| !probes.excluded.iter().any(|p| p == spec.program))
        .filter(|spec| crate::vendor_direct::watch::follows_vendor(layout, spec))
        .map(|spec| spec.program)
        .collect()
}

/// (8c) Which host watches the vendor heads right now, read off the store's rendezvous
/// without taking it ([`crate::vendor_direct::watch::watcher`]) — for the vendor programs
/// that update without an index pass ([`vendor_followers`]); nothing when there are none.
/// Nobody watching is a `note`, not a fault: the next update pass still finds the release,
/// hours later rather than minutes.
fn head_watch_line(layout: &Layout, probes: &Probes) -> Option<Row> {
    use crate::vendor_direct::watch::Watcher;
    let names = vendor_followers(layout, probes);
    if names.is_empty() {
        return None;
    }
    let names = spoken_list(&names, "or");
    let (level, text) = match crate::vendor_direct::watch::watcher(layout) {
        Watcher::Window => (
            Level::Ok,
            format!(
                "an aterm window watches the vendor heads: a new {names} release is found \
                 within minutes"
            ),
        ),
        Watcher::Session(pid) => (
            Level::Ok,
            format!(
                "a terminal session{} watches the vendor heads, no aterm window being open: a \
                 new {names} release is found within minutes",
                pid.map(|pid| format!(" (pid {pid})")).unwrap_or_default()
            ),
        ),
        Watcher::Nobody => (
            Level::Note,
            format!(
                "nothing watches the vendor heads now (no aterm window, and no terminal session \
                 running aterm, is open): a new {names} release is found by the next update \
                 pass instead of within minutes"
            ),
        ),
        Watcher::Unknown(why) => (
            Level::Note,
            format!(
                "could not tell whether anything watches the vendor heads ({why}): a new \
                 {names} release may wait for the next update pass"
            ),
        ),
    };
    Some(Row { level, text })
}

/// Whole days since `updated_at` (RFC3339), or `None` if it cannot be parsed.
fn index_age_days(updated_at: &str, now: i64) -> Option<i64> {
    let then = crate::flow::rfc3339_to_unix(updated_at)?;
    Some((now - then) / 86_400)
}

/// Why the last pass did not reach the signed index, as that pass wrote it into
/// `status.toml`'s outcome: the fetcher's cause a pass on the cached index appends
/// (`(index from cache — <why>)`, `record_index_freshness` in cli.rs), else the failed
/// pass's own sentence (`update failed: <why>`). `None` when the outcome says neither —
/// a later verb rewrote it — so the row never names a cause it did not read.
fn unreached_cause(outcome: &str) -> Option<&str> {
    const FROM_CACHE: &str = "(index from cache \u{2014} ";
    let why = match outcome.find(FROM_CACHE) {
        Some(at) => outcome[at + FROM_CACHE.len()..].strip_suffix(')')?,
        None => outcome.strip_prefix("update failed: ")?,
    };
    Some(why.trim()).filter(|why| !why.is_empty())
}

/// (5f b) For every recorded seam, each stock name in the view must present its tool: the
/// store's ([`presents`]), or the checkout's while trust is dev-linked to a sysroot (the view
/// is exec stubs there, so the stub's target is read). `store_bin` is the active (else
/// `current`) store build's `bin/`, `None` when there is none. A mismatch is a tool that
/// cannot run: `repair` rebuilds the view, except under a dev-link the seam refuses.
fn view_stock_names_check(f: &mut Vec<Finding>, layout: &Layout, store_bin: Option<PathBuf>) {
    let unlink = || Remedy::Command(String::from("aterm pkg unlink trust"));
    let source = crate::seam::view_source(layout);
    const STORE_WHAT: &str = "a clone of the store's";
    const STORE_WRONG: &str = "absent, other bytes, or the hard link a view held before clones";
    let (presented, presented_what, wrong, fix, remedy) = match (&source, store_bin) {
        (crate::seam::ViewSource::Linked(checkout), _) => (
            checkout.join("bin"),
            "the dev-linked checkout's",
            "absent, or a separate file",
            "`aterm pkg repair` rebuilds the view",
            Remedy::Repair,
        ),
        (crate::seam::ViewSource::LinkedNoSysroot(_), Some(bin)) => (
            bin,
            STORE_WHAT,
            STORE_WRONG,
            "`aterm pkg unlink trust` — atpkg cannot present a dev-link whose checkout is not a \
             sysroot, so `repair` cannot rebuild the view until the link goes",
            unlink(),
        ),
        (crate::seam::ViewSource::Store, Some(bin)) => (
            bin,
            STORE_WHAT,
            STORE_WRONG,
            "`aterm pkg repair` rebuilds the view",
            Remedy::Repair,
        ),
        // A refused dev-link with no store build behind the view: nothing the view
        // could be checked against, and nothing it could correctly present — whatever
        // it last presented is what `rustup run trust` runs. Said, with the one fix.
        (crate::seam::ViewSource::LinkedNoSysroot(checkout), None) => {
            for name in crate::seam::recorded_names(layout) {
                f.push(
                    Finding::change(
                        c::of(c::RUSTUP_VIEW, &name),
                        Class::Person,
                        format!("rustup `{name}`'s view"),
                        format!(
                            "rustup `{name}`: trust is dev-linked to {}, which atpkg cannot \
                             present (no bin/ and lib/, or a tree inside its own prefix), and no \
                             installed build stands behind the view, so `rustc +{name}` runs \
                             whatever the view last presented, or nothing; fix: `aterm pkg \
                             unlink trust`",
                            checkout.display()
                        ),
                        "the managed tools",
                        unlink(),
                    )
                    .detail(c::CANNOT_RUN),
                );
            }
            return;
        }
        (crate::seam::ViewSource::Store, None) => return,
    };
    // `repair` rebuilds a view; a dev-link it refuses is the person's to unlink.
    let class = if remedy == Remedy::Repair {
        Class::Relay
    } else {
        Class::Person
    };
    for name in crate::seam::recorded_names(layout) {
        let view_bin = crate::seam::view_dir(layout, &name).join("bin");
        for (public, trust) in crate::seam::STOCK_NAMES {
            let tool = presented.join(trust);
            if !tool.is_file() {
                continue;
            }
            let view_tool = view_bin.join(public);
            // A linked view's entries are exec stubs into the checkout; a store view's are
            // clones of the store's tools.
            let presented_ok = match &source {
                crate::seam::ViewSource::Linked(_) => {
                    crate::platform::resolve_shim(&view_tool).is_some_and(|t| t == tool)
                }
                _ => presents(&tool, &view_tool),
            };
            if !presented_ok {
                f.push(
                    Finding::change(
                        c::of(c::RUSTUP_VIEW, format!("{name}.{public}")),
                        class,
                        format!("`{public}` in rustup `{name}`'s view"),
                        format!(
                            "rustup `{name}`: {} is not {presented_what} {} ({wrong}), so \
                             `{public} +{name}` runs something other than the managed {trust}, \
                             or nothing; fix: {fix}",
                            view_tool.display(),
                            tool.display()
                        ),
                        format!("the managed {trust}"),
                        remedy.clone(),
                    )
                    .detail(c::CANNOT_RUN),
                );
            }
        }
    }
}

/// The one re-point of rustup's `trust` entry (`link`) to the managed view. A foreign
/// entry is refused before the view is laid, so when the view is missing the `ln` would
/// lay a link to nothing: `repair` then lays it.
fn repoint_fix(layout: &Layout, link: &Path) -> String {
    let target = crate::seam::seam_target(layout, "trust");
    let then = if target.is_dir() {
        ""
    } else {
        ", then aterm pkg repair"
    };
    format!("ln -sfn '{}' '{}'{then}", target.display(), link.display())
}

/// The doctor line for a rustup `trust` entry that is not the managed seam, or `None` when
/// the entry is absent or already ours (the callers' other checks speak then). Pure over the
/// probe result.
///
/// A foreign link names the exact re-point ([`repoint_fix`]); a foreign directory or file
/// cannot be re-pointed over (`ln -sfn` onto a directory lays the link inside it), so that
/// shape gets [`crate::seam::DETACH_FIX`]. A link into the store (`current`, or a numbered
/// build) is a note, not a warn: `repair` re-points it to the view by itself.
pub(crate) fn seam_line(st: &crate::seam::SeamStatus, layout: &Layout) -> Option<(Row, Remedy)> {
    use crate::seam::Entry;
    let target = crate::seam::seam_target(layout, "trust");
    let row = |level, text| Row { level, text };
    match &st.entry {
        Ok(Entry::Link(raw)) if !st.in_prefix => Some((
            row(
                Level::Warn,
                format!(
                    "rustup `trust` -> {} is NOT the managed store ({}): `cargo +trust`, \
             `rustup run trust` and every repo pinning `channel = \"trust\"` build with THAT \
             copy, not the one `aterm pkg update` keeps current. re-point it: {}; revert \
             by re-linking the old target the same way",
                    raw.display(),
                    target.display(),
                    repoint_fix(layout, &st.path)
                ),
            ),
            Remedy::Command(repoint_fix(layout, &st.path)),
        )),
        Ok(Entry::Link(_)) if !st.targets_view => Some((
            row(
                Level::Note,
                format!(
                    "rustup `trust` -> {} is inside the store, not the view atpkg lays \
             ({}); `aterm pkg repair` re-points it so the stock names rustup resolves are \
             the managed tools and updates move the channel",
                    st.target.as_deref().unwrap_or(&st.path).display(),
                    target.display()
                ),
            ),
            Remedy::Repair,
        )),
        Ok(entry @ (Entry::Dir | Entry::File | Entry::Other)) => Some((
            row(
                Level::Warn,
                format!(
                    "rustup `trust` at {} is {} — not a link into the managed store ({}), so \
             `cargo +trust` builds with whatever that holds; {}",
                    st.path.display(),
                    entry.describe(layout),
                    target.display(),
                    crate::seam::DETACH_FIX
                ),
            ),
            Remedy::Person {
                pane: st.path.display().to_string(),
                how: crate::seam::DETACH_FIX.to_owned(),
            },
        )),
        Ok(Entry::Link(_) | Entry::Absent) => None,
        Err(e) => Some((
            row(
                Level::Warn,
                format!(
                    "rustup `trust` at {} could not be inspected ({e})",
                    st.path.display()
                ),
            ),
            Remedy::None,
        )),
    }
}

/// The note for a stale rustup `trust` link the unattended re-assertion replaced
/// ([`crate::seam::live_build_tree`]): what it named and the one command that puts it back,
/// or, once put back, that no pass will move it again ([`crate::seam::replaced_note`]). A
/// note, since the channel works either way. `None` when nothing was replaced or the entry
/// has since changed.
fn replaced_seam_line(layout: &Layout, rustup_home: &Path) -> Option<String> {
    crate::seam::replaced_note(layout, rustup_home, crate::seam::DEFAULT_SEAM)
}

/// Where `rustup` is: the first executable `rustup` on `PATH`, else
/// `$CARGO_HOME/bin/rustup`, else `~/.cargo/bin/rustup` — the two places
/// rustup-init lays it, which an app-spawned pass (no rc file read) does not
/// have on its PATH. Existence only; whether it answers is [`rustup_present`].
fn rustup_binary(home: Option<&Path>) -> Option<std::path::PathBuf> {
    // The platform's executable spelling: a bare `rustup` is never a file on Windows.
    let file = rustup_file_name();
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(&file))
            .find(|candidate| candidate.is_file())
    });
    on_path
        .or_else(|| {
            std::env::var_os("CARGO_HOME")
                .map(|cargo_home| Path::new(&cargo_home).join("bin").join(&file))
                .filter(|candidate| candidate.is_file())
        })
        .or_else(|| {
            let candidate = home?.join(".cargo").join("bin").join(&file);
            candidate.is_file().then_some(candidate)
        })
}

/// `rustup` with the platform's executable suffix (`rustup.exe` on Windows, `rustup`
/// elsewhere) — the file name [`rustup_binary`] looks for.
fn rustup_file_name() -> String {
    let mut name = String::from("rustup");
    name.push_str(crate::platform::EXE_SUFFIX);
    name
}

/// Whether the `rustup` at `bin` answers `--version` inside [`PROBE_TIMEOUT`].
fn rustup_present(bin: &Path) -> bool {
    output_bounded(aterm_uds::spawnfd::command(bin).arg("--version"))
        .is_some_and(|o| o.status.success())
}

/// Does rustup resolve a `trust` channel? `rustup which cargo --toolchain trust` exits
/// non-zero (printing `'rustc' is not installed for the custom toolchain 'trust'`) when the
/// channel is absent or dangling; a linked channel answers with the cargo path.
fn rustup_trust_channel_resolves(bin: &Path) -> bool {
    output_bounded(aterm_uds::spawnfd::command(bin).args([
        "which",
        "cargo",
        "--toolchain",
        "trust",
    ]))
    .is_some_and(|o| o.status.success() && !o.stdout.is_empty())
}

/// How long one `--version` probe may take before doctor gives up on it: a healthy local
/// binary answers in milliseconds, and a wedged one (a stale NFS mount, a `rustup` shim
/// waiting on a network fetch) would otherwise hang the whole report.
const PROBE_TIMEOUT: std::time::Duration = crate::prereq::PROBE_TIMEOUT;

/// Poll interval while waiting for a probe to exit.
const PROBE_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// Run a version probe with a bounded wall clock, killing and reaping it on timeout. `None`
/// when it could not be spawned, finish in time, or be waited for: unlike the updater's
/// fail-closed helpers, doctor never fails because a probe failed, and a timed-out probe
/// reads `unknown`. Callers build the probe with `aterm_uds::spawnfd::command`, which on
/// Windows keeps a console probe from opening a visible window.
pub(crate) fn output_bounded(
    cmd: &mut aterm_uds::spawnfd::Command,
) -> Option<std::process::Output> {
    output_bounded_within(cmd, probe_timeout())
}

/// [`output_bounded`] within the caller's `within` instead of [`PROBE_TIMEOUT`]: a probe
/// with its own ceiling (`crate::platform`'s `defaults` reads), whose tests
/// hand it a hang detector instead.
pub(crate) fn output_bounded_within(
    cmd: &mut aterm_uds::spawnfd::Command,
    within: std::time::Duration,
) -> Option<std::process::Output> {
    bounded(cmd, false, within)
}

/// [`PROBE_TIMEOUT`], or a test's shorter one (`ShorterProbes`, unix tests only).
fn probe_timeout() -> std::time::Duration {
    // A test waits for a probe as for anything that must happen: a hang detector, never the
    // production ceiling, which a loaded machine can miss. A leg that wedges a probe on
    // purpose sets its own, shorter bound.
    #[cfg(all(test, unix))]
    return PROBE_TIMEOUT_IN_TEST
        .with(std::cell::Cell::get)
        .unwrap_or(aterm_spec::wait::MUST_HAPPEN);
    #[cfg(not(all(test, unix)))]
    PROBE_TIMEOUT
}

#[cfg(all(test, unix))]
thread_local! {
    static PROBE_TIMEOUT_IN_TEST: std::cell::Cell<Option<std::time::Duration>> =
        const { std::cell::Cell::new(None) };
}

/// A test's shorter [`PROBE_TIMEOUT`] for [`output_bounded`] on this thread until dropped,
/// for a leg whose driver is wedged on purpose. The production bound is pinned by
/// `prereq`'s wedged-compiler test. Unix only, like its one user (a script fixture).
#[cfg(all(test, unix))]
pub(crate) struct ShorterProbes;

#[cfg(all(test, unix))]
impl ShorterProbes {
    pub(crate) fn of(bound: std::time::Duration) -> Self {
        PROBE_TIMEOUT_IN_TEST.with(|b| b.set(Some(bound)));
        Self
    }
}

#[cfg(all(test, unix))]
impl Drop for ShorterProbes {
    fn drop(&mut self) {
        PROBE_TIMEOUT_IN_TEST.with(|b| b.set(None));
    }
}

/// [`output_bounded`] that keeps stderr too, within `within` — for a probe whose refusal
/// is the answer (`crate::prereq`: a broken compiler's first stderr line, verbatim).
pub(crate) fn output_bounded_capturing(
    cmd: &mut aterm_uds::spawnfd::Command,
    within: std::time::Duration,
) -> Option<std::process::Output> {
    bounded(cmd, true, within)
}

/// The most of a probe's stdout or stderr that is kept: a version answer is a line or two.
/// Past it the pipe is no longer read, so it fills and the child meets the deadline.
const PROBE_OUTPUT_CAP: usize = 1 << 20;

/// [`output_bounded`] and [`output_bounded_capturing`]: the child's pipes are read while it
/// runs and once more when it has exited, and never waited on past that.
///
/// Not `read_to_end` after the exit: the child's exit does not close its pipes, and any
/// process it started holds them as long as it lives (a `cargo` wrapper that backgrounds a
/// usage logger answers `-vV` at once, then holds the pipe for the logger's whole life).
/// Everything the child itself wrote is in the pipe by the time it has exited, so the last
/// read takes what is there and stops; on unix the pipes are non-blocking ([`Capture`]).
fn bounded(
    cmd: &mut aterm_uds::spawnfd::Command,
    keep_stderr: bool,
    within: std::time::Duration,
) -> Option<std::process::Output> {
    use aterm_uds::spawnfd::Stdio;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(if keep_stderr {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + within;
    let mut stdout = Capture::new(child.stdout.take());
    let mut stderr = Capture::new(child.stderr.take());
    let status = loop {
        stdout.pump();
        stderr.pump();
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                std::thread::sleep(PROBE_POLL.min(remaining));
            }
            Err(_) => return None,
        }
    };
    Some(std::process::Output {
        status,
        stdout: stdout.finish(deadline),
        stderr: stderr.finish(deadline),
    })
}

/// One pipe of a [`bounded`] probe, read without ever blocking on it: non-blocking, so a
/// read takes what is in the pipe and returns.
#[cfg(unix)]
struct Capture<R> {
    /// `None` at end of file, after an error, or when it could not be made non-blocking —
    /// a pipe that could block is never read.
    pipe: Option<R>,
    out: Vec<u8>,
}

#[cfg(unix)]
impl<R: std::io::Read + std::os::fd::AsRawFd> Capture<R> {
    fn new(pipe: Option<R>) -> Self {
        let pipe = pipe.filter(|pipe| {
            let fd = pipe.as_raw_fd();
            // SAFETY: `fd` is the open read end of a pipe this process owns (`pipe` holds
            // it for the whole call), and F_GETFL/F_SETFL read and set its status flags and
            // touch no memory.
            unsafe {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                flags >= 0 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0
            }
        });
        Self {
            pipe,
            out: Vec::new(),
        }
    }

    /// Read what is in the pipe now, up to [`PROBE_OUTPUT_CAP`].
    fn pump(&mut self) {
        let mut chunk = [0u8; 8192];
        while let Some(pipe) = self.pipe.as_mut() {
            let room = PROBE_OUTPUT_CAP.saturating_sub(self.out.len());
            if room == 0 {
                return;
            }
            let want = room.min(chunk.len());
            match pipe.read(&mut chunk[..want]) {
                Ok(0) => self.pipe = None,
                Ok(n) => self.out.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(_) => self.pipe = None,
            }
        }
    }

    /// What the child wrote: the pipe read once more after it exited, and no longer.
    fn finish(mut self, _deadline: std::time::Instant) -> Vec<u8> {
        self.pump();
        self.out
    }
}

/// One pipe of a [`bounded`] probe where a pipe cannot be made non-blocking: a thread reads
/// it, and [`Capture::finish`] waits for its end of file only until the probe's deadline,
/// then takes what it has read. The thread outlives the probe only while a process the
/// child left behind holds the pipe open.
#[cfg(not(unix))]
struct Capture {
    /// What the thread has read, and whether it is done reading.
    read: std::sync::Arc<std::sync::Mutex<(Vec<u8>, bool)>>,
}

#[cfg(not(unix))]
impl Capture {
    fn new<R: std::io::Read + Send + 'static>(pipe: Option<R>) -> Self {
        let read = std::sync::Arc::new(std::sync::Mutex::new((Vec::new(), pipe.is_none())));
        if let Some(mut pipe) = pipe {
            // The thread's handle is not called `shared`: the lock-order census
            // names a lock by its receiver, and a `shared` taken in a fn named `new`
            // reads as a lock held across `io::Error::new`.
            let filled = std::sync::Arc::clone(&read);
            let spawned = std::thread::Builder::new()
                .name("probe-pipe".into())
                .spawn(move || {
                    let mut chunk = [0u8; 8192];
                    loop {
                        let got = pipe.read(&mut chunk);
                        let mut read = filled.lock().unwrap_or_else(|e| e.into_inner());
                        match got {
                            Ok(n) if n > 0 && read.0.len() < PROBE_OUTPUT_CAP => {
                                let keep = n.min(PROBE_OUTPUT_CAP - read.0.len());
                                read.0.extend_from_slice(&chunk[..keep]);
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                            // End of file, an error, or past the cap (the pipe is no longer
                            // read, as on unix).
                            _ => {
                                read.1 = true;
                                return;
                            }
                        }
                    }
                });
            if spawned.is_err() {
                read.lock().unwrap_or_else(|e| e.into_inner()).1 = true;
            }
        }
        Self { read }
    }

    /// Nothing to do: the thread reads as the child writes.
    fn pump(&mut self) {}

    /// What the thread read by its end of file, or by `deadline`, whichever came first.
    fn finish(self, deadline: std::time::Instant) -> Vec<u8> {
        loop {
            {
                let read = self.read.lock().unwrap_or_else(|e| e.into_inner());
                if read.1 || std::time::Instant::now() >= deadline {
                    return read.0.clone();
                }
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            std::thread::sleep(PROBE_POLL.min(remaining));
        }
    }
}

/// Copy-pasteable manual PATH-append for this platform's shell: PowerShell on Windows,
/// POSIX `export` elsewhere. Only a fallback: an aterm shell sources the `shell.d` hook that
/// does this already.
#[cfg(windows)]
fn manual_path_hint(bin: &Path) -> String {
    format!("$env:PATH += \";{}\"", bin.display())
}
#[cfg(not(windows))]
fn manual_path_hint(bin: &Path) -> String {
    format!("export PATH=\"$PATH:{}\"", bin.display())
}

/// The `shell.d` hook extension the interactive shell on this platform actually sources.
#[cfg(windows)]
fn native_hook_ext() -> &'static str {
    "ps1"
}
#[cfg(not(windows))]
fn native_hook_ext() -> &'static str {
    "zsh"
}

/// Report the aterm app's own update posture beside the toolchain's, from its separate
/// updater's records (a running notarized `.app` is swapped by the in-session PTY handoff,
/// not by atpkg). It never says to reopen the app: a staged build applies in-session, and
/// only the window applies, so with no window open the note names `aterm --window`.
/// `installs_by_itself` is `[update] auto_apply`. Silent when there is no updater state (a
/// bare CLI install).
fn report_aterm_posture(
    layout: &crate::store::Layout,
    app_dirs: &[PathBuf],
    installs_by_itself: bool,
) -> Option<Row> {
    let support = layout.prefix.parent()?;
    let running = running_bundle_plist();
    let bundle_build = running.as_deref().and_then(plist_bundle_version);
    // (build, version) off every bundle readable here: the running one, then each app
    // dir's `aterm.app` — so a source build still names the installed bundle's version.
    let versions: Vec<(u64, String)> = running
        .into_iter()
        .chain(app_dirs.iter().filter_map(|d| {
            crate::metadata_io::read_bounded_regular_utf8(
                &d.join("aterm.app/Contents/Info.plist"),
                1024 * 1024,
            )
            .ok()
        }))
        .filter_map(|t| Some((plist_bundle_version(&t)?, plist_short_version(&t)?)))
        .collect();
    report_aterm_posture_at(
        &support.join("Updates"),
        bundle_build,
        &versions,
        installs_by_itself,
    )
}

/// [`report_aterm_posture`] over an explicit updater ledger dir, the sealed build of the
/// running bundle (`None` outside one), and each bundle's `(build, version)`. "Installed on
/// disk" is the newer of the receipt and the bundle's `CFBundleVersion` (a hand-placed
/// bundle is newer than any receipt), and the note fires only when that is newer than what
/// runs.
fn report_aterm_posture_at(
    updates: &Path,
    bundle_build: Option<u64>,
    versions: &[(u64, String)],
    installs_by_itself: bool,
) -> Option<Row> {
    let field = |file: &str, key: &str| -> Option<String> {
        let text = std::fs::read_to_string(updates.join(file)).ok()?;
        text.lines()
            .find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == key))
            .map(|(_, v)| v.trim().trim_matches('"').to_string())
    };
    let receipt = field("installed.toml", "build_number")?;
    let note = |text: String| {
        Some(Row {
            level: Level::Note,
            text,
        })
    };
    let installed = match (receipt.parse::<u64>().ok(), bundle_build) {
        (Some(r), Some(b)) => r.max(b).to_string(),
        (None, Some(b)) => b.to_string(),
        _ => receipt,
    };
    let current = field("status.toml", "current_build");
    let newer_on_disk = match (current.as_deref(), installed.parse::<u64>().ok()) {
        (Some(c), Some(i)) => c.parse::<u64>().is_ok_and(|c| i > c),
        _ => false,
    };
    // A build as a person names it: its version when a bundle readable here is sealed at
    // it, else `build N` — never a version it did not read off that build.
    let named = |build: &str| -> String {
        versions
            .iter()
            .find(|(b, _)| b.to_string() == build)
            .map_or_else(|| format!("build {build}"), |(_, v)| v.clone())
    };
    // Two builds as a person tells them apart: by version, unless both read the same (a dev
    // bundle stamps the workspace version over a new build number), then by build number.
    let pair = |new: &str, running: &str| -> (String, String) {
        match (named(new), named(running)) {
            (a, b) if a == b => (format!("build {new}"), format!("build {running}")),
            names => names,
        }
    };
    // Off `[update] auto_apply`, a build waits for Install — `aterm update`'s words.
    let by_hand = "install it from Settings \u{25b8} Software Update; your shells keep running";
    match (current, field("status.toml", "staged_build")) {
        (Some(current), Some(staged)) if current != staged => {
            // Only the window applies (see the fn doc). When its installs of this build keep
            // failing, no install is promised; the count is health.toml's per-target one,
            // keyed to the build it tried, since `apply_failures` carries a superseded
            // build's tries onto a fresh download.
            let tries = field("health.toml", "apply_failures_for_target")
                .and_then(|n| n.parse::<u32>().ok())
                .filter(|n| *n > 0)
                .filter(|_| {
                    let tried = field("health.toml", "last_apply_failure_target_build")
                        .and_then(|b| b.parse::<u64>().ok());
                    tried.is_some() && tried == staged.parse::<u64>().ok()
                });
            let (staged, current) = pair(&staged, &current);
            if let Some(n) = tries {
                let tries = if n == 1 {
                    String::from("1 try")
                } else {
                    format!("{n} tries")
                };
                note(format!(
                    "aterm {staged} is downloaded (running {current}) but didn\u{2019}t install \
                     ({tries})"
                ))
            } else if installs_by_itself {
                note(format!(
                    "aterm {staged} is downloaded (running {current}) and installs within a \
                     minute while an aterm window is open — your shells keep running; with no \
                     window open: aterm --window"
                ))
            } else {
                note(format!(
                    "aterm {staged} is downloaded (running {current}) — {by_hand}"
                ))
            }
        }
        (Some(current), _) if newer_on_disk => {
            // A newer bundle is already on disk: the window activates it in place, the same
            // in-session lane — and, as above, only the window.
            let (installed, current) = pair(&installed, &current);
            if installs_by_itself {
                note(format!(
                    "aterm {installed} is on disk (running {current}) and an aterm window \
                     switches to it in place — your shells keep running; with no window open: \
                     aterm --window"
                ))
            } else {
                note(format!(
                    "aterm {installed} is on disk (running {current}) — {by_hand}"
                ))
            }
        }
        _ => {
            // The version a person recognises, read off whichever bundle's sealed build is
            // the installed one; with none readable the row stays build-only, so it never
            // names a version it did not read off that build.
            Some(Row {
                level: Level::Ok,
                text: match versions.iter().find(|(b, _)| b.to_string() == installed) {
                    Some((_, v)) => format!("aterm {v} (build {installed}) installed"),
                    None => format!("aterm build {installed} installed"),
                },
            })
        }
    }
}

/// The text of the `Contents/Info.plist` of the `.app` this process runs from; `None`
/// outside a bundle (a source build, a test binary) or on any doubt. One read serving
/// both [`plist_bundle_version`] and [`plist_short_version`].
fn running_bundle_plist() -> Option<String> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let macos = exe.parent()?;
    if macos.file_name()?.to_str()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()?.to_str()? != "Contents" {
        return None;
    }
    crate::metadata_io::read_bounded_regular_utf8(&contents.join("Info.plist"), 1024 * 1024).ok()
}

/// `CFBundleShortVersionString` out of an XML plist — the marketing version the cutter
/// seals (`0.93.0`) — bound to the `<string>` immediately after the key like
/// [`plist_bundle_version`], and admitted only as a short version-shaped token (digits,
/// dots, letters, `-`, `+`, at most 32 bytes): it reaches a terminal.
fn plist_short_version(text: &str) -> Option<String> {
    let key = "<key>CFBundleShortVersionString</key>";
    let after = text.find(key)? + key.len();
    let value = text[after..].trim_start().strip_prefix("<string>")?;
    let end = value.find("</string>")?;
    let v = value[..end].trim();
    let shaped = !v.is_empty()
        && v.len() <= 32
        && v.bytes().next().is_some_and(|b| b.is_ascii_digit())
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'));
    shaped.then(|| v.to_string())
}

/// `CFBundleVersion` out of an XML plist: the `<string>` immediately after the key (modulo
/// whitespace), never a string further on that another key owns, parsed as the integer
/// build number the cutter seals. `None` for a missing key, an intervening element, or a
/// non-integer value.
fn plist_bundle_version(text: &str) -> Option<u64> {
    let key = "<key>CFBundleVersion</key>";
    let after = text.find(key)? + key.len();
    let value = text[after..].trim_start().strip_prefix("<string>")?;
    let end = value.find("</string>")?;
    value[..end].trim().parse::<u64>().ok()
}

/// The index (in `split_paths` order) of the first `PATH` entry holding an executable `name`
/// outside the managed `prefix` (the reroute dir lives under it, so a laid stub never counts
/// as its own upstream). A local walk on purpose: the vendor probes are
/// [`crate::store::ToolName`]-gated and `cargo` is deny-listed there. Relative entries are
/// skipped (they name the cwd, not a toolchain), and only an executable regular file counts.
fn upstream_index_on_path(
    entries: &[std::path::PathBuf],
    prefix: &Path,
    name: &str,
) -> Option<usize> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    entries.iter().position(|dir| {
        if dir.as_os_str().is_empty() || !dir.is_absolute() || dir.starts_with(prefix) {
            return false;
        }
        let Ok(meta) = std::fs::metadata(dir.join(&file)) else {
            return false;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            meta.is_file() && meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            meta.is_file()
        }
    })
}

/// A doctor row's weight; each doctor spells it in its own marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Nothing to do.
    Ok,
    /// Something the owner should do; never a failure.
    Warn,
    /// A fact, or a measurement that could not be made.
    Note,
}

/// One doctor row: its weight and its words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Ok, warn or note.
    pub level: Level,
    /// The sentence after the mark.
    pub text: String,
}

/// A row's weight in this report's words.
fn level_word(level: Level) -> &'static str {
    match level {
        Level::Ok => "ok",
        Level::Warn => "warn",
        Level::Note => "note",
    }
}

#[cfg(test)]
#[path = "doctor_tests.rs"]
mod tests;
