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
    ///
    /// A folder with no canonical path starts none: without the worker folder as its trusted
    /// folder, neither the disk-worker rule's fence nor the next look's check for an earlier
    /// worker ([`earlier_worker`]) would know it.
    pub(crate) fn start_disk_worker(&mut self, task: String, folder: &Path) {
        let Some(trusted) = folder
            .to_str()
            .and_then(crate::spawn_folder::canonical_folder)
        else {
            aterm_log::warn!(
                "harness disk watch: the disk worker did not start: its folder has no canonical path"
            );
            worker_spawned(Err("no-folder".to_owned()));
            return;
        };
        let order = crate::task_launch::TaskOrder {
            task: task.into_bytes(),
            agent: Program::Claude,
            model: None,
            caller: None,
            manager_node: None,
            trusted_cwd: Some(trusted),
            fact_line: false,
            autopilot: aterm_agent::supervise::Autopilot::Land,
            chrome: false,
            program: None,
        };
        let booking = crate::control::control_media::SpawnBooking {
            cwd: folder.to_str().map(str::to_string),
            split: None,
            raise: false,
            admission: crate::tab_spawn::SpawnAdmission::Plain,
            fleet: false,
        };
        let plan = match self.resolve_spawn(crate::control::SpawnAim::Front, Some(order)) {
            Ok(plan) => plan,
            Err(why) => {
                aterm_log::warn!("harness disk watch: the disk worker did not start: {why}");
                worker_spawned(Err(spawn_word(&why)));
                return;
            }
        };
        let _act = crate::tab_marks::ActScope::enter(
            crate::session_store::ExitActor::Harness,
            "free-disk",
        );
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

    /// The disk worker `sid` ran out of its time ([`ask::WORKER_MS`]): a task still working fails
    /// `why=expired`, and its Autopilot goes Off, written by aterm, so Land types nothing more into
    /// it.
    pub(crate) fn disk_worker_expired(&mut self, sid: &str) {
        self.mark_disk_worker_expired(sid);
        if self.worker_autopilot_off(sid) {
            aterm_log::info!(
                "harness disk watch: the disk worker @{sid} ran out of its time; its Autopilot is \
                 Off"
            );
        }
    }

    /// [`Self::disk_worker_expired`]'s mark, on the window's thread, so it waits on no update's
    /// fence.
    fn mark_disk_worker_expired(&self, sid: &str) {
        let handle = {
            let store = self.store.read().unwrap_or_else(PoisonError::into_inner);
            store
                .live_handles()
                .find(|h| h.sid.as_str() == sid)
                .map(|h| (h.local_id, h.ctx.clone()))
        };
        let Some((local, ctx)) = handle else {
            return;
        };
        let mut mark = crate::task_record::TaskMark::new(TaskState::Failed);
        mark.why = Some(crate::task_record::WHY_EXPIRED);
        // A stalled worker runs out of its time too (a stall is no end, `task_seen`): it ends
        // failed, and the deadline's ask it raised goes with it, as nobody owes it an answer now.
        let running = |r: Option<&crate::task_record::TaskRecord>, _: Option<&str>| {
            r.is_some_and(|r| matches!(r.state, TaskState::Working | TaskState::Stalled))
        };
        if crate::task_record::mark_session_task_with(
            &ctx,
            local,
            Some(&mark),
            crate::task_record::AttentionEdit::TakeDown(
                crate::harness_host::deadline_attention_stands,
            ),
            std::time::Duration::ZERO,
            running,
        )
        .is_err()
        {
            aterm_log::warn!(
                "harness disk watch: the disk worker @{sid}'s record does not say it expired: an \
                 update is committing"
            );
        }
    }

    /// Session `sid`'s Autopilot turned Off, written by aterm, where `sid` is a live session of
    /// this process: whether it was. The disk worker stops so when it runs out of its time
    /// ([`Self::disk_worker_expired`]).
    pub(crate) fn worker_autopilot_off(&mut self, sid: &str) -> bool {
        let local = {
            let store = self.store.read().unwrap_or_else(PoisonError::into_inner);
            store
                .live_handles()
                .find(|h| h.sid.as_str() == sid)
                .map(|h| h.local_id)
        };
        let Some(local) = local else {
            return false;
        };
        self.write_autopilot_batch(
            &[(local, Some(aterm_agent::supervise::Autopilot::Off), None)],
            aterm_slog::Actor::Aterm,
            None,
        );
        true
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
    let read = crate::logging::open_regular_nonblock_with_meta(&path).and_then(|(f, meta)| {
        let cap = usize::try_from(meta.len().min(REPORT_MAX_BYTES).saturating_add(1)).unwrap_or(0);
        text.reserve(cap);
        f.take(REPORT_MAX_BYTES).read_to_string(&mut text)
    });
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
    #[cfg(unix)]
    crate::control_auth::ensure_private_dir(&dir).ok()?;
    #[cfg(not(unix))]
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// What session `sid`'s task record says, for the disk worker this process runs
/// ([`ask::Ahead::task`]): `None` where no live session of this process is `sid`, or it has no
/// task record. A stall reads working: a worker stopped at a screen, a sign-in or an install may
/// move on once someone acts, and the lease's time bound ends one that does not
/// ([`HostNote::DiskWorkerExpired`]).
fn task_seen(store: &Store, sid: &str) -> Option<ask::TaskSeen> {
    let record = task_record_of(store, sid)?;
    Some(match (record.state, record.conclusion()) {
        (TaskState::Working | TaskState::Stalled, _) | (_, None) => ask::TaskSeen::Working,
        (_, Some(crate::task_record::Conclusion::Finished)) => ask::TaskSeen::Ended,
        (_, Some(crate::task_record::Conclusion::Failed)) => {
            ask::TaskSeen::Failed(record.why.unwrap_or("failed").to_owned())
        }
    })
}

/// A live session of this process that an earlier disk worker still runs, its sid and its task
/// record's state, the first found: the record trusts the worker folder
/// ([`aterm_agent::harness::disk::fence::is_worker_folder`]) and is stalled, or working for less
/// than [`ask::WORKER_MS`] since it last changed, and the session's own Autopilot is not Off.
/// The lease runs a worker through a stall and ends it at [`ask::WORKER_MS`], marking its record
/// `failed why=expired` ([`crate::App::disk_worker_expired`]); this read bars a second one beside it
/// while that mark has not been made (an update committing) or the lease no longer holds it.
///
/// Every live session is read, not only those whose foreground is Claude Code: a worker can stall
/// before Claude Code runs at all (`stalled why=install` when installing it fails, as it may on a
/// full disk, or `why=shell`), and one stopped at its start (`why=startup`: a sign-in or
/// first-run screen where its composer should be) counts the same. A worker always carries its
/// own Autopilot word (it starts under Land), so a person who switches it Off takes it over. One
/// working past [`ask::WORKER_MS`] since its last change is past the time any worker is given.
///
/// While one runs this host starts no other ([`worker_ok_here`]): the next would share its folder,
/// and one stopped at its start would stop again beside the one that already carries the
/// attention asking a person to act. Once it is done, exits, is closed or is switched Off, the
/// host may start one.
fn earlier_worker(store: &Store, now_ms: i64) -> Option<(String, String)> {
    let handles: Vec<_> = {
        let g = store.read().unwrap_or_else(PoisonError::into_inner);
        g.live_handles().cloned().collect()
    };
    handles.into_iter().find_map(|h| {
        let state = {
            let t = h
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let record = t.task()?;
            let in_folder = record.trusted_cwd.as_deref().is_some_and(|folder| {
                aterm_agent::harness::disk::fence::is_worker_folder(Path::new(folder))
            });
            let since = i64::try_from(record.since_unix_ms).unwrap_or(i64::MAX);
            let runs = match record.state {
                TaskState::Stalled => true,
                TaskState::Working => now_ms.saturating_sub(since) < ask::WORKER_MS,
                TaskState::Done | TaskState::Exited | TaskState::Failed => false,
            };
            if !(in_folder && runs) {
                return None;
            }
            match record.why {
                Some(why) => format!("{} why={why}", record.state.as_str()),
                None => record.state.as_str().to_owned(),
            }
        };
        let off = h
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .autopilot_word()
            == Some(aterm_agent::supervise::Autopilot::Off);
        (!off).then(|| (h.sid.as_str().to_owned(), state))
    })
}

/// Whether this host may start a disk worker at `now_ms` ([`ask::Ahead::worker_ok`]): no earlier
/// one still runs here ([`earlier_worker`]), which the look's debug log names when one does.
fn worker_ok_here(store: &Store, now_ms: i64) -> bool {
    let Some((sid, state)) = earlier_worker(store, now_ms) else {
        return true;
    };
    aterm_log::debug!(
        "harness disk watch: an earlier disk worker @{sid} is {state}: no other starts here while \
         it runs"
    );
    false
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
    let lease = match ask::read(&state) {
        Ok(lease) => lease,
        Err(e) => {
            aterm_log::debug!(
                "harness disk watch: the lease in {} could not be read ({e}); the step after \
                 the pass tries again",
                state.display()
            );
            return;
        }
    };
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
    let worker_target = floor_now
        .saturating_add(disk::PRESSURE_MARGIN_GIB << 30)
        .saturating_add(projected.map_or(0, |p| p.need));
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
            worker_ok: worker_ok_here(store, now),
            task: Some(&task),
            spawned: spawned.as_ref(),
            target: Some(worker_target),
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
                target: worker_target,
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
        && ask::read(state)
            .is_ok_and(|lease| ask::rests(&lease, key, typed_last(key).map(|t| t.at_ms)))
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
        // EVERY OVER THE LEASE RECORDS IS TOLD ONCE, by the process that moved it, which is the one
        // this step ran in: a worker that never started and one whose host died have an end too.
        // Only this host's own worker has a report to read and an Autopilot to turn Off.
        ask::WorkerSaid::Over {
            sid,
            why,
            rose,
            target_met,
            here,
        } => {
            let ours = sid.as_deref().filter(|_| *here);
            let report = ours.map_or_else(Vec::new, |sid| worker_report(store, sid));
            if !report.is_empty() {
                *WORKER_REPORT.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some((key.to_owned(), report.clone()));
            }
            if let Some(sid) = ours
                && why == "expired"
            {
                note(HostNote::DiskWorkerExpired {
                    sid: sid.to_owned(),
                });
            }
            note(HostNote::DiskWorkerOver {
                sid: sid.clone(),
                volume: key.to_owned(),
                why: why.clone(),
                rose: *rose,
                target_met: *target_met,
                removed: sid.as_deref().map_or(0, |sid| worker_removed(state, sid)),
                report,
            });
        }
        ask::WorkerSaid::Started { .. } => {}
    }
}

/// The bytes the worker `sid` removed itself, by the disk ledger's rows
/// (`disk::free::removed_by`); `0` where the ledger cannot be read.
fn worker_removed(state: &Path, sid: &str) -> u64 {
    let ledger = state.join(aterm_agent::harness::cli::DISK_LEDGER);
    match aterm_agent::harness::cli::tail_lines(&ledger, WORKER_LEDGER_ROWS) {
        Ok(rows) => disk::free::removed_by(&rows, sid),
        Err(e) => {
            aterm_log::debug!(
                "harness disk watch: {} could not be read for what the worker removed ({e})",
                ledger.display()
            );
            0
        }
    }
}

/// The ledger rows read back for what a worker removed: more than a run's rows at any pace.
const WORKER_LEDGER_ROWS: usize = 4096;

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
/// volume, with no task record that stands ([`task_bar_of`]: done, exited, failed, or a manager's
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
    /// reader reads busy there and its published word is that turn's rest
    /// ([`aterm_phase::background_rest`]), and its loop refuses that line for the draft.
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

/// Whether `t`'s live screen holds a draft in its composer, read as the fenced write reads it
/// ([`draft_on`]); `None` while it is scrolled back or mid-frame.
pub(crate) fn draft_shown(t: &aterm_core::terminal::Terminal) -> Option<bool> {
    match draft_on(t).0 {
        Draft::Unread => None,
        Draft::Clear => Some(false),
        Draft::Text(_) => Some(true),
    }
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
        TaskState::Failed => Some("task-failed"),
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
    /// Its agent reads busy: it works (a turn over under its own background work publishes its
    /// rest, not busy).
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
    /// ([`draft_on`]): its reader reads busy (its published word is the turn's rest), and its
    /// loop offers a line there as at an idle point ([`Drafted::AtBackgroundBreak`]).
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
    /// The server's observe gate withheld the line (the session's agent is unconfirmed,
    /// [`aterm_agent::harness::agent_image::WITHHELD`]), and the lease handed the ask on, as for
    /// a session that may not be asked: no later turn break of this agent would take it.
    Withheld(ask::Said),
    /// The step's word: `typed`, `wait:<why>` or `gone`.
    Step(String),
}

/// The live act ([`super::Acts::disk_free`]): at session `sid`'s turn break, type `owed`'s line
/// when the lease still names it, or release it when the session may not be asked (`unfit`, the
/// worker's finding, or what else the seat rule bars it by, [`judge_at`], so a session the look
/// would not seat by a lasting fact is never typed its line, nor is one the loop holds for
/// someone else for good: its `exchange` or `managed`, which the worker hands in as `unfit`). A
/// line `held` at this turn break for a while (the loop's word, `person`: a person's grace or
/// another driver's hand, the occupancy door's guard; `held`: a line the loop holds in custody,
/// which a line typed behind would join; or what the seat rule holds it by,
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
        || {
            disk::type_resume(
                sock,
                token.as_ref(),
                sid,
                &owed.text,
                aterm_agent::harness::agent_image::Lane::DiskFree,
            )
        },
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
                    if step == aterm_agent::harness::agent_image::WITHHELD
                        && let Some(said) = ask::release(l, key, self.host, self.sid, self.now)
                    {
                        return Stepped::Withheld(said);
                    }
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
            Ok(Some(Stepped::Withheld(said))) => {
                journal(
                    self.state, self.now, key, &said, self.level, self.free, self.host,
                );
                RELEASED_UNCONFIRMED.to_owned()
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

/// The step's word for a line the observe gate withheld and the lease handed on
/// ([`Stepped::Withheld`]).
pub(super) const RELEASED_UNCONFIRMED: &str = "released:unconfirmed";

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
#[path = "harness_disk_ask_tests.rs"]
mod tests;
