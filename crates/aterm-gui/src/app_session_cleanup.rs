// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit cleanup of unattended, single-shell tabs through the normal tab close.

use aterm_core::terminal::Terminal;

use crate::app_tabs::terminal_projection_index;
use crate::session_store::{CloseAttribution, ExitActor, ExitReason};
use crate::tab_model::{TabId, View};
use crate::{App, Session, WindowId};

/// A cursor at the start alone is ambiguous: Home on a typed command leaves the draft to its
/// right, and the remainder of a multiline input can be below it.
fn empty_shell_prompt(term: &Terminal) -> bool {
    let Some(start) = term.input_region_start() else {
        return false;
    };
    let cursor = term.cursor();
    (cursor.row, cursor.col) == (start.row, start.col)
        && !term.grid().pending_wrap()
        && term.grid().visible_row_view(start.row).written_len() <= start.col
        && (start.row + 1..term.rows()).all(|row| term.grid().visible_row_view(row).is_empty())
}

/// Only the kernel can distinguish a shell at the prompt with a background job from one alone.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn unattended_shell(session: &Session) -> bool {
    if !session.shell_identity.verify()
        || session.ctx.sink.foreground_group() != Some(session.pid)
        || !session.ctx.sink.input_look().read
    {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        // This reader distinguishes an empty list from a zero result with a libproc error.
        let shell = crate::quit_safety::process_name(session.pid);
        shell.is_some_and(|name| crate::quit_safety::is_shell_name(&name))
            && u32::try_from(session.pid)
                .ok()
                .and_then(aterm_sysprobe::jobs::child_pids)
                .is_some_and(|children| children.is_empty())
    }
    #[cfg(target_os = "linux")]
    {
        // Linux lists children of one thread at a time. The leader's empty list only
        // accounts for the whole shell when it has a single thread.
        let stat = std::path::PathBuf::from(format!("/proc/{}/stat", session.pid));
        let Some(stat) = aterm_uds::process::read_proc_text(&stat)
            .ok()
            .and_then(|text| aterm_sysprobe::jobs::linux::parse_stat(&text))
        else {
            return false;
        };
        stat.threads == 1
            && crate::quit_safety::is_shell_name(&stat.name)
            && crate::foreground_handback::linux_children(session.pid)
                .is_some_and(|children| children.is_empty())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unattended_shell(_: &Session) -> bool {
    false
}

impl App {
    /// Close eligible shells across this instance, leaving each window's current tab selected.
    pub(crate) fn clean_up_idle_tabs(&mut self) -> usize {
        self.clean_up_idle_tabs_with(unattended_shell)
    }

    fn clean_up_idle_tabs_with(&mut self, shell_is_idle: impl Fn(&Session) -> bool) -> usize {
        if self.incoming_handoff_pending
            || self.update_handoff_in_flight()
            || self.restore_drain.is_some()
            || self.pending_restore.is_some()
            || self.close_banner.is_some()
            || !self.close_banner_queue.is_empty()
        {
            return 0;
        }
        let _closing = CloseAttribution::enter(ExitReason::UiClose, ExitActor::Human);
        let mut closed = 0;
        let windows: Vec<_> = self.windows.keys().copied().collect();
        for window in windows {
            // Descending slots keep earlier targets in place and give Reopen their original indices.
            let count = self.windows.get(&window).map_or(0, |ws| ws.tab_set.len());
            for index in (0..count).rev() {
                let Some((tab, session)) = self.cleanup_tab(window, index) else {
                    continue;
                };
                let before = session.ctx.sink.input_epoch();
                if !cleanup_prompt(session) {
                    continue;
                }
                let ws = &self.windows[&window];
                let Some(projection) =
                    terminal_projection_index(&ws.tab_set, &self.view_store, tab)
                else {
                    continue;
                };
                let Some(recovery) = ws
                    .tab_set
                    .get(tab)
                    .and_then(|tab| self.tab_restore_descriptor(tab))
                else {
                    continue;
                };
                // Snapshotting takes locks. Recheck before committing and seal the sink only
                // while both its direct and spilled input still match the original observation.
                if !shell_is_idle(session)
                    || !cleanup_prompt(session)
                    || !unclaimed(session)
                    || !session.ctx.sink.try_sever_quiet_input(before)
                {
                    continue;
                }
                self.remove_closed_terminal_tab(window, index, tab, projection, Some(recovery));
                closed += 1;
            }
        }
        closed
    }

    fn cleanup_tab(&self, window: WindowId, index: usize) -> Option<(TabId, &Session)> {
        let ws = self.windows.get(&window)?;
        let tab = ws.tab_set.tab_at(index)?;
        if ws.pending_close
            || ws.tab_set.active_id()? == tab.id
            || ws.preview_on_chip(tab.id).is_some()
            || tab.parked
            || tab.ephemeral
            || tab.root.len() != 1
        {
            return None;
        }
        let id = self
            .view_store
            .get(tab.focus)
            .copied()
            .and_then(View::terminal_session)?;
        if self.pool.views(id) != Some(1) || self.pool.rewind(id).is_some() {
            return None;
        }
        let session = self.pool.get(id)?;
        (!session.frozen_path && session.pty_closed_at.get().is_none() && unclaimed(session))
            .then_some((tab.id, session))
    }
}

/// A manager, a pinned or held session, or a task awaiting its launch or result owns its tab.
fn unclaimed(session: &Session) -> bool {
    let ctx = &session.ctx;
    ctx.fabric.hold().is_none()
        && ctx
            .meta
            .try_lock()
            .is_ok_and(|meta| !meta.pinned && meta.supervisor.is_none())
        && ctx
            .timeline
            .try_lock()
            .is_ok_and(|timeline| timeline.task().is_none())
        && ctx.turn_lease.try_lock().is_ok_and(|lease| {
            lease
                .as_ref()
                .is_none_or(|lease| !lease.is_live(crate::metrics::now_us()))
        })
}

fn cleanup_prompt(session: &Session) -> bool {
    let ctx = &session.ctx;
    if ctx.human_input.delivery_pending() || ctx.sink.ordered_egress_count() != 0 {
        return false;
    }
    let Some(now) = aterm_time::unix_millis() else {
        return false;
    };
    let last_person = ctx
        .human_input
        .typed_ms_since(crate::metrics::now_us())
        .map(|age| now.saturating_sub(age));
    let last_driver = ctx.typed.last().map(|typed| typed.at_unix_ms);
    session.term.try_lock().is_ok_and(|term| {
        let Some(prompt) = term.current_block().and_then(|b| b.prompt_time_ms) else {
            return false;
        };
        // A shell can hide input or have read a key its repaint has yet to echo. Keep any
        // prompt someone typed at, even if its visible line now looks empty.
        prompt <= now
            && last_person.is_none_or(|typed| typed < prompt)
            && last_driver.is_none_or(|typed| typed < prompt)
            && empty_shell_prompt(&term)
    })
}

#[cfg(test)]
#[path = "app_session_cleanup_tests.rs"]
mod tests;
