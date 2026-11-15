// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a subscriber counts of its own passes (the loop of `aterm secrets watch`), and the lines
//! it says them in.
//!
//! Read them on a running Mac in the subscriber's log, which is where launchd sends the
//! standard error of the job `aterm secrets agent on` installs:
//! `grep 'pass counts' ~/.config/aterm/secrets/systems.alab.aterm-secrets.log`
//! ([`crate::service::log_path`]). The loop says two lines as its 1st, 2nd, 4th ... 256th pass
//! ends and then as each 256th does ([`aterm_uds::tally::due`]): every total since the loop
//! began, and the same totals over the last [`WINDOW`] passes, so one read gives an average over
//! that many passes as well as over the loop's life. The lines hold names and numbers only, and
//! leave out a total that is 0.
//!
//! Every count only rises and is one relaxed add where it is counted, found through a
//! thread-local the pass sets on the threads that work for it ([`metered`], [`starting`]): a
//! thread that works for no pass counts nothing, so no caller's signature carries the counts and
//! a reader of a vault that is no subscriber adds to none. Time is read where the loop's thread
//! changes [`Phase`], a few tens of times a pass, and once by each thread a pass starts, as it
//! ends.
//!
//! Which counter a later change is judged by:
//! - a body equal to the held vault taking the held entry's memo
//!   (`client::chain_cache::verify_as_held`): `fresh_probe` and `fresh_select_peer` per pass
//!   (each a parse and a signature ask of a peer's bytes: 0 while every peer serves the held
//!   bytes) against `memo_probe`, `memo_select_peer`, `memo_select_held` and `memo_admin` (each a
//!   verification the held entry answered, which parses nothing); `fresh_floor`, which still
//!   parses the held bytes where it runs; and `parses`, `signature_asks` and
//!   `signatures_computed` (a signature already verified is a kept fact, [`crate::ed25519`], so
//!   `signatures_computed` says what the asks cost);
//! - a probe that asks before it reads a body: `probe_bodies` and `probe_bytes` per pass over
//!   the last [`WINDOW`] passes, and `probe_unanswered`;
//! - records from the source first: `records_peers` per pass, and what the records fan-out
//!   costs, which is `records_threads_cpu_ns` (its `vault-read` threads and the one that asks
//!   the source) plus `records_cpu_ns` (the loop thread's share), not the loop thread's alone;
//! - what a pass spends where: `<phase>_cpu_ns` and `<phase>_wall_ns` of the loop's thread, and
//!   `<phase>_threads` and `<phase>_threads_cpu_ns` of the threads it started in that phase
//!   (`probe_threads_cpu_ns` is the envelope probe's `vault-read` threads, `peers_threads_cpu_ns`
//!   the threads that each resolve one LAN name). `<phase>_entries` is how many times the loop's
//!   thread passed a mark that puts it in the phase ([`enter`], [`within`]): the laws count
//!   those, so a mark that is dropped shows as a count, where the time it moved would not.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use aterm_uds::cpu::ThreadsCpu;

/// The passes the second line covers.
pub(crate) const WINDOW: usize = 8;

/// One thing a pass counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Count {
    /// Subscriber passes begun.
    Passes,
    /// The source answered with a body.
    PrimaryBody,
    /// The source holds no such vault (a 404).
    PrimaryMissing,
    /// The source answered 204 and named its seq.
    PrimarySeq204,
    /// The source answered 204 and named no seq.
    PrimaryBare204,
    /// The source held the request to its deadline and answered nothing.
    PrimaryHeldOut,
    /// The source failed.
    PrimaryError,
    /// Records fan-outs to peers, and the peers they asked.
    RecordsFanouts,
    RecordsPeers,
    /// Envelope probes of peers, and the peers they asked.
    Probes,
    ProbePeers,
    /// Probed peers asked a second time, for the whole of an answer longer than the first
    /// read's bound (`fan_out::ENVELOPES`' peek): each is one more request, and that bound's
    /// bytes read and dropped, which `probe_bytes` does not hold.
    ProbeWholeReads,
    /// What the probed peers answered: a body (and its bytes), a 204, a 404, or nothing that was
    /// read whole in time (no answer, a failure, a body longer than any envelope).
    ProbeBodies,
    ProbeBytes,
    ProbeNoNewer,
    ProbeMissing,
    ProbeUnanswered,
    /// LAN names handed to a resolver for a pass's peers.
    NamesResolved,
    /// On the pass's threads, whatever asked: every envelope parsed ([`crate::envelope::parse`]);
    /// every verification of bytes, which parses them
    /// ([`crate::envelope::verify_for_reader_with`]); every signature asked for
    /// ([`crate::envelope::Envelope::verify_signature`]), kept fact or not; every signature
    /// computed, which is an ask no kept fact answered; and every verification asked of a held
    /// vault's entry, which parses nothing and asks for its signature once.
    Parses,
    Verifies,
    SignatureAsks,
    SignaturesComputed,
    EntryVerifies,
    /// Verifications of bytes (`verifies`) by the site that asked: the source's body, a probed
    /// peer's body, each found peer's body in selection, and the held vault for a floor.
    /// `verifies` less their sum is what other sites asked.
    FreshPrimary,
    FreshProbe,
    FreshSelectPeer,
    FreshFloor,
    /// Verifications the held vault's entry answered, of the bytes it was parsed from, by the
    /// site that asked: a probed peer's body that is those bytes, the held vault and each found
    /// peer's body that is those bytes in selection, and the held vault for the admin text.
    MemoProbe,
    MemoSelectHeld,
    MemoSelectPeer,
    MemoAdmin,
}

impl Count {
    pub(crate) const ALL: [Self; 31] = [
        Self::Passes,
        Self::PrimaryBody,
        Self::PrimaryMissing,
        Self::PrimarySeq204,
        Self::PrimaryBare204,
        Self::PrimaryHeldOut,
        Self::PrimaryError,
        Self::RecordsFanouts,
        Self::RecordsPeers,
        Self::Probes,
        Self::ProbePeers,
        Self::ProbeWholeReads,
        Self::ProbeBodies,
        Self::ProbeBytes,
        Self::ProbeNoNewer,
        Self::ProbeMissing,
        Self::ProbeUnanswered,
        Self::NamesResolved,
        Self::Parses,
        Self::Verifies,
        Self::SignatureAsks,
        Self::SignaturesComputed,
        Self::EntryVerifies,
        Self::FreshPrimary,
        Self::FreshProbe,
        Self::FreshSelectPeer,
        Self::FreshFloor,
        Self::MemoProbe,
        Self::MemoSelectHeld,
        Self::MemoSelectPeer,
        Self::MemoAdmin,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Passes => "passes",
            Self::PrimaryBody => "primary_body",
            Self::PrimaryMissing => "primary_missing",
            Self::PrimarySeq204 => "primary_204_seq",
            Self::PrimaryBare204 => "primary_204_bare",
            Self::PrimaryHeldOut => "primary_held_out",
            Self::PrimaryError => "primary_error",
            Self::RecordsFanouts => "records_fanouts",
            Self::RecordsPeers => "records_peers",
            Self::Probes => "probes",
            Self::ProbePeers => "probe_peers",
            Self::ProbeWholeReads => "probe_whole_reads",
            Self::ProbeBodies => "probe_bodies",
            Self::ProbeBytes => "probe_bytes",
            Self::ProbeNoNewer => "probe_204",
            Self::ProbeMissing => "probe_missing",
            Self::ProbeUnanswered => "probe_unanswered",
            Self::NamesResolved => "names_resolved",
            Self::Parses => "parses",
            Self::Verifies => "verifies",
            Self::SignatureAsks => "signature_asks",
            Self::SignaturesComputed => "signatures_computed",
            Self::EntryVerifies => "entry_verifies",
            Self::FreshPrimary => "fresh_primary",
            Self::FreshProbe => "fresh_probe",
            Self::FreshSelectPeer => "fresh_select_peer",
            Self::FreshFloor => "fresh_floor",
            Self::MemoProbe => "memo_probe",
            Self::MemoSelectHeld => "memo_select_held",
            Self::MemoSelectPeer => "memo_select_peer",
            Self::MemoAdmin => "memo_admin",
        }
    }
}

/// Where the loop's thread is, for its CPU and wall time: every moment of the loop is in exactly
/// one, so the phases' wall times sum to the loop's and their CPU times to its thread's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// The loop's round before its pass.
    Before,
    /// The records pass, less its peer selection.
    Records,
    /// Choosing the peers a fan-out asks, their names' resolution included.
    Peers,
    /// The membership log's vault pass.
    Removal,
    /// The request to the source, its long-poll included.
    Primary,
    /// The admin text check of the held vault.
    Admin,
    /// The envelope probe of the peers, less its peer selection.
    Probe,
    /// Selection among what the peers served.
    Select,
    /// Applying what the pass came to.
    Apply,
    /// The rest of the pass.
    Rest,
    /// The loop's round after its pass.
    After,
    /// The pause or nap that ends the round.
    Pause,
}

impl Phase {
    pub(crate) const ALL: [Self; 12] = [
        Self::Before,
        Self::Records,
        Self::Peers,
        Self::Removal,
        Self::Primary,
        Self::Admin,
        Self::Probe,
        Self::Select,
        Self::Apply,
        Self::Rest,
        Self::After,
        Self::Pause,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::Records => "records",
            Self::Peers => "peers",
            Self::Removal => "removal",
            Self::Primary => "primary",
            Self::Admin => "admin",
            Self::Probe => "probe",
            Self::Select => "select",
            Self::Apply => "apply",
            Self::Rest => "rest",
            Self::After => "after",
            Self::Pause => "pause",
        }
    }
}

/// A subscriber's counts, for as long as its loop runs.
pub(crate) struct PassCounts {
    counts: [AtomicU64; Count::ALL.len()],
    /// How many times a thread that keeps time (the loop's) passed a mark into each phase
    /// ([`enter`], [`within`]); returning to a phase as a [`within`] ends is not one.
    entered: [AtomicU64; Phase::ALL.len()],
    /// The loop thread's CPU and wall time in each phase.
    cpu_ns: [AtomicU64; Phase::ALL.len()],
    wall_ns: [AtomicU64; Phase::ALL.len()],
    /// The threads started in each phase that ended, and their CPU time.
    threads: [ThreadsCpu; Phase::ALL.len()],
    /// Every total as each of the last [`WINDOW`] passes ended, oldest first, and before them
    /// the totals as the pass before those ended (none: every total 0).
    ended: Mutex<VecDeque<Vec<u64>>>,
}

impl Default for PassCounts {
    fn default() -> Self {
        Self {
            counts: std::array::from_fn(|_| AtomicU64::new(0)),
            entered: std::array::from_fn(|_| AtomicU64::new(0)),
            cpu_ns: std::array::from_fn(|_| AtomicU64::new(0)),
            wall_ns: std::array::from_fn(|_| AtomicU64::new(0)),
            threads: std::array::from_fn(|_| ThreadsCpu::new()),
            ended: Mutex::new(VecDeque::from([Vec::new()])),
        }
    }
}

impl PassCounts {
    /// `what` so far.
    pub(crate) fn get(&self, what: Count) -> u64 {
        self.counts[what as usize].load(Ordering::Relaxed)
    }

    /// The threads started in `phase` that ended, and their CPU time.
    #[cfg(test)]
    pub(crate) fn threads(&self, phase: Phase) -> &ThreadsCpu {
        &self.threads[phase as usize]
    }

    /// How many times a mark put a thread in `phase`.
    #[cfg(test)]
    pub(crate) fn entered(&self, phase: Phase) -> u64 {
        self.entered[phase as usize].load(Ordering::Relaxed)
    }

    /// Every total's name, in the order [`Self::totals`] gives them.
    fn names() -> Vec<String> {
        let mut names: Vec<String> = Count::ALL
            .iter()
            .map(|what| what.name().to_owned())
            .collect();
        for phase in Phase::ALL {
            let phase = phase.name();
            names.extend([
                format!("{phase}_entries"),
                format!("{phase}_cpu_ns"),
                format!("{phase}_wall_ns"),
                format!("{phase}_threads"),
                format!("{phase}_threads_cpu_ns"),
            ]);
        }
        names
    }

    fn totals(&self) -> Vec<u64> {
        let mut totals: Vec<u64> = Count::ALL.iter().map(|what| self.get(*what)).collect();
        for phase in Phase::ALL {
            let at = phase as usize;
            totals.extend([
                self.entered[at].load(Ordering::Relaxed),
                self.cpu_ns[at].load(Ordering::Relaxed),
                self.wall_ns[at].load(Ordering::Relaxed),
                self.threads[at].ended.load(Ordering::Relaxed),
                self.threads[at].cpu_ns.load(Ordering::Relaxed),
            ]);
        }
        totals
    }

    /// A pass ended: its totals are kept for the window, and where the passes so far are due
    /// ([`aterm_uds::tally::due`]) the two lines to say, the totals and the last passes'. Call it
    /// on the loop's thread, which reads its clock first so the lines hold the pass's time.
    pub(crate) fn pass_ended(&self) -> Option<[String; 2]> {
        settle();
        let totals = self.totals();
        let mut ended = self.ended.lock().unwrap_or_else(PoisonError::into_inner);
        if ended.len() > WINDOW {
            ended.pop_front();
        }
        ended.push_back(totals.clone());
        if !aterm_uds::tally::due(self.get(Count::Passes)) {
            return None;
        }
        let before = ended.front().cloned().unwrap_or_default();
        let over = ended.len() - 1;
        drop(ended);
        let line = |head: String, values: &mut dyn Iterator<Item = u64>| {
            let mut line = head;
            for (name, value) in Self::names().iter().zip(values) {
                if value != 0 {
                    let _ = write!(line, " {name}={value}");
                }
            }
            line
        };
        Some([
            line("pass counts:".to_owned(), &mut totals.iter().copied()),
            line(
                format!("pass counts, last {over} passes:"),
                &mut totals
                    .iter()
                    .enumerate()
                    .map(|(at, now)| now.saturating_sub(before.get(at).copied().unwrap_or(0))),
            ),
        ])
    }
}

/// The pass a thread works for: its counts, the phase it is in, and on the loop's thread the
/// clock readings its time in that phase is counted from.
struct Here {
    counts: Arc<PassCounts>,
    phase: Phase,
    clock: Option<Clock>,
}

/// A thread's CPU time and the wall clock as its time was last added.
struct Clock {
    cpu_ns: u64,
    wall: Instant,
}

impl Clock {
    fn now() -> Self {
        Self {
            cpu_ns: aterm_uds::cpu::thread_ns(),
            wall: Instant::now(),
        }
    }
}

impl Here {
    /// Add the time since it was last added to the phase the thread is in.
    fn settle(&mut self) {
        if self.clock.is_some() {
            self.settle_at(Clock::now());
        }
    }

    /// [`Self::settle`] at the readings `now`: each nanosecond between two readings goes to one
    /// phase, the one the thread was in.
    fn settle_at(&mut self, now: Clock) {
        let Some(clock) = &mut self.clock else {
            return;
        };
        let at = self.phase as usize;
        self.counts.cpu_ns[at]
            .fetch_add(now.cpu_ns.saturating_sub(clock.cpu_ns), Ordering::Relaxed);
        let waited = now.wall.saturating_duration_since(clock.wall);
        self.counts.wall_ns[at].fetch_add(
            u64::try_from(waited.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        *clock = now;
    }

    /// A mark puts the thread in `phase`, counted as one entry of it where the thread keeps
    /// time (the loop's: the entries are beside its times): the phase it was in.
    fn enter(&mut self, phase: Phase) -> Phase {
        if self.clock.is_some() {
            self.counts.entered[phase as usize].fetch_add(1, Ordering::Relaxed);
        }
        self.back_to(phase)
    }

    /// Settle, and put the thread in `phase` with no entry counted (a [`within`] that ends puts
    /// it back where it was): the phase it was in.
    fn back_to(&mut self, phase: Phase) -> Phase {
        self.settle();
        std::mem::replace(&mut self.phase, phase)
    }
}

thread_local! {
    /// Read with `try_with` wherever a count is added: a thread that is ending (its
    /// thread-locals being dropped) works for no pass and counts nothing, and must not panic.
    static HERE: RefCell<Option<Here>> = const { RefCell::new(None) };
}

/// Count one `what` for the pass this thread works for; nothing where it works for none.
pub(crate) fn add(what: Count) {
    add_n(what, 1);
}

/// Count `n` of `what` for the pass this thread works for.
pub(crate) fn add_n(what: Count, n: usize) {
    let _ = HERE.try_with(|here| {
        if let Some(here) = here.borrow().as_ref() {
            here.counts.counts[what as usize]
                .fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
        }
    });
}

fn settle() {
    let _ = HERE.try_with(|here| {
        if let Some(here) = here.borrow_mut().as_mut() {
            here.settle();
        }
    });
}

/// What [`metered`] and a started thread put on a thread, taken off when it drops.
pub(crate) struct Working(bool);

impl Drop for Working {
    fn drop(&mut self) {
        if self.0 {
            let _ = HERE.try_with(|here| {
                if let Some(mut here) = here.borrow_mut().take() {
                    here.settle();
                }
            });
        }
    }
}

fn work(counts: &Arc<PassCounts>, phase: Phase, clock: bool) -> Working {
    HERE.try_with(|here| {
        let mut here = here.borrow_mut();
        if here.is_some() {
            return Working(false);
        }
        *here = Some(Here {
            counts: Arc::clone(counts),
            phase,
            clock: clock.then(Clock::now),
        });
        Working(true)
    })
    .unwrap_or(Working(false))
}

/// This thread is a subscriber loop's (or one pass's) until the guard drops: what it counts goes
/// to `counts`, and its time to the [`Phase`] it is in, [`Phase::Rest`] to begin with. On a thread
/// that already works for a pass it changes nothing.
pub(crate) fn metered(counts: &Arc<PassCounts>) -> Working {
    work(counts, Phase::Rest, true)
}

/// The phase this thread is in, where it works for a pass.
#[cfg(test)]
pub(crate) fn phase() -> Option<Phase> {
    HERE.with(|here| here.borrow().as_ref().map(|here| here.phase))
}

/// The pass a thread about to start threads works for (none where it works for none), and the
/// phase it is in: each thread it starts runs its work under [`Started::run`].
pub(crate) struct Started(Option<(Arc<PassCounts>, Phase)>);

/// What the threads this thread is about to start work for.
pub(crate) fn starting() -> Started {
    Started(
        HERE.try_with(|here| {
            here.borrow()
                .as_ref()
                .map(|here| (Arc::clone(&here.counts), here.phase))
        })
        .ok()
        .flatten(),
    )
}

impl Started {
    /// Run `work` on a thread started for it alone, as the last thing that thread does: what it
    /// counts goes to the pass's counts, and as it ends it is counted among the threads of the
    /// phase it was started in, with all its CPU time ([`ThreadsCpu::ending`]). Where the
    /// starting thread worked for no pass, `work` alone.
    pub(crate) fn run<T>(&self, work: impl FnOnce() -> T) -> T {
        let Some((counts, phase)) = &self.0 else {
            return work();
        };
        let _working = self::work(counts, *phase, false);
        counts.threads[*phase as usize].ending(work)
    }
}

/// This thread is in `phase` from now, until the next [`enter`] or the end of a [`within`] around
/// this one: on the loop's thread one entry of `phase` is counted.
pub(crate) fn enter(phase: Phase) {
    let _ = HERE.try_with(|here| {
        if let Some(here) = here.borrow_mut().as_mut() {
            here.enter(phase);
        }
    });
}

/// A [`within`]'s phase, left when it drops for the phase the thread was in before.
pub(crate) struct Within(Option<Phase>);

impl Drop for Within {
    fn drop(&mut self) {
        if let Some(before) = self.0 {
            let _ = HERE.try_with(|here| {
                if let Some(here) = here.borrow_mut().as_mut() {
                    here.back_to(before);
                }
            });
        }
    }
}

/// This thread is in `phase` until the guard drops, then back in the phase it was in: on the
/// loop's thread one entry of `phase` is counted and two clock reads are made at each end, and
/// on any other neither.
pub(crate) fn within(phase: Phase) -> Within {
    Within(
        HERE.try_with(|here| here.borrow_mut().as_mut().map(|here| here.enter(phase)))
            .ok()
            .flatten(),
    )
}

#[cfg(test)]
#[path = "pass_counts_tests.rs"]
mod tests;
