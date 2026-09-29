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
//! # The one automatic grant: below the floor, build caches, oldest first
//!
//! [`auto_plan`] is the only removal nobody names at the moment it happens:
//! when free space is below `disk.auto_free_gib` ([`DEFAULT_AUTO_FREE_GIB`],
//! decided 2026-09-25 under the owner's standing direction), the host's tick
//! reclaims the removable [`Class::CargoTargets`] rows — each still behind its
//! witness and [`guard`] — and nothing of any other class. Above the floor,
//! or on a free figure nobody could read, it plans nothing. Below it, in
//! LEAST-RECENTLY-USED order (decided 2026-09-28: at the idle window alone
//! the tick freed nothing, measured 2026-09-27 and -28, because every agent
//! builds in its checkout daily):
//!
//! 1. every IDLE profile's `incremental/` (unwritten for `target_stale_days`)
//!    in each witnessed build directory, through [`target::reclaim`] — all of
//!    them, as before: they are the least recently used there are;
//! 2. then, one PROFILE at a time and oldest LAST USED first, a profile
//!    inside the window ([`Witness::LeastRecentlyUsed`], through
//!    [`target::reclaim_lru`]) — free space MEASURED AGAIN before each, and
//!    the pass stops as soon as it is back at the floor plus
//!    [`PRESSURE_MARGIN_GIB`] ([`Config::pressure_target_bytes`]), or once
//!    the bytes these rows released, as their unlinks counted them, cover
//!    what free space was short of that at the first of them (a figure a
//!    local snapshot keeps from rising must not cost every profile), or on
//!    an unreadable figure ([`StopBy`]). A profile a build holds is never one
//!    (cargo's locks, taken again for the delete), nor one on another volume
//!    than the one measured.
//!
//! No reclaim, idle or pressure, takes a profile written into within
//! [`PRESSURE_MIN_IDLE_S`] (ten minutes: a build that just finished is
//! usually followed by another) — a rule only `target_stale_days = 0` could
//! otherwise reach in the idle pass. The idle rows are taken WHOLE, with no
//! re-measure: the stops above are the pressure pass's.
//!
//! The owner's verb applies the same rows: `--apply cargo-targets` takes the
//! idle profiles at any free figure, and below the floor the pressure rows
//! too, under the same stop.
//!
//! # Every row carries the witness that makes it safe to remove
//!
//! A row with no witness is not a candidate; it is a note. The three classes
//! the design names each have one, and each witness is a MEASURABLE fact
//! about the directory rather than a rule about its name:
//!
//! * [`Class::CargoTargets`] — a build directory that carries a marker only
//!   a build tool writes ([`CACHEDIR_SIGNATURE`] or `.rustc_info.json`),
//!   holds no source at its root, whose cargo profiles are all known (found
//!   at any depth by one bounded walk), and at least one of whose profiles is
//!   IDLE — what a compile writes (`deps/`, `.fingerprint/`, `build/`,
//!   `examples/`) unwritten for `target_stale_days` (default one day) —
//!   holds a non-empty `incremental/`, and has no build holding its locks
//!   ([`target::target_witness`]). What its removal takes is ONLY those idle
//!   profiles' `incremental/`, each deleted while this process holds every
//!   one of cargo's build locks of that profile ([`target::reclaim`]). Below
//!   the floor a PROFILE inside the window is a row of its own
//!   ([`Witness::LeastRecentlyUsed`]: the same fences, no build holding its
//!   locks, its last use and its rank oldest first), taken the same way
//!   ([`target::reclaim_lru`]). A build directory named inside another is
//!   walked by both, and each of its profiles is decided ONCE, in both
//!   passes, by the innermost named directory holding it (`speaker`): its
//!   row, or its note keeping the profile — the outer directory's row
//!   neither counts such a profile nor reclaims it. Never
//!   the directory, `deps/`, `build/`, `.fingerprint/`, an uplifted binary,
//!   or anything else. The row SAYS what it costs: a slower next build.
//!   "Nothing is using it" is cargo's own answer, not a clock proxy: this
//!   file once said the honest check "wants a lock acquisition this crate
//!   will not write — `unsafe` is banned here". That was wrong: std's
//!   `File::try_lock` takes cargo's `flock` with no `unsafe`, and the reclaim
//!   holds every one of a profile's locks while it deletes.
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
//! sleep, no loop and no cadence in this file. The window's harness host
//! (`aterm-gui` `harness_host.rs`) keeps it, and hands each tick to
//! [`super::cli::disk_tick`] ([`Trigger::Tick`]).
//!
//! # Every removal is journalled before and after
//!
//! [`apply`] and [`apply_auto`] tell their caller each [`Step`] as it
//! happens: the intent BEFORE a row's removal starts, its outcome (what went,
//! the bytes actually released, what was skipped and why, and every delete
//! error) after, and every refusal. The host writes each step to its journal
//! as it comes, so a pass cut short leaves the intent on record.
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested on faithful cargo
//! layouts (locks in each profile, none at the root), including the idle
//! profile whose `incremental/` alone goes, a profile a build holds locked
//! (skipped), a symlinked path component (refused), the bytes an unlink
//! released, the journal's intent row surviving a remover that panics, the
//! outside-the-safelist path that is refused with a denial row, the
//! report-only default, the automatic floor, and the pressure pass (every
//! profile recent: the oldest unlocked one goes first, the pass stops once
//! free space is back, a locked or just-built one is never taken, and a
//! re-measure that never rises stops it once the counted bytes cover what
//! was short). The re-measure is the host's `statvfs` (the verb's `df`); how
//! soon APFS shows an unlink in it was not measured, and a local snapshot
//! holding the blocks keeps the figure low — then the COUNTED stop ends the
//! pass, and since an APFS clone's shared blocks are counted although the
//! clone keeps them, it can end it early (the next tick looks again).
//! Deeper tiers (`deps/`,
//! `build/`, `.fingerprint/`) are not reclaimed: they need run-phase
//! protection ([`target`]'s docs). The `--diagnose` line of §5.5 has no
//! host.

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

/// `disk.target_stale_days` (design §5.5): ONE day. A profile unwritten for
/// that long is IDLE: the owner's `--apply cargo-targets` takes its
/// `incremental/` at any free figure, and below [`DEFAULT_AUTO_FREE_GIB`] the
/// tick takes every idle one first, whole, before it reaches any recent one.
/// A profile inside the window is kept above the floor and, below it, taken
/// only in least-recently-used order until the volume is back above the
/// floor (the module docs) — since 2026-09-28 the window no longer keeps a
/// profile out of that pass. The unit is `incremental/` alone, which is free
/// to lose (cargo rebuilds it; the uplifted binaries, `deps/` and
/// `.fingerprint/` stay). It was 14, a window sized for the whole-directory
/// removal this replaced; measured 2026-09-27, the agents' build directories
/// were never idle that long. An explicit setting is read as written.
pub const DEFAULT_TARGET_STALE_DAYS: i64 = 1;

/// `disk.apply` (design §5.5): the owner's verb removes nothing until this is
/// on. The automatic floor below is its own, narrower grant.
pub const DEFAULT_APPLY: bool = false;

/// `disk.auto_free_gib`: below this many free GiB the host's tick reclaims the
/// witnessed build directories' idle caches by itself — [`Class::CargoTargets`]
/// only, each row carrying its witness ([`auto_plan`]). `0` turns it off.
/// Decided 2026-09-25 under the owner's standing direction (self-healing,
/// batteries included): a full disk stalled a worker for 21.8 h
/// (`docs/AUDIT-claude-harness-2026-09-23.md`), and what this removes is the
/// `incremental/` of a cargo profile idle past `target_stale_days`, taken
/// under cargo's own locks — never the directory, never a transcript, never
/// another class. Below it, profiles inside that window go too, least
/// recently used first, until free space is back at this floor plus
/// [`PRESSURE_MARGIN_GIB`].
pub const DEFAULT_AUTO_FREE_GIB: u64 = 10;

/// How far above the floor the pressure pass reclaims before it stops: ONE
/// GiB, a tenth of the default floor. A pass that stopped exactly at the
/// floor would leave the next build's writes to push the volume under it
/// again, six hours before the next tick looks; more than a little would
/// take caches that were not needed. A TARGET, not a measurement.
pub const PRESSURE_MARGIN_GIB: u64 = 1;

/// The `CACHEDIR.TAG` signature every cargo-compatible build tool writes into
/// a target directory. VERIFIED against the Cache Directory Tagging
/// Specification's fixed signature line, which cargo emits verbatim; it is
/// the marker that makes "a build tool laid this" a fact rather than a guess
/// about the directory's name.
pub const CACHEDIR_SIGNATURE: &str = "Signature: 8a477f597d28d172789f06886806bc55";

/// The other marker a cargo target directory carries.
pub const RUSTC_INFO: &str = ".rustc_info.json";

#[path = "disk_target.rs"]
pub mod target;

pub use target::{CARGO_LOCKS, INCREMENTAL, Judge, PRESSURE_MIN_IDLE_S, Profile, TargetDir};

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
    /// Build directories whose idle cargo profiles' `incremental/` goes —
    /// and below the floor, recent profiles', least recently used first.
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
    #[cfg(test)]
    #[must_use]
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Class::AtpkgGc => "package builds superseded by the live build",
            Class::ClaudeStaleVersions => {
                "vendor version dirs that are not the live symlink target"
            }
            Class::CargoTargets => {
                "cargo profiles' incremental caches (idle ones; below the floor, least recently used first), under cargo's locks"
            }
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
    /// A cargo profile a compile wrote into (its `deps/`, `.fingerprint/`,
    /// `build/` or `examples/`) more recently than this many days is not
    /// idle: its `incremental/` is kept above the automatic floor, and below
    /// it is taken only in least-recently-used order, after every idle one,
    /// until free space is back above the floor ([`auto_plan`]). A build
    /// directory of any age is a candidate through an idle profile. `0`
    /// makes every profile idle, except one written into within
    /// [`PRESSURE_MIN_IDLE_S`]: no reclaim takes that one.
    pub target_stale_days: i64,
    /// The durable `disk.apply` switch. `false` makes [`plan`] answer
    /// [`Plan::Denied`] for every class, whatever the caller named.
    pub apply: bool,
    /// Below this many free GiB the host's tick applies [`auto_plan`]; `0`
    /// is off ([`DEFAULT_AUTO_FREE_GIB`]).
    pub auto_free_gib: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            warn_free_gib: DEFAULT_WARN_FREE_GIB,
            target_stale_days: DEFAULT_TARGET_STALE_DAYS,
            apply: DEFAULT_APPLY,
            auto_free_gib: DEFAULT_AUTO_FREE_GIB,
        }
    }
}

/// Every `[disk]` key of `aterm.toml`, in the order the help lists them. The
/// window's settings checker knows each one (aterm-gui's config language pins
/// the two lists), so none of them reads there as "unknown".
pub const KEYS: &[&str] = &[
    "apply",
    "warn_free_gib",
    "target_stale_days",
    "auto_free_gib",
];

impl Config {
    /// The shipped default of `[disk] <key>` as a settings help line shows it
    /// (`None` for a key that is not in [`KEYS`]).
    #[must_use]
    pub fn default_shown(key: &str) -> Option<String> {
        let d = Config::default();
        Some(match key {
            "apply" => d.apply.to_string(),
            "warn_free_gib" => d.warn_free_gib.to_string(),
            "target_stale_days" => d.target_stale_days.to_string(),
            "auto_free_gib" => format!("{} (0: off)", d.auto_free_gib),
            _ => return None,
        })
    }

    /// How a NEGATIVE `[disk] <key>` is read, for the settings checker to say
    /// on the line: a floor below zero is never crossed, so `auto_free_gib`
    /// reads it as `0`, off (never as the default, which would leave the
    /// removal on for a person who wrote `-1` to stop it); the other two fall
    /// back to their defaults, the safe side for a warning threshold and for
    /// a stale window (a negative one would make every directory stale).
    #[must_use]
    pub fn negative_reading(key: &str) -> Option<String> {
        let d = Config::default();
        Some(match key {
            "auto_free_gib" => "0: the automatic removal is off".to_owned(),
            "warn_free_gib" => format!("the default, {}", d.warn_free_gib),
            "target_stale_days" => format!("the default, {}", d.target_stale_days),
            _ => return None,
        })
    }

    /// Whether `free` bytes is below the automatic floor. An unknown figure
    /// never is (it FAILS OPEN: nothing is removed on a number nobody read).
    #[must_use]
    pub fn below_auto_floor(&self, free: Option<u64>) -> bool {
        self.auto_free_gib > 0 && free.is_some_and(|f| f < self.auto_free_gib.saturating_mul(GIB))
    }

    /// The free bytes the pressure pass reclaims up to before it stops: the
    /// floor plus [`PRESSURE_MARGIN_GIB`].
    #[must_use]
    pub fn pressure_target_bytes(&self) -> u64 {
        self.auto_free_gib
            .saturating_add(PRESSURE_MARGIN_GIB)
            .saturating_mul(GIB)
    }
}

// ---------------------------------------------------------------------------
// The survey — every fact the decision needs, injected
// ---------------------------------------------------------------------------

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

/// What made the host re-measure: the owner's verb, or the window's harness
/// host on §5.5's one timer (a free-space figure has no event to hang on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The owner typed the verb.
    OnDemand,
    /// The host's tick.
    Tick,
}

impl Trigger {
    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::OnDemand => "on-demand",
            Trigger::Tick => "tick",
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
    /// The device of [`Self::root`], when it could be read: below the floor
    /// only a build directory on it gets pressure rows (reclaiming one
    /// anywhere else frees nothing here).
    pub volume_device: Option<u64>,
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
            volume_device: None,
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
    /// A build directory a build tool laid, with idle cargo profiles whose
    /// `incremental/` no build holds ([`target::target_witness`]).
    StaleBuildDir {
        /// The marker found (`CACHEDIR.TAG` or `.rustc_info.json`).
        marker: String,
        /// How many of its profiles are candidates.
        profiles: usize,
        /// Days since the most recently used candidate's caches were written.
        idle_days: i64,
        /// The threshold every candidate cleared.
        threshold_days: i64,
        /// The build directories named inside it, each of which decides its
        /// own profiles (the report's `speaker`): the row neither counts nor
        /// reclaims a profile in one of them ([`target::reclaim_leaving`]).
        leaves: Vec<PathBuf>,
    },
    /// Below the floor, ONE cargo profile inside the idle window, taken in
    /// least-recently-used order until free space is back above the floor
    /// (the module docs). The row's path is the PROFILE's.
    LeastRecentlyUsed {
        /// The build directory's marker.
        marker: String,
        /// The build directory the profile is in (canonical).
        build_dir: PathBuf,
        /// Its LAST USED, as the survey read it (unix seconds).
        last_used: i64,
        /// Seconds since then, at the survey.
        age_s: i64,
        /// The idle window it is inside.
        threshold_days: i64,
        /// The floor free space fell below, in GiB.
        floor_gib: u64,
        /// The free GiB the pass stops at.
        until_gib: u64,
        /// The device measured: the reclaim refuses the build directory on
        /// any other.
        device: u64,
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
                profiles,
                idle_days,
                threshold_days,
                leaves,
            } => format!(
                "{marker} present, {profiles} idle profile{} (caches unwritten {idle_days}d, threshold {threshold_days}d), no build holding its locks — only incremental/ goes (costs a slower next build){}",
                if *profiles == 1 { "" } else { "s" },
                leaving(leaves)
            ),
            Witness::LeastRecentlyUsed {
                marker,
                age_s,
                threshold_days,
                floor_gib,
                until_gib,
                ..
            } => format!(
                "{marker} present, profile last written {} ago (inside the {threshold_days}d idle window), no build holding its locks — free space is under the {floor_gib} GiB floor, so the least recently used profiles give up incremental/ first, until {until_gib} GiB are free (costs a slower next build)",
                target::human_age(*age_s)
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
            Witness::LeastRecentlyUsed { .. } => "least-recently-used",
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
    #[cfg(test)]
    #[must_use]
    pub(crate) fn rows_of(&self, class: Class) -> Vec<&Row> {
        self.rows.iter().filter(|r| r.class == class).collect()
    }

    /// Bytes the removable rows of one class would free.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn reclaimable(&self, class: Class) -> u64 {
        self.rows
            .iter()
            .filter(|r| r.class == class && r.removable)
            .fold(0u64, |a, r| a.saturating_add(r.bytes))
    }

    /// Bytes every removable row but a pressure row would free: what an
    /// apply takes WHOLE. The pressure rows are
    /// [`Self::reclaimable_under_pressure`], since the pass takes only as
    /// many of them as free space needs.
    #[must_use]
    pub fn reclaimable_total(&self) -> u64 {
        self.rows
            .iter()
            .filter(|r| r.removable && !is_pressure(r))
            .fold(0u64, |a, r| a.saturating_add(r.bytes))
    }

    /// Bytes the pressure rows would free if the pass took every one: an
    /// UPPER BOUND, since it stops once free space is back at
    /// [`Config::pressure_target_bytes`] (or the bytes it released cover
    /// what it was short). `0` at or above the floor.
    #[must_use]
    pub fn reclaimable_under_pressure(&self) -> u64 {
        self.rows
            .iter()
            .filter(|r| r.removable && is_pressure(r))
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
            "OK schema=1 kind=disk root={} free={free} warn_below={} state={} trigger={} rows={} reclaimable={} under_pressure_up_to={} apply={}",
            self.root.display(),
            human_bytes(self.warn_free_bytes),
            if self.warn { "low" } else { "ok" },
            self.trigger.as_str(),
            self.rows.len(),
            human_bytes(self.reclaimable_total()),
            human_bytes(self.reclaimable_under_pressure()),
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
            "reclaimable_under_pressure_up_to_bytes".to_owned(),
            Value::from(self.reclaimable_under_pressure()),
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
    if let Witness::LeastRecentlyUsed { last_used, .. } = &r.witness {
        o.insert("last_used".to_owned(), Value::from(rfc3339_utc(*last_used)));
    }
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
    // ONE VERDICT PER PROFILE, the innermost build directory's ([`speaker`]),
    // as under pressure: a build directory named inside another is walked
    // by both surveys, one after the other, and each probes the nested
    // profile's locks. The directories that decide are the ones clearing
    // their fences — only theirs had their profiles probed.
    let targets = &survey.targets[..survey.targets.len().min(MAX_ROWS_PER_CLASS)];
    let deciding: Vec<&Path> = targets
        .iter()
        .filter(|t| t.fences().is_ok())
        .map(|t| t.path.as_path())
        .collect();
    let mut next = 0;
    for t in targets {
        // Its place among `deciding`, when it is one of them.
        let at = t.fences().is_ok().then(|| {
            next += 1;
            next - 1
        });
        let (own, leaves) = decided_by(t, at, &deciding);
        if own.profiles.is_empty() && !t.profiles.is_empty() {
            notes.push(format!(
                "cargo-targets: {} — every profile in it is in a build directory named inside it, which decides it",
                t.path.display()
            ));
            continue;
        }
        match target::target_witness(&own, survey.now, config.target_stale_days) {
            Ok(mut w) => {
                let (bytes, bytes_partial) = own.reclaimable(survey.now, config.target_stale_days);
                if let Witness::StaleBuildDir { leaves: left, .. } = &mut w {
                    *left = leaves;
                }
                rows.push(Row {
                    class: Class::CargoTargets,
                    path: t.path.clone(),
                    bytes,
                    bytes_partial,
                    witness: w,
                    removable: true,
                    blocked: None,
                });
            }
            Err(why) => notes.push(format!(
                "cargo-targets: {} — {why}{}",
                t.path.display(),
                leaving(&leaves)
            )),
        }
    }
    pressure_rows(survey, config, &mut rows, &mut notes);

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

    // Class order; within `cargo-targets` the idle build directories first,
    // then the pressure rows least recently used first — the order the
    // pass takes them in.
    rows.sort_by(|a, b| {
        a.class
            .cmp(&b.class)
            .then_with(|| lru_rank(a).cmp(&lru_rank(b)))
            .then_with(|| a.path.cmp(&b.path))
    });

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

/// Is `r` a pressure row ([`Witness::LeastRecentlyUsed`])?
fn is_pressure(r: &Row) -> bool {
    matches!(r.witness, Witness::LeastRecentlyUsed { .. })
}

/// Where a row sorts inside its class: every row but a pressure row first,
/// then the pressure rows by LAST USED, oldest first.
fn lru_rank(r: &Row) -> (bool, i64) {
    match r.witness {
        Witness::LeastRecentlyUsed { last_used, .. } => (true, last_used),
        _ => (false, 0),
    }
}

/// Below the automatic floor, a row for each profile the pressure pass may
/// take ([`TargetDir::pressure_candidates`]) in a build directory on the
/// measured volume, oldest first and at most [`MAX_ROWS_PER_CLASS`], and a
/// note for each recent one it keeps — each profile decided once, by the
/// innermost build directory holding it ([`speaker`]); at or above it, one
/// note saying no recent profile would be taken.
fn pressure_rows(survey: &Survey, config: Config, rows: &mut Vec<Row>, notes: &mut Vec<String>) {
    if survey.targets.is_empty() {
        return;
    }
    if !config.below_auto_floor(survey.free_bytes) {
        let why = if config.auto_free_gib == 0 {
            "the automatic floor is off".to_owned()
        } else {
            format!(
                "free space is not under the {} GiB floor",
                config.auto_free_gib
            )
        };
        notes.push(format!(
            "cargo-targets: {why}, so no profile written into within {}d would be taken",
            config.target_stale_days
        ));
        return;
    }
    if survey.volume_device.is_none() {
        notes.push(
            "cargo-targets: the measured volume's device could not be read, so no profile inside the idle window would be taken".to_owned(),
        );
        return;
    }
    let until_gib = config.auto_free_gib.saturating_add(PRESSURE_MARGIN_GIB);
    // The build directories that may give a pressure row: fenced, on the
    // measured volume, their recent profiles probed.
    let mut eligible: Vec<(&TargetDir, String, u64)> = Vec::new();
    for t in survey.targets.iter().take(MAX_ROWS_PER_CLASS) {
        let (Ok(marker), Some(device)) = (t.fences(), survey.volume_device) else {
            continue;
        };
        if t.device != Some(device) {
            notes.push(format!(
                "cargo-targets: {} is on another volume than the one measured — reclaiming it frees nothing there",
                t.path.display()
            ));
            continue;
        }
        if !t.probed_recent {
            notes.push(format!(
                "cargo-targets: {}'s recent profiles were neither probed for a build's locks nor sized (surveyed as if above the floor), so none is taken under pressure on this survey",
                t.path.display()
            ));
            continue;
        }
        eligible.push((t, marker, device));
    }
    let dirs: Vec<&Path> = eligible.iter().map(|(t, ..)| t.path.as_path()).collect();
    let mut picks: Vec<Row> = Vec::new();
    for (i, (t, marker, device)) in eligible.iter().enumerate() {
        let speaks = |p: &Profile| speaker(&dirs, &p.path) == Some(i);
        let (candidates, kept) = t.pressure_candidates(survey.now, config.target_stale_days);
        notes.extend(kept.into_iter().filter(|(p, _)| speaks(p)).map(|(p, why)| {
            format!(
                "cargo-targets under pressure: kept {}: {why}",
                p.path.display()
            )
        }));
        for p in candidates {
            if !speaks(p) {
                continue;
            }
            let Some(last_used) = p.last_used else {
                continue;
            };
            let row = Row {
                class: Class::CargoTargets,
                path: p.path.clone(),
                bytes: p.bytes,
                bytes_partial: p.bytes_partial,
                witness: Witness::LeastRecentlyUsed {
                    marker: marker.clone(),
                    build_dir: t.path.clone(),
                    last_used,
                    age_s: survey.now.saturating_sub(last_used),
                    threshold_days: config.target_stale_days,
                    floor_gib: config.auto_free_gib,
                    until_gib,
                    device: *device,
                },
                removable: true,
                blocked: None,
            };
            picks.push(row);
        }
    }
    picks.sort_by(|a, b| {
        lru_rank(a)
            .cmp(&lru_rank(b))
            .then_with(|| a.path.cmp(&b.path))
    });
    picks.truncate(MAX_ROWS_PER_CLASS);
    rows.extend(picks);
}

/// Which of `dirs` SPEAKS FOR the profile at `profile`, in the idle pass
/// and under pressure alike: the innermost one holding it (the first of two
/// that are the same). ONE VERDICT PER PROFILE: a build directory named
/// inside another is walked by both surveys, one after the other, and each
/// probes the profile's locks — so a build that takes one between the two
/// is read free by one survey and held by the other. Were each to speak, the
/// report would count the profile as removable in one row and keep it in a
/// note because "a build holds" it. So only the innermost directory's survey
/// decides — its row, or its reason to keep the profile — and every outer
/// survey's reading of that profile is dropped, a row and a note alike.
fn speaker(dirs: &[&Path], profile: &Path) -> Option<usize> {
    dirs.iter()
        .enumerate()
        .filter(|(_, d)| profile.starts_with(d))
        .max_by_key(|&(i, d)| (d.components().count(), std::cmp::Reverse(i)))
        .map(|(i, _)| i)
}

/// The build directory `t` as the IDLE pass judges it: only the profiles it
/// speaks for among `deciding` ([`speaker`]; `at` is its own place there,
/// `None` when it is not one of them and so decides nothing but its fences),
/// and the deciding directories named inside it — whose own surveys decide
/// every other profile it holds, so its row neither counts one of those nor
/// reclaims it ([`target::reclaim_leaving`]).
fn decided_by(t: &TargetDir, at: Option<usize>, deciding: &[&Path]) -> (TargetDir, Vec<PathBuf>) {
    let mut own = t.clone();
    let Some(at) = at else {
        return (own, Vec::new());
    };
    own.profiles
        .retain(|p| speaker(deciding, &p.path) == Some(at));
    let leaves = deciding
        .iter()
        .filter(|d| **d != t.path && d.starts_with(&t.path))
        .map(|d| d.to_path_buf())
        .collect();
    (own, leaves)
}

/// What a build directory leaves to the ones named inside it, as the clause
/// its row or note ends with — nothing when it holds none.
fn leaving(leaves: &[PathBuf]) -> String {
    match leaves {
        [] => String::new(),
        [one] => format!(
            " — not the profiles of {}, a build directory named inside it, which decides its own",
            one.display()
        ),
        many => format!(
            " — not the profiles of the {} build directories named inside it, which decide their own",
            many.len()
        ),
    }
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
    /// Its intent row could not be journalled (no journal, a full disk), so
    /// the removal did not start: nothing is removed without a record.
    Unjournalled(PathBuf),
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
            Refusal::Unjournalled(_) => "unjournalled",
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
            | Refusal::RemoveFailed(p, _)
            | Refusal::Unjournalled(p) => Some(p.as_path()),
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
                    .filter(|c| c.delegate().is_none())
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
            Refusal::Unjournalled(p) => format!(
                "{} was not touched: its intent could not be journalled, and nothing is removed without a record",
                p.display()
            ),
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

/// One INTENT ROW, written BEFORE a row's removal starts: a pass cut short
/// (a quit, a panic, an update) leaves this on record even when it leaves no
/// outcome row.
#[must_use]
pub fn intent_row(now: i64, sid: &str, row: &Row) -> String {
    let mut o = Map::new();
    o.insert("ts".to_owned(), Value::from(rfc3339_utc(now)));
    o.insert("sid".to_owned(), Value::from(sid.to_owned()));
    o.insert("kind".to_owned(), Value::from("removing".to_owned()));
    let Value::Object(inner) = row_value(row) else {
        return "{\"kind\":\"removing\",\"reason\":\"unserializable\"}".to_owned();
    };
    for (k, v) in inner {
        o.insert(k, v);
    }
    aterm_json::to_string(&Value::Object(o))
        .unwrap_or_else(|_| "{\"kind\":\"removing\",\"reason\":\"unserializable\"}".to_owned())
}

/// One REMOVAL ROW. The witness travels WITH it: a row that says what was
/// removed without saying why it was safe is not a record, it is a receipt.
/// And what it says was removed is the remover's answer — the bytes actually
/// released, each unit that went, each skipped unit and every delete error —
/// never the report's estimate.
#[must_use]
pub fn removal_row(now: i64, sid: &str, row: &Row, removed: &Removed) -> String {
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
    o.insert("freed_bytes".to_owned(), Value::from(removed.bytes));
    let texts = |items: Vec<String>| {
        Value::Array(
            items
                .into_iter()
                .take(MAX_ROWS_PER_CLASS)
                .map(|t| Value::from(truncate_bytes(&t, ROW_TEXT_CAP).to_owned()))
                .collect(),
        )
    };
    o.insert(
        "units".to_owned(),
        texts(
            removed
                .units
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
        ),
    );
    o.insert(
        "skipped".to_owned(),
        texts(
            removed
                .skipped
                .iter()
                .map(|(p, why)| format!("{}: {why}", p.display()))
                .collect(),
        ),
    );
    o.insert("trouble".to_owned(), texts(removed.trouble.clone()));
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
    o.insert(
        "reclaimable_under_pressure_up_to_bytes".to_owned(),
        Value::from(rep.reclaimable_under_pressure()),
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

/// What ONE row's removal did — the remover's answer, never the report's
/// estimate.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Removed {
    /// Bytes it released. For [`Class::CargoTargets`], the blocks each
    /// unlinked file released when it was that file's last name
    /// ([`target`]'s docs); for a whole-directory removal, the row's
    /// reported size.
    pub bytes: u64,
    /// What went, each a directory that is gone (a profile's `incremental/`,
    /// or the row's own directory).
    pub units: Vec<PathBuf>,
    /// What was left alone, and why (a profile a build holds, a lock that
    /// could not be created).
    pub skipped: Vec<(PathBuf, String)>,
    /// Every delete error on the way, one line each, naming what stayed. A
    /// failed delete is reported here and never counted in [`Self::bytes`].
    pub trouble: Vec<String>,
}

/// Which of the pressure pass's stops fired ([`Stop`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopBy {
    /// Free space measured again is at [`Config::pressure_target_bytes`].
    Measured,
    /// The bytes the pass's own removals released (as the unlinks counted
    /// them) cover what free space was SHORT of that target at its first
    /// pressure row — though the figure has not risen to match: a local
    /// snapshot, a clone sharing the blocks or a deferred free can keep an
    /// unlink out of `statvfs`/`df` for a while, and without this stop the
    /// pass would then take every row it has.
    Counted,
    /// Free space could not be measured again.
    Unmeasured,
}

impl StopBy {
    /// As the journal row spells it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            StopBy::Measured => "measured",
            StopBy::Counted => "counted",
            StopBy::Unmeasured => "unmeasured",
        }
    }
}

/// Why the pressure pass stopped before its last row: free space measured
/// again is back at [`Config::pressure_target_bytes`], or the bytes the pass
/// released cover what it was short, or the figure could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    /// Which stop fired.
    pub by: StopBy,
    /// Free bytes as measured before the row it stopped at; `None`: the
    /// measurement failed, and nothing is removed on a number nobody read.
    pub free: Option<u64>,
    /// The free bytes the pass reclaims up to.
    pub target: u64,
    /// What free space was short of `target` when it was measured before the
    /// first pressure row (0 when the pass stopped there).
    pub short: u64,
    /// The bytes the pressure rows it took released, as their unlinks
    /// counted them.
    pub released: u64,
    /// The rows it did not take (each a profile more recently used than
    /// every one it took).
    pub left: usize,
}

impl Stop {
    /// The sentence a person reads.
    #[must_use]
    pub fn describe(&self) -> String {
        let kept = format!(
            "{} more recently used profile{} kept",
            self.left,
            if self.left == 1 { "" } else { "s" }
        );
        match (self.by, self.free) {
            (StopBy::Unmeasured, _) | (_, None) => format!(
                "free space could not be measured again, and nothing is removed on a number nobody read — {kept}"
            ),
            (StopBy::Measured, Some(f)) => format!(
                "{} free, at or above the {} the pass reclaims up to — {kept}",
                human_bytes(f),
                human_bytes(self.target)
            ),
            (StopBy::Counted, Some(f)) => format!(
                "the {} the pass released covers the {} free space was short of {} when it began, though {} is measured free (a local snapshot, a clone or a deferred free may still hold the blocks) — {kept}",
                human_bytes(self.released),
                human_bytes(self.short),
                human_bytes(self.target),
                human_bytes(f)
            ),
        }
    }
}

/// One step of an apply, told to the caller AS IT HAPPENS ([`apply`],
/// [`apply_auto`]) — so a journal written from it records a removal's intent
/// before the removal starts.
#[derive(Debug, Clone, Copy)]
pub enum Step<'a> {
    /// This row is about to be removed: it passed [`guard`].
    Removing(&'a Row),
    /// This row's removal finished, and this is what it did.
    Removed(&'a Row, &'a Removed),
    /// A refusal: of the whole apply, of one path, or a removal that failed.
    Denied(&'a Refusal),
    /// The pressure pass stopped: enough is free again (or it could not
    /// tell).
    Stopped(&'a Stop),
}

/// What [`apply`] and [`apply_auto`] tell each [`Step`] to, as it happens.
/// It answers whether the step is ON RECORD (its journal row written): a
/// [`Step::Removing`] that is not stops that row's removal before it starts
/// ([`Refusal::Unjournalled`]) — nothing is removed without its intent on
/// record, so a pass cut short mid-removal always leaves one. The answer to
/// any other step is not acted on.
pub type Record<'a> = dyn FnMut(&Step<'_>) -> bool + 'a;

/// The journal line of one [`Step`].
#[must_use]
pub fn step_row(now: i64, sid: &str, step: &Step<'_>) -> String {
    match step {
        Step::Removing(row) => intent_row(now, sid, row),
        Step::Removed(row, removed) => removal_row(now, sid, row, removed),
        Step::Denied(refusal) => denial_row(now, sid, refusal),
        Step::Stopped(stop) => stop_row(now, sid, stop),
    }
}

/// One STOP ROW: where the pressure pass stopped, on what free figure, and
/// how many rows it left.
#[must_use]
pub fn stop_row(now: i64, sid: &str, stop: &Stop) -> String {
    let mut o = Map::new();
    o.insert("ts".to_owned(), Value::from(rfc3339_utc(now)));
    o.insert("sid".to_owned(), Value::from(sid.to_owned()));
    o.insert("kind".to_owned(), Value::from("stopped".to_owned()));
    o.insert(
        "class".to_owned(),
        Value::from(Class::CargoTargets.as_str().to_owned()),
    );
    o.insert(
        "free_bytes".to_owned(),
        stop.free.map_or(Value::Null, Value::from),
    );
    o.insert("target_free_bytes".to_owned(), Value::from(stop.target));
    o.insert("by".to_owned(), Value::from(stop.by.as_str().to_owned()));
    o.insert("short_bytes".to_owned(), Value::from(stop.short));
    o.insert("released_bytes".to_owned(), Value::from(stop.released));
    o.insert(
        "left".to_owned(),
        Value::from(u64::try_from(stop.left).unwrap_or(u64::MAX)),
    );
    o.insert(
        "text".to_owned(),
        Value::from(truncate_bytes(&stop.describe(), ROW_TEXT_CAP).to_owned()),
    );
    aterm_json::to_string(&Value::Object(o))
        .unwrap_or_else(|_| "{\"kind\":\"stopped\",\"reason\":\"unserializable\"}".to_owned())
}

/// What an apply DID.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Applied {
    /// The rows that were (at least in part) removed, each with what its
    /// removal did.
    pub removed: Vec<(Row, Removed)>,
    /// Every refusal, in the order they happened.
    pub denials: Vec<Refusal>,
    /// Bytes the removals released, as each remover counted them.
    pub freed_bytes: u64,
    /// Where the pressure pass stopped, when it stopped before its last row.
    pub stopped: Option<Stop>,
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

/// THE AUTOMATIC GRANT. PURE. Below the report's automatic floor
/// ([`Config::below_auto_floor`]) the removable [`Class::CargoTargets`] rows,
/// in the order they are taken — the idle build directories, then the
/// pressure rows least recently used first, of which [`apply_auto`] takes
/// only as many as free space needs — and never a row of another class,
/// whatever `disk.apply` says; at or above it, or on an unknown free figure,
/// `None`: nothing is planned.
#[must_use]
pub fn auto_plan(rep: &Report) -> Option<Vec<Row>> {
    rep.config.below_auto_floor(rep.free_bytes).then(|| {
        rep.rows
            .iter()
            .filter(|r| r.class == Class::CargoTargets && r.removable)
            .cloned()
            .collect()
    })
}

/// A row's remover: what [`apply`] calls for each row that passed [`guard`].
pub type Remove<'a> = dyn FnMut(&Row) -> std::io::Result<Removed> + 'a;

/// Free space on the measured volume, measured AGAIN: what the pressure pass
/// reads before each of its rows. `None` (a failed measurement) stops it.
pub type Measure<'a> = dyn FnMut() -> Option<u64> + 'a;

/// Carry out [`auto_plan`] against `rep` through `remove`, each path behind
/// [`guard`] as [`apply`]'s are, each [`Step`] told to `record` as it
/// happens, and free space re-read through `measure` before each pressure
/// row: the pass stops once it is at [`Config::pressure_target_bytes`].
/// `None` when the floor was not crossed.
pub fn apply_auto(
    rep: &Report,
    transcripts: Option<&Path>,
    remove: &mut Remove<'_>,
    measure: &mut Measure<'_>,
    record: &mut Record<'_>,
) -> Option<Applied> {
    let rows = auto_plan(rep)?;
    let mut done = Applied::default();
    remove_rows(
        rep,
        transcripts,
        Class::CargoTargets,
        rows,
        (remove, measure),
        record,
        &mut done,
    );
    Some(done)
}

/// Carry out `class` against `rep`, removing through `remove` and telling
/// `record` each [`Step`] as it happens. A pressure row (on a report taken
/// below the floor) is preceded by a fresh `measure`, and the pass stops at
/// [`Config::pressure_target_bytes`], as the tick's does.
///
/// `remove` is INJECTED so this function is testable without a disk and so
/// the removal primitive is the caller's choice ([`remover`] is the real
/// one); [`guard`] runs before every single call to it, and a guard refusal
/// is recorded rather than returned, because one refused path must not
/// abandon the rest of the grant.
pub fn apply(
    rep: &Report,
    transcripts: Option<&Path>,
    class: Option<Class>,
    remove: &mut Remove<'_>,
    measure: &mut Measure<'_>,
    record: &mut Record<'_>,
) -> Applied {
    let mut done = Applied::default();
    let rows = match plan(rep, class) {
        Plan::ReportOnly(r) | Plan::Denied(r) => {
            record(&Step::Denied(&r));
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
    remove_rows(
        rep,
        transcripts,
        class,
        rows,
        (remove, measure),
        record,
        &mut done,
    );
    done
}

/// Remove `rows` of `class`, each behind [`guard`], into `done`: the intent
/// told BEFORE each removal — and a removal whose intent is not on record
/// does not start — its outcome after. Before each pressure row free space
/// is measured again, and the pass stops (a [`Stop`], told and kept) once it
/// is at [`Config::pressure_target_bytes`], once the bytes the pressure rows
/// released cover what it was short at the first of them ([`StopBy::Counted`]:
/// a figure that does not move must not cost every profile), or when it
/// cannot be read.
fn remove_rows(
    rep: &Report,
    transcripts: Option<&Path>,
    class: Class,
    rows: Vec<Row>,
    (remove, measure): (&mut Remove<'_>, &mut Measure<'_>),
    record: &mut Record<'_>,
    done: &mut Applied,
) {
    let target = rep.config.pressure_target_bytes();
    let total = rows.len();
    // What free space was short of `target` before the first pressure row,
    // and what the pressure rows have released since, as counted.
    let mut short: Option<u64> = None;
    let mut released: u64 = 0;
    for (i, row) in rows.into_iter().enumerate() {
        let pressure = matches!(row.witness, Witness::LeastRecentlyUsed { .. });
        if pressure {
            let free = measure();
            let by = match free {
                None => Some(StopBy::Unmeasured),
                Some(f) if f >= target => Some(StopBy::Measured),
                Some(f) => {
                    let short = *short.get_or_insert(target - f);
                    (released >= short).then_some(StopBy::Counted)
                }
            };
            if let Some(by) = by {
                let stop = Stop {
                    by,
                    free,
                    target,
                    short: short.unwrap_or(0),
                    released,
                    left: total - i,
                };
                record(&Step::Stopped(&stop));
                done.stopped = Some(stop);
                return;
            }
        }
        if let Err(r) = guard(rep, transcripts, class, &row.path) {
            record(&Step::Denied(&r));
            done.denials.push(r);
            continue;
        }
        if !record(&Step::Removing(&row)) {
            let r = Refusal::Unjournalled(row.path.clone());
            record(&Step::Denied(&r));
            done.denials.push(r);
            continue;
        }
        match remove(&row) {
            Ok(removed) => {
                done.freed_bytes = done.freed_bytes.saturating_add(removed.bytes);
                if pressure {
                    released = released.saturating_add(removed.bytes);
                }
                record(&Step::Removed(&row, &removed));
                done.removed.push((row, removed));
            }
            Err(e) => {
                let r = Refusal::RemoveFailed(row.path.clone(), e.to_string());
                record(&Step::Denied(&r));
                done.denials.push(r);
            }
        }
    }
}

/// THE REAL REMOVER, one primitive per class: [`Class::CargoTargets`] goes
/// through [`target::reclaim_leaving`] under `judge` (each idle profile's
/// `incremental/`, under cargo's locks — never the directory, and never a
/// profile of a build directory named inside it, which its own row takes)
/// or, for a pressure row, [`target::reclaim_lru`] (that one profile's, on
/// the device its row was measured on), and [`Class::ClaudeStaleVersions`]
/// through [`remove_tree`]. A delegated class never reaches a remover
/// ([`plan`] refuses it).
pub fn remover(judge: Judge) -> impl FnMut(&Row) -> std::io::Result<Removed> {
    move |row: &Row| match row.class {
        Class::CargoTargets => match &row.witness {
            Witness::LeastRecentlyUsed {
                build_dir,
                last_used,
                device,
                ..
            } => target::reclaim_lru(
                build_dir,
                &row.path,
                &Judge {
                    device: Some(*device),
                    ..judge
                },
                *last_used,
            ),
            Witness::StaleBuildDir { leaves, .. } => {
                target::reclaim_leaving(&row.path, &judge, leaves)
            }
            _ => target::reclaim(&row.path, &judge),
        },
        Class::ClaudeStaleVersions => {
            remove_tree(&row.path)?;
            Ok(Removed {
                bytes: row.bytes,
                units: vec![row.path.clone()],
                ..Removed::default()
            })
        }
        Class::AtpkgGc | Class::ClaudePurge => Err(std::io::Error::other(
            "a delegated class is never removed from here",
        )),
    }
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
        s.volume_device = target::dev_of(v);
    }
    // Below the floor every profile is sized and probed, recent ones too:
    // the pressure pass may take any of them.
    let pressure = config.below_auto_floor(free_bytes);
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
    // ONE ROW PER DIRECTORY: a `target` link and the `target.noindex` it
    // names, or two cwds that differ by a linked component, are one build
    // directory once canonical — counted once, reclaimed once.
    for t in roots.targets.iter().take(MAX_ROWS_PER_CLASS) {
        let Ok(canonical) = std::fs::canonicalize(t) else {
            continue;
        };
        if s.targets.iter().any(|seen| seen.path == canonical) {
            continue;
        }
        if let Some(td) = scan_target(&canonical, now, config.target_stale_days, pressure) {
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

/// One build directory's facts, or `None` when the path is not a directory:
/// [`target::assess_under`] with the shipped walk bounds (under `pressure`,
/// every profile sized and probed, not only the idle ones). The path it
/// reports is CANONICAL.
#[must_use]
pub fn scan_target(
    path: &Path,
    now: i64,
    threshold_days: i64,
    pressure: bool,
) -> Option<TargetDir> {
    target::assess_under(
        path,
        now,
        threshold_days,
        pressure,
        target::WalkBudget::SHIPPED,
    )
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

/// Remove one directory tree: [`remover`]'s primitive for
/// [`Class::ClaudeStaleVersions`] (never for a build directory, whose unit is
/// [`target::reclaim`]'s).
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
pub(crate) mod tests;

#[path = "disk_pressure_tests.rs"]
#[cfg(all(test, unix))]
pub(crate) mod pressure_tests;
