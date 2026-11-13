// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `reveal`: whether the process asking may be shown a secret in plaintext, decided by the
//! session of this instance its process lineage leads to (docs/DESIGN-alab-one-mesh-2026-11-05.md
//! §4.8, §11 A7). An agent marker in the environment cannot decide it: every shell aterm spawns
//! loses the agents' variables, and `ctl spawn` gives a command a real terminal. So `aterm db
//! emails --show-passwords`, `phones --show-numbers` and `--edit` ask here, and show plaintext only
//! on `allowed`.
//!
//! The verdict is the first that holds, in this order ([`reveal_verdict`]): `remote` (the bridge
//! or a fleet driver asked: not a process of this Mac), `indirect` (the kernel names no peer
//! process), `none` (no session here is the peer's ancestor: the client then falls back to the
//! agent marker, as outside aterm), `spawned`, `carried`, `held`, `driven`, `agent`, else
//! `allowed`.
//!
//! Beside the verdict the answer says `fleet=yes` where another Mac is behind the question: the
//! bridge or a fleet driver asked, or a fleet driver was admitted to type into the session the
//! asking process descends from (a local driver's typing is `driven` and `fleet=no`). A sign-in
//! reads it before it opens anything on this Mac's desk (`aterm harness accounts renew`,
//! `accounts token`): a session another Mac started carries that in its environment instead
//! (`crate::tab_spawn`), since a tab's first command can ask before any answer marks it.
//!
//! `driven` is any typing by a control client other than aterm's own since the session started,
//! counted from its admission, before its bytes are written: what a driver once typed can still
//! be running and asking, and what it is typing now can ask before the write returns.
//! `carried` is a session an aterm update carried over: the handoff keeps no record of who
//! started or drove it, so it never reveals; a tab opened since does.

use std::sync::PoisonError;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::session_store::Store;

/// What `reveal` answers, in the order it is decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Remote,
    Indirect,
    None,
    Spawned,
    Carried,
    Held,
    Driven,
    Agent,
    Allowed,
}

impl Verdict {
    /// The wire word.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Remote => "remote",
            Self::Indirect => "indirect",
            Self::None => "none",
            Self::Spawned => "spawned",
            Self::Carried => "carried",
            Self::Held => "held",
            Self::Driven => "driven",
            Self::Agent => "agent",
            Self::Allowed => "allowed",
        }
    }
}

/// Who started a session, as `reveal` reads it ([`crate::SessionCtx::spawn_mark`]): a person
/// (the default), `ctl spawn` ([`SpawnMark::ctl`]), or no one aterm knows of, because an update
/// carried it over ([`SpawnMark::at_spawn`]).
#[derive(Debug, Default)]
pub(crate) struct SpawnMark(AtomicU8);

impl SpawnMark {
    const PERSON: u8 = 0;
    const CTL: u8 = 1;
    const CARRIED: u8 = 2;

    /// The mark a session starts with: `adopted`, an aterm update carried it over, and the
    /// handoff keeps no record of its spawn or its drivers, so it never reveals; otherwise a
    /// person's until `ctl spawn`'s answer marks it.
    pub(crate) const fn at_spawn(adopted: bool) -> Self {
        Self(AtomicU8::new(if adopted {
            Self::CARRIED
        } else {
            Self::PERSON
        }))
    }

    /// Mark the session `ctl spawn`'s. A carried session keeps its mark.
    pub(crate) fn ctl(&self) {
        let _ =
            self.0
                .compare_exchange(Self::PERSON, Self::CTL, Ordering::AcqRel, Ordering::Acquire);
    }

    fn read(&self) -> u8 {
        self.0.load(Ordering::Acquire)
    }
}

/// What aterm knows of the session the asking process descends from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionFacts {
    /// `ctl spawn` started it, on this Mac or through a fleet driver ([`SpawnMark::ctl`]).
    pub(crate) spawned: bool,
    /// An aterm update carried it over ([`SpawnMark::at_spawn`]).
    pub(crate) carried: bool,
    /// It is under the `origin=remote` hold.
    pub(crate) held: bool,
    /// A control client other than aterm's own was admitted to type into it
    /// ([`crate::typed_by::TypedStamp::driver_admitted`]).
    pub(crate) driven: bool,
    /// An agent runs in its process tree.
    pub(crate) agent: bool,
    /// Another Mac's fleet drive was admitted to type into it
    /// ([`crate::typed_by::TypedStamp::fleet_admitted`]). It decides no verdict: `driven`
    /// already withholds a secret from it.
    pub(crate) fleet: bool,
}

/// What a `reveal` is decided from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RevealFacts {
    /// The bridge or a fleet driver asked.
    pub(crate) remote: bool,
    /// The kernel named the connection's peer process.
    pub(crate) peer: bool,
    /// The session of this instance the peer's lineage leads to.
    pub(crate) session: Option<SessionFacts>,
}

/// The verdict `facts` decide: the first word of the module docs' order that holds.
pub(crate) const fn reveal_verdict(facts: RevealFacts) -> Verdict {
    if facts.remote {
        return Verdict::Remote;
    }
    if !facts.peer {
        return Verdict::Indirect;
    }
    let Some(session) = facts.session else {
        return Verdict::None;
    };
    if session.spawned {
        Verdict::Spawned
    } else if session.carried {
        Verdict::Carried
    } else if session.held {
        Verdict::Held
    } else if session.driven {
        Verdict::Driven
    } else if session.agent {
        Verdict::Agent
    } else {
        Verdict::Allowed
    }
}

/// The process table, as [`Live`] reads it or a test fakes it.
pub(crate) trait Procs {
    /// `pid`'s parent.
    fn parent_of(&self, pid: i32) -> Option<i32>;
    /// `pid`'s process group.
    fn pgid_of(&self, pid: i32) -> Option<i32>;
    /// `pid`'s terminal session (its leader's pid, `getsid`).
    fn session_of(&self, pid: i32) -> Option<i32>;
    /// Every process on the host.
    fn pids(&self) -> Vec<i32>;
    /// Whether `pid` runs a program the agent table names (`aterm_phase::program_of`).
    fn agent(&self, pid: i32) -> bool;
}

/// The host's process table.
pub(crate) struct Live;

impl Procs for Live {
    fn parent_of(&self, pid: i32) -> Option<i32> {
        crate::session_status::program::parent_of(pid)
    }

    fn pgid_of(&self, pid: i32) -> Option<i32> {
        crate::consent_observer::live_pgid_of(pid)
    }

    fn session_of(&self, pid: i32) -> Option<i32> {
        #[cfg(unix)]
        {
            // SAFETY: `getsid` reads the process table; any pid is a valid argument.
            let leader = unsafe { libc::getsid(pid) };
            (leader > 0).then_some(leader)
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            None
        }
    }

    fn pids(&self) -> Vec<i32> {
        crate::spare::host_pids()
    }

    fn agent(&self, pid: i32) -> bool {
        crate::session_status::program::program_of(pid)
            .is_some_and(|program| aterm_phase::program_of(&program).is_some())
    }
}

/// How far up a parent walk goes before it gives up.
const LINEAGE_MAX: usize = 64;

/// Whether `pid` is in the process tree under `shell`: in its terminal session, or `shell` an
/// ancestor (a process that left the session by `setsid` included).
fn in_tree(pid: i32, shell: i32, procs: &dyn Procs) -> bool {
    if procs.session_of(pid) == Some(shell) {
        return true;
    }
    let mut at = pid;
    for _ in 0..LINEAGE_MAX {
        if at == shell {
            return true;
        }
        match procs.parent_of(at) {
            Some(parent) if parent > 1 => at = parent,
            _ => return false,
        }
    }
    false
}

/// Whether any process in the tree under `shell` runs an agent.
fn agent_in_tree(shell: i32, procs: &dyn Procs) -> bool {
    shell > 1
        && procs
            .pids()
            .into_iter()
            .filter(|&pid| pid > 1)
            .any(|pid| in_tree(pid, shell, procs) && procs.agent(pid))
}

/// The session of `store` whose shell is `peer` or an ancestor of it (`sid`), with what aterm knows
/// of it: `None` where no live session's tree holds `peer`.
fn session_facts(store: &Store, peer: i32, procs: &dyn Procs) -> Option<(String, SessionFacts)> {
    // The process table is read with the store's lock released.
    let shells: Vec<(u64, i32)> = store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .live_handles()
        .filter(|handle| handle.shell_identity.pid > 1)
        .map(|handle| (handle.local_id, handle.shell_identity.pid))
        .collect();
    let local = crate::consent_observer::session_of_pid(
        peer,
        &shells,
        |pid| procs.parent_of(pid),
        |pid| procs.pgid_of(pid),
    )?;
    let (sid, shell, ctx) = {
        let store = store.read().unwrap_or_else(PoisonError::into_inner);
        let handle = store.by_local(local)?;
        (
            handle.sid.as_str().to_owned(),
            handle.shell_identity.pid,
            std::sync::Arc::clone(&handle.ctx),
        )
    };
    let fronts_agent = ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .agent()
        .reader
        .is_some();
    let facts = SessionFacts {
        agent: fronts_agent || agent_in_tree(shell, procs),
        ..marks_of(&ctx)
    };
    Some((sid, facts))
}

/// What aterm records of session `ctx` but whether an agent runs in it (`agent` is `false`): who
/// started it, whether an update carried it, whether the remote hold is on it, whether a
/// control client other than aterm's own was ever admitted to type into it, and whether another
/// Mac's fleet drive was.
pub(crate) fn marks_of(ctx: &crate::SessionCtx) -> SessionFacts {
    let mark = ctx.spawn_mark.read();
    SessionFacts {
        spawned: mark == SpawnMark::CTL,
        carried: mark == SpawnMark::CARRIED,
        held: ctx.fabric.holds().remote().is_some(),
        driven: ctx.typed.driver_admitted(),
        agent: false,
        fleet: ctx.typed.fleet_admitted(),
    }
}

/// `reveal`'s reply for a peer `peer` (the kernel's, when it named one), asked `remote`ly or not.
fn answer(store: &Store, remote: bool, peer: Option<i32>, procs: &dyn Procs) -> String {
    let found = peer
        .filter(|_| !remote)
        .and_then(|pid| session_facts(store, pid, procs));
    let verdict = reveal_verdict(RevealFacts {
        remote,
        peer: peer.is_some(),
        session: found.as_ref().map(|(_, facts)| *facts),
    });
    let sid = found.as_ref().map_or("-", |(sid, _)| sid.as_str());
    let fleet = remote || found.as_ref().is_some_and(|(_, facts)| facts.fleet);
    format!(
        "OK 1\nverdict={} session={sid} fleet={}\n",
        verdict.word(),
        if fleet { "yes" } else { "no" }
    )
}

/// `reveal` on the control thread: the connection's kernel peer, read now, asked through the
/// bridge (`bridge`) or not.
pub(crate) fn cmd_reveal(store: &Store, bridge: bool, rest: &str) -> String {
    served(store, bridge, rest, &Live)
}

/// [`cmd_reveal`] over the process table `procs`.
fn served(store: &Store, bridge: bool, rest: &str, procs: &dyn Procs) -> String {
    if !rest.trim().is_empty() {
        return "ERR usage: reveal takes no words\n".to_string();
    }
    let remote = bridge || crate::fabric::serving_driver().is_some();
    let peer = super::super::serving_peer_pid()
        .and_then(|pid| i32::try_from(pid).ok())
        .filter(|pid| *pid > 1);
    answer(store, remote, peer, procs)
}

/// Record that `ctl spawn` started session `sid`, before its `OK <sid>` goes out: from then on its
/// `reveal` says `spawned`.
pub(crate) fn mark_ctl_spawned(store: &Store, sid: &str) {
    let store = store.read().unwrap_or_else(PoisonError::into_inner);
    if let Some(handle) = store.by_sid(&aterm_session::SessionId::new(sid)) {
        handle.ctx.spawn_mark.ctl();
    }
}

#[cfg(test)]
#[path = "control_reveal_tests.rs"]
mod tests;
