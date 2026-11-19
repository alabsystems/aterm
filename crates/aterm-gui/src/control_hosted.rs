// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE JOIN: `hosted <start|end> kind=<word> [exit=<n>]`, a run's post of its start or its end
//! to the aterm that hosts it, answered with the session the run belongs to.
//!
//! A run is a `verify land` landing or any other `verify` pass (`aterm-verify`'s `hosting`).
//! Which session it belongs to is measured here, out of the run's reach: aterm reads the
//! posting connection's peer process and its chain of parents
//! ([`crate::typed_by::Who::parent_chain`], as a description's author is placed,
//! [`super::control_session::desc_author_as`]) and finds the live session whose program (its
//! shell, the root of its process tree) is that process or an ancestor of it
//! ([`super::control_session::owner_tab_sid`]). The answer is that session's ID as `aterm ctl
//! @self` addresses it: a minted `t` ID, or the legacy `s-` ID of a session opened before it,
//! placed the same way. The request names no session and takes no word that could: a
//! variable a process sets in its environment reaches nothing here.
//!
//! A process in no session's tree (a LaunchAgent, a bare ssh, a process that left its tree) is
//! hosted by none, and its run belongs to its computer. So is one whose chain of parents is
//! longer than aterm reads with no session's program in what was read: a program may be above
//! it, so the run is placed nowhere rather than guessed.
//!
//! ONE CASE TAKES A SECOND STEP. `verify land --tab` runs the landing as a program worker in a
//! session of its own, so lineage places the run in the worker's session, which is not the
//! session whose work is landing. A run a program worker's session hosts while that worker's
//! task is open, started by another session of this Mac, is credited to the session that
//! started it; `worker` names the worker's session. Without it the session that asked would
//! read as landing nothing while its landing runs.
//!
//! Who started the worker is measured too, and not taken from the spawn's words. The worker's
//! task record names a session ([`crate::task_record::TaskRecord::from`]), but that is the
//! spawn request's `from=`, which `aterm ctl spawn` fills from `ATERM_PARENT_SESSION_ID` and
//! a `task mark` can rewrite: a word, as the variable is. So the credit is given only where the
//! worker session's open mark agrees: its `by=` is the session aterm placed the spawn request's
//! own connection in when it opened the worker
//! ([`crate::session_log::SessionLog::tab_opened_by`], which
//! [`super::control_session::caller_actor_of`] read from that connection's process, its parents
//! and its process group). A spawn that names a session its process does not run in credits
//! nobody: the run stays the worker session's own.

use std::sync::Arc;

use crate::SessionCtx;
use crate::session_store::Store;
use crate::task_record::{TaskRecord, TaskState};
use crate::typed_by::Who;

/// The usage line of a `hosted` request that does not parse.
pub(crate) const USAGE: &str = "ERR usage: hosted <start|end> kind=<word> [exit=<n>]\n";

/// The longest `kind` a post names, in bytes.
const KIND_MAX: usize = 32;

/// Which end of a run a post is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Start,
    End,
}

impl Phase {
    const fn word(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::End => "end",
        }
    }
}

/// A `hosted` request, parsed: nothing in it names a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Post<'a> {
    pub(crate) phase: Phase,
    /// The run's kind: one lower-case word (`land`, `verify`).
    pub(crate) kind: &'a str,
    /// An end's exit code, where the run gave one.
    pub(crate) exit: Option<i32>,
}

impl<'a> Post<'a> {
    /// `rest`, a `hosted` request's words, as a post; `None` for any other shape: a phase that
    /// is neither `start` nor `end`, no `kind=` or one that is not a lower-case letter followed
    /// by at most 31 lower-case letters, digits and `-`, an `exit=` on a start or one that is
    /// no canonical decimal `i32`, a word given twice, or any other word (a `session=`, a
    /// `sid=`: a post names no session).
    pub(crate) fn parse(rest: &'a str) -> Option<Self> {
        let mut words = rest.split_whitespace();
        let phase = match words.next()? {
            "start" => Phase::Start,
            "end" => Phase::End,
            _ => return None,
        };
        let (mut kind, mut exit) = (None, None);
        for word in words {
            match word.split_once('=')? {
                ("kind", value) if kind.is_none() && is_kind(value) => kind = Some(value),
                ("exit", value) if exit.is_none() && phase == Phase::End => {
                    exit = Some(
                        value
                            .parse::<i32>()
                            .ok()
                            .filter(|code| code.to_string() == value)?,
                    );
                }
                _ => return None,
            }
        }
        Some(Self {
            phase,
            kind: kind?,
            exit,
        })
    }
}

/// Whether `kind` is a run kind's word.
fn is_kind(kind: &str) -> bool {
    let mut bytes = kind.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && kind.len() <= KIND_MAX
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The session a run belongs to, as [`place`] measured it. The default is none: the run
/// belongs to its computer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Placed {
    /// The session the run is credited to.
    pub(crate) hosted: Option<String>,
    /// The worker session the run runs in, where [`Self::hosted`] is the session that started
    /// that worker.
    pub(crate) worker: Option<String>,
}

impl Placed {
    /// ` hosted=<sid|-> worker=<sid|->`, as the reply and the log line carry it.
    fn words(&self) -> String {
        format!(
            "hosted={} worker={}",
            self.hosted.as_deref().unwrap_or("-"),
            self.worker.as_deref().unwrap_or("-")
        )
    }
}

/// The session the run of process `who` belongs to, over the sessions `store` registers now.
///
/// `who` is placed by its chain of parents alone: the live session whose program is `who`'s
/// process or an ancestor of it hosts the run. A chain cut at aterm's bound with no session's
/// program in it, a process in no session's tree, and a caller that is no local process (aterm
/// itself, the message bus, a script with no peer process) are hosted by none.
///
/// Where the hosting session is a program worker's with its task open, opened by the session
/// its task record names ([`asked_by`]), the run is credited to that session, and the hosting
/// session is its `worker`.
///
/// It reads the store, then, with the store's lock let go, the hosting session's timeline.
pub(crate) fn place(who: &Who, store: &Store) -> Placed {
    let who = who.clone().parent_chain();
    let Some(sid) = super::control_session::owner_tab_sid(&who, store) else {
        return Placed::default();
    };
    let ctx: Option<Arc<SessionCtx>> = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .live_handles()
        .find(|handle| handle.sid.as_str() == sid)
        .map(|handle| handle.ctx.clone());
    let asker = ctx.and_then(|ctx| {
        let timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let opened_by = timeline.slog().tab_opened_by();
        timeline
            .task()
            .and_then(|task| asked_by(task, opened_by, &sid))
            .map(str::to_owned)
    });
    match asker {
        Some(asker) => Placed {
            hosted: Some(asker),
            worker: Some(sid),
        },
        None => Placed {
            hosted: Some(sid),
            worker: None,
        },
    }
}

/// The session that asked for the run a program worker's session `worker` hosts, by the
/// worker's task record `task` and who opened the worker's session, `opened_by` (its open
/// mark's `by=`): the record's `from`, where the record is a program worker's
/// ([`TaskRecord::runs_a_program`]) whose task is open (`working`, or `stalled` by a deadline
/// that read it while its program ran), started by another session of this Mac, and that
/// session is the one aterm placed the spawn's request in. `None` for any other record: an
/// agent's worker (its own work is what lands), a task that concluded (a run started in the
/// worker's session afterwards is that session's own), one a `ctl` client or another Mac's
/// session started (no session here asked), one that names the worker itself, and one whose
/// `from` names a session other than the one that opened the worker (a spawn's `from=` or a
/// `task mark`'s is a word of a request, and no word moves a run).
fn asked_by<'a>(
    task: &'a TaskRecord,
    opened_by: Option<aterm_slog::Actor>,
    worker: &str,
) -> Option<&'a str> {
    let open = matches!(task.state, TaskState::Working | TaskState::Stalled);
    let asker = task.from.as_str();
    let opened_it = crate::session_log::sid_ident(asker)
        .is_some_and(|asker| opened_by == Some(aterm_slog::Actor::Sid(asker)));
    (task.runs_a_program()
        && open
        && task.manager_node.is_none()
        && asker != worker
        && crate::keeper_link::valid_sid(asker)
        && opened_it)
        .then_some(asker)
}

/// `hosted <start|end> kind=<word> [exit=<n>]` -> `OK hosted=<sid|-> worker=<sid|->`: the
/// session the posting process's run belongs to ([`place`]), `client` naming the connection's
/// process (the connection's peer, or a test's process and lineage; read only for a request
/// that parses). One line of aterm.log records the post: its phase, kind, exit code and the
/// answer, all words and ids.
pub(crate) fn cmd_hosted(store: &Store, rest: &str, client: impl FnOnce() -> Who) -> String {
    let Some(post) = Post::parse(rest) else {
        return USAGE.to_string();
    };
    let placed = place(&client(), store);
    let exit = post
        .exit
        .map_or_else(|| "-".to_owned(), |code| code.to_string());
    aterm_log::info!(
        "hosted {} kind={} exit={exit} {}",
        post.phase.word(),
        post.kind,
        placed.words()
    );
    format!("OK {}\n", placed.words())
}

#[cfg(all(test, unix))]
#[path = "control_hosted_tests.rs"]
mod tests;
