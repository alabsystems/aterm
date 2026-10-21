// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The manual reset: the escape hatch for a terminal the automatic foreground handback
//! (`crate::foreground_handback`) cannot reach. It is the `reset` control verb
//! (`aterm ctl @<sid> reset [flush]`) and the Edit ▸ Reset Terminal menu row
//! (`invoke ResetTerminal`).
//!
//! The handback acts only at a foreground change whose old holder is gone and owned an
//! input-hijacking mode. Modes armed under a holder that never exits (a live shell), a shifted
//! charset, an OSC 8 link a killed program left open, or a foreground change the sampler missed
//! stay stuck. [`aterm_core::terminal::Terminal::manual_handback`] is the handback's byte plan
//! without its evidence gate (a torn sequence gets `CAN`, every program-negotiable mode goes
//! back to its host target, an open OSC 8 link is closed, and program-set palette/fg/bg/cursor/
//! selection colours revert via OSC 104/110-119 with the colour stack kept); it never clears
//! the screen or the scrollback.
//!
//! # Where it runs
//!
//! On the parse stage of the session's PTY reader, as one more message on the gather→parse
//! channel ([`ResetLane`]), so it lands between two PTY batches: its synthesized bytes are
//! recorded on the temporal spine as `RawIn` under the term-lock hold that processed them and
//! ride the cast/`bytes` taps in order (a replay reaches the live state), and its
//! `modes-restored` event is written by the thread that writes the automatic ones. A reset run
//! under the control thread's own term lock would be recorded out of order against the
//! reader's cast tap.
//!
//! When no reader runs (Windows, an exited reader) or an update has parked the session, the
//! reset runs directly under the term lock, and on a parked session advances the carry serial
//! (`fabric::SessionFabric::note_direct_reset`) so a concurrent `Commit` stands down rather than
//! restoring the pre-reset carry.
//!
//! `flush` (the verb only) then drops the tty input queue, the kernel's and whatever aterm
//! still holds behind it, so the reports the stuck modes typed go too. macOS only: the discard
//! needs the slave's input-queue reading (`aterm_pty::input_queue_len`), measured on Darwin
//! alone (a Linux master's FIONREAD counts the other direction). Elsewhere the reply says
//! `discarded=-`.

use std::sync::Arc;
#[cfg(unix)]
use std::sync::mpsc::TrySendError;
use std::sync::mpsc::{RecvTimeoutError, SyncSender};
use std::time::Duration;

use aterm_core::terminal::Terminal;

use crate::SessionCtx;

/// Who asked for a reset — the `source=` of its timeline event.
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
    /// Where the parse stage answers; `None` for a fire-and-forget request
    /// (the menu: the main thread never waits on the reader).
    pub(crate) reply: Option<SyncSender<ResetReport>>,
}

/// What one reset did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResetReport {
    /// The handback's `reverted` names, in emission order (empty: the
    /// terminal was already at its host defaults).
    pub(crate) reverted: Vec<&'static str>,
    /// How many synthesized bytes were processed.
    pub(crate) bytes: usize,
}

/// How long the verb waits for the parse stage. Past it the reply says the request stays
/// queued: it runs when the parse stage reaches it, unless the reader stops first (a park or
/// the session's end drops the channel and the request). A request the reader drops while the
/// verb still waits runs directly ([`request`]). A direct reset waits as long again for a
/// committing update ([`run_direct`]).
pub(crate) const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// The verb's answer when a seamless update was committing the session for its whole wait
/// ([`run_direct`]): the reset ran on this process's engine, which the update is replacing.
/// `ERR busy` is the class a driver retries, and the retry reaches whichever process then owns
/// the session.
pub(crate) const RESET_BUSY: &str = "ERR busy update-committing (the reset reached this aterm, \
                                     which an update is replacing; retry)\n";

/// The `modes-restored` payload of a manual reset:
/// `reason=manual source=<ctl|menu> reverted=<csv|-> bytes=<n>`. An automatic handback's
/// starts `from=`.
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

/// The engine half, under a lock the caller already holds.
pub(crate) fn apply(t: &mut Terminal) -> (ResetReport, Vec<u8>) {
    let h = t.manual_handback();
    let report = ResetReport {
        reverted: h.reverted,
        bytes: h.bytes.len(),
    };
    (report, h.bytes)
}

/// Record the reset on the session's timeline and in the log. The timeline is
/// a strict leaf: the caller holds no other lock.
pub(crate) fn record(
    sid: u64,
    timeline: &std::sync::Mutex<crate::session_timeline::SessionTimeline>,
    source: ResetSource,
    report: &ResetReport,
) {
    let payload = manual_payload(source, report);
    {
        let mut tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
        tl.record("modes-restored", payload.clone());
        // The session log's `modes` mark: a person's reset is theirs; the
        // verb's caller is not known here (the verb's reply wakes, the
        // reader's rides its output wake).
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
    }
    aterm_log::info!("session {sid}: manual reset: {payload}");
}

/// The reset where no parse stage can carry it: under the term lock on this
/// thread, then the carry serial and the timeline (see the module docs).
///
/// `fence_wait` bounds the wait for a committing update's fence
/// (`fabric::SessionFabric::note_direct_reset`); past it the answer is
/// [`RESET_BUSY`]. `None` on the main thread, which never waits: it raises the
/// fence itself.
pub(crate) fn run_direct(
    sid: u64,
    term: &std::sync::Mutex<Terminal>,
    ctx: &SessionCtx,
    source: ResetSource,
    fence_wait: Option<Duration>,
) -> Result<ResetReport, String> {
    let (report, _) = {
        let mut t = crate::term_lock(term);
        apply(&mut t)
    };
    let noted = ctx.fabric.note_direct_reset(fence_wait);
    record(sid, &ctx.timeline, source, &report);
    if noted {
        Ok(report)
    } else {
        Err(RESET_BUSY.to_string())
    }
}

/// Ask for a reset and, with `wait`, for its report. The parse stage runs it
/// when the lane is open; otherwise it runs here, directly.
///
/// `Ok(None)`: queued on the parse stage and not waited for (`wait` = `None`).
/// `Err`: the refusal line for the verb (`ERR …\n`).
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
    // The queued message holds the only reply sender from here on, so a reader that ends
    // without answering disconnects the wait below instead of letting it time out.
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
        // The reader exited with the request unanswered: no stream is left to
        // order against, so run it here.
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
    /// The channel is full (it holds the gather's batches too): refused.
    Busy,
    /// No parse stage takes requests: run it directly.
    Closed,
}

/// The session's way onto its PTY reader's parse stage: a clone of the gather→parse sender,
/// installed by each reader when it starts and removed when either stage ends
/// ([`Self::close_if`]). Lives on [`SessionCtx`], held by the control and main threads.
///
/// It must never keep the channel open itself: the parse stage waits on `recv()` with no
/// timeout and relies on the gather to end the channel, so a guard on the gather's exit path
/// drops the clone ([`LaneGuard`]). The generation stamp keeps a late-exiting old reader from
/// removing the sender a re-attached reader just installed.
#[derive(Default)]
pub(crate) struct ResetLane {
    #[cfg(unix)]
    slot: std::sync::Mutex<Option<(u64, SyncSender<crate::spawn::GatherMsg>)>>,
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
    /// Install `tx` (a reader starting); returns the generation to close with.
    #[cfg(unix)]
    pub(crate) fn open(&self, tx: SyncSender<crate::spawn::GatherMsg>) -> u64 {
        let generation = LANE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *self.slot.lock().unwrap_or_else(|p| p.into_inner()) = Some((generation, tx));
        generation
    }

    /// Remove the sender if it is still generation `generation`'s.
    #[cfg(unix)]
    pub(crate) fn close_if(&self, generation: u64) {
        let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        if slot.as_ref().is_some_and(|(g, _)| *g == generation) {
            *slot = None;
        }
    }

    /// Nudge the parse stage to bring the session's crash-restore journal up to
    /// date (`crate::crash_restore`): a host resize or settings change that no
    /// output will follow is recorded, or keyframed, now. The lane is the reader's
    /// own channel, so the nudge needs no lane of its own nor a second generation
    /// stamp: it rides the sender this lane already holds, and goes when the reader
    /// does. Never blocks; `false` when no reader is attached (its next hold catches
    /// up, and the header word the caller stored meanwhile counts what a crash would
    /// miss) or its queue is full (a batch is queued, whose hold catches up first).
    #[cfg(unix)]
    pub(crate) fn nudge(&self) -> bool {
        let slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
        slot.as_ref()
            .is_some_and(|(_, tx)| tx.try_send(crate::spawn::GatherMsg::Journal).is_ok())
    }

    /// Offer `req` to the parse stage without blocking (the main thread calls
    /// this for the menu).
    pub(crate) fn offer(&self, req: &ManualReset) -> LaneOffer {
        #[cfg(unix)]
        {
            let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
            let Some((_, tx)) = slot.as_ref() else {
                return LaneOffer::Closed;
            };
            match tx.try_send(crate::spawn::GatherMsg::Reset(req.clone())) {
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

/// Closes a [`ResetLane`] generation when dropped; held by the gather thread so the lane's
/// sender goes on every exit path, unwinding included.
#[cfg(unix)]
pub(crate) struct LaneGuard {
    pub(crate) lane: Arc<ResetLane>,
    pub(crate) generation: u64,
}

#[cfg(unix)]
impl Drop for LaneGuard {
    fn drop(&mut self) {
        self.lane.close_if(self.generation);
    }
}

/// The `reset [flush]` control verb against one session.
///
/// Reply: `OK reset reverted=<csv|-> bytes=<n>`, plus with `flush` ` discarded=<n>` (the bytes
/// FIONREAD reported plus any aterm still held behind a full queue) or ` discarded=-` where
/// nothing could be flushed ([`discarded_field`]). The flush also drops a partial line typed
/// under canonical mode, which FIONREAD does not count. [`RESET_BUSY`] when an update was
/// committing the session for the whole wait. A held session never gets here: `reset` is in
/// the halt set (`fabric::is_pty_reaching`); the menu row, which no halt gates, is the
/// human's escape hatch under a hold.
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
        // Unreachable with a `wait`; answered rather than panicking.
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
/// ([`aterm_pty::input_queue_len`]), and for a session with no tty. Then
/// `SinkWriter::discard_unread_input` drops nothing, not even the sink's spill.
///
/// Labelled [`aterm_session::sink::Discard::Flush`] so the input watch never reads it as
/// `signal term`'s pre-signal drop: a published stall is held through the flush with its line
/// and remedy unchanged ([`crate::input_stall::Restart`]). The watch is woken after the drop,
/// as `signal` wakes it, so the hold dates from the flush, not from the next recheck
/// ([`crate::input_stall::RECHECK`]).
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
/// flushed. `0` ("flushed, nothing was queued") and `-` ("not flushed") are different facts,
/// as with `reverted=-`.
fn discarded_field(discarded: Option<usize>) -> String {
    match discarded {
        Some(n) => format!(" discarded={n}"),
        None => " discarded=-".to_string(),
    }
}

impl crate::App {
    /// Edit ▸ Reset Terminal on `wid`'s focused session: queued on its PTY reader and not
    /// waited for (the main thread never blocks on a reader); with no reader it runs here.
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
    /// `source=menu`. Negative control: the stuck modes are in force before the click.
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
    /// gets. Negative control: `unwrap_or(0)` fails the first assertion.
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
    /// reset runs directly rather than timing out as "stays queued". Negative control: keeping the
    /// caller's copy of the reply sender alive past the offer makes this time out.
    #[test]
    #[cfg(unix)]
    fn a_reader_that_ends_without_answering_runs_the_reset_directly() {
        use std::time::Duration;
        let s = crate::stub_session(7);
        let (tx, rx) = std::sync::mpsc::sync_channel::<crate::spawn::GatherMsg>(4);
        let generation = s.ctx.reset_lane.open(tx);
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
                Some(Duration::from_secs(4)),
            )
        });
        let msg = rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the reset was queued on the lane");
        assert!(matches!(msg, crate::spawn::GatherMsg::Reset(_)));
        // The "reader" ends without answering.
        s.ctx.reset_lane.close_if(generation);
        drop(msg);
        drop(rx);
        // Answered by the disconnect: a timeout would be an `Err`, which the expect refuses.
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
