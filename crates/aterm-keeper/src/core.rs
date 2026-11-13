// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pure keeper: the custody table, the §5.4 death classifier, offer exclusivity and the relaunch brake.

use std::collections::BTreeMap;

use crate::wire::{
    Birth, CAP_JOURNAL, CAP_LINK, CAP_SENDS_BYE, COMPANION_JOURNAL, MarkerRef, MasterHeader,
    PeerClass, RefuseCode,
};

/// A master's identity: its `st_rdev`.
pub type Rdev = u64;
/// A connection, numbered by the caller.
pub type ConnId = u64;

/// Hard caps (§5.7): masters held, connections, claimants per master.
pub const MAX_MASTERS: usize = 1024;
/// See [`MAX_MASTERS`].
pub const MAX_CONNECTIONS: usize = 16;
/// See [`MAX_MASTERS`].
pub const MAX_CLAIMANTS: usize = 2;
/// How long a peer's death waits for its missing half of the evidence (the EOF, or the kernel's exit status) before it is judged on what arrived.
pub const DEATH_GRACE_MS: u64 = 1_000;
/// How long a PENDING names its successor without that successor registering.
pub const PENDING_TTL_MS: u64 = 30_000;
/// A relaunch whose window has not said HELLO by then has failed (§5.3 step 6; `open`'s exit status is no evidence, §11 M8).
pub const HELLO_DEADLINE_MS: u64 = 15_000;
/// A relaunched window alive this long resets the brake's streak.
pub const HEALTHY_MS: u64 = 30 * 60_000;
/// How long after the keeper runs `open` an App HELLO still counts as that relaunch answering, even past [`HELLO_DEADLINE_MS`]: a slow-booting build is a failed relaunch at the deadline and the keeper's own launch at its HELLO, never a manual launch that resets the brake (a slow-booting crash loop would relaunch forever). §6.2's five-minute "died early" window.
pub const LATE_HELLO_MS: u64 = 5 * 60_000;
/// How often a record held for a handoff is re-scanned with no PENDING alive: the successor that holds it may die, or may never register.
pub const HOLD_RESCAN_MS: u64 = 2_000;
/// Relaunches the brake grants in a row before it holds (§6.2: `MaxStreak`).
pub const MAX_STREAK: u32 = 3;
/// The wait before the first, second and third relaunch in a streak.
pub const BACKOFF_MS: [u64; MAX_STREAK as usize] = [0, 10_000, 60_000];

/// What the world answers about who holds a master.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holders {
    /// No live process of this uid other than the keeper holds it.
    None,
    /// These processes hold it.
    Held(Vec<u32>),
    /// The scan could not decide (a table it needed was unreadable, or the scan itself failed): treated as held, never as free.
    Unknown,
}

/// What the core asks of the world.
pub trait KeeperEnv {
    /// Who, other than the keeper, holds a descriptor for `rdev`.
    fn holders(&mut self, rdev: Rdev) -> Holders;
    /// [`Self::holders`] for each of `rdevs`, in order, from one look at the world.
    fn holders_of(&mut self, rdevs: &[Rdev]) -> Vec<Holders> {
        rdevs.iter().map(|rdev| self.holders(*rdev)).collect()
    }
    /// Whether `pid` is still the shell born at `birth`.
    fn shell_alive(&mut self, pid: u32, birth: Birth) -> bool;
    /// Whether `pid` is still the process born at `birth` (or, with no birth record, whether it names any live process of ours).
    fn pid_alive(&mut self, pid: u32, birth: Option<Birth>) -> bool;
    /// What the crash marker `marker` names for the window `pid` says now (§5.4 row 3's cross-check).
    fn marker(&mut self, pid: u32, marker: &MarkerRef) -> MarkerEvidence {
        let _ = (pid, marker);
        MarkerEvidence::Unknown
    }
}

/// What a window's crash marker says (`aterm-gui`'s `crash_signal`: created empty at every start, `flock`ed by its owner for its whole life, unlinked by its exit path, and written only by the fatal-signal handler).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerEvidence {
    /// No marker named, or none that could be read: no evidence.
    Unknown,
    /// The marker is gone: its owner's exit path unlinked it, or another start's sweep removed a dead one (so a Released marker alone never makes a quit: the status must say `exit(0)`).
    Released,
    /// The marker is still there and its lock is free: its owner died without running its exit path (a signal, SIGKILL, jetsam).
    Dead,
    /// The marker is still there and someone holds its lock: its owner, or a forked child that inherited the descriptor.
    Held,
}

/// A companion descriptor, numbered by the caller.
pub type CompanionId = u64;

/// How many recovering windows may die on one region: at this count the keeper closes the region it retained and offers none (the crash-restore plan §5, "the cap").
pub const MAX_ATTEMPTS: u8 = 2;

/// A crash-restore journal region registered beside a master (COMPANION): the core's handle on the read-only descriptor its caller holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Companion {
    /// The caller's number for the descriptor.
    pub id: CompanionId,
    /// What the region is ([`COMPANION_JOURNAL`]).
    pub kind: u8,
    /// Its length in bytes (the server checked it against the descriptor before the core saw it).
    pub len: u64,
}

/// One record's companions and its poison brake (the crash-restore plan §5, §3.2 N4 and N5).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Journal {
    /// Each live claim's own companion.
    pub own: BTreeMap<ConnId, Companion>,
    /// The claim that reads the master (above); `None` with no claim, or once the reader's claim ended before an older one's.
    pub reader: Option<ConnId>,
    /// Whether [`Self::reader`] settled: its poison window has ended.
    pub settled: bool,
    /// The companion an offer carries: left by a claim that ended (the reader's own, judged at its end, or the outgoing window's while its update successor has not settled), and closed as soon as the reader settles, which supersedes it.
    pub retained: Option<Companion>,
    /// Recovering windows that died on [`Self::retained`] before their own COMPANION: an offer that reverted to Orphaned, or a reader whose claim ended unsettled (§3.2 N4: such a window may have read output the region never saw, so a restore from it is never exact).
    pub attempts: u8,
    /// The process whose reader claim ended while an older claim went on (an update's successor gone during its outgoing window's drain).
    pub reader_gone: Option<u32>,
}

/// A test build's fault point on the poison brake: armed, a death before a COMPANION counts no attempt, the defect the companion conformance's negative control plants (as `aterm_core::fault::triggered` is used; the shipped build's check is constant `false`).
#[cfg(any(test, debug_assertions))]
pub mod fault {
    use std::cell::Cell;

    thread_local! {
        static ATTEMPTS_UNCOUNTED: Cell<bool> = const { Cell::new(false) };
    }

    /// Run `f` with this thread's keeper counting no attempt, disarmed after it even on unwind.
    pub fn with_attempts_uncounted<T>(f: impl FnOnce() -> T) -> T {
        struct Disarm;
        impl Drop for Disarm {
            fn drop(&mut self) {
                ATTEMPTS_UNCOUNTED.with(|a| a.set(false));
            }
        }
        ATTEMPTS_UNCOUNTED.with(|a| a.set(true));
        let _disarm = Disarm;
        f()
    }

    /// Whether the fault is armed on this thread.
    pub(crate) fn attempts_uncounted() -> bool {
        ATTEMPTS_UNCOUNTED.with(Cell::get)
    }
}

#[cfg(not(any(test, debug_assertions)))]
mod fault {
    /// The shipped build: never armed.
    #[inline]
    pub(crate) fn attempts_uncounted() -> bool {
        false
    }
}

impl Journal {
    fn close_all(&mut self, out: &mut Vec<Out>) {
        for c in std::mem::take(&mut self.own).into_values() {
            out.push(Out::CloseCompanion(c.id));
        }
        if let Some(c) = self.retained.take() {
            out.push(Out::CloseCompanion(c.id));
        }
        self.settled = false;
    }

    fn retain(&mut self, c: Companion, out: &mut Vec<Out>) {
        if let Some(old) = self.retained.replace(c)
            && old.id != c.id
        {
            out.push(Out::CloseCompanion(old.id));
        }
    }

    fn drop_retained(&mut self, out: &mut Vec<Out>) {
        if let Some(c) = self.retained.take() {
            out.push(Out::CloseCompanion(c.id));
        }
    }

    fn count_attempt(&mut self, out: &mut Vec<Out>) {
        if !fault::attempts_uncounted() {
            self.attempts = self.attempts.saturating_add(1);
        }
        if self.attempts >= MAX_ATTEMPTS {
            self.drop_retained(out);
        }
    }

    const fn claimed(&mut self, conn: ConnId) {
        self.reader = Some(conn);
        self.settled = false;
        self.reader_gone = None;
    }

    fn claim_moved(&mut self, from: ConnId, to: ConnId, out: &mut Vec<Out>) {
        if let Some(c) = self.own.remove(&from) {
            match self.own.entry(to) {
                std::collections::btree_map::Entry::Occupied(_) => {
                    out.push(Out::CloseCompanion(c.id));
                }
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(c);
                }
            }
        }
        if self.reader == Some(from) {
            self.reader = Some(to);
        }
    }

    fn would_offer(&self) -> Option<Companion> {
        match self.reader {
            Some(r) if self.settled => self.own.get(&r).copied(),
            Some(r) => self
                .own
                .iter()
                .find(|(c, _)| **c != r)
                .map(|(_, c)| *c)
                .or(self.retained),
            None => self.retained,
        }
    }

    fn reader_ended(&mut self, conn: ConnId, out: &mut Vec<Out>) {
        let own = self.own.remove(&conn);
        if std::mem::take(&mut self.settled) {
            self.attempts = 0;
            match own {
                Some(c) => self.retain(c, out),
                None => self.drop_retained(out),
            }
        } else {
            // A companion settles its claim, so an unsettled reader holds none.
            if let Some(c) = own {
                out.push(Out::CloseCompanion(c.id));
            }
            if let Some(&older) = self.own.keys().next_back()
                && let Some(c) = self.own.remove(&older)
            {
                self.retain(c, out);
            }
            self.count_attempt(out);
        }
        for c in std::mem::take(&mut self.own).into_values() {
            out.push(Out::CloseCompanion(c.id));
        }
        self.reader = None;
    }

    fn claim_left(&mut self, conn: ConnId, pid: Option<u32>, out: &mut Vec<Out>) {
        if self.reader == Some(conn) {
            self.reader_ended(conn, out);
            self.reader_gone = pid;
            return;
        }
        if let Some(c) = self.own.remove(&conn) {
            if self.reader.is_some() && !self.settled {
                self.retain(c, out);
            } else {
                out.push(Out::CloseCompanion(c.id));
            }
        }
    }

    fn last_claim_left(&mut self, conn: ConnId, out: &mut Vec<Out>) {
        if self.reader == Some(conn) {
            self.reader_ended(conn, out);
        }
        for c in std::mem::take(&mut self.own).into_values() {
            out.push(Out::CloseCompanion(c.id));
        }
        self.reader = None;
        self.settled = false;
    }

    fn offerable(&self) -> Option<Companion> {
        self.retained.filter(|_| self.attempts < MAX_ATTEMPTS)
    }

    fn held(&self) -> impl Iterator<Item = &Companion> {
        self.own.values().chain(self.retained.iter())
    }
}

/// What an OFFER to a window that stated [`CAP_JOURNAL`] carries beside the master (an OFFER's trailing fields, and the COMPANION_OFFER after it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OfferJournal {
    /// The record's [`Journal::attempts`].
    pub attempts: u8,
    /// The region offered with the master, if any.
    pub companion: Option<Companion>,
}

/// The companion side of one record, as `KeeperCompanionCustody` sees it (the Tier-1 projection).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompanionView {
    /// The companion an offer would carry if the record were orphaned now: the reader's own once it settled, else the fallback (the model's `comp`).
    pub offered: Option<CompanionId>,
    /// Whether the reader ([`Journal::reader`]) has settled; `None` with no claim reading the master (the model's `done`).
    pub reader_settled: Option<bool>,
    /// The brake.
    pub attempts: u8,
    /// The own region of a claim that is not the reader: an update's outgoing window's, from its successor's Commit to its own end (the model's `ocomp`).
    pub outgoing: Option<CompanionId>,
}

/// A decision the caller carries out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    /// Keep the descriptor that arrived with this REGISTER as the custody copy.
    Keep(Rdev),
    /// Close the descriptor that arrived with this REGISTER: the keeper already holds a copy of that master (a claim that moved).
    DropDuplicate(Rdev),
    /// Close the custody copy of this master: its record is gone, and every companion beside it closes with it (a companion travels only with its master).
    Close(Rdev),
    /// Keep the descriptor that arrived with this COMPANION: the core holds it as a claim's companion.
    KeepCompanion(CompanionId),
    /// Close this companion's descriptor: superseded, capped, or no longer anyone's.
    CloseCompanion(CompanionId),
    /// Send WELCOME with this many OFFERs to follow.
    Welcome { conn: ConnId, offers: u16 },
    /// Send an OFFER of this master, with a duplicate of the custody copy; to a window that stated [`CAP_JOURNAL`], with its trailing fields, and a COMPANION_OFFER of the companion right after it.
    Offer {
        conn: ConnId,
        rdev: Rdev,
        journal: Option<OfferJournal>,
    },
    /// Send ORPHANS(n): a live sibling may ADOPT.
    Orphans { conn: ConnId, n: u16 },
    /// Send REFUSED.
    Refused {
        conn: ConnId,
        code: RefuseCode,
        rdev: Rdev,
    },
    /// Start watching this pid's exit (a PENDING successor, a hold's holder, an orphan's shell).
    WatchPid(u32),
    /// Relaunch the app (`/usr/bin/open -a <own bundle>`).
    Relaunch,
}

/// The §5.4 verdict on one dead peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A live successor still holds the masters, or this window's own unexpired PENDING names a live pid, or its `exit(0)` came with the end of the successor its own PENDING named (a Commit to a successor that died first): hold, and judge by the successor's end.
    Handoff,
    /// BYE read before EOF: close its masters now.
    Quit,
    /// No BYE arrived, but the window ended with `exit(0)` and its crash marker is gone: a quit whose BYE was lost (the link was reconnecting, the write did not land).
    CleanExit,
    /// A peer that never spoke BYE-capable AKP1 ended with `exit(0)`: a pre-keeper build reached by rollback.
    CleanPreKeeper,
    /// Anything else: a signal, a non-zero exit, a lost BYE.
    Crash,
}

/// The evidence the classifier reads, in its table's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeathEvidence {
    /// A successor holds this peer's masters ([`Verdict::Handoff`]'s three cases).
    pub successor_holds: bool,
    /// BYE was read before EOF.
    pub bye: bool,
    /// The peer's HELLO said it sends BYE on its quit path.
    pub sends_bye: bool,
    /// The kernel's `wait(2)` status, when the exit watch delivered one.
    pub status: Option<i32>,
    /// What the window's crash marker says (§5.4 row 3's cross-check).
    pub marker: MarkerEvidence,
}

/// Whether a `wait(2)` status is `exit(0)`.
#[must_use]
pub const fn exited_zero(status: i32) -> bool {
    status & 0x7f == 0 && (status >> 8) & 0xff == 0
}

/// The §5.4 classifier, evidence read in the table's order.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "PtyKeeperCustody",
        action = "KeeperHonoursBye",
        project = "aterm_keeper::KeeperCore::custody_view"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "KeeperDeathJudgement",
        action = "Judge",
        project = "aterm_keeper::KeeperCore::last_verdict"
    )
)]
pub fn classify_death(e: DeathEvidence) -> Verdict {
    if e.successor_holds {
        return Verdict::Handoff;
    }
    if e.bye {
        return Verdict::Quit;
    }
    let exit_zero = e.status.is_some_and(exited_zero);
    if exit_zero && e.marker == MarkerEvidence::Released {
        return Verdict::CleanExit;
    }
    if !e.sends_bye && exit_zero && e.marker != MarkerEvidence::Dead {
        return Verdict::CleanPreKeeper;
    }
    Verdict::Crash
}

/// What the brake decides at a death.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrakeDecision {
    /// A quit: nothing to relaunch.
    Nothing,
    /// Relaunch at this clock reading.
    RelaunchAt(u64),
    /// The streak is spent: hold until a manual launch.
    Hold,
}

/// The relaunch brake (§6.2), a writer/reader pair: [`RelaunchBrake::decide`] reads the streak to choose relaunch or hold and writes the decision back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelaunchBrake {
    streak: u32,
    held: bool,
}

impl RelaunchBrake {
    /// A fresh brake: nothing spent.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "KeeperRelaunchBrake",
            action = "PickRepeatedDeaths",
            reason = "The environment's pick of a scenario; no brake code runs."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "KeeperRelaunchBrake",
            action = "PickBootTrial",
            reason = "As PickRepeatedDeaths."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "KeeperRelaunchBrake",
            action = "Death",
            reason = "A window's non-clean end; the brake is consulted at Decide (KeeperCore's \
                      peer_died and resolve_holds call RelaunchBrake::decide)."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "KeeperRelaunchBrake",
            action = "CleanQuit",
            reason = "A window's quit; decided at Decide with clean = true."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "KeeperRelaunchBrake",
            action = "RelaunchFails",
            reason = "The relaunched window's early death or missing HELLO (KeeperCore's \
                      HELLO_DEADLINE_MS); decided at the next Decide."
        )
    )]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Relaunches granted in a row since the last reset.
    #[must_use]
    pub const fn streak(&self) -> u32 {
        self.streak
    }

    /// Whether the brake is holding (only a manual launch lifts it).
    #[must_use]
    pub const fn held(&self) -> bool {
        self.held
    }

    /// Decide at a death: a quit (`clean`) relaunches nothing; otherwise relaunch after the streak's backoff while it has room, else hold.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperRelaunchBrake",
            action = "Decide",
            project = "aterm_keeper::RelaunchBrake::streak"
        )
    )]
    pub fn decide(&mut self, now: u64, clean: bool) -> BrakeDecision {
        if clean {
            return BrakeDecision::Nothing;
        }
        if self.streak < MAX_STREAK {
            let wait = BACKOFF_MS
                .get(self.streak as usize)
                .copied()
                .unwrap_or(BACKOFF_MS[BACKOFF_MS.len() - 1]);
            self.streak += 1;
            self.held = false;
            BrakeDecision::RelaunchAt(now.saturating_add(wait))
        } else {
            self.held = true;
            BrakeDecision::Hold
        }
    }

    /// The relaunched window lived [`HEALTHY_MS`]: the brake resets.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperRelaunchBrake",
            action = "Healthy",
            project = "aterm_keeper::RelaunchBrake::streak"
        )
    )]
    pub fn healthy(&mut self) {
        self.streak = 0;
        self.held = false;
    }

    /// A launch the keeper did not cause said HELLO: the brake resets.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperRelaunchBrake",
            action = "ManualLaunch",
            project = "aterm_keeper::RelaunchBrake::streak"
        )
    )]
    pub fn manual_launch(&mut self) {
        self.streak = 0;
        self.held = false;
    }
}

/// A record's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordState {
    /// Live claimants hold it.
    Claimed,
    /// Its last claimant died in a handoff: waiting for the successor.
    Held {
        since: u64,
        /// The process the hold waits on: the PENDING successor, or the one process the holder scan found holding the master — and, once that process ended while another holds it, that holder.
        successor: Option<u32>,
        /// The claimant that left it held, and each holder that handed it on since, ended as a quit does — its BYE, or `exit(0)` (an update's Commit `_exit`s 0) — not by a crash.
        outgoing_clean: bool,
    },
    /// Its last claimant crashed: on offer to the next window.
    Orphaned,
    /// Offered to this connection, which has not registered it yet.
    Offered { to: ConnId },
}

/// One master the keeper holds a custody copy of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub header: MasterHeader,
    /// Opaque: never decoded.
    pub tag: Vec<u8>,
    /// Opaque: never decoded.
    pub meta: Vec<u8>,
    /// Its claims, oldest first: the last is the READER, the process that registered it last (an update's successor after its Commit, else the only claimant).
    pub claimants: Vec<ConnId>,
    pub state: RecordState,
    /// Its crash-restore companions and their poison brake.
    pub journal: Journal,
    /// Its last window's quit kept it ([`KeeperCore::bye_keeping`]), so it is on offer because a
    /// BYE named it, not because a window crashed: an offer of it says so to a window that reads
    /// it ([`crate::wire::CAP_KEEPS`]), whose launch then tells of no crash. Until a window
    /// claims it again.
    pub kept: bool,
}

/// One connection's peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub pid: u32,
    pub birth: Option<Birth>,
    pub class: Option<PeerClass>,
    pub caps: u8,
    pub bye: bool,
    /// The records its quit's BYE named to outlive it ([`KeeperCore::bye_keeping`]): each one's
    /// shell goes on in the keeper's custody for the next launch to adopt.
    pub keeps: Vec<Rdev>,
    /// When EOF was read.
    pub eof_at: Option<u64>,
    /// When the exit status arrived, and what it was.
    pub exit: Option<(u64, i32)>,
    /// When HELLO arrived.
    pub hello_at: Option<u64>,
    /// This window is one the keeper relaunched.
    pub relaunched: bool,
    /// The window's crash marker, once HELLO named one the keeper found held by a live owner ([`KeeperCore::name_marker`]).
    pub marker: Option<MarkerRef>,
    /// Offers made to this connection went back to Orphaned when its EOF was read ([`KeeperCore::eof`]): its death, when judged, owes those orphans a window unless the process quit.
    pub returned_offers: bool,
}

impl Peer {
    fn is_app(&self) -> bool {
        self.class == Some(PeerClass::App)
    }
    const fn journals(&self) -> bool {
        self.caps & CAP_JOURNAL != 0
    }
    const fn connected(&self) -> bool {
        self.eof_at.is_none() && self.exit.is_none()
    }
}

/// A PENDING: the update successor a window named before its grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingSuccessor {
    pub pid: u32,
    pub birth: Option<Birth>,
    pub expires: u64,
    /// The window process (pid and birth) that named it: only that window's masters wait on this successor.
    pub from_pid: u32,
    pub from_birth: Option<Birth>,
    /// When the successor's `wait(2)` status was read, and what it was.
    pub ended: Option<(u64, i32)>,
}

impl PendingSuccessor {
    fn is_from(&self, pid: u32, birth: Option<Birth>) -> bool {
        (self.from_pid, self.from_birth) == (pid, birth)
    }

    fn named_by(&self, peer: &Peer) -> bool {
        self.is_from(peer.pid, peer.birth) && self.pid != peer.pid
    }
}

/// Where the relaunch lane stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelaunchState {
    /// Nothing to do.
    Idle,
    /// A relaunch is due at this clock reading.
    Due { at: u64 },
    /// `open` ran at `since`; `hello` is the window that answered, if one has.
    InFlight {
        since: u64,
        hello: Option<(ConnId, u64)>,
    },
    /// The brake holds.
    Held,
}

/// How the keeper has judged the ends it saw, and what it pruned (status).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EndCounts {
    /// Records closed on a BYE.
    pub quit: u64,
    /// Records a quit's BYE named to keep, left orphaned for the next launch with their shells
    /// alive ([`KeeperCore::bye_keeping`]).
    pub kept: u64,
    /// Records closed on `exit(0)` with the crash marker gone (a lost BYE).
    pub clean_exit: u64,
    /// Records closed on a pre-keeper peer's `exit(0)`, or on the `exit(0)` of a held master's successor that never took part in the keeper.
    pub clean_pre_keeper: u64,
    /// Records a crash left (orphaned, or closed because the shell was gone).
    pub crash: u64,
    /// Records held for a handoff.
    pub handoff: u64,
    /// Orphans closed because their shell died (never offered, never listed).
    pub pruned: u64,
}

/// The keeper's side of the custody model for two named masters (the Tier-1 projection): whether it holds each, and whether each is an orphan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CustodyView {
    pub holds_a: bool,
    pub holds_b: bool,
    pub orphan_a: bool,
    pub orphan_b: bool,
}

/// The keeper's state and every decision it makes.
#[derive(Clone, Debug)]
pub struct KeeperCore {
    records: BTreeMap<Rdev, Record>,
    peers: BTreeMap<ConnId, Peer>,
    /// The updates in flight, keyed by the window process (pid and birth) that sent each: every
    /// such process runs its own update lane, so two can overlap.
    pending: Vec<PendingSuccessor>,
    /// Each window process's newest PENDING once it is no longer in flight, kept until that window
    /// is judged: whether its `exit(0)` was its Commit to a successor that died first
    /// ([`Self::peer_died`]).
    settled: Vec<PendingSuccessor>,
    brake: RelaunchBrake,
    relaunch: RelaunchState,
    /// Whether this keeper can relaunch at all (it runs inside an app bundle).
    can_relaunch: bool,
    relaunches: u64,
    last_hold_scan: u64,
    /// When the keeper last ran `open` (see [`LATE_HELLO_MS`]).
    last_relaunch_at: Option<u64>,
    /// How ends were judged (status, and the Tier-1 projection).
    ends: EndCounts,
    /// The newest verdict the classifier gave.
    last_verdict: Option<Verdict>,
    /// The `wait(2)` status of each process a hold waits on that ended without ever saying HELLO
    /// here as a window, or that died before its outgoing window's Commit was judged
    /// ([`Self::peer_died`]), kept until its holds are judged ([`Self::resolve_holds`]).
    quiet_ends: BTreeMap<u32, i32>,
}

impl KeeperCore {
    /// A keeper with an empty table.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "PtyKeeperCustody",
            action = "Quit",
            reason = "The window writes BYE on its final-exit path (aterm-gui's keeper_link::bye, beside \
                      lib.rs's exit(0)); the keeper only records it (KeeperCore::bye), which this \
                      runner drives, and judges the end at the EOF and exit (KeeperHonoursBye)."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "PtyKeeperCustody",
            action = "QuitKeepingA",
            reason = "As Quit, with the window's BYE naming the masters to keep past it (aterm-gui's \
                      keeper_link::bye_keeping: the Mac Setup tab's installer); the keeper records \
                      it (KeeperCore::bye_keeping), which this runner drives, and judges the end \
                      at the EOF and exit (KeeperHonoursBye)."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "PtyKeeperCustody",
            action = "LaunchAfterQuit",
            reason = "A person's or macOS's launch of a new window after a quit that kept a master \
                      (Quit & Reopen's reopen, the Dock); the keeper sees only its HELLO \
                      (KeeperCore::hello), which this runner drives and whose OFFER it maps to \
                      OfferA."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "PtyKeeperCustody",
            action = "StartSuccessor",
            reason = "The seamless update's own step (app_update_handoff); the keeper's half is \
                      KeeperCore::pending, driven here as the PENDING frame."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "PtyKeeperCustody",
            action = "Rollback",
            reason = "The window proves its successor dead (app_update_handoff); the keeper sees \
                      only the successor's exit status, driven here through KeeperCore::peer_exit."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "PtyKeeperCustody",
            action = "AdoptA",
            reason = "The recovering window's admission and reader (aterm-gui's \
                      keeper_link::admit_recovered_master); \
                      the keeper sees its REGISTER, driven here."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "PtyKeeperCustody",
            action = "AdoptB",
            reason = "As AdoptA."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "PtyKeeperCustody",
            action = "KeeperCrashes",
            reason = "The keeper process dies; the kernel closes every custody copy. The runner \
                      drops the core."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "PtyKeeperCustody",
            action = "KeeperRestarts",
            reason = "launchd respawns the job (M1: KeepAlive, 10 s throttle); a fresh KeeperCore \
                      with an empty table, which the runner constructs."
        )
    )]
    #[must_use]
    pub fn new(can_relaunch: bool) -> Self {
        Self {
            records: BTreeMap::new(),
            peers: BTreeMap::new(),
            pending: Vec::new(),
            settled: Vec::new(),
            brake: RelaunchBrake::new(),
            relaunch: RelaunchState::Idle,
            last_relaunch_at: None,
            can_relaunch,
            relaunches: 0,
            last_hold_scan: 0,
            ends: EndCounts::default(),
            last_verdict: None,
            quiet_ends: BTreeMap::new(),
        }
    }

    /// How the keeper has judged the ends it saw.
    #[must_use]
    pub const fn ends(&self) -> EndCounts {
        self.ends
    }

    /// The newest verdict the classifier gave (the `KeeperDeathJudgement` projection).
    #[must_use]
    pub const fn last_verdict(&self) -> Option<Verdict> {
        self.last_verdict
    }

    /// The records, for status and tests.
    #[must_use]
    pub const fn records(&self) -> &BTreeMap<Rdev, Record> {
        &self.records
    }

    /// The peers, for status and tests.
    #[must_use]
    pub const fn peers(&self) -> &BTreeMap<ConnId, Peer> {
        &self.peers
    }

    /// The brake, for status and tests.
    #[must_use]
    pub const fn brake(&self) -> &RelaunchBrake {
        &self.brake
    }

    /// The relaunch lane, for status and tests.
    #[must_use]
    pub const fn relaunch_state(&self) -> RelaunchState {
        self.relaunch
    }

    /// How many relaunches this keeper has made.
    #[must_use]
    pub const fn relaunches(&self) -> u64 {
        self.relaunches
    }

    /// The updates in flight, keyed by the window process that sent each.
    #[must_use]
    pub fn pending_successors(&self) -> &[PendingSuccessor] {
        &self.pending
    }

    /// The Tier-1 projection of two masters (see [`CustodyView`]).
    #[must_use]
    pub fn custody_view(&self, a: Rdev, b: Rdev) -> CustodyView {
        let orphan = |r: Rdev| {
            self.records
                .get(&r)
                .is_some_and(|rec| rec.state == RecordState::Orphaned)
        };
        CustodyView {
            holds_a: self.records.contains_key(&a),
            holds_b: self.records.contains_key(&b),
            orphan_a: orphan(a),
            orphan_b: orphan(b),
        }
    }

    /// The companion side of `rdev`'s record (the `KeeperCompanionCustody` projection), `None` once the record is gone.
    #[must_use]
    pub fn companion_view(&self, rdev: Rdev) -> Option<CompanionView> {
        let rec = self.records.get(&rdev)?;
        let j = &rec.journal;
        let reader_settled = (rec.state == RecordState::Claimed)
            .then_some(j.reader)
            .flatten()
            .map(|_| j.settled);
        let outgoing = j
            .own
            .iter()
            .find(|(c, _)| j.reader != Some(**c))
            .map(|(_, c)| c.id);
        Some(CompanionView {
            offered: j.would_offer().map(|c| c.id),
            reader_settled,
            attempts: j.attempts,
            outgoing,
        })
    }

    /// Every companion the core holds (each claim's own and each record's retained one), by the caller's number: exactly the descriptors its caller must hold after carrying out every [`Out`] so far, no more and no fewer (the server's tests hold it to that).
    #[must_use]
    pub fn companions_held(&self) -> std::collections::BTreeSet<CompanionId> {
        self.records
            .values()
            .flat_map(|r| r.journal.held())
            .map(|c| c.id)
            .collect()
    }

    /// Orphans on offer now.
    #[must_use]
    pub fn orphan_count(&self) -> usize {
        self.records
            .values()
            .filter(|r| r.state == RecordState::Orphaned)
            .count()
    }

    /// A connection was accepted, from a same-uid peer the kernel named.
    pub fn accept(&mut self, conn: ConnId, pid: u32, birth: Option<Birth>) -> bool {
        let live = self.peers.values().filter(|p| p.connected()).count();
        if live >= MAX_CONNECTIONS {
            return false;
        }
        self.peers.insert(
            conn,
            Peer {
                pid,
                birth,
                class: None,
                caps: 0,
                bye: false,
                keeps: Vec::new(),
                eof_at: None,
                exit: None,
                hello_at: None,
                relaunched: false,
                marker: None,
                returned_offers: false,
            },
        );
        true
    }

    /// The window on `conn` named its crash marker in its HELLO.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "PickP3Window",
            reason = "The environment's pick of a window build (a P3 window names no marker); no keeper code runs."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "PickPreByeWindow",
            reason = "As PickP3Window (a window from before BYE: no CAP_SENDS_BYE, no marker)."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "QuitWithBye",
            reason = "The window's final-exit path (aterm-gui's keeper_link::bye, then exit(0), whose atexit \
                      unlinks the marker); the keeper records the BYE (KeeperCore::bye) and judges at Judge."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "QuitByeLost",
            reason = "As QuitWithBye with the BYE not written; the keeper sees only the EOF and the exit."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "Killed",
            reason = "The kernel ends the window with no exit path (SIGKILL, jetsam, a fatal signal); the \
                      marker stays, its lock freed. The keeper sees the EOF and the exit status."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "PanicExit",
            reason = "An unwinding panic: exit(101) through the atexit that unlinks the marker."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "StatusLost",
            reason = "The exit watch could not be armed (ESRCH at the connect); the keeper judges on the \
                      EOF and a dead pid after DEATH_GRACE_MS, with no status."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "MarkerSwept",
            reason = "Another aterm start's install sweep removes a dead, empty marker (aterm-gui's \
                      crash_signal::markers::sweep)."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "MarkerInherited",
            reason = "A forked child still holds the marker's descriptor and lock."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "ShellDies",
            reason = "The orphan's session leader exits; the keeper learns it from the exit watch it armed \
                      (Out::WatchPid) and prunes at KeeperSeesShellExit."
        )
    )]
    pub fn name_marker(
        &mut self,
        conn: ConnId,
        marker: MarkerRef,
        env: &mut dyn KeeperEnv,
    ) -> bool {
        let Some(peer) = self.peers.get(&conn) else {
            return false;
        };
        if env.marker(peer.pid, &marker) != MarkerEvidence::Held {
            return false;
        }
        if let Some(p) = self.peers.get_mut(&conn) {
            p.marker = Some(marker);
        }
        true
    }

    /// HELLO.
    pub fn hello(
        &mut self,
        conn: ConnId,
        class: PeerClass,
        caps: u8,
        now: u64,
        env: &mut dyn KeeperEnv,
    ) -> Vec<Out> {
        let Some(peer) = self.peers.get_mut(&conn) else {
            return Vec::new();
        };
        peer.class = Some(class);
        peer.caps = caps;
        peer.hello_at = Some(now);
        if class == PeerClass::Status || caps & CAP_LINK != 0 {
            return vec![Out::Welcome { conn, offers: 0 }];
        }
        match self.relaunch {
            RelaunchState::InFlight { since, hello: None } => {
                if let Some(p) = self.peers.get_mut(&conn) {
                    p.relaunched = true;
                }
                self.relaunch = RelaunchState::InFlight {
                    since,
                    hello: Some((conn, now)),
                };
            }
            _ if self
                .last_relaunch_at
                .is_some_and(|at| now < at.saturating_add(LATE_HELLO_MS)) =>
            {
                if let Some(p) = self.peers.get_mut(&conn) {
                    p.relaunched = true;
                }
                if let (Some(since), RelaunchState::Due { .. } | RelaunchState::Held) =
                    (self.last_relaunch_at, self.relaunch)
                {
                    self.relaunch = RelaunchState::InFlight {
                        since,
                        hello: Some((conn, now)),
                    };
                }
            }
            _ => {
                self.brake.manual_launch();
                if matches!(
                    self.relaunch,
                    RelaunchState::Due { .. } | RelaunchState::Held
                ) {
                    self.relaunch = RelaunchState::Idle;
                }
            }
        }
        self.offers_for(conn, env)
    }

    /// ADOPT: a live sibling that read ORPHANS asks for the offers now.
    pub fn adopt(&mut self, conn: ConnId, env: &mut dyn KeeperEnv) -> Vec<Out> {
        match self.peers.get(&conn) {
            Some(p) if p.is_app() => self.offers_for(conn, env),
            Some(_) => vec![Out::Refused {
                conn,
                code: RefuseCode::WrongClass,
                rdev: 0,
            }],
            None => Vec::new(),
        }
    }

    /// The offer rule: every Orphaned record whose master no live process holds (the holder scan)
    /// and whose shell is still the one registered is offered to `conn` and marked Offered to it;
    /// the keeper keeps its copy.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "OfferA",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "OfferB",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperCompanionCustody",
            action = "OfferNew",
            project = "aterm_keeper::KeeperCore::companion_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperCompanionCustody",
            action = "OfferOld",
            project = "aterm_keeper::KeeperCore::companion_view"
        )
    )]
    fn offers_for(&mut self, conn: ConnId, env: &mut dyn KeeperEnv) -> Vec<Out> {
        let mut offers = Vec::new();
        let journals = self.peers.get(&conn).is_some_and(Peer::journals);
        let mut closes = Vec::new();
        let orphans: Vec<Rdev> = self
            .records
            .iter()
            .filter(|(_, r)| r.state == RecordState::Orphaned)
            .map(|(rdev, _)| *rdev)
            .collect();
        let mut live = Vec::new();
        for rdev in orphans {
            let Some(rec) = self.records.get(&rdev) else {
                continue;
            };
            if env.shell_alive(rec.header.shell_pid, rec.header.shell_birth) {
                live.push(rdev);
            } else {
                self.records.remove(&rdev);
                closes.push(Out::Close(rdev));
            }
        }
        let holders = env.holders_of(&live);
        for (rdev, holders) in live.into_iter().zip(holders) {
            if holders != Holders::None {
                continue;
            }
            if let Some(rec) = self.records.get_mut(&rdev) {
                rec.state = RecordState::Offered { to: conn };
                let journal = journals.then(|| OfferJournal {
                    attempts: rec.journal.attempts,
                    companion: rec.journal.offerable(),
                });
                offers.push(Out::Offer {
                    conn,
                    rdev,
                    journal,
                });
            }
        }
        let mut out = closes;
        out.push(Out::Welcome {
            conn,
            offers: u16::try_from(offers.len()).unwrap_or(u16::MAX),
        });
        out.extend(offers);
        out
    }

    /// REGISTER: a claim on a master, proved by possession — the descriptor's `st_rdev` (`fd_rdev`, read by the caller with `fstat`) must be the one the header names.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "RegisterA",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "RegisterB",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperCompanionCustody",
            action = "Claim",
            project = "aterm_keeper::KeeperCore::companion_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperCompanionCustody",
            action = "SuccessorCommits",
            project = "aterm_keeper::KeeperCore::companion_view"
        )
    )]
    pub fn register(
        &mut self,
        conn: ConnId,
        header: MasterHeader,
        tag: Vec<u8>,
        fd_rdev: Rdev,
    ) -> Vec<Out> {
        let rdev = header.rdev;
        let refuse = |code| vec![Out::Refused { conn, code, rdev }, Out::DropDuplicate(rdev)];
        let (pid, birth) = match self.peers.get(&conn) {
            Some(p) if p.is_app() && p.connected() => (p.pid, p.birth),
            _ => return refuse(RefuseCode::WrongClass),
        };
        if fd_rdev != rdev {
            return refuse(RefuseCode::RdevMismatch);
        }
        self.retire_pending(|p, _| p.pid == pid);
        let same_process: Vec<ConnId> = self
            .peers
            .iter()
            .filter(|(c, p)| {
                **c != conn
                    && p.pid == pid
                    && (p.birth == birth
                        || (p.eof_at.is_some() && (p.birth.is_none() || birth.is_none())))
            })
            .map(|(c, _)| *c)
            .collect();
        let journals = self.peers.get(&conn).is_some_and(Peer::journals);
        if let Some(rec) = self.records.get_mut(&rdev) {
            if rec
                .claimants
                .iter()
                .any(|c| *c > conn && same_process.contains(c))
            {
                return vec![Out::DropDuplicate(rdev)];
            }
            let moved: Vec<ConnId> = rec
                .claimants
                .iter()
                .copied()
                .filter(|c| same_process.contains(c))
                .collect();
            rec.claimants.retain(|c| !same_process.contains(c));
            let mut new_claim = false;
            if !rec.claimants.contains(&conn) {
                // Only a claim that replaced none can find the slots full: nothing moved.
                if rec.claimants.len() >= MAX_CLAIMANTS {
                    return refuse(RefuseCode::TooManyClaimants);
                }
                rec.claimants.push(conn);
                new_claim = moved.is_empty();
            }
            let mut out = Vec::new();
            if new_claim {
                rec.journal.claimed(conn);
            }
            for from in moved {
                rec.journal.claim_moved(from, conn, &mut out);
            }
            if !journals {
                rec.journal.close_all(&mut out);
            }
            rec.state = RecordState::Claimed;
            rec.kept = false;
            rec.header = header;
            if !tag.is_empty() {
                rec.tag = tag;
            }
            out.push(Out::DropDuplicate(rdev));
            return out;
        }
        if self.records.len() >= MAX_MASTERS {
            return refuse(RefuseCode::TooManyMasters);
        }
        self.records.insert(
            rdev,
            Record {
                header,
                tag,
                meta: Vec::new(),
                claimants: vec![conn],
                state: RecordState::Claimed,
                journal: Journal {
                    reader: Some(conn),
                    ..Journal::default()
                },
                kept: false,
            },
        );
        vec![Out::Keep(rdev)]
    }

    /// META: the newest Repaint-rung scalar state of a master this connection claims.
    pub fn meta(&mut self, conn: ConnId, rdev: Rdev, meta: Vec<u8>) -> Vec<Out> {
        match self.records.get_mut(&rdev) {
            Some(rec) if rec.claimants.contains(&conn) => {
                rec.meta = meta;
                Vec::new()
            }
            _ => vec![Out::Refused {
                conn,
                code: RefuseCode::NotClaimant,
                rdev,
            }],
        }
    }

    fn companion_sender(&self, conn: ConnId, rdev: Rdev, kind: u8) -> Result<(), RefuseCode> {
        match self.peers.get(&conn) {
            Some(p) if p.is_app() && p.connected() && p.journals() => {}
            _ => return Err(RefuseCode::WrongClass),
        }
        if kind != COMPANION_JOURNAL {
            return Err(RefuseCode::NotAJournal);
        }
        match self.records.get(&rdev) {
            None => Err(RefuseCode::NoSuchRecord),
            Some(rec) if !rec.claimants.contains(&conn) => Err(RefuseCode::NotClaimant),
            Some(_) => Ok(()),
        }
    }

    /// COMPANION: the journal region of a master this connection claims, `id` being the caller's number for the read-only descriptor it already checked (POSIX shared memory, `O_RDONLY`, `len` bytes within the keeper's bounds).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperCompanionCustody",
            action = "ReaderCompanion",
            project = "aterm_keeper::KeeperCore::companion_view"
        )
    )]
    pub fn companion(
        &mut self,
        conn: ConnId,
        rdev: Rdev,
        kind: u8,
        len: u64,
        id: CompanionId,
    ) -> Vec<Out> {
        if let Err(code) = self.companion_sender(conn, rdev, kind) {
            return vec![Out::Refused { conn, code, rdev }];
        }
        let mut out = Vec::new();
        if let Some(rec) = self.records.get_mut(&rdev) {
            let j = &mut rec.journal;
            if let Some(old) = j.own.insert(conn, Companion { id, kind, len }) {
                out.push(Out::CloseCompanion(old.id));
            }
            if j.reader == Some(conn) {
                j.settled = true;
                j.attempts = 0;
                j.drop_retained(&mut out);
            }
            out.push(Out::KeepCompanion(id));
        }
        out
    }

    /// COMPANION_RELEASE: this connection's claim on `rdev` has no journal (its region could not be made, or memory pressure shed it).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperCompanionCustody",
            action = "ReaderReleases",
            project = "aterm_keeper::KeeperCore::companion_view"
        )
    )]
    pub fn companion_release(&mut self, conn: ConnId, rdev: Rdev, kind: u8) -> Vec<Out> {
        if let Err(code) = self.companion_sender(conn, rdev, kind) {
            return vec![Out::Refused { conn, code, rdev }];
        }
        let mut out = Vec::new();
        if let Some(rec) = self.records.get_mut(&rdev) {
            let j = &mut rec.journal;
            if let Some(old) = j.own.remove(&conn) {
                out.push(Out::CloseCompanion(old.id));
            }
            if j.reader == Some(conn) {
                j.settled = true;
                j.attempts = 0;
                j.drop_retained(&mut out);
            }
        }
        out
    }

    /// RELEASE: the tab or pane closed (or a recovering window refused the offer).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "CloseTabA",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "CloseTabB",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "OutgoingReleasesA",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "SuccessorClosesTabA",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperCompanionCustody",
            action = "Release",
            project = "aterm_keeper::KeeperCore::companion_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperCompanionCustody",
            action = "OutgoingEnds",
            project = "aterm_keeper::KeeperCore::companion_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperCompanionCustody",
            action = "ReaderDies",
            project = "aterm_keeper::KeeperCore::companion_view"
        )
    )]
    pub fn release(&mut self, conn: ConnId, rdev: Rdev) -> Vec<Out> {
        let peers = &self.peers;
        let me = peers.get(&conn).map(|p| (p.pid, p.birth));
        let mine = |c: &ConnId| {
            *c == conn
                || (*c < conn
                    && me.is_some_and(|me| peers.get(c).is_some_and(|p| (p.pid, p.birth) == me)))
        };
        // A master the keeper holds no record of is told apart from one it keeps for another
        // claim: a window retiring an old journal reads NoSuchRecord as that journal gone with its
        // record, and NotClaimant as one the keeper may still offer.
        let Some(rec) = self.records.get(&rdev) else {
            return vec![Out::Refused {
                conn,
                code: RefuseCode::NoSuchRecord,
                rdev,
            }];
        };
        if !(rec.claimants.iter().any(mine) || rec.state == RecordState::Offered { to: conn }) {
            return vec![Out::Refused {
                conn,
                code: RefuseCode::NotClaimant,
                rdev,
            }];
        }
        let others_hold = self
            .records
            .get(&rdev)
            .is_some_and(|rec| rec.claimants.iter().any(|c| !mine(c)));
        if others_hold {
            let mut out = Vec::new();
            if let Some(rec) = self.records.get_mut(&rdev) {
                let ended: Vec<ConnId> = rec.claimants.iter().copied().filter(mine).collect();
                rec.claimants.retain(|c| !mine(c));
                for c in ended {
                    rec.journal.claim_left(c, me.map(|(pid, _)| pid), &mut out);
                }
            }
            return out;
        }
        self.records.remove(&rdev);
        vec![Out::Close(rdev)]
    }

    /// BYE: quit intent, keeping nothing.
    pub fn bye(&mut self, conn: ConnId) {
        self.bye_keeping(conn, &[]);
    }

    /// BYE: quit intent, naming the records to `keep` past the quit (`Frame::Bye`'s keep set,
    /// read from a window this keeper's WELCOME told it keeps, [`crate::wire::CAP_KEEPS`]). The
    /// process's end is judged as a quit still ([`Self::peer_died`]): every master it claimed
    /// closes, save each kept one whose shell lives, which is orphaned for the next window's HELLO
    /// to be offered, with no relaunch and no notice, since nobody's window went down.
    pub fn bye_keeping(&mut self, conn: ConnId, keep: &[Rdev]) {
        let Some(who) = self.peers.get(&conn).map(|p| (p.pid, p.birth)) else {
            return;
        };
        for p in self.peers.values_mut() {
            if (p.pid, p.birth) == who {
                p.bye = true;
                for rdev in keep {
                    if !p.keeps.contains(rdev) {
                        p.keeps.push(*rdev);
                    }
                }
            }
        }
    }

    /// PENDING: the update successor's kernel pid, sent before the grant.
    pub fn pending(&mut self, conn: ConnId, pid: u32, now: u64, birth: Option<Birth>) -> Vec<Out> {
        let (from_pid, from_birth) = match self.peers.get(&conn) {
            Some(p) if p.is_app() => (p.pid, p.birth),
            _ => {
                return vec![Out::Refused {
                    conn,
                    code: RefuseCode::WrongClass,
                    rdev: 0,
                }];
            }
        };
        self.pending.retain(|p| !p.is_from(from_pid, from_birth));
        if self.pending.len() >= MAX_CONNECTIONS {
            let oldest = self.pending.first().copied();
            self.retire_pending(|p, _| Some(*p) == oldest);
        }
        self.pending.push(PendingSuccessor {
            pid,
            birth,
            expires: now.saturating_add(PENDING_TTL_MS),
            from_pid,
            from_birth,
            ended: None,
        });
        vec![Out::WatchPid(pid)]
    }

    fn retire_pending(
        &mut self,
        done: impl Fn(&PendingSuccessor, &BTreeMap<ConnId, Peer>) -> bool,
    ) -> bool {
        let (retired, in_flight): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|p| done(p, &self.peers));
        self.pending = in_flight;
        for p in &retired {
            if !self
                .peers
                .values()
                .any(|q| (q.pid, q.birth) == (p.from_pid, p.from_birth))
            {
                continue;
            }
            self.settled
                .retain(|q| !q.is_from(p.from_pid, p.from_birth));
            if self.settled.len() >= MAX_CONNECTIONS {
                self.settled.remove(0);
            }
            self.settled.push(*p);
        }
        !retired.is_empty()
    }

    /// The connection read EOF.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "LinkDrops",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "RelaunchedLinkDrops",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperCompanionCustody",
            action = "RecipientDrops",
            project = "aterm_keeper::KeeperCore::companion_view"
        )
    )]
    pub fn eof(&mut self, conn: ConnId, now: u64, env: &mut dyn KeeperEnv) -> Vec<Out> {
        let mut out = Vec::new();
        if let Some(p) = self.peers.get_mut(&conn)
            && p.eof_at.is_none()
        {
            p.eof_at = Some(now);
            for rec in self.records.values_mut() {
                if rec.state == (RecordState::Offered { to: conn }) {
                    rec.state = RecordState::Orphaned;
                    p.returned_offers = true;
                    out.push(Out::WatchPid(rec.header.shell_pid));
                    if p.journals() {
                        rec.journal.count_attempt(&mut out);
                    }
                }
            }
        }
        out.extend(self.settle(now, env));
        out
    }

    /// The kernel delivered `pid`'s `wait(2)` status (the exit watch).
    pub fn peer_exit(
        &mut self,
        pid: u32,
        status: i32,
        now: u64,
        env: &mut dyn KeeperEnv,
    ) -> Vec<Out> {
        let awaited = self
            .records
            .values()
            .any(|r| matches!(r.state, RecordState::Held { successor: Some(s), .. } if s == pid));
        if awaited && !self.peers.values().any(|p| p.pid == pid && p.is_app()) {
            self.quiet_ends.insert(pid, status);
        }
        for p in self.peers.values_mut() {
            if p.pid == pid && p.exit.is_none() {
                p.exit = Some((now, status));
            }
        }
        for named in self.pending.iter_mut().chain(self.settled.iter_mut()) {
            if named.pid == pid && named.ended.is_none() {
                named.ended = Some((now, status));
            }
        }
        let mut out = self.settle(now, env);
        if self.pending.is_empty() && self.has_holds() {
            self.last_hold_scan = now;
            out.extend(self.resolve_holds(now, env));
        }
        out.extend(self.prune_dead_orphans(env));
        out
    }

    /// Close every Orphaned record whose shell is no longer the process it registered, hanging up whatever still sits on the slave as closing an exited shell's tab would.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperDeathJudgement",
            action = "KeeperSeesShellExit",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    pub fn prune_dead_orphans(&mut self, env: &mut dyn KeeperEnv) -> Vec<Out> {
        let dead: Vec<Rdev> = self
            .records
            .iter()
            .filter(|(_, r)| r.state == RecordState::Orphaned)
            .filter(|(_, r)| !env.shell_alive(r.header.shell_pid, r.header.shell_birth))
            .map(|(rdev, _)| *rdev)
            .collect();
        let mut out = Vec::new();
        for rdev in dead {
            self.records.remove(&rdev);
            self.ends.pruned += 1;
            out.push(Out::Close(rdev));
        }
        out
    }

    fn has_holds(&self) -> bool {
        self.records
            .values()
            .any(|r| matches!(r.state, RecordState::Held { .. }))
    }

    /// The clock: judge deaths whose grace ran out, resolve holds, and run the relaunch lane.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "Relaunch",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    pub fn tick(&mut self, now: u64, env: &mut dyn KeeperEnv) -> Vec<Out> {
        let mut out = self.settle(now, env);
        if self.retire_pending(|p, _| p.ended.is_none() && now >= p.expires)
            && self.pending.is_empty()
        {
            self.last_hold_scan = now;
            out.extend(self.resolve_holds(now, env));
        }
        if self.pending.is_empty()
            && self.has_holds()
            && now >= self.last_hold_scan.saturating_add(HOLD_RESCAN_MS)
        {
            self.last_hold_scan = now;
            out.extend(self.resolve_holds(now, env));
        }
        out.extend(self.run_relaunch(now));
        out
    }

    fn app_connected(&self) -> bool {
        self.peers.values().any(|p| p.is_app() && p.connected())
    }

    fn run_relaunch(&mut self, now: u64) -> Vec<Out> {
        match self.relaunch {
            RelaunchState::Due { at } => {
                if self.orphan_count() == 0 || self.app_connected() {
                    self.relaunch = RelaunchState::Idle;
                } else if now >= at && self.can_relaunch {
                    self.relaunch = RelaunchState::InFlight {
                        since: now,
                        hello: None,
                    };
                    self.relaunches += 1;
                    self.last_relaunch_at = Some(now);
                    return vec![Out::Relaunch];
                }
            }
            RelaunchState::InFlight { since, hello: None } => {
                if now >= since.saturating_add(HELLO_DEADLINE_MS) {
                    self.relaunch = self.after_decision(now, false);
                }
            }
            RelaunchState::InFlight {
                hello: Some((conn, at)),
                ..
            } => {
                let alive = self.peers.get(&conn).is_some_and(Peer::connected);
                if alive && now >= at.saturating_add(HEALTHY_MS) {
                    self.brake.healthy();
                    self.relaunch = RelaunchState::Idle;
                }
            }
            RelaunchState::Idle | RelaunchState::Held => {}
        }
        Vec::new()
    }

    fn after_decision(&mut self, now: u64, clean: bool) -> RelaunchState {
        if self
            .last_relaunch_at
            .is_none_or(|at| now >= at.saturating_add(HEALTHY_MS))
        {
            self.brake.healthy();
        }
        match self.brake.decide(now, clean) {
            BrakeDecision::Nothing => RelaunchState::Idle,
            BrakeDecision::RelaunchAt(at) => RelaunchState::Due { at },
            BrakeDecision::Hold => RelaunchState::Held,
        }
    }

    fn settle(&mut self, now: u64, env: &mut dyn KeeperEnv) -> Vec<Out> {
        let mut out = Vec::new();
        let ready: Vec<ConnId> = self
            .peers
            .iter()
            .filter(|(_, p)| {
                let complete = match (p.eof_at, p.exit) {
                    (Some(_), Some(_)) => true,
                    (Some(eof), None) => {
                        now >= eof.saturating_add(DEATH_GRACE_MS) && !env.pid_alive(p.pid, p.birth)
                    }
                    (None, Some((at, _))) => now >= at.saturating_add(DEATH_GRACE_MS),
                    (None, None) => false,
                };
                complete && !self.awaits_successor_end(p, now, env)
            })
            .map(|(c, _)| *c)
            .collect();
        for conn in ready {
            out.extend(self.peer_died(conn, now, env));
        }
        if self
            .retire_pending(|p, peers| p.ended.is_some() && !peers.values().any(|q| q.pid == p.pid))
            && self.pending.is_empty()
        {
            out.extend(self.resolve_holds(now, env));
        }
        out
    }

    fn awaits_successor_end(&self, p: &Peer, now: u64, env: &mut dyn KeeperEnv) -> bool {
        let Some((exited_at, status)) = p.exit else {
            return false;
        };
        let last_evidence = p.eof_at.map_or(exited_at, |eof| eof.max(exited_at));
        p.is_app()
            && !p.bye
            && exited_zero(status)
            && now < last_evidence.saturating_add(DEATH_GRACE_MS)
            && self.pending.iter().chain(&self.settled).any(|named| {
                named.named_by(p) && named.ended.is_none() && !env.pid_alive(named.pid, named.birth)
            })
    }

    /// One peer is gone: drop it from every claim; judge the records it was the
    /// last claimant of; return offers made to it to Orphaned.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "WindowCrashes",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "RelaunchedWindowCrashes",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "RelaunchedWindowQuits",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "SuccessorCrashes",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "SuccessorCrashesFirst",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "Commit",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "SuccessorQuits",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "OutgoingDrained",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperCompanionCustody",
            action = "ReaderDies",
            project = "aterm_keeper::KeeperCore::companion_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperCompanionCustody",
            action = "OutgoingEnds",
            project = "aterm_keeper::KeeperCore::companion_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "KeeperCompanionCustody",
            action = "RecipientDrops",
            project = "aterm_keeper::KeeperCore::companion_view"
        )
    )]
    fn peer_died(&mut self, conn: ConnId, now: u64, env: &mut dyn KeeperEnv) -> Vec<Out> {
        let Some(peer) = self.peers.remove(&conn) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut orphaned_any = false;
        let mut returned_offers = peer.returned_offers;
        let mut last: Vec<Rdev> = Vec::new();
        for (rdev, rec) in &mut self.records {
            if rec.state == (RecordState::Offered { to: conn }) {
                rec.state = RecordState::Orphaned;
                returned_offers = true;
                out.push(Out::WatchPid(rec.header.shell_pid));
                if peer.journals() {
                    rec.journal.count_attempt(&mut out);
                }
                continue;
            }
            if let Some(i) = rec.claimants.iter().position(|c| *c == conn) {
                rec.claimants.remove(i);
                if rec.claimants.is_empty() {
                    rec.journal.last_claim_left(conn, &mut out);
                    last.push(*rdev);
                } else {
                    rec.journal.claim_left(conn, Some(peer.pid), &mut out);
                }
            }
        }
        if !peer.is_app() {
            return out;
        }
        let own_pending = self.pending.iter().copied().find(|p| {
            p.named_by(&peer)
                && p.ended.is_none()
                && now < p.expires
                && env.pid_alive(p.pid, p.birth)
        });
        let pending_live = own_pending.is_some();
        let status = peer.exit.map(|(_, s)| s);
        let last_evidence = peer.eof_at.max(peer.exit.map(|(at, _)| at)).unwrap_or(now);
        let fell = self
            .pending
            .iter()
            .chain(&self.settled)
            .filter(|p| !peer.bye && status.is_some_and(exited_zero) && p.named_by(&peer))
            .find_map(|p| match p.ended {
                Some((at, s)) if at.saturating_add(DEATH_GRACE_MS) >= last_evidence => {
                    Some((p.pid, s))
                }
                _ => None,
            });
        let mut held_for_fallen = false;
        let returned_unexplained = returned_offers && !peer.bye;
        let marker = match (&peer.marker, last.is_empty() && !returned_unexplained) {
            (Some(m), false) => env.marker(peer.pid, m),
            _ => MarkerEvidence::Unknown,
        };
        if returned_unexplained
            && classify_death(DeathEvidence {
                successor_holds: false,
                bye: false,
                sends_bye: peer.caps & CAP_SENDS_BYE != 0,
                status,
                marker,
            }) == Verdict::Crash
        {
            orphaned_any = true;
        }
        let outgoing_clean = peer.bye || status.is_some_and(exited_zero);
        let mut verdicts = Vec::new();
        let mut scans =
            (!pending_live && fell.is_none()).then(|| env.holders_of(&last).into_iter());
        for rdev in last {
            let scan = scans
                .as_mut()
                .map(|answers| answers.next().unwrap_or(Holders::Unknown));
            let successor_holds = pending_live
                || fell.is_some()
                || matches!(scan, Some(Holders::Held(_) | Holders::Unknown));
            let verdict = classify_death(DeathEvidence {
                successor_holds,
                bye: peer.bye,
                sends_bye: peer.caps & CAP_SENDS_BYE != 0,
                status,
                marker,
            });
            verdicts.push(verdict);
            self.last_verdict = Some(verdict);
            match verdict {
                Verdict::Handoff => {
                    self.ends.handoff += 1;
                    let successor = match (&scan, fell) {
                        (_, Some((pid, _))) => Some(pid),
                        (None, None) => own_pending.map(|p| p.pid),
                        (Some(Holders::Held(pids)), None) if pids.len() == 1 => Some(pids[0]),
                        (Some(_), None) => None,
                    };
                    if let Some(pid) = successor {
                        match fell {
                            Some((_, ended)) => {
                                self.quiet_ends.insert(pid, ended);
                                held_for_fallen = true;
                            }
                            None => {
                                self.quiet_ends.remove(&pid);
                            }
                        }
                        if scan.is_some() {
                            out.push(Out::WatchPid(pid));
                        }
                    }
                    if let Some(rec) = self.records.get_mut(&rdev) {
                        rec.state = RecordState::Held {
                            since: now,
                            successor,
                            outgoing_clean,
                        };
                    }
                }
                Verdict::Quit | Verdict::CleanExit | Verdict::CleanPreKeeper => {
                    // A record the quit named to keep, its shell alive, outlives it: an orphan
                    // for the next window to be offered, owing no relaunch (the quit stands).
                    let kept = peer.keeps.contains(&rdev)
                        && self.records.get(&rdev).is_some_and(|r| {
                            env.shell_alive(r.header.shell_pid, r.header.shell_birth)
                        });
                    if kept {
                        self.ends.kept += 1;
                        if let Some(rec) = self.records.get_mut(&rdev) {
                            rec.state = RecordState::Orphaned;
                            rec.kept = true;
                            out.push(Out::WatchPid(rec.header.shell_pid));
                        }
                    } else {
                        match verdict {
                            Verdict::Quit => self.ends.quit += 1,
                            Verdict::CleanExit => self.ends.clean_exit += 1,
                            _ => self.ends.clean_pre_keeper += 1,
                        }
                        self.records.remove(&rdev);
                        out.push(Out::Close(rdev));
                    }
                }
                Verdict::Crash => {
                    self.ends.crash += 1;
                    let shell_alive = self
                        .records
                        .get(&rdev)
                        .is_some_and(|r| env.shell_alive(r.header.shell_pid, r.header.shell_birth));
                    if shell_alive {
                        if let Some(rec) = self.records.get_mut(&rdev) {
                            rec.state = RecordState::Orphaned;
                            out.push(Out::WatchPid(rec.header.shell_pid));
                        }
                        orphaned_any = true;
                    } else {
                        self.records.remove(&rdev);
                        out.push(Out::Close(rdev));
                    }
                }
            }
        }
        let quit = peer.bye
            || verdicts.iter().all(|v| {
                matches!(
                    v,
                    Verdict::Quit | Verdict::CleanExit | Verdict::CleanPreKeeper
                )
            });
        let was_relaunched = matches!(
            self.relaunch,
            RelaunchState::InFlight { hello: Some((c, _)), .. } if c == conn
        );
        if orphaned_any || was_relaunched {
            if quit && !orphaned_any {
                self.relaunch = RelaunchState::Idle;
            } else if self.orphan_count() > 0 {
                match self.relaunch {
                    RelaunchState::Held => {}
                    RelaunchState::Due { .. } => {}
                    _ => self.relaunch = self.after_decision(now, false),
                }
            } else {
                self.relaunch = RelaunchState::Idle;
            }
        }
        if orphaned_any {
            let n = u16::try_from(self.orphan_count()).unwrap_or(u16::MAX);
            for (c, p) in &self.peers {
                if p.is_app() && p.connected() {
                    out.push(Out::Orphans { conn: *c, n });
                }
            }
        }
        if held_for_fallen && self.pending.is_empty() {
            self.last_hold_scan = now;
            out.extend(self.resolve_holds(now, env));
        }
        if !self
            .peers
            .values()
            .any(|q| (q.pid, q.birth) == (peer.pid, peer.birth))
        {
            self.settled.retain(|p| !p.is_from(peer.pid, peer.birth));
        }
        out
    }

    /// A PENDING ended (its successor died, or it expired), or a held master's holder let go:
    /// every Held record no live process holds is an orphan now (F11: a crash mid-update), unless
    /// the successor it waited on never took part and quit (§5.4 row 3): then it is closed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "SuccessorFailStops",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "PtyKeeperCustody",
            action = "SuccessorQuits",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    fn resolve_holds(&mut self, now: u64, env: &mut dyn KeeperEnv) -> Vec<Out> {
        let held: Vec<Rdev> = self
            .records
            .iter()
            .filter(|(_, r)| matches!(r.state, RecordState::Held { .. }))
            .map(|(k, _)| *k)
            .collect();
        let mut out = Vec::new();
        let mut orphaned = false;
        let scans = env.holders_of(&held);
        for (rdev, holders) in held.into_iter().zip(scans) {
            // The awaited process ended while another still holds the master: it handed it on
            // (its own update's Commit `_exit(0)`s), so its end judges nothing. The hold waits
            // on the holder the scan found instead, and keeps how the awaited process ended
            // beside the outgoing window's end: a crash anywhere in the chain still orphans.
            if let Holders::Held(pids) = &holders
                && let Some(rec) = self.records.get_mut(&rdev)
                && let RecordState::Held {
                    since,
                    successor: Some(awaited),
                    outgoing_clean,
                } = rec.state
                && let Some(&status) = self.quiet_ends.get(&awaited)
            {
                let next = match pids.as_slice() {
                    [only] => Some(*only),
                    _ => None,
                };
                rec.state = RecordState::Held {
                    since,
                    successor: next,
                    outgoing_clean: outgoing_clean && exited_zero(status),
                };
                if let Some(pid) = next {
                    self.quiet_ends.remove(&pid);
                    out.push(Out::WatchPid(pid));
                }
            }
            if holders != Holders::None {
                continue;
            }
            let Some(rec) = self.records.get(&rdev) else {
                continue;
            };
            let quit = match rec.state {
                RecordState::Held {
                    successor: Some(pid),
                    outgoing_clean: true,
                    ..
                } => self.quiet_ends.get(&pid).is_some_and(|s| exited_zero(*s)),
                _ => false,
            };
            let alive = !quit && env.shell_alive(rec.header.shell_pid, rec.header.shell_birth);
            if quit {
                self.ends.clean_pre_keeper += 1;
            } else {
                self.ends.crash += 1;
            }
            if alive {
                if let Some(rec) = self.records.get_mut(&rdev) {
                    let judged = matches!(
                        rec.state,
                        RecordState::Held { successor: Some(pid), .. }
                            if rec.journal.reader_gone == Some(pid)
                    );
                    if !judged {
                        rec.journal.close_all(&mut out);
                    }
                    rec.state = RecordState::Orphaned;
                    out.push(Out::WatchPid(rec.header.shell_pid));
                }
                orphaned = true;
            } else {
                self.records.remove(&rdev);
                out.push(Out::Close(rdev));
            }
        }
        let records = &self.records;
        self.quiet_ends.retain(|pid, _| {
            records.values().any(
                |r| matches!(r.state, RecordState::Held { successor: Some(s), .. } if s == *pid),
            )
        });
        if orphaned
            && !matches!(
                self.relaunch,
                RelaunchState::Held | RelaunchState::Due { .. }
            )
        {
            self.relaunch = self.after_decision(now, false);
        }
        out
    }

    /// A connection that never said HELLO, or a status client, left: forget it (a window's end is judged by [`Self::eof`] and [`Self::peer_exit`]).
    pub fn forget_if_passive(&mut self, conn: ConnId) {
        if self.peers.get(&conn).is_some_and(|p| !p.is_app()) {
            self.peers.remove(&conn);
        }
    }

    /// The status report: `key=value` lines, masters by rdev and state, never a tag's or a META's contents.
    #[must_use]
    pub fn status_lines(&self) -> Vec<String> {
        let count = |pred: &dyn Fn(&RecordState) -> bool| {
            self.records.values().filter(|r| pred(&r.state)).count()
        };
        let mut lines = vec![
            format!("masters={}", self.records.len()),
            format!(
                "claimed={} held={} orphaned={} offered={}",
                count(&|s| *s == RecordState::Claimed),
                count(&|s| matches!(s, RecordState::Held { .. })),
                count(&|s| *s == RecordState::Orphaned),
                count(&|s| matches!(s, RecordState::Offered { .. })),
            ),
            format!(
                "windows={}",
                self.peers
                    .values()
                    .filter(|p| p.is_app() && p.connected())
                    .count()
            ),
            format!(
                "relaunch={} state={} relaunches={}",
                if self.can_relaunch {
                    "available"
                } else {
                    "unavailable"
                },
                match self.relaunch {
                    RelaunchState::Idle => "idle",
                    RelaunchState::Due { .. } => "due",
                    RelaunchState::InFlight { hello: None, .. } => "launching",
                    RelaunchState::InFlight { .. } => "relaunched",
                    RelaunchState::Held => "held",
                },
                self.relaunches
            ),
            format!(
                "brake streak={}/{} held={}",
                self.brake.streak(),
                MAX_STREAK,
                u8::from(self.brake.held())
            ),
            format!(
                "ends quit={} clean_exit={} clean_pre_keeper={} crash={} handoff={} pruned={} kept={}",
                self.ends.quit,
                self.ends.clean_exit,
                self.ends.clean_pre_keeper,
                self.ends.crash,
                self.ends.handoff,
                self.ends.pruned,
                self.ends.kept
            ),
            format!(
                "window_markers={}",
                self.peers
                    .values()
                    .filter(|p| p.is_app() && p.connected() && p.marker.is_some())
                    .count()
            ),
        ];
        for (rdev, rec) in &self.records {
            lines.push(format!(
                "master rdev={},{} state={} local_id={} shell={} claimants={}",
                (rdev >> 24) & 0xff,
                rdev & 0xff_ffff,
                match rec.state {
                    RecordState::Claimed => "claimed",
                    RecordState::Held { .. } => "held",
                    RecordState::Orphaned => "orphaned",
                    RecordState::Offered { .. } => "offered",
                },
                rec.header.local_id,
                rec.header.shell_pid,
                rec.claimants.len()
            ));
        }
        lines
    }
}

#[cfg(test)]
#[path = "core_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "core_late_hello_tests.rs"]
mod late_hello_tests;

#[cfg(test)]
#[path = "core_companion_tests.rs"]
mod companion_tests;
