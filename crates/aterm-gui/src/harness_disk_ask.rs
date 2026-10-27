// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's side of the ask (`disk-free@v1`, [`aterm_agent::harness::disk::ask`],
//! `docs/DESIGN-disk-free-ask-2026-10-24.md`): after each disk look ([`super::disk_look`]) this
//! process steps the machine-wide lease with its own sessions, journals what it decided, keeps
//! the line owed one of its sessions on the board for that session's worker
//! ([`super::WorkerIdle`]), and says on the owner's disk line what the asks did.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use aterm_agent::harness::disk::{self, Hog, Level, ask};
use aterm_agent::supervise::Guard;
use aterm_phase::Program;

use crate::task_record::TaskState;

use super::{HostNote, NoteFn, Store, agent_of, harness_state};

/// What the disk look read that the ask needs ([`super::disk_look`]).
#[derive(Debug, Default)]
pub(super) struct Looked {
    /// The thresholds in force on the home volume; `None`: the look stopped before it read them.
    pub(super) thresholds: Option<disk::Thresholds>,
    /// The look ran in a private state root ([`super::disk_private`]): this process takes no
    /// part in the ask, as it removes nothing.
    pub(super) private: bool,
    /// Why aterm's own freeing is done, where the look itself said (`[disk]` turned removal off,
    /// or the volume is out of inodes).
    pub(super) done: Option<ask::Done>,
    /// Another instance's pass held the pass lock: it may be freeing space now.
    pub(super) contended: bool,
    /// What this look's own pass, and a reserve it released, freed, each with when it ended: the
    /// step records it in the lease ([`ask::Look::reclaimed`]), so no asked session's rise counts
    /// it.
    pub(super) reclaimed: Vec<ask::Reclaimed>,
    /// The pass lock this look's pass took, held until the step has recorded what the pass freed
    /// ([`after_look`]): another instance's look meanwhile reads a pass running
    /// ([`ask::Look::passing`]), never the space it freed unrecorded.
    pub(super) pass_lock: Option<disk::reserve::PassLock>,
}

/// This process's passes the lease has yet to record, by volume key: a step the lease's lock kept
/// from landing records them at the next.
static UNRECORDED_PASSES: Mutex<Vec<(String, ask::Reclaimed)>> = Mutex::new(Vec::new());

/// This process's passes on `key` the lease has yet to record, `this` look's added, oldest first:
/// at most [`ask::MAX_RECLAIMED`], past it the two oldest kept as one, as the lease keeps them.
fn passes_unrecorded(key: &str, this: &[ask::Reclaimed]) -> Vec<ask::Reclaimed> {
    let mut all = UNRECORDED_PASSES
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    all.extend(this.iter().map(|r| (key.to_owned(), *r)));
    let mut mine: Vec<ask::Reclaimed> = all
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, r)| *r)
        .collect();
    mine.sort_by_key(|r| r.ended_ms);
    while mine.len() > ask::MAX_RECLAIMED {
        let first = mine.remove(0);
        mine[0].bytes = mine[0].bytes.saturating_add(first.bytes);
    }
    all.retain(|(k, _)| k != key);
    all.extend(mine.iter().map(|r| (key.to_owned(), *r)));
    mine
}

/// The lease recorded `recorded` of this process's passes on `key` (or has no episode open to
/// record them in): they are no longer this process's to report.
fn passes_recorded(key: &str, recorded: &[ask::Reclaimed]) {
    UNRECORDED_PASSES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|(k, r)| k != key || !recorded.contains(r));
}

/// Whether another instance's pass holds the pass lock of `home`'s volume now: it may have freed
/// space this look read that its host's step has yet to record.
fn another_pass_runs(home: &Path) -> bool {
    let path = disk::reserve::pass_lock_for(home);
    match disk::reserve::take_pass(&path) {
        Ok(Some(_free)) => false,
        Ok(None) => true,
        Err(e) => {
            aterm_log::debug!(
                "harness disk watch: the pass lock {} could not be read ({e}); the ask's step \
                 takes no pass to be running",
                path.display()
            );
            false
        }
    }
}

/// One of this process's sessions the ask could go to, and the checkout it works in, of those
/// the pass's holders might serve ([`checkout_of`]).
struct Seated {
    seat: ask::Seat,
    checkout: Option<PathBuf>,
    /// Another of this process's live sessions, supervised or not, works in that same checkout
    /// ([`rule`]): its builds are not this tab's alone, and its line says so
    /// ([`ask::Line::shared`]).
    shared: bool,
}

/// The last word each volume's step said here, so a standing state is journalled once.
static SAID: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// What the owner's disk line says of the ask now ([`ask::owner_line`]), as this process's
/// last step read the lease.
static OWNER: Mutex<Option<String>> = Mutex::new(None);

/// What the owner's disk line says of the ask now: [`HostNote::DiskLow`]'s `asked`.
pub(super) fn owner_said() -> Option<String> {
    OWNER.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// What this process's last disk-worker spawn did, until a step records it
/// ([`ask::Ahead::spawned`]).
static SPAWNED: Mutex<Option<ask::Spawned>> = Mutex::new(None);

/// The main thread's answer to [`HostNote::DiskWorker`]: the session it started, or why none
/// started. The next disk look's step records it in the lease.
pub(crate) fn worker_spawned(outcome: Result<String, String>) {
    *SPAWNED.lock().unwrap_or_else(PoisonError::into_inner) = Some(ask::Spawned {
        at_ms: now_ms(),
        outcome,
    });
}

impl crate::App {
    /// Start the home volume's disk worker (`task`, in `folder`) the way Summarize starts its
    /// worker: a Claude Code tab in the background under Autopilot Land, with no starting session
    /// and no fact line, its trusted folder the worker folder, so the harness's disk-worker rule
    /// fences its boxes. What the spawn did goes to the next disk look ([`worker_spawned`]).
    pub(crate) fn start_disk_worker(&mut self, task: String, folder: &Path) {
        let order = crate::task_launch::TaskOrder {
            task: task.into_bytes(),
            agent: Program::Claude,
            model: None,
            caller: None,
            trusted_cwd: folder
                .to_str()
                .and_then(crate::spawn_folder::canonical_folder),
            fact_line: false,
            autopilot: aterm_agent::supervise::Autopilot::Land,
        };
        let booking = crate::control::control_media::SpawnBooking {
            cwd: folder.to_str().map(str::to_string),
            split: None,
            raise: false,
            admission: crate::tab_spawn::SpawnAdmission::Plain,
        };
        let plan = match self.resolve_spawn(crate::control::SpawnAim::Front, Some(order)) {
            Ok(plan) => plan,
            Err(why) => {
                aterm_log::warn!("harness disk watch: the disk worker did not start: {why}");
                worker_spawned(Err(spawn_word(&why)));
                return;
            }
        };
        self.book_spawn(
            plan,
            booking,
            Box::new(|app, spawned| {
                match spawned {
                    Ok(spawned) => {
                        let sid = app.start_spawned(spawned);
                        aterm_log::info!("harness disk watch: the disk worker runs as @{sid}");
                        worker_spawned(Ok(sid));
                    }
                    Err(why) => {
                        aterm_log::warn!(
                            "harness disk watch: the disk worker did not start: {why}"
                        );
                        worker_spawned(Err(spawn_word(&why)));
                    }
                }
                app.mark_status_dirty();
            }),
        );
    }

    /// The disk worker `sid` ran out of its time ([`ask::WORKER_MS`]): its Autopilot goes Off,
    /// written by aterm, so Land types nothing more into it.
    pub(crate) fn disk_worker_expired(&mut self, sid: &str) {
        let local = {
            let store = self.store.read().unwrap_or_else(PoisonError::into_inner);
            store
                .live_handles()
                .find(|h| h.sid.as_str() == sid)
                .map(|h| h.local_id)
        };
        let Some(local) = local else {
            return;
        };
        self.write_autopilot_batch(
            &[(local, Some(aterm_agent::supervise::Autopilot::Off), None)],
            aterm_slog::Actor::Aterm,
            None,
        );
        aterm_log::info!(
            "harness disk watch: the disk worker @{sid} ran out of its time; its Autopilot is Off"
        );
    }
}

/// Why a spawn failed, as the lease's `spawn:<reason>` says it: its plain words, clipped.
fn spawn_word(why: &str) -> String {
    let plain: String = why
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let mut word = plain
        .split('-')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    word.truncate(48);
    if word.is_empty() {
        "unknown".to_owned()
    } else {
        word
    }
}

/// Session `sid`'s task record, where `sid` is a live session of this process.
fn task_record_of(store: &Store, sid: &str) -> Option<crate::task_record::TaskRecord> {
    let handle = {
        let g = store.read().unwrap_or_else(PoisonError::into_inner);
        g.live_handles().find(|h| h.sid.as_str() == sid).cloned()
    }?;
    let t = handle
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    t.task().cloned()
}

/// What the last disk worker of this process reported, by volume key: its `disk-free filling:`
/// and `disk-free owner-only:` lines ([`report_lines`]), which the owner's filling entry and
/// alert show as its words.
static WORKER_REPORT: Mutex<Option<(String, Vec<String>)>> = Mutex::new(None);

/// The heads of the lines a disk worker's report ends with ([`ask::worker_task`]).
const REPORT_HEADS: [&str; 2] = ["disk-free filling:", "disk-free owner-only:"];

/// The longest a report line is shown, in characters.
const REPORT_LINE_CAP: usize = 160;

/// The largest result read for a worker's report.
const REPORT_MAX_BYTES: u64 = 256 * 1024;

/// The worker `sid`'s report lines, from its task record's result.
fn worker_report(store: &Store, sid: &str) -> Vec<String> {
    use std::io::Read as _;
    let Some(path) = task_record_of(store, sid).and_then(|r| r.result_path) else {
        return Vec::new();
    };
    let mut text = String::new();
    let read =
        std::fs::File::open(&path).and_then(|f| f.take(REPORT_MAX_BYTES).read_to_string(&mut text));
    if let Err(e) = read {
        aterm_log::debug!(
            "harness disk watch: the disk worker's result {} could not be read ({e})",
            path.display()
        );
        return Vec::new();
    }
    report_lines(&text)
}

/// The last line under each of [`REPORT_HEADS`] in `text`, read, not trusted: kept to plain
/// characters (letters, digits, spaces and `/._-~+@:,=()'%`), its spaces collapsed, and clipped
/// to [`REPORT_LINE_CAP`], so nothing planted in a result reaches the owner's glass as more than
/// words.
fn report_lines(text: &str) -> Vec<String> {
    REPORT_HEADS
        .iter()
        .filter_map(|head| {
            let said = text.lines().rev().find_map(|l| {
                l.trim()
                    .trim_start_matches(['-', '*', ' '])
                    .strip_prefix(head)
            })?;
            let plain: String = said
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || " /._-~+@:,=()'%".contains(c) {
                        c
                    } else {
                        ' '
                    }
                })
                .collect();
            let words: String = plain
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(REPORT_LINE_CAP)
                .collect();
            (!words.is_empty()).then(|| format!("{head} {words}"))
        })
        .collect()
}

/// The last [`HostNote::DiskFilling`] this process sent, so a look says it only when it changed.
static FILLING: Mutex<Option<HostNote>> = Mutex::new(None);

/// The folder a disk worker starts in, under the harness state `state`, made owner-only
/// ([`ask::WORKER_FOLDER`]).
fn worker_folder(state: &Path) -> Option<PathBuf> {
    let dir = state.join(ask::WORKER_FOLDER);
    std::fs::create_dir_all(&dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    Some(dir)
}

/// What session `sid`'s task record says, for the disk worker this process runs
/// ([`ask::Ahead::task`]): `None` where no live session of this process is `sid`, or it has no
/// task record.
fn task_seen(store: &Store, sid: &str) -> Option<ask::TaskSeen> {
    let record = task_record_of(store, sid)?;
    Some(match record.state {
        TaskState::Working => ask::TaskSeen::Working,
        TaskState::Done | TaskState::Exited => ask::TaskSeen::Ended,
        TaskState::Stalled => ask::TaskSeen::Stalled(record.why.unwrap_or("stalled").to_owned()),
    })
}

/// The unix ms now.
fn now_ms() -> i64 {
    aterm_time::unix_millis()
        .and_then(|ms| i64::try_from(ms).ok())
        .unwrap_or(0)
}

/// As a look begins, before its pass: take over what a host that ended left in the home volume's
/// open episode of this process's sessions `sids` (the supervised ones, Autopilot not Off), a
/// seamless update's predecessor's pick, nominee and held records among them
/// ([`ask::take_over`]). It reads only what the pass does not change: the volume's key, the lease
/// and this process's seat rule. A look's pass can run a minute or more (discovery, profile
/// walks), and the step after it ([`after_look`]) was the first a successor adopted at, so
/// another host's look while its first pass ran, [`ask::ADOPT_MS`] after it first read the
/// predecessor ended, handed the pick off as undelivered, or dropped its nominee. The lease is
/// read first without its lock, and its lock taken only when there is something to take over; a
/// contended lock leaves it to the step after the pass, which adopts as well.
pub(super) fn before_pass(sids: &[String], store: &Store) {
    let (Some(home), Some(state)) = (aterm_primer::home_dir(), harness_state()) else {
        return;
    };
    if super::disk_private(&home).is_some() {
        return;
    }
    let Ok(reading) = disk::volume_of(&home, &aterm_sysprobe::volumes::table()) else {
        return;
    };
    let key = reading.key.spelled();
    let host = ask::HostId::this();
    let lease = ask::read(&state);
    let open = lease.entry(&key).is_some_and(|e| e.episode_ms.is_some());
    if !open {
        return;
    }
    let Rule {
        barred, live, held, ..
    } = rule(store, sids, &key, &Left::NONE, &DRAFTS, false);
    let alive = |h: &ask::HostId| h.alive();
    let roster = ask::Roster {
        host: &host,
        key: &key,
        supervised: sids,
        barred: &barred,
        live: &live,
        held: &held,
        alive: &alive,
    };
    if ask::take_over(&mut lease.clone(), &roster).is_empty() {
        return;
    }
    match ask::transact(&state, &ask::LOCK_LADDER, |l| ask::take_over(l, &roster)) {
        Ok(Some(took)) if !took.is_empty() => aterm_log::info!(
            "harness disk watch: {} took over {} from a host that ended, before this look's pass",
            ask::RULE_DISK_FREE,
            took.iter()
                .map(|s| format!("@{s}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Ok(Some(_)) => {}
        Ok(None) => aterm_log::debug!(
            "harness disk watch: {} is held; the ask's take-over waits for the step after the pass",
            state.join(ask::LOCK_FILE).display()
        ),
        Err(e) => aterm_log::warn!(
            "harness disk watch: the ask's lease in {} could not take over before the pass ({e}); \
             the step after the pass adopts",
            state.display()
        ),
    }
}

/// After a look: step the home volume's lease with this process's sessions `sids` (the
/// supervised ones, Autopilot not Off), as [`Looked`] says the look went. The look judges the
/// volume short against this process's own floor ([`disk::Thresholds::short_below`]); the lease
/// holds an open episode to the floor it recorded ([`ask::Entry::floor`]), so a look here that
/// reads it clear of this process's floor steps the episode on until it clears that one too, and
/// records the floor it held it to, which the line and the owner's note then say. A look that
/// reads the volume clear takes no lock only when the lease has no episode open there and holds
/// every line this process typed on it ([`ask::rests`]): a line whose record a full disk kept from
/// the lease, and whose episode another host then ended as the space came back, is still recorded
/// ([`ask::typed_unrecorded`]), so its cooldown starts and the next shortage types no second line
/// within it.
pub(super) fn after_look(looked: &Looked, sids: &[String], store: &Store, note: &NoteFn) {
    let Some(thresholds) = looked.thresholds else {
        return;
    };
    if looked.private {
        return;
    }
    let (Some(home), Some(state)) = (aterm_primer::home_dir(), harness_state()) else {
        return;
    };
    let table = aterm_sysprobe::volumes::table();
    let Ok(reading) = disk::volume_of(&home, &table) else {
        return;
    };
    let level = thresholds.level(Some(reading.free), reading.files_free);
    let key = reading.key.spelled();
    let host = ask::HostId::this();
    let now = now_ms();
    let reclaimed = passes_unrecorded(&key, &looked.reclaimed);
    let floor_now = thresholds.short_below();
    let (projected, cold) = super::disk_fill_ask(&key, reading.free, floor_now, reading.total);
    if look_rests(&state, &key, level, projected.is_some()) {
        // At or above the floor with no episode open, and no line of this process's that the
        // lease has yet to record: nothing to step, no lock to take, and no line a pass's space
        // could be credited to.
        passes_recorded(&key, &reclaimed);
        crate::disk_board::publish(|b| {
            b.asks.retain(|_, o| o.key != key);
            b.unfit.retain(|sid, _| sids.contains(sid));
            b.walls.retain(|sid, _| sids.contains(sid));
            b.withholds.retain(|sid, _| sids.contains(sid));
        });
        *OWNER.lock().unwrap_or_else(PoisonError::into_inner) = None;
        journal(
            &state,
            now,
            &key,
            &ask::Said::Quiet,
            level,
            reading.free,
            &host,
        );
        say_filling(&FILLING, note, None);
        return;
    }
    let (ran_out, free_after, holders) = {
        let b = crate::disk_board::SHELF.board_lock();
        let pass = b.last_pass.as_ref().filter(|p| p.key == reading.key);
        (
            pass.is_some_and(|p| p.ran_out),
            pass.and_then(|p| p.free_after),
            pass.map(|p| p.left.clone()).unwrap_or_default(),
        )
    };
    // Done when the last pass ran out and left the volume short, or with a crossing still
    // projected from the free space it left: a pass that freed the horizon's fill succeeded.
    let spent = ran_out
        && (thresholds.level(free_after, None) >= Level::Short
            || free_after.is_some_and(|f| {
                super::disk_fill_ask(&key, f, floor_now, reading.total)
                    .0
                    .is_some()
            }));
    let done = if looked.contended {
        None
    } else {
        looked.done.or(spent.then_some(ask::Done::Spent))
    };
    let Rule {
        seated,
        barred,
        live,
        held,
    } = rule(
        store,
        sids,
        &key,
        &Left {
            holders: &holders,
            home: Some(&home),
            root: &checkout_root,
            there: &dir_there,
        },
        &DRAFTS,
        true,
    );
    let seats: Vec<ask::Seat> = seated.iter().map(|s| s.seat.clone()).collect();
    let alive = |h: &ask::HostId| h.alive();
    // Read after the volume: a pass that freed space this look read has either been recorded by
    // its host's step, which it makes before it lets the lock go, or holds the lock still.
    let passing = looked.pass_lock.is_none() && another_pass_runs(&home);
    let spawned = SPAWNED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let task = |sid: &str| task_seen(store, sid);
    let look = ask::Look {
        now_ms: now,
        host: &host,
        key: &key,
        level,
        free: reading.free,
        floor: thresholds.short_below(),
        files_free: reading.files_free,
        files_floor: thresholds.files_emergency,
        done,
        seats: &seats,
        supervised: sids,
        barred: &barred,
        live: &live,
        held: &held,
        reclaimed: &reclaimed,
        passing,
        alive: &alive,
        ahead: ask::Ahead {
            projected,
            cold,
            worker_ok: true,
            task: Some(&task),
            spawned: spawned.as_ref(),
        },
    };
    let unrecorded = typed_last(&key);
    let stepped = ask::transact(&state, &ask::LOCK_LADDER, |lease| {
        if let Some(t) = &unrecorded {
            ask::typed_unrecorded(lease, &key, &host, &t.sid, t.at_ms, t.free);
        }
        let said = ask::step(lease, &look);
        let worker = ask::worker_step(lease, &look);
        (said, worker, lease.entry(&key).cloned())
    });
    let (said, worker, entry) = match stepped {
        Ok(Some(stepped)) => {
            passes_recorded(&key, &reclaimed);
            stepped
        }
        Ok(None) => {
            aterm_log::debug!(
                "harness disk watch: {} is held; the ask steps at the next look",
                state.join(ask::LOCK_FILE).display()
            );
            return;
        }
        Err(e) => {
            aterm_log::warn!(
                "harness disk watch: the ask's lease in {} could not be stepped ({e}); it \
                 steps at the next look",
                state.display()
            );
            return;
        }
    };
    journal(&state, now, &key, &said, level, reading.free, &host);
    // The spawn's outcome is the lease's once no claim of this host's waits on it.
    let claimed_here = entry
        .as_ref()
        .and_then(|e| e.worker.as_ref())
        .is_some_and(|w| w.host == host && matches!(w.state, ask::WorkerState::Claimed));
    if !claimed_here {
        *SPAWNED.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
    if let Some(w) = &worker {
        worker_moved(&state, now, &key, w, &reading, &host, (note, store), || {
            ask::worker_task(&ask::Brief {
                mount: &reading.mount,
                total: reading.total,
                free: reading.free,
                floor: entry
                    .as_ref()
                    .and_then(|e| e.floor.as_ref())
                    .map_or(floor_now, |f| f.bytes),
                projected,
                target: floor_now
                    .saturating_add(disk::PRESSURE_MARGIN_GIB << 30)
                    .saturating_add(projected.map_or(0, |p| p.need)),
                holders: &holders,
                asked: entry
                    .as_ref()
                    .and_then(|e| ask::owner_line(e, now))
                    .as_deref(),
                home: Some(&home),
            })
        });
    }
    let owed_here = entry.as_ref().and_then(|e| match &e.phase {
        ask::Phase::Owed { pick, .. } if pick.host == host => Some(pick.sid.clone()),
        _ => None,
    });
    let owed = owed_here.and_then(|sid| {
        let s = seated.iter().find(|s| s.seat.sid == sid)?;
        let text = ask::line(&ask::Line {
            mount: &reading.mount,
            total: reading.total,
            free: reading.free,
            // The floor this look held the episode to ([`ask::Entry::floor`]).
            floor: if level == Level::Emergency {
                thresholds.emergency
            } else {
                entry
                    .as_ref()
                    .and_then(|e| e.floor.as_ref())
                    .map_or(thresholds.short_below(), |f| f.bytes)
            },
            emergency: level == Level::Emergency,
            done: entry
                .as_ref()
                .and_then(|e| e.done)
                .or(done)
                .unwrap_or(ask::Done::Spent),
            holders: &holders,
            checkout: s.checkout.as_deref(),
            shared: s.shared,
            home: Some(&home),
            projected,
        });
        Some((sid, text))
    });
    crate::disk_board::publish(|b| {
        b.unfit.retain(|sid, _| sids.contains(sid));
        b.walls.retain(|sid, _| sids.contains(sid));
        b.withholds.retain(|sid, _| sids.contains(sid));
        b.asks
            .retain(|sid, o| o.key != key || owed.as_ref().is_some_and(|(s, _)| s == sid));
        if let Some((sid, text)) = &owed {
            b.asks.insert(
                sid.clone(),
                crate::disk_board::AskOwed {
                    key: key.clone(),
                    text: text.clone(),
                },
            );
        }
    });
    let owner = entry.as_ref().and_then(|e| ask::owner_line(e, now));
    *OWNER.lock().unwrap_or_else(PoisonError::into_inner) = owner.clone();
    // At a measured emergency `system.disk.full` is the owner's alert, as before.
    let needed =
        level != Level::Emergency && ask::owner_needed(entry.as_ref(), &look, thresholds.emergency);
    say_filling(
        &FILLING,
        note,
        (needed || (projected.is_some() && level < Level::Short)).then(|| HostNote::DiskFilling {
            needed,
            projected_only: projected.is_some() && level < Level::Short,
            free: reading.free,
            floor: floor_now,
            rate_bps: projected.map(|p| p.rate_bps),
            eta_s: projected.map(|p| p.eta_ms / 1_000),
            said: filling_said(owner.as_deref(), entry.as_ref(), &report_for(&key)),
        }),
    );
    if out_raises_low(
        &said,
        level,
        thresholds.floor,
        entry.as_ref().and_then(|e| e.done),
    ) {
        let glass: Vec<String> = holders
            .iter()
            .map(|h| h.line_within(Some(&home), crate::message_reporters::DISK_LOW_HOG_CHARS))
            .collect();
        // The floor this look held the episode to: a host whose own floor is met may be the one
        // whose look ran the asks out.
        let floor = entry
            .as_ref()
            .and_then(|e| e.floor.as_ref())
            .map_or(thresholds.floor, |f| f.bytes);
        note(HostNote::DiskLow {
            free_gib: reading.free >> 30,
            floor_gib: floor >> 30,
            hogs: glass,
            asked: owner,
        });
    }
}

/// Whether a look whose step said `said`, reading the volume at `level` against this process's
/// floor `floor` (`[disk] auto_free_gib`, in bytes), raises the owner's `disk.low` line itself
/// with the ask's out sentence: the episode ran out at this look after a spent pass, and this look
/// reads the volume short. The pass's own line is `disk.low`; out of inodes or with removal off,
/// the owner's line is the one the look raises itself (`disk.full`, `disk.low-cleanup-off`), which
/// says [`owner_said`] at each look. A look that reads the volume at or above its own floor (an
/// episode another host's floor holds open, within [`ask::END_MARGIN`]) raises nothing: its
/// `disk.low` said the home volume was still short, the next look found it plenty and cleared it,
/// and the owner kept a false alert's record. The out sentence then goes with the next short
/// look's line ([`owner_said`]).
fn out_raises_low(said: &ask::Said, level: Level, floor: u64, done: Option<ask::Done>) -> bool {
    matches!(said, ask::Said::Out { .. })
        && level >= Level::Short
        && floor > 0
        && done == Some(ask::Done::Spent)
}

/// Whether a look that reads volume `key` at `level` leaves the lease in `state` as it is, its lock
/// untaken ([`after_look`]): the volume is at or above this process's floor, no episode is open
/// there, and the lease holds the last line this process typed on it ([`ask::rests`] of
/// [`typed_last`]). A line whose record a full disk kept from the lease, and whose episode another
/// host then ended as the space came back, so steps the lease at this look, which records it.
fn look_rests(state: &Path, key: &str, level: Level, projected: bool) -> bool {
    level < Level::Short
        && !projected
        && ask::rests(&ask::read(state), key, typed_last(key).map(|t| t.at_ms))
}

/// Journal what a worker step decided, and act on it: a claim of this host's starts the worker
/// on the main thread with the task `task` makes ([`HostNote::DiskWorker`]); a worker of this
/// host's that ran out of its time has its Autopilot turned Off ([`HostNote::DiskWorkerExpired`]).
#[expect(
    clippy::too_many_arguments,
    reason = "one look's figures, each read once by its caller"
)]
fn worker_moved(
    state: &Path,
    now_ms: i64,
    key: &str,
    said: &ask::WorkerSaid,
    reading: &aterm_sysprobe::volumes::Reading,
    host: &ask::HostId,
    (note, store): (&NoteFn, &Store),
    task: impl FnOnce() -> String,
) {
    aterm_log::info!(
        "harness disk watch: {} {}: {}",
        ask::RULE_DISK_FREE,
        said.word(),
        said.describe()
    );
    aterm_agent::harness::cli::disk_ask_journal(
        state,
        &[ask::worker_row(
            now_ms / 1000,
            key,
            said,
            reading.free,
            host,
        )],
    );
    match said {
        ask::WorkerSaid::Claimed => match worker_folder(state) {
            Some(folder) => note(HostNote::DiskWorker {
                task: task(),
                folder,
            }),
            None => worker_spawned(Err("no-folder".to_owned())),
        },
        ask::WorkerSaid::Over {
            sid: Some(sid),
            why,
            here: true,
            ..
        } => {
            let report = worker_report(store, sid);
            if !report.is_empty() {
                *WORKER_REPORT.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some((key.to_owned(), report));
            }
            if why == "expired" {
                note(HostNote::DiskWorkerExpired { sid: sid.clone() });
            }
        }
        ask::WorkerSaid::Started { .. } | ask::WorkerSaid::Over { .. } => {}
    }
}

/// What the owner's filling entry and alert say the asks and the worker did: the owner's line
/// ([`ask::owner_line`]), the worker's state, then what its report said ([`report_lines`]).
/// The last worker report of this process for volume `key` ([`WORKER_REPORT`]).
fn report_for(key: &str) -> Vec<String> {
    WORKER_REPORT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .filter(|(k, _)| k == key)
        .map(|(_, lines)| lines.clone())
        .unwrap_or_default()
}

fn filling_said(
    owner: Option<&str>,
    entry: Option<&ask::Entry>,
    report: &[String],
) -> Option<String> {
    let worker = entry
        .and_then(|e| e.worker.as_ref())
        .map(|w| match &w.state {
            ask::WorkerState::Claimed => "aterm is starting a disk worker".to_owned(),
            ask::WorkerState::Running { sid, .. } => {
                format!("aterm's disk worker @{sid} is freeing space")
            }
            ask::WorkerState::Over { why, rose, .. } => format!(
                "aterm's disk worker is over ({why}; free space rose {})",
                disk::human_bytes(*rose)
            ),
        });
    let said: Vec<String> = owner
        .map(str::to_owned)
        .into_iter()
        .chain(worker)
        .chain(report.iter().map(|l| format!("its worker said {l}")))
        .collect();
    (!said.is_empty()).then(|| said.join("; "))
}

/// Send `told` (a [`HostNote::DiskFilling`]) when it differs from the last one sent (`last`,
/// [`FILLING`]), or [`HostNote::DiskFillingOver`] once, when it is `None` after one.
fn say_filling(last: &Mutex<Option<HostNote>>, note: &NoteFn, told: Option<HostNote>) {
    let mut last = last.lock().unwrap_or_else(PoisonError::into_inner);
    if *last == told {
        return;
    }
    match &told {
        Some(t) => note(t.clone()),
        None => note(HostNote::DiskFillingOver),
    }
    *last = told;
}

/// Journal `said` when it moved the episode, or when its standing word changed here.
fn journal(
    state: &Path,
    now_ms: i64,
    key: &str,
    said: &ask::Said,
    level: Level,
    free: u64,
    host: &ask::HostId,
) {
    let word = format!("{} {}", said.word(), said.sid());
    let changed = {
        let mut seen = SAID.lock().unwrap_or_else(PoisonError::into_inner);
        let before = seen.insert(key.to_owned(), word.clone());
        match before {
            Some(before) => before != word,
            None => !matches!(said, ask::Said::Quiet),
        }
    };
    if !said.moves() && !changed {
        return;
    }
    if said.moves() {
        aterm_log::info!(
            "harness disk watch: {} {}: {}",
            ask::RULE_DISK_FREE,
            said.word(),
            said.describe()
        );
    }
    aterm_agent::harness::cli::disk_ask_journal(
        state,
        &[ask::row(now_ms / 1000, key, said, level, free, host)],
    );
}

/// What the seat rule reads of one of this process's sessions ([`judge`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Facts {
    /// The harness supervises it, its Autopilot not Off.
    supervised: bool,
    /// The supervised agent its publication names ([`agent_of`]).
    program: Option<Program>,
    /// Its task record's state.
    task: Option<TaskState>,
    /// Why its worker found it unfit at its last turn break (`Board::unfit`), as that finding
    /// holds now ([`crate::disk_board::Unfit::bars`]).
    unfit: Option<&'static str>,
    /// What holds it where it cannot take a line now and passes ([`held_of`]): `limited`, an
    /// unspent usage-limit wall; `box`, a box its loop escalated to its owner; `hold`, the owner's
    /// hold or a person's take-over; `draft`, a draft standing in its composer at a turn break (a
    /// rest, or a break of the agent's own background work), or unchanged since one.
    held: Option<ask::HeldBy>,
    /// Whether the board places its directory on the volume asked about; `None`: its directory,
    /// or the volume it is on, was not read.
    on_volume: Option<bool>,
}

/// What the seat rule says of one session ([`judge`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Judged {
    /// It may be nominated.
    Seat,
    /// It may not be, by a fact that lasts past this look: a line owed it is released at this
    /// look ([`ask::Look::barred`]) or its turn break ([`act`]), never typed.
    Barred(&'static str),
    /// It is not seated this look, by nothing that need last: it is not supervised, its
    /// publication names no agent, or its directory or its volume was not read. A line owed it
    /// stays owed while it is supervised, and is handed on once it is not.
    Unseated,
    /// It would be seated but for what holds it now and passes ([`Facts::held`]): it is not
    /// nominated, a line owed it goes back to the episode at this look, once an episode, and is
    /// released the next time ([`ask::Look::held`]), as it could not take it, and a round with
    /// nobody else to elect waits for it to be past.
    Held(ask::HeldBy),
}

/// The seat rule: a supervised Claude Code (the line is written for its composer) working on the
/// volume, with no task record that stands ([`task_bar_of`]: done, exited, or a manager's
/// worker's) and whose worker did not find it unfit at its last turn break
/// ([`super::WorkerIdle`]: its task finished or landed, it has none, its Autopilot is Off, or it
/// is the person's own exchange). The task record bars a session whether or not it is still
/// supervised, so an agent that exited, and left the supervised roster, is released, not handed
/// on as undelivered.
fn judge(f: &Facts) -> Judged {
    if let Some(why) = task_bar_of(f.task).or(f.unfit) {
        return Judged::Barred(why);
    }
    if !f.supervised {
        return Judged::Unseated;
    }
    match f.program {
        Some(Program::Claude) => {}
        // The line is written for Claude Code's composer; another agent gets no seat until it
        // is checked against its own.
        Some(
            Program::Codex
            | Program::Agy
            | Program::Muse
            | Program::Grok
            | Program::Kimi
            | Program::Generic,
        ) => return Judged::Barred("not-claude"),
        None => return Judged::Unseated,
    }
    match f.on_volume {
        Some(true) => f.held.map_or(Judged::Seat, Judged::Held),
        Some(false) => Judged::Barred("off-volume"),
        None => Judged::Unseated,
    }
}

/// What holds a session where it cannot take a line now, as a look reads it ([`Facts::held`], each
/// [`ask::HeldBy`] here by its word):
/// - `hold` while the owner's hold or a person's take-over stands on it (`halted`: the fabric's
///   hold, `status hold=1`), whatever its agent reads;
/// - `credits` while its loop's credits hold stands (`withhold`,
///   [`crate::disk_board::Withhold::holds`] at `now_s`: until it lifts);
/// - `sign-in` while its agent reads a sign-in wall (`wall:auth`) or its loop held its last turn
///   break at one: only a person's browser brings the login back;
/// - `wall` while its loop held its last turn break at another wall that is no usage limit;
/// - `limited` while its agent reads a usage-limit wall (a `wall:<kind>` word of a kind
///   [`aterm_phase::WallKind::reads_limited`]) its loop has open and has not yet taken
///   as spent (`wall`, [`crate::disk_board::Wall::holds`] at `now_s`: before its reset and the
///   loop's grace, when the loop rests there and its line could go);
/// - `box` while its agent reads a box (`prompt`, `question`) its loop escalated to its owner
///   (`escalated`: the supervisor's own attention stands on it, not a limit's);
/// - `draft` while a draft stands in its composer at a turn break (`drafted`, [`Drafted`]): its
///   agent reads `idle`, or its screen is at a break of the agent's own background work, where
///   it reads `busy` but its loop offers a line as at an idle point and refuses it for the draft
///   (`background fact held: draft`); and on through a turn the agent opens itself while the
///   draft that stood at its last break stands unchanged ([`Drafted::SinceBreak`]), as it meets
///   that turn's break too. A draft typed while the agent works mid-turn, never yet left over a
///   turn break, goes with its turn, and holds nothing.
///
/// A box no rule answers waits on a person, a sign-in on a person too, and a wall on its reset:
/// none of those turns ends, and the credits hold types nothing at the turns that do, so a line
/// owed would wait out its delivery window. A hold and a draft keep the session at a turn break,
/// but the fenced write refuses every try there (`wait:held`, `wait:draft`) while they last, so a
/// line owed would wait it out as well, its claim stretching it to the delivery window and a
/// grace. All of them pass, so the session is held, not barred.
fn held_of(
    word: &str,
    escalated: bool,
    wall: Option<&crate::disk_board::Wall>,
    withhold: Option<&crate::disk_board::Withhold>,
    halted: bool,
    drafted: Drafted,
    now_s: i64,
) -> Option<ask::HeldBy> {
    use crate::disk_board::Withhold;
    let sign_in = aterm_phase::WallKind::Auth.wire();
    if halted {
        return Some(ask::HeldBy::Hold);
    }
    match withhold.filter(|w| w.holds(now_s)) {
        Some(Withhold::Credits { .. }) => return Some(ask::HeldBy::Credits),
        Some(Withhold::Wall { wire }) if *wire == sign_in => return Some(ask::HeldBy::SignIn),
        Some(Withhold::Wall { .. }) => return Some(ask::HeldBy::Wall),
        None => {}
    }
    match word {
        w if w == sign_in => Some(ask::HeldBy::SignIn),
        w if limit_word(w) && wall.is_some_and(|w| w.holds(now_s)) => Some(ask::HeldBy::Limit),
        "prompt" | "question" if escalated => Some(ask::HeldBy::Box),
        "idle" if drafted != Drafted::No => Some(ask::HeldBy::Draft),
        _ if matches!(drafted, Drafted::AtBackgroundBreak | Drafted::SinceBreak) => {
            Some(ask::HeldBy::Draft)
        }
        _ => None,
    }
}

/// Whether `word`, an agent's published word, is a wall of a kind its loop reads as a usage limit
/// ([`aterm_phase::WallKind::reads_limited`]): the usage windows, a model's bucket, spend, and an
/// API error, whose rate limit (429) alone does (the word does not say which; the loop's limit
/// episode, [`crate::disk_board::Board::walls`], does).
fn limit_word(word: &str) -> bool {
    use aterm_phase::{ApiCause, WallKind};
    [
        WallKind::UsageSession,
        WallKind::UsageWeekly,
        WallKind::ModelBucket { consent: false },
        WallKind::Spend,
        WallKind::ApiError {
            code: Some(429),
            retryable: true,
            cause: ApiCause::Server,
        },
    ]
    .iter()
    .any(|kind| kind.reads_limited() && kind.wire() == word)
}

/// What a session's composer held as a look read it ([`draft_in`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Draft {
    /// Its screen was not read: its terminal was busy, scrolled back or mid-frame, or the session
    /// is not supervised.
    Unread,
    /// No typed text: an empty composer, its placeholder, or none drawn.
    Clear,
    /// Typed text stands there.
    Text(String),
}

impl Draft {
    /// Whether this reading is a draft standing since `last`, what this process's last look read
    /// of the composer ([`DRAFTS`]): the same typed text, a look (a minute) apart, as the loop's
    /// own guard reads a draft that stands. A draft a person is still typing reads as no draft
    /// that stands.
    fn stands(&self, last: Option<&str>) -> bool {
        matches!(self, Draft::Text(t) if last == Some(t.as_str()))
    }
}

/// Whether a draft stands in a session's composer as a look reads it, and whether its screen is
/// then at a break of the agent's own background work, or the draft stood at a turn break before
/// ([`held_of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drafted {
    /// None stands: its composer is clear or was not read, or its text is new since this
    /// process's last look ([`Draft::stands`]).
    No,
    /// One stands, its screen at no break of the agent's own background work, and no look read
    /// it at a turn break: at a rest (its agent reads `idle`, which is one), or mid-turn, where it
    /// goes with the turn.
    Stands,
    /// One stands, and its screen is at a break of the agent's own background work: its turn is
    /// over, and only a shell or a monitor it left runs on
    /// ([`aterm_phase::turn_over_background`], the reader its loop offers a line there by). Its
    /// agent reads `busy` there, and its loop refuses that line for the draft.
    AtBackgroundBreak,
    /// One stands that a look read at a turn break, and every look since read unchanged
    /// ([`Kept::at_break`]), its screen now at no break of the agent's own background work: the
    /// agent works on in a turn it opened itself (a worker's report ending its background await, a
    /// monitor's event), as a person's message takes the composer's text with it. The draft meets
    /// that turn's break as it met the last, where its loop refuses the line for it again. Read
    /// as a draft typed mid-turn, the session was seated and elected again through such a turn,
    /// and held again at its break it was released ([`ask::Miss::Released`]): an
    /// episode whose only seat it was ran out and parked for its cooldown.
    SinceBreak,
}

impl Drafted {
    /// `draft` as it stands since `last`, what this process's last look kept of the composer
    /// ([`Draft::stands`], [`Kept`]), read on a screen at a break of the agent's own background
    /// work or not (`background`).
    fn of_reading(draft: &Draft, last: Option<&Kept>, background: bool) -> Self {
        if !draft.stands(last.map(|k| k.text.as_str())) {
            Drafted::No
        } else if background {
            Drafted::AtBackgroundBreak
        } else if last.is_some_and(|k| k.at_break) {
            Drafted::SinceBreak
        } else {
            Drafted::Stands
        }
    }
}

/// What the composer drawn in `rows` holds, the caret at `cursor` and `dim` saying whether a
/// row's first composer glyph is dim (Claude Code's placeholder, [`glyph_dim`]): read as the
/// fenced write reads it ([`aterm_agent::harness::upgrade::composer_is_empty`]), so a draft the
/// look reads is one the write would refuse.
fn draft_in(rows: &[String], cursor: (usize, usize), dim: impl Fn(usize) -> bool) -> Draft {
    let Some((caret, lines)) = aterm_phase::phase::composer_draft(rows) else {
        return Draft::Clear;
    };
    if aterm_agent::harness::upgrade::composer_is_empty(rows, Some(cursor), dim(caret)) {
        Draft::Clear
    } else {
        Draft::Text(lines.join("\n"))
    }
}

/// What `t`'s live screen shows in its composer ([`draft_in`]), and whether it is at a break of
/// the agent's own background work, read off the same rows ([`aterm_phase::turn_over_background`]:
/// its turn is over, and only a shell or a monitor it left runs on, where its loop offers a line
/// as at an idle point); [`Draft::Unread`], and no such break, while it is scrolled back or
/// mid-frame.
fn draft_on(t: &aterm_core::terminal::Terminal) -> (Draft, bool) {
    if t.grid().display_offset() > 0 || crate::frame_unfinished_now(t) {
        return (Draft::Unread, false);
    }
    let rows: Vec<String> = (0..usize::from(t.rows()))
        .map(|r| crate::claude_lights::row_text(t, r))
        .collect();
    let cursor = t.cursor();
    let draft = draft_in(
        &rows,
        (usize::from(cursor.row), usize::from(cursor.col)),
        |row| {
            let cells = u16::try_from(row)
                .ok()
                .and_then(|row| t.grid().row_at_screen(row));
            glyph_dim(|col| {
                let col = u16::try_from(col).ok()?;
                cells.and_then(|cells| cells.get(col)).map(|c| c.flags())
            })
        },
    );
    (draft, aterm_phase::turn_over_background(&rows))
}

/// Whether the composer's first glyph is dim, `flags(col)` the composer row's cell flags at
/// `col`: the glyph at [`aterm_agent::supervise::screen::COMPOSER_COL`], or, where the program
/// draws its own caret over it (`inverse`, as Claude Code draws its suggestion's first glyph), the
/// glyph after it, as the fenced write reads the same cells
/// ([`aterm_agent::supervise::screen::composer_glyph_cell`]). Read at the caret alone, a
/// suggestion's inverse first glyph reads plain, and the suggestion as a draft.
fn glyph_dim(flags: impl Fn(usize) -> Option<aterm_core::grid::CellFlags>) -> bool {
    use aterm_agent::supervise::screen::COMPOSER_COL;
    use aterm_core::grid::CellFlags;
    let glyph = match flags(COMPOSER_COL) {
        Some(first) if first.contains(CellFlags::INVERSE) => flags(
            COMPOSER_COL
                + if first.contains(CellFlags::WIDE) {
                    2
                } else {
                    1
                },
        ),
        first => first,
    };
    glyph.is_some_and(|f| f.contains(CellFlags::DIM))
}

/// What each session's composer held at a look, by sid, so the next look knows a draft that
/// stands ([`Draft::stands`]) and one that stood at a turn break ([`Drafted::SinceBreak`]): this
/// process's is [`DRAFTS`], and a test keeps its own, as a look's own rule keeps only its store's
/// live sessions in it ([`rule`]).
type Drafts = Mutex<BTreeMap<String, Kept>>;

/// What a look kept of a session's composer ([`Drafts`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Kept {
    /// The typed text it held.
    text: String,
    /// A look read this same text at a turn break (its agent at a rest, `idle`, or its screen at a
    /// break of the agent's own background work), and every look since that read the composer
    /// read it unchanged: a turn opened since took nothing from the composer, so it was not a
    /// person's message but the agent's own wake ([`Drafted::SinceBreak`]).
    at_break: bool,
}

impl Kept {
    /// What a look keeps of `text`, read in a composer after `last`, at a turn break or not
    /// (`at_break`: its agent at a rest, or its screen at a break of the agent's own background
    /// work), as [`Kept::at_break`] says.
    fn after(last: Option<&Kept>, text: &str, at_break: bool) -> Self {
        let at_break = at_break || last.is_some_and(|k| k.at_break && k.text == text);
        Self {
            text: text.to_owned(),
            at_break,
        }
    }
}

/// What each of this process's sessions' composers held at its last look ([`after_look`]).
static DRAFTS: Drafts = Mutex::new(BTreeMap::new());

/// Why the seat rule bars a session by its task record: its task is done (it landed) or
/// exited, or it goes on, so the session is a manager's worker (`managed`, as its loop's guard
/// reads the same record at its first turn break: `Guards::theirs`). This is read at every look,
/// so a managed worker is barred from its first look on, mid-turn or not, and in a seamless
/// update's successor before any turn break of its own; no finding of its worker's need stand
/// for it. `None`: it has no task record.
fn task_bar_of(state: Option<TaskState>) -> Option<&'static str> {
    match state? {
        TaskState::Done => Some("task-done"),
        TaskState::Exited => Some("task-exited"),
        TaskState::Working | TaskState::Stalled => Some(Guard::Managed.word()),
    }
}

/// One of this process's live sessions as a look reads it, before the board places it.
struct Read {
    sid: String,
    local: u64,
    supervised: bool,
    program: Option<Program>,
    task: Option<TaskState>,
    /// Its directory, off its terminal (while that is busy, as the look's start read it): a
    /// seat's checkout is read off it, and every other session's says whether it works in that
    /// checkout too ([`Seated::shared`]).
    cwd: Option<PathBuf>,
    /// It rests at a turn break.
    at_break: bool,
    /// Its agent reads busy: it works.
    working: bool,
    /// Its agent's word ([`held_of`]).
    word: &'static str,
    /// The supervisor's own attention stands on it, and is no usage limit's: its loop escalated a
    /// box to its owner ([`held_of`]).
    escalated: bool,
    /// The owner's hold or a person's take-over stands on it ([`held_of`]).
    halted: bool,
    /// What its composer holds, read only for a supervised session ([`Draft`]).
    draft: Draft,
    /// Its screen, read with [`Self::draft`], is at a break of the agent's own background work
    /// ([`draft_on`]): its agent reads `busy`, but its loop offers a line there as at an idle
    /// point ([`Drafted::AtBackgroundBreak`]).
    background: bool,
}

/// This process's live sessions (`only`: that one alone), each as [`Read`] says; `sids` are the
/// supervised ones.
fn read_sessions(store: &Store, sids: &[String], only: Option<&str>) -> Vec<Read> {
    use crate::cwd_native::ReportedCwd as _;
    let handles: Vec<_> = {
        let g = store.read().unwrap_or_else(PoisonError::into_inner);
        g.live_handles()
            .filter(|h| only.is_none_or(|sid| h.sid.as_str() == sid))
            .cloned()
            .collect()
    };
    let known = super::DISK_ALL_CWDS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    handles
        .iter()
        .map(|h| {
            let (program, word, task) = {
                let t = h
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let p = t.agent();
                (agent_of(p), p.word, t.task().map(|r| r.state))
            };
            let escalated = h
                .ctx
                .meta
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .attention_owners
                .get(aterm_agent::supervise::ATTENTION_OWNER)
                .is_some_and(|t| !t.starts_with(aterm_agent::supervise::ATTENTION_PREFIX));
            let halted = h.ctx.fabric.hold().is_some();
            let sid = h.sid.as_str().to_owned();
            let supervised = sids.contains(&sid);
            let (cwd, (draft, background)) = match h.term.try_lock() {
                Ok(t) => (
                    t.native_working_directory()
                        .filter(|c| !c.is_empty())
                        .map(|c| PathBuf::from(c.into_owned())),
                    if supervised {
                        draft_on(&t)
                    } else {
                        (Draft::Unread, false)
                    },
                ),
                Err(_) => (
                    known
                        .iter()
                        .find(|(id, _)| *id == h.local_id)
                        .map(|(_, cwd)| cwd.clone()),
                    (Draft::Unread, false),
                ),
            };
            let cwd = cwd.filter(|cwd| cwd.is_absolute());
            Read {
                sid,
                local: h.local_id,
                supervised,
                program,
                task,
                cwd,
                at_break: word == "idle",
                working: word == "busy",
                word,
                escalated,
                halted,
                draft,
                background,
            }
        })
        .collect()
}

/// The seat rule ([`judge`]) over `read` on volume `key` (its spelling), as the board places
/// each session and says what its worker found at its last turn break, as that holds now: all of
/// it until the session works again, and only whose it is (the person's own exchange, a manager's
/// wait) while its agent reads busy ([`crate::disk_board::Unfit::bars`]), so a session given
/// something to do since is seated again. A draft stands when `drafts` holds the same text from
/// the last look.
fn judged(read: Vec<Read>, key: &str, drafts: &Drafts) -> Vec<(Read, Judged)> {
    let now_s = aterm_time::unix_secs()
        .and_then(|s| i64::try_from(s).ok())
        .unwrap_or(0);
    let drafts = drafts
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let b = crate::disk_board::SHELF.board_lock();
    read.into_iter()
        .map(|r| {
            let drafted = Drafted::of_reading(&r.draft, drafts.get(&r.sid), r.background);
            let facts = Facts {
                supervised: r.supervised,
                program: r.program,
                task: r.task,
                unfit: b.unfit.get(&r.sid).and_then(|u| u.bars(r.working)),
                held: held_of(
                    r.word,
                    r.escalated,
                    b.walls.get(&r.sid),
                    b.withholds.get(&r.sid),
                    r.halted,
                    drafted,
                    now_s,
                ),
                on_volume: r
                    .cwd
                    .as_ref()
                    .and_then(|cwd| b.row_of(r.local, cwd.to_str()))
                    .map(|row| row.reading.key.spelled() == key),
            };
            let judged = judge(&facts);
            (r, judged)
        })
        .collect()
}

/// What the seat rule found for this process's sessions on one volume ([`rule`]).
struct Rule {
    /// Those the ask could go to, each with what `holders` hold for its checkout and whether it
    /// rests at a turn break.
    seated: Vec<Seated>,
    /// Those it bars by a fact that lasts ([`ask::Look::barred`]).
    barred: Vec<String>,
    /// Every live session.
    live: Vec<String>,
    /// Those it would seat but for what holds them now and passes, each with what holds it
    /// ([`ask::Look::held`]).
    held: Vec<(String, ask::HeldBy)>,
}

/// What the pass left that a seat counts ([`rule`]): its largest holders, `$HOME`, and how a
/// directory is read as a checkout's root ([`checkout_root`]) and as there at all
/// ([`dir_there`]), which a test replaces so it reads no disk.
struct Left<'a> {
    holders: &'a [Hog],
    home: Option<&'a Path>,
    root: &'a dyn Fn(&Path) -> bool,
    there: &'a dyn Fn(&Path) -> bool,
}

impl Left<'static> {
    /// Nothing left: no seat holds anything, and no directory is read.
    const NONE: Self = Self {
        holders: &[],
        home: None,
        root: &unread,
        there: &unread,
    };
}

/// No directory is read: none is a checkout's root, or there ([`Left::NONE`]).
const fn unread(_: &Path) -> bool {
    false
}

/// The seat rule over this process's live sessions on volume `key` (its spelling), `sids` the
/// supervised ones and `left` what the pass left, each seat holding what its own checkout's
/// builds hold ([`checkout_of`], [`held_in`]), and knowing whether another live session works in
/// that checkout too ([`shared_with`]). A look's own rule (`remember`, after its pass) keeps
/// what each composer held in `drafts` for the next look, and only its store's live sessions'
/// ([`DRAFTS`]); the take-over before the pass reads it alone.
fn rule(
    store: &Store,
    sids: &[String],
    key: &str,
    left: &Left<'_>,
    drafts: &Drafts,
    remember: bool,
) -> Rule {
    let mut out = Rule {
        seated: Vec::new(),
        barred: Vec::new(),
        live: Vec::new(),
        held: Vec::new(),
    };
    let judged = judged(read_sessions(store, sids, None), key, drafts);
    if remember {
        let mut drafts = drafts.lock().unwrap_or_else(PoisonError::into_inner);
        for (r, _) in &judged {
            match &r.draft {
                Draft::Text(t) => {
                    let kept = Kept::after(drafts.get(&r.sid), t, r.at_break || r.background);
                    drafts.insert(r.sid.clone(), kept);
                }
                Draft::Clear => {
                    drafts.remove(&r.sid);
                }
                Draft::Unread => {}
            }
        }
        drafts.retain(|sid, _| judged.iter().any(|(r, _)| r.sid == *sid));
    }
    let dirs: Vec<(String, PathBuf)> = judged
        .iter()
        .filter_map(|(r, _)| Some((r.sid.clone(), r.cwd.clone()?)))
        .collect();
    for (r, judged) in judged {
        out.live.push(r.sid.clone());
        match (judged, r.cwd) {
            (Judged::Barred(_), _) => out.barred.push(r.sid),
            (Judged::Held(why), _) => out.held.push((r.sid, why)),
            (Judged::Seat, Some(cwd)) => {
                let checkout = checkout_of(left.holders, &cwd, left.home, left.root, left.there);
                let shared = checkout
                    .as_deref()
                    .is_some_and(|c| shared_with(c, &r.sid, &dirs, left));
                out.seated.push(Seated {
                    seat: ask::Seat {
                        held: held_in(left.holders, checkout.as_deref()),
                        sid: r.sid,
                        at_break: r.at_break,
                    },
                    checkout,
                    shared,
                });
            }
            (Judged::Seat | Judged::Unseated, _) => {}
        }
    }
    out
}

/// Whether a session other than `sid`, of those in `dirs` (each live session's sid and
/// directory), works in `checkout` itself ([`checkout_of`] over `left`): several tabs in one
/// checkout build in its one cache, which is then not `sid`'s alone ([`Seated::shared`]). Only a
/// directory under `checkout` is read for its own checkout, so a session elsewhere costs no walk.
fn shared_with(checkout: &Path, sid: &str, dirs: &[(String, PathBuf)], left: &Left<'_>) -> bool {
    dirs.iter().any(|(other, cwd)| {
        other != sid
            && cwd.starts_with(checkout)
            && checkout_of(left.holders, cwd, left.home, left.root, left.there).as_deref()
                == Some(checkout)
    })
}

/// What the seat rule says of this process's session `sid` on volume `key` (its spelling) now,
/// read as the look reads it ([`judge`]), so a session the look would not seat by a lasting fact
/// is never typed its line, and one it holds is neither claimed nor typed it.
/// A draft stands when `drafts` holds the same text from the last look ([`DRAFTS`]).
/// [`Judged::Unseated`]: it is not this process's.
fn judge_at(store: &Store, sid: &str, key: &str, drafts: &Drafts) -> Judged {
    judged(
        read_sessions(store, &[sid.to_owned()], Some(sid)),
        key,
        drafts,
    )
    .into_iter()
    .map(|(_, judged)| judged)
    .next()
    .unwrap_or(Judged::Unseated)
}

/// The most directories [`checkout_of`] looks into for a session's own checkout.
const CHECKOUT_WALK: usize = 32;

/// Whether `dir` is a checkout's root: it holds a `.git`, a repository's directory or a
/// worktree's or submodule's file, as the disk look's discovery reads one
/// ([`disk::discover::worktree_root_of`]). One `lstat`, made off the UI thread by the look's
/// step ([`after_look`]).
fn checkout_root(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(".git")).is_ok()
}

/// Whether `dir` is there, a directory: a session's own directory may have been removed under it
/// (a worktree its session landed and removed). One `stat`, made off the UI thread by the look's
/// step ([`after_look`]).
fn dir_there(dir: &Path) -> bool {
    std::fs::metadata(dir).is_ok_and(|m| m.is_dir())
}

/// The checkout `cwd` works in, of those `holders` might serve: the deepest checkout a holder
/// serves that `cwd` sits under, or, nearer, the first directory from `cwd` up, short of that one,
/// that `root` reads as a checkout's root ([`checkout_root`]). So a worktree nested in another
/// checkout is its own whether or not the pass lists a holder of its own: `holders` are only its
/// largest ([`disk::MAX_HOGS`]), and a build of its own already reclaimed is none of them. `root`
/// is asked of no directory at or above that served checkout or above `home`, and of at most
/// [`CHECKOUT_WALK`]. When it finds no root, `there` is asked once whether `cwd` itself is there:
/// a removed directory holds no `.git` to find, so a worktree removed under its session would
/// read as the checkout around it. `None`: no holder serves a checkout `cwd` sits under, so none
/// of them is its own, and nothing is asked; or `cwd`, below the served checkout, is gone, so
/// which checkout it was is unknown, and none of them is its own.
fn checkout_of(
    holders: &[Hog],
    cwd: &Path,
    home: Option<&Path>,
    root: &dyn Fn(&Path) -> bool,
    there: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let served = holders
        .iter()
        .filter_map(|h| h.serves.as_deref())
        .filter(|c| cwd.starts_with(c))
        .max_by_key(|c| c.components().count())?;
    for dir in cwd
        .ancestors()
        .take_while(|dir| *dir != served)
        .take(CHECKOUT_WALK)
    {
        if root(dir) {
            return Some(dir.to_path_buf());
        }
        if Some(dir) == home {
            break;
        }
    }
    (cwd == served || there(cwd)).then(|| served.to_path_buf())
}

/// What `holders` hold for `checkout` ([`checkout_of`]): only the builds that serve it exactly,
/// never a checkout around it.
fn held_in(holders: &[Hog], checkout: Option<&Path>) -> u64 {
    let Some(checkout) = checkout else {
        return 0;
    };
    holders
        .iter()
        .filter(|h| h.serves.as_deref() == Some(checkout))
        .fold(0u64, |a, h| a.saturating_add(h.bytes))
}

/// A line this process typed.
#[derive(Debug, Clone)]
struct Typed {
    sid: String,
    /// Unix ms.
    at_ms: i64,
    /// The volume's free bytes then.
    free: u64,
}

/// The last line this process typed, by volume. A line whose record could not be written to
/// the lease (a full disk) is never typed twice, and the next look records it
/// ([`ask::typed_unrecorded`]).
static TYPED: Mutex<BTreeMap<String, Typed>> = Mutex::new(BTreeMap::new());

/// The last line this process typed on volume `key`.
fn typed_last(key: &str) -> Option<Typed> {
    TYPED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(key)
        .cloned()
}

/// Whether this process typed session `sid` a line on volume `key` within the cooldown.
fn typed_here(key: &str, sid: &str, now: i64) -> bool {
    typed_last(key).is_some_and(|t| {
        t.sid == sid && (0..ask::COOLDOWN_MS).contains(&now.saturating_sub(t.at_ms))
    })
}

/// What the act's step under the lease's lock came to.
enum Stepped {
    /// The lease handed the ask on.
    Released(ask::Said),
    /// The step's word: `typed`, `wait:<why>` or `gone`.
    Step(String),
}

/// The live act ([`super::Acts::disk_free`]): at session `sid`'s turn break, type `owed`'s line
/// when the lease still names it, or release it when the session may not be asked (`unfit`, the
/// worker's finding, or what else the seat rule bars it by, [`judge_at`], so a session the look
/// would not seat by a lasting fact is never typed its line, nor is one the loop holds for
/// someone else for good: its `exchange` or `managed`, which the worker hands in as `unfit`). A
/// line `held` at this turn break for a while (the loop's word, `person`: a person's grace or
/// another driver's hand, the occupancy door's guard; or what the seat rule holds it by,
/// [`held_of`]: the owner's hold, a draft standing in its composer) is not claimed or typed and
/// stays owed, `wait:<held>`, until the look gives it back to the episode. The claim, the typing
/// and its record hold the lease's lock as one step ([`ask::at_break`]), so no other host hands
/// the line on while it is typed.
/// The step: `typed`, `wait:<why>` (tried again at its next turn break), `released:<why>`, or
/// `gone` (the lease names it no more).
pub(super) fn act(
    store: &Store,
    sock: &str,
    sid: &str,
    owed: &crate::disk_board::AskOwed,
    unfit: Option<&'static str>,
    held: Option<&'static str>,
) -> String {
    let Some(state) = harness_state() else {
        return "wait:no-state".to_owned();
    };
    let host = ask::HostId::this();
    let now = now_ms();
    let (free, level) = volume_on(&owed.key);
    let (unfit, held) = guards(judge_at(store, sid, &owed.key, &DRAFTS), unfit, held);
    let token = super::harness_drive_token();
    let at = AtBreak {
        state: &state,
        host: &host,
        sid,
        owed,
        now,
        unfit,
        held,
        free,
        level,
    };
    let step = at.step(
        || disk::type_resume(sock, token.as_ref(), sid, &owed.text),
        || free_now(&owed.key),
    );
    if step == "typed" {
        super::log_label(store, sid, aterm_agent::harness::acts::DISK_FREE_LABEL);
    }
    aterm_agent::harness::cli::disk_ask_journal(
        &state,
        &[ask::step_row(now / 1000, sid, &owed.key, &step, &host)],
    );
    step
}

/// What bars and what holds session `sid`'s line at its turn break ([`act`]): the worker's
/// `unfit` and `held`, joined with the seat rule's reading of the session now (`judged`,
/// [`judge_at`]): one it bars is released, and one it holds (a wall, a box, the owner's hold, a
/// draft that stood a look) waits, `wait:<why>`, unclaimed, so its claim does not stretch its time.
fn guards(
    judged: Judged,
    unfit: Option<&'static str>,
    held: Option<&'static str>,
) -> (Option<&'static str>, Option<&'static str>) {
    match judged {
        Judged::Barred(why) => (unfit.or(Some(why)), held),
        Judged::Held(why) => (unfit, held.or(Some(why.word()))),
        Judged::Seat | Judged::Unseated => (unfit, held),
    }
}

/// One turn break's step for the line [`Self::owed`] holds for session [`Self::sid`]
/// ([`act`]), as this host reads it.
struct AtBreak<'a> {
    /// The harness state directory: the lease's.
    state: &'a Path,
    host: &'a ask::HostId,
    sid: &'a str,
    owed: &'a crate::disk_board::AskOwed,
    /// Unix ms.
    now: i64,
    /// Why the session may not be asked ([`act`]).
    unfit: Option<&'static str>,
    /// What holds the line at this turn break ([`act`]).
    held: Option<&'static str>,
    /// The free bytes and the level the board last read on the volume ([`volume_on`]).
    free: u64,
    level: Level,
}

impl AtBreak<'_> {
    /// The step's word: a line already typed here is `gone`; one a guard holds waits, untyped
    /// and unclaimed; otherwise the lease releases or claims it, and a claimed one is typed
    /// (`type_line`, the fenced write's word back). A line that went is recorded with the free
    /// bytes `measure` reads as it went ([`free_now`]), or the board's where it reads none, so
    /// the rise the asked session must make is measured from the volume it was asked about,
    /// never from a look a minute older.
    fn step(
        &self,
        type_line: impl FnOnce() -> String,
        measure: impl FnOnce() -> Option<u64>,
    ) -> String {
        let key = &self.owed.key;
        if self.unfit.is_none() && typed_here(key, self.sid, self.now) {
            return "gone".to_owned();
        }
        if self.unfit.is_none()
            && let Some(why) = self.held
        {
            return format!("wait:{why}");
        }
        let stepped = ask::transact(self.state, &ask::LOCK_LADDER, |l| {
            match ask::at_break(l, key, self.host, self.sid, self.now, self.unfit.is_some()) {
                ask::Break::Released(said) => Stepped::Released(said),
                ask::Break::Gone => Stepped::Step("gone".to_owned()),
                ask::Break::Yours => {
                    let step = type_line();
                    if step == "typed" {
                        let at = now_ms();
                        let free = measure().unwrap_or(self.free);
                        TYPED.lock().unwrap_or_else(PoisonError::into_inner).insert(
                            key.clone(),
                            Typed {
                                sid: self.sid.to_owned(),
                                at_ms: at,
                                free,
                            },
                        );
                        ask::typed(l, key, self.host, self.sid, at, free);
                    }
                    Stepped::Step(step)
                }
            }
        });
        match stepped {
            Ok(Some(Stepped::Released(said))) => {
                journal(
                    self.state, self.now, key, &said, self.level, self.free, self.host,
                );
                format!("released:{}", self.unfit.unwrap_or("unfit"))
            }
            Ok(Some(Stepped::Step(step))) => step,
            Ok(None) => "wait:lease-held".to_owned(),
            Err(e) if self.unfit.is_none() && typed_here(key, self.sid, now_ms()) => {
                aterm_log::warn!(
                    "harness @{}: {} typed, but the lease could not record it ({e}); the next \
                     look records it",
                    self.sid,
                    ask::RULE_DISK_FREE
                );
                "typed".to_owned()
            }
            Err(_) => "wait:lease-unwritten".to_owned(),
        }
    }
}

/// The free bytes of the volume the lease keys `key`, read now: the home volume's, the one the
/// ask covers; `None` where it cannot be read or is no longer that volume.
fn free_now(key: &str) -> Option<u64> {
    let home = aterm_primer::home_dir()?;
    let reading = disk::volume_of(&home, &aterm_sysprobe::volumes::table()).ok()?;
    (reading.key.spelled() == key).then_some(reading.free)
}

/// The free bytes and the level the board last read on the volume the lease keys `key`.
fn volume_on(key: &str) -> (u64, Level) {
    let b = crate::disk_board::SHELF.board_lock();
    b.volumes
        .iter()
        .find(|r| r.reading.key.spelled() == key)
        .map_or((0, Level::Unknown), |r| (r.reading.free, r.level))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why the seat rule bars this process's session `sid` on volume `key` now ([`judge_at`]).
    /// `None`: it may be asked, or it is not this process's.
    fn bar_at(store: &Store, sid: &str, key: &str) -> Option<&'static str> {
        match judge_at(store, sid, key, &Drafts::default()) {
            Judged::Barred(why) => Some(why),
            Judged::Seat | Judged::Unseated | Judged::Held(_) => None,
        }
    }

    fn rows(state: &Path) -> Vec<String> {
        std::fs::read_to_string(state.join(aterm_agent::harness::cli::DISK_LEDGER))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_standing_word_is_journalled_once_and_every_move_each_time() {
        let state = aterm_tempfile::TempDir::new().expect("owned disk journal");
        let host = ask::HostId {
            pid: 1,
            start: String::new(),
        };
        let key = "dev:journal-once";
        let at = |said: &ask::Said| journal(state.path(), 0, key, said, Level::Short, 0, &host);
        at(&ask::Said::Quiet);
        assert!(
            rows(state.path()).is_empty(),
            "nothing to say while all is well"
        );
        at(&ask::Said::Freeing);
        at(&ask::Said::Freeing);
        at(&ask::Said::Opened {
            done: ask::Done::Spent,
            projected: None,
        });
        at(&ask::Said::Opened {
            done: ask::Done::Spent,
            projected: None,
        });
        at(&ask::Said::Settled);
        at(&ask::Said::Settled);
        at(&ask::Said::Quiet);
        let words: Vec<String> = rows(state.path())
            .iter()
            .filter_map(|r| {
                let v: aterm_json::Value = aterm_json::from_str(r).ok()?;
                Some(v.get("said")?.as_str()?.to_owned())
            })
            .collect();
        assert_eq!(words, ["freeing", "opened", "opened", "settled", "quiet"]);
    }

    #[test]
    fn the_seat_rule_seats_a_supervised_claude_on_the_volume_and_bars_only_by_lasting_facts() {
        let seat = Facts {
            supervised: true,
            program: Some(Program::Claude),
            task: None,
            unfit: None,
            held: None,
            on_volume: Some(true),
        };
        assert_eq!(
            judge(&seat),
            Judged::Seat,
            "no task record: the worker's finding decides"
        );
        for (facts, why) in [
            (
                Facts {
                    task: Some(TaskState::Working),
                    ..seat
                },
                "managed",
            ),
            (
                Facts {
                    task: Some(TaskState::Stalled),
                    supervised: false,
                    ..seat
                },
                "managed",
            ),
            (
                Facts {
                    task: Some(TaskState::Done),
                    ..seat
                },
                "task-done",
            ),
            (
                Facts {
                    task: Some(TaskState::Exited),
                    supervised: false,
                    program: None,
                    on_volume: None,
                    ..seat
                },
                "task-exited",
            ),
            (
                Facts {
                    unfit: Some("finished"),
                    ..seat
                },
                "finished",
            ),
            (
                Facts {
                    program: Some(Program::Codex),
                    ..seat
                },
                "not-claude",
            ),
            (
                Facts {
                    on_volume: Some(false),
                    ..seat
                },
                "off-volume",
            ),
            (
                Facts {
                    task: Some(TaskState::Done),
                    held: Some(ask::HeldBy::Limit),
                    ..seat
                },
                "task-done",
            ),
            (
                Facts {
                    on_volume: Some(false),
                    held: Some(ask::HeldBy::Box),
                    ..seat
                },
                "off-volume",
            ),
        ] {
            assert_eq!(judge(&facts), Judged::Barred(why), "{facts:?}");
        }
        for why in [ask::HeldBy::Limit, ask::HeldBy::Box] {
            assert_eq!(
                judge(&Facts {
                    held: Some(why),
                    ..seat
                }),
                Judged::Held(why),
                "a seat but for what passes: held, not barred"
            );
        }
        for facts in [
            Facts {
                supervised: false,
                ..seat
            },
            Facts {
                program: None,
                ..seat
            },
            Facts {
                on_volume: None,
                ..seat
            },
            Facts {
                on_volume: None,
                held: Some(ask::HeldBy::Limit),
                ..seat
            },
            Facts {
                supervised: false,
                held: Some(ask::HeldBy::Box),
                ..seat
            },
        ] {
            assert_eq!(
                judge(&facts),
                Judged::Unseated,
                "nothing that lasts: an owed line stays owed while supervised: {facts:?}"
            );
        }
    }

    /// A session at a usage-limit wall its loop has open, before its reset and the loop's grace,
    /// or at a box its loop escalated to its owner, cannot take a line before its delivery window
    /// ends: the look holds it out of its seats. A seat rule that seated such a tab elected it,
    /// often first (its checkout the largest), and the line waited out its window and was handed
    /// on as undelivered, spending an election with nothing typed. The wall is read by the word
    /// its agent publishes, `wall:<kind>`, of a kind its loop reads as a usage limit: the seat
    /// rule once matched a `limited` no agent publishes, and held no session at a wall at all.
    #[test]
    fn a_session_at_an_unspent_wall_or_an_escalated_box_is_held() {
        use crate::disk_board::Wall;
        let wall = |until_s| Wall { until_s };
        let now = 1_000_000;
        let held = |word, escalated, wall: Option<&Wall>| {
            held_of(word, escalated, wall, None, false, Drafted::No, now)
        };
        for word in [
            "wall:usage-session",
            "wall:usage-weekly",
            "wall:model-bucket",
            "wall:spend",
            "wall:api-error",
        ] {
            assert_eq!(
                held(word, false, Some(&wall(Some(now + 1)))),
                Some(ask::HeldBy::Limit),
                "{word}"
            );
            assert_eq!(
                held(word, false, Some(&wall(None))),
                Some(ask::HeldBy::Limit),
                "{word}: no reset known: held until the loop closes the wall"
            );
            assert_eq!(
                held(word, false, Some(&wall(Some(now)))),
                None,
                "{word}: spent: the loop rests there, and the line can go"
            );
            assert_eq!(
                held(word, false, None),
                None,
                "{word}: no wall the loop has open (an API error that is no rate limit)"
            );
        }
        for word in ["wall:context", "wall:overloaded", "wall:memory"] {
            assert_eq!(
                held(word, false, Some(&wall(None))),
                None,
                "{word}: no usage limit's wall"
            );
        }
        assert_eq!(
            held("idle", false, Some(&wall(None))),
            None,
            "past the wall: its agent no longer reads limited"
        );
        for word in ["prompt", "question"] {
            assert_eq!(held(word, true, None), Some(ask::HeldBy::Box), "{word}");
            assert_eq!(
                held(word, false, None),
                None,
                "{word}: a box a rule answers is no person's to wait on"
            );
        }
        assert_eq!(held("busy", true, None), None);
    }

    /// What this process's passes freed is kept for the lease until a step lands it: a step the
    /// lease's lock kept from landing reports it again at the next, merged past the lease's own
    /// bound so nothing is lost, and a landed step forgets it.
    #[test]
    fn a_pass_the_lease_has_yet_to_record_is_reported_until_a_step_records_it() {
        let key = "dev:unrecorded-passes-test";
        let at = |ended_ms, bytes| ask::Reclaimed { ended_ms, bytes };
        assert_eq!(passes_unrecorded(key, &[at(10, 1)]), [at(10, 1)]);
        assert_eq!(
            passes_unrecorded(key, &[at(20, 2)]),
            [at(10, 1), at(20, 2)],
            "the step that missed the lease reports its pass again"
        );
        assert!(
            passes_unrecorded("dev:another-volume", &[]).is_empty(),
            "by volume"
        );
        let more: Vec<ask::Reclaimed> = (0..40).map(|i| at(100 + i, 1)).collect();
        let kept = passes_unrecorded(key, &more);
        assert_eq!(kept.len(), ask::MAX_RECLAIMED);
        assert_eq!(
            kept.iter().map(|r| r.bytes).sum::<u64>(),
            43,
            "nothing lost"
        );
        passes_recorded(key, &kept);
        assert!(
            passes_unrecorded(key, &[]).is_empty(),
            "recorded: forgotten"
        );
    }

    /// Another instance's pass holds the pass lock from its pass until its own step has recorded
    /// what it freed: a look meanwhile reads it running ([`ask::Look::passing`]), and settles no
    /// asked episode on the space it freed.
    #[test]
    fn a_look_reads_another_instances_pass_running_while_its_lock_is_held() {
        let home = aterm_tempfile::TempDir::new().expect("a scratch home");
        assert!(!another_pass_runs(home.path()), "nobody's pass");
        let lock = disk::reserve::take_pass(&disk::reserve::pass_lock_for(home.path()))
            .expect("the lock opens")
            .expect("nobody holds it");
        assert!(another_pass_runs(home.path()), "a pass holds it");
        drop(lock);
        assert!(!another_pass_runs(home.path()), "let go");
    }

    /// The loop's lasting withholds hold a session out of the seats for as long as they stand, as
    /// a usage-limit wall does: its credits hold (`[harness] usage_credits = false`, the default),
    /// which types nothing at its turn breaks until the limit resets, and a sign-in wall
    /// (`wall:auth`), which only a person's browser moves. Seated, such a session was elected,
    /// typed nothing for the delivery window and a grace, and was handed on as undelivered; as
    /// the only seat, the episode then parked for six hours with nothing freed.
    #[test]
    fn a_session_its_loop_holds_for_credits_or_a_sign_in_is_held_until_it_is_past() {
        use crate::disk_board::Withhold;
        let now = 1_000_000;
        let held = |word, withhold: Option<&Withhold>| {
            held_of(word, false, None, withhold, false, Drafted::No, now)
        };
        let credits = |until_s| Withhold::Credits { until_s };
        assert_eq!(
            held("idle", Some(&credits(Some(now + 1)))),
            Some(ask::HeldBy::Credits)
        );
        assert_eq!(
            held("busy", Some(&credits(None))),
            Some(ask::HeldBy::Credits),
            "no lift time known: held until its loop finds it lifted"
        );
        assert_eq!(
            held("idle", Some(&credits(Some(now)))),
            None,
            "lifted: its loop types at the next turn break"
        );
        let auth = Withhold::Wall {
            wire: aterm_phase::WallKind::Auth.wire(),
        };
        assert_eq!(held("idle", Some(&auth)), Some(ask::HeldBy::SignIn));
        assert_eq!(
            held("wall:auth", None),
            Some(ask::HeldBy::SignIn),
            "its agent reads the sign-in wall before its loop decides a break there"
        );
        assert_eq!(
            held(
                "idle",
                Some(&Withhold::Wall {
                    wire: "wall:overloaded"
                })
            ),
            Some(ask::HeldBy::Wall)
        );
        assert_eq!(held("idle", None), None);
        assert_eq!(
            ask::HeldBy::Credits.word(),
            "credits",
            "the lease and the journal name it"
        );
    }

    /// A session the owner holds (or a person took over: `status hold=1`), or whose composer
    /// holds a draft that stands at a turn break (a rest, or a break of the agent's own background
    /// work, where its agent reads busy), keeps to that turn break, and the loop refuses every
    /// try there (`wait:held`, `wait:draft`, `background fact held: draft`): the look holds it,
    /// as a wall or a box. Read as fit, it was nominated before a working session in the same GiB
    /// (it rests at a turn break), elected, refused at every step of the wait for the delivery
    /// window and a grace, and handed on as undelivered, spending an election with no line typed.
    #[test]
    fn a_held_session_or_one_whose_draft_stands_at_a_turn_break_is_held() {
        let now = 1_000_000;
        for word in ["idle", "busy", "prompt", "wall:usage-session"] {
            assert_eq!(
                held_of(word, false, None, None, true, Drafted::No, now),
                Some(ask::HeldBy::Hold),
                "{word}: the hold refuses any typed line"
            );
        }
        for drafted in [
            Drafted::Stands,
            Drafted::AtBackgroundBreak,
            Drafted::SinceBreak,
        ] {
            assert_eq!(
                held_of("idle", false, None, None, false, drafted, now),
                Some(ask::HeldBy::Draft),
                "{drafted:?}: at rest"
            );
        }
        assert_eq!(
            held_of(
                "busy",
                false,
                None,
                None,
                false,
                Drafted::AtBackgroundBreak,
                now
            ),
            Some(ask::HeldBy::Draft),
            "its turn over, only its own shell or monitor running: the loop's break refuses it"
        );
        assert_eq!(
            held_of("busy", false, None, None, false, Drafted::SinceBreak, now),
            Some(ask::HeldBy::Draft),
            "the draft that stood at its last break, through a turn the agent opened itself"
        );
        assert_eq!(
            held_of("busy", false, None, None, false, Drafted::Stands, now),
            None,
            "a draft typed while it works mid-turn goes with its turn"
        );
        for word in ["idle", "busy"] {
            assert_eq!(
                held_of(word, false, None, None, false, Drafted::No, now),
                None,
                "{word}"
            );
        }
        // What the look reads: the draft standing since its last look, and where.
        let typed = Draft::Text("land it".to_owned());
        let kept = |text: &str, at_break| Kept {
            text: text.to_owned(),
            at_break,
        };
        assert_eq!(
            Drafted::of_reading(&typed, Some(&kept("land it", false)), false),
            Drafted::Stands
        );
        assert_eq!(
            Drafted::of_reading(&typed, Some(&kept("land it", false)), true),
            Drafted::AtBackgroundBreak
        );
        assert_eq!(
            Drafted::of_reading(&typed, Some(&kept("land it", true)), true),
            Drafted::AtBackgroundBreak
        );
        assert_eq!(
            Drafted::of_reading(&typed, Some(&kept("land it", true)), false),
            Drafted::SinceBreak,
            "read at a break before, and the turn since took nothing from the composer"
        );
        assert_eq!(
            Drafted::of_reading(&typed, None, true),
            Drafted::No,
            "first read: it may be a person typing"
        );
        assert_eq!(
            Drafted::of_reading(&typed, Some(&kept("land", true)), false),
            Drafted::No,
            "still being typed"
        );
        assert_eq!(Drafted::of_reading(&Draft::Clear, None, true), Drafted::No);
        // What a look keeps: a text read at a turn break, and kept so while it stands unchanged.
        assert_eq!(Kept::after(None, "land it", true), kept("land it", true));
        assert_eq!(Kept::after(None, "land it", false), kept("land it", false));
        assert_eq!(
            Kept::after(Some(&kept("land it", true)), "land it", false),
            kept("land it", true),
            "unchanged since the break"
        );
        assert_eq!(
            Kept::after(Some(&kept("land it", true)), "land it now", false),
            kept("land it now", false),
            "typed into since, mid-turn: a new draft"
        );
        assert_eq!(
            Kept::after(Some(&kept("land it", false)), "land it", false),
            kept("land it", false),
            "typed mid-turn, never at a break"
        );
        let seat = Facts {
            supervised: true,
            program: Some(Program::Claude),
            task: None,
            unfit: None,
            held: None,
            on_volume: Some(true),
        };
        for why in [ask::HeldBy::Hold, ask::HeldBy::Draft] {
            assert_eq!(
                judge(&Facts {
                    held: Some(why),
                    ..seat
                }),
                Judged::Held(why),
                "neither seated nor barred"
            );
        }
        // A draft stands when the look before read the same typed text there.
        let text = Draft::Text("fix the flaky test".to_owned());
        assert!(text.stands(Some("fix the flaky test")));
        assert!(!text.stands(None), "first read: it may be a person typing");
        assert!(!text.stands(Some("fix the")), "still being typed");
        assert!(!Draft::Clear.stands(Some("fix the flaky test")));
        assert!(!Draft::Unread.stands(Some("fix the flaky test")));
    }

    /// The composer as the fenced write reads it: typed text is a draft; an empty composer, or
    /// Claude Code's dim placeholder with the caret at its start, is none.
    #[test]
    fn a_draft_is_read_off_the_composer_as_the_fenced_write_reads_it() {
        let rule = "\u{2500}".repeat(60);
        let screen = |line: &str| vec![rule.clone(), format!("\u{276f} {line}"), rule.clone()];
        assert_eq!(
            draft_in(&screen("fix the flaky test"), (1, 20), |_| false),
            Draft::Text("fix the flaky test".to_owned())
        );
        assert_eq!(draft_in(&screen(""), (1, 2), |_| false), Draft::Clear);
        assert_eq!(
            draft_in(&screen("Try \"refactor this\""), (1, 2), |row| row == 1),
            Draft::Clear,
            "the dim placeholder"
        );
        assert_eq!(
            draft_in(&screen("Try \"refactor this\""), (1, 2), |_| false),
            Draft::Text("Try \"refactor this\"".to_owned()),
            "the same words typed"
        );
        assert_eq!(
            draft_in(&["$ ls".to_owned()], (0, 4), |_| false),
            Draft::Clear,
            "no composer drawn"
        );
    }

    /// Through the look: the owner's hold reads on the session at once, and its composer off its
    /// own screen, a look's own rule keeping what it held for the next look (its [`Drafts`]), so the
    /// second look to read the same typed text reads it standing; the take-over's rule before the
    /// pass keeps nothing, and a cleared composer is forgotten.
    #[test]
    fn a_look_reads_the_owners_hold_and_a_draft_that_stands_since_its_last_look() {
        let drafts = Drafts::default();
        let mut sessions = crate::session_store::SessionStore::default();
        let h = crate::session_store::test_handle(9_300);
        sessions.register(h.clone());
        let sid = h.sid.as_str().to_owned();
        let sids = [sid.clone()];
        let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
        let key = "fsid:9300";
        let draw = |line: &str| {
            let rule = "\u{2500}".repeat(60);
            let col = line.chars().count() + 3;
            h.term
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .process(
                    format!("\x1b[2J\x1b[H{rule}\r\n\u{276f} {line}\r\n{rule}\x1b[2;{col}H")
                        .as_bytes(),
                );
        };
        let read = || {
            let r = read_sessions(&store, &sids, Some(&sid))
                .pop()
                .expect("the session");
            let last = drafts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&sid)
                .map(|k| k.text.clone());
            let stands = r.draft.stands(last.as_deref());
            (r.halted, r.draft, stands)
        };
        draw("");
        let clear = read();
        assert!(crate::fabric::apply_hold_for_test(
            &*h.ctx,
            Some(crate::fabric::Hold {
                reason: "-".into(),
                origin: "local".into(),
            })
        ));
        let halted = read().0;
        assert!(crate::fabric::apply_hold_for_test(&*h.ctx, None));
        draw("fix the flaky test");
        let first = read();
        let _ = rule(&store, &sids, key, &Left::NONE, &drafts, false);
        let before_pass = read();
        let _ = rule(&store, &sids, key, &Left::NONE, &drafts, true);
        let second = read();
        draw("");
        let _ = rule(&store, &sids, key, &Left::NONE, &drafts, true);
        let forgotten = drafts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&sid)
            .is_none();
        assert_eq!(
            clear,
            (false, Draft::Clear, false),
            "CONTROL: nothing holds it"
        );
        assert!(halted, "the owner's hold: status hold=1");
        let text = Draft::Text("fix the flaky test".to_owned());
        assert_eq!(
            first,
            (false, text.clone(), false),
            "first read: maybe a person typing"
        );
        assert_eq!(
            before_pass,
            (false, text.clone(), false),
            "the take-over keeps nothing"
        );
        assert_eq!(second, (false, text, true), "it stood since the last look");
        assert!(forgotten, "cleared: forgotten");
    }

    /// Through the seat rule itself ([`rule`], [`judge_at`]) and the turn break ([`guards`],
    /// [`AtBreak`]): a fit Claude Code tab resting at a turn break on the volume is seated; under
    /// the owner's hold (`status hold=1`) it is held (`hold`), not seated, and a line owed it waits
    /// there (`wait:hold`), unclaimed; lifted, it is seated again. With a draft typed in its prompt
    /// it is seated at the look that first reads the draft (a person may be typing), held
    /// (`draft`) from the next look that reads the same text, its owed line waiting the same way,
    /// and seated again once the prompt is cleared. A seat rule that left the hold or the draft out
    /// seated such a tab, which, at rest, outranked a working one in the same GiB, was elected, and
    /// was refused at every turn break until the delivery window handed it on as undelivered.
    #[cfg(unix)]
    #[test]
    fn the_seat_rule_holds_a_tab_on_hold_or_with_a_standing_draft_and_its_line_waits_unclaimed() {
        use aterm_sysprobe::volumes::VolumeKey;
        let drafts = Drafts::default();
        let mut sessions = crate::session_store::SessionStore::default();
        let h = crate::session_store::test_handle(9_400);
        sessions.register(h.clone());
        let sid = h.sid.as_str().to_owned();
        let sids = [sid.clone()];
        let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
        publishes(&h, "idle", Program::Claude);
        let draw = |line: &str| {
            let rule = "\u{2500}".repeat(60);
            let col = line.chars().count() + 3;
            h.term
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .process(
                    format!(
                        "\x1b]7;file://localhost/u/held-tab\x07\x1b[2J\x1b[H{rule}\r\n\u{276f} \
                         {line}\r\n{rule}\x1b[2;{col}H"
                    )
                    .as_bytes(),
                );
        };
        // The board places the tab on the volume asked about, as a pass does.
        let volume = VolumeKey::Fsid(9_400);
        let key = volume.spelled();
        placed(&volume, Some(&[h.local_id]));
        let read = || {
            let r = rule(&store, &sids, &key, &Left::NONE, &drafts, true);
            (
                r.seated
                    .iter()
                    .map(|s| s.seat.sid.clone())
                    .collect::<Vec<_>>(),
                r.held,
            )
        };
        let hold = |on: bool| {
            assert!(crate::fabric::apply_hold_for_test(
                &*h.ctx,
                on.then(|| crate::fabric::Hold {
                    reason: "-".into(),
                    origin: "local".into(),
                })
            ));
        };
        draw("");
        let clear = read();
        hold(true);
        let on_hold = read();
        let judged_hold = judge_at(&store, &sid, &key, &drafts);
        hold(false);
        let lifted = read();
        draw("refactor the parser");
        let first = read();
        let second = read();
        let judged_draft = judge_at(&store, &sid, &key, &drafts);
        draw("");
        let cleared = read();
        placed(&volume, None);
        let seated = (vec![sid.clone()], Vec::new());
        let held = |why| (Vec::new(), vec![(sid.clone(), why)]);
        assert_eq!(clear, seated, "CONTROL: a fit tab at rest is seated");
        assert_eq!(on_hold, held(ask::HeldBy::Hold), "the owner's hold");
        assert_eq!(lifted, seated, "the hold lifted");
        assert_eq!(first, seated, "a draft read once: a person may be typing");
        assert_eq!(second, held(ask::HeldBy::Draft), "the same draft a look on");
        assert_eq!(cleared, seated, "the prompt cleared");
        assert_eq!(judged_hold, Judged::Held(ask::HeldBy::Hold));
        assert_eq!(judged_draft, Judged::Held(ask::HeldBy::Draft));
        // A line owed it waits at its turn break, unclaimed, so its time is not stretched.
        let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
        let host = ask::HostId {
            pid: 9,
            start: "start-9".to_owned(),
        };
        let now = now_ms();
        owed_at(state.path(), &key, &host, &sid, now);
        let before = ask::read(state.path());
        let owed = crate::disk_board::AskOwed {
            key: key.clone(),
            text: "the line".to_owned(),
        };
        for (judged, why) in [(judged_hold, "hold"), (judged_draft, "draft")] {
            let (unfit, held) = guards(judged, None, None);
            let at = AtBreak {
                state: state.path(),
                host: &host,
                sid: &sid,
                owed: &owed,
                now,
                unfit,
                held,
                free: 10 * GIB,
                level: Level::Short,
            };
            assert_eq!(
                at.step(
                    || panic!("typed a tab {why} holds"),
                    || panic!("measured at a tab {why} holds")
                ),
                format!("wait:{why}")
            );
        }
        assert_eq!(
            ask::read(state.path()),
            before,
            "unclaimed, and owed until the look gives it back to the episode"
        );
        assert_eq!(
            guards(Judged::Seat, None, None),
            (None, None),
            "CONTROL: a seat is typed"
        );
    }

    /// The board places sessions `locals` on volume `volume`, as a pass does; `None` takes them,
    /// and the volume, off it again. Its callers read unix paths off a terminal (`OSC 7`).
    #[cfg(unix)]
    fn placed(volume: &aterm_sysprobe::volumes::VolumeKey, locals: Option<&[u64]>) {
        use aterm_sysprobe::volumes::Reading;
        let mut b = crate::disk_board::SHELF.board_lock();
        let Some(locals) = locals else {
            if let Some(at) = b.volumes.iter().position(|r| r.reading.key == *volume) {
                b.sessions.retain(|_, row| *row != at);
                b.volumes.remove(at);
                for row in b.sessions.values_mut() {
                    if *row > at {
                        *row -= 1;
                    }
                }
            }
            return;
        };
        b.volumes.push(crate::disk_board::VolumeRow {
            reading: Reading {
                mount: PathBuf::from("/u"),
                key: volume.clone(),
                total: 2000 * GIB,
                free: 10 * GIB,
                files_free: Some(1 << 30),
                dev: 0,
                read_only: false,
            },
            thresholds: disk::Config::default().thresholds(Some(2000 * GIB)),
            level: Level::Short,
            room: None,
            as_of: std::time::Instant::now(),
            as_of_unix: 0,
            reclaimed_here: true,
        });
        let at = b.volumes.len() - 1;
        for local in locals {
            b.sessions.insert(*local, at);
        }
    }

    /// Claude Code publishes `word` for session `h`.
    #[cfg(unix)]
    fn publishes(h: &crate::session_store::SessionHandle, word: &'static str, program: Program) {
        h.ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .publish_agent(
                word,
                None,
                None,
                Some(program),
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                    fp: 1,
                },
            );
    }

    /// A tab whose turn is over with only its own background shell still running (a manager in
    /// `aterm ctl await task` on its workers) reads busy, yet its loop offers its line at that
    /// break as at an idle point, and refuses it there for a draft standing in its composer
    /// (`background fact held: draft`). The look reads the same: from the second look that reads
    /// the same text it holds the tab (`draft`), so a line owed it goes back to the episode at its
    /// host's next look, at no election's cost. The look held a draft only at `idle`, so such a
    /// pick stood owed through every refused break and was handed on as undelivered at its
    /// deadline: one of the episode's elections spent, the tab never asked again that episode
    /// once its draft was sent, and with no other seat the episode ran out and parked for its
    /// cooldown. A draft queued while the agent works mid-turn goes with its turn: it holds
    /// nothing, and a line owed it stays owed.
    #[cfg(unix)]
    #[test]
    fn a_draft_standing_at_a_break_of_the_agents_own_background_work_gives_its_pick_back() {
        use aterm_sysprobe::volumes::VolumeKey;
        let drafts = Drafts::default();
        let mut sessions = crate::session_store::SessionStore::default();
        let h = crate::session_store::test_handle(9_500);
        sessions.register(h.clone());
        let sid = h.sid.as_str().to_owned();
        let sids = [sid.clone()];
        let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
        publishes(&h, "busy", Program::Claude);
        let draw = |status: &str, line: &str| {
            let rule = "\u{2500}".repeat(60);
            let col = line.chars().count() + 3;
            h.term
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .process(
                    format!(
                        "\x1b]7;file://localhost/u/manager\x07\x1b[2J\x1b[H\u{23fa} The workers \
                         run on; I land each as it reports.\r\n{status}\r\n{rule}\r\n\u{276f} \
                         {line}\r\n{rule}\x1b[4;{col}H"
                    )
                    .as_bytes(),
                );
        };
        let rows = || {
            let t = h.term.lock().unwrap_or_else(PoisonError::into_inner);
            (0..usize::from(t.rows()))
                .map(|r| crate::claude_lights::row_text(&t, r))
                .collect::<Vec<_>>()
        };
        let background =
            "\u{273b} Crunched for 9m 27s \u{b7} done 11:07 AM \u{b7} 1 shell still running";
        let working = "\u{273b} Crunching\u{2026} (3s \u{b7} esc to interrupt)";
        let volume = VolumeKey::Fsid(9_500);
        let key = volume.spelled();
        placed(&volume, Some(&[h.local_id]));
        // Each screen read by two looks a minute apart, the same draft typed in its prompt.
        let read = |status: &str| {
            draw(status, "");
            let _ = rule(&store, &sids, &key, &Left::NONE, &drafts, true);
            draw(status, "and then land the merge");
            let screen = rows();
            let first = rule(&store, &sids, &key, &Left::NONE, &drafts, true);
            let second = rule(&store, &sids, &key, &Left::NONE, &drafts, true);
            (screen, first, second)
        };
        let (bg_rows, bg_first, bg) = read(background);
        let (mid_rows, _, mid) = read(working);
        placed(&volume, None);
        assert!(
            aterm_phase::turn_over_background(&bg_rows),
            "CONTROL: the loop's own reader takes this screen as a break of its background work: \
             {bg_rows:#?}"
        );
        assert!(
            !aterm_phase::turn_over_background(&mid_rows),
            "CONTROL: mid-turn: {mid_rows:#?}"
        );
        let seated = |r: &Rule| {
            r.seated
                .iter()
                .map(|s| s.seat.sid.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            (seated(&bg_first), bg_first.held.clone()),
            (vec![sid.clone()], Vec::new()),
            "a draft read once: a person may be typing"
        );
        assert_eq!(
            (seated(&bg), bg.held.clone()),
            (Vec::new(), vec![(sid.clone(), ask::HeldBy::Draft)]),
            "the same draft a look on, at a break its loop would offer the line at"
        );
        assert_eq!(
            (seated(&mid), mid.held.clone()),
            (vec![sid.clone()], Vec::new()),
            "a draft queued mid-turn goes with its turn"
        );
        // A line owed it, at its host's next look, as each reading leaves the lease.
        let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
        let host = ask::HostId {
            pid: 10,
            start: "start-10".to_owned(),
        };
        let now = now_ms();
        let alive = |_: &ask::HostId| true;
        let next_look = |r: &Rule| {
            owed_at(state.path(), &key, &host, &sid, now);
            let seats: Vec<ask::Seat> = r.seated.iter().map(|s| s.seat.clone()).collect();
            let look = ask::Look {
                now_ms: now + 60_000,
                host: &host,
                key: &key,
                level: Level::Short,
                free: 10 * GIB,
                floor: 25 * GIB,
                files_free: None,
                files_floor: 0,
                done: Some(ask::Done::Spent),
                seats: &seats,
                supervised: &sids,
                barred: &r.barred,
                live: &r.live,
                held: &r.held,
                reclaimed: &[],
                passing: false,
                alive: &alive,
                ahead: ask::Ahead::default(),
            };
            let said = ask::transact(state.path(), &ask::LOCK_LADDER, |l| ask::step(l, &look))
                .expect("the lease is stepped")
                .expect("nobody holds its lock");
            (
                said,
                ask::read(state.path()).entry(&key).cloned().expect("entry"),
            )
        };
        let pick = ask::Pick {
            host: host.clone(),
            sid: sid.clone(),
        };
        let (said, entry) = next_look(&bg);
        assert_eq!(
            said,
            ask::Said::Returned {
                from: pick.clone(),
                why: ask::HeldBy::Draft
            },
            "{entry:?}"
        );
        assert!(entry.picks.is_empty(), "no election spent: {entry:?}");
        assert_eq!(
            entry.returned,
            std::slice::from_ref(&sid),
            "back in the episode"
        );
        assert_eq!(entry.released, 0, "{entry:?}");
        let (said, entry) = next_look(&mid);
        assert!(
            matches!(&said, ask::Said::Owed { pick: p, .. } if *p == pick),
            "CONTROL: mid-turn, owed until its turn break: {said:?} {entry:?}"
        );
    }

    /// A manager in `aterm ctl await task` on its workers, with text left in its prompt, wakes
    /// whenever a worker reports: its background command ends, Claude Code opens a turn of the
    /// agent's own (`Background command "…" completed`), and that turn works for minutes before
    /// it goes back to its await. The text stays in the prompt throughout. The look holds the tab
    /// at the first break (`draft`), and its owed pick goes back to the episode; it holds it on
    /// through the turn the agent's own wake opened, as the text that stood at the break stands
    /// unchanged (that turn took nothing from the composer), whether or not the turn's first row
    /// is still on screen; and at the next break. So the round, with nobody else to elect, waits
    /// naming the draft, and never runs out; once the text is sent, the tab is seated and elected
    /// again. The look read a draft that stood at a break as one typed mid-turn once the agent
    /// woke: it seated the tab, the round elected it again, the next break held it, and a
    /// session goes back to the episode only once, so it was released, and with no other seat
    /// the episode ran out and parked for its cooldown, the text still in the prompt. A draft
    /// typed while the agent works mid-turn, never yet left over a break, still holds nothing.
    #[cfg(unix)]
    #[test]
    fn a_draft_that_stood_at_a_break_holds_its_tab_through_the_agents_own_wake_and_its_round_waits()
    {
        use aterm_sysprobe::volumes::VolumeKey;
        let drafts = Drafts::default();
        let mut sessions = crate::session_store::SessionStore::default();
        let h = crate::session_store::test_handle(9_700);
        sessions.register(h.clone());
        let sid = h.sid.as_str().to_owned();
        let sids = [sid.clone()];
        let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
        publishes(&h, "busy", Program::Claude);
        // Its screen: the transcript rows `above` its status row, then its composer holding `line`.
        let draw = |above: &[&str], status: &str, line: &str| {
            let rule = "\u{2500}".repeat(60);
            let col = line.chars().count() + 3;
            let row = above.len() + 3;
            let above: String = above.iter().map(|r| format!("{r}\r\n")).collect();
            h.term
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .process(
                    format!(
                        "\x1b]7;file://localhost/u/manager\x07\x1b[2J\x1b[H{above}{status}\r\n\
                         {rule}\r\n\u{276f} {line}\r\n{rule}\x1b[{row};{col}H"
                    )
                    .as_bytes(),
                );
        };
        let rows = || {
            let t = h.term.lock().unwrap_or_else(PoisonError::into_inner);
            (0..usize::from(t.rows()))
                .map(|r| crate::claude_lights::row_text(&t, r))
                .collect::<Vec<_>>()
        };
        let waits = "\u{23fa} The workers run on; I land each as it reports.";
        let woke = "\u{23fa} Background command \"aterm ctl await task @w1,@w2 since=0\" completed \
                    (exit code 0)";
        let merging = "\u{23fa} w1 landed its work; merging it and running the checks.";
        let verifying = "\u{23fa} Bash(targo --unverified verify -p aterm-gui)";
        let merged = "\u{23fa} Merged w1. Waiting on w2.";
        let background =
            "\u{273b} Crunched for 9m 27s \u{b7} done 11:07 AM \u{b7} 1 shell still running";
        let working = "\u{273b} Crunching\u{2026} (2m 3s \u{b7} esc to interrupt)";
        let draft = "and then land the merge";
        let volume = VolumeKey::Fsid(9_700);
        let key = volume.spelled();
        placed(&volume, Some(&[h.local_id]));
        // One look a minute, each reading the screen drawn before it.
        let look = |above: &[&str], status: &str, line: &str| {
            draw(above, status, line);
            let screen = rows();
            (
                screen,
                rule(&store, &sids, &key, &Left::NONE, &drafts, true),
            )
        };
        let (_, typed) = look(&[waits], background, draft);
        let (break_rows, at_break) = look(&[waits], background, draft);
        let (wake_rows, woken) = look(&[waits, woke, merging], working, draft);
        let (later_rows, later) = look(&[verifying], working, draft);
        let (_, back) = look(&[merging, merged], background, draft);
        let sent_with = format!("\u{276f} {draft}");
        let (_, sent) = look(&[merged, &sent_with], working, "");
        let fresh = "also bump the version";
        let (_, fresh_first) = look(&[merged, &sent_with], working, fresh);
        let (_, fresh_stands) = look(&[merged, &sent_with], working, fresh);
        placed(&volume, None);
        assert!(
            aterm_phase::turn_over_background(&break_rows),
            "CONTROL: a break of its background work: {break_rows:#?}"
        );
        assert!(
            !aterm_phase::turn_over_background(&wake_rows)
                && !aterm_phase::turn_over_background(&later_rows),
            "CONTROL: its own wake's turn is no break: {wake_rows:#?} {later_rows:#?}"
        );
        assert_eq!(
            (
                aterm_phase::opened_by_own_wake(&wake_rows),
                aterm_phase::opened_by_own_wake(&later_rows)
            ),
            (Some(true), None),
            "CONTROL: its own wake opened the turn, and the screen stops saying so"
        );
        let seated = |r: &Rule| {
            r.seated
                .iter()
                .map(|s| s.seat.sid.clone())
                .collect::<Vec<_>>()
        };
        let draft_held = (Vec::new(), vec![(sid.clone(), ask::HeldBy::Draft)]);
        let read = |r: &Rule| (seated(r), r.held.clone());
        assert_eq!(
            read(&typed),
            (vec![sid.clone()], Vec::new()),
            "read once: a person may be typing"
        );
        assert_eq!(read(&at_break), draft_held, "at its background break");
        assert_eq!(
            read(&woken),
            draft_held,
            "its own wake: the draft stood at the break and the turn took nothing from it"
        );
        assert_eq!(
            read(&later),
            draft_held,
            "the turn's first row scrolled off: the same text still stands"
        );
        assert_eq!(read(&back), draft_held, "back at its break");
        assert_eq!(
            read(&sent),
            (vec![sid.clone()], Vec::new()),
            "the text sent: seated"
        );
        assert_eq!(
            (read(&fresh_first), read(&fresh_stands)),
            (
                (vec![sid.clone()], Vec::new()),
                (vec![sid.clone()], Vec::new())
            ),
            "CONTROL: a draft typed mid-turn, never left over a break, goes with its turn"
        );
        // The lease, owed to it since the look that read the draft once seated it, stepped by
        // its host at each look after.
        let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
        let host = ask::HostId {
            pid: 11,
            start: "start-11".to_owned(),
        };
        let t0 = now_ms();
        owed_at(state.path(), &key, &host, &sid, t0);
        let alive = |_: &ask::HostId| true;
        let step_at = |r: &Rule, at: i64| {
            let seats: Vec<ask::Seat> = r.seated.iter().map(|s| s.seat.clone()).collect();
            let look = ask::Look {
                now_ms: at,
                host: &host,
                key: &key,
                level: Level::Short,
                free: 10 * GIB,
                floor: 25 * GIB,
                files_free: None,
                files_floor: 0,
                done: Some(ask::Done::Spent),
                seats: &seats,
                supervised: &sids,
                barred: &r.barred,
                live: &r.live,
                held: &r.held,
                reclaimed: &[],
                passing: false,
                alive: &alive,
                ahead: ask::Ahead::default(),
            };
            let said = ask::transact(state.path(), &ask::LOCK_LADDER, |l| ask::step(l, &look))
                .expect("the lease is stepped")
                .expect("nobody holds its lock");
            (
                said,
                ask::read(state.path()).entry(&key).cloned().expect("entry"),
            )
        };
        let minute = 60_000;
        let pick = ask::Pick {
            host: host.clone(),
            sid: sid.clone(),
        };
        let (said, _) = step_at(&at_break, t0 + minute);
        assert_eq!(
            said,
            ask::Said::Returned {
                from: pick.clone(),
                why: ask::HeldBy::Draft
            }
        );
        let waiting = ask::Said::Waiting {
            held: vec![sid.clone()],
            why: vec![ask::HeldBy::Draft],
        };
        let round_past = t0 + minute + ask::NOMINATE_MS;
        let (said, _) = step_at(&woken, t0 + 2 * minute);
        assert!(
            matches!(said, ask::Said::Nominating { seat: None, .. }),
            "its round gathers, the tab held: {said:?}"
        );
        for (r, at, what) in [
            (&later, round_past, "through its own wake's turn"),
            (&back, round_past + minute, "back at its break"),
        ] {
            let (said, entry) = step_at(r, at);
            assert_eq!(said, waiting, "{what}: {entry:?}");
            assert!(
                entry.picks.is_empty() && entry.released == 0,
                "{what}: no election spent, none released: {entry:?}"
            );
        }
        let (said, entry) = step_at(&sent, round_past + 2 * minute);
        assert_eq!(
            said,
            ask::Said::Elected {
                pick: pick.clone(),
                held: 0
            },
            "the text sent: elected again, {entry:?}"
        );
    }

    /// The asked tab's line marks its own checkout's builds `(this tab's)` only while no other
    /// session of this process works in that checkout, supervised or not: several Claude Code
    /// tabs in `~/aterm` is ordinary, and they build in one cache, so the line says `(this tab's
    /// checkout, shared)`, and never tells the agent a cache other sessions build in is its own.
    /// A tab in a worktree nested in the checkout works in its own checkout, and shares nothing
    /// of it. The seat's rank is unchanged: it holds what its checkout's builds hold.
    #[cfg(unix)]
    #[test]
    fn a_seat_whose_checkout_another_session_works_in_is_told_its_builds_are_shared() {
        use aterm_sysprobe::volumes::VolumeKey;
        let asked = crate::session_store::test_handle(9_600);
        let shell = crate::session_store::test_handle(9_601);
        let nested = crate::session_store::test_handle(9_602);
        let peer = crate::session_store::test_handle(9_603);
        let cd = |h: &crate::session_store::SessionHandle, dir: &str| {
            h.term
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .process(format!("\x1b]7;file://localhost{dir}\x07").as_bytes());
        };
        cd(&asked, "/u/aterm/crates/x");
        cd(&shell, "/u/aterm");
        cd(&nested, "/u/aterm/.claude/worktrees/w/crates");
        cd(&peer, "/u/aterm/crates/y");
        publishes(&asked, "idle", Program::Claude);
        publishes(&nested, "busy", Program::Claude);
        publishes(&peer, "busy", Program::Claude);
        let sid = asked.sid.as_str().to_owned();
        let peer_sid = peer.sid.as_str().to_owned();
        let volume = VolumeKey::Fsid(9_600);
        let key = volume.spelled();
        placed(
            &volume,
            Some(&[
                asked.local_id,
                shell.local_id,
                nested.local_id,
                peer.local_id,
            ]),
        );
        let holders = [
            Hog {
                built_s_ago: None,
                path: PathBuf::from("/u/Library/Caches/aterm/cargo-build.noindex/a"),
                bytes: 61 * GIB,
                bytes_partial: false,
                why: String::new(),
                serves: Some(PathBuf::from("/u/aterm")),
            },
            Hog {
                built_s_ago: None,
                path: PathBuf::from("/u/trust/build"),
                bytes: 30 * GIB,
                bytes_partial: false,
                why: String::new(),
                serves: Some(PathBuf::from("/u/trust")),
            },
        ];
        let roots = ["/u/aterm", "/u/aterm/.claude/worktrees/w", "/u/trust"];
        let root = |dir: &Path| roots.iter().any(|r| dir == Path::new(r));
        let there = |_: &Path| true;
        let left = Left {
            holders: &holders,
            home: Some(Path::new("/u")),
            root: &root,
            there: &there,
        };
        // The asked tab's seat and its line, with `with` the other live sessions and `supervised`
        // those the harness supervises.
        let seat = |with: &[&crate::session_store::SessionHandle], supervised: &[String]| {
            let mut sessions = crate::session_store::SessionStore::default();
            sessions.register(asked.clone());
            for h in with {
                sessions.register((*h).clone());
            }
            let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
            let r = rule(&store, supervised, &key, &left, &Drafts::default(), false);
            let s = r
                .seated
                .into_iter()
                .find(|s| s.seat.sid == sid)
                .expect("the asked tab is seated");
            let text = ask::line(&ask::Line {
                mount: Path::new("/System/Volumes/Data"),
                total: 1000 * GIB,
                free: 18 * GIB,
                floor: 25 * GIB,
                emergency: false,
                done: ask::Done::Spent,
                holders: &holders,
                checkout: s.checkout.as_deref(),
                shared: s.shared,
                home: Some(Path::new("/u")),
                projected: None,
            });
            (s.seat.held, s.shared, text)
        };
        let one = std::slice::from_ref(&sid);
        let alone = seat(&[], one);
        let beside_nested = seat(&[&nested], &[sid.clone(), nested.sid.as_str().to_owned()]);
        let beside_shell = seat(&[&shell, &nested], one);
        let beside_peer = seat(&[&peer], &[sid.clone(), peer_sid.clone()]);
        placed(&volume, None);
        let own = "~/aterm's builds 61.0 GiB (this tab's); $HOME/trust's builds 30.0 GiB.";
        let shared =
            "~/aterm's builds 61.0 GiB (this tab's checkout, shared); $HOME/trust's builds 30.0 GiB.";
        assert_eq!((alone.0, alone.1), (61 * GIB, false), "CONTROL: alone");
        assert!(alone.2.contains(own), "{}", alone.2);
        assert_eq!(
            (beside_nested.0, beside_nested.1),
            (61 * GIB, false),
            "a worktree nested in the checkout is its own checkout"
        );
        assert!(beside_nested.2.contains(own), "{}", beside_nested.2);
        assert_eq!(
            (beside_shell.0, beside_shell.1),
            (61 * GIB, true),
            "a shell in the same checkout, unsupervised: shared, and the seat ranks as before"
        );
        assert!(beside_shell.2.contains(shared), "{}", beside_shell.2);
        assert!(
            !beside_shell.2.contains("(this tab's)"),
            "{}",
            beside_shell.2
        );
        assert_eq!(
            (beside_peer.0, beside_peer.1),
            (61 * GIB, true),
            "another Claude Code tab working in the same checkout"
        );
        assert!(beside_peer.2.contains(shared), "{}", beside_peer.2);
        for text in [&alone.2, &beside_shell.2, &beside_peer.2] {
            assert!(text.chars().count() <= ask::LINE_CAP, "{text}");
        }
    }

    /// The composer's first glyph is read past a caret the program draws over it, as the fenced
    /// write reads it: Claude Code draws its suggestion's first glyph inverse and the rest dim, and
    /// a typed draft's first glyph inverse and the rest plain.
    #[test]
    fn the_composers_first_glyph_is_read_past_a_drawn_caret() {
        use aterm_core::grid::CellFlags;
        let at = |cells: Vec<(usize, CellFlags)>| {
            move |col: usize| cells.iter().find(|(c, _)| *c == col).map(|(_, f)| *f)
        };
        let (dim, plain, inverse) = (CellFlags::DIM, CellFlags::empty(), CellFlags::INVERSE);
        assert!(glyph_dim(at(vec![(2, dim)])), "dim at the caret");
        assert!(
            glyph_dim(at(vec![(2, inverse), (3, dim)])),
            "a suggestion under a drawn caret"
        );
        assert!(
            !glyph_dim(at(vec![(2, inverse), (3, plain)])),
            "a typed draft under a drawn caret"
        );
        assert!(
            glyph_dim(at(vec![
                (2, inverse.union(CellFlags::WIDE)),
                (3, plain),
                (4, dim),
            ])),
            "past a wide glyph"
        );
        assert!(!glyph_dim(at(vec![(2, plain)])));
        assert!(!glyph_dim(at(Vec::new())), "no cell");
    }

    /// A look that runs the episode out raises the owner's `disk.low` itself only when it reads
    /// the volume short against its own floor. A look that read it at or above its floor (an
    /// episode another host's higher floor held open) raised "the home volume is still short",
    /// and the next look, finding it plenty, cleared it: a false alert kept as a message record.
    #[test]
    fn a_look_that_runs_the_episode_out_raises_disk_low_only_reading_the_volume_short() {
        let out = ask::Said::Out {
            last: Some(ask::Miss::NothingFreed),
            asked: true,
        };
        let spent = Some(ask::Done::Spent);
        let floor = 32 << 30;
        assert!(out_raises_low(&out, Level::Short, floor, spent));
        assert!(out_raises_low(&out, Level::Emergency, floor, spent));
        for level in [Level::Low, Level::Ok, Level::Unknown] {
            assert!(
                !out_raises_low(&out, level, floor, spent),
                "{level:?}: not read short against this host's floor"
            );
        }
        assert!(
            !out_raises_low(&out, Level::Short, 0, spent),
            "the floor at 0"
        );
        assert!(
            !out_raises_low(&out, Level::Short, floor, Some(ask::Done::CleanupOff)),
            "removal off: its own line"
        );
        assert!(!out_raises_low(
            &ask::Said::Spent,
            Level::Short,
            floor,
            spent
        ));
    }

    /// The look reads a box as escalated by the supervisor's own attention entry on the session,
    /// which its loop writes as it asks the owner, and not by the badge of a usage limit it
    /// raised, which a wall the board holds says instead.
    #[test]
    fn a_look_reads_a_box_its_loop_escalated_and_not_a_limits_badge() {
        let mut sessions = crate::session_store::SessionStore::default();
        let h = crate::session_store::test_handle(9_200);
        sessions.register(h.clone());
        let sid = h.sid.as_str().to_owned();
        let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
        h.ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .publish_agent(
                "prompt",
                None,
                None,
                Some(Program::Claude),
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                    fp: 1,
                },
            );
        let read = |badge: Option<&str>| {
            h.ctx
                .meta
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .set_attention_owned(
                    aterm_agent::supervise::ATTENTION_OWNER,
                    badge.map(str::to_owned),
                )
                .expect("a slot");
            let r = read_sessions(&store, std::slice::from_ref(&sid), Some(&sid))
                .pop()
                .expect("the session");
            (r.word, r.escalated)
        };
        assert_eq!(read(None), ("prompt", false), "CONTROL: nobody asked");
        assert_eq!(read(Some("approve: rm -rf target")), ("prompt", true));
        let limit = format!("{} resets 3pm", aterm_agent::supervise::ATTENTION_PREFIX);
        assert_eq!(read(Some(&limit)), ("prompt", false), "a limit's badge");
    }

    #[test]
    fn a_look_and_a_turn_break_bar_a_done_an_exited_and_an_unfit_session_alike() {
        use crate::task_record::{TaskMark, mark_session_task};
        let mut sessions = crate::session_store::SessionStore::default();
        let handles: Vec<_> = (0..5u64)
            .map(|local| {
                let h = crate::session_store::test_handle(9_000 + local);
                h.ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .set_agent_reader_program_for_test(Some(Program::Claude));
                sessions.register(h.clone());
                h
            })
            .collect();
        let sid = |i: usize| handles[i].sid.as_str().to_owned();
        let mark = |i: usize, state: TaskState| {
            mark_session_task(&handles[i].ctx, handles[i].local_id, &TaskMark::new(state))
                .expect("unfenced");
        };
        // 0: its task landed; 1: its agent exited, so it left the supervised roster; 2: its
        // worker found it unfit; 3: a manager's worker, its task working, its turn break not yet
        // found; 4: no task record, not supervised.
        mark(0, TaskState::Done);
        mark(1, TaskState::Exited);
        mark(3, TaskState::Working);
        let sids = [sid(0), sid(2), sid(3)];
        let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
        crate::disk_board::SHELF.board_lock().unfit.insert(
            sid(2),
            crate::disk_board::Unfit {
                own: None,
                theirs: Some("exchange"),
            },
        );
        let key = "fsid:9000";
        let found = rule(&store, &sids, key, &Left::NONE, &Drafts::default(), false);
        let bars: Vec<Option<&'static str>> =
            (0..5).map(|i| bar_at(&store, &sid(i), key)).collect();
        crate::disk_board::SHELF.board_lock().unfit.remove(&sid(2));
        let mut barred = found.barred.clone();
        barred.sort();
        let mut want = vec![sid(0), sid(1), sid(2), sid(3)];
        want.sort();
        assert_eq!(barred, want, "what the look releases an owed line of");
        assert!(
            found.seated.is_empty(),
            "no directory on the volume was read"
        );
        for i in 0..5 {
            assert!(found.live.contains(&sid(i)), "every live session: {i}");
        }
        assert_eq!(
            bars,
            [
                Some("task-done"),
                Some("task-exited"),
                Some("exchange"),
                Some("managed"),
                None
            ],
            "the turn break bars what the look bars"
        );
        assert_eq!(bar_at(&store, "s-not-here", key), None);
    }

    #[test]
    fn a_session_holds_what_its_own_checkout_holds_and_a_line_typed_here_is_never_typed_twice() {
        let hog = |path: &str, gib: u64, serves: Option<&str>| Hog {
            built_s_ago: None,
            path: PathBuf::from(path),
            bytes: gib << 30,
            bytes_partial: false,
            why: String::new(),
            serves: serves.map(PathBuf::from),
        };
        let holders = [
            hog("/c/aterm-build", 40, Some("/u/aterm")),
            hog("/u/aterm/target", 2, Some("/u/aterm")),
            hog("/u/trust/build", 90, Some("/u/trust")),
            hog("/u/loose/target", 7, None),
            hog(
                "/u/aterm/.claude/worktrees/w/target",
                3,
                Some("/u/aterm/.claude/worktrees/w"),
            ),
        ];
        let roots = ["/u/aterm", "/u/aterm/.claude/worktrees/w", "/u/trust"];
        let root = |dir: &Path| roots.iter().any(|r| dir == Path::new(r));
        let there = |_: &Path| true;
        let home = Some(Path::new("/u"));
        let held_under = |cwd: &str| {
            held_in(
                &holders,
                checkout_of(&holders, Path::new(cwd), home, &root, &there).as_deref(),
            )
        };
        assert_eq!(held_under("/u/aterm/crates/x"), 42 << 30);
        assert_eq!(held_under("/u/atermx"), 0);
        assert_eq!(held_under("/u/loose"), 0);
        // A worktree nested in a checkout is its own checkout, not the one around it.
        let nested = Path::new("/u/aterm/.claude/worktrees/w/crates");
        assert_eq!(held_under("/u/aterm/.claude/worktrees/w/crates"), 3 << 30);
        assert_eq!(
            checkout_of(&holders, nested, home, &root, &there).as_deref(),
            Some(Path::new("/u/aterm/.claude/worktrees/w"))
        );
        assert_eq!(
            checkout_of(&holders, Path::new("/u/aterm"), home, &root, &there).as_deref(),
            Some(Path::new("/u/aterm"))
        );
        let key = "dev:typed-here";
        assert!(!typed_here(key, "s-a", 1_000));
        TYPED.lock().unwrap_or_else(PoisonError::into_inner).insert(
            key.to_owned(),
            Typed {
                sid: "s-a".to_owned(),
                at_ms: 1_000,
                free: 0,
            },
        );
        assert!(typed_here(key, "s-a", 1_000 + ask::COOLDOWN_MS - 1));
        assert!(!typed_here(key, "s-b", 2_000));
        assert!(!typed_here(key, "s-a", 1_000 + ask::COOLDOWN_MS));
    }

    #[test]
    fn a_worktree_nested_in_a_checkout_with_no_holder_of_its_own_holds_nothing_of_the_checkouts() {
        let hog = |path: &str, gib: u64, serves: &str| Hog {
            built_s_ago: None,
            path: PathBuf::from(path),
            bytes: gib << 30,
            bytes_partial: false,
            why: String::new(),
            serves: Some(PathBuf::from(serves)),
        };
        // The pass's holders: the enclosing checkout's builds, and none of the worktree's (past
        // the five largest, or its idle build already reclaimed).
        let holders = [
            hog(
                "/u/Library/Caches/aterm/cargo-build.noindex/a",
                61,
                "/u/aterm",
            ),
            hog("/u/trust/build", 30, "/u/trust"),
        ];
        let roots = ["/u/aterm", "/u/aterm/.claude/worktrees/v", "/u/trust"];
        let looked = Mutex::new(Vec::<PathBuf>::new());
        let root = |dir: &Path| {
            looked
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(dir.to_path_buf());
            roots.iter().any(|r| dir == Path::new(r))
        };
        let there = |_: &Path| true;
        let home = Path::new("/u");
        let looked_at =
            || std::mem::take(&mut *looked.lock().unwrap_or_else(PoisonError::into_inner));
        let said = |checkout: Option<&Path>| {
            ask::line(&ask::Line {
                mount: Path::new("/System/Volumes/Data"),
                total: 1000 << 30,
                free: 18 << 30,
                floor: 25 << 30,
                emergency: false,
                done: ask::Done::Spent,
                holders: &holders,
                checkout,
                shared: false,
                home: Some(home),
                projected: None,
            })
        };
        let wt = Path::new("/u/aterm/.claude/worktrees/v/crates/x");
        let checkout = checkout_of(&holders, wt, Some(home), &root, &there);
        assert_eq!(
            checkout.as_deref(),
            Some(Path::new("/u/aterm/.claude/worktrees/v"))
        );
        assert_eq!(held_in(&holders, checkout.as_deref()), 0);
        let text = said(checkout.as_deref());
        assert!(text.contains("~/aterm's builds 61.0 GiB"), "{text}");
        assert!(!text.contains("(this tab's)"), "{text}");
        // It looks only between the tab's directory and the checkout around it, and stops at the
        // first root.
        assert_eq!(
            looked_at(),
            [
                PathBuf::from("/u/aterm/.claude/worktrees/v/crates/x"),
                PathBuf::from("/u/aterm/.claude/worktrees/v/crates"),
                PathBuf::from("/u/aterm/.claude/worktrees/v"),
            ]
        );
        // The worktree's root itself is its own too.
        let at_root = checkout_of(
            &holders,
            Path::new("/u/aterm/.claude/worktrees/v"),
            Some(home),
            &root,
            &there,
        );
        assert_eq!(held_in(&holders, at_root.as_deref()), 0);
        looked_at();
        // A tab in the checkout itself still holds its builds, and is told they are its own.
        for cwd in ["/u/aterm", "/u/aterm/crates/x"] {
            let checkout = checkout_of(&holders, Path::new(cwd), Some(home), &root, &there);
            assert_eq!(checkout.as_deref(), Some(Path::new("/u/aterm")), "{cwd}");
            assert_eq!(held_in(&holders, checkout.as_deref()), 61 << 30, "{cwd}");
            let text = said(checkout.as_deref());
            assert!(
                text.contains("~/aterm's builds 61.0 GiB (this tab's)"),
                "{text}"
            );
            assert!(!looked_at().contains(&PathBuf::from("/u/aterm")), "{cwd}");
        }
        // No holder serves a checkout around the tab: nothing is read, and nothing is its own.
        assert_eq!(
            checkout_of(&holders, Path::new("/u/other/x"), Some(home), &root, &there),
            None
        );
        assert!(looked_at().is_empty());
        // Nothing above `$HOME` is read for a root.
        let wide = [hog("/c/root-build", 5, "/")];
        assert_eq!(
            checkout_of(
                &wide,
                Path::new("/h/u/p/q"),
                Some(Path::new("/h/u")),
                &root,
                &there
            )
            .as_deref(),
            Some(Path::new("/"))
        );
        assert_eq!(
            looked_at(),
            [
                PathBuf::from("/h/u/p/q"),
                PathBuf::from("/h/u/p"),
                PathBuf::from("/h/u")
            ]
        );
    }

    #[test]
    fn a_worktree_removed_under_its_session_holds_nothing_of_the_checkout_around_it() {
        // ~/aterm's builds are the one holder left; the tab's shell last said it works in a
        // worktree nested in ~/aterm that has since been removed (its session landed it), so no
        // `.git` is left to find between its directory and ~/aterm.
        let holders = [Hog {
            built_s_ago: None,
            path: PathBuf::from("/u/Library/Caches/aterm/cargo-build.noindex/a"),
            bytes: 61 << 30,
            bytes_partial: false,
            why: String::new(),
            serves: Some(PathBuf::from("/u/aterm")),
        }];
        let roots = ["/u/aterm", "/u/aterm/.claude/worktrees/v"];
        let root = |dir: &Path| roots.iter().any(|r| dir == Path::new(r));
        let gone = Path::new("/u/aterm/.claude/worktrees/gone");
        let asked = Mutex::new(Vec::<PathBuf>::new());
        let there = |dir: &Path| {
            asked
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(dir.to_path_buf());
            !dir.starts_with(gone) && !dir.starts_with("/u/aterm/.claude/worktrees/v/tmp")
        };
        let asked_of =
            || std::mem::take(&mut *asked.lock().unwrap_or_else(PoisonError::into_inner));
        let home = Path::new("/u");
        let of = |cwd: &str| checkout_of(&holders, Path::new(cwd), Some(home), &root, &there);
        for cwd in [
            "/u/aterm/.claude/worktrees/gone/crates",
            "/u/aterm/.claude/worktrees/gone",
        ] {
            let checkout = of(cwd);
            assert_eq!(checkout, None, "{cwd}");
            assert_eq!(held_in(&holders, checkout.as_deref()), 0, "{cwd}");
            let text = ask::line(&ask::Line {
                mount: Path::new("/System/Volumes/Data"),
                total: 1000 << 30,
                free: 18 << 30,
                floor: 25 << 30,
                emergency: false,
                done: ask::Done::Spent,
                holders: &holders,
                checkout: checkout.as_deref(),
                shared: false,
                home: Some(home),
                projected: None,
            });
            assert!(text.contains("~/aterm's builds 61.0 GiB"), "{text}");
            assert!(!text.contains("(this tab's)"), "{text}");
            // Only the tab's own directory is asked, once the walk found no root.
            assert_eq!(asked_of(), [PathBuf::from(cwd)], "{cwd}");
        }
        // A directory removed inside a worktree that is still there is that worktree's: its
        // `.git` is found, and nothing more is asked.
        assert_eq!(
            of("/u/aterm/.claude/worktrees/v/tmp/x").as_deref(),
            Some(Path::new("/u/aterm/.claude/worktrees/v"))
        );
        assert!(asked_of().is_empty());
        // A tab whose directory is there, in ~/aterm or in ~/aterm itself, still holds its builds.
        assert_eq!(
            of("/u/aterm/crates/x").as_deref(),
            Some(Path::new("/u/aterm"))
        );
        assert_eq!(asked_of(), [PathBuf::from("/u/aterm/crates/x")]);
        assert_eq!(of("/u/aterm").as_deref(), Some(Path::new("/u/aterm")));
        assert!(asked_of().is_empty());
    }

    #[test]
    fn a_checkouts_root_is_read_off_its_git_directory_or_a_worktrees_git_file() {
        let dir = aterm_tempfile::TempDir::new().expect("owned checkouts");
        let repo = dir.path().join("repo");
        let wt = repo.join(".claude").join("worktrees").join("w");
        std::fs::create_dir_all(repo.join(".git")).expect("the checkout's .git");
        std::fs::create_dir_all(wt.join("crates")).expect("the worktree");
        std::fs::write(wt.join(".git"), "gitdir: ../../../.git/worktrees/w\n")
            .expect("the worktree's .git file");
        assert!(checkout_root(&repo));
        assert!(checkout_root(&wt));
        assert!(!checkout_root(&wt.join("crates")));
        let holders = [Hog {
            built_s_ago: None,
            path: repo.join("target"),
            bytes: 61 << 30,
            bytes_partial: false,
            why: String::new(),
            serves: Some(repo.clone()),
        }];
        let of = |cwd: &Path| checkout_of(&holders, cwd, None, &checkout_root, &dir_there);
        let checkout = of(&wt.join("crates"));
        assert_eq!(checkout.as_deref(), Some(wt.as_path()));
        assert_eq!(held_in(&holders, checkout.as_deref()), 0);
        std::fs::create_dir_all(repo.join("crates")).expect("the checkout's own directory");
        let checkout = of(&repo.join("crates"));
        assert_eq!(held_in(&holders, checkout.as_deref()), 61 << 30);
        // The worktree removed under its session: no `.git` is left to find, and its directory is
        // gone, so it is not read as the checkout around it.
        assert!(dir_there(&wt.join("crates")));
        std::fs::remove_dir_all(&wt).expect("the worktree removed");
        assert!(!dir_there(&wt.join("crates")));
        assert_eq!(of(&wt.join("crates")), None);
        assert_eq!(of(&wt), None);
        assert_eq!(of(&repo).as_deref(), Some(repo.as_path()));
    }

    /// A lease in `state` that owes host `host`'s session `sid` its line on volume `key`, elected
    /// at `now`.
    fn owed_at(state: &Path, key: &str, host: &ask::HostId, sid: &str, now: i64) {
        let pick = ask::Pick {
            host: host.clone(),
            sid: sid.to_owned(),
        };
        let lease = ask::Lease {
            schema: 0,
            volumes: vec![ask::Entry {
                key: key.to_owned(),
                episode_ms: Some(now - ask::NOMINATE_MS),
                done: Some(ask::Done::Spent),
                phase: ask::Phase::Owed {
                    pick: pick.clone(),
                    elected_ms: now,
                    until_ms: now + ask::DELIVER_MS,
                    orphan_ms: None,
                },
                picks: vec![pick],
                ..ask::Entry::default()
            }],
        };
        ask::transact(state, &ask::LOCK_LADDER, |l| *l = lease)
            .expect("the lease is written")
            .expect("nobody holds its lock");
    }

    const GIB: u64 = 1 << 30;

    #[test]
    fn a_typed_line_records_the_free_figure_read_as_it_went_not_the_boards() {
        let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
        let host = ask::HostId {
            pid: 7,
            start: "start-7".to_owned(),
        };
        let now = now_ms();
        for (key, measured, recorded) in [
            ("dev:free-measured", Some(40 * GIB), 40 * GIB),
            ("dev:free-unmeasured", None, 10 * GIB),
        ] {
            owed_at(state.path(), key, &host, "s-fresh", now);
            let owed = crate::disk_board::AskOwed {
                key: key.to_owned(),
                text: "the line".to_owned(),
            };
            let at = AtBreak {
                state: state.path(),
                host: &host,
                sid: "s-fresh",
                owed: &owed,
                now,
                unfit: None,
                held: None,
                free: 10 * GIB,
                level: Level::Short,
            };
            assert_eq!(at.step(|| "typed".to_owned(), || measured), "typed");
            let lease = ask::read(state.path());
            assert!(
                matches!(
                    &lease.entry(key).expect("entry").phase,
                    ask::Phase::Asked { free_at, best_free, .. }
                        if *free_at == recorded && *best_free == recorded
                ),
                "{key}: the rise is measured from the line's own reading, the board's a fallback: \
                 {lease:?}"
            );
            assert_eq!(typed_last(key).map(|t| t.free), Some(recorded), "{key}");
        }
    }

    #[test]
    fn a_turn_break_the_loop_holds_types_nothing_and_claims_nothing() {
        let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
        let host = ask::HostId {
            pid: 8,
            start: "start-8".to_owned(),
        };
        let now = now_ms();
        let key = "dev:held-break";
        owed_at(state.path(), key, &host, "s-held", now);
        let before = ask::read(state.path());
        let owed = crate::disk_board::AskOwed {
            key: key.to_owned(),
            text: "the line".to_owned(),
        };
        let at = |unfit: Option<&'static str>, held: Option<&'static str>| AtBreak {
            state: state.path(),
            host: &host,
            sid: "s-held",
            owed: &owed,
            now,
            unfit,
            held,
            free: 10 * GIB,
            level: Level::Short,
        };
        for why in ["person", "exchange", "managed"] {
            assert_eq!(
                at(None, Some(why)).step(
                    || panic!("typed at a turn break {why} holds"),
                    || panic!("measured at a turn break {why} holds")
                ),
                format!("wait:{why}")
            );
        }
        assert_eq!(
            ask::read(state.path()),
            before,
            "unclaimed, so its time is not stretched, and still owed"
        );
        assert_eq!(
            at(Some("task-done"), Some("person"))
                .step(|| panic!("typed a barred session"), || None),
            "released:task-done",
            "a session the seat rule bars is released whatever holds its turn break"
        );
        assert!(
            !matches!(
                ask::read(state.path()).entry(key).map(|e| &e.phase),
                Some(ask::Phase::Owed { .. })
            ),
            "released"
        );
    }

    /// What a session's worker found of the session itself at its last turn break (its task
    /// finished, nobody gave it one) holds only until it works again, as a session that works was
    /// given something to do: a look while its agent reads busy seats it. Whose it is (the
    /// person's own exchange, a manager's wait) holds across its turns until its next turn break.
    /// A finding kept mid-turn barred a tab through the whole of the turn its first task started,
    /// so an episode that opened then found nobody to ask, said only the owner could free the
    /// space, and stayed out for its cooldown.
    #[test]
    fn a_finding_of_the_session_itself_holds_until_it_works_and_whose_it_is_across_turns() {
        use crate::disk_board::Unfit;
        let own = |why| Unfit {
            own: Some(why),
            theirs: None,
        };
        for why in ["taskless", "finished", "autopilot-off"] {
            assert_eq!(
                own(why).bars(false),
                Some(why),
                "resting, or held at a wall"
            );
            assert_eq!(own(why).bars(true), None, "{why} while it works: seated");
        }
        let theirs = Unfit {
            own: None,
            theirs: Some("exchange"),
        };
        assert_eq!(theirs.bars(false), Some("exchange"));
        assert_eq!(theirs.bars(true), Some("exchange"), "theirs while it works");
        let both = Unfit {
            own: Some("taskless"),
            theirs: Some("managed"),
        };
        assert_eq!(both.bars(false), Some("taskless"));
        assert_eq!(both.bars(true), Some("managed"));
        assert_eq!(Unfit::default().bars(false), None);
        // Through the look: the same tab found taskless, at rest and then working.
        let mut sessions = crate::session_store::SessionStore::default();
        let h = crate::session_store::test_handle(9_100);
        sessions.register(h.clone());
        let sid = h.sid.as_str().to_owned();
        let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
        let key = "fsid:9100";
        let reads = |word: &'static str, seq: u64| {
            h.ctx
                .timeline
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .publish_agent(
                    word,
                    None,
                    None,
                    Some(Program::Claude),
                    crate::session_timeline::AgentStamp {
                        generation: crate::control::ScreenGen { epoch: 1, seq },
                        fp: seq,
                    },
                );
            bar_at(&store, &sid, key)
        };
        crate::disk_board::SHELF
            .board_lock()
            .unfit
            .insert(sid.clone(), own("taskless"));
        let resting = reads("idle", 1);
        let working = reads("busy", 2);
        crate::disk_board::SHELF.board_lock().unfit.remove(&sid);
        assert_eq!(working, None, "given a task since, it may be asked");
        assert_eq!(resting, Some("taskless"), "resting where it was found");
    }

    /// A look that reads the volume clear takes no lock only while the lease holds the last line
    /// this process typed on it ([`look_rests`]). A line whose record a full disk lost, and whose
    /// episode another host ended as the space came back, steps the lease at this process's next
    /// look, which records it, so its cooldown holds the next shortage shut. The look before
    /// rested on such a lease, as no episode was open, and the next shortage asked a second agent
    /// inside the cooldown.
    #[test]
    fn a_clear_look_steps_a_lease_that_lacks_this_processs_last_line_and_rests_once_it_holds_it() {
        let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
        let key = "dev:rests-unrecorded";
        let host = ask::HostId {
            pid: 1,
            start: String::new(),
        };
        let at = 1_000_000;
        assert!(
            look_rests(state.path(), key, Level::Ok, false),
            "nothing to step"
        );
        assert!(!look_rests(state.path(), key, Level::Short, false), "short");
        TYPED.lock().unwrap_or_else(PoisonError::into_inner).insert(
            key.to_owned(),
            Typed {
                sid: "s-a".to_owned(),
                at_ms: at,
                free: 0,
            },
        );
        // An earlier line of the volume's, its episode ended: no episode open, the lease still
        // without the line this process typed since.
        ask::transact(state.path(), &ask::LOCK_LADDER, |l| {
            ask::typed(l, key, &host, "s-b", at - 60_000, 0);
        })
        .expect("written")
        .expect("locked");
        let lacks = look_rests(state.path(), key, Level::Ok, false);
        ask::transact(state.path(), &ask::LOCK_LADDER, |l| {
            ask::typed_unrecorded(l, key, &host, "s-a", at, 0)
        })
        .expect("written")
        .expect("locked");
        let holds = look_rests(state.path(), key, Level::Ok, false);
        TYPED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key);
        assert!(!lacks, "the lease lacks the line: the look steps it");
        assert!(holds, "recorded: the look rests");
        assert_eq!(
            ask::read(state.path())
                .entry(key)
                .and_then(|e| e.last_ask_ms),
            Some(at),
            "its cooldown runs from the lost line"
        );
    }

    #[test]
    fn a_filling_note_is_said_when_it_changes_and_its_end_once() {
        let last = Mutex::new(None);
        let told = std::sync::Arc::new(Mutex::new(Vec::new()));
        let sink = std::sync::Arc::clone(&told);
        let note = move |n: HostNote| sink.lock().unwrap_or_else(PoisonError::into_inner).push(n);
        let filling = |eta_s: i64, needed: bool| HostNote::DiskFilling {
            needed,
            projected_only: !needed,
            free: 75 << 30,
            floor: 47 << 30,
            rate_bps: Some(39 << 20),
            eta_s: Some(eta_s),
            said: None,
        };
        say_filling(&last, &note, None);
        assert!(
            told.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty(),
            "no end before a start"
        );
        say_filling(&last, &note, Some(filling(780, false)));
        say_filling(&last, &note, Some(filling(780, false)));
        say_filling(&last, &note, Some(filling(720, false)));
        say_filling(&last, &note, Some(filling(720, true)));
        say_filling(&last, &note, None);
        say_filling(&last, &note, None);
        let told = told.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert_eq!(told.len(), 4, "{told:#?}");
        assert!(matches!(
            told[2],
            HostNote::DiskFilling { needed: true, .. }
        ));
        assert_eq!(told[3], HostNote::DiskFillingOver);
    }

    #[test]
    fn a_projected_look_never_rests() {
        let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
        let key = "dev:rests-projected";
        assert!(look_rests(state.path(), key, Level::Low, false));
        assert!(
            !look_rests(state.path(), key, Level::Low, true),
            "a crossing projected steps the lease, so an episode opens before the floor"
        );
    }

    #[test]
    fn a_spawn_failure_is_said_as_one_plain_word() {
        assert_eq!(
            spawn_word("no window to host the session"),
            "no-window-to-host-the-session"
        );
        assert_eq!(
            spawn_word("`claude` not found: $PATH"),
            "claude-not-found-path"
        );
        assert_eq!(spawn_word("\u{1b}[31m"), "31m");
        assert_eq!(spawn_word("!!!"), "unknown");
        assert!(spawn_word(&"x ".repeat(100)).len() <= 48);
    }

    #[test]
    fn the_filling_line_says_the_ask_then_the_worker() {
        let host = ask::HostId {
            pid: 7,
            start: "s".to_owned(),
        };
        let mut entry = ask::Entry::default();
        assert_eq!(filling_said(None, Some(&entry), &[]), None);
        assert_eq!(
            filling_said(Some("asked @a"), None, &[]).as_deref(),
            Some("asked @a")
        );
        entry.worker = Some(ask::Worker {
            host,
            at_ms: 0,
            state: ask::WorkerState::Running {
                sid: "s-w".to_owned(),
                started_ms: 0,
                free_at: 0,
                best_free: 0,
            },
            orphan_ms: None,
            free_at_claim: None,
        });
        assert_eq!(
            filling_said(Some("asked @a"), Some(&entry), &[]).as_deref(),
            Some("asked @a; aterm's disk worker @s-w is freeing space")
        );
        let report = report_lines(
            "Nothing left: freed ~/Library/Caches/pip 3.1 GiB\n\
             - disk-free filling: a Docker VM image in ~/Library/Containers grows 30 GiB/h\n\
             disk-free owner-only: the VM image \u{1b}[31m`rm -rf ~`\n",
        );
        assert_eq!(
            report,
            [
                "disk-free filling: a Docker VM image in ~/Library/Containers grows 30 GiB/h",
                "disk-free owner-only: the VM image 31m rm -rf ~",
            ]
        );
        assert!(
            filling_said(None, None, &report)
                .is_some_and(|s| s.starts_with("its worker said disk-free filling:"))
        );
        assert!(report_lines("no report here").is_empty());
    }
}
