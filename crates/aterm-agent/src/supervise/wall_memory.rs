// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wall the loop waits on is remembered.
//!
//! A limit's wait is hours long, and the wall is read off the screen: the notice must be the last
//! thing said above the composer. A scroll back through the transcript, or a local command
//! (`/model`, `/usage`) whose rows are said after the wall, changes that without ending it; a
//! screen that then holds only the agent's own background work reads `Busy` with no wall, and the
//! standing wait would be dropped (a shell-less tab reads `Idle` and keeps the reset by the
//! policy's own rule, `turn_end::still_shown`).
//!
//! What is kept is the wall the loop last read and waited on — a limit that
//! resets (`reads_limited`: a usage window, a spend limit, a model bucket, a
//! 429) — in memory and, for a loop that follows it, as a ledger row
//! ([`approvals::RULE_WALL_WAIT`]). While the screen shows nothing but the
//! agent's own background work ([`aterm_phase::background_only`]: no
//! spinner, no `esc to interrupt`, no message just submitted; on agy and
//! Muse Code, their readers' [`ScreenReader::background_wait`]), or the
//! session at rest on a turn's end the screen hides
//! ([`ScreenReader::end_hidden`]: Muse Code's `/status` cards alone saying
//! `FAILED`), the memory stands for the wall the screen no longer shows; the
//! standing `WaitUntil`
//! then fires at the reset and its grace as it would have with the rows
//! showing — unless the screen draws the wall's end under that wall's own
//! notice ([`ended_under`]: a person's short turn, which no read saw busy,
//! is work since the wall, and so is the vendor's own continuation; under
//! another limit's notice, a `/resume`d conversation's, neither is). A live
//! turn (the worker worked: the
//! owner typed, the vendor's own resume), a question or an idle screen with
//! no wall, and another wall end it
//! — the ledger row for the end ([`approvals::RULE_WALL_WAIT_END`]) says so to
//! the next loop; so does any act the policy typed since
//! ([`approvals::open_wall`]), and the wall that act meets again is written
//! again ([`Session::wall_closed_by_act`]). NOT an idle screen while the
//! policy still waits on the reset the notice named
//! ([`TurnEndState::waits_on_named_reset`]): Esc on the vendor's countdown
//! leaves one, and the wait it leaves standing can
//! last days. The wall stays open, and a loop that follows takes that reset up
//! ([`Session::seed_wall_memory`]): with the wall's end written, a successor
//! typed `keep going` into the standing limit (the review of 2026-10-14).
//!
//! Nothing is typed by this module: it decides what a screen reads as.

use super::super::policy::outlook::ExceptionKind;
use super::super::policy::turn_end::{
    Conversation, HostTurn, RULE_MODEL_FALLBACK, ended_under, met_under_another,
    reads_conversation, screen_alone,
};
use super::approvals::{self, Outcome, RULE_WALL_WAIT, RULE_WALL_WAIT_END, WALL_LEFT_NO_TASK};
use super::turn_end_loop::resume_jitter;
use super::*;

/// How long past its reset a wall stays remembered when nothing has ended it:
/// the ladder that follows the reset (10 minutes, then 30, for ever) and the
/// policy's `Futile` exception judge a wall that outlasts its reset by hours
/// (`exception.rs`); a memory older than this is history.
pub(super) const MEMORY_TTL: Duration = Duration::from_secs(24 * 3600);

/// Why a wall's wait ended, as its ledger row ([`RULE_WALL_WAIT_END`]) says it: the screen left
/// it, the worker worked, another wall replaced it.
const WALL_LEFT: &str = "the wall left the screen or the worker worked";

/// How long a screen may read busy on the agent's own background work alone,
/// with the agent's OWN title saying idle, before the loop says so in its
/// journal ([`Session::stuck_busy_note`]). The ladder's unit: its second rung
/// is three of these and its third six.
pub(super) const STUCK_AFTER: Duration = Duration::from_secs(30 * 60);

/// How many times shorter than [`STUCK_AFTER`] the dialog flavour's unit is
/// ([`Session::dialog_note`]): nothing runs on its screen, so no long background job can explain
/// it.
const DIALOG_DIVISOR: u32 = 3;

/// The glyph Claude Code puts before its terminal title while idle (`✳ Claude Code`); a working
/// agent animates a spinner glyph there instead.
const IDLE_TITLE_GLYPH: char = '\u{2733}';

/// The wall being waited on, and what was last written of it to the ledger.
#[derive(Debug, Clone)]
pub(super) struct WallMemory {
    pub(super) wall: aterm_phase::Wall,
    pub(super) conversation: Conversation,
    /// THE PROGRAM THE WALL WAS MET UNDER: the reader that last read its
    /// notice ([`Session::remember_wall`]), or the one the row a loop before
    /// this one wrote names ([`approvals::OpenWall::program`]). `None`: a row
    /// an older build wrote, read as before. Written with the wall
    /// ([`approvals::PROGRAM`]).
    pub(super) program: Option<Program>,
    /// The reset placement last written to the ledger, with the scope and
    /// the program it was written with: `None` before the first write, then
    /// `Some(the placement)` — and `None` again once an act typed closed the
    /// ledger's wall ([`Session::wall_closed_by_act`]).
    saved: Option<(Option<i64>, Conversation, Option<Program>)>,
    /// The wall WAS WRITTEN to the ledger, by this loop
    /// ([`Session::persist_wall`]) or the one it took the wall up from
    /// ([`Session::seed_wall_memory`]). Its end is written whenever it was
    /// ([`Session::forget_wall`]), an act typed since or not: the act's row
    /// closes the ledger's wall for [`approvals::open_wall`], not for every
    /// reader. [`approvals::account_moves`] ends a move refused at the wall
    /// ([`approvals::AccountMoves::unmade`]) at a typed row or the wall's
    /// end row only. Shown before by the verifier of 49f07f50e (V21-3, a
    /// probe): a try typed at the wall, then a move to another account asked
    /// there and refused (`skipped … at=wall`), then the wall ended by a
    /// person's work with no typed row; no end row was written, the ledger
    /// kept `unmade`, and a loop that followed carried the latch onto its
    /// first wall, where no move was asked beside an account that could serve.
    written: bool,
}

/// Whether the canary's witness on `p`'s screen is the agent's own title
/// ([`Witness::Title`]): Claude Code's, which rests on `✳` while it does not
/// work whatever its screen's words say. agy's and Muse Code's titles are
/// unmeasured; their own words are the witness ([`Witness::Words`]).
fn title_witness(p: Program) -> bool {
    match p {
        Program::Claude => true,
        Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => false,
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

/// What stands still on agy's or Muse Code's screen while the canary climbs
/// on their own background work ([`Witness::Words`]): the work their reader
/// names, the letters of the last rows above the composer (the reply or the
/// question the turn ended on and the tool rows over it, so a clock's digits
/// are no change and a turn of the agent's own is one), and the draft. The
/// panel and the footer under the composer, whose glyphs and clocks redraw
/// every second, are no part of it.
fn still_words(reader: &dyn ScreenReader, rows: &[String], what: &str) -> String {
    let mut words = format!("{what}\n");
    if let Some((caret, draft)) = reader.composer(rows) {
        push_words_above(&mut words, &rows[..caret.min(rows.len())]);
        words.push_str(&draft.join("\n"));
    }
    words
}

/// THE SCREEN SHOWS NOTHING BUT THE AGENT'S OWN BACKGROUND WORK, its turn
/// over: Claude Code's by its grammar ([`aterm_phase::background_only`]: no
/// spinner, no `esc to interrupt`, no message just submitted); any other
/// agent's by its reader ([`ScreenReader::background_wait`]: a task, a
/// timer or a subagent in agy's panel, a backgrounded shell or a monitor in
/// Muse Code's, under a turn that ended; Codex's and Gemini CLI's readers
/// have no such read). The remembered wall stands through it, on every
/// agent as on Claude Code.
fn background_alone(reader: &dyn ScreenReader, rows: &[String]) -> bool {
    match reader.program() {
        Program::Claude => aterm_phase::background_only(rows),
        Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic => {
            reader.background_wait(rows).is_some()
        }
    }
}

/// A wall the memory is kept for: a limit that resets, whose wait the loop
/// owns. An API error's ladder rereads the screen at every rung, a login and
/// a full context are acted on and left, a memory banner is no turn's end.
fn is_kept(wall: &aterm_phase::Wall) -> bool {
    wall.kind.reads_limited()
}

impl<C: Ctl> Session<'_, C> {
    /// The remembered wall, when `rows` show nothing but the agent's own
    /// background work ([`background_alone`]: Claude Code's screen, its
    /// composer frame drawn, no live turn and no message just submitted; agy's
    /// or Muse Code's, its reader naming the work under an ended turn) — or
    /// the session at rest on a turn's end the screen hides
    /// ([`ScreenReader::end_hidden`]: Muse Code's cards alone, the newest
    /// saying the run `FAILED` or naming none) — and the wall not ended under
    /// its own notice ([`ended_under`]: a person's
    /// short turn under it, which no read saw busy, is work since the wall,
    /// and so is the vendor's own continuation; either under another limit's
    /// notice is not). Never a wall met under another agent than the one
    /// `reader` reads ([`met_under_another`]): read on that agent's
    /// background work, it held the agent until the other's reset and was
    /// then resumed into it.
    pub(super) fn remembered_wall(
        &self,
        reader: &dyn ScreenReader,
        rows: &[String],
    ) -> Option<aterm_phase::Wall> {
        let memory = self.wall_memory.as_ref()?;
        // Another agent's limit is none of this one's
        // ([`met_under_another`]): its screen stands for no wall of theirs.
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

    /// `rows` read `Busy`: a LIVE turn on a screen with a composer (a spinner,
    /// `esc to interrupt`, a message just submitted) ends the memory. A busy
    /// read that is only background work — by Claude Code's grammar
    /// ([`aterm_phase::background_only`]) or, on another agent, its reader's
    /// ([`background_alone`]: agy's panel, Muse Code's; never Claude Code's
    /// `background_wait`, which names a shell under a live turn's `esc to
    /// interrupt` too) — or a screen with no composer frame (a dialog
    /// covering it), leaves it.
    /// Where that turn TAKES THE WALL'S
    /// RETRY a wall answered inside its take window
    /// ([`TurnEndState::retry_met_untaken`]), the ledger says so
    /// ([`approvals::WALL_TAKEN`]) whether or not the memory still stands:
    /// the wall read was the screen the retry was typed at, and a loop that
    /// follows reads no try met again. Written only where the memory ended at
    /// the take, a take after the memory had ended inside the same window (a
    /// person's `/model` under the notice read as an idle point, which ends
    /// it) wrote nothing: the take window's wall row read as the try met
    /// again, and a successor attached during the person's three-hour turn
    /// that took the try held the try's two-hour rung, where the first loop
    /// counted that turn's work as the try's and typed 30 minutes from the
    /// wall it ended on (the verifier of 5b72b1613, M7-2).
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
        // The wall's retry this turn takes ([`TurnEndState::took`]) was typed
        // at the wall read since: no answer of its. Once: the take clears it.
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

    /// The reading of a point folded into the memory ([`Self::turn_end_reading`]): a wall that
    /// resets is remembered, another wall ends it, and a busy screen with only background work
    /// reads as the remembered wall. An idle screen and a question are the policy's to judge first
    /// ([`Self::observe_point`]).
    pub(super) fn wall_memory_step(
        &mut self,
        reader: &dyn ScreenReader,
        rows: &[String],
        reading: &mut aterm_phase::Reading,
        seq: u64,
    ) {
        match reading.wall.clone() {
            // A bucket the session left, its notice redrawn by the fallback's
            // relaunch, is no wall of the session's: none is kept, and the
            // ledger's wall ends ([`TurnEndState::bucket_left`]).
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
                // AT REST ON A TURN'S END THE SCREEN HIDES (Muse Code's cards
                // alone, the newest saying `FAILED`): a person's look, no work
                // since. The remembered wall is that end still, and the point
                // is decided as at its notice. Read as the idle point it shows,
                // a wall whose notice named no reset was over at the first such
                // read, and its resume went into the standing limit.
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

    /// A point folded into the policy ([`TurnEndState::observe`]) and then into the memory: an
    /// idle screen or a question ends the memory, unless the policy still waits on the reset a
    /// limit's notice named ([`TurnEndState::waits_on_named_reset`]), so the memory ends at the
    /// point the wait does. A conversation with no task ([`TurnEndReading::taskless`]: a `/clear`
    /// at the limit) ends the policy's wait too, and the ledger's end row says so, not that the
    /// worker worked.
    pub(super) fn observe_point(&mut self, reading: &TurnEndReading, now: Instant, seq: u64) {
        let unasked = self.turn_end.fallback_unasked();
        let land = self.turn_end.land_episode().0;
        self.turn_end.observe(reading, now);
        self.land_over(land, seq);
        self.fallback_asked(unasked, seq);
        if self.turn_end.limit_ended() {
            // The policy's end of a conversation with no task says so, as the
            // memory's does (below).
            let why = if reading.taskless && reading.wall.is_none() {
                WALL_LEFT_NO_TASK
            } else {
                approvals::LIMIT_ENDED
            };
            self.limit_ended_row(reading.wall.is_some(), why, seq);
        }
        if matches!(reading.phase, Phase::Idle | Phase::Question)
            && !self.turn_end.waits_on_named_reset()
        {
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

    /// A TURN THE HOST TYPED ([`TurnEndState::host_typed`]): awaited as the
    /// policy's own acts are, its answer no work of the worker's, and, the
    /// fallback's relaunch unasked till now, written as the turn asked on it
    /// ([`Self::fallback_asked`]).
    pub(super) fn host_turn(&mut self, kind: HostTurn, seq: u64) {
        let unasked = self.turn_end.fallback_unasked();
        self.turn_end.host_typed(self.fx.mono(), kind);
        self.fallback_asked(unasked, seq);
    }

    /// A TURN ASKED ON THE FALLBACK that typed no row of the loop's (`unasked`
    /// before, [`TurnEndState::fallback_unasked`] no longer): a point that
    /// drew no notice of the bucket the relaunch left, or a turn the host
    /// typed. The ledger says so ([`approvals::FALLBACK_ASKED`]), so a loop
    /// that follows reads that bucket's notice as the wall it is again, as
    /// this one does ([`approvals::OpenSwitch::asked`]). Where the loop types
    /// the act, the act's own row says it.
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

    /// A LIMIT'S TRACK ENDED at this point by no act of the loop's
    /// ([`TurnEndState::limit_ended`]): the ledger says so and `why`
    /// ([`approvals::LIMIT_ENDED`], [`WALL_LEFT_NO_TASK`]), and a loop that
    /// follows takes up no try made before it ([`approvals::usage_attempt`]).
    /// That row ends the remembered wall; a wall `shown` at the point — the
    /// same limit met by that work, a new episode of it, or the other wall
    /// that took its place — is written again after the end.
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

    /// Remember `wall`, met under `program` (the reader that read it): the
    /// same wall read again (its rows shift, its footer ticks) changes
    /// nothing.
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
                // A fresh confirmation replaces old ownership as well as
                // old words. A missing later host cannot erase host scope.
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

    /// AN ACT TYPED CLOSED THE LEDGER'S WALL ([`approvals::open_wall`]: no
    /// `typed` row of any rule may follow the wall's row), so the remembered
    /// wall is written again at its next reading ([`Self::persist_wall`]). The
    /// act may have met the wall again: the same notice, no busy read between
    /// (Claude Code answers a `keep going` at a standing limit at once), which
    /// the memory holds unchanged. That wall stands for a loop that follows as
    /// it stands for this one. Shown before by the verifier of the successor's
    /// seed's refix (V2-1, a probe): nothing was written after the act, the
    /// successor found no wall open, and at a screen with no notice (Esc on the
    /// countdown, a `/resume`) the continue policy typed `keep going` into the
    /// standing limit two minutes in. A wall the act ended (the worker's work,
    /// another wall) is not read again; its end row is still written
    /// ([`WallMemory::written`]), for the readers the act's row does not close.
    pub(super) fn wall_closed_by_act(&mut self) {
        if let Some(m) = self.wall_memory.as_mut() {
            m.saved = None;
        }
    }

    /// Forget the wall: the ledger says so to a loop that follows this one.
    pub(super) fn forget_wall(&mut self, seq: u64) {
        self.forget_wall_because(seq, WALL_LEFT);
    }

    /// [`Self::forget_wall`], the ledger's end row saying `why`: written for a
    /// wall that was written ([`WallMemory::written`]).
    fn forget_wall_because(&mut self, seq: u64, why: &str) {
        if let Some(m) = self.wall_memory.take()
            && m.written
        {
            self.ledger_row(RULE_WALL_WAIT_END, Outcome::Waiting, "", why, seq);
        }
    }

    /// Write the remembered wall to the ledger when it, or the reset the loop
    /// placed for it, is not what was last written. After the reset is placed
    /// ([`Self::wall_reset_at`]).
    pub(super) fn persist_wall(&mut self, seq: u64) {
        let unix = self.wall_reset.as_ref().and_then(|(_, u)| *u);
        let Some(m) = self.wall_memory.as_mut() else {
            return;
        };
        let saved = Some((unix, m.conversation.clone(), m.program));
        if m.saved == saved {
            return;
        }
        m.saved = saved;
        m.written = true;
        let reason = approvals::wall_wait_reason(
            unix,
            m.wall.reset.as_deref(),
            m.wall.kind,
            &m.wall.message,
        );
        // The try a loop before this one made, which this loop's seed found
        // aged out ([`TurnEndState::take_try_over`]): over before this wall,
        // which stands ([`approvals::RULE_TRY_OVER`]).
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

    /// THE LIMIT'S CONVERSATION SCOPED BY A DECISION'S READ of `program`'s
    /// screen, `host` there or not ([`Conversation::unread`]). A real host of
    /// Claude Code, whose conversation the loop reads, makes even a legacy
    /// wall require conversation evidence ([`Self::require_limit_conversation`]):
    /// monotone until the wall is ended, never undone by an absent host. On
    /// Muse Code, agy and Gemini CLI ([`screen_alone`]) no read can name it,
    /// so the screen is all the evidence there is, with a host or without: an
    /// unread scope of a limit MET UNDER ONE OF THEM ([`Self::limit_met_under`]),
    /// a row a host-scoped loop wrote included, is the hostless lane's, and
    /// the remembered limit waits for its reset and resumes as there — held
    /// unread, a Muse Code's limit was looked at every five minutes for good.
    /// A LIMIT MET UNDER ANOTHER AGENT ([`met_under_another`]) is not this
    /// agent's to scope: it ends at this decision's fold
    /// ([`TurnEndState::observe`]), holding nothing of this agent's, and
    /// scoped first it was written once more for nothing. So a Claude Code
    /// decision under a host does NOT require a limit met under Muse Code, agy
    /// or Gemini CLI: no read of Claude Code's names such a limit's
    /// conversation. Required, it was written `"conversation":null` for
    /// nothing (the other agent's next decision released it again), and
    /// scoped either way it held Claude Code's tab for good
    /// (`limit-conversation-unread`, a look every five minutes) where it now
    /// ends. Nor does one of those agents' decisions release
    /// Claude Code's unread limit (released, it was written so, and a
    /// hostless loop that followed typed its resume into a conversation no
    /// read confirmed). A row that names no program (an older build's) is
    /// released as before. Codex and a screen no reader names (a shell, a
    /// Claude Code drawing neither its composer frame nor a box) name no
    /// program the limit could be told by, so they keep its scope as it
    /// stands. A named scope is kept either way.
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

    /// THE PROGRAM THE LIMIT WAS MET UNDER: the remembered wall's
    /// ([`WallMemory::program`]), else the limit's track's
    /// ([`TurnEndState::limit_program`]). `None`: neither names one (a row an
    /// older build wrote, or no limit).
    pub(super) fn limit_met_under(&self) -> Option<Program> {
        self.wall_memory
            .as_ref()
            .and_then(|m| m.program)
            .or_else(|| self.turn_end.limit_program())
    }

    /// A real host makes even a legacy wall require conversation evidence:
    /// at the loop's start ([`Self::require_seeded_limit_conversation`]) and
    /// at each decision on Claude Code. Kept until the wall is ended or the
    /// read of an agent whose screen is all the evidence releases it
    /// ([`Self::scope_limit_conversation`]), never undone by an absent host.
    pub(super) fn require_limit_conversation(&mut self, seq: u64) {
        self.rescope_limit(&Conversation::Hostless, &Conversation::Unknown, seq);
    }

    /// A REAL HOST'S LOOP STARTS ON THE WALL IT TOOK UP: its conversation is
    /// required whatever the program (no screen has named the tab's yet;
    /// the first decision's read of an agent whose screen is all the
    /// evidence releases it, [`Self::scope_limit_conversation`]), so a loop
    /// that ends before that decision leaves a hostless successor no legacy
    /// row to act on — but not where the row names the wall met under such an
    /// agent ([`Self::limit_met_under`], [`screen_alone`]): no read could
    /// ever name its conversation, and required here, it was written
    /// `"conversation":null` at every start and released at the first
    /// decision, two rows a loop for nothing. A row naming no program (an
    /// older build's) is required as before.
    pub(super) fn require_seeded_limit_conversation(&mut self) {
        if !self.limit_met_under().is_some_and(screen_alone) {
            self.require_limit_conversation(0);
        }
    }

    /// The limit's track and memory whose conversation is `from` take `to`,
    /// the memory's change persisted.
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
            // Persist scope even when the screen draws no wall. A later
            // hostless successor must not reinterpret this admitted seed, nor
            // any successor hold a scope no read can bind.
            self.persist_wall(seq);
        }
    }

    /// A loop that follows another on the session goes on waiting for the wall
    /// its predecessor waited on ([`approvals::open_wall`]) when the screen
    /// shows none of its rows — the update that applied while the owner's
    /// scroll hid it, or Esc on the vendor's countdown: the policy is handed
    /// the reset the notice named ([`TurnEndState::seed_named_reset`]) —
    /// or, its notice placing none (a spend limit, a bucket that names no
    /// reset), the wall's appearance, its first try's rung counted from there
    /// ([`TurnEndState::seed_unnamed_wall`]). Kept
    /// for [`MEMORY_TTL`] past its reset (a wall with no placed reset, past its
    /// row).
    pub(super) fn seed_wall_memory(&mut self, open: Option<approvals::OpenWall>) {
        if self.wall_memory.is_some() {
            return;
        }
        let Some(open) = open else {
            return;
        };
        let now = self.now_unix();
        let ttl = i64::try_from(MEMORY_TTL.as_secs()).unwrap_or(i64::MAX);
        let since = open.reset_unix.unwrap_or(open.at_ms / 1000);
        if now > since.saturating_add(ttl) {
            return;
        }
        // The kind as the loop that wrote it read it ([`approvals::wall_kind`]:
        // the row's own `kind=`, a bucket named by its `rateLimitType` too).
        let Some(kind) =
            approvals::wall_kind(open.kind, &open.message).filter(|k| k.reads_limited())
        else {
            return;
        };
        let key = format!("{}|{}", kind.name(), open.reset.as_deref().unwrap_or("-"));
        self.wall_reset = Some((key, open.reset_unix));
        if let Some(unix) = open.reset_unix {
            // This session's share of the herd, as a reading of the wall
            // gives it ([`Self::turn_end_reading`]).
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
            // No reset placed (a spend limit, a bucket that names none): the
            // track from where the loop that wrote the row counted the wall's
            // appearance, so the first try's rung counts from there
            // ([`TurnEndState::seed_unnamed_wall`]): the row, the wall's
            // first read after the last act — but where that act was the
            // wall's own retry ([`approvals::OpenWall::retry_ms`]), the
            // point that judged its answer ([`approvals::OpenWall::judged_ms`]),
            // or, none judged yet, the end of the take window the row opened.
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
            saved: Some((open.reset_unix, open.conversation, open.program)),
            written: true,
        });
    }
}

/// How far up the canary's ladder a stall has climbed ([`Session::stuck_busy_note`],
/// [`Session::dialog_note`]): each rung a multiple of [`STUCK_AFTER`] (a third of it for a
/// dialog) under an idle title. The journal at 30 minutes (10 for a dialog), the host's record
/// at 90 (30), a line of the owner's digest at 3 hours (an hour).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum StuckRung {
    /// Nothing said.
    Quiet,
    /// `STUCK` is in the journal.
    Said,
    /// `STALLED` is in the journal and the host has been told.
    Told,
    /// The exception is up.
    Raised,
}

impl StuckRung {
    /// The rung above this one and the multiple of the ladder's unit it needs
    /// ([`STUCK_AFTER`]; a third of it for a dialog), or `None` at the top.
    fn next(self) -> Option<(Self, u32)> {
        match self {
            Self::Quiet => Some((Self::Said, 1)),
            Self::Said => Some((Self::Told, 3)),
            Self::Told => Some((Self::Raised, 6)),
            Self::Raised => None,
        }
    }
}

impl<C: Ctl> Session<'_, C> {
    /// The canary for a screen no reader names: a build that words a wall or a turn's end
    /// differently reads `busy` for as long as a shell runs. The one witness the words do not
    /// depend on is Claude Code's own terminal title, which animates while the agent works and
    /// rests on `✳` when it does not. So a screen that has read busy on background work alone for
    /// [`STUCK_AFTER`] under a resting title gets one `STUCK …` journal line per episode; the
    /// title is asked no more than once per [`STUCK_AFTER`]. It types nothing.
    ///
    /// THE LADDER ABOVE IT (design-exceptions 9.3; the input is this canary, not
    /// screen age: workflow footers tick and would false-alarm): still true at
    /// 90 minutes, `STALLED` to the journal and [`IdleHost::stall_unread`] (a
    /// record and an error line at the host); still true at 3 hours, an
    /// [`Exception::stalled`] (`EXCEPTION … kind=stalled`, a line of the digest:
    /// "open the tab and type keep going"). The title asked again at each rung:
    /// one that moves is a working agent, and the ladder is over. It ends, too,
    /// when the screen stops reading background-only, at the next point and
    /// when the loop ends ([`Self::stuck_over`]).
    ///
    /// AGY'S AND MUSE CODE'S OWN BACKGROUND WORK climbs the same ladder
    /// ([`background_alone`]: their reader names the work under a turn that
    /// ended as evidence). Their readers hold an ended turn, and a question
    /// it ended on over a task or a shell (the hard rule), busy while that
    /// work runs, so a server or a daemon left running would hold the tab
    /// for ever with nothing typed and nothing said; the ladder is the
    /// backstop Claude Code's shell has. No title of theirs is measured: the
    /// witness is their words standing still ([`Witness::Words`],
    /// [`still_words`]), the episode counted from when they last moved; the
    /// host's record is told as their work's ([`StallSeen::OwnWork`]), and
    /// the last rung is [`Exception::stalled_on_work`].
    ///
    /// A BACKGROUND CALL THE KERNEL SAYS MADE NO PROGRESS explains the screen
    /// (the hung-call design's R1: a test that slept on its own mutex for
    /// 26 h, while each rung said nothing here explained it): the host's jobs
    /// reading ([`IdleHost::background_jobs`]) names it, and the ladder goes
    /// to `STALLED` at once in its words, and to [`Exception::no_progress`]
    /// one unit later if it still stands ([`Self::stuck_climb`]).
    /// `since`: when the screen began reading background work only
    /// ([`Self::host_steps_in_background`]'s clock). A busy read, background
    /// work alone included, is also the end of a dialog's episode
    /// ([`Self::dialog_note`]), so a clock left running meets no dialog hours
    /// later.
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
            // agy, Muse Code: the episode is the words' (the work named, the
            // reply, the draft) since they last moved.
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

    /// THE KERNEL'S WORD on the agent's own background work
    /// ([`IdleHost::background_jobs`]): the call the host's jobs reading says
    /// has made no progress, or `None` (no host, no reading, or nothing of
    /// this session's): today's ladder.
    fn no_progress_call(&self) -> Option<super::super::background::BackgroundCall> {
        let host = self.stall_host.as_ref()?;
        match host.background_jobs() {
            super::super::background::BackgroundEvidence::NoProgress { worst } => Some(worst),
            super::super::background::BackgroundEvidence::Unknown => None,
        }
    }

    /// The canary's second flavour, for a dialog no reader names: Claude Code stops at a dialog
    /// this build has no reading of (such as a notice with no options, `Enter to continue · Esc
    /// to cancel`), and the screen reads `idle` with no composer and nothing busy for as long as
    /// it stands. The evidence is the screen's unchanged words ([`Self::dialog_words`]); the
    /// witness, asked only when a rung is due, is the agent's title resting on `✳` and `status`
    /// naming Claude as the tab's program.
    ///
    /// The ladder is the busy flavour's at a third of its unit ([`DIALOG_DIVISOR`]), its top rung
    /// [`Exception::dialog`] asking the owner to answer it in the tab. Nothing is typed: a key
    /// aterm cannot place answers the wrong dialog. Called at the top of each wait step
    /// ([`Self::wait_for_next`]) and at each read of a screen that never holds still
    /// ([`Self::await_turn_from`]); hosted watch only, as the stall probe is.
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
            // A screen new to the clock, or another dialog on it: its own
            // episode, from now. What the ladder still held under the screen
            // before (a busy flavour's rungs) is over with it.
            _ => {
                self.stuck_over(screen.seq, "the screen changed", review);
                self.stuck_dialog = Some((words, self.fx.mono()));
            }
        }
    }

    /// The words of a dialog no reader names on `screen` ([`Self::dialog_note`]), or `None`: a
    /// screen Claude Code's grammar reads `idle` without evidence (no composer, shell-mode frame,
    /// busy row, question, box, wall or survey card), that nothing else of the loop's owns (a held
    /// stall, its own panel or reset dialog, a point already told: the host keeps one exception
    /// per tab), and that has words: the letters of its last [`KEY_ROWS`] non-blank rows
    /// ([`push_words_above`]), so a ticking clock is no change and a sentence added is one.
    pub(super) fn dialog_words(&self, screen: &Screen) -> Option<String> {
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
        let mut words = String::new();
        push_words_above(&mut words, rows);
        let words = words.trim();
        (!words.is_empty()).then(|| words.to_string())
    }

    /// One climb of the canary's ladder for the episode that began at `at`
    /// ([`Self::stuck_busy_note`], [`Self::dialog_note`]): when the next rung
    /// is due — its multiple of `after`, the title not asked within `after`
    /// — one `status` read is its witness ([`Witness::Title`]; agy's and Muse
    /// Code's words, [`Witness::Words`], were read on the screen by the
    /// caller), and the rung is said in `kind`'s and the witness's words. A
    /// title that moves is an agent at work, and, for a dialog, a tab that
    /// runs no Claude (a shell left in a swapped tab keeps the title it had)
    /// has none to wait for: either ends the episode.
    ///
    /// THE KERNEL'S WORD (the busy flavour's: a dialog runs no background
    /// work), asked of the host, once the break has stood the host's own
    /// break settle, at each climb the title's own spacing allows (none
    /// within `after` of the last title read; past that, at every busy read
    /// until the next rung is said): a background call of the agent's own
    /// that the host's jobs reading says has made no progress
    /// ([`Self::no_progress_call`]) explains the screen. The ladder then
    /// climbs as the design's `AllStuck` does: to `STALLED` at once, naming
    /// the call (`STUCK`, which says no reader names the screen, is not
    /// said), and to the exception one `after` past `STALLED` if that same
    /// call still stands, in its words; another call (its group differs) is
    /// named at `STALLED` in its turn and waits its own unit. Without it (no
    /// reading, or refused), the rungs and their words are today's.
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
        if self.stuck_asked.is_some_and(|t| self.fx.since(t) < after) {
            return;
        }
        let dialog = kind == ExceptionKind::Dialog;
        // Asked once the break has stood the settle the host's own break
        // waits for ([`IdleHost::at_background`]'s `background_settle`): a
        // turn that has just ended over a shell (a rating card about to
        // come up, a notice about to wake the agent) is no stall to explain.
        let call = if dialog || self.fx.since(at) < self.setup.background_settle {
            None
        } else {
            self.no_progress_call()
        };
        let (next, due) = match &call {
            Some(_) if next <= StuckRung::Told => (StuckRung::Told, true),
            // A call other than the one `STALLED` named (or one where it
            // named none): it is named in its turn, and its exception waits
            // its own unit.
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
        // The words' witness was read by the caller on this screen
        // ([`Self::stuck_busy_note`]): no `status` is asked for it.
        let status = match witness {
            Witness::Words(_) => String::new(),
            Witness::Title => {
                self.stuck_asked = Some(self.fx.mono());
                let Ok(r) = self.call(&["status"]) else {
                    return;
                };
                if !title_rests(&r.stdout) {
                    // A title that moves is an agent at work: whatever the
                    // ladder said, it is over.
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
            // The kernel explains the screen: the call is named, in the
            // journal, to the host's record and in the exception.
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
            StuckRung::Said if dialog => {
                let agent = crate::supervise::screen::status_field(&status, "agent");
                review.note(&format!(
                    "STUCK seq={} the agent's own title says idle, and the screen has stood \
                     unchanged for {minutes} min with no composer, box, wall or busy row \
                     (agent={}): no reader here names it. If the tab is stalled, this is a \
                     dialog this build does not read, and only a person can answer it. rows: {}",
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
            StuckRung::Told => {
                self.stuck_told = Some((self.fx.mono(), None));
                let what = match witness {
                    _ if dialog => "a dialog this build does not read",
                    Witness::Title => "a wait this build does not classify",
                    Witness::Words(_) => "background work of the agent's own that has not ended",
                };
                review.note(&format!(
                    "STALLED seq={} still so after {minutes} min: {what}, told to the window as \
                     a record",
                    screen.seq,
                ));
                // The host's record speaks in the witness's words: agy's and
                // Muse Code's own work is read, and no title of theirs is (a
                // dialog's witness is Claude Code's title).
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
                    _ if dialog => Exception::dialog(),
                    Witness::Title => Exception::stalled(),
                    Witness::Words(_) => Exception::stalled_on_work(),
                };
                self.raise_stuck(screen.seq, &exception, kind, review);
            }
        }
    }

    /// The ladder's third rung: `exception` journaled (`EXCEPTION seq=<n>
    /// kind=<k> when=<w> <why> => <action>`), told to the host, and kept as
    /// raised for its end's `CLEARED` line ([`Self::stuck_over`]).
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

    /// THE STALL NO READER NAMED IS OVER (or never was): the screen moved on,
    /// the title moves, a point was reached, the loop ended. What the ladder
    /// told ends with it: the record's host is told `None`, and the raised
    /// exception journals `CLEARED seq=<n> exception=<stalled|dialog>
    /// after=<s>s <cause>` and is withdrawn from the host. Quiet where nothing
    /// was said. A dialog's clock ([`Self::dialog_note`]) and the words' clock
    /// ([`Witness::Words`]) end with it, so every end the busy flavour has is
    /// the dialog's too.
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
            // The host keeps one exception per tab: a point that stood under the stall is told
            // again.
            if let Some(host) = &self.stall_host {
                host.exception(self.exception_open.as_ref().map(|(open, _)| open));
            }
        }
    }
}

/// Whether a `status` reply says Claude Code's own title rests: its source is
/// the program's own (`osc`, not a title the owner pinned) and it opens with
/// the idle glyph ([`IDLE_TITLE_GLYPH`]).
fn title_rests(stdout: &str) -> bool {
    let field = |f: &str| crate::supervise::screen::status_field(stdout, f);
    field("subject_source") == Some("osc")
        && field("subject")
            .map(crate::supervise::report::pct_decode)
            .is_some_and(|t| t.trim_start().starts_with(IDLE_TITLE_GLYPH))
}

/// Whether a `status` reply names Claude Code as the tab's foreground program
/// (`program=`), read fresh: the screen of a dialog has no frame to say it,
/// and a shell left in a swapped tab, Codex or a foreground `vim` are none.
fn names_claude(stdout: &str) -> bool {
    match crate::supervise::screen::status_field(stdout, "program")
        .and_then(aterm_phase::program_of)
    {
        Some(Program::Claude) => true,
        Some(
            Program::Codex | Program::Agy | Program::Muse | Program::Gemini | Program::Generic,
        )
        | None => false,
    }
}

/// How many characters of a screen's last rows the dialog flavour's journal
/// line carries ([`tail_rows`]).
const TAIL_ROWS_CHARS: usize = 400;

/// The last five non-blank rows of `rows`, trimmed, joined by ` ⏎ ` (a control character a
/// space): what a reader is taught the dialog by. Cut at [`TAIL_ROWS_CHARS`] from the front
/// (`…`), since a dialog is named by its footer and options, its last rows.
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
