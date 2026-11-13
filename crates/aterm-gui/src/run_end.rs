// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The run's end: what this process does once its event loop is over, before it exits (the
//! System State worker settled, every tab closed and recorded, the desk written, the PTYs hung
//! up, the control replies settled, Quit & Reopen's helper started, the keeper told BYE), but a
//! Mac Setup tab whose program the quit carries (`App::carry_developer_tools_install_at_exit`):
//! that tab closes nothing and its PTY is not hung up, and the BYE names its master, which the
//! keeper keeps for the next launch to adopt.
//!
//! It runs once ([`App::end_run`]), where the loop's end leaves the process: after `run_app`
//! returns, or, on macOS, inside AppKit's `terminate:` (`ApplicationHandler::exiting`, when
//! `aterm_window::platform::macos::exit_ends_in_appkit` holds), since AppKit ends the process
//! when that returns and `run_app` never does: a quit aterm took up (`NSTerminateLater`), whose
//! YES AppKit hears only after this, and a `terminate:` AppKit carried out by itself. So a
//! logout, restart, shutdown or Quit & Reopen goes on only once this is done, and macOS is not
//! told the quit is going before it is.

use std::time::Duration;

use crate::{App, VideoExportState};

/// How long the run's end waits for a cancelled video export to clean up before it goes on
/// without it. The end runs before macOS hears that a logout, restart or shutdown may go on
/// (inside AppKit's `terminate:`), so a stuck export must not hold it: the update handoff's
/// Commit bounds the same wait (`app_update_handoff::VIDEO_EXPORT_COMMIT_WAIT`).
pub(crate) const VIDEO_EXPORT_QUIT_WAIT: Duration = Duration::from_secs(2);

/// What the run's end needs beyond `App`'s own state, held until it runs once.
pub(crate) struct RunEnd {
    /// The launch's first session, whose published records go with the control socket's even
    /// when its tab closed long before.
    root_sid: aterm_session::SessionId,
    /// A test's count of the times the run's end ran, in place of the shutdown itself.
    #[cfg(test)]
    counted: Option<std::rc::Rc<std::cell::Cell<usize>>>,
}

impl RunEnd {
    /// The run's end of the launch whose first session is `root_sid`.
    pub(crate) const fn new(root_sid: aterm_session::SessionId) -> Self {
        Self {
            root_sid,
            #[cfg(test)]
            counted: None,
        }
    }
}

/// Wait at most `bound` for the cancelled video export to clean up, and say whether it did. Past
/// the bound the run's end goes on without it: the process may exit before the export's thread
/// removes its partial directory, which stays unpublished (no completion artifact), and the log
/// says so.
fn settle_cancelled_video_export(export: &VideoExportState, bound: Duration) -> bool {
    if export.wait_idle(bound) {
        return true;
    }
    crate::logging::stderr_line!(
        "aterm-gui: the cancelled video export did not clean up within {} ms; quitting without \
         it, its partial directory possibly left behind, unpublished",
        bound.as_millis()
    );
    false
}

impl App {
    /// The run's end, once ([`RunEnd`]): a second call, and a call on an App that never ran a
    /// loop, does nothing. The process exits after it (lib.rs, or AppKit).
    pub(crate) fn end_run(&mut self) {
        self.restored_workers.clear();
        let Some(end) = self.run_end.take() else {
            return;
        };
        #[cfg(test)]
        if let Some(counted) = &end.counted {
            counted.set(counted.get() + 1);
            return;
        }
        // The loop has ended (or ends in AppKit's `terminate:` once this returns): nothing beats
        // the watchdog's rest at `about_to_wait`'s park awake, so the teardown is judged at
        // `ShutdownWait` from its first step.
        crate::watchdog::beat(crate::watchdog::Breadcrumb::ShutdownWait);
        #[cfg(unix)]
        crate::fleet_listen::node_serve::window_stops_applying();
        #[cfg(all(unix, not(test)))]
        crate::net_place::stop();
        // The permission helper's panel is aterm's own window, not winit's, so winit's exit close
        // leaves it: it goes with the terminal windows, before the shutdown waits (macOS's Quit &
        // Reopen after a grant ends with it up).
        #[cfg(target_os = "macos")]
        crate::devtools_helper::live::close();
        // A logout, restart or shut down nobody cancelled: this Mac gives up its account claims first,
        // within `QUIET_WITHIN`, while the system still waits (docs/DESIGN-fluid-swarm-2026-10-28.md
        // F19).
        #[cfg(target_os = "macos")]
        crate::harness_host::claims_off_as_the_system_quits();
        self.finish_system_state_shutdown();
        let carried = self.carry_developer_tools_install_at_exit();
        self.shutdown_tab_spawns();
        self.shutdown_spares();
        crate::spawn_prep::release_at_exit(std::time::Duration::from_secs(1));
        if let Some(host) = self.harness.as_ref() {
            host.shutdown_and_join();
        }
        // THE QUIT CLOSES EVERY TAB (but those it carries), THEN EVERY LIVE SESSION'S LOG, HERE
        // (fix unit Q1), where its bye stood: on unix `bye_at_quit` closes the logs, gives the bus
        // those closes within the quit budget, then says `bye quit`; elsewhere they close alone.
        // Either way the journal's last lines below carry the same records, and a closed log
        // takes no record after its close.
        self.close_sessions_at_quit(&carried);
        // The host's workers write what they hold (its Commons runtime closes the network history)
        // after the claims above went out through them, and before the process exits under them.
        crate::host_quit::run();
        self.write_harness_ab_at_exit();
        let _ = self.vitals.stop_at_exit(crate::vitals_host::STOP_BOUND);
        let _ = self.video_abort_app_shutdown();
        let _ = settle_cancelled_video_export(&self.video_export, VIDEO_EXPORT_QUIT_WAIT);
        self.kitty_log.flush_exit();
        let _ = self.close_messages_at_exit();
        // The quit closed every tab above: their requests' records are written before it ends.
        let _ = crate::requests_host::flush_within(crate::requests_host::FLUSH_WAIT);
        self.session_log_journal
            .finish_at_quit(crate::session_log_journal::quit_budget());
        let mut quit_wrote_its_desk = false;
        if self.desk.kept() && self.config.restore_session_or_default() {
            let mut manifest = self
                .quit_capture
                .take()
                .unwrap_or_else(|| self.capture_restore_manifest());
            let keeps = self.desk.quit_keeps_agents(crate::system_quit::asked());
            self.quit_agents(&mut manifest, keeps);
            let run = self
                .crash_journal
                .run()
                .map(crate::crash_journal::JournalId::stem);
            quit_wrote_its_desk = !manifest.is_empty();
            if quit_wrote_its_desk
                && let Err(e) = crate::restore::write_quit(&manifest, run.as_deref())
            {
                crate::logging::stderr_line!(
                    "aterm-gui: could not persist the session layout: {e}"
                );
            }
        }
        #[cfg(unix)]
        self.settle_owed_layout_take_back_at_quit(quit_wrote_its_desk);
        #[cfg(not(unix))]
        let _ = quit_wrote_its_desk;
        crate::restore::settle_take();
        if let Err(error) = self.crash_journal.retire(std::time::Duration::from_secs(2)) {
            crate::logging::stderr_line!("aterm-gui: crash journal not removed at quit: {error}");
        }
        if self.sock_bound.load(std::sync::atomic::Ordering::SeqCst)
            && let Some(plan) = &self.sock_plan
        {
            crate::owned_endpoint::release(plan);
            let dir = crate::control_auth::dir_of_socket(&plan.sock_path);
            let handles = {
                self.store
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .snapshot()
            };
            let sessions =
                std::iter::once(end.root_sid).chain(handles.into_iter().map(|h| h.sid.clone()));
            crate::proxy::retire_own_sessions(&dir, &plan.sock_path, sessions);
            let _ = crate::proxy::flush_publications(std::time::Duration::from_secs(1));
        }
        #[cfg(unix)]
        {
            self.pool.retire_at_quit(&carried);
            let _ = aterm_pty::wait_for_reaps(aterm_pty::REAP_GRACE_WAIT);
        }
        #[cfg(windows)]
        aterm_pty::end_programs_outside_jobs();
        let parked = self.pool.predecessors_sessions();
        crate::task_launch::retire_task_files_at_quit(&self.store, &parked);
        self.forget_retired_workers_at_quit();
        crate::crash_signal::remove_marker_on_clean_exit();
        let input_replies = crate::control::input_replies_of(&self.store);
        if !crate::control::settle_final_replies(
            std::time::Duration::from_millis(500),
            &input_replies,
        ) {
            aterm_log::warn!("exiting with a control reply still undelivered after 500 ms");
        }
        #[cfg(target_os = "macos")]
        if self.desk.kept() {
            crate::quit_reopen::leave_reopen_at_quit();
        }
        #[cfg(unix)]
        aterm_log::info!("{}", crate::recovery_census::QUIT_LINE);
        crate::keeper_link::bye(&carried);
        let _ = crate::logging::flush_within(crate::logging::LOG_FLUSH_EXIT_WAIT);
    }
}

#[cfg(test)]
#[path = "run_end_tests.rs"]
mod tests;
