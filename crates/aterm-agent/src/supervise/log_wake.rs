// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The loop waits on the session log.
//!
//! While a read has shown a turn running and the host's session log holds that turn open, the
//! loop waits for the turn's end or a box with
//! `await log close=turn,need,kind=agent since=<log_seq> ep=<log_ep> base=<log_base>`
//! ([`LOG_WAITS`]; `base=` only from a host whose `status` prints `log_base=`, so the wait names
//! the log its cursor is of), sent over the wire (aterm-agent links no `aterm-slog`). The cursor and the open turn come from one
//! `status` reply ([`LogHead::of_status`]), printed under one hold of the log's lock, so any
//! later record is past the cursor and answers the wait. A session's first busy wait, and the
//! first after a reconnect, read that `status` themselves, so the first turn waits on the log too.
//!
//! The log only wakes the loop: the screen read after it decides every press, and a turn the log
//! closed while the screen still shows it running (a transcript can end a turn before its last
//! frame is drawn) is read as running.
//!
//! Every other wait is the screen's own `await gone <busy row>`. A log wait would sleep its whole
//! step, its record already behind the cursor, from a head that names an open need or whose
//! reader word owes no record (`agent=` neither `busy` nor `idle`), and after a look that started
//! on `await gone` (a press the box did not take, say). A host whose `status` prints no
//! `log_seq=` ([`Caps::log`] follows each `status` read), or that refuses the wait (an older
//! build answers the comma list with `ERR usage`; [`Caps::await_log`] latches until a
//! reconnect), is waited on the screen.
//!
//! The log wait carries the session's word (`autopilot=<word>`) as the screen's waits inside the
//! turn do ([`Session::busy_wait`]), so a switch made while the agent works is noted at once
//! ([`LogWoke::Word`]) and the point the turn ends on is decided on the word read then. A host
//! that refuses the token as a usage error gets the wait bare ([`Caps::await_log_word`]).

use super::*;

/// What the loop's log wait lists: a turn's close, a need raised (a box, a question, a wall, a
/// stall, a run's failure, the session's close), or an `agent` record (the screen reader's word
/// moving). The `agent` record wakes the loop at most a sampling period (250 ms) after the frame
/// that ends a turn, where the log's own close of it waits a further settle (1 s).
pub(super) const LOG_WAITS: &str = "close=turn,need,kind=agent";

/// How many words a `status` reply prints for the session log, from `log_seq=` on:
/// `log_seq= log_ep= log_base= run= turn= step= need= need_since_ms=` (an older host prints no
/// `log_base=`, and the window then ends one word past its own).
const LOG_FIELDS: usize = 8;

/// Where a host's session log stands, as one `status` reply prints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LogHead {
    /// The newest record a reader is shown (`log_seq=`).
    pub(crate) seq: u64,
    /// The log's epoch (`log_ep=`), which a cursor carries.
    pub(crate) ep: u32,
    /// The log's identity (`log_base=`), which the wait names (`base=`) so a cursor of another
    /// log is answered as a reset; `None` from an older host, whose wait names none.
    pub(crate) base: Option<u32>,
    /// Whether the log holds a turn open (`turn=<t>`; `turn=-` holds none).
    pub(crate) turn_open: bool,
    /// Whether the log holds a need open (`need=<word>`; `need=-` holds none): a box, a
    /// question, a wall or a stall, recorded at or before `seq`.
    pub(crate) need_open: bool,
    /// Whether the screen reader's word, printed under the same hold (`agent=`), owes the log
    /// a record: `busy` (any move off it is an `agent` record) or `idle` (the turn's close, a
    /// settle on). Any other word, or none, owes nothing the wait could wake for.
    pub(crate) reader_owes: bool,
}

impl LogHead {
    /// The head a `status` reply prints, read from its `log_seq=` word and the words the host
    /// prints with it (never another field that happens to share a key). Returns `None` when
    /// the reply has no session log (an older host) or a log field does not parse. A missing
    /// `turn=` reads as no turn open.
    pub(crate) fn of_status(stdout: &str) -> Option<LogHead> {
        let line = stdout.lines().find(|l| crate::is_ok_reply(l))?;
        let words: Vec<&str> = line.split_whitespace().collect();
        let at = words.iter().position(|w| w.starts_with("log_seq="))?;
        let fields = &words[at..words.len().min(at + LOG_FIELDS)];
        let field = |key: &str| {
            fields
                .iter()
                .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
        };
        let open = |key: &str| field(key).is_some_and(|v| !v.is_empty() && v != "-");
        let base = match field("log_base") {
            Some(v) => Some(crate::parse_uint(v)?),
            None => None,
        };
        Some(LogHead {
            seq: crate::parse_uint(field("log_seq")?)?,
            ep: crate::parse_uint(field("log_ep")?)?,
            base,
            turn_open: open("turn"),
            need_open: open("need"),
            reader_owes: matches!(
                crate::supervise::screen::status_field(stdout, "agent"),
                Some("busy" | "idle")
            ),
        })
    }

    /// Whether a wait from this head can still wake for what it waits on: a turn open, no
    /// need open, and a reader word that owes a record (module docs).
    pub(crate) const fn owes_a_wake(&self) -> bool {
        self.turn_open && !self.need_open && self.reader_owes
    }
}

/// What one log wait came to ([`Session::log_wake`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LogWoke {
    /// The log cannot serve this wait: the host keeps no log, refused the wait, holds no
    /// turn open, already shows what the wait would wake for, or says the session is gone.
    /// The screen's own wait serves it.
    Screen,
    /// The log answered: a turn closed, a need was raised, or a gap was crossed. The loop
    /// reads the screen, and the next wait asks `status` again.
    Answered,
    /// The step ran out with nothing past the cursor. The loop reads the screen, and the
    /// cursor stands for the next wait.
    TimedOut,
    /// The session's word moved to this one (`OK autopilot <word>`): the loop notes the switch
    /// ([`Session::word_moved`]) and reads the screen, and the cursor stands for the next wait,
    /// which carries the word reported.
    Word(Option<Autopilot>),
}

impl<C: Ctl> Session<'_, C> {
    /// One wait on the log for a running turn's end or a box, at most `step`. It starts from
    /// `at`, the cursor the last wait left standing, or else from the head a fresh `status`
    /// prints when that head still owes the wake ([`LogHead::owes_a_wake`]). `at` is left set
    /// only after a timeout.
    ///
    /// # Errors
    /// [`Fail::Lost`] when the host did not serve the request; [`Fail::Hard`] from
    /// [`Self::call`] (a stopped loop begins no wait).
    pub(super) fn log_wake(
        &mut self,
        at: &mut Option<LogHead>,
        step: Duration,
    ) -> Result<LogWoke, Fail> {
        if self.caps.log == Some(false) || self.caps.await_log == Some(false) {
            *at = None;
            return Ok(LogWoke::Screen);
        }
        let head = match at.take() {
            Some(head) => head,
            None => {
                let r = self.call(&["status"])?;
                if self.unserved(&r) {
                    return Err(Fail::Lost(format!("status failed: {}", r.stderr.trim())));
                }
                if !r.ok() {
                    // A host that refuses `status` is read as keeping no log until an OK one
                    // says otherwise ([`Self::note_status`]): no `status` at every wait.
                    if self.caps.log.is_none() && !r.is_err("exited") {
                        self.caps.log = Some(false);
                    }
                    return Ok(LogWoke::Screen);
                }
                match LogHead::of_status(&r.stdout) {
                    Some(head) if head.owes_a_wake() => head,
                    _ => return Ok(LogWoke::Screen),
                }
            }
        };
        let since = format!("since={}", head.seq);
        let ep = format!("ep={}", head.ep);
        let base = head.base.map(|b| format!("base={b}"));
        let ms = step.as_nanos().div_ceil(1_000_000).to_string();
        let mut args = vec!["await", "log", LOG_WAITS, since.as_str(), ep.as_str()];
        args.extend(base.as_deref());
        args.extend(["timeout", ms.as_str()]);
        let word = match self.caps.await_log_word {
            Some(false) => None,
            _ => self.word_latch()?,
        };
        args.extend(word.as_deref());
        let mut r = self.call(&args)?;
        let mut carried = word.is_some();
        if carried && r.usage_error() {
            args.pop();
            let bare = self.call(&args)?;
            if !bare.usage_error() {
                self.caps.await_log_word = Some(false);
            }
            (r, carried) = (bare, false);
        }
        let mut reply = r.stdout.split_whitespace().skip(1);
        if r.ok() && carried && reply.next() == Some("autopilot") {
            let stored = reply.next().unwrap_or("-");
            let read = Autopilot::ALL.into_iter().find(|a| a.word() == stored);
            if read.is_none() && stored != "-" {
                // A word this build cannot name would wake every wait carrying the old one.
                self.caps.await_autopilot = Some(false);
                return Ok(LogWoke::Answered);
            }
            *at = Some(head);
            Ok(LogWoke::Word(read))
        } else if r.ok() {
            Ok(LogWoke::Answered)
        } else if self.unserved(&r) {
            Err(Fail::Lost(format!(
                "await log {LOG_WAITS} {since} {ep} failed: {}",
                r.stderr.trim()
            )))
        } else if r.timed_out() {
            *at = Some(head);
            Ok(LogWoke::TimedOut)
        } else {
            if !r.is_err("exited") {
                self.caps.await_log = Some(false);
            }
            Ok(LogWoke::Screen)
        }
    }
}

#[cfg(test)]
#[path = "log_wake_tests.rs"]
mod tests;
