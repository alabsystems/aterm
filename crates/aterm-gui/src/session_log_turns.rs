// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PROGRAM'S CLAIMED TURNS (ATERM-KI-1029-1): every edge the engine kept of a codex's turns
//! (OSC 7501, `aterm_core::terminal::program_state::ProgramTurns`), read past the log's own cursor
//! on every status sweep, into the codex run's claimed turns at the parse batch times
//! (`aterm_slog::TurnTracker::claim_edge`).
//!
//! The sweep reads the edges under its terminal guard ([`ProgramRead::of`]) and feeds them in one
//! hold of the timeline: on a foreground move, the edges before the leaving program's `clear` go
//! to the run still in front before the block read can close it ([`SessionLog::feed_leaving`]);
//! after the block read, the program bucket's tick and the foreground move, the rest go to the
//! codex run in front, wait while the program in front has no name or its run waits for its block
//! read, and are dropped under any other program ([`SessionLog::feed_front`]). A turn the run's
//! turn bucket refuses, and a turn whose edges cannot be told to a run, is counted, and the next
//! `program` mark (or the run's close) says how many (`dropped=`); turns the engine's ring lost
//! before the log read them are a `gap why=evicted lost=<n>`.

use aterm_core::terminal::program_state::{ProgramApp, ProgramStateWord, TurnCursor};
use aterm_slog::{ClaimEdge, EdgeOutcome, GapWhy, Stamp};

use super::SessionLog;

/// One edge of a codex's turn the engine kept, past the log's cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReadEdge {
    /// The report that drew it.
    seq: u64,
    /// The turn it belongs to
    /// ([`ProgramTurn::n`](aterm_core::terminal::program_state::ProgramTurn::n)).
    n: u64,
    edge: ClaimEdge,
    /// Its parse batch's wall clock; `None` where that batch read none (a restore's).
    wall_ms: Option<u64>,
    /// The report named codex (`app=codex`): another program's edges move no codex run.
    codex: bool,
}

/// What a status sweep read of the program's turns under its terminal guard: the edges past the
/// log's cursor, oldest first, and how far the log consumed them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProgramRead {
    edges: Vec<ReadEdge>,
    /// How many of `edges` were consumed, fed or dropped.
    done: usize,
    /// Turns kept past the cursor that the engine's ring lost before this read.
    lost: u64,
    /// Where a log that consumed every edge read stands.
    head: TurnCursor,
    /// The report's latest `clear` (`ProgramStateReport::cleared`).
    cleared: u64,
    /// The report's sequence went back past the cursor: another engine under a live log.
    went_back: bool,
    /// A read was made ([`Self::of`]); the default read feeds nothing and moves no cursor.
    read: bool,
}

/// What took the front from the program a foreground move's sweep saw leave
/// ([`SessionLog::feed_leaving`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Leaving {
    /// Nothing was in front before (a session's first reading), or the session's own shell.
    Nothing,
    /// A named program that is no codex.
    Other,
    /// A codex (its name honoured as one), or a group whose name was never read, which may have
    /// been.
    MaybeCodex,
}

/// What stands in front for the edges past the move ([`SessionLog::feed_front`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Front {
    /// The group in front has no name yet: its edges wait.
    Unnamed,
    /// Its name is honoured as a codex's (a codex, or a script runtime one runs under).
    Codex,
    /// Any other program.
    Other,
}

impl ProgramRead {
    /// The edges `term` kept past `cursor` (`None`: the log's first reading, every kept edge),
    /// read with its report under the sweep's one guard.
    pub(crate) fn of(term: &aterm_core::terminal::Terminal, cursor: Option<TurnCursor>) -> Self {
        let report = term.program_state();
        let mut turns = Vec::new();
        let lost = term.program_turns_after(cursor, &mut turns);
        let seq = cursor.map_or(0, |c| c.seq);
        let went_back = report.seq < seq;
        let mut edges = Vec::new();
        for t in &turns {
            if went_back {
                break;
            }
            let codex = t.app == ProgramApp::Codex;
            if let Some(b) = t.began.filter(|b| b.seq > seq || cursor.is_none()) {
                let edge = if t.resumed {
                    ClaimEdge::Resumed
                } else {
                    ClaimEdge::Began
                };
                edges.push(ReadEdge {
                    seq: b.seq,
                    n: t.n,
                    edge,
                    wall_ms: b.wall_ms,
                    codex,
                });
            }
            if let Some((word, e)) = t.ended.filter(|(_, e)| e.seq > seq || cursor.is_none()) {
                edges.push(ReadEdge {
                    seq: e.seq,
                    n: t.n,
                    edge: rest_edge(word),
                    wall_ms: e.wall_ms,
                    codex,
                });
            }
        }
        ProgramRead {
            edges,
            done: 0,
            lost: if went_back { 0 } else { lost },
            head: term.program_turns().head(report.seq),
            cleared: report.cleared,
            went_back,
            read: true,
        }
    }

    /// The edges not consumed yet.
    fn left(&self) -> &[ReadEdge] {
        &self.edges[self.done..]
    }

    /// THE READER OF THE ENGINE'S EDGES ALONE (a negative control): this read with its resumed
    /// beginnings taken out, as a host that reads no resumed entry would have it.
    #[cfg(test)]
    pub(crate) fn drop_resumed(&mut self) {
        self.edges.retain(|e| e.edge != ClaimEdge::Resumed);
    }
}

/// The claimed edge a rest of `word` is: `error` failed, any other rest rested.
const fn rest_edge(word: ProgramStateWord) -> ClaimEdge {
    match word {
        ProgramStateWord::Error => ClaimEdge::Failed,
        ProgramStateWord::Idle
        | ProgramStateWord::Done
        | ProgramStateWord::Working
        | ProgramStateWord::Blocked => ClaimEdge::Rested,
    }
}

/// How a batch of edges is consumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Disposition {
    /// Fed to the codex run in front.
    Fed,
    /// Dropped and counted: their turns are in no log, and the log says how many.
    Counted,
    /// Dropped with nothing said: they are no codex run's (a stale report under another program).
    Quiet,
}

impl SessionLog {
    /// Where the log has read the engine's turn edges to ([`ProgramRead::of`]); `None` before
    /// its first reading. Never carried: a successor's engine numbers its turns afresh.
    pub(crate) const fn program_cursor(&self) -> Option<TurnCursor> {
        self.program_cursor
    }

    /// A FOREGROUND MOVE'S EDGES, before the block read can close the run that left: the edges
    /// before the latest `clear` past the cursor are the leaving program's (codex-alab clears at
    /// its exit and before a job-control stop), fed to the codex run still in front, or dropped,
    /// counted when what left was a codex (`leaving`) and quietly otherwise. With no `clear` past
    /// the cursor every edge's writer is what left when that was a codex or a group never named,
    /// so they are dropped and counted; else they wait for the arriving program
    /// ([`Self::feed_front`]). Answers whether a record was written.
    pub(crate) fn feed_leaving(
        &mut self,
        read: &mut ProgramRead,
        leaving: Leaving,
        at: Stamp,
    ) -> bool {
        if !read.read || read.went_back {
            return false;
        }
        let cut = self.program_cursor.map_or(0, |c| c.seq);
        let split = read.cleared > cut || self.program_cursor.is_none() && read.cleared > 0;
        let upto = if split {
            read.left()
                .iter()
                .take_while(|e| e.seq < read.cleared)
                .count()
        } else if leaving == Leaving::MaybeCodex {
            read.left().len()
        } else {
            return false;
        };
        let how = if self.codex_in_front() {
            Disposition::Fed
        } else if leaving == Leaving::MaybeCodex {
            Disposition::Counted
        } else {
            Disposition::Quiet
        };
        let how = if split { how } else { Disposition::Counted };
        self.consume(read, upto, how, at)
    }

    /// THE EDGES PAST THE MOVE, after the block read, the bucket's tick and the foreground move:
    /// fed to the codex run in front; kept, the cursor where it was, while the program in front
    /// has no name or its run waits for its block read (`front`, and the names this log holds
    /// for the next block read); dropped under any other program, counted where its name is
    /// honoured as a codex's (a codex under a script runtime, whose run is no agent run) and
    /// quietly otherwise. Answers whether a record was written.
    pub(crate) fn feed_front(&mut self, read: &mut ProgramRead, front: Front, at: Stamp) -> bool {
        if !read.read {
            return false;
        }
        if read.went_back {
            self.program_cursor = Some(read.head);
            return false;
        }
        let how = if self.codex_in_front() {
            Disposition::Fed
        } else if front == Front::Unnamed || !self.pending.is_empty() {
            if read.left().is_empty() && read.lost == 0 {
                self.program_cursor = Some(read.head);
            }
            return false;
        } else if front == Front::Codex {
            Disposition::Counted
        } else {
            Disposition::Quiet
        };
        let all = read.left().len();
        self.consume(read, all, how, at)
    }

    /// THE GATE BEFORE THE FIX (a negative control): `read` consumed with nothing fed or said, the
    /// cursor at its head, as the host that dropped a move's and an unnamed front's edges had it.
    #[cfg(test)]
    pub(crate) fn skip_program_read(&mut self, read: &mut ProgramRead) {
        if read.read {
            read.done = read.edges.len();
            read.lost = 0;
            self.program_cursor = Some(read.head);
        }
    }

    /// Whether the run in front is a codex's agent run, which a claimed edge moves.
    fn codex_in_front(&self) -> bool {
        let run = self.front_run();
        self.codex_run() == Some(run)
    }

    /// Consume the next `n` of `read`'s edges `how`, then move the cursor past them (to the read's
    /// head once none is left). Answers whether a record was written.
    fn consume(&mut self, read: &mut ProgramRead, n: usize, how: Disposition, at: Stamp) -> bool {
        let edges: Vec<ReadEdge> = read.left()[..n].to_vec();
        read.done += n;
        let seq0 = self.log.last_seq();
        let lost = std::mem::take(&mut read.lost);
        match how {
            Disposition::Fed => {
                if lost > 0 {
                    self.log.gap(GapWhy::Evicted, lost, at);
                }
                self.feed_codex(&edges, at);
            }
            Disposition::Counted => {
                let turns = edges
                    .iter()
                    .filter(|e| e.codex && matches!(e.edge, ClaimEdge::Began | ClaimEdge::Resumed))
                    .count();
                self.observed.program_dropped(
                    u64::try_from(turns)
                        .unwrap_or(u64::MAX)
                        .saturating_add(lost),
                );
            }
            Disposition::Quiet => {}
        }
        self.program_cursor = Some(if read.left().is_empty() {
            read.head
        } else {
            let last = edges
                .last()
                .map_or(TurnCursor::default(), |e| TurnCursor { seq: e.seq, n: e.n });
            let prior = self.program_cursor.unwrap_or_default();
            TurnCursor {
                seq: last.seq.max(prior.seq),
                // An evicted prefix is consumed even when every retained edge waits for the
                // arriving program. Its next read must not report that same loss again.
                n: last.n.max(prior.n.saturating_add(lost)),
            }
        });
        self.log.last_seq() != seq0
    }

    /// Feed `edges` to the codex run in front, oldest first, each at its parse batch's wall clock
    /// (the sweep's where the batch read none), a turn it would open paying one token of the run's
    /// turn bucket and a refusal counted.
    fn feed_codex(&mut self, edges: &[ReadEdge], at: Stamp) {
        let run = self.front_run();
        self.enter_program_run(run, at);
        self.reconcile_person(at);
        self.resync();
        let Self {
            log,
            agent,
            observed,
            ..
        } = self;
        let Some(a) = agent.as_mut() else {
            return;
        };
        let mut refused = 0_u64;
        for e in edges.iter().filter(|e| e.codex) {
            let edge_at = super::stamp_at_wall(at, e.wall_ms.unwrap_or(at.wall_ms));
            let outcome = a.tracker.claim_edge(log, e.edge, edge_at, &mut || {
                observed.spend_turn(at.mono_ms)
            });
            refused += u64::from(outcome == EdgeOutcome::Refused);
        }
        observed.program_dropped(refused);
    }
}

#[cfg(test)]
#[path = "session_log_turns_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "session_log_turns_conformance.rs"]
mod conformance;
