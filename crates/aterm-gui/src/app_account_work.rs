// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! App-owned account work is drained before process replacement or logical exit.

use aterm_agent::harness::account_work::{Drained, Pause};
use aterm_window::event_loop::ActiveEventLoop;
use std::time::{Duration, Instant};

use crate::{App, CloseOutcome, Wake, WindowId};

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
    const CONSTRUCTOR_SWEEP: Duration = Duration::from_millis(10);
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
            wait.min(CONSTRUCTOR_SWEEP)
        } else {
            wait
        });
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

impl App {
    /// A failed initial surface has no window for a native close prompt; it still owes account settlement.
    pub(crate) fn initial_surface_exit_ready(&mut self) -> bool {
        self.accounts_drained_before(Continuation::StartupFailure)
    }

    /// Close admission immediately; only the worker waits for already admitted resources.
    pub(crate) fn accounts_drained_before(&mut self, continuation: Continuation) -> bool {
        if self.account_drain.drained.is_some() && !self.account_drain.retirement_pending {
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
        match std::thread::Builder::new()
            .name("aterm-account-drain".into())
            .spawn(move || {
                if let Ok(input) = receive.recv() {
                    let drained = settle_accounts(input, &lifetime);
                    let _ = proxy.send_event(Wake::AccountsDrained { serial, drained });
                }
            }) {
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
            self.retry_account_drain_worker(Instant::now());
        }
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
        if let Continuation::Handoff {
            attempt: Some(attempt),
            ..
        } = pending
            && self.native_updater_service.abort_apply(&attempt, reason)
        {
            self.publish_native_update_state();
        }
    }

    pub(crate) fn account_drain_pending(&self) -> bool {
        self.account_drain.pending.is_some()
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
    /// before this point cannot invalidate a snapshot that has not been taken.
    fn finish_account_handoff(&mut self, handoff: Continuation) {
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
        if self.next_update_handoff_id != next_attempt {
            self.retire_account_drain_handoff(
                Continuation::Handoff {
                    mode,
                    attempt,
                    same_image,
                    next_attempt,
                },
                "the handoff changed while account work drained",
            );
            self.resume_drained_accounts();
            return;
        }
        if attempt
            .as_ref()
            .is_some_and(|attempt| !self.native_updater_service.apply_ticket_is_current(attempt))
        {
            self.resume_drained_accounts();
            return;
        }
        let retained_attempt = attempt.clone();
        let result = self
            .revalidate_native_update_safety()
            .map_err(|reasons| crate::UpdateHandoffStartError::refused(reasons.join("; ")))
            .and_then(|safety| self.apply_staged_update_now(safety, mode, attempt, same_image));
        if let Err(error) = result {
            self.resume_drained_accounts();
            if let Some(attempt) = retained_attempt {
                let outcome = self.account_update_start_outcome(&attempt, mode, error);
                self.surface_update_apply_outcome_for_target(
                    "account settlement",
                    outcome,
                    false,
                    attempt.target_build(),
                );
            } else {
                aterm_log::info!("update waits after account work drained: {error}");
            }
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
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn paused_app_retires_the_actual_unclaimed_constructor_and_releases_its_gate() {
        let (root, sources, lifetime) = crate::task_launch::pending_codex_constructor_for_drain();
        let gate = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.path().join("home/.aterm/codex-source-custody/gate"))
            .unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "observe the actual pending constructor's kernel exclusion"
        )]
        let before = gate.try_lock();
        assert!(matches!(before, Err(std::fs::TryLockError::WouldBlock)));
        let pause = lifetime
            .pause()
            .unwrap()
            .try_wait()
            .expect_err("the real constructor still owns its App scope");
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let worker_sources = sources.clone();
        let worker_lifetime = lifetime.clone();
        let worker = std::thread::spawn(move || {
            send.send(drain_account_sources(
                pause,
                Some(&worker_sources),
                &worker_lifetime,
            ))
            .unwrap();
        });
        let drained = receive
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("cancelled unclaimed constructor must retire");
        worker.join().unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "verify revocation released the actual constructor gate"
        )]
        gate.try_lock().unwrap();
        assert_eq!(sources.revoke_cancelled_unclaimed().unwrap(), 0);
        drained.resume().unwrap();
        assert!(lifetime.enter().is_some());
    }

    #[test]
    fn source_cleanup_does_not_retire_other_owned_account_work() {
        for registered in [false, true] {
            let lifetime = aterm_agent::harness::account_work::Lifetime::default();
            let work = lifetime.enter().unwrap();
            let pause = lifetime.pause().unwrap();
            assert!(work.cancelled());
            let (send, receive) = std::sync::mpsc::sync_channel(1);
            let worker_lifetime = lifetime.clone();
            let worker = std::thread::spawn(move || {
                let sources =
                    registered.then(aterm_agent::harness::codex_source_registry::Registry::default);
                let drained = drain_account_sources(pause, sources.as_ref(), &worker_lifetime);
                send.send(drained).unwrap();
            });
            assert!(matches!(
                receive.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ));
            assert!(lifetime.enter().is_none());
            drop(work);
            let drained = receive
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("actual owned work retired");
            worker.join().unwrap();
            drained.resume().unwrap();
            assert!(lifetime.enter().is_some());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn activity_during_account_retirement_reaches_the_fresh_handoff_preflight() {
        if !crate::test_alone::alone() {
            return;
        }
        use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};
        let mut app = App::headless_for_test();
        let attempt = ApplyAttemptTicket::for_test(42, "committed-build", &"ab".repeat(32));
        attempt.make_current_apply_for_test(&mut app.native_updater_service);
        let work = app.account_work.enter().unwrap();
        assert!(!app.accounts_drained_before(Continuation::Handoff {
            mode: ApplyMode::AutomaticPastGrace,
            attempt: Some(attempt),
            same_image: None,
            next_attempt: app.next_update_handoff_id,
        }));
        app.note_update_handoff_activity();
        // A real blocker appearing while the accounts retire must be read now, rather
        // than discarding the request merely because the earlier activity counter moved.
        app.land_install_session_for_test(0);
        drop(work);
        let drained = app.take_retired_account_pause().unwrap();
        let handoff = app
            .accept_account_drain(app.account_drain.serial, drained)
            .unwrap();
        app.finish_account_handoff(handoff);
        assert_eq!(
            app.native_updater_service.snapshot().error.as_deref(),
            Some("a System State Apply or Restore is still pending")
        );
        assert!(
            app.auto_apply_intent.is_some(),
            "the automatic request remains owed"
        );
        assert!(
            app.account_work.enter().is_some(),
            "the deferred handoff reopened account work"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_handoff_or_artifact_that_superseded_the_drain_is_never_resumed() {
        use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};
        for new_artifact in [false, true] {
            let mut app = App::headless_for_test();
            let attempt = ApplyAttemptTicket::for_test(42, "committed-build", &"ab".repeat(32));
            attempt.make_current_apply_for_test(&mut app.native_updater_service);
            let work = app.account_work.enter().unwrap();
            assert!(!app.accounts_drained_before(Continuation::Handoff {
                mode: ApplyMode::AutomaticPastGrace,
                attempt: Some(attempt.clone()),
                same_image: None,
                next_attempt: app.next_update_handoff_id,
            }));
            let newer = ApplyAttemptTicket::for_test(43, "newer-build", &"cd".repeat(32));
            if new_artifact {
                newer.make_current_apply_for_test(&mut app.native_updater_service);
            } else {
                app.next_update_handoff_id += 1;
            }
            drop(work);
            let drained = app.take_retired_account_pause().unwrap();
            let handoff = app
                .accept_account_drain(app.account_drain.serial, drained)
                .unwrap();
            app.finish_account_handoff(handoff);
            assert!(app.auto_apply_intent.is_none());
            assert!(app.account_work.enter().is_some());
            if new_artifact {
                assert!(app.native_updater_service.apply_ticket_is_current(&newer));
            } else {
                assert!(!app.native_updater_service.apply_ticket_is_current(&attempt));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_late_account_deferral_retries_only_the_automatic_request_and_keeps_its_artifact() {
        use crate::native_app::UpdateOutcome;
        use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode, UpdaterPhase};

        for mode in [ApplyMode::AutomaticPastGrace, ApplyMode::Immediate] {
            let mut app = App::headless_for_test();
            let digest = "ab".repeat(32);
            let attempt = ApplyAttemptTicket::for_test(42, "committed-build", &digest);
            attempt.make_current_apply_for_test(&mut app.native_updater_service);
            let outcome = app.account_update_start_outcome(
                &attempt,
                mode,
                crate::UpdateHandoffStartError::activity("a tab is still starting"),
            );
            assert_eq!(
                outcome,
                UpdateOutcome::Deferred {
                    reason: "a tab is still starting".into()
                }
            );
            let snapshot = app.native_updater_service.snapshot();
            assert_eq!(snapshot.phase, UpdaterPhase::Staged);
            assert_eq!(snapshot.error.as_deref(), Some("a tab is still starting"));
            assert_eq!(snapshot.staged.as_ref().unwrap().dmg_sha256, digest);
            if mode.is_automatic() {
                let retry = app
                    .auto_apply_intent
                    .expect("the accepted automatic request remains owed");
                assert_eq!(retry.build, 42);
                assert_eq!(retry.dmg_sha256, [0xab; 32]);
            } else {
                assert!(app.auto_apply_intent.is_none());
            }
        }
    }

    #[test]
    fn a_late_account_refusal_or_failure_keeps_its_verdict() {
        use crate::native_app::UpdateOutcome;
        use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};

        for (error, expected) in [
            (
                crate::UpdateHandoffStartError::refused("draft changed"),
                UpdateOutcome::Blocked {
                    reasons: vec!["draft changed".into()],
                },
            ),
            (
                crate::UpdateHandoffStartError::failed("launch failed"),
                UpdateOutcome::Failed {
                    message: "launch failed".into(),
                },
            ),
        ] {
            let mut app = App::headless_for_test();
            let attempt = ApplyAttemptTicket::for_test(42, "committed-build", &"ab".repeat(32));
            attempt.make_current_apply_for_test(&mut app.native_updater_service);
            assert_eq!(
                app.account_update_start_outcome(&attempt, ApplyMode::Immediate, error),
                expected
            );
            assert!(app.auto_apply_intent.is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_late_preflight_block_keeps_a_bounded_retry_for_the_same_artifact() {
        use crate::native_app::UpdateOutcome;
        use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};
        let mut app = App::headless_for_test();
        let attempt = ApplyAttemptTicket::for_test(42, "committed-build", &"ab".repeat(32));
        attempt.make_current_apply_for_test(&mut app.native_updater_service);
        assert_eq!(
            app.account_update_start_outcome(
                &attempt,
                ApplyMode::AutomaticPastGrace,
                crate::UpdateHandoffStartError::refused("draft changed"),
            ),
            UpdateOutcome::Blocked {
                reasons: vec!["draft changed".into()]
            },
        );
        let retry = app.auto_apply_intent.unwrap();
        assert_eq!(retry.build, 42);
        assert_eq!(retry.dmg_sha256, [0xab; 32]);
        app.auto_overlap_retry.as_mut().unwrap().cycles = u8::MAX;
        let cooldown = app.arm_account_preflight_retry(&attempt).unwrap();
        assert!(cooldown > std::time::Duration::ZERO);
        assert!(cooldown <= std::time::Duration::from_secs(60));
        app.auto_apply_intent = None;
        attempt.make_current_apply_for_test(&mut app.native_updater_service);
        let _ = app.account_update_start_outcome(
            &attempt,
            ApplyMode::AutomaticPastGrace,
            crate::UpdateHandoffStartError::failed("launch failed"),
        );
        assert!(
            app.auto_apply_intent.is_none(),
            "a real failure is not a cheap preflight retry"
        );
    }

    #[test]
    fn unavailable_retirement_metadata_does_not_turn_real_resource_retirement_into_a_quit_stall() {
        let mut app = App::headless_for_test();
        let work = app.account_work.enter().unwrap();
        assert!(!app.accounts_drained_before(Continuation::Quit));
        drop(work);
        let drained = app.take_retired_account_pause().unwrap();
        let serial = app.account_drain.serial;
        assert!(matches!(
            app.accept_account_settlement(
                serial,
                Settlement {
                    drained,
                    sealed: false
                }
            ),
            Some(Continuation::Quit)
        ));
        assert!(app.account_drain.drained.is_some());
        assert!(!app.account_drain.failed);
        assert!(!app.account_drain.retirement_pending);
        assert!(app.account_drain.worker_retry_at.is_none());
        assert!(app.account_work.enter().is_none());
        app.resume_drained_accounts();
        assert!(app.account_work.enter().is_some());
    }

    #[test]
    fn an_empty_quit_closes_admission_without_a_worker_or_completion_channel() {
        let mut app = App::headless_for_test();
        assert!(app.proxy.is_none());
        assert!(app.account_work.enter().is_some());
        assert_eq!(app.quit_barriers(), crate::app_window::QuitBarrier::Ready);
        assert!(app.account_work.enter().is_none());
        assert!(app.account_drain.drained.is_some());
        assert!(!app.account_drain.failed);
    }

    #[test]
    fn a_busy_last_window_stays_logically_present_without_settlement_delivery() {
        let mut app = App::headless_for_test();
        let work = app.account_work.enter().unwrap();
        let sessions = app
            .pool
            .iter()
            .map(|session| session.id)
            .collect::<Vec<_>>();
        assert!(matches!(
            app.close_window_logical(WindowId(0)),
            CloseOutcome::Stay
        ));
        assert!(work.cancelled());
        assert!(app.windows.contains_key(&WindowId(0)));
        assert_eq!(
            app.pool
                .iter()
                .map(|session| session.id)
                .collect::<Vec<_>>(),
            sessions
        );
        assert!(app.account_drain.drained.is_none());
        drop(work);
        // The retained Pause can prove actual settlement on the next request, without a worker.
        assert!(app.account_work.enter().is_none());
        assert!(app.account_drain.pause.is_some());
        // Exercise the event loop's nonblocking retirement path, with no second quit request.
        let drained = app.take_retired_account_pause().unwrap();
        let serial = app.account_drain.serial;
        assert!(matches!(
            app.accept_account_drain(serial, drained),
            Some(Continuation::Window(WindowId(0), _))
        ));
        app.resume_drained_accounts();
        assert!(app.account_work.enter().is_some());
        assert!(!app.account_drain.failed);
    }

    #[test]
    fn initial_surface_failure_waits_for_actual_account_retirement() {
        for busy in [false, true] {
            let mut app = App::headless_for_test();
            let work = busy.then(|| app.account_work.enter().unwrap());
            assert_eq!(app.initial_surface_exit_ready(), !busy);
            assert!(app.account_work.enter().is_none());
            if let Some(work) = work {
                assert!(work.cancelled());
                assert!(app.take_retired_account_pause().is_none());
                drop(work);
                let drained = app.take_retired_account_pause().unwrap();
                let serial = app.account_drain.serial;
                assert!(matches!(
                    app.accept_account_drain(serial, drained),
                    Some(Continuation::StartupFailure)
                ));
                assert!(app.initial_surface_exit_ready());
            }
            assert!(app.account_work.enter().is_none());
        }
    }

    #[test]
    fn a_refused_last_window_close_does_not_enter_the_account_gate() {
        use crate::native_app::{ActionInvocation, AppEvent, TextInputEvent};
        let mut app = App::headless_for_test();
        let window = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let (_, settings) = app.active_native_view(window).unwrap();
        for event in [
            AppEvent::Action(ActionInvocation {
                id: crate::native_ui::ActionId::new(format!(
                    "settings/set/{}",
                    crate::prefs::EDIT_FONT_FAMILY
                )),
                value: None,
            }),
            AppEvent::TextInput(TextInputEvent::SelectAll),
            AppEvent::TextInput(TextInputEvent::Commit("Inert Draft Mono".into())),
        ] {
            app.dispatch_native_view_event(window, settings, event)
                .unwrap();
        }
        let work = app.account_work.enter().unwrap();
        assert!(matches!(
            app.close_window_through_barriers(window),
            CloseOutcome::Stay
        ));
        assert!(!work.cancelled());
        assert!(app.account_work.enter().is_some());
        assert!(app.account_drain.drained.is_none());
        assert!(app.windows.contains_key(&window));
    }

    #[test]
    fn a_refused_busy_close_disarms_its_exit_even_if_the_draft_is_resolved_before_retirement() {
        use crate::native_app::{ActionInvocation, AppEvent, TextInputEvent};
        for (window_close, rearm) in [(false, false), (false, true), (true, false), (true, true)] {
            let mut app = App::headless_for_test();
            let work = app.account_work.enter().unwrap();
            if window_close {
                assert!(matches!(
                    app.close_window_through_barriers(WindowId(0)),
                    CloseOutcome::Stay
                ));
                assert!(matches!(
                    app.account_drain.pending,
                    Some(Continuation::Window(..))
                ));
            } else {
                assert_eq!(app.quit_barriers(), crate::app_window::QuitBarrier::Saving);
                assert!(matches!(
                    app.account_drain.pending,
                    Some(Continuation::Quit)
                ));
            }
            let window = WindowId(0);
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
            let (_, settings) = app.active_native_view(window).unwrap();
            for event in [
                AppEvent::Action(ActionInvocation {
                    id: crate::native_ui::ActionId::new(format!(
                        "settings/set/{}",
                        crate::prefs::EDIT_FONT_FAMILY
                    )),
                    value: None,
                }),
                AppEvent::TextInput(TextInputEvent::SelectAll),
                AppEvent::TextInput(TextInputEvent::Commit("Inert Draft Mono".into())),
            ] {
                app.dispatch_native_view_event(window, settings, event)
                    .unwrap();
            }
            if window_close {
                assert!(matches!(
                    app.close_window_through_barriers(window),
                    CloseOutcome::Stay
                ));
            } else {
                assert_eq!(app.quit_barriers(), crate::app_window::QuitBarrier::Refused);
            }
            assert!(matches!(
                app.account_drain.pending,
                Some(Continuation::ResumeAfterDrain)
            ));
            // Resolving the draft does not revive the refused request.
            for _ in 0..2 {
                app.dispatch_native_view_event(
                    window,
                    settings,
                    AppEvent::Action(ActionInvocation {
                        id: crate::native_ui::ActionId::new("settings/drafts/discard-all"),
                        value: None,
                    }),
                )
                .unwrap();
            }
            assert!(app.prepare_quit_native_shutdown().unwrap());
            assert!(app.account_work.enter().is_none());
            if rearm {
                if window_close {
                    assert!(matches!(
                        app.close_window_through_barriers(window),
                        CloseOutcome::Stay
                    ));
                } else {
                    assert_eq!(app.quit_barriers(), crate::app_window::QuitBarrier::Saving);
                }
            }
            drop(work);
            let drained = app.take_retired_account_pause().unwrap();
            let serial = app.account_drain.serial;
            let continuation = app.accept_account_drain(serial, drained);
            if rearm {
                if window_close {
                    assert!(matches!(
                        continuation,
                        Some(Continuation::Window(WindowId(0), _))
                    ));
                } else {
                    assert!(matches!(continuation, Some(Continuation::Quit)));
                }
                assert!(app.account_work.enter().is_none());
                assert_eq!(app.quit_barriers(), crate::app_window::QuitBarrier::Ready);
            } else {
                assert!(
                    continuation.is_none(),
                    "the cancelled completion cannot execute Quit"
                );
                assert!(app.account_work.enter().is_some());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_busy_update_cannot_start_any_successor_without_settlement_delivery() {
        let mut app = App::headless_for_test();
        let work = app.account_work.enter().unwrap();
        let attempt = app.next_update_handoff_id;
        let result = app.apply_staged_update_now(
            crate::app_native::NativeUpdateSafetyToken::carrying_for_test(0, 0, 0),
            crate::native_updater_service::ApplyMode::Immediate,
            None,
            None,
        );
        assert!(result.is_err());
        assert!(work.cancelled());
        assert_eq!(app.next_update_handoff_id, attempt);
        assert!(app.pending_update_handoff.is_none());
        assert!(app.account_drain.drained.is_none());
        assert!(app.account_work.enter().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_quit_wait_does_not_accept_an_unqueued_update_request() {
        let mut app = App::headless_for_test();
        let _work = app.account_work.enter().unwrap();
        assert!(!app.accounts_drained_before(Continuation::Quit));
        assert!(
            app.apply_staged_update_now(
                crate::app_native::NativeUpdateSafetyToken::carrying_for_test(0, 0, 0),
                crate::native_updater_service::ApplyMode::Immediate,
                None,
                None,
            )
            .is_err()
        );
        assert!(matches!(
            app.account_drain.pending,
            Some(Continuation::Quit)
        ));
        assert!(app.pending_update_handoff.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn only_the_matching_returned_handoff_can_release_its_drained_epoch() {
        let mut app = App::headless_for_test();
        assert!(app.accounts_drained_before(Continuation::Quit));
        app.bind_account_handoff(7);
        app.returned_account_handoff(6, false);
        assert!(app.account_work.enter().is_none());
        app.returned_account_handoff(7, false);
        assert!(app.account_work.enter().is_some());
        assert!(app.accounts_drained_before(Continuation::Quit));
        app.bind_account_handoff(8);
        app.returned_account_handoff(7, false);
        assert!(app.account_work.enter().is_none());
        // A matching completion hands its closed scope directly to deferred Quit.
        app.returned_account_handoff(8, true);
        assert!(app.account_work.enter().is_none());
        app.resume_drained_accounts();
        assert!(app.account_work.enter().is_some());
    }

    #[test]
    fn incoming_successor_clones_stay_closed_until_its_commit_releases_them() {
        let mut app = App::headless_for_test();
        app.account_drain = Drain::for_launch(&app.account_work, true);
        let host = app.account_work.clone();
        let menu = app.account_work.clone();
        assert!(host.enter().is_none());
        assert!(menu.enter().is_none());
        assert!(app.account_drain.drained.is_some());
        app.resume_drained_accounts();
        assert!(host.enter().is_some());
        assert!(menu.enter().is_some());
    }
    #[test]
    fn unavailable_drain_worker_retries_without_losing_custody_or_reopening_admission() {
        let mut app = App::headless_for_test();
        assert!(app.proxy.is_none());
        let work = app.account_work.enter().unwrap();
        assert!(!app.accounts_drained_before(Continuation::ResumeAfterDrain));
        let serial = app.account_drain.serial;
        let due = app
            .account_drain
            .worker_retry_at
            .expect("unavailable dispatch is retried");
        assert!(work.cancelled());
        assert!(!app.retry_account_drain_worker(due - Duration::from_nanos(1)));
        assert_eq!(app.account_drain.worker_retry_at, Some(due));
        assert!(app.retry_account_drain_worker(due));
        assert_eq!(app.account_drain.serial, serial);
        assert!(app.account_drain.pause.is_some());
        assert!(app.account_drain.worker_retry_at.is_some());
        assert!(app.account_work.enter().is_none());

        drop(work);
        let drained = app.take_retired_account_pause().unwrap();
        assert!(app.accept_account_drain(serial, drained).is_none());
        assert!(app.account_drain.worker_retry_at.is_none());
        assert!(!app.retry_account_drain_worker(due + WORKER_RETRY));
        assert!(app.account_work.enter().is_some());
    }
}
