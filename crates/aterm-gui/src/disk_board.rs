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
    /// names in the last table (pure: nothing is looked at).
    pub(crate) fn row_of(&self, session: u64, cwd: Option<&str>) -> Option<&VolumeRow> {
        if let Some(row) = self
            .sessions
            .get(&session)
            .and_then(|i| self.volumes.get(*i))
        {
            return Some(row);
        }
        self.row_at(Path::new(cwd?))
    }

    /// The row of the volume holding `path`, by the last table (pure).
    pub(crate) fn row_at(&self, path: &Path) -> Option<&VolumeRow> {
        let mount = aterm_sysprobe::volumes::mount_of(path, &self.table)?;
        self.volumes
            .iter()
            .find(|r| r.reading.mount == mount.on)
            .or_else(|| {
                let key = mount.container.clone().map(VolumeKey::Container)?;
                self.volumes.iter().find(|r| r.reading.key == key)
            })
    }

    /// The row of volume `key`.
    pub(crate) fn row_keyed(&self, key: &VolumeKey) -> Option<&VolumeRow> {
        self.volumes.iter().find(|r| &r.reading.key == key)
    }

    /// Put `row` in place of the row of its volume (or add it, while there
    /// is room): a wait's own reading.
    pub(crate) fn refresh(&mut self, row: VolumeRow) {
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
/// where the board holds no row for it (not measured yet, an unsupported
/// platform, a remote or automounted mount). From the board under its leaf
/// lock; no I/O.
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
    // Unix: a spelling resolves through the mount table `aterm_sysprobe::volumes`
    // reads on macOS and Linux only.
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
}
