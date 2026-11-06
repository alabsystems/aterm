// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounds unfinished credit-claim workers without taking away their panes.
//! A concluded task keeps both its pane and any program still running there. A seamless update
//! protects every worker before any signal or task mark. Open claim workers alone retain their
//! execution budget; their processes end and their task records become exited.

use std::sync::PoisonError;
use std::time::{Duration, Instant};

use aterm_agent::supervise::Autopilot;

use crate::harness_host::credit_claim;
use crate::retired_workers::{Retired, forget_results};
use crate::task_record::{TaskRecord, TaskState};

/// The former retirement delay, retained only as a regression-test age.
#[cfg(test)]
pub(crate) const LINGER: Duration = Duration::from_secs(3 * 60);

/// How long a worker's program has to end after the first `SIGTERM` the look delivered to it
/// ([`note_term`]): one still running then gets `SIGKILL`, a program that ignores the signal or
/// stopped reading its terminal.
pub(crate) const DEAF_FOR: Duration = Duration::from_secs(2 * 60);

/// Whether the record has a concluded task, whose program and pane must be preserved.
pub(crate) fn settled(record: &TaskRecord) -> bool {
    record.state.has_an_end_to_read()
}

/// Why a look ends a worker's program ([`over`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Over {
    /// It is a credit claim worker whose open task ran out of its time
    /// ([`credit_claim::ran_out`]).
    RanOut,
}

/// Whether an unfinished credit-claim task has exhausted its execution budget.
pub(crate) fn over(record: &TaskRecord, now: Instant, typed: Option<Duration>) -> Option<Over> {
    if settled(record) {
        return None;
    }
    let stood = credit_claim::stood(record, now, typed);
    credit_claim::ran_out(record.trusted_cwd.as_deref(), stood).then_some(Over::RanOut)
}

/// What a look does with a worker whose task is over ([`ending`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    /// Leave it for a later look.
    Leave,
    /// Ask its program to end.
    Term,
    /// End a program that did not.
    Kill,
    /// Mark its open task `exited`, its program gone, which concludes it.
    MarkExited,
}

/// What a look reads of a worker whose task is over ([`ending`]).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Stand {
    /// Its tab is on screen in a focused window, or a hold or a driver has it.
    pub(crate) in_hand: bool,
    /// Its task concluded ([`settled`]): no exit mark is owed.
    pub(crate) concluded: bool,
    /// Its task is marked `exited`, so a job in its tab now is not the worker's.
    pub(crate) exited: bool,
    /// A foreground job runs in its tab.
    pub(crate) job: bool,
    /// The program in front names itself the agent the worker's record names (its own argv, as
    /// a native, atpkg or `versions/<n>` install of Claude Code does), so a job there is the
    /// worker's own; any other program is someone else's, a runtime such as `node` among them,
    /// whatever its screen shows (an exited worker's last frame can stand on it).
    pub(crate) worker: bool,
    /// A seamless update is under way; no signal or mark may race its capture.
    pub(crate) updating: bool,
    /// The program in front, its process group, has had [`DEAF_FOR`] to end since the first
    /// `SIGTERM` delivered to it ([`deaf`]).
    pub(crate) deaf: bool,
}

/// Preserve updates, concluded tasks and occupied tabs. An unfinished claim worker alone
/// may be signalled after its budget; once its process ends, mark it exited and keep its pane.
pub(crate) const fn ending(stand: Stand) -> Ending {
    if stand.updating
        || stand.concluded
        || stand.in_hand
        || (stand.job && (stand.exited || !stand.worker))
    {
        Ending::Leave
    } else if stand.job {
        if stand.deaf {
            Ending::Kill
        } else {
            Ending::Term
        }
    } else {
        Ending::MarkExited
    }
}

/// What a look read of a worker, under its timeline's lock.
pub(crate) struct Look {
    /// Its task record as the look read it.
    pub(crate) record: TaskRecord,
    /// The program in front names itself the agent its record names ([`Stand::worker`]).
    pub(crate) worker: bool,
    /// The worker is waiting for its own quota reset.
    pub(crate) limited: bool,
}

/// The first `SIGTERM` the look delivered to a worker's program ([`note_term`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Termed {
    /// The start of the task record it was sent for.
    pub(crate) record: Instant,
    /// The process group it reached, the tab's foreground group then.
    pub(crate) group: i32,
    /// When.
    pub(crate) at: Instant,
}

/// Each worker's [`Termed`], by session ([`crate::App::reap_workers`]).
pub(crate) type TermedBySession = std::collections::HashMap<u64, Termed>;

/// Whether the program in front of a worker's tab, foreground process group `group` now, for the
/// task record that started at `started`, has had [`DEAF_FOR`] by `now` to end since the first
/// `SIGTERM` delivered to it (`sent`, [`note_term`]). One never sent one, sent one for an earlier
/// record, or another program that came to the front since, is not deaf, so `SIGKILL` is never a
/// program's first signal.
pub(crate) fn deaf(sent: Option<&Termed>, started: Instant, group: i32, now: Instant) -> bool {
    sent.is_some_and(|sent| {
        sent.record == started
            && group > 0
            && sent.group == group
            && now.saturating_duration_since(sent.at) >= DEAF_FOR
    })
}

/// Records in `termed` that session `session`'s worker, of the task record that started at
/// `started`, was sent `SIGTERM` at `now`, where `said` (the `signal` verb's reply) says which
/// process group it reached (`OK signalled pgrp <n>`): the first one delivered to that group for
/// that record stands, so [`DEAF_FOR`] counts from it. A reply that refused (`ERR …`, such as no
/// foreground process group) records nothing.
pub(crate) fn note_term(
    termed: &mut TermedBySession,
    session: u64,
    started: Instant,
    now: Instant,
    said: &str,
) {
    let Some(group) = said
        .strip_prefix("OK signalled pgrp ")
        .and_then(|rest| {
            rest.split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse::<i32>()
                .ok()
        })
        .filter(|group| *group > 0)
    else {
        return;
    };
    let first = Termed {
        record: started,
        group,
        at: now,
    };
    let stands = termed
        .get(&session)
        .is_some_and(|sent| sent.record == started && sent.group == group);
    if !stands {
        termed.insert(session, first);
    }
}

/// Marks session `local`'s task `exited`, its claim worker's program gone, if its record is still
/// `read`: as the harness's own exit mark does (`aterm_agent::supervise::mark_task_exited`), the
/// mark takes down the attention a deadline raised for the worker (the held or startup ask), which
/// a program that ended no longer owes. It runs once the claim's time is up on a task still open
/// (working or stalled: a concluded one is [`settled`] and left alone), so it ends `why=`
/// its stall's reason where it never got going (signed out, never started, held), and anything
/// else ends `expired`; the claim's one report says which. It waits on no update's fence (`Err`
/// until a later look). `Ok(None)` when the record moved since it was read.
fn mark_ran_out(
    ctx: &crate::SessionCtx,
    local: u64,
    read: &TaskRecord,
) -> Result<Option<crate::task_record::MarkedWith>, crate::task_record::MarkFenced> {
    let mut mark = crate::task_record::TaskMark::new(TaskState::Exited);
    mark.why = Some(match (read.state, read.why) {
        (TaskState::Stalled, Some(why)) => why,
        _ => crate::task_record::WHY_EXPIRED,
    });
    crate::task_record::mark_session_task_with(
        ctx,
        local,
        Some(&mark),
        crate::task_record::AttentionEdit::TakeDown(crate::harness_host::deadline_attention_stands),
        Duration::ZERO,
        |record, _| {
            record.is_some_and(|record| record.rev == read.rev && record.state != TaskState::Exited)
        },
    )
}

/// The words of a log line for the worker `sid`, `title`, that a look moved on.
fn named(sid: &str, title: &str) -> String {
    format!("workers: @{sid} ({title})")
}

impl crate::App {
    /// Whether `session` is a leaf of a tab its task opened for itself
    /// ([`crate::tab_model::Tab::ephemeral`]).
    pub(crate) fn session_is_ephemeral(&self, session: u64) -> bool {
        self.windows.values().any(|ws| {
            ws.tab_set
                .tabs()
                .iter()
                .any(|tab| tab.ephemeral && self.tab_holds_session(tab, session))
        })
    }

    /// Move expired claim workers one step toward an exited task, preserving every pane.
    /// Also age out retained results from earlier closes.
    pub(crate) fn reap_workers(&mut self, now: Instant) -> Vec<(u64, Ending)> {
        // In session order, so the rows a look retires stand in the order the sessions began.
        let mut handles: Vec<_> = {
            let store = self.store.read().unwrap_or_else(PoisonError::into_inner);
            store.live_handles().cloned().collect()
        };
        handles.sort_unstable_by_key(|handle| handle.local_id);
        let now_us = crate::metrics::now_us();
        let mut steps = Vec::new();
        let mut concluded = Vec::new();
        for handle in handles {
            if !self.session_is_ephemeral(handle.local_id) {
                continue;
            }
            // Typing alone, in awake time as `now` is, a seamless update's carried age too: a
            // click or a scroll in the tab is no person taking it over.
            let typed = handle
                .ctx
                .human_input
                .typed_awake_ms(now_us)
                .map(Duration::from_millis);
            let look = {
                let timeline = handle
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                timeline.task().map(|task| Look {
                    limited: matches!(
                        timeline.agent().word,
                        "wall:usage-session" | "wall:usage-weekly" | "wall:model-bucket"
                    ),
                    worker: task.agent.is_some_and(|agent| {
                        timeline
                            .agent()
                            .program
                            .as_deref()
                            .and_then(aterm_phase::program_of)
                            == Some(agent)
                    }),
                    record: task.clone(),
                })
            };
            let Some(look) = look else {
                continue;
            };
            if look.worker && look.limited {
                continue;
            }
            // Keep the last observed activity in the handoff metadata for older successors.
            if settled(&look.record) {
                concluded.push(handle.local_id);
                let exited = look.record.state == TaskState::Exited;
                if self.session_foreground_busy(handle.local_id) && (exited || !look.worker) {
                    self.worker_job_seen.insert(handle.local_id, now);
                }
            }
            if let Some(over) = over(&look.record, now, typed) {
                steps.push((handle.local_id, self.reap_worker(&handle, &look, over, now)));
            }
        }
        // A worker typed back into its time, or gone, owes no signal: its next end starts with
        // `SIGTERM` again.
        self.worker_termed
            .retain(|session, _| steps.iter().any(|(over, _)| over == session));
        self.worker_job_seen
            .retain(|session, _| concluded.contains(session));
        let aged = self
            .store
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .retired_workers_mut()
            .drop_aged(aterm_time::unix_millis().unwrap_or(0));
        if !aged.is_empty() {
            forget_results(&aged);
            self.settle_retired_windows(&aged);
        }
        steps
    }

    /// One step for an expired claim worker. Concluded workers keep their Autopilot and pane.
    fn reap_worker(
        &mut self,
        handle: &crate::session_store::SessionHandle,
        look: &Look,
        over: Over,
        now: Instant,
    ) -> Ending {
        let (session, sid) = (handle.local_id, handle.sid.as_str());
        if over == Over::RanOut {
            self.reaped_autopilot_off(handle);
        }
        let looked_at = self.windows.iter().any(|(wid, ws)| {
            crate::os_window_focused(ws) && self.visible_session_rect(*wid, session).is_some()
        });
        let driven = crate::input_stall::other_hand(&handle.ctx, crate::metrics::now_us());
        let step = ending(Stand {
            in_hand: looked_at || driven.is_some(),
            concluded: settled(&look.record),
            exited: look.record.state == TaskState::Exited,
            job: self.session_foreground_busy(session),
            worker: look.worker,
            updating: self.update_handoff_in_flight(),
            deaf: deaf(
                self.worker_termed.get(&session),
                look.record.started,
                crate::quit_safety::foreground_pgrp(handle.master),
                now,
            ),
        });
        let title = self.tab_label_of_session(session);
        let why = match over {
            Over::RanOut => "the credit claim worker ran out of its time",
        };
        match step {
            Ending::Leave => {}
            Ending::Term | Ending::Kill => {
                let word = if step == Ending::Kill { "kill" } else { "term" };
                let said = crate::control::control_input::signal_verb(
                    handle.master,
                    word,
                    &handle.ctx,
                    aterm_slog::Actor::Harness,
                );
                if step == Ending::Term {
                    note_term(
                        &mut self.worker_termed,
                        session,
                        look.record.started,
                        now,
                        &said,
                    );
                }
                aterm_log::info!(
                    "{}: {why}; `signal {word}` to its program: {}",
                    named(sid, &title),
                    said.trim_end()
                );
            }
            Ending::MarkExited => {
                let marked = mark_ran_out(&handle.ctx, session, &look.record);
                if let Ok(Some(marked)) = &marked
                    && marked.attention_moved
                {
                    crate::presence::post_meta_changed(session);
                }
                aterm_log::info!(
                    "{}: {why}; its program ended, its task {}",
                    named(sid, &title),
                    match marked {
                        Ok(Some(_)) => "marked exited: its pane stays",
                        Ok(None) => "moved since the look: a later look marks it",
                        Err(crate::task_record::MarkFenced) => "waits for a later look to mark",
                    }
                );
            }
        }
        step
    }

    /// A reaped worker's Autopilot Off, in aterm's own name, which fails no task.
    fn reaped_autopilot_off(&mut self, handle: &crate::session_store::SessionHandle) {
        let autopilot = handle
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .autopilot_word();
        if autopilot != Some(Autopilot::Off) {
            self.worker_autopilot_off(handle.sid.as_str());
        }
    }

    /// Settle the Workers tab of each window whose page listed one of the retired rows `dropped`.
    pub(crate) fn settle_retired_windows(&mut self, dropped: &[Retired]) {
        let mut windows: Vec<crate::WindowId> =
            dropped.iter().filter_map(|row| row.window).collect();
        windows.sort_unstable();
        windows.dedup();
        for wid in windows {
            if self.windows.contains_key(&wid) {
                self.settle_workers_tab(wid);
            }
        }
    }
}

#[cfg(test)]
#[path = "worker_reaper_tests.rs"]
pub(crate) mod tests;
