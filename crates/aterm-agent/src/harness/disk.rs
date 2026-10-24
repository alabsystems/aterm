// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Disk watch and cleanup (design `docs/DESIGN-aterm-wrapper-2026-09-17.md` §5.5).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use aterm_json::{Map, Value};

use super::truncate_bytes;
use super::usage::rfc3339_utc;

/// `disk.warn_free_gib` (design §5.5).
pub const DEFAULT_WARN_FREE_GIB: u64 = 40;

/// `disk.target_stale_days` (design §5.5).
pub const DEFAULT_TARGET_STALE_DAYS: i64 = 1;

/// `disk.apply` (design §5.5): the owner's verb removes nothing until this is on.
pub const DEFAULT_APPLY: bool = false;

/// `disk.auto_free_gib`.
pub const DEFAULT_AUTO_FREE_GIB: u64 = 10;

/// How far above the floor the pressure pass reclaims before it stops.
pub const PRESSURE_MARGIN_GIB: u64 = 5;

/// The automatic floor is at least this percent of the volume's size ([`Config::for_volume`]).
pub const AUTO_FREE_PCT: u64 = 5;

/// The warning threshold is at least this percent of the volume's size ([`Config::for_volume`]).
pub const WARN_FREE_PCT: u64 = 8;

/// The most the volume's share ([`AUTO_FREE_PCT`]) raises the automatic floor to.
pub const AUTO_FREE_SHARE_MAX_GIB: u64 = 32;

/// The most the volume's share ([`WARN_FREE_PCT`]) raises the warning to.
pub const WARN_FREE_SHARE_MAX_GIB: u64 = 64;

/// The most the volume's share ([`EMERGENCY_FREE_PCT`]) raises the emergency line to.
pub const EMERGENCY_FREE_SHARE_MAX_GIB: u64 = 8;

/// An UNWRITTEN `warn_free_gib` is held to at most this percent of the volume ([`Config::on_volume`]).
pub const SMALL_WARN_MAX_PCT: u64 = 20;

/// An UNWRITTEN `auto_free_gib` is held to at most this percent of the volume ([`Config::on_volume`]).
pub const SMALL_FLOOR_MAX_PCT: u64 = 10;

/// THE EMERGENCY: below the larger of this many GiB and [`EMERGENCY_FREE_PCT`] of the volume ([`Thresholds::emergency`]), or with fewer than [`EMERGENCY_FILES_FREE`] inodes, writes are about to fail with `ENOSPC`.
pub const EMERGENCY_FREE_GIB: u64 = 4;

/// See [`EMERGENCY_FREE_GIB`].
pub const EMERGENCY_FREE_PCT: u64 = 1;

/// Fewer free inodes than this is an emergency whatever the bytes say: a volume out of inodes fails a write with `ENOSPC` too.
pub const EMERGENCY_FILES_FREE: u64 = 50_000;

/// The `CACHEDIR.TAG` signature every cargo-compatible build tool writes into a target directory (the Cache Directory Tagging Specification's fixed line, which cargo emits verbatim).
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

#[path = "disk_room.rs"]
pub mod room;

#[path = "disk_idle.rs"]
pub mod idle;

#[path = "disk_beat.rs"]
pub mod beat;

#[path = "disk_versions.rs"]
pub mod versions;

#[cfg(windows)]
#[path = "disk_win.rs"]
pub(crate) mod win;

pub use target::{CARGO_LOCKS, INCREMENTAL, Judge, PRESSURE_MIN_IDLE_S, Profile, TargetDir};

/// `disk.idle_build_dirs`.
pub const DEFAULT_IDLE_BUILD_DIRS: bool = true;

/// The most bytes of any quoted path or reason one ledger row may carry.
pub const ROW_TEXT_CAP: usize = 512;

/// The most rows one report carries per class: one report stays one screen of decisions, and every walk below stays bounded.
pub const MAX_ROWS_PER_CLASS: usize = 64;

/// The most directory entries one size walk visits before it stops and marks the figure partial.
pub const MAX_WALK_ENTRIES: usize = 200_000;

/// The most build directories one survey measures: the named ones first, then what discovery found ([`discover::MAX_FOUND`]).
pub const MAX_TARGETS: usize = discover::MAX_FOUND;

/// The most entries of the report's list of the largest build directories it cannot reclaim ([`Report::hogs`]).
pub const MAX_HOGS: usize = 5;

/// `disk.dead_build_dirs`: below the floor, a build directory whose OWNER IS GONE goes WHOLE ([`Witness::OwnerGone`]).
pub const DEFAULT_DEAD_BUILD_DIRS: bool = true;

const GIB: u64 = 1024 * 1024 * 1024;

const DAY_S: i64 = 86_400;

/// The safelist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    AtpkgGc,
    ClaudeStaleVersions,
    CargoTargets,
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
    pub const fn as_str(self) -> &'static str {
        match self {
            Class::AtpkgGc => "atpkg-gc",
            Class::ClaudeStaleVersions => "claude-stale-versions",
            Class::CargoTargets => "cargo-targets",
            Class::ClaudePurge => "claude-purge",
        }
    }

    /// Parse a class name.
    #[must_use]
    pub fn parse(s: &str) -> Option<Class> {
        Class::ALL.iter().copied().find(|c| c.as_str() == s)
    }

    /// The command that owns removal for this class, when it is not us.
    #[must_use]
    pub const fn delegate(self) -> Option<Delegate> {
        match self {
            Class::AtpkgGc => Some(Delegate::AtermPkgGc),
            Class::ClaudePurge => Some(Delegate::ClaudeProjectPurge),
            Class::ClaudeStaleVersions | Class::CargoTargets => None,
        }
    }
}

/// A command that owns a class's removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delegate {
    AtermPkgGc,
    ClaudeProjectPurge,
}

impl Delegate {
    /// The command to run, verbatim.
    #[must_use]
    pub const fn command(self) -> &'static str {
        match self {
            Delegate::AtermPkgGc => "aterm pkg gc",
            Delegate::ClaudeProjectPurge => "claude project purge --dry-run",
        }
    }

    /// Why this module will not run it.
    #[must_use]
    pub const fn because(self) -> &'static str {
        match self {
            Delegate::AtermPkgGc => "the store is under atpkg's single-writer lock",
            Delegate::ClaudeProjectPurge => "it reaches the vendor's own project history",
        }
    }
}

/// The `[disk]` knobs of design §5.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Below this many free GiB the report warns.
    pub warn_free_gib: u64,
    /// A cargo profile a compile wrote into (`deps/`, `.fingerprint/`, `build/` or `examples/`) within this many days is not idle.
    pub target_stale_days: i64,
    /// The durable `disk.apply` switch.
    pub apply: bool,
    /// Below this many free GiB the host's tick applies [`auto_plan`]; `0` is off ([`DEFAULT_AUTO_FREE_GIB`]).
    pub auto_free_gib: u64,
    /// Once the volume is low, may a build directory whose owner is gone go whole ([`DEFAULT_DEAD_BUILD_DIRS`], tier 1)?
    pub dead_build_dirs: bool,
    /// `disk.room_gib`: the room PINNED, read as written (`None`: the watch's measured room, [`room`]).
    pub room_gib: Option<u64>,
    /// `disk.idle_build_dirs`.
    pub idle_build_dirs: bool,
    /// `disk.idle_build_days`: tier 3's window ([`idle::DEFAULT_IDLE_BUILD_DAYS`]).
    pub idle_build_days: i64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            warn_free_gib: DEFAULT_WARN_FREE_GIB,
            target_stale_days: DEFAULT_TARGET_STALE_DAYS,
            apply: DEFAULT_APPLY,
            auto_free_gib: DEFAULT_AUTO_FREE_GIB,
            dead_build_dirs: DEFAULT_DEAD_BUILD_DIRS,
            room_gib: None,
            idle_build_dirs: DEFAULT_IDLE_BUILD_DIRS,
            idle_build_days: idle::DEFAULT_IDLE_BUILD_DAYS,
        }
    }
}

/// Every `[disk]` key of `aterm.toml`, in the order the help lists them.
pub const KEYS: &[&str] = &[
    "apply",
    "warn_free_gib",
    "target_stale_days",
    "auto_free_gib",
    "dead_build_dirs",
    "room_gib",
    "idle_build_dirs",
    "idle_build_days",
];

impl Config {
    /// The shipped default of `[disk] <key>` as a settings help line shows it (`None` for a key that is not in [`KEYS`]).
    #[must_use]
    pub fn default_shown(key: &str) -> Option<String> {
        let d = Config::default();
        Some(match key {
            "apply" => d.apply.to_string(),
            "warn_free_gib" => d.warn_free_gib.to_string(),
            "target_stale_days" => d.target_stale_days.to_string(),
            "auto_free_gib" => format!("{} (0: off)", d.auto_free_gib),
            "dead_build_dirs" => d.dead_build_dirs.to_string(),
            "room_gib" => {
                "unset (measured: the second-largest daily two-hour fall of the week)".to_owned()
            }
            "idle_build_dirs" => d.idle_build_dirs.to_string(),
            "idle_build_days" => format!("{} (below 1: tier 3 off)", d.idle_build_days),
            _ => return None,
        })
    }

    /// How a negative `[disk] <key>` is read, for the settings checker to say on the line.
    #[must_use]
    pub fn negative_reading(key: &str) -> Option<String> {
        let d = Config::default();
        Some(match key {
            "auto_free_gib" => "0: the automatic removal is off".to_owned(),
            "warn_free_gib" => format!("the default, {}", d.warn_free_gib),
            "target_stale_days" => format!("the default, {}", d.target_stale_days),
            "idle_build_days" => "below 1: tier 3 is off".to_owned(),
            _ => return None,
        })
    }

    /// Whether tier 3 ([`Witness::IdleWhole`]) may remove.
    #[must_use]
    pub const fn idle_whole_on(&self) -> bool {
        self.idle_build_dirs && self.idle_build_days >= 1 && self.auto_free_gib > 0
    }

    /// Whether `free` bytes is below the automatic floor.
    #[must_use]
    pub fn below_auto_floor(&self, free: Option<u64>) -> bool {
        self.auto_free_gib > 0 && free.is_some_and(|f| f < self.auto_free_gib.saturating_mul(GIB))
    }

    /// Whether `free` bytes is below the warning: the volume is `low` (or worse) and tiers 1 and 2 run ([`pass_plan`]).
    #[must_use]
    pub fn below_warn(&self, free: Option<u64>) -> bool {
        free.is_some_and(|f| f < self.warn_free_gib.saturating_mul(GIB))
    }

    /// The free bytes the idle tiers (2 and 3) reclaim up to ([`pass_plan`]).
    #[must_use]
    pub fn idle_target_bytes(&self, short: bool) -> u64 {
        let warn = self.warn_free_gib.saturating_mul(GIB);
        if short {
            warn.max(self.pressure_target_bytes())
        } else {
            warn
        }
    }

    /// The free bytes the pressure pass reclaims up to before it stops: the floor plus [`PRESSURE_MARGIN_GIB`].
    #[must_use]
    pub const fn pressure_target_bytes(&self) -> u64 {
        self.auto_free_gib
            .saturating_add(PRESSURE_MARGIN_GIB)
            .saturating_mul(GIB)
    }

    /// This config on a volume of `total` bytes with no knob written ([`Self::on_volume`]).
    #[must_use]
    pub fn for_volume(self, total: Option<u64>) -> Config {
        self.on_volume(total, Written::default())
    }

    /// This config on a volume of `total` bytes.
    #[must_use]
    pub fn on_volume(self, total: Option<u64>, written: Written) -> Config {
        self.on_volume_with(total, written, None)
    }

    /// [`Self::on_volume`], the lines raised to this machine's ROOM (`room` bytes, [`room`]; `disk.room_gib` pins it, read as written).
    #[must_use]
    pub fn on_volume_with(self, total: Option<u64>, written: Written, room: Option<u64>) -> Config {
        let base = self.on_volume_base(total, written);
        let Some(room) = self.room_gib.map(|g| g.saturating_mul(GIB)).or(room) else {
            return base;
        };
        if total.is_none() {
            return base;
        }
        let room_gib = room.div_ceil(GIB);
        Config {
            warn_free_gib: if written.warn_free_gib {
                base.warn_free_gib
            } else {
                base.warn_free_gib.max(room_gib.saturating_mul(2))
            },
            auto_free_gib: if base.auto_free_gib == 0 || written.auto_free_gib {
                base.auto_free_gib
            } else {
                base.auto_free_gib.max(room_gib)
            },
            ..base
        }
    }

    fn on_volume_base(self, total: Option<u64>, written: Written) -> Config {
        let Some(total) = total else {
            return self;
        };
        let share = |pct: u64, max: u64| (total / 100).saturating_mul(pct).div_ceil(GIB).min(max);
        let held = |knob: u64, pct: u64| knob.min((total.saturating_mul(pct) / 100 / GIB).max(1));
        Config {
            warn_free_gib: if written.warn_free_gib {
                self.warn_free_gib
            } else {
                held(self.warn_free_gib, SMALL_WARN_MAX_PCT)
                    .max(share(WARN_FREE_PCT, WARN_FREE_SHARE_MAX_GIB))
            },
            auto_free_gib: if self.auto_free_gib == 0 || written.auto_free_gib {
                self.auto_free_gib
            } else {
                held(self.auto_free_gib, SMALL_FLOOR_MAX_PCT)
                    .max(share(AUTO_FREE_PCT, AUTO_FREE_SHARE_MAX_GIB))
            },
            ..self
        }
    }

    /// The thresholds in force on a volume of `total` bytes with no knob written ([`Self::thresholds_on`]).
    #[must_use]
    pub fn thresholds(self, total: Option<u64>) -> Thresholds {
        self.thresholds_on(total, Written::default())
    }

    /// The thresholds in force on a volume of `total` bytes ([`Self::on_volume`]), in bytes.
    #[must_use]
    pub fn thresholds_on(self, total: Option<u64>, written: Written) -> Thresholds {
        self.thresholds_with(total, written, None)
    }

    /// [`Self::thresholds_on`], the lines raised to the volume's room ([`Self::on_volume_with`]).
    #[must_use]
    pub fn thresholds_with(
        self,
        total: Option<u64>,
        written: Written,
        room: Option<u64>,
    ) -> Thresholds {
        let c = self.on_volume_with(total, written, room);
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

    /// How long nothing of a gone owner may have been written before its build directory goes whole.
    #[must_use]
    pub fn quiet_s(&self) -> i64 {
        self.target_stale_days.max(1).saturating_mul(DAY_S)
    }
}

/// Which of the volume-scaled `[disk]` knobs the owner WROTE in aterm.toml ([`Config::on_volume`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Written {
    /// `disk.auto_free_gib` is written.
    pub auto_free_gib: bool,
    /// `disk.warn_free_gib` is written.
    pub warn_free_gib: bool,
}

/// The free-space thresholds in force on one volume ([`Config::thresholds`]), in bytes.
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
    Unknown,
    Ok,
    Low,
    Short,
    Emergency,
}

impl Level {
    /// The wire spelling (`disk=` of a status line, `level` of the JSON).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
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
    /// Where `free` bytes and `files_free` inodes stand.
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

/// One vendor version under `versions/`: a directory, or (every native install, and Windows') a single executable FILE.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VersionDir {
    /// Absolute path.
    pub path: PathBuf,
    /// Size in bytes: a file's length, a directory's as far as the walk got.
    pub bytes: u64,
    /// `true` when the walk hit [`MAX_WALK_ENTRIES`] and stopped.
    pub bytes_partial: bool,
    /// Why it is KEPT though it is not the live one ([`versions::kept`]).
    pub kept: Option<String>,
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

/// What made the host re-measure: the owner's verb, or the window's harness host on its tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    OnDemand,
    Tick,
}

impl Trigger {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Trigger::OnDemand => "on-demand",
            Trigger::Tick => "tick",
        }
    }
}

/// Everything [`report`] reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Survey {
    /// Unix seconds.
    pub now: i64,
    /// What made this measurement happen.
    pub trigger: Trigger,
    /// The volume the free figure is about.
    pub root: PathBuf,
    /// Free bytes, or `None` when the query failed: the report then says `free=unknown` and never warns on a number it does not have.
    pub free_bytes: Option<u64>,
    /// The device of [`Self::root`], when it could be read.
    pub volume_device: Option<u64>,
    /// Candidate build directories.
    pub targets: Vec<TargetDir>,
    /// Vendor version directories.
    pub versions: Vec<VersionDir>,
    /// The live version, when the launcher named one ([`versions::live_of`]).
    pub live_version: Option<PathBuf>,
    /// When [`Self::live_version`] was named BY CONTENT, the launcher that is its copy (`~\.local\bin\claude.exe`).
    pub live_copy: Option<PathBuf>,
    /// Why no live version was named, when the launcher named none (`None`: the live symlink did not resolve).
    pub live_unnamed: Option<String>,
    /// Builds in the package store.
    pub store: Vec<StoreBuild>,
    /// The live build of each program in the store.
    pub live_builds: BTreeMap<String, String>,
    /// Where transcripts live.
    pub transcripts_root: Option<PathBuf>,
    /// Is the vendor's `claude project purge` available to surface?
    pub purge_available: bool,
    /// `$HOME`, for the report's lines (`~/…`).
    pub home: Option<PathBuf>,
    /// What the search for build directories could not finish ([`discover::Discovered::notes`]).
    pub notes: Vec<String>,
    /// The volume's ROOM, when one was read ([`room`]): what one round of this machine's builds takes.
    pub room: Option<u64>,
    /// The mount table read for this survey ([`aterm_sysprobe::volumes::table`]).
    pub mounts: Option<aterm_sysprobe::volumes::Table>,
    /// The free bytes an agent's wait needs and the pass adopted (a DEMAND, `await sys free>=`).
    pub demand: Option<u64>,
}

impl Survey {
    /// An empty survey at `now`.
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
            live_copy: None,
            live_unnamed: None,
            store: Vec::new(),
            live_builds: BTreeMap::new(),
            transcripts_root: None,
            purge_available: false,
            home: None,
            notes: Vec::new(),
            room: None,
            mounts: None,
            demand: None,
        }
    }
}

/// Why a row is safe to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Witness {
    StaleBuildDir {
        marker: String,
        profiles: usize,
        idle_days: i64,
        threshold_days: i64,
        leaves: Vec<PathBuf>,
    },
    LeastRecentlyUsed {
        marker: String,
        build_dir: PathBuf,
        last_used: i64,
        age_s: i64,
        threshold_days: i64,
        floor_gib: u64,
        until_gib: u64,
        device: u64,
    },
    OwnerGone {
        marker: String,
        evidence: String,
        owner: discover::Owner,
        last_used: i64,
        age_s: i64,
        quiet_s: i64,
        floor_gib: u64,
        until_gib: u64,
        device: u64,
    },
    IdleWhole {
        marker: String,
        checkout: PathBuf,
        profiles: usize,
        last_used: i64,
        age_s: i64,
        window_days: i64,
        device: u64,
    },
    NotLinkTarget {
        live: PathBuf,
    },
    NotLauncherCopy {
        launcher: PathBuf,
        live: PathBuf,
    },
    Superseded {
        program: String,
        live: String,
    },
    VendorReports {
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
                "{marker} present (a cache by its own mark); {evidence}; no build holding its locks, every profile unwritten for {}, no mount point and nothing of another user's inside — free space is under the {floor_gib} GiB line and nobody builds there again, so the whole build directory goes first (until {until_gib} GiB are free is no stop for it: it costs nobody anything)",
                target::human_age(*age_s)
            ),
            Witness::IdleWhole {
                marker,
                checkout,
                profiles,
                age_s,
                window_days,
                ..
            } => format!(
                "{marker} present; every one of its {profiles} profile{} unwritten for {} (the {window_days}d window), its checkout {} quiet as long (no session or process in it, its git files and working tree unwritten), nothing installed pointing into it — tier 3 empties each profile under its locks and keeps the directory, its lock files and everything at its root that is not a profile (costs a full rebuild)",
                if *profiles == 1 { "" } else { "s" },
                target::human_age(*age_s),
                checkout.display()
            ),
            Witness::NotLinkTarget { live } => {
                format!("not the live symlink target {}", live.display())
            }
            Witness::NotLauncherCopy { launcher, live } => format!(
                "not the live version {}, of which {} is a byte-for-byte copy",
                live.display(),
                launcher.display()
            ),
            Witness::Superseded { program, live } => {
                format!("superseded: {program} live build is {live}")
            }
            Witness::VendorReports { command } => format!("surfaced by `{command}`"),
        }
    }

    /// The witness kind, as the JSON row spells it.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Witness::StaleBuildDir { .. } => "stale-build-dir",
            Witness::LeastRecentlyUsed { .. } => "least-recently-used",
            Witness::OwnerGone { .. } => "owner-gone",
            Witness::IdleWhole { .. } => "idle-whole",
            Witness::NotLinkTarget { .. } => "not-link-target",
            Witness::NotLauncherCopy { .. } => "not-launcher-copy",
            Witness::Superseded { .. } => "superseded",
            Witness::VendorReports { .. } => "vendor-reports",
        }
    }
}

/// THE TIERS of the automatic pass, cheapest to the owner first (design `docs/DESIGN-disk-awareness-2026-10-17.md` §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    EndedOwners = 1,
    IdleIncremental = 2,
    IdleWhole = 3,
    RecentIncremental = 4,
}

impl Tier {
    /// Its number, as a row's `tier=` and the journal spell it.
    #[must_use]
    pub const fn number(self) -> u8 {
        self as u8
    }
}

/// One reported target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The safelist class this row belongs to.
    pub class: Class,
    /// The path, or — for a delegated row with no single path — the tree the delegate owns.
    pub path: PathBuf,
    /// Bytes this row would free, as far as the walk got.
    pub bytes: u64,
    /// `true` when [`Row::bytes`] is a floor rather than a figure.
    pub bytes_partial: bool,
    /// Why it is safe to remove.
    pub witness: Witness,
    /// May [`apply`] remove it? A delegated class is `false`, and so is a row whose own witness was undermined (an unresolvable live symlink).
    pub removable: bool,
    /// Why not, when `removable` is `false`.
    pub blocked: Option<String>,
}

impl Row {
    /// The tier of the automatic pass that would take it ([`Tier`]); `None` for a row of another class.
    #[must_use]
    pub fn tier(&self) -> Option<Tier> {
        if self.class != Class::CargoTargets {
            return None;
        }
        match self.witness {
            Witness::OwnerGone { .. } => Some(Tier::EndedOwners),
            Witness::StaleBuildDir { .. } => Some(Tier::IdleIncremental),
            Witness::IdleWhole { .. } => Some(Tier::IdleWhole),
            Witness::LeastRecentlyUsed { .. } => Some(Tier::RecentIncremental),
            _ => None,
        }
    }

    /// The text-report line, every path in it spelled as a person reads it ([`shown`]).
    #[must_use]
    pub fn line(&self) -> String {
        let size = human_bytes(self.bytes);
        let partial = if self.bytes_partial { "+" } else { "" };
        let state = match &self.blocked {
            Some(why) => format!("blocked: {why}"),
            None => "removable".to_owned(),
        };
        let tier = self
            .tier()
            .map_or_else(String::new, |t| format!(" tier={}", t.number()));
        shown(&format!(
            "{:<22} {size}{partial:<1} {}{tier} — {} [{state}]",
            self.class.as_str(),
            self.path.display(),
            self.witness.describe()
        ))
        .into_owned()
    }
}

/// `text` as a person reads it: on Windows every path in it spelled plain.
#[must_use]
pub fn shown(text: &str) -> std::borrow::Cow<'_, str> {
    const VERBATIM: &str = r"\\?\";
    if !cfg!(windows) || !text.contains(VERBATIM) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(VERBATIM) {
        out.push_str(&rest[..at]);
        let tail = &rest[at + VERBATIM.len()..];
        if let Some(share) = tail.strip_prefix(r"UNC\") {
            out.push_str(r"\\");
            rest = share;
        } else if matches!(tail.as_bytes(), [drive, b':', b'\\', ..] if drive.is_ascii_alphabetic())
        {
            rest = tail;
        } else {
            out.push_str(VERBATIM);
            rest = tail;
        }
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
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
    /// The rows: class order, then the order the pass takes them, then path.
    pub rows: Vec<Row>,
    /// Notes about rows the report did not make (an unresolvable live symlink, a class with no roots).
    pub notes: Vec<String>,
    /// The largest build directories this report cannot reclaim, biggest first, at most [`MAX_HOGS`], each with what frees it.
    pub hogs: Vec<Hog>,
    /// `$HOME`, for the lines (`~/…`).
    pub home: Option<PathBuf>,
    /// The config that produced it.
    pub config: Config,
    /// The volume's room ([`Survey::room`]).
    pub room: Option<u64>,
    /// The demand the pass adopted ([`Survey::demand`]).
    pub demand: Option<u64>,
}

/// One build directory — or an x.py checkout's whole `build/` — holding bytes no row of the report would free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hog {
    /// The directory.
    pub path: PathBuf,
    /// The bytes it holds that no row frees: its size ([`target::total_bytes`]) less what its rows would release.
    pub bytes: u64,
    /// `true` when a size walk stopped at its bound: the figure is a floor.
    pub bytes_partial: bool,
    /// What it is, and what frees it.
    pub why: String,
}

impl Hog {
    /// The line a person reads: `$HOME/trust/build 94.0 GiB (x.py bootstrap; run scripts/slim_build.py)`.
    #[must_use]
    pub fn line(&self, home: Option<&Path>) -> String {
        self.line_at(&display_home(&self.path, home))
    }

    /// [`Self::line`] in at most `chars` characters, for a place that clips at the end.
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

    fn line_at(&self, at: &str) -> String {
        format!(
            "{at} {}{} ({})",
            human_bytes(self.bytes),
            if self.bytes_partial { "+" } else { "" },
            shown(&self.why)
        )
    }
}

const HOG_PATH_MIN: usize = 24;

fn display_home(path: &Path, home: Option<&Path>) -> String {
    fn plain(p: &Path) -> std::borrow::Cow<'_, Path> {
        if cfg!(windows) {
            aterm_sysprobe::paths::plain(p)
        } else {
            std::borrow::Cow::Borrowed(p)
        }
    }
    let path = plain(path);
    let home = home.map(plain);
    match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rel) if !rel.as_os_str().is_empty() => {
            format!("~{}{}", std::path::MAIN_SEPARATOR, rel.display())
        }
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

    /// Bytes every removable row but a pressure row would free: what an apply takes whole.
    #[must_use]
    pub fn reclaimable_total(&self) -> u64 {
        self.rows
            .iter()
            .filter(|r| r.removable && !is_pressure(r) && r.tier() != Some(Tier::IdleWhole))
            .fold(0u64, |a, r| a.saturating_add(r.bytes))
    }

    /// Bytes tier 3 would free if it emptied every whole idle build directory it lists, removable or not (report-only while `disk.idle_build_dirs` is off).
    #[must_use]
    pub fn idle_whole_up_to(&self) -> u64 {
        self.rows
            .iter()
            .filter(|r| r.tier() == Some(Tier::IdleWhole))
            .fold(0u64, |a, r| a.saturating_add(r.bytes))
    }

    /// Bytes the pressure rows would free if the pass took every one.
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
            "OK schema=1 kind=disk root={} free={free} warn_below={} room={} state={} trigger={} rows={} reclaimable={} under_pressure_up_to={} idle_whole_up_to={} apply={}",
            self.root.display(),
            human_bytes(self.warn_free_bytes),
            self.room.map_or_else(|| "unknown".to_owned(), human_bytes),
            if self.warn { "low" } else { "ok" },
            self.trigger.as_str(),
            self.rows.len(),
            human_bytes(self.reclaimable_total()),
            human_bytes(self.reclaimable_under_pressure()),
            human_bytes(self.idle_whole_up_to()),
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
        o.insert(
            "room_bytes".to_owned(),
            self.room.map_or(Value::Null, Value::from),
        );
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
            "idle_whole_up_to_bytes".to_owned(),
            Value::from(self.idle_whole_up_to()),
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
    if let Some(tier) = r.tier() {
        o.insert("tier".to_owned(), Value::from(u64::from(tier.number())));
    }
    if let Witness::LeastRecentlyUsed { last_used, .. }
    | Witness::OwnerGone { last_used, .. }
    | Witness::IdleWhole { last_used, .. } = &r.witness
    {
        o.insert("last_used".to_owned(), Value::from(rfc3339_utc(*last_used)));
    }
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

fn path_text(p: &Path) -> String {
    truncate_bytes(&p.display().to_string(), ROW_TEXT_CAP).to_owned()
}

/// Build the §5.5 report from `survey`.
#[must_use]
pub fn report(survey: &Survey, config: Config) -> Report {
    let warn_free_bytes = config.warn_free_gib.saturating_mul(GIB);
    let mut rows: Vec<Row> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    let d = Delegate::AtermPkgGc;
    for b in survey.store.iter().take(MAX_ROWS_PER_CLASS) {
        let Some(live) = survey.live_builds.get(&b.program) else {
            notes.push(format!(
                "atpkg-gc: {} has no live build recorded, so no build of it is superseded",
                b.program
            ));
            continue;
        };
        if gc_retains(&survey.store, &b.program, live, &b.build) {
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

    match &survey.live_version {
        Some(live) => {
            let witness = match &survey.live_copy {
                Some(launcher) => Witness::NotLauncherCopy {
                    launcher: launcher.clone(),
                    live: live.clone(),
                },
                None => Witness::NotLinkTarget { live: live.clone() },
            };
            for v in survey.versions.iter().take(MAX_ROWS_PER_CLASS) {
                if &v.path == live {
                    continue;
                }
                rows.push(Row {
                    class: Class::ClaudeStaleVersions,
                    path: v.path.clone(),
                    bytes: v.bytes,
                    bytes_partial: v.bytes_partial,
                    witness: witness.clone(),
                    removable: v.kept.is_none(),
                    blocked: v.kept.clone(),
                });
            }
        }
        None if !survey.versions.is_empty() => {
            let why = survey
                .live_unnamed
                .as_deref()
                .unwrap_or("the live symlink did not resolve");
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
                    blocked: Some(format!("{why}, so nothing here has a witness")),
                });
            }
            notes.push(format!(
                "claude-stale-versions: {why} — every row is blocked"
            ));
        }
        None => {}
    }

    let targets = &survey.targets[..survey.targets.len().min(MAX_TARGETS)];
    let deciding: Vec<&Path> = targets
        .iter()
        .filter(|t| t.fences().is_ok())
        .map(|t| t.path.as_path())
        .collect();
    let mut whole: Vec<&Path> = Vec::new();
    let mut next = 0;
    for t in targets {
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
    idle_whole_rows(survey, config, &mut rows, &mut notes);
    notes.extend(survey.notes.iter().cloned());

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
        room: survey.room,
        demand: survey.demand,
    }
}

fn idle_whole_rows(survey: &Survey, config: Config, rows: &mut Vec<Row>, notes: &mut Vec<String>) {
    for t in survey.targets.iter().take(MAX_TARGETS) {
        match &t.idle {
            None => {}
            Some(Err(why)) => notes.push(format!(
                "cargo-targets: {} — tier 3 keeps it whole: {why}",
                t.path.display()
            )),
            Some(Ok(facts)) => {
                let Some(device) = survey.volume_device else {
                    continue;
                };
                let blocked = (!config.idle_whole_on()).then(|| {
                    if config.idle_build_days < 1 {
                        "`[disk] idle_build_days` is below 1, which turns tier 3 off".to_owned()
                    } else if !config.idle_build_dirs {
                        "`[disk] idle_build_dirs` is off: tier 3 is report-only until it is on"
                            .to_owned()
                    } else {
                        "the automatic removal is off".to_owned()
                    }
                });
                rows.push(Row {
                    class: Class::CargoTargets,
                    path: t.path.clone(),
                    bytes: facts.bytes.0,
                    bytes_partial: facts.bytes.1,
                    witness: Witness::IdleWhole {
                        marker: "CACHEDIR.TAG".to_owned(),
                        checkout: facts.checkout.clone(),
                        profiles: facts.profiles,
                        last_used: facts.last_used,
                        age_s: survey.now.saturating_sub(facts.last_used),
                        window_days: config.idle_build_days,
                        device,
                    },
                    removable: blocked.is_none(),
                    blocked,
                });
            }
        }
    }
}

fn gc_retains(store: &[StoreBuild], program: &str, live: &str, build: &str) -> bool {
    if build == live {
        return true;
    }
    let (Ok(live), Ok(build)) = (live.parse::<u64>(), build.parse::<u64>()) else {
        return true;
    };
    let rollback = store
        .iter()
        .filter(|b| b.program == program)
        .filter_map(|b| b.build.parse::<u64>().ok())
        .filter(|b| *b < live)
        .max();
    rollback == Some(build)
}

fn whole_verdict(
    t: &TargetDir,
    survey: &Survey,
    config: Config,
    leaves: &[PathBuf],
) -> Option<Result<Witness, String>> {
    let discover::Owned::Gone { evidence, owner } = &t.owned else {
        return None;
    };
    let low = config.below_warn(survey.free_bytes) || survey.demand.is_some();
    if !config.dead_build_dirs || config.auto_free_gib == 0 || !low {
        return None;
    }
    let short = config.below_auto_floor(survey.free_bytes);
    let device = survey.volume_device?;
    Some((|| {
        let marker = t.fences()?;
        if marker != "CACHEDIR.TAG" {
            return Err(format!(
                "only a signed CACHEDIR.TAG lets a whole build directory go, and it carries {marker}"
            ));
        }
        whole_fences(t, survey.mounts.as_ref())?;
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
        let quiet_s = owner.quiet_s(config.quiet_s(), short);
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
            floor_gib: if short {
                config.auto_free_gib
            } else {
                config.warn_free_gib
            },
            until_gib: config.warn_free_gib,
            device,
        })
    })())
}

fn whole_fences(
    t: &TargetDir,
    mounts: Option<&aterm_sysprobe::volumes::Table>,
) -> Result<(), String> {
    mounts_clear(&t.path, mounts)?;
    if let Some(foreign) = &t.foreign {
        return Err(foreign.clone());
    }
    Ok(())
}

/// No mount point of `table` at or under `dir`, and `table` read whole ([`aterm_sysprobe::volumes::mounts_under`]).
pub(crate) fn mounts_clear(
    dir: &Path,
    table: Option<&aterm_sysprobe::volumes::Table>,
) -> Result<(), String> {
    let Some(table) = table.filter(|t| t.complete) else {
        return Err(
            "the mount table was not read whole, so no mount point inside it is ruled out"
                .to_owned(),
        );
    };
    match aterm_sysprobe::volumes::mounts_under(dir, table).next() {
        Some(m) => Err(format!(
            "a volume is mounted inside it at {} — nothing under a mount point is removed",
            m.on.display()
        )),
        None => Ok(()),
    }
}

fn row_dir(r: &Row) -> Option<&Path> {
    match &r.witness {
        Witness::StaleBuildDir { .. } | Witness::OwnerGone { .. } | Witness::IdleWhole { .. } => {
            Some(&r.path)
        }
        Witness::LeastRecentlyUsed { build_dir, .. } => Some(build_dir),
        _ => None,
    }
}

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

/// The largest build directories a pass left: [`Report::hogs`] given what [`apply_auto`] did (`done`).
#[must_use]
pub fn hogs_left(
    survey: &Survey,
    rep: &Report,
    done: &Applied,
    there: &dyn Fn(&Path) -> bool,
) -> Vec<Hog> {
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
                Witness::StaleBuildDir { .. }
                | Witness::LeastRecentlyUsed { .. }
                | Witness::IdleWhole { .. } => removed.bytes != row.bytes,
                Witness::NotLinkTarget { .. }
                | Witness::NotLauncherCopy { .. }
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

fn hog_of(t: &TargetDir, config: Config, survey: &Survey, notes: &[String]) -> (PathBuf, String) {
    use discover::{Origin, Owned};
    let gone_why = |what: &str| -> String {
        if !config.dead_build_dirs {
            return format!("{what}; `[disk] dead_build_dirs` is off");
        }
        if !config.below_warn(survey.free_bytes) && survey.demand.is_none() {
            return format!(
                "{what}; it goes whole once free space is below the {} GiB warning",
                config.warn_free_gib
            );
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

const fn is_pressure(r: &Row) -> bool {
    matches!(r.witness, Witness::LeastRecentlyUsed { .. })
}

fn lru_rank(r: &Row) -> (u8, i64) {
    let tier = r.tier().map_or(0, Tier::number);
    match r.witness {
        Witness::OwnerGone { last_used, .. }
        | Witness::LeastRecentlyUsed { last_used, .. }
        | Witness::IdleWhole { last_used, .. } => (tier, last_used),
        Witness::StaleBuildDir { idle_days, .. } => (tier, -idle_days),
        _ => (tier, 0),
    }
}

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

/// Is `path` at or under `dir`, where a miss must never be read as "not under" (S1, the one path rule).
pub(crate) fn placed_under(path: &Path, dir: &Path) -> Option<bool> {
    if let Some(placed) = aterm_sysprobe::paths::is_under(path, dir) {
        return Some(placed);
    }
    let want = aterm_sysprobe::volumes::file_id(dir, true)?;
    for at in path.ancestors().filter(|a| !a.as_os_str().is_empty()) {
        if aterm_sysprobe::volumes::file_id(at, true)? == want {
            return Some(true);
        }
    }
    Some(false)
}

fn under_transcripts(path: &Path, root: &Path) -> Option<bool> {
    #[cfg(windows)]
    {
        let keyed = aterm_sysprobe::paths::is_under(path, root);
        if keyed == Some(true) {
            return Some(true);
        }
        let Some(want) = aterm_sysprobe::volumes::file_id(root, true) else {
            return match std::fs::metadata(root) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
                _ => None,
            };
        };
        for at in path.ancestors().filter(|a| !a.as_os_str().is_empty()) {
            if aterm_sysprobe::volumes::file_id(at, true)? == want {
                return Some(true);
            }
        }
        Some(false)
    }
    #[cfg(not(windows))]
    {
        placed_under(path, root)
    }
}

fn same_place(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        let key = aterm_sysprobe::paths::windows_key;
        key(a).is_some_and(|a| key(b).is_some_and(|b| a == b))
    } else {
        a == b
    }
}

fn speaker(dirs: &[&Path], profile: &Path) -> Option<usize> {
    dirs.iter()
        .enumerate()
        .filter(|(_, d)| placed_under(profile, d) != Some(false))
        .max_by_key(|&(i, d)| (d.components().count(), std::cmp::Reverse(i)))
        .map(|(i, _)| i)
}

fn decided_by(t: &TargetDir, at: Option<usize>, deciding: &[&Path]) -> (TargetDir, Vec<PathBuf>) {
    let mut own = t.clone();
    let Some(at) = at else {
        return (own, Vec::new());
    };
    own.profiles
        .retain(|p| speaker(deciding, &p.path) == Some(at));
    let leaves = deciding
        .iter()
        .filter(|d| !same_place(d, &t.path) && placed_under(d, &t.path) != Some(false))
        .map(|d| d.to_path_buf())
        .collect();
    (own, leaves)
}

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

/// Whole days between `then` and `now`.
#[must_use]
pub fn age_days(now: i64, then: i64) -> i64 {
    now.saturating_sub(then).max(0) / DAY_S
}

/// A byte count a person can read, deliberately coarse: the bounded walk has no byte precision.
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

/// Why an apply, or one path inside it, was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    NoClass,
    ApplyOff(Class),
    Delegated(Class, Delegate),
    NotReported(PathBuf),
    Blocked(PathBuf, String),
    Transcript(PathBuf),
    NotAnchored(PathBuf),
    RemoveFailed(PathBuf, String),
    Unjournalled(PathBuf),
}

impl Refusal {
    /// The short reason code the ledger row carries.
    #[must_use]
    pub const fn code(&self) -> &'static str {
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
    pub const fn class(&self) -> Option<Class> {
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

/// One denial row.
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

/// One intent row, written before a row's removal starts.
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

/// One removal row.
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
        "room_bytes".to_owned(),
        rep.room.map_or(Value::Null, Value::from),
    );
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

/// What an apply would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    ReportOnly(Refusal),
    Denied(Refusal),
    Remove(Class, Vec<Row>),
}

/// Decide what `class` would do against `rep`.
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
        .filter(|r| r.class == class && r.removable && r.tier() != Some(Tier::IdleWhole))
        .cloned()
        .collect();
    Plan::Remove(class, rows)
}

/// The second fence: `path` may be removed under `class` only if all of these hold, else a [`Refusal`] for the caller to journal.
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
        && under_transcripts(path, t) != Some(false)
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

/// [`guard`] for the very row `row`, not only its path.
pub fn guard_row(
    rep: &Report,
    survey_transcripts: Option<&Path>,
    class: Class,
    row: &Row,
) -> Result<(), Refusal> {
    guard_path(survey_transcripts, &row.path)?;
    let Some(held) = rep
        .rows
        .iter()
        .find(|r| r.class == class && r.path == row.path && r.witness == row.witness)
    else {
        return Err(Refusal::NotReported(row.path.clone()));
    };
    if !held.removable {
        return Err(Refusal::Blocked(
            row.path.clone(),
            held.blocked.clone().unwrap_or_else(|| "blocked".to_owned()),
        ));
    }
    Ok(())
}

fn guard_path(survey_transcripts: Option<&Path>, path: &Path) -> Result<(), Refusal> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(Refusal::NotAnchored(path.to_path_buf()));
    }
    if let Some(t) = survey_transcripts
        && under_transcripts(path, t) != Some(false)
    {
        return Err(Refusal::Transcript(path.to_path_buf()));
    }
    Ok(())
}

/// What one row's removal did — the remover's answer, never the report's estimate.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Removed {
    /// Bytes it released.
    pub bytes: u64,
    /// What went, each a directory that is gone (a profile's `incremental/`, or the row's own directory).
    pub units: Vec<PathBuf>,
    /// What was left alone, and why (a profile a build holds, a lock that could not be created).
    pub skipped: Vec<(PathBuf, String)>,
    /// Every delete error on the way, one line each, naming what stayed.
    pub trouble: Vec<String>,
}

/// Which of the pressure pass's stops fired ([`Stop`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopBy {
    Measured,
    Counted,
    Unmeasured,
}

impl StopBy {
    /// As the journal row spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            StopBy::Measured => "measured",
            StopBy::Counted => "counted",
            StopBy::Unmeasured => "unmeasured",
        }
    }
}

/// Where and why the pressure pass stopped before its last row ([`StopBy`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    /// Which stop fired.
    pub by: StopBy,
    /// Free bytes as measured before the row it stopped at.
    pub free: Option<u64>,
    /// The free bytes the pass reclaims up to.
    pub target: u64,
    /// What free space was short of `target` when it was measured before the first pressure row (0 when the pass stopped there).
    pub short: u64,
    /// The bytes the pressure rows it took released, as their unlinks counted them.
    pub released: u64,
    /// The rows of its tier it did not take (each, in tier 4, a build cache more recently used than every one it took).
    pub left: usize,
    /// The tier it stopped ([`Tier::number`]); `None` for a row of no tier.
    pub tier: Option<u8>,
}

impl Stop {
    /// The sentence a person reads.
    #[must_use]
    pub fn describe(&self) -> String {
        let what = match self.tier {
            Some(1) => "build director",
            Some(2) => "idle cache",
            Some(3) => "idle build director",
            _ => "more recently used cache",
        };
        let plural = match (self.left == 1, what.ends_with('r')) {
            (true, true) => "y",
            (false, true) => "ies",
            (true, false) => "",
            (false, false) => "s",
        };
        let kept = format!("{} {what}{plural} kept", self.left);
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

/// One step of an apply, told to the caller as it happens ([`apply`], [`apply_auto`]), so a journal written from it records a removal's intent before the removal starts.
#[derive(Debug, Clone, Copy)]
pub enum Step<'a> {
    Removing(&'a Row),
    Removed(&'a Row, &'a Removed),
    Denied(&'a Refusal),
    Stopped(&'a Stop),
}

/// What [`apply`] and [`apply_auto`] tell each [`Step`] to.
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

/// One `reserve` row of the disk ledger.
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

/// Why the watch's sweep took a build directory of aterm's Rust build cache, in words, with the figures it was taken on (`atpkg::build_cache::Unused`).
#[must_use]
pub fn sweep_words(why: atpkg::build_cache::Unused) -> String {
    use atpkg::build_cache::Unused;
    let span = sweep_span;
    match why {
        Unused::Gone { idle, after } => format!(
            "its workspace is gone, and nothing wrote it for {} (the sweep takes one unwritten \
             {}); no build held its locks",
            span(idle),
            span(after)
        ),
        Unused::Idle { idle } => format!(
            "nothing wrote it for {}, a month or more, whatever its workspace's state; no build \
             held its locks",
            span(idle)
        ),
        Unused::Unfinished => {
            "an earlier sweep renamed it aside and did not finish removing it".to_owned()
        }
        Unused::Tooling { idle, after } => format!(
            "a pinned tool of the tooling cache nothing ran for {} (the sweep takes \
             one unused {}); `targo --unverified verify provision` lays it again (`--linux` \
             for the Linux lane's rows, `--record` for wasm-bindgen)",
            span(idle),
            span(after)
        ),
        Unused::GivenUp => "a scratch a failed provisioning run left, past macOS's \
                            first-exec assessment of what ran from it"
            .to_owned(),
        Unused::Stale {
            incremental,
            units,
            after,
        } => format!(
            "what no build reads again, moved aside from a build directory that stays: \
             {incremental} incremental crate folder(s) and {units} unit(s) built by a compiler \
             that is no longer the store's current one, each unwritten for {} or more; no \
             build held its locks",
            span(after)
        ),
    }
}

/// Why the watch's sweep took a build directory, in the few words its line in the watch's account carries (the log, the freed entry, the resume).
#[must_use]
pub fn sweep_reason(why: atpkg::build_cache::Unused) -> String {
    use atpkg::build_cache::Unused;
    match why {
        Unused::Gone { .. } => "its workspace is gone".to_owned(),
        Unused::Idle { idle } => format!("unwritten {}", sweep_span(idle)),
        Unused::Unfinished => "left half-removed by an earlier sweep".to_owned(),
        Unused::Tooling { idle, .. } => format!("a tool unused {}", sweep_span(idle)),
        Unused::GivenUp => "a failed provisioning run's scratch".to_owned(),
        Unused::Stale {
            incremental,
            units,
            after,
        } => format!(
            "{incremental} incremental folder(s) and {units} old-compiler unit(s) unwritten {}",
            sweep_span(after)
        ),
    }
}

fn sweep_span(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s < 3_600 {
        format!("{} min", s / 60)
    } else if s < 2 * 86_400 {
        format!("{}h", s / 3_600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// One `build-cache-sweep` row of the disk ledger: a build directory of aterm's Rust build cache.
#[must_use]
pub fn sweep_row(
    now: i64,
    path: &Path,
    action: &str,
    bytes: u64,
    why: atpkg::build_cache::Unused,
) -> String {
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
    let what = match why {
        atpkg::build_cache::Unused::Stale { .. } => "part of a build cache",
        atpkg::build_cache::Unused::Tooling { .. } => "a pinned tool, whole",
        atpkg::build_cache::Unused::GivenUp => "a tooling scratch, whole",
        _ => "a build cache, whole",
    };
    o.insert(
        "text".to_owned(),
        Value::from(format!("{what}: {}", sweep_words(why))),
    );
    aterm_json::to_string(&Value::Object(o))
        .unwrap_or_else(|_| "{\"kind\":\"build-cache-sweep\"}".to_owned())
}

/// One `poke` row of the disk ledger.
#[must_use]
pub fn poke_row(now: i64, cause: &poke::Cause, free: Option<u64>, level: Level) -> String {
    let mut o = Map::new();
    o.insert("ts".to_owned(), Value::from(rfc3339_utc(now)));
    o.insert(
        "sid".to_owned(),
        Value::from(match cause {
            poke::Cause::Screen { sid, .. } | poke::Cause::Demand { sid, .. } => sid.clone(),
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

/// One stop row: where the pressure pass stopped, on what free figure, and how many rows it left.
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
    if let Some(tier) = stop.tier {
        o.insert("tier".to_owned(), Value::from(u64::from(tier)));
    }
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
    /// The rows that were (at least in part) removed, each with what its removal did.
    pub removed: Vec<(Row, Removed)>,
    /// Every refusal, in the order they happened.
    pub denials: Vec<Refusal>,
    /// Bytes the removals released, as each remover counted them.
    pub freed_bytes: u64,
    /// Where the pass ended, when a stop ended it before its last row: the last of [`Self::stops`].
    pub stopped: Option<Stop>,
    /// Every tier's stop, in the order they fired.
    pub stops: Vec<Stop>,
    /// The pass did not reach every row it may have: why (the search for build directories stopped short).
    pub unfinished: Option<String>,
}

impl Applied {
    /// Did the pass RUN OUT of what it may take.
    #[must_use]
    pub fn ran_out(&self) -> bool {
        self.unfinished.is_none() && self.stops.iter().all(|s| s.by == StopBy::Measured)
    }

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
            if matches!(row.witness, Witness::IdleWhole { .. }) {
                went.idle_whole += 1;
            } else if !matches!(row.witness, Witness::OwnerGone { .. }) {
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

/// What an apply removed, counted ([`Applied::went`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Went {
    /// Gone owners' build directories that went whole.
    pub whole: usize,
    /// Gone owners' build directories that went only in part.
    pub in_part: usize,
    /// Live checkouts' whole idle build directories whose profiles were emptied (tier 3), in whole or in part.
    pub idle_whole: usize,
    /// Every other unit that went.
    pub profiles: usize,
}

/// One row of a pass, with where its tier stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    /// The row.
    pub row: Row,
    /// Its tier's target, or `None`: taken whole.
    pub target: Option<u64>,
}

/// THE TIER PLANNER of the automatic pass (design §4.1).
#[must_use]
pub fn pass_plan(rep: &Report) -> Option<Vec<Planned>> {
    let c = rep.config;
    let free = rep.free_bytes?;
    if c.auto_free_gib == 0 {
        return None;
    }
    let low = c.below_warn(Some(free));
    let short = c.below_auto_floor(Some(free));
    let demand = rep
        .demand
        .map(|need| need.saturating_add(c.auto_free_gib.saturating_mul(GIB)));
    if !low && !short && demand.is_none() {
        return None;
    }
    let idle = |measured: bool| {
        let line = if measured {
            c.idle_target_bytes(short)
        } else {
            0
        };
        line.max(demand.unwrap_or(0))
    };
    Some(
        rep.rows
            .iter()
            .filter(|r| r.class == Class::CargoTargets && r.removable)
            .filter_map(|r| {
                let target = match r.tier()? {
                    Tier::EndedOwners => (low || short || demand.is_some()).then_some(None),
                    Tier::IdleIncremental => {
                        (low || short || demand.is_some()).then(|| Some(idle(low || short)))
                    }
                    Tier::IdleWhole => (short || demand.is_some()).then(|| Some(idle(short))),
                    Tier::RecentIncremental => short.then(|| Some(c.pressure_target_bytes())),
                }?;
                Some(Planned {
                    row: r.clone(),
                    target,
                })
            })
            .collect(),
    )
}

/// The rows [`pass_plan`] would take, without their targets: what the automatic grant reaches.
#[must_use]
pub fn auto_plan(rep: &Report) -> Option<Vec<Row>> {
    pass_plan(rep).map(|rows| rows.into_iter().map(|p| p.row).collect())
}

/// A row's remover: what [`apply`] calls for each row that passed [`guard`].
pub type Remove<'a> = dyn FnMut(&Row) -> std::io::Result<Removed> + 'a;

/// Free space on the measured volume, measured again: what the pass reads before each row that has a target.
pub type Measure<'a> = dyn FnMut() -> Option<u64> + 'a;

/// Carry out [`pass_plan`] against `rep` through `remove`, each path behind [`guard`] as [`apply`]'s are, each [`Step`] told to `record` as it happens, and free space re-read through `measure` before each row with a target.
pub fn apply_auto(
    rep: &Report,
    transcripts: Option<&Path>,
    remove: &mut Remove<'_>,
    measure: &mut Measure<'_>,
    record: &mut Record<'_>,
) -> Option<Applied> {
    let rows = pass_plan(rep)?;
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

/// Carry out `class` against `rep`, removing through `remove` and telling `record` each [`Step`] as it happens.
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
    let Some(class) = class else {
        done.denials.push(Refusal::NoClass);
        return done;
    };
    let target = rep.config.pressure_target_bytes();
    let rows = rows
        .into_iter()
        .map(|row| Planned {
            target: is_pressure(&row).then_some(target),
            row,
        })
        .collect();
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

#[derive(Default)]
struct TierRun {
    short: Option<u64>,
    released: u64,
    stopped: bool,
}

fn remove_rows(
    rep: &Report,
    transcripts: Option<&Path>,
    class: Class,
    rows: Vec<Planned>,
    (remove, measure): (&mut Remove<'_>, &mut Measure<'_>),
    record: &mut Record<'_>,
    done: &mut Applied,
) {
    let tier_of = |p: &Planned| p.row.tier().map(Tier::number);
    let mut runs: BTreeMap<Option<u8>, TierRun> = BTreeMap::new();
    for (i, planned) in rows.iter().enumerate() {
        let tier = tier_of(planned);
        let run = runs.entry(tier).or_default();
        if run.stopped {
            continue;
        }
        if let Some(target) = planned.target {
            let free = measure();
            let by = match free {
                None => Some(StopBy::Unmeasured),
                Some(f) if f >= target => Some(StopBy::Measured),
                Some(f) => {
                    let short = *run.short.get_or_insert(target - f);
                    (run.released >= short).then_some(StopBy::Counted)
                }
            };
            if let Some(by) = by {
                let left = rows[i..]
                    .iter()
                    .filter(|p| tier_of(p) == tier && p.target.is_some())
                    .count();
                let stop = Stop {
                    by,
                    free,
                    target,
                    short: run.short.unwrap_or(0),
                    released: run.released,
                    left,
                    tier,
                };
                record(&Step::Stopped(&stop));
                run.stopped = true;
                done.stopped = Some(stop.clone());
                done.stops.push(stop);
                if by == StopBy::Unmeasured {
                    return;
                }
                continue;
            }
        }
        let row = planned.row.clone();
        if let Err(r) = guard_row(rep, transcripts, class, &row) {
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
                if planned.target.is_some() {
                    let run = runs.entry(tier).or_default();
                    run.released = run.released.saturating_add(removed.bytes);
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

/// The real remover.
pub fn remover(
    judge: Judge,
    sessions: impl Fn() -> idle::Sessions,
) -> impl FnMut(&Row) -> std::io::Result<Removed> {
    remover_among(
        judge,
        discover::scan_processes,
        discover::running_claude_census,
        sessions,
        aterm_sysprobe::volumes::table,
    )
}

/// [`remover`], judging gone owners and sessions against `processes` ([`discover::Processes`]) and `census` ([`discover::Census`]).
pub(crate) fn remover_among(
    judge: Judge,
    processes: impl Fn(&[PathBuf], &[u32]) -> std::io::Result<aterm_uds::holders::PathScan>,
    census: impl Fn(Option<&Path>) -> discover::ClaudeCensus,
    sessions: impl Fn() -> idle::Sessions,
    tables: impl Fn() -> aterm_sysprobe::volumes::Table,
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
            Witness::IdleWhole {
                checkout,
                last_used,
                window_days,
                device,
                ..
            } => {
                let dirs = [checkout.clone(), row.path.clone()];
                let window_s = window_days.max(&1).saturating_mul(DAY_S);
                let no_longer =
                    |why: String| std::io::Error::other(format!("no longer a candidate: {why}"));
                let table = tables();
                mounts_clear(&row.path, Some(&table)).map_err(no_longer)?;
                sessions()
                    .clear(checkout, &row.path, Some(&table))
                    .map_err(no_longer)?;
                idle::git_quiet(checkout, judge.now, window_s, Some(&table)).map_err(no_longer)?;
                idle::tree_quiet(
                    checkout,
                    std::slice::from_ref(&row.path),
                    judge.now,
                    window_s,
                    idle::TREE_WALL,
                )
                .map_err(no_longer)?;
                discover::unused_under(&dirs, &processes).map_err(no_longer)?;
                let locked: std::cell::Cell<Option<aterm_sysprobe::volumes::Table>> =
                    std::cell::Cell::new(None);
                target::reclaim_profiles(
                    &row.path,
                    &Judge {
                        device: Some(*device),
                        ..judge
                    },
                    *last_used,
                    window_s,
                    &mut || {
                        let fresh = tables();
                        let fence = mounts_clear(&row.path, Some(&fresh));
                        locked.set(Some(fresh));
                        fence
                    },
                    &mut || {
                        let Some(table) = locked.take() else {
                            return Err("the mount table was not read under its locks".to_owned());
                        };
                        sessions().clear(checkout, &row.path, Some(&table))?;
                        discover::unused_under(&dirs, &processes)?;
                        idle::git_quiet(checkout, judge.now, window_s, Some(&table))
                    },
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

/// The ledger's name for the resume typed into a session whose screen said the disk was full ([`resume_text`]), once space is back.
pub const RULE_DISK_RESUME: &str = "disk-resume@v1";

/// What is typed into a session whose Claude Code said `ENOSPC` (or `EDQUOT`) while the volume measured full, once a look measures it back.
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

/// Type `text` into tab `sid` of the instance at `sock` as one fenced turn (`upgrade_drive::type_fenced`: under the hand, judged on a fresh read — a box, a draft, a person typing or an owner's hold refuses it), presenting `token` ([`super::upgrade_drive::harness_client`]: a window's harness token reads `by=harness`).
#[must_use]
pub fn type_resume(
    sock: &str,
    token: Option<&super::upgrade_drive::HarnessToken>,
    sid: &str,
    text: &str,
) -> String {
    let mut c = match super::upgrade_drive::harness_client(sock, token) {
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

/// Where to look.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Roots {
    /// The volume the free-space figure is about.
    pub volume: Option<PathBuf>,
    /// The vendor's `versions/` directory.
    pub versions_dir: Option<PathBuf>,
    /// The live launcher: the `claude` symlink into `versions/` (macOS, Linux), or `claude.exe`, the installer's COPY of the live version (Windows) ([`versions::live_of`]).
    pub live_link: Option<PathBuf>,
    /// Claude Code's lock folder (`~/.local/state/claude/locks`): a version with a `<version>.lock` there is held ([`versions::kept`]).
    pub version_locks: Option<PathBuf>,
    /// The digest cache under the harness state directory ([`versions::HASH_FILE`]).
    pub version_hashes: Option<PathBuf>,
    /// The package store's root (`<prefix>/store`), read only.
    pub store_dir: Option<PathBuf>,
    /// Candidate build directories, named by the caller.
    pub targets: Vec<PathBuf>,
    /// Build directories discovery found ([`discover::discover`]), surveyed after the named ones, each with how it was found.
    pub found: Vec<discover::Found>,
    /// What discovery could not finish, carried into the report's notes.
    pub found_notes: Vec<String>,
    /// `$HOME`: how the report's lines abbreviate paths.
    pub home: Option<PathBuf>,
    /// The Claude Code directories whose registries and transcripts say whether a session ended ([`discover::Roots::claude_dirs`]).
    pub claude_dirs: Vec<PathBuf>,
    /// The running Claude Codes ([`discover::Roots::claudes`]).
    pub claudes: Vec<discover::RunningClaude>,
    /// Why no session is called ended: a process that may be a Claude Code could not be read ([`discover::Roots::claude_unread`]).
    pub claude_unread: Option<String>,
    /// Where transcripts live.
    pub transcripts: Option<PathBuf>,
    /// The mount table, read for the whole-removal fences ([`Survey::mounts`]).
    pub mounts: Option<aterm_sysprobe::volumes::Table>,
    /// What places a workspace nested in a removed checkout ([`discover::placing_now`], [`discover::Owners::placed`]).
    pub placing: Option<discover::checkout::Placing>,
    /// The demand the pass adopted ([`Survey::demand`]).
    pub demand: Option<u64>,
    /// Assess the found build directories for TIER 3 ([`idle::assess`]).
    pub idle_whole: bool,
    /// The working directory of every session of this instance (tier 3's clause 4a).
    pub session_cwds: Vec<PathBuf>,
    /// Why the working directory of some session of this instance is not known (on Windows a live tab that reported no absolute native one).
    pub session_cwd_unknown: Option<String>,
    /// The folders whose links clause 5 reads (`~/.local/bin`, `~/.cargo/bin`, `$ATPKG_BIN`, `~/.rustup/toolchains`).
    pub link_dirs: Vec<PathBuf>,
}

/// Measure one machine into a [`Survey`]: the one function here that touches a filesystem.
#[must_use]
pub fn scan(
    roots: &Roots,
    now: i64,
    trigger: Trigger,
    config: Config,
    free_bytes: Option<u64>,
) -> Survey {
    scan_with(
        roots,
        now,
        trigger,
        config,
        free_bytes,
        &discover::scan_processes,
    )
}

/// [`scan`], tier 3's clause 4b read through `processes` ([`discover::Processes`]: the product's is every process of this uid, a test's the ones it names).
#[must_use]
pub fn scan_with(
    roots: &Roots,
    now: i64,
    trigger: Trigger,
    config: Config,
    free_bytes: Option<u64>,
    processes: discover::Processes<'_>,
) -> Survey {
    let mut s = Survey::new(now);
    s.trigger = trigger;
    s.free_bytes = free_bytes;
    s.transcripts_root = roots.transcripts.clone();
    s.mounts = roots.mounts.clone();
    s.demand = roots.demand;
    if let Some(v) = &roots.volume {
        s.root = v.clone();
        s.volume_device = target::dev_of(v);
    }
    let pressure = config.below_warn(free_bytes) || roots.demand.is_some();
    let read = versions::read(roots, processes);
    s.versions = read.entries;
    s.live_version = read.live;
    s.live_copy = read.copy_of;
    s.live_unnamed = read.unnamed;
    if let Some(store) = &roots.store_dir {
        scan_store(store, &mut s);
    }
    s.home = roots.home.clone();
    s.notes = roots.found_notes.clone();
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
    )
    .placed(roots.placing.clone());
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
            let (total, foreign) = target::total_bytes_owned(&td.path);
            td.total = Some(total);
            let whole = matches!(td.owned, discover::Owned::Gone { .. })
                || (roots.idle_whole && idle::checkout_of(&td.origin, &td.path).is_some());
            td.foreign = foreign.or_else(|| {
                (whole && total.1)
                    .then(|| target::foreign_beyond(&td.path))
                    .flatten()
            });
            s.targets.push(td);
            beat::beat();
        }
    }
    if roots.idle_whole {
        assess_idle_whole(&mut s, roots, config, now, processes);
    }
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

fn assess_idle_whole(
    s: &mut Survey,
    roots: &Roots,
    config: Config,
    now: i64,
    processes: discover::Processes<'_>,
) {
    let mounts = s.mounts.clone();
    let links = idle::inbound_links(&roots.link_dirs, mounts.as_ref());
    let found: Vec<PathBuf> = s.targets.iter().map(|t| t.path.clone()).collect();
    let cwds: Vec<idle::SessionCwd> = roots
        .session_cwds
        .iter()
        .map(|c| idle::SessionCwd::on(c, mounts.as_ref()))
        .collect();
    let cx = idle::Context {
        now,
        window_s: config.idle_build_days.max(1).saturating_mul(DAY_S),
        device: s.volume_device,
        session_cwds: &cwds,
        cwd_unknown: roots.session_cwd_unknown.as_deref(),
        links: &links,
        found: &found,
        home: roots.home.as_deref(),
        mounts: mounts.as_ref(),
    };
    let fences = |t: &TargetDir| whole_fences(t, mounts.as_ref());
    let mut candidates: Vec<(usize, PathBuf, PathBuf)> = Vec::new();
    for (i, t) in s.targets.iter_mut().enumerate() {
        if idle::checkout_of(&t.origin, &t.path).is_none() {
            continue;
        }
        let verdict = idle::assess(t, &cx, &fences);
        if let Ok(facts) = &verdict {
            candidates.push((i, facts.checkout.clone(), t.path.clone()));
        }
        t.idle = Some(verdict);
    }
    let pairs: Vec<(PathBuf, PathBuf)> = candidates
        .iter()
        .map(|(_, c, d)| (c.clone(), d.clone()))
        .collect();
    for ((i, ..), clear) in
        candidates
            .iter()
            .zip(idle::processes_clear(&pairs, processes, mounts.as_ref()))
    {
        if let (Err(why), Some(t)) = (clear, s.targets.get_mut(*i)) {
            t.idle = Some(Err(why));
        }
    }
}

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

fn version_dir_of(resolved: &Path, versions_dir: Option<&Path>) -> Option<PathBuf> {
    let root = versions_dir?;
    resolved
        .ancestors()
        .find(|p| p.parent() == Some(root))
        .map(Path::to_path_buf)
}

/// One build directory's facts, or `None` when the path is not a directory.
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

fn mtime_of(p: &Path) -> Option<i64> {
    let m = std::fs::symlink_metadata(p).ok()?.modified().ok()?;
    match m.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).ok(),
        Err(_) => Some(0),
    }
}

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
#[must_use]
pub fn dir_bytes(dir: &Path) -> (u64, bool) {
    walk_bytes(dir, |_| true, std::fs::Metadata::len)
}

fn walk_bytes(
    dir: &Path,
    enter: impl Fn(&std::fs::Metadata) -> bool,
    size: impl Fn(&std::fs::Metadata) -> u64,
) -> (u64, bool) {
    walk_bytes_until(dir, enter, size, None)
}

fn walk_bytes_until(
    dir: &Path,
    enter: impl Fn(&std::fs::Metadata) -> bool,
    size: impl Fn(&std::fs::Metadata) -> u64,
    deadline: Option<std::time::Instant>,
) -> (u64, bool) {
    let mut total: u64 = 0;
    let mut seen = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    let mut unread = false;
    while let Some(d) = stack.pop() {
        if deadline.is_some_and(|at| std::time::Instant::now() >= at) {
            return (total, true);
        }
        let rd = match std::fs::read_dir(&d) {
            Ok(rd) => rd,
            Err(e) => {
                unread |= e.kind() != std::io::ErrorKind::NotFound;
                continue;
            }
        };
        for entry in rd.flatten() {
            seen += 1;
            beat::tick(1);
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
    (total, unread)
}

#[cfg(test)]
thread_local! {
    static VOLUME_ERRORS_QUIET: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// The volume holding `path`, read now through the mount table `table` ([`aterm_sysprobe::volumes`]).
pub fn volume_of(
    path: &Path,
    table: &aterm_sysprobe::volumes::Table,
) -> Result<aterm_sysprobe::volumes::Reading, aterm_sysprobe::volumes::Unread> {
    use aterm_sysprobe::volumes::{self, Unread};
    let path = table.long_spelling(path);
    let spelled = volumes::mount_of(&path, table).ok_or(Unread::NoMount)?;
    if spelled.automounted || !spelled.local {
        return volumes::read(spelled);
    }
    let _quiet = aterm_sysprobe::quiet_errors();
    #[cfg(test)]
    VOLUME_ERRORS_QUIET.set(Some(aterm_sysprobe::errors_quiet()));
    let placed = path
        .ancestors()
        .find_map(|a| std::fs::canonicalize(a).ok())
        .ok_or(Unread::NoMount)?;
    volumes::read_path(&placed, table)
}

/// Free bytes on the volume holding `path` ([`volume_of`] through a fresh mount table), or `None` on any failure.
#[must_use]
pub fn free_bytes(path: &Path) -> Option<u64> {
    volume_of(path, &aterm_sysprobe::volumes::table())
        .ok()
        .map(|r| r.free)
}

/// Remove one tree: [`remover`]'s primitive for [`Class::ClaudeStaleVersions`].
pub fn remove_tree(p: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        target::remove_entry(p).map(|_| ())
    }
    #[cfg(not(windows))]
    {
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
}

#[path = "disk_tests.rs"]
#[cfg(test)]
pub(crate) mod tests;

#[path = "disk_pressure_tests.rs"]
#[cfg(all(test, unix))]
pub(crate) mod pressure_tests;
