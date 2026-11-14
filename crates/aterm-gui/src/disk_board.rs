// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared disk-watch state board (`status disk=` and `await sys`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::harness::disk::budget::Sizing;
use aterm_agent::harness::disk::{Cap, Class, Hog, Level, Thresholds};
use aterm_sysprobe::volumes::{Reading, Table, VolumeKey};
use aterm_vitals::sys::{BudgetFacts, BudgetRead};

/// Maximum number of deduplicated volumes retained on the board.
pub(crate) const MAX_VOLUMES: usize = 8;

/// Maximum number of Windows 8.3 short-to-long working directory mappings cached.
pub(crate) const MAX_SPELLED: usize = 256;

/// Disk-watch progress state for `await sys` hang detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Watch {
    /// No disk look has started yet.
    NotStarted,
    /// The watch runs, with its last progress at `beat`; a look may or may not be in flight
    /// ([`Board::look_in_flight`]).
    Running { beat: Instant },
    /// Host is suspended: no beat moves and nothing reads one, whatever look still runs.
    Suspended,
}

/// Last measured state of a single volume.
#[derive(Debug, Clone)]
pub(crate) struct VolumeRow {
    /// Raw volume space and inode reading.
    pub(crate) reading: Reading,
    /// Active warning and floor thresholds for the volume.
    pub(crate) thresholds: Thresholds,
    /// Evaluated disk pressure [`Level`].
    pub(crate) level: Level,
    /// Reclaimable bytes on the volume (`None` when unmeasured).
    pub(crate) room: Option<u64>,
    /// Monotonic timestamp of the reading.
    pub(crate) as_of: Instant,
    /// Unix timestamp (seconds) of the reading.
    pub(crate) as_of_unix: i64,
    /// Whether automatic reclamation runs on this volume (home volume only).
    pub(crate) reclaimed_here: bool,
}

/// Summary of the most recent reclamation pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PassSummary {
    /// Volume the pass ran on.
    pub(crate) key: VolumeKey,
    /// Monotonic completion timestamp.
    pub(crate) ended_at: Instant,
    /// Free bytes measured after the pass.
    pub(crate) free_after: Option<u64>,
    /// Reclamation tier numbers executed.
    pub(crate) tiers: Vec<u8>,
    /// Total bytes freed by the pass.
    pub(crate) freed: u64,
    /// Demand target (in bytes free) adopted by the pass.
    pub(crate) demand: Option<u64>,
    /// Whether the pass exhausted all admissible reclamation candidates.
    pub(crate) ran_out: bool,
    /// Whether the pass exhausted candidates while free space remained below `demand`.
    pub(crate) spent: bool,
    /// Whether `disk.idle_build_dirs = false` held back tier-3 reclamation.
    pub(crate) held_back: bool,
    /// Count and largest size of unreclaimed build directories.
    pub(crate) hogs: usize,
    pub(crate) largest: u64,
    /// Unreclaimed build-directory hogs reported by the pass.
    pub(crate) left: Vec<Hog>,
    /// Unix completion timestamp (`None` if wall clock was unavailable).
    pub(crate) ended_at_unix: Option<i64>,
}

/// Which volume this process's disk watch sizes the rebuildable budget on ([`Board::budget_look`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BudgetHome {
    /// No look has said yet.
    Unlooked,
    /// None: this process is not the account's own watch, or its home volume was not read.
    Unsized,
    /// The home volume, by its spelled key.
    Volume(String),
}

/// Whether a reclamation pass for `demand` exhausted all candidates without meeting `demand`.
pub(crate) fn spent(demand: Option<u64>, free_after: Option<u64>, ran_out: bool) -> bool {
    ran_out && demand.is_some_and(|need| free_after.is_some_and(|f| f < need))
}

/// Pending `await sys` free-space demand registered on the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Demand {
    /// Demand identifier ([`Board::demand`]).
    pub(crate) id: u64,
    /// Requesting session ID.
    pub(crate) session: u64,
    /// Target volume key.
    pub(crate) key: VolumeKey,
    /// Required free bytes.
    pub(crate) need: u64,
    /// Requesting session's working directory.
    pub(crate) cwd: Option<PathBuf>,
    /// Monotonic timestamp when the demand was registered.
    pub(crate) armed_at: Instant,
}

/// Episode state tracking sessions stopped by a full disk.
#[derive(Debug)]
pub(crate) struct DiskEpisode {
    /// Home volume level at the last look.
    pub(crate) level: Option<Level>,
    /// Sessions stopped by a full disk (`sid -> error_code`).
    pub(crate) stuck: BTreeMap<String, String>,
    /// Sessions owed a resume prompt once space recovers (`sid -> (prompt, tries_left)`).
    pub(crate) owed: BTreeMap<String, (String, u8)>,
    /// Total and largest bytes freed during this episode.
    pub(crate) freed: u64,
    pub(crate) went: Vec<String>,
    /// Sessions whose full-disk stop has already been logged.
    pub(crate) said: BTreeSet<String>,
    /// Whether the standing owner alert is active.
    pub(crate) alerted: bool,
}

/// Monotonic and wall-clock snapshot taken outside the board lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Clock {
    /// Monotonic instant.
    pub(crate) at: Instant,
    /// Unix timestamp in seconds (`None` before epoch).
    pub(crate) unix: Option<i64>,
    /// Local UTC offset in seconds (`None` if unavailable).
    pub(crate) offset: Option<i64>,
}

impl Clock {
    /// Sample monotonic, Unix, and local-offset clocks now.
    pub(crate) fn read() -> Self {
        let unix = unix_now();
        Self {
            at: Instant::now(),
            unix,
            offset: unix.and_then(crate::presence::local_offset_at),
        }
    }

    /// Local calendar day count since `1970-01-01`.
    pub(crate) fn day(&self) -> Option<u32> {
        let local = self.unix?.saturating_add(self.offset?);
        u32::try_from(local.div_euclid(86_400)).ok()
    }
}

/// Current Unix timestamp in seconds (`None` before epoch).
pub(crate) fn unix_now() -> Option<i64> {
    aterm_time::unix_secs().map(|s| i64::try_from(s).unwrap_or(i64::MAX))
}

/// Per-day tally of bytes freed by `aterm`'s cleaners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FreedTally {
    /// Local calendar day ([`Clock::day`]) for this tally.
    pub(crate) day: u32,
    /// Total bytes freed today.
    pub(crate) aterm_bytes: u64,
    /// Unix timestamp (seconds) of the most recent removal.
    pub(crate) last_at: i64,
    /// Short label describing the most recent removal ([`freed_what`]).
    pub(crate) last_what: String,
}

impl FreedTally {
    const fn new() -> Self {
        Self {
            day: 0,
            aterm_bytes: 0,
            last_at: 0,
            last_what: String::new(),
        }
    }

    /// Add a removal of `bytes` (`what`) at `now`, resetting first if the local day advanced.
    pub(crate) fn add(&mut self, bytes: u64, what: &str, now: &Clock) {
        let (Some(day), Some(unix)) = (now.day(), now.unix) else {
            return;
        };
        if self.day != day {
            *self = Self { day, ..Self::new() };
        }
        if bytes == 0 {
            return;
        }
        self.aterm_bytes = self.aterm_bytes.saturating_add(bytes);
        what.clone_into(&mut self.last_what);
        self.last_at = unix;
    }

    /// Return `Some(self)` if this tally belongs to `now`'s local day and is non-zero.
    pub(crate) fn today(&self, now: &Clock) -> Option<&Self> {
        (now.day() == Some(self.day) && self.aterm_bytes > 0).then_some(self)
    }
}

/// Short human-readable label for a supervised pass removal of `class`.
pub(crate) const fn freed_what(class: Class) -> &'static str {
    match class {
        Class::CargoTargets => BUILD_CACHES,
        Class::ClaudeStaleVersions => "old Claude Code versions",
        Class::AtpkgGc => "old package builds",
        Class::ClaudePurge => "Claude Code project data",
    }
}

/// Label for build-cache removals.
pub(crate) const BUILD_CACHES: &str = "build caches";

/// Raw home-volume disk facts read by the Stats Deck ([`Board::deck_facts`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DiskFactsRaw {
    /// Home volume pressure level at the last look.
    pub(crate) level: Option<Level>,
    /// Warning, floor, and target free-byte thresholds on the home volume.
    pub(crate) warn: Option<u64>,
    pub(crate) floor: Option<u64>,
    pub(crate) emergency: Option<u64>,
    /// Whether an emergency is caused by inode exhaustion rather than free bytes.
    pub(crate) files_emergency: bool,
    /// Number of sessions stopped by a full disk.
    pub(crate) stuck: u16,
    /// Other measured volumes as `(name, level, free_bytes)`.
    pub(crate) others: Vec<(String, Level, u64)>,
    /// Unix timestamp of the last watch beat when the watch has stalled.
    pub(crate) stalled_since: Option<i64>,
    /// Today's freed-space tally, if non-zero.
    pub(crate) freed_today: Option<FreedTally>,
    /// Unreclaimed build-directory hogs from the last pass and its completion timestamp.
    pub(crate) hogs: Vec<Hog>,
    pub(crate) hogs_at: Option<i64>,
    /// Whether the disk watch is active.
    pub(crate) watch: bool,
}

fn volume_name(mount: &Path) -> String {
    mount.file_name().map_or_else(
        || mount.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Shared disk-watch board state ([`Shelf::board_lock`]).
#[derive(Debug)]
pub(crate) struct Board {
    /// Current watch progress state.
    pub(crate) watch: Watch,
    /// A look is running ([`Shelf::look_began`], [`Shelf::look_ended`]), so an old beat says it is
    /// hung. Between looks an old beat says only that none has started.
    pub(crate) look_in_flight: bool,
    /// Whether a disk watch runs in this process.
    pub(crate) watched: bool,
    /// Mount table from the last look.
    pub(crate) table: Table,
    /// Measured volumes (up to [`MAX_VOLUMES`], home volume first).
    pub(crate) volumes: Vec<VolumeRow>,
    /// Per-session volume index into [`Self::volumes`].
    pub(crate) sessions: BTreeMap<u64, usize>,
    /// Cached Windows 8.3 `(short, long)` working-directory spellings per session.
    pub(crate) spelled: BTreeMap<u64, (PathBuf, PathBuf)>,
    /// Summary of the last reclamation pass.
    pub(crate) last_pass: Option<PassSummary>,
    /// The rebuildable budget's last sizing (`None`: none in this process or in the record it
    /// read, `aterm_agent::harness::disk::budget::Record`). A reading for `sys disk` alone: no
    /// level, wait or alert reads it.
    pub(crate) budget: Option<Sizing>,
    /// `disk.cap_used_gib` as the last look read it ([`Self::budget_look`]).
    pub(crate) budget_cap: Cap,
    /// The volume the budget is sized on, as the last look said ([`Self::budget_look`]).
    pub(crate) budget_home: BudgetHome,
    /// The checkouts' `target*` links whose target a pass's search found not there, kept while
    /// each look finds them still dangling ([`Shelf::keep_dangling`]; the leftover view's `link`
    /// rows).
    pub(crate) dangling: Vec<aterm_agent::harness::disk::discover::Dangling>,
    /// Active unmet free-space demands.
    pub(crate) demands: Vec<Demand>,
    /// Full-disk episode state.
    pub(crate) episode: DiskEpisode,
    /// Today's freed-bytes tally.
    pub(crate) freed: FreedTally,
    /// The `disk-free@v1` line owed a session of this process (`sid -> line`), as the lease
    /// last read elected it (`crate::harness_host`'s ask).
    pub(crate) asks: BTreeMap<String, AskOwed>,
    /// Why a session of this process may not be asked to free space (`sid -> why`: its task
    /// finished or landed, it has none, its Autopilot is Off, or it is the person's own exchange
    /// or a manager's worker), as its worker found it at its last turn break: the ask seats none
    /// of these while the finding holds ([`Unfit::bars`], `crate::harness_disk_ask`).
    pub(crate) unfit: BTreeMap<String, Unfit>,
    /// The usage-limit wall a session of this process stands at (`sid -> wall`), as its loop
    /// opened its limit episode there and dated it, and its worker published it
    /// (`crate::harness_host`'s `WorkerIdle::limit_wall`, again whenever the loop moves its
    /// reset) until the episode closed: while the wall holds ([`Wall::holds`])
    /// and the session's agent reads `limited`, the ask holds it out of its seats and waits for
    /// it to be past rather than running out (`crate::harness_disk_ask`).
    pub(crate) walls: BTreeMap<String, Wall>,
    /// The lasting withhold of its loop's a session of this process stands at (`sid -> withhold`),
    /// as its loop decided its last turn break and its worker published it
    /// (`crate::harness_host`'s `WorkerIdle::lasting_hold`, again whenever it changes): the
    /// credits hold until it lifts, or a wall that is no usage limit until the loop finds it gone
    /// or reads the session working past it (a live read, never a break of its own background
    /// work). While it holds ([`Withhold::holds`]) the ask holds the session
    /// out of its seats and waits for it to be past rather than electing a session whose loop
    /// would type it nothing (`crate::harness_disk_ask`).
    pub(crate) withholds: BTreeMap<String, Withhold>,
    /// The next demand's id.
    next_demand: u64,
}

/// A lasting withhold of a session's loop at its turn breaks ([`Board::withholds`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Withhold {
    /// The credits hold (`credits-hold@v1`): its loop types nothing until `until_s` (unix seconds:
    /// the usage limit's reset plus the loop's grace, or the hold's bound); `None`, until its loop
    /// finds it lifted.
    Credits { until_s: Option<i64> },
    /// A wall on screen that is no usage limit, by its kind's wire word (`wall:auth`, ...).
    Wall { wire: &'static str },
}

impl Withhold {
    /// Whether it holds the session from a line at `now_s` (unix seconds).
    pub(crate) fn holds(&self, now_s: i64) -> bool {
        match self {
            Withhold::Credits { until_s } => until_s.is_none_or(|until| now_s < until),
            Withhold::Wall { .. } => true,
        }
    }
}

/// A usage-limit wall a session's loop has open ([`Board::walls`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Wall {
    /// When the loop takes the wall as spent and rests there, able to take a line (the reset its
    /// limit episode holds the wall to, or where it knows none the one its transcript names, plus
    /// the loop's grace, unix seconds); `None`: its reset is not known, so it holds until the loop
    /// closes its episode.
    pub(crate) until_s: Option<i64>,
}

impl Wall {
    /// Whether the wall holds the session from a line at `now_s` (unix seconds).
    pub(crate) fn holds(&self, now_s: i64) -> bool {
        self.until_s.is_none_or(|until| now_s < until)
    }
}

/// Why a session's worker found it may not be asked to free space at its last turn break
/// ([`Board::unfit`]): what holds while the session rests there, and what holds across its turns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Unfit {
    /// What the session itself was there: its task finished or landed (`finished`), nobody gave
    /// it one (`taskless`), or its Autopilot is Off (`autopilot-off`). This holds only until the
    /// session works again, as one that works was given something to do: its worker drops this at
    /// the turn's first busy read, and the seat rule reads none of it while the session's agent
    /// reads busy.
    pub(crate) own: Option<&'static str>,
    /// Whose the session is for as long as they keep it: the person's own exchange (`exchange`)
    /// or a manager's wait (`managed`). This holds across the session's turns until its next turn
    /// break finds it again.
    pub(crate) theirs: Option<&'static str>,
}

impl Unfit {
    /// The word that bars the session from a seat, its agent working now (`working`: it reads
    /// busy) or not: whose it is alone while it works ([`Self::own`]). `None`: it may be seated.
    pub(crate) fn bars(&self, working: bool) -> Option<&'static str> {
        if working {
            self.theirs
        } else {
            self.own.or(self.theirs)
        }
    }
}

/// A `disk-free@v1` line owed one session ([`Board::asks`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AskOwed {
    /// The volume's lease key ([`VolumeKey::spelled`]).
    pub(crate) key: String,
    /// The line.
    pub(crate) text: String,
}

impl Board {
    /// The watch made progress. A suspended watch stays suspended.
    fn beat(&mut self) {
        if self.watch != Watch::Suspended {
            self.watch = Watch::Running {
                beat: Instant::now(),
            };
        }
    }

    const fn new() -> Self {
        Self {
            watch: Watch::NotStarted,
            look_in_flight: false,
            watched: false,
            table: Table {
                mounts: Vec::new(),
                complete: false,
                firmlinks: Vec::new(),
            },
            volumes: Vec::new(),
            sessions: BTreeMap::new(),
            spelled: BTreeMap::new(),
            last_pass: None,
            budget: None,
            budget_cap: Cap::Unwritten,
            budget_home: BudgetHome::Unlooked,
            dangling: Vec::new(),
            demands: Vec::new(),
            episode: DiskEpisode {
                level: None,
                stuck: BTreeMap::new(),
                owed: BTreeMap::new(),
                freed: 0,
                went: Vec::new(),
                said: BTreeSet::new(),
                alerted: false,
            },
            freed: FreedTally::new(),
            asks: BTreeMap::new(),
            unfit: BTreeMap::new(),
            walls: BTreeMap::new(),
            withholds: BTreeMap::new(),
            next_demand: 1,
        }
    }

    /// Snapshot [`DiskFactsRaw`] for the home volume `home` at `now`.
    pub(crate) fn deck_facts(&self, home: &VolumeKey, now: &Clock) -> DiskFactsRaw {
        if !self.watched {
            return DiskFactsRaw::default();
        }
        let row = self.row_keyed(home).filter(|r| r.level != Level::Unknown);
        let lines = row.map(|r| r.thresholds);
        let stalled_since = match self.watch {
            Watch::Running { beat } => {
                let quiet = now.at.saturating_duration_since(beat);
                let quiet_s = i64::try_from(quiet.as_secs()).unwrap_or(i64::MAX);
                now.unix
                    .filter(|_| quiet >= crate::control::control_disk::STALL)
                    .map(|unix| unix.saturating_sub(quiet_s))
            }
            Watch::NotStarted | Watch::Suspended => None,
        };
        let (hogs, hogs_at) = match &self.last_pass {
            Some(p) if &p.key == home && !p.left.is_empty() => (p.left.clone(), p.ended_at_unix),
            _ => (Vec::new(), None),
        };
        DiskFactsRaw {
            level: row.map(|r| r.level),
            warn: lines.map(|t| t.warn),
            floor: row
                .filter(|r| r.reclaimed_here)
                .map(|r| r.thresholds.floor)
                .filter(|f| *f > 0),
            emergency: lines.map(|t| t.emergency),
            files_emergency: row.is_some_and(|r| {
                r.level == Level::Emergency && r.reading.free >= r.thresholds.emergency
            }),
            stuck: u16::try_from(self.episode.stuck.len()).unwrap_or(u16::MAX),
            others: self
                .volumes
                .iter()
                .filter(|r| &r.reading.key != home && r.level != Level::Unknown)
                .map(|r| (volume_name(&r.reading.mount), r.level, r.reading.free))
                .collect(),
            stalled_since,
            freed_today: self.freed.today(now).cloned(),
            hogs,
            hogs_at,
            watch: true,
        }
    }

    /// Look up the [`VolumeRow`] for `session` (or resolve `cwd` against `table`).
    pub(crate) fn row_of(&self, session: u64, cwd: Option<&str>) -> Option<&VolumeRow> {
        if let Some(row) = self
            .sessions
            .get(&session)
            .and_then(|i| self.volumes.get(*i))
        {
            return Some(row);
        }
        let cwd = Path::new(cwd?);
        let at = match self.spelled.get(&session) {
            Some((short, long)) if short == cwd => long.as_path(),
            _ => cwd,
        };
        self.row_at(at)
    }

    /// Cache `long` as the expanded path for `session`'s 8.3 path `short`.
    pub(crate) fn spell(&mut self, session: u64, short: PathBuf, long: PathBuf) {
        self.spelled.insert(session, (short, long));
        while self.spelled.len() > MAX_SPELLED {
            self.spelled.pop_first();
        }
    }

    /// Look up the [`VolumeRow`] containing `path`.
    pub(crate) fn row_at(&self, path: &Path) -> Option<&VolumeRow> {
        let mount = aterm_sysprobe::volumes::mount_of(path, &self.table)?;
        self.volumes
            .iter()
            .find(|r| r.reading.mount == mount.on)
            .or_else(|| {
                let key = mount.key()?;
                self.volumes.iter().find(|r| r.reading.key == key)
            })
    }

    /// Look up the [`VolumeRow`] for `key`.
    pub(crate) fn row_keyed(&self, key: &VolumeKey) -> Option<&VolumeRow> {
        self.volumes.iter().find(|r| &r.reading.key == key)
    }

    /// What a look of the disk watch read for the budget: `disk.cap_used_gib` (`cap`), and the
    /// volume this process sizes it on (`home`, `None` where it sizes none). The last sizing's
    /// figures give their budget under the cap each look reads, so an edit shows with no walk.
    pub(crate) fn budget_look(&mut self, cap: Cap, home: Option<String>) {
        self.budget_cap = cap;
        self.budget_home = home.map_or(BudgetHome::Unsized, BudgetHome::Volume);
    }

    /// Keep `sizing` as the budget's last one, unless the board holds one that ended later
    /// (a record another process kept, read after this process's own sizing).
    pub(crate) fn budget_sized(&mut self, sizing: Sizing) {
        if self
            .budget
            .as_ref()
            .is_none_or(|kept| kept.sized_unix <= sizing.sized_unix)
        {
            self.budget = Some(sizing);
        }
    }

    /// What `sys disk` says of the budget on the volume `key` (`None`: one the watch has no
    /// row for) at `now_unix`: warming before a watching process's first look; not sampled
    /// where this process's watch does not size that volume; off where `disk.cap_used_gib`
    /// turns it off; the last sizing of that volume under the cap last read (its age `0` where
    /// the clock is unread or reads before it); else warming.
    pub(crate) fn budget_read(&self, key: Option<&VolumeKey>, now_unix: Option<i64>) -> BudgetRead {
        let home = match &self.budget_home {
            BudgetHome::Unlooked if self.watched => return BudgetRead::Warming,
            BudgetHome::Unlooked | BudgetHome::Unsized => return BudgetRead::NotSampled,
            BudgetHome::Volume(home) => home,
        };
        if key.is_none_or(|key| *home != key.spelled()) {
            return BudgetRead::NotSampled;
        }
        if self.budget_cap == Cap::Off {
            return BudgetRead::Off;
        }
        let Some(sizing) = self.budget.as_ref().filter(|s| s.volume == *home) else {
            return BudgetRead::Warming;
        };
        let Some(budget) = self
            .budget_cap
            .budget(sizing.total, sizing.used, sizing.sized.bytes)
        else {
            return BudgetRead::Off;
        };
        BudgetRead::Sized(BudgetFacts {
            cap: budget.cap,
            cap_written: budget.cap_written,
            kept: budget.kept,
            target: budget.target,
            rebuildable: budget.rebuildable,
            partial: sizing.sized.partial,
            sized_s: now_unix
                .and_then(|now| u64::try_from(now.saturating_sub(sizing.sized_unix)).ok())
                .unwrap_or(0),
        })
    }

    /// Update `row`'s reading from a waiter probe while preserving the watch's pressure judgment.
    pub(crate) fn refresh(&mut self, row: VolumeRow) {
        let row = match self.row_keyed(&row.reading.key) {
            Some(held) => VolumeRow {
                level: held
                    .thresholds
                    .level(Some(row.reading.free), row.reading.files_free),
                thresholds: held.thresholds,
                room: held.room,
                reclaimed_here: held.reclaimed_here,
                ..row
            },
            None => row,
        };
        self.measured(row);
    }

    /// Record an authoritative volume reading and judgment from the disk watch.
    pub(crate) fn measured(&mut self, row: VolumeRow) {
        match self
            .volumes
            .iter()
            .position(|r| r.reading.key == row.reading.key)
        {
            Some(i) => self.volumes[i] = row,
            None if self.volumes.len() < MAX_VOLUMES => self.volumes.push(row),
            None => {}
        }
    }

    /// Register a free-space demand for `session` on `key` and return its demand ID.
    pub(crate) fn demand(
        &mut self,
        session: u64,
        key: VolumeKey,
        need: u64,
        cwd: Option<PathBuf>,
        now: Instant,
    ) -> u64 {
        let id = self.next_demand;
        self.next_demand = self.next_demand.wrapping_add(1).max(1);
        self.demands.push(Demand {
            id,
            session,
            key,
            need,
            cwd,
            armed_at: now,
        });
        id
    }

    /// Remove demand `id`.
    pub(crate) fn drop_demand(&mut self, id: u64) {
        self.demands.retain(|d| d.id != id);
    }

    /// Return the maximum demanded bytes on `key` and the working directories of demanding
    /// sessions.
    pub(crate) fn adopt(&self, key: &VolumeKey) -> Option<(u64, Vec<PathBuf>)> {
        let mine: Vec<&Demand> = self.demands.iter().filter(|d| &d.key == key).collect();
        let need = mine.iter().map(|d| d.need).max()?;
        Some((need, mine.iter().filter_map(|d| d.cwd.clone()).collect()))
    }
}

/// Mutex-guarded [`Board`] paired with a [`Condvar`] for parked waiters.
pub(crate) struct Shelf {
    board: Mutex<Board>,
    changed: Condvar,
}

impl Shelf {
    /// Create an empty [`Shelf`].
    pub(crate) const fn new() -> Self {
        Self {
            board: Mutex::new(Board::new()),
            changed: Condvar::new(),
        }
    }

    /// Lock and return the [`Board`].
    pub(crate) fn board_lock(&self) -> MutexGuard<'_, Board> {
        self.board.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The watch made progress.
    pub(crate) fn beat(&self) {
        #[cfg(test)]
        BEATS_HERE.set(BEATS_HERE.get() + 1);
        self.board_lock().beat();
    }

    /// A look starts. Its flag and its beat move under one lock: a waiter that read the flag with
    /// the old beat beside it would answer a stall for a look that has only just begun.
    pub(crate) fn look_began(&self) {
        let mut b = self.board_lock();
        b.look_in_flight = true;
        b.beat();
    }

    /// A look ended, under one lock for the same reason as [`Self::look_began`].
    pub(crate) fn look_ended(&self) {
        let mut b = self.board_lock();
        b.look_in_flight = false;
        b.beat();
    }

    /// Mutate the [`Board`] under its lock and notify all parked waiters.
    pub(crate) fn publish(&self, f: impl FnOnce(&mut Board)) {
        {
            let mut b = self.board_lock();
            f(&mut b);
        }
        self.changed.notify_all();
    }

    /// Record `(bytes, what)` removals into today's [`FreedTally`].
    pub(crate) fn tally_freed(&self, removals: &[(u64, &str)]) {
        if removals.is_empty() {
            return;
        }
        let now = Clock::read();
        let mut b = self.board_lock();
        for &(bytes, what) in removals {
            b.freed.add(bytes, what, &now);
        }
    }

    /// The build directories the last reclamation pass left and must keep (the `disk.low` hogs
    /// line's), on any volume.
    pub(crate) fn kept_build_dirs(&self) -> Vec<String> {
        self.board_lock()
            .last_pass
            .as_ref()
            .map(|p| {
                p.left
                    .iter()
                    .map(|h| h.path.display().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The dangling `target*` links the disk watch's searches recorded ([`Board::dangling`]).
    pub(crate) fn dangling(&self) -> Vec<aterm_agent::harness::disk::discover::Dangling> {
        self.board_lock().dangling.clone()
    }

    /// Keeps on the board each dangling link that still dangles, with those `found` by a look's
    /// search added ([`aterm_agent::harness::disk::discover::still_dangling`], read with no lock
    /// held); publishes only a change. Each look calls it, so a link fixed since drops off with
    /// no search, and a search that walked some checkouts drops none that still dangles.
    pub(crate) fn keep_dangling(&self, found: Vec<aterm_agent::harness::disk::discover::Dangling>) {
        let kept = self.dangling();
        if kept.is_empty() && found.is_empty() {
            return;
        }
        let now = aterm_agent::harness::disk::discover::still_dangling(kept.clone(), found);
        if now != kept {
            self.publish(|b| b.dangling = now);
        }
    }

    /// Read [`DiskFactsRaw`] for `home` at `now` from this shelf.
    pub(crate) fn deck_facts(&self, home: &VolumeKey, now: &Clock) -> DiskFactsRaw {
        self.board_lock().deck_facts(home, now)
    }

    /// Whether the disk watch is active on this shelf.
    pub(crate) fn watched(&self) -> bool {
        self.board_lock().watched
    }

    /// Wait on the shelf's condvar up to `wait`.
    pub(crate) fn park<'a>(
        &'a self,
        guard: MutexGuard<'a, Board>,
        wait: Duration,
    ) -> MutexGuard<'a, Board> {
        match self.changed.wait_timeout(guard, wait) {
            Ok((g, _)) => g,
            Err(p) => p.into_inner().0,
        }
    }
}

/// Process-wide disk board shelf.
pub(crate) static SHELF: Shelf = Shelf::new();

#[cfg(test)]
impl Shelf {
    /// Whether the board mutex is currently unlocked (for tests).
    pub(crate) fn is_free(&self) -> bool {
        self.board.try_lock().is_ok()
    }
}

/// Mutate [`SHELF`] under its lock and wake all parked waiters.
pub(crate) fn publish(f: impl FnOnce(&mut Board)) {
    SHELF.publish(f);
}

#[cfg(test)]
thread_local! {
    /// This thread's [`Shelf::beat`]s ([`beats_here`]).
    static BEATS_HERE: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Record a disk-watch progress heartbeat on [`SHELF`]: what a look's thread hears from
/// [`aterm_uds::progress`].
pub(crate) fn beat() {
    SHELF.beat();
}

/// This thread's [`Shelf::beat`]s so far.
#[cfg(test)]
pub(crate) fn beats_here() -> u32 {
    BEATS_HERE.get()
}

/// Set whether the disk watch on [`SHELF`] is suspended.
pub(crate) fn suspend(suspended: bool) {
    let mut b = SHELF.board_lock();
    b.watch = match (suspended, b.watch) {
        (true, _) => Watch::Suspended,
        (false, Watch::Suspended) => Watch::Running {
            beat: Instant::now(),
        },
        (false, w) => w,
    };
}

/// Set whether a disk watch is active in this process.
pub(crate) fn watching(on: bool) {
    let mut b = SHELF.board_lock();
    b.watched = on;
}

/// Format `status` disk fields (`disk=`, `disk_free_mb=`, `disk_room_mb=`) for `session`.
pub(crate) fn status_fields(session: u64, cwd: Option<&str>) -> String {
    SHELF
        .board_lock()
        .status_words(session, cwd, Instant::now())
}

impl Board {
    /// [`status_fields`] from this board at `now`.
    pub(crate) fn status_words(&self, session: u64, cwd: Option<&str>, now: Instant) -> String {
        match self.row_of(session, cwd) {
            Some(r) => format!(
                "disk={} disk_free_mb={} disk_room_mb={} disk_age_ms={}",
                r.level.as_str(),
                r.reading.free >> 20,
                r.room
                    .map_or_else(|| "-".to_owned(), |room| (room >> 20).to_string()),
                now.saturating_duration_since(r.as_of).as_millis()
            ),
            None => "disk=- disk_free_mb=- disk_room_mb=- disk_age_ms=-".to_owned(),
        }
    }
}

#[cfg(test)]
#[path = "disk_board_tests.rs"]
mod tests;
