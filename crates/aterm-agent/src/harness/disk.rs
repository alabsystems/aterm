// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! DISK WATCH AND CLEANUP (design `docs/DESIGN-aterm-wrapper-2026-09-17.md`
//! §5.5): the free-space figure, the stale targets, and the one apply path
//! that is allowed to remove anything.
//!
//! # Report-only is the default, and apply is not a mode — it is an argument
//!
//! [`report`] is a pure function of a [`Survey`]: it never removes a byte.
//! Removal happens only through [`plan`] + [`apply`], and [`plan`] answers
//! [`Plan::ReportOnly`] unless the caller NAMED a class. There is no
//! `--apply` that means "apply everything": the class is the grant, and a
//! grant for `cargo-targets` cannot reach a row of any other class.
//!
//! # Every row carries the witness that makes it safe to remove
//!
//! A row with no witness is not a candidate; it is a note. The three classes
//! the design names each have one, and each witness is a MEASURABLE fact
//! about the directory rather than a rule about its name:
//!
//! * [`Class::CargoTargets`] — a build directory whose lock file is older
//!   than `target_stale_days`, whose whole root is older than that same
//!   threshold (the [`TargetDir::live_build`] proxy — see its docs), and
//!   which carries a marker only a build tool writes
//!   ([`CACHEDIR_SIGNATURE`] or `.rustc_info.json`). The marker is the "we
//!   can prove we laid it" half; the two clocks are the "nothing is using
//!   it" half. Removing it costs a rebuild, and the row SAYS so.
//! * [`Class::ClaudeStaleVersions`] — a vendor version directory that is not
//!   the live symlink's resolved target. The witness NAMES the live target,
//!   and if the live target cannot be resolved at all, every row in the
//!   class becomes unsafe: with nothing to compare against, "not the live
//!   one" is an assumption, not a witness.
//! * [`Class::AtpkgGc`] — a package build superseded by the live build of
//!   the same program. Reported, never removed from here: `store/` is under
//!   atpkg's single-writer lock and `aterm pkg gc` is its one gardener, so
//!   the row carries [`Delegate::AtermPkgGc`] and [`apply`] refuses it.
//!
//! [`Class::ClaudePurge`] is surfaced the same way — a row naming
//! `claude project purge --dry-run` — and is likewise delegated, because it
//! reaches the vendor's own project history and §5.5 forbids this module from
//! touching transcripts under any flag.
//!
//! # What this module will never do
//!
//! * Remove a transcript, or anything under [`Survey::transcripts_root`].
//!   [`guard`] refuses such a path even when it is on the report — a second
//!   fence behind the first, because this is the one mistake that is not
//!   recoverable by rebuilding.
//! * Remove a path that is not on the report for the class named. A path
//!   arriving from anywhere else is [`Refusal::NotReported`], and the
//!   refusal is journalled as a DENIAL ROW rather than dropped, so a refused
//!   apply leaves the same kind of evidence a successful one does.
//! * Remove `~/.cargo/git`, or any path the design's safelist does not name.
//!   The safelist is the [`Class`] enum: a class that is not in it has no
//!   spelling that reaches [`apply`].
//!
//! # The one timer in the design, and why it is here
//!
//! §5.5 keeps a 6 h tick, and §4.2 row 18 says why: a free-space figure has
//! no event to hang on — nothing in aterm or the vendor announces "a gigabyte
//! left". That tick belongs to the HOST that calls [`report`]; there is no
//! sleep, no loop and no cadence in this file. Two event-shaped re-measure
//! reasons ride it instead of a second clock ([`Trigger`]), so a host that
//! has them can re-measure on the event and leave the tick as the floor.
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested, including the
//! synthetic stale target that is reported and then removed with its witness
//! in the row, the outside-the-safelist path that is refused with a denial
//! row, and the report-only default. The 6 h tick and the `--diagnose` line
//! of §5.5 have no host yet; [`Trigger`] is the vocabulary they will use.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use aterm_json::{Map, Value};

use super::truncate_bytes;
use super::usage::rfc3339_utc;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// `disk.warn_free_gib` (design §5.5).
pub const DEFAULT_WARN_FREE_GIB: u64 = 40;

/// `disk.target_stale_days` (design §5.5).
pub const DEFAULT_TARGET_STALE_DAYS: i64 = 14;

/// `disk.apply` (design §5.5): report-only until the owner says otherwise.
pub const DEFAULT_APPLY: bool = false;

/// The `CACHEDIR.TAG` signature every cargo-compatible build tool writes into
/// a target directory. VERIFIED against the Cache Directory Tagging
/// Specification's fixed signature line, which cargo emits verbatim; it is
/// the marker that makes "a build tool laid this" a fact rather than a guess
/// about the directory's name.
pub const CACHEDIR_SIGNATURE: &str = "Signature: 8a477f597d28d172789f06886806bc55";

/// The other marker a cargo target directory carries.
pub const RUSTC_INFO: &str = ".rustc_info.json";

/// The lock file cargo keeps at a target directory's root.
pub const CARGO_LOCK_FILE: &str = ".cargo-lock";

/// The most bytes of any quoted path or reason one ledger row may carry.
pub const ROW_TEXT_CAP: usize = 512;

/// The most rows one report will carry per class. A machine with a thousand
/// stale target directories is a machine with a different problem; this keeps
/// one report one screen's worth of decisions and bounds every walk below.
pub const MAX_ROWS_PER_CLASS: usize = 64;

/// The most directory entries one size walk will visit before it stops and
/// marks the figure partial. A report must never be the reason a disk-bound
/// machine gets slower.
pub const MAX_WALK_ENTRIES: usize = 200_000;

/// Bytes in a GiB.
const GIB: u64 = 1024 * 1024 * 1024;

/// Seconds in a day.
const DAY_S: i64 = 86_400;

// ---------------------------------------------------------------------------
// The safelist
// ---------------------------------------------------------------------------

/// THE SAFELIST. A class not named here has no spelling that reaches
/// [`apply`], which is the whole of the "refuses any path outside the list"
/// rule: the list is a closed enum, not a configurable string set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// Package builds superseded by the live one. DELEGATED to `aterm pkg gc`.
    AtpkgGc,
    /// Vendor version directories that are not the live symlink's target.
    ClaudeStaleVersions,
    /// Build directories that are stale by their own clocks.
    CargoTargets,
    /// The vendor's own project purge. DELEGATED, and never run from here.
    ClaudePurge,
}

impl Class {
    /// Every class, in report order.
    pub const ALL: &'static [Class] = &[
        Class::AtpkgGc,
        Class::ClaudeStaleVersions,
        Class::CargoTargets,
        Class::ClaudePurge,
    ];

    /// The wire spelling (design §5.5's safelist names, verbatim).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Class::AtpkgGc => "atpkg-gc",
            Class::ClaudeStaleVersions => "claude-stale-versions",
            Class::CargoTargets => "cargo-targets",
            Class::ClaudePurge => "claude-purge",
        }
    }

    /// Parse a class name. Unknown names answer `None`; there is no prefix
    /// matching and no case folding, because a typo that resolved to a
    /// different class would be a grant the owner did not give.
    #[must_use]
    pub fn parse(s: &str) -> Option<Class> {
        Class::ALL.iter().copied().find(|c| c.as_str() == s)
    }

    /// The command that owns removal for this class, when it is not us.
    ///
    /// `None` means this module removes the row itself. `Some` means the row
    /// is REPORTED and [`apply`] refuses it, naming the command — the honest
    /// answer for a tree under another writer's lock.
    #[must_use]
    pub fn delegate(self) -> Option<Delegate> {
        match self {
            Class::AtpkgGc => Some(Delegate::AtermPkgGc),
            Class::ClaudePurge => Some(Delegate::ClaudeProjectPurge),
            Class::ClaudeStaleVersions | Class::CargoTargets => None,
        }
    }

    /// One line of `--help`-shaped prose.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Class::AtpkgGc => "package builds superseded by the live build",
            Class::ClaudeStaleVersions => {
                "vendor version dirs that are not the live symlink target"
            }
            Class::CargoTargets => "build dirs stale by their lock file and their own root",
            Class::ClaudePurge => "the vendor's own project purge (surfaced, never run here)",
        }
    }
}

/// A command that owns a class's removal. Every variant is a tool with its
/// own lock or its own data; none of them is invoked from this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delegate {
    /// `store/` is under atpkg's single-writer lock; its gardener is
    /// `aterm pkg gc`, whose own witness is the live build.
    AtermPkgGc,
    /// The vendor's project history. §5.5: never transcripts, under any flag.
    ClaudeProjectPurge,
}

impl Delegate {
    /// The command to run, verbatim.
    #[must_use]
    pub fn command(self) -> &'static str {
        match self {
            Delegate::AtermPkgGc => "aterm pkg gc",
            Delegate::ClaudeProjectPurge => "claude project purge --dry-run",
        }
    }

    /// Why this module will not run it.
    #[must_use]
    pub fn because(self) -> &'static str {
        match self {
            Delegate::AtermPkgGc => "the store is under atpkg's single-writer lock",
            Delegate::ClaudeProjectPurge => "it reaches the vendor's own project history",
        }
    }
}

// ---------------------------------------------------------------------------
// Knobs
// ---------------------------------------------------------------------------

/// The `[disk]` knobs of design §5.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Below this many free GiB the report warns.
    pub warn_free_gib: u64,
    /// A build directory younger than this is never a candidate.
    pub target_stale_days: i64,
    /// The durable `disk.apply` switch. `false` makes [`plan`] answer
    /// [`Plan::Denied`] for every class, whatever the caller named.
    pub apply: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            warn_free_gib: DEFAULT_WARN_FREE_GIB,
            target_stale_days: DEFAULT_TARGET_STALE_DAYS,
            apply: DEFAULT_APPLY,
        }
    }
}

// ---------------------------------------------------------------------------
// The survey — every fact the decision needs, injected
// ---------------------------------------------------------------------------

/// One candidate build directory, with the facts that decide it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetDir {
    /// Absolute path of the target directory itself.
    pub path: PathBuf,
    /// Size in bytes, as far as the walk got.
    pub bytes: u64,
    /// `true` when the walk hit [`MAX_WALK_ENTRIES`] and stopped.
    pub bytes_partial: bool,
    /// Unix seconds of the lock file's mtime, when there is a lock file.
    pub lock_mtime: Option<i64>,
    /// The newest mtime of any entry at the directory's ROOT.
    pub root_mtime: Option<i64>,
    /// The marker that proves a build tool laid this, when one was found.
    pub marker: Option<String>,
    /// Is a build live in this workspace?
    ///
    /// UNVERIFIED as a process fact and deliberately so: [`scan_target`] sets
    /// it from a clock PROXY (anything at the root touched inside the
    /// threshold reads as live), because the honest process check wants a
    /// lock acquisition this crate will not write — `unsafe` is banned here
    /// and a `ps` subprocess is a sample, not a fact. It is a FIELD rather
    /// than a computation so a host that owns a real check can set it, and
    /// the tie breaks toward `true`: an unknown answer is a live build.
    pub live_build: bool,
}

/// One vendor version directory under `versions/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionDir {
    /// Absolute path.
    pub path: PathBuf,
    /// Size in bytes, as far as the walk got.
    pub bytes: u64,
    /// `true` when the walk hit [`MAX_WALK_ENTRIES`] and stopped.
    pub bytes_partial: bool,
}

/// One build in the package store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreBuild {
    /// The program the build belongs to.
    pub program: String,
    /// The build id.
    pub build: String,
    /// Absolute path of the build's tree.
    pub path: PathBuf,
    /// Size in bytes, as far as the walk got.
    pub bytes: u64,
    /// `true` when the walk hit [`MAX_WALK_ENTRIES`] and stopped.
    pub bytes_partial: bool,
}

/// What made the host re-measure. Two of the three are EVENTS, which is the
/// §5.5 point: the tick is the floor, not the mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The 6 h tick — the one timer the design keeps (§4.2 row 18).
    Tick,
    /// An `appstatus` row reported `kind=update phase=done`.
    UpdateDone,
    /// An `appstatus` row reported a toolchain install finishing.
    ToolchainDone,
    /// The owner typed the verb.
    OnDemand,
}

impl Trigger {
    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Tick => "tick",
            Trigger::UpdateDone => "update-done",
            Trigger::ToolchainDone => "toolchain-done",
            Trigger::OnDemand => "on-demand",
        }
    }
}

/// Everything [`report`] reads. Built by [`scan`] on a real machine and by a
/// literal in every test, so the decision core never touches a disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Survey {
    /// Unix seconds.
    pub now: i64,
    /// What made this measurement happen.
    pub trigger: Trigger,
    /// The volume the free figure is about.
    pub root: PathBuf,
    /// Free bytes, or `None` when the query failed. `None` FAILS OPEN: the
    /// report says `free=unknown` and never warns on a number it does not
    /// have (the same discipline as `atpkg`'s own `freespace`).
    pub free_bytes: Option<u64>,
    /// Candidate build directories.
    pub targets: Vec<TargetDir>,
    /// Vendor version directories.
    pub versions: Vec<VersionDir>,
    /// The live symlink's RESOLVED target, when it resolved.
    pub live_version: Option<PathBuf>,
    /// Builds in the package store.
    pub store: Vec<StoreBuild>,
    /// The live build of each program in the store.
    pub live_builds: BTreeMap<String, String>,
    /// Where transcripts live. Nothing under this path is ever removable,
    /// whatever else says otherwise.
    pub transcripts_root: Option<PathBuf>,
    /// Is the vendor's `claude project purge` available to surface?
    pub purge_available: bool,
}

impl Survey {
    /// An empty survey at `now`. Tests build from this.
    #[must_use]
    pub fn new(now: i64) -> Survey {
        Survey {
            now,
            trigger: Trigger::OnDemand,
            root: PathBuf::from("/"),
            free_bytes: None,
            targets: Vec::new(),
            versions: Vec::new(),
            live_version: None,
            store: Vec::new(),
            live_builds: BTreeMap::new(),
            transcripts_root: None,
            purge_available: false,
        }
    }
}

// ---------------------------------------------------------------------------
// The witness
// ---------------------------------------------------------------------------

/// WHY a row is safe to remove. There is no `Witness::None`: a row with no
/// witness is not a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Witness {
    /// A build directory that is stale by BOTH its clocks and carries a
    /// build tool's own marker.
    StaleBuildDir {
        /// The marker found (`CACHEDIR.TAG` or `.rustc_info.json`).
        marker: String,
        /// Days since the lock file was written.
        lock_age_days: i64,
        /// Days since anything at the root was written.
        root_age_days: i64,
        /// The threshold both cleared.
        threshold_days: i64,
    },
    /// A vendor version directory the live symlink does not point at.
    NotLinkTarget {
        /// The live symlink's resolved target, named so a reader can check.
        live: PathBuf,
    },
    /// A package build superseded by the live build of the same program.
    Superseded {
        /// The program.
        program: String,
        /// The live build that supersedes this one.
        live: String,
    },
    /// The vendor's own command reports it; this module only surfaces it.
    VendorReports {
        /// The command that owns it.
        command: &'static str,
    },
}

impl Witness {
    /// One line naming the evidence, for the text report and the ledger row.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Witness::StaleBuildDir {
                marker,
                lock_age_days,
                root_age_days,
                threshold_days,
            } => format!(
                "{marker} present, lock {lock_age_days}d old, root {root_age_days}d old, threshold {threshold_days}d (removing it costs a rebuild)"
            ),
            Witness::NotLinkTarget { live } => {
                format!("not the live symlink target {}", live.display())
            }
            Witness::Superseded { program, live } => {
                format!("superseded: {program} live build is {live}")
            }
            Witness::VendorReports { command } => format!("surfaced by `{command}`"),
        }
    }

    /// The witness kind, as the JSON row spells it.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Witness::StaleBuildDir { .. } => "stale-build-dir",
            Witness::NotLinkTarget { .. } => "not-link-target",
            Witness::Superseded { .. } => "superseded",
            Witness::VendorReports { .. } => "vendor-reports",
        }
    }
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

/// One reported target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The safelist class this row belongs to.
    pub class: Class,
    /// The path, or — for a delegated row with no single path — the tree the
    /// delegate owns.
    pub path: PathBuf,
    /// Bytes this row would free, as far as the walk got.
    pub bytes: u64,
    /// `true` when [`Row::bytes`] is a floor rather than a figure.
    pub bytes_partial: bool,
    /// Why it is safe to remove.
    pub witness: Witness,
    /// May [`apply`] remove it? A delegated class is `false`, and so is a row
    /// whose own witness was undermined (an unresolvable live symlink).
    pub removable: bool,
    /// Why not, when `removable` is `false`.
    pub blocked: Option<String>,
}

impl Row {
    /// The text-report line.
    #[must_use]
    pub fn line(&self) -> String {
        let size = human_bytes(self.bytes);
        let partial = if self.bytes_partial { "+" } else { "" };
        let state = match &self.blocked {
            Some(why) => format!("blocked: {why}"),
            None => "removable".to_owned(),
        };
        format!(
            "{:<22} {size}{partial:<1} {} — {} [{state}]",
            self.class.as_str(),
            self.path.display(),
            self.witness.describe()
        )
    }
}

/// The whole §5.5 report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Unix seconds the survey was taken at.
    pub now: i64,
    /// What made it happen.
    pub trigger: Trigger,
    /// The volume.
    pub root: PathBuf,
    /// Free bytes, or `None` for a failed query.
    pub free_bytes: Option<u64>,
    /// The warn threshold in bytes.
    pub warn_free_bytes: u64,
    /// Is free space below the threshold? A `None` free figure NEVER warns.
    pub warn: bool,
    /// The rows, class order then path order.
    pub rows: Vec<Row>,
    /// Notes about rows the report deliberately did NOT make — an
    /// unresolvable live symlink, a class with no roots. Data, not warnings.
    pub notes: Vec<String>,
    /// The config that produced it.
    pub config: Config,
}

impl Report {
    /// Rows of one class.
    #[must_use]
    pub fn rows_of(&self, class: Class) -> Vec<&Row> {
        self.rows.iter().filter(|r| r.class == class).collect()
    }

    /// Bytes the removable rows of one class would free.
    #[must_use]
    pub fn reclaimable(&self, class: Class) -> u64 {
        self.rows
            .iter()
            .filter(|r| r.class == class && r.removable)
            .fold(0u64, |a, r| a.saturating_add(r.bytes))
    }

    /// Bytes every removable row would free.
    #[must_use]
    pub fn reclaimable_total(&self) -> u64 {
        self.rows
            .iter()
            .filter(|r| r.removable)
            .fold(0u64, |a, r| a.saturating_add(r.bytes))
    }

    /// The header line.
    #[must_use]
    pub fn headline(&self) -> String {
        let free = match self.free_bytes {
            Some(b) => human_bytes(b),
            None => "unknown".to_owned(),
        };
        format!(
            "OK schema=1 kind=disk root={} free={free} warn_below={} state={} trigger={} rows={} reclaimable={} apply={}",
            self.root.display(),
            human_bytes(self.warn_free_bytes),
            if self.warn { "low" } else { "ok" },
            self.trigger.as_str(),
            self.rows.len(),
            human_bytes(self.reclaimable_total()),
            if self.config.apply { "allowed" } else { "off" },
        )
    }

    /// The whole report as one JSON object.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(1u64));
        o.insert("kind".to_owned(), Value::from("disk".to_owned()));
        o.insert("ts".to_owned(), Value::from(rfc3339_utc(self.now)));
        o.insert(
            "trigger".to_owned(),
            Value::from(self.trigger.as_str().to_owned()),
        );
        o.insert(
            "root".to_owned(),
            Value::from(self.root.display().to_string()),
        );
        o.insert(
            "free_bytes".to_owned(),
            self.free_bytes.map_or(Value::Null, Value::from),
        );
        o.insert(
            "warn_free_bytes".to_owned(),
            Value::from(self.warn_free_bytes),
        );
        o.insert("warn".to_owned(), Value::from(self.warn));
        o.insert("apply_allowed".to_owned(), Value::from(self.config.apply));
        o.insert(
            "reclaimable_bytes".to_owned(),
            Value::from(self.reclaimable_total()),
        );
        o.insert(
            "rows".to_owned(),
            Value::Array(self.rows.iter().map(row_value).collect()),
        );
        o.insert(
            "notes".to_owned(),
            Value::Array(
                self.notes
                    .iter()
                    .map(|n| Value::from(n.clone()))
                    .collect::<Vec<_>>(),
            ),
        );
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| {
            "{\"schema\":1,\"kind\":\"disk\",\"error\":\"unserializable\"}".to_owned()
        })
    }
}

/// One row as JSON, shared by the report and the ledger.
fn row_value(r: &Row) -> Value {
    let mut o = Map::new();
    o.insert("class".to_owned(), Value::from(r.class.as_str().to_owned()));
    o.insert("path".to_owned(), Value::from(path_text(&r.path)));
    o.insert("bytes".to_owned(), Value::from(r.bytes));
    o.insert("bytes_partial".to_owned(), Value::from(r.bytes_partial));
    o.insert(
        "witness".to_owned(),
        Value::from(r.witness.kind().to_owned()),
    );
    o.insert(
        "witness_text".to_owned(),
        Value::from(truncate_bytes(&r.witness.describe(), ROW_TEXT_CAP).to_owned()),
    );
    o.insert("removable".to_owned(), Value::from(r.removable));
    if let Some(why) = &r.blocked {
        o.insert(
            "blocked".to_owned(),
            Value::from(truncate_bytes(why, ROW_TEXT_CAP).to_owned()),
        );
    }
    Value::Object(o)
}

/// A path, bounded, for a row.
fn path_text(p: &Path) -> String {
    truncate_bytes(&p.display().to_string(), ROW_TEXT_CAP).to_owned()
}

// ---------------------------------------------------------------------------
// The report, as a pure function
// ---------------------------------------------------------------------------

/// Build the §5.5 report from `survey`. PURE: no clock, no filesystem, no
/// environment.
#[must_use]
pub fn report(survey: &Survey, config: Config) -> Report {
    let warn_free_bytes = config.warn_free_gib.saturating_mul(GIB);
    let mut rows: Vec<Row> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    // -- atpkg-gc: superseded package builds, delegated -------------------
    let d = Delegate::AtermPkgGc;
    for b in survey.store.iter().take(MAX_ROWS_PER_CLASS) {
        let Some(live) = survey.live_builds.get(&b.program) else {
            notes.push(format!(
                "atpkg-gc: {} has no live build recorded, so no build of it is superseded",
                b.program
            ));
            continue;
        };
        if live == &b.build {
            continue;
        }
        rows.push(Row {
            class: Class::AtpkgGc,
            path: b.path.clone(),
            bytes: b.bytes,
            bytes_partial: b.bytes_partial,
            witness: Witness::Superseded {
                program: b.program.clone(),
                live: live.clone(),
            },
            removable: false,
            blocked: Some(format!("run `{}` — {}", d.command(), d.because())),
        });
    }

    // -- claude-stale-versions --------------------------------------------
    match &survey.live_version {
        Some(live) => {
            for v in survey.versions.iter().take(MAX_ROWS_PER_CLASS) {
                if &v.path == live {
                    continue;
                }
                rows.push(Row {
                    class: Class::ClaudeStaleVersions,
                    path: v.path.clone(),
                    bytes: v.bytes,
                    bytes_partial: v.bytes_partial,
                    witness: Witness::NotLinkTarget { live: live.clone() },
                    removable: true,
                    blocked: None,
                });
            }
        }
        None if !survey.versions.is_empty() => {
            // THE TIE BREAKS SAFE. With no resolved live target, "not the
            // live one" is an assumption: every version directory is listed
            // as blocked rather than silently kept or silently removed.
            for v in survey.versions.iter().take(MAX_ROWS_PER_CLASS) {
                rows.push(Row {
                    class: Class::ClaudeStaleVersions,
                    path: v.path.clone(),
                    bytes: v.bytes,
                    bytes_partial: v.bytes_partial,
                    witness: Witness::NotLinkTarget {
                        live: PathBuf::new(),
                    },
                    removable: false,
                    blocked: Some(
                        "the live symlink did not resolve, so nothing here has a witness"
                            .to_owned(),
                    ),
                });
            }
            notes.push(
                "claude-stale-versions: the live symlink did not resolve — every row is blocked"
                    .to_owned(),
            );
        }
        None => {}
    }

    // -- cargo-targets -----------------------------------------------------
    for t in survey.targets.iter().take(MAX_ROWS_PER_CLASS) {
        match target_witness(t, survey.now, config.target_stale_days) {
            Ok(w) => rows.push(Row {
                class: Class::CargoTargets,
                path: t.path.clone(),
                bytes: t.bytes,
                bytes_partial: t.bytes_partial,
                witness: w,
                removable: true,
                blocked: None,
            }),
            Err(why) => notes.push(format!("cargo-targets: {} — {why}", t.path.display())),
        }
    }

    // -- claude-purge: surfaced, delegated --------------------------------
    if survey.purge_available {
        let d = Delegate::ClaudeProjectPurge;
        rows.push(Row {
            class: Class::ClaudePurge,
            path: survey
                .transcripts_root
                .clone()
                .unwrap_or_else(|| PathBuf::from("-")),
            bytes: 0,
            bytes_partial: false,
            witness: Witness::VendorReports {
                command: d.command(),
            },
            removable: false,
            blocked: Some(format!("run `{}` — {}", d.command(), d.because())),
        });
    }

    rows.sort_by(|a, b| a.class.cmp(&b.class).then_with(|| a.path.cmp(&b.path)));

    let warn = survey.free_bytes.is_some_and(|b| b < warn_free_bytes);
    Report {
        now: survey.now,
        trigger: survey.trigger,
        root: survey.root.clone(),
        free_bytes: survey.free_bytes,
        warn_free_bytes,
        warn,
        rows,
        notes,
        config,
    }
}

/// The witness for one build directory, or the reason it has none.
///
/// Every clause is a REFUSAL: a directory earns a witness by clearing all of
/// them, and the first one it fails is the reason it is not a candidate.
fn target_witness(t: &TargetDir, now: i64, threshold_days: i64) -> Result<Witness, String> {
    if t.live_build {
        return Err("a build is live in this workspace".to_owned());
    }
    let Some(marker) = t.marker.clone() else {
        return Err(format!(
            "no build-tool marker ({CACHEDIR_SIGNATURE} in CACHEDIR.TAG, or {RUSTC_INFO}) — the harness cannot prove it laid this"
        ));
    };
    let Some(lock) = t.lock_mtime else {
        return Err(format!("no {CARGO_LOCK_FILE} to date it by"));
    };
    let Some(root) = t.root_mtime else {
        return Err("the directory's own mtime could not be read".to_owned());
    };
    let lock_age_days = age_days(now, lock);
    let root_age_days = age_days(now, root);
    if lock_age_days < threshold_days {
        return Err(format!(
            "the lock file is {lock_age_days}d old, under the {threshold_days}d threshold"
        ));
    }
    if root_age_days < threshold_days {
        return Err(format!(
            "something at the root is {root_age_days}d old, under the {threshold_days}d threshold"
        ));
    }
    Ok(Witness::StaleBuildDir {
        marker,
        lock_age_days,
        root_age_days,
        threshold_days,
    })
}

/// Whole days between `then` and `now`. A FUTURE mtime answers 0, which reads
/// as "brand new" and so never clears a threshold — the safe direction for a
/// machine whose clock moved.
#[must_use]
pub fn age_days(now: i64, then: i64) -> i64 {
    now.saturating_sub(then).max(0) / DAY_S
}

/// A byte count a person can read. Deliberately coarse: this is a report, and
/// a figure to the byte would imply a precision the bounded walk does not
/// have.
#[must_use]
pub fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

// ---------------------------------------------------------------------------
// Apply — the plan, the guard, the denial
// ---------------------------------------------------------------------------

/// Why an apply, or one path inside it, was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No class was named. This is the DEFAULT, not an error: report-only.
    NoClass,
    /// `disk.apply` is off.
    ApplyOff(Class),
    /// The class is owned by another command.
    Delegated(Class, Delegate),
    /// The path is not on the report for this class.
    NotReported(PathBuf),
    /// The path is on the report but its row is blocked.
    Blocked(PathBuf, String),
    /// The path is, or is under, the transcripts root.
    Transcript(PathBuf),
    /// The path is relative, or walks up through `..`.
    NotAnchored(PathBuf),
    /// The removal itself failed.
    RemoveFailed(PathBuf, String),
}

impl Refusal {
    /// The short reason code the ledger row carries.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::NoClass => "no-class",
            Refusal::ApplyOff(_) => "apply-off",
            Refusal::Delegated(..) => "delegated",
            Refusal::NotReported(_) => "not-reported",
            Refusal::Blocked(..) => "blocked",
            Refusal::Transcript(_) => "transcript",
            Refusal::NotAnchored(_) => "not-anchored",
            Refusal::RemoveFailed(..) => "remove-failed",
        }
    }

    /// The class this refusal is about, when it has one.
    #[must_use]
    pub fn class(&self) -> Option<Class> {
        match self {
            Refusal::ApplyOff(c) | Refusal::Delegated(c, _) => Some(*c),
            _ => None,
        }
    }

    /// The path this refusal is about, when it has one.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Refusal::NotReported(p)
            | Refusal::Blocked(p, _)
            | Refusal::Transcript(p)
            | Refusal::NotAnchored(p)
            | Refusal::RemoveFailed(p, _) => Some(p.as_path()),
            _ => None,
        }
    }

    /// The sentence a person reads.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Refusal::NoClass => format!(
                "report only: name a class to apply ({})",
                Class::ALL
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Refusal::ApplyOff(c) => {
                format!("disk.apply is off, so `{}` removed nothing", c.as_str())
            }
            Refusal::Delegated(c, d) => format!(
                "`{}` is owned by `{}` — {}",
                c.as_str(),
                d.command(),
                d.because()
            ),
            Refusal::NotReported(p) => format!(
                "{} is not on the report for this class — refused",
                p.display()
            ),
            Refusal::Blocked(p, why) => format!("{} is blocked: {why}", p.display()),
            Refusal::Transcript(p) => format!(
                "{} is under the transcripts root — never, under any flag",
                p.display()
            ),
            Refusal::NotAnchored(p) => format!(
                "{} is not an absolute path without `..` — refused",
                p.display()
            ),
            Refusal::RemoveFailed(p, e) => format!("{} could not be removed: {e}", p.display()),
        }
    }
}

/// One DENIAL ROW. Every refusal above becomes one of these; none is dropped.
#[must_use]
pub fn denial_row(now: i64, sid: &str, refusal: &Refusal) -> String {
    let mut o = Map::new();
    o.insert("ts".to_owned(), Value::from(rfc3339_utc(now)));
    o.insert("sid".to_owned(), Value::from(sid.to_owned()));
    o.insert("kind".to_owned(), Value::from("denial".to_owned()));
    o.insert("reason".to_owned(), Value::from(refusal.code().to_owned()));
    if let Some(c) = refusal.class() {
        o.insert("class".to_owned(), Value::from(c.as_str().to_owned()));
    }
    if let Some(p) = refusal.path() {
        o.insert("path".to_owned(), Value::from(path_text(p)));
    }
    o.insert(
        "text".to_owned(),
        Value::from(truncate_bytes(&refusal.describe(), ROW_TEXT_CAP).to_owned()),
    );
    aterm_json::to_string(&Value::Object(o))
        .unwrap_or_else(|_| "{\"kind\":\"denial\",\"reason\":\"unserializable\"}".to_owned())
}

/// One REMOVAL ROW. The witness travels WITH it: a row that says what was
/// removed without saying why it was safe is not a record, it is a receipt.
#[must_use]
pub fn removal_row(now: i64, sid: &str, row: &Row) -> String {
    let mut o = Map::new();
    o.insert("ts".to_owned(), Value::from(rfc3339_utc(now)));
    o.insert("sid".to_owned(), Value::from(sid.to_owned()));
    o.insert("kind".to_owned(), Value::from("removed".to_owned()));
    let Value::Object(inner) = row_value(row) else {
        return "{\"kind\":\"removed\",\"reason\":\"unserializable\"}".to_owned();
    };
    for (k, v) in inner {
        o.insert(k, v);
    }
    aterm_json::to_string(&Value::Object(o))
        .unwrap_or_else(|_| "{\"kind\":\"removed\",\"reason\":\"unserializable\"}".to_owned())
}

/// One REPORT ROW, for the durable record of a measurement.
#[must_use]
pub fn report_row(now: i64, sid: &str, rep: &Report) -> String {
    let mut o = Map::new();
    o.insert("ts".to_owned(), Value::from(rfc3339_utc(now)));
    o.insert("sid".to_owned(), Value::from(sid.to_owned()));
    o.insert("kind".to_owned(), Value::from("report".to_owned()));
    o.insert(
        "trigger".to_owned(),
        Value::from(rep.trigger.as_str().to_owned()),
    );
    o.insert(
        "free_bytes".to_owned(),
        rep.free_bytes.map_or(Value::Null, Value::from),
    );
    o.insert("warn".to_owned(), Value::from(rep.warn));
    o.insert(
        "rows".to_owned(),
        Value::from(u64::try_from(rep.rows.len()).unwrap_or(u64::MAX)),
    );
    o.insert(
        "reclaimable_bytes".to_owned(),
        Value::from(rep.reclaimable_total()),
    );
    aterm_json::to_string(&Value::Object(o))
        .unwrap_or_else(|_| "{\"kind\":\"report\",\"reason\":\"unserializable\"}".to_owned())
}

/// What an apply would do. PURE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// No class named: the default, and it removes nothing.
    ReportOnly(Refusal),
    /// A class was named and refused.
    Denied(Refusal),
    /// A class was named and these rows would go.
    Remove(Class, Vec<Row>),
}

/// Decide what `class` would do against `rep`. PURE: no filesystem.
///
/// `None` is [`Plan::ReportOnly`] — the design's "report-only by default",
/// stated as the shape of the function rather than as a flag somewhere.
#[must_use]
pub fn plan(rep: &Report, class: Option<Class>) -> Plan {
    let Some(class) = class else {
        return Plan::ReportOnly(Refusal::NoClass);
    };
    if let Some(d) = class.delegate() {
        return Plan::Denied(Refusal::Delegated(class, d));
    }
    if !rep.config.apply {
        return Plan::Denied(Refusal::ApplyOff(class));
    }
    let rows: Vec<Row> = rep
        .rows
        .iter()
        .filter(|r| r.class == class && r.removable)
        .cloned()
        .collect();
    Plan::Remove(class, rows)
}

/// THE SECOND FENCE. `path` may be removed under `class` only if every one of
/// these holds; anything else is a [`Refusal`], and the caller journals it.
///
/// This repeats work [`plan`] already did, on purpose: [`plan`]'s answer is a
/// list the caller could edit, and the thing that finally reaches the
/// filesystem is a path. This function is what that path is checked against.
///
/// # Errors
///
/// The path is relative or contains `..`; it is under the transcripts root;
/// it is not a removable row of `class` on this report.
pub fn guard(
    rep: &Report,
    survey_transcripts: Option<&Path>,
    class: Class,
    path: &Path,
) -> Result<(), Refusal> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(Refusal::NotAnchored(path.to_path_buf()));
    }
    if let Some(t) = survey_transcripts
        && (path == t || path.starts_with(t))
    {
        return Err(Refusal::Transcript(path.to_path_buf()));
    }
    let Some(row) = rep.rows.iter().find(|r| r.class == class && r.path == path) else {
        return Err(Refusal::NotReported(path.to_path_buf()));
    };
    if !row.removable {
        return Err(Refusal::Blocked(
            path.to_path_buf(),
            row.blocked.clone().unwrap_or_else(|| "blocked".to_owned()),
        ));
    }
    Ok(())
}

/// What an apply DID.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Applied {
    /// The rows that went.
    pub removed: Vec<Row>,
    /// Every refusal, in the order they happened.
    pub denials: Vec<Refusal>,
    /// Bytes the removed rows were reported at.
    pub freed_bytes: u64,
}

impl Applied {
    /// The one summary line.
    #[must_use]
    pub fn headline(&self) -> String {
        format!(
            "OK schema=1 kind=disk-apply removed={} freed={} denied={}",
            self.removed.len(),
            human_bytes(self.freed_bytes),
            self.denials.len()
        )
    }
}

/// Carry out `class` against `rep`, removing through `remove`.
///
/// `remove` is INJECTED so this function is testable without a disk and so
/// the removal primitive is the caller's choice; [`guard`] runs before every
/// single call to it, and a guard refusal is recorded rather than returned,
/// because one refused path must not abandon the rest of the grant.
pub fn apply(
    rep: &Report,
    transcripts: Option<&Path>,
    class: Option<Class>,
    remove: &mut dyn FnMut(&Path) -> std::io::Result<()>,
) -> Applied {
    let mut done = Applied::default();
    let rows = match plan(rep, class) {
        Plan::ReportOnly(r) | Plan::Denied(r) => {
            done.denials.push(r);
            return done;
        }
        Plan::Remove(_, rows) => rows,
    };
    // `class` is Some on this path: `plan` answered `Remove`.
    let Some(class) = class else {
        done.denials.push(Refusal::NoClass);
        return done;
    };
    for row in rows {
        if let Err(r) = guard(rep, transcripts, class, &row.path) {
            done.denials.push(r);
            continue;
        }
        match remove(&row.path) {
            Ok(()) => {
                done.freed_bytes = done.freed_bytes.saturating_add(row.bytes);
                done.removed.push(row);
            }
            Err(e) => done
                .denials
                .push(Refusal::RemoveFailed(row.path.clone(), e.to_string())),
        }
    }
    done
}

// ---------------------------------------------------------------------------
// The scan — the one impure half
// ---------------------------------------------------------------------------

/// Where to look. Every path is injected; this module reads no environment
/// variable and derives no path from a name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Roots {
    /// The volume the free-space figure is about.
    pub volume: Option<PathBuf>,
    /// The vendor's `versions/` directory.
    pub versions_dir: Option<PathBuf>,
    /// The live `claude` symlink.
    pub live_link: Option<PathBuf>,
    /// The package store's root (`<prefix>/store`). READ ONLY: every row it
    /// produces is delegated to `aterm pkg gc`, so nothing here ever takes
    /// the store lock or writes a byte under it.
    pub store_dir: Option<PathBuf>,
    /// Candidate build directories, named by the caller.
    pub targets: Vec<PathBuf>,
    /// Where transcripts live.
    pub transcripts: Option<PathBuf>,
}

/// Measure one machine into a [`Survey`]. The ONE function here that touches
/// a filesystem; everything else is a pure function of what it returns.
///
/// Free space is left to the caller (`free`), because the statvfs edge lives
/// in `atpkg` and this crate does not depend on it. `None` fails OPEN.
#[must_use]
pub fn scan(
    roots: &Roots,
    now: i64,
    trigger: Trigger,
    config: Config,
    free_bytes: Option<u64>,
) -> Survey {
    let mut s = Survey::new(now);
    s.trigger = trigger;
    s.free_bytes = free_bytes;
    s.transcripts_root = roots.transcripts.clone();
    if let Some(v) = &roots.volume {
        s.root = v.clone();
    }
    // BOTH SIDES ARE CANONICAL or neither is compared. `report` decides the
    // whole class by a path equality, and on macOS a symlink resolves
    // `/var` to `/private/var`, so comparing a resolved link against an
    // unresolved listing would make the LIVE version read as stale — the one
    // mistake in this class that costs the owner their working install.
    let versions_root = roots
        .versions_dir
        .as_ref()
        .and_then(|d| std::fs::canonicalize(d).ok());
    s.live_version = roots
        .live_link
        .as_ref()
        .and_then(|l| std::fs::canonicalize(l).ok())
        // The link points at the executable INSIDE a version directory on a
        // native install; the version directory is the ancestor whose parent
        // is the versions root, and the link's own target when there is no
        // such ancestor.
        .map(|p| version_dir_of(&p, versions_root.as_deref()));
    if let Some(dir) = &versions_root {
        s.versions = read_children(dir)
            .into_iter()
            // A child that will not canonicalize has no comparable identity,
            // so it gets no row rather than an unchecked one.
            .filter_map(|p| std::fs::canonicalize(&p).ok())
            .take(MAX_ROWS_PER_CLASS)
            .map(|path| {
                let (bytes, bytes_partial) = dir_bytes(&path);
                VersionDir {
                    path,
                    bytes,
                    bytes_partial,
                }
            })
            .collect();
    }
    if let Some(store) = &roots.store_dir {
        scan_store(store, &mut s);
    }
    for t in roots.targets.iter().take(MAX_ROWS_PER_CLASS) {
        if let Some(td) = scan_target(t, now, config.target_stale_days) {
            s.targets.push(td);
        }
    }
    s
}

/// Read `<prefix>/store` into [`Survey::store`] and [`Survey::live_builds`].
///
/// The layout is `<store>/<program>/<build>/` with a `current` SYMLINK naming
/// the live build (MEASURED on this machine 2026-09-21:
/// `store/ty/current -> …/store/ty/3007` beside `2984/`, `3007/` and their
/// `.ready` files). A program whose `current` does not resolve contributes
/// NO live build, which makes every one of its builds unsupersedable — the
/// safe direction, and the reason `report` emits a note instead of rows.
fn scan_store(store: &Path, s: &mut Survey) {
    for program_dir in read_children(store).into_iter().take(MAX_ROWS_PER_CLASS) {
        let Some(program) = program_dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Ok(live) = std::fs::canonicalize(program_dir.join("current"))
            && let Some(build) = live.file_name().and_then(|n| n.to_str())
        {
            s.live_builds.insert(program.to_owned(), build.to_owned());
        }
        for build_dir in read_children(&program_dir) {
            if !build_dir.is_dir() {
                continue;
            }
            let Some(build) = build_dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if build == "current" {
                continue;
            }
            let (bytes, bytes_partial) = dir_bytes(&build_dir);
            s.store.push(StoreBuild {
                program: program.to_owned(),
                build: build.to_owned(),
                path: build_dir.clone(),
                bytes,
                bytes_partial,
            });
        }
    }
}

/// Which directory under `versions_dir` a resolved link target names.
fn version_dir_of(resolved: &Path, versions_dir: Option<&Path>) -> PathBuf {
    let Some(root) = versions_dir else {
        return resolved.to_path_buf();
    };
    let mut p = resolved;
    while let Some(parent) = p.parent() {
        if parent == root {
            return p.to_path_buf();
        }
        p = parent;
    }
    resolved.to_path_buf()
}

/// One build directory's facts, or `None` when the path is not a directory.
#[must_use]
pub fn scan_target(path: &Path, now: i64, threshold_days: i64) -> Option<TargetDir> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_dir() {
        return None;
    }
    let marker = if cachedir_tag_signed(&path.join("CACHEDIR.TAG")) {
        Some("CACHEDIR.TAG".to_owned())
    } else if path.join(RUSTC_INFO).exists() {
        Some(RUSTC_INFO.to_owned())
    } else {
        None
    };
    let lock_mtime = mtime_of(&path.join(CARGO_LOCK_FILE));
    // The ROOT's newest mtime, over the directory's own entries only — not a
    // recursive walk, which on a 48 GiB tree would be a minute of I/O to
    // learn something the top level already says.
    let mut root_mtime = mtime_of(path);
    for child in read_children(path) {
        if let Some(m) = mtime_of(&child) {
            root_mtime = Some(root_mtime.map_or(m, |c: i64| c.max(m)));
        }
    }
    // THE PROXY, stated where it is computed: anything at the root touched
    // inside the threshold reads as a live build. UNVERIFIED as a process
    // fact; the tie breaks toward `true`.
    let live_build = root_mtime.is_none_or(|m| age_days(now, m) < threshold_days)
        || lock_mtime.is_some_and(|m| age_days(now, m) < threshold_days);
    let (bytes, bytes_partial) = dir_bytes(path);
    Some(TargetDir {
        path: path.to_path_buf(),
        bytes,
        bytes_partial,
        lock_mtime,
        root_mtime,
        marker,
        live_build,
    })
}

/// Does this `CACHEDIR.TAG` carry the specification's signature? The file is
/// read BOUNDED (the signature is the first line) and a file that is not a
/// regular file answers `false`.
fn cachedir_tag_signed(p: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(p) else {
        return false;
    };
    if !meta.is_file() || meta.len() > 4096 {
        return false;
    }
    std::fs::read_to_string(p)
        .map(|t| t.starts_with(CACHEDIR_SIGNATURE))
        .unwrap_or(false)
}

/// Unix-seconds mtime of `p`, or `None`.
fn mtime_of(p: &Path) -> Option<i64> {
    let m = std::fs::symlink_metadata(p).ok()?.modified().ok()?;
    match m.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).ok(),
        // Before the epoch: old, which is the direction that MATTERS here,
        // but 0 is the honest floor rather than a negative guess.
        Err(_) => Some(0),
    }
}

/// The immediate children of `dir`, sorted, or empty.
fn read_children(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .take(MAX_WALK_ENTRIES)
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    out.sort();
    out
}

/// Bytes under `dir`, and whether the walk was cut short.
///
/// Iterative, never follows a symlink, and stops at [`MAX_WALK_ENTRIES`].
#[must_use]
pub fn dir_bytes(dir: &Path) -> (u64, bool) {
    let mut total: u64 = 0;
    let mut seen = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            seen += 1;
            if seen > MAX_WALK_ENTRIES {
                return (total, true);
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    (total, false)
}

/// Free bytes on the volume holding `path`, or `None` on any failure.
///
/// `None` FAILS OPEN, exactly as `atpkg`'s `freespace::available_bytes` does:
/// a report that cannot read the disk says `free=unknown` and warns about
/// nothing, because a cleanup prompted by a query error is a cleanup nobody
/// asked for.
///
/// WHY `df -Pk` AND NOT `statvfs`. std has no free-space call; `statvfs`
/// needs `unsafe`, which this crate does not write; and the one safe shipped
/// spelling, `atpkg`'s `platform::volume_free_bytes`, sits behind a crate
/// this one deliberately does not depend on. `crates/aterm-verify/src/disk.rs`
/// settled the same question the same way and its `parse_df` is the reference
/// for [`parse_df_kb`] below — it is not CALLED because `aterm-verify` is the
/// gate, dependency-free on purpose and not in the shipped graph.
///
/// This runs ONE subprocess per report. It is a measurement, not a sample:
/// nothing here loops, and §5.5's tick belongs to the host.
///
/// It is also BOUNDED. `df` walks the mount table, and a stale NFS or SMB
/// mount under `path` makes it block for as long as that server stays gone —
/// a deadline-free `output()` would wedge the whole report there. A
/// measurement that cannot return is worse than `free=unknown`, which this
/// function already answers safely, so [`DF_BUDGET`] is the deadline and the
/// shipped bounded runner ([`super::align::capture_bounded`]) is what enforces
/// it.
#[must_use]
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut cmd = std::process::Command::new("df");
    cmd.arg("-Pk").arg(path);
    let got = super::align::capture_bounded(cmd, DF_BUDGET, None, MAX_DF_STDOUT).ok()?;
    if !got.success() {
        return None;
    }
    parse_df_kb(&String::from_utf8_lossy(&got.stdout))
}

/// The deadline on the one `df` a report runs. TARGET, not measured: a local
/// volume answers in single-digit milliseconds, and anything that takes
/// longer than this is a mount that is not going to answer.
pub const DF_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// The byte cap on `df` output. `df -Pk <path>` prints a header and one row;
/// the cap is what keeps a pathological filesystem list out of memory.
pub const MAX_DF_STDOUT: usize = 64 * 1024;

/// The `Available` column of `df -Pk` output, in bytes.
///
/// ANCHORED ON `Capacity`, NEVER ON A FIELD COUNT — the lesson
/// `aterm_verify::disk::parse_df` records: `-P` fixes the column ORDER
/// (`Filesystem 1024-blocks Used Available Capacity Mounted on`) and nothing
/// else, and on macOS `df -Pk /System/Volumes/Data/home` prints seven fields
/// because autofs names the source `map auto_home`. Read by position the
/// fourth field is `Used`, which is a plausible WRONG number. `Capacity` is
/// the one percentage cell and `Available` is the field before it.
///
/// An unreadable row answers `None`, never a guess.
#[must_use]
pub fn parse_df_kb(text: &str) -> Option<u64> {
    let row = text.lines().nth(1)?;
    let fields: Vec<&str> = row.split_whitespace().collect();
    let capacity = fields.iter().position(|f| is_percentage(f))?;
    let kib: u64 = fields.get(capacity.checked_sub(1)?)?.parse().ok()?;
    kib.checked_mul(1024)
}

/// A `df` `Capacity` cell: digits then `%`, and nothing else — so a mount
/// name that merely CONTAINS a `%` cannot be mistaken for the anchor.
fn is_percentage(field: &str) -> bool {
    field
        .strip_suffix('%')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Remove one directory tree, for [`apply`]'s `remove` argument.
///
/// It refuses a symlink outright: a `remove_dir_all` through a symlink would
/// reach a tree no witness was ever taken of.
///
/// # Errors
///
/// The path is a symlink, or the removal failed.
pub fn remove_tree(p: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(p)?;
    if meta.is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to remove through a symlink",
        ));
    }
    if meta.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

#[path = "disk_tests.rs"]
#[cfg(test)]
mod tests;
