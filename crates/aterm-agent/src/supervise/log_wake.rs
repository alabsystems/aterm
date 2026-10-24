// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The loop waits on the session log.
//!
//! While a read has shown a turn running and the host's session log holds that turn open, the
//! loop waits for the turn's end or a box with
//! `await log close=turn,need,kind=agent since=<log_seq> ep=<log_ep>` ([`LOG_WAITS`]), sent over
//! the wire (aterm-agent links no `aterm-slog`). The cursor and the open turn come from one
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

use super::*;

/// What the loop's log wait lists: a turn's close, a need raised (a box, a question, a wall, a
/// stall, a run's failure, the session's close), or an `agent` record (the screen reader's word
/// moving). The `agent` record wakes the loop at most a sampling period (250 ms) after the frame
/// that ends a turn, where the log's own close of it waits a further settle (1 s).
pub(super) const LOG_WAITS: &str = "close=turn,need,kind=agent";

/// How many words a `status` reply prints for the session log, from `log_seq=` on:
/// `log_seq= log_ep= run= turn= step= need= need_since_ms=`.
const LOG_FIELDS: usize = 7;

/// Where a host's session log stands, as one `status` reply prints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LogHead {
    /// The newest record a reader is shown (`log_seq=`).
    pub(crate) seq: u64,
    /// The log's epoch (`log_ep=`), which a cursor carries.
    pub(crate) ep: u32,
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
        Some(LogHead {
            seq: field("log_seq")?.parse().ok()?,
            ep: field("log_ep")?.parse().ok()?,
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
        let ms = step.as_nanos().div_ceil(1_000_000).to_string();
        let r = self.call(&["await", "log", LOG_WAITS, &since, &ep, "timeout", &ms])?;
        if r.ok() {
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
mod tests {
    use super::*;

    /// A `status` line as the host prints it: the log's words before the `seq=` stamp.
    const LIVE: &str = "OK schema=1 sid=s-1 phase=running hold=0 program=claude agent=busy \
                        agent_detail=- log_seq=812 log_ep=1760000000 run=r3 turn=t17 step=p204 need=- \
                        need_since_ms=- rollout=- seq=104 hash=0\n";

    /// [`LogHead::of_status`] reads the cursor and the open turn from the log's own words only.
    #[test]
    fn a_status_reply_names_the_logs_cursor_and_its_open_turn() {
        assert_eq!(
            LogHead::of_status(LIVE),
            Some(LogHead {
                seq: 812,
                ep: 1_760_000_000,
                turn_open: true,
                need_open: false,
                reader_owes: true,
            })
        );
        assert!(LogHead::of_status(LIVE).is_some_and(|h| h.owes_a_wake()));
        let rested = LIVE.replace("turn=t17", "turn=-");
        assert_eq!(
            LogHead::of_status(&rested).map(|h| h.turn_open),
            Some(false),
            "{rested}"
        );
        assert_eq!(
            LogHead::of_status("OK schema=1 sid=s-1 phase=running seq=104 hash=0\n"),
            None,
            "an older host's status has no log"
        );
        let foreign = LIVE
            .replace("hold=0", "hold=0 turn=t99")
            .replace("turn=t17", "turn=-");
        assert_eq!(
            LogHead::of_status(&foreign).map(|h| h.turn_open),
            Some(false),
            "a turn= ahead of log_seq= is not the log's: {foreign}"
        );
        assert_eq!(
            LogHead::of_status(&LIVE.replace("log_ep=1760000000", "log_ep=x")),
            None,
            "a cursor with no epoch is none"
        );
        assert_eq!(
            LogHead::of_status(&format!("aterm-ctl: ERR usage\n{LIVE}")).map(|h| h.seq),
            Some(812),
            "the reply's OK line is read"
        );
    }

    /// A head that already shows what the wait would wake for (an open need, or a reader word
    /// that owes no record) owes no wake; `idle` still owes one, and a `need=` ahead of the
    /// log's words is not the log's.
    #[test]
    fn a_head_that_already_names_the_wake_owes_none() {
        let boxed = LIVE.replace("need=-", "need=approval:bash");
        let head = LogHead::of_status(&boxed).expect("a head");
        assert!(head.need_open && !head.owes_a_wake(), "{boxed}");
        for word in ["unknown", "prompt", "wall:unresponsive", "survey", "-"] {
            let read = LIVE.replace("agent=busy", &format!("agent={word}"));
            let head = LogHead::of_status(&read).expect("a head");
            assert!(!head.reader_owes && !head.owes_a_wake(), "{read}");
        }
        let none = LIVE.replace("agent=busy agent_detail=- ", "");
        assert!(
            LogHead::of_status(&none).is_some_and(|h| !h.owes_a_wake()),
            "{none}"
        );
        let idle = LIVE.replace("agent=busy", "agent=idle");
        assert!(
            LogHead::of_status(&idle).is_some_and(|h| h.owes_a_wake()),
            "{idle}"
        );
        let foreign = LIVE.replace("hold=0", "hold=0 need=approval:bash");
        assert!(
            LogHead::of_status(&foreign).is_some_and(|h| h.owes_a_wake()),
            "a need= ahead of log_seq= is not the log's: {foreign}"
        );
    }
}
