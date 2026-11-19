// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE JOIN: which session a verify run belongs to, asked of the aterm instance that hosts it.
//!
//! A run posts its start and its end to the hosting instance on the path a pass already tells
//! it what it ended ([`crate::runs::ToInstance`], [`crate::runs::converse`]): one
//! `hosted <start|end> kind=<word> [exit=<n>]` line (`aterm ctl help hosted`). The instance
//! places the posting process by its chain of parents, finds the session whose program is its
//! ancestor, and answers `OK hosted=<sid|-> worker=<sid|->`: the session the run is credited to
//! and, where that run is a program worker's (`verify land --tab`), the worker session it runs
//! in. The request names no session, and no variable of this process's environment reaches the
//! answer: `ATERM_PARENT_SESSION_ID` finds the instance to ask and nothing more.
//!
//! The post is best effort under the pass's own bound ([`crate::runs::POST_BOUND`]). With no
//! instance to ask (a headless Mac, a bare ssh, a LaunchAgent), one that does not know the verb
//! or none that answers in time, the run goes on and names no session: it belongs to its
//! computer.

use std::time::Instant;

use crate::runs::{POST_BOUND, ToInstance, converse, within};

/// The control verb a run posts.
pub const VERB: &str = "hosted";
/// A landing's kind: the `verify land` controller.
pub const LAND: &str = "land";
/// Any other verify pass's kind.
pub const VERIFY: &str = "verify";

/// Which session a run belongs to, as the hosting instance placed the posting process. The
/// default is no hosting session known: the run belongs to its computer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hosted {
    /// The session the run is credited to: the one whose program is the posting process's
    /// ancestor, or, for a program worker's run, the session that started the worker.
    pub hosted: Option<String>,
    /// The worker session the run runs in, where [`Self::hosted`] names the session that asked
    /// for it; never without [`Self::hosted`], and never the same session.
    pub worker: Option<String>,
}

impl Hosted {
    /// What `reply`, the instance's answer to a `hosted` post, says: `OK hosted=<sid|->
    /// worker=<sid|->`, each id a session's (`t` and 10 base32 characters, or a legacy `s-` and
    /// 20 hex digits). Any other answer (an instance that does not know the verb, a refusal, a
    /// word that is no session id, a worker with no session to credit or one that is that
    /// session) names nothing.
    #[must_use]
    pub fn of_reply(reply: &str) -> Self {
        let mut words = reply.split_whitespace();
        if words.next() != Some("OK") {
            return Self::default();
        }
        let mut field = |key: &str| {
            let value = words.next()?.strip_prefix(key)?.strip_prefix('=')?;
            match value {
                "-" => Some(None),
                sid if is_session_id(sid) => Some(Some(sid.to_owned())),
                _ => None,
            }
        };
        let (Some(hosted), Some(worker)) = (field("hosted"), field("worker")) else {
            return Self::default();
        };
        if words.next().is_some() || (worker.is_some() && (hosted.is_none() || worker == hosted)) {
            return Self::default();
        }
        Self { hosted, worker }
    }
}

/// Whether `text` is a session's id: the minted `t` form or a legacy `s-` one, which a session
/// opened before the `t` form holds until aterm restores it.
#[must_use]
pub fn is_session_id(text: &str) -> bool {
    aterm_types::tab_id::is_tab_id(text) || aterm_types::tab_id::is_legacy(text)
}

/// Whether `kind` is a run kind's word: a lower-case letter, then at most 31 more of lower-case
/// letters, digits and `-`.
#[must_use]
pub fn is_kind(kind: &str) -> bool {
    let mut bytes = kind.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && kind.len() <= 32
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The request line of a run's start.
#[must_use]
pub fn start_line(kind: &str) -> String {
    format!("{VERB} start kind={kind}")
}

/// The request line of a run's end, with the code it exited with where it has one.
#[must_use]
pub fn end_line(kind: &str, exit: Option<i32>) -> String {
    match exit {
        Some(code) => format!("{VERB} end kind={kind} exit={code}"),
        None => format!("{VERB} end kind={kind}"),
    }
}

/// Where a run posts its start and end. [`Unhosted`] asks nobody (every test's, and a run with
/// no aterm); verify's own entry asks the instance it runs under ([`ToInstance`]).
pub trait Host: Send + Sync + std::fmt::Debug {
    /// Posts the start of a run of `kind`: which session it belongs to, as the instance placed
    /// this process; nothing known where none answered within its bound.
    fn start(&self, kind: &str) -> Hosted;
    /// Posts the end of a run of `kind` that exited `exit`, best effort within its bound.
    fn end(&self, kind: &str, exit: Option<i32>);
}

/// The host that asks nobody: no hosting session is known.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unhosted;

impl Host for Unhosted {
    fn start(&self, _: &str) -> Hosted {
        Hosted::default()
    }
    fn end(&self, _: &str, _: Option<i32>) {}
}

impl ToInstance {
    /// `line` to the instance, on a thread of its own the caller waits for at most
    /// [`POST_BOUND`]: its answer, where one came in time.
    fn ask(&self, line: String) -> Option<String> {
        let (sid, dir) = (self.sid.clone()?, self.dir.clone()?);
        within(POST_BOUND, move || {
            converse(&dir, &sid, &[line], Instant::now() + POST_BOUND)
                .ok()
                .and_then(|answers| answers.into_iter().next())
        })
        .flatten()
    }
}

impl Host for ToInstance {
    fn start(&self, kind: &str) -> Hosted {
        if !is_kind(kind) {
            return Hosted::default();
        }
        self.ask(start_line(kind))
            .map(|reply| Hosted::of_reply(&reply))
            .unwrap_or_default()
    }

    fn end(&self, kind: &str, exit: Option<i32>) {
        if is_kind(kind) {
            let _ = self.ask(end_line(kind, exit));
        }
    }
}

/// One verify pass's start and end, posted to `host`: the start when it is made, the end once,
/// by [`Self::end`] with the pass's exit code or, where the pass returned without one, as it is
/// dropped. A pass a signal ends posts no end: its process is gone, which its instance sees.
#[derive(Debug)]
pub struct Pass<'a> {
    host: &'a dyn Host,
    kind: &'static str,
    ended: std::cell::Cell<bool>,
}

impl<'a> Pass<'a> {
    /// Posts the start of a pass of `kind` to `host`.
    #[must_use]
    pub fn start(host: &'a dyn Host, kind: &'static str) -> Self {
        let _ = host.start(kind);
        Self {
            host,
            kind,
            ended: std::cell::Cell::new(false),
        }
    }

    /// Posts the pass's end, exited `exit`; a second call posts nothing.
    pub fn end(&self, exit: Option<i32>) {
        if !self.ended.replace(true) {
            self.host.end(self.kind, exit);
        }
    }
}

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        self.end(None);
    }
}

#[cfg(test)]
#[path = "hosting_tests.rs"]
pub(crate) mod tests;
