// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! App-owned account work is drained before process replacement or logical exit, and past
//! [`ACCOUNT_WORK_RETIRES_WITHIN`] they go on without it.

use std::time::{Duration, Instant};

use aterm_agent::harness::account_work::{Drained, Pause};
use aterm_window::event_loop::ActiveEventLoop;

use crate::native_app::UpdateOutcome;
use crate::native_updater_service::ApplyMode;
use crate::{App, CloseOutcome, Wake, WindowId};

/// How long what waits on a closed account epoch waits for its operations to retire and for its
/// enrollment metadata to seal. A cancelled operation stops within seconds (its child group killed, a
/// network call's own budget), so one still out after this is stuck on what may never answer (a read
/// or a write on a stopped volume, a child nothing can reap) or on storage that keeps failing a finished
/// sign-in's write. An update or a quit goes on without its proof: an update waiting on that for good
/// is the worse mistake. This process's end takes what is in memory along, a response not yet stored
/// included, and leaves an enrollment's record uncertain. Account work stays closed until it retires,
/// or, where its worker panicked and took the pause with it, until aterm restarts.
const ACCOUNT_WORK_RETIRES_WITHIN: Duration = Duration::from_secs(5 * 60);

const WORKER_RETRY: Duration = Duration::from_secs(1);

/// A cancelled constructor that has not bound a real child can release its
/// permission. Claimed children and provider-response custody retain their real
/// lifetimes. Keep retrying off the event loop: a previously admitted launch
/// worker can insert its reservation after an earlier sweep.
fn drain_account_sources(
    pause: Pause,
    sources: Option<&aterm_agent::harness::codex_source_registry::Registry>,
    lifetime: &aterm_agent::harness::account_work::Lifetime,
) -> Drained {
    drain_account_sources_inner(
        pause,
        sources,
        lifetime,
        #[cfg(all(test, unix))]
        None,
    )
}

#[cfg(all(test, unix))]
pub(crate) struct CustodySweepCheckpoint {
    pub(crate) swept: std::sync::mpsc::Sender<()>,
    pub(crate) resume: std::sync::mpsc::Receiver<()>,
}

fn drain_account_sources_inner(
    mut pause: Pause,
    sources: Option<&aterm_agent::harness::codex_source_registry::Registry>,
    lifetime: &aterm_agent::harness::account_work::Lifetime,
    #[cfg(all(test, unix))] mut checkpoint: Option<CustodySweepCheckpoint>,
) -> Drained {
    #[cfg(not(unix))]
    if sources.is_none() {
        return pause.wait();
    }
    const FIRST_CUSTODY_RETRY: Duration = Duration::from_millis(100);
    const MAX_CUSTODY_RETRY: Duration = Duration::from_secs(1);
    let began = Instant::now();
    let mut next_custody = Instant::now();
    let mut custody_retry = FIRST_CUSTODY_RETRY;
    loop {
        if let Some(sources) = sources {
            let _ = sources.revoke_cancelled_unclaimed();
        }
        if Instant::now() >= next_custody {
            #[cfg(unix)]
            let pending = App::settle_enrollment_custody(lifetime);
            #[cfg(not(unix))]
            let pending = {
                let _ = lifetime;
                false
            };
            let delay = if pending {
                custody_retry
            } else {
                FIRST_CUSTODY_RETRY
            };
            next_custody = Instant::now() + delay;
            custody_retry = if pending {
                delay.saturating_mul(2).min(MAX_CUSTODY_RETRY)
            } else {
                FIRST_CUSTODY_RETRY
            };
            #[cfg(all(test, unix))]
            if let Some(checkpoint) = checkpoint.take() {
                let _ = checkpoint.swept.send(());
                let _ = checkpoint
                    .resume
                    .recv_timeout(aterm_spec::wait::MUST_HAPPEN);
            }
        }
        pause = match pause.try_wait() {
            Ok(drained) => return drained,
            Err(pause) => pause,
        };
        // This clock only paces owned storage retries, never proves retirement.
        let wait = next_custody.saturating_duration_since(Instant::now());
        std::thread::park_timeout(if sources.is_some() {
            wait.min(source_sweep_waits(began.elapsed()))
        } else {
            wait
        });
    }
}

/// How long a drain that has swept for `so_far` waits for its next sweep. Past
/// [`ACCOUNT_WORK_RETIRES_WITHIN`] nothing waits on it, and a process that lives on keeps it for as long
/// as the stuck work lasts, so it stops waking a hundred times a second.
fn source_sweep_waits(so_far: Duration) -> Duration {
    if so_far < ACCOUNT_WORK_RETIRES_WITHIN {
        Duration::from_millis(10)
    } else {
        Duration::from_secs(1)
    }
}

#[cfg(all(test, unix))]
pub(crate) fn drain_custody_for_test(
    pause: Pause,
    lifetime: &aterm_agent::harness::account_work::Lifetime,
    checkpoint: Option<CustodySweepCheckpoint>,
) -> Drained {
    drain_account_sources_inner(pause, None, lifetime, checkpoint)
}

/// An automatic attempt whose handoff waits on account work, so it has no answer yet.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AutoApplyAttempt {
    pub(crate) intent: crate::AutoApplyIntent,
    pub(crate) phase: crate::native_update_auto_intent::ApplyPhase,
    pub(crate) announce: bool,
    pub(crate) activation: bool,
}

#[derive(Debug)]
pub(crate) struct Settlement {
    drained: Drained,
    sealed: bool,
}

impl Settlement {
    pub(crate) fn durably_sealed(&self) -> bool {
        self.sealed
    }
}

pub(crate) enum DrainInput {
    Wait(Pause),
    Ready(Drained),
}

fn enrollment_pending(lifetime: &aterm_agent::harness::account_work::Lifetime) -> bool {
    #[cfg(unix)]
    return App::enrollment_retirement_pending(lifetime);
    #[cfg(not(unix))]
    {
        let _ = lifetime;
        false
    }
}

pub(crate) fn settle_accounts(
    input: DrainInput,
    lifetime: &aterm_agent::harness::account_work::Lifetime,
) -> Settlement {
    let drained = match input {
        DrainInput::Wait(pause) => {
            let sources = crate::control::source_registry();
            drain_account_sources(pause, sources.as_deref(), lifetime)
        }
        DrainInput::Ready(drained) => drained,
    };
    #[cfg(unix)]
    let sealed = App::seal_enrollment_retirement(lifetime);
    #[cfg(not(unix))]
    let sealed = {
        let _ = lifetime;
        true
    };
    Settlement { drained, sealed }
}

pub(crate) enum Continuation {
    Quit,
    ResumeAfterDrain,
    StartupFailure,
    Window(WindowId, crate::DeferredCloseBy),
    #[cfg(unix)]
    NativeTerminate(u64),
    Handoff {
        mode: crate::native_updater_service::ApplyMode,
        attempt: Option<crate::native_updater_service::ApplyAttemptTicket>,
        same_image: Option<crate::app_update_handoff::SameImageHandoff>,
        #[cfg(unix)]
        next_attempt: u64,
    },
}

#[derive(Default)]
pub(crate) struct Drain {
    serial: u64,
    pending: Option<Continuation>,
    drained: Option<Drained>,
    pause: Option<Pause>,
    failed: bool,
    /// When the current drain began.
    since: Option<Instant>,
    /// Its operations outlived [`ACCOUNT_WORK_RETIRES_WITHIN`] or its worker panicked: nothing waits
    /// on them any more, and the epoch stays closed until they retire, or, after a panic, until aterm
    /// restarts.
    outlived: bool,
    /// The automatic attempt the pending handoff will answer.
    parked_auto_apply: Option<AutoApplyAttempt>,
    worker_retry_at: Option<Instant>,
    retirement_pending: bool,
    #[cfg(unix)]
    handoff_attempt: Option<u64>,
}

impl Drain {
    /// The successor's fresh manager is still unpublished, so no work can precede this pause.
    pub(crate) fn for_launch(
        lifetime: &aterm_agent::harness::account_work::Lifetime,
        incoming: bool,
    ) -> Self {
        if !incoming {
            return Self::default();
        }
        match lifetime.pause().and_then(|pause| pause.try_wait().ok()) {
            Some(drained) => Self {
                drained: Some(drained),
                ..Self::default()
            },
            None => Self {
                failed: true,
                ..Self::default()
            },
        }
    }
}

/// What held a handoff at its safety re-read. Unsaved work holds a renewal as it does at a direct look,
/// uncounted, and refuses an update.
fn account_safety_refusal(renewal: bool, reasons: &[String]) -> crate::UpdateHandoffStartError {
    #[cfg(target_os = "macos")]
    if renewal {
        return crate::UpdateHandoffStartError::activity(
            crate::native_update_lane_hold::LaneHold::BoundaryItem,
            reasons.join("; "),
        );
    }
    #[cfg(not(target_os = "macos"))]
    let _ = renewal;
    crate::UpdateHandoffStartError::refused(reasons.join("; "))
}

/// Says [`Wake::AccountsOutlived`] for the drain `serial` once its worker has outlived
/// [`ACCOUNT_WORK_RETIRES_WITHIN`], whether it is waiting on an operation or sealing metadata. Dropping
/// what this returns, as the worker's end does, ends the wait with nothing said. No worker starts
/// without one: that drain stays on the event loop, which watches the same bound.
fn spawn_account_work_bound(
    proxy: aterm_window::event_loop::EventLoopProxy<Wake>,
    serial: u64,
) -> std::io::Result<std::sync::mpsc::Sender<()>> {
    let (worker_lives, worker_ended) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("aterm-account-drain-bound".into())
        .spawn(move || {
            if outlives_account_work_bound(&worker_ended, ACCOUNT_WORK_RETIRES_WITHIN) {
                let _ = proxy.send_event(Wake::AccountsOutlived { serial });
            }
        })?;
    Ok(worker_lives)
}

/// What a worker's settlement comes to: [`Wake::AccountsOutlived`] where it panicked, since its pause
/// died with it, so no settlement will come and nothing else would take what waits.
fn worker_answer(serial: u64, settle: impl FnOnce() -> Settlement) -> Wake {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(settle)) {
        Ok(drained) => Wake::AccountsDrained { serial, drained },
        Err(_) => Wake::AccountsOutlived { serial },
    }
}

/// Whether the worker that holds the other end is still going after `within`.
fn outlives_account_work_bound(
    worker_ended: &std::sync::mpsc::Receiver<()>,
    within: Duration,
) -> bool {
    matches!(
        worker_ended.recv_timeout(within),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    )
}

impl App {
    /// A failed initial surface has no window for a native close prompt; it still owes account settlement.
    pub(crate) fn initial_surface_exit_ready(&mut self) -> bool {
        self.accounts_drained_before(Continuation::StartupFailure)
    }

    /// Close admission immediately; only the worker waits for already admitted resources.
    pub(crate) fn accounts_drained_before(&mut self, continuation: Continuation) -> bool {
        if (self.account_drain.drained.is_some() && !self.account_drain.retirement_pending)
            || self.account_drain.outlived
        {
            return true;
        }
        if self.account_drain.pending.is_some() {
            let ready = self.take_retired_account_pause().or_else(|| {
                self.account_drain
                    .retirement_pending
                    .then(|| self.account_drain.drained.take())
                    .flatten()
            });
            // A quit supersedes an update that has not launched. Its closed epoch transfers
            // directly to shutdown, rather than briefly reopening account admission.
            if !matches!(continuation, Continuation::Handoff { .. }) {
                if let Some(pending) = self.account_drain.pending.take() {
                    self.retire_account_drain_handoff(
                        pending,
                        "shutdown superseded the waiting update",
                    );
                }
                self.account_drain.pending = Some(continuation);
            }
            if let Some(drained) = ready {
                if enrollment_pending(&self.account_work) {
                    self.account_drain.drained = Some(drained);
                    self.account_drain.retirement_pending = true;
                    self.dispatch_account_drain(self.account_drain.serial);
                    if self.account_drain.retirement_pending {
                        return false;
                    }
                    self.account_drain.pending = None;
                    return true;
                }
                self.account_drain.pending = None;
                self.account_drain.drained = Some(drained);
                self.account_drain.failed = false;
                self.account_drain.worker_retry_at = None;
                self.account_drain.retirement_pending = false;
                return true;
            }
            return false;
        }
        if self.account_drain.failed {
            return false;
        }
        let Some(serial) = self.account_drain.serial.checked_add(1) else {
            self.account_drain.failed = true;
            return false;
        };
        let Some(pause) = self.account_work.pause() else {
            self.account_drain.failed = true;
            return false;
        };
        self.account_drain.serial = serial;
        let pause = match pause.try_wait() {
            Ok(drained) => {
                self.account_drain.drained = Some(drained);
                if enrollment_pending(&self.account_work) {
                    self.account_drain.pending = Some(continuation);
                    self.account_drain.retirement_pending = true;
                    self.dispatch_account_drain(serial);
                    if self.account_drain.retirement_pending {
                        return false;
                    }
                    self.account_drain.pending = None;
                }
                return true;
            }
            Err(pause) => pause,
        };
        self.wake_parked_launchers();
        self.account_drain.pending = Some(continuation);
        self.account_drain.pause = Some(pause);
        self.account_drain.since = Some(Instant::now());
        self.dispatch_account_drain(serial);

        false
    }

    /// Rings every session's watchers, so a launcher parked on its session's list reads the
    /// cancellation now. A pause only sets a flag, which that launcher otherwise reads at its next
    /// look, up to a minute later for a launch held on a wall, while the quit or update waits for it
    /// and a stop's own clock can run out first and lose the launch's `why=cancelled`.
    fn wake_parked_launchers(&self) {
        let store = self.store.read().unwrap_or_else(|e| e.into_inner());
        for handle in store.live_handles() {
            handle.ctx.watchers.ring_watchers();
        }
    }

    fn dispatch_account_drain(&mut self, serial: u64) {
        if self.account_drain.pause.is_none() && self.account_drain.drained.is_none() {
            return;
        }
        let Some(proxy) = self.proxy.clone() else {
            self.note_account_drain_worker_unavailable();
            if self.account_drain.drained.is_some() {
                self.account_drain.failed = false;
                aterm_log::info!(
                    "account resources retired; enrollment retirement metadata worker has no event proxy"
                );
                self.account_drain.retirement_pending = false;
            }
            return;
        };
        // Spawn before transferring the unique Pause. A refused or dropped unstarted worker
        // cannot destroy the App's only proof-producing handle.
        let (send, receive) = std::sync::mpsc::sync_channel::<DrainInput>(1);
        let lifetime = self.account_work.clone();
        let worker = spawn_account_work_bound(proxy.clone(), serial).and_then(|bound| {
            std::thread::Builder::new()
                .name("aterm-account-drain".into())
                .spawn(move || {
                    let _bound = bound;
                    if let Ok(input) = receive.recv() {
                        let answer = worker_answer(serial, || settle_accounts(input, &lifetime));
                        let _ = proxy.send_event(answer);
                    }
                })
        });
        match worker {
            Ok(_) => {
                self.account_drain.failed = false;
                self.account_drain.worker_retry_at = None;
                let input = self
                    .account_drain
                    .pause
                    .take()
                    .map(DrainInput::Wait)
                    .or_else(|| self.account_drain.drained.take().map(DrainInput::Ready));
                if let Some(input) = input
                    && let Err(error) = send.send(input)
                {
                    match error.0 {
                        DrainInput::Wait(pause) => self.account_drain.pause = Some(pause),
                        DrainInput::Ready(drained) => self.account_drain.drained = Some(drained),
                    }
                    self.note_account_drain_worker_unavailable();
                    if self.account_drain.drained.is_some() {
                        self.account_drain.retirement_pending = false;
                        self.account_drain.failed = false;
                    }
                }
            }
            Err(error) => {
                if !self.account_drain.failed {
                    if self.account_drain.drained.is_some() {
                        aterm_log::info!(
                            "account resources retired; enrollment retirement metadata worker unavailable: {error}"
                        );
                    } else {
                        aterm_log::info!("account work drain waits on the event loop: {error}");
                    }
                }
                self.note_account_drain_worker_unavailable();
                if self.account_drain.drained.is_some() {
                    self.account_drain.retirement_pending = false;
                    self.account_drain.failed = false;
                }
            }
        }
    }

    fn note_account_drain_worker_unavailable(&mut self) {
        self.account_drain.failed = true;
        self.account_drain.worker_retry_at = self
            .account_drain
            .pause
            .as_ref()
            .map(|_| Instant::now() + WORKER_RETRY);
    }

    fn retry_account_drain_worker(&mut self, now: Instant) -> bool {
        if !self.account_drain.failed
            || self.account_drain.pause.is_none()
            || self.account_drain.pending.is_none()
            || self.account_drain.worker_retry_at.is_none_or(|at| now < at)
        {
            return false;
        }
        self.account_drain.worker_retry_at = None;
        self.dispatch_account_drain(self.account_drain.serial);
        true
    }

    /// A worker-creation failure keeps the original Pause and retries at a bounded pace.
    /// Resource retirement never depends on metadata availability; only a successful seal
    /// enables later recovery.
    pub(crate) fn poll_account_drain(&mut self, el: &ActiveEventLoop) {
        if let Some(drained) = self.take_retired_account_pause() {
            if enrollment_pending(&self.account_work) {
                self.account_drain.drained = Some(drained);
                self.account_drain.retirement_pending = true;
                self.dispatch_account_drain(self.account_drain.serial);
                if !self.account_drain.retirement_pending
                    && let Some(drained) = self.account_drain.drained.take()
                {
                    self.finish_account_drain(
                        el,
                        self.account_drain.serial,
                        Settlement {
                            drained,
                            sealed: false,
                        },
                    );
                }
            } else {
                self.finish_account_drain(
                    el,
                    self.account_drain.serial,
                    Settlement {
                        drained,
                        sealed: true,
                    },
                );
            }
        } else {
            // In this order, since a worker that can start at last still has to retire what is stuck.
            let now = Instant::now();
            if self.account_drain_overdue(now) {
                self.go_on_past_account_work(el, self.account_drain.serial);
            }
            self.retry_account_drain_worker(now);
        }
    }

    /// Whether a drain only this loop watches ([`Self::poll_account_drain`]) has outlived its bound.
    fn account_drain_overdue(&self, now: Instant) -> bool {
        self.account_drain.pause.is_some()
            && self.account_drain.since.is_some_and(|since| {
                now.saturating_duration_since(since) >= ACCOUNT_WORK_RETIRES_WITHIN
            })
    }

    /// Account work has outlived its bound ([`ACCOUNT_WORK_RETIRES_WITHIN`]): what waits on it goes
    /// on, once, and anything that comes later waits no longer.
    pub(crate) fn go_on_past_account_work(&mut self, el: &ActiveEventLoop, serial: u64) {
        let Some(continuation) = self.take_outlived_continuation(serial) else {
            return;
        };
        aterm_log::warn!(
            "account work did not stop within {} s of being cancelled, or its worker panicked: \
             what waited on it goes on, and account work stays off until it stops, or until aterm \
             restarts if its worker panicked",
            ACCOUNT_WORK_RETIRES_WITHIN.as_secs()
        );
        self.continue_after_accounts(el, continuation);
    }

    /// What waited on the drain `serial`, now it has outlived its bound. The drain keeps its pause,
    /// and its retirement still reopens admissions ([`Self::accept_account_drain`]), unless its worker
    /// panicked, which took the pause with it: account work then stays off until aterm restarts.
    fn take_outlived_continuation(&mut self, serial: u64) -> Option<Continuation> {
        let drain = &mut self.account_drain;
        if drain.serial != serial || drain.drained.is_some() || drain.outlived {
            return None;
        }
        let continuation = drain.pending.take()?;
        drain.pending = Some(Continuation::ResumeAfterDrain);
        drain.outlived = true;
        Some(continuation)
    }

    fn take_retired_account_pause(&mut self) -> Option<Drained> {
        let pause = self.account_drain.pause.take()?;
        match pause.try_wait() {
            Ok(drained) => Some(drained),
            Err(pause) => {
                self.account_drain.pause = Some(pause);
                None
            }
        }
    }

    pub(crate) fn account_drain_needs_park_wake(&self) -> bool {
        self.account_drain.pause.is_some()
    }

    fn retire_account_drain_handoff(&mut self, pending: Continuation, reason: &str) {
        let Continuation::Handoff {
            attempt,
            same_image,
            ..
        } = pending
        else {
            return;
        };
        if same_image.is_some_and(crate::app_update_handoff::SameImageHandoff::renews_launch_record)
        {
            self.renewal_did_not_start(reason);
        }
        if let Some(attempt) = attempt
            && self.native_updater_service.abort_apply(&attempt, reason)
        {
            self.publish_native_update_state();
        }
        if let Some(parked) = self.account_drain.parked_auto_apply.take() {
            // What held the attempt was the account work its handoff waited behind.
            self.settle_auto_apply_attempt(
                parked,
                UpdateOutcome::Deferred {
                    reason: reason.to_string(),
                    hold: crate::native_update_lane_hold::LaneHold::BoundaryItem,
                },
            );
        }
    }

    pub(crate) fn account_drain_pending(&self) -> bool {
        self.account_drain.pending.is_some() && !self.account_drain.outlived
    }

    #[cfg(all(test, unix))]
    pub(crate) fn account_drain_pending_for_test(&self) -> Option<&Continuation> {
        self.account_drain.pending.as_ref()
    }

    /// Whether a handoff is waiting on account work, so an attempt that started it has no answer yet.
    pub(crate) fn account_drain_holds_a_handoff(&self) -> bool {
        matches!(
            self.account_drain.pending,
            Some(Continuation::Handoff { .. })
        )
    }

    /// Whether the handoff the drain holds is a launch-record renewal's.
    #[cfg(unix)]
    pub(crate) fn account_drain_holds_a_renewal(&self) -> bool {
        matches!(
            self.account_drain.pending,
            Some(Continuation::Handoff { same_image: Some(same_image), .. })
                if same_image.renews_launch_record()
        )
    }

    /// A person's press takes the place of a launch-record renewal's handoff waiting on account work.
    /// The renewal installs nothing and an automatic lane follows it, but a press has no lane to follow
    /// it: told to wait, it was dropped with auto-apply off. The press now waits on the drain as any
    /// update's handoff does, and the person hears what came of it.
    #[cfg(unix)]
    pub(crate) fn press_takes_the_renewal_s_place(&mut self, press: Continuation) {
        if let Some(renewal) = self.account_drain.pending.replace(press) {
            self.retire_account_drain_handoff(renewal, "a person's Install took its place");
        }
    }

    /// The caller hears this handoff failed, so it must not start later: the drain goes on proving
    /// retirement, and its end reopens account work. Only the call that just stored it can reach this,
    /// since a drain already pending refuses a later one first.
    pub(crate) fn disarm_failed_account_handoff(&mut self) {
        if matches!(
            self.account_drain.pending,
            Some(Continuation::Handoff { .. })
        ) {
            self.account_drain.pending = Some(Continuation::ResumeAfterDrain);
        }
    }

    /// Keep `attempt` until the handoff waiting on account work answers it.
    pub(crate) fn park_auto_apply_for_accounts(&mut self, attempt: AutoApplyAttempt) {
        self.account_drain.parked_auto_apply = Some(attempt);
    }

    pub(crate) fn account_drain_unavailable(&self) -> bool {
        self.account_drain.failed
    }

    /// Reopen only the closed, actually drained epoch this App still owns.
    pub(crate) fn resume_drained_accounts(&mut self) {
        #[cfg(unix)]
        if self.account_drain.handoff_attempt.is_some() {
            return;
        }
        // A refusal may arrive before the worker can return Drained. Disarm the exit
        // continuation now, but keep its unique Pause until actual resource retirement.
        match self.account_drain.pending.as_ref() {
            Some(
                Continuation::Quit | Continuation::Window(..) | Continuation::ResumeAfterDrain,
            ) => {
                self.account_drain.pending = Some(Continuation::ResumeAfterDrain);
            }
            #[cfg(unix)]
            Some(Continuation::NativeTerminate(_)) => {
                self.account_drain.pending = Some(Continuation::ResumeAfterDrain);
            }
            Some(Continuation::Handoff { .. } | Continuation::StartupFailure) => return,
            None => {}
        }
        if let Some(drained) = self.account_drain.drained.take()
            && let Err(drained) = drained.resume()
        {
            self.account_drain.drained = Some(drained);
        }
    }

    pub(crate) fn disarm_account_window_close(&mut self, window: WindowId) {
        if matches!(self.account_drain.pending, Some(Continuation::Window(held, _)) if held == window)
        {
            self.resume_drained_accounts();
        }
    }

    #[cfg(unix)]
    pub(crate) fn bind_account_handoff(&mut self, attempt: u64) {
        self.account_drain.handoff_attempt = Some(attempt);
    }

    /// A stale completion cannot reopen the epoch held by a newer handoff or shutdown.
    #[cfg(unix)]
    pub(crate) fn returned_account_handoff(&mut self, attempt: u64, quitting: bool) {
        if self.account_drain.handoff_attempt == Some(attempt) {
            self.account_drain.handoff_attempt = None;
            if !quitting {
                self.resume_drained_accounts();
            }
        }
    }

    #[cfg(unix)]
    pub(crate) fn account_quit_generation(&mut self, generation: u64) {
        if matches!(self.account_drain.pending, Some(Continuation::Quit)) {
            self.account_drain.pending = Some(Continuation::NativeTerminate(generation));
        }
    }

    fn accept_account_drain(&mut self, serial: u64, drained: Drained) -> Option<Continuation> {
        if self.account_drain.serial != serial {
            return None;
        }
        let continuation = self.account_drain.pending.take()?;
        self.account_drain.drained = Some(drained);
        self.account_drain.failed = false;
        self.account_drain.outlived = false;
        self.account_drain.worker_retry_at = None;
        self.account_drain.retirement_pending = false;
        if matches!(continuation, Continuation::ResumeAfterDrain) {
            self.resume_drained_accounts();
            return None;
        }
        Some(continuation)
    }

    pub(crate) fn finish_account_drain(
        &mut self,
        el: &ActiveEventLoop,
        serial: u64,
        settlement: Settlement,
    ) {
        let Some(continuation) = self.accept_account_settlement(serial, settlement) else {
            return;
        };
        self.continue_after_accounts(el, continuation);
    }

    fn continue_after_accounts(&mut self, el: &ActiveEventLoop, continuation: Continuation) {
        match continuation {
            Continuation::ResumeAfterDrain => self.resume_drained_accounts(),
            Continuation::StartupFailure => el.exit(),
            Continuation::Quit => match self.quit_barriers() {
                crate::app_window::QuitBarrier::Ready => {
                    let _ = self.video_abort_app_shutdown();
                    el.exit();
                }
                crate::app_window::QuitBarrier::Refused => self.resume_drained_accounts(),
                crate::app_window::QuitBarrier::Saving => {}
            },
            Continuation::Window(window, by) => {
                let _by = by.map(|(reason, actor)| {
                    crate::session_store::CloseAttribution::enter(reason, actor)
                });
                if matches!(
                    self.close_window_through_barriers(window),
                    CloseOutcome::Exit
                ) {
                    el.exit();
                } else {
                    self.resume_drained_accounts();
                }
            }
            #[cfg(unix)]
            Continuation::NativeTerminate(generation) => {
                if crate::menu::native_termination_is_current(generation) {
                    self.resume_native_termination(el, generation, true);
                } else {
                    self.resume_drained_accounts();
                }
            }
            handoff @ Continuation::Handoff { .. } => self.finish_account_handoff(handoff),
        }
    }

    /// Nothing has been captured while account work drains. Re-read safety now; activity
    /// before this point cannot invalidate a snapshot that has not been taken. What the handoff comes
    /// to reads as [`Self::apply_native_update`] would have answered it.
    pub(crate) fn finish_account_handoff(&mut self, handoff: Continuation) {
        let Continuation::Handoff {
            mode,
            attempt,
            same_image,
            #[cfg(unix)]
            next_attempt,
        } = handoff
        else {
            unreachable!("only a handoff continuation reaches its completion");
        };
        #[cfg(unix)]
        let superseded = self.next_update_handoff_id != next_attempt;
        #[cfg(not(unix))]
        let superseded = false;
        let renewal = same_image
            .is_some_and(crate::app_update_handoff::SameImageHandoff::renews_launch_record);
        if let Some(reason) = self.handoff_gives_way(mode, attempt.as_ref(), renewal, superseded) {
            self.retire_account_drain_handoff(
                Continuation::Handoff {
                    mode,
                    attempt,
                    same_image,
                    #[cfg(unix)]
                    next_attempt,
                },
                reason,
            );
            self.resume_drained_accounts();
            return;
        }
        let retained_attempt = attempt.clone();
        let result = self
            .revalidate_native_update_safety()
            .map_err(|reasons| account_safety_refusal(renewal, &reasons))
            .and_then(|safety| self.apply_staged_update_now(safety, mode, attempt, same_image));
        let outcome = match result {
            Ok(()) => UpdateOutcome::Accepted,
            Err(error) => {
                self.resume_drained_accounts();
                #[cfg(target_os = "macos")]
                if renewal {
                    self.renewal_refused_after_accounts(&error);
                }
                match &retained_attempt {
                    Some(attempt) => self.account_update_start_outcome(attempt, mode, error),
                    None => UpdateOutcome::Failed {
                        message: error.into_message(),
                    },
                }
            }
        };
        let target = retained_attempt
            .as_ref()
            .map(crate::native_updater_service::ApplyAttemptTicket::target_build);
        self.answer_handoff_after_accounts(mode, target, renewal, outcome);
    }

    /// Why a handoff that waited on account work may no longer start: another took the next attempt
    /// or the stage's ticket, or `[update] auto_apply` went off under an automatic attempt, which the
    /// lane's next look reads again. A person's press is not held by that setting, and a launch-record
    /// renewal installs nothing.
    pub(crate) fn handoff_gives_way(
        &self,
        mode: ApplyMode,
        attempt: Option<&crate::native_updater_service::ApplyAttemptTicket>,
        renewal: bool,
        superseded: bool,
    ) -> Option<&'static str> {
        if superseded {
            Some("the handoff changed while account work drained")
        } else if attempt
            .is_some_and(|attempt| !self.native_updater_service.apply_ticket_is_current(attempt))
        {
            Some("another update attempt took over while account work drained")
        } else if mode.is_automatic()
            && !renewal
            && !crate::app_config::update_auto_apply(&self.config)
        {
            Some("automatic updates were turned off while account work drained")
        } else {
            None
        }
    }

    /// Answer the apply that waited on account work, as a caller of
    /// [`Self::apply_native_update`] answers one that did not wait. An automatic attempt settles
    /// with what came of it, which says so itself. A person's press hears what came of it, and the
    /// automatic lane is armed again, as for a control request that did not land; an automatic attempt
    /// that joined that press looks again. A renewal that did not start goes back to its own ladder.
    /// A press that stopped here ends as one held for a read does, its row no longer saying it is
    /// installing and its switch record ended; unlike that one's, the record here is routine where the
    /// press was deferred and the lane will try again. Deferred with a ready row up it gets its own
    /// word unless the lane will try again: the row says nothing new, and a Settings tab opening this
    /// long after the press would take focus.
    pub(crate) fn answer_handoff_after_accounts(
        &mut self,
        mode: ApplyMode,
        target: Option<u64>,
        renewal: bool,
        outcome: UpdateOutcome,
    ) {
        let started = outcome == UpdateOutcome::Accepted;
        if renewal && !started {
            self.renewal_did_not_start(&format!("{outcome:?}"));
        }
        if let Some(parked) = self.account_drain.parked_auto_apply.take() {
            let answer = if started || mode.is_automatic() {
                outcome.clone()
            } else {
                UpdateOutcome::Deferred {
                    reason: "the install it joined did not start".to_string(),
                    hold: crate::native_update_lane_hold::LaneHold::HandoffInFlight,
                }
            };
            self.settle_auto_apply_attempt(parked, answer);
            if mode.is_automatic() {
                return;
            }
        }
        if started {
            return;
        }
        let Some(target) = target else {
            aterm_log::info!("update waits after account work drained: {outcome:?}");
            return;
        };
        #[cfg(unix)]
        let stopped = if mode.is_automatic() {
            None
        } else {
            outcome.why_it_stopped()
        };
        #[cfg(unix)]
        if stopped.is_some() {
            self.retire_update_installing();
        }
        let deferred = matches!(outcome, UpdateOutcome::Deferred { .. });
        self.surface_update_apply_outcome_for_target("account settlement", outcome, false, target);
        if mode.is_automatic() {
            return;
        }
        let staged = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|staged| (staged.build, staged.dmg_sha256.clone()));
        if let Some((build, digest)) = staged {
            self.arm_native_auto_apply(build, &digest);
        }
        let retries = self.automatic_apply_retry_scheduled(target);
        #[cfg(unix)]
        if let Some(why) = stopped {
            self.record_update_switch_stopped(deferred && retries, &why);
        }
        if deferred && !retries && self.staged_update_row().is_some() {
            let message = self.stopped_lane_outcome(crate::app_update_screen::UPDATE_DIDNT_INSTALL);
            self.note_update_outcome(message);
        }
    }

    fn renewal_did_not_start(&mut self, detail: &str) {
        #[cfg(target_os = "macos")]
        self.renewal_did_not_start_after_accounts(detail);
        #[cfg(not(target_os = "macos"))]
        let _ = detail;
    }

    /// A person's press joined the handoff waiting on account work: it now carries that press, so
    /// what stops only an automatic attempt no longer stops it, and the person hears what came of it.
    pub(crate) fn press_joins_handoff_waiting_on_accounts(&mut self) {
        if let Some(Continuation::Handoff {
            mode,
            attempt: Some(_),
            ..
        }) = &mut self.account_drain.pending
        {
            *mode = ApplyMode::Immediate;
        }
    }

    pub(crate) fn accept_account_settlement(
        &mut self,
        serial: u64,
        settlement: Settlement,
    ) -> Option<Continuation> {
        if self.account_drain.serial != serial {
            return None;
        }
        if !settlement.durably_sealed() {
            aterm_log::info!(
                "account resources retired; enrollment retirement metadata remains uncertain"
            );
        }
        let Settlement { drained, .. } = settlement;
        self.accept_account_drain(serial, drained)
    }

    /// Account settlement completed after the original request was accepted. A temporary
    /// refusal retains that request's artifact and retry cadence instead of waiting for a check.
    fn account_update_start_outcome(
        &mut self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
        mode: crate::native_updater_service::ApplyMode,
        error: crate::UpdateHandoffStartError,
    ) -> crate::native_app::UpdateOutcome {
        let outcome = self.unstarted_native_apply_outcome(attempt, mode, error);
        #[cfg(unix)]
        if mode.is_automatic() && self.auto_apply_intent.is_none() {
            match &outcome {
                crate::native_app::UpdateOutcome::Deferred { .. } => {
                    self.arm_activity_revoked_overlap_retry(attempt);
                }
                crate::native_app::UpdateOutcome::Blocked { .. } => {
                    self.arm_account_preflight_retry(attempt);
                }
                _ => {}
            }
        }
        outcome
    }
}

#[cfg(test)]
#[path = "app_account_work_tests.rs"]
mod tests;
