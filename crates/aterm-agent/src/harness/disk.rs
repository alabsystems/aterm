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
//! when free space is below `disk.auto_free_gib` ([`DEFAULT_AUTO_FREE_GIB`]),
//! the host's tick
//! reclaims the removable [`Class::CargoTargets`] rows — each still behind its
//! witness and [`guard`] — and nothing of any other class. Above the floor,
//! or on a free figure nobody could read, it plans nothing. Below it, in
//! LEAST-RECENTLY-USED order (at the idle window alone the tick freed
//! nothing, because every agent builds in its checkout daily):
//!
//! 1. every IDLE profile's `incremental/` (unwritten for `target_stale_days`)
//!    in each witnessed build directory, through [`target::reclaim`] — all of
//!    them: they are the least recently used there are;
//! 2. then, free space measured again before each, the build directories
//!    whose OWNER IS GONE, WHOLE, the longest unwritten first
//!    ([`Witness::OwnerGone`], through [`target::reclaim_whole`];
//!    `disk.dead_build_dirs`, on by default): an ended
//!    Claude Code session's scratch build — no running Claude Code names the
//!    session in its own registry, nothing of it written for
//!    [`Config::quiet_s`] (`target_stale_days`, at least a day) ([`discover`]'s
//!    owners). A signed
//!    `CACHEDIR.TAG` (the specification's mark of regenerable contents), no
//!    source at its root, every profile known, none locked and every one
//!    unwritten for the quiet window; no running process using it or the
//!    owner's folder ([`discover::unused`]); every lock of every profile held
//!    for the delete and, under them, the owner checked again — the
//!    directory inside its folder, the owner still gone
//!    ([`discover::recheck`]). Nobody builds there again, so these go before
//!    any live checkout's cache;
//! 3. then, one PROFILE at a time and oldest LAST USED first, a profile
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
//! # Where the build directories come from
//!
//! The owner's verb surveys the build directories NAMED on its line; named
//! none, it SEARCHES for them, as the window's tick does below the floor
//! ([`discover::discover`]): the agents' working directories and their
//! worktrees, `$HOME`'s checkouts and their git worktrees two levels down
//! (a `target -> target.noindex` link resolved), x.py's bootstrap build
//! directories (never a sysroot, stage0 or `llvm`), the agents'
//! `CARGO_TARGET_DIR` and Claude Code's temp root. What no
//! row can free is the report's [`Report::hogs`]: the largest build
//! directories left, each with what frees it (`$HOME/trust/build 94.0 GiB (x.py
//! bootstrap; run scripts/slim_build.py)`), which the window's "Disk space is
//! low" line names.
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
//!   or anything else — but for a gone owner's build directory below the
//!   floor ([`Witness::OwnerGone`], step 2 above), which goes whole. The row
//!   SAYS what it costs: a slower next build.
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
//! was short). The re-measure is the host's `statvfs` (the verb's `df`). A
//! drill on a scratch APFS volume saw each unlink there at once and the
//! pass stop `by=measured`;
//! with the caches made clones of a kept file it stopped `by=counted`. A
//! local snapshot holding the blocks keeps the figure low the same way, and
//! since an APFS clone's shared blocks are counted although the clone keeps
//! them, the counted stop can end a pass early (the next tick looks again).
//! `deps/`, `build/` and `.fingerprint/` are never reclaimed, by design
//! ([`target`]'s docs). The `--diagnose` line of §5.5 has no host.

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

/// `disk.target_stale_days` (design §5.5): one day. A profile unwritten for
/// that long is idle: the owner's `--apply cargo-targets` takes its
/// `incremental/` at any free figure, and below [`DEFAULT_AUTO_FREE_GIB`] the
/// tick takes every idle one first, whole, before any recent one. A profile
/// inside the window is kept above the floor and, below it, taken only in
/// least-recently-used order until the volume is back above the floor. The
/// unit is `incremental/` alone, which cargo rebuilds. An explicit setting is
/// read as written.
pub const DEFAULT_TARGET_STALE_DAYS: i64 = 1;

/// `disk.apply` (design §5.5): the owner's verb removes nothing until this is
/// on. The automatic floor below is its own, narrower grant.
pub const DEFAULT_APPLY: bool = false;

/// `disk.auto_free_gib`: below this many free GiB the host's tick reclaims
/// the witnessed build directories' idle caches by itself
/// ([`Class::CargoTargets`] only, each row carrying its witness,
/// [`auto_plan`]); a full disk stalls workers. It removes only the
/// `incremental/` of cargo profiles, under cargo's own locks: never the
/// directory, a transcript, or another class. Below it, profiles inside the
/// idle window go too, least recently used first, until free space is back
/// at this floor plus [`PRESSURE_MARGIN_GIB`]. `0` turns it off.
pub const DEFAULT_AUTO_FREE_GIB: u64 = 10;

/// How far above the floor the pressure pass reclaims before it stops: five
/// GiB, so the next build's writes (one aterm workspace's cache is 12-43
/// GiB) do not push the volume straight back under it before the next look.
/// It was one GiB until the disk filled twice on 2026-10-16. A target, not a
/// measurement.
pub const PRESSURE_MARGIN_GIB: u64 = 5;

/// The automatic floor is at least this percent of the volume's size
/// ([`Config::for_volume`]): ten GiB is one build from full on a 460 GiB
/// disk where one workspace's build cache is 12-43 GiB.
pub const AUTO_FREE_PCT: u64 = 5;

/// The warning threshold is at least this percent of the volume's size
/// ([`Config::for_volume`]).
pub const WARN_FREE_PCT: u64 = 8;

/// The most the volume's share ([`AUTO_FREE_PCT`]) raises the automatic
/// floor to: on a 4 TB volume 5% is 200 GiB, and a floor there would take
/// live checkouts' caches with 150 GiB free. A floor the owner WROTE is
/// never raised at all ([`Written`]).
pub const AUTO_FREE_SHARE_MAX_GIB: u64 = 32;

/// The most the volume's share ([`WARN_FREE_PCT`]) raises the warning to.
pub const WARN_FREE_SHARE_MAX_GIB: u64 = 64;

/// The most the volume's share ([`EMERGENCY_FREE_PCT`]) raises the
/// emergency line to.
pub const EMERGENCY_FREE_SHARE_MAX_GIB: u64 = 8;

/// THE EMERGENCY: below the larger of this many GiB and
/// [`EMERGENCY_FREE_PCT`] of the volume ([`Thresholds::emergency`]), or with
/// fewer than [`EMERGENCY_FILES_FREE`] inodes, writes are about to fail with
/// `ENOSPC`: the watch releases its reserve ([`reserve`]), passes at once,
/// and an owner alert follows when nothing safe is left to free.
pub const EMERGENCY_FREE_GIB: u64 = 4;

/// See [`EMERGENCY_FREE_GIB`].
pub const EMERGENCY_FREE_PCT: u64 = 1;

/// Fewer free inodes than this is an emergency whatever the bytes say: a
/// volume out of inodes fails a write with `ENOSPC` too.
pub const EMERGENCY_FILES_FREE: u64 = 50_000;

/// The `CACHEDIR.TAG` signature every cargo-compatible build tool writes into
/// a target directory (the Cache Directory Tagging Specification's fixed
/// line, which cargo emits verbatim): the marker that makes "a build tool
/// laid this" a fact rather than a guess from the directory's name.
pub const CACHEDIR_SIGNATURE: &str = "Signature: 8a477f597d28d172789f06886806bc55";

/// The other marker a cargo target directory carries.
pub const RUSTC_INFO: &str = ".rustc_info.json";

#[path = "disk_target.rs"]
pub mod target;

#[path = "disk_discover.rs"]
pub mod discover;

#[path = "disk_reserve.rs"]
pub mod reserve;

#[path = "disk_poke.rs"]
pub mod poke;

pub use target::{CARGO_LOCKS, INCREMENTAL, Judge, PRESSURE_MIN_IDLE_S, Profile, TargetDir};

/// The most bytes of any quoted path or reason one ledger row may carry.
pub const ROW_TEXT_CAP: usize = 512;

/// The most rows one report carries per class: one report stays one screen
/// of decisions, and every walk below stays bounded.
pub const MAX_ROWS_PER_CLASS: usize = 64;

/// The most directory entries one size walk visits before it stops and
/// marks the figure partial.
pub const MAX_WALK_ENTRIES: usize = 200_000;

/// The most build directories one survey measures: the named ones first,
/// then what discovery found ([`discover::MAX_FOUND`]).
pub const MAX_TARGETS: usize = discover::MAX_FOUND;

/// The most entries of the report's list of the largest build directories it
/// cannot reclaim ([`Report::hogs`]).
pub const MAX_HOGS: usize = 5;

/// `disk.dead_build_dirs`: below the floor, a build directory whose OWNER IS
/// GONE goes WHOLE ([`Witness::OwnerGone`]) — a Claude Code session's scratch
/// build once no running Claude Code names the session and nothing of it
/// was written for the quiet window ([`Config::quiet_s`]) (measured: 45 GB of
/// ended sessions' scratch builds on a volume at 3-6 GB free). `false`: such a directory is
/// any other, its idle and least-recently-used `incremental/` only.
pub const DEFAULT_DEAD_BUILD_DIRS: bool = true;

/// Bytes in a GiB.
const GIB: u64 = 1024 * 1024 * 1024;

/// Seconds in a day.
const DAY_S: i64 = 86_400;

// ---------------------------------------------------------------------------
// The safelist
// ---------------------------------------------------------------------------

/// The safelist. A class not named here has no spelling that reaches
/// [`apply`]: the list is a closed enum, not a configurable string set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// Package builds superseded by the live one. Delegated to `aterm pkg gc`.
    AtpkgGc,
    /// Vendor version directories that are not the live symlink's target.
    ClaudeStaleVersions,
    /// Build directories whose idle cargo profiles' `incremental/` goes —
    /// and below the floor, recent profiles', least recently used first.
    CargoTargets,
    /// The vendor's own project purge. Delegated, and never run from here.
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

    /// Parse a class name. Unknown names answer `None`; no prefix matching or
    /// case folding, since a typo resolving to a different class would be a
    /// grant the owner did not give.
    #[must_use]
    pub fn parse(s: &str) -> Option<Class> {
        Class::ALL.iter().copied().find(|c| c.as_str() == s)
    }

    /// The command that owns removal for this class, when it is not us.
    ///
    /// `None`: this module removes the row itself. `Some`: the row is
    /// reported and [`apply`] refuses it, naming the command (a tree under
    /// another writer's lock).
    #[must_use]
    pub fn delegate(self) -> Option<Delegate> {
        match self {
            Class::AtpkgGc => Some(Delegate::AtermPkgGc),
            Class::ClaudePurge => Some(Delegate::ClaudeProjectPurge),
            Class::ClaudeStaleVersions | Class::CargoTargets => None,
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
    /// Below the floor, may a build directory whose owner is gone go whole
    /// ([`DEFAULT_DEAD_BUILD_DIRS`])?
    pub dead_build_dirs: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            warn_free_gib: DEFAULT_WARN_FREE_GIB,
            target_stale_days: DEFAULT_TARGET_STALE_DAYS,
            apply: DEFAULT_APPLY,
            auto_free_gib: DEFAULT_AUTO_FREE_GIB,
            dead_build_dirs: DEFAULT_DEAD_BUILD_DIRS,
        }
    }
}

/// Every `[disk]` key of `aterm.toml`, in the order the help lists them. The
/// window's settings checker reads this list, so none of them reads there as
/// "unknown".
pub const KEYS: &[&str] = &[
    "apply",
    "warn_free_gib",
    "target_stale_days",
    "auto_free_gib",
    "dead_build_dirs",
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
            "dead_build_dirs" => d.dead_build_dirs.to_string(),
            _ => return None,
        })
    }

    /// How a negative `[disk] <key>` is read, for the settings checker to say
    /// on the line: `auto_free_gib` reads it as `0`, off (never the default,
    /// which would leave removal on for a person who wrote `-1` to stop it);
    /// the other two fall back to their defaults, the safe side for a warning
    /// threshold and for a stale window.
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
    /// never is: nothing is removed on a number nobody read.
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

    /// This config on a volume of `total` bytes with no knob written
    /// ([`Self::on_volume`]).
    #[must_use]
    pub fn for_volume(self, total: Option<u64>) -> Config {
        self.on_volume(total, Written::default())
    }

    /// This config on a volume of `total` bytes: a floor the owner did not
    /// write raised to [`AUTO_FREE_PCT`] of it, at most
    /// [`AUTO_FREE_SHARE_MAX_GIB`], and a warning not written to
    /// [`WARN_FREE_PCT`], at most [`WARN_FREE_SHARE_MAX_GIB`] — each the
    /// larger of the knob and the share. A value the owner WROTE is read as
    /// written ([`Written`]); a floor of `0`, off, stays off. An unknown size
    /// changes nothing.
    #[must_use]
    pub fn on_volume(self, total: Option<u64>, written: Written) -> Config {
        let Some(total) = total else {
            return self;
        };
        let share = |pct: u64, max: u64| (total / 100).saturating_mul(pct).div_ceil(GIB).min(max);
        Config {
            warn_free_gib: if written.warn_free_gib {
                self.warn_free_gib
            } else {
                self.warn_free_gib
                    .max(share(WARN_FREE_PCT, WARN_FREE_SHARE_MAX_GIB))
            },
            auto_free_gib: if self.auto_free_gib == 0 || written.auto_free_gib {
                self.auto_free_gib
            } else {
                self.auto_free_gib
                    .max(share(AUTO_FREE_PCT, AUTO_FREE_SHARE_MAX_GIB))
            },
            ..self
        }
    }

    /// The thresholds in force on a volume of `total` bytes with no knob
    /// written ([`Self::thresholds_on`]).
    #[must_use]
    pub fn thresholds(self, total: Option<u64>) -> Thresholds {
        self.thresholds_on(total, Written::default())
    }

    /// The thresholds in force on a volume of `total` bytes
    /// ([`Self::on_volume`]), in bytes. The emergency line is the larger of
    /// [`EMERGENCY_FREE_GIB`] and [`EMERGENCY_FREE_PCT`] of the volume (that
    /// share at most [`EMERGENCY_FREE_SHARE_MAX_GIB`]), and never above a
    /// floor that is on: below the emergency the pass must be able to run.
    #[must_use]
    pub fn thresholds_on(self, total: Option<u64>, written: Written) -> Thresholds {
        let c = self.on_volume(total, written);
        let pct = |p: u64| total.map_or(0, |t| (t / 100).saturating_mul(p));
        let floor = c.auto_free_gib.saturating_mul(GIB);
        let emergency = EMERGENCY_FREE_GIB
            .saturating_mul(GIB)
            .max(pct(EMERGENCY_FREE_PCT).min(EMERGENCY_FREE_SHARE_MAX_GIB.saturating_mul(GIB)));
        Thresholds {
            warn: c.warn_free_gib.saturating_mul(GIB),
            floor,
            target: c.pressure_target_bytes(),
            emergency: if floor > 0 {
                emergency.min(floor)
            } else {
                emergency
            },
            files_emergency: EMERGENCY_FILES_FREE,
        }
    }

    /// How long nothing of a gone owner may have been written before its
    /// build directory goes whole: `target_stale_days`, and never less than
    /// ONE day (a window of `0` makes every profile idle; it does not make a
    /// session that wrote a minute ago ended).
    #[must_use]
    pub fn quiet_s(&self) -> i64 {
        self.target_stale_days.max(1).saturating_mul(DAY_S)
    }
}

/// Which of the volume-scaled `[disk]` knobs the owner WROTE in aterm.toml
/// ([`Config::on_volume`]): a written one is read as written, never raised
/// to the volume's share.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Written {
    /// `disk.auto_free_gib` is written.
    pub auto_free_gib: bool,
    /// `disk.warn_free_gib` is written.
    pub warn_free_gib: bool,
}

/// The free-space thresholds in force on one volume ([`Config::thresholds`]),
/// in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    /// Below this the report warns.
    pub warn: u64,
    /// Below this the automatic pass runs (`0`: off).
    pub floor: u64,
    /// Where the pressure pass stops: the floor plus [`PRESSURE_MARGIN_GIB`].
    pub target: u64,
    /// Below this writes are about to fail ([`EMERGENCY_FREE_GIB`]).
    pub emergency: u64,
    /// Fewer free inodes than this is an emergency ([`EMERGENCY_FILES_FREE`]).
    pub files_emergency: u64,
}

/// Where a volume stands against its [`Thresholds`] ([`Thresholds::level`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Free bytes were not measured, and no measured inode emergency is known.
    Unknown,
    /// At or above the warning threshold.
    Ok,
    /// Below the warning threshold, at or above the floor.
    Low,
    /// Below the automatic floor: the pass runs.
    Short,
    /// Below the emergency line, or out of inodes: writes are failing or
    /// about to.
    Emergency,
}

impl Level {
    /// The wire spelling (`disk=` of a status line, `level` of the JSON).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Unknown => "unknown",
            Level::Ok => "ok",
            Level::Low => "low",
            Level::Short => "short",
            Level::Emergency => "emergency",
        }
    }
}

impl Thresholds {
    /// Where `free` bytes and `files_free` inodes stand. Without free bytes,
    /// only a measured inode emergency is decisive; otherwise the level is
    /// [`Level::Unknown`]. The floor at `0` (off) is never crossed, the
    /// emergency always can be.
    #[must_use]
    pub fn level(&self, free: Option<u64>, files_free: Option<u64>) -> Level {
        if free.is_some_and(|f| f < self.emergency)
            || files_free.is_some_and(|f| f < self.files_emergency)
        {
            return Level::Emergency;
        }
        match free {
            Some(f) if self.floor > 0 && f < self.floor => Level::Short,
            Some(f) if f < self.warn => Level::Low,
            Some(_) => Level::Ok,
            None => Level::Unknown,
        }
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
    /// Free bytes, or `None` when the query failed: the report then says
    /// `free=unknown` and never warns on a number it does not have.
    pub free_bytes: Option<u64>,
    /// The device of [`Self::root`], when it could be read: below the floor
    /// only a build directory on it gets pressure rows (reclaiming one
    /// anywhere else frees nothing here).
    pub volume_device: Option<u64>,
    /// Candidate build directories.
    pub targets: Vec<TargetDir>,
    /// Vendor version directories.
    pub versions: Vec<VersionDir>,
    /// The live symlink's resolved target, when it resolved.
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
    /// `$HOME`, for the report's lines (`~/…`).
    pub home: Option<PathBuf>,
    /// What the search for build directories could not finish
    /// ([`discover::Discovered::notes`]).
    pub notes: Vec<String>,
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
            home: None,
            notes: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// The witness
// ---------------------------------------------------------------------------

/// Why a row is safe to remove. There is no `Witness::None`: a row with no
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
    /// Below the floor, one cargo profile inside the idle window, taken in
    /// least-recently-used order until free space is back above the floor
    /// (the module docs). The row's path is the profile's.
    LeastRecentlyUsed {
        /// The build directory's marker.
        marker: String,
        /// The build directory the profile is in (canonical).
        build_dir: PathBuf,
        /// Its last use, as the survey read it (unix seconds).
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
    /// Below the floor, a WHOLE build directory whose owner is gone (a Claude
    /// Code session no running process names), every profile unwritten for
    /// the quiet window, none locked
    /// (the module docs; [`target::reclaim_whole`]). The row's path is the
    /// build directory's.
    OwnerGone {
        /// Its marker: a signed `CACHEDIR.TAG`, the mark of a cache.
        marker: String,
        /// Whose it was and why they are gone, as the survey read it.
        evidence: String,
        /// What the removal checks again first ([`discover::still_gone`]).
        owner: discover::Owner,
        /// The newest LAST USED of its profiles (unix seconds).
        last_used: i64,
        /// Seconds since then, at the survey.
        age_s: i64,
        /// The quiet window, in seconds ([`Config::quiet_s`]).
        quiet_s: i64,
        /// The floor free space fell below, in GiB.
        floor_gib: u64,
        /// The free GiB the pass stops at.
        until_gib: u64,
        /// The device measured.
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
            Witness::OwnerGone {
                marker,
                evidence,
                age_s,
                floor_gib,
                until_gib,
                ..
            } => format!(
                "{marker} present (a cache by its own mark); {evidence}; no build holding its locks, every profile unwritten for {} — free space is under the {floor_gib} GiB floor, so the whole build directory goes, before any live checkout's cache, until {until_gib} GiB are free",
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
            Witness::OwnerGone { .. } => "owner-gone",
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
    /// Is free space below the threshold? A `None` free figure never warns.
    pub warn: bool,
    /// The rows, class order then path order.
    pub rows: Vec<Row>,
    /// Notes about rows the report deliberately did NOT make — an
    /// unresolvable live symlink, a class with no roots. Data, not warnings.
    pub notes: Vec<String>,
    /// The largest build directories this report can NOT reclaim, biggest
    /// first, at most [`MAX_HOGS`]: what is left for a person to free, and
    /// what frees it.
    pub hogs: Vec<Hog>,
    /// `$HOME`, for the lines (`~/…`).
    pub home: Option<PathBuf>,
    /// The config that produced it.
    pub config: Config,
}

/// One build directory — or an x.py checkout's whole `build/` — holding
/// bytes no row of the report would free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hog {
    /// The directory.
    pub path: PathBuf,
    /// The bytes it holds that no row frees: its size ([`target::total_bytes`])
    /// less what its rows would release.
    pub bytes: u64,
    /// `true` when a size walk stopped at its bound: the figure is a floor.
    pub bytes_partial: bool,
    /// What it is, and what frees it.
    pub why: String,
}

impl Hog {
    /// The line a person reads: `$HOME/trust/build 94.0 GiB (x.py bootstrap; run
    /// scripts/slim_build.py)`.
    #[must_use]
    pub fn line(&self, home: Option<&Path>) -> String {
        self.line_at(&display_home(&self.path, home))
    }

    /// [`Self::line`] in at most `chars` characters, for a place that clips
    /// a line at its END (a message's detail line): what it is and why stays
    /// whole, and the PATH gives way — its middle written `…`, its head and
    /// its last components kept — down to [`HOG_PATH_MIN`] characters; only
    /// past that is the line cut at its end.
    #[must_use]
    pub fn line_within(&self, home: Option<&Path>, chars: usize) -> String {
        let full: Vec<char> = display_home(&self.path, home).chars().collect();
        let mut line = self.line_at(&full.iter().collect::<String>());
        let over = line.chars().count().saturating_sub(chars);
        if over == 0 {
            return line;
        }
        let keep = full.len().saturating_sub(over + 1).max(HOG_PATH_MIN);
        if keep + 1 < full.len() {
            let head = keep / 3;
            let at: String = full[..head]
                .iter()
                .chain(std::iter::once(&'…'))
                .chain(&full[full.len() - (keep - head)..])
                .collect();
            line = self.line_at(&at);
        }
        if line.chars().count() > chars {
            line = line.chars().take(chars.saturating_sub(1)).collect();
            line.push('…');
        }
        line
    }

    /// The line, the path written `at`.
    fn line_at(&self, at: &str) -> String {
        format!(
            "{at} {}{} ({})",
            human_bytes(self.bytes),
            if self.bytes_partial { "+" } else { "" },
            self.why
        )
    }
}

/// The fewest characters of a hog's path [`Hog::line_within`] keeps.
const HOG_PATH_MIN: usize = 24;

/// `path` with `$HOME` written `~`.
fn display_home(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rel) if !rel.as_os_str().is_empty() => format!("~/{}", rel.display()),
        _ => path.display().to_string(),
    }
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
    /// apply takes whole. The pressure rows are
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
    /// upper bound, since it stops once free space is back at
    /// [`Config::pressure_target_bytes`] (or the bytes it released cover what
    /// it was short). `0` at or above the floor.
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
        o.insert(
            "hogs".to_owned(),
            Value::Array(
                self.hogs
                    .iter()
                    .map(|h| {
                        let mut r = Map::new();
                        r.insert("path".to_owned(), Value::from(path_text(&h.path)));
                        r.insert("bytes".to_owned(), Value::from(h.bytes));
                        r.insert("bytes_partial".to_owned(), Value::from(h.bytes_partial));
                        r.insert(
                            "why".to_owned(),
                            Value::from(truncate_bytes(&h.why, ROW_TEXT_CAP).to_owned()),
                        );
                        Value::Object(r)
                    })
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
    if let Witness::LeastRecentlyUsed { last_used, .. } | Witness::OwnerGone { last_used, .. } =
        &r.witness
    {
        o.insert("last_used".to_owned(), Value::from(rfc3339_utc(*last_used)));
    }
    // Whose it was: the session and its folder — what the ledger needs to
    // explain a whole removal.
    if let Witness::OwnerGone { owner, .. } = &r.witness {
        o.insert(
            "owner".to_owned(),
            Value::from(truncate_bytes(&owner.describe(), ROW_TEXT_CAP).to_owned()),
        );
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

/// Build the §5.5 report from `survey`. Pure: no clock, no filesystem, no
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
            // The tie breaks safe: with no resolved live target, "not the
            // live one" is an assumption, so every version directory is
            // listed as blocked.
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
    let targets = &survey.targets[..survey.targets.len().min(MAX_TARGETS)];
    let deciding: Vec<&Path> = targets
        .iter()
        .filter(|t| t.fences().is_ok())
        .map(|t| t.path.as_path())
        .collect();
    // Below the floor, the build directories whose owner is gone go WHOLE:
    // each its own row, and none of its profiles a row of another kind.
    let mut whole: Vec<&Path> = Vec::new();
    let mut next = 0;
    for t in targets {
        // Its place among `deciding`, when it is one of them.
        let at = t.fences().is_ok().then(|| {
            next += 1;
            next - 1
        });
        let (own, leaves) = decided_by(t, at, &deciding);
        match whole_verdict(t, survey, config, &leaves) {
            Some(Ok(witness)) => {
                let (bytes, bytes_partial) = t.total.unwrap_or((0, true));
                rows.push(Row {
                    class: Class::CargoTargets,
                    path: t.path.clone(),
                    bytes,
                    bytes_partial,
                    witness,
                    removable: true,
                    blocked: None,
                });
                whole.push(&t.path);
                continue;
            }
            Some(Err(why)) => notes.push(format!(
                "cargo-targets: {} — its owner is gone, but it does not go whole: {why}",
                t.path.display()
            )),
            None => {}
        }
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
    pressure_rows(survey, config, &whole, &mut rows, &mut notes);
    notes.extend(survey.notes.iter().cloned());

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
    // then the pressure rows least recently used first, the order the pass
    // takes them in.
    rows.sort_by(|a, b| {
        a.class
            .cmp(&b.class)
            .then_with(|| lru_rank(a).cmp(&lru_rank(b)))
            .then_with(|| a.path.cmp(&b.path))
    });

    let warn = survey.free_bytes.is_some_and(|b| b < warn_free_bytes);
    let hogs = hogs(survey, config, &rows, &notes, &|_| true);
    Report {
        now: survey.now,
        trigger: survey.trigger,
        root: survey.root.clone(),
        free_bytes: survey.free_bytes,
        warn_free_bytes,
        warn,
        rows,
        notes,
        hogs,
        home: survey.home.clone(),
        config,
    }
}

/// Below the floor, with `disk.dead_build_dirs` on, the verdict on taking
/// the build directory `t` WHOLE because its owner is gone: `None` when
/// that question does not arise (its owner is not gone, the floor is not
/// crossed, the key is off, no device was measured), `Err` with why it is
/// kept whole-less (it is then judged as any other), `Ok` with the witness.
/// `leaves` are the build directories named inside it, which decide their
/// own profiles.
fn whole_verdict(
    t: &TargetDir,
    survey: &Survey,
    config: Config,
    leaves: &[PathBuf],
) -> Option<Result<Witness, String>> {
    let discover::Owned::Gone { evidence, owner } = &t.owned else {
        return None;
    };
    if !config.dead_build_dirs || !config.below_auto_floor(survey.free_bytes) {
        return None;
    }
    let device = survey.volume_device?;
    Some((|| {
        let marker = t.fences()?;
        if marker != "CACHEDIR.TAG" {
            return Err(format!(
                "only a signed CACHEDIR.TAG lets a whole build directory go, and it carries {marker}"
            ));
        }
        if t.device != Some(device) {
            return Err(
                "it is on another volume than the one measured — removing it frees nothing there"
                    .to_owned(),
            );
        }
        if !leaves.is_empty() {
            return Err("a build directory named inside it decides its own profiles".to_owned());
        }
        if !t.probed_recent {
            return Err("its profiles were not probed for a build's locks".to_owned());
        }
        if t.profiles.is_empty() {
            return Err("it holds no cargo profile".to_owned());
        }
        if let Some(why) = t.profiles.iter().find_map(|p| p.in_use.clone()) {
            return Err(why);
        }
        let mut newest = 0;
        for p in &t.profiles {
            let Some(m) = p.last_used else {
                return Err(format!(
                    "{}: its caches' clock could not be read",
                    p.path.display()
                ));
            };
            newest = newest.max(m);
        }
        let age_s = survey.now.saturating_sub(newest);
        let quiet_s = owner.quiet_s(config.quiet_s());
        if age_s < quiet_s {
            return Err(format!(
                "a profile was written into {} ago, inside the {} quiet window",
                target::human_age(age_s),
                target::human_age(quiet_s)
            ));
        }
        if t.total.is_none() {
            return Err("it was not sized".to_owned());
        }
        Ok(Witness::OwnerGone {
            marker,
            evidence: evidence.clone(),
            owner: owner.clone(),
            last_used: newest,
            age_s,
            quiet_s,
            floor_gib: config.auto_free_gib,
            until_gib: config.auto_free_gib.saturating_add(PRESSURE_MARGIN_GIB),
            device,
        })
    })())
}

/// The build directory a [`Class::CargoTargets`] row frees bytes of.
fn row_dir(r: &Row) -> Option<&Path> {
    match &r.witness {
        Witness::StaleBuildDir { .. } | Witness::OwnerGone { .. } => Some(&r.path),
        Witness::LeastRecentlyUsed { build_dir, .. } => Some(build_dir),
        _ => None,
    }
}

/// The largest build directories the report can NOT reclaim ([`Report::hogs`]):
/// each sized one not inside another and still `there`, less what its rows
/// (and its nested ones') would free, x.py's grouped under their checkout's
/// `build/`, biggest first, at most [`MAX_HOGS`].
fn hogs(
    survey: &Survey,
    config: Config,
    rows: &[Row],
    notes: &[String],
    there: &dyn Fn(&Path) -> bool,
) -> Vec<Hog> {
    let sized: Vec<&TargetDir> = survey
        .targets
        .iter()
        .take(MAX_TARGETS)
        .filter(|t| t.total.is_some() && there(&t.path))
        .collect();
    let mut by: BTreeMap<PathBuf, Hog> = BTreeMap::new();
    for t in &sized {
        if sized
            .iter()
            .any(|o| o.path != t.path && t.path.starts_with(&o.path))
        {
            continue;
        }
        let Some((total, partial)) = t.total else {
            continue;
        };
        let freed = rows
            .iter()
            .filter(|r| {
                r.class == Class::CargoTargets
                    && r.removable
                    && row_dir(r).is_some_and(|d| d.starts_with(&t.path))
            })
            .fold(0u64, |a, r| a.saturating_add(r.bytes));
        let left = total.saturating_sub(freed);
        if left == 0 {
            continue;
        }
        let (path, why) = hog_of(t, config, survey, notes);
        let h = by.entry(path.clone()).or_insert(Hog {
            path,
            bytes: 0,
            bytes_partial: false,
            why,
        });
        h.bytes = h.bytes.saturating_add(left);
        h.bytes_partial |= partial;
    }
    let mut out: Vec<Hog> = by.into_values().collect();
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    out.truncate(MAX_HOGS);
    out
}

/// The largest build directories a pass LEFT: [`Report::hogs`] once `done`
/// — what [`apply_auto`] did — is known. The report counts every removable
/// row as freed; a row whose removal was REFUSED (a process running from a
/// gone owner's build directory, a lock taken or the session resumed since
/// the survey, a guard) freed nothing, and a gone owner's build directory
/// that went only IN PART freed only what its unlinks released — as did a
/// cargo profile row that released other than it was sized at (a profile a
/// build took since the survey, a delete error in an `incremental/`): each is
/// a hog again, a gone owner's saying why it stayed. A row the pass did not
/// reach (it stopped once free space was back) stays counted: the next pass
/// may take it. A build directory no longer `there` — something else removed
/// it during the pass (another sweep, a person) — is no hog, whatever this
/// pass's own removal of it said, and the next one moves up in its place.
#[must_use]
pub fn hogs_left(
    survey: &Survey,
    rep: &Report,
    done: &Applied,
    there: &dyn Fn(&Path) -> bool,
) -> Vec<Hog> {
    // Each row that did not go as planned: what it freed, and why.
    let mut short: Vec<(&Path, u64, String)> = done
        .denials
        .iter()
        .filter_map(|d| {
            let why = match d {
                Refusal::RemoveFailed(_, why) => why.clone(),
                other => other.describe(),
            };
            Some((d.path()?, 0, why))
        })
        .collect();
    short.extend(
        done.removed
            .iter()
            .filter(|(row, removed)| match &row.witness {
                Witness::OwnerGone { .. } => !removed.units.contains(&row.path),
                // A profile row is sized as its removal counts
                // (`target::cache_bytes`, the blocks its unlinks release):
                // released other than that, what it left differs too.
                Witness::StaleBuildDir { .. } | Witness::LeastRecentlyUsed { .. } => {
                    removed.bytes != row.bytes
                }
                Witness::NotLinkTarget { .. }
                | Witness::Superseded { .. }
                | Witness::VendorReports { .. } => false,
            })
            .map(|(row, removed)| {
                let why = removed
                    .trouble
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "part of it could not be removed".to_owned());
                (row.path.as_path(), removed.bytes, why)
            }),
    );
    let mut notes = rep.notes.clone();
    let rows: Vec<Row> = rep
        .rows
        .iter()
        .filter_map(|r| {
            let Some((_, freed, why)) = short.iter().find(|(p, ..)| *p == r.path) else {
                return Some(r.clone());
            };
            if matches!(r.witness, Witness::OwnerGone { .. }) {
                // `hog_of` reads it as the survey's own kept rows read.
                notes.push(format!(
                    "cargo-targets: {} — its owner is gone, but it does not go whole: {why}",
                    r.path.display()
                ));
            }
            (*freed > 0).then(|| Row {
                bytes: *freed,
                ..r.clone()
            })
        })
        .collect();
    hogs(survey, rep.config, &rows, &notes, there)
}

/// Where a hog is listed (x.py's under its checkout's `build/`) and what it
/// is, with what frees it.
fn hog_of(t: &TargetDir, config: Config, survey: &Survey, notes: &[String]) -> (PathBuf, String) {
    use discover::{Origin, Owned};
    let gone_why = |what: &str| -> String {
        let floor = config.auto_free_gib;
        if !config.dead_build_dirs {
            return format!("{what}; `[disk] dead_build_dirs` is off");
        }
        if !config.below_auto_floor(survey.free_bytes) {
            return format!("{what}; it goes whole below the {floor} GiB floor");
        }
        let prefix = format!("cargo-targets: {} — its owner is gone", t.path.display());
        match notes.iter().find_map(|n| n.strip_prefix(&prefix)) {
            Some(rest) => format!(
                "{what}; kept{}",
                rest.strip_prefix(", but it does not go whole")
                    .unwrap_or(rest)
            ),
            None => what.to_owned(),
        }
    };
    match (&t.origin, &t.owned) {
        (Origin::Bootstrap { repo, slim }, _) => (
            repo.join("build"),
            if *slim {
                "x.py bootstrap; run scripts/slim_build.py".to_owned()
            } else {
                "x.py bootstrap; `./x.py clean`".to_owned()
            },
        ),
        (Origin::ClaudeScratch { .. }, Owned::Gone { .. }) => (
            t.path.clone(),
            gone_why("an ended Claude Code session's scratch build"),
        ),
        // KEPT IS NOT LIVE: a session not called ended may run, or may only
        // not be ruled out (a process not read, a registry not read whole, no
        // transcript known): its line says which, as measured.
        (Origin::ClaudeScratch { .. }, Owned::Live(why)) => {
            (t.path.clone(), format!("kept: {why}"))
        }
        (Origin::ClaudeScratch { .. }, Owned::NotOwned) => (
            t.path.clone(),
            "a Claude Code session's scratch build".to_owned(),
        ),
        (Origin::Linked { from }, _) => (
            t.path.clone(),
            format!(
                "cargo build output reached through {}: deps/ and build/ are `targo clean`'s",
                display_home(from, survey.home.as_deref())
            ),
        ),
        (Origin::AgentTargetDir, _) => (
            t.path.clone(),
            "an agent's CARGO_TARGET_DIR: deps/ and build/ are `targo clean`'s".to_owned(),
        ),
        (Origin::BuildCache { .. }, Owned::Gone { .. }) => (
            t.path.clone(),
            gone_why("the Rust build cache of a removed workspace"),
        ),
        (Origin::BuildCache { .. }, _) => (
            t.path.clone(),
            "a workspace's Rust build cache: `targo clean` in that workspace".to_owned(),
        ),
        (Origin::Named | Origin::Checkout { .. }, _) => (
            t.path.clone(),
            "cargo build output: deps/ and build/ are `targo clean`'s".to_owned(),
        ),
    }
}

/// Is `r` a pressure row — one the pass measures free space again before
/// and may stop short of ([`Witness::OwnerGone`],
/// [`Witness::LeastRecentlyUsed`])?
fn is_pressure(r: &Row) -> bool {
    matches!(
        r.witness,
        Witness::LeastRecentlyUsed { .. } | Witness::OwnerGone { .. }
    )
}

/// Where a row sorts inside its class — the order the pass takes them in:
/// every row but a pressure row first (an idle cache, taken whole); then a
/// gone owner's whole build directory, the longest unwritten first (nothing
/// builds there again); then the least recently used profiles, oldest
/// first.
fn lru_rank(r: &Row) -> (u8, i64) {
    match r.witness {
        Witness::OwnerGone { last_used, .. } => (1, last_used),
        Witness::LeastRecentlyUsed { last_used, .. } => (2, last_used),
        _ => (0, 0),
    }
}

/// Below the automatic floor, a row for each profile the pressure pass may
/// take ([`TargetDir::pressure_candidates`]) in a build directory on the
/// measured volume, oldest first and at most [`MAX_ROWS_PER_CLASS`], and a
/// note for each recent one it keeps, each profile decided once by the
/// innermost build directory holding it ([`speaker`]); at or above it, one
/// note saying no recent profile would be taken.
fn pressure_rows(
    survey: &Survey,
    config: Config,
    whole: &[&Path],
    rows: &mut Vec<Row>,
    notes: &mut Vec<String>,
) {
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
    for t in survey.targets.iter().take(MAX_TARGETS) {
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
        // A whole row takes every profile of its build directory.
        if whole.contains(&t.path.as_path()) {
            continue;
        }
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

/// Which of `dirs` speaks for the profile at `profile`, in the idle pass and
/// under pressure alike: the innermost one holding it (the first of two that
/// are the same). A build directory named inside another is walked by both
/// surveys, one after the other, and each probes the profile's locks, so a
/// build starting between the two could make one read it free and the other
/// held. Only the innermost directory's survey decides (a row, or its reason
/// to keep the profile), and every outer survey's reading of that profile is
/// dropped.
fn speaker(dirs: &[&Path], profile: &Path) -> Option<usize> {
    dirs.iter()
        .enumerate()
        .filter(|(_, d)| profile.starts_with(d))
        .max_by_key(|&(i, d)| (d.components().count(), std::cmp::Reverse(i)))
        .map(|(i, _)| i)
}

/// The build directory `t` as the idle pass judges it: only the profiles it
/// speaks for among `deciding` ([`speaker`]; `at` is its own place there,
/// `None` when it decides nothing but its fences), and the deciding
/// directories named inside it, whose own surveys decide every other profile
/// it holds, so its row neither counts nor reclaims those
/// ([`target::reclaim_leaving`]).
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

/// Whole days between `then` and `now`. A future mtime answers 0 ("brand
/// new"), which never clears a threshold: the safe direction for a machine
/// whose clock moved.
#[must_use]
pub fn age_days(now: i64, then: i64) -> i64 {
    now.saturating_sub(then).max(0) / DAY_S
}

/// A byte count a person can read, deliberately coarse: the bounded walk has
/// no byte precision.
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
    /// No class was named. This is the default, not an error: report-only.
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

/// One denial row. Every refusal above becomes one of these; none is dropped.
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

/// One intent row, written before a row's removal starts: a pass cut short
/// (a quit, a panic, an update) leaves this on record even with no outcome
/// row.
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

/// One removal row. The witness travels with it, and what it says was
/// removed is the remover's answer (the bytes actually released, each unit
/// that went, each skipped unit and every delete error), never the report's
/// estimate.
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

/// One report row, for the durable record of a measurement.
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

/// What an apply would do. Pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// No class named: the default, and it removes nothing.
    ReportOnly(Refusal),
    /// A class was named and refused.
    Denied(Refusal),
    /// A class was named and these rows would go.
    Remove(Class, Vec<Row>),
}

/// Decide what `class` would do against `rep`. Pure: no filesystem.
///
/// `None` is [`Plan::ReportOnly`]: the design's "report-only by default" as
/// the shape of the function.
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

/// The second fence. `path` may be removed under `class` only if every one
/// of these holds; anything else is a [`Refusal`], and the caller journals it.
///
/// This repeats work [`plan`] did, on purpose: [`plan`]'s answer is a list
/// the caller could edit, and what finally reaches the filesystem is a path,
/// checked here.
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

/// What one row's removal did — the remover's answer, never the report's
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
    /// them) cover what free space was short of that target at its first
    /// pressure row, though the figure has not risen to match: a local
    /// snapshot, a clone sharing the blocks or a deferred free can keep an
    /// unlink out of `statvfs`/`df` for a while, and without this stop the
    /// pass would take every row it has.
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
    /// The rows it did not take (each a build cache more recently used than
    /// every one it took).
    pub left: usize,
}

impl Stop {
    /// The sentence a person reads.
    #[must_use]
    pub fn describe(&self) -> String {
        let kept = format!(
            "{} more recently used cache{} kept",
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

/// One step of an apply, told to the caller as it happens ([`apply`],
/// [`apply_auto`]), so a journal written from it records a removal's intent
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
/// It answers whether the step is on record (its journal row written): a
/// [`Step::Removing`] that is not stops that row's removal before it starts
/// ([`Refusal::Unjournalled`]), so nothing is removed without its intent on
/// record. The answer to any other step is not acted on.
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

/// One `reserve` row of the disk ledger: the reserve ([`reserve`]) was
/// `released` (its `bytes` unlinked, so the rows after it can be written) or
/// `written`, or `refused` (something not ours in its place).
#[must_use]
pub fn reserve_row(now: i64, action: &str, bytes: Option<u64>, text: &str) -> String {
    let mut o = Map::new();
    o.insert("ts".to_owned(), Value::from(rfc3339_utc(now)));
    o.insert("sid".to_owned(), Value::from(String::new()));
    o.insert("kind".to_owned(), Value::from("reserve".to_owned()));
    o.insert("action".to_owned(), Value::from(action.to_owned()));
    o.insert("bytes".to_owned(), bytes.map_or(Value::Null, Value::from));
    o.insert(
        "text".to_owned(),
        Value::from(truncate_bytes(text, ROW_TEXT_CAP).to_owned()),
    );
    aterm_json::to_string(&Value::Object(o))
        .unwrap_or_else(|_| "{\"kind\":\"reserve\",\"reason\":\"unserializable\"}".to_owned())
}

/// One `build-cache-sweep` row of the disk ledger: a removed workspace's
/// build directory in aterm's Rust build cache, `bytes` in it, about to go
/// whole under its locks (`action` `removing`, the write-ahead intent row)
/// or gone (`removed`) (`atpkg::build_cache::reclaim_under_pressure`).
#[must_use]
pub fn sweep_row(now: i64, path: &Path, action: &str, bytes: u64) -> String {
    let mut o = Map::new();
    o.insert("ts".to_owned(), Value::from(rfc3339_utc(now)));
    o.insert("sid".to_owned(), Value::from(String::new()));
    o.insert(
        "kind".to_owned(),
        Value::from("build-cache-sweep".to_owned()),
    );
    o.insert("action".to_owned(), Value::from(action.to_owned()));
    o.insert("path".to_owned(), Value::from(path_text(path)));
    o.insert("bytes".to_owned(), Value::from(bytes));
    o.insert(
        "text".to_owned(),
        Value::from(
            "a removed workspace's build cache, whole: unwritten ten minutes, no build \
             holding its locks"
                .to_owned(),
        ),
    );
    aterm_json::to_string(&Value::Object(o))
        .unwrap_or_else(|_| "{\"kind\":\"build-cache-sweep\"}".to_owned())
}

/// One `poke` row of the disk ledger: something saw the disk full and asked
/// for a look now ([`poke`]), with the free figure measured on it.
#[must_use]
pub fn poke_row(now: i64, cause: &poke::Cause, free: Option<u64>, level: Level) -> String {
    let mut o = Map::new();
    o.insert("ts".to_owned(), Value::from(rfc3339_utc(now)));
    o.insert(
        "sid".to_owned(),
        Value::from(match cause {
            poke::Cause::Screen { sid, .. } => sid.clone(),
            poke::Cause::Write(_) => String::new(),
        }),
    );
    o.insert("kind".to_owned(), Value::from("poke".to_owned()));
    o.insert(
        "free_bytes".to_owned(),
        free.map_or(Value::Null, Value::from),
    );
    o.insert("level".to_owned(), Value::from(level.as_str().to_owned()));
    o.insert(
        "text".to_owned(),
        Value::from(truncate_bytes(&cause.describe(), ROW_TEXT_CAP).to_owned()),
    );
    aterm_json::to_string(&Value::Object(o))
        .unwrap_or_else(|_| "{\"kind\":\"poke\",\"reason\":\"unserializable\"}".to_owned())
}

/// One stop row: where the pressure pass stopped, on what free figure, and
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

/// What an apply did.
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

    /// What went, counted by what it was ([`Went`]).
    #[must_use]
    pub fn went(&self) -> Went {
        let mut went = Went::default();
        for (row, removed) in &self.removed {
            if !matches!(row.witness, Witness::OwnerGone { .. }) {
                went.profiles += removed.units.len();
            } else if removed.units.contains(&row.path) {
                went.whole += 1;
            } else {
                went.in_part += 1;
            }
        }
        went
    }
}

/// What an apply removed, counted ([`Applied::went`]): a gone owner's build
/// directory goes whole or, when a delete error kept part of it (its
/// [`Removed::trouble`]), only in part; every other unit is one cargo
/// profile's `incremental/` (or one row's directory of another class).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Went {
    /// Gone owners' build directories that went whole.
    pub whole: usize,
    /// Gone owners' build directories that went only in part.
    pub in_part: usize,
    /// Every other unit that went.
    pub profiles: usize,
}

/// The automatic grant. Pure. Below the report's automatic floor
/// ([`Config::below_auto_floor`]) the removable [`Class::CargoTargets`] rows,
/// in the order they are taken (the idle build directories, then the
/// pressure rows least recently used first, of which [`apply_auto`] takes
/// only as many as free space needs), and never a row of another class,
/// whatever `disk.apply` says; at or above it, or on an unknown free figure,
/// `None`.
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

/// Free space on the measured volume, measured again: what the pressure pass
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
/// `remove` is injected so this is testable without a disk ([`remover`] is
/// the real one); [`guard`] runs before every call to it, and a guard refusal
/// is recorded rather than returned, so one refused path does not abandon
/// the rest of the grant.
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
/// told before each removal (a removal whose intent is not on record does
/// not start), its outcome after. Before each pressure row free space is
/// measured again, and the pass stops (a [`Stop`], told and kept) once it is
/// at [`Config::pressure_target_bytes`], once the bytes the pressure rows
/// released cover what it was short at the first of them
/// ([`StopBy::Counted`]), or when it cannot be read.
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
        let pressure = is_pressure(&row);
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

/// The real remover, one primitive per class: [`Class::CargoTargets`] goes
/// through [`target::reclaim_leaving`] under `judge` (each idle profile's
/// `incremental/`, under cargo's locks — never the directory, and never a
/// profile of a build directory named inside it, which its own row takes)
/// or, for a pressure row, [`target::reclaim_lru`] (that one profile's, on
/// the device its row was measured on) — or, for a gone owner's build
/// directory, no process using it ([`discover::unused`], every process of
/// this uid) and then [`target::reclaim_whole`], the owner checked again
/// under its locks ([`discover::recheck`], a session's by the running
/// processes read again) — and [`Class::ClaudeStaleVersions`] through
/// [`remove_tree`]. A delegated class never reaches a remover ([`plan`]
/// refuses it).
pub fn remover(judge: Judge) -> impl FnMut(&Row) -> std::io::Result<Removed> {
    remover_among(
        judge,
        aterm_uds::holders::scan_path_users,
        discover::running_claude_census,
    )
}

/// [`remover`], a gone owner's build directory judged against the running
/// processes `processes` reads ([`discover::Processes`]) and a session's
/// against the running Claude Codes and the directories the processes were
/// started with (`census`, [`discover::Census`]): the product's are every
/// process of this uid, a test's the ones it names.
pub(crate) fn remover_among(
    judge: Judge,
    processes: impl Fn(&[PathBuf], &[u32]) -> std::io::Result<aterm_uds::holders::PathScan>,
    census: impl Fn(Option<&Path>) -> discover::ClaudeCensus,
) -> impl FnMut(&Row) -> std::io::Result<Removed> {
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
            // A process running from it keeps it ([`discover::unused`]) —
            // asked BEFORE the reclaim opens anything there, since a child
            // this process forks meanwhile carries the reclaim's own handles
            // until it execs. Then, under every lock, right before the
            // delete ([`discover::recheck`]): the directory still inside its
            // owner's folder, and a session resumed since the survey keeps it.
            Witness::OwnerGone {
                owner,
                last_used,
                quiet_s,
                device,
                ..
            } => {
                discover::unused(owner, &row.path, &processes).map_err(|why| {
                    std::io::Error::other(format!("no longer a candidate: {why}"))
                })?;
                target::reclaim_whole(
                    &row.path,
                    &Judge {
                        device: Some(*device),
                        ..judge
                    },
                    *last_used,
                    *quiet_s,
                    &mut || discover::recheck(owner, &row.path, judge.now, *quiet_s, &census),
                )
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
// The resume — a session the full disk stopped is told once space is back
// ---------------------------------------------------------------------------

/// The ledger's name for the resume typed into a session whose screen said
/// the disk was full ([`resume_text`]), once space is back.
pub const RULE_DISK_RESUME: &str = "disk-resume@v1";

/// What is typed into a session whose Claude Code said `ENOSPC` (or
/// `EDQUOT`) while the volume measured full, once a look measures it back:
/// the harness's mark, what aterm freed (`freed` bytes; `top`, the largest
/// of what went, at most two named), and the ask to retry what failed. It
/// says no more than was measured.
#[must_use]
pub fn resume_text(code: &str, freed: u64, top: &[String]) -> String {
    let what = if freed == 0 {
        "space is free again".to_owned()
    } else {
        let named: Vec<&str> = top.iter().take(2).map(String::as_str).collect();
        if named.is_empty() {
            format!("aterm freed {}", human_bytes(freed))
        } else {
            format!("aterm freed {} ({})", human_bytes(freed), named.join("; "))
        }
    };
    format!(
        "{} The disk ran out of space ({code}) and {what}. Writes work again: retry what failed \
         with {code} and carry on.",
        super::upgrade::HARNESS_MARK
    )
}

/// Type `text` into tab `sid` of the instance at `sock` as one fenced turn
/// (`upgrade_drive::type_fenced`: under the hand, judged on a fresh read —
/// a box, a draft, a person typing or an owner's hold refuses it). The
/// step's word: `typed`, or `wait:<why>` to try again at the next idle
/// point.
#[must_use]
pub fn type_resume(sock: &str, sid: &str, text: &str) -> String {
    let mut c = match crate::RelayClient::connect_local_beside(sock) {
        Ok(c) => c,
        Err(e) => return format!("wait:no-socket:{e}"),
    };
    match super::upgrade_drive::type_fenced(
        &mut c,
        sid,
        text,
        None,
        super::upgrade_drive::Put::Paste,
        super::Timings::for_build().fence_every,
    ) {
        Ok(()) => "typed".to_owned(),
        Err(why) => format!("wait:{}", why.word()),
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
    /// The package store's root (`<prefix>/store`), read only: every row it
    /// produces is delegated to `aterm pkg gc`, so nothing here takes the
    /// store lock or writes under it.
    pub store_dir: Option<PathBuf>,
    /// Candidate build directories, named by the caller.
    pub targets: Vec<PathBuf>,
    /// Build directories discovery found ([`discover::discover`]), surveyed
    /// after the named ones, each with how it was found.
    pub found: Vec<discover::Found>,
    /// What discovery could not finish, carried into the report's notes.
    pub found_notes: Vec<String>,
    /// `$HOME`: how the report's lines abbreviate paths.
    pub home: Option<PathBuf>,
    /// The Claude Code directories whose registries and transcripts say
    /// whether a session ended ([`discover::Roots::claude_dirs`]).
    pub claude_dirs: Vec<PathBuf>,
    /// The running Claude Codes ([`discover::Roots::claudes`]).
    pub claudes: Vec<discover::RunningClaude>,
    /// Why no session is called ended: a process that may be a Claude Code
    /// could not be read ([`discover::Roots::claude_unread`]).
    pub claude_unread: Option<String>,
    /// Where transcripts live.
    pub transcripts: Option<PathBuf>,
}

/// Measure one machine into a [`Survey`]: the one function here that
/// touches a filesystem; everything else is a pure function of its result.
///
/// Free space is left to the caller (`free`): the statvfs edge lives in
/// `atpkg`, which this crate does not depend on. `None` fails open.
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
    // Both sides are canonical or neither is compared: `report` decides the
    // whole class by path equality, and on macOS `/var` resolves to
    // `/private/var`, so comparing a resolved link against an unresolved
    // listing would make the live version read as stale.
    let versions_root = roots
        .versions_dir
        .as_ref()
        .and_then(|d| std::fs::canonicalize(d).ok());
    s.live_version = roots
        .live_link
        .as_ref()
        .and_then(|l| std::fs::canonicalize(l).ok())
        // The link points at the executable inside a version directory on a
        // native install; the version directory is the ancestor whose parent
        // is the versions root, else the link's own target.
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
    s.home = roots.home.clone();
    s.notes = roots.found_notes.clone();
    // ONE ROW PER DIRECTORY: a `target` link and the `target.noindex` it
    // names, or two cwds that differ by a linked component, are one build
    // directory once canonical — counted once, reclaimed once.
    let named = roots
        .targets
        .iter()
        .map(|p| (p.as_path(), discover::Origin::Named));
    let found = roots
        .found
        .iter()
        .map(|f| (f.path.as_path(), f.origin.clone()));
    let mut owners = discover::Owners::new(
        roots.home.clone(),
        roots.claude_dirs.clone(),
        roots.claudes.clone(),
        roots.claude_unread.clone(),
        now,
        config.quiet_s(),
    );
    for (t, origin) in named.chain(found).take(MAX_TARGETS) {
        let Ok(canonical) = std::fs::canonicalize(t) else {
            continue;
        };
        if s.targets.iter().any(|seen| seen.path == canonical) {
            continue;
        }
        if let Some(mut td) = scan_target(&canonical, now, config.target_stale_days, pressure) {
            td.owned = owners.of(&origin);
            td.origin = origin;
            td.total = Some(target::total_bytes(&td.path));
            s.targets.push(td);
        }
    }
    // The census refusal, said once where the report is read (`aterm
    // harness disk`'s notes): while it stands no session's scratch build is
    // called ended. Each one's hog line says it too ([`hog_of`]).
    if let Some(why) = &roots.claude_unread
        && s.targets
            .iter()
            .any(|t| matches!(t.origin, discover::Origin::ClaudeScratch { .. }))
    {
        s.notes.push(format!(
            "cargo-targets: no Claude Code session's scratch build is called ended: {why}"
        ));
    }
    s
}

/// Read `<prefix>/store` into [`Survey::store`] and [`Survey::live_builds`].
///
/// The layout is `<store>/<program>/<build>/` with a `current` symlink naming
/// the live build (e.g. `store/ty/current -> …/store/ty/3007` beside `2984/`,
/// `3007/` and their `.ready` files). A program whose `current` does not
/// resolve contributes no live build, so none of its builds is superseded:
/// `report` emits a note instead of rows.
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
/// reports is canonical.
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

/// Does this `CACHEDIR.TAG` carry the specification's signature? Read
/// bounded (the signature is the first line); a file that is not a regular
/// file answers `false`.
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
        // Before the epoch: old, which is the direction that matters here,
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
    walk_bytes(dir, |_| true, std::fs::Metadata::len)
}

/// The bounded byte walk under [`dir_bytes`] and [`target::cache_bytes`]:
/// iterative, never following a symlink, stopping at [`MAX_WALK_ENTRIES`];
/// it enters a directory only where `enter` says so and sums `size` over
/// everything else. Returns the bytes, and whether the walk was cut short.
fn walk_bytes(
    dir: &Path,
    enter: impl Fn(&std::fs::Metadata) -> bool,
    size: impl Fn(&std::fs::Metadata) -> u64,
) -> (u64, bool) {
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
                if enter(&meta) {
                    stack.push(entry.path());
                }
            } else {
                total = total.saturating_add(size(&meta));
            }
        }
    }
    (total, false)
}

/// Free bytes on the volume holding `path`, or `None` on any failure.
///
/// `None` fails open, as `atpkg`'s `freespace::available_bytes` does: a
/// report that cannot read the disk says `free=unknown` and warns about
/// nothing.
///
/// Uses `df -Pk`: std has no free-space call, `statvfs` needs `unsafe`
/// (not written in this crate), and `atpkg`'s `platform::volume_free_bytes`
/// sits behind a crate this one does not depend on.
/// `crates/aterm-verify/src/disk.rs`'s `parse_df` is the reference for
/// [`parse_df_kb`].
///
/// It is also BOUNDED. `df` walks the mount table, and a stale NFS or SMB
/// mount under `path` makes it block for as long as that server stays gone —
/// a deadline-free `output()` would wedge the whole report there. A
/// measurement that cannot return is worse than `free=unknown`, which this
/// function already answers safely, so [`DF_BUDGET`] is the deadline and the
/// shipped bounded runner ([`super::align::capture_bounded`]) is what enforces
/// it.
///
/// Built through `aterm_uds::spawnfd::command`: on Windows a `df` on `PATH` (Git for
/// Windows, MSYS) is a console image, and the window's harness host, which has
/// no console, would flash a console window for it.
#[must_use]
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut cmd = aterm_uds::spawnfd::command("df");
    cmd.arg("-Pk").arg(path);
    let got = super::align::capture_bounded(cmd, DF_BUDGET, MAX_DF_STDOUT).ok()?;
    if !got.success() {
        return None;
    }
    parse_df_kb(&String::from_utf8_lossy(&got.stdout))
}

/// The deadline on the one `df` a report runs. A target: a local volume
/// answers in milliseconds, and a mount slower than this is not going to
/// answer.
pub const DF_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// The byte cap on `df` output. `df -Pk <path>` prints a header and one row;
/// the cap is what keeps a pathological filesystem list out of memory.
pub const MAX_DF_STDOUT: usize = 64 * 1024;

/// The `Available` column of `df -Pk` output, in bytes.
///
/// Anchored on `Capacity`, never on a field count: `-P` fixes the column
/// order (`Filesystem 1024-blocks Used Available Capacity Mounted on`) and
/// nothing else, and on macOS `df -Pk /System/Volumes/Data/home` prints seven
/// fields because autofs names the source `map auto_home`. `Capacity` is the
/// one percentage cell and `Available` is the field before it.
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
/// name that merely contains a `%` cannot be mistaken for the anchor.
fn is_percentage(field: &str) -> bool {
    field
        .strip_suffix('%')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Remove one directory tree: [`remover`]'s primitive for
/// [`Class::ClaudeStaleVersions`] (never for a build directory, whose unit is
/// [`target::reclaim`]'s).
///
/// It refuses a symlink: `remove_dir_all` through one would reach a tree no
/// witness was taken of.
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
