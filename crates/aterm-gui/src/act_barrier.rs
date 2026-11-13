// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ACT BARRIER: a harness act a session's program records over the control socket
//! (`meta set harness_act …`) that reads the session's counts (`control_session::
//! harness_act_reads_counts`) reads them only once the session's parser holds every byte the
//! program wrote to its PTY before it. An unanswered barrier, a stopped reader or an actual
//! caller disconnect refuses the act before its counts are read or its effects applied.
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

/// Hang detector for a required act barrier. A peer's own connection deadline is
/// unchanged: actual disconnect cancels this wait and admits no act. Every act
/// that reads counts requires its own barrier.
pub(crate) const PATIENCE: Duration = Duration::from_secs(60);

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
    /// The minute hang detector expired without a parser answer.
    Late,
    /// The actual caller disconnected before its act could be admitted.
    Cancelled,
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
/// ([`ResetLane::pass`](crate::manual_reset::ResetLane::pass)). An act whose
/// barrier does not pass is refused, and no later act bypasses its own barrier.
pub(crate) fn before_act(ctx: &crate::SessionCtx) -> Passage {
    let passage = ctx.reset_lane.pass(PATIENCE);
    match passage {
        Passage::Passed | Passage::Unbarriered | Passage::Cancelled => {}
        Passage::Gone => aterm_log::info!(
            "act barrier: @{}: the reader stopped before its parser caught up; the act was not applied",
            ctx.self_id.as_str()
        ),
        Passage::Late => aterm_log::warn!(
            "act barrier: @{}: the parser did not catch up within {} ms; the act was not applied",
            ctx.self_id.as_str(),
            PATIENCE.as_millis()
        ),
    }
    passage
}

#[cfg(all(test, unix))]
#[path = "act_barrier_tests.rs"]
mod tests;
