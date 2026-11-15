// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A TURN WORKED TO A BREAK OF THE AGENT'S OWN BACKGROUND WORK IS ITS LOGIN SERVING
//! ([`Session::break_login`]), watched apart from Cruise's carries
//! ([`super::cruise_carry_loop::CarryWatch`]) and told whatever the Autopilot, as a point's login
//! is ([`IdleHost::login_seen`]): a break reaches no point
//! ([`super::super::policy::turn_end::TurnEndState::login_seen`] reads points alone), so before
//! this a session whose every turn ended while its background work ran told its login's return
//! never, and a row a wall recorded signed out stayed so while its own process served it (m2,
//! 2026-11-13).

use std::time::Instant;

use super::super::policy::turn_end::LoginSeen;
use super::*;

/// How far before a break's turn opened (rounded to the second) its own newest reply may be
/// stamped in Claude Code's transcript ([`Session::break_login`]): the turn's opening is rounded
/// to the nearest second and a row's stamp is read to its whole second, so a reply written as the
/// turn opened can read up to 1.5 s before it.
const BREAK_ROW_SLACK_S: i64 = 2;

/// The host's door the watch tells ([`IdleHost::login_served_at`]): a tape recorded before it
/// replays as recorded, with no clock read for the watch.
const DOOR: &str = "host.login_served_at";

/// The turn a settled break of the agent's own background work tells the login of: read on every
/// busy read whatever the Autopilot, where Cruise's carries are stepped only under an Autopilot
/// that is on ([`Session::background_steps`]).
#[derive(Debug, Default)]
pub(super) struct BreakLogin {
    /// When the turn read live since the last settled break or judged point opened: its first
    /// live read ([`Session::took`]).
    opened: Option<Instant>,
    /// When the break read now began: its first read at the break, until a live read of the turn
    /// or a read that is no such break.
    began: Option<Instant>,
}

impl BreakLogin {
    /// A live read of a running turn, at `at` (read only where it opens one): the first since
    /// the last settled break or judged point opens the turn, and the break read before it was a
    /// pause inside it.
    fn live(&mut self, at: impl FnOnce() -> Instant) {
        if self.opened.is_none() {
            self.opened = Some(at());
        }
        self.began = None;
    }

    /// A judged point: the point tells the login of the turn it ends
    /// ([`Session::account_tells`]), so no break tells it again.
    pub(super) fn point(&mut self) {
        self.opened = None;
        self.began = None;
    }
}

impl<C: Ctl> Session<'_, C> {
    /// A live read of a running turn ([`Session::took`]), for its login's watch at a break
    /// ([`BreakLogin::live`]).
    pub(super) fn break_login_live(&mut self) {
        if self.fx.replay_lacks_host_door(DOOR) {
            return;
        }
        self.break_login.live(|| self.fx.mono());
    }

    /// A BUSY READ'S STEPS AT A BREAK OF THE AGENT'S OWN BACKGROUND WORK: the login a turn
    /// worked to it served, told whatever the Autopilot ([`Self::break_login`]), as a point's is,
    /// then the host's, the door's and Cruise's steps ([`Self::host_steps_in_background`]) only
    /// where the session is not `off` (unattended under Off, which types nothing).
    pub(super) fn background_steps(
        &mut self,
        screen: &Screen,
        since: &mut Option<Instant>,
        off: bool,
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        self.break_login(screen);
        if off {
            return Ok(());
        }
        self.host_steps_in_background(screen, since, cfg, review)
    }

    /// THE LOGIN A TURN WORKED TO A SETTLED BREAK OF CLAUDE CODE'S OWN BACKGROUND WORK SERVED,
    /// told the host once a turn ([`IdleHost::login_served_at`]): where `screen` is such a break
    /// (a busy read under a background wait, or the transcript's own wait, with no wall drawn),
    /// it has stood for the break's settle ([`SessionSetup::background_settle`]) and a turn read
    /// live opened since the last settled break or judged point ([`BreakLogin`]), and only where
    /// Claude Code's transcript shows the API last answered this process with the model's own
    /// reply, written since the process started and since that turn opened
    /// ([`super::wall_confirm::served_since`], [`BREAK_ROW_SLACK_S`]). The reply's own stamp
    /// dates the evidence, never the settle's read some time after it, so the host lifts no
    /// strike recorded between the two. A break no turn worked to, one the page or a notice
    /// answered last, and a session of another program tell nothing.
    fn break_login(&mut self, screen: &Screen) {
        if self.break_login.opened.is_none()
            || self.stall_host.is_none()
            || self.login_fact_told == Some(LoginSeen::Served)
            || self.fx.replay_lacks_host_door(DOOR)
        {
            return;
        }
        let reader = self.reader(&screen.rows);
        match reader.program() {
            Program::Claude => {}
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic => return,
        }
        let runs = reader.runs(screen.title.as_deref());
        let carried = self.transcript_wait_seq == Some(screen.seq);
        let at_break = (carried
            || (phase_of(reader, screen) == Phase::Busy
                && reader.background_wait_seen(&screen.rows, runs).is_some()))
            && reader.wall(&screen.rows).is_none();
        if !at_break {
            self.break_login.began = None;
            return;
        }
        let now = self.fx.mono();
        let began = *self.break_login.began.get_or_insert(now);
        if now.saturating_duration_since(began) < self.setup.background_settle {
            return;
        }
        let (Some(opened), Some(host)) = (self.break_login.opened.take(), self.stall_host.clone())
        else {
            return;
        };
        let turn_opened = self.unix_of(opened).saturating_sub(BREAK_ROW_SLACK_S);
        let served = host.agent_started().and_then(|started| {
            host.transcript_tail()
                .and_then(|tail| super::wall_confirm::served_since(&tail, started.max(turn_opened)))
        });
        if let Some(at) = served {
            host.login_served_at(at);
            self.login_fact_told = Some(LoginSeen::Served);
        }
    }
}
