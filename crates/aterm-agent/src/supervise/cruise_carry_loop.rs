// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supervision-loop seam of Cruise's carry at a break of the agent's own background work
//! ([`super::super::policy::cruise_carry`]), and of the needs such a break's report names.

use std::time::{Duration, Instant};

use aterm_types::control_verbs::TURN_SUBMIT_BUSY_REPLY;

use super::super::config::{Autopilot, SupervisorConfig};
use super::super::policy::cruise_carry::{
    BackgroundCarries, RULE_BACKGROUND, RULE_BACKGROUND_RESET, carry_words, recovery_words,
};
use super::super::policy::go_ahead::{self, RULE_GO_AHEAD};
use super::super::policy::needs;
use super::super::policy::turn_end::{
    GO_AHEAD_REFUSED, GO_AHEAD_WAIT, GoAheadVerdict, TurnEndAction, TurnEndReading, TurnEndTiming,
    go_ahead_answer, go_ahead_answers_on, needs_words,
};
use super::approvals::Outcome;
use super::turn_end_loop::Written;
use super::*;
use crate::harness::upgrade::HARNESS_MARK;

/// Cruise's carries at breaks: their pacing books and what the journal last said of them.
#[derive(Debug, Default)]
pub(super) struct CarryWatch {
    /// When the last carry went and the streak it set ([`BackgroundCarries`]).
    books: BackgroundCarries,
    /// The turn read live since the last settled break or point, which the next settled break
    /// judges ([`Self::at_break`]).
    opened: Option<Opened>,
    /// When the last carry was typed, until the next settled break: a turn read live before then
    /// is its answer, its work counted from the typing.
    carried: Option<Instant>,
    /// The person's count (`human_seq`, the `if-human=` fence) the last carry was typed under.
    count: Option<u64>,
    /// The session's submissions (`submits`) where the last turn was judged or the last carry
    /// typed, whichever came later: a submission past them but the harness's own is someone
    /// else's message.
    submits: Option<u64>,
    /// The harness's own submissions since [`Self::submits`] was read: the loop's
    /// ([`Self::wrote`]), its lines' and its answers to the boxes a turn raised, and the host's
    /// lines ([`Self::host_typed`]).
    keyed: u64,
    /// A key the loop wrote since its own last Enter, which that Enter submits ([`submits_of`]).
    /// Another hand's keys and Enters move the host's own unseen: its Enter between the loop's key
    /// and the loop's Enter leaves the loop's submitting nothing, so one message beside it goes
    /// unseen.
    composed: bool,
    /// What the journal last said of a carry not typed: once a change.
    said: Option<CarrySaid>,
    /// When the last carry of a loop before this one was typed ([`Self::seed`]), until this loop's
    /// first busy read: the counts that carry was typed under were that loop's.
    owed: Option<Instant>,
    /// A loop before this one left the books ([`Self::seed`]), until this loop's first settled
    /// break: a turn read live before then may have begun before this loop did, a break short of
    /// its settle a pause inside it, so where it began went unread ([`Opened::unread`]).
    unseen_start: bool,
    /// The turn a settled break read whole from the history (`Session::break_turn`), by the mark
    /// of the words its screen shows: read once while they stand, for that turn only. A new turn
    /// read live ([`Self::live`]), a judged point and a carry typed forget it, so a later turn
    /// whose screen shows the same words (an agent that repeats its report under tool rows) is
    /// read whole again, never judged on an earlier turn's messages (2026-10-30 review 9).
    turn_read: Option<(u64, Option<TurnWords>)>,
    /// The go-ahead the read of a settled Cruise break judged before any of its lines
    /// ([`Session::break_go_ahead`]), which that read's carry types or holds
    /// ([`Session::cruise_carry`]); `None` from the start of every read until it is judged.
    judged: Option<BreakPark>,
}

/// The go-ahead a settled Cruise break's read judged ([`Session::break_go_ahead`]).
#[derive(Debug, Clone)]
struct BreakPark {
    /// The turn's park, past an irreversible ask and the person's hold: the go-ahead answers it
    /// where answers are on.
    parked: Option<go_ahead::Park>,
    /// [`Self::parked`] with answers on.
    park: Option<go_ahead::Park>,
    /// The continuation owed in the carry's place (a `/compact`'s,
    /// [`super::super::policy::turn_end::TurnEndState::owed_at_break`]).
    owed: Option<&'static str>,
    /// The routine answer budget is spent: use forward recovery on the carry's ladder.
    spent: bool,
}

impl BreakPark {
    /// The park the go-ahead answers in the carry's place, on the carry's ladder: none where
    /// the bound is spent or a continuation is owed.
    fn answers(&self) -> Option<&go_ahead::Park> {
        self.park
            .as_ref()
            .filter(|_| !self.spent && self.owed.is_none())
    }

    /// What the break does with the go-ahead's lines (DESIGN-harness-autopilot §15.6).
    fn verdict(&self) -> GoAheadVerdict {
        if self.spent {
            return GoAheadVerdict::Refused;
        }
        self.answers().map_or(GoAheadVerdict::Other, |park| {
            GoAheadVerdict::Answers(park.needs.clone())
        })
    }
}

/// The turn a settled break ends, as a point reads it ([`Session::break_turn`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TurnWords {
    /// Its last message's lines and its earlier messages' lines, each line the program wrapped
    /// read as one (`said_lines`, `turn_lines_before`).
    lines: (Option<String>, Option<String>),
    /// Its last message and its earlier messages as drawn (`said_tail`, `turn_said_before`), as a
    /// turn end's reading holds them: the irreversible guard reads the last as drawn too
    /// ([`go_ahead_answers_on`]).
    said: (Option<String>, Option<String>),
    /// Where each earlier message after the first opens in `lines.1` and `said.1`
    /// ([`needs::Breaks`]).
    breaks: needs::Breaks,
    /// It opens cut: the act's own rows may head it.
    opens_cut: bool,
}

impl TurnWords {
    /// THE WORDS THE NEEDS FOLD AND THE GO-AHEAD'S PARK BOTH READ, `(before, tail)`: each message
    /// as its measured lines where they hold the words it draws, else as drawn, its earlier
    /// messages told apart ([`needs_words`]), as a point reads its turn
    /// ([`TurnEndReading::needs_said`]). So a line the go-ahead answers at a break is the line
    /// the fold stands, never a row of one.
    fn needs_said(&self) -> (Option<needs::Before<'_>>, Option<&str>) {
        needs_words(
            (self.said.0.as_deref(), self.said.1.as_deref()),
            (self.lines.0.as_deref(), self.lines.1.as_deref()),
            &self.breaks,
        )
    }
}

/// A turn read live since the last settled break or point.
#[derive(Debug, Clone, Copy)]
struct Opened {
    /// The harness's own line opened it (`TurnEndState::typed_untaken`).
    ours: bool,
    /// It opened and counts from the carry's typing ([`CarryWatch::carried`]) until its first
    /// read, which may draw a wake that opened it ([`CarryWatch::read`]).
    from_typing: bool,
    /// A read of it drew someone else's prompt row newest ([`newest_prompt_ours`]): a message no
    /// key in this window typed (Claude Code's Remote Control, another driver) or a person's.
    theirs: bool,
    /// When it opened: the carry's typing, for the carry's answer, the first read of the
    /// program's own wake that opened it in the answer's place ([`CarryWatch::read`]), else its
    /// first live read.
    from: Instant,
    /// Its live stretches' busy time so far, each from its live edge (the carry's typing, for the
    /// carry's answer) to the start of the break it reached.
    busy: Duration,
    /// When the stretch running now began: `None` while the turn stands at a break short of its
    /// settle, which is no work.
    since: Option<Instant>,
    /// Its work is unknown: a break read as begun before the stretch then running (the loop
    /// stepped no break while it ran: under Off), where that stretch ended went unread, or it was
    /// read live before this loop's first settled break on books a loop before it left, where it
    /// began did ([`CarryWatch::seed`]; unless a wake opened it since, [`Self::wake_tells`]).
    unread: bool,
    /// It is unread for where it began, and opened after this loop's first read found a break,
    /// until its first read: one that draws the program's own wake newest began there, its start
    /// read ([`CarryWatch::read`]).
    wake_tells: bool,
}

/// Why the books started again where they held a carry, as the row that says it gives it
/// ([`Session::books_again`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Again {
    /// A turn of real work ([`BackgroundCarries::ended`]).
    Work,
    /// Someone else's message.
    Theirs,
    /// A turn whose work went unread ([`Opened::unread`]).
    Unread,
    /// The person's gesture after a loop before this one's last carry ([`CarryWatch::read`]).
    Touched,
}

impl Again {
    /// The reason its row gives.
    const fn reason(self) -> &'static str {
        match self {
            Again::Work => "a turn of real work started Cruise's background ladder again",
            Again::Theirs => "someone else's message started Cruise's background ladder again",
            Again::Unread => {
                "a turn whose work went unread started Cruise's background ladder again"
            }
            Again::Touched => {
                "the person's gesture after the last carry started Cruise's background ladder \
                 again"
            }
        }
    }
}

/// Why a due carry was not typed, as the journal says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CarrySaid {
    /// This guard holds it.
    Held(Guard),
    /// The streak's back-off runs to this time.
    Waits(Instant),
    /// The hour's typing budget (`continue_per_hour`) is spent.
    Budget,
    /// The break parks on a go-ahead its limit of acts already answered
    /// ([`super::super::policy::turn_end::TurnEndState::go_ahead_spent`]): nothing more goes.
    Refused,
    /// The write's duty is not this loop's to carry out ([`Session::duty_withheld`]): another
    /// supervisor holds it, or the host's word on who does could not be read.
    Duty(Scope),
}

impl CarryWatch {
    /// A read of a live turn (`Session::took`): the first since the last settled break or point
    /// opens a turn (`ours` when the harness's own line opened it; unread where it may have begun
    /// before this loop, [`Self::seed`]), the first after a pause inside it starts its next
    /// stretch, each at `at` (read only then), and the settled break that ends it judges it
    /// ([`Self::at_break`]).
    pub(super) fn live(&mut self, ours: bool, at: impl FnOnce() -> Instant) {
        match self.opened.as_mut() {
            None => {
                let unread = std::mem::take(&mut self.unseen_start);
                let from = self.carried.unwrap_or_else(at);
                self.opened = Some(Opened {
                    ours,
                    from_typing: self.carried.is_some(),
                    theirs: false,
                    from,
                    busy: Duration::ZERO,
                    since: Some(from),
                    unread,
                    wake_tells: unread && self.owed.is_none(),
                });
                self.turn_read = None;
            }
            Some(opened) if opened.since.is_none() => opened.since = Some(at()),
            Some(_) => {}
        }
    }

    /// A LOOP BEFORE THIS ONE'S LADDER: its last carry typed `ago` before `now`, the `streak`-th
    /// in a row. The counts it was typed under were that loop's, so this loop's first busy read
    /// stands for them ([`Self::read`]): a turn that opens after this loop's first settled break is
    /// judged by its busy time unless someone else's message lands in it after that read, never as
    /// a person's for counts this loop never saw. A TURN RUNNING AT THE RESTART IS UNREAD: one read
    /// live before that break may have begun before this loop, so its work and a message another
    /// driver sent it then are unknown, and its break starts the books again ([`Self::at_break`]).
    /// A break this loop has not yet seen settle may be a pause inside such a turn, or one that
    /// stood for minutes; a turn opened after the first read found one is unread unless the
    /// program's own wake its first read draws opened it there ([`Self::read`]: Claude Code's).
    pub(super) fn seed(&mut self, ago: Duration, streak: u32, now: Instant) {
        let Some(last) = now.checked_sub(ago) else {
            return;
        };
        self.books = BackgroundCarries::carried(last, streak);
        self.owed = Some(last);
        self.unseen_start = true;
    }

    /// A busy read on `screen`, at `at` (read only for the first after a seed, or of a turn): an
    /// open turn whose newest prompt row is not the harness's has someone else's message in it.
    /// The first read of a turn counted from the carry's typing that draws the program's own wake
    /// newest ([`opened_by_wake`]) opens it there and counts it from there: the carry's answer was
    /// over unseen. The first read that draws it of a turn opened after a seeded loop's first read
    /// found a break reads where that turn began: the wake opened it since, so its busy time
    /// judges it ([`Opened::wake_tells`]). The first after a seed stands for the counts the last
    /// carry was typed under, but not where the person's last gesture the host reports came after
    /// that carry: the seeded books start again even if their turn already reached its break
    /// before the restart, so no live edge was read. The counts stay unknown and a live turn not
    /// the harness's is a person's ([`Self::at_break`]). Returns why the books started again where
    /// they held a carry.
    #[must_use]
    pub(super) fn read(
        &mut self,
        reader: &dyn ScreenReader,
        screen: &Screen,
        at: impl Fn() -> Instant,
    ) -> Option<Again> {
        let mut again = None;
        if let Some(last) = self.owed.take() {
            let touched = match screen.human {
                HumanInput::Ago(ms) => {
                    Duration::from_millis(ms) < at().saturating_duration_since(last)
                }
                HumanInput::Unknown | HumanInput::Never | HumanInput::NeverFor(_) => false,
            };
            if touched {
                again = self.start_again(Again::Touched, BackgroundCarries::reset);
            } else {
                self.count = screen.human_seq;
                self.submits = screen.submits;
                self.keyed = 0;
            }
        }
        if let Some(opened) = self.opened.as_mut() {
            if std::mem::take(&mut opened.from_typing) && opened_by_wake(reader, &screen.rows) {
                let woke = at();
                opened.from = woke;
                opened.since = Some(woke);
            }
            if std::mem::take(&mut opened.wake_tells) && opened_by_wake(reader, &screen.rows) {
                opened.unread = false;
            }
            if !opened.theirs {
                opened.theirs = newest_prompt_ours(reader, &screen.rows) == Some(false);
            }
        }
        again
    }

    /// A TURN ENDS ONLY AT A SETTLED BREAK: a read on `screen` of a break of the agent's own
    /// background work that began at `began` ends the open turn's stretch there. Unsettled, it is
    /// a pause inside the turn (a reply's text drawn between tool calls under a status line), and
    /// the pause is no work. Settled ([`background_settle`]) with no turn open, it ends the last
    /// carry's claim and a loop before this one's ([`Self::seed`]): an answer or a turn no read saw
    /// is over, and the next turn counts from its own live edge. Settled with one open, it ends
    /// the turn, which is judged:
    /// - SOMEONE ELSE'S MESSAGE starts the books again, whoever opened the turn: a submission
    ///   since the last judged turn or carry that is not the harness's own (a person's Enter,
    ///   another driver's `send`; not one the harness submitted, [`Self::keyed`]: an Enter the
    ///   loop wrote over a key it pressed, its answer to a box the turn raised among them, or the
    ///   host's line), or a prompt row not the harness's ([`Opened::theirs`]: Remote Control's).
    ///   A scroll, a click or a key that submits nothing is no message. Where a submission count
    ///   is unknown, the person's count (any gesture) moved since the last carry, or unknown on a
    ///   turn not the harness's, is a person's turn.
    /// - A turn whose work went unread ([`Opened::unread`]: a stretch's end, or its start before
    ///   this loop's) starts them again: unsure.
    /// - THE PROGRAM'S OWN WAKE keeps them, as the harness's own line does: REAL WORK IS BUSY
    ///   TIME, the turn's stretches summed, and a turn of real work starts them again while a
    ///   short one leaves them ([`BackgroundCarries::ended`]).
    ///
    /// Returns why the books started again where they held a carry, and, settled, when the turn
    /// the break ends opened ([`Opened::from`]), which bounds the person's key it holds the
    /// go-ahead for ([`super::super::policy::turn_end::TurnEndState::break_keyed`]), and its busy
    /// time, whose real work starts the go-ahead's span again
    /// ([`super::super::policy::turn_end::TurnEndState::go_ahead_worked`]); `None` unsettled, or
    /// with no turn read live since the last settled break or point.
    #[must_use]
    pub(super) fn at_break(
        &mut self,
        timing: &TurnEndTiming,
        screen: &Screen,
        began: Instant,
        settled: bool,
    ) -> (Option<Again>, Option<(Instant, Duration)>) {
        let Some(mut turn) = self.opened.take() else {
            if settled {
                self.carried = None;
                self.unseen_start = false;
            }
            return (None, None);
        };
        if let Some(since) = turn.since.take() {
            turn.unread |= began < since;
            turn.busy = turn
                .busy
                .saturating_add(began.saturating_duration_since(since));
        }
        if !settled {
            self.opened = Some(turn);
            return (None, None);
        }
        self.carried = None;
        let someone = turn.theirs
            || match (screen.submits, self.submits, screen.human_seq, self.count) {
                (Some(now), Some(then), ..) => now > then.saturating_add(self.keyed),
                (.., Some(now), Some(then)) => now != then,
                _ => !turn.ours,
            };
        self.submits = screen.submits;
        self.keyed = 0;
        if someone || turn.unread {
            let why = if someone {
                Again::Theirs
            } else {
                Again::Unread
            };
            let again = self.start_again(why, BackgroundCarries::reset);
            return (again, Some((turn.from, turn.busy)));
        }
        (
            self.start_again(Again::Work, |books| books.ended(timing, turn.busy)),
            Some((turn.from, turn.busy)),
        )
    }

    /// Starts the books again as `start` does, for `why`, which it returns where that moved them:
    /// they held a carry.
    fn start_again(
        &mut self,
        why: Again,
        start: impl FnOnce(&mut BackgroundCarries),
    ) -> Option<Again> {
        let held = self.books;
        start(&mut self.books);
        (self.books != held).then_some(why)
    }

    /// A judged point: the policy carries the session from here, and the next break is carried
    /// at its settle again.
    pub(super) fn point(&mut self) {
        self.opened = None;
        self.carried = None;
        self.books.reset();
        self.said = None;
        self.turn_read = None;
    }

    /// A write the loop made, `args`, answered `reply`: an Enter the server took over a key the
    /// loop pressed since its last is the harness's own submission ([`submits_of`]).
    pub(super) fn wrote(&mut self, args: &[&str], reply: &CtlReply) {
        let (submitted, composed) = submits_of(args, reply, self.composed);
        self.keyed = self.keyed.saturating_add(submitted);
        self.composed = composed;
    }

    /// The host typed a line of its own and submitted it, apart from the loop's writes
    /// (`IdleHost::at_background`, a fact's or a point's step, a relaunch's carry-on): the
    /// harness's own submission.
    pub(super) fn host_typed(&mut self) {
        self.keyed = self.keyed.saturating_add(1);
        self.composed = false;
    }

    /// The carry was typed at `now` at a break read on `screen`, the harness's submissions
    /// `before` it ([`Self::keyed`]): those since are the carry's.
    fn typed(&mut self, now: Instant, screen: &Screen, before: u64) {
        self.books.typed(now);
        self.carried = Some(now);
        self.count = screen.human_seq;
        self.submits = screen.submits;
        self.keyed = self.keyed.saturating_sub(before);
        self.said = None;
        self.turn_read = None;
    }
}

/// What a write of the loop's (`args`, answered `reply`) adds to the session's submissions as the
/// host counts them (`submits`: an Enter over a key pressed since the last), `composed` whether
/// the loop pressed one since its own last Enter; returns that and whether one stands pressed
/// after it. A `key` the server took is an Enter or a key pressed; a `send` it took is its text,
/// a newline at its end an Enter; a `turn` that pressed its Enter submits its text, and one that
/// typed it and pressed none leaves it pressed. Anything else, a write refused or skipped among
/// them, changes nothing.
pub(super) fn submits_of(args: &[&str], reply: &CtlReply, composed: bool) -> (u64, bool) {
    let took = reply.ok() && !reply.skipped();
    let last = args.last().copied().unwrap_or_default();
    match args.iter().find(|a| !a.starts_with('@')).copied() {
        Some("key") if took && last == "enter" => (u64::from(composed), false),
        Some("key") if took => (0, true),
        Some("send") if took => {
            let line = last.strip_suffix("\\n").or_else(|| last.strip_suffix('\n'));
            // Each CR submits what was pressed since the last, as the host counts raw bytes.
            line.unwrap_or(last)
                .bytes()
                .chain(line.map(|_| b'\r'))
                .fold((0, composed), |(n, composed), b| {
                    if b == b'\r' {
                        (n.saturating_add(u64::from(composed)), false)
                    } else {
                        (n, true)
                    }
                })
        }
        Some("turn") if reply.submitted() || reply.pressed_unverified() => (1, false),
        Some("turn")
            if reply.turn_verdict().is_some()
                || reply.err_text().starts_with(TURN_SUBMIT_BUSY_REPLY) =>
        {
            (0, true)
        }
        _ => (0, composed),
    }
}

/// Whether the program's own wake opened the turn drawn last on `rows` (Claude Code's
/// `⏺ Background command "…" completed` or a monitor's event under the newest prompt row,
/// [`aterm_phase::opened_by_own_wake`]): never for another program.
fn opened_by_wake(reader: &dyn ScreenReader, rows: &[String]) -> bool {
    match reader.program() {
        Program::Claude => aterm_phase::opened_by_own_wake(rows) == Some(true),
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Generic => false,
    }
}

/// Whether the newest prompt row of Claude Code's transcript on `rows` (`❯ ` at its left edge,
/// above the live zone) is a line of the harness's: `None` for another program, or with none in
/// view. A wake draws no prompt row of its own (`⏺ Background command "…" completed`), so the
/// newest one stays the carry's.
fn newest_prompt_ours(reader: &dyn ScreenReader, rows: &[String]) -> Option<bool> {
    match reader.program() {
        Program::Claude => {
            let end = reader.transcript_end(rows).min(rows.len());
            rows[..end].iter().rev().find_map(|row| {
                let said = row.strip_prefix('❯')?;
                said.starts_with(char::is_whitespace)
                    .then(|| said.trim_start().starts_with(HARNESS_MARK))
            })
        }
        Program::Codex
        | Program::Grok
        | Program::Kimi
        | Program::Agy
        | Program::Muse
        | Program::Generic => None,
    }
}

impl<C: Ctl> Session<'_, C> {
    /// A busy read of a background break on `screen` ([`CarryWatch::read`]), and the row books it
    /// started again owe ([`Self::books_again`]).
    pub(super) fn carry_read(&mut self, reader: &dyn ScreenReader, screen: &Screen) {
        let again = self.cruise_carry.read(reader, screen, || self.fx.mono());
        self.books_again(again, screen.seq);
    }

    /// THE BOOKS STARTED AGAIN ARE WRITTEN DOWN: where they held a carry, a row says why
    /// ([`RULE_BACKGROUND_RESET`]), so a later loop's replay of the ladder starts after it
    /// ([`super::approvals::background_carries_in`]) though no carry was typed since. A judged
    /// point's own row ends that replay too ([`CarryWatch::point`]).
    fn books_again(&mut self, again: Option<Again>, seq: u64) {
        if let Some(why) = again {
            self.ledger_row(
                RULE_BACKGROUND_RESET,
                Outcome::Waiting,
                "",
                why.reason(),
                seq,
            );
        }
    }

    /// THE BREAK THAT BEGAN AT `at`, read on `screen` ([`CarryWatch::at_break`]), and the row
    /// books it started again owe ([`Self::books_again`]). Settled, the real work of the turn it
    /// ends starts the go-ahead's span again
    /// ([`super::super::policy::turn_end::TurnEndState::go_ahead_worked`]), and it reads the
    /// person's last key as a turn end does
    /// ([`super::super::policy::turn_end::TurnEndState::break_keyed`]), bounded by when the turn
    /// the break ends opened: the window's measured typing (`human_typed_ms`, from the last
    /// `status` or the screen) or a driver's hand, never a draft (`Session::key_ago`). Nothing the harness
    /// types stamps a key, so its own carries and go-aheads never renew the hour.
    pub(super) fn break_judged(&mut self, screen: &Screen, at: Instant, settled: bool) {
        let (again, turn) = self
            .cruise_carry
            .at_break(&self.turn_end.timing, screen, at, settled);
        self.books_again(again, screen.seq);
        if !settled {
            return;
        }
        if let Some((_, busy)) = turn {
            self.turn_end.go_ahead_worked(busy, false);
        }
        let opened = turn.map(|(from, _)| from);
        let now = self.fx.mono();
        if let Some(key) = self
            .key_ago(now, screen)
            .and_then(|ago| now.checked_sub(ago))
        {
            self.turn_end.break_keyed(key, at, opened, now);
            self.book_go_ahead_hour(screen.seq);
        }
    }

    /// CRUISE NEVER PARKS AT A BREAK OF THE AGENT'S OWN BACKGROUND WORK:
    /// where nothing else went at the break that began at `at` (`line_gone`: a
    /// wake's, the host's, the stuck fact's or the door's line), it has settled
    /// ([`background_settle`]) and the carry is due ([`BackgroundCarries::due`]), types
    /// [`BACKGROUND_CARRY`] under Cruise, after every guard a line at a break passes but the
    /// person's own exchange and a manager's wait, which hold no Cruise session: a brake (a stall,
    /// a limit episode, an act or line untaken, the wake's settle, a credits hold), a person
    /// typing or another driver's hand, a halt, a draft or unread input, a session nobody asked
    /// anything, the memory wall, the API's route down, the host's withhold, and the write's
    /// duty where another supervisor holds it ([`Self::duty_withheld`]). Off and Land type
    /// nothing here, by the mode the loop last read and again by the one read just before the
    /// write ([`Self::autopilot_at_busy`]). A `/compact`'s continuation owed is no act untaken:
    /// it is typed in the carry's place, as the policy's act
    /// ([`super::super::policy::turn_end::TurnEndState::owed_at_break`]), ahead of the go-ahead
    /// and of a spent go-ahead's change of approach, which wait for a later break
    /// ([`BreakPark::answers`], [`BreakPark::verdict`]). Else, where the break's
    /// turn parks its own reversible work on a person's go-ahead (read as a turn end reads it,
    /// [`super::super::policy::turn_end::TurnEndState::go_ahead_at_break`]) and answers are on,
    /// the owner's standing go-ahead ([`RULE_GO_AHEAD`]) goes in the carry's place, on the
    /// carry's ladder with its wait at most [`GO_AHEAD_WAIT`], as at a point, unless a person's
    /// turn holds it
    /// ([`super::super::policy::turn_end::TurnEndState::go_ahead_held`]); once its bound is spent
    /// the carry changes approach without asking the person. With answers
    /// off the carry, or the continuation owed in its place, goes without its sentence that the
    /// go-ahead is no need over the park the person answers themselves. The go-ahead is the
    /// one the read judged before any of the break's lines ([`Self::break_go_ahead`]). Returns
    /// whether it typed.
    pub(super) fn cruise_carry(
        &mut self,
        screen: &Screen,
        (at, waited): (Instant, Duration),
        line_gone: bool,
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let Some(cfg) = cfg else {
            return Ok(false);
        };
        if cfg.autopilot_for(self.autopilot.word()) != Autopilot::Cruise
            || !cfg.continue_policy
            || line_gone
            || waited < self.setup.background_settle
            || !review.unattended()
        {
            return Ok(false);
        }
        let Some(host) = self.stall_host.clone() else {
            return Ok(false);
        };
        // The go-ahead this read judged before any line went ([`Self::break_go_ahead`]).
        // A spent go-ahead changes the approach; the normal carry keeps working.
        let Some(judged) = self.cruise_carry.judged.take() else {
            return Ok(false);
        };
        let seq = screen.seq;
        let now = at + waited;
        // The lines the go-ahead answers are off the menu bar from the judgment, its ladder's
        // wait included, unless the go-ahead is not written (DESIGN-harness-autopilot §15.6). That
        // wait is at most [`GO_AHEAD_WAIT`] from the last carry, as a point's is from the last act
        // typed, so no line is held back longer at a break.
        let cap = if judged.answers().is_some() {
            GO_AHEAD_WAIT
        } else {
            Duration::MAX
        };
        let BreakPark {
            parked,
            park,
            owed,
            spent,
        } = judged;
        let park = park.filter(|_| !spent);
        let due = self
            .cruise_carry
            .books
            .due_within(&self.turn_end.timing, at, cap);
        if now < due {
            self.carry_said(seq, CarrySaid::Waits(due), review);
            return Ok(false);
        }
        let cap = usize::try_from(cfg.continue_per_hour).unwrap_or(usize::MAX);
        if cap != 0 && self.turn_end.acts_in_window(now) >= cap {
            self.turn_end.go_ahead_unwritten();
            self.carry_said(seq, CarrySaid::Budget, review);
            return Ok(false);
        }
        let guards = match self.write_guards(screen, now, cfg, host.as_ref())? {
            Ok(guards) => guards,
            Err(guard) => {
                self.turn_end.go_ahead_unwritten();
                self.carry_said(seq, CarrySaid::Held(guard), review);
                return Ok(false);
            }
        };
        let brake = match (guards.brake, owed) {
            (Some(Guard::ActUntaken), Some(_)) => {
                self.occupancy_brake(now, cfg, &screen.rows, false)
            }
            (brake, _) => brake,
        };
        let held = brake
            .or_else(|| {
                [
                    (Guard::Person, guards.person),
                    (Guard::Halted, guards.halted),
                    (Guard::Draft, guards.draft),
                    (Guard::Taskless, guards.taskless),
                    (Guard::Memory, guards.memory),
                    (Guard::Reach, guards.reach_down),
                ]
                .into_iter()
                .find_map(|(guard, holds)| holds.then_some(guard))
            })
            .or_else(|| {
                Self::write_at(host.as_ref())
                    .withheld
                    .then_some(Guard::HostWithheld)
            });
        if let Some(guard) = held {
            self.turn_end.go_ahead_unwritten();
            self.carry_said(seq, CarrySaid::Held(guard), review);
            return Ok(false);
        }
        // The mode the session keeps, read afresh for the write: a switch from the chip since
        // the loop last read it (to Land or Off) types nothing.
        if self.autopilot_at_busy(cfg)? != Some(Autopilot::Cruise) {
            self.turn_end.go_ahead_unwritten();
            return Ok(false);
        }
        let rules = cfg
            .rules_file
            .as_ref()
            .and_then(|path| self.fx.read_text(path))
            .and_then(|text| super::turn_end_loop::standing_rules_in(&text));
        // The owed continuation is the policy's act, typed on one line as a point types it.
        let (text, rule, act, why) = match (owed, &park) {
            (Some(rule), _) => {
                let words =
                    self.turn_end
                        .owed_words(rule, cfg, rules.as_deref(), parked.is_some(), now);
                let text = super::turn_end_loop::one_turn(&words);
                let act = TurnEndAction::Type {
                    text: words,
                    rule_id: rule,
                };
                (text, rule, Some(act), None)
            }
            (None, Some(park)) => (
                go_ahead_answer(park, false, rules.as_deref()),
                RULE_GO_AHEAD,
                None,
                Some(
                    "the agent parked its own reversible work on a person's go-ahead at a break \
                     of its background work (autopilot cruise)",
                ),
            ),
            // With answers off the carry goes, but never says the go-ahead is no need over a
            // park the person chose to answer themselves.
            (None, None) if spent => (
                recovery_words(rules.as_deref()),
                RULE_BACKGROUND,
                None,
                Some("the standing decision was refused; changing approach and continuing"),
            ),
            (None, None) => (
                go_ahead::unless_held(
                    &carry_words(rules.as_deref()),
                    self.turn_end.go_ahead_held(now) || parked.is_some(),
                )
                .into_owned(),
                RULE_BACKGROUND,
                None,
                Some(
                    "the turn ended while the agent's own background work runs (autopilot cruise)",
                ),
            ),
        };
        // The write is a semantic duty's, as a point's act is (`Session::turn_end_execute`): the
        // continuation owed goes under its own where its rule names one; any other line goes
        // under the questions duty where the break's turn ended on a prose question (the rest
        // the break is published at, `ScreenReader::background_rest_seen`), since the line is
        // then typed over a question that duty's holder answers, and under the turn end's
        // otherwise. A window's host refuses a write that names no duty, and one whose duty
        // another supervisor holds.
        let scope = {
            let reader = self.reader(&screen.rows);
            let asked = reader
                .background_rest_seen(&screen.rows, reader.runs(screen.title.as_deref()))
                == Some(Phase::Question);
            let rest = if asked {
                Scope::Questions
            } else {
                Scope::TurnEnd
            };
            act.as_ref()
                .and_then(duty::known_action_scope)
                .unwrap_or(rest)
        };
        let _duty = self.duty_guard(scope);
        if self.duty_withheld(scope) {
            self.turn_end.go_ahead_unwritten();
            self.carry_said(seq, CarrySaid::Duty(scope), review);
            return Ok(false);
        }
        let point = self.turn_of(screen.clone());
        let keyed = self.cruise_carry.keyed;
        let written = self.type_text_written(&point, &text, rule, review)?;
        if written != Written::Submitted {
            self.turn_end.go_ahead_unwritten();
            return Ok(false);
        }
        let typed_at = self.fx.mono();
        self.cruise_carry.typed(typed_at, screen, keyed);
        // The act's answer is a new yield, as `Session::turn_end_write`'s is: the host's
        // repository and left reads of the one before it are spent.
        self.repo_seen = None;
        self.left_seen = None;
        if let Some(act) = act {
            let reading = self
                .reader(&screen.rows)
                .read(&screen.rows, Some(screen.cursor_col));
            let mut reading =
                TurnEndReading::of(&reading, &screen.rows, None::<bool>, None, None, rules);
            reading.autopilot = Autopilot::Cruise;
            reading.submits = screen.submits;
            self.turn_end.acted(&act, &reading, typed_at);
        } else {
            self.turn_end.background_carried(typed_at, &text);
            if let Some(park) = &park {
                self.turn_end.go_ahead_typed(park, typed_at);
            }
        }
        let typed_ms = self.hold_ms(typed_at);
        self.break_lined(typed_ms);
        self.hold_line(seq, Some(cfg), review);
        host.break_taken();
        host.inform(&text);
        review.say(&format!("CONTINUED seq={seq} rule={rule} {}", clip(&text)))?;
        let reason = why.map_or_else(
            || super::approvals::act_reason_after(self.carried(), self.turn_end.limit_try()),
            str::to_string,
        );
        // The row names the streak it went at, the carry's as what went in its place (the
        // go-ahead, the continuation owed: all go on the carry's ladder), so a later loop takes
        // the ladder up as it stands, where it started again in memory alone too.
        let reason =
            super::approvals::background_carry_reason(&reason, self.cruise_carry.books.streak());
        self.ledger_row(rule, Outcome::Typed, &text, &reason, seq);
        Ok(true)
    }

    /// THE NEEDS A BREAK'S REPORT NAMES: a turn that ends with the agent's own background work
    /// running reaches no point, where needs are read, so its `NEEDS YOU:` lines, in every message
    /// of the turn, are read here at the settled break, before any line the break types, and put
    /// on the menu bar as a point puts them once its lines went
    /// ([`Self::break_needs`]), and the work goes on. Where the turn names no need the fold
    /// answers the lines a go-ahead decided at an earlier read held back, as the agent went on
    /// past them; then the read judges the go-ahead again, before any line the break types
    /// ([`Self::break_go_ahead`]; DESIGN-harness-autopilot §15.6). The key of the needs the menu
    /// bar showed before the read, `None` where the break is not read (unsettled, or Off).
    pub(super) fn break_fold(
        &mut self,
        screen: &Screen,
        (at, waited): (Instant, Duration),
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) -> Option<u64> {
        self.cruise_carry.judged = None;
        let cfg = cfg?;
        if waited < self.setup.background_settle
            || cfg.autopilot_for(self.autopilot.word()) == Autopilot::Off
        {
            return None;
        }
        let turn = self.break_turn(screen);
        let (earlier, tail) = turn.needs_said();
        let before = self.turn_end.shown_key();
        self.turn_end
            .fold_break_needs(earlier, tail, turn.opens_cut);
        self.break_go_ahead(&turn, (screen.seq, at + waited), cfg, review);
        Some(before)
    }

    /// THE GO-AHEAD A SETTLED BREAK DECIDES, judged at its read `seq` on the break's `turn` at
    /// `now` before any line the break types, as a point judges its decision before a host's
    /// step: a line another takes the break with (a wake's, the host's, the stuck fact's or the
    /// door's) leaves it decided, so the lines it answers stay off the menu bar, as through the
    /// carry's ladder, until the next read judges again (2026-10-30 review 9). The carry types it
    /// or holds it ([`Self::cruise_carry`]), and a hold or a write that does not go puts its lines
    /// back. A park whose bound is spent switches to forward recovery, recorded ahead of
    /// the carry's ladder, its budget and the write's guards, as a point tells them at its
    /// decision, and its lines are back beside the exception; not where the window's host
    /// withholds the session's decisions, where a point tells nothing either. Where the carry
    /// cannot go (not Cruise, the continuation off, an attended run, no window host) nothing is
    /// decided and no line is being answered, and a line answered before stays off only where
    /// the turn's park still answers it
    /// ([`super::super::policy::turn_end::TurnEndState::go_ahead_unjudged`]), so a person's need
    /// that cleans to it shows (2026-10-30 review 9). The go-ahead's count starts again
    /// where the turn parks no more
    /// ([`super::super::policy::turn_end::TurnEndState::go_ahead_read`]), before the carry's
    /// ladder, its budget and the write guards, as at an idle point.
    fn break_go_ahead(
        &mut self,
        turn: &TurnWords,
        (seq, now): (u64, Instant),
        cfg: &SupervisorConfig,
        review: &mut dyn Review,
    ) {
        // The break's turn read as a turn end reads it, whole where it opens above the screen,
        // in the words the needs fold read ([`Self::break_turn`], [`TurnWords::needs_said`]).
        let (before, tail) = turn.needs_said();
        let at_break =
            tail.and_then(|t| self.turn_end.go_ahead_at_break(before, t, turn.opens_cut));
        let parks = at_break.is_some();
        // The go-ahead goes at a break where it would at a turn end: the turn read whole (its
        // earlier messages too), never over an irreversible ask, read in the rows as drawn and in
        // the words the park read ([`go_ahead_answers_on`]), never within a person's hold. Past
        // the routine answer budget, the carry changes approach.
        let parked = at_break
            .filter(|_| go_ahead_answers_on(turn.said.0.as_deref(), tail))
            .filter(|_| !self.turn_end.go_ahead_held(now));
        let park = parked.clone().filter(|_| cfg.answer_questions);
        let host = self.stall_host.clone().filter(|_| {
            cfg.autopilot_for(self.autopilot.word()) == Autopilot::Cruise
                && cfg.continue_policy
                && review.unattended()
        });
        let Some(host) = host else {
            // No go-ahead is judged here: a line answered before stays off only where this
            // turn's park still answers it, so a person's need that cleans to it shows.
            self.turn_end.go_ahead_unjudged(park.as_ref());
            return;
        };
        self.turn_end.go_ahead_read(parks);
        let owed = self.turn_end.owed_at_break();
        let spent = owed.is_none() && park.is_some() && self.turn_end.go_ahead_spent(now);
        let judged = BreakPark {
            parked,
            park,
            owed,
            spent,
        };
        self.turn_end.go_ahead_judged(judged.verdict());
        self.cruise_carry.judged = Some(judged);
        if spent && !Self::write_at(host.as_ref()).withheld {
            self.carry_said(seq, CarrySaid::Refused, review);
        }
    }

    /// THE TURN A SETTLED BREAK ENDS, read as a point reads it: from the screen, and where it
    /// opens above the screen, whole from the history, read once per turn while the screen's
    /// words stand ([`CarryWatch::turn_read`]). The needs fold ([`Self::break_fold`]) and the
    /// go-ahead ([`Self::cruise_carry`]) both judge this one turn, so a plan in a message that
    /// scrolled off parks nothing at a break either (2026-10-30 review 9).
    fn break_turn(&mut self, screen: &Screen) -> TurnWords {
        let reader = self.reader(&screen.rows);
        let words = |rows: &[String]| {
            let (turn_lines, lines_breaks) =
                needs::joined(reader.turn_message_lines_before(rows, needs::apart));
            let (turn_said, said_breaks) = needs::joined(reader.turn_messages_before(rows));
            let lines = (reader.said_lines(rows, needs::apart), turn_lines);
            let opens_cut =
                lines.0.is_some() && !reader.bounds_messages() && !reader.turn_whole(rows);
            TurnWords {
                lines,
                said: (reader.said_tail(rows), turn_said),
                breaks: needs::Breaks {
                    said: said_breaks,
                    lines: lines_breaks,
                },
                opens_cut,
            }
        };
        let shown = words(&screen.rows);
        if shown.lines.0.is_none() || reader.turn_whole(&screen.rows) {
            return shown;
        }
        let mark = aterm_hash::fnv1a_64(
            format!(
                "{}\0{}",
                shown.lines.1.as_deref().unwrap_or_default(),
                shown.lines.0.as_deref().unwrap_or_default()
            )
            .as_bytes(),
        );
        let read = match &self.cruise_carry.turn_read {
            Some((at, read)) if *at == mark => read.clone(),
            _ => {
                let read = self
                    .turn_read_whole(screen, reader, true)
                    .map(|(rows, _)| rows)
                    .filter(|rows| reader.said_whole(rows))
                    .map(|rows| words(&rows));
                self.cruise_carry.turn_read = Some((mark, read.clone()));
                read
            }
        };
        read.filter(|turn| turn.lines.0.is_some()).unwrap_or(shown)
    }

    /// THE NEEDS THE BREAK READ ([`Self::break_fold`], which returned `before`), told once its
    /// lines went, whatever they returned: on the menu bar where what it shows moved
    /// ([`Self::sync_needs_under`]), so a line the carry's go-ahead answers never goes up,
    /// or where the entry is still owed what it shows ([`Self::needs_owed`]): a tell lost or
    /// refused at an earlier read is made again here, as no retry of the loop's own runs inside
    /// the agent's background work.
    pub(super) fn break_needs(
        &mut self,
        screen: &Screen,
        before: Option<u64>,
        cfg: Option<&SupervisorConfig>,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let (Some(before), Some(cfg)) = (before, cfg) else {
            return Ok(());
        };
        if self.turn_end.shown_key() == before && !self.needs_owed() {
            return Ok(());
        }
        self.sync_needs_under(screen, cfg, review)
    }

    /// WHETHER A WRITE UNDER `scope` IS WITHHELD FROM THIS LOOP, read from the authority the duty's
    /// guard just observed ([`Session::duty_guard`]): on a window's hosted loop
    /// (`builtin_duties`), another supervisor holds that duty, one holds the whole session, or
    /// the observation did not read. Such a write is refused by the host however often it is
    /// tried, so the carry holds and says so once ([`CarrySaid::Duty`]) in place of a refused
    /// write at every look. An ordinary loop keeps its claim protocol and is never withheld here.
    fn duty_withheld(&self, scope: Scope) -> bool {
        self.setup.builtin_duties
            && match self.builtin_authority {
                BuiltinAuthority::Complement { .. } => !self.builtin_authority.keeps(scope),
                BuiltinAuthority::Unknown | BuiltinAuthority::Exclusive => true,
                BuiltinAuthority::Legacy | BuiltinAuthority::Unsupported => false,
            }
    }

    /// Journal why a due carry was not typed, once a change.
    fn carry_said(&mut self, seq: u64, said: CarrySaid, review: &mut dyn Review) {
        if self.cruise_carry.said == Some(said) {
            return;
        }
        self.cruise_carry.said = Some(said);
        let why = match said {
            CarrySaid::Held(guard) => format!("held:{}", guard.word()),
            CarrySaid::Waits(until) => format!(
                "waits until {} ({} short answer(s) in a row)",
                self.stamp_in(until),
                self.cruise_carry.books.streak()
            ),
            CarrySaid::Budget => "held:continue_per_hour".to_string(),
            CarrySaid::Refused => format!("recovering:{GO_AHEAD_REFUSED}"),
            CarrySaid::Duty(scope) => format!("held:duty:{}", scope.word()),
        };
        review.note(&format!("DEFERRED seq={seq} {RULE_BACKGROUND} {why}"));
    }
}
