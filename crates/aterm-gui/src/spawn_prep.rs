// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Background pre-allocation of PTY spawn kits and child provisions, and off-thread edge-file retirement.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, OnceLock, PoisonError};

/// Number of spawn kits and child provisions kept ready in the background pool.
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

/// Pre-allocated child recursion provisioning (`provision_child_files`).
pub(crate) struct Provision {
    env: Vec<(String, String)>,
    prov: Option<crate::spawn::ChildProvision>,
    edge_file: Option<crate::proxy::EdgeFile>,
    /// The control dir the file was written under.
    dir: PathBuf,
}

/// Child env, tokens, and edge-token file taken by a spawn.
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
        if let Some(file) = self.edge_file.take() {
            remove_edge_file(file);
        }
    }
}

/// Starts the background prep thread and queues an initial fill.
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

/// Prepares session 0's spawn kit and optional child provision on a responsive boot thread.
pub(crate) fn ready_first_spawn(provision: bool) {
    let spawned = crate::qos::spawn_lane_thread(
        "aterm-spawn-prep0",
        crate::qos::Role::Responsive,
        move || {
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

/// Requests a background refill of spawn kits and provisions.
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
    if released.load(Ordering::Acquire) && matches!(&job, Job::Retire(_)) {
        return Err(job);
    }
    tx.send(job).map_err(|refused| refused.0)
}

/// Takes a ready child provision written under `sock_dir`, if available.
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

/// Removes a closed session's edge-token file on the prep thread, or inline if unavailable.
pub(crate) fn retire_edge_file(file: crate::proxy::EdgeFile) {
    retire_using(file, send);
}

/// The actual retirement path, with its queue admission supplied by `send`.
fn retire_using(file: crate::proxy::EdgeFile, send: impl FnOnce(Job) -> Result<(), Job>) {
    if let Err(Job::Retire(file)) = send(Job::Retire(file)) {
        remove_edge_file(file);
    }
}

/// Flushes queued prep work up to `within` and drops unused provisions at exit.
pub(crate) fn release_at_exit(within: std::time::Duration) {
    RELEASED.store(true, Ordering::Release);
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
                    fill = false;
                    let _ = done.send(());
                }
            }
        }
        if fill && !RELEASED.load(Ordering::Acquire) {
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
