// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Persistent memory and stuck-screen canary for rate-limit and health walls.

use super::super::policy::outlook::ExceptionKind;
use super::super::policy::turn_end::{
    Conversation, HostTurn, RULE_MODEL_FALLBACK, ended_under, met_under_another,
    reads_conversation, screen_alone,
};
use super::approvals::{self, Outcome, RULE_WALL_WAIT, RULE_WALL_WAIT_END, WALL_LEFT_NO_TASK};
use super::turn_end_loop::resume_jitter;
use super::*;
use crate::harness::profile::AccountKey;

/// Duration past reset before a remembered wall expires (`24h`).
pub(super) const MEMORY_TTL: Duration = Duration::from_secs(24 * 3600);

const WALL_LEFT: &str = "the wall left the screen or the worker worked";

/// Duration of background-only busy state before firing a stuck-screen canary (`30m`).
pub(super) const STUCK_AFTER: Duration = Duration::from_secs(30 * 60);

const DIALOG_DIVISOR: u32 = 3;

const IDLE_TITLE_GLYPH: char = '\u{2733}';

type WallRowSaid = (
    Option<i64>,
    Option<i64>,
    Option<AccountKey>,
    Conversation,
    Option<Program>,
);

/// Remembered wall state and last-persisted ledger snapshot.
#[derive(Debug, Clone)]
pub(super) struct WallMemory {
    pub(super) wall: aterm_phase::Wall,
    pub(super) conversation: Conversation,
    /// Agent program under which the wall was encountered.
    pub(super) program: Option<Program>,
    /// What was last written to the ledger: `None` before the first write, and again once an act closed the ledger's wall.
    saved: Option<WallRowSaid>,
    /// The wall WAS WRITTEN to the ledger, by this loop
    /// ([`Session::persist_wall`]) or the one it took the wall up from
    /// ([`Session::seed_wall_memory`]).
    written: bool,
}

const fn title_witness(p: Program) -> bool {
    match p {
        Program::Claude => true,
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Generic => false,
    }
}

/// What vouches, at each rung of the canary's ladder
/// ([`Session::stuck_climb`]), that the agent does not work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Witness {
    /// Claude Code's own terminal title rests on `✳` (one `status` read): it
    /// animates while the agent works, and a screen no reader names says
    /// nothing either way.
    Title,
    /// agy's or Muse Code's own words stood still since the episode began
    /// ([`still_words`]): their reader read the turn's end as evidence and
    /// names the work (`what`, [`ScreenReader::background_wait`]); a turn of
    /// the agent's own, a draft typed, or other work moves them.
    Words(&'static str),
}

fn still_words(reader: &dyn ScreenReader, rows: &[String], what: &str) -> String {
    let mut words = format!("{what}\n");
    if let Some((caret, draft)) = reader.composer(rows) {
        push_words_above(&mut words, &rows[..caret.min(rows.len())]);
        words.push_str(&draft.join("\n"));
    }
    words
}

fn background_alone(reader: &dyn ScreenReader, rows: &[String]) -> bool {
    match reader.program() {
        Program::Claude => aterm_phase::background_only(rows),
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Generic => reader.background_wait(rows).is_some(),
    }
}

const fn is_kept(wall: &aterm_phase::Wall) -> bool {
    wall.kind.reads_limited()
}

impl<C: Ctl> Session<'_, C> {
    /// Return the remembered wall when `rows` show only background work or a hidden turn end.
    pub(super) fn remembered_wall(
        &self,
        reader: &dyn ScreenReader,
        rows: &[String],
    ) -> Option<aterm_phase::Wall> {
        let memory = self.wall_memory.as_ref()?;
        if met_under_another(memory.program, reader.program()) {
            return None;
        }
        ((background_alone(reader, rows) || reader.end_hidden(rows))
            && !ended_under(
                aterm_phase::since_limit(rows).as_ref(),
                memory.wall.kind,
                limit::ResetClock::of(memory.wall.reset.as_deref()),
            ))
        .then(|| memory.wall.clone())
    }

    /// Clear wall memory when `rows` show a live turn on a screen with a composer.
    pub(super) fn end_memory_on_live_turn(
        &mut self,
        reader: &dyn ScreenReader,
        rows: &[String],
        seq: u64,
    ) {
        if reader.composer(rows).is_none()
            || aterm_phase::background_only(rows)
            || background_alone(reader, rows)
            || super::super::policy::login::dialog_stands(rows)
        {
            return;
        }
        if self.turn_end.retry_met_untaken() {
            self.wall_memory = None;
            self.ledger_row(
                RULE_WALL_WAIT_END,
                Outcome::Waiting,
                "",
                approvals::WALL_TAKEN,
                seq,
            );
        } else {
            self.forget_wall_because(seq, WALL_LEFT);
        }
    }

    /// Fold a turn-end reading into wall memory and leave the effective [`Wall`](aterm_phase::Wall)
    /// in `reading`.
    pub(super) fn wall_memory_step(
        &mut self,
        reader: &dyn ScreenReader,
        rows: &[String],
        reading: &mut aterm_phase::Reading,
        seq: u64,
    ) {
        match reading.wall.clone() {
            Some(w) if is_kept(&w) && !self.turn_end.left_bucket(w.kind, &w.message) => {
                self.remember_wall(&w, reader.program(), seq);
            }
            Some(_) => self.forget_wall(seq),
            None => match reading.phase {
                aterm_phase::Phase::Busy => {
                    if let Some(w) = self.remembered_wall(reader, rows) {
                        reading.phase = aterm_phase::Phase::Limited {
                            message: w.message.clone(),
                            reset: w.reset.clone(),
                        };
                        reading.wall = Some(w);
                    } else {
                        self.end_memory_on_live_turn(reader, rows, seq);
                    }
                }
                aterm_phase::Phase::Idle if reader.end_hidden(rows) => {
                    if let Some(w) = self.remembered_wall(reader, rows) {
                        reading.phase = aterm_phase::Phase::Limited {
                            message: w.message.clone(),
                            reset: w.reset.clone(),
                        };
                        reading.wall = Some(w);
                    }
                }
                aterm_phase::Phase::Idle
                | aterm_phase::Phase::Question
                | aterm_phase::Phase::Prompt
                | aterm_phase::Phase::Limited { .. } => {}
            },
        }
    }

    /// Update wall memory after observing an idle or busy turn-end point.
    pub(super) fn observe_point(&mut self, reading: &TurnEndReading, now: Instant, seq: u64) {
        let unasked = self.turn_end.fallback_unasked();
        let land = self.turn_end.land_episode().0;
        self.turn_end.observe(reading, now);
        if reading.login_moved && self.turn_end.login_resume_owed() {
            self.login_taken(seq);
        }
        // The limit's login lasts as its wait does, or its owed resume: a point
        // with neither (a served turn that ended the limit) reads no profile.
        let limit_stands =
            self.turn_end.waits_on_named_reset() || self.turn_end.login_resume_owed();
        if reading.wall.is_none() && !limit_stands {
            self.wall_login = None;
        }
        self.land_over(land, seq);
        self.fallback_asked(unasked, seq);
        if self.turn_end.limit_ended() {
            let why = if self.turn_end.limit_ended_elsewhere() {
                approvals::LIMIT_ENDED_ELSEWHERE
            } else if reading.taskless && reading.wall.is_none() {
                WALL_LEFT_NO_TASK
            } else {
                approvals::LIMIT_ENDED
            };
            self.limit_ended_row(reading.wall.is_some(), why, seq);
        }
        if matches!(reading.phase, Phase::Idle | Phase::Question) && !limit_stands {
            self.forget_wall_because(
                seq,
                if reading.taskless {
                    WALL_LEFT_NO_TASK
                } else {
                    WALL_LEFT
                },
            );
        }
    }

    /// Record a host-typed turn (`kind`) against wall memory.
    pub(super) fn host_turn(&mut self, kind: HostTurn, seq: u64) {
        let unasked = self.turn_end.fallback_unasked();
        self.turn_end.host_typed(self.fx.mono(), kind);
        self.fallback_asked(unasked, seq);
    }

    /// Record an unasked fallback turn against wall memory.
    pub(super) fn fallback_asked(&mut self, unasked: bool, seq: u64) {
        if unasked && !self.turn_end.fallback_unasked() {
            self.ledger_row(
                RULE_MODEL_FALLBACK,
                Outcome::Waiting,
                "",
                approvals::FALLBACK_ASKED,
                seq,
            );
        }
    }

    fn limit_ended_row(&mut self, shown: bool, why: &str, seq: u64) {
        self.ledger_row(RULE_WALL_WAIT_END, Outcome::Waiting, "", why, seq);
        match self.wall_memory.as_mut() {
            Some(m) if shown => {
                m.saved = None;
                m.written = false;
                self.persist_wall(seq);
            }
            _ => self.wall_memory = None,
        }
    }

    fn remember_wall(&mut self, wall: &aterm_phase::Wall, program: Program, seq: u64) {
        let conversation = self
            .wall_conversation
            .as_ref()
            .filter(|(at, _)| *at == seq)
            .map(|(_, c)| c.clone())
            .unwrap_or(Conversation::Unknown);
        match &mut self.wall_memory {
            Some(m)
                if m.wall.kind == wall.kind
                    && m.wall.message == wall.message
                    && m.wall.reset == wall.reset =>
            {
                m.conversation = conversation.fresh_after(&m.conversation);
                m.program = Some(program);
            }
            slot => {
                *slot = Some(WallMemory {
                    wall: wall.clone(),
                    conversation,
                    program: Some(program),
                    saved: None,
                    written: false,
                });
            }
        }
    }

    /// Mark the ledger's open wall as closed by a typed supervisor action.
    pub(super) fn wall_closed_by_act(&mut self) {
        if let Some(m) = self.wall_memory.as_mut() {
            m.saved = None;
        }
    }

    /// Clear the remembered wall and persist its closure to the ledger.
    pub(super) fn forget_wall(&mut self, seq: u64) {
        self.forget_wall_because(seq, WALL_LEFT);
    }

    fn forget_wall_because(&mut self, seq: u64, why: &str) {
        if let Some(m) = self.wall_memory.take()
            && m.written
        {
            self.ledger_row(RULE_WALL_WAIT_END, Outcome::Waiting, "", why, seq);
        }
    }

    /// Persist the remembered wall, its scheduled reset and the usage tap's timing of it to the
    /// ledger if changed.
    pub(super) fn persist_wall(&mut self, seq: u64) {
        let unix = self.wall_reset.as_ref().and_then(|(_, u)| *u);
        let tap = self.tap_unix();
        let login = self.wall_login_account();
        let Some(m) = self.wall_memory.as_mut() else {
            return;
        };
        let saved = Some((unix, tap, login, m.conversation.clone(), m.program));
        if m.saved == saved {
            return;
        }
        m.saved = saved;
        m.written = true;
        let reason = approvals::wall_wait_reason_of(
            unix,
            tap,
            login,
            m.wall.reset.as_deref(),
            m.wall.kind,
            &m.wall.message,
        );
        if self.turn_end.take_try_over() {
            self.ledger_row(
                approvals::RULE_TRY_OVER,
                Outcome::Waiting,
                "",
                approvals::TRY_OVER,
                seq,
            );
        }
        self.ledger_row(RULE_WALL_WAIT, Outcome::Waiting, "", &reason, seq);
    }

    /// Scope the remembered limit's conversation requirement from `program` and `host`.
    pub(super) fn scope_limit_conversation(&mut self, program: Program, host: bool, seq: u64) {
        if met_under_another(self.limit_met_under(), program) {
            return;
        }
        if screen_alone(program) {
            if self.limit_met_under().is_none_or(screen_alone) {
                self.rescope_limit(&Conversation::Unknown, &Conversation::Hostless, seq);
            }
        } else if host && reads_conversation(program) {
            self.require_limit_conversation(seq);
        }
    }

    /// Program under which the current limit wall was met.
    pub(super) fn limit_met_under(&self) -> Option<Program> {
        self.wall_memory
            .as_ref()
            .and_then(|m| m.program)
            .or_else(|| self.turn_end.limit_program())
    }

    /// Require conversation evidence for the remembered limit wall.
    pub(super) fn require_limit_conversation(&mut self, seq: u64) {
        self.rescope_limit(&Conversation::Hostless, &Conversation::Unknown, seq);
    }

    /// Require conversation evidence for a limit wall seeded at loop startup.
    pub(super) fn require_seeded_limit_conversation(&mut self) {
        if !self.limit_met_under().is_some_and(screen_alone) {
            self.require_limit_conversation(0);
        }
    }

    fn rescope_limit(&mut self, from: &Conversation, to: &Conversation, seq: u64) {
        self.turn_end.rescope_limit_conversation(from, to);
        let changed = if let Some(m) = self.wall_memory.as_mut()
            && m.conversation == *from
        {
            m.conversation = to.clone();
            true
        } else {
            false
        };
        if changed {
            self.persist_wall(seq);
        }
    }

    /// Seed wall memory from an open wall recorded in the approval ledger, waited to the usage
    /// tap's reset where the row names one.
    pub(super) fn seed_wall_memory(&mut self, open: Option<approvals::OpenWall>) {
        if self.wall_memory.is_some() {
            return;
        }
        let Some(open) = open else {
            return;
        };
        let now = self.now_unix();
        let ttl = i64::try_from(MEMORY_TTL.as_secs()).unwrap_or(i64::MAX);
        let waited = open.tap_unix.or(open.reset_unix);
        let since = waited.unwrap_or(open.at_ms / 1000);
        if now > since.saturating_add(ttl) {
            return;
        }
        let Some(kind) =
            approvals::wall_kind(open.kind, &open.message).filter(|k| k.reads_limited())
        else {
            return;
        };
        let key = format!("{}|{}", kind.name(), open.reset.as_deref().unwrap_or("-"));
        self.wall_reset = Some((key.clone(), open.reset_unix));
        if let Some(at) = open.tap_unix {
            self.seed_tap(key, kind, &open.message, at, now);
        }
        self.seed_login(open.login);
        if let Some(unix) = waited {
            let jitter = self.setup.sid.as_deref().map_or(Duration::ZERO, |sid| {
                resume_jitter(sid, unix, self.turn_end.timing.reset_jitter)
            });
            let ago = u64::try_from(self.now_unix_ms().saturating_sub(open.at_ms))
                .map_or(Duration::ZERO, Duration::from_millis);
            self.turn_end.seed_named_reset(
                kind,
                &open.message,
                self.instant_of(unix),
                jitter,
                limit::resumes_by_itself(&open.message),
                limit::ResetClock::of(open.reset.as_deref()),
                self.fx
                    .mono()
                    .checked_sub(ago)
                    .unwrap_or_else(|| self.fx.mono()),
            );
        } else {
            let take = i64::try_from(self.turn_end.timing.take_within.as_millis()).unwrap_or(0);
            let appearance = if open.retry_ms.is_some() {
                open.judged_ms.unwrap_or(open.at_ms.saturating_add(take))
            } else {
                open.at_ms
            };
            let ago = u64::try_from(self.now_unix_ms().saturating_sub(appearance))
                .map_or(Duration::ZERO, Duration::from_millis);
            self.turn_end.seed_unnamed_wall(
                kind,
                &open.message,
                self.fx
                    .mono()
                    .checked_sub(ago)
                    .unwrap_or_else(|| self.fx.mono()),
            );
        }
        self.turn_end
            .seed_limit_conversation(open.conversation.clone(), open.program);
        self.wall_memory = Some(WallMemory {
            conversation: open.conversation.clone(),
            program: open.program,
            wall: aterm_phase::Wall {
                kind,
                message: open.message,
                reset: open.reset,
                row: 0,
                placement: aterm_phase::Placement::Gutter,
                under_call: false,
            },
            saved: Some((
                open.reset_unix,
                open.tap_unix,
                open.login,
                open.conversation,
                open.program,
            )),
            written: true,
        });
    }
}

/// Escalation rung for the stuck-screen canary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum StuckRung {
    /// Nothing said.
    Quiet,
    /// `STUCK` is in the journal.
    Said,
    /// `STALLED` is in the journal, and the host has been told unless the screen is Claude
    /// Code's own Settings panel ([`StuckOn::Panel`]).
    Told,
    /// The exception is up.
    Raised,
}

/// What the dialog canary's screen is ([`Session::dialog_words`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StuckOn {
    /// A screen no reader names: if the tab is stalled, [`UNREAD_DIALOG`].
    Unread,
    /// Claude Code's own Settings panel (`/status`, `/config`, `/usage`), which
    /// [`aterm_phase::settings_panel`] reads. The harness closes only the panel its own
    /// `/usage` probe opens, and the canary is off while that probe is under way, so this
    /// one stands until whoever opened it closes it.
    Panel,
}

/// What the dialog canary calls a screen no reader names ([`StuckOn::Unread`]).
pub(super) const UNREAD_DIALOG: &str = "a dialog this build does not read";

/// What the dialog canary calls Claude Code's own Settings panel ([`StuckOn::Panel`]).
pub(super) const SETTINGS_PANEL: &str = "Claude Code's own Settings panel";

impl StuckRung {
    const fn next(self) -> Option<(Self, u32)> {
        match self {
            Self::Quiet => Some((Self::Said, 1)),
            Self::Said => Some((Self::Told, 3)),
            Self::Told => Some((Self::Raised, 6)),
            Self::Raised => None,
        }
    }
}

impl<C: Ctl> Session<'_, C> {
    /// Evaluate and fire the stuck-busy canary when background work stalls.
    pub(super) fn stuck_busy_note(
        &mut self,
        screen: &Screen,
        since: Option<Instant>,
        review: &mut dyn Review,
    ) {
        if self.stuck_dialog.is_some() {
            self.stuck_over(screen.seq, "the screen moved on", review);
        }
        let reader = self.reader(&screen.rows);
        let stuck = since.is_some() && background_alone(reader, &screen.rows);
        let Some(since) = since.filter(|_| stuck) else {
            self.stuck_over(screen.seq, "the screen moved on", review);
            self.stuck_asked = None;
            return;
        };
        let (at, witness) = if title_witness(reader.program()) {
            (since, Witness::Title)
        } else {
            let what = reader.background_wait(&screen.rows).unwrap_or_default();
            let words = still_words(reader, &screen.rows, what);
            let from = match &self.stuck_still {
                Some((was, from)) if *was == words => *from,
                _ => {
                    self.stuck_over(screen.seq, "the agent's words moved", review);
                    let now = self.fx.mono();
                    self.stuck_still = Some((words, now));
                    now
                }
            };
            (from.max(since), Witness::Words(what))
        };
        self.stuck_climb(
            screen,
            at,
            self.setup.stuck_after,
            ExceptionKind::Stalled,
            witness,
            review,
        );
    }

    fn no_progress_call(&self) -> Option<super::super::background::BackgroundCall> {
        let host = self.stall_host.as_ref()?;
        match host.background_jobs() {
            super::super::background::BackgroundEvidence::NoProgress { worst } => Some(worst),
            super::super::background::BackgroundEvidence::Unknown => None,
        }
    }

    /// Evaluate and fire the unrecognized-dialog canary.
    pub(super) fn dialog_note(&mut self, screen: &Screen, review: &mut dyn Review) {
        if !review.unattended() {
            return;
        }
        let Some(words) = self.dialog_words(screen) else {
            // Nothing a person could be left at: an episode that was open is over.
            if self.stuck_dialog.is_some() {
                self.stuck_over(screen.seq, "the screen moved on", review);
            }
            return;
        };
        match self.stuck_dialog.as_ref() {
            Some((was, at)) if *was == words => {
                let at = *at;
                let after = self.setup.stuck_after / DIALOG_DIVISOR;
                self.stuck_climb(
                    screen,
                    at,
                    after,
                    ExceptionKind::Dialog,
                    Witness::Title,
                    review,
                );
            }
            _ => {
                self.stuck_over(screen.seq, "the screen changed", review);
                self.stuck_dialog = Some((words, self.fx.mono()));
            }
        }
    }

    /// Extract the words of a still screen that holds the tab from `screen`, if present, and
    /// what it is: one no reader names, or Claude Code's own Settings panel, which a reader
    /// names and is no dialog this build does not read.
    pub(super) fn dialog_words(&self, screen: &Screen) -> Option<(String, StuckOn)> {
        if self.stalled.is_some()
            || self.probe != UsageProbe::Idle
            || self.reset != ResetStep::Idle
            || self.exception_open.is_some()
        {
            return None;
        }
        let rows = &screen.rows;
        let reader: &dyn ScreenReader = &aterm_phase::ClaudeReader;
        let phase = reader.phase(rows);
        if phase != Phase::Idle || reader.phase_authoritative(rows, &phase) || reader.survey(rows) {
            return None;
        }
        let on = if aterm_phase::settings_panel(rows).is_some() {
            StuckOn::Panel
        } else {
            StuckOn::Unread
        };
        let mut words = String::new();
        push_words_above(&mut words, rows);
        let words = words.trim();
        (!words.is_empty()).then(|| (words.to_string(), on))
    }

    fn stuck_climb(
        &mut self,
        screen: &Screen,
        at: Instant,
        after: Duration,
        kind: ExceptionKind,
        witness: Witness,
        review: &mut dyn Review,
    ) {
        let Some((next, times)) = self.stuck_rung.next() else {
            return;
        };
        let paced = self.stuck_asked.is_some_and(|t| self.fx.since(t) < after);
        // A rung said with no call named (`STUCK`, or a `STALLED` in today's words) does not
        // hold back a call the reading names since: it is told at once (the hung-call design's
        // R1), not one status pace after that rung's read. Every other rung keeps the pace.
        let unnamed = self.stuck_rung == StuckRung::Said
            || (self.stuck_rung == StuckRung::Told
                && self.stuck_told.is_some_and(|(_, pgid)| pgid.is_none()));
        if paced && !unnamed {
            return;
        }
        let dialog = kind == ExceptionKind::Dialog;
        let panel = dialog && matches!(self.stuck_dialog, Some(((_, StuckOn::Panel), _)));
        let call = if dialog || self.fx.since(at) < self.setup.background_settle {
            None
        } else {
            self.no_progress_call()
        };
        if paced && call.is_none() {
            return;
        }
        let (next, due) = match &call {
            Some(_) if next <= StuckRung::Told => (StuckRung::Told, true),
            Some(c) if self.stuck_told.is_some_and(|(_, g)| g != Some(c.pgid)) => {
                (StuckRung::Told, true)
            }
            Some(_) => (
                next,
                self.stuck_told
                    .is_some_and(|(told, _)| self.fx.since(told) >= after),
            ),
            None => (next, self.fx.since(at) >= after * times),
        };
        if !due {
            return;
        }
        let status = match witness {
            Witness::Words(_) => String::new(),
            Witness::Title => {
                self.stuck_asked = Some(self.fx.mono());
                let Ok(r) = self.call(&["status"]) else {
                    return;
                };
                if !title_rests(&r.stdout) {
                    self.stuck_over(screen.seq, "the agent's title moves: it works", review);
                    return;
                }
                if dialog && !names_claude(&r.stdout) {
                    self.stuck_over(screen.seq, "the tab runs no Claude", review);
                    return;
                }
                r.stdout
            }
        };
        let minutes = self.fx.since(at).as_secs() / 60;
        self.stuck_rung = next;
        match (next, &call) {
            (StuckRung::Told, Some(call)) => {
                self.stuck_told = Some((self.fx.mono(), Some(call.pgid)));
                review.note(&format!(
                    "STALLED seq={} {}; told to the window as a record",
                    screen.seq,
                    call.fact(),
                ));
                if let Some(host) = &self.stall_host {
                    host.stall_unread(Some(StallSeen::NoProgress {
                        minutes: call.quiet_min(),
                        pid: call.pid,
                    }));
                }
                return;
            }
            (StuckRung::Raised, Some(call)) => {
                self.raise_stuck(screen.seq, &Exception::no_progress(call), kind, review);
                return;
            }
            _ => {}
        }
        match next {
            StuckRung::Quiet => {}
            StuckRung::Said if panel => {
                let agent = crate::supervise::screen::status_field(&status, "agent");
                review.note(&format!(
                    "STUCK seq={} the agent's own title says idle, and {SETTINGS_PANEL} has \
                     stood open, unchanged, for {minutes} min (agent={}): the harness did not \
                     open it and does not close it, so it stands until whoever opened it \
                     closes it. rows: {}",
                    screen.seq,
                    agent.unwrap_or("-"),
                    tail_rows(&screen.rows),
                ));
            }
            StuckRung::Said if dialog => {
                let agent = crate::supervise::screen::status_field(&status, "agent");
                review.note(&format!(
                    "STUCK seq={} the agent's own title says idle, and the screen has stood \
                     unchanged for {minutes} min with no composer, box, wall or busy row \
                     (agent={}): no reader here names it. If the tab is stalled, this is \
                     {UNREAD_DIALOG}, and only a person can answer it. rows: {}",
                    screen.seq,
                    agent.unwrap_or("-"),
                    tail_rows(&screen.rows),
                ));
            }
            StuckRung::Said => match witness {
                Witness::Title => review.note(&format!(
                    "STUCK seq={} the agent's own title says idle, and the screen has read busy \
                     on background work alone (a shell, a monitor, a workflow) for {minutes} \
                     min: no wall, question or box is recognised there. If the tab is stalled, \
                     this screen is one this build does not read.",
                    screen.seq,
                )),
                Witness::Words(what) => review.note(&format!(
                    "STUCK seq={} the agent's turn ended and its own background work ({what}) \
                     has run for {minutes} min under words that stood still: nothing is typed \
                     while it runs, a question the turn ended on included. If that work never \
                     ends (a server, a daemon, a repeating timer), only a person can end it.",
                    screen.seq,
                )),
            },
            StuckRung::Told if panel => {
                // A record of a screen this build does not recognise would be false here: the
                // journal keeps it, and the third rung names the panel.
                self.stuck_told = Some((self.fx.mono(), None));
                review.note(&format!(
                    "STALLED seq={} still so after {minutes} min: {SETTINGS_PANEL}, which the \
                     harness did not open, noted here alone",
                    screen.seq,
                ));
            }
            StuckRung::Told => {
                self.stuck_told = Some((self.fx.mono(), None));
                let what = match witness {
                    _ if dialog => UNREAD_DIALOG,
                    Witness::Title => "a wait this build does not classify",
                    Witness::Words(_) => "background work of the agent's own that has not ended",
                };
                review.note(&format!(
                    "STALLED seq={} still so after {minutes} min: {what}, told to the window as \
                     a record",
                    screen.seq,
                ));
                let seen = match witness {
                    Witness::Title => StallSeen::Unread { minutes },
                    Witness::Words(_) => StallSeen::OwnWork { minutes },
                };
                if let Some(host) = &self.stall_host {
                    host.stall_unread(Some(seen));
                }
            }
            StuckRung::Raised => {
                let exception = match witness {
                    _ if panel => Exception::settings_panel(),
                    _ if dialog => Exception::dialog(),
                    Witness::Title => Exception::stalled(),
                    Witness::Words(_) => Exception::stalled_on_work(),
                };
                self.raise_stuck(screen.seq, &exception, kind, review);
            }
        }
    }

    fn raise_stuck(
        &mut self,
        seq: u64,
        exception: &Exception,
        kind: ExceptionKind,
        review: &mut dyn Review,
    ) {
        review.note(&format!(
            "EXCEPTION seq={seq} kind={} when={} {} => {}",
            exception.kind.word(),
            exception.when.word(),
            exception.why,
            exception.action
        ));
        if let Some(host) = &self.stall_host {
            host.exception(Some(exception));
        }
        self.stuck_raised = Some((self.fx.mono(), kind));
    }

    /// Clear any active stuck-screen canary and log recovery if one had fired.
    pub(super) fn stuck_over(&mut self, seq: u64, cause: &str, review: &mut dyn Review) {
        self.stuck_dialog = None;
        self.stuck_still = None;
        self.stuck_told = None;
        let rung = std::mem::replace(&mut self.stuck_rung, StuckRung::Quiet);
        let raised = self.stuck_raised.take();
        if rung >= StuckRung::Told
            && let Some(host) = &self.stall_host
        {
            host.stall_unread(None);
        }
        if let Some((since, kind)) = raised {
            review.note(&format!(
                "CLEARED seq={seq} exception={} after={}s {cause}",
                kind.word(),
                self.fx.since(since).as_secs(),
            ));
            if let Some(host) = &self.stall_host {
                host.exception(self.exception_open.as_ref().map(|(open, _)| open));
            }
        }
    }
}

fn title_rests(stdout: &str) -> bool {
    let field = |f: &str| crate::supervise::screen::status_field(stdout, f);
    field("subject_source") == Some("osc")
        && field("subject")
            .map(crate::supervise::report::pct_decode)
            .is_some_and(|t| t.trim_start().starts_with(IDLE_TITLE_GLYPH))
}

fn names_claude(stdout: &str) -> bool {
    match crate::supervise::screen::status_field(stdout, "program")
        .and_then(aterm_phase::program_of)
    {
        Some(Program::Claude) => true,
        Some(
            Program::Codex
            | Program::Grok
            | Program::Kimi
            | Program::Agy
            | Program::Muse
            | Program::Generic,
        )
        | None => false,
    }
}

const TAIL_ROWS_CHARS: usize = 400;

fn tail_rows(rows: &[String]) -> String {
    let said: Vec<&str> = rows
        .iter()
        .map(|r| r.trim())
        .filter(|r| !r.is_empty())
        .collect();
    let line: Vec<char> = said[said.len().saturating_sub(5)..]
        .join(" ⏎ ")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let cut = line.len().saturating_sub(TAIL_ROWS_CHARS);
    let tail: String = line[cut..].iter().collect();
    if cut > 0 { format!("…{tail}") } else { tail }
}
