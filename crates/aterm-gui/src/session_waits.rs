// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-side meters and actions for paste and scrollback-rewrap waits.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Instant;

use aterm_messages::waits::{self, SessionWait, WaitEnd, WaitSample, WaitStep};
use aterm_messages::{Message, MessageId, Severity, tags};
use aterm_session::sink::{BulkMeter, BulkState, SinkWriter};

use crate::App;

/// A session's rewrap progress, written by the reflow worker and read by the loop's sampler.
#[derive(Debug, Default)]
pub(crate) struct RewrapGauge {
    base: AtomicU64,
    current: AtomicU64,
    total: AtomicU64,
    running: AtomicUsize,
    begun: AtomicU64,
    failed: AtomicBool,
    cancelled: AtomicBool,
}

/// One rewrap job's hold on its session's gauge.
#[derive(Debug)]
pub(crate) struct RewrapJob {
    gauge: Arc<RewrapGauge>,
    end: WaitEnd,
}

const PASTE_STOPPED: &str = "Paste stopped";

impl RewrapJob {
    /// The gauge the job's passes report into.
    pub(crate) fn gauge(&self) -> &RewrapGauge {
        &self.gauge
    }

    /// The job is over, this way.
    pub(crate) fn finish(mut self, end: WaitEnd) {
        self.end = end;
    }
}

impl Drop for RewrapJob {
    fn drop(&mut self) {
        self.gauge.end(self.end);
    }
}

/// One reading of a [`RewrapGauge`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RewrapReading {
    /// Lines rewrapped.
    pub(crate) done: u64,
    /// Lines detached.
    pub(crate) total: u64,
    /// A rewrap is in flight.
    pub(crate) running: bool,
    /// Jobs begun over the session's life.
    pub(crate) jobs: u64,
    /// How the last one ended, once it has.
    pub(crate) end: Option<WaitEnd>,
}

impl RewrapGauge {
    /// A job of `lines` detached lines was handed to a worker.
    pub(crate) fn begin(&self, lines: usize) {
        if self.running.fetch_add(1, Ordering::AcqRel) == 0 {
            self.base.store(0, Ordering::Relaxed);
            self.current.store(0, Ordering::Relaxed);
            self.total.store(0, Ordering::Relaxed);
            self.failed.store(false, Ordering::Relaxed);
            self.cancelled.store(false, Ordering::Relaxed);
        }
        self.total.fetch_add(lines as u64, Ordering::Relaxed);
        self.begun.fetch_add(1, Ordering::AcqRel);
    }

    /// [`Self::begin`], held by a [`RewrapJob`] that ends it on every exit.
    pub(crate) fn begin_job(self: &Arc<Self>, lines: usize) -> RewrapJob {
        self.begin(lines);
        RewrapJob {
            gauge: self.clone(),
            end: WaitEnd::Failed,
        }
    }

    /// The running pass has read `done` of its lines (after each step).
    pub(crate) fn step(&self, done: usize) {
        self.current.store(done as u64, Ordering::Relaxed);
    }

    /// Record a completed pass of `lines` and optional convergence pass `next`.
    pub(crate) fn pass_done(&self, lines: usize, next: Option<usize>) {
        self.base.fetch_add(lines as u64, Ordering::Relaxed);
        self.current.store(0, Ordering::Relaxed);
        if let Some(next) = next {
            self.total.fetch_add(next as u64, Ordering::Relaxed);
        }
    }

    /// The job is over: re-attached, cancelled with its session, or lost.
    pub(crate) fn end(&self, end: WaitEnd) {
        match end {
            WaitEnd::Failed => self.failed.store(true, Ordering::Relaxed),
            WaitEnd::Gone | WaitEnd::Stopped => self.cancelled.store(true, Ordering::Relaxed),
            WaitEnd::Done => {}
        }
        let _ = self
            .running
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1));
    }

    /// Now.
    pub(crate) fn reading(&self) -> RewrapReading {
        let jobs = self.begun.load(Ordering::Acquire);
        let running = self.running.load(Ordering::Acquire) > 0;
        let total = self.total.load(Ordering::Relaxed);
        let done = self
            .base
            .load(Ordering::Relaxed)
            .saturating_add(self.current.load(Ordering::Relaxed))
            .min(total);
        let end = (!running).then(|| {
            if self.failed.load(Ordering::Relaxed) {
                WaitEnd::Failed
            } else if self.cancelled.load(Ordering::Relaxed) {
                WaitEnd::Gone
            } else {
                WaitEnd::Done
            }
        });
        RewrapReading {
            done,
            total,
            running,
            jobs,
            end,
        }
    }
}

struct PasteWatch {
    sink: Weak<SinkWriter>,
    meters: Vec<(Arc<BulkMeter>, u64)>,
    completed_sent: u64,
    completed_total: u64,
    completed_failed: bool,
    completed_short: bool,
    started: Instant,
    wait: SessionWait,
    row: Option<MessageId>,
}

impl PasteWatch {
    fn compact(&mut self) {
        self.meters.retain(|(meter, bytes)| {
            let p = meter.progress();
            if !matches!(
                p.state,
                BulkState::Delivered | BulkState::Stopped | BulkState::Failed
            ) {
                return true;
            }
            let size = if p.total > 0 { p.total } else { *bytes };
            let sent = if p.state == BulkState::Delivered {
                size
            } else {
                p.sent.min(size)
            };
            self.completed_sent = self.completed_sent.saturating_add(sent);
            self.completed_total = self.completed_total.saturating_add(size);
            self.completed_failed |= p.state == BulkState::Failed;
            self.completed_short |= p.state == BulkState::Stopped && sent < size;
            false
        });
    }

    fn amounts(&self) -> (u64, u64) {
        self.meters.iter().fold(
            (self.completed_sent, self.completed_total),
            |(done, total), (meter, bytes)| {
                let p = meter.progress();
                let size = if p.total > 0 { p.total } else { *bytes };
                let sent = match p.state {
                    BulkState::Delivered => size,
                    _ => p.sent.min(size),
                };
                (done.saturating_add(sent), total.saturating_add(size))
            },
        )
    }
}

struct RewrapWatch {
    gauge: Arc<RewrapGauge>,
    asked_at: Instant,
    wait: SessionWait,
    row: Option<MessageId>,
}

/// Every watched wait, and when the next sample falls due.
#[derive(Default)]
pub(crate) struct SessionWaits {
    pastes: HashMap<u64, PasteWatch>,
    rewraps: HashMap<u64, RewrapWatch>,
    next_sample: Option<Instant>,
    series: u64,
}

impl App {
    /// Watch a large paste of `bytes` queued for `session` with `meter`.
    pub(crate) fn watch_paste(
        &mut self,
        session: u64,
        sink: &Arc<SinkWriter>,
        meter: Arc<BulkMeter>,
        bytes: u64,
        now: Instant,
    ) {
        let waits = &mut self.session_waits;
        if let Some(watch) = waits.pastes.get_mut(&session) {
            watch.compact();
            watch.meters.push((meter, bytes));
        } else {
            waits.series = waits.series.wrapping_add(1);
            let series = aterm_messages::Amount::series_of(&format!(
                "{}#{}",
                waits::wait_key(waits::WaitKind::Paste, session),
                waits.series
            ));
            waits.pastes.insert(
                session,
                PasteWatch {
                    sink: Arc::downgrade(sink),
                    meters: vec![(meter, bytes)],
                    completed_sent: 0,
                    completed_total: 0,
                    completed_failed: false,
                    completed_short: false,
                    started: now,
                    wait: SessionWait::paste(session, series),
                    row: None,
                },
            );
        }
        self.schedule_session_waits(now);
    }

    /// Stop every watched paste into `session` and drop unsent bytes.
    pub(crate) fn stop_paste(&mut self, session: u64) -> bool {
        let Some(mut watch) = self.session_waits.pastes.remove(&session) else {
            return false;
        };
        let (sent, total) = watch.amounts();
        for (meter, _) in &watch.meters {
            meter.request_stop();
        }
        let _ = watch.wait.step(&WaitSample {
            done: sent,
            total,
            elapsed: Instant::now().saturating_duration_since(watch.started),
            asked: true,
            end: Some(WaitEnd::Stopped),
        });
        self.record_paste_stopped(watch.row.take(), sent, total);
        true
    }

    fn record_paste_stopped(&mut self, row: Option<MessageId>, sent: u64, total: u64) {
        let detail = [format!(
            "{} of {} sent; the rest was not",
            waits::size_words(sent),
            waits::size_words(total)
        )];
        match row {
            Some(id)
                if self
                    .messages
                    .withdraw_as(id, PASTE_STOPPED, &detail, Instant::now()) =>
            {
                self.sync_messages();
            }
            _ => {
                self.record_message(
                    Message::new(tags::SESSION, Severity::Info, PASTE_STOPPED)
                        .lines(detail)
                        .record(),
                );
            }
        }
    }

    /// The person scrolled back into `session`'s history or searched it.
    pub(crate) fn note_history_wanted(&mut self, session: u64) {
        let Some(gauge) = self.pool.get(session).map(|s| s.ctx.rewrap_gauge.clone()) else {
            return;
        };
        if !gauge.reading().running || self.session_waits.rewraps.contains_key(&session) {
            return;
        }
        let now = Instant::now();
        self.session_waits.rewraps.insert(
            session,
            RewrapWatch {
                gauge,
                asked_at: now,
                wait: SessionWait::rewrap(session),
                row: None,
            },
        );
        self.sample_session_waits(now);
    }

    /// Whether `session`'s scrollback is detached for a rewrap now (searches then report a floor).
    pub(crate) fn rewrap_in_flight(&self, session: u64) -> bool {
        self.pool
            .get(session)
            .is_some_and(|s| s.ctx.rewrap_gauge.reading().running)
    }

    /// Whether `session` is on screen: in the active tab of a window that is on screen.
    pub(crate) fn session_on_screen(&self, session: u64) -> bool {
        self.windows.values().any(|ws| {
            crate::messages_host::window_on_screen(ws)
                && ws.tab_set.active().is_some_and(|tab| {
                    tab.root.any_leaf(&mut |view| {
                        self.view_store
                            .get(*view)
                            .copied()
                            .and_then(crate::tab_model::View::terminal_session)
                            == Some(session)
                    })
                })
        })
    }

    /// The loop's tick: sample the watched waits when a sample is due.
    pub(crate) fn session_waits_tick(&mut self, now: Instant) {
        if self.session_waits.next_sample.is_some_and(|t| now >= t) {
            self.sample_session_waits(now);
        }
    }

    /// The next sample, folded under `DeadlineOwner::SessionWaits`.
    pub(crate) const fn session_waits_deadline(&self) -> Option<Instant> {
        self.session_waits.next_sample
    }

    fn schedule_session_waits(&mut self, now: Instant) {
        let waits = &self.session_waits;
        let every = waits
            .pastes
            .values()
            .map(|w| w.wait.sample_every())
            .chain(waits.rewraps.values().map(|w| w.wait.sample_every()))
            .min();
        self.session_waits.next_sample = every.map(|every| now + every);
    }

    /// Sample every watched wait and perform its step.
    pub(crate) fn sample_session_waits(&mut self, now: Instant) {
        let sessions: Vec<u64> = self.session_waits.pastes.keys().copied().collect();
        for session in sessions {
            let alive = self.pool.get(session).is_some();
            let on_screen = self.session_on_screen(session);
            let Some(mut watch) = self.session_waits.pastes.remove(&session) else {
                continue;
            };
            watch.compact();
            let (done, total) = watch.amounts();
            let sink = watch.sink.upgrade();
            let settled = watch.meters.is_empty();
            let failed = watch.completed_failed;
            let end = match sink {
                None => Some(WaitEnd::Gone),
                _ if !alive || failed => Some(WaitEnd::Gone),
                Some(sink) if settled && sink.ordered_egress_count() == 0 => {
                    Some(if watch.completed_short {
                        WaitEnd::Stopped
                    } else {
                        WaitEnd::Done
                    })
                }
                Some(_) => None,
            };
            let step = watch.wait.step(&WaitSample {
                done,
                total,
                elapsed: now.saturating_duration_since(watch.started),
                asked: on_screen,
                end,
            });
            if end == Some(WaitEnd::Stopped) {
                self.record_paste_stopped(watch.row.take(), done, total);
            } else {
                self.perform_wait_step(&mut watch.row, step);
            }
            if end.is_none() {
                self.session_waits.pastes.insert(session, watch);
            }
        }
        let sessions: Vec<u64> = self.session_waits.rewraps.keys().copied().collect();
        for session in sessions {
            let alive = self.pool.get(session).is_some();
            let Some(mut watch) = self.session_waits.rewraps.remove(&session) else {
                continue;
            };
            let reading = watch.gauge.reading();
            let end = if alive {
                reading.end
            } else {
                Some(WaitEnd::Gone)
            };
            let step = watch.wait.step(&WaitSample {
                done: reading.done,
                total: reading.total,
                elapsed: now.saturating_duration_since(watch.asked_at),
                asked: true,
                end,
            });
            self.perform_wait_step(&mut watch.row, step);
            match end {
                None => {
                    self.session_waits.rewraps.insert(session, watch);
                }
                Some(_) => self.rerun_searches_of(session),
            }
        }
        self.schedule_session_waits(now);
    }

    fn perform_wait_step(&mut self, row: &mut Option<MessageId>, step: WaitStep) {
        match step {
            WaitStep::Idle => {}
            WaitStep::Post(msg) => *row = Some(self.post_message(msg)),
            WaitStep::Restate(msg) => {
                if let Some(id) = *row {
                    self.restate_message(id, crate::messages_host::restatement_of(&msg));
                }
            }
            WaitStep::End(echo) => {
                if let Some(id) = row.take()
                    && self.messages.withdraw_with(id, echo, Instant::now())
                {
                    self.sync_messages();
                }
            }
        }
    }

    fn rerun_searches_of(&mut self, session: u64) {
        let windows: Vec<crate::WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| ws.search.is_some())
            .map(|(wid, _)| *wid)
            .collect();
        for wid in windows {
            if self.front_terminal(wid).map(|t| t.session) == Some(session) {
                self.search_recompute_in(wid);
            }
        }
    }
}

#[cfg(all(test, unix))]
#[path = "session_waits_tests.rs"]
mod tests;
