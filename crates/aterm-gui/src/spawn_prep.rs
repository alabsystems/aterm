// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The filesystem work a new shell needs, done before anyone asks for one, and
//! the work a closed one leaves, done where nobody waits.
//!
//! Measured 2026-10-17 by `sample` of the spawner thread over 200 back-to-back
//! tab spawns (load 7-16, before this module): the exec-status fifo's
//! `mkfifo`, `unlink` and the close that reclaims its node took about a third of
//! a spawn, the edge-token file's create and write about a quarter, the
//! `/dev/ptmx` open 5-8% and the fork 12%. Every new tab's reply waited for all
//! of it. A closed tab's edge-token file, removed and its locked handle closed,
//! held the event loop about 1 ms.
//!
//! One `aterm-spawn-prep` thread, started after the boot grace ([`warm`]) so it
//! is never on the cold start's path, keeps [`DEPTH`] of each ready:
//! * spawn kits, the PTY pair and exec-status channel
//!   (`aterm_pty::fill_spawn_kits`), whose fill also closes the channel ends
//!   finished spawns left;
//! * child provisions: a child identity minted, its edge-token file written
//!   and locked under the control dir ([`take_provision`]), and owned only by
//!   the spawn that takes it (its `proxy::ChildProxy`).
//!
//! Each fresh spawn tops both up ([`poke`]). A closed session's edge-token file
//! is removed there too ([`retire_edge_file`]), in the same order as before:
//! removed while still locked, then unlocked. With no thread, before the grace
//! or when one could not start, all of it runs inline as it did.
//!
//! The cold start's own spawn, session 0, is the exception: its kit and its
//! provision are made as the boot begins, on an `aterm-spawn-prep0` thread of
//! their own ([`ready_first_spawn`]), while the UI thread warms the config and
//! builds the event loop. Measured 2026-10-18 (probe builds, headless, launcher
//! spun 200 ms first, interleaved, n=12 per arm and kind): session 0's
//! edge-token file cost the UI thread 0.10-0.14 ms and now 0.03, and its fork
//! call, which opened the kit, 1.9-2.2 ms and now 1.2-1.3. The thread is done
//! 1.3-2.3 ms before session 0 asks. A spawn that arrives first opens and
//! writes its own, as before, and what the thread made waits for the next one.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, OnceLock, PoisonError};

/// How many of each the thread keeps ready: a Cmd-T, and the split or tab that
/// follows it before the thread has run again.
pub(crate) const DEPTH: usize = 2;

/// What the thread is asked to do.
enum Job {
    /// Top up the spawn kits and the child provisions.
    Fill,
    /// Remove a closed session's edge-token file, then let its lock go.
    Retire(crate::proxy::EdgeFile),
    /// Answer once every job queued before it is done (the final exit).
    Flush(Sender<()>),
}

/// The thread's queue, once [`warm`] started it (`None` inside: it could not).
static PREP: OnceLock<Option<Mutex<Sender<Job>>>> = OnceLock::new();

/// The child provisions waiting for spawns.
static PROVISIONS: Mutex<Vec<Provision>> = Mutex::new(Vec::new());

/// Set by the final exit: no fill runs after it.
static RELEASED: AtomicBool = AtomicBool::new(false);

/// Whether a kit refusal was logged yet.
#[cfg(unix)]
static KIT_REFUSAL_SAID: AtomicBool = AtomicBool::new(false);

/// One child's recursion provisioning, prepared ahead
/// ([`crate::spawn::provision_child_files`]): its identity env, its tokens and
/// its edge-token file under `dir`. One no spawn takes removes its file.
pub(crate) struct Provision {
    env: Vec<(String, String)>,
    prov: Option<crate::spawn::ChildProvision>,
    edge_file: Option<crate::proxy::EdgeFile>,
    /// The control dir the file was written under.
    dir: PathBuf,
}

/// What a spawn takes: the child's env, its tokens, and its edge-token file.
pub(crate) type ProvisionParts = (
    Vec<(String, String)>,
    crate::spawn::ChildProvision,
    Option<crate::proxy::EdgeFile>,
);

impl Provision {
    /// Mint one under `dir`; `None` when the dir refuses the file (the inline
    /// path says so, as it always did).
    fn prepare(dir: &Path) -> Option<Self> {
        let (env, prov, edge_file) = crate::spawn::provision_child_files(Some(dir));
        edge_file.as_ref()?;
        Some(Self {
            env,
            prov: Some(prov),
            edge_file,
            dir: dir.to_path_buf(),
        })
    }

    fn into_parts(mut self) -> Option<ProvisionParts> {
        let prov = self.prov.take()?;
        Some((std::mem::take(&mut self.env), prov, self.edge_file.take()))
    }
}

impl Drop for Provision {
    fn drop(&mut self) {
        // Removed while still locked, as a session's is.
        if let Some(file) = self.edge_file.take() {
            remove_edge_file(file);
        }
    }
}

/// Start the thread and fill once. Called after the boot grace; later calls do
/// nothing.
pub(crate) fn warm() {
    let mut started = false;
    let prep = PREP.get_or_init(|| {
        started = true;
        let (tx, rx) = channel::<Job>();
        crate::qos::spawn_lane_thread(
            "aterm-spawn-prep",
            crate::qos::Role::Background,
            move || {
                serve(&rx);
            },
        )
        .map_err(|e| aterm_log::warn!("spawn prep: could not start its thread: {e}"))
        .ok()
        .map(|_| Mutex::new(tx))
    });
    if started && prep.is_some() {
        let _ = send(Job::Fill);
    }
}

/// Session 0's spawn kit and, when it will have a child proxy (`provision`:
/// not a one-shot `-e` command), its child provisions, made on an
/// `aterm-spawn-prep0` thread started as the boot begins (after the intake,
/// which settles the control dir), so the first spawn finds them ready. The
/// pool turns on with this kit, so spent channel ends wait for the next fill
/// from here on. Nothing waits on the thread: a spawn that finds nothing ready
/// makes its own inline. `Responsive`, as session 0 is wanted now.
pub(crate) fn ready_first_spawn(provision: bool) {
    let spawned = crate::qos::spawn_lane_thread(
        "aterm-spawn-prep0",
        crate::qos::Role::Responsive,
        move || {
            // The provision first: the spawn takes it before it opens a kit.
            if provision {
                fill_provisions(
                    &PROVISIONS,
                    crate::control_auth::socket_dir().as_deref(),
                    Provision::prepare,
                    &RELEASED,
                );
            }
            #[cfg(unix)]
            if let Err(e) = aterm_pty::fill_spawn_kits(1)
                && !KIT_REFUSAL_SAID.swap(true, Ordering::Relaxed)
            {
                aterm_log::warn!("spawn prep: could not open a spawn kit: {e}");
            }
        },
    );
    if let Err(e) = spawned {
        aterm_log::warn!("spawn prep: could not start session 0's: {e}");
    }
}

/// Top the kits and provisions up after a spawn took one. Nothing before
/// [`warm`].
pub(crate) fn poke() {
    let _ = send(Job::Fill);
}

/// Hand `job` to the thread; back when there is none.
fn send(job: Job) -> Result<(), Job> {
    match PREP.get() {
        Some(Some(tx)) => send_to(tx, &RELEASED, job),
        _ => Err(job),
    }
}

/// Queue a job while holding the same admission lock used by the exit's
/// Flush. A retirement refused here remains owned by its caller.
fn send_to(tx: &Mutex<Sender<Job>>, released: &AtomicBool, job: Job) -> Result<(), Job> {
    let tx = tx.lock().unwrap_or_else(PoisonError::into_inner);
    // Release is stored before the final Flush takes this mutex. A retire
    // queued before it is in the prefix; after it, return the owned file so
    // its caller removes it inline, outside this lock.
    if released.load(Ordering::Acquire) && matches!(&job, Job::Retire(_)) {
        return Err(job);
    }
    tx.send(job).map_err(|refused| refused.0)
}

/// A ready child provision written under `sock_dir`, if one waits.
pub(crate) fn take_provision(sock_dir: Option<&Path>) -> Option<ProvisionParts> {
    take_from(&PROVISIONS, sock_dir)
}

/// [`take_provision`] from `ready`.
fn take_from(ready: &Mutex<Vec<Provision>>, sock_dir: Option<&Path>) -> Option<ProvisionParts> {
    let dir = sock_dir?;
    let taken = {
        let mut ready = ready.lock().unwrap_or_else(PoisonError::into_inner);
        let at = ready.iter().position(|p| p.dir == dir)?;
        ready.swap_remove(at)
    };
    taken.into_parts()
}

/// Remove a closed session's edge-token file and then let its lock go: on the
/// thread when it runs, else now.
pub(crate) fn retire_edge_file(file: crate::proxy::EdgeFile) {
    retire_using(file, send);
}

/// The actual retirement path, with its queue admission supplied by `send`.
fn retire_using(file: crate::proxy::EdgeFile, send: impl FnOnce(Job) -> Result<(), Job>) {
    if let Err(Job::Retire(file)) = send(Job::Retire(file)) {
        remove_edge_file(file);
    }
}

/// The final exit, after the tab spawner joined and the spare worker was
/// asked to stop: retire unused provisions and wait up to `within` for the
/// already queued prefix. Later retirements run inline. Filesystem calls and
/// dropping those provisions are not bounded by the queue wait.
pub(crate) fn release_at_exit(within: std::time::Duration) {
    RELEASED.store(true, Ordering::Release);
    // A fill the thread is in finishes before the flush is answered, so what it
    // made is among what goes below.
    let (done_tx, done_rx) = channel();
    if send(Job::Flush(done_tx)).is_ok() {
        let _ = done_rx.recv_timeout(within);
    }
    let unused = std::mem::take(&mut *PROVISIONS.lock().unwrap_or_else(PoisonError::into_inner));
    drop(unused);
}

/// Removed while still locked, so no sweep can take it in between; the lock
/// goes as `file` drops.
fn remove_edge_file(file: crate::proxy::EdgeFile) {
    let _ = std::fs::remove_file(file.path());
    drop(file);
}

/// The thread: each wake does every queued removal, then one fill.
fn serve(rx: &Receiver<Job>) {
    while let Ok(first) = rx.recv() {
        let mut fill = false;
        for job in std::iter::once(first).chain(rx.try_iter()) {
            match job {
                Job::Fill => fill = true,
                Job::Retire(file) => remove_edge_file(file),
                Job::Flush(done) => {
                    // Exiting: nothing is made after the release.
                    fill = false;
                    let _ = done.send(());
                }
            }
        }
        if fill && !RELEASED.load(Ordering::Acquire) {
            // Said once: a refusal that lasts (the descriptor limit, every pty
            // taken) would otherwise repeat at every spawn, which then opens its
            // own as before and fails or succeeds on its own.
            #[cfg(unix)]
            if let Err(e) = aterm_pty::fill_spawn_kits(DEPTH)
                && !KIT_REFUSAL_SAID.swap(true, Ordering::Relaxed)
            {
                aterm_log::warn!("spawn prep: could not open a spawn kit: {e}");
            }
            fill_provisions(
                &PROVISIONS,
                crate::control_auth::socket_dir().as_deref(),
                Provision::prepare,
                &RELEASED,
            );
        }
    }
}

/// Keep [`DEPTH`] provisions for `sock_dir` in `ready`, minted by `prepare`:
/// ones written under another dir go first. Minted outside the lock, so a spawn
/// taking one never waits on the volume. One minted after `released` was set
/// (a fill the final exit stopped waiting for) removes its own file instead of
/// waiting where the exit already looked.
fn fill_provisions(
    ready: &Mutex<Vec<Provision>>,
    sock_dir: Option<&Path>,
    prepare: impl Fn(&Path) -> Option<Provision>,
    released: &AtomicBool,
) {
    let lock = || ready.lock().unwrap_or_else(PoisonError::into_inner);
    let stale: Vec<Provision> = {
        let mut ready = lock();
        let (keep, stale) = std::mem::take(&mut *ready)
            .into_iter()
            .partition(|p| Some(p.dir.as_path()) == sock_dir);
        *ready = keep;
        stale
    };
    drop(stale);
    let Some(dir) = sock_dir else {
        return;
    };
    while lock().len() < DEPTH {
        let Some(provision) = prepare(dir) else {
            return;
        };
        let mut ready = lock();
        // Read under the lock the exit takes the provisions with, after it
        // set the flag: pushed before that take, or dropped here.
        if released.load(Ordering::Acquire) {
            drop(ready);
            drop(provision);
            return;
        }
        ready.push(provision);
    }
}

#[cfg(all(test, unix))]
#[path = "spawn_prep_tests.rs"]
mod tests;
