// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `atpkg doctor` health surface (§15) — a no-network, no-mutation diagnostic over
//! atpkg's own store/ops/sig/status primitives.
//!
//! Structural breakage (a broken bin shim, an active build whose store tree vanished, a
//! fish-breaking stray `.sh`, a world-writable login-sourced dir) is a PROBLEM → nonzero
//! exit. Everything advisory (bin not yet on PATH, a frozen-looking index, a foreign
//! sysroot wiring) stays a WARNING → exit 0. A managed tool that cannot run (tippy
//! refused by its own Trust bundle), or one doctor cannot show to run (an exec root that
//! differs from its build, a build it cannot read), is neither a store fault nor health:
//! the report ends "not healthy", names the fix, and still exits 0. It reads no
//! unverified index/manifest (verify-before-parse): its freshness surface reads atpkg's
//! OWN `status.toml` + the durable [`crate::sig::Floor`].

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use aterm_primer::CLAUDE_TOP_EFFORT_KEY;

use crate::store::Layout;

const GIB: u64 = 1 << 30;

/// Whether a recorded program `state` describes a FAULT this machine should act on.
///
/// The fault prefixes are the ones the install/update paths actually write.
///
/// * `aborted: <phase>` (cli.rs, the `TxnOutcome::Aborted` arm) — a coherence-group
///   transaction killed mid-flight, precisely the state in which a tuple's members may
///   disagree with each other. Recorded against the FAILED member only, and on a group
///   disk-shortfall that member is an arbitrary one of the tuple, so the row names where
///   the transaction stopped rather than what is individually wrong.
/// * `tombstoned: pin yanked/below floor` — silent everywhere else. A yanked pin
///   replaces the program's working shims with failing stubs
///   ([`crate::activate::install_tombstone_shim`]); the broken-shim scan skips
///   tombstones BY DESIGN, and a tombstoned program drops out of `active_builds`, so
///   without this row doctor would call a stubbed compiler "healthy". Both clear
///   themselves: a later successful update rewrites the row and replaces the tombstone
///   shim with a real symlink.
///
/// Deliberately NOT a fault: `active`, `dev-linked (skipped)` (a §13 hard-skip the user
/// asked for), `held: …` (a tuple whose new pin is not published for this target), and any
/// future informational state. Allow-by-prefix, so an unrecognized state reads as benign
/// rather than as a failure.
fn is_problem_state(state: &str) -> bool {
    state.starts_with("error:")
        || state.starts_with("unavailable:")
        || state.starts_with("blocked:")
        || state.starts_with("aborted:")
        || state.starts_with("tombstoned:")
        // A member HELD after a failed stage (`held: last attempt failed with …; <retry>`)
        // is not installed and will not be by any unattended pass — a problem, and one
        // whose row already names the act. The group hold (`held: pinned build …`) is
        // the benign state the allow-list keeps.
        || state.starts_with(crate::state::HELD_FAILED_PREFIX)
}

/// Where the per-problem listing should START — or `None` when no verdict owns it.
///
/// The listing hangs off the branch that actually printed a verdict; `store_empty` alone
/// is wrong on a DECLINED store (it would drop the only finding, or print orphan lines
/// under a verdict nobody gave).
///
/// A declined store lists nothing: the decline IS the verdict, and a stale fault row
/// describes a toolset the user deliberately removed. Whether such a row should also flip
/// the exit code is a separate product question, deliberately not answered here.
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

/// The programs this machine WANTS but does not have, split by whether the removed
/// ledger explains the absence: `(unexplained, on_purpose)`, each alphabetical.
///
/// "Wants" is [`crate::cli::wanted_programs`] — the signed set narrowed by
/// `[packages].exclude` — so this asks the same question the install lanes ask, and
/// cannot drift from them into a second opinion.
///
/// The split is the whole point. A program on the removed ledger is a decision, and
/// reporting it as a problem would be the manager arguing with the user. A program
/// missing with NO ledger entry is the state nothing else in this report can see: no
/// shim, no `status.toml` row, no recorded fault — invisible to every check that reads
/// the record rather than the index.
///
/// Offline and fail-quiet: with no cached index there is no trustworthy answer about
/// what SHOULD be installed, and inventing one would turn an unreachable network into a
/// pile of phantom missing programs.
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

/// The decision [`missing_against_index`] makes, without the I/O: which wanted programs
/// are absent, and which of those the removed ledger accounts for.
///
/// Pure: the I/O path needs a signed index, a registry and a store.
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
        // A DEV-LINKED program is PRESENT — its shims are in the managed bin/ and they
        // run — it just has no store build for `installed` to count. It outranks the
        // removed ledger: a link made after a removal is the newer decision.
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

/// Whether one `status.toml` row is a fault [`recorded_problems`] lists: a real program
/// (a `-`-prefixed key is a stray row, never a program) in a problem state
/// ([`is_problem_state`]). Public so the window's Settings ▸ Packages badge counts the
/// doctor's own list; the badge then drops what the doctor also drops on a DECLINED store
/// (everything), and the one fault nobody can act on
/// ([`crate::state::is_unserved_toolset`]).
#[must_use]
pub fn is_recorded_problem(name: &str, state: &str) -> bool {
    !name.starts_with('-') && is_problem_state(state)
}

/// EVERY recorded fault, formatted `"<program>: <state>"`, in program order — `BTreeMap`
/// order, i.e. alphabetical by program name.
///
/// Every finding, not the first: one per repair cycle is a guessing game.
///
/// Stray rows (keys beginning with `-`) are excluded — see the caller: they cannot be
/// programs at all, and counting them condemned healthy toolsets.
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

/// The toolset-wide "no build for this architecture" verdict, on the ONE platform where
/// it is a state and not a fault: WINDOWS. `Some(names)` — the programs whose own rows
/// say `unavailable on <target>: …` — when the `*toolset*` row reads that verdict here;
/// `None` on every other platform, whatever the row says.
///
/// Why the platform split. On an Intel Mac the absence of an x86_64 build is a
/// publishing gap the report is right to call a PROBLEM and to answer with `aterm pkg
/// install --default-set`: the seal serves that machine the day it is cut. On Windows
/// (index build 44) every ALab row is `prebuilt-only` with no Windows artifact and only
/// the agents are published, so `install --default-set` cannot change the state and a
/// PROBLEM line recommending it would be false. The row is also STALE by construction:
/// it is written when a pass installs nothing on an empty store and cleared only by a
/// clean pass with something installed, so it outlives the day the agents became
/// servable here.
fn toolset_unserved_here(status: Option<&crate::Status>) -> Option<Vec<String>> {
    if !cfg!(windows) {
        return None;
    }
    let s = status?;
    // The members whose OWN rows say the index publishes nothing for this target — the
    // durable signal. The toolset-wide row is written only by a pass that installed
    // nothing on an empty store and is cleared by the first clean pass with something
    // installed, so after `claude`/`codex` land here it is gone while the ten unserved
    // members are exactly as unserved as before.
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

/// [`split_missing`]'s "unexplained" list with the unserved members taken out: on an
/// unserved platform each of them IS explained — by its own `unavailable on <target>` row,
/// which the note names. Identity when `unserved` is `None` (every platform but Windows).
fn drop_unserved(missing: Vec<String>, unserved: Option<&[String]>) -> Vec<String> {
    match unserved {
        Some(unserved) => missing
            .into_iter()
            .filter(|p| !unserved.contains(p))
            .collect(),
        None => missing,
    }
}

/// Whether a recorded problem (`"<program>: <state>"`) is the toolset-wide
/// "no build for this architecture" verdict ([`crate::state::is_unserved_toolset`]) — the
/// ONE `*toolset*` row [`toolset_unserved_here`] turns into a note. Every other
/// `*toolset*` verdict (`unavailable: index unreachable`, `unavailable: every published
/// build was refused`, `unavailable: [packages].exclude leaves nothing to install`, …) is
/// a fault about THIS machine and stays a problem on every platform.
fn is_unserved_toolset_problem(why: &str) -> bool {
    why.strip_prefix("*toolset*: ")
        .is_some_and(crate::state::is_unserved_toolset)
}

/// Whether a recorded problem is a member HELD after a failed stage
/// ([`crate::state::HELD_FAILED_PREFIX`]) — a problem no unattended pass moves.
fn is_held_problem(why: &str) -> bool {
    why.split_once(": ")
        .is_some_and(|(_, state)| state.starts_with(crate::state::HELD_FAILED_PREFIX))
}

/// The first member the recorded problems say is HELD ([`is_held_problem`]), by name —
/// wherever it sorts: a `*toolset*` row precedes every program in `BTreeMap` order, so on
/// an empty store the held member is never the first problem.
fn first_held_member(recorded_problems: &[String]) -> Option<&str> {
    recorded_problems
        .iter()
        .filter(|why| is_held_problem(why))
        .find_map(|why| why.split_once(": ").map(|(program, _)| program))
}

/// The explicit door for a member HELD after a failed stage: `aterm pkg install <p>`, which
/// forgets its memo and lands it — or, for a coherence-group member (`grouped`), `aterm pkg
/// update --retry`, which forgets every memo and runs the pass that moves or completes the
/// tuple whole; `install` would land that one member outside its tuple.
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

/// Whether an EMPTY store on an unserved platform is the quiet state — the note, "none is
/// missing", healthy: only when nothing is recorded against a member the index serves
/// here AND no served member is missing without a row. A network failure writes no
/// per-program row by design (`failed_install_state`), so an empty problem list alone
/// does not prove every served member arrived; the index's own list has to agree.
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

/// The ONE doctor line for installed files that still carry `com.apple.provenance` — the
/// files every package pass and `aterm pkg repair` clear ([`crate::provenance::heal_store`])
/// and could not: how many, what that breaks, and the verb that tries again. At most 160
/// characters with the speaker's prefix. Pure, so the words are pinned by a test that
/// mints a synthetic attribute rather than the one it cannot.
pub(crate) fn provenance_line(scan: &crate::provenance::Scan) -> String {
    let n = scan.carriers.len();
    format!(
        "warn — {} still {} a macOS tag (com.apple.provenance) that release builds refuse. \
         fix: aterm pkg repair",
        crate::provenance::count_of(n, "installed file"),
        if n == 1 { "carries" } else { "carry" },
    )
}

/// What the WORKSPACE the operator is standing in demands of the installed Trust
/// toolchain, as decided by the installed `targo` itself (`targo locate-project
/// --workspace` parses the manifest, compiles nothing, and refuses a `[trust]` policy
/// key it does not know).
///
/// A workspace committed against a LOCAL Trust seal newer than anything atpkg has
/// published fails on every managed machine with an "unknown field" error that reads
/// like a typo; this check names the toolchain that is behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspacePolicy {
    /// The installed targo accepts the workspace's `[trust]` policy.
    Accepted,
    /// The installed targo refuses a policy key it does not know: the workspace
    /// needs a NEWER Trust build than atpkg has published.
    UnknownField { field: String },
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

/// rustup's `trust` channel resolving OUTSIDE the atpkg store: a locally built or
/// sealed toolchain. Newer than the store's, it is a publisher's seal: anything committed
/// against a feature only it has will not build on an atpkg-managed machine until the seal
/// is PUBLISHED. Older or gone, it is stale, and `repair` re-points it at the store.
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
    /// The extended attribute the bundle/shim scan looks for —
    /// [`crate::provenance::PROVENANCE_XATTR`] in production. Injected because that name
    /// cannot be minted by a test (`xattr -w com.apple.provenance` is refused), so the
    /// scan is exercised with a `user.*` attribute through the very `listxattr` path.
    pub provenance_attr: &'static str,
    /// Whether packages update here on their own: the manager armed, and `[packages]
    /// enabled` on — Automatic updates, default on, with the retired `auto_update` folded
    /// in ([`crate::config::PackagesConfig::enabled`]). With [`Self::excluded`], what the
    /// never-checked line may claim of the vendor programs.
    pub automatic: bool,
    /// The programs `[packages] exclude` names: never updated here, by any lane.
    pub excluded: Vec<String>,
    /// What the `[packages]` table's own spelling asks of its owner — a retired key to
    /// rename or remove ([`crate::config::PackagesConfig::config_notes`]), one `note` line
    /// each.
    pub config_notes: Vec<String>,
    /// A `[packages] prefix` this shipped build does not use, naming another store
    /// ([`crate::config::PackagesConfig::ignored_prefix`]): a `warn`, because nothing is
    /// installed unattended until the line goes.
    pub ignored_prefix: Option<PathBuf>,
    /// Retired environment opt-outs still EXPORTED to this process — `(name, the setting
    /// that replaced it)` ([`retired_opt_outs_exported`]): a `warn` each, because the
    /// person who set one believes something is off that is not.
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
}

impl Default for Probes {
    fn default() -> Self {
        Self {
            workspace: None,
            local_seal: None,
            provenance_attr: crate::provenance::PROVENANCE_XATTR,
            automatic: true,
            excluded: Vec::new(),
            config_notes: Vec::new(),
            ignored_prefix: None,
            retired_env: Vec::new(),
            stub_env: crate::reroute::StubEnv::default(),
            index_head: false,
            cc: None,
            claude_defaults: None,
        }
    }
}

/// Whether Claude Code's rating surveys (its optional 1/2/3/0 prompts) are OFF under this
/// `~/.claude/settings.json` text.
///
/// Read from Claude Code itself (2.1.284): every survey kind returns before it rolls when
/// `CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY` reads true — a value TRIMMED and lower-cased that is
/// `1`, `true`, `yes` or `on` (`0`, `false`, `no`, `off`, `2` and the rest are NOT off) — and the
/// settings `env` block's entries are written into the process environment, live, when the
/// file is saved. Otherwise the SESSION survey (the only kind that reaches this account today)
/// appears with probability `settings.feedbackSurveyRate ?? <remote default>`; a rate of `0` is
/// off for it and for it alone. Anything else — no file, unreadable JSON, no key — leaves the
/// vendor's default, which shows them.
///
/// Why aterm cares: a rating card draws under the prompt of a session the harness is
/// supervising and waits for a keypress no one is there to give.
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

/// Claude Code's `bool` parse of an `env` value (`Le`): stringified, trimmed, lower-cased, one of
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

/// The Claude Code defaults the owner runs with — `(key, value as JSON text)` —
/// MIRRORED from `aterm_primer::CLAUDE_DEFAULTS`, which the window writes where a
/// key is unset. The values are JSON text here and typed there, so the table is a
/// mirror, not a re-export: atpkg takes only the top-effort key's one spelling from
/// aterm-primer ([`CLAUDE_TOP_EFFORT_KEY`]), and the two tables are pinned equal by
/// a test in aterm-gui (which depends on both).
pub const CLAUDE_DEFAULTS: [(&str, &str); 5] = [
    (CLAUDE_TOP_EFFORT_KEY, "true"),
    ("effortLevel", "\"xhigh\""),
    ("feedbackSurveyRate", "0"),
    (CLAUDE_SURVEY_SWITCH, "\"1\""),
    (CLAUDE_DRAFTS_KEY, "\"quiet\""),
];

/// The key of the drafted-feedback card's setting — MIRRORED from
/// `aterm_primer::CLAUDE_DRAFTS_KEY`, pinned equal by the same aterm-gui test.
pub const CLAUDE_DRAFTS_KEY: &str = "feedbackDrafts";

/// The key of the default that turns EVERY rating survey off: the `env` block's
/// `CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY` (a dotted key is a nested one) — MIRRORED from
/// `aterm_primer::CLAUDE_SURVEY_SWITCH`, pinned equal by the same aterm-gui test.
pub const CLAUDE_SURVEY_SWITCH: &str = "env.CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY";

/// The defaults that SET AN EFFORT — MIRRORED from
/// `aterm_primer::CLAUDE_EFFORT_DEFAULTS`, pinned equal by the same aterm-gui test
/// module. The window adds neither while a person's own effort stands
/// ([`claude_efforts_set_by_hand`]): Claude Code resolves the effort to `xhigh`
/// whenever the top-effort key ([`CLAUDE_TOP_EFFORT_KEY`]) is true, before it reads
/// the level a person chose.
pub const CLAUDE_EFFORT_DEFAULTS: [&str; 2] = [CLAUDE_TOP_EFFORT_KEY, "effortLevel"];

/// A survey probability a person set BY HAND in the Claude settings text `settings_json`:
/// the top-level `feedbackSurveyRate` at any value but the number zero, as its JSON text
/// (`0.3`, `1`, `"0.5"`, `null`); `None` when it is unset or zero, and for text that does
/// not parse. The window adds [`CLAUDE_SURVEY_SWITCH`] only where none stands (the switch
/// silences every survey, whatever the probability asks for).
///
/// MIRRORED from `aterm_primer::claude_survey_rate_set_by_hand` and pinned equal to it by
/// a test in aterm-gui, as [`CLAUDE_DEFAULTS`] is.
#[must_use]
pub fn claude_survey_rate_set_by_hand(settings_json: &str) -> Option<String> {
    aterm_json::from_str::<aterm_json::Value>(settings_json)
        .ok()
        .and_then(|v| survey_rate_set_by_hand(&v))
}

/// [`claude_survey_rate_set_by_hand`] over a parsed document.
fn survey_rate_set_by_hand(v: &aterm_json::Value) -> Option<String> {
    let rate = v.get("feedbackSurveyRate")?;
    // A JSON number that is zero; `null`, a string or any other number is the person's own.
    let zero = matches!(rate, aterm_json::Value::Number(_)) && rate.as_f64() == Some(0.0);
    (!zero).then(|| aterm_json::to_string(rate).unwrap_or_default())
}

/// Every effort a person set BY HAND in the Claude settings text `settings_json`
/// that is not the default's `xhigh`, as `(path, value as JSON text)`: the
/// top-level `effortLevel` first, then each `modelSettings.<model>.effortLevel`
/// (where `/effort <level>` saves it) in model-name order, the last of a duplicate
/// key winning. Empty when there is none, and for text that does not parse.
///
/// MIRRORED from `aterm_primer::claude_efforts_set_by_hand` — the rule by which
/// the window holds [`CLAUDE_EFFORT_DEFAULTS`] back — and pinned equal to it by a
/// test in aterm-gui, as [`CLAUDE_DEFAULTS`] is.
#[must_use]
pub fn claude_efforts_set_by_hand(settings_json: &str) -> Vec<(String, String)> {
    aterm_json::from_str::<aterm_json::Value>(settings_json)
        .map(|v| efforts_set_by_hand(&v))
        .unwrap_or_default()
}

/// [`claude_efforts_set_by_hand`] over a parsed document. [`aterm_json::Map`] is
/// sorted and keeps the last of a duplicate key, which is the order and the rule
/// the mirror states.
fn efforts_set_by_hand(v: &aterm_json::Value) -> Vec<(String, String)> {
    let xhigh = CLAUDE_DEFAULTS
        .iter()
        .find(|(k, _)| *k == "effortLevel")
        .map_or("", |(_, want)| *want);
    let mut found = Vec::new();
    let mut note = |path: String, effort: &aterm_json::Value| {
        let text = aterm_json::to_string(effort).unwrap_or_default();
        if text != xhigh {
            found.push((path, text));
        }
    };
    if let Some(effort) = v.get("effortLevel") {
        note("effortLevel".to_string(), effort);
    }
    if let Some(models) = v
        .get("modelSettings")
        .and_then(aterm_json::Value::as_object)
    {
        for (model, settings) in models {
            if let Some(effort) = settings.get("effortLevel") {
                note(format!("modelSettings.{model}.effortLevel"), effort);
            }
        }
    }
    found
}

/// Where one default stands in a person's Claude settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultState {
    /// Set to the default (or, for the survey, turned off another way).
    Default,
    /// Set by hand to something else — kept, never overwritten.
    Manual(String),
    /// Not set, and not added: one of [`CLAUDE_EFFORT_DEFAULTS`] while a person's
    /// own effort stands — named here, `<path> = <value>` (several joined by `, `).
    HeldForEffort(String),
    /// Not set, and not added: [`CLAUDE_SURVEY_SWITCH`] while a person's own survey
    /// probability stands — named here as its JSON text. The switch silences every
    /// survey, so it would override the probability the person chose.
    HeldForRate(String),
    /// The top-effort key ([`CLAUDE_TOP_EFFORT_KEY`]) `true` beside an effort a
    /// person set by hand — named here as [`DefaultState::HeldForEffort`] names it.
    /// Claude Code resolves the effort to `xhigh` whenever that key is true, so that
    /// effort is never used. Nothing can tell who wrote the key, so the window never
    /// removes it — the doctor names it and the person decides.
    OverridesEffort(String),
    /// Not set: an aterm window adds the default at its next start.
    Unset,
}

/// Each of [`CLAUDE_DEFAULTS`] as `settings_json` leaves it, by the window's own
/// rule (`aterm_primer::add_claude_defaults`): an unset key is added, except the
/// effort defaults while a person's own effort stands — and a set top-effort key
/// ([`CLAUDE_TOP_EFFORT_KEY`]) that overrides such an effort is
/// [`DefaultState::OverridesEffort`], never the default.
///
/// # Errors
///
/// A file the window refuses to add to, and leaves exactly as it is: the parse
/// error (an empty file included), or "the top level is not a JSON object". No
/// key is reported for it — "an aterm window adds …" would promise a repair that
/// never happens.
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
    let rate_by_hand = survey_rate_set_by_hand(&v);
    let by_hand: Vec<String> = efforts_set_by_hand(&v)
        .into_iter()
        .map(|(path, value)| format!("{path} = {value}"))
        .collect();
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
                    // The switch is on for whatever Claude Code reads as true (`yes`, `TRUE`, ` 1 `,
                    // a JSON `true`), not only for the spelling the window writes.
                    let is_default = if key == CLAUDE_SURVEY_SWITCH {
                        claude_env_true(have)
                    } else {
                        // The file's own value only: the `env` survey switch sets no
                        // rate, and a rate a person wrote is theirs, whatever the switch.
                        text == want || (key == "feedbackSurveyRate" && have.as_f64() == Some(0.0))
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
/// [`CLAUDE_DEFAULTS`] absent from the top level, unless it is an effort default
/// held back by a person's own effort: the keys the rows call `Unset`.
fn window_adds(settings_json: &str) -> bool {
    let Ok(v) = aterm_json::from_str::<aterm_json::Value>(settings_json) else {
        return false;
    };
    let held = !efforts_set_by_hand(&v).is_empty();
    let rate_held = survey_rate_set_by_hand(&v).is_some();
    CLAUDE_DEFAULTS.iter().any(|(key, _)| {
        default_value(&v, key).is_none()
            && !(held && CLAUDE_EFFORT_DEFAULTS.contains(key))
            && !(rate_held && *key == CLAUDE_SURVEY_SWITCH)
    })
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

/// What (10h) reads of the Claude settings file at `path` (`~/.claude/settings.json`):
/// [`claude_defaults_state`] over its text, or `None` when there is no file (and no
/// link) at all. What the text cannot show makes it, instead, the `Err` the one
/// refusal row names:
///
/// * A file that cannot be read — a link loop, a directory at that path, bytes that
///   are not UTF-8, a file this user may not read. `aterm_primer::add_claude_defaults`
///   reads it the same way and refuses it at every start.
/// * A symlink whose target does not exist — a dotfiles link to a file not there
///   yet — which `aterm_primer::add_claude_defaults` refuses at every start rather
///   than replace the link with a plain file. Read through the link it looked like
///   no file at all, and the doctor said nothing.
/// * A file whose REAL directory (a link's target's: the window's atomic write puts
///   its temp file there and renames it over the target) this user cannot write — a
///   link into a read-only store, as home-manager lays one out — while the text
///   leaves a key the window would add ([`window_adds`]). Every window start tries
///   that write and fails, so the row promising the addition was false. A file that
///   needs nothing added is judged by its text alone: the window writes nothing to
///   it, and its rows (a hand-set value kept, a top-effort key over a hand-set
///   effort) still hold.
fn probe_claude_settings(path: &Path) -> Option<Result<Vec<(&'static str, DefaultState)>, String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // NotFound through a link means the LINK is there and its target is not:
            // the window's own reading (`add_claude_defaults`).
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
    let states = claude_defaults_state(&text);
    if states.is_ok() && window_adds(&text) {
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

/// Retired environment knobs a person may still export from a shell rc or a script —
/// each with the setting or launch flag that replaced it. An inert opt-out silently
/// stops opting out (`ATERM_NO_AUTO_UPDATE=1` no longer holds updates), so the doctor
/// names it.
///
/// THE ONE PLACE A RETIRED NAME IS READ, and it reads PRESENCE only — never the value, and
/// nothing acts on it: this is the doctor telling a person their switch no longer does
/// anything and naming the one that does. `crates/aterm-update-core/tests/env_reads.rs`
/// admits this table by name (`RETIRED_DETECTOR`) and no other read of these names.
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
        "gpu = false (Settings ▸ Performance), or launch with --cpu",
    ),
    (
        "ATERM_GPU",
        "the default: the GPU renderer is on unless gpu = false (--gpu forces it for one launch)",
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
        "font_family (Settings ▸ Typography), or launch with --font",
    ),
    (
        "ATERM_FONT_PX",
        "font_px (Settings ▸ Typography), or launch with --font-px",
    ),
    ("ATERM_FONT_HINTING", "font_hinting in aterm.toml"),
    ("ATERM_FONT_SUBPIXEL", "font_subpixel in aterm.toml"),
    (
        "ATERM_FALLBACK_FONT",
        "fallback_fonts (Settings ▸ Typography)",
    ),
    ("ATERM_SYMBOL_FONT", "symbol_font (Settings ▸ Typography)"),
    ("ATERM_EMOJI_FONT", "emoji_font (Settings ▸ Typography)"),
    ("ATERM_STEM_GAMMA", "stem_gamma (Settings ▸ Typography)"),
    ("ATERM_FORCE_SCALE", "launch with --scale"),
    (
        "ATERM_TAB_STRIP_ROWS",
        "tab_strip_rows (Settings ▸ Window & Tabs)",
    ),
    (
        "ATERM_NO_DARK_CHROME",
        "window_theme = \"auto\" (Settings ▸ Appearance)",
    ),
    (
        "ATERM_WINDOWING_BEHAVIOR",
        "windowing_behavior in aterm.toml",
    ),
    (
        "ATERM_LATENCY_TRACE",
        "`aterm ctl metrics` (the latency numbers, in every build)",
    ),
    (
        "ATERM_SNAPSHOT_PATH",
        "`aterm ctl image <file>` (SIGUSR1 writes to the control directory)",
    ),
    // The session launch knobs.
    ("ATERM_HEADLESS", "aterm --headless"),
    (
        "ATERM_COLUMNS",
        "aterm --columns <n> (or columns in aterm.toml)",
    ),
    ("ATERM_LINES", "aterm --lines <n> (or lines in aterm.toml)"),
    (
        "ATERM_CONTROL_SOCK",
        "aterm --control-sock <path> for the window, aterm ctl --sock <path> for a client",
    ),
    ("ATERM_NO_CONTROL_SOCK", "aterm --no-control-sock"),
    (
        "ATERM_CONTROL_TOKEN",
        "the token file beside the socket (aterm drive --socket <path>)",
    ),
    ("ATERM_CTL", "the aterm-ctl beside aterm, or on PATH"),
    (
        "ATERM_SHELL",
        "shell in aterm.toml, or aterm --shell <name>",
    ),
    (
        "ATERM_EXEC",
        "aterm -e <command> (with --hold to keep the window)",
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
    ("ATERM_NO_SHELL_INTEGRATION", "aterm --no-shell-integration"),
    ("ATERM_OPERATOR", "[operator] enabled = true"),
    (
        "ATERM_NO_OPERATOR",
        "[operator] enabled = false (the default)",
    ),
    (
        "ATERM_OPERATOR_PROFILE",
        "the default (one operator profile per state root)",
    ),
    ("ATERM_NET_LISTEN", "[net] listen"),
    ("ATERM_NET_CERT", "[net] cert"),
    ("ATERM_NET_KEY", "[net] key"),
    ("ATERM_VERBOSE", "aterm --verbose"),
    // `aterm drive`'s prompt-ready pattern.
    ("ATERM_DRIVE_READY", "aterm drive --ready <regex>"),
];

/// Which [`RETIRED_OPT_OUTS`] are exported to this process (set at all, whatever the
/// value — an exported `=0` is still a line in an rc a person should delete).
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
pub const PUBLISH_RUSTC_GROUP: &str = "tools/atpkg-publish-rustc-group.sh";

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

/// Ask the INSTALLED targo whether it accepts the workspace's `[trust]` policy.
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
            match unknown_field_in(&stderr) {
                Some(field) => WorkspacePolicy::UnknownField { field },
                None if out.status.success() => WorkspacePolicy::Accepted,
                None => WorkspacePolicy::ProbeFailed {
                    why: stderr.lines().next().unwrap_or("targo failed").to_string(),
                },
            }
        }
        Err(e) => WorkspacePolicy::ProbeFailed { why: e.to_string() },
    };
    Some(WorkspacePolicyProbe { workspace, policy })
}

/// Where rustup's `trust` channel points, when that is NOT inside the atpkg store. The
/// rustup home is the one rustup itself resolves — `$RUSTUP_HOME`, else `<home>/.rustup`
/// ([`crate::seam::rustup_home_with`]) — so a relocated home is probed, not skipped.
fn probe_local_seal(layout: &Layout, home: Option<&Path>) -> Option<LocalSealProbe> {
    let link = crate::seam::rustup_home_with(std::env::var_os("RUSTUP_HOME").as_deref(), home)?
        .join("toolchains")
        .join("trust");
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
    /// The verdict, then only what needs attention — each in one short line with its next
    /// step — and the one next act.
    Problems,
    /// The verdict, then every row and its whole explanation.
    Everything,
}

/// Run the health surface, printing the report ([`present`]). Returns `true` iff there
/// were NO structural problems (`main` maps `false` → exit 1). Reads the real environment:
/// home, PATH, the clock, and the `[packages]` config's account, automatic-update switches
/// and exclusions.
#[must_use]
pub fn run(layout: &Layout, prefix: &str, detail: Detail) -> bool {
    let home = aterm_types::dirs::home_dir();
    let path = std::env::var_os("PATH");
    let cfg_account = crate::config::cached().account().map(str::to_string);
    let cfg = crate::config::cached();
    let probes = Probes {
        workspace: std::env::current_dir()
            .ok()
            .and_then(|cwd| probe_workspace_policy(layout, &cwd)),
        local_seal: probe_local_seal(layout, home.as_deref()),
        provenance_attr: crate::provenance::PROVENANCE_XATTR,
        automatic: crate::manager_enabled() && cfg.enabled(),
        excluded: cfg.exclude().to_vec(),
        config_notes: cfg.config_notes(),
        ignored_prefix: cfg.ignored_prefix.clone(),
        retired_env: retired_opt_outs_exported(),
        claude_defaults: home
            .as_deref()
            .and_then(|h| probe_claude_settings(&h.join(".claude").join("settings.json"))),
        stub_env: crate::reroute::StubEnv::of_process(),
        index_head: crate::index_probe::probes_this_source(),
        cc: Some(crate::prereq::probe(path.as_deref())),
    };
    let mut report = Vec::new();
    let mut faults = Vec::new();
    let healthy = run_with(
        layout,
        home.as_deref(),
        path.as_deref(),
        crate::flow::now_unix(),
        cfg_account.as_deref(),
        prefix,
        &probes,
        &mut report,
        &mut faults,
    );
    let shown = present(
        &String::from_utf8_lossy(&report),
        &String::from_utf8_lossy(&faults),
        prefix,
        detail,
    );
    // The verdict first, then the structural faults (on stderr), then the rest. Errors
    // ignored: a reader that closed the pipe (`doctor | head`) costs an `EPIPE` nobody
    // sees, never a panic.
    use std::io::Write as _;
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(shown.verdict.as_bytes());
    let _ = stdout.flush();
    let _ = std::io::stderr().write_all(shown.faults.as_bytes());
    let _ = stdout.write_all(shown.rest.as_bytes());
    let _ = stdout.flush();
    healthy
}

/// The longest line the default report prints — a row, its next step included.
pub const SHORT_ROW: usize = 160;

/// The head of the line a declined store's report carries — said by default too
/// ([`present`]): it is why nothing is installed.
const DECLINED_LINE: &str = "the ALab toolset was removed on this machine";

/// The (8) row for a store no index update pass has completed on, whether or not a failed
/// one left a `status.toml` — followed by the vendor programs that update without one.
const NEVER_CHECKED: &str = "warn — no index update pass has completed yet — run: aterm pkg update";

/// The report as [`present`] lays it out, in the order it is printed: the verdict, the
/// structural faults ([`run_with`]'s stderr lines, which stay on stderr), then the rest.
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

/// THE REPORT AS A PERSON READS IT. [`run_with`]'s full report buries the one actionable
/// warning under dozens of long lines, so the report leads with its VERDICT, and by
/// default ([`Detail::Problems`]) says only what needs attention: each
/// `warn`/`FAIL`/`PROBLEM` row in one line of at most [`SHORT_ROW`]
/// characters — the fact, and its next step when the row names one — then the one next
/// act, and where the rest is. `healthy` is said only when nothing at all was flagged;
/// a report with warnings and no fault is "working", with the count of what to look at.
/// [`Detail::Everything`] prints the verdict and then every line the checks wrote. Pure
/// over the full report's text — `report` its stdout, `faults` its stderr — so the exit
/// code and every check stay [`run_with`]'s.
#[must_use]
pub fn present(report: &str, faults: &str, p: &str, detail: Detail) -> Presented {
    let head = format!("{p}: ");
    let mut problems: Vec<String> = Vec::new();
    let mut body: Vec<&str> = Vec::new();
    let mut verdict: Option<&str> = None;
    let mut next: Option<&str> = None;
    let mut active: Option<&str> = None;
    let mut elided = false;
    // A row as the default report prints it, and whether that dropped anything: a
    // `warn`/`FAIL`/`PROBLEM` row in its short form, any other line cut to fit.
    let shorten = |line: &str| -> (String, bool) {
        let rest = line.strip_prefix(&head).unwrap_or(line);
        let level = ["warn", "FAIL", "PROBLEM"]
            .into_iter()
            .find(|l| rest.starts_with(&format!("{l} \u{2014} ")));
        let row = match level {
            Some(level) => short_row(p, level, &rest[level.len() + " \u{2014} ".len()..]),
            None if line.chars().count() > SHORT_ROW => {
                let mut row: String = line.chars().take(SHORT_ROW - 1).collect();
                row.push('\u{2026}');
                row
            }
            None => line.to_string(),
        };
        let cut = row != line;
        (row, cut)
    };
    for line in report.lines() {
        let rest = line.strip_prefix(&head).unwrap_or(line);
        if rest == "healthy"
            || rest.starts_with("not healthy \u{2014} ")
            || (rest.starts_with("found ") && rest.ends_with(" problem(s)"))
        {
            verdict = Some(rest);
            continue;
        }
        if rest.starts_with("next \u{2014} ") {
            next = Some(line);
            continue;
        }
        body.push(line);
        if let Some(count) = rest.strip_suffix(" program(s) active")
            && count.chars().all(|c| c.is_ascii_digit())
        {
            active = Some(count);
        }
        let flagged_row = ["warn", "FAIL", "PROBLEM"]
            .iter()
            .any(|l| rest.starts_with(&format!("{l} \u{2014} ")));
        // The recorded problems a PROBLEM verdict lists, indented under it — and the
        // one plain fact that explains an empty store: it was removed on purpose.
        if flagged_row || rest.starts_with("  ") || rest.starts_with(DECLINED_LINE) {
            let (row, cut) = shorten(line);
            elided |= cut;
            problems.push(row);
        } else {
            elided = true;
        }
    }
    // The structural faults, one row each, whole under `--verbose`.
    let mut fault_rows: Vec<String> = Vec::new();
    for line in faults.lines() {
        match detail {
            Detail::Everything => fault_rows.push(line.to_string()),
            Detail::Problems => {
                let (row, cut) = shorten(line);
                elided |= cut;
                fault_rows.push(row);
            }
        }
    }
    let flagged = fault_rows.len()
        + problems
            .iter()
            .filter(|r| {
                let rest = r.strip_prefix(&head).unwrap_or(r);
                !rest.starts_with("  ") && !rest.starts_with(DECLINED_LINE)
            })
            .count();
    // Not "ALab program(s)": the count includes the vendor-direct agents ([`run_with`]).
    let programs = active.map_or_else(String::new, |n| format!("{n} program(s) installed"));
    let verdict_line = match verdict {
        Some("healthy") if flagged == 0 => match programs.as_str() {
            "" => format!("{p}: healthy"),
            programs => format!("{p}: healthy \u{2014} {programs} and working"),
        },
        Some("healthy") => match programs.as_str() {
            "" => format!("{p}: working \u{2014} {flagged} thing(s) to look at"),
            programs => {
                format!("{p}: working \u{2014} {programs}; {flagged} thing(s) to look at")
            }
        },
        Some(said) => format!("{p}: {said}"),
        None => format!("{p}: {flagged} thing(s) to look at"),
    };
    let mut rest = String::new();
    let rows: Vec<&str> = match detail {
        Detail::Everything => body,
        Detail::Problems => problems.iter().map(String::as_str).collect(),
    };
    for row in rows {
        rest.push_str(row);
        rest.push('\n');
    }
    if let Some(next) = next {
        rest.push_str(next);
        rest.push('\n');
    }
    if detail == Detail::Problems && elided {
        rest.push_str(&format!("{p}: the full report: aterm pkg {p} --verbose\n"));
    }
    let mut faults = String::new();
    for row in fault_rows {
        faults.push_str(&row);
        faults.push('\n');
    }
    Presented {
        verdict: format!("{verdict_line}\n"),
        faults,
        rest,
    }
}

/// One problem row in at most [`SHORT_ROW`] characters: the row as written when it fits,
/// else its first clause — the fact, up to the first ` — `, `; ` or sentence end — and
/// the next step the row names ([`next_step`]), the fact cut short with a visible `…` if
/// the two still do not fit.
fn short_row(p: &str, level: &str, text: &str) -> String {
    let whole = format!("{p}: {level} \u{2014} {text}");
    if whole.chars().count() <= SHORT_ROW {
        return whole;
    }
    let fact = text[..first_clause_end(text)]
        .trim_end()
        .trim_end_matches('.');
    let lead = format!("{p}: {level} \u{2014} ");
    // The step is what a person acts on, so the fact yields room to it, never the reverse.
    let tail = next_step(text).map_or_else(String::new, |step| format!(" \u{2014} {step}"));
    let room = SHORT_ROW
        .saturating_sub(lead.chars().count() + tail.chars().count())
        .max(20);
    let fact: String = if fact.chars().count() > room {
        let mut cut: String = fact.chars().take(room - 1).collect();
        cut.push('…');
        cut
    } else {
        fact.to_string()
    };
    format!("{lead}{fact}{tail}")
}

/// Where a row's first clause ends: at its first ` — `, `; ` or `. ` outside parentheses
/// and backticks (a parenthetical's `;` is not the end of the fact), else the whole row.
fn first_clause_end(text: &str) -> usize {
    let mut depth = 0usize;
    let mut quoted = false;
    for (at, c) in text.char_indices() {
        match c {
            '`' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth == 0
            && !quoted
            && [" \u{2014} ", "; ", ". "]
                .iter()
                .any(|sep| text[at..].starts_with(sep))
        {
            return at;
        }
    }
    text.len()
}

/// The ONE next step a row names: its `fix:`, `now:` or `or add:` clause (a PATH line to
/// paste), else the first `aterm …` command it quotes in backticks. `None` when the row
/// names none, or only one too long to leave room for the fact beside it.
fn next_step(text: &str) -> Option<String> {
    let clause = |word: &str| {
        text.find(word).map(|at| {
            let rest = &text[at + word.len()..];
            rest.split(" \u{2014} ")
                .next()
                .unwrap_or(rest)
                .split(" (")
                .next()
                .unwrap_or(rest)
                .trim()
                .trim_end_matches(['.', ';'])
                .to_string()
        })
    };
    let step = clause("fix: ")
        .or_else(|| clause("Fix: "))
        .map(|fix| format!("fix: {fix}"))
        .or_else(|| clause("now: ").map(|now| format!("run {now}")))
        .or_else(|| clause("or add: ").map(|line| format!("add: {line}")))
        .or_else(|| {
            let at = text.find("`aterm ")?;
            let cmd = &text[at + 1..];
            let end = cmd.find('`')?;
            Some(format!("run `{}`", &cmd[..end]))
        })?;
    (step.chars().count() <= 110 && !step.is_empty()).then_some(step)
}

/// The testable core: `home`, the `PATH` value, `now` and the `[packages].account`
/// config override are injected so the surface can be exercised against a synthetic
/// environment without mutating the process env.
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
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> bool {
    // THE SPEAKER IS THE VERB THE USER TYPED (`status` is an alias for this report), so a
    // script keying on the prefix keys on the verb it ran.
    let p = prefix;
    let mut fails = 0usize;

    // (0) WHICH atpkg IS SPEAKING. A machine can carry several (the app's, a dev `dist/`,
    // a `target/release`), and a stale one misreports the store — whose natural next step
    // is reinstalling something that was never broken.
    let _ = writeln!(
        out,
        "{p}: this atpkg is {} at {}",
        env!("CARGO_PKG_VERSION"),
        std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown path>".to_string())
    );
    let app_dirs: Vec<PathBuf> = std::iter::once(PathBuf::from("/Applications"))
        .chain(home.map(|h| h.join("Applications")))
        .collect();
    report_aterm_posture(
        layout,
        &app_dirs,
        aterm_update_core::settings::update_auto_apply(),
        p,
        out,
    );

    // (1) TRUST ROOT + INDEX SOURCE.
    let _ = writeln!(
        out,
        "{p}: index source github.com/{}",
        crate::resolve_account(cfg_account).slug()
    );
    if crate::manager_enabled() {
        // The root is the PAPER MASTER — the same anchor the app updater uses, not a
        // package-specific key. Naming it here is what lets an operator answer "which
        // trust root is this build on?" without reading source.
        let _ = writeln!(
            out,
            "{p}: ok — paper master pinned (fingerprint {}, {} key(s))",
            crate::root_key_fingerprint(),
            crate::PKG_TRUST_ANCHORS.len()
        );
    } else {
        let _ = writeln!(
            out,
            "{p}: warn — disabled/inert (no paper master compiled in: \
             pins::PAPER_MASTER_PUBKEYS is empty) — this build installs nothing"
        );
    }
    // The table's own spelling: a retired key to rename or remove. Notes, never failures —
    // every one of them is still honoured or harmlessly ignored.
    for note in &probes.config_notes {
        let _ = writeln!(out, "{p}: note — {note}");
    }
    if let Some(ignored) = probes.ignored_prefix.as_deref() {
        let _ = writeln!(
            out,
            "{p}: warn — {}",
            crate::config::ignored_prefix_note(ignored)
        );
    }
    for (name, setting) in &probes.retired_env {
        let _ = writeln!(
            out,
            "{p}: warn — ${name} is exported but does nothing — delete it and use {setting}"
        );
    }

    // (2) PREFIX / STORE.
    if layout.prefix.is_dir() {
        let _ = writeln!(out, "{p}: ok — prefix {}", layout.prefix.display());
    } else {
        let _ = writeln!(
            out,
            "{p}: warn — prefix {} does not exist yet (nothing installed)",
            layout.prefix.display()
        );
    }

    // (3) PATH WIRING.
    let bin_dir = layout.bin_dir();
    let on_path = path_var
        .map(|p| std::env::split_paths(p).any(|d| d == bin_dir))
        .unwrap_or(false);
    if on_path {
        let _ = writeln!(out, "{p}: ok — managed bin/ is on PATH");
    } else {
        // The path appears once, in the pasteable export. An aterm shell, and any rc file
        // carrying atpkg's block, sources ~/.aterm/shell.d, which appends it.
        let _ = writeln!(
            out,
            "{p}: warn — managed bin/ is not on PATH here; a new aterm shell adds it, or add: {}",
            manual_path_hint(&bin_dir)
        );
    }

    // (3b) REROUTE (philosophy §4, `crate::reroute`): per row, the stub's state; then
    // ORDER, not presence: a `~/.cargo/bin` ahead of everything managed makes a bare
    // `cargo` in a session run upstream Rust silently. Everything here is
    // advisory (warn, exit 0): outside an aterm session the reroute dir is not on PATH
    // BY DESIGN (the prepend is session-scoped, never machine-wide), and a missing stub
    // is re-laid at the next spawn. Doctor names; it never lays.
    let reroute_dir = layout.reroute_dir();
    // …and every agent program whose `agents/` twin stands: its stub is what
    // routes `claude` to the managed copy at exec time in a shell whose PATH predates it.
    let agent_rows = crate::reroute::agents_routes(layout)
        .into_iter()
        .map(|name| (name, crate::reroute::stub_state(layout, name)));
    for (name, state) in crate::reroute::states(layout).into_iter().chain(agent_rows) {
        match state {
            crate::reroute::StubState::Laid => {
                let _ = writeln!(out, "{p}: ok — reroute stub {name} laid");
            }
            crate::reroute::StubState::Missing if cfg!(windows) => {
                let _ = writeln!(
                    out,
                    "{p}: note — reroute stub {name} not laid: Windows lays no reroute stubs yet (TARGET)"
                );
            }
            // A recorded decline lays none — nothing will, so it is a state, not a warning.
            crate::reroute::StubState::Missing if layout.declined().is_file() => {
                let _ = writeln!(
                    out,
                    "{p}: note — reroute stub {name} not laid: {DECLINED_LINE}"
                );
            }
            crate::reroute::StubState::Missing => {
                let _ = writeln!(
                    out,
                    "{p}: warn — reroute stub {name} missing; the next aterm session lays it \
                     — now: aterm pkg repair"
                );
            }
            crate::reroute::StubState::Foreign => {
                let _ = writeln!(
                    out,
                    "{p}: warn — {} is not ours (foreign file; never touched)",
                    crate::reroute::stub_path(layout, name).display()
                );
            }
        }
    }
    let path_entries: Vec<std::path::PathBuf> = path_var
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    let reroute_at = path_entries.iter().position(|d| *d == reroute_dir);
    let upstream_at = upstream_index_on_path(&path_entries, &layout.prefix, "cargo");
    match (reroute_at, upstream_at) {
        (None, _) => {
            let _ = writeln!(
                out,
                "{p}: note — reroute dir not on this PATH (expected outside an aterm session)"
            );
        }
        (Some(i), Some(j)) if i < j => {
            let _ = writeln!(
                out,
                "{p}: ok — reroute dir precedes upstream cargo (PATH[{i}] < PATH[{j}])"
            );
        }
        (Some(i), Some(j)) => {
            let _ = writeln!(
                out,
                "{p}: warn — upstream cargo (PATH[{j}]) is ahead of the reroute dir \
                 (PATH[{i}]) in this shell; a new aterm tab puts the reroute dir first"
            );
        }
        (Some(i), None) => {
            let _ = writeln!(
                out,
                "{p}: ok — reroute dir on PATH (PATH[{i}]); no upstream cargo on this PATH"
            );
        }
    }

    // (4) BROKEN SHIM SCAN of bin/ — a shim whose forward target is GONE (a dangling
    // symlink on Unix; on Windows a `.cmd` forwarding to a missing exe, which no symlink
    // scan could ever catch). `resolve_shim` reads the target cross-platform; a tombstone
    // (deliberately target-less) yields `None` and is never flagged.
    //
    // A SHIM WE COULD NOT STAT IS NOT A BROKEN SHIM: an unreadable target (EACCES on a
    // parent, privacy consent's EPERM, EIO) is not evidence of a fault, and
    // `tools/install.sh` hands this exit code to failed-seed installers as the diagnostic
    // to trust. Nor is it swallowed: it is a warn that names the cause.
    if let Ok(entries) = std::fs::read_dir(&bin_dir) {
        for e in entries.flatten() {
            let shim = e.path();
            let Some(target) = crate::platform::resolve_shim(&shim) else {
                continue;
            };
            match crate::store::presence(&target) {
                crate::store::Presence::Present => {}
                crate::store::Presence::Absent => {
                    fails += 1;
                    let _ = writeln!(err, "{p}: FAIL — broken bin shim {}", shim.display());
                }
                crate::store::Presence::Unknown(why) => {
                    let _ = writeln!(
                        out,
                        "{p}: warn — bin shim {} could not be checked: its target {} is \
                         unreadable ({why}) — not a broken shim",
                        shim.display(),
                        target.display()
                    );
                }
            }
        }
    }

    // (5) ACTIVE-BUILD STORE INTEGRITY.
    let active = crate::ops::active_builds(layout);
    // The first program whose store tree is missing/incomplete — remembered so the
    // verdict tail can name ONE structural repair (`install <program>`) instead of a menu.
    let mut next_install_program: Option<String> = None;
    for (program, build) in &active {
        let bd = layout.build_dir(program, *build);
        if !bd.is_dir() || !crate::store::build_is_complete(&bd) {
            fails += 1;
            let _ = writeln!(
                err,
                "{p}: FAIL — active {} store missing/incomplete",
                crate::vendor_direct::display_build(program, *build)
            );
            if next_install_program.is_none() {
                next_install_program = Some(program.clone());
            }
        }
    }
    // No count here: (11) says how many programs are active, from the tally that counts a
    // dev-linked program too (this loop's own count said 0 beside (11)'s 1 for one).

    // (5a) INSTALLED FILES THAT STILL CARRY `com.apple.provenance` (macOS).
    //
    // A file a provenance-tracked process writes carries the tag, and a tagged executable,
    // shim or dylib tags what it writes in turn — every object file and proof snapshot a
    // release cut builds with it, which tools/proof_snapshot.py refuses AFTER the ledger
    // claim (v0.83.0 burned a build number on `trust/8590`). Every package pass and
    // `aterm pkg repair` clear it ([`crate::provenance::heal_store`]); this reads what the
    // clearing left, over the same roots, and says it in ONE line — nothing when clean.
    //
    // A WARNING, never a PROBLEM: every tool still runs — only a release cut cannot use
    // it — and doctor clears nothing itself: it reports what is on disk.
    if cfg!(target_os = "macos") {
        let scan = crate::provenance::scan_roots(
            &crate::provenance::store_roots(layout),
            probes.provenance_attr,
        );
        if !scan.carriers.is_empty() {
            let _ = writeln!(out, "{p}: {}", provenance_line(&scan));
        }
    }

    // (5c) SOLVERS THE `trust` BUNDLE PINS PRIVATELY.
    //
    // trustc resolves its SMT solver as a SIBLING of its own executable — see
    // `sibling_solver_candidate` / `resolve_ay_solver_identity_from_candidates`
    // in trust's `compiler/rustc_mir_transform/src/trust_verify.rs`: an explicit
    // `AY_PATH` wins, otherwise the copy inside the bundle's own `bin/`, and
    // there is NO PATH fallback at all.
    //
    // The pin itself is deliberate and should not be "fixed" away: trust
    // snapshots and fingerprints the solver binary so a proof records which
    // solver produced it, and a solver that floated with whatever atpkg last
    // installed would make proof identity float with it.
    //
    // But a managed `ay` newer than the bundle's is not the one Trust uses, and nothing
    // else says so: reported, never failed.
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
            // Equal versions are the healthy case. An unanswered probe on
            // either side is NOT evidence of divergence, so it stays silent
            // rather than reporting "pinned unknown vs managed 0.13.0" — a
            // line that reads like a finding and carries none.
            if pinned_version == managed_version
                || pinned_version == "unknown"
                || managed_version == "unknown"
            {
                continue;
            }
            // No `AY_PATH` hint: it could point at an older solver than the bundle's.
            let _ = writeln!(
                out,
                "{p}: note — Trust builds use the {program} pinned inside the trust bundle \
                 ({pinned_version}), not the managed {program} {managed_version} \
                 (build {managed_build})"
            );
        }
    }

    // (5f) tippy's compiler is a plain file, and the rustup view is a clone of the build.
    //
    // Current bundles ship Trust's names only, and the stock names live in atpkg's rustup
    // view ([`crate::seam::refresh_view`]) as copy-on-write clones of the Trust tools.
    // Older bundles (8571, 8589, 8590, 8595) ship `bin/rustc` as a separately signed copy
    // of `trustc` — an ad-hoc signature embeds the file name, so the bytes differ — and
    // their tippy (before trust `e79c1142a5`) refuses it. The store is never modified;
    // atpkg lays a per-build exec root — a clone of the build where `rustc` holds
    // `trustc`'s bytes — and routes the trust shims through it ([`crate::compat`]). This
    // check reads file facts (`lstat`, byte compares), never a run of tippy, whose
    // ancestor-directory checks refuse intermittently and would make the verdict flap.
    //
    // (a) `trustc` must be a plain file: tippy (and tippy-driver) refuse a symbolic
    //     link for the selected compiler, measured. It withholds "healthy" while tippy
    //     is in the bundle to be refused.
    // (b) For every recorded seam, each stock name rustup resolves must be a clone of the
    //     store's Trust tool holding its bytes — or `cargo +trust` / `rustc +trust` run
    //     something other than the managed toolchain. A view laid as hard links before
    //     clones is not one. A mismatch is a warn that withholds "healthy", and
    //     `aterm pkg repair` rebuilds the view.
    // (c) When the active build [`crate::compat::needs_root`] (`rustc` neither `trustc`'s
    //     inode nor its bytes, beside a tippy): an exec root that is a clone of the build
    //     at `Deep` depth, with every trust shim in `bin/` carrying the guard through it,
    //     is a NOTE and the report stays healthy. No root, a root that differs from the
    //     build, or a shim that does not route is a warn that withholds "healthy" — both
    //     inode numbers in the line, so a reader can `stat` the claim — with the one fix,
    //     `aterm pkg repair`, which lays the root at `Deep` and re-renders every shim.
    //     Behind a link or a file at `compat` or `compat/trust` repair refuses on every
    //     run, so that warn names the path and says to remove it first.
    // (d) A root under `<prefix>/compat/trust` no build needs — its build reclaimed (its
    //     clones keep the reclaimed blocks allocated), its build needing none, or not a
    //     directory at all — is an advisory warn naming `aterm pkg gc`, whose closing
    //     sweep removes exactly these ([`crate::compat::strays`] is that sweep's own
    //     classification). It leaves a healthy report healthy: no tool fails over it.
    // (e) A build that needs no root — every Trust-names-only bundle, a pack whose `rustc`
    //     is a hard link of `trustc`, a byte-identical copy — gets no (c) line at all.
    // (f) A build whose `bin/rustc` or `bin/trustc` cannot be READ is not proof of either:
    //     a warn that withholds "healthy", naming the failed read, because nothing here can
    //     vouch for PATH tippy — and the passes leave any root standing for it alone.
    //
    // A warn that says "cannot run" counts in `tools_cannot_run`; one that says "may not
    // run" or "cannot tell" counts in `tools_unproven`, so the closing line never claims a
    // tool cannot run where the line it summarises only failed to show that it can.
    let mut tools_cannot_run = 0usize;
    let mut tools_unproven = 0usize;
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
            tools_cannot_run += 1;
            let _ = writeln!(
                out,
                "{p}: warn — trust build {trust_build}: tippy cannot run — {} is {what}, and \
                 tippy requires the selected compiler trustc to be a plain file, so every tippy \
                 run stops at a setup error (the bundle's fix: {TRUST_COMPILER_FIX})",
                trustc.display()
            );
        }
        view_stock_names_check(out, p, layout, Some(bin.clone()), &mut tools_cannot_run);
        match crate::compat::inspect(layout, trust_build) {
            Some(Ok(inspection)) => {
                let (line, claim) = exec_root_line(p, trust_build, &inspection);
                match claim {
                    ToolClaim::Runs => {}
                    ToolClaim::CannotRun => tools_cannot_run += 1,
                    ToolClaim::Unproven => tools_unproven += 1,
                }
                let _ = writeln!(out, "{line}");
            }
            Some(Err(why)) => {
                tools_unproven += 1;
                let _ = writeln!(out, "{}", exec_root_unread_line(p, trust_build, &why));
            }
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
        view_stock_names_check(out, p, layout, store_bin, &mut tools_cannot_run);
    }
    for (path, why) in crate::compat::strays(layout) {
        let _ = writeln!(out, "{}", stray_root_line(p, &path, why));
    }

    // (5d) THE WORKSPACE THE OPERATOR IS STANDING IN vs THE INSTALLED TRUST.
    //
    // atpkg keeps the toolchain current with what is PUBLISHED; it cannot keep a
    // repository from depending on a Trust build that was never published. When that
    // happens the symptom is targo refusing the workspace with an "unknown field"
    // error that looks like a manifest typo. The installed targo decides the verdict
    // here, and the line says what is actually behind: the toolchain, on the
    // publisher's side, not this machine.
    let mut next_publish = false;
    if let Some(ws) = &probes.workspace {
        let trust_build = active
            .iter()
            .find(|(p, _)| p.as_str() == "trust")
            .map(|(_, b)| b.to_string())
            .unwrap_or_else(|| "?".to_string());
        match &ws.policy {
            WorkspacePolicy::Accepted => {
                let _ = writeln!(
                    out,
                    "{p}: ok — workspace {} declares a [trust] policy the installed trust \
                     build {trust_build} accepts",
                    ws.workspace.display()
                );
            }
            WorkspacePolicy::UnknownField { field } => {
                fails += 1;
                next_publish = true;
                let _ = writeln!(
                    err,
                    "{p}: FAIL — workspace {} declares a [trust] policy key the installed \
                     trust build {trust_build} does not know: `{field}`. Whoever committed \
                     it built on a NEWER Trust seal that atpkg has not published, so this \
                     workspace does not open with targo on any atpkg-managed machine. Cure \
                     (a rostered publisher, on the machine holding that seal): \
                     {PUBLISH_RUSTC_GROUP}; then here: aterm pkg update trust",
                    ws.workspace.display()
                );
            }
            WorkspacePolicy::ProbeFailed { why } => {
                let _ = writeln!(
                    out,
                    "{p}: warn — could not ask the installed targo about workspace {} \
                     ({why})",
                    ws.workspace.display()
                );
            }
        }
    }

    // (5e) A LOCAL TOOLCHAIN BEHIND rustup's `trust` CHANNEL. Newer than the store's (or
    // of unknown age), it is the user's choice to keep: the line says which compiler
    // `cargo +trust` gets and the one re-point that hands it back to the managed one —
    // the only line about this entry, since (9) stays silent on it. Older,
    // or gone, it is stale, and `repair` re-points the channel at the store — as the next
    // unattended pass does by itself for an older LIVE BUILD TREE nobody put back
    // ([`crate::seam::live_build_tree`]); `repair` stays the fix named, because it is the
    // one that acts now.
    if let Some(seal) = &probes.local_seal {
        let _ = match &seal.stale {
            Some(crate::seam::Stale::Dangling) => writeln!(
                out,
                "{p}: warn — rustup's trust channel names {}, which no longer exists, so \
                 `cargo +trust` and every repo pinning `channel = \"trust\"` fail with \
                 `'rustc' is not installed for the custom toolchain 'trust'`; fix: `aterm pkg \
                 repair` re-points it at the store",
                seal.link_target.display()
            ),
            Some(crate::seam::Stale::Older { its, store }) => writeln!(
                out,
                "{p}: warn — rustup's trust channel resolves to a LOCAL toolchain OLDER than \
                 the atpkg store's: {} (trustc {}, {}; the store's is {}), so `cargo +trust` \
                 and every repo pinning `channel = \"trust\"` compile with a stale Trust; fix: \
                 `aterm pkg repair` re-points it at the store (the old toolchain is left \
                 where it is)",
                seal.link_target.display(),
                seal.trustc,
                its,
                store
            ),
            None => {
                // `None` also covers no store build and a dev-linked trust, where the
                // target may be gone and there is no managed toolchain to point at.
                let what = if seal.link_target.exists() {
                    format!(
                        "names a local toolchain, not the managed one: {} (trustc {}); \
                         `cargo +trust` and projects pinning `channel = \"trust\"` build with it",
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
                let fix = if layout.program_current("trust").join("bin").is_dir() {
                    format!(
                        " — to use the managed one: {}",
                        repoint_fix(layout, &seal.link)
                    )
                } else {
                    String::new()
                };
                writeln!(out, "{p}: warn — rustup `trust` {what}{fix}")
            }
        };
    }

    // THE TOOLCHAIN UPDATE THAT WAITS, AND THE RUNS HOLDING BUILDS: states, never
    // failures. An unattended pass that found the Trust toolchain in use staged the
    // new one and holds its flip for quiet ([`crate::quiet`]); a merge-contract run or a
    // cut holds a lease on the build it resolved ([`crate::lease`]), which gc keeps. Each
    // line says what, on whom, until when — and the one command that ends the wait.
    for line in hold_lines(layout, now) {
        let _ = writeln!(out, "{p}: {line}");
    }

    // (5b) LIVE-BUILD WITNESS. `gc` reclaims a program's superseded builds only when the
    // authoritative `store/<program>/current` symlink and the derived `bin/` shim view name
    // the SAME build; where they don't it abstains rather than guess (guessing is what
    // deleted live trees), and the program accumulates builds silently — so this makes it
    // visible. A genuine disagreement is STRUCTURAL: some tool on PATH is running a build
    // activation does not select. A merely-absent witness is not breakage, so it warns.
    let live = crate::gc::live_builds(layout);
    // A shim/link divergence's repair is a re-run of `update` — remembered for the
    // verdict tail's single `next` act.
    let mut next_update_divergence = false;
    for d in live.diverged() {
        match &d.reason {
            crate::gc::Diverged::LinkShimMismatch {
                link_says,
                shims_say,
            } => {
                fails += 1;
                next_update_divergence = true;
                let _ = writeln!(
                    err,
                    "{p}: FAIL — {}: its `current` link selects {} but its bin/ shims run {} — fix: \
                     aterm pkg update {}",
                    d.program,
                    crate::vendor_direct::build_words(*link_says),
                    crate::vendor_direct::build_words(*shims_say),
                    d.program
                );
            }
            crate::gc::Diverged::ShimsDisagree { builds } => {
                fails += 1;
                next_update_divergence = true;
                let _ = writeln!(
                    err,
                    "{p}: FAIL — {}: its bin/ shims are split across builds {} — fix: aterm \
                     pkg repair, then aterm pkg update {}",
                    d.program,
                    build_list(builds),
                    d.program
                );
            }
            crate::gc::Diverged::NoLiveWitness { shims_say } => {
                let _ = writeln!(
                    out,
                    "{p}: warn — {}: {} runs but is not recorded as current, so its older \
                     builds are never removed — fix: aterm pkg update {}",
                    d.program,
                    crate::vendor_direct::build_words(*shims_say),
                    d.program
                );
            }
        }
    }
    let _ = writeln!(
        out,
        "{p}: ok — {} program(s) with a proven live build",
        live.len()
    );

    // (6) SHELL HOOKS + FISH-SAFETY.
    if let Some(home) = home {
        let aterm = home.join(".aterm");
        let shell_d = aterm.join("shell.d");
        // Probe the dialect the interactive shell on THIS platform actually sources: `.ps1`
        // (PowerShell) on Windows, `.zsh` elsewhere. An install writes the whole set, so a
        // present platform-native hook means PATH wiring is in place.
        let native_hook = format!("{}.{}", crate::hooks::HOOK_BASENAME, native_hook_ext());
        if shell_d.join(&native_hook).is_file() {
            let _ = writeln!(out, "{p}: ok — shell.d hooks present");
        } else {
            let _ = writeln!(
                out,
                "{p}: warn — shell.d hooks not generated yet (an install writes them)"
            );
        }
        if let Ok(entries) = std::fs::read_dir(&shell_d) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().ends_with(".sh") {
                    fails += 1;
                    let _ = writeln!(
                        err,
                        "{p}: FAIL — shell.d/{}: a POSIX .sh breaks fish — remove it",
                        e.file_name().to_string_lossy()
                    );
                }
            }
        }
        // (6b) rc wiring — which shells outside aterm reach the managed `bin/`, and for the
        // ones that do not, why. One line per rc file that exists (atpkg never creates one).
        // Without it the three states a user can be in — wired, opted out, and skipped at
        // the consent fence — are indistinguishable from outside.
        //
        // Doctor must not invent a fault from evidence it did not measure. `rc_wiring` reads
        // each file's content for a `/.aterm/shell.d/` line, and also the two bash login
        // profiles install.sh can elect that atpkg has no row for
        // (`hooks::INSTALL_SH_PROFILES`), so a Mac wired by install.sh is not called
        // unwired. It still reads six named files, not every file a shell can start from, so
        // when none of them sources shell.d the note names the six (`hooks::rc_files_read`)
        // instead of calling the machine aterm-only.
        #[cfg(unix)]
        {
            let wiring = crate::hooks::rc_wiring(home);
            let names = |want: crate::hooks::RcState| -> Vec<String> {
                wiring
                    .iter()
                    .filter(|(_, s)| *s == want)
                    .map(|(rc, _)| format!("~/{rc}"))
                    .collect()
            };
            let wired = names(crate::hooks::RcState::Wired);
            let elsewhere = names(crate::hooks::RcState::SourcedElsewhere);
            if !wired.is_empty() {
                let _ = writeln!(
                    out,
                    "{p}: ok — {} sources ~/.aterm/shell.d (atpkg's block; delete it to opt \
                     out — `aterm pkg repair` puts it back)",
                    wired.join(", ")
                );
            }
            if !elsewhere.is_empty() {
                let _ = writeln!(
                    out,
                    "{p}: ok — {} sources ~/.aterm/shell.d via a line that is not atpkg's \
                     block (tools/install.sh's, or your own)",
                    elsewhere.join(", ")
                );
            }
            for (rc, state) in &wiring {
                match state {
                    crate::hooks::RcState::Wired | crate::hooks::RcState::SourcedElsewhere => {}
                    crate::hooks::RcState::OptedOut => {
                        let _ = writeln!(
                            out,
                            "{p}: note — ~/{rc}: atpkg's block was deleted — opted out, \
                             left alone by every install and update pass (`aterm pkg \
                             repair` lays it again)"
                        );
                    }
                    crate::hooks::RcState::ConsentFenced => {
                        let _ = writeln!(
                            out,
                            "{p}: note — ~/{rc} resolves under a folder macOS guards with \
                             a consent dialog, so no atpkg pass opens it — it is left \
                             unwired; source ~/.aterm/shell.d from it yourself, or reach \
                             the tools with `aterm <tool>`"
                        );
                    }
                    crate::hooks::RcState::Unwired => {
                        let _ = writeln!(
                            out,
                            "{p}: warn — ~/{rc} does not source ~/.aterm/shell.d (an \
                             install pass or `aterm pkg repair` appends atpkg's block)"
                        );
                    }
                }
            }
            if wired.is_empty() && elsewhere.is_empty() {
                let checked: Vec<String> = crate::hooks::rc_files_read()
                    .map(|rc| format!("~/{rc}"))
                    .collect();
                let _ = writeln!(
                    out,
                    "{p}: note — none of {} sources ~/.aterm/shell.d, so a shell outside \
                     aterm that starts from only those files does not reach the managed \
                     tools; use `aterm <tool>` there, or: {}",
                    checked.join(", "),
                    manual_path_hint(&bin_dir)
                );
            }
        }
        // (6c) A file at a command path: a copy never updates, and either way it runs
        // instead of the installed aterm wherever `~/.local/bin` leads PATH. What replaces
        // it differs by platform (`hooks::copied_command_remedy`): on macOS the installed
        // app's own `atpkg repair` — `aterm` may be the very copy named here.
        #[cfg(unix)]
        for path in crate::hooks::copied_command_links(home) {
            let _ = writeln!(
                out,
                "{p}: warn — {} is a file, not a link to the installed aterm, and it runs \
                 instead of it (a copy that never updates, or a build of your own) — fix: {}",
                path.display(),
                crate::hooks::copied_command_remedy(&path)
            );
        }
        // Privacy of the login-sourced dirs (READ-ONLY — doctor never chmods).
        for dir in [&aterm, &shell_d] {
            if let Ok(m) = std::fs::symlink_metadata(dir)
                && m.file_type().is_dir()
                && !crate::platform::dir_meta_is_private(&m)
            {
                fails += 1;
                let _ = writeln!(
                    err,
                    "{p}: FAIL — {} is group/other-writable (login shells source it)",
                    dir.display()
                );
            }
        }
    }

    // (7) DISK HEADROOM.
    match crate::freespace::available_bytes(&layout.prefix) {
        Some(free) if free < 5 * GIB => {
            let _ = writeln!(
                out,
                "{p}: warn — only {} free (a toolchain update needs ~2.5x its artifact size)",
                crate::cost::human_bytes(free)
            );
        }
        Some(free) => {
            let _ = writeln!(out, "{p}: ok — {} free", crate::cost::human_bytes(free));
        }
        None => {
            let _ = writeln!(out, "{p}: warn — could not query free space");
        }
    }

    // (7b) SPOTLIGHT EXPOSURE OF RUST BUILD OUTPUT. `mds` indexing build output is one of
    // the two amplifiers that let a busy build stall WindowServer into a watchdog kill
    // (`crate::noindex` carries the measurement), and the one a per-directory rename removes.
    //
    // A WARNING, never a PROBLEM: nothing here is structurally broken, the scan is a
    // NAME-based heuristic over a depth-limited walk, and doctor must not invent a fault
    // from evidence it did not measure (`is_problem_state`'s allow-by-prefix rule, above).
    // And doctor NEVER probes: measuring exclusion means writing a probe file into the
    // user's tree, and doctor does not mutate. It reports the exposure and names the verb
    // that measures it.
    //
    // The depth under $HOME is `noindex::DOCTOR_DEPTH`'s to state (its note has the
    // measurement). Dot-directories are pruned, and that is sound rather than merely
    // cheap: their whole subtree is already excluded (measured). So are the six folders
    // macOS guards with a consent dialog and `Library` (`noindex::SKIP_DIRS`) — a walk
    // that raised aterm's name on a privacy modal at the
    // top of an unattended pass is the one cost this scan may never impose, which is why
    // the line below says which places were not walked. Anything deeper than the constant
    // is the deliberate `aterm pkg noindex scan <root>`, which the line below names.
    //
    // No `cfg` here by design: `noindex::scan` returns an empty, `complete` scan on
    // non-macOS, so the whole block prints nothing there — the same discipline as
    // `store.rs`'s unconditional `platform::exclude_from_backup`.
    if let Some(home) = home {
        let found = crate::noindex::scan(
            home,
            crate::noindex::DOCTOR_DEPTH,
            &crate::noindex::Budget::DOCTOR,
        );
        let exposed: Vec<&crate::noindex::Target> = found.exposed().collect();
        if exposed.is_empty() && !found.targets.is_empty() {
            // The provenance caveat every surface carries: `exclusion_of` reads the NAME,
            // and `.noindex` is observed behaviour, not a documented Apple API — and doctor,
            // which never probes, is the last surface that should sound sure.
            let _ = writeln!(
                out,
                "{p}: ok — {} cargo target dir(s) under {} are hidden from Spotlight — that \
                 is what the NAMES say; `aterm pkg noindex verify <dir>` measures it",
                found.targets.len(),
                home.display()
            );
        } else if !exposed.is_empty() {
            let size = crate::noindex::size_of_all(&exposed, &crate::noindex::Budget::DOCTOR_SIZE);
            // WHICH of them the unattended pass can reach. The pass renames only a
            // target dir beside a Cargo.toml (`noindex::apply_under`, `require_repo`): a
            // FREE-STANDING one (`$HOME/trust/build/bootstrap`) is pointed at by an env var or
            // a build system the pass cannot re-point, so every pass skips it. Say which
            // are which, and what moves the free ones — a count that never falls otherwise
            // reads as a pass that is not doing its job.
            let (in_repo, free): (Vec<&crate::noindex::Target>, Vec<&crate::noindex::Target>) =
                exposed
                    .iter()
                    .copied()
                    .partition(|t| crate::noindex::repo_of(&t.path).is_some());
            let reach = spotlight_reach(in_repo.len(), &free);
            let _ = writeln!(
                out,
                // State, reach, the action, then the list/measure verbs, then why.
                "{p}: warn — {at_least}{} of {} cargo target dir(s) under {} are indexed by \
                 Spotlight ({}); {reach}{now}; `aterm pkg noindex` lists them, `aterm pkg \
                 noindex verify <dir>` measures one (indexing build output can stall \
                 WindowServer; [machine] spotlight_noindex, default on)",
                exposed.len(),
                found.targets.len(),
                home.display(),
                crate::noindex::human_size(&size),
                // THE REMEDY ONLY WHEN IT REACHES SOMETHING: `--all` walks with
                // `require_repo`, so with every dir free-standing it is a no-op.
                // A budget-stopped scan counted only what it reached, and the default
                // report keeps only this row's first clause, so the count says "at least".
                at_least = if found.complete { "" } else { "at least " },
                now = if in_repo.is_empty() {
                    String::new()
                } else {
                    " — now: `aterm pkg machine apply` (or `aterm pkg noindex apply --all`, \
                     which walks with the verb's larger budget)"
                        .to_string()
                }
            );
        }
        if !found.complete {
            let _ = writeln!(
                out,
                "{p}: note — the Spotlight-exposure scan did not cover all of {}; \
                 `aterm pkg noindex scan <root>` covers a tree it did not reach",
                home.display()
            );
        }
    }

    // (7b) UNIVERSAL CONTROL (macOS): the other machine setting the pass applies per
    // this report (R5). Read through `defaults -currentHost`, never written here; the
    // line carries the revert when it is off and the opt-out when it is about to be.
    if cfg!(target_os = "macos") {
        let state = crate::machine::universal_control_state(&crate::machine::SystemDefaults);
        let policy = crate::config::cached_machine().universal_control();
        let _ = writeln!(out, "{p}: {}", state.doctor_line(policy));
    }

    // (8) INDEX FREEZE / AGE (no unverified parse — atpkg's OWN diagnostics only).
    // The clock is `last_success_at`, stamped by a pass that resolved the index and RAN
    // TO ITS END — clean, or with member failures each recorded in its own row. It is
    // never `updated_at`, which every writer moves (a failing pass included). The line
    // says "completed pass", not "successful update", because that is what the stamp
    // measures; the member rows below say what failed.
    // "NEVER CHECKED" IS SAID HERE AND NOWHERE ELSE, and only what
    // is true: an index pass moves the index-pinned programs; the vendor programs this
    // machine keeps at their vendors' heads update without one (design §1).
    let vendors = vendor_update_clause(layout, probes);
    if let Some(status) = crate::status::read(layout) {
        if status.last_success_at.trim().is_empty() {
            let _ = writeln!(out, "{p}: {NEVER_CHECKED}{vendors}");
        } else {
            match index_age_days(&status.last_success_at, now) {
                Some(days) if days > 30 => {
                    let _ = writeln!(
                        out,
                        "{p}: warn — {days} day(s) since the last completed update pass ({}) — \
                         this machine has been off, or the app has not run",
                        status.last_success_at
                    );
                }
                Some(days) => {
                    let _ = writeln!(
                        out,
                        "{p}: ok — {days} day(s) since the last completed update pass"
                    );
                }
                None => {
                    let _ = writeln!(out, "{p}: warn — could not parse the last-success time");
                }
            }
            // HAS THE INDEX BEEN REACHED LATELY: the last success is the last pass that reached
            // the signed index, and a pass that recorded itself failed or offline after it
            // (`last_pass` / `last_pass_at`) says every pass since ran on the cached index —
            // read by the schedulers' own reader (`Stamps::last_failed`), so doctor and the
            // failure ladder can never disagree about which pass failed.
            let reached_days = index_age_days(&status.last_success_at, now);
            let unreached =
                aterm_update_core::pkg_check::Stamps::parse(&status.to_toml().unwrap_or_default())
                    .last_failed();
            match reached_days {
                Some(days) if unreached && days > INDEX_UNREACHED_WARN_DAYS => {
                    let _ = match unreached_cause(&status.outcome) {
                        Some(why) => writeln!(
                            out,
                            "{p}: warn — the signed index has not been reached for {days} \
                             day(s): {why} — now: aterm pkg update"
                        ),
                        None => writeln!(
                            out,
                            "{p}: warn — the signed index has not been reached for {days} \
                             day(s) — `aterm pkg update` says why"
                        ),
                    };
                }
                _ => {}
            }
            if !status.index_build_changed_at.is_empty()
                && let Some(days) = index_age_days(&status.index_build_changed_at, now)
                && days > INDEX_FROZEN_WARN_DAYS
                && reached_days.is_some_and(|d| d <= INDEX_UNREACHED_WARN_DAYS)
            {
                let _ = writeln!(
                    out,
                    "{p}: warn — index build {} has not changed in {days} day(s) while the \
                     index was reachable — publishing looks frozen upstream",
                    status.last_index_build
                );
            }
        }
    } else {
        let _ = writeln!(out, "{p}: {NEVER_CHECKED}{vendors}");
    }
    // The build floor is printed WITH the generation that recorded it, because that pair
    // is the actual gate: a floor stamped with an older generation is re-based by the next
    // master-signed one rather than obeyed (`sig::BuildFloor`), so a reader who saw only
    // the number could not tell a binding floor from an inherited one.
    let build_floor = crate::sig::BuildFloor {
        index_build: crate::sig::Floor::new(layout.floor()).current(),
        roster_seq: crate::sig::Floor::new(layout.floor_generation()).current(),
    };
    // A floor of 0 is no floor (`Floor::current` reads a missing file as 0): no index, or no
    // generation, has been accepted, and the row says that rather than an acceptance of 0.
    let named = match (build_floor.index_build, build_floor.roster_seq) {
        (0, _) => {
            let _ = writeln!(out, "{p}: no index accepted yet");
            None
        }
        (build, 0) => {
            let _ = writeln!(out, "{p}: index {build} accepted");
            None
        }
        (build, seq) => {
            let _ = writeln!(out, "{p}: index {build} accepted (roster generation {seq})");
            Some(seq)
        }
    };
    // The SECOND durable ratchet, which moves independently of the first: which generation
    // of the machine roster this store has accepted. A roster floor that is stuck while
    // machines have been minted or revoked means this store has not seen a publish since.
    // Said only when it is not the generation the row above already names.
    match (
        crate::sig::Floor::new(layout.roster_floor()).current(),
        named,
    ) {
        (0, None) => {}
        (0, Some(_)) => {
            let _ = writeln!(out, "{p}: no roster generation recorded");
        }
        (roster, named) if Some(roster) != named => {
            let _ = writeln!(out, "{p}: roster generation {roster} accepted");
        }
        _ => {}
    }
    // ...AND WHETHER IT IS STILL THE NEWEST. The channel-head probe writes its last answer
    // beside the floor; this reads it back, offline, and says it.
    if probes.index_head {
        let last_pass = LastPass {
            reached_index_at: crate::status::read(layout)
                .and_then(|s| crate::flow::rfc3339_to_unix(&s.last_success_at)),
            held: crate::index_probe::held_index_builds(layout),
        };
        index_head_line(
            &crate::index_probe::cached_answer(layout),
            &last_pass,
            now,
            p,
            out,
        );
    }
    // (8c) WHO WATCHES THE VENDOR HEADS: an aterm window, or, with no
    // window open, one terminal session — or nobody, and then a new Claude Code or Codex
    // release waits for the next update pass. Said wherever it matters: a vendor program
    // kept at its vendor's head on a machine whose packages update on their own.
    head_watch_line(layout, probes, p, out);

    // (9) RUSTUP + RELOCATABILITY. Two questions, kept apart: is there a rustup on
    // this machine at all (PATH, then `$CARGO_HOME/bin`, then `~/.cargo/bin` — the
    // binary an app-spawned pass may not have on ITS PATH), and does it answer inside
    // the probe ceiling: under load a present rustup can miss the `--version` deadline,
    // and the seam audit needs no rustup binary at all.
    let rustup = rustup_binary(home);
    let rustup_answers = rustup.as_deref().is_some_and(rustup_present);
    if rustup.is_none() {
        // A NOTE, not a warn: the product's own shape has no rustup — the managed Trust
        // toolchain runs as `targo`/`trustc` from the store — so its absence is a fact
        // about this machine, never something to fix.
        let _ = writeln!(
            out,
            "{p}: note — no rustup on PATH, in $CARGO_HOME/bin or in ~/.cargo/bin; the \
             managed toolchain runs without it (`targo`, `trustc`)"
        );
    } else if !rustup_answers {
        let _ = writeln!(
            out,
            "{p}: warn — rustup at {} did not answer `--version` within {} s (a wedged shim, \
             or a toolchain fetch in flight); rustup `trust` is checked below without it",
            rustup
                .as_deref()
                .map_or_else(String::new, |r| r.display().to_string()),
            PROBE_TIMEOUT.as_secs()
        );
    }
    // A link (5e) named has its line and its fix there: neither the re-point below nor
    // the unlinked-channel line — a link to nothing does not resolve either — says it a
    // second time with another fix.
    let local_said = probes.local_seal.is_some();
    if !local_said
        && let Some(rustup_home) =
            crate::seam::rustup_home_with(std::env::var_os("RUSTUP_HOME").as_deref(), home)
        && layout.program_current("trust").join("bin").is_dir()
        && let Some(line) = seam_line(
            &crate::seam::status(layout, &rustup_home, crate::seam::DEFAULT_SEAM),
            layout,
        )
    {
        // THE ENTRY EXISTS AND IS NOT OURS: the link resolves, so the check below would
        // call it healthy while the managed build sits unused by every `cargo +trust`.
        // Doctor SAYS it and names the one command; it re-points nothing — an entry under
        // `~/.rustup` that aterm did not lay is the user's, by the seam module's own rule.
        let _ = writeln!(out, "{p}: {line}");
    } else if !local_said
        && rustup_answers
        && layout.program_current("trust").join("bin").is_dir()
        && !rustup_trust_channel_resolves(rustup.as_deref().unwrap_or(Path::new("rustup")))
    {
        // The managed Trust toolchain is HERE, but rustup does not know it, so
        // `cargo +trust` and a `rust-toolchain.toml` pinning `channel = "trust"`
        // both fail with `'rustc' is not installed for the custom toolchain
        // 'trust'` — the message that reads as a blocked machine and is not one.
        // The fix is `repair`: it lays the entry when it is absent and rebuilds the
        // view when the entry names a view that no longer resolves. A hand `rustup
        // toolchain link` at the store would not do — the store ships only Trust's
        // names, and rustup resolves the stock ones (see the seam module's header).
        // `repair` lays nothing without `<rustup_home>/toolchains` (a fresh rustup with
        // no toolchain), so the fix creates it first.
        let fix =
            match crate::seam::rustup_home_with(std::env::var_os("RUSTUP_HOME").as_deref(), home) {
                Some(rh) if !crate::seam::toolchains_dir(&rh).is_dir() => format!(
                    "mkdir -p '{}' && aterm pkg repair",
                    crate::seam::toolchains_dir(&rh).display()
                ),
                _ => "aterm pkg repair".to_owned(),
            };
        let _ = writeln!(
            out,
            "{p}: warn — rustup has no working `trust` toolchain, so `cargo +trust` and \
             projects pinning `channel = \"trust\"` fail here (`targo` and `trustc` still \
             work) — fix: {fix}"
        );
    }
    // A STALE LINK THE MACHINE REPLACED ON ITS OWN ([`replaced_seam_line`]).
    if let Some(rustup_home) =
        crate::seam::rustup_home_with(std::env::var_os("RUSTUP_HOME").as_deref(), home)
        && let Some(line) = replaced_seam_line(layout, &rustup_home)
    {
        let _ = writeln!(out, "{p}: {line}");
    }

    // (10) THE QUESTION A USER ACTUALLY CAME HERE WITH: do I have the toolchain?
    //
    // Everything above audits the STRUCTURE of the store, and passes vacuously on a
    // machine that received nothing at all; health is never reported over an empty store.
    let installed = crate::ops::active_builds(layout);
    let status = crate::status::read(layout);
    // A recorded key beginning with `-` cannot ever be a program: no shim can be created
    // under one and the signed index cannot name one. Such a row is a STRAY (an old
    // atpkg recorded a mistyped flag as a program), and counting it would fail a healthy
    // toolset — and the exit code `tools/install.sh` hands to failed-seed installers.
    let strays: Vec<&String> = status
        .as_ref()
        .map(|s| s.programs.keys().filter(|k| k.starts_with('-')).collect())
        .unwrap_or_default();
    for stray in &strays {
        let _ = writeln!(
            out,
            "{p}: warn — the record holds a stray row for {stray:?}, which cannot be a \
             program name (it is a command-line flag, left by a mistyped `atpkg install`). \
             No program is missing because of it; the next successful `aterm pkg update` \
             clears it"
        );
    }
    // (10a) SYSTEM-SATISFIED MEMBERS. A member the signed index marks `system = "<bin>"`
    // that the pass found on PATH outside the prefix is deliberately NOT managed here —
    // its row says so in the canonical words (`system: <path> — not managed by aterm`),
    // and it is neither missing nor a fault. Re-check the recorded path: a binary that
    // has since gone away is the one state worth a line, because the next pass will
    // install the member through its artifact and a user may wonder why.
    if let Some(s) = status.as_ref() {
        for (program, row) in &s.programs {
            let Some(path) = crate::state::system_path(&row.state) else {
                continue;
            };
            if std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
                let _ = writeln!(
                    out,
                    "{p}: ok — {program}: {} (remove that copy to have atpkg manage {program})",
                    row.state
                );
            } else {
                let _ = writeln!(
                    out,
                    "{p}: warn — {program}: recorded as `{}`, but that copy is gone — the \
                     next `aterm pkg update` reinstalls the managed copy",
                    row.state
                );
            }
        }
    }
    // (10g) MEMBERS HELD ON THEIR CURRENT BUILD (`held: …`, [`crate::state::held_unpublished`]):
    // the update lane's row for an INSTALLED member whose coherence group's new pin is
    // not published for this target. A DEFERRED state, never a fault: nothing was
    // downloaded or staged, the group stays whole on the builds it has, and the pass
    // that finds the build published moves it. Said here as `ok`, quoting the row, so
    // the frozen tuple is VISIBLE — without this line the only trace of it was its
    // siblings' `pinned by index <old>` rows, which read as stale.
    if let Some(s) = status.as_ref() {
        for (program, row) in &s.programs {
            // Only the GROUP hold. A member held after a failed stage (`held: last attempt
            // failed with …`) is the opposite verdict — not installed, and no pass moves
            // it — and it is listed with the problems, retry and all (`is_problem_state`).
            if row.state.starts_with(crate::state::HELD_PREFIX)
                && !row.state.starts_with(crate::state::HELD_FAILED_PREFIX)
            {
                let _ = writeln!(
                    out,
                    "{p}: ok — {program}: {} (nothing to do here; the next pass moves the \
                     group once the index publishes for this target)",
                    row.state
                );
            }
        }
    }
    // (10d) SHADOWED MANAGED MEMBERS (design S5). For every managed member and every tool
    // it exposes, a foreign executable of that name EARLIER on PATH than the managed
    // bin/ is what actually runs — silently ahead of the build the index pins. Probed
    // LIVE against this process's PATH, never trusted from the record: a warning, never a
    // fault, and never "fixed" here — the user owns PATH.
    for (program, build) in &installed {
        if crate::linkmode::is_linked(layout, program) {
            continue;
        }
        // AN AGENT PROGRAM'S REROUTE STUB DECIDES AT EXEC TIME: with
        // `reroute/<name>` first on this PATH, what runs is the stub's choice — the managed
        // twin inside aterm, which is no shadow however stale this shell's PATH is (said
        // below as an `ok` naming the copy it out-ranks), or a pass-through, whose walk is
        // the one probed (`cli::shadow_in_shell`).
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
            let _ = writeln!(out, "{p}: ok — {program}: {row}");
            continue;
        }
        if let Some((tool, path)) = shadow {
            // …and outside aterm, a stub that passes through to the user's own copy is
            // the design, said as a note, never SHADOWED.
            if let Some(row) = crate::cli::agent_passed_row(
                layout,
                program,
                *build,
                &path,
                path_var,
                probes.stub_env,
            ) {
                let _ = writeln!(out, "{p}: note — {program}: {row}");
                continue;
            }
            // An AGENT PROGRAM whose `agents/` twin is laid and current: aterm's copy is
            // what every tab runs, so this is THIS shell's state — a shell that has not
            // run the hook — and the remedy works in place (`cli::shell_remedy_command`:
            // source the hook; never `exec $SHELL`, which drops an aterm tab's shell
            // integration). Never "open a new tab", never "remove or reorder that copy".
            if let Some(row) =
                crate::cli::agent_shell_shadow(layout, program, *build, &path, path_var)
            {
                let _ = writeln!(out, "{p}: warn — {program}: {row}");
                continue;
            }
            // The canonical state in the canonical words, then — when `alab-<tool>` is
            // laid — the one trailing sentence that names the way to the managed copy
            // without touching PATH (`cli::alias_fix`; never part of the state).
            let fix = crate::cli::alias_fix(layout, &tool, program, path_var)
                .map(|f| format!(" — {f}"))
                .unwrap_or_default();
            let managed = crate::vendor_direct::spec(program).map_or_else(
                || String::from("the pinned build"),
                |s| format!("the copy aterm updates from {}", s.vendor),
            );
            let _ = writeln!(
                out,
                "{p}: warn — {program}: {} (not {managed}; atpkg never edits PATH — remove or \
                 reorder that copy if you want the managed one){fix}",
                crate::state::shadowed(*build, &path)
            );
        }
    }
    // (10f) MANAGED MEMBERS WHOSE SHIM EXPORTS AN ENVIRONMENT (design S7): a vendor tool
    // whose policy declares `shim_env` runs, through the managed shim, with its own
    // updater off — `Claude Code's own updater is off here (DISABLE_AUTOUPDATER=1)`. Read
    // off the shim as laid (the thing that runs), printed as ONE trailing sentence after
    // the canonical row, never inside it; withheld
    // when a foreign copy shadows the shim (the env never reaches a system copy — that
    // member's line is (10d)'s warn). An `ok`, never a fault.
    //
    // A shim that exports other than its BUILD declares (the sidecar, written from the
    // signed policy — `activate::shim_env_drift`, the predicate the pass heals by) is
    // NAMED instead, as a `warn` with the one remedy — otherwise a plain shim reads as
    // nothing to say while the vendor's updater runs. Said even when a foreign copy
    // shadows the shim in this shell, because the shim still reaches something: the
    // `agents/` twin every aterm tab runs, an `alab-` alias, or any other shell whose
    // PATH finds it — each copies its exports.
    for (program, build) in &installed {
        if crate::linkmode::is_linked(layout, program) {
            continue;
        }
        let tools = crate::ops::active_tools(layout, program, *build);
        if let Some(drift) =
            crate::activate::shim_env_drift(layout, &layout.build_dir(program, *build), &tools)
            && let Some((shim, _)) = drift.shims.first()
        {
            let have = crate::platform::shim_env_of(shim);
            let line = env_drift_line(program, *build, shim, &have, &drift.declared);
            let _ = writeln!(out, "{p}: warn — {program}: {line}");
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
        let _ = writeln!(out, "{p}: ok — {program}: {line}");
    }
    // (10h) CLAUDE CODE'S DEFAULTS: the top-effort mode
    // (`CLAUDE_TOP_EFFORT_KEY`), `xhigh` effort, and the session-quality survey off.
    // An aterm window ADDS each where it is unset (`aterm_primer::add_claude_defaults`)
    // and never touches one set by hand, so this says which is which: the defaults as
    // one `ok`, a person's own value as an `ok` that names it, an effort default held
    // back by a person's own effort as an `ok` that names that effort, a top-effort
    // key that overrides such an effort as a `warn` naming both and the fix, and an
    // unset key as a `warn` naming what the next window start adds. A file the window
    // refuses to add to is one `warn` naming why, and no key: nothing will be added
    // to it. Every row prints the key from the constant, so a person reads the very
    // key their settings file holds.
    if installed.iter().any(|(program, _)| *program == "claude")
        && let Some(states) = &probes.claude_defaults
    {
        let states = match states {
            Ok(states) => states.as_slice(),
            Err(why) => {
                let _ = writeln!(
                    out,
                    "{p}: warn — claude: ~/.claude/settings.json cannot take aterm's defaults \
                     ({why}) — an aterm window leaves the file exactly as it is and adds none \
                     of them"
                );
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
                    key => key,
                })
                .collect();
            let _ = writeln!(
                out,
                "{p}: ok — claude: {} set to aterm's {} ({}) in ~/.claude/settings.json",
                defaults.join(", "),
                if defaults.len() == 1 {
                    "default"
                } else {
                    "defaults"
                },
                meaning.join(", ")
            );
        }
        for (key, state) in states {
            match state {
                DefaultState::Default => {}
                DefaultState::Manual(value) => {
                    // A hand-set switch that Claude Code does not read as true leaves the
                    // surveys ON: kept all the same, and said — with what an aterm-supervised
                    // session then does about them (its harness still dismisses each one).
                    let on = if *key == CLAUDE_SURVEY_SWITCH {
                        " — Claude Code reads only 1, true, yes or on as off, so it will \
                         still show its rating surveys (an aterm-supervised session dismisses \
                         each with a 0 unless [harness] dismiss_surveys is false)"
                    } else {
                        ""
                    };
                    let _ = writeln!(
                        out,
                        "{p}: ok — claude: {key} is {value}, set by hand — kept (aterm adds \
                         its default only where a key is unset){on}"
                    );
                }
                DefaultState::HeldForEffort(effort) => {
                    let _ = writeln!(
                        out,
                        "{p}: ok — claude: {key} is unset and stays so — an effort is set by \
                         hand ({effort}), and aterm adds {CLAUDE_TOP_EFFORT_KEY} and xhigh \
                         effort only where no other effort was chosen"
                    );
                }
                DefaultState::HeldForRate(rate) => {
                    let _ = writeln!(
                        out,
                        "{p}: ok — claude: {key} is unset and stays so — feedbackSurveyRate is \
                         set by hand ({rate}), and aterm adds the switch that turns EVERY rating \
                         survey off only where that probability is unset or 0"
                    );
                }
                DefaultState::OverridesEffort(effort) => {
                    let _ = writeln!(
                        out,
                        "{p}: warn — claude: \"{key}\": true in ~/.claude/settings.json \
                         overrides the effort set by hand ({effort}) — Claude Code runs xhigh \
                         while it stands — fix: remove \"{key}\" from ~/.claude/settings.json \
                         to use that effort (a ~/.claude/settings.json.bak-<unix> may hold \
                         the file from before)"
                    );
                }
                DefaultState::Unset => {
                    let want = CLAUDE_DEFAULTS
                        .iter()
                        .find(|(k, _)| k == key)
                        .map_or("", |(_, v)| *v);
                    let line = default_line(key, want);
                    let _ = writeln!(
                        out,
                        "{p}: warn — claude: {key} is unset in ~/.claude/settings.json — an \
                         aterm window adds {line} at its next start"
                    );
                }
            }
        }
    }
    // EVERY problem, not the first one.
    let recorded_problems = recorded_problems(status.as_ref());
    // (11a) AN UNSERVED PLATFORM IS A STATE, NOT A FAULT — Windows only, see
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
    // (11) COMPLETENESS AGAINST THE SIGNED INDEX.
    //
    // Everything above audits what the RECORD mentions: shims for installed programs,
    // rows in `status.toml`, faults the last pass wrote down. A program the machine
    // wants and never attempted has none of those — no shim, no row, no fault.
    //
    // The index is the only thing that knows what SHOULD be here, so ask it. Offline and
    // best-effort by construction (`cached_index`): an unreachable index must never
    // invent a missing program.
    let (dev_linked, link_problems) = live_dev_links(layout);
    for why in &link_problems {
        let _ = writeln!(out, "{p}: PROBLEM — {why}");
    }
    fails += link_problems.len();
    let active_count = installed
        .keys()
        .chain(dev_linked.iter())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let (missing_unexplained, missing_on_purpose, _indexed_dev_linked) =
        missing_against_index(layout, &installed, &dev_linked);
    // (11b) An unserved member is explained by its own row, not missing without a reason
    // (`drop_unserved`; identity everywhere but Windows).
    let missing_unexplained = drop_unserved(missing_unexplained, toolset_unserved.as_deref());
    let mut toolset_problem = false;
    if let Some(unserved) = &toolset_unserved {
        // Said once, as what it is: a fact about the index and this target, which no
        // command on this machine changes — the state, and no act. The members it names
        // are the ones whose own rows say so; the served members (the agents) are
        // reported like any other — their per-program lines print ABOVE this note (the
        // program loop runs first), so the note does not point "below".
        let _ = writeln!(
            out,
            "{p}: note — the ALab toolset is unavailable on this platform: no build is \
             published for this target{}; nothing on this machine changes that",
            if unserved.is_empty() {
                String::new()
            } else {
                format!(" ({})", unserved.join(", "))
            }
        );
    }
    if declined {
        // Intended emptiness. Say so, so it does not read as a fault.
        let _ = writeln!(
            out,
            "{p}: {DECLINED_LINE} (`aterm pkg install --default-set` reinstalls it)"
        );
    } else if active_count == 0
        && unserved_store_is_quiet(
            toolset_unserved.is_some(),
            &recorded_problems,
            &missing_unexplained,
        )
    {
        // Nothing active because nothing published for this target is missing: the note
        // above is the whole story, and it is not a problem to count or to exit 1 for.
        // (A served member that failed, is held, or is missing without a row takes the
        // branch below instead.)
        let _ = writeln!(
            out,
            "{p}: 0 program(s) active — none is published for this platform, so none is \
             missing"
        );
    } else if active_count == 0 {
        toolset_problem = true;
        match recorded_problems.first() {
            Some(why) => {
                let _ = writeln!(out, "{p}: PROBLEM — no ALab programs are installed ({why})");
            }
            // No per-program row survives an ENVIRONMENTAL failure any more — an
            // unreachable index says nothing about any particular program — so the
            // aggregate sentence is now the only place the reason lives. Preferring it to
            // the generic hint is what keeps "why did nothing arrive?" answerable offline.
            None => match status
                .as_ref()
                .map(|s| s.outcome.as_str())
                .filter(|o| !o.is_empty())
            {
                Some(outcome) => {
                    let _ = writeln!(
                        out,
                        "{p}: PROBLEM — no ALab programs are installed (last attempt: {outcome})"
                    );
                }
                // The fix is the tail's one `next` act (`aterm pkg install --default-set`),
                // said there and not twice.
                None => {
                    let _ = writeln!(out, "{p}: PROBLEM — no ALab programs are installed");
                }
            },
        }
    } else if !recorded_problems.is_empty() {
        // Something IS installed, but the record carries live failures — a partial first
        // run, a blocked disk, a member with no build for this triple, an aborted
        // coherence-group transaction.
        toolset_problem = true;
        // No COUNT here: the tail's "found N problem(s)" counts differently (structural
        // failures + the toolset condition as one), and two Ns would contradict.
        let _ = writeln!(
            out,
            "{p}: PROBLEM — the toolset is incomplete; {} program(s) active",
            active_count
        );
    } else if !missing_unexplained.is_empty() {
        // The machine wants these, does not have them, and NOTHING recorded a reason.
        // Distinct from the branch above: that one reports failures the record already
        // names, this one reports absences the record is silent about.
        toolset_problem = true;
        let _ = writeln!(
            out,
            "{p}: PROBLEM — the toolset is incomplete: {} of {} program(s) the signed \
             index serves are not installed ({})",
            missing_unexplained.len(),
            active_count + missing_unexplained.len() + missing_on_purpose.len(),
            missing_unexplained.join(", ")
        );
    } else {
        // Not "ALab program(s)": the count includes the vendor-direct agents.
        let _ = writeln!(out, "{p}: {active_count} program(s) active");
    }
    // A DELIBERATE removal is not a fault — but it must be VISIBLE. The ledger that
    // records it is a file no user reads, and its effect (no stub, no unattended
    // reinstall, and a coherence group that never activates) is indistinguishable from
    // the program never having existed. Say it plainly, and name the way back.
    for program in &dev_linked {
        let from = crate::linkmode::linked_checkout(layout, program)
            .map(|c| c.display().to_string())
            .unwrap_or_else(|| "a checkout".to_string());
        let _ = writeln!(
            out,
            "{p}: ok — {program}: dev-linked from {from} (not the index build; `aterm pkg unlink \
             {program}` returns it to the index)"
        );
    }
    for program in &missing_on_purpose {
        let _ = writeln!(
            out,
            "{p}: ok — {program}: removed on purpose — no unattended pass reinstalls it \
             (aterm pkg install {program} brings it back)"
        );
    }
    if let Some(start) = problem_listing_start(declined, active_count == 0, recorded_problems.len())
    {
        for why in recorded_problems.iter().skip(start) {
            let _ = writeln!(out, "{p}:   {why}");
        }
    }

    // (11) THE C TOOLCHAIN — a prerequisite, probed and never shipped (`crate::prereq`).
    // Only where a toolset is installed: that is what it stops from building. A verdict
    // that is an ANSWER (no toolchain, or one that refuses) is a FAIL — "healthy" over a
    // machine whose first `targo build` dies in a build script is the vacuous pass (10)
    // was written against; an unknown is a warn; a target it does not apply to, a note.
    let mut next_prereq = false;
    if active_count > 0
        && let Some(verdict) = probes.cc.as_ref()
    {
        let line = verdict.line();
        match verdict {
            crate::prereq::CcVerdict::Ready { .. } => {
                let _ = writeln!(out, "{p}: ok — {line}");
            }
            crate::prereq::CcVerdict::NotProbed => {
                let _ = writeln!(out, "{p}: note — {line}");
            }
            crate::prereq::CcVerdict::Unanswered { .. }
            | crate::prereq::CcVerdict::Unproven { .. } => {
                let _ = writeln!(out, "{p}: warn — {line}");
            }
            _ => {
                fails += 1;
                next_prereq = true;
                let _ = writeln!(err, "{p}: FAIL — {line}");
            }
        }
    }

    if fails == 0 && !toolset_problem && tools_cannot_run == 0 && tools_unproven == 0 {
        let _ = writeln!(out, "{p}: healthy");
        true
    } else if fails == 0 && !toolset_problem {
        // Nothing structural — but a managed tool that cannot run, or that doctor cannot
        // show to run, is not health, and this line is the one a reader takes away. The
        // warn above carries its own remedy, so no `next` line: the tail's rule for
        // failures with an inline remedy.
        let _ = writeln!(
            out,
            "{p}: not healthy — {} (see below)",
            withheld_summary(tools_cannot_run, tools_unproven)
        );
        true
    } else {
        let total = fails + usize::from(toolset_problem);
        let _ = writeln!(out, "{p}: found {total} problem(s)");
        // The whole-set install cannot serve an unserved platform (the note above); there
        // the act is the explicit door of the served member the verdict is about — unless
        // the verdict is toolset-wide (an unreachable index), which the ordinary act
        // answers exactly as it does elsewhere.
        let unserved_door = if toolset_problem && active_count == 0 && toolset_unserved.is_some() {
            unserved_platform_next(&recorded_problems, &missing_unexplained)
        } else {
            None
        };
        let next = next_act(
            &NextFacts {
                unserved_door,
                toolset_problem,
                empty_store: active_count == 0,
                recorded_problems: &recorded_problems,
                prereq_failed: next_prereq,
                prereq_fix: probes.cc.as_ref().and_then(crate::prereq::CcVerdict::fix),
                publish: next_publish,
                declined,
                update_divergence: next_update_divergence,
                install_program: next_install_program.as_deref(),
            },
            |program| in_coherence_group(layout, program),
        );
        if let Some(act) = next {
            let _ = writeln!(out, "{p}: next — {act}");
        }
        false
    }
}

/// What doctor's one next act is chosen from ([`next_act`]): facts the report has read.
#[derive(Debug, Default)]
struct NextFacts<'a> {
    /// The served member's door on an unserved platform ([`unserved_platform_next`]).
    unserved_door: Option<String>,
    /// A toolset-wide verdict is a problem.
    toolset_problem: bool,
    /// No managed program is active.
    empty_store: bool,
    /// `"<program>: <state>"` for each recorded problem.
    recorded_problems: &'a [String],
    /// The C toolchain prerequisite failed (11).
    prereq_failed: bool,
    /// Its one act, when it has one ([`crate::prereq::CcVerdict::fix`]).
    prereq_fix: Option<&'static str>,
    /// The newer Trust coherence group has to be published first.
    publish: bool,
    /// The owner declined the toolset.
    declined: bool,
    /// A managed shim diverged from the build its store names.
    update_divergence: bool,
    /// The one program whose structural repair is the act.
    install_program: Option<&'a str>,
}

/// THE ONE NEXT ACT — a single command, never a menu: a report that ends in a pile of
/// problems and three suggestions teaches a first-hour user to close the terminal.
/// Priority: install the missing SET (an empty store has exactly one fix), then `update` (a
/// successful pass rewrites every recorded fault row and re-flips diverged shims), then the
/// structural repair of one named program. Failures with their remedy already inline (a
/// stray .sh — "remove it") add no line rather than a second, vaguer act. `grouped` says
/// whether a program is in a coherence group, which decides a HELD member's door
/// ([`held_door`]).
fn next_act(f: &NextFacts<'_>, grouped: impl Fn(&str) -> bool) -> Option<String> {
    let door = |program: &str| held_door(program, grouped(program));
    if f.unserved_door.is_some() {
        f.unserved_door.clone()
    } else if f.toolset_problem && f.empty_store {
        // …but never over a HELD member: the whole-set pass honours the hold and repeats it
        // (`skip_held_member`), so the act is that member's explicit door, which forgets its
        // memo before it stages.
        Some(
            first_held_member(f.recorded_problems)
                .map_or_else(|| String::from("aterm pkg install --default-set"), door),
        )
    } else if f.prereq_failed {
        // No amount of updating installs a C compiler: the prerequisite outranks it. A
        // compiler that refuses with no one act to name (macOS: it is installed, and its own
        // stderr is the cause) has its remedy inline: no `next` line.
        f.prereq_fix.map(String::from)
    } else if f.publish {
        Some(format!(
            "publish the newer Trust coherence group ({PUBLISH_RUSTC_GROUP} on a rostered \
             machine holding the seal), then aterm pkg update trust"
        ))
    } else if !f.declined && f.recorded_problems.iter().any(|why| is_held_problem(why)) {
        // A HELD member is the one recorded problem a plain `update` cannot move: the pass
        // honours the hold and repeats it. When it is the only problem, the act is that
        // member's own door, so the screen carries one remedy; with others beside it,
        // `--retry` is the one pass that answers every recorded fault.
        Some(
            match (
                f.recorded_problems.len(),
                first_held_member(f.recorded_problems),
            ) {
                (1, Some(program)) => door(program),
                _ => String::from("aterm pkg update --retry"),
            },
        )
    } else if (!f.recorded_problems.is_empty() && !f.declined) || f.update_divergence {
        Some(String::from("aterm pkg update"))
    } else {
        f.install_program
            .map(|program| format!("aterm pkg install {program}"))
    }
}

/// The bare version token from `<bin> --version`, for comparing two copies of
/// the same program at a glance.
///
/// Takes the SECOND whitespace token (`ay 0.13.0+build.8174.…` -> `0.13.0…`)
/// and drops any `+build…`/commit/date suffix, because the question this
/// answers is "are these the same release", not "which exact commit".
///
/// Best-effort by construction: a binary that will not run, will not answer, or
/// answers in some other shape reports `unknown` and the caller stays quiet
/// about it. `doctor` must never fail because a probe failed — the probe is a
/// convenience, and the store integrity checks above are the real verdict.
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

/// What the bundle has to ship for tippy to run, as (5f) words it: tippy refuses a
/// symbolic link for the compiler, measured.
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

/// (10f)'s drift warn, after `<program>: `: what `shim` exports (`have`) against what
/// its build declares; the consequence only when the build declares a self-update switch
/// ([`crate::shim_env::SELF_UPDATE_SWITCHES`]) and the shim exports NONE — the one shape
/// where "on here" is certain, and the mirror of [`crate::shim_env::ShimEnv::fix_line`],
/// whose "off here" sentence this warn replaces; and the one remedy: `repair` re-lays each
/// shim with its build's sidecar. Pure, so every wording is pinned without a store.
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
            .any(|(name, _)| crate::shim_env::SELF_UPDATE_SWITCHES.contains(&name.as_str()))
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

/// [`run_with`]'s lines for the toolchain flip held for quiet and the live build leases,
/// each after its level: `note — …`, or `warn — …` when the leases cannot be read (then
/// gc keeps every build it would reclaim, and the held flip waits, until they can).
fn hold_lines(layout: &Layout, now: i64) -> Vec<String> {
    let mut lines = Vec::new();
    // Only a record that still describes a wait is said as one; one that outlived every
    // build it moves off is said as that, and left for a door that writes to end
    // ([`crate::quiet::held`] — doctor changes nothing).
    match crate::quiet::held(layout) {
        crate::quiet::Held::Nothing => {}
        crate::quiet::Held::Waits(deferral) => lines.push(format!(
            "note \u{2014} {}",
            deferral.sentence(now, &crate::vendor_direct::watch::watcher(layout))
        )),
        crate::quiet::Held::Outlived(record) => lines.push(format!(
            "note \u{2014} the record of a held Trust toolchain update ({}) outlived every \
             build it moves off, so nothing waits for it; `aterm pkg repair` ends it, as the \
             next update pass does",
            record.moves(),
        )),
    }
    match crate::lease::live(&layout.prefix) {
        Ok(leases) => {
            lines.extend(
                leases
                    .iter()
                    .filter_map(|(subject, holders)| lease_line(subject, holders)),
            );
        }
        Err(e) => lines.push(format!(
            "warn \u{2014} the build leases under {} cannot be read ({e}): gc keeps every \
             build it would reclaim, and a held toolchain update waits, until they can",
            crate::lease::dir(&layout.prefix).display()
        )),
    }
    lines
}

/// The note for one leased subject, or `None` for a free one. What the lease protects, by
/// subject: gc keeps a leased build, and the seam leaves a leased view as it stands.
/// Either way it stays as it is, which is what the line says, in the shared words
/// ([`crate::lease::Subject::describe`], [`crate::lease::Holders::clause`]) the
/// held-flip row uses for the same lease. It stays
/// until the holder's run ends, or, with no holder known, until its lease can be read.
fn lease_line(subject: &crate::lease::Subject, holders: &crate::lease::Holders) -> Option<String> {
    let until = match holders {
        crate::lease::Holders::Held(_) => "that run ends",
        crate::lease::Holders::Unknown(_) => "its lease can be read",
        crate::lease::Holders::Free => return None,
    };
    Some(format!(
        "note \u{2014} {} is {}; it stays as it is until {until}",
        subject.describe(),
        holders.clause()
    ))
}

/// The (5f)(c) line for an active trust build that [`crate::compat::needs_root`], and
/// what it claims ([`ToolClaim`]): a tool that cannot run, or one it cannot show to run
/// (both warns that withhold "healthy"), rather than a note. Pure over the inspection, so
/// every wording is testable without a store.
///
/// * Root absent: every trust shim's guard is false and runs the store path, where the
///   build's tippy refuses — PATH tippy cannot run.
/// * Root differs from the build: a shim whose own file differs runs the store path, and
///   the rest run a tree that is not the build — tippy MAY not run, and nothing vouches
///   for what does; the root, the store build and the first path that differs are each
///   named as what they are.
/// * Root matches, some shims unrouted: those run the store path — tippy cannot run
///   through them; the count and one name (`tippy` when it is among them) are given.
/// * Root matches, every shim routed: the note.
/// * `compat` or `compat/trust` not a real directory ([`crate::compat::Blocked`]): no
///   root is laid or rendered through it, and `aterm pkg repair` refuses too, so the fix
///   is to remove what stands there first. tippy cannot run when no shim carries a route;
///   one routed earlier may still run through the link, which atpkg does not vouch for,
///   so then it MAY not.
fn exec_root_line(p: &str, build: u64, i: &crate::compat::Inspection) -> (String, ToolClaim) {
    use crate::compat::RootState;
    let root = i.root.display();
    let copy = format!(
        "bin/rustc (inode {}) is a separate file from bin/trustc (inode {}; {} byte(s) differ)",
        i.rustc_ino, i.trustc_ino, i.differing
    );
    let n = i.shims.len();
    match (&i.state, i.unrouted.is_empty()) {
        (RootState::Blocked(blocked), _) => (
            format!(
                "{p}: warn — trust build {build}: PATH tippy {} run — {copy}, which this \
                 build's tippy refuses, and {}; fix: {}",
                if i.unrouted.len() == n {
                    "cannot"
                } else {
                    "may not"
                },
                blocked.what(),
                blocked.fix()
            ),
            if i.unrouted.len() == n {
                ToolClaim::CannotRun
            } else {
                ToolClaim::Unproven
            },
        ),
        (RootState::Matches, true) => (
            format!(
                "{p}: note — trust build {build}: {copy}, which its tippy refuses; its {n} trust \
                 shim(s) run each tool from {root}, where rustc holds trustc's bytes (a \
                 copy-on-write clone; the store is untouched) — removed with the build"
            ),
            ToolClaim::Runs,
        ),
        (RootState::Absent, _) => (
            format!(
                "{p}: warn — trust build {build}: PATH tippy cannot run — {copy}, which this \
                 build's tippy refuses, and no exec root stands at {root}; fix: {EXEC_ROOT_FIX}"
            ),
            ToolClaim::CannotRun,
        ),
        (RootState::Differs(at), _) => (
            format!(
                "{p}: warn — trust build {build}: PATH tippy may not run — {copy}, which this \
                 build's tippy refuses, and the exec root {root} is not the store build {} \
                 file for file (first difference: {}); fix: {EXEC_ROOT_FIX}",
                i.build_dir.display(),
                at.display()
            ),
            ToolClaim::Unproven,
        ),
        (RootState::Matches, false) => {
            let example = i
                .unrouted
                .iter()
                .find(|name| name.as_str() == "tippy")
                .unwrap_or(&i.unrouted[0]);
            (
                format!(
                    "{p}: warn — trust build {build}: PATH tippy cannot run through {} of {n} \
                     trust shim(s) (e.g. {example}) — {copy}, which this build's tippy refuses, \
                     and those shims do not route through the exec root {root}; fix: \
                     {EXEC_ROOT_FIX}",
                    i.unrouted.len()
                ),
                ToolClaim::CannotRun,
            )
        }
    }
}

/// The (5f)(f) line for an active trust build whose need for an exec root could not be
/// read ([`crate::compat::inspect`]'s `Err`, which names the path and the error): a warn
/// that withholds "healthy". No re-lay makes a store file readable, so `repair` is not
/// offered; the line points at `aterm pkg verify trust`, which checks every file of the
/// build against its signed tree.
fn exec_root_unread_line(p: &str, build: u64, why: &str) -> String {
    format!(
        "{p}: warn — trust build {build}: cannot tell whether PATH tippy can run — {why}; any \
         exec root standing for the build is left as it is; fix: make that store file readable \
         again (`aterm pkg verify trust` checks the build against its signed tree)"
    )
}

/// The (5f)(d) line for an entry under `<prefix>/compat/trust` that no build needs — an
/// advisory warn: the next `gc` removes it ([`crate::compat::sweep`] takes exactly what
/// [`crate::compat::strays`] classifies), and no tool fails over it.
fn stray_root_line(p: &str, path: &Path, why: crate::compat::Stray) -> String {
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
    format!("{p}: warn — {what}; fix: `aterm pkg gc` removes it")
}

/// Whether the view file `at` presents the store's Trust tool `store`: a clone of it
/// ([`crate::clone::is_clone_of`] — length, mode and time, not inode, so the hard link a
/// view held before clones does not present it) holding its bytes, since the attributes
/// alone cannot tell it from a bundle's separately signed copy. Writes nothing.
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

/// How recent a "nothing newer" answer must be for (8) to say `ok`: the window re-asks
/// the next two tags every thirty seconds ([`crate::index_probe`]), so an answer older than
/// this means nothing is asking any more (no window running), and its age is the honest
/// thing to print.
const INDEX_HEAD_FRESH_SECS: i64 = 15 * 60;

/// What the last signed update pass left on disk that bears on the channel head: when a
/// pass last REACHED the signed index (`status.toml`'s `last_success_at` — stamped only by
/// a pass that verified an index the channel served, never by one on the §14 cache), and
/// the index builds of the signed candidates it downloaded
/// ([`crate::index_probe::held_index_builds`]). `None` in either field when that record is
/// absent or unreadable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LastPass {
    reached_index_at: Option<i64>,
    held: Option<Vec<u64>>,
}

/// (8)'s channel-head line: the probe's cached answer for the CURRENT floor
/// ([`crate::index_probe::cached_answer`]) set beside what the last signed update pass left
/// ([`LastPass`]), said as one line. None withholds "healthy" — like the other freshness
/// lines, it is a fact about the channel, not a fault in the store. Pure over its inputs
/// and `now`, so tests drive it through the probe's and the cache's real writers.
///
/// # The remedy is only printed where it can work
///
/// "run: aterm pkg update" is right when no signed pass has looked since the newer index
/// appeared. It is wrong — and loops, update saying "already current" and doctor saying
/// "not the newest" — when a pass already downloaded the newer index and refused it, or
/// walked the channel's tags and found no complete newer release there. The last pass's
/// downloaded candidates (the §14 cache, accepted or not) tell those apart offline,
/// so each gets its own sentence. And the notes never promise that `aterm pkg update`
/// refreshes the probe's answer — it does not write the probe's stamps; what it does move,
/// the time a pass last reached the index, is printed beside them.
fn index_head_line(
    cached: &crate::index_probe::CachedProbe,
    last: &LastPass,
    now: i64,
    p: &str,
    out: &mut dyn std::io::Write,
) {
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
    // A NEWER INDEX ALREADY DOWNLOADED AND NOT LANDED. A pass that reaches the channel
    // caches what it served whether or not it selected (`flow::resolve_verified_index`),
    // so this is independent of the probe: the pass held it, and the floor did not move.
    if let Some(held) = last
        .held
        .as_ref()
        .and_then(|builds| builds.iter().copied().filter(|b| *b > floor).max())
    {
        let _ = writeln!(
            out,
            "{p}: warn — index {floor} is not the newest: index {held} was downloaded but \
             not accepted — its signatures did not verify here, or its pass stopped early; \
             if `aterm pkg update` still ends on index {floor}, the release needs fixing, \
             not this machine"
        );
        return;
    }
    match cached.near {
        Some((RangeAnswer::Published, seen)) => {
            if last.reached_index_at.is_some() && last.held.is_some() {
                // The last pass reached the channel and held nothing above the floor: at
                // that moment it had no COMPLETE signed release newer than it.
                let _ = writeln!(
                    out,
                    "{p}: warn — index {floor} is not the newest: the index probe found a \
                     newer ALab index (seen {} ago; an unverified hint), but {pass} and \
                     found no complete signed release newer than {floor} there — if the new \
                     one was up by then, its signed files are not all uploaded and no update \
                     can land it yet; otherwise run: aterm pkg update",
                    ago(age(seen))
                );
            } else {
                let _ = writeln!(
                    out,
                    "{p}: warn — index {floor} is not the newest: the index probe found a \
                     newer ALab index (seen {} ago; an unverified hint — no signed pass has \
                     landed it on this machine) — run: aterm pkg update",
                    ago(age(seen))
                );
            }
        }
        Some((RangeAnswer::Missing, checked)) => {
            // Said as what was CHECKED, and when: the probe's own rule is that absence is
            // never authority. Only a running window re-asks, so a stale answer says so.
            let checked = age(checked);
            if checked <= INDEX_HEAD_FRESH_SECS {
                let _ = writeln!(
                    out,
                    "{p}: ok — no index newer than {floor} is published (checked {} ago)",
                    ago(checked)
                );
            } else {
                let _ = writeln!(
                    out,
                    "{p}: note — no index newer than {floor} was published when last checked, \
                     {} ago (an aterm window re-checks while open); {pass}",
                    ago(checked)
                );
            }
        }
        Some((RangeAnswer::Failed, when)) => {
            let _ = writeln!(
                out,
                "{p}: note — the last check for an index newer than {floor} got no usable \
                 answer ({} ago), so whether one is published is unknown here; {pass}",
                ago(age(when))
            );
        }
        None => {
            let _ = writeln!(
                out,
                "{p}: note — no check for an index newer than {floor} is recorded on this \
                 machine (an aterm window checks while open); {pass}"
            );
        }
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

/// The tail of the never-checked lines — `; claude and codex update from their vendors
/// without it` — naming only the vendor programs that do: kept at their vendor's head
/// ([`crate::vendor_direct::watch::follows_vendor`]: installed, not removed, held or
/// dev-linked), not in `[packages] exclude`, on a machine whose packages update on their
/// own ([`Probes::automatic`]). Empty when none does, and the line claims nothing of them.
fn vendor_update_clause(layout: &Layout, probes: &Probes) -> String {
    let names = vendor_followers(layout, probes);
    match names.as_slice() {
        [] => String::new(),
        [one] => format!("; {one} updates from its vendor without it"),
        many => format!(
            "; {} update from their vendors without it",
            many.join(" and ")
        ),
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
fn head_watch_line(layout: &Layout, probes: &Probes, p: &str, out: &mut dyn std::io::Write) {
    use crate::vendor_direct::watch::Watcher;
    let names = vendor_followers(layout, probes);
    if names.is_empty() {
        return;
    }
    let names = names.join(" and ");
    let line = match crate::vendor_direct::watch::watcher(layout) {
        Watcher::Window => format!(
            "ok — an aterm window watches the vendor heads: a new {names} release is found \
             within minutes"
        ),
        Watcher::Session(pid) => format!(
            "ok — a terminal session{} watches the vendor heads, no aterm window being open: \
             a new {names} release is found within minutes",
            pid.map(|pid| format!(" (pid {pid})")).unwrap_or_default()
        ),
        Watcher::Nobody => format!(
            "note — nothing watches the vendor heads now (no aterm window, and no terminal \
             session running aterm, is open): a new {names} release is found by the next \
             update pass instead of within minutes"
        ),
        Watcher::Unknown(why) => format!(
            "note — could not tell whether anything watches the vendor heads ({why}): a new \
             {names} release may wait for the next update pass"
        ),
    };
    let _ = writeln!(out, "{p}: {line}");
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

/// (5f b) For every recorded seam, each stock name the view holds must present the tool it
/// stands for: the store's while the store's build is what the view presents (a clone of it
/// holding its bytes, [`presents`]), the dev-linked checkout's while trust is dev-linked to
/// a sysroot (the view is exec stubs there, so the stub's target is read rather than an
/// inode compared). `store_bin` is the store build's `bin/` — the active one, or `current`'s
/// when no shim names a build — or `None` when the store holds none. A mismatch is a tool
/// that cannot run: `repair` rebuilds the view, except under a dev-link the seam refuses,
/// where nothing rebuilds it until the link goes.
fn view_stock_names_check(
    out: &mut dyn std::io::Write,
    p: &str,
    layout: &Layout,
    store_bin: Option<PathBuf>,
    tools_cannot_run: &mut usize,
) {
    let source = crate::seam::view_source(layout);
    const STORE_WHAT: &str = "a clone of the store's";
    const STORE_WRONG: &str = "absent, other bytes, or the hard link a view held before clones";
    let (presented, presented_what, wrong, fix) = match (&source, store_bin) {
        (crate::seam::ViewSource::Linked(checkout), _) => (
            checkout.join("bin"),
            "the dev-linked checkout's",
            "absent, or a separate file",
            "`aterm pkg repair` rebuilds the view",
        ),
        (crate::seam::ViewSource::LinkedNoSysroot(_), Some(bin)) => (
            bin,
            STORE_WHAT,
            STORE_WRONG,
            "`aterm pkg unlink trust` — atpkg cannot present a dev-link whose checkout is not a \
             sysroot, so `repair` cannot rebuild the view until the link goes",
        ),
        (crate::seam::ViewSource::Store, Some(bin)) => (
            bin,
            STORE_WHAT,
            STORE_WRONG,
            "`aterm pkg repair` rebuilds the view",
        ),
        // A refused dev-link with no store build behind the view: nothing the view
        // could be checked against, and nothing it could correctly present — whatever
        // it last presented is what `rustup run trust` runs. Said, with the one fix.
        (crate::seam::ViewSource::LinkedNoSysroot(checkout), None) => {
            for name in crate::seam::recorded_names(layout) {
                *tools_cannot_run += 1;
                let _ = writeln!(
                    out,
                    "{p}: warn — rustup `{name}`: trust is dev-linked to {}, which atpkg cannot \
                     present (no bin/ and lib/, or a tree inside its own prefix), and no \
                     installed build stands behind the view, so `rustc +{name}` runs whatever \
                     the view last presented, or nothing; fix: `aterm pkg unlink trust`",
                    checkout.display()
                );
            }
            return;
        }
        (crate::seam::ViewSource::Store, None) => return,
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
                *tools_cannot_run += 1;
                let _ = writeln!(
                    out,
                    "{p}: warn — rustup `{name}`: {} is not {presented_what} {} ({wrong}), so \
                     `{public} +{name}` runs something other than the managed {trust}, or \
                     nothing; fix: {fix}",
                    view_tool.display(),
                    tool.display()
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

/// The doctor line for a rustup `trust` entry that is NOT the managed seam, or `None`
/// when the entry is absent or already ours (the callers' other checks speak then).
/// Pure over the probe result, so the words are testable without a rustup.
///
/// A foreign LINK names the exact re-point ([`repoint_fix`]); a foreign DIRECTORY/FILE cannot be re-pointed over — `ln -sfn` onto a
/// directory would lay the link INSIDE it — so that shape gets [`crate::seam::DETACH_FIX`].
/// A link into the store — `current`, or a numbered build, the layouts from before the
/// view — is a note, not a warn: `repair` re-points it to the view by itself.
fn seam_line(st: &crate::seam::SeamStatus, layout: &Layout) -> Option<String> {
    use crate::seam::Entry;
    let target = crate::seam::seam_target(layout, "trust");
    match &st.entry {
        Ok(Entry::Link(raw)) if !st.in_prefix => Some(format!(
            "warn — rustup `trust` -> {} is NOT the managed store ({}): `cargo +trust`, \
             `rustup run trust` and every repo pinning `channel = \"trust\"` build with THAT \
             copy, not the one `aterm pkg update` keeps current. re-point it: {}; revert \
             by re-linking the old target the same way",
            raw.display(),
            target.display(),
            repoint_fix(layout, &st.path)
        )),
        Ok(Entry::Link(_)) if !st.targets_view => Some(format!(
            "note — rustup `trust` -> {} is inside the store, not the view atpkg lays \
             ({}); `aterm pkg repair` re-points it so the stock names rustup resolves are \
             the managed tools and updates move the channel",
            st.target.as_deref().unwrap_or(&st.path).display(),
            target.display()
        )),
        Ok(entry @ (Entry::Dir | Entry::File | Entry::Other)) => Some(format!(
            "warn — rustup `trust` at {} is {} — not a link into the managed store ({}), so \
             `cargo +trust` builds with whatever that holds; {}",
            st.path.display(),
            entry.describe(layout),
            target.display(),
            crate::seam::DETACH_FIX
        )),
        Ok(Entry::Link(_) | Entry::Absent) => None,
        Err(e) => Some(format!(
            "warn — rustup `trust` at {} could not be inspected ({e})",
            st.path.display()
        )),
    }
}

/// The doctor line for a stale rustup `trust` link a re-assertion REPLACED: the
/// unattended pass re-points an older live build tree at the store by itself
/// ([`crate::seam::live_build_tree`]), and a change nobody asked for is said where the person
/// looks — what it named and the one command that puts it back, or, once it has been put
/// back, that no pass will move it again ([`crate::seam::replaced_note`]). A note, not a
/// warn: the channel works either way, and the stale half of a link put back is (5e)'s to
/// say. `None` when nothing was replaced, or the entry has since become something else.
fn replaced_seam_line(layout: &Layout, rustup_home: &Path) -> Option<String> {
    let line = crate::seam::replaced_note(layout, rustup_home, crate::seam::DEFAULT_SEAM)?;
    Some(format!("note — {line}"))
}

/// Where `rustup` is: the first executable `rustup` on `PATH`, else
/// `$CARGO_HOME/bin/rustup`, else `~/.cargo/bin/rustup` — the two places
/// rustup-init lays it, which an app-spawned pass (no rc file read) does not
/// have on its PATH. Existence only; whether it answers is [`rustup_present`].
fn rustup_binary(home: Option<&Path>) -> Option<std::path::PathBuf> {
    // The platform's EXECUTABLE spelling: a bare `rustup` is never a file on Windows.
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

/// Does rustup resolve a `trust` channel? `rustup which cargo --toolchain trust`
/// exits non-zero (and prints the famous `'rustc' is not installed for the
/// custom toolchain 'trust'`) when the channel is absent or dangling; a linked
/// channel answers with the cargo path. Bounded like every other probe here.
fn rustup_trust_channel_resolves(bin: &Path) -> bool {
    output_bounded(aterm_uds::spawnfd::command(bin).args([
        "which",
        "cargo",
        "--toolchain",
        "trust",
    ]))
    .is_some_and(|o| o.status.success() && !o.stdout.is_empty())
}

/// How long ONE `--version` probe may take before `doctor` gives up on it.
///
/// A healthy local binary answers in milliseconds; the ceiling is for a wedged one (a
/// stale NFS mount, a `rustup` shim waiting on a network fetch), which would otherwise
/// hang the whole report silently.
const PROBE_TIMEOUT: std::time::Duration = crate::prereq::PROBE_TIMEOUT;

/// Poll interval while waiting for a probe to exit.
const PROBE_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// Run a version probe with a bounded wall clock, killing and reaping it on
/// timeout. `None` when it could not be spawned, did not finish in time, or
/// could not be waited for.
///
/// Fails to `None`, not to an error, and that is the right direction HERE (the
/// opposite of the updater's fail-closed helpers this mirrors): the probe is a
/// convenience that renders one column of a report, and `doctor`'s contract is
/// that it never fails because a probe failed — the store integrity checks are
/// the verdict. A timed-out probe reads `unknown`, exactly like a binary that
/// will not run.
///
/// Every probe handed here is built through `aterm_uds::spawnfd::command` (B27 of
/// `tools/grep_guard.sh`): a no-op from a console, and on Windows a windowless
/// console from a process with none, where a bare console probe opens a visible one.
pub(crate) fn output_bounded(
    cmd: &mut aterm_uds::spawnfd::Command,
) -> Option<std::process::Output> {
    bounded(cmd, false, probe_timeout())
}

/// [`PROBE_TIMEOUT`], or a test's shorter one (`ShorterProbes`, unix tests only).
fn probe_timeout() -> std::time::Duration {
    #[cfg(all(test, unix))]
    if let Some(bound) = PROBE_TIMEOUT_IN_TEST.with(std::cell::Cell::get) {
        return bound;
    }
    PROBE_TIMEOUT
}

#[cfg(all(test, unix))]
thread_local! {
    static PROBE_TIMEOUT_IN_TEST: std::cell::Cell<Option<std::time::Duration>> =
        const { std::cell::Cell::new(None) };
}

/// A test's shorter [`PROBE_TIMEOUT`] for [`output_bounded`], on THIS thread
/// until dropped: a leg whose driver is wedged on purpose waits out the probe's
/// bound, not five seconds of the production one. The bound itself is pinned at
/// its production length by `prereq`'s wedged-compiler test. Unix only, like its
/// one user (a script fixture): a Windows `cfg(test)` build carries no unused
/// helper.
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
/// is the answer (`crate::prereq`: a broken compiler's first stderr line, verbatim). Both
/// pipes are read after exit, so it is for probes whose output is a line or two.
pub(crate) fn output_bounded_capturing(
    cmd: &mut aterm_uds::spawnfd::Command,
    within: std::time::Duration,
) -> Option<std::process::Output> {
    bounded(cmd, true, within)
}

fn bounded(
    cmd: &mut aterm_uds::spawnfd::Command,
    keep_stderr: bool,
    within: std::time::Duration,
) -> Option<std::process::Output> {
    use aterm_uds::spawnfd::Stdio;
    use std::io::Read as _;
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
    let status = loop {
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
    // The child has exited, so its stdout is closed and this read cannot block —
    // and a version line is one line, far inside any pipe buffer, so nothing
    // could have wedged the child on a full pipe before it got here either.
    let mut stdout = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_end(&mut stdout);
    }
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_end(&mut stderr);
    }
    Some(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// Copy-pasteable manual PATH-append for the shell of THIS platform: PowerShell on Windows
/// (';' separator, `$env:PATH`), POSIX `export` elsewhere. Only a fallback — an aterm shell
/// auto-sources the `shell.d` hook that does this already.
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

/// Report the aterm app's own update posture beside the toolchain's.
///
/// The app updates itself through a SEPARATE updater (it cannot be an atpkg program:
/// swapping a running, notarized `.app` needs Gatekeeper assessment, a boot sentinel and
/// the in-session PTY handoff), so this reads that updater's own records rather than
/// forming a second opinion. It never says to reopen the app: a staged build applies
/// in-session. It names WHICH process applies — the window; a terminal session checks and
/// stages but never applies, so with no window open the note names `aterm --window`.
/// `installs_by_itself` is `[update] auto_apply`: off, the window installs nothing until a
/// person presses Install, and the note names Settings ▸ Software Update, as `aterm update`
/// does. A staged build whose installs keep failing (health.toml's apply streak) promises no
/// install either: the note says it did not install and how many tries, as `aterm update`
/// does.
///
/// Silent when there is no updater state: a bare CLI install is a legitimate posture, not a
/// fault.
fn report_aterm_posture(
    layout: &crate::store::Layout,
    app_dirs: &[PathBuf],
    installs_by_itself: bool,
    p: &str,
    out: &mut dyn std::io::Write,
) {
    let Some(support) = layout.prefix.parent() else {
        return;
    };
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
        p,
        out,
    );
}

/// [`report_aterm_posture`] over an explicit updater ledger dir, the SEALED build
/// number of the bundle this process runs from (`None` outside a bundle), and the
/// `(build, version)` read off each bundle found, so the arms are testable against a
/// fixture.
///
/// "Installed on disk" is the NEWER of the updater's receipt and the bundle's own
/// `CFBundleVersion`: a bundle placed by hand (`install.sh`, a DMG drag) is newer than
/// any receipt the updater wrote, and the receipt alone could name an OLDER build as
/// "installed on disk". The note fires only when what is on disk is strictly newer than
/// what runs; an equal or older receipt is the quiet line.
fn report_aterm_posture_at(
    updates: &Path,
    bundle_build: Option<u64>,
    versions: &[(u64, String)],
    installs_by_itself: bool,
    p: &str,
    out: &mut dyn std::io::Write,
) {
    let field = |file: &str, key: &str| -> Option<String> {
        let text = std::fs::read_to_string(updates.join(file)).ok()?;
        text.lines()
            .find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == key))
            .map(|(_, v)| v.trim().trim_matches('"').to_string())
    };
    let Some(receipt) = field("installed.toml", "build_number") else {
        return;
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
            // The staged build is applied IN-SESSION by the WINDOW's overlap handoff
            // (automatic within a minute by default, one click otherwise); the shells keep
            // running. A terminal session never applies, so the note names the window for a
            // Mac that has none open, and never asks the user to reopen anything.
            // The window's installs of THIS build keep failing: no install is on its way,
            // so none is promised. The count is health.toml's per-target one, keyed to the
            // build it tried — the escalation streak `apply_failures` carries a superseded
            // build's tries onto a fresh download nothing has tried yet.
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
                let _ = writeln!(
                    out,
                    "{p}: note — aterm {staged} is downloaded (running {current}) but \
                     didn\u{2019}t install ({tries})"
                );
            } else if installs_by_itself {
                let _ = writeln!(
                    out,
                    "{p}: note — aterm {staged} is downloaded (running {current}) and installs \
                     within a minute while an aterm window is open — your shells keep running; \
                     with no window open: aterm --window"
                );
            } else {
                let _ = writeln!(
                    out,
                    "{p}: note — aterm {staged} is downloaded (running {current}) — {by_hand}"
                );
            }
        }
        (Some(current), _) if newer_on_disk => {
            // A newer bundle is already on disk: the window activates it in place, the same
            // in-session lane — and, as above, only the window.
            let (installed, current) = pair(&installed, &current);
            if installs_by_itself {
                let _ = writeln!(
                    out,
                    "{p}: note — aterm {installed} is on disk (running {current}) and an aterm \
                     window switches to it in place — your shells keep running; with no window \
                     open: aterm --window"
                );
            } else {
                let _ = writeln!(
                    out,
                    "{p}: note — aterm {installed} is on disk (running {current}) — {by_hand}"
                );
            }
        }
        _ => {
            // The version a person recognises, read off whichever bundle's sealed build IS
            // the installed one — the running bundle or an app dir's. With no such bundle
            // readable the row stays build-only, so it never names a version it did not
            // read off that build.
            match versions.iter().find(|(b, _)| b.to_string() == installed) {
                Some((_, v)) => {
                    let _ = writeln!(out, "{p}: ok — aterm {v} (build {installed}) installed");
                }
                None => {
                    let _ = writeln!(out, "{p}: ok — aterm build {installed} installed");
                }
            }
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

/// `CFBundleVersion` out of an XML plist: the `<string>` IMMEDIATELY after the key
/// (modulo whitespace) — never a string further on that another key owns — parsed as
/// the integer build number the cutter seals. `None` for a missing key, an intervening
/// element, or a non-integer value.
fn plist_bundle_version(text: &str) -> Option<u64> {
    let key = "<key>CFBundleVersion</key>";
    let after = text.find(key)? + key.len();
    let value = text[after..].trim_start().strip_prefix("<string>")?;
    let end = value.find("</string>")?;
    value[..end].trim().parse::<u64>().ok()
}

/// The index (in `split_paths` order) of the first `PATH` entry holding an executable
/// `name` OUTSIDE the managed `prefix` — the reroute dir lives under it, so a laid stub
/// never counts as its own upstream. A local walk on purpose: the vendor probes are
/// [`crate::store::ToolName`]-gated and `cargo` is deny-listed there. Relative entries
/// are skipped (they name the cwd, not a toolchain), and only an executable regular file
/// counts — a directory named `cargo` is not a copy that runs.
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

/// The clause of the Spotlight warning that says how far the unattended apply reaches
/// (`aterm pkg machine apply`, which the window runs as it opens and a terminal session once
/// a day): the exposed target dirs beside a
/// Cargo.toml are its to migrate; the FREE-STANDING ones (`free`, no Cargo.toml beside
/// them) are pointed at by an env var or a build system it cannot re-point, so it never
/// renames them and the operator has to — by name, after re-pointing. Up to three are
/// named; a count that names nothing sends the reader to `aterm pkg noindex` to find out
/// which.
fn spotlight_reach(in_repo: usize, free: &[&crate::noindex::Target]) -> String {
    const SHOWN: usize = 3;
    let named = || {
        let mut names: Vec<String> = free
            .iter()
            .take(SHOWN)
            .map(|t| t.path.display().to_string())
            .collect();
        if free.len() > SHOWN {
            names.push(format!("+{} more", free.len() - SHOWN));
        }
        names.join(", ")
    };
    match (in_repo, free.len()) {
        (_, 0) => "aterm migrates them once a day, at the first window or terminal session \
             (every one is beside a Cargo.toml)"
            .to_string(),
        (0, _) => format!(
            "NONE is beside a Cargo.toml, so aterm never renames any of them: each \
             is a free-standing target dir an env var or a build system points at ({}) — \
             re-point that, then `aterm pkg noindex apply <dir>` by name",
            named()
        ),
        (r, f) => format!(
            "aterm migrates the {r} beside a Cargo.toml once a day, at the first window or \
             terminal session; the other {f} \
             {} free-standing — an env var or a build system points at {} ({}), which \
             aterm cannot re-point, so it never renames {} — re-point that, then `aterm pkg \
             noindex apply <dir>` by name",
            if f == 1 { "is" } else { "are" },
            if f == 1 { "it" } else { "each" },
            named(),
            if f == 1 { "it" } else { "them" },
        ),
    }
}

#[cfg(test)]
#[path = "doctor_tests.rs"]
mod tests;
