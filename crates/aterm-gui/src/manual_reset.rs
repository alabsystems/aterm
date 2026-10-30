// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The manual reset.

use std::sync::Arc;
#[cfg(unix)]
use std::sync::mpsc::TrySendError;
use std::sync::mpsc::{RecvTimeoutError, SyncSender};
use std::time::Duration;

use aterm_core::terminal::{ClockReading, Terminal};

use crate::SessionCtx;

/// Who asked for a reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResetSource {
    /// The `reset` control verb.
    Verb,
    /// Edit ▸ Reset Terminal (the menu bar, the palette, `invoke ResetTerminal`).
    Menu,
}

impl ResetSource {
    /// The `source=` token.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            ResetSource::Verb => "ctl",
            ResetSource::Menu => "menu",
        }
    }
}

/// One reset request, as it travels to the parse stage.
#[derive(Clone, Debug)]
pub(crate) struct ManualReset {
    pub(crate) source: ResetSource,
    /// Where the parse stage answers.
    pub(crate) reply: Option<SyncSender<ResetReport>>,
}

/// What one reset did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResetReport {
    /// The handback's `reverted` names, in emission order (empty: the terminal was already at its host defaults).
    pub(crate) reverted: Vec<&'static str>,
    /// How many synthesized bytes were processed.
    pub(crate) bytes: usize,
}

/// How long the verb waits for the parse stage.
#[cfg(not(test))]
pub(crate) const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// A unit test's parse stage is its own live reader, which answers: a reply a loaded machine
/// slows is waited for to the hang detector, not answered "timed out".
#[cfg(test)]
pub(crate) const REPLY_TIMEOUT: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The verb's answer when a seamless update was committing the session for its whole wait ([`run_direct`]).
pub(crate) const RESET_BUSY: &str = "ERR busy update-committing (the reset reached this aterm, \
                                     which an update is replacing; retry)\n";

/// The `modes-restored` payload of a manual reset.
pub(crate) fn manual_payload(source: ResetSource, report: &ResetReport) -> String {
    let reverted = if report.reverted.is_empty() {
        "-".to_string()
    } else {
        report.reverted.join(",")
    };
    format!(
        "reason=manual source={} reverted={reverted} bytes={}",
        source.as_str(),
        report.bytes
    )
}

/// The engine half at the caller's `clock`, under a lock the caller already holds.
pub(crate) fn apply(t: &mut Terminal, clock: ClockReading) -> (ResetReport, Vec<u8>) {
    let h = t.manual_handback_at(clock);
    let report = ResetReport {
        reverted: h.reverted,
        bytes: h.bytes.len(),
    };
    (report, h.bytes)
}

/// Record the reset on the session's timeline and in the log.
pub(crate) fn record(
    sid: u64,
    timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
    source: ResetSource,
    report: &ResetReport,
) {
    let payload = manual_payload(source, report);
    {
        let mut tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
        let by = match source {
            ResetSource::Menu => aterm_slog::Actor::Human,
            ResetSource::Verb => aterm_slog::Actor::Nobody,
        };
        let at = tl.slog().now();
        tl.slog_mut().observe_modes(
            crate::session_log::observed::ModesFrom::Reset(source.as_str()),
            &report.reverted,
            report.bytes,
            by,
            at,
        );
        tl.record("modes-restored");
    }
    aterm_log::info!("session {sid}: manual reset: {payload}");
}

/// The reset where no parse stage can carry it.
pub(crate) fn run_direct(
    sid: u64,
    term: &std::sync::Mutex<Terminal>,
    ctx: &SessionCtx,
    source: ResetSource,
    fence_wait: Option<Duration>,
) -> Result<ResetReport, String> {
    let (report, drain) = {
        let mut t = crate::term_lock(term);
        let recording = ctx
            .temporal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .total_events()
            > 0;
        let pre = recording.then(|| crate::temporal::SpinePre::take(&t));
        let clock = ClockReading::now();
        let (report, bytes) = apply(&mut t, clock);
        let drain = if let Some(pre) = pre
            && let Ok(tx) = crate::temporal::open(&ctx.temporal, crate::temporal::QUEUE_CAP)
        {
            let mut mark = crate::temporal::SpineMark::attach(tx.spine());
            if !bytes.is_empty() {
                mark.record(
                    &tx,
                    pre,
                    &t,
                    crate::temporal::InputSlice::at(&bytes[..], clock),
                );
            } else {
                mark.flush_host(&tx, &t);
            }
            Some(tx.drain())
        } else {
            None
        };
        (report, drain)
    };
    if let Some(drain) = drain {
        drain.wait();
    }
    let noted = ctx.fabric.note_direct_reset(fence_wait);
    record(sid, &ctx.timeline, source, &report);
    if noted {
        Ok(report)
    } else {
        Err(RESET_BUSY.to_string())
    }
}

/// Ask for a reset and, with `wait`, for its report.
pub(crate) fn request(
    sid: u64,
    term: &std::sync::Mutex<Terminal>,
    ctx: &SessionCtx,
    source: ResetSource,
    wait: Option<Duration>,
) -> Result<Option<ResetReport>, String> {
    let (tx, rx) = match wait {
        Some(_) => {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            (Some(tx), Some(rx))
        }
        None => (None, None),
    };
    let req = ManualReset { source, reply: tx };
    let offer = ctx.reset_lane.offer(&req);
    drop(req);
    match offer {
        LaneOffer::Queued => {}
        LaneOffer::Busy => {
            return Err(
                "ERR busy reset (this session's reader queue is full; retry)\n".to_string(),
            );
        }
        LaneOffer::Closed => return run_direct(sid, term, ctx, source, wait).map(Some),
    }
    let (Some(rx), Some(timeout)) = (rx, wait) else {
        return Ok(None);
    };
    match rx.recv_timeout(timeout) {
        Ok(report) => Ok(Some(report)),
        Err(RecvTimeoutError::Timeout) => Err(format!(
            "ERR reset timed out after {} ms (the session's reader is busy; the reset stays \
             queued and runs when the reader reaches it, unless the reader stops first)\n",
            timeout.as_millis()
        )),
        Err(RecvTimeoutError::Disconnected) => run_direct(sid, term, ctx, source, wait).map(Some),
    }
}

/// What [`ResetLane::offer`] did with a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "only a unix reader takes a reset off the lane")
)]
pub(crate) enum LaneOffer {
    /// On the parse stage's channel.
    Queued,
    /// The channel is full (it holds the gather's batches too).
    Busy,
    /// No parse stage takes requests.
    Closed,
}

/// The session's way onto its PTY reader's parse stage.
#[derive(Default)]
pub(crate) struct ResetLane {
    #[cfg(unix)]
    slot: std::sync::Mutex<Option<OpenLane>>,
}

/// One reader generation's lane: its parse stage's channel, and its gather's poke when it has a
/// poke pipe (`crate::act_barrier`).
#[cfg(unix)]
struct OpenLane {
    generation: u64,
    tx: SyncSender<crate::spawn::GatherMsg>,
    poke: Option<crate::act_barrier::Poke>,
    /// A barrier went [`Passage::Late`](crate::act_barrier::Passage::Late) and the parse stage has
    /// answered none since ([`ResetLane::answered`]).
    behind: bool,
}

impl std::fmt::Debug for ResetLane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResetLane").finish_non_exhaustive()
    }
}

/// Distinct install generations across every session of the process.
#[cfg(unix)]
static LANE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl ResetLane {
    /// Install `tx` (a reader starting), with `poke_wr`, the write end of its gather's poke pipe,
    /// which the lane then owns and closes.
    #[cfg(unix)]
    pub(crate) fn open(
        &self,
        tx: SyncSender<crate::spawn::GatherMsg>,
        poke_wr: Option<i32>,
    ) -> u64 {
        let generation = LANE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let poke = poke_wr.map(|wr| crate::act_barrier::Poke {
            wr,
            asked: Vec::new(),
        });
        *self.slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(OpenLane {
            generation,
            tx,
            poke,
            behind: false,
        });
        generation
    }

    /// Remove the sender if it is still generation `generation`'s.
    #[cfg(unix)]
    pub(crate) fn close_if(&self, generation: u64) {
        let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        if slot
            .as_ref()
            .is_some_and(|lane| lane.generation == generation)
        {
            *slot = None;
        }
    }

    /// Nudge the parse stage to bring the session's crash-restore journal up to date (`crate::crash_restore`).
    #[cfg(unix)]
    pub(crate) fn nudge(&self) -> bool {
        let slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        slot.as_ref()
            .is_some_and(|lane| lane.tx.try_send(crate::spawn::GatherMsg::Journal).is_ok())
    }

    /// Ask the session's reader for a barrier (`crate::act_barrier`): the answer comes once the
    /// parser holds every byte the session's program wrote to its PTY before this call (off macOS,
    /// every byte the gather had read when it took the ask), and disconnects if the reader stops
    /// first. `None` with no live reader that takes one. [`Self::pass`] asks and waits; a test
    /// holds the answer itself.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn ask(&self) -> Option<std::sync::mpsc::Receiver<()>> {
        self.ask_at().map(|(_, answer)| answer)
    }

    /// Ask for a barrier (`ask` in a test), with the reader generation that took the ask.
    #[cfg(unix)]
    fn ask_at(&self) -> Option<(u64, std::sync::mpsc::Receiver<()>)> {
        let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        let lane = slot.as_mut()?;
        let poke = lane.poke.as_mut()?;
        let (tx, answer) = std::sync::mpsc::sync_channel(1);
        poke.asked.push(tx);
        // A full pipe already holds a poke the gather has yet to take.
        aterm_pty::wake(poke.wr);
        Some((lane.generation, answer))
    }

    /// The parse stage of generation `generation` answered a barrier: its parser has caught up.
    #[cfg(unix)]
    pub(crate) fn answered(&self, generation: u64) {
        let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(lane) = slot.as_mut().filter(|lane| lane.generation == generation) {
            lane.behind = false;
        }
    }

    /// Whether a barrier went late and the parser has answered none since.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn behind_for_test(&self) -> bool {
        let slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        slot.as_ref().is_some_and(|lane| lane.behind)
    }

    /// How many barriers wait for the gather to take them ([`Self::ask`]).
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn asked_for_test(&self) -> usize {
        let slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        slot.as_ref()
            .and_then(|lane| lane.poke.as_ref())
            .map_or(0, |poke| poke.asked.len())
    }

    /// Wait, at most `patience`, on a barrier (`ask`); while an earlier one is late and
    /// unanswered, ask none ([`Passage::Behind`](crate::act_barrier::Passage::Behind)). Holds the
    /// lane's own lock only to ask, so the caller must hold neither the terminal's lock nor the
    /// timeline's, which the parse stage takes.
    pub(crate) fn pass(&self, patience: Duration) -> crate::act_barrier::Passage {
        use crate::act_barrier::Passage;
        #[cfg(unix)]
        {
            if self.behind_now() {
                return Passage::Behind;
            }
            let Some((generation, answer)) = self.ask_at() else {
                return Passage::Unbarriered;
            };
            match answer.recv_timeout(patience) {
                Ok(()) => Passage::Passed,
                Err(RecvTimeoutError::Timeout) => {
                    let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
                    // Under the lock [`Self::answered`] takes after it answers: an answer that came
                    // since is seen here, and one that comes later clears what is set here.
                    if answer.try_recv().is_ok() {
                        return Passage::Passed;
                    }
                    if let Some(lane) = slot.as_mut().filter(|lane| lane.generation == generation) {
                        lane.behind = true;
                    }
                    Passage::Late
                }
                Err(RecvTimeoutError::Disconnected) => Passage::Gone,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = patience;
            Passage::Unbarriered
        }
    }

    #[cfg(unix)]
    fn behind_now(&self) -> bool {
        let slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        slot.as_ref().is_some_and(|lane| lane.behind)
    }

    /// The barriers asked of generation `generation`'s gather since it last took them (its poke).
    #[cfg(unix)]
    pub(crate) fn take_asked(&self, generation: u64) -> Vec<SyncSender<()>> {
        let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        slot.as_mut()
            .filter(|lane| lane.generation == generation)
            .and_then(|lane| lane.poke.as_mut())
            .map(|poke| std::mem::take(&mut poke.asked))
            .unwrap_or_default()
    }

    /// Offer `req` to the parse stage without blocking (the main thread calls this for the menu).
    pub(crate) fn offer(&self, req: &ManualReset) -> LaneOffer {
        #[cfg(unix)]
        {
            let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
            let Some(lane) = slot.as_ref() else {
                return LaneOffer::Closed;
            };
            match lane
                .tx
                .try_send(crate::spawn::GatherMsg::Reset(req.clone()))
            {
                Ok(()) => LaneOffer::Queued,
                Err(TrySendError::Full(_)) => LaneOffer::Busy,
                Err(TrySendError::Disconnected(_)) => {
                    *slot = None;
                    LaneOffer::Closed
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = req;
            LaneOffer::Closed
        }
    }
}

/// Closes a [`ResetLane`] generation when dropped, then `poke_rd`, the read end of its gather's
/// poke pipe (`-1` for none): closed after the lane, whose write end then no longer takes a poke.
#[cfg(unix)]
pub(crate) struct LaneGuard {
    pub(crate) lane: Arc<ResetLane>,
    pub(crate) generation: u64,
    pub(crate) poke_rd: i32,
}

#[cfg(unix)]
impl Drop for LaneGuard {
    fn drop(&mut self) {
        self.lane.close_if(self.generation);
        aterm_pty::close_fd(self.poke_rd);
    }
}

/// The `reset [flush]` control verb against one session.
pub(crate) fn cmd_reset(
    sid: u64,
    term: &Arc<std::sync::Mutex<Terminal>>,
    ctx: &SessionCtx,
    rest: &str,
) -> String {
    let flush = match rest.trim() {
        "" => false,
        "flush" => true,
        _ => return "ERR usage: reset [flush]\n".to_string(),
    };
    let report = match request(sid, term, ctx, ResetSource::Verb, Some(REPLY_TIMEOUT)) {
        Ok(Some(report)) => report,
        Ok(None) => return "OK reset queued\n".to_string(),
        Err(refusal) => return refusal,
    };
    let reverted = if report.reverted.is_empty() {
        "-".to_string()
    } else {
        report.reverted.join(",")
    };
    let mut reply = format!("OK reset reverted={reverted} bytes={}", report.bytes);
    if flush {
        reply.push_str(&discarded_field(flush_input(ctx)));
    }
    reply.push('\n');
    reply
}

/// `flush`'s discard: how many unread input bytes went, or `None` when nothing could be
/// flushed: on every platform but macOS, the only one that measures the slave's input queue
/// ([`aterm_pty::input_queue_len`]), and for a session with no tty.
#[cfg(unix)]
fn flush_input(ctx: &SessionCtx) -> Option<usize> {
    let dropped = ctx
        .sink
        .discard_unread_input(aterm_session::sink::Discard::Flush);
    if dropped.is_some() {
        let _ = ctx.sink.wake_input_hook();
    }
    dropped
}

/// Windows twin of the Unix `flush_input`: ConPTY has no measured queue to
/// flush (`aterm_pty::flush_input_queue` is `None` there), so nothing goes.
#[cfg(not(unix))]
fn flush_input(_ctx: &SessionCtx) -> Option<usize> {
    None
}

/// The ` discarded=` field of a `reset flush` reply: the count, or `-` when nothing was
/// flushed.
fn discarded_field(discarded: Option<usize>) -> String {
    match discarded {
        Some(n) => format!(" discarded={n}"),
        None => " discarded=-".to_string(),
    }
}

impl crate::App {
    /// Edit ▸ Reset Terminal on `wid`'s focused session.
    pub(crate) fn menu_reset_terminal(&self, wid: crate::WindowId) {
        let Some(s) = self
            .focused_session_id(wid)
            .and_then(|id| self.pool.get(id))
        else {
            aterm_log::info!("reset terminal dropped: no focused session");
            return;
        };
        if let Err(refusal) = request(s.id, &s.term, &s.ctx, ResetSource::Menu, None) {
            aterm_log::warn!("reset terminal: {}", refusal.trim_end());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ResetReport, ResetSource, manual_payload};
    use crate::menu::MenuAction;
    use crate::{App, WindowId};

    #[test]
    fn manual_reset_payload_names_its_reason_and_source() {
        let r = ResetReport {
            reverted: vec!["alt", "mouse"],
            bytes: 17,
        };
        assert_eq!(
            manual_payload(ResetSource::Verb, &r),
            "reason=manual source=ctl reverted=alt,mouse bytes=17"
        );
        let empty = ResetReport {
            reverted: Vec::new(),
            bytes: 0,
        };
        assert_eq!(
            manual_payload(ResetSource::Menu, &empty),
            "reason=manual source=menu reverted=- bytes=0"
        );
    }

    /// Edit ▸ Reset Terminal: enabled over a terminal tab, reachable by its `invoke` name, writes
    /// no PTY input, and on a reader-less stub resets the focused session and records
    /// `source=menu`.
    #[test]
    fn reset_terminal_menu_row_resets_the_focused_session() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let sid = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid));
        app.frontmost_window = Some(wid);
        let (term, ctx) = {
            let s = app.pool.get(sid).expect("stub pooled");
            (s.term.clone(), s.ctx.clone())
        };
        term.lock()
            .expect("terminal lock")
            .process(b"\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b(0\x0e% ");
        assert!(term.lock().expect("terminal lock").program_owns_terminal());

        let row = app
            .palette_snapshot(wid)
            .rows()
            .iter()
            .find(|r| r.action == MenuAction::ResetTerminal)
            .cloned()
            .expect("Reset Terminal has a palette row");
        assert!(row.enabled, "enabled over a terminal tab");
        assert_eq!(
            MenuAction::from_invoke_name("ResetTerminal"),
            Some(MenuAction::ResetTerminal)
        );
        assert!(!MenuAction::ResetTerminal.writes_pty_input());

        app.menu_reset_terminal(wid);
        let t = term.lock().expect("terminal lock");
        assert!(!t.program_owns_terminal(), "the modes are handed back");
        assert!(!t.is_alternate_screen());
        drop(t);
        let events: Vec<String> = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .since(None)
            .filter(|e| e.kind == "modes-restored")
            .map(|e| e.payload.to_string())
            .collect();
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(
            events[0].starts_with("reason=manual source=menu reverted=")
                && events[0].contains("alt")
                && events[0].contains("mouse")
                && events[0].contains("g0")
                && events[0].contains("gl"),
            "{events:?}"
        );
    }

    /// `reset flush` where nothing can be flushed answers `discarded=-`, never `discarded=0`. The
    /// stub's sink has no tty (`master = -1`), the same `None` every Linux and Windows session
    /// gets.
    #[test]
    fn reset_flush_that_flushes_nothing_says_so() {
        let s = crate::stub_session(11);
        s.term
            .lock()
            .expect("terminal lock")
            .process(b"\x1b[?1003h\x1b[?1006h");
        assert_eq!(
            super::cmd_reset(11, &s.term, &s.ctx, "flush"),
            "OK reset reverted=mouse,mouse-encoding bytes=16 discarded=-\n"
        );
        assert!(
            !s.term
                .lock()
                .expect("terminal lock")
                .program_owns_terminal(),
            "the reset itself still ran"
        );
        assert_eq!(super::discarded_field(None), " discarded=-");
        assert_eq!(super::discarded_field(Some(0)), " discarded=0");
        assert_eq!(super::discarded_field(Some(9)), " discarded=9");
    }

    /// On a real raw pty with an unread SGR mouse report queued, `reset flush` drops and counts it.
    #[test]
    #[cfg(target_os = "macos")]
    fn reset_flush_on_a_real_tty_discards_and_counts_the_queue() {
        use crate::input_stall::tests::raw_pty_pair;
        let (master, slave) = raw_pty_pair();
        let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(master));
        let s = crate::stub_session_with_sink(12, sink.clone());
        let report = b"\x1b[<35;10;5M";
        sink.write_frame_nonparking(report).expect("queued");
        assert_eq!(aterm_pty::input_queue_len(master), Some(report.len()));
        assert_eq!(
            super::cmd_reset(12, &s.term, &s.ctx, "flush"),
            format!("OK reset reverted=- bytes=0 discarded={}\n", report.len())
        );
        assert_eq!(aterm_pty::input_queue_len(master), Some(0));
        drop(s);
        drop(sink);
        // SAFETY: both fds came from `raw_pty_pair` and are closed once, here.
        unsafe {
            libc::close(slave);
            libc::close(master);
        }
    }

    /// A reader that ends with a reset queued and unanswered disconnects the verb's wait, and the
    /// reset runs directly rather than timing out as "stays queued".
    #[test]
    #[cfg(unix)]
    fn a_reader_that_ends_without_answering_runs_the_reset_directly() {
        let s = crate::stub_session(7);
        let (tx, rx) = std::sync::mpsc::sync_channel::<crate::spawn::GatherMsg>(4);
        let generation = s.ctx.reset_lane.open(tx, None);
        s.term
            .lock()
            .expect("terminal lock")
            .process(b"\x1b[?1000h");
        let (term, ctx) = (s.term.clone(), s.ctx.clone());
        let asker = std::thread::spawn(move || {
            super::request(
                7,
                &term,
                &ctx,
                ResetSource::Verb,
                Some(aterm_spec::wait::MUST_HAPPEN),
            )
        });
        let msg = rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the reset was queued on the lane");
        assert!(matches!(msg, crate::spawn::GatherMsg::Reset(_)));
        s.ctx.reset_lane.close_if(generation);
        drop(msg);
        drop(rx);
        let report = asker
            .join()
            .expect("the asker thread")
            .expect("a report, not a timeout")
            .expect("waited for");
        assert_eq!(report.reverted, vec!["mouse"]);
        assert!(
            !s.term
                .lock()
                .expect("terminal lock")
                .program_owns_terminal()
        );
    }
}
