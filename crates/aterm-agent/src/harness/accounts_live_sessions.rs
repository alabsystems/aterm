// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The session table holds only sessions that are alive ([`reconcile_sessions`]).
//!
//! A row of [`State::sessions`] says which account a supervised session runs on. Only a window's
//! harness host notes rows, each for a tab of its own process, so a row names the process that
//! noted it ([`SessionRow::owner`]). Two aterm processes of one login share one store (the state
//! root is under `$HOME`), so a host answers only for its own: the rows it noted, and every tab it
//! has, whatever row names it. A session of a tab it has stays where it runs an agent of the
//! pool's CLI there. A row whose owner is gone (ended, or its pid taken by another process), or
//! that a build before owners noted, is asked of this machine's discovery graph, where every
//! instance publishes each session it holds (`<control dir>/graph/<sid>`). A row another running
//! process owns is never touched.
//!
//! One read never ends a session: a crashed window's sessions run on in the keeper until a
//! relaunch adopts them, and a roster can miss an agent for a pass. A session goes only once its
//! host has read it as over at reads [`SESSION_OVER_GRACE_S`] apart, with no read between that
//! found it live ([`OverReads`]). Age alone ends only a row no running process answers for
//! ([`drop_unanswered_sessions`]): a live session that sits idle reaches no account point, so the
//! row of one whose window runs can be any age.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use super::{SessionRow, State, Store, forget_session};

/// How long a session must go on reading as over before its row goes ([`OverReads`]): well past
/// a relaunch's recovery, in which the keeper carries a crashed window's sessions until the next
/// window adopts and publishes them, and past a roster that misses an agent for a pass.
pub const SESSION_OVER_GRACE_S: i64 = 10 * 60;

/// A row no running process answers for (it names no owner, or one that ended) goes once it is
/// older than this, when a transaction writes the state ([`drop_unanswered_sessions`]).
pub const SESSION_KEEP_S: i64 = 30 * 24 * 3600;

/// The process that noted a session's row ([`SessionRow::owner`]): its pid and its kernel start
/// ([`aterm_uds::process::read_birth`]), which tells it from a later process given the same pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Owner {
    /// The pid.
    pub pid: u32,
    /// Its kernel start ([`aterm_uds::process::Birth`]).
    pub birth: u64,
}

impl Owner {
    /// This process, read once; `None` where the kernel would not say when it started.
    #[must_use]
    pub fn here() -> Option<Owner> {
        static HERE: OnceLock<Option<Owner>> = OnceLock::new();
        *HERE.get_or_init(|| Owner::of(std::process::id()))
    }

    /// Process `pid` as it runs now; `None` where it does not run or its start cannot be read.
    #[must_use]
    pub fn of(pid: u32) -> Option<Owner> {
        let birth = aterm_uds::process::read_birth(i32::try_from(pid).ok()?)?;
        Some(Owner {
            pid,
            birth: birth.0,
        })
    }

    /// Whether this process still runs: `Some(false)` once it ended or another process took its
    /// pid, `None` where its start could not be read.
    #[must_use]
    pub fn runs(self) -> Option<bool> {
        if !aterm_uds::process::pid_alive(self.pid) {
            return Some(false);
        }
        Owner::of(self.pid).map(|now| now == self)
    }

    /// The state file's word for it: `<pid>@<birth>`.
    #[must_use]
    pub fn token(self) -> String {
        format!("{}@{}", self.pid, self.birth)
    }

    /// Reads [`Self::token`]'s word, exactly: digits only, no leading zero, a pid above 0.
    #[must_use]
    pub fn parse(token: &str) -> Option<Owner> {
        fn number<T: std::str::FromStr>(text: &str) -> Option<T> {
            (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
                .then(|| text.parse().ok())
                .flatten()
        }
        let (pid, birth) = token.split_once('@')?;
        let owner = Owner {
            pid: number(pid).filter(|pid| *pid > 0)?,
            birth: number(birth)?,
        };
        (owner.token() == token).then_some(owner)
    }
}

/// What this machine's discovery graph says of a session the table names, as
/// [`LiveSessions::served`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// Its entry names the reconciling process.
    Here,
    /// Its entry names another running aterm instance.
    Elsewhere,
    /// No running instance serves it.
    Gone,
    /// It could not be read: the session stays.
    Unknown,
}

/// What a host knows of the sessions when it reconciles one pool's session table
/// ([`reconcile_sessions`]).
pub struct LiveSessions<'a> {
    /// The reconciling process ([`Owner::here`]): the owner of every row it noted.
    pub me: Owner,
    /// When the host began to read its tabs, in Unix seconds: a row this process noted in that
    /// second or later may be of a session the read did not see yet, so it stays.
    pub read_at: i64,
    /// Whether this process holds session `sid` for the pool: an agent of the pool's CLI runs in
    /// its tab, is being brought back there, or was not looked for (a host that did not read what
    /// runs in its tabs holds each of them in every pool).
    pub holds: &'a dyn Fn(&str) -> bool,
    /// Whether session `sid` is a tab of this process, whatever runs in it, as read at
    /// [`Self::read_at`] or later: this process answers for it, whoever noted its row.
    pub tab: &'a dyn Fn(&str) -> bool,
    /// Whether process `owner` still runs ([`Owner::runs`]).
    pub runs: &'a dyn Fn(Owner) -> Option<bool>,
    /// What this machine's discovery graph says of session `sid` ([`Liveness`]).
    pub served: &'a dyn Fn(&str) -> Liveness,
}

/// What [`reconcile_sessions`] did, each session once, in order of its id.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SessionsReconciled {
    /// The sessions forgotten ([`forget_session`]): their rows, canary holds and wall waits.
    pub forgot: Vec<String>,
    /// The rows of sessions this process holds that it now owns: rows noted by a process that
    /// handed the session over (a seamless update, a relaunch after a crash), or by a build before
    /// owners.
    pub adopted: Vec<String>,
    /// Every session read as over, [`Self::forgot`]'s and those whose grace still runs.
    pub over: Vec<String>,
}

impl SessionsReconciled {
    /// Whether it changed nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.forgot.is_empty() && self.adopted.is_empty()
    }
}

/// Reconciles `state`'s sessions with what `live` knows of them: every session id its rows, its
/// wall waits ([`State::wall_reads`]) and its canary holds (the canary and each undo's) name.
///
/// - A session this process holds for the pool stays, and its row becomes this process's.
/// - A row this process noted at [`LiveSessions::read_at`] or later stays: the read may not have
///   seen its session yet.
/// - A session of a tab of this process, or of a row it noted earlier, that it does not hold, is
///   over unless the graph names another live instance serving it (a successor it handed the
///   session to) or cannot be read.
/// - A row of another process that runs, or whose start cannot be read, stays: that process
///   answers for it.
/// - A row of a process that is gone, a row with no owner, and a wait or hold with no row are
///   over only where the graph says no live instance serves the session.
///
/// A session over is forgotten, as [`forget_session`] forgets an ended session, only where `due`
/// says so ([`OverReads::due`]); [`SessionsReconciled::over`] names each one either way.
pub fn reconcile_sessions(
    state: &mut State,
    live: &LiveSessions<'_>,
    due: &dyn Fn(&str) -> bool,
) -> SessionsReconciled {
    let mut sids: BTreeSet<String> = state.sessions.keys().cloned().collect();
    sids.extend(
        state
            .wall_reads
            .values()
            .flat_map(|ladder| ladder.waiters.keys().cloned()),
    );
    sids.extend(
        state
            .canary
            .iter()
            .chain(state.undo.iter().filter_map(|over| over.canary.as_ref()))
            .map(|canary| canary.sid.clone()),
    );
    let mut out = SessionsReconciled::default();
    for sid in sids {
        if (live.holds)(&sid) {
            if let Some(row) = state.sessions.get_mut(&sid)
                && row.owner != Some(live.me)
            {
                row.owner = Some(live.me);
                out.adopted.push(sid);
            }
            continue;
        }
        if !over(&sid, state.sessions.get(&sid), live) {
            continue;
        }
        if due(&sid) && forget_session(state, &sid) {
            out.forgot.push(sid.clone());
        }
        out.over.push(sid);
    }
    out
}

/// Whether session `sid`, which this process does not hold, with `row`, is over.
fn over(sid: &str, row: Option<&SessionRow>, live: &LiveSessions<'_>) -> bool {
    let owner = row.and_then(|row| row.owner);
    let own = owner == Some(live.me);
    if own && row.is_some_and(|row| row.at >= live.read_at) {
        return false;
    }
    if own || (live.tab)(sid) {
        return matches!((live.served)(sid), Liveness::Here | Liveness::Gone);
    }
    if owner.is_some_and(|owner| (live.runs)(owner) != Some(false)) {
        return false;
    }
    (live.served)(sid) == Liveness::Gone
}

/// When each session a host has read as over first read so, for one pool's table, kept from one
/// reconcile to the next ([`record_reconciled`]): a session goes only once it has read as over at
/// reads [`SESSION_OVER_GRACE_S`] apart, with no read between that found it live.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OverReads {
    since: BTreeMap<String, i64>,
}

impl OverReads {
    /// Notes the sessions `over` that a read at `now` found over: each keeps its first mark (one
    /// ahead of `now`, from a clock that stepped back, is marked again at `now`), a new one is
    /// marked at `now`, and a session not over any more loses its mark.
    pub fn note(&mut self, over: &[String], now: i64) {
        let over: BTreeSet<&str> = over.iter().map(String::as_str).collect();
        self.since.retain(|sid, _| over.contains(sid.as_str()));
        for sid in over {
            let since = self.since.entry(sid.to_string()).or_insert(now);
            *since = (*since).min(now);
        }
    }

    /// Whether session `sid` has read as over since [`SESSION_OVER_GRACE_S`] before `now` or
    /// earlier.
    #[must_use]
    pub fn due(&self, sid: &str, now: i64) -> bool {
        self.since
            .get(sid)
            .is_some_and(|since| now.saturating_sub(*since) >= SESSION_OVER_GRACE_S)
    }

    /// Whether no session is marked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.since.is_empty()
    }

    /// Forgets every mark: the next read that finds a session over marks it anew.
    pub fn clear(&mut self) {
        self.since.clear();
    }
}

/// Reconciles `store`'s session table ([`reconcile_sessions`]) under a bounded lock wait, noting
/// each session over in `marks` and forgetting only those whose grace is out
/// ([`OverReads::due`]), and touches `wake` when that released a canary hold. A read of the state
/// first decides whether anything would change; nothing is locked or written where nothing would.
/// # Errors
///
/// As [`Store::load`] and [`Store::transact_within`]: the state is not this build's to read or
/// write, a file could not be locked or written, or the lock stayed held (exactly
/// [`super::LOCK_HELD`]); or `wake` could not be touched. `marks` keeps what it had where the
/// state could not be read.
pub fn record_reconciled(
    store: &Store,
    live: &LiveSessions<'_>,
    marks: &mut OverReads,
    now: i64,
) -> Result<SessionsReconciled, String> {
    if !store.state_file().exists() {
        marks.clear();
        return Ok(SessionsReconciled::default());
    }
    let (mut seen, _) = store.load(now)?;
    let read = reconcile_sessions(&mut seen, live, &|_| false);
    marks.note(&read.over, now);
    let marked = &*marks;
    let due = |sid: &str| marked.due(sid, now);
    if read.adopted.is_empty() && !read.over.iter().any(|sid| due(sid)) {
        return Ok(SessionsReconciled {
            over: read.over,
            ..SessionsReconciled::default()
        });
    }
    let mut done = SessionsReconciled::default();
    let mut released = false;
    let mut note = None;
    store.transact_within(store.ladder, now, &mut note, |state| {
        let holding = state.canary_holds(now);
        done = reconcile_sessions(state, live, &due);
        released = holding && !state.canary_holds(now);
        Ok(Vec::new())
    })?;
    marks.since.retain(|sid, _| !done.forgot.contains(sid));
    if released {
        store
            .touch_wake()
            .map_err(|e| format!("{}: {e}", store.wake_path().display()))?;
    }
    Ok(done)
}

/// Forgets ([`forget_session`]) each row older than [`SESSION_KEEP_S`] at `now` that no running
/// process answers for: it names no owner, or its owner ended or lost its pid
/// ([`Owner::runs`]). A row whose owner runs, or whose owner's start cannot be read, is kept
/// whatever its age: that window reconciles it. Returns how many rows went.
pub fn drop_unanswered_sessions(state: &mut State, now: i64) -> usize {
    let aged: Vec<String> = state
        .sessions
        .iter()
        .filter(|(_, row)| now.saturating_sub(row.at) > SESSION_KEEP_S)
        .filter(|(_, row)| row.owner.is_none_or(|owner| owner.runs() == Some(false)))
        .map(|(sid, _)| sid.clone())
        .collect();
    let mut gone = 0;
    for sid in aged {
        if forget_session(state, &sid) {
            gone += 1;
        }
    }
    gone
}

#[cfg(test)]
#[path = "accounts_live_sessions_tests.rs"]
mod tests;
