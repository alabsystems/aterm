// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! App-owned account work is drained before process replacement or logical exit.

use aterm_agent::harness::account_work::{Drained, Pause};
use aterm_window::event_loop::ActiveEventLoop;

use crate::{App, CloseOutcome, Wake, WindowId};

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
        activity: u64,
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
        if self.account_drain.drained.is_some() {
            return true;
        }
        if self.account_drain.pending.is_some() {
            let ready = self.take_retired_account_pause();
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
                self.account_drain.pending = None;
                self.account_drain.drained = Some(drained);
                self.account_drain.failed = false;
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
                return true;
            }
            Err(pause) => pause,
        };
        self.account_drain.pending = Some(continuation);
        self.account_drain.pause = Some(pause);
        self.dispatch_account_drain(serial);

        false
    }

    fn dispatch_account_drain(&mut self, serial: u64) {
        let Some(proxy) = self.proxy.clone() else {
            self.account_drain.failed = true;
            return;
        };
        // Spawn before transferring the unique Pause. A refused or dropped unstarted worker
        // cannot destroy the App's only proof-producing handle.
        let (send, receive) = std::sync::mpsc::sync_channel::<Pause>(1);
        match std::thread::Builder::new()
            .name("aterm-account-drain".into())
            .spawn(move || {
                if let Ok(pause) = receive.recv() {
                    let drained = pause.wait();
                    let _ = proxy.send_event(Wake::AccountsDrained { serial, drained });
                }
            }) {
            Ok(_) => {
                if let Some(pause) = self.account_drain.pause.take()
                    && let Err(error) = send.send(pause)
                {
                    self.account_drain.pause = Some(error.0);
                    self.account_drain.failed = true;
                }
            }
            Err(error) => {
                if !self.account_drain.failed {
                    aterm_log::info!("account work drain waits on the event loop: {error}");
                }
                self.account_drain.failed = true;
            }
        }
    }

    /// A worker-creation failure keeps the original Pause; the event loop can still prove
    /// settlement without spawning, blocking, or treating a timeout as completion.
    pub(crate) fn poll_account_drain(&mut self, el: &ActiveEventLoop) {
        if let Some(drained) = self.take_retired_account_pause() {
            self.finish_account_drain(el, self.account_drain.serial, drained);
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
        drained: Drained,
    ) {
        let Some(continuation) = self.accept_account_drain(serial, drained) else {
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
            Continuation::Handoff {
                mode,
                attempt,
                same_image,
                #[cfg(unix)]
                activity,
                #[cfg(unix)]
                next_attempt,
            } => {
                #[cfg(unix)]
                if self.update_handoff_activity_epoch != activity
                    || self.next_update_handoff_id != next_attempt
                {
                    self.retire_account_drain_handoff(
                        Continuation::Handoff {
                            mode,
                            attempt,
                            same_image,
                            activity,
                            next_attempt,
                        },
                        "the handoff changed while account work drained",
                    );
                    self.resume_drained_accounts();
                    return;
                }
                if attempt.as_ref().is_some_and(|attempt| {
                    !self.native_updater_service.apply_ticket_is_current(attempt)
                }) {
                    self.resume_drained_accounts();
                    return;
                }
                let retained_attempt = attempt.clone();
                let result = self
                    .revalidate_native_update_safety()
                    .map_err(|reasons| reasons.join("; "))
                    .and_then(|safety| {
                        self.apply_staged_update_now(safety, mode, attempt, same_image)
                            .map_err(|error| error.to_string())
                    });
                if let Err(error) = result {
                    if let Some(attempt) = retained_attempt
                        && self.native_updater_service.abort_apply(&attempt, &error)
                    {
                        self.publish_native_update_state();
                    }
                    self.resume_drained_accounts();
                    aterm_log::info!("update waits after account work drained: {error}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
