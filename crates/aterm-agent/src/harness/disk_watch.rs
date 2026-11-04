// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Foreground automatic disk maintenance. No helper or vendor process is spawned.
//! Ordinary unblocked SIGTERM ends this process, including an in-progress pass: its kernel
//! closes the lifetime pass lock and the existing write-ahead journal and durable removal stages
//! describe any interrupted work. This is not a bound on uninterruptible filesystem I/O.

use std::cell::RefCell;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{cadence, discover, fill, idle, maintenance, reserve};
use crate::harness::cli::{self, DiskLook, DiskTick, Env};

const LOOK_EVERY: Duration = Duration::from_secs(60);
const CENSUS_WAIT: Duration = Duration::from_secs(5);

struct Watch {
    cursor: discover::Cursor,
    last: Option<cadence::Pass>,
    daily: Mutex<maintenance::Schedule>,
    rooms: super::room::Rooms,
    fill: Option<(String, fill::Fill)>,
    notes: Vec<String>,
}

impl Watch {
    fn new(state: &Path) -> Self {
        Self {
            cursor: discover::Cursor::new(),
            last: None,
            daily: Mutex::new(maintenance::Schedule::new()),
            rooms: super::room::Rooms::load(state),
            fill: None,
            notes: Vec::new(),
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
        if self.rooms.save_due(now) {
            self.rooms.save(&env.state)?;
        }
        let config = reading.on_volume_with(Some(volume.total), Some(room));
        let thresholds = reading.thresholds_with(Some(volume.total), Some(room));
        let level = thresholds.level(Some(volume.free), volume.files_free);
        if config.auto_free_gib == 0 || level == super::Level::Unknown {
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
            *entry = (key, fill::Fill::new());
            self.last = None;
        }
        let fill = &mut entry.1;
        let projected = fill.observe(
            now.saturating_mul(1_000),
            volume.free,
            Some(volume.total),
            thresholds.short_below(),
        );
        let placing = discover::placing_of(home, &table);
        let pass_path = reserve::pass_lock_for(home);
        let started = Instant::now();
        let mut freed = 0u64;
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
            },
        )?;
        let pass_level = if projected.is_some() && level < super::Level::Short {
            super::Level::Short
        } else {
            level
        };
        let due = cadence::due(
            &mut self.last,
            started,
            pass_level,
            Some(volume.free),
            false,
            room,
        );
        // The outer run retains the same real guard through off/standby/errors/sleep. A
        // legacy GUI admission captured before config changed cannot slip between ticks.
        held.validate_for(&pass_path)?;
        let reserve_path = reserve::path_for(home);
        let moved = cli::disk_reserve_look(
            &env.state,
            &reserve_path,
            level,
            Some(volume.free),
            &thresholds,
            now,
        );
        if let cli::ReserveMove::Released(bytes) = moved {
            freed = freed.saturating_add(bytes);
        }
        if !due {
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
        let running = discover::running_claude_census(Some(home));
        let (running, claude_unread) = match running {
            Ok(running) => (running, None),
            Err(why) => (discover::Running::default(), Some(why)),
        };
        let roots = discover::Roots {
            cwds: snapshot.sessions.cwds.clone(),
            home: Some(home.to_path_buf()),
            claude_dirs: discover::claude_dirs(Some(home), running.dirs),
            claudes: running.claudes,
            claude_unread,
            agent_target_dirs: snapshot.targets,
            claude_tmp: Some(discover::claude_tmp_root(discover::own_uid())),
            device: discover::device_of(home),
            volume: super::target::VolumeGuid::of_path(home, &table),
            mounts: table.points(),
            skip_home_children: false,
        };
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
        // Every tier-3 removal refreshes the actual reported-session census. Once a read
        // fails, retain that unknown for this pass instead of repeatedly timing out.
        let unknown: RefCell<Option<String>> = RefCell::new(None);
        let mut actual_remove = super::remover(
            judge,
            Some(crate::harness::first_exec::holds_dir(&env.state)),
            || {
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
        );
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

#[derive(Default)]
struct Snapshot {
    sessions: idle::Sessions,
    targets: Vec<PathBuf>,
}

impl Snapshot {
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
mod tests {
    use super::*;

    #[test]
    fn reported_session_directories_are_actual_or_unknown() {
        assert_eq!(
            reported_cwd("OK /tmp/a%20b"),
            Some(PathBuf::from("/tmp/a b"))
        );
        for bad in [
            "OK ",
            "OK relative",
            "ERR busy",
            "OK /tmp/%ZZ",
            "OK /tmp/%FF",
            "OK /tmp/%00",
        ] {
            assert_eq!(reported_cwd(bad), None, "{bad}");
        }
    }
}
