// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One root step a person applies from the System Resources page, its macOS configuration
//! section's (SD-97) or its Spotlight document's buttons (SD-96): `/usr/bin/sudo -k -A -- <this
//! aterm> machines here apply --plain --only <step> [--keep-out <folder>]`, a compiled
//! single-step plan ([`aterm_types::macos_setup::root_only::Step`]) whose password sheet hangs on
//! the page's window and stands for the view a person pressed in.
//!
//! It is a sibling of the CPU policy collection's operation (SD-83's
//! [`crate::cpu_policy_authorization`]), not that type: that one is a read-only collector bound
//! by its hello handshake, this one a root write seen out by its exit, whose one line on its
//! standard output ([`root_only::STARTED`]) tells a step that ran from a sudo that refused the
//! password. Both share the view's origin, its admission (the view is its window's front tab in a
//! window that is not occluded, askpass is neither halted nor relayed off), its withdrawal by
//! view and window, and the one password sheet ([`OwnedOperation`]). One root step runs at a time
//! on the machine, and none while a CPU policy collection is pending, so the page never stacks
//! two password requests. The Spotlight document's buttons are steps of the same table and run
//! here; a pause or resume that ended well is recorded by the run's own end
//! ([`finish`]), on its own thread, never on the App's.

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_sysprobe::cpu_policy::Identity;
use aterm_types::macos_setup::root_only::{self, Step};
use aterm_types::macos_setup::spotlight_scope;

use crate::cpu_policy_authorization::{Origin, Owner, Refusal, read_identity};
use crate::tab_model::ViewId;
use crate::{App, Wake, WindowId};

/// `-k`: this password authorizes this one run and touches no cached record; `-A`: the askpass
/// helper asks this aterm.
const SUDO_ARGS: [&str; 3] = ["-k", "-A", "--"];

/// How long, once sudo ended, its standard output may take to deliver the step's started line
/// already written to it, or its end: a reader on another thread that has not answered by then
/// stands for a step that never started.
const STARTED_WAIT: Duration = Duration::from_secs(5);

/// The hang detector on the whole run, password included: a step that has not ended by then is
/// signalled, and its run then reads as unfinished.
pub(crate) const RUN_BOUND: Duration = Duration::from_mins(10);

static OPERATION: Mutex<Option<Arc<Operation>>> = Mutex::new(None);

/// A password sheet's owned operation: the CPU policy collection or a root step.
#[derive(Clone, Debug)]
pub(crate) enum OwnedOperation {
    CpuPolicy(Arc<crate::cpu_policy_authorization::Operation>),
    RootStep(Arc<Operation>),
}

impl OwnedOperation {
    /// What it stands for.
    pub(crate) fn origin(&self) -> &Origin {
        match self {
            Self::CpuPolicy(op) => op.origin(),
            Self::RootStep(op) => &op.origin,
        }
    }

    /// Whether the sudo it spawned still runs and it was not cancelled.
    pub(crate) fn matches_live_sudo(&self) -> bool {
        match self {
            Self::CpuPolicy(op) => op.matches_live_sudo(),
            Self::RootStep(op) => op.matches_live_sudo(),
        }
    }

    /// Release the typed password to its one sudo, once.
    pub(crate) fn release_secret(&self) -> bool {
        match self {
            Self::CpuPolicy(op) => op.release_secret(),
            Self::RootStep(op) => op.release_secret(),
        }
    }

    /// When a person's own key or press started it: a root step's always
    /// ([`App::start_root_step`] admits no other), a CPU policy collection's where its
    /// [`crate::cpu_policy_authorization::Starter`] was a person. It lets the password sheet past
    /// the spacing a sheet nobody touched set before it (`askpass::Backoff::refusal`).
    pub(crate) fn person_at(&self) -> Option<Instant> {
        match self {
            Self::CpuPolicy(op) => op.person_at(),
            Self::RootStep(op) => Some(op.person_at),
        }
    }

    /// The sheet's message and details.
    pub(crate) fn sheet_words(&self) -> (String, String) {
        match self {
            Self::CpuPolicy(_) => (
                "Authorize CPU diagnostics".to_owned(),
                "Read CPU scheduling information and save a diagnostic report. Authorization \
                 applies to this collection only."
                    .to_owned(),
            ),
            Self::RootStep(op) => sheet_words(op.step, op.folder.as_deref()),
        }
    }
}

/// A root step's sheet: what it changes, and the one command the password authorizes, with
/// the folder a keep-out names and the Spotlight tool a Spotlight step runs.
pub(crate) fn sheet_words(step: Step, folder: Option<&Path>) -> (String, String) {
    let change = match (step, folder) {
        (Step::SpotlightKeepOut, Some(folder)) => {
            format!("keep {} out of Spotlight", folder.display())
        }
        _ => step.change().to_owned(),
    };
    let page = if step.spotlight() {
        "System Resources ▸ Spotlight"
    } else {
        "System Resources ▸ macOS configuration"
    };
    let tool = step.mdutil().map_or_else(String::new, |args| {
        format!(
            ", which runs {} {} {}",
            spotlight_scope::MDUTIL,
            args.join(" "),
            spotlight_scope::DATA_VOLUME
        )
    });
    (
        format!("Enter your Mac password to {change}."),
        format!(
            "{page} runs, as root:\n\naterm {}{}\n\n{}Your password authorizes this one run, \
             not later ones.",
            step.argv().join(" "),
            folder.map_or_else(String::new, |folder| format!(
                " {} {}",
                root_only::KEEP_OUT,
                folder.display()
            )),
            if tool.is_empty() {
                String::new()
            } else {
                format!("That is the step{tool}. ")
            },
        ),
    )
}

/// Record how a run of `step` that ended `ended` leaves the person's pause, in aterm's state
/// folder `dir` at `now_unix_ms` ([`spotlight_scope::PAUSE_RECORD`]): a Pause that applied is
/// theirs from now, a Resume that applied ends it once a later reading says how indexing stands.
/// Any other step or end records nothing. The run's own end calls it ([`finish`]), never the App.
///
/// # Errors
/// The record could not be written.
fn note_pause(dir: &Path, step: Step, ended: &Ended, now_unix_ms: i64) -> Result<(), String> {
    let mark = match (step, ended) {
        (Step::SpotlightPause, Ended::Applied) => spotlight_scope::Mark::Paused,
        (Step::SpotlightResume, Ended::Applied) => spotlight_scope::Mark::Resumed,
        _ => return Ok(()),
    };
    spotlight_scope::record_pause(
        dir,
        &spotlight_scope::PauseRecord {
            mark,
            at_unix_ms: now_unix_ms,
        },
    )
}

/// One root step's run, bound to the view that pressed its Apply.
pub(crate) struct Operation {
    id: u64,
    owner: Owner,
    origin: Origin,
    step: Step,
    /// The folder a keep-out names ([`root_only::KEEP_OUT`]).
    folder: Option<PathBuf>,
    /// When the person's press that started it came ([`App::start_root_step`]).
    person_at: Instant,
    cancelled: AtomicBool,
    requested: AtomicBool,
    released: AtomicBool,
    sudo: Mutex<Option<Identity>>,
    ticket: Mutex<Option<Arc<crate::askpass::AskpassTicket>>>,
}

impl std::fmt::Debug for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootStepOperation")
            .field("id", &self.id)
            .field("step", &self.step)
            .finish_non_exhaustive()
    }
}

impl Operation {
    fn new(
        owner: Owner,
        origin: Origin,
        step: Step,
        folder: Option<PathBuf>,
        person_at: Instant,
    ) -> Self {
        Self {
            id: crate::alert_keys::next_confirm_id(),
            owner,
            origin,
            step,
            folder,
            person_at,
            cancelled: AtomicBool::new(false),
            requested: AtomicBool::new(false),
            released: AtomicBool::new(false),
            sudo: Mutex::new(None),
            ticket: Mutex::new(None),
        }
    }

    /// The step it runs.
    pub(crate) const fn step(&self) -> Step {
        self.step
    }

    /// The folder its keep-out names.
    #[cfg(test)]
    pub(crate) fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(ticket) = self
            .ticket
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            ticket.cancel();
        }
    }

    fn bound(&self) -> bool {
        !self.cancelled.load(Ordering::Acquire)
            && self
                .sudo
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
    }

    /// Not on the App thread: reads the process table.
    pub(crate) fn matches_live_sudo(&self) -> bool {
        let expected = *self.sudo.lock().unwrap_or_else(|e| e.into_inner());
        expected.is_some_and(|id| read_identity(id.pid) == Some(id))
            && !self.cancelled.load(Ordering::Acquire)
    }

    /// Release the password once, to the sudo this operation bound, after its one request.
    pub(crate) fn release_secret(&self) -> bool {
        self.bound()
            && self.requested.load(Ordering::Acquire)
            && self
                .released
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
}

/// `owner`'s operation, if the slot holds one of its.
fn owned(owner: Owner) -> Option<Arc<Operation>> {
    OPERATION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|op| op.owner == owner)
        .cloned()
}

/// A root step's operation and the one ticket of its sudo's askpass request.
pub(crate) type OwnedPrompt = (Arc<Operation>, Arc<crate::askpass::AskpassTicket>);

/// The askpass request of the sudo `sudo_pid`, where it is the one a root step bound: its
/// operation and its one ticket. A second request of the same sudo cancels the run.
pub(crate) fn request(sudo_pid: i32) -> Result<Option<OwnedPrompt>, ()> {
    let Some(operation) = OPERATION.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        return Ok(None);
    };
    let expected = *operation.sudo.lock().unwrap_or_else(|e| e.into_inner());
    if expected.is_none_or(|id| Some(id.pid) != u32::try_from(sudo_pid).ok()) {
        return Ok(None);
    }
    if !operation.matches_live_sudo() {
        return Err(());
    }
    if operation
        .requested
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        operation.cancel();
        return Err(());
    }
    let ticket = Arc::new(crate::askpass::AskpassTicket::new(
        crate::alert_keys::next_confirm_id(),
        Instant::now() + crate::askpass::ASKPASS_DEADLINE,
    ));
    *operation.ticket.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&ticket));
    Ok(Some((operation, ticket)))
}

/// The step `owner` has running, if any.
pub(crate) fn pending(owner: Owner) -> Option<Step> {
    owned(owner).map(|op| op.step)
}

fn cancel_where(owner: Owner, gone: impl FnOnce(&Origin) -> bool) {
    if let Some(op) = owned(owner)
        && gone(&op.origin)
    {
        op.cancel();
    }
}

/// Cancel `owner`'s run whose view is in `window`.
pub(crate) fn cancel_window(owner: Owner, window: WindowId) {
    cancel_where(owner, |origin| origin.window() == window);
}

/// Cancel `owner`'s run the view `view` started.
pub(crate) fn cancel_view(owner: Owner, view: ViewId) {
    cancel_where(
        owner,
        |origin| matches!(origin, Origin::View { view: v, .. } if *v == view),
    );
}

/// The view whose run of `owner`'s is pending and not cancelled.
pub(crate) fn pending_view(owner: Owner) -> Option<(WindowId, ViewId)> {
    match owned(owner).map(|op| (op.origin.clone(), op.cancelled.load(Ordering::Acquire))) {
        Some((Origin::View { window, view }, false)) => Some((window, view)),
        _ => None,
    }
}

/// Empty the slot if it still holds the operation `id`.
fn vacate(id: u64) {
    let mut active = OPERATION.lock().unwrap_or_else(|e| e.into_inner());
    if active.as_ref().is_some_and(|op| op.id == id) {
        *active = None;
    }
}

/// `sudo`'s arguments for `step` run by the aterm at `exe`: `-k -A -- <exe> machines here apply
/// --plain --only <step>`, then `--keep-out <folder>` where a keep-out names one.
pub(crate) fn sudo_args(exe: &Path, step: Step, folder: Option<&Path>) -> Vec<std::ffi::OsString> {
    SUDO_ARGS
        .iter()
        .map(Into::into)
        .chain(std::iter::once(exe.as_os_str().to_owned()))
        .chain(step.argv().iter().map(Into::into))
        .chain(
            folder
                .into_iter()
                .flat_map(|folder| [root_only::KEEP_OUT.into(), folder.as_os_str().to_owned()]),
        )
        .collect()
}

/// How a run ended, in the page's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ended {
    /// The step finished.
    Applied,
    /// It ran and did not finish, or ended by a signal: why.
    NotApplied(String),
    /// It never ran: why (the password was declined, the sheet withdrawn, sudo did not start).
    NotRun(String),
}

/// How a run ended that exited with `code` (`None` for a signal): `started` where the step
/// printed its started line, `released` where a password went to sudo, `cancelled` where the
/// request was withdrawn. sudo exits 0 only where the step finished. Otherwise a run whose step
/// never started never ran: a password sudo did not accept ends there, and so does a second
/// request (a mistyped one), which cancels the run.
pub(crate) fn verdict(
    code: Option<i32>,
    started: bool,
    released: bool,
    cancelled: bool,
    step: Step,
) -> Ended {
    match (code, started) {
        (Some(0), _) => Ended::Applied,
        (_, false) => Ended::NotRun(if released {
            "the password was not accepted".to_owned()
        } else if cancelled {
            "the password request was withdrawn".to_owned()
        } else {
            "no password was given".to_owned()
        }),
        (Some(code), true) => Ended::NotApplied(format!(
            "the step ran and did not finish (exit {code}); `aterm machines here apply --plain \
             --only {}` journals why",
            step.word()
        )),
        (None, true) => Ended::NotApplied("the step ended by a signal".to_owned()),
    }
}

/// Read a run's standard output to its end on its own thread, sending `true` at the step's
/// started line and `false` at an end with none.
fn watch_started(
    output: impl std::io::Read + Send + 'static,
) -> std::io::Result<std::sync::mpsc::Receiver<bool>> {
    let (tx, rx) = std::sync::mpsc::channel();
    crate::qos::spawn_lane_thread(
        "aterm-root-step-output",
        crate::qos::Role::Responsive,
        move || {
            let mut started = false;
            for line in BufReader::new(output).lines() {
                match line {
                    Ok(line) if !started && line == root_only::STARTED => {
                        started = true;
                        let _ = tx.send(true);
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            if !started {
                let _ = tx.send(false);
            }
        },
    )?;
    Ok(rx)
}

impl App {
    /// Whether `op`'s view is still in front, under the halt and relay refusals: the CPU policy
    /// collection's admission, for a root step.
    pub(crate) fn root_step_origin_live(&self, op: &Operation) -> bool {
        op.owner == self.cpu_policy_owner()
            && op.bound()
            && self.root_step_refusal(&op.origin).is_none()
    }

    fn root_step_refusal(&self, origin: &Origin) -> Option<Refusal> {
        let Origin::View { window, view } = origin else {
            return Some(Refusal::NotInFront);
        };
        if !self.view_in_front(*window, *view) {
            Some(Refusal::NotInFront)
        } else if crate::fabric::app_halt_refusal(&self.store, "askpass").is_some() {
            Some(Refusal::Halted)
        } else if crate::askpass::relay_off() {
            Some(Refusal::RelayOff)
        } else {
            None
        }
    }

    /// The step this App has running, if any.
    pub(crate) fn root_step_pending(&self) -> Option<Step> {
        pending(self.cpu_policy_owner())
    }

    /// A person's Apply of `step` on the page's view `view` in `window`, with the `folder` a
    /// keep-out names: one run bound to that view, admitted while it is its window's front tab,
    /// refused while any root step or CPU policy collection is pending, and while the gesture
    /// is no person's (the dispatch refused it already; this holds wherever the call came from).
    pub(crate) fn start_root_step(
        &mut self,
        window: WindowId,
        view: ViewId,
        step: Step,
        folder: Option<PathBuf>,
    ) -> Result<(), Refusal> {
        if self.gesture_unvouched().is_some() {
            return Err(Refusal::NotAPerson);
        }
        if step.takes_folder() != folder.is_some() {
            return Err(Refusal::NoFolder);
        }
        let origin = Origin::View { window, view };
        if let Some(refusal) = self.root_step_refusal(&origin) {
            return Err(refusal);
        }
        if self.cpu_policy_pending() {
            return Err(Refusal::Pending);
        }
        let operation = Arc::new(Operation::new(
            self.cpu_policy_owner(),
            origin,
            step,
            folder,
            Instant::now(),
        ));
        let proxy = self.proxy.clone();
        // A headless test's App has no event loop: its run stands in the slot, with no worker and
        // so no sudo, until the test empties the slot.
        #[cfg(not(test))]
        let Some(proxy) = proxy else {
            return Err(Refusal::NoEventLoop);
        };
        {
            let mut active = OPERATION.lock().unwrap_or_else(|e| e.into_inner());
            if active.is_some() {
                return Err(Refusal::Pending);
            }
            *active = Some(Arc::clone(&operation));
        }
        #[cfg(test)]
        let Some(proxy) = proxy else {
            return Ok(());
        };
        let worker = Arc::clone(&operation);
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-root-step",
            crate::qos::Role::Responsive,
            move || {
                let ended = run(&worker);
                let state = aterm_types::dirs::state_dir();
                let _ = proxy.send_event(finish(&worker, ended, state.as_deref(), wall_ms()));
            },
        );
        if let Err(error) = spawned {
            vacate(operation.id);
            aterm_log::warn!("resources: the root step's worker did not start: {error}");
            return Err(Refusal::NoEventLoop);
        }
        Ok(())
    }
}

/// `op`'s run ended `ended`, as its worker sees it out: a Pause or Resume that applied is
/// recorded in aterm's state folder `state` at `now_unix_ms` ([`note_pause`]), the slot is
/// emptied, and the wake that tells the App how it ended is what the worker sends.
pub(crate) fn finish(op: &Operation, ended: Ended, state: Option<&Path>, now_unix_ms: i64) -> Wake {
    if let Some(dir) = state
        && let Err(why) = note_pause(dir, op.step, &ended, now_unix_ms)
    {
        aterm_log::warn!("resources: the Spotlight pause was not recorded: {why}");
    }
    vacate(op.id);
    Wake::RootStepEnded {
        step: op.step,
        ended,
    }
}

/// The wall clock, unix ms.
fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Run `op`'s step under sudo and see it out, never on the App thread.
fn run(op: &Operation) -> Ended {
    let (exe, helper) = match crate::cpu_policy_authorization::fixed_paths() {
        Ok(paths) => paths,
        Err(why) => return Ended::NotRun(format!("this aterm cannot ask for a password: {why}")),
    };
    let Some(sock) = crate::proxy::self_sock_path() else {
        return Ended::NotRun("this aterm has no control socket to ask on".to_owned());
    };
    let mut command = aterm_uds::spawnfd::command("/usr/bin/sudo");
    command
        .args(sudo_args(&exe, op.step, op.folder.as_deref()))
        .env("SUDO_ASKPASS", &helper)
        .env(aterm_uds::sudo::ASKPASS_SOCK_ENV, sock)
        .env_remove("ATERM_PARENT_SESSION_ID")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .start_suspended(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return Ended::NotRun(format!("sudo did not start: {error}")),
    };
    // Its reader starts while sudo is suspended: no step writes to a pipe nobody reads.
    let Some(output) = child.stdout.take() else {
        unreachable!("requested piped stdout")
    };
    let start_line = match watch_started(output) {
        Ok(start_line) => start_line,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Ended::NotRun(format!("the step's output could not be read: {error}"));
        }
    };
    // Spawned suspended: no sudo instruction can ask for a password before this binding exists.
    let Some(sudo) = read_identity(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Ended::NotRun("sudo's process could not be read".to_owned());
    };
    *op.sudo.lock().unwrap_or_else(|e| e.into_inner()) = Some(sudo);
    if let Err(error) = child.resume() {
        let _ = child.kill();
        let _ = child.wait();
        return Ended::NotRun(format!("sudo did not start: {error}"));
    }
    let started = Instant::now();
    let mut signalled: Option<Instant> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // The line was written before the step ended; its reader may not have sent yet.
                let started = start_line.recv_timeout(STARTED_WAIT).unwrap_or(false);
                return verdict(
                    status.code(),
                    started,
                    op.released.load(Ordering::Acquire),
                    op.cancelled.load(Ordering::Acquire),
                    op.step,
                );
            }
            Ok(None) => {}
            Err(error) => return Ended::NotApplied(format!("sudo's end was not read: {error}")),
        }
        let over = started.elapsed() >= RUN_BOUND;
        if (op.cancelled.load(Ordering::Acquire) && !op.released.load(Ordering::Acquire)) || over {
            match signalled {
                None => {
                    op.cancel();
                    // sudo relays the signal to the root command; aterm cannot signal root.
                    let _ = child.terminate();
                    signalled = Some(Instant::now());
                }
                Some(at) if at.elapsed() >= aterm_uds::process::END_GRACE => {
                    return Ended::NotApplied(
                        "the step outlived its signal and runs on to its own end".to_owned(),
                    );
                }
                Some(_) => {}
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The operation in the slot, for a test to read.
#[cfg(test)]
pub(crate) fn active_for_test() -> Option<Arc<Operation>> {
    OPERATION.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[cfg(test)]
impl Operation {
    /// Bind it to `sudo` as the worker binds the `sudo` it spawned, so its sheet may be raised.
    pub(crate) fn bind_sudo_for_test(&self, sudo: Identity) {
        *self.sudo.lock().unwrap_or_else(|e| e.into_inner()) = Some(sudo);
    }

    /// What it stands for.
    pub(crate) fn origin_for_test(&self) -> Origin {
        self.origin.clone()
    }

    /// Whether it was cancelled.
    pub(crate) fn cancelled_for_test(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Empty the slot, as a worker does once its run ended; a test that starts a run empties it.
#[cfg(test)]
pub(crate) fn vacate_for_test() {
    *OPERATION.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
#[path = "root_step_authorization_tests.rs"]
mod tests;
