// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CUSTODY OF A LINE HELD (ATERM-KI-1101-3), AS PLAIN DECISIONS: a line the loop wrote and
//! gave up on undrawn, on a read that shows none of it, is held in custody until evidence
//! settles it, and while it stands no loop writer writes. What the custody decides is here, as
//! values from plain inputs: no `Ctl`, no clock and no journal. The session's methods
//! (`Session::custody_formed`, `custody_look`, `custody_at_break`, `custody_turned`,
//! `custody_gate`, `custody_lost` and `custody_holds_host`) gather what a read, a `status` word,
//! the host and the loop's clock say, call these, and do what they decide: the journal's
//! records, the due, the restart in place.
//!
//! - [`Line::formed`]: what the first `status` read after the write says of it ([`Held`]).
//! - [`Line::look`]: what settles it on a read ([`Settles`]): its draw (whole, a part arriving,
//!   or with other words joined to it), or a person's keys drawn after it; a draw a break of the
//!   agent's own background work saw is kept as evidence ([`Look::Kept`]) for the point's look.
//! - [`Line::ended`]: its program ended (the host names another agent process).
//! - [`Line::took`], [`Line::weighs`], [`Line::turned`], [`Line::outlasted`]: a turn read running
//!   after it settles it once `status` reads the input clear on a read that shows none of it
//!   ([`Turned`]); a line unread, or one no word can say of, outlasts the turn; a read that cannot
//!   say what the composer holds keeps the turn's evidence for the next read that can.
//! - [`Line::gate_read`] and [`Rest::gate`]: what a write at a point gets while the line stands
//!   ([`CustodyGate`]): held, or the line lost where the worker read it and drew none of it
//!   [`ECHO_HANG`] on, at rest.
//! - [`answer_goes`]: once an input read proves the worker read the line, a key that answers a
//!   box over the composer goes past it; without that proof the line holds the key too.
//! - [`Line::lose`] and [`Restarts`]: the one restart in place a lost line gets short of an act
//!   submitted, or a doubling wait ([`Loss`]).
//! - [`Line::restarted`] and [`Restarts::answered`]: what the host's word that it made the
//!   restart comes to ([`Restarted`]), on the process the restart's own report names after it
//!   (else the process the host names now): the line's program ended, and the restart made, where
//!   that is another process than the one the line was written to, or where none is named at
//!   either end, the word all there is to go on; with the line's own process named, nothing shows
//!   the restart ended it, and the restart stands asked as a deferral does
//!   ([`Restarts::deferred`]), spent once the line settles ([`Restarts::settled`]).
//! - [`line_held`]: what the host is told ([`LineHeld`]).
//!
//! `LineCustody` (`aterm_models_harness::line_custody_model`) is bound to these functions by
//! `tests/conformance_custody.rs`, with its environment's steps waived in
//! `custody_environment.rs`.

use std::time::{Duration, Instant};

use super::occupancy::fault::{Fault, armed};
use super::policy::approval::squashed;
use super::run::DARK_WAKE_CAP;

/// How long the echo of a fenced write may take to show whole in the composer before the text is
/// escalated as typed and not submitted (`Session::await_echo`); and how long after its first
/// clear read a line held is read again for its loss ([`Line::gate_read`]).
pub const ECHO_HANG: Duration = Duration::from_secs(60);

/// The first wait of a line held that no clock of its own settles (its input read unread or
/// unknown, a read of it not at rest, no restart in place, or a second loss), doubling at each
/// wait in a row to [`CUSTODY_CAP`]: a held line wakes the loop a few times an hour, never three
/// times a minute, and its draw, a person's keys or a turn wake it at once.
pub const CUSTODY_STEP: Duration = Duration::from_secs(20);

/// The longest wait of a line held ([`CUSTODY_STEP`]), the dark wake's cap.
pub const CUSTODY_CAP: Duration = DARK_WAKE_CAP;

/// The `waits`-th wait in a row of a line held: [`CUSTODY_STEP`] doubling to [`CUSTODY_CAP`].
#[must_use]
pub fn custody_step(waits: u32) -> Duration {
    CUSTODY_STEP
        .checked_mul(1u32.checked_shl(waits).unwrap_or(u32::MAX))
        .map_or(CUSTODY_CAP, |d| d.min(CUSTODY_CAP))
}

/// What the composer shows of a line this loop wrote and did not submit
/// (`Session::left_line_look`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftStands {
    /// No typed text: an empty composer or a grey suggestion, whatever that suggestion says.
    Nothing,
    /// Typed text that is not the whole line: a part of it arriving, or a person's.
    Part,
    /// The line, whole.
    Whole,
}

/// Whether `shown` (a composer's text, or a draft a read saw) is the line `text`, whitespace
/// aside.
#[must_use]
pub(crate) fn same_line(text: &str, shown: &str) -> bool {
    squashed(shown) == squashed(text)
}

/// Whether `shown` holds the line `text` whole with other words joined to it (a person's keys
/// landed before its Enter), whitespace aside: neither the line nor a draft of the person's own.
#[must_use]
pub(crate) fn joined_line(text: &str, shown: &str) -> bool {
    let (shown, own) = (squashed(shown), squashed(text));
    shown != own && shown.contains(&own)
}

/// WHAT `status input=` HAS SAID OF A LINE HELD. The session's input is one queue, read in
/// order, so a read that finds it clear after the write proves the worker read the line:
/// whatever it read since, it read the line first. Nothing on the screen says this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// Every read since the write found input unread (`input=pending|typeahead`, or a stall's
    /// word): the line may still be queued, and the worker draw it.
    Unread,
    /// A read found the input clear, the first at `since`: the worker read the line by then. It
    /// stays read whatever later reads say, since the line came before any input they find.
    Read {
        /// The first clear read.
        since: Instant,
    },
    /// No read said (`input=-`, no field, or no `status` answer).
    Unknown,
}

impl Held {
    /// The state a `status` input word `word` (`None`: no field) leaves `self` in at `now`.
    #[must_use]
    pub fn after(self, word: Option<&str>, now: Instant) -> Self {
        match (self, word) {
            (Self::Read { .. }, _) | (_, None | Some("-")) => self,
            (_, Some("clear")) => Self::Read { since: now },
            (_, Some(_)) => Self::Unread,
        }
    }

    /// The state the first `status` read after a write leaves.
    #[must_use]
    pub fn first(word: Option<&str>, now: Instant) -> Self {
        Self::Unknown.after(word, now)
    }
}

/// What holds a write while a line stands held, as its records name it: the line's [`Held`]
/// state and the input word `word` the last `status` read said.
#[must_use]
pub fn hold_said(held: Held, word: &str) -> String {
    match held {
        Held::Unread => format!("unread:{word}"),
        Held::Read { .. } => "read".to_string(),
        Held::Unknown => "unknown".to_string(),
    }
}

/// What holds a write where the line was read and drew none of it a minute on, but the read is
/// not at rest ([`Rest::gate`]), as its records name it.
pub const NOT_AT_REST: &str = "read:not-at-rest";

/// What a line held last said where it is lost and waits, no restart asked ([`Line::lose`]).
const LOST_WAITS: &str = "lost";

/// What a line held last said where it is lost and a restart in place is asked for it
/// ([`Line::lose`]), so a hold's record after the ask is written as the change it is. Whether the
/// ask's own record stands is [`Line`]'s `restart_noted`, which no hold's record moves.
const LOST_RESTARTING: &str = "lost:restarting";

/// How a line held settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settles {
    /// Its program drew it, whole, in part or with other words joined to it.
    Drawn,
    /// A person's keys drew after it, and the draft is theirs.
    Person,
    /// A turn ran after it and `status` read the input clear.
    Turn,
    /// Its program ended: the host names another agent process than the one at the write (a
    /// restart in place that took, [`Line::restarted`], an upgrade's move, the program's own end).
    Ended,
}

impl Settles {
    /// The words of its `NOTE` ([`super::journal::LINE_SETTLED`]).
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::Drawn => "is drawn",
            Self::Person => "is under a person's keys",
            Self::Turn => "went with a turn",
            Self::Ended => "ended with its program",
        }
    }
}

/// WHAT A LINE THE LOOP HOLDS IN CUSTODY LETS THE HOST DO (`IdlePoint::line_held`,
/// ATERM-KI-1101-3): the worker read the line, or may yet, and drew none of it. Whichever it is,
/// a relaunch's carry-on and the ask to free disk space wait, kept owed, for as long as the line
/// stands, and the host's steps that type nothing go on; the upgrade's step is held as each says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineHeld {
    /// The line may still come: unread, read within its minute or not at rest, or no read can say
    /// (`input=-`). A line typed behind it would join it or land beside it, and a restart could
    /// lose it (a copy the worker holds, or input still queued that a shell would read once the
    /// agent exits), so the upgrade's step waits whole, its restart included. A line the loop
    /// judged lost stands too until [`Line::lose`] finds no restart to make for it: while the
    /// loop's own restart in place for it stands asked ([`Restarts::standing`]: deferred, or made
    /// with nothing showing it ended the line's process), asked again on its back-off, and from a
    /// refusal of that restart until the loop weighs the loss again. The upgrade's step waits
    /// whole then too, for as long as that lasts and the line is held: a deferral that never
    /// resolves, on a line nothing settles, holds it with no bound.
    Stands,
    /// The loop judged the line lost (read, undrawn a minute on, at rest:
    /// [`super::policy::turn_end::Restart::InputLost`]'s evidence) and makes no restart in place
    /// for it (relaunch off, no restart lane for the program, a restart refused or already made):
    /// the line waits on its draw, a person's keys or a turn, none of which may come. The
    /// upgrade's step goes as at a free point, its notice or tell included: its move ends the
    /// process that read the line, the restart the loop would make itself where it could, and the
    /// composer that read found empty at rest holds no copy of it for a line to join.
    Lost,
}

/// The agent's process as the host names it (`IdleHost::agent_pid`, `IdleHost::agent_started`).
pub type Process = (Option<u32>, Option<i64>);

/// WHICH LINE HELD A RESTART IN PLACE IS ASKED FOR
/// ([`super::policy::turn_end::Restart::InputLost`]'s `line`): the loop's wall clock when it took
/// the line into custody, in Unix milliseconds, and how many lines the loop took into custody
/// before it. Every ask of one line's restart carries the same, its first and each asked again
/// after a deferral; another line's, a later loop's included, carries another. It is that
/// restart's ask ([`super::policy::turn_end::RestartAsk::of`]), which the relaunch's record keeps
/// once an ask claims the record (`St::ask`), so a restart asked again meets its own record by it,
/// never by its cause alone (`relaunch::its_own`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineAsk {
    /// The loop's wall clock when it took the line into custody, Unix milliseconds.
    pub at_ms: i64,
    /// How many lines the loop took into custody before it.
    pub nth: u32,
}

impl LineAsk {
    /// The line's word, `<at_ms>.<nth>`, which its restart's ask names.
    #[must_use]
    pub fn word(self) -> String {
        format!("{}.{}", self.at_ms, self.nth)
    }
}

/// What a read shows of a line held: what its composer holds of it ([`LeftStands`], `None`
/// where the read cannot say: no composer drawn, a dialog over it, or a glyph's cell unread),
/// the composer's text, and the person's key count on the read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shows {
    /// What the composer holds of the line.
    pub stands: Option<LeftStands>,
    /// The composer's lines, joined by a space (empty where none is drawn).
    pub composer: String,
    /// The person's key count on the read.
    pub human: Option<u64>,
}

/// Where a look at a line held is taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    /// A point the loop decides at (an idle point, the point a busy spell ended on).
    Point,
    /// A break of the agent's own background work, which gives no Enter and makes no restart.
    Break,
}

/// What a look at a line held came to ([`Line::look`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    /// Nothing on this read settles it.
    Stands,
    /// A break saw it drawn: kept as the evidence of its draw ([`Line::drawn`]), it stands held.
    Kept,
    /// It settled, as the first says; the second is what the composer is handed back as showing
    /// where its draw settled it (`LeftLine::stands`): [`LeftStands::Nothing`] where this read
    /// showed the draw, what a break saw where only a break did.
    Settled(Settles, LeftStands),
}

/// What a fresh `status` read after a turn read running comes to for a line held, on what a read
/// showed of it ([`Line::turned`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turned {
    /// On a read that shows none of the line, the input reads clear: it went with the turn
    /// ([`Settles::Turn`]).
    Settles,
    /// On a read that shows none of the line, the input still reads unread, or no word can say:
    /// the line outlasted the turn ([`Line::outlasted`]).
    Outlasts,
    /// Nothing moves: no turn read running since the write (or since the last turn it outlasted),
    /// or a read that shows typed text in the composer (the look's to settle) or cannot say what
    /// it holds (a dialog over it, no composer drawn, a glyph's cell unread), under which the line
    /// may stand drawn. The turn's evidence ([`Line::ran`]) is kept for the next read that can say.
    Stands,
}

/// What a line held lets a write at a point do (`Session::custody_gate`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CustodyGate {
    /// The write goes: nothing held, or a key that answers a box over the composer, past the
    /// line held ([`answer_goes`]).
    Free,
    /// The line holds the write; a due stands for the next look.
    Hold,
    /// The worker read the line and drew none of it a minute on, at rest: a restart in place.
    Lost,
}

/// WHETHER THE LOOP'S ANSWER TO A BOX GOES PAST THE LINE HELD, on the read `shows` the answer was
/// decided on and what `status input=` has proved of the line (`held`): a key that is no line's (a
/// dialog's Enter or Esc, fenced on the box's own row and on no person's key since that read) goes
/// only where the worker is known to have read the line and the read cannot say what the composer
/// holds, the box over it. The box takes the key, never the composer, so the key neither joins nor
/// enters the line, nor writes a second copy of it. The line stays held for the read after the box.
/// With no proof the worker read the line, the key may be queued behind it and is held; where the
/// read can say what the composer holds, no box is over it and the key is held too. A key behind
/// later input `status` reads unread is also held by the session's own hold
/// (`Session::input_unread`) after this gate.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "LineCustody",
        action = "AnswersBox",
        project = "conformance_custody::project"
    )
)]
#[must_use]
pub fn answer_goes(held: Held, shows: &Shows) -> bool {
    let box_shown = shows.stands.is_none() || armed(Fault::CustodyAnswersComposer);
    let line_read = matches!(held, Held::Read { .. }) || armed(Fault::CustodyAnswersWithoutRead);
    box_shown && line_read
}

/// What the gate's `status` read came to for a line held ([`Line::gate_read`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateRead {
    /// The write is held on this read.
    Holds,
    /// The worker read the line [`ECHO_HANG`] ago and the input still reads clear, where the
    /// policy wants to write a line now: the screen is read again, in order after `status` (a
    /// draw that came before the clear read shows on it), and the line is lost where that read
    /// shows none of it and is at rest ([`Rest::gate`]).
    Again,
}

/// WHETHER A READ OF A LINE HELD IS AT REST, so a restart in place loses nothing but the line:
/// what the session gathers from the read, its own state and the host ([`Rest::gate`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rest {
    /// The read is a turn's end its reader stands by, of whichever phase the policy writes a line
    /// at (idle, a question, a limit's notice: no turn running and no box), a composer drawn, no
    /// dialog and no frame torn mid-redraw.
    pub turn_ended: bool,
    /// The agent's own background work runs under that turn's end, as the read names it (a
    /// monitor, a shell, a background agent, a workflow or other work the turn left running): a
    /// restart in place would end it, so the read is no rest while it runs.
    pub working: bool,
    /// The composer shows no typed text (read only where [`Self::turn_ended`]).
    pub clear: bool,
    /// A hold is on the session.
    pub halted: bool,
    /// The session's word is Cruise or Land.
    pub autopilot: bool,
    /// How long ago the person last keyed, where a read says.
    pub person: Option<Duration>,
    /// The person's grace.
    pub grace: Duration,
    /// A background call of the agent's standing that the host reads makes no progress
    /// (`IdleHost::background_jobs`; read only where [`Self::asks_jobs`]).
    pub stuck: bool,
}

impl Rest {
    /// Whether everything but the host's background calls is at rest, so the host is asked of
    /// those ([`Self::stuck`]): it is asked of no read that is not at rest already.
    #[must_use]
    pub fn asks_jobs(&self) -> bool {
        self.turn_ended
            && !self.working
            && self.clear
            && !self.halted
            && self.autopilot
            && self.person.is_none_or(|ago| ago >= self.grace)
    }

    /// What the gate gives a write whose `status` read came to [`GateRead::Again`] and whose
    /// fresh read shows none of the line: [`CustodyGate::Lost`] at rest, else
    /// [`CustodyGate::Hold`].
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "GateHolds",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "AsksRestart",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "WaitsLost",
            project = "conformance_custody::project"
        )
    )]
    #[must_use]
    pub fn gate(&self) -> CustodyGate {
        if self.asks_jobs() && !self.stuck {
            CustodyGate::Lost
        } else {
            CustodyGate::Hold
        }
    }
}

/// Where a restart in place could be made for a lost line: `[harness] relaunch`, a host to make
/// it, and whether that host makes one for the program (`IdleHost::can_restart`, asked only
/// where [`Self::asks_host`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lane {
    /// `[harness] relaunch` is on.
    pub relaunch: bool,
    /// The loop runs with a host.
    pub host: bool,
    /// The host makes a restart in place for the program.
    pub can_restart: bool,
}

impl Lane {
    /// Whether the host is asked whether it makes a restart for the program: only where no
    /// restart was asked since the last act submitted, relaunch is on, and a host is there.
    #[must_use]
    pub fn asks_host(&self, restarts: &Restarts) -> bool {
        restarts.spent().is_none() && self.relaunch && self.host
    }
}

/// Why a lost line gets no restart in place ([`Loss::Wait`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoRestart {
    /// A restart was made for a line since the last act submitted, or stood asked when the line
    /// settled ([`LostRestart::Unseen`]): a second loss is never restarted again.
    Again,
    /// The host did not make the restart asked (its step, or `-` where it answered none).
    Unmade(String),
    /// `[harness] relaunch = false`.
    RelaunchOff,
    /// The loop runs with no host.
    NoHost,
    /// The host makes no restart in place for the program.
    NoLane,
}

/// What a lost line comes to ([`Line::lose`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Loss {
    /// A restart in place is asked (its wait, where the host makes none now, until `until`);
    /// `first` where no record of it was written since the line was lost.
    Restart {
        /// When the line is looked at again where the restart is not made.
        until: Instant,
        /// No record of the restart asked stands yet: none was asked for the line before, however
        /// often a host's deferral has it asked again and whatever holds came between the asks.
        first: bool,
    },
    /// A wait that writes nothing until `until`, the loss's `waits`-th, for `why`.
    Wait {
        /// Why no restart is made.
        why: NoRestart,
        /// When the line is looked at again: the next step of the line's waits in a row, which
        /// the holds before the loss counted too.
        until: Instant,
        /// The waits of this loss before this one, on which its records are thinned (its first is
        /// always due): a hold before the loss is none of them.
        waits: u32,
    },
}

/// WHAT THE HOST'S WORD THAT IT MADE THE RESTART IN PLACE ASKED FOR A LINE HELD COMES TO
/// ([`Line::restarted`]): the word (`adopted`, `relaunched`, `continued`, `done`) is the host's
/// claim; the process the restart's own report names after it, else the one the host names now,
/// the evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restarted {
    /// The line's program ended: the process named is another than the one the line was written
    /// to, or none is named at either end, where the host's word is all there is to go on. The
    /// line settles [`Settles::Ended`] and the restart is made ([`Restarts::answered`]).
    Ended,
    /// The process named is the one the line was written to: nothing shows the restart ended it
    /// (a relaunch in flight carried, the asker only adopted). The line stands held, written
    /// again to no process, and the restart is neither made nor refused: it stands asked, and is
    /// asked again on its back-off, as a deferral is ([`Restarts::deferred`]).
    Stands,
}

/// What became of the restart in place asked for a line held since the last act submitted: either
/// way, a line lost from here waits and writes nothing, and the host is asked no more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LostRestart {
    /// The host made it, its word and another process named after it, or none named at either
    /// end ([`Restarts::answered`]): a second loss is never restarted again.
    Made,
    /// The line settled while the restart asked for it stood deferred ([`Restarts::settled`]):
    /// the host may have made it unseen (a restart whose signal went answers a deferral where the
    /// process it started has yet to register), so a second loss is never restarted again either.
    Unseen,
    /// The host did not make it (its step, or `-` where it answered none): a repeated failure
    /// calls for a different approach, so it is not asked again at each due.
    Unmade(String),
}

/// WHAT THE HOST ANSWERED TO THE RESTART IN PLACE ASKED FOR A LOST LINE SINCE THE LAST ACT
/// SUBMITTED, if it answered: one answer per line short of an act submitted, made or refused
/// ([`Line::lose`]). A deferral is no answer: the restart stands asked, and is asked again while
/// the line stands held; a line that settles meanwhile spends it ([`LostRestart::Unseen`]), as
/// the host may have made it unseen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Restarts {
    /// What the host answered, if it did.
    answer: Option<LostRestart>,
    /// The restart was asked, and deferred, since its last answer ([`Self::deferred`]).
    standing: bool,
}

impl Restarts {
    /// What became of the restart asked since the last act submitted.
    #[must_use]
    pub fn get(&self) -> Option<&LostRestart> {
        self.answer.as_ref()
    }

    /// Whether the restart asked for the line held stands deferred, unanswered
    /// ([`Self::deferred`]).
    #[must_use]
    pub fn standing(&self) -> bool {
        self.standing && self.answer.is_none()
    }

    /// What [`Line::lose`] and [`Lane::asks_host`] take as spent.
    fn spent(&self) -> Option<&LostRestart> {
        if armed(Fault::CustodyRestartAgain) {
            return None;
        }
        self.answer.as_ref()
    }

    /// WHAT THE HOST'S WORD THAT IT MADE THE RESTART ASKED FOR THE LINE HELD (`line`) COMES TO, on
    /// the process the restart's own report names after it (`left`), else the one the host names
    /// now (`named`) ([`Line::restarted`]): where the line's program ended ([`Restarted::Ended`]:
    /// another process named, or none at either end, the word all there is), or no line is held,
    /// the restart is made ([`Self::made`]); where the process the line was written to is named
    /// ([`Restarted::Stands`]), nothing is answered: the restart stands asked, and is asked again
    /// on its back-off, as a deferral is ([`Self::deferred`]). The word with the line's own
    /// process named books no restart: this is the one way to [`LostRestart::Made`].
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "RestartMade",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "RestartStands",
            project = "conformance_custody::project"
        )
    )]
    pub fn answered(
        &mut self,
        line: Option<&Line>,
        left: Option<Process>,
        named: Option<Process>,
    ) -> Restarted {
        let restarted = line.map_or(Restarted::Ended, |line| line.restarted(left, named));
        match restarted {
            Restarted::Ended => self.made(),
            Restarted::Stands => self.deferred(),
        }
        restarted
    }

    /// The host made the restart asked for a lost line: its word, and another process named
    /// after it, or none at either end ([`Self::answered`]).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "RestartMade",
            project = "conformance_custody::project"
        )
    )]
    fn made(&mut self) {
        self.answer = Some(LostRestart::Made);
        self.standing = false;
    }

    /// THE RESTART ASKED FOR THE LINE HELD STANDS ASKED, UNANSWERED: the host deferred it
    /// (`wait:…`, `busy:another-sweep`; another `busy:` step, a state directory that cannot hold
    /// the sweep's lock, is a refusal: [`Self::unmade`]), or answered it with its word while the
    /// line's own process is named ([`Restarted::Stands`]). It is asked again on its back-off
    /// while the line stands held; the host may have made it all the same (a restart whose signal
    /// went answers a deferral where the process it started has yet to register), so a line that
    /// settles meanwhile spends it ([`Self::settled`]).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "RestartDeferred",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "RestartStands",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "RestartMadeDeferred",
            project = "conformance_custody::project"
        )
    )]
    pub fn deferred(&mut self) {
        if self.answer.is_none() {
            self.standing = true;
        }
    }

    /// THE LINE HELD SETTLED ([`Settles`], `Session::custody_ended`): a restart that stood asked
    /// for it ([`Self::deferred`]) is spent ([`LostRestart::Unseen`]), whatever settled it, since
    /// the host may have made it unseen: a loss from here, short of an act submitted, waits
    /// ([`NoRestart::Again`]), so a line is never restarted twice for one restart the host made
    /// and answered as a deferral.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "LookDrawn",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "LookPerson",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "SeesEnded",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "TurnSettles",
            project = "conformance_custody::project"
        )
    )]
    pub fn settled(&mut self) {
        if self.standing() && !armed(Fault::CustodySettleKeepsAsked) {
            self.answer = Some(LostRestart::Unseen);
        }
        self.standing = false;
    }

    /// The host refused the restart asked for a lost line, or answered none (`step`: its step):
    /// any answer but a success word, a `wait:…` or `busy:another-sweep` (a `refused:…`, another
    /// `busy:` step), or no answer.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "RestartUnmade",
            project = "conformance_custody::project"
        )
    )]
    pub fn unmade(&mut self, step: Option<&str>) {
        self.answer = Some(LostRestart::Unmade(step.unwrap_or("-").to_string()));
        self.standing = false;
    }

    /// An act was submitted: a line lost from here may be restarted for again.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "Types",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "Enters",
            project = "conformance_custody::project"
        )
    )]
    pub fn submitted(&mut self) {
        *self = Self::default();
    }
}

/// A LINE THIS LOOP WROTE AND GAVE UP ON UNDRAWN, HELD IN CUSTODY, and what the custody knows of
/// it: one per session until it settles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// The line.
    text: String,
    /// The person's key count at the write: keys after it came after the line.
    human: u64,
    /// What `status input=` has said of it.
    held: Held,
    /// The agent's process at the write, where the host names one: another one since is the
    /// line's program ended.
    process: Option<Process>,
    /// What the last record of the hold said, so the journal takes one line per change.
    said: Option<String>,
    /// The waits in a row with no clock of the custody's own ([`custody_step`]).
    waits: u32,
    /// The waits the line made lost ([`Loss::Wait`]), on which their records are thinned.
    lost_waits: u32,
    /// The record of the restart in place asked for the line was written ([`Loss::Restart`]'s
    /// `first`): it is written once for the line, whatever holds come between the asks.
    restart_noted: bool,
    /// A turn was read running since the write, or since a `status` read after one last found
    /// the input not clear ([`Self::took`], [`Self::outlasted`]).
    ran: bool,
    /// What a break of the agent's own background work last showed of the line drawn
    /// ([`Look::Kept`]).
    drawn: Option<LeftStands>,
    /// The loop judged the line lost and makes no restart in place for it ([`Loss::Wait`]).
    lost: bool,
}

impl Line {
    /// THE LINE `text`, WRITTEN UNDER THE PERSON'S KEY COUNT `human` AND GIVEN UP ON UNDRAWN, TAKEN
    /// INTO CUSTODY at `now`: the first `status` read since says what the worker has read of it
    /// (`word`, the read's input word), and a clear read by the write's held-text note at
    /// `noted` dates its read that early. `process` is the agent's process the host names.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "WritesQueued",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "WritesRead",
            project = "conformance_custody::project"
        )
    )]
    #[must_use]
    pub fn formed(
        text: &str,
        human: u64,
        word: Option<&str>,
        noted: Option<Instant>,
        process: Option<Process>,
        now: Instant,
    ) -> Self {
        let held = noted.map_or_else(|| Held::first(word, now), |since| Held::Read { since });
        Self {
            text: text.to_string(),
            human,
            held,
            process,
            said: Some(hold_said(held, word.unwrap_or("-"))),
            waits: 0,
            lost_waits: 0,
            restart_noted: false,
            ran: false,
            drawn: None,
            lost: false,
        }
    }

    /// The line.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The person's key count the line was written under.
    #[must_use]
    pub const fn human(&self) -> u64 {
        self.human
    }

    /// Whether `shown` (a draft a read saw) is the line, whitespace aside.
    #[must_use]
    pub fn is(&self, shown: &str) -> bool {
        same_line(&self.text, shown)
    }

    /// What `status input=` has said of it.
    #[must_use]
    pub const fn held(&self) -> Held {
        self.held
    }

    /// A turn was read running after it (and no `status` read since found the input not clear).
    #[must_use]
    pub const fn ran(&self) -> bool {
        self.ran
    }

    /// What a break last showed of it drawn, kept for the point's look.
    #[must_use]
    pub const fn drawn(&self) -> Option<LeftStands> {
        self.drawn
    }

    /// The loop judged it lost and makes no restart in place for it.
    #[must_use]
    pub const fn is_lost(&self) -> bool {
        self.lost
    }

    /// How long ago `status` first read the input clear after it, at `now`, where one did.
    #[must_use]
    pub fn read_ago(&self, now: Instant) -> Option<Duration> {
        match self.held {
            Held::Read { since } => Some(now.saturating_duration_since(since)),
            Held::Unread | Held::Unknown => None,
        }
    }

    /// WHAT THE READ `shows` SHOWS OF THE LINE, settling nothing: its draw ([`Settles::Drawn`]:
    /// whole, a part of it arriving, or with other words joined to it, or any typed text with no
    /// person's key since the write), or typed text that is not the line under a person's key
    /// since the write ([`Settles::Person`]: in order, their keys came after the line), each with
    /// what the composer shows; `None` where it shows none of it, or the read cannot say.
    fn seen(&self, shows: &Shows) -> Option<(Settles, LeftStands)> {
        let stands = match shows.stands {
            Some(stands) => stands,
            None if armed(Fault::CustodySettlesUnreadable) => LeftStands::Whole,
            None => return None,
        };
        let settles = match stands {
            LeftStands::Nothing => return None,
            LeftStands::Whole => Settles::Drawn,
            LeftStands::Part
                if joined_line(&self.text, &shows.composer)
                    || squashed(&self.text).starts_with(&squashed(&shows.composer)) =>
            {
                Settles::Drawn
            }
            LeftStands::Part if shows.human != Some(self.human) => Settles::Person,
            LeftStands::Part => Settles::Drawn,
        };
        Some((settles, stands))
    }

    /// A LOOK AT THE LINE ON A READ THAT SHOWS `shows`, taken `at` a point or a break. Its draw
    /// on the read settles it at a point and is kept at a break ([`Look::Kept`]: a break gives no
    /// Enter, so the point the spell ends on hands it back, whether its composer still shows it
    /// or has emptied since); a person's keys drawn after it settle it at either; at a point, a
    /// draw a break kept settles it where this read shows none of it. Nothing else settles it
    /// here: a read that shows none of it, or cannot say, is no evidence.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "LookDrawn",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "LookPerson",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "BreakKeepsDraw",
            project = "conformance_custody::project"
        )
    )]
    pub fn look(&mut self, shows: &Shows, at: At) -> Look {
        match (self.seen(shows), at) {
            (Some((Settles::Drawn, stands)), At::Break) => {
                self.drawn = Some(stands);
                Look::Kept
            }
            (Some((settles, _)), _) => Look::Settled(settles, LeftStands::Nothing),
            (None, At::Break) => Look::Stands,
            (None, At::Point) => match self.drawn {
                Some(kept) => Look::Settled(Settles::Drawn, kept),
                None if armed(Fault::CustodySettlesUnshown) => {
                    Look::Settled(Settles::Drawn, LeftStands::Nothing)
                }
                None => Look::Stands,
            },
        }
    }

    /// Whether the program the line was written to ended: the host names `now` another agent
    /// process than the one at the write.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "SeesEnded",
            project = "conformance_custody::project"
        )
    )]
    #[must_use]
    pub fn ended(&self, now: Option<Process>) -> bool {
        matches!(
            (self.process, now),
            (Some((Some(was), _)), Some((Some(is), _))) if was != is
        )
    }

    /// The agent's process at the write, as the host named it (`(None, None)` where it named
    /// none): the process a restart in place for the line is of
    /// ([`super::policy::turn_end::Restart::InputLost`]).
    #[must_use]
    pub fn process(&self) -> Process {
        self.process.unwrap_or((None, None))
    }

    /// WHAT THE HOST'S WORD THAT IT MADE THE RESTART IN PLACE COMES TO ([`Restarted`]), on the
    /// process the restart's own report names after it (`left`: the process it started where it
    /// made one, the one it was asked by where it carried a relaunch in flight or only adopted),
    /// else, where its report names none, the process the host names now (`named`), whose kept
    /// snapshot may lag a restart it made: [`Restarted::Stands`] where that is the very process
    /// the line was written to, which nothing shows a restart ended (the custody settles only on
    /// evidence, and that process may still hold the line); else [`Restarted::Ended`], the line's
    /// program ended (another process named, as [`Self::ended`] reads it), or no process named at
    /// the write or after the word, the host's word all the evidence there is.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "RestartMade",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "RestartStands",
            project = "conformance_custody::project"
        )
    )]
    #[must_use]
    pub fn restarted(&self, left: Option<Process>, named: Option<Process>) -> Restarted {
        let names = |p: &Process| p.0.is_some();
        let (first, then) = if armed(Fault::CustodyRestartBySnapshot) {
            (named, left)
        } else {
            (left, named)
        };
        let now = first.filter(names).or(then);
        match (self.process, now) {
            (Some((Some(was), _)), Some((Some(is), _)))
                if was == is && !armed(Fault::CustodyRestartOnWord) =>
            {
                Restarted::Stands
            }
            _ => Restarted::Ended,
        }
    }

    /// A turn was read running after the line (`Session::took`).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "TurnRuns",
            project = "conformance_custody::project"
        )
    )]
    pub fn took(&mut self) {
        self.ran = true;
    }

    /// WHETHER A FRESH `status` READ CAN MOVE THE LINE ON THE READ `shows` that the look at it
    /// settled nothing on ([`Self::turned`]): a turn read running since the write ([`Self::ran`])
    /// and a read that shows none of the line. A read that cannot say what the composer holds (a
    /// dialog over it, no composer drawn, a glyph's cell unread) keeps the turn's evidence for the
    /// next read that can, so the session reads `status` for the turn only where this says.
    #[must_use]
    pub fn weighs(&self, shows: &Shows) -> bool {
        let stands = match shows.stands {
            None if armed(Fault::CustodyTurnUnreadable) => Some(LeftStands::Nothing),
            stands => stands,
        };
        self.ran && stands == Some(LeftStands::Nothing)
    }

    /// WHAT A FRESH `status` READ AFTER A TURN READ RUNNING ([`Self::ran`]) SAYS OF THE LINE, on
    /// the read `shows` that the look at it settled nothing on ([`Turned`]). On a read that shows
    /// none of it ([`Self::weighs`]), the input word `word` clear settles it ([`Settles::Turn`]),
    /// which in order proves the worker read it by then, a turn run since its write; a line still
    /// unread, or one no word can say of, outlasts the turn ([`Self::outlasted`]). A read that
    /// cannot say what the composer holds is no evidence either way: the line may stand drawn
    /// under it, and a turn settled there would end the custody without handing the line back to
    /// its writer, the line then taken for a person's draft; the turn's evidence is kept, nothing
    /// moved, for the next read that can say. With no turn read running since the write, or since
    /// the last turn it outlasted, no word settles it: a clear read alone (a busy spell of break
    /// reads) proves only that the worker read it.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "TurnSettles",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "TurnOutlasted",
            project = "conformance_custody::project"
        )
    )]
    #[must_use]
    pub fn turned(&self, word: Option<&str>, shows: &Shows) -> Turned {
        if !self.weighs(shows) {
            Turned::Stands
        } else if word == Some("clear") || armed(Fault::CustodyTurnAnyWord) {
            Turned::Settles
        } else {
            Turned::Outlasts
        }
    }

    /// The line outlasted the turn ([`Turned::Outlasts`]) on the input word `word` read at `now`:
    /// what the read said moves [`Held`], and another turn read running is waited for (a worker
    /// that holds it queued draws it after).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "TurnOutlasted",
            project = "conformance_custody::project"
        )
    )]
    pub fn outlasted(&mut self, word: Option<&str>, now: Instant) {
        self.held = self.held.after(word, now);
        self.ran = false;
    }

    /// THE GATE'S `status` READ OF THE LINE, its input word `word` at `now`, for a write of a
    /// line (`writes_a_line`: a typed act, a command, a draft's Enter) or of anything else: the
    /// word moves [`Held`] (and a move starts the line's waits in a row again), and a line the
    /// worker read [`ECHO_HANG`] ago whose input still reads clear, where the write is a line's,
    /// is read again ([`GateRead::Again`]); every other read holds the write.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "GateHolds",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "AsksRestart",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "WaitsLost",
            project = "conformance_custody::project"
        )
    )]
    pub fn gate_read(&mut self, word: Option<&str>, writes_a_line: bool, now: Instant) -> GateRead {
        let was = self.held;
        self.held = self.held.after(word, now);
        if self.held != was {
            self.waits = 0;
        }
        let clear = word == Some("clear") || armed(Fault::CustodyLostBehind);
        let again = match self.held {
            Held::Read { since } => {
                writes_a_line
                    && clear
                    && (now >= since + ECHO_HANG || armed(Fault::CustodyLostWithinMinute))
            }
            Held::Unread | Held::Unknown => writes_a_line && armed(Fault::CustodyLostUnread),
        };
        if again {
            GateRead::Again
        } else {
            GateRead::Holds
        }
    }

    /// THE DUE THE LINE STANDS ON, from `now`: a read line's minute from its first clear read
    /// ([`ECHO_HANG`]), else the next of a doubling wait ([`custody_step`], counted).
    pub fn rearm(&mut self, now: Instant) -> Instant {
        match self.held {
            Held::Read { since } if since + ECHO_HANG > now => since + ECHO_HANG,
            _ => {
                let at = now + custody_step(self.waits);
                self.waits = self.waits.saturating_add(1);
                at
            }
        }
    }

    /// Whether a record of what holds the write (`said`: [`hold_said`]'s words, or
    /// [`NOT_AT_REST`]) is due: what holds it changed since the last, which this keeps.
    pub fn says(&mut self, said: String) -> bool {
        if self.said.as_deref() == Some(said.as_str()) {
            return false;
        }
        self.said = Some(said);
        true
    }

    /// THE LINE IS LOST ([`CustodyGate::Lost`]) at `now`: the first time since the last act
    /// submitted (`restarts`), where `lane` makes one, a restart in place ([`Loss::Restart`],
    /// `first` at its first ask alone, however often a host's deferral has it asked again and
    /// whatever holds' records come between); else a doubling wait that writes nothing
    /// ([`Loss::Wait`]), its step the next of the line's waits in a row (which the holds before it
    /// counted too, and the restart's asks leave alone), counted the loss's own for its records,
    /// and the line lost for good to the loop ([`Self::is_lost`], what the host is told).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "AsksRestart",
            project = "conformance_custody::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LineCustody",
            action = "WaitsLost",
            project = "conformance_custody::project"
        )
    )]
    pub fn lose(&mut self, restarts: &Restarts, lane: &Lane, now: Instant) -> Loss {
        let none = match restarts.spent() {
            Some(LostRestart::Made | LostRestart::Unseen) => Some(NoRestart::Again),
            Some(LostRestart::Unmade(step)) => Some(NoRestart::Unmade(step.clone())),
            None if !lane.relaunch => Some(NoRestart::RelaunchOff),
            None if !lane.host => Some(NoRestart::NoHost),
            None if !lane.can_restart => Some(NoRestart::NoLane),
            None => None,
        };
        let waits = self.waits;
        let until = now + custody_step(waits);
        match none {
            Some(why) => {
                self.waits = waits.saturating_add(1);
                let nth = self.lost_waits;
                self.lost_waits = nth.saturating_add(1);
                self.said = Some(LOST_WAITS.to_string());
                self.lost = true;
                Loss::Wait {
                    why,
                    until,
                    waits: nth,
                }
            }
            None => {
                let first = !self.restart_noted;
                self.restart_noted = true;
                self.said = Some(LOST_RESTARTING.to_string());
                Loss::Restart { until, first }
            }
        }
    }
}

/// What a line held lets the host do ([`LineHeld`]): [`LineHeld::Lost`] once the loop judged it
/// lost and makes no restart in place for it ([`Line::is_lost`]), else [`LineHeld::Stands`];
/// `None` where no line is held.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "LineCustody",
        action = "HostTypes",
        project = "conformance_custody::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "LineCustody",
        action = "HostUpgrades",
        project = "conformance_custody::project"
    )
)]
#[must_use]
pub fn line_held(line: Option<&Line>) -> Option<LineHeld> {
    let line = line?;
    if line.lost {
        Some(LineHeld::Lost)
    } else if armed(Fault::CustodyHostUntold) {
        None
    } else {
        Some(LineHeld::Stands)
    }
}
