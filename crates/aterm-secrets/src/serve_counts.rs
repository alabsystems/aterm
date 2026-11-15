// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a vault server (`aterm secrets serve`) counts of its own rounds, its pulls from its
//! peers and the threads it starts, and the line it says them in.
//!
//! Read them on a running Mac in the server's log, which is where launchd sends the server's
//! standard error: `grep 'round counts' /Library/Logs/aterm/systems.alab.aterm-vault-server.log`
//! for the system job ([`crate::service::DAEMON_LOG`]), or
//! `grep 'round counts' ~/.config/aterm/secrets/systems.alab.aterm-secrets-server.log` for the
//! job `aterm secrets server on` installs ([`crate::service::log_path`]). The `vault-tend` thread
//! says one line as its 1st, 2nd, 4th ... 256th round ends and then as each 256th does
//! ([`aterm_uds::tally::due`]): every total since the server began, so two lines give a rate. The
//! line holds names and numbers only, and leaves out a total that is 0.
//!
//! Every count only rises and is one relaxed add where it is counted; none takes a lock.
//!
//! Only the `vault-tend` thread says the line. A server that runs none (one off macOS, and one
//! on macOS that neither keeps this Mac awake nor advertises) counts its `vault-conn` threads
//! and says nothing: no round ends there, and a request's thread writes no log line for it.
//!
//! What the CPU figures cover: `tend_*_cpu_ns` is the `vault-tend` thread's, by phase;
//! `resolve_threads_cpu_ns` the threads a round starts to resolve one LAN name each (its peers'
//! and the `alab-secrets` holders'); `pull_threads_cpu_ns` the `vault-peer-pull` threads', one
//! per peer per round; `conn_threads_cpu_ns` the `vault-conn` threads', one per request served.
//! `process_cpu_ns` is the whole process's, so what it holds beyond those four is the threads
//! no count covers, the accept loop (`vault-accept`), the standing browse and a registration's
//! wait (`vault-register`) among them.
//!
//! Which counter a later change is judged by:
//! - a quiet round that pulls from fewer peers: `rounds` by what ended the nap before each
//!   (`nap_timer` against the others: only a timer-ended round is quiet), `pulls` and
//!   `pulls_stored` per round, and what serving costs at rest, which is `process_cpu_ns` between
//!   two lines; the four kinds of thread above say where it went, not the `vault-tend`
//!   thread's time alone;
//! - a server that resolves the `alab-secrets` holders only where the round reads them:
//!   `alab_names` per round;
//! - a pull that skips a peer whose advert names the held version: `envelope_204` against
//!   `envelope_bodies` per round, and `peer_names`.

#[cfg(any(test, target_os = "macos"))]
use std::cell::RefCell;
use std::fmt::Write as _;
#[cfg(any(test, target_os = "macos"))]
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use aterm_uds::cpu::ThreadsCpu;

use crate::wake::Wake;

/// One thing a server counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Count {
    /// Rounds of the `vault-tend` thread begun.
    Rounds,
    /// Rounds by what came before them: nothing (the first), or the nap before, by what ended
    /// it ([`Wake`]).
    RoundFirst,
    NapTimer,
    NapRoute,
    NapBrowse,
    NapBroker,
    NapListener,
    NapPut,
    NapResumed,
    NapStop,
    /// Rounds that found sharing off (`[swarm] enabled`): they resolve and pull nothing.
    RoundsHeld,
    /// LAN names handed to a resolver for a round's peers, and for the `alab-secrets` holders.
    PeerNames,
    AlabNames,
    /// Pulls from a peer begun (one per peer a round resolved), and those that stored anything.
    Pulls,
    PullsStored,
    /// Envelope requests made a second time, for the whole of an answer longer than the first
    /// read's bound (`ENVELOPE_PEEK`): each is one more request to the peer, and that bound's
    /// bytes read and dropped.
    EnvelopeWholeReads,
    /// What a peer answered a pull's envelope request (`?after=<held seq>`): a 204, a body, a
    /// 404, or nothing that was read whole in time (no answer, a failure, a body longer than any
    /// envelope); and the bodies that advanced the vault held.
    Envelope204,
    EnvelopeBodies,
    EnvelopeMissing,
    EnvelopeUnanswered,
    EnvelopesStored,
    /// Records requests to a peer that were answered, those that were not, and the records the
    /// answers added to the store.
    RecordsFetches,
    RecordsUnanswered,
    RecordsStored,
    /// Probes of `alab-secrets.local` or a holder heard for a verified ALab envelope.
    AlabProbes,
}

impl Count {
    pub(crate) const ALL: [Self; 25] = [
        Self::Rounds,
        Self::RoundFirst,
        Self::NapTimer,
        Self::NapRoute,
        Self::NapBrowse,
        Self::NapBroker,
        Self::NapListener,
        Self::NapPut,
        Self::NapResumed,
        Self::NapStop,
        Self::RoundsHeld,
        Self::PeerNames,
        Self::AlabNames,
        Self::Pulls,
        Self::PullsStored,
        Self::EnvelopeWholeReads,
        Self::Envelope204,
        Self::EnvelopeBodies,
        Self::EnvelopeMissing,
        Self::EnvelopeUnanswered,
        Self::EnvelopesStored,
        Self::RecordsFetches,
        Self::RecordsUnanswered,
        Self::RecordsStored,
        Self::AlabProbes,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Rounds => "rounds",
            Self::RoundFirst => "round_first",
            Self::NapTimer => "nap_timer",
            Self::NapRoute => "nap_route",
            Self::NapBrowse => "nap_browse",
            Self::NapBroker => "nap_broker",
            Self::NapListener => "nap_listener",
            Self::NapPut => "nap_put",
            Self::NapResumed => "nap_resumed",
            Self::NapStop => "nap_stop",
            Self::RoundsHeld => "rounds_held",
            Self::PeerNames => "peer_names",
            Self::AlabNames => "alab_names",
            Self::Pulls => "pulls",
            Self::PullsStored => "pulls_stored",
            Self::EnvelopeWholeReads => "envelope_whole_reads",
            Self::Envelope204 => "envelope_204",
            Self::EnvelopeBodies => "envelope_bodies",
            Self::EnvelopeMissing => "envelope_missing",
            Self::EnvelopeUnanswered => "envelope_unanswered",
            Self::EnvelopesStored => "envelopes_stored",
            Self::RecordsFetches => "records_fetches",
            Self::RecordsUnanswered => "records_unanswered",
            Self::RecordsStored => "records_stored",
            Self::AlabProbes => "alab_probes",
        }
    }

    /// The count of a round that began after a nap `why` ended, or after none.
    pub(crate) const fn after(why: Option<Wake>) -> Self {
        match why {
            None => Self::RoundFirst,
            Some(Wake::Timer) => Self::NapTimer,
            Some(Wake::Route) => Self::NapRoute,
            Some(Wake::Browse) => Self::NapBrowse,
            Some(Wake::Broker) => Self::NapBroker,
            Some(Wake::Listener) => Self::NapListener,
            Some(Wake::Put) => Self::NapPut,
            Some(Wake::Resumed) => Self::NapResumed,
            Some(Wake::Stop) => Self::NapStop,
        }
    }
}

/// Where the `vault-tend` thread is, for its CPU time: every moment of its loop is in exactly
/// one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Reading whether sharing is on, starting the standing browse where none runs, reading
    /// this Mac's addresses and resolving the round's peers and holders (the thread's own
    /// share: each name is resolved on a thread of its own, counted apart).
    Resolve,
    /// Deciding the role and pulling from the peers (the thread's own share: each pull runs on a
    /// `vault-peer-pull` thread, counted apart).
    Pull,
    /// Keeping awake and registering the adverts.
    Register,
    /// The rest: the nap between rounds, saying the line, and starting.
    Rest,
}

impl Phase {
    pub(crate) const ALL: [Self; 4] = [Self::Resolve, Self::Pull, Self::Register, Self::Rest];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Resolve => "resolve",
            Self::Pull => "pull",
            Self::Register => "register",
            Self::Rest => "rest",
        }
    }
}

/// A server's counts, for as long as it runs.
pub(crate) struct ServeCounts {
    counts: [AtomicU64; Count::ALL.len()],
    /// The `vault-tend` thread's CPU time in each phase.
    tend_cpu_ns: [AtomicU64; Phase::ALL.len()],
    /// The threads a round started to resolve one LAN name each that ended, and their CPU time
    /// (`resolving`; a build that starts none leaves both 0).
    pub(crate) resolve_threads: ThreadsCpu,
    /// The `vault-peer-pull` threads that ended, and their CPU time.
    pub(crate) pull_threads: ThreadsCpu,
    /// The `vault-conn` threads that ended, and their CPU time.
    pub(crate) conn_threads: ThreadsCpu,
}

impl Default for ServeCounts {
    fn default() -> Self {
        Self {
            counts: std::array::from_fn(|_| AtomicU64::new(0)),
            tend_cpu_ns: std::array::from_fn(|_| AtomicU64::new(0)),
            resolve_threads: ThreadsCpu::new(),
            pull_threads: ThreadsCpu::new(),
            conn_threads: ThreadsCpu::new(),
        }
    }
}

impl ServeCounts {
    /// Count one `what`: its count after this one.
    pub(crate) fn add(&self, what: Count) -> u64 {
        self.add_n(what, 1)
    }

    /// Count `n` of `what`: its count after them.
    pub(crate) fn add_n(&self, what: Count, n: usize) -> u64 {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        self.counts[what as usize]
            .fetch_add(n, Ordering::Relaxed)
            .saturating_add(n)
    }

    /// `what` so far.
    pub(crate) fn get(&self, what: Count) -> u64 {
        self.counts[what as usize].load(Ordering::Relaxed)
    }

    /// The total the `vault-tend` thread's CPU time in `phase` is added to.
    pub(crate) fn tend_cpu(&self, phase: Phase) -> &AtomicU64 {
        &self.tend_cpu_ns[phase as usize]
    }

    /// Every total by its name, in the order the line says them.
    fn totals(&self) -> Vec<(String, u64)> {
        let mut totals: Vec<(String, u64)> = Count::ALL
            .iter()
            .map(|what| (what.name().to_owned(), self.get(*what)))
            .collect();
        for phase in Phase::ALL {
            totals.push((
                format!("tend_{}_cpu_ns", phase.name()),
                self.tend_cpu(phase).load(Ordering::Relaxed),
            ));
        }
        for (kind, threads) in [
            ("resolve", &self.resolve_threads),
            ("pull", &self.pull_threads),
            ("conn", &self.conn_threads),
        ] {
            totals.push((
                format!("{kind}_threads"),
                threads.ended.load(Ordering::Relaxed),
            ));
            totals.push((
                format!("{kind}_threads_cpu_ns"),
                threads.cpu_ns.load(Ordering::Relaxed),
            ));
        }
        totals
    }

    /// The line of every total that is not 0, and the process's CPU time so far
    /// ([`aterm_uds::cpu::process_ns`]).
    pub(crate) fn line(&self) -> String {
        self.line_at(aterm_uds::cpu::process_ns())
    }

    /// [`Self::line`] with `process_cpu_ns` as the process's CPU time.
    fn line_at(&self, process_cpu_ns: u64) -> String {
        let mut line = "round counts:".to_owned();
        for (name, value) in self.totals() {
            if value != 0 {
                let _ = write!(line, " {name}={value}");
            }
        }
        let _ = write!(line, " process_cpu_ns={process_cpu_ns}");
        line
    }

    /// The line to say as a round ends, where the rounds so far are due
    /// ([`aterm_uds::tally::due`]).
    pub(crate) fn round_ended(&self) -> Option<String> {
        aterm_uds::tally::due(self.get(Count::Rounds)).then(|| self.line())
    }
}

// A round starts a thread for each name it resolves only where this build resolves names
// through the LAN broker (macOS) and in tests, so what follows is compiled only there.
#[cfg(any(test, target_os = "macos"))]
thread_local! {
    /// The server whose round this thread runs, while it runs one ([`tending`]).
    static TENDING: RefCell<Option<Arc<ServeCounts>>> = const { RefCell::new(None) };
}

/// What [`tending`] put on a thread, taken off when it drops.
#[cfg(any(test, target_os = "macos"))]
pub(crate) struct Tended(bool);

#[cfg(any(test, target_os = "macos"))]
impl Drop for Tended {
    fn drop(&mut self) {
        if self.0 {
            let _ = TENDING.try_with(|tending| tending.borrow_mut().take());
        }
    }
}

/// This thread runs a round of the server `counts` belongs to until the guard drops: the
/// threads it starts under [`resolving`] are counted there. On a thread that already runs one,
/// it changes nothing.
#[cfg(any(test, target_os = "macos"))]
pub(crate) fn tending(counts: &Arc<ServeCounts>) -> Tended {
    TENDING
        .try_with(|tending| {
            let mut tending = tending.borrow_mut();
            if tending.is_some() {
                return Tended(false);
            }
            *tending = Some(Arc::clone(counts));
            Tended(true)
        })
        .unwrap_or(Tended(false))
}

/// The server whose round the thread about to start threads runs (none where it runs none):
/// each thread it starts to resolve one name runs its work under [`Resolving::run`].
#[cfg(any(test, target_os = "macos"))]
pub(crate) struct Resolving(Option<Arc<ServeCounts>>);

/// What the threads this thread is about to start to resolve names are counted for.
#[cfg(any(test, target_os = "macos"))]
pub(crate) fn resolving() -> Resolving {
    Resolving(
        TENDING
            .try_with(|tending| tending.borrow().clone())
            .ok()
            .flatten(),
    )
}

#[cfg(any(test, target_os = "macos"))]
impl Resolving {
    /// Run `work` on a thread started for it alone, as the last thing that thread does: as it
    /// ends it is counted among the server's resolve threads, with all its CPU time
    /// ([`ThreadsCpu::ending`]). Where the starting thread ran no server's round, `work` alone.
    pub(crate) fn run<T>(&self, work: impl FnOnce() -> T) -> T {
        match &self.0 {
            Some(counts) => counts.resolve_threads.ending(work),
            None => work(),
        }
    }
}

#[cfg(test)]
#[path = "serve_counts_tests.rs"]
mod tests;
