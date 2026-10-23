// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared disk-watch state board (`status disk=` and `await sys`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::harness::disk::{Class, Hog, Level, Thresholds};
use aterm_sysprobe::volumes::{Reading, Table, VolumeKey};

/// Maximum number of deduplicated volumes retained on the board.
pub(crate) const MAX_VOLUMES: usize = 8;

/// Maximum number of Windows 8.3 short-to-long working directory mappings cached.
pub(crate) const MAX_SPELLED: usize = 256;

/// Disk-watch progress state for `await sys` hang detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Watch {
    /// No disk look has started yet.
    NotStarted,
    /// Disk look is active with last progress at `beat`.
    Running { beat: Instant },
    /// Host is suspended; no look runs.
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
    /// Active unmet free-space demands.
    pub(crate) demands: Vec<Demand>,
    /// Full-disk episode state.
    pub(crate) episode: DiskEpisode,
    /// Today's freed-bytes tally.
    pub(crate) freed: FreedTally,
    /// The next demand's id.
    next_demand: u64,
}

impl Board {
    const fn new() -> Self {
        Self {
            watch: Watch::NotStarted,
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

/// Record a disk-watch progress heartbeat on [`SHELF`].
pub(crate) fn beat() {
    let mut b = SHELF.board_lock();
    if b.watch != Watch::Suspended {
        b.watch = Watch::Running {
            beat: Instant::now(),
        };
    }
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
    let b = SHELF.board_lock();
    match b.row_of(session, cwd) {
        Some(r) => format!(
            "disk={} disk_free_mb={} disk_room_mb={} disk_age_ms={}",
            r.level.as_str(),
            r.reading.free >> 20,
            r.room
                .map_or_else(|| "-".to_owned(), |room| (room >> 20).to_string()),
            r.as_of.elapsed().as_millis()
        ),
        None => "disk=- disk_free_mb=- disk_room_mb=- disk_age_ms=-".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(mount: &str, key: VolumeKey, free: u64, level: Level) -> VolumeRow {
        VolumeRow {
            reading: Reading {
                mount: PathBuf::from(mount),
                key,
                total: 2000 << 30,
                free,
                files_free: Some(1 << 30),
                dev: 1,
                read_only: false,
            },
            thresholds: aterm_agent::harness::disk::Config::default().thresholds(Some(2000 << 30)),
            level,
            room: Some(16 << 30),
            as_of: Instant::now(),
            as_of_unix: 0,
            reclaimed_here: true,
        }
    }

    /// `status` reads `-` on an empty board.
    #[cfg(unix)]
    #[test]
    fn status_reads_the_row_of_the_sessions_volume_or_dashes() {
        let mut b = Board::new();
        let empty = b.row_of(7, Some("/Users//x")).is_none();
        assert!(empty);
        b.table = Table {
            mounts: vec![aterm_sysprobe::volumes::Mount {
                on: PathBuf::from("/System/Volumes/Data"),
                from: "/dev/disk3s5".to_owned(),
                fstype: "apfs".to_owned(),
                local: true,
                automounted: false,
                removable: false,
                read_only: false,
                container: Some("disk3".to_owned()),
            }],
            complete: true,
            firmlinks: aterm_sysprobe::volumes::parse_firmlinks("/Users\tUsers\n"),
        };
        b.volumes.push(row(
            "/System/Volumes/Data",
            VolumeKey::Container("disk3".to_owned()),
            41 << 30,
            Level::Low,
        ));
        b.sessions.insert(7, 0);
        let mapped = b.row_of(7, None).expect("mapped");
        assert_eq!(mapped.level, Level::Low);
        let spelled = b.row_of(8, Some("/Users//x/repo")).expect("by its spelling");
        assert_eq!(spelled.reading.free, 41 << 30);
        assert!(b.row_of(9, Some("/elsewhere")).is_none());
        assert!(b.row_of(9, Some("relative")).is_none());
    }

    /// WINDOWS: a session's working directory in any spelling a Windows tab reports reads the row
    /// of the volume the table places it on.
    #[cfg(windows)]
    #[test]
    fn a_windows_spelling_reads_the_row_of_its_volume() {
        use aterm_sysprobe::volumes::{Mount, NETWORK};
        let volume = |on: &str, n: u32, guid: &str| Mount {
            on: PathBuf::from(on),
            from: format!(r"\Device\HarddiskVolume{n}"),
            fstype: String::new(),
            local: true,
            automounted: false,
            removable: false,
            read_only: false,
            container: Some(format!("Volume{{{guid}}}")),
        };
        let c = volume(r"C:\", 3, "6140fc3b-044f-43d7-b060-cb98170c7a63");
        let key = c.key().expect("a volume key");
        let mut b = Board::new();
        b.table = Table {
            mounts: vec![
                Mount {
                    on: PathBuf::from(NETWORK),
                    from: "UNC".to_owned(),
                    fstype: String::new(),
                    local: false,
                    automounted: false,
                    removable: false,
                    read_only: false,
                    container: None,
                },
                c,
                volume(r"D:\", 4, "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d"),
            ],
            complete: true,
            firmlinks: Vec::new(),
        };
        b.volumes.push(row(r"C:\", key, 41 << 30, Level::Low));
        for spelled in [
            r"C:\Users\x\repo",
            r"\\?\C:\Users\x\repo",
            r"c:\USERS\X\REPO\",
            "C:/Users//x/repo",
            r"C:\",
        ] {
            let found = b.row_of(8, Some(spelled)).map(|r| r.reading.free);
            assert_eq!(found, Some(41 << 30), "{spelled}");
        }
        for unplaced in [r"D:\work", r"\\srv\share\x", r"C:\PROGRA~1\x", r"Users\x"] {
            assert!(b.row_of(9, Some(unplaced)).is_none(), "{unplaced}");
        }
    }

    #[test]
    fn demands_are_adopted_by_their_volumes_largest_need() {
        let mut b = Board::new();
        let key = VolumeKey::Container("disk3".to_owned());
        let other = VolumeKey::Fsid(9);
        let now = Instant::now();
        let a = b.demand(1, key.clone(), 5 << 30, Some(PathBuf::from("/h/a")), now);
        let c = b.demand(2, key.clone(), 9 << 30, Some(PathBuf::from("/h/b")), now);
        b.demand(3, other.clone(), 99 << 30, None, now);
        let (need, cwds) = b.adopt(&key).expect("adopted");
        assert_eq!(need, 9 << 30);
        assert_eq!(cwds.len(), 2);
        b.drop_demand(c);
        assert_eq!(b.adopt(&key).map(|(n, _)| n), Some(5 << 30));
        b.drop_demand(a);
        assert!(b.adopt(&key).is_none());
        b.refresh(row("/m", key.clone(), 1, Level::Short));
        b.refresh(row("/m", key.clone(), 2, Level::Short));
        assert_eq!(b.volumes.len(), 1);
        assert_eq!(b.row_keyed(&key).map(|r| r.reading.free), Some(2));
    }

    const GIB: u64 = 1 << 30;
    const UTC_MIDNIGHT: i64 = 20_718 * 86_400;
    const EAST_2H: i64 = 7_200;

    fn clock(unix: i64) -> Clock {
        Clock {
            at: Instant::now(),
            unix: Some(unix),
            offset: Some(EAST_2H),
        }
    }

    #[test]
    fn freed_tally_counts_the_local_day_and_starts_again_at_its_midnight() {
        let local_midnight = UTC_MIDNIGHT - EAST_2H;
        let mut t = FreedTally::new();
        assert_eq!(t.today(&clock(local_midnight - 600)), None);

        let at_2350 = clock(local_midnight - 600);
        t.add(5 * GIB, BUILD_CACHES, &at_2350);
        t.add(GIB, "old Claude Code versions", &at_2350);
        let today = t.today(&at_2350).expect("freed today");
        assert_eq!(today.aterm_bytes, 6 * GIB);
        assert_eq!(today.last_at, local_midnight - 600);
        assert_eq!(today.last_what, "old Claude Code versions");

        let at_2359 = clock(local_midnight - 60);
        t.add(3 * GIB, BUILD_CACHES, &at_2359);
        t.add(0, "old package builds", &clock(local_midnight - 30));
        let today = t.today(&at_2359).expect("freed today");
        assert_eq!(today.aterm_bytes, 9 * GIB);
        assert_eq!(today.last_at, local_midnight - 60);
        assert_eq!(today.last_what, "build caches");

        let no_zone = Clock {
            offset: None,
            ..clock(local_midnight + 60)
        };
        let no_wall = Clock {
            unix: None,
            offset: None,
            ..no_zone
        };
        t.add(4 * GIB, BUILD_CACHES, &no_zone);
        t.add(4 * GIB, BUILD_CACHES, &no_wall);
        assert_eq!((no_zone.day(), no_wall.day()), (None, None));
        assert_eq!(t.today(&no_zone), None);
        assert_eq!(t.today(&at_2359).map(|t| t.aterm_bytes), Some(9 * GIB));

        assert_eq!(t.today(&clock(local_midnight + 60)), None, "a new day");
        t.add(GIB, BUILD_CACHES, &clock(local_midnight + 60));
        t.add(2 * GIB, BUILD_CACHES, &clock(UTC_MIDNIGHT));
        let today = t.today(&clock(UTC_MIDNIGHT + 1)).expect("freed today");
        assert_eq!(today.aterm_bytes, 3 * GIB);
        assert_eq!(Some(today.day), clock(local_midnight).day());
        assert_eq!(today.last_at, UTC_MIDNIGHT);
    }

    fn lines() -> Thresholds {
        Thresholds {
            warn: 69 * GIB,
            floor: 34 * GIB,
            target: 50 * GIB,
            emergency: 8 * GIB,
            files_emergency: 10_000,
        }
    }

    fn measured(mount: &str, key: VolumeKey, free: u64, files_free: u64) -> VolumeRow {
        let thresholds = lines();
        VolumeRow {
            reading: Reading {
                mount: PathBuf::from(mount),
                key,
                total: 2000 * GIB,
                free,
                files_free: Some(files_free),
                dev: 1,
                read_only: false,
            },
            thresholds,
            level: thresholds.level(Some(free), Some(files_free)),
            room: Some(16 * GIB),
            as_of: Instant::now(),
            as_of_unix: 0,
            reclaimed_here: true,
        }
    }

    fn pass(key: VolumeKey, left: Vec<Hog>, ended_at_unix: Option<i64>) -> PassSummary {
        PassSummary {
            key,
            ended_at: Instant::now(),
            free_after: None,
            tiers: vec![1],
            freed: 0,
            demand: None,
            ran_out: false,
            spent: false,
            held_back: false,
            hogs: left.len(),
            largest: left.iter().map(|h| h.bytes).max().unwrap_or(0),
            left,
            ended_at_unix,
        }
    }

    #[test]
    fn deck_facts_with_the_watch_on() {
        let home = VolumeKey::Container("disk3".to_owned());
        let backup = VolumeKey::Fsid(9);
        let mut b = Board::new();
        b.watched = true;
        b.volumes.push(measured(
            "/System/Volumes/Data",
            home.clone(),
            41 * GIB,
            1 << 30,
        ));
        b.volumes.push(measured(
            "/Volumes/Backup",
            backup.clone(),
            12 * GIB,
            1 << 30,
        ));
        b.episode.stuck.insert("s1".to_owned(), "ENOSPC".to_owned());
        b.episode.stuck.insert("s2".to_owned(), "ENOSPC".to_owned());
        let beat = Instant::now();
        b.watch = Watch::Running { beat };
        let now = Clock {
            at: beat + crate::control::control_disk::STALL + Duration::from_secs(90),
            ..clock(UTC_MIDNIGHT + 37_800)
        };
        b.freed
            .add(41 * GIB, BUILD_CACHES, &clock(UTC_MIDNIGHT + 30_000));
        let hog = Hog {
            path: PathBuf::from("/Users//x/aterm/target"),
            bytes: 212 * GIB,
            bytes_partial: false,
            why: "a live checkout's build directory".to_owned(),
        };
        b.last_pass = Some(pass(
            home.clone(),
            vec![hog.clone()],
            Some(UTC_MIDNIGHT + 30_000),
        ));

        let f = b.deck_facts(&home, &now);
        assert!(f.watch);
        assert_eq!(f.level, Some(Level::Low));
        assert_eq!(
            (f.warn, f.floor, f.emergency),
            (Some(69 * GIB), Some(34 * GIB), Some(8 * GIB))
        );
        assert!(!f.files_emergency);
        assert_eq!(f.stuck, 2);
        assert_eq!(f.others, [("Backup".to_owned(), Level::Short, 12 * GIB)]);
        let quiet = crate::control::control_disk::STALL.as_secs() + 90;
        assert_eq!(
            f.stalled_since,
            Some(UTC_MIDNIGHT + 37_800 - i64::try_from(quiet).expect("seconds"))
        );
        let freed = f.freed_today.expect("freed today");
        assert_eq!(
            (freed.aterm_bytes, freed.last_at),
            (41 * GIB, UTC_MIDNIGHT + 30_000)
        );
        assert_eq!(f.hogs, std::slice::from_ref(&hog));
        assert_eq!(f.hogs_at, Some(UTC_MIDNIGHT + 30_000));

        let unread = Clock {
            unix: None,
            offset: None,
            ..now
        };
        assert_eq!(b.deck_facts(&home, &unread).stalled_since, None);
        b.last_pass = Some(pass(home.clone(), vec![hog.clone()], None));
        let f = b.deck_facts(&home, &now);
        assert_eq!((f.hogs.len(), f.hogs_at), (1, None));

        let fresh = Clock {
            at: beat + crate::control::control_disk::STALL - Duration::from_secs(1),
            ..now
        };
        assert_eq!(b.deck_facts(&home, &fresh).stalled_since, None);
        b.watch = Watch::Suspended;
        assert_eq!(b.deck_facts(&home, &now).stalled_since, None);

        b.volumes[0] = measured("/System/Volumes/Data", home.clone(), 41 * GIB, 10);
        let f = b.deck_facts(&home, &now);
        assert_eq!(f.level, Some(Level::Emergency));
        assert!(f.files_emergency);
        b.volumes[0] = measured("/System/Volumes/Data", home.clone(), 6 * GIB, 10);
        assert!(!b.deck_facts(&home, &now).files_emergency);
        b.volumes[0].reclaimed_here = false;
        let f = b.deck_facts(&home, &now);
        assert_eq!((f.warn, f.floor), (Some(69 * GIB), None));
        assert_eq!(f.emergency, Some(8 * GIB));
        b.volumes[0].reclaimed_here = true;
        assert_eq!(b.deck_facts(&home, &now).floor, Some(34 * GIB));
        b.volumes[0].thresholds.floor = 0;
        assert_eq!(b.deck_facts(&home, &now).floor, None);

        b.last_pass = Some(pass(backup.clone(), vec![hog], Some(UTC_MIDNIGHT)));
        let f = b.deck_facts(&home, &now);
        assert!(f.hogs.is_empty());
        assert_eq!(f.hogs_at, None);

        let elsewhere = b.deck_facts(&VolumeKey::Fsid(77), &now);
        assert!(elsewhere.watch);
        assert_eq!((elsewhere.level, elsewhere.warn), (None, None));
        assert_eq!(elsewhere.others.len(), 2);

        b.watched = false;
        assert_eq!(b.deck_facts(&home, &now), DiskFactsRaw::default());
    }

    #[test]
    fn with_the_harness_off_the_deck_reads_no_board_facts() {
        let home = VolumeKey::Container("disk3".to_owned());
        let shelf = Shelf::new();
        shelf.publish(|b| {
            b.watched = true;
            b.volumes.push(measured(
                "/System/Volumes/Data",
                home.clone(),
                41 * GIB,
                1 << 30,
            ));
            b.episode.stuck.insert("s1".to_owned(), "ENOSPC".to_owned());
            b.freed.add(GIB, BUILD_CACHES, &clock(UTC_MIDNIGHT));
        });
        let now = clock(UTC_MIDNIGHT + 60);
        assert!(shelf.watched());
        let on = shelf.deck_facts(&home, &now);
        assert!(on.watch);
        assert_eq!(on.level, Some(Level::Low));
        assert_eq!(on.stuck, 1);
        assert!(on.freed_today.is_some());
        shelf.publish(|b| b.watched = false);
        assert!(!shelf.watched());
        let off = shelf.deck_facts(&home, &now);
        assert_eq!(off, DiskFactsRaw::default());
        assert!(!off.watch);
        assert_eq!(off.level, None);
        assert_eq!((off.warn, off.floor, off.emergency), (None, None, None));
        assert_eq!((off.stuck, off.freed_today), (0, None));
    }

    #[test]
    fn a_waits_own_reading_keeps_the_watchs_judgement_of_its_volume() {
        let key = VolumeKey::Container("disk3".to_owned());
        let plain = aterm_agent::harness::disk::Config::default().thresholds(Some(2000 << 30));
        let raised = aterm_agent::harness::disk::Thresholds {
            warn: 900 << 30,
            ..plain
        };
        let mut b = Board::new();
        b.volumes.push(VolumeRow {
            thresholds: raised,
            level: raised.level(Some(41 << 30), Some(1 << 30)),
            ..row("/m", key.clone(), 41 << 30, Level::Low)
        });
        let wait = |free: u64, key: VolumeKey| VolumeRow {
            thresholds: plain,
            level: plain.level(Some(free), Some(1 << 30)),
            room: None,
            reclaimed_here: false,
            ..row("/m", key, free, Level::Ok)
        };
        assert_ne!(
            raised.level(Some(500 << 30), Some(1 << 30)),
            plain.level(Some(500 << 30), Some(1 << 30)),
            "PREMISE: the raised lines read 500 GiB otherwise"
        );
        b.refresh(wait(500 << 30, key.clone()));
        let held = b.row_keyed(&key).expect("the watch's row").clone();
        assert_eq!(held.reading.free, 500 << 30, "the wait's reading");
        assert_eq!(held.room, Some(16 << 30), "the watch's room");
        assert!(held.reclaimed_here, "the watch's reclaim");
        assert_eq!(held.thresholds, raised, "the watch's lines");
        assert_eq!(held.level, raised.level(Some(500 << 30), Some(1 << 30)));

        let other = VolumeKey::Fsid(9);
        b.refresh(wait(7 << 30, other.clone()));
        let added = b.row_keyed(&other).expect("a volume no row held");
        assert_eq!(
            (added.room, added.reclaimed_here, added.thresholds),
            (None, false, plain)
        );

        b.measured(wait(400 << 30, key.clone()));
        let judged = b.row_keyed(&key).expect("the watch's row");
        assert_eq!(
            (judged.room, judged.reclaimed_here, judged.thresholds),
            (None, false, plain)
        );
    }

    fn one_volume_board() -> (Board, &'static str, &'static str) {
        let mut b = Board::new();
        #[cfg(windows)]
        let (mount, placed, short) = (
            aterm_sysprobe::volumes::Mount {
                on: PathBuf::from(r"C:\"),
                from: r"\Device\HarddiskVolume3".to_owned(),
                fstype: String::new(),
                local: true,
                automounted: false,
                removable: false,
                read_only: false,
                container: Some("Volume{6140fc3b-044f-43d7-b060-cb98170c7a63}".to_owned()),
            },
            r"\\?\C:\Program Files\x",
            r"C:\PROGRA~1\x",
        );
        #[cfg(not(windows))]
        let (mount, placed, short) = (
            aterm_sysprobe::volumes::Mount {
                on: PathBuf::from("/System/Volumes/Data"),
                from: "/dev/disk3s5".to_owned(),
                fstype: "apfs".to_owned(),
                local: true,
                automounted: false,
                removable: false,
                read_only: false,
                container: Some("disk3".to_owned()),
            },
            "/System/Volumes/Data/Users//x/repo",
            "/tmp/REPO~1/x",
        );
        let key = mount.key().expect("a volume key");
        b.volumes
            .push(row(&mount.on.to_string_lossy(), key, 41 << 30, Level::Low));
        b.table = Table {
            mounts: vec![mount],
            complete: true,
            firmlinks: Vec::new(),
        };
        (b, placed, short)
    }

    #[test]
    fn a_session_spelled_long_reads_its_volumes_row_while_it_reports_it() {
        let (mut b, placed, short) = one_volume_board();
        assert!(b.row_at(Path::new(placed)).is_some(), "PREMISE: {placed}");
        assert!(b.row_of(7, Some(short)).is_none(), "unspelled: {short}");
        b.spell(7, PathBuf::from(short), PathBuf::from(placed));
        let free = |b: &Board, id: u64, cwd: &str| b.row_of(id, Some(cwd)).map(|r| r.reading.free);
        assert_eq!(free(&b, 7, short), Some(41 << 30));
        assert_eq!(free(&b, 8, short), None, "another session's spelling");
        let moved = if cfg!(windows) {
            r"C:\PROGRA~2\y"
        } else {
            "/tmp/OTHER~1/y"
        };
        assert_eq!(free(&b, 7, moved), None, "the session moved");
        let first = b.volumes[0].clone();
        b.volumes = vec![VolumeRow {
            reading: Reading {
                free: 9 << 30,
                ..first.reading
            },
            ..first
        }];
        assert_eq!(free(&b, 7, short), Some(9 << 30), "the new rows");
        for id in 100..100 + MAX_SPELLED as u64 {
            b.spell(id, PathBuf::from(short), PathBuf::from(placed));
        }
        assert_eq!(b.spelled.len(), MAX_SPELLED);
        assert_eq!(free(&b, 7, short), None, "the oldest went");
        assert_eq!(free(&b, 100, short), Some(9 << 30));
    }
}
