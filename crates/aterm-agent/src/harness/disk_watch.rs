// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Foreground automatic disk maintenance. No helper or vendor process is spawned.
//! Ordinary unblocked SIGTERM ends this process, including an in-progress pass: its kernel
//! closes the lifetime pass lock and the existing write-ahead journal and durable removal stages
//! describe any interrupted work. This is not a bound on uninterruptible filesystem I/O.
//! Between pressure passes it sizes the rebuildable budget on a thread of its own and, over the
//! cap, runs the budget's pass ([`BudgetWatch`]).

use std::cell::RefCell;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use super::{budget, cadence, discover, fill, idle, maintenance, reserve, worktree};
use crate::harness::cli::{self, DiskLook, DiskTick, Env};

const LOOK_EVERY: Duration = Duration::from_secs(60);
const CENSUS_WAIT: Duration = Duration::from_secs(5);

struct Watch {
    cursor: discover::Cursor,
    last: Option<cadence::Pass>,
    /// An emergency pass that could not meet the line, and what it waits for.
    emergency: cadence::Emergency,
    daily: Mutex<maintenance::Schedule>,
    rooms: super::room::Rooms,
    fill: Option<(String, fill::Fill)>,
    notes: Vec<String>,
    worktrees: worktree::Schedule,
    budget: BudgetWatch,
}

impl Watch {
    fn new(state: &Path) -> Self {
        Self {
            cursor: discover::Cursor::new(),
            last: None,
            emergency: cadence::Emergency::default(),
            daily: Mutex::new(maintenance::Schedule::new()),
            rooms: super::room::Rooms::load(state),
            fill: None,
            notes: Vec::new(),
            worktrees: worktree::Schedule::new(),
            budget: BudgetWatch::new(state),
        }
    }

    fn look(
        &mut self,
        env: &Env,
        home: &Path,
        held: &reserve::PassLock,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> io::Result<()> {
        // The supported config is read at every look, including disabled and contended ones.
        let reading = cli::disk_config_read(env.config.as_deref());
        if self.notes != reading.notes {
            for note in &reading.notes {
                writeln!(err, "aterm harness disk watch: {note}")?;
            }
            self.notes.clone_from(&reading.notes);
        }
        held.validate_for(&reserve::pass_lock_for(home))?;
        let now = i64::try_from(
            aterm_time::unix_secs()
                .ok_or_else(|| io::Error::other("disk watch clock unavailable"))?,
        )
        .map_err(io::Error::other)?;
        let table = aterm_sysprobe::volumes::table();
        let measure = || super::volume_of(home, &table).ok();
        let Some(volume) = measure() else {
            self.cursor.reset_cycle();
            writeln!(out, "disk watch: volume unknown; no pass")?;
            return Ok(());
        };
        let key = volume.key.spelled();
        if let Some(meter) = self.rooms.meter(&key) {
            meter.observe(now, volume.free);
        }
        let room = self.rooms.room(&key, now, volume.total);
        let config = reading.on_volume_with(Some(volume.total), Some(room));
        let thresholds = reading.thresholds_with(Some(volume.total), Some(room));
        let level = thresholds.level(Some(volume.free), volume.files_free);
        let off = config.auto_free_gib == 0 || level == super::Level::Unknown;
        let pass_path = reserve::pass_lock_for(home);
        let started = Instant::now();
        let mut freed = 0u64;
        // The reserve's bytes are no sweep's or pass's: they never stand for the pass's progress.
        let mut released = 0u64;
        if !off {
            // The reserve goes FIRST, before anything this look writes: its unlink needs no
            // space, and at a real ENOSPC the room record's save below could not be written.
            // The outer run retains the same real guard through off/standby/errors/sleep.
            held.validate_for(&pass_path)?;
            let reserve_path = reserve::path_for(home);
            let moved = cli::disk_reserve_look(
                &env.state,
                &reserve_path,
                level,
                Some(volume.free),
                &thresholds,
                Some(room),
                now,
            );
            if let cli::ReserveMove::Released(bytes) = moved {
                released = bytes;
                freed = freed.saturating_add(bytes);
            }
        }
        // A save that fails is said and not fatal to the look (nor is saying it, on a full disk
        // that holds this process's stderr): `save_due` stamped it, so it is tried again within
        // the hour.
        if self.rooms.save_due(now)
            && let Err(e) = self.rooms.save(&env.state)
        {
            let _ = writeln!(
                err,
                "aterm harness disk watch: the room record was not saved ({e}); it is saved \
                 again within the hour"
            );
        }
        if off {
            self.cursor.reset_cycle();
            writeln!(
                out,
                "disk watch: automatic removal disabled or volume unknown; no pass"
            )?;
            return Ok(());
        }
        let entry = self
            .fill
            .get_or_insert_with(|| (key.clone(), fill::Fill::new()));
        if entry.0 != key {
            *entry = (key.clone(), fill::Fill::new());
            self.last = None;
            self.emergency.reset();
        }
        let fill = &mut entry.1;
        let projected = fill.observe(
            now.saturating_mul(1_000),
            volume.free,
            Some(volume.total),
            thresholds.short_below(),
        );
        let placing = discover::placing_of(home, &table);
        // The shared daily stamp borrows the genuine lifetime pass.lock guard.
        let daily =
            maintenance::eligible(&config, level, true, placing.is_some(), projected.is_some());
        maintenance::run(
            &self.daily,
            started,
            daily,
            || {
                let path = reserve::pass_lock_for(home);
                maintenance::claim_held(
                    path.parent()
                        .ok_or_else(|| io::Error::other("disk pass directory unavailable"))?,
                    now,
                    held,
                )
                .map(|due| due.then_some(held))
            },
            || {
                let pass = cli::disk_sweep_journalled(&env.state, now, |record| {
                    atpkg::build_cache::reclaim_home_watched(
                        atpkg::build_cache::ReclaimAfter::CALM,
                        placing.as_ref(),
                        &mut |path, why| held.validate_for(&pass_path).is_ok() && record(path, why),
                    )
                });
                freed = freed.saturating_add(pass.freed());
                // While space is ample, measure who holds the disk, so the row a later
                // crossing writes names holders from its first tick.
                let _ = atpkg::build_cache::holders::measure_home(super::HOLDERS_DAILY_BUDGET);
            },
        )?;
        let pass_level = if projected.is_some() && level < super::Level::Short {
            super::Level::Short
        } else {
            level
        };
        cli::disk_emergency_look(
            &env.state,
            &mut self.emergency,
            pass_level,
            Some(volume.free),
            thresholds.emergency,
        );
        // An emergency pass that wrote `cannot-meet` waits for free space, a lock, the clock or
        // the live sessions to move, not for the interval.
        let look = cadence::Look {
            at: started,
            unix: now,
            level: pass_level,
            free: Some(volume.free),
            room,
            poked: false,
            demand: false,
        };
        let due = self.emergency.due(&mut self.last, &look, || {
            let snap = Snapshot::read();
            snap.sessions.unknown.is_none().then(|| snap.live())
        });
        // A legacy GUI admission captured before config changed cannot slip between ticks.
        held.validate_for(&pass_path)?;
        // The worktree step runs hourly on a look with no pressure pass due, never ahead of
        // one: under pressure it waits for a look between passes (a pass that freed nothing
        // stops being due), and it is bounded by its wall and its git's.
        if !due && self.worktrees.due(started) {
            let snap = Snapshot::read();
            let open = worktree::open_sessions(home);
            let transcripts = home.join(".claude/projects");
            // The watch holds no window's sessions, so a build-start line says nothing here of
            // whose a worktree is: each waits seven days of quiet, beside the session, transcript
            // and process checks. The step reads the record again before a worktree that stays
            // gives back a build directory.
            let record = atpkg::build_start::record();
            let starts = record
                .as_deref()
                .map(atpkg::build_start::read)
                .unwrap_or_default();
            let look = cli::WorktreeLook {
                state: &env.state,
                home,
                now,
                transcripts: Some(&transcripts),
                session_cwds: &snap.sessions.cwds,
                session_cwd_unknown: snap.sessions.unknown.as_deref(),
                open: &open,
                table: Some(&table),
                roster: None,
                starts: &starts,
                record: record.as_deref(),
            };
            held.validate_for(&pass_path)?;
            let done = cli::disk_worktree_step(&look, config, &mut self.worktrees, &|| {
                held.validate_for(&pass_path).is_ok()
            });
            freed = freed.saturating_add(done.freed());
            if !done.removed.is_empty() {
                writeln!(
                    out,
                    "disk watch: removed {} worktree(s), {} back",
                    done.removed.len(),
                    super::human_bytes(done.removed_bytes())
                )?;
            }
            if !done.given_back.is_empty() {
                writeln!(
                    out,
                    "disk watch: shared build directories of worktrees that stay: {} went, {} back",
                    done.given_back.len(),
                    super::human_bytes(done.given_back_bytes())
                )?;
            }
        }
        if !due {
            // A projection's looks belong to the pressure pass, which may be due at the next.
            if projected.is_none() {
                let at = BudgetAt {
                    env,
                    home,
                    held,
                    table: &table,
                    now,
                    level,
                    thresholds: &thresholds,
                    config,
                    volume: &volume,
                    room,
                    placing: placing.as_ref(),
                };
                freed = freed.saturating_add(self.budget.step(&at, out)?);
            }
            credit(fill, freed, volume.free, measure().map(|r| r.free), started);
            writeln!(
                out,
                "disk watch: {} free; no pressure pass due",
                super::human_bytes(volume.free)
            )?;
            return Ok(());
        }
        let before = measure().map(|r| r.free);
        let goal = projected.as_ref().map_or_else(
            || config.pressure_target_bytes(),
            |p| config.projected_target(p),
        );
        if config.dead_build_dirs {
            let after = atpkg::build_cache::ReclaimAfter::by_floor(config.below_auto_floor(before));
            let pass = cli::disk_sweep_journalled(&env.state, now, |record| {
                atpkg::build_cache::reclaim_home_watched(
                    after,
                    placing.as_ref(),
                    &mut |path, why| {
                        held.validate_for(&pass_path).is_ok()
                            && measure().is_some_and(|r| r.free < goal)
                            && record(path, why)
                    },
                )
            });
            freed = freed.saturating_add(pass.freed());
        }
        let snapshot = Snapshot::read();
        let roots = search_roots(home, &table, &snapshot);
        let transcripts = home.join(".claude/projects");
        let links = cli::default_link_dirs(home);
        let look = DiskLook {
            state: &env.state,
            volume: home,
            free: measure().map(|r| r.free),
            targets: &[],
            discover: Some(&roots),
            transcripts: Some(&transcripts),
            now,
            room: Some(room),
            table: Some(&table),
            demand: None,
            projected,
            session_cwds: &snapshot.sessions.cwds,
            session_cwd_unknown: snapshot.sessions.unknown.as_deref(),
            link_dirs: &links,
        };
        let judge = super::Judge {
            now,
            threshold_days: config.target_stale_days,
            device: discover::device_of(home),
            volume: super::target::VolumeGuid::of_path(home, &table),
        };
        let mut actual_remove = remover(&env.state, judge);
        let mut remove = |row: &super::Row| {
            held.validate_for(&pass_path)?;
            actual_remove(row)
        };
        let tick = cli::disk_tick_paged(
            &look,
            config,
            &mut remove,
            &mut || measure().map(|r| r.free),
            &mut self.cursor,
        );
        let reclaimed = match &tick {
            DiskTick::Reclaimed(done, _) => done.freed_bytes,
            DiskTick::Plenty => 0,
        };
        freed = freed.saturating_add(reclaimed);
        let after_reading = measure();
        let after = after_reading.as_ref().map(|r| r.free);
        let level_after = thresholds.level(after, after_reading.and_then(|r| r.files_free));
        let at = cli::EmergencyAt {
            state: &env.state,
            now,
            level: pass_level,
            volume: &key,
            total: volume.total,
            line: thresholds.emergency,
            live: snapshot.live(),
        };
        let done = match &tick {
            DiskTick::Reclaimed(done, _) => Some(done),
            DiskTick::Plenty => None,
        };
        // What this look's sweeps freed: all it freed but the pass's own and the reserve's.
        let swept = freed.saturating_sub(reclaimed).saturating_sub(released);
        cli::disk_emergency_end(&at, &mut self.emergency, done, swept, after);
        credit(fill, freed, volume.free, after, started);
        let progress = freed > 0;
        cadence::ran(
            &mut self.last,
            started,
            before,
            !progress
                && (level_after == super::Level::Emergency
                    || (level_after == super::Level::Unknown && level == super::Level::Emergency)),
            pass_level,
            progress,
        );
        writeln!(
            out,
            "disk watch: pressure pass; {} accounted reclaimed; {} free",
            super::human_bytes(freed),
            after
                .map(super::human_bytes)
                .unwrap_or_else(|| "unknown".into())
        )?;
        Ok(())
    }
}

fn credit(fill: &mut fill::Fill, counted: u64, before: u64, after: Option<u64>, started: Instant) {
    if let Some(after) = after {
        fill.credit(
            counted,
            before,
            after,
            i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
        );
    }
}

/// What one look read that its budget step ([`BudgetWatch::step`]) runs against.
struct BudgetAt<'a> {
    env: &'a Env,
    home: &'a Path,
    /// The watch's lifetime pass lock: every removal checks it is still this one.
    held: &'a reserve::PassLock,
    table: &'a aterm_sysprobe::volumes::Table,
    now: i64,
    /// The volume's level at the look: a page stands down where a measure reads it worse.
    level: super::Level,
    thresholds: &'a super::Thresholds,
    config: super::Config,
    volume: &'a aterm_sysprobe::volumes::Reading,
    room: u64,
    placing: Option<&'a discover::checkout::Placing>,
}

/// THE BUDGET'S SIDE OF THE WATCH: its sizing (on a thread of its own, at most hourly across
/// the account's processes, `budget::claim`) and its pass, one page a look on a look with no
/// pressure pass due ([`budget::Run`]), with a discovery cursor of its own.
struct BudgetWatch {
    cursor: discover::Cursor,
    run: budget::Run,
    /// When this process last began a sizing, or the record says one began.
    began: Option<i64>,
    /// The last sizing: the record's at start, then each this process's thread kept.
    sizing: Arc<Mutex<Option<budget::Sizing>>>,
    /// A sizing runs on its thread now: no other begins and no page runs meanwhile.
    flight: Arc<AtomicBool>,
}

/// Clears a sizing's flight flag when its thread ends, a panic included.
struct Landed(Arc<AtomicBool>);

impl Drop for Landed {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl BudgetWatch {
    fn new(state: &Path) -> Self {
        let record = budget::Record::load(state);
        Self {
            cursor: discover::Cursor::new(),
            run: budget::Run::default(),
            began: record.began_unix,
            sizing: Arc::new(Mutex::new(record.last)),
            flight: Arc::new(AtomicBool::new(false)),
        }
    }

    /// One look's budget step: begin a sizing where one is due and the volume reads `ok`, else,
    /// over the cap by the last sizing, one page of the pass (on the run's first page the shared
    /// build cache's parts no live session's builds used, unwritten an hour, [`budget_sweep`],
    /// then tier 1, 2 and 3 rows of what no live session keeps, [`cli::disk_tick_budget`]), each
    /// removal under `at.held` and stopping at the volume's free target, or where a measure
    /// reads the volume worse than `at.level`. What keeps the live sessions' builds is read at
    /// the page's start and again before its rows, and the page stands down where it does not
    /// read whole. The bytes it accounted reclaimed. One line goes to `out` where a page ran.
    fn step(&mut self, at: &BudgetAt<'_>, out: &mut dyn Write) -> io::Result<u64> {
        let cap = at.config.cap_used_gib;
        if self.flight.load(Ordering::SeqCst) {
            return Ok(0);
        }
        if budget::admitted(at.level, true, false, false, cap) && budget::due(self.began, at.now) {
            self.size(at);
            return Ok(0);
        }
        if !budget::pass_admitted(true, false, false, cap, at.config.auto_free_gib) {
            return Ok(0);
        }
        let Some(sizing) = self
            .sizing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return Ok(0);
        };
        let key = at.volume.key.spelled();
        let Some(over) = budget::over(&sizing, cap, &key, at.volume.free) else {
            return Ok(0);
        };
        let Some(page) = self.run.next(sizing.sized_unix) else {
            return Ok(0);
        };
        if page == 1 {
            self.cursor.reset_cycle();
        }
        let snap = Snapshot::read();
        let live = budget::Live::read(snap.sids, snap.sessions.cwds, snap.sessions.unknown);
        if live.blind() {
            self.run.ran(sizing.sized_unix, page, false);
            writeln!(
                out,
                "disk watch: over the rebuildable budget; no budget pass: what the live sessions \
                 build could not be read whole"
            )?;
            return Ok(0);
        }
        let inputs = budget::Inputs::of(&sizing, &over.budget, live.digest(), sizing.sized.partial);
        if !budget::plans(budget::load(&at.env.state).as_ref(), &inputs, at.now) {
            // Its cannot-meet row is written for this volume and cap, and nothing grew since.
            self.run.ran(sizing.sized_unix, page, false);
            return Ok(0);
        }
        let (level, thresholds, free_target) = (at.level, at.thresholds, over.free_target);
        let measure = || super::volume_of(at.home, at.table).ok();
        let pass_path = reserve::pass_lock_for(at.home);
        // One sweep a run: its walk reaches the whole shared cache, so a later page would walk
        // again only what the first one kept.
        let mut freed = if page == 1 {
            budget_sweep(at, &live, free_target)
        } else {
            0
        };
        let after_sweep = measure();
        let end = if after_sweep.as_ref().is_some_and(|r| r.free >= free_target) {
            // The memo goes under the pass lock, as a page writes it under that lock.
            match at.held.validate_for(&pass_path) {
                Ok(_) => {
                    if let Err(e) = budget::met(&at.env.state) {
                        aterm_log::warn!(
                            "harness disk watch: the budget's memo was not removed: {e}"
                        );
                    }
                    cli::BudgetEnd::Met
                }
                Err(e) => cli::BudgetEnd::StoodDown(e.to_string()),
            }
        } else {
            // The sweep may have walked for minutes: what keeps a session's builds is read again.
            let snap = Snapshot::read();
            let live = budget::Live::read(
                snap.sids.clone(),
                snap.sessions.cwds.clone(),
                snap.sessions.unknown.clone(),
            );
            if live.blind() {
                let why = "what the live sessions build could not be read whole".to_owned();
                cli::BudgetEnd::StoodDown(why)
            } else {
                let roots = search_roots(at.home, at.table, &snap);
                let transcripts = at.home.join(".claude/projects");
                let links = cli::default_link_dirs(at.home);
                let look = DiskLook {
                    state: &at.env.state,
                    volume: at.home,
                    free: after_sweep.as_ref().map(|r| r.free),
                    targets: &[],
                    discover: Some(&roots),
                    transcripts: Some(&transcripts),
                    now: at.now,
                    room: Some(at.room),
                    table: Some(at.table),
                    demand: None,
                    projected: None,
                    session_cwds: &live.cwds,
                    session_cwd_unknown: live.cwd_unknown.as_deref(),
                    link_dirs: &links,
                };
                let judge = super::Judge {
                    now: at.now,
                    threshold_days: at.config.target_stale_days,
                    device: discover::device_of(at.home),
                    volume: super::target::VolumeGuid::of_path(at.home, at.table),
                };
                let mut actual_remove = remover(&at.env.state, judge);
                let mut remove = |row: &super::Row| {
                    at.held.validate_for(&pass_path)?;
                    actual_remove(row)
                };
                let mut take = || {
                    at.held
                        .validate_for(&pass_path)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                };
                let mut gated = || {
                    measure()
                        .filter(|r| thresholds.level(Some(r.free), r.files_free) <= level)
                        .map(|r| r.free)
                };
                let page_of = cli::BudgetLook {
                    sizing: &sizing,
                    over,
                    live: &live,
                    page,
                };
                let tick = cli::disk_tick_budget(
                    &look,
                    at.config,
                    &page_of,
                    &mut take,
                    &mut remove,
                    &mut gated,
                    &mut self.cursor,
                );
                if let DiskTick::Reclaimed(done, _) = &tick.tick {
                    freed = freed.saturating_add(done.freed_bytes);
                }
                tick.end
            }
        };
        self.run.ran(
            sizing.sized_unix,
            page,
            matches!(end, cli::BudgetEnd::Unfinished),
        );
        writeln!(
            out,
            "disk watch: budget pass page {page}; {} accounted reclaimed; {}",
            super::human_bytes(freed),
            end.describe()
        )?;
        Ok(freed)
    }

    /// Begin a sizing on a thread of its own ([`size_budget`]): no look waits for it.
    fn size(&mut self, at: &BudgetAt<'_>) {
        self.began = Some(at.now);
        self.flight.store(true, Ordering::SeqCst);
        let landed = Landed(Arc::clone(&self.flight));
        let sizing = Arc::clone(&self.sizing);
        let (state, home, table, now) = (
            at.env.state.clone(),
            at.home.to_path_buf(),
            at.table.clone(),
            at.now,
        );
        let spawned = std::thread::Builder::new()
            .name("aterm-harness-disk-budget".to_owned())
            .spawn(move || {
                let _landed = landed;
                if let Some(sized) = size_budget(&state, &home, &table, now) {
                    *sizing.lock().unwrap_or_else(PoisonError::into_inner) = Some(sized);
                }
            });
        if let Err(e) = spawned {
            self.flight.store(false, Ordering::SeqCst);
            aterm_log::warn!(
                "harness disk watch: the rebuildable budget's sizing could not start: {e}"
            );
        }
    }
}

/// A budget page's sweep of the shared build cache (`atpkg::build_cache::reclaim_over_budget`):
/// the parts no `live` session's builds used, unwritten an hour, each removal journalled and
/// asked first of the shortfall (what the entries put on record so far were sized at, against
/// what free space was short of `free_target` before the sweep: a snapshot that holds the blocks
/// keeps free space from moving), of `at.held`, and of a measure that reads the volume short of
/// `free_target` and no worse than `at.level`. The bytes it accounted freed.
fn budget_sweep(at: &BudgetAt<'_>, live: &budget::Live, free_target: u64) -> u64 {
    let measure = || super::volume_of(at.home, at.table).ok();
    let pass_path = reserve::pass_lock_for(at.home);
    let short = measure().map_or(0, |r| free_target.saturating_sub(r.free));
    let keeps = |leaf: &Path, workspace: &Path| {
        budget::keeps_leaf(live, leaf, workspace, Some(at.table), at.now)
    };
    let swept = cli::disk_sweep_journalled_counted(&at.env.state, at.now, |record, recorded| {
        atpkg::build_cache::reclaim_over_budget(
            at.home,
            SystemTime::now(),
            &keeps,
            at.placing,
            &mut |path, why| {
                recorded() < short
                    && at.held.validate_for(&pass_path).is_ok()
                    && measure().is_some_and(|r| {
                        r.free < free_target
                            && at.thresholds.level(Some(r.free), r.files_free) <= at.level
                    })
                    && record(path, why)
            },
        )
    });
    swept.freed()
}

/// One sizing ([`BudgetWatch::size`]'s thread): claim it in the record every process of the
/// account shares (where another began one within the hour, that record's last sizing), then
/// the rebuildable bytes a search from the sessions and `home` finds on its volume within
/// [`budget::WALL`] ([`cli::disk_rebuildable`]), against the volume's used bytes once the walk
/// is done, kept in the record. It removes and journals nothing and takes no build's lock.
fn size_budget(
    state: &Path,
    home: &Path,
    table: &aterm_sysprobe::volumes::Table,
    now: i64,
) -> Option<budget::Sizing> {
    match budget::claim(state, now) {
        Ok(budget::Claim::Claimed(_)) => {}
        Ok(budget::Claim::NotDue(record)) => return record.last,
        Err(e) => {
            aterm_log::warn!("harness disk watch: the rebuildable budget was not sized: {e}");
            return None;
        }
    }
    let deadline = Instant::now() + budget::WALL;
    let snapshot = Snapshot::read();
    let roots = search_roots(home, table, &snapshot);
    let sized = cli::disk_rebuildable(home, &roots, Some(table), deadline);
    let reading = super::volume_of(home, table).ok()?;
    let sizing = budget::Sizing {
        volume: reading.key.spelled(),
        total: reading.total,
        used: reading.total.saturating_sub(reading.free),
        sized,
        sized_unix: aterm_time::unix_secs()
            .and_then(|s| i64::try_from(s).ok())
            .unwrap_or(now),
    };
    if let Err(e) = budget::keep(state, now, sizing.clone()) {
        aterm_log::warn!("harness disk watch: the rebuildable budget's record was not kept: {e}");
    }
    Some(sizing)
}

/// Where a search for build directories starts ([`discover::discover`]): the sessions' working
/// directories and their agents' `CARGO_TARGET_DIR`s (`snapshot`), `home`'s checkouts, and
/// Claude Code's own folders and temp root, on `home`'s volume as `table` places it.
fn search_roots(
    home: &Path,
    table: &aterm_sysprobe::volumes::Table,
    snapshot: &Snapshot,
) -> discover::Roots {
    let (running, claude_unread) = match discover::running_claude_census(Some(home)) {
        Ok(running) => (running, None),
        Err(why) => (discover::Running::default(), Some(why)),
    };
    discover::Roots {
        cwds: snapshot.sessions.cwds.clone(),
        home: Some(home.to_path_buf()),
        claude_dirs: discover::claude_dirs(Some(home), running.dirs),
        claudes: running.claudes,
        claude_unread,
        agent_target_dirs: snapshot.targets.clone(),
        claude_tmp: Some(discover::claude_tmp_root(discover::own_uid())),
        device: discover::device_of(home),
        volume: super::target::VolumeGuid::of_path(home, table),
        mounts: table.points(),
        skip_home_children: false,
    }
}

/// A pass's remover ([`super::remover`]). Every tier-3 removal refreshes the actual
/// reported-session census. Once a read fails, retain that unknown for this pass instead of
/// repeatedly timing out.
fn remover(
    state: &Path,
    judge: super::Judge,
) -> impl FnMut(&super::Row) -> io::Result<super::Removed> {
    let unknown: RefCell<Option<String>> = RefCell::new(None);
    super::remover(
        judge,
        Some(crate::harness::first_exec::holds_dir(state)),
        move || {
            if let Some(why) = &*unknown.borrow() {
                return idle::Sessions {
                    cwds: Vec::new(),
                    unknown: Some(why.clone()),
                };
            }
            let sessions = Snapshot::read().sessions;
            unknown.borrow_mut().clone_from(&sessions.unknown);
            sessions
        },
    )
}

#[derive(Default)]
struct Snapshot {
    sessions: idle::Sessions,
    targets: Vec<PathBuf>,
    /// The id of each session the census read.
    sids: Vec<String>,
}

impl Snapshot {
    /// The live sessions' digest ([`super::budget::live_digest`]).
    fn live(&self) -> u64 {
        super::budget::live_digest(
            self.sids.iter().map(String::as_str),
            self.sessions.cwds.iter().map(PathBuf::as_path),
        )
    }

    fn read() -> Self {
        let deadline = Instant::now() + CENSUS_WAIT;
        match Self::read_until(deadline) {
            Ok(snapshot) => snapshot,
            Err(why) => Self {
                sessions: idle::Sessions {
                    cwds: Vec::new(),
                    unknown: Some(why),
                },
                targets: Vec::new(),
                sids: Vec::new(),
            },
        }
    }

    fn read_until(deadline: Instant) -> Result<Self, String> {
        if aterm_uds::control_socket_dir_is_private() {
            return Err("session discovery namespace is private".into());
        }
        let instances = aterm_ctl::local_instances_complete().map_err(|e| e.reason)?;
        let mut snapshot = Self::default();
        for (_, socket) in instances {
            let remaining = || {
                deadline
                    .checked_duration_since(Instant::now())
                    .filter(|d| !d.is_zero())
                    .ok_or_else(|| "session census deadline expired".to_owned())
            };
            // Machine discovery names instance sockets, not a mutable latest alias. Bound
            // opening too: harness_client_within bounds requests, not its initial connect.
            use std::os::unix::fs::FileTypeExt as _;
            if !std::fs::symlink_metadata(&socket)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_socket()
            {
                return Err("session instance path is not a real socket".into());
            }
            let stream = aterm_uds::connect::until(Path::new(&socket), deadline)
                .map_err(|e| e.to_string())?;
            let token = aterm_ctl::read_token_beside(&socket).map_err(|e| e.to_string())?;
            stream
                .set_write_timeout(Some(remaining()?))
                .map_err(|e| e.to_string())?;
            let mut client = crate::RelayClient::authenticated(stream, &token)
                .map_err(|e| e.to_string())?
                .reads_only();
            client.bound_by(remaining()?);
            let (head, body) = client.request_counted("who").map_err(|e| e.to_string())?;
            let rows = crate::harness::upgrade_drive::roster_rows(&body)
                .ok_or("invalid session roster")?;
            if head.split_whitespace().count() != 2
                || crate::reply_count(&head).map_err(|e| e.to_string())? != Some(rows.len())
            {
                return Err("incomplete session roster".into());
            }
            for (_, tab) in rows {
                client.bound_by(remaining()?);
                let reply = client
                    .request_line(&format!("@{} cwd", tab.sid))
                    .map_err(|e| e.to_string())?;
                let cwd =
                    reported_cwd(&reply).ok_or("session reported no absolute working directory")?;
                if !snapshot.sessions.cwds.contains(&cwd) {
                    snapshot.sessions.cwds.push(cwd);
                }
                snapshot.sids.push(tab.sid.clone());
                if let Some(pid) = tab
                    .fgpgid
                    .and_then(|p| u32::try_from(p).ok())
                    .filter(|pid| *pid > 0)
                    && let Ok(native_pid) = i32::try_from(pid)
                    && aterm_uds::process::thread_ran(native_pid) == Some(true)
                    && let Some(args) = atpkg::caller_shell::process_args(pid)
                    && let Some(path) = args
                        .env_var("CARGO_TARGET_DIR")
                        .map(PathBuf::from)
                        .filter(|p| p.is_absolute())
                    && !snapshot.targets.contains(&path)
                {
                    snapshot.targets.push(path);
                }
            }
        }
        snapshot.sessions.cwds.sort();
        snapshot.sessions.cwds.dedup();
        snapshot.targets.sort();
        snapshot.targets.dedup();
        snapshot.sids.sort();
        snapshot.sids.dedup();
        Ok(snapshot)
    }
}

fn reported_cwd(reply: &str) -> Option<PathBuf> {
    let raw = reply.strip_prefix("OK ")?;
    let decoded = aterm_codec::pct::decode(raw);
    (aterm_codec::pct::encode(&decoded) == raw && !decoded.contains('\0'))
        .then(|| PathBuf::from(decoded))
        .filter(|p| p.is_absolute())
}

fn term_is_default() -> io::Result<()> {
    unsafe extern "C" {
        // Darwin's signal.h declaration; the generated libc surface omits this member.
        fn sigismember(set: *const libc::sigset_t, signal: libc::c_int) -> libc::c_int;
    }
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::zeroed();
    let mut mask = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
    // SAFETY: both calls only read the process disposition/calling thread's mask into live
    // correctly sized storage. No disposition or mask is changed.
    let read = unsafe {
        libc::sigaction(libc::SIGTERM, std::ptr::null(), action.as_mut_ptr()) == 0
            && libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), mask.as_mut_ptr()) == 0
    };
    if !read {
        return Err(io::Error::other(
            "cannot validate disk watch SIGTERM shutdown",
        ));
    }
    // SAFETY: successful calls initialized both plain native records.
    let (action, mask) = unsafe { (action.assume_init(), mask.assume_init()) };
    // SAFETY: sigismember reads the initialized native set, changing no process state.
    if action.sa_sigaction != libc::SIG_DFL || unsafe { sigismember(&mask, libc::SIGTERM) } != 0 {
        return Err(io::Error::other(
            "disk watch requires default, unblocked SIGTERM",
        ));
    }
    Ok(())
}

/// Run the explicit foreground macOS watch with the actual user's home and pass lock.
/// # Errors
/// Startup home/shutdown validation fails. Individual look errors defer and are reported.
pub fn run(env: &Env, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    let home = aterm_types::dirs::home_dir();
    let Some(home) = home.filter(|home| home.is_absolute() && env.home.as_ref() == Some(home))
    else {
        let _ = writeln!(err, "aterm harness disk watch: actual home unavailable");
        return ExitCode::from(2);
    };
    if let Err(error) = term_is_default() {
        let _ = writeln!(err, "aterm harness disk watch: {error}");
        return ExitCode::from(2);
    }
    let path = reserve::pass_lock_for(&home);
    let mut waiting = String::new();
    let held = loop {
        let why = match reserve::take_pass(&path) {
            Ok(Some(held)) => break held,
            Ok(None) => "another pass holds the real lock".to_owned(),
            Err(error) => error.to_string(),
        };
        if waiting != why {
            let _ = writeln!(err, "disk watch: awaiting lifetime guard: {why}");
            waiting = why;
        }
        // No cleanup or substitute guard runs in standby, and policy is still re-read.
        let _ = cli::disk_config_read(env.config.as_deref());
        std::thread::sleep(Duration::from_secs(1));
    };
    match held.validate_for(&path) {
        Ok((dev, ino)) => {
            let _ = writeln!(
                out,
                "disk watch: guard acquired pid={} dev={dev} ino={ino} path={}",
                std::process::id(),
                aterm_codec::pct::encode(&path.to_string_lossy())
            );
            let _ = out.flush();
        }
        Err(error) => {
            let _ = writeln!(
                err,
                "disk watch: guard unverified; no admission ACK: {error}"
            );
        }
    }
    let mut watch = Watch::new(&env.state);
    loop {
        if let Err(error) = watch.look(env, &home, &held, out, err) {
            let _ = writeln!(err, "aterm harness disk watch: deferred: {error}");
        }
        std::thread::sleep(LOOK_EVERY);
    }
}

#[cfg(test)]
#[path = "disk_watch_tests.rs"]
mod tests;
