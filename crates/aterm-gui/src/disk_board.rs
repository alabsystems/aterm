// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DISK BOARD (design `docs/DESIGN-disk-awareness-2026-10-17.md` §3.1):
//! what the disk watch last measured, ONE leaf-locked value written by the
//! watch (`harness_host::disk_look`) and read by `status` (`disk=`,
//! [`status_fields`]) and `await sys` (`control_disk`). It holds the
//! episode the watch keeps of sessions a full disk stopped ([`DiskEpisode`]),
//! so there is one store of the watch's last look, not two.
//!
//! Its lock is a LEAF: nothing is locked, read from the file system or
//! logged under it. A reader takes it, copies what it needs and lets go;
//! `status` does no I/O at all.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::harness::disk::{Level, Thresholds};
use aterm_sysprobe::volumes::{Reading, Table, VolumeKey};

/// The most volumes the board holds: the home volume and the volumes the
/// sessions work on, deduped by [`VolumeKey`].
pub(crate) const MAX_VOLUMES: usize = 8;

/// The most sessions whose 8.3 working directory the board keeps spelled
/// long ([`Board::spelled`]); past it the oldest session's (the lowest local
/// id) goes first.
pub(crate) const MAX_SPELLED: usize = 256;

/// Where the watch stands, for the hang detector of `await sys`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Watch {
    /// No look yet.
    NotStarted,
    /// Looking, and the last sign of progress came at `beat`
    /// (`aterm_agent::harness::disk::beat`).
    Running { beat: Instant },
    /// The host is not active (a seamless successor before its Commit): no
    /// look runs, and the hang detector does not count.
    Suspended,
}

/// One volume as the watch last measured it.
#[derive(Debug, Clone)]
pub(crate) struct VolumeRow {
    /// The reading.
    pub(crate) reading: Reading,
    /// The lines in force on it (the `[disk]` knobs, its share, its room).
    pub(crate) thresholds: Thresholds,
    /// Where it stands against them.
    pub(crate) level: Level,
    /// Its room ([`aterm_agent::harness::disk::room`]); `None` where no
    /// meter reads it (a volume only a wait measured, or the harness state
    /// missing): a `free>=room` on it is unavailable, never met at once.
    pub(crate) room: Option<u64>,
    /// When it was read.
    pub(crate) as_of: Instant,
    /// When it was read, unix seconds (a reply's `as_of=`).
    pub(crate) as_of_unix: i64,
    /// A pass removes on it (the home volume; never another).
    pub(crate) reclaimed_here: bool,
}

/// What the last pass did, for a wait to answer from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PassSummary {
    /// The volume it ran on.
    pub(crate) key: VolumeKey,
    /// When it ended.
    pub(crate) ended_at: Instant,
    /// Free bytes measured after it.
    pub(crate) free_after: Option<u64>,
    /// The tiers it ran (numbers).
    pub(crate) tiers: Vec<u8>,
    /// What its removals counted.
    pub(crate) freed: u64,
    /// The demand it adopted, need bytes.
    pub(crate) demand: Option<u64>,
    /// It RAN OUT of what it may take: every row of every tier it admitted
    /// tried, no tier stopped by a figure it could not read or by the bytes
    /// it counted, the search for build directories finished
    /// (`aterm_agent::harness::disk::Applied::ran_out`).
    pub(crate) ran_out: bool,
    /// It ran out ([`Self::ran_out`]) for its demand and free space is still
    /// below the need (`ERR unmet why=spent`): [`spent`].
    pub(crate) spent: bool,
    /// `disk.idle_build_dirs` off kept tier 3 from running (`why=held-back`).
    pub(crate) held_back: bool,
    /// How many build directories it could not reclaim, and the largest.
    pub(crate) hogs: usize,
    pub(crate) largest: u64,
}

/// THE SPENT RULE: a pass that adopted `demand` (need bytes) was spent for
/// it — what is left is not aterm's to free (`ERR unmet why=spent`) — only
/// when it `ran_out` of what it may take ([`PassSummary::ran_out`]) and free
/// space MEASURED after it (`free_after`) is still below the need. A pass
/// stopped for another reason (a figure it could not read, its search cut
/// short), or one whose figure after is unknown, is not spent: the wait parks
/// on. Bound to `DiskAwait`'s `pass` (`disk_await_conformance`).
pub(crate) fn spent(demand: Option<u64>, free_after: Option<u64>, ran_out: bool) -> bool {
    ran_out && demand.is_some_and(|need| free_after.is_some_and(|f| f < need))
}

/// An agent's wait for free space the watch has not met yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Demand {
    /// The wait's own id ([`Board::demand`]).
    pub(crate) id: u64,
    /// The session that waits.
    pub(crate) session: u64,
    /// The volume it waits on.
    pub(crate) key: VolumeKey,
    /// The free bytes it needs.
    pub(crate) need: u64,
    /// Its working directory: its checkout joins the pass's reach.
    pub(crate) cwd: Option<PathBuf>,
    /// When it was armed.
    pub(crate) armed_at: Instant,
}

/// THE DISK EPISODE: what the watch knows of sessions a full disk stopped,
/// read by each worker's idle points (`harness_host::disk_withholds`,
/// `disk_owed`).
#[derive(Debug)]
pub(crate) struct DiskEpisode {
    /// The volume's level at the last look (`None`: not measured yet).
    pub(crate) level: Option<Level>,
    /// Sessions whose Claude Code said the disk is full while the volume
    /// MEASURED at an emergency, with the code it named: no continuation is
    /// typed into them while it stands.
    pub(crate) stuck: BTreeMap<String, String>,
    /// Sessions owed the resume text once space is back, with it and the
    /// tries left.
    pub(crate) owed: BTreeMap<String, (String, u8)>,
    /// What the passes of this episode freed, and the largest of it.
    pub(crate) freed: u64,
    pub(crate) went: Vec<String>,
    /// Sessions whose sighting was said in the log (said again once the
    /// notice left the screen and came back).
    pub(crate) said: BTreeSet<String>,
    /// The owner's alert stands.
    pub(crate) alerted: bool,
}

/// The board ([`Shelf::board_lock`]).
#[derive(Debug)]
pub(crate) struct Board {
    /// Where the watch stands.
    pub(crate) watch: Watch,
    /// A disk watch runs in this process: the harness host is on
    /// (`[harness]` enabled; a suspended successor's host counts, it resumes
    /// at its Commit). Off, a wait registers no demand (`why=no-watch`) and a
    /// look adopts none.
    pub(crate) watched: bool,
    /// The mount table of the last look.
    pub(crate) table: Table,
    /// Each volume measured, at most [`MAX_VOLUMES`]; the first is the home
    /// volume once a look has run.
    pub(crate) volumes: Vec<VolumeRow>,
    /// Each session's volume (an index into [`Self::volumes`]), from its
    /// working directory resolved at the last look.
    pub(crate) sessions: BTreeMap<u64, usize>,
    /// Each session whose tab reported its working directory with 8.3 short
    /// names (`C:\Users\ANDREW~1\x`, which no mount places as spelled): that
    /// spelling and its long one, spelled OFF the status path when the
    /// report arrived or a wait read it (`harness_host::disk_cwd_reported`,
    /// `harness_host::disk_spell_now`). [`Self::row_of`] places the session
    /// by the long one while the tab still reports the short one, so `status
    /// disk=` fills before the watch's first look and where no watch runs.
    /// Spellings, never rows: no index here goes stale when the first
    /// reading or a look replaces the rows, or when the tab moves (its new
    /// spelling no longer matches). At most [`MAX_SPELLED`].
    pub(crate) spelled: BTreeMap<u64, (PathBuf, PathBuf)>,
    /// The last pass.
    pub(crate) last_pass: Option<PassSummary>,
    /// The waits for free space not met yet.
    pub(crate) demands: Vec<Demand>,
    /// The episode.
    pub(crate) episode: DiskEpisode,
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
            next_demand: 1,
        }
    }

    /// The row of session `session`: the volume its working directory was
    /// resolved onto at the last look, else the mount its spelling `cwd`
    /// names in the last table — the long spelling of that spelling where
    /// one was spelled for this session ([`Self::spelled`]) and the session
    /// still reports it (pure: nothing is looked at).
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

    /// Keep `long` as the long spelling of the working directory `short`
    /// session `session` reports ([`Self::spelled`]; its earlier one
    /// replaced), the oldest session's dropped past [`MAX_SPELLED`].
    pub(crate) fn spell(&mut self, session: u64, short: PathBuf, long: PathBuf) {
        self.spelled.insert(session, (short, long));
        while self.spelled.len() > MAX_SPELLED {
            self.spelled.pop_first();
        }
    }

    /// The row of the volume holding `path`, by the last table (pure).
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

    /// The row of volume `key`.
    pub(crate) fn row_keyed(&self, key: &VolumeKey) -> Option<&VolumeRow> {
        self.volumes.iter().find(|r| &r.reading.key == key)
    }

    /// Put a wait's own reading `row` in place of the row of its volume (or
    /// add it, while there is room). THE WATCH'S JUDGEMENT STAYS: a wait
    /// reads free space and nothing else, so a row already held keeps its
    /// room, its lines (the `[disk]` knobs raised to the room) and whether a
    /// pass removes on it, the level read again on those lines — whatever
    /// the wait built its row from. A wait whose spelling the board could not
    /// place (an 8.3 `vol=`) read the volume with no row to start from: its
    /// reading must not leave every tab on that volume `free>=room`
    /// `why=room-unknown`, `disk_room_mb=-`, and a demand `why=not-reclaimed`,
    /// until the next look (live5 F1). A volume no row holds yet is added as
    /// the wait read it: the owner's lines, no room, nothing removed there.
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

    /// Put the watch's own reading `row`, its judgement with it, in place of
    /// the row of its volume (or add it, while there is room): the volume as
    /// a pass left it (`harness_host::disk_look`).
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

    /// Register a demand: session `session` needs `need` bytes on `key`.
    /// Answers its id.
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

    /// Forget demand `id` (its wait answered or hung up).
    pub(crate) fn drop_demand(&mut self, id: u64) {
        self.demands.retain(|d| d.id != id);
    }

    /// The largest need on volume `key` and every demanding session's
    /// working directory there: what a pass adopts.
    pub(crate) fn adopt(&self, key: &VolumeKey) -> Option<(u64, Vec<PathBuf>)> {
        let mine: Vec<&Demand> = self.demands.iter().filter(|d| &d.key == key).collect();
        let need = mine.iter().map(|d| d.need).max()?;
        Some((need, mine.iter().filter_map(|d| d.cwd.clone()).collect()))
    }
}

/// A board and the condvar its parked waits sleep on: the process's one
/// ([`SHELF`]), or a test's own.
pub(crate) struct Shelf {
    board: Mutex<Board>,
    changed: Condvar,
}

impl Shelf {
    /// An empty board.
    pub(crate) const fn new() -> Self {
        Self {
            board: Mutex::new(Board::new()),
            changed: Condvar::new(),
        }
    }

    /// The board, its leaf lock held (the lock-order census's `board`).
    pub(crate) fn board_lock(&self) -> MutexGuard<'_, Board> {
        self.board.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Change the board under its lock and wake every parked wait.
    pub(crate) fn publish(&self, f: impl FnOnce(&mut Board)) {
        {
            let mut b = self.board_lock();
            f(&mut b);
        }
        self.changed.notify_all();
    }

    /// Park on the board until it changes or `wait` passes, `guard` released
    /// meanwhile and held again on return.
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

/// The process's board.
pub(crate) static SHELF: Shelf = Shelf::new();

#[cfg(test)]
impl Shelf {
    /// Whether nobody holds this board's lock this instant (a test's: a host
    /// callback must be able to read it). Asked of a test's OWN board: every
    /// host tick, idle point and `status` reply of another test holds the
    /// process's [`SHELF`] for an instant, so asked of that it is a race.
    pub(crate) fn is_free(&self) -> bool {
        self.board.try_lock().is_ok()
    }
}

/// Change the board under its lock and wake every parked wait.
pub(crate) fn publish(f: impl FnOnce(&mut Board)) {
    SHELF.publish(f);
}

/// THE BEAT: the watch made progress now ([`Watch::Running`]). Wakes no
/// wait: a wait reads the beat at its own wakes.
pub(crate) fn beat() {
    let mut b = SHELF.board_lock();
    if b.watch != Watch::Suspended {
        b.watch = Watch::Running {
            beat: Instant::now(),
        };
    }
}

/// The watch is suspended (`true`: the host is not active) or running again.
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

/// Whether a disk watch runs in this process (the harness host is on:
/// [`Board::watched`]), told by the host at each of its ticks.
pub(crate) fn watching(on: bool) {
    let mut b = SHELF.board_lock();
    b.watched = on;
}

/// `status`'s disk fields for session `session` working in `cwd` (its
/// reported spelling): `disk=<level> disk_free_mb=<n> disk_room_mb=<n>
/// disk_age_ms=<n>` of the volume holding its working directory, `-` each
/// where the board holds no row for it (not measured yet, a platform with no
/// volume reader, a remote or automounted mount, a spelling the table cannot
/// place and none spelled long for the session yet: [`Board::spelled`]).
/// From the board under its leaf lock; no I/O.
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

    /// `status` reads `-` on an empty board, the session's mapped row once a
    /// look resolved its working directory, and — for a session not mapped
    /// yet — the row its spelling's mount names in the last table. NEGATIVE
    /// CONTROL: a spelling no row holds reads `-`.
    // Unix: its spellings are macOS's (Windows' are
    // `a_windows_spelling_reads_the_row_of_its_volume`).
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

    /// WINDOWS: a session's working directory in any spelling a Windows tab
    /// reports — plain, verbatim (`\\?\`), another case, a trailing
    /// separator, forward slashes, the drive path an MSYS `/c/…` report is
    /// rewritten to — reads the row of the volume the table places it on,
    /// so `status disk=` fills for a Windows tab. NEGATIVE CONTROLS: a drive
    /// the board holds no row for, a share (`\\srv\share`: no local volume),
    /// an 8.3 spelling the key cannot place, and a relative path read `-`.
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

    /// A demand is registered, adopted as the largest need on its volume with
    /// every demanding session's directory, and dropped by its id; a wait's
    /// own reading replaces its volume's row.
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

    /// THE WATCH'S JUDGEMENT STAYS (live5 F1): a wait's own reading of a
    /// volume the watch measured — one built with no row to start from, as a
    /// wait whose 8.3 `vol=` the board could not place builds it: the
    /// owner's plain lines, no room, nothing removed there — replaces the
    /// row's reading and keeps the watch's room, its raised lines and
    /// `reclaimed_here`, the level read again on those lines. NEGATIVE
    /// CONTROLS: a volume no row holds yet is added as the wait read it, and
    /// the watch's own reading ([`Board::measured`]) replaces the row whole.
    #[test]
    fn a_waits_own_reading_keeps_the_watchs_judgement_of_its_volume() {
        let key = VolumeKey::Container("disk3".to_owned());
        let plain = aterm_agent::harness::disk::Config::default().thresholds(Some(2000 << 30));
        // The lines raised to a room (warn at least twice it).
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

        // The watch's own reading is its judgement: it replaces the row whole.
        b.measured(wait(400 << 30, key.clone()));
        let judged = b.row_keyed(&key).expect("the watch's row");
        assert_eq!(
            (judged.room, judged.reclaimed_here, judged.thresholds),
            (None, false, plain)
        );
    }

    /// A board holding one volume's row, its table placing `placed` on it,
    /// and an 8.3 spelling `short` of that place the table cannot place as
    /// written: a Windows drive (`C:\PROGRA~1\x`), or on unix a macOS
    /// spelling outside every mount (no short names exist there; the rule
    /// is pure).
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

    /// AN 8.3 TAB SPELLED LONG READS ITS VOLUME'S ROW (live5 F2): before any
    /// look maps it and where no watch runs, a session whose reported
    /// spelling was spelled long ([`Board::spell`]) reads the row its long
    /// spelling places on — after the rows are replaced too (the first
    /// reading publishes them whole), with no index to go stale — and only
    /// while it still reports that spelling. NEGATIVE CONTROLS: the spelling
    /// before it is spelled, another session reporting it, and the session
    /// once it moved, read no row. Past [`MAX_SPELLED`] the oldest session's
    /// spelling goes.
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
