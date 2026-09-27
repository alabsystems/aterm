// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST SIDE OF A SESSION'S WAITS (design rulings 231–234): a large paste
//! on its way to a program, and the scrollback rewrap after a width change.
//! The policy, the words and the rows are the engine's
//! ([`aterm_messages::waits`]); this module only MEASURES and PERFORMS:
//!
//! * **Measures.** A paste's bytes come from the writer side
//!   ([`BulkMeter`], one relaxed store per `write(2)` slice); a rewrap's lines
//!   from the worker ([`RewrapGauge`], one store per 50 000-line step). The
//!   loop reads them when a sample falls due — [`aterm_messages::waits::WAIT_SAMPLE`]
//!   while a row is up, [`aterm_messages::waits::WAIT_SAMPLE_IDLE`] otherwise —
//!   and only while something is watched; with nothing watched it arms no
//!   deadline at all (`DeadlineOwner::SessionWaits`, slot 41).
//! * **Says who waits.** A paste's person waits while its session is on
//!   screen; a rewrap's once they scroll back into the missing history or
//!   search it (`App::note_history_wanted`).
//! * **Performs** what [`SessionWait::step`] returns: post, restate, end with
//!   the echo. `Stop paste` stops the meters (`App::stop_paste`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Instant;

use aterm_messages::waits::{self, SessionWait, WaitEnd, WaitSample, WaitStep};
use aterm_messages::{Hold, Message, MessageId, Severity, tags};
use aterm_session::sink::{BulkMeter, BulkState, SinkWriter};

use crate::App;

/// A session's rewrap progress, written by the reflow worker and read by the
/// loop's sampler: plain atomics, neither side waits. One job at a time per
/// session (the grid detaches nothing while a rewrap is in flight), plus the
/// convergence passes that job may run (RFL-3), whose lines are added to the
/// total so the fill never runs backwards.
#[derive(Debug, Default)]
pub(crate) struct RewrapGauge {
    /// Lines of the passes already finished.
    base: AtomicU64,
    /// Lines the running pass has read.
    current: AtomicU64,
    /// Lines detached, over every pass of this job.
    total: AtomicU64,
    /// Jobs in flight (0 or 1).
    running: AtomicUsize,
    /// The job could not re-attach its history (a panicking rewrap).
    failed: AtomicBool,
    /// The job was cancelled with its session.
    cancelled: AtomicBool,
}

/// ONE REWRAP JOB'S HOLD ON ITS SESSION'S GAUGE (design ruling 236): minted
/// by [`RewrapGauge::begin_job`] where the job is detached, carried with the
/// job to whoever runs it, and ended exactly once. [`Self::finish`] says how
/// the job ended; on EVERY other exit — an unwind outside the rewrap's own
/// `catch_unwind` (the re-attach, the gauge's pass accounting), or a job that
/// no thread ever took — its `Drop` ends the gauge `Failed`. A job left
/// running would leave `rewrap_in_flight` true for the rest of the session:
/// every search there a floor, and a rewrap row sampled at 4 Hz forever.
#[derive(Debug)]
pub(crate) struct RewrapJob {
    gauge: Arc<RewrapGauge>,
    end: WaitEnd,
}

/// A stopped paste's record (design ruling 231): its words, and ONE entry
/// (ruling 265).
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RewrapReading {
    /// Lines rewrapped.
    pub(crate) done: u64,
    /// Lines detached.
    pub(crate) total: u64,
    /// A rewrap is in flight.
    pub(crate) running: bool,
    /// How the last one ended, once it has.
    pub(crate) end: Option<WaitEnd>,
}

impl RewrapGauge {
    /// A job of `lines` detached lines was handed to a worker (the loop
    /// thread, at the hand-off).
    pub(crate) fn begin(&self, lines: usize) {
        if self.running.fetch_add(1, Ordering::AcqRel) == 0 {
            self.base.store(0, Ordering::Relaxed);
            self.current.store(0, Ordering::Relaxed);
            self.total.store(0, Ordering::Relaxed);
            self.failed.store(false, Ordering::Relaxed);
            self.cancelled.store(false, Ordering::Relaxed);
        }
        self.total.fetch_add(lines as u64, Ordering::Relaxed);
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

    /// A pass of `lines` finished; `next` is the convergence pass the
    /// re-attach handed back, if any.
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

    /// Now. Not named `read`: the lock-order census identifies a lock by that
    /// name, and this is a snapshot of atomics, not a guard.
    pub(crate) fn reading(&self) -> RewrapReading {
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
            end,
        }
    }
}

/// One session's watched paste: every large paste the person made into it
/// until the queue drains (a second paste behind the first joins it).
struct PasteWatch {
    sink: Weak<SinkWriter>,
    /// Each paste's meter and its text's size (its frame's size is known
    /// only once its write begins).
    meters: Vec<(Arc<BulkMeter>, u64)>,
    completed_sent: u64,
    completed_total: u64,
    completed_failed: bool,
    started: Instant,
    wait: SessionWait,
    row: Option<MessageId>,
}

impl PasteWatch {
    /// A long burst may finish many pastes while later work keeps its watch
    /// alive. Preserve their totals and failure verdict, releasing the meters
    /// so storage follows outstanding jobs rather than the whole burst.
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
            false
        });
    }

    /// Bytes delivered and bytes in all, over the burst.
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

/// One session's watched rewrap: created when the person first waits on it.
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
    /// A large paste of `bytes` was queued for `session` with `meter` (the
    /// paste arm, after a successful enqueue): watch it, joining a paste
    /// already watched there.
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
                    started: now,
                    wait: SessionWait::paste(session, series),
                    row: None,
                },
            );
        }
        self.schedule_session_waits(now);
    }

    /// `Stop paste` (design ruling 231): every watched paste into `session`
    /// drops what it has not yet sent — one still queued sends nothing, the
    /// one being written is cut into a well-formed frame
    /// (`SinkWriter::write_frame_metered_with_receipt`) — and input queued
    /// after them goes through in order. The row fades at once; the record
    /// says how much went. `false` when nothing was pasting there.
    pub(crate) fn stop_paste(&mut self, session: u64) -> bool {
        let Some(mut watch) = self.session_waits.pastes.remove(&session) else {
            return false;
        };
        let (sent, total) = watch.amounts();
        for (meter, _) in &watch.meters {
            meter.request_stop();
        }
        // The wait is over whatever its row: the step only disarms it.
        let _ = watch.wait.step(&WaitSample {
            done: sent,
            total,
            elapsed: Instant::now().saturating_duration_since(watch.started),
            asked: true,
            end: Some(WaitEnd::Stopped),
        });
        let detail = [format!(
            "{} of {} sent; the rest was not",
            waits::size_words(sent),
            waits::size_words(total)
        )];
        // ONE ENTRY PER PASTE (design ruling 265): the row that stood on the
        // band BECOMES the record `Paste stopped` (it fades, as a stop claims
        // nothing) — never a withdrawn `Pasting 4.2 MB` that read as still
        // running beside a second record saying it stopped. A paste stopped
        // before its row was posted is written down alone.
        match watch.row.take() {
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
                        .hold(Hold::LogOnly),
                );
            }
        }
        true
    }

    /// The person scrolled back into `session`'s history or searched it: if
    /// its scrollback is being rewrapped they are waiting on that (design
    /// ruling 233), and its row is wanted from now.
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

    /// Whether `session`'s scrollback is detached for a rewrap now (the
    /// search's honesty, ruling 234).
    pub(crate) fn rewrap_in_flight(&self, session: u64) -> bool {
        self.pool
            .get(session)
            .is_some_and(|s| s.ctx.rewrap_gauge.reading().running)
    }

    /// Whether `session` is on screen: in the active tab of a window whose
    /// band is on screen.
    pub(crate) fn session_on_screen(&self, session: u64) -> bool {
        self.windows.values().any(|ws| {
            crate::messages_host::band_on_screen(ws)
                && ws.tab_set.active().is_some_and(|tab| {
                    tab.root.leaves().into_iter().any(|view| {
                        self.view_store
                            .get(view)
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

    /// The next sample, folded under `DeadlineOwner::SessionWaits`; `None`
    /// while nothing is watched.
    pub(crate) fn session_waits_deadline(&self) -> Option<Instant> {
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
                Some(sink) if settled && sink.ordered_egress_count() == 0 => Some(WaitEnd::Done),
                Some(_) => None,
            };
            let step = watch.wait.step(&WaitSample {
                done,
                total,
                elapsed: now.saturating_duration_since(watch.started),
                asked: on_screen,
                end,
            });
            self.perform_wait_step(&mut watch.row, step);
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
                // The history is back: a search that ran without it runs again.
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

    /// Re-run the open searches over `session` (its history came back).
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
mod tests {
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use aterm_messages::waits::{REWRAPPED, REWRAPPING, WAIT_SAMPLE, WAIT_SAMPLE_IDLE};
    use aterm_messages::{EchoKind, Intent, PROGRESS_GRACE};
    use aterm_session::sink::SinkWriter;

    use crate::app_search::FindAction;
    use crate::input::{InputEvent, PasteFraming, ScrollIntent, Source};
    use crate::message_reporters::{Attention, attention};
    use crate::{App, WindowId};

    const OPEN: &[u8] = b"\x1b[200~";
    const CLOSE: &[u8] = b"\x1b[201~";

    /// A headless App whose one session writes into a socket the test
    /// reads, with its window's band on screen.
    fn app_with_reader() -> (App, Arc<SinkWriter>, UnixStream) {
        let (reader, writer) = UnixStream::pair().expect("socketpair");
        reader
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        let sink = Arc::new(SinkWriter::new_owned(writer.into()));
        let mut app = App::headless_for_test_with_sink(sink.clone());
        app.windows
            .get_mut(&WindowId(0))
            .expect("window 0")
            .band_on_screen_for_test = true;
        (app, sink, reader)
    }

    fn paste(app: &mut App, bytes: usize) {
        let text = "p".repeat(bytes);
        app.input(
            WindowId(0),
            InputEvent::Paste(text, PasteFraming::Gesture { bracketed: true }),
            Source::Human,
        );
    }

    fn drained(sink: &SinkWriter) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while sink.ordered_egress_count() != 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(sink.ordered_egress_count(), 0, "the ordered queue drains");
    }

    /// A SMALL PASTE NEVER SHOWS A ROW (ruling 231): under 64 KiB it is not
    /// even watched, so it arms no sample and posts nothing.
    #[test]
    fn a_small_paste_never_shows_a_row() {
        let (mut app, sink, mut reader) = app_with_reader();
        let small = usize::try_from(aterm_messages::waits::LARGE_PASTE_BYTES).unwrap() - 1;
        paste(&mut app, small);
        assert!(app.session_waits.pastes.is_empty(), "not watched");
        assert_eq!(app.session_waits_deadline(), None, "nothing armed");
        let mut got = vec![0u8; small + OPEN.len() + CLOSE.len()];
        reader.read_exact(&mut got).expect("delivered");
        drained(&sink);
        app.sample_session_waits(Instant::now() + PROGRESS_GRACE * 5);
        assert_eq!(app.messages.live_rows().count(), 0);
    }

    /// A REWRAP JOB ENDS ON EVERY EXIT (ruling 236): finished, it ends as it
    /// says; unwound past the rewrap's own guard (a panicking re-attach), or
    /// never taken by any thread, its hold ends the gauge `Failed` — never a
    /// gauge left running, which would make every later search a floor.
    #[test]
    fn a_rewrap_job_ends_on_every_exit() {
        use aterm_messages::WaitEnd;

        use super::RewrapGauge;

        let gauge = Arc::new(RewrapGauge::default());
        let job = gauge.begin_job(1_000);
        assert!(gauge.reading().running);
        job.gauge().pass_done(1_000, None);
        job.finish(WaitEnd::Done);
        let done = gauge.reading();
        assert!(!done.running);
        assert_eq!(
            (done.done, done.total, done.end),
            (1_000, 1_000, Some(WaitEnd::Done))
        );

        // A panic after the rewrap, where the re-attach runs.
        let job = gauge.begin_job(500);
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            job.gauge().step(250);
            panic!("finish_resize_offload");
        }));
        assert!(unwound.is_err());
        let lost = gauge.reading();
        assert!(!lost.running, "the unwind ended the hold");
        assert_eq!(lost.end, Some(WaitEnd::Failed));

        // A job parked in its hand-off slot that no thread ever took.
        let slot = std::sync::Mutex::new(Some(gauge.begin_job(500)));
        assert!(gauge.reading().running);
        drop(slot);
        assert_eq!(gauge.reading().end, Some(WaitEnd::Failed));
    }

    /// A LARGE PASTE THE SANITIZER EMPTIES ENDS AT ONCE (ruling 236): 64 KiB
    /// of controls, unbracketed, comes to no byte on the wire. Its meter is
    /// settled by the seam, so the watch ends at the next sample with no row,
    /// no echo and no deadline left armed.
    #[test]
    fn a_large_paste_the_sanitizer_empties_ends_with_no_row() {
        let (mut app, sink, _reader) = app_with_reader();
        let big = usize::try_from(aterm_messages::waits::LARGE_PASTE_BYTES).unwrap();
        app.input(
            WindowId(0),
            InputEvent::Paste(
                "\u{1}".repeat(big),
                PasteFraming::Gesture { bracketed: false },
            ),
            Source::Human,
        );
        assert_eq!(app.session_waits.pastes.len(), 1, "watched");
        drained(&sink);
        app.sample_session_waits(Instant::now() + PROGRESS_GRACE * 5);
        assert!(app.session_waits.pastes.is_empty(), "the watch ended");
        assert_eq!(app.session_waits_deadline(), None, "nothing armed");
        assert_eq!(app.messages.live_rows().count(), 0, "no row");
        assert!(app.messages.echoes().is_empty(), "no echo");
    }

    /// THE PASTE'S ROW AND ITS STOP (rulings 231, 232): a large paste into a
    /// program that is not reading is watched; its row is progress with the
    /// one `Stop paste` capsule, posted inside the grace from the paste's
    /// start and filled by the bytes the program has taken. `Stop paste`
    /// fades the row, drops the unsent middle, closes the bracketed paste,
    /// and the key typed after the paste — queued behind it, never ahead —
    /// arrives next.
    #[test]
    fn a_large_paste_takes_its_row_and_stop_drops_only_its_rest() {
        let (mut app, sink, mut reader) = app_with_reader();
        let body = 1 << 20;
        paste(&mut app, body);
        app.input(WindowId(0), InputEvent::Text("K".into()), Source::Human);
        assert_eq!(app.session_waits.pastes.len(), 1, "watched");
        // THE FIRST SAMPLE IS DATED FROM THE PASTE, NOT READ OFF THE CLOCK AFTER
        // IT (the load-sensitive test audit of 2026-09-27). `t0` was
        // `Instant::now()` taken after the 1 MiB paste and a key had been
        // queued, so a test thread stalled a second in between left less than
        // half the grace and failed a correct tree. The sample's instant is the
        // loop's to choose — the tests above pass `now + grace * 5` — so it is
        // placed a quarter of the grace after the paste's own `started`, and
        // what the row must say is then exact: three quarters of the grace
        // left, which a grace counted from the sample (all of it) cannot say.
        let started = app.session_waits.pastes[&0].started;
        let t0 = started + PROGRESS_GRACE / 4;
        assert!(
            app.session_waits_deadline()
                .is_some_and(|d| d <= t0 + WAIT_SAMPLE_IDLE),
            "a sample is armed"
        );
        app.sample_session_waits(t0);
        let live = app.messages.live_rows().next().expect("the paste's row");
        let id = live.id;
        assert_eq!(live.msg.title, "Pasting 1.0 MB");
        assert_eq!(live.msg.finished_title(), "Pasted 1.0 MB");
        assert_eq!(live.msg.actions, [Intent::StopPaste { session: 0 }]);
        assert_eq!(attention(&live.msg), Ok(Attention::Progress));
        assert_eq!(
            live.msg.reveal_after,
            Some(PROGRESS_GRACE - PROGRESS_GRACE / 4),
            "the grace counts from the paste"
        );
        assert_eq!(app.message_band_rows, 0, "inside its grace");
        assert!(
            app.session_waits_deadline()
                .is_some_and(|d| d <= t0 + WAIT_SAMPLE),
            "4 Hz while the row is up"
        );
        // The program takes a prefix; the row follows the writer's count.
        let mut got = vec![0u8; 256 * 1024];
        reader.read_exact(&mut got).expect("a prefix");
        let deadline = Instant::now() + Duration::from_secs(5);
        let stats = loop {
            app.sample_session_waits(Instant::now());
            let meter = app
                .messages
                .live(id)
                .and_then(|l| l.msg.meter.clone())
                .expect("the meter");
            if meter.fill_permille.is_some_and(|f| f >= 200) || Instant::now() > deadline {
                break meter.stats;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(stats.ends_with("/ 1.0 MB"), "{stats}");
        assert!(!stats.trim_start().starts_with("0 B"), "{stats}");
        // Stop paste.
        assert!(app.perform_intent(WindowId(0), id, Intent::StopPaste { session: 0 }));
        assert!(app.session_waits.pastes.is_empty(), "no longer watched");
        assert_eq!(app.messages.live_rows().count(), 0, "the row fades");
        assert!(
            app.messages
                .echoes()
                .iter()
                .all(|e| e.kind == EchoKind::Vanish),
            "a stop claims nothing"
        );
        let mut chunk = [0u8; 8192];
        while !got.ends_with(b"K") {
            let n = reader.read(&mut chunk).expect("the rest");
            assert!(n > 0);
            got.extend_from_slice(&chunk[..n]);
        }
        assert!(got.starts_with(OPEN));
        assert!(
            got.ends_with(&[CLOSE, b"K"].concat()),
            "the paste is closed and the key follows it"
        );
        assert!(
            got.len() < body + OPEN.len() + CLOSE.len(),
            "the rest was dropped"
        );
        drained(&sink);
        // The record says how much went.
        let log = app.read_messages(&aterm_messages::wire::ReadQuery::default());
        assert!(
            log.iter()
                .any(|l| l.contains("Paste%20stopped") || l.contains("Paste stopped")),
            "{log:?}"
        );
        // ONE entry (ruling 265): the row became the record; no withdrawn
        // `Pasting 1.0 MB` beside it, and its dead `Stop paste` is not offered.
        let state = app.messages_state();
        assert_eq!(state.entries.len(), 1, "{:?}", state.entries);
        let entry = &state.entries[0];
        assert_eq!(entry.title, "Paste stopped");
        assert_eq!(entry.id, id.raw(), "the row's own record");
        assert!(
            entry.detail[0].ends_with("sent; the rest was not"),
            "{:?}",
            entry.detail
        );
        assert!(entry.actions.is_empty(), "{:?}", entry.actions);
        assert_eq!((entry.severity, entry.glyph), ("info", '\u{2139}'));
        // A second press finds nothing to stop.
        assert!(!app.stop_paste(0));
    }

    /// A paste delivered in full ends in its finished words, and the sample
    /// disarms once nothing is watched.
    #[test]
    fn a_delivered_paste_completes_in_its_words() {
        let (mut app, sink, mut reader) = app_with_reader();
        paste(&mut app, 1 << 20);
        // Past the grace, before the program reads: the row is up.
        let late = Instant::now() + PROGRESS_GRACE * 2;
        app.sample_session_waits(late);
        app.settle_messages(late);
        assert_eq!(app.message_band_rows, 1, "revealed");
        let drain = std::thread::spawn(move || {
            let mut sink_bytes = Vec::new();
            let mut chunk = [0u8; 65536];
            while !sink_bytes.ends_with(CLOSE) {
                let n = reader.read(&mut chunk).expect("read");
                assert!(n > 0);
                sink_bytes.extend_from_slice(&chunk[..n]);
            }
            sink_bytes.len()
        });
        assert_eq!(
            drain.join().expect("drain"),
            (1 << 20) + OPEN.len() + CLOSE.len()
        );
        drained(&sink);
        app.sample_session_waits(late + WAIT_SAMPLE);
        assert!(app.session_waits.pastes.is_empty());
        assert_eq!(app.session_waits_deadline(), None, "idle again");
        let echo = app.messages.echoes().first().expect("the echo");
        assert_eq!(echo.kind, EchoKind::Complete);
        assert_eq!(echo.msg.finished_title(), "Pasted 1.0 MB");
        // Its record (ruling 265): the finished words under ✓ Success, no
        // in-flight frame, and no `Stop paste` left to press.
        let state = app.messages_state();
        let entry = &state.entries[0];
        assert_eq!(entry.title, "Pasted 1.0 MB");
        assert!(entry.detail.is_empty(), "{:?}", entry.detail);
        assert_eq!((entry.severity, entry.glyph), ("success", '\u{2713}'));
        assert!(entry.actions.is_empty(), "{:?}", entry.actions);
        assert!(
            entry.state_words().starts_with("took "),
            "{}",
            entry.state_words()
        );
    }

    /// THE REWRAP'S TRIGGER (ruling 233): a width change's rewrap nobody
    /// scrolls back into takes no row and arms nothing; a scroll toward
    /// history while it runs posts `Rewrapping scrollback` after the grace,
    /// restated with the lines done, and the re-attach ends it Complete.
    #[test]
    fn a_rewrap_takes_a_row_only_when_the_person_reaches_for_history() {
        let (mut app, _sink, _reader) = app_with_reader();
        let gauge = app.pool.get(0).expect("session 0").ctx.rewrap_gauge.clone();
        // A rewrap nobody waits on: silent from start to end.
        gauge.begin(3_400_000);
        gauge.step(1_200_000);
        app.sample_session_waits(Instant::now());
        assert!(app.session_waits.rewraps.is_empty());
        assert_eq!(app.session_waits_deadline(), None, "nothing armed");
        // Scrolling DOWN is not reaching for history.
        app.input(
            WindowId(0),
            InputEvent::ScrollView(ScrollIntent::Down),
            Source::Human,
        );
        assert!(app.session_waits.rewraps.is_empty());
        gauge.pass_done(3_400_000, None);
        gauge.end(aterm_messages::WaitEnd::Done);
        assert_eq!(app.messages.live_rows().count(), 0);

        // The next one: the person scrolls up into the missing history.
        gauge.begin(3_400_000);
        gauge.step(1_200_000);
        app.input(
            WindowId(0),
            InputEvent::ScrollView(ScrollIntent::Up),
            Source::Human,
        );
        let live = app.messages.live_rows().next().expect("the rewrap's row");
        let id = live.id;
        // The wait's size, said once (ruling 246): `Rewrapping N lines`.
        assert!(
            live.msg.title.starts_with("Rewrapping ") && live.msg.title.ends_with(" lines"),
            "{}",
            live.msg.title
        );
        assert_ne!(live.msg.title, REWRAPPING, "the count is known");
        assert_eq!(attention(&live.msg), Ok(Attention::Progress));
        assert!(live.msg.actions.is_empty(), "nothing to decide");
        assert_eq!(
            live.msg.meter.as_ref().map(|m| m.stats.as_str()),
            Some("1.2M of 3.4M lines")
        );
        assert!(
            live.msg
                .reveal_after
                .is_some_and(|d| d > PROGRESS_GRACE / 2),
            "the grace counts from the ask"
        );
        assert!(app.session_waits_deadline().is_some());
        gauge.step(2_000_000);
        let later = Instant::now() + PROGRESS_GRACE * 2;
        app.sample_session_waits(later);
        app.settle_messages(later);
        assert_eq!(app.message_band_rows, 1, "revealed");
        assert_eq!(
            app.messages
                .live(id)
                .and_then(|l| l.msg.meter.as_ref().map(|m| m.stats.clone()))
                .as_deref(),
            Some("2.0M of 3.4M lines")
        );
        gauge.pass_done(3_400_000, None);
        gauge.end(aterm_messages::WaitEnd::Done);
        app.sample_session_waits(later + WAIT_SAMPLE);
        assert!(app.session_waits.rewraps.is_empty());
        assert_eq!(app.session_waits_deadline(), None);
        let echo = app.messages.echoes().first().expect("the echo");
        assert_eq!(echo.kind, EchoKind::Complete);
        assert_eq!(echo.msg.finished_title(), REWRAPPED);
    }

    /// THE SEARCH'S HONESTY (rulings 234, 237): a search while the history is
    /// detached is a floor — `truncated` on the wire — and the bar and the
    /// title say `none yet, history rewrapping` (never `no matches`); the
    /// person searching is waiting on the rewrap, so its row is posted; and
    /// at the re-attach the search runs again by itself.
    #[test]
    fn a_search_during_a_rewrap_says_partial_history_and_runs_again() {
        let (mut app, _sink, _reader) = app_with_reader();
        app.frontmost_window = Some(WindowId(0));
        let gauge = app.pool.get(0).expect("session 0").ctx.rewrap_gauge.clone();
        gauge.begin(500_000);
        app.find_cmd(FindAction::Open);
        let status = app
            .find_cmd(FindAction::Type("needle".into()))
            .expect("status");
        assert_eq!(status.matches, 0);
        assert!(status.truncated, "a floor, not a verdict");
        let title = app.windows[&WindowId(0)]
            .search
            .as_ref()
            .expect("search")
            .window_title();
        // Ruling 237: the history is away, not empty of the text.
        assert!(title.ends_with("(none yet, history rewrapping)"), "{title}");
        assert!(!title.contains("no matches"), "{title}");
        assert_eq!(
            app.messages
                .live_rows()
                .next()
                .map(|l| l.msg.title.starts_with("Rewrapping ")),
            Some(true),
            "the person searching waits on the rewrap"
        );
        gauge.pass_done(500_000, None);
        gauge.end(aterm_messages::WaitEnd::Done);
        app.sample_session_waits(Instant::now() + WAIT_SAMPLE);
        let status = app.find_cmd(FindAction::Status).expect("status");
        assert!(!status.truncated, "the whole history was searched again");
        assert!(
            !app.windows[&WindowId(0)]
                .search
                .as_ref()
                .expect("search")
                .history_away,
            "the history is back"
        );
        // With no rewrap, a search is the plain census.
        let (mut plain, _s, _r) = app_with_reader();
        plain.frontmost_window = Some(WindowId(0));
        plain.find_cmd(FindAction::Open);
        let status = plain
            .find_cmd(FindAction::Type("needle".into()))
            .expect("status");
        assert!(!status.truncated);
        assert_eq!(plain.messages.live_rows().count(), 0);
    }

    /// A TORN RERUN DOES NOT KEEP SAYING `rewrapping` (ruling 240): the
    /// re-attach runs the search again, and under streaming output both
    /// full-history passes can come back inconsistent. The stale result is
    /// kept (dirty, still a floor), but the history is home, so the bar and
    /// the title stop saying the history is away.
    #[test]
    fn a_torn_rerun_after_the_rewrap_does_not_keep_saying_rewrapping() {
        let (mut app, _sink, _reader) = app_with_reader();
        app.frontmost_window = Some(WindowId(0));
        let gauge = app.pool.get(0).expect("session 0").ctx.rewrap_gauge.clone();
        gauge.begin(500_000);
        app.find_cmd(FindAction::Open);
        app.find_cmd(FindAction::Type("needle".into()))
            .expect("status");
        assert!(
            app.windows[&WindowId(0)]
                .search
                .as_ref()
                .expect("search")
                .history_away,
            "fixture: the search ran while the history was away"
        );
        crate::app_search::FORCE_TORN_SEARCH.with(|torn| torn.set(true));
        gauge.pass_done(500_000, None);
        gauge.end(aterm_messages::WaitEnd::Done);
        app.sample_session_waits(Instant::now() + WAIT_SAMPLE);
        crate::app_search::FORCE_TORN_SEARCH.with(|torn| torn.set(false));
        let search = app.windows[&WindowId(0)].search.as_ref().expect("search");
        assert!(search.results_dirty, "fixture: the rerun came back torn");
        assert!(!search.history_away, "the history is home");
        assert!(search.truncated, "the kept result is still a floor");
        let title = search.window_title();
        assert!(!title.contains("rewrapping"), "{title}");
        assert!(
            title.ends_with("(no matches+…)"),
            "an ordinary stale floor: {title}"
        );
    }

    /// A GLIDE NEVER COMPOUNDS INTO THE AIM (ruling 240): a notch glide in
    /// flight when a width change detaches the history used to re-issue its
    /// unfinished distance on every wake (the viewport rests at the attached
    /// top, so the glide never reached its row), and the aim grew by the
    /// sum. The resize now lands the glide first, so the detach restores the
    /// place it was going to; and a glide that meets a detach it did not see
    /// coming (a `ctl resize` off the loop) settles once.
    #[test]
    fn a_glide_across_a_rewrap_lands_on_its_target_not_the_sum_of_its_ticks() {
        let (mut app, _sink, _reader) = app_with_reader();
        let wid = WindowId(0);
        let (term, pending) = rewrapping_history(&mut app, 500);
        reattach(&mut app, &term, pending);
        let cols = crate::term_lock(&term).cols();
        // Reading 3 rows up, then a notch of 6 more: bound for 9.
        crate::term_lock(&term).scroll_display(3);
        app.scroll_wheel_animated(wid, &term, 6);
        let end = app.windows[&wid]
            .scroll_glide
            .as_ref()
            .expect("a glide")
            .glide
            .end();
        app.tick_scroll_glide(wid, end - Duration::from_millis(150));
        let mid = crate::term_lock(&term).grid().display_offset();
        assert!((4..9).contains(&mid), "fixture: mid-ease ({mid})");
        // The window narrows through the loop's own resize.
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.cols = cols - 10;
        app.resize_panes(wid);
        assert!(
            app.windows[&wid].scroll_glide.is_none(),
            "the glide landed before the detach"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while crate::term_lock(&term).grid().reflow_offload_in_flight() {
            assert!(Instant::now() < deadline, "the rewrap re-attaches");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            crate::term_lock(&term).grid().display_offset(),
            9,
            "the re-attach restores the row the glide was going to"
        );

        // A glide that meets a detach it did not see (the history taken
        // mid-ease, as a cross-session resize does off the loop) settles
        // once: the wakes after it add nothing to the reader's aim.
        let (mut app, _sink, _reader) = app_with_reader();
        let (term, pending) = rewrapping_history(&mut app, 500);
        reattach(&mut app, &term, pending);
        crate::term_lock(&term).scroll_display(3);
        app.scroll_wheel_animated(wid, &term, 6);
        let end = app.windows[&wid]
            .scroll_glide
            .as_ref()
            .expect("a glide")
            .glide
            .end();
        app.tick_scroll_glide(wid, end - Duration::from_millis(150));
        let restored = crate::term_lock(&term).grid().display_offset();
        let pending = {
            let mut t = crate::term_lock(&term);
            let (rows, cols) = (t.rows(), t.cols());
            t.resize_offloading_scrollback(rows, cols - 10)
                .expect("a width change detaches the tiered history")
        };
        for ms in [140, 120, 100, 80, 60, 40, 20, 0] {
            app.tick_scroll_glide(wid, end - Duration::from_millis(ms));
        }
        app.tick_scroll_glide(wid, end + Duration::from_millis(1));
        assert!(app.windows[&wid].scroll_glide.is_none(), "the glide ended");
        let next = crate::term_lock(&term).finish_resize_offload(pending.reflow());
        assert!(next.is_none(), "one pass");
        assert_eq!(
            crate::term_lock(&term).grid().display_offset(),
            restored,
            "the restore stands: the glide's wakes compounded no aim"
        );
    }

    /// THE FINGER MOVES THE VIEW ONCE DURING A REWRAP (ruling 240): a
    /// trackpad's sub-row deltas used to arm the tracked band, whose rows
    /// landed on the reader primitive while the same pixels also banked into
    /// the whole rows the ruling-238 route applies — every row twice. Now the
    /// bank alone carries the finger while the history is away: 3.3 rows of
    /// finger over the rows output attached during the window is 3 rows.
    #[test]
    fn a_trackpad_swipe_during_a_rewrap_moves_the_view_by_the_finger_once() {
        use winit::dpi::PhysicalPosition;
        use winit::event::{MouseScrollDelta, TouchPhase};

        let (mut app, _sink, _reader) = app_with_reader();
        let wid = WindowId(0);
        let (term, pending) = rewrapping_history(&mut app, 500);
        {
            let mut t = crate::term_lock(&term);
            for i in 0..60 {
                t.process(format!("W{i}\r\n").as_bytes());
            }
            assert!(
                t.grid().scrollback_lines() > 8,
                "fixture: output attached rows during the window"
            );
            assert_eq!(t.grid().display_offset(), 0);
        }
        let cell_h = app.win_cell_size(wid).1.max(1) as f64;
        let swipe = |dy: f64| MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, dy));
        // A sub-row delta while the history is away is nothing to do, and
        // costs the engine nothing: no route probe, no band landing.
        let before = crate::term_lock_acquisitions_on_this_thread();
        app.on_mouse_wheel_phased(wid, swipe(cell_h * 0.3), TouchPhase::Moved);
        assert_eq!(
            crate::term_lock_acquisitions_on_this_thread() - before,
            0,
            "a sub-row delta during the rewrap takes no engine lock"
        );
        for _ in 1..11 {
            app.on_mouse_wheel_phased(wid, swipe(cell_h * 0.3), TouchPhase::Moved);
            assert!(
                app.windows[&wid].scroll_glide.is_none(),
                "no band over a history that is away"
            );
        }
        app.on_mouse_wheel_phased(wid, swipe(0.0), TouchPhase::Ended);
        let now = Instant::now();
        for ms in [20, 60, 120, 240, 480] {
            app.service_due_scroll_motion(now + Duration::from_millis(ms));
        }
        assert_eq!(
            crate::term_lock(&term).grid().display_offset(),
            3,
            "the banked rows, applied once"
        );
        assert!(!crate::term_lock(&term).grid().reader_aim_held());
        reattach(&mut app, &term, pending);
    }

    #[test]
    fn completed_paste_meters_release_during_a_continuous_burst_without_losing_totals() {
        use aterm_session::sink::BulkMeter;

        let (mut app, sink, _reader) = app_with_reader();
        let now = Instant::now();
        let outstanding = Arc::new(BulkMeter::new());
        app.watch_paste(0, &sink, outstanding.clone(), 100, now);
        let mut expected_sent = 0;
        let mut expected_total = 100;
        for index in 0usize..256 {
            let meter = Arc::new(BulkMeter::new());
            app.watch_paste(0, &sink, meter.clone(), 100, now);
            if index.is_multiple_of(2) {
                meter.settle_unmetered(true);
                expected_sent += 100;
            } else {
                meter.abandon();
            }
            expected_total += 100;
            let released = Arc::downgrade(&meter);
            drop(meter);
            app.sample_session_waits(now);
            assert!(
                released.upgrade().is_none(),
                "completed meter {index} was retained"
            );
            let watch = &app.session_waits.pastes[&0];
            assert_eq!(watch.meters.len(), 1, "only unfinished work stays resident");
            assert!(Arc::ptr_eq(&watch.meters[0].0, &outstanding));
            assert_eq!(watch.amounts(), (expected_sent, expected_total));
            assert!(!watch.completed_failed);
        }

        // Joining a later paste uses the same compaction path. Its predecessor's
        // failure must survive releasing that meter and still end the wait.
        let failed = Arc::new(BulkMeter::new());
        app.watch_paste(0, &sink, failed.clone(), 200, now);
        failed.settle_unmetered(false);
        let released = Arc::downgrade(&failed);
        drop(failed);
        let later = Arc::new(BulkMeter::new());
        app.watch_paste(0, &sink, later.clone(), 300, now);
        let watch = &app.session_waits.pastes[&0];
        assert!(released.upgrade().is_none());
        assert_eq!(watch.meters.len(), 2);
        assert!(
            watch.completed_failed,
            "compaction must retain the failed verdict"
        );
        assert_eq!(watch.amounts(), (expected_sent, expected_total + 500));
        app.sample_session_waits(now);
        assert!(!app.session_waits.pastes.contains_key(&0));
        assert_eq!(Arc::strong_count(&outstanding), 1);
        assert_eq!(Arc::strong_count(&later), 1);
    }

    /// Session 0 given a tiered history of `n` short lines, detached for a
    /// width change with its gauge booked — a rewrap in flight, as the loop's
    /// hand-off leaves it. Returns the job to finish.
    fn rewrapping_history(
        app: &mut App,
        n: usize,
    ) -> (
        Arc<std::sync::Mutex<aterm_core::terminal::Terminal>>,
        aterm_grid::PendingScrollbackReflow,
    ) {
        let term = app.pool.get(0).expect("session 0").term.clone();
        let pending = {
            let mut t = crate::term_lock(&term);
            let (rows, cols) = (t.rows(), t.cols());
            // The session's own terminal keeps its mode mirror (the seam checks
            // it); only its grid becomes a tiered one, as a real session's is.
            *t.grid_mut() = aterm_grid::Grid::with_tiered_scrollback(
                rows,
                cols,
                8,
                aterm_scrollback::ScrollbackStorage::from(aterm_scrollback::Scrollback::new(
                    64, 512, 8_000_000,
                )),
            );
            let mut buf = Vec::new();
            for i in 0..n {
                buf.extend_from_slice(format!("H{i}\r\n").as_bytes());
            }
            t.process(&buf);
            t.resize_offloading_scrollback(rows, cols - 10)
                .expect("a width change detaches the tiered history")
        };
        let gauge = app.pool.get(0).expect("session 0").ctx.rewrap_gauge.clone();
        gauge.begin(pending.lines_total());
        (term, pending)
    }

    fn reattach(
        app: &mut App,
        term: &std::sync::Mutex<aterm_core::terminal::Terminal>,
        pending: aterm_grid::PendingScrollbackReflow,
    ) {
        let next = crate::term_lock(term).finish_resize_offload(pending.reflow());
        assert!(next.is_none(), "one pass");
        let gauge = app.pool.get(0).expect("session 0").ctx.rewrap_gauge.clone();
        gauge.end(aterm_messages::WaitEnd::Done);
        app.sample_session_waits(Instant::now() + WAIT_SAMPLE_IDLE);
    }

    /// THE ASK SURVIVES THE REWRAP (ruling 238): a wheel up while the history
    /// is away moves nothing — there is nothing above to show — but the view
    /// lands where it was asked to at the re-attach, with no second scroll and
    /// no jump the person did not ask for. A keystroke in between is them
    /// coming back to the prompt, and the re-attach leaves them there.
    #[test]
    fn a_scroll_up_during_a_rewrap_lands_where_it_asked_at_the_reattach() {
        use aterm_types::mouse::WheelDir;

        use crate::input::PixelOffset;

        let wheel_up = || InputEvent::Wheel {
            dir: WheelDir::Up,
            lines: 3,
            row: 0,
            col: 0,
            mods: 0,
            px_off: PixelOffset::CELL_ORIGIN,
        };
        let (mut app, _sink, _reader) = app_with_reader();
        let (term, pending) = rewrapping_history(&mut app, 500);
        app.input(WindowId(0), wheel_up(), Source::Human);
        {
            let t = crate::term_lock(&term);
            assert_eq!(t.grid().display_offset(), 0, "nothing to show yet");
            assert!(t.grid().reader_aim_held(), "the ask is kept");
        }
        assert!(
            app.windows[&WindowId(0)].scroll_glide.is_none(),
            "no glide over a history that is not there"
        );
        assert_eq!(
            app.messages
                .live_rows()
                .next()
                .map(|l| l.msg.title.starts_with("Rewrapping ")),
            Some(true),
            "the person reaching for history waits on the rewrap"
        );
        app.input(WindowId(0), wheel_up(), Source::Human);
        reattach(&mut app, &term, pending);
        let landed = crate::term_lock(&term).grid().display_offset();
        assert!(landed > 0, "the view lands up in the history it asked for");

        // The same two notches with the history home: the same place.
        let (mut plain, _s, _r) = app_with_reader();
        let (pterm, ppending) = rewrapping_history(&mut plain, 500);
        reattach(&mut plain, &pterm, ppending);
        let before = crate::term_lock(&pterm).grid().display_offset();
        assert_eq!(before, 0);
        for _ in 0..2 {
            plain.input(WindowId(0), wheel_up(), Source::Human);
            plain.settle_scroll_motion_at_target(WindowId(0), Instant::now());
        }
        assert_eq!(
            crate::term_lock(&pterm).grid().display_offset(),
            landed,
            "exactly where the same notches go once the history is home"
        );

        // Aim up, then type: the person came back to the prompt.
        let (mut app, _sink, _reader) = app_with_reader();
        let (term, pending) = rewrapping_history(&mut app, 500);
        app.input(WindowId(0), wheel_up(), Source::Human);
        assert!(crate::term_lock(&term).grid().reader_aim_held());
        app.input(WindowId(0), InputEvent::Text("k".into()), Source::Human);
        reattach(&mut app, &term, pending);
        assert_eq!(
            crate::term_lock(&term).grid().display_offset(),
            0,
            "typing brought the aim home; the re-attach does not jump the view"
        );
    }
}
