// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MANUAL RESET (2026-09-26): the escape hatch for a terminal the
//! automatic foreground handback cannot reach — the `reset` control verb
//! (`aterm ctl @<sid> reset [flush]`) and the Edit ▸ Reset Terminal menu row
//! (`invoke ResetTerminal`).
//!
//! # Why
//!
//! The foreground handback (2026-09-25, `crate::foreground_handback`) hands the
//! terminal back only at a foreground change whose old holder is GONE and
//! owned an input-hijacking mode. The robustness audit of 2026-09-26 found a
//! live session outside that gate for good: window pid 6874, sid 0 at
//! `~/publication`, idle about 97 000 s, a zsh prompt under `alt_screen=true
//! mouse_mode=any mouse_encoding=sgr kitty_keyboard=disambiguate,…
//! modify_other_keys=2`, with `997;1n997;2n` colour-scheme reports typed into
//! it. The modes were armed before the handback existed, so the only holder
//! its reader ever saw was the live zsh — never gone — and nothing could
//! hand them back: every mouse move typed an SGR report at the prompt and zle
//! rang the bell at each CSI-u chord, forever. The same holds for a shifted
//! charset, an OSC 8 link a killed program left open, display modes a
//! SIGKILLed `less` left, and any foreground change the sampler missed. The
//! backlog item (robustness backlog #9, 2026-09-26) is this module.
//!
//! # What
//!
//! [`aterm_core::terminal::Terminal::manual_handback`]: the handback's own
//! byte plan without its evidence gate (a torn sequence gets `CAN`; every
//! program-negotiable mode goes back to its host target; an open OSC 8 link
//! is closed), never clearing the screen or the scrollback.
//!
//! # Where it runs — in the byte stream, like the handback
//!
//! The reset runs ON THE PARSE STAGE of the session's PTY reader, as one more
//! message on the gather→parse channel ([`ResetLane`]). That is what makes it
//! the handback's twin rather than a sweep: it lands between two PTY batches,
//! never inside one; its synthesized bytes are recorded on the temporal spine
//! as `RawIn` under the same term-lock hold that processed them, and ride the
//! cast/`bytes` taps in the same order, so a replay of either recording
//! reaches the live state; and the `modes-restored` timeline event it records
//! (`reason=manual source=<ctl|menu> reverted=<csv|-> bytes=<n>`) is written
//! by the one thread that writes the automatic ones. A reset issued from the
//! control thread under its own term lock would have been processed in
//! engine order but recorded out of order against the reader's cast tap.
//!
//! Where no reader runs (Windows, whose single-thread reader has no parse
//! channel; a session whose reader has exited) the reset runs DIRECTLY under
//! the term lock on the calling thread and records the timeline event only —
//! there is no stream left to order it against, or no parse stage to carry it.
//!
//! `flush` (the verb only) then drops the tty input queue — the kernel's and
//! whatever aterm still holds behind it (`SinkWriter::discard_unread_input`,
//! the `signal term` remedy's discard, labelled `Discard::Flush` so it is
//! never taken for that remedy's restart signal) — AFTER the reset, so mouse reports
//! and CSI-u chords the stuck modes generated up to that moment go with it.
//! macOS ONLY: the discard needs the slave's input-queue reading
//! (`aterm_pty::input_queue_len`), which is measured on Darwin alone (a Linux
//! master's FIONREAD counts the other direction; the `TIOCGPTPEER` reading is
//! not built). Elsewhere nothing is flushed and the reply says `discarded=-`.

use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, SyncSender, TrySendError};
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

/// How long the verb waits for the parse stage. A reader that does not answer
/// in this long is parked (an update handoff) or wedged; the request stays
/// queued and runs when it resumes, and the reply says so.
pub(crate) const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// The `modes-restored` payload of a manual reset:
/// `reason=manual source=<ctl|menu> reverted=<csv|-> bytes=<n>`. An automatic
/// handback's payload starts `from=`; `reason=manual` is how a reader of the
/// timeline tells the two apart.
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
    timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .record("modes-restored", payload.clone());
    aterm_log::info!("session {sid}: manual reset: {payload}");
}

/// The reset where no parse stage can carry it: under the term lock on this
/// thread, then the timeline (see the module docs).
pub(crate) fn run_direct(
    sid: u64,
    term: &std::sync::Mutex<Terminal>,
    ctx: &SessionCtx,
    source: ResetSource,
) -> ResetReport {
    let (report, _) = {
        let mut t = crate::term_lock(term);
        apply(&mut t)
    };
    record(sid, &ctx.timeline, source, &report);
    report
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
    // The queued message holds the only reply sender from here on, so a
    // reader that ends without answering DISCONNECTS the wait below instead
    // of leaving it to time out.
    drop(req);
    match offer {
        LaneOffer::Queued => {}
        LaneOffer::Busy => {
            return Err(
                "ERR busy reset (this session's reader queue is full; retry)\n".to_string(),
            );
        }
        LaneOffer::Closed => return Ok(Some(run_direct(sid, term, ctx, source))),
    }
    let (Some(rx), Some(timeout)) = (rx, wait) else {
        return Ok(None);
    };
    match rx.recv_timeout(timeout) {
        Ok(report) => Ok(Some(report)),
        Err(RecvTimeoutError::Timeout) => Err(format!(
            "ERR reset timed out after {} ms (the session's reader is parked or busy; the reset \
             stays queued and runs if that reader resumes)\n",
            timeout.as_millis()
        )),
        // The reader exited with the request unanswered: no stream is left to
        // order against, so run it here.
        Err(RecvTimeoutError::Disconnected) => Ok(Some(run_direct(sid, term, ctx, source))),
    }
}

/// What [`ResetLane::offer`] did with a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaneOffer {
    /// On the parse stage's channel.
    Queued,
    /// The channel is full (it holds the gather's batches too): refused.
    Busy,
    /// No parse stage takes requests: run it directly.
    Closed,
}

/// The session's way onto its PTY reader's parse stage: a clone of the
/// gather→parse sender, installed by each reader when it starts and removed
/// when it ends (either stage, whichever ends first — see
/// [`Self::close_if`]). Lives on [`SessionCtx`], which the control thread
/// and the main thread both hold.
///
/// It never keeps the channel open on its own account: a parse stage waits on
/// `recv()` with no timeout, relying on the gather to end the channel, so the
/// lane's clone is dropped by a guard on the gather's exit path (unwinding
/// included). The generation stamp keeps a late-exiting old reader from
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

/// Closes a [`ResetLane`] generation when dropped — held by the gather thread
/// so the lane's sender goes with it on every exit path, unwinding included.
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
/// Reply: `OK reset reverted=<csv|-> bytes=<n>`, plus ` discarded=<n>` with
/// `flush`: the bytes the kernel reported readable (FIONREAD) plus any aterm
/// still held behind a full queue — or ` discarded=-` where nothing could be
/// flushed ([`discarded_field`]: every platform but macOS, and a session with
/// no tty), never a `0` that would read as an empty queue. The flush also drops a PARTIAL line typed
/// under canonical mode, which FIONREAD does not report — measured on the
/// live headless try of 2026-09-26: `junkjunk` typed during `sleep 4` answered
/// `discarded=0` and never reached zle, where the same run without `flush`
/// put `junkjunk` on the next command line. A HELD session
/// never gets here: `reset` is in the halt set (`fabric::is_pty_reaching`), so
/// the dispatch answers `ERR halted …` first — the human's escape hatch under a
/// hold is the menu row, which no halt gates.
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
        // `request` with a `wait` always answers a report; keep the verb
        // total without a panic path all the same.
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

/// `flush`'s discard: how many unread input bytes went, or `None` when
/// nothing could be flushed — every platform but macOS, where aterm cannot
/// measure the slave's input queue ([`aterm_pty::input_queue_len`]), and a
/// session with no tty behind it. [`aterm_session::sink::SinkWriter::
/// discard_unread_input`] drops nothing at all in that case, not even the
/// sink's own spill.
///
/// A FLUSH, NOT A RESTART (robustness review of the manual reset,
/// 2026-09-26): the discard says so ([`aterm_session::sink::Discard::Flush`]).
/// It used to be the restart remedy's discard, unlabelled, and on a session
/// whose program was published `input=stalled` the input watch read it as
/// `signal term`'s pre-signal drop — five seconds later the server's line
/// read "… is still running after its restart signal … end it: … signal
/// kill" and every input verb was refused naming that signal, though no
/// signal was ever sent (reproduced on a headless instance: `discarded=433`,
/// then event 6 of the timeline). Now the stall is HELD through the flush
/// (the dropped queue is not a read) with its frozen line and its `signal
/// term` remedy unchanged ([`crate::input_stall::Restart`]). The watch is
/// woken after the drop, as `signal` wakes it after its own, so the hold
/// dates from the flush rather than from the watch's next recheck
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

/// The ` discarded=` field of a `reset flush` reply: the count, or `-` when
/// NOTHING WAS FLUSHED.
///
/// WHY `-` AND NOT `0` (robustness review of the manual reset, 2026-09-26):
/// the first cut wrote `discard_unread_input().unwrap_or(0)`, so on Linux and
/// Windows — where the discard returns `None` before it touches the spill or
/// calls `tcflush`, because the queue is macOS-measured only — `reset flush`
/// flushed nothing and still answered `discarded=0`: a successful flush that
/// found an empty queue, to anyone reading it. The mouse reports and CSI-u
/// chords the stuck modes queued were still there for the shell to read. `0`
/// and `-` are different facts ("flushed, nothing was queued" against "not
/// flushed"), the same distinction `reverted=-` draws, so the reply keeps
/// them apart. Pinned by `reset_flush_that_flushes_nothing_says_so`.
fn discarded_field(discarded: Option<usize>) -> String {
    match discarded {
        Some(n) => format!(" discarded={n}"),
        None => " discarded=-".to_string(),
    }
}

impl crate::App {
    /// Edit ▸ Reset Terminal on `wid`'s focused session: queued on that
    /// session's PTY reader and NOT waited for — the main thread never blocks
    /// on a reader. The reader records `modes-restored reason=manual
    /// source=menu` and posts the output wake that repaints the window; with
    /// no reader to carry it the reset runs here, directly.
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

    /// Edit ▸ Reset Terminal: enabled over a terminal tab, reachable by its
    /// `invoke` name at the `WriteInput` class, and — clicked on a session with
    /// no reader (the stub; the fallback path) — it resets the FOCUSED
    /// session and records `source=menu`. NEGATIVE CONTROL: the same stuck
    /// modes are still in force before the click.
    #[test]
    fn reset_terminal_menu_row_resets_the_focused_session() {
        let _statics = crate::menu::MENU_STATICS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
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
            .map(|e| e.payload.clone())
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

    /// `reset flush` where NOTHING can be flushed answers `discarded=-`, never
    /// `discarded=0` (robustness review of the manual reset, 2026-09-26). The
    /// stub's sink has no tty behind it (`master = -1`), so its discard
    /// returns `None` before touching anything — the same `None` every
    /// session gets on Linux and Windows, where the input queue is not
    /// measured. NEGATIVE CONTROL: the first cut's `unwrap_or(0)` answers
    /// `… discarded=0`, a successful flush of an empty queue, and fails the
    /// first assertion. The `Some` arm still prints the count, `0` included:
    /// "flushed, nothing was queued" stays a distinct, truthful reply.
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

    /// The macOS twin of the test above: over a real raw pty whose slave has
    /// an unread SGR mouse report queued (what the stuck modes type), `reset
    /// flush` drops it and counts it, and the queue reads empty after.
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

    /// A reader that ends with a reset queued and unanswered (the parse stage
    /// left its loop — a park for the update handoff, a session closing) must
    /// not make the verb wait out [`super::REPLY_TIMEOUT`] and then claim the
    /// reset "stays queued": the queued message dies with the channel, the
    /// wait DISCONNECTS, and the reset runs directly. NEGATIVE CONTROL: with
    /// the caller's own copy of the reply sender kept alive past the offer
    /// (the first cut), the wait never disconnects and this answers `Err` after
    /// the full timeout.
    #[test]
    #[cfg(unix)]
    fn a_reader_that_ends_without_answering_runs_the_reset_directly() {
        use std::time::{Duration, Instant};
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
            .recv_timeout(Duration::from_secs(10))
            .expect("the reset was queued on the lane");
        assert!(matches!(msg, crate::spawn::GatherMsg::Reset(_)));
        // The "reader" ends without answering.
        s.ctx.reset_lane.close_if(generation);
        let started = Instant::now();
        drop(msg);
        drop(rx);
        let report = asker
            .join()
            .expect("the asker thread")
            .expect("a report, not a timeout")
            .expect("waited for");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "answered by the disconnect, not the timeout: {:?}",
            started.elapsed()
        );
        assert_eq!(report.reverted, vec!["mouse"]);
        assert!(
            !s.term
                .lock()
                .expect("terminal lock")
                .program_owns_terminal()
        );
    }
}
