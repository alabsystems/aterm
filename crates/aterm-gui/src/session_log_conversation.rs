// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CONVERSATION RECORDS' ROWS (HX-15h): a harness's `turn`, `tool`, `subagent`, `msg` and
//! `diff` records, written as harness claims under their own words through the stream's one write
//! arm. Every row is built from one [`HarnessStamp`] (`holder=`, `hseq=`, `src=harness`,
//! `conf=claim`, `by=<the tab>`, and `fp=` for a record whose body went to the tab's Out ring),
//! so no writer of these rows can leave a stamp out (D30); none spends the session's `mark`
//! budget (D10). A `turn` goes through the run's turn tracker, the one writer of a run's turns
//! ([`aterm_slog::TurnTracker::stream_turn`]); a `tool` or `subagent` is a step under the open
//! turn, else the agent run; a `msg` or `diff` is a mark on the agent run.

use aterm_slog::{Fields, HarnessStamp, Key, Kind, Node, ShortId, Stamp, StreamTurn, Val};

use super::{SessionLog, with};
use crate::session_timeline::{ConversationTaken, StreamedStep};

/// THE CLASS OF A STREAM STEP (`cls=`), as the transcript feeders class the same call, so the
/// fold's per-class counts and the chrome's tool words read the stream's steps as they read the
/// transcript's: a `subagent` is `agent`; a `tool` is classed by its name with Claude Code's
/// table, else Codex's (`exec_command` is `shell`), else `other`.
fn step_class(kind: Kind, name: Option<&str>) -> &'static str {
    use aterm_agent::harness::{codex_steps, steps};
    match (kind, name) {
        (Kind::Subagent, _) => "agent",
        (_, Some(name)) => match steps::tool_class(name).0 {
            "other" => codex_steps::tool_class(None, name).0,
            cls => cls,
        },
        (_, None) => "other",
    }
}

/// The most `tool` and `subagent` steps the stream holds open at once for a tab, and the most
/// subagents its current-state record counts open: a `started` beyond it is acknowledged
/// `dropped=refused:steps` and opens nothing, as a harness that never closes its steps would
/// otherwise grow the list without end. A step closed by its turn's close (`out=gone`) leaves the
/// list at the next record.
pub(crate) const STREAM_STEPS_MAX: usize = 64;

#[cfg_attr(
    not(unix),
    allow(dead_code, reason = "only the Unix control stream applies a record")
)]
impl SessionLog {
    /// A `turn` record through the open agent run's tracker ([`aterm_slog::TurnTracker::
    /// stream_turn`]): whether it stands. A tab with no agent run open writes nothing (the
    /// stream opens one before it applies a record, D15, unless its `state` said `ended`).
    pub(crate) fn stream_turn(
        &mut self,
        edge: StreamTurn<'_>,
        stamp: &HarnessStamp,
        at: Stamp,
    ) -> bool {
        let Self { log, agent, .. } = self;
        let Some(a) = agent.as_mut().filter(|a| log.is_open(a.node)) else {
            return false;
        };
        a.tracker.stream_turn(log, edge, stamp, at)
    }

    /// A `tool` (`kind` [`Kind::Tool`], `name` its tool) or `subagent` record's step, keyed by its
    /// kind and the record's `id`: a `started` opens one under the open turn, else the agent run
    /// (`tool=<name>` where the name fits, `vref=<id>` where the id is a log ID), and a repeated
    /// `started` of an open one writes nothing; an end closes the one open (`out=ok|error|
    /// interrupted`), or opens and closes one at once where none is open, so every applied record
    /// has its row.
    pub(crate) fn stream_step(
        &mut self,
        kind: Kind,
        id: &str,
        name: Option<&str>,
        state: StreamedStep,
        stamp: &HarnessStamp,
        at: Stamp,
    ) -> ConversationTaken {
        let mut steps = std::mem::take(&mut self.stream_steps);
        steps.retain(|(_, _, node)| self.log.is_open(*node));
        let open = steps.iter().position(|(k, i, _)| *k == kind && i == id);
        let taken = self.write_stream_step(&mut steps, open, (kind, id, name), state, stamp, at);
        self.stream_steps = steps;
        taken
    }

    fn write_stream_step(
        &mut self,
        steps: &mut Vec<(Kind, String, Node)>,
        open: Option<usize>,
        (kind, id, name): (Kind, &str, Option<&str>),
        state: StreamedStep,
        stamp: &HarnessStamp,
        at: Stamp,
    ) -> ConversationTaken {
        const LOG: ConversationTaken = ConversationTaken::Refused("log");
        let named = || {
            let f = with(
                Fields::new(),
                Key::Cls,
                Some(Val::Word(step_class(kind, name))),
            );
            let f = with(
                f,
                Key::Tool,
                name.and_then(|name| aterm_slog::BoundedText::new(Key::Tool, name))
                    .map(Val::Text),
            );
            with(f, Key::Vref, ShortId::new(id).map(Val::Id))
        };
        let opened = |log: &mut SessionLog| -> Option<Node> {
            let parent = log
                .agent
                .as_ref()
                .and_then(|a| a.tracker.turn())
                .or_else(|| log.stream_run());
            let mut row = stamp.open(kind, named()).ok()?;
            if let Some(parent) = parent.filter(|parent| log.log.is_open(*parent)) {
                row = row.under(parent);
            }
            log.log.open(row, at).ok()
        };
        match state.out() {
            None if open.is_some() => ConversationTaken::Applied,
            None if steps.len() >= STREAM_STEPS_MAX => ConversationTaken::Refused("steps"),
            None => match opened(self) {
                Some(node) => {
                    steps.push((kind, id.to_string(), node));
                    ConversationTaken::Applied
                }
                None => LOG,
            },
            Some(out) => {
                let node = match open {
                    Some(i) => steps.remove(i).2,
                    None => match opened(self) {
                        Some(node) => node,
                        None => return LOG,
                    },
                };
                let Ok(close) = stamp.close(aterm_slog::Out::Step(out), Fields::new()) else {
                    return LOG;
                };
                if self.log.close(node, close, at).is_ok() {
                    ConversationTaken::Applied
                } else {
                    LOG
                }
            }
        }
    }

    /// The step the stream's `kind` record `id` holds open, if one does ([`Self::stream_step`]).
    pub(crate) fn stream_step_node(&self, kind: Kind, id: &str) -> Option<Node> {
        self.stream_steps
            .iter()
            .find(|(k, i, node)| *k == kind && i == id && self.log.is_open(*node))
            .map(|(.., node)| *node)
    }

    /// A `msg` or `diff` record's row, `kind` [`Kind::Msg`] or [`Kind::Diff`]: `mark <agent run>
    /// msg|diff hseq= fp= holder=`, on the session where no run is open. Its body is the Out
    /// ring's, never the row's. Whether it was written.
    pub(crate) fn stream_out(&mut self, kind: Kind, stamp: &HarnessStamp, at: Stamp) -> bool {
        let node = self.stream_run().unwrap_or(Node::SESSION);
        stamp
            .mark(kind, Fields::new())
            .ok()
            .and_then(|mark| self.log.mark(mark.at(node), at).ok())
            .is_some()
    }
}
