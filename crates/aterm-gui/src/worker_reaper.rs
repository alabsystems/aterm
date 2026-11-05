// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE REAPER OF WORKERS' TABS: a tab a task opened for itself ([`crate::tab_model::Tab::ephemeral`]:
//! every `spawn agent=` worker, the disk worker and the credit claim worker land so) goes once its
//! task is over, and a person's tab never does. The disk watch's look (`HostNote::DiskLooked`, once a
//! minute whatever any session does) moves each such tab one step of [`ending`] on, parked or not:
//!
//! - A task that concluded ([`settled`]: done, failed or exited, a failure at a usage limit too,
//!   into which nothing types again; not a stall, which may move on) is over once its end has
//!   stood [`LINGER`], or at once where the look already asked its program to end for that task
//!   ([`over`]). A person who typed in its tab since its end took the tab up: it stays
//!   while that end stands, and a turn they type reopens the task `working`, so its next end counts
//!   anew.
//! - A credit claim worker whose task never concludes is over once it ran
//!   [`credit_claim::WORKER_FOR`] since it started or a person last typed in its tab
//!   ([`credit_claim::ran_out`]): its program is ended and its task marked `exited`, which concludes
//!   it, and its tab then goes as any concluded worker's does.
//!
//! A tab over is left while anything holds it: on screen in a focused window, a hold or a driver,
//! a program someone else started running in front, its window's last tab (which asks before it
//! goes), and a seamless update under way. Otherwise its agent is asked to end (`SIGTERM`, then
//! `SIGKILL` once [`DEAF_FOR`] has passed), so its pane closes with no confirmation to raise, and the
//! pane closes. Each close logs one INFO line naming the session and why, and leaves the worker a
//! retired row ([`crate::retired_workers`]), so `result` still answers and its Workers row reads
//! Finished. Keep as Tab (`App::keep_as_tab`) takes the mark off: the tab is a person's from then on.

use std::sync::PoisonError;
use std::time::{Duration, Instant};

use aterm_agent::supervise::Autopilot;

use crate::harness_host::credit_claim;
use crate::retired_workers::{Retired, forget_results};
use crate::task_record::{TaskRecord, TaskState};

/// How long a worker's tab stays after its task concluded, for its end to be read on screen,
/// before it goes: long enough to glance at a worker that just finished, short enough that a
/// screen of finished workers clears itself. What it left stays readable after
/// ([`crate::retired_workers`]).
pub(crate) const LINGER: Duration = Duration::from_secs(3 * 60);

/// How long a worker's program has to end after the first `SIGTERM` the look delivered to it
/// ([`note_term`]): one still running then gets `SIGKILL`, a program that ignores the signal or
/// stopped reading its terminal.
pub(crate) const DEAF_FOR: Duration = Duration::from_secs(2 * 60);

/// Whether `record`'s end is one its tab goes for: a conclusion a person can read (done, failed or
/// exited, [`TaskState::has_an_end_to_read`]). A failure at a usage limit is one: aterm-agent's
/// supervisor ends a managed worker there once its reset is too far off to wait for, and types
/// nothing into it again ([`crate::task_record::WHY_USAGE_LIMIT`]), so its tab would only stand
/// blocked; whoever started it may start it again.
pub(crate) fn settled(record: &TaskRecord) -> bool {
    record.state.has_an_end_to_read()
}

/// Why a look ends a worker's tab ([`over`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Over {
    /// Its task concluded ([`settled`]) and that end has stood [`LINGER`].
    Concluded,
    /// It is a credit claim worker whose open task ran out of its time
    /// ([`credit_claim::ran_out`]).
    RanOut,
}

/// Whether, and why, the worker whose task is `record` is over at `now`, `typed` after a person last
/// typed in its tab, with `ending` where the look already asked its program to end for this task
/// (its record's start): a concluded task once its end stood [`LINGER`], or at once where it is
/// ending, unless a person typed in its tab since that end; an open one where a claim worker's ran
/// out of its time.
pub(crate) fn over(
    record: &TaskRecord,
    now: Instant,
    typed: Option<Duration>,
    ending: bool,
) -> Option<Over> {
    if settled(record) {
        let stood = now.saturating_duration_since(record.since);
        let theirs = typed.is_some_and(|typed| typed < stood);
        return (!theirs && (ending || stood >= LINGER)).then_some(Over::Concluded);
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
    /// Close its pane.
    ClosePane,
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
    /// Closing its pane would close its window.
    pub(crate) ends_window: bool,
    /// A seamless update is under way, which a close would call off.
    pub(crate) updating: bool,
    /// The program in front, its process group, has had [`DEAF_FOR`] to end since the first
    /// `SIGTERM` delivered to it ([`deaf`]).
    pub(crate) deaf: bool,
    /// A program someone else ran in the tab since its task concluded was seen by a look less than
    /// [`LINGER`] ago, so its output is still to be read.
    pub(crate) reading: bool,
}

/// What to do with a worker whose task is over: nothing to a tab in someone's hand or to a job that
/// is not the worker's (its task already `exited`, or another program in front); its program
/// first, which leaves a pane that closes with no confirmation to raise; then an open task marked
/// `exited`, unless something already concluded it; and its pane only where that leaves its window
/// standing, since a window's last tab asks before it goes, and once the output of a program
/// someone ran there since has stood [`LINGER`]. A seamless update under way gets neither a mark,
/// which would wait on its fence, nor a close, which would call it off.
pub(crate) const fn ending(stand: Stand) -> Ending {
    if stand.in_hand || (stand.job && (stand.exited || !stand.worker)) {
        Ending::Leave
    } else if stand.job {
        if stand.deaf {
            Ending::Kill
        } else {
            Ending::Term
        }
    } else if stand.updating {
        Ending::Leave
    } else if !stand.concluded {
        Ending::MarkExited
    } else if stand.ends_window || stand.reading {
        Ending::Leave
    } else {
        Ending::ClosePane
    }
}

/// What a look read of a worker, under its timeline's lock.
pub(crate) struct Look {
    /// Its task record as the look read it.
    pub(crate) record: TaskRecord,
    /// The program in front names itself the agent its record names ([`Stand::worker`]).
    pub(crate) worker: bool,
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
/// (working or stalled: a concluded one is [`settled`], and goes as concluded), so it ends `why=`
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

    /// The disk watch's look ([`HostNote::DiskLooked`], on a timer of its own, whatever any session
    /// does): every live worker on an ephemeral tab whose task is over ([`over`]) is moved on by one
    /// step of [`ending`], so its program ends at one look, an open task is marked `exited` at a
    /// later one, and its pane closes once its end has stood [`LINGER`]; and the retired rows a day
    /// old go. Nothing here raises a confirmation: a pane closes only with no job in it and with its
    /// window left standing. Returns each such worker's session and the step taken.
    ///
    /// [`HostNote::DiskLooked`]: crate::harness_host::HostNote::DiskLooked
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
            // A program someone else runs in the tab of a worker whose task concluded is seen at
            // every look, not only once the tab's end is due, so its output keeps the tab open
            // [`LINGER`] after it ends ([`Stand::reading`]).
            if settled(&look.record) {
                concluded.push(handle.local_id);
                let exited = look.record.state == TaskState::Exited;
                if self.session_foreground_busy(handle.local_id) && (exited || !look.worker) {
                    self.worker_job_seen.insert(handle.local_id, now);
                }
            }
            let ending_now = self
                .worker_termed
                .get(&handle.local_id)
                .is_some_and(|sent| sent.record == look.record.started);
            if let Some(over) = over(&look.record, now, typed, ending_now) {
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

    /// One step of [`ending`] at `now` for the worker `handle`, read as `look`, over for `over`. A
    /// claim worker that ran out has its Autopilot turned Off at every look, since Land would
    /// otherwise keep typing in a tab a person left in front, or bring its program back; a
    /// concluded worker's goes Off only as its program is asked to end, so a manager's next turn
    /// into a worker someone still holds finds Land there.
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
            concluded: over == Over::Concluded,
            exited: look.record.state == TaskState::Exited,
            job: self.session_foreground_busy(session),
            worker: look.worker,
            ends_window: self.close_ends_window(session),
            updating: self.update_handoff_in_flight(),
            deaf: deaf(
                self.worker_termed.get(&session),
                look.record.started,
                crate::quit_safety::foreground_pgrp(handle.master),
                now,
            ),
            reading: over == Over::Concluded
                && self
                    .worker_job_seen
                    .get(&session)
                    .is_some_and(|&seen| now.saturating_duration_since(seen) < LINGER),
        });
        let title = self.tab_label_of_session(session);
        let why = match over {
            Over::Concluded => "its task concluded",
            Over::RanOut => "the credit claim worker ran out of its time",
        };
        match step {
            Ending::Leave => {}
            Ending::Term | Ending::Kill => {
                if over == Over::Concluded {
                    self.reaped_autopilot_off(handle);
                }
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
                        Ok(Some(_)) => "marked exited: its tab closes after its end has stood",
                        Ok(None) => "moved since the look: a later look marks it",
                        Err(crate::task_record::MarkFenced) => "waits for a later look to mark",
                    }
                );
            }
            Ending::ClosePane => self.close_reaped(handle, &look.record, &title),
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

    /// Close worker `handle`'s pane, its task's end `record`, keeping a retired row first, so the
    /// close keeps its result files and `result` answers once its session is gone; a close that
    /// does not happen takes the row back.
    fn close_reaped(
        &mut self,
        handle: &crate::session_store::SessionHandle,
        record: &TaskRecord,
        title: &str,
    ) {
        let (session, sid) = (handle.local_id, handle.sid.as_str());
        let Some(window) = self.window_of_session(session) else {
            return;
        };
        let hosting = self.harness_hosting();
        let row = self.worker_row(handle, &hosting);
        let pushed = self
            .store
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .retired_workers_mut()
            .keep(Retired {
                sid: handle.sid.clone(),
                local: Some(session),
                window: Some(window),
                title: row.title,
                last_words: row.last_words,
                record: record.clone(),
                closed_unix_ms: aterm_time::unix_millis().unwrap_or(0),
            });
        forget_results(&pushed);
        let _closing = crate::session_store::CloseAttribution::enter(
            crate::session_store::ExitReason::Reaper,
            crate::session_store::ExitActor::Harness,
        );
        let end = match record.conclusion() {
            Some(conclusion) => conclusion.word(),
            None => record.state.as_str(),
        };
        let age = crate::app_overview::coarse_age(record.since_ms());
        let why = record
            .why
            .map_or_else(String::new, |why| format!(" why={why}"));
        match self.close_session_by_id(session) {
            Ok(_) => aterm_log::info!(
                "{}: its tab closed: its task {end}{why} {age} ago and nobody held the tab; \
                 `result` answers for it {} more",
                named(sid, title),
                crate::app_overview::coarse_age(crate::retired_workers::RETIRED_FOR_MS),
            ),
            Err(refused) => {
                self.store
                    .write()
                    .unwrap_or_else(PoisonError::into_inner)
                    .retired_workers_mut()
                    .drop_local(session);
                aterm_log::info!(
                    "{}: its task {end}; its tab stays for now: {refused}",
                    named(sid, title)
                );
            }
        }
        self.settle_workers_tab(window);
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
