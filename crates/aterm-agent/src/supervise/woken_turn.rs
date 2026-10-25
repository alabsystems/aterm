// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A TURN THE MAC SLEPT THROUGH IN THE VENDOR'S API RETRY ([`Session::unstick_woken_turn`]).
//!
//! Claude Code retries a request the network failed by itself, backing off up to 32 s between
//! tries, and waits up to 180 s for a first byte. A turn whose request went out while the Mac
//! slept, or in one of the seconds-long dark wakes macOS makes with the lid closed, can sit in
//! that wait long after the Mac woke for real and the network came back: the screen busy, a
//! person's words queued behind it, until someone presses Esc. Measured 2026-10-24: Claude Code
//! 2.1.310's own auto-continue after a weekly limit fired in a 19 s dark wake with the network
//! down, its request sat through two more sleeps, and the person who opened the lid 24 minutes
//! later had to press Esc to be heard.
//!
//! Nothing on the loop's busy path acted on a wake before this: the wake was measured
//! ([`Session::watch_sleep`]) and dropped until the next point. The loop now presses that Esc
//! itself, once per wake, where every one of these holds on the busy read:
//!
//! * a busy read showed the worker's turn running when the loop measured the sleep;
//! * the worker is Claude Code (the only reader of its retry row), the session is unattended,
//!   Autopilot is not off, `retry_api_errors` is on and nothing is being written or signed in;
//! * the status row is Claude Code's API-retry row of a network or no-response class
//!   ([`aterm_phase::api_retry_row`]), never a wait of the vendor's own (a usage limit, a queued
//!   request, the auto-mode check), and where it counts down to its next try, at least
//!   [`WOKEN_SHORTEST_WAIT_S`] are left;
//! * the Mac is in a full wake, not a dark one ([`super::fx::Fx::full_wake`]);
//! * the host measured the API up since the wake ([`Reach::Up`]), and [`WOKEN_MARGIN`] has passed
//!   since, so the vendor's own retry after the wake had its chance;
//! * a no-response row (`No response from the API after 3m · retrying once, waiting up to 10m`)
//!   names the first-byte window of the try that failed and the retry's, never when the retry
//!   now in flight went out. A retry sent once the network was back may be waiting on a gateway
//!   that buffers its stream, which the vendor's longer wait is for, so the loop cuts the row
//!   only where the same row stood at the last busy read before the Mac slept and at every busy
//!   read since: that retry went out before the sleep and crossed it (the vendor's own suspend
//!   watchdog drops such a try only after a sleep longer than half its window). A no-response
//!   row first read since the wake is left to the vendor's wait, however long the API is up;
//! * the composer holds no draft (a queued message under Claude Code's placeholder is none);
//! * the server can fence the press on the person's keys (`if-human=`).
//!
//! The press is guarded on the row itself ([`aterm_phase::ApiRetryRow::guard`]; a no-response
//! row's whole text, so the Esc never lands on a later try's row). Only a press the server took
//! spends the wake's one Esc: a skipped one (the row moved on) or a yielded one (a
//! person typed since the read) is tried again at the next read. Claude Code then sends what was
//! queued itself; with nothing queued it stops at a point the turn-end policy decides as the
//! harness's own Esc, not a person's ([`TurnEndState::woken_esc`]): the vendor's continuation after
//! a usage limit gets the limit's resume, any other turn what the policy types at its end. Every
//! live read clears that mark ([`TurnEndState::took`]); a busy read that still shows the retry row
//! within [`WOKEN_MARGIN`] of the press sets it again, because Claude Code has not taken the key
//! yet. A person's Esc on a later retry row is a person's again.

use std::time::{Duration, Instant};

use super::super::journal::UNSTUCK;
use super::super::policy::turn_end::{Composer, Reach, TurnEndReading};
use super::*;
use aterm_phase::RetryCause;

/// How long the API must have been measured up before the loop presses Esc: the vendor's own
/// retry fires within a second of the wake, and a try it made with the network back has had
/// this long to answer.
pub(super) const WOKEN_MARGIN: Duration = Duration::from_secs(10);

/// The shortest countdown to the vendor's next try that the Esc cuts short: a shorter one the
/// vendor takes itself.
pub(super) const WOKEN_SHORTEST_WAIT_S: u64 = 5;

/// A wake the loop measured while the worker's turn ran ([`Session::watch_sleep`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WokenTurn {
    /// The sleep the loop measured: its wall clock (unix ms) before and after, and the loop's own
    /// clock after.
    wall_then: i64,
    wall_now: i64,
    mono_now: Instant,
    /// When the Mac woke, on the loop's clock, once read ([`Session::woken_at`]).
    at: Option<Instant>,
    /// The no-response retry row, as drawn, that the last busy read before the sleep showed, while
    /// every busy read since shows it ([`Session::note_retry_row`]): a try that crossed the sleep.
    carried: Option<String>,
    /// When this wake's one Esc was taken, on the loop's clock.
    pressed_at: Option<Instant>,
    /// The turn it stopped was the vendor's continuation after a usage limit.
    resume: bool,
    /// This wake's WAITING line was said.
    said: bool,
}

impl WokenTurn {
    /// The sleep the loop measured from `wall_then` to `wall_now` (unix ms), at `mono_now`, with
    /// the no-response row the last busy read before it showed.
    pub(super) const fn measured(
        wall_then: i64,
        wall_now: i64,
        mono_now: Instant,
        carried: Option<String>,
    ) -> Self {
        Self {
            wall_then,
            wall_now,
            mono_now,
            at: None,
            carried,
            pressed_at: None,
            resume: false,
            said: false,
        }
    }

    /// Whether this wake's one Esc was taken.
    #[cfg(test)]
    pub(super) const fn pressed(&self) -> bool {
        self.pressed_at.is_some()
    }
}

impl<C: Ctl> Session<'_, C> {
    /// A busy read: keep the no-response retry row it shows ([`Session::retry_seen`]). A
    /// no-response row stands unchanged while its retry waits, so any other row, or none, ends
    /// it, and with it the wake's carried row ([`WokenTurn::carried`]): a later try's row drawn
    /// the same is never taken for the try that crossed the sleep.
    pub(super) fn note_retry_row(&mut self, screen: &Screen) {
        let row = aterm_phase::api_retry_row(&screen.rows)
            .filter(|r| r.cause == RetryCause::NoResponse)
            .map(|r| r.text);
        if row.is_some() && row == self.retry_seen {
            return;
        }
        if let Some(w) = self.woken.as_mut() {
            w.carried = None;
        }
        self.retry_seen = row;
    }

    /// THE WAKE, ON THE LOOP'S CLOCK: the kernel's record of it where that falls inside the sleep
    /// the loop measured, else the moment the loop measured it (later than the wake, so a
    /// measure of the API taken in between never reads as after it).
    fn woken_at(&mut self) -> Option<Instant> {
        let (wall_then, wall_now, mono_now) = match self.woken.as_ref()? {
            WokenTurn { at: Some(at), .. } => return Some(*at),
            w => (w.wall_then, w.wall_now, w.mono_now),
        };
        let at = match self.fx.last_sleep() {
            Some((_, woke)) if wall_then < woke && woke <= wall_now => {
                self.instant_of_ms(woke).min(mono_now)
            }
            _ => mono_now,
        };
        if let Some(w) = self.woken.as_mut() {
            w.at = Some(at);
        }
        Some(at)
    }

    /// A busy read: press Esc on a turn the Mac slept through in the vendor's API retry, where
    /// the module's every condition holds ([module docs](self)).
    pub(super) fn unstick_woken_turn(
        &mut self,
        screen: &Screen,
        reader: &dyn ScreenReader,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let Some(row) = aterm_phase::api_retry_row(&screen.rows) else {
            return Ok(());
        };
        let Some(w) = self.woken.as_ref() else {
            return Ok(());
        };
        let carried = w.carried.as_deref() == Some(row.text.as_str());
        if let Some(at) = w.pressed_at {
            // The Esc's own row still reads live: Claude Code has not taken the key yet, and the
            // point it stops stays the harness's ([`TurnEndState::woken_esc`]).
            let resume = w.resume;
            if self.fx.since(at) < WOKEN_MARGIN {
                self.turn_end.woken_esc(resume);
            }
            return Ok(());
        }
        let claude = match reader.program() {
            Program::Claude => true,
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic => false,
        };
        if !claude || !opts.policy.retry_api_errors || screen.human_seq.is_none() {
            return Ok(());
        }
        let cuts_a_wait = match (row.cause, row.next_try_s) {
            // The row names no time its retry went out: only one that crossed the sleep.
            (RetryCause::NoResponse, _) => carried,
            (RetryCause::Unreachable | RetryCause::Stalled, next) => {
                next.is_some_and(|s| s >= WOKEN_SHORTEST_WAIT_S)
            }
        };
        if !cuts_a_wait || self.fx.full_wake() != Some(true) {
            return Ok(());
        }
        let Some(woke) = self.woken_at() else {
            return Ok(());
        };
        let reach = opts
            .idle_host
            .as_ref()
            .map_or(Reach::Unknown, |h| h.reach());
        let up = match reach {
            Reach::Up { since } if since >= woke => since,
            Reach::Up { .. } | Reach::Down { .. } | Reach::Unknown => {
                self.woken_waiting(screen.seq, &row.text, review);
                return Ok(());
            }
        };
        let up_for = self.fx.since(up);
        if up_for < WOKEN_MARGIN {
            return Ok(());
        }
        let why = match row.cause {
            RetryCause::Unreachable | RetryCause::Stalled => {
                "the API is measured up since the wake"
            }
            RetryCause::NoResponse => {
                "its try crossed the sleep, and the API is measured up since the wake"
            }
        };
        let reading = reader.read(&screen.rows, Some(screen.cursor_col));
        let read = self.composer_read(&reading, screen);
        let composer = TurnEndReading::of(&reading, &screen.rows, read, None, None, None).composer;
        if !matches!(composer, Composer::Empty | Composer::Placeholder(_)) {
            return Ok(());
        }
        match self.guarded_key(screen.seq, &row.guard, "esc", screen.human_seq)? {
            Dismiss::Pressed { seq } => {
                let resume = self.retry.is_some();
                let now = self.fx.mono();
                if let Some(w) = self.woken.as_mut() {
                    w.pressed_at = Some(now);
                    w.resume = resume;
                }
                self.turn_end.woken_esc(resume);
                review.say(&format!(
                    "{UNSTUCK} seq={seq} the turn slept in the vendor's API retry ({}); {why}: Esc",
                    clip(&row.text)
                ))?;
            }
            Dismiss::Skipped | Dismiss::Yielded => {}
            Dismiss::Refused { halted, why } => self.refused(halted, &why, screen.seq, review)?,
        }
        Ok(())
    }

    /// `WAITING seq=<seq> the turn slept in the vendor's API retry (<row>): …`, once a wake.
    fn woken_waiting(&mut self, seq: u64, row: &str, review: &mut dyn Review) {
        let Some(w) = self.woken.as_mut().filter(|w| !w.said) else {
            return;
        };
        w.said = true;
        review.note(&format!(
            "WAITING seq={seq} the turn slept in the vendor's API retry ({}): Esc once the API is \
             measured up since the wake",
            clip(row)
        ));
    }
}
