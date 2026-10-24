// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ACT BARRIER: a harness act a session's program records over the control socket
//! (`meta set harness_act …`) that reads the session's counts (`control_session::
//! harness_act_reads_counts`) reads them only once the session's parser holds every byte the
//! program wrote to its PTY before it. An act whose barrier is not answered within [`PATIENCE`],
//! or whose reader stops first, is read as it stands, as before the barrier.
//!
//! A program writes its turn's end marks (OSC 7501) before it sends the act that closes the turn
//! (codex-alab's landing waits for its screen to take the turn's end). On a loaded Mac aterm's
//! reader can still hold those bytes unparsed in the kernel's queue, in the gather's batch or in
//! the channel to the parse stage while a control lane takes the act's mark; parsed after it, the
//! turn's own end counted as a turn past the mark and reopened a finished landing.
//!
//! The control lane asks ([`ResetLane::pass`](crate::manual_reset::ResetLane::pass)) and pokes
//! the gather (`spawn::spawn_pty_gather`). At the top of its loop, where every byte it has read
//! has gone out, the gather reads the master's queue (`TIOCOUTQ`) and owes the barrier once that
//! many more bytes went out behind it ([`Countdown`]). The barrier then rides the parse stage's
//! channel behind those bytes, and the parse stage, which ingests each batch whole before its next
//! message, answers it. This is the update park's catch-up (`spawn::GatherMsg::CaughtUp`), asked
//! for per act.
//!
//! On a unix other than macOS the master's queue cannot be read, so a barrier there covers what the
//! gather had read when it took the ask, not what the kernel still held. A non-unix reader takes no
//! barrier, and with no live reader, or no poke pipe, an act goes through unbarriered, as before
//! the barrier.

#[cfg(unix)]
use std::sync::mpsc::SyncSender;
use std::time::Duration;

/// How long an act waits for its barrier before it is read without one, as before the barrier.
///
/// codex-alab gives a control call 5 s (alab-aterm's `CALL_TIMEOUT`), counted from when the call
/// queued behind the ones before it on its connection, and past it drops the connection that may
/// hold its claim. So only the first act a lagging parser leaves unanswered waits this long: the
/// session's acts after it are read at once ([`Passage::Behind`]) until that parser answers a
/// barrier again, and a run of acts costs one patience, not one each. A healthy parser answers in
/// milliseconds.
#[cfg(not(test))]
pub(crate) const PATIENCE: Duration = Duration::from_secs(2);
/// A test's patience is a hang detector, never the 2 s an act waits: a loaded test host can parse
/// slower than that without anything being wrong.
#[cfg(test)]
pub(crate) const PATIENCE: Duration = aterm_spec::wait::MUST_HAPPEN;

/// What [`ResetLane::pass`](crate::manual_reset::ResetLane::pass) saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "only a unix reader answers a barrier")
)]
pub(crate) enum Passage {
    /// The parser holds every byte the program wrote before the ask (off macOS, every byte the
    /// gather had read when it took the ask).
    Passed,
    /// No live reader with a poke pipe: nothing to wait for.
    Unbarriered,
    /// The reader stopped before it answered.
    Gone,
    /// Not answered within the patience: the session's parser is behind.
    Late,
    /// Not asked: a barrier before it went [`Passage::Late`], and the parser has answered none
    /// since.
    Behind,
}

/// A reader generation's poke: the write end of its gather's poke pipe and who waits on the next
/// barrier. Dropped with its lane, it closes the write end.
#[cfg(unix)]
pub(crate) struct Poke {
    pub(crate) wr: i32,
    pub(crate) asked: Vec<SyncSender<()>>,
}

#[cfg(unix)]
impl Drop for Poke {
    fn drop(&mut self) {
        aterm_pty::close_fd(self.wr);
    }
}

/// The gather's barriers: each owed until the bytes the master held when it was asked have gone
/// out to the parse stage.
#[cfg(unix)]
#[derive(Default)]
pub(crate) struct Countdown {
    sent: u64,
    owed: Vec<(u64, Vec<SyncSender<()>>)>,
}

#[cfg(unix)]
impl Countdown {
    /// The barriers `asked` for at the top of the gather's loop, with `queued` bytes (`TIOCOUTQ`,
    /// `None` where it cannot be read) unread on the master: those to send now, behind every
    /// batch already sent, or none while the queued bytes are owed first.
    pub(crate) fn ask(
        &mut self,
        asked: Vec<SyncSender<()>>,
        queued: Option<usize>,
    ) -> Option<Vec<SyncSender<()>>> {
        if asked.is_empty() {
            return None;
        }
        match queued {
            Some(queued) if queued > 0 => {
                let target = self.sent.saturating_add(queued as u64);
                self.owed.push((target, asked));
                None
            }
            _ => Some(asked),
        }
    }

    /// The barriers due once a batch of `filled` bytes went out: those whose bytes have all gone,
    /// and every one owed once `drained` (read only while one is owed) says the master holds
    /// nothing, since bytes the kernel discarded never come.
    pub(crate) fn sent(
        &mut self,
        filled: usize,
        drained: impl FnOnce() -> bool,
    ) -> Vec<SyncSender<()>> {
        self.sent = self.sent.saturating_add(filled as u64);
        let mut due = Vec::new();
        if self.owed.is_empty() {
            return due;
        }
        let (sent, all) = (self.sent, drained());
        self.owed.retain_mut(|(target, asked)| {
            let now = all || *target <= sent;
            if now {
                due.append(asked);
            }
            !now
        });
        due
    }
}

/// Wait, holding no lock, until the session's parser holds what its program wrote before this act
/// ([`ResetLane::pass`](crate::manual_reset::ResetLane::pass)); an act that cannot is read without
/// the barrier, and the log says why once per lag.
pub(crate) fn before_act(ctx: &crate::SessionCtx) -> Passage {
    let passage = ctx.reset_lane.pass(PATIENCE);
    match passage {
        Passage::Passed | Passage::Unbarriered | Passage::Behind => {}
        Passage::Gone => aterm_log::info!(
            "act barrier: @{}: the reader stopped before its parser caught up; the act is read as it stands",
            ctx.self_id.as_str()
        ),
        Passage::Late => aterm_log::warn!(
            "act barrier: @{}: the parser did not catch up within {} ms; this act, and the session's acts until it does, are read as they stand",
            ctx.self_id.as_str(),
            PATIENCE.as_millis()
        ),
    }
    passage
}

#[cfg(all(test, unix))]
mod tests {
    use super::Countdown;

    fn waiter() -> (
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::Receiver<()>,
    ) {
        std::sync::mpsc::sync_channel(1)
    }

    #[test]
    fn a_barrier_asked_over_an_empty_queue_goes_at_once() {
        let mut countdown = Countdown::default();
        let (tx, _rx) = waiter();
        assert_eq!(countdown.ask(vec![tx], Some(0)).map(|v| v.len()), Some(1));
        let (tx, _rx) = waiter();
        assert_eq!(
            countdown.ask(vec![tx], None).map(|v| v.len()),
            Some(1),
            "a queue that cannot be read owes nothing"
        );
        assert!(
            countdown.ask(Vec::new(), Some(9)).is_none(),
            "no ask, no barrier"
        );
    }

    #[test]
    fn a_barrier_waits_for_the_bytes_queued_when_it_was_asked() {
        let mut countdown = Countdown::default();
        assert!(
            countdown
                .sent(100, || unreachable!("nothing owed"))
                .is_empty()
        );
        let (tx, _rx) = waiter();
        assert!(
            countdown.ask(vec![tx], Some(50)).is_none(),
            "50 bytes are owed first"
        );
        assert!(
            countdown.sent(49, || false).is_empty(),
            "one byte is still owed"
        );
        assert_eq!(
            countdown.sent(1, || false).len(),
            1,
            "its bytes have all gone"
        );
        assert!(
            countdown
                .sent(10, || unreachable!("nothing owed"))
                .is_empty()
        );
    }

    #[test]
    fn an_emptied_queue_releases_every_barrier_owed() {
        let mut countdown = Countdown::default();
        let (a, _ra) = waiter();
        let (b, _rb) = waiter();
        assert!(countdown.ask(vec![a], Some(10)).is_none());
        assert!(countdown.ask(vec![b], Some(1_000)).is_none());
        assert_eq!(countdown.sent(10, || false).len(), 1, "the first is due");
        assert_eq!(
            countdown.sent(5, || true).len(),
            1,
            "bytes the kernel dropped never come: an empty master releases the rest"
        );
    }
}
