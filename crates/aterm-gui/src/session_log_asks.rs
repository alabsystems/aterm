// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE `ask`, `asked` AND `wall` RECORDS' ROWS (HX-15i1), each a harness claim built from one
//! [`HarnessStamp`] (`holder=`, `hseq=`, `src=harness`, `conf=claim`, `by=<the tab>`, D30) and
//! spending no mark budget (D10).
//!
//! An `ask` opens an approval or question step under the open turn, else the agent run: its open
//! raises the need the fold gives such a step (`approval:other`, since the record names no box,
//! or `question`). Its `asked` writes `mark act word=approve|deny|answer rule=asked as=<who
//! answered>` on that step (D31) and closes it `out=approved|declined|answered`; an `asked` whose
//! ask holds no step open here still writes its act, on the agent run. A `wall` opens a wall step
//! (`wall=<kind>` where aterm knows the kind, else none: the bare `wall`) under the agent run,
//! since a wall outlives the turn it ended; it stands until the stream says it passed (a `state`
//! that is not `walled`, a turn that starts, a wall of another kind) or stops feeding the kind.
//!
//! While the stream feeds a kind the run's turn tracker opens no screen step of it
//! ([`aterm_slog::TurnTracker::set_stream_steps`]), so each box and wall has one writer. A step
//! the stream left open when it stops feeding its kind is closed by aterm, `out=passed
//! src=aterm conf=strong by=aterm holder=<who opened it>`, as the end of a `compact` feed closes
//! that step ([`SessionLog::end_stream_asks`]).

use aterm_slog::{Fields, HarnessStamp, Key, Kind, Node, Out, Stamp, StepOut, Val, WallKind};

use super::conversation::STREAM_STEPS_MAX;
use super::{SessionLog, StreamDown, with};
use crate::session_timeline::{ConversationTaken, StreamedAsk, StreamedReply};

/// How many of the stream's applied `ask` records a log remembers for their `asked`
/// ([`KnownAsk`]): four times the open steps it may hold; an older one is forgotten.
const ASKS_KNOWN: usize = 4 * STREAM_STEPS_MAX;

/// An `ask` record this log's stream applied ([`SessionLog::stream_ask`]), as its `asked` is
/// judged against it ([`SessionLog::stream_asked`]): an `asked` of another kind, or a second
/// one, answers nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    allow(dead_code, reason = "only the Unix control stream applies a record")
)]
pub(crate) struct KnownAsk {
    /// The claim's holder whose record it was: each harness numbers its own records.
    holder: String,
    /// The record's `seq`.
    hseq: u64,
    /// What it asked for.
    kind: StreamedAsk,
    /// An `asked` answered it.
    answered: bool,
}

/// An `ask` step the stream holds open ([`SessionLog::stream_ask`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AskStep {
    /// The `ask` record's `seq`, which its `asked` names.
    hseq: u64,
    /// What it asks for.
    kind: StreamedAsk,
    /// When it arrived and how long after the harness said it will answer it, on the awake
    /// clock (`None`: it named no time, as while it waits for a person).
    will: Option<(u64, u64)>,
    node: Node,
    /// The claim's holder whose record opened it.
    holder: String,
}

/// The wall step the stream's latest `wall` record opened ([`SessionLog::stream_wall`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WallStep {
    /// Its kind, `None` for one aterm does not know.
    kind: Option<WallKind>,
    node: Node,
    /// The claim's holder whose record opened it.
    holder: String,
}

/// WHAT THE STREAM'S OPEN STEPS HOLD of aterm's progress watch
/// ([`crate::harness_watchdog::look`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StreamHolds {
    /// The turn the stream's `turn` records opened stands open in the log (the run's tracker
    /// takes its turns from the stream, [`aterm_slog::TurnTracker::stream_turns`]).
    pub(crate) turn: bool,
    /// An `ask` step stands open: the agent waits on its answer, which is no stalled turn.
    pub(crate) ask: bool,
    /// The open ask whose answer time comes first, each counted from the watch's floor at the
    /// earliest (its `will_in_ms` after the later of its arrival and that floor): that time on
    /// the awake clock, and the ask's `seq`.
    pub(crate) ask_due: Option<(u64, u64)>,
    /// The stream's wall step stands: the agent waits out its wall.
    pub(crate) wall: bool,
    /// A `tool` or `subagent` step the stream opened stands: progress is in flight.
    pub(crate) steps: bool,
}

#[cfg_attr(
    not(unix),
    allow(dead_code, reason = "only the Unix control stream applies a record")
)]
impl SessionLog {
    /// An `ask` RECORD's step: an approval or question step (`kind`) under the open turn, else
    /// the agent run, stamped, its arrival and answer time `will` kept for the progress watch.
    /// At most [`STREAM_STEPS_MAX`] stand open at once: one more is refused `steps` and opens
    /// nothing.
    pub(crate) fn stream_ask(
        &mut self,
        kind: StreamedAsk,
        will: Option<(u64, u64)>,
        stamp: &HarnessStamp,
        at: Stamp,
    ) -> ConversationTaken {
        const LOG: ConversationTaken = ConversationTaken::Refused("log");
        let Some(hseq) = stamp.hseq else {
            return LOG;
        };
        let mut asks = std::mem::take(&mut self.stream_asks);
        asks.retain(|ask| self.log.is_open(ask.node));
        let taken = if asks.len() >= STREAM_STEPS_MAX {
            ConversationTaken::Refused("steps")
        } else {
            let parent = self
                .agent
                .as_ref()
                .and_then(|a| a.tracker.turn())
                .or_else(|| self.stream_run())
                .filter(|parent| self.log.is_open(*parent));
            let opened = stamp.open(kind.step(), Fields::new()).ok().and_then(|row| {
                let row = match parent {
                    Some(parent) => row.under(parent),
                    None => row,
                };
                self.log.open(row, at).ok()
            });
            match opened {
                Some(node) => {
                    asks.push(AskStep {
                        hseq,
                        kind,
                        will,
                        node,
                        holder: stamp.holder.as_str().to_string(),
                    });
                    if self.stream_known.len() >= ASKS_KNOWN {
                        self.stream_known.pop_front();
                    }
                    self.stream_known.push_back(KnownAsk {
                        holder: stamp.holder.as_str().to_string(),
                        hseq,
                        kind,
                        answered: false,
                    });
                    ConversationTaken::Applied
                }
                None => LOG,
            }
        };
        self.stream_asks = asks;
        taken
    }

    /// An `asked` RECORD's rows (D31): `mark act word=<reply> rule=asked as=<answerer>` on the
    /// step of the ask whose `seq` is `ask`, then that step's close `out=approved|declined|
    /// answered`, both stamped. Where no step of that ask stands open (it closed with its turn,
    /// or it was asked before this run) the act is written on the agent run, else the session,
    /// and closes nothing. An answerer the log refuses as a name writes no `as=`.
    ///
    /// An `asked` of an ask this log knows its claim's holder applied ([`KnownAsk`]) answers it
    /// once and only as its own kind: one whose ask an earlier `asked` answered, or whose `kind`
    /// is not that ask's, is refused `ask` and writes nothing. One of an ask this log does not
    /// know (another run's, as a successor's after a seamless update) is written as above.
    /// `Refused("log")` where the log refused a row.
    pub(crate) fn stream_asked(
        &mut self,
        ask: u64,
        kind: StreamedAsk,
        reply: StreamedReply,
        answerer: &str,
        stamp: &HarnessStamp,
        at: Stamp,
    ) -> ConversationTaken {
        const LOG: ConversationTaken = ConversationTaken::Refused("log");
        let holder = stamp.holder.as_str();
        let known = self
            .stream_known
            .iter()
            .position(|known| known.holder == holder && known.hseq == ask);
        if known.is_some_and(|i| {
            let known = &self.stream_known[i];
            known.answered || known.kind != kind
        }) {
            return ConversationTaken::Refused("ask");
        }
        self.stream_asks.retain(|held| self.log.is_open(held.node));
        let held = self
            .stream_asks
            .iter()
            .position(|held| held.holder == holder && held.hseq == ask);
        if held.is_some_and(|i| self.stream_asks[i].kind != kind) {
            return ConversationTaken::Refused("ask");
        }
        let Ok(f) = Fields::new()
            .word(Key::Word, reply.word())
            .and_then(|f| f.id(Key::Rule, "asked"))
        else {
            return LOG;
        };
        let f = with(
            f,
            Key::As,
            aterm_slog::BoundedText::new(Key::As, answerer).map(Val::Text),
        );
        let on = held
            .map(|i| self.stream_asks[i].node)
            .or_else(|| self.stream_run())
            .unwrap_or(Node::SESSION);
        let Ok(mark) = stamp.mark(Kind::Act, f) else {
            return LOG;
        };
        if self.log.mark(mark.at(on), at).is_err() {
            return LOG;
        }
        if let Some(i) = known {
            self.stream_known[i].answered = true;
        }
        let Some(i) = held else {
            return ConversationTaken::Applied;
        };
        let step = self.stream_asks.remove(i);
        let closed = stamp
            .close(Out::Step(reply.out()), Fields::new())
            .ok()
            .is_some_and(|close| self.log.close(step.node, close, at).is_ok());
        if closed {
            ConversationTaken::Applied
        } else {
            LOG
        }
    }

    /// A `wall` RECORD's step: a wall step (`wall=<kind>` for a kind aterm knows) under the agent
    /// run, stamped. A wall of the same kind already standing writes nothing; one of another kind
    /// passes first ([`Self::stream_wall_passed`]). Whether the record stands.
    pub(crate) fn stream_wall(
        &mut self,
        kind: Option<WallKind>,
        stamp: &HarnessStamp,
        at: Stamp,
    ) -> bool {
        if self
            .stream_wall
            .as_ref()
            .is_some_and(|wall| wall.kind == kind && self.log.is_open(wall.node))
        {
            return true;
        }
        self.stream_wall_passed(stamp, at);
        let f = with(
            Fields::new(),
            Key::Wall,
            kind.map(|kind| Val::Word(kind.name())),
        );
        let parent = self.stream_run();
        let Some(node) = stamp.open(Kind::Wall, f).ok().and_then(|row| {
            let row = match parent {
                Some(parent) => row.under(parent),
                None => row,
            };
            self.log.open(row, at).ok()
        }) else {
            return false;
        };
        self.stream_wall = Some(WallStep {
            kind,
            node,
            holder: stamp.holder.as_str().to_string(),
        });
        true
    }

    /// THE STREAM SAYS ITS WALL PASSED (a `state` that is not `walled`, a `turn` that starts, a
    /// `wall` of another kind): the wall step it opened closes `out=passed` under `stamp`, the
    /// record that said so. Whether one closed.
    pub(crate) fn stream_wall_passed(&mut self, stamp: &HarnessStamp, at: Stamp) -> bool {
        let Some(wall) = self.stream_wall.take() else {
            return false;
        };
        self.log.is_open(wall.node)
            && stamp
                .close(Out::Step(StepOut::Passed), Fields::new())
                .ok()
                .is_some_and(|close| self.log.close(wall.node, close, at).is_ok())
    }

    /// THE STREAM NO LONGER FEEDS A KIND (its claim was released, lapsed, placed by another
    /// claimant or renewed without the scope, or the log refused the kind's row): each `ask`
    /// step of a kind `down` no longer stands down for, and the wall step where it no longer
    /// stands the wall readers down, has nobody left to close it, so aterm closes it `out=passed
    /// src=aterm conf=strong by=aterm holder=<who opened it>` (how it ended is not known). Its
    /// reader opens the step again where the screen still shows it. Whether one closed.
    pub(crate) fn end_stream_asks(&mut self, down: StreamDown, at: Stamp) -> bool {
        let fed = |kind: StreamedAsk| match kind {
            StreamedAsk::Approval => down.approvals,
            StreamedAsk::Question => down.questions,
        };
        let mut ended: Vec<(Node, String)> = Vec::new();
        self.stream_asks.retain(|ask| {
            let keep = fed(ask.kind);
            if !keep {
                ended.push((ask.node, ask.holder.clone()));
            }
            keep
        });
        if !down.walls
            && let Some(wall) = self.stream_wall.take()
        {
            ended.push((wall.node, wall.holder));
        }
        let mut closed = false;
        for (node, holder) in ended {
            if self.log.is_open(node)
                && let Some(close) = Self::passed_stream_compact(&holder)
            {
                closed |= self.log.close(node, close, at).is_ok();
            }
        }
        closed
    }

    /// THE READERS THE STREAM GAVE BACK TAKE WHAT IS STILL SHOWN, at the settle of the
    /// published word that follows the end of a stand-down ([`aterm_slog::TurnTracker::retake`]):
    /// the box or wall the word still shows opens its screen step again, and so does a claimed
    /// box withheld while the stream fed its kind. Whether a step opened.
    pub(crate) fn retake_shown(&mut self, at: Stamp) -> bool {
        let Self { log, agent, .. } = self;
        agent.as_mut().is_some_and(|a| a.tracker.retake(log, at))
    }

    /// Whether a kind the stream gave back waits on [`Self::retake_shown`].
    pub(crate) fn retake_due(&self) -> bool {
        self.agent.as_ref().is_some_and(|a| a.tracker.retake_due())
    }

    /// What the stream's open steps hold of the progress watch ([`StreamHolds`]), each open
    /// ask's answer time counted from `floor` at the earliest.
    pub(crate) fn stream_holds(&self, floor: u64) -> StreamHolds {
        let open = |node: Node| self.log.is_open(node);
        let asks = self.stream_asks.iter().filter(|ask| open(ask.node));
        StreamHolds {
            turn: self
                .agent
                .as_ref()
                .is_some_and(|a| a.tracker.stream_turns() && a.tracker.turn().is_some_and(open)),
            ask: self.stream_asks.iter().any(|ask| open(ask.node)),
            ask_due: asks
                .filter_map(|ask| {
                    ask.will
                        .map(|(arrived, will)| (arrived.max(floor).saturating_add(will), ask.hseq))
                })
                .min(),
            wall: self
                .stream_wall
                .as_ref()
                .is_some_and(|wall| open(wall.node)),
            steps: self.stream_steps.iter().any(|(.., node)| open(*node)),
        }
    }

    /// The step the stream's `ask` record `hseq` holds open, if one does.
    #[cfg(test)]
    pub(crate) fn stream_ask_node(&self, hseq: u64) -> Option<Node> {
        self.stream_asks
            .iter()
            .find(|ask| ask.hseq == hseq && self.log.is_open(ask.node))
            .map(|ask| ask.node)
    }

    /// The wall step the stream's `wall` record holds open, if one does.
    #[cfg(test)]
    pub(crate) fn stream_wall_node(&self) -> Option<Node> {
        self.stream_wall
            .as_ref()
            .map(|wall| wall.node)
            .filter(|node| self.log.is_open(*node))
    }
}
