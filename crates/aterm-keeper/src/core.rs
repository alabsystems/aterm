// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PURE KEEPER: the custody table, the §5.4 death classifier, offer
//! exclusivity and the relaunch brake — every decision the keeper makes, and
//! nothing that touches the operating system.
//!
//! The core owns no descriptor. It tells its caller what to do with the ones
//! the caller holds ([`Out::Keep`], [`Out::Close`], [`Out::Offer`]), and it asks
//! the world three questions through [`KeeperEnv`]: who holds a master (the
//! holder scan), whether a shell is still the process it registered, and
//! whether a pid is still the process that connected. Time is a caller-supplied
//! millisecond clock. That is what lets the Tier-1 bind
//! (`src/conformance_custody.rs`) drive the REAL decisions through every
//! bounded event sequence against `PtyKeeperCustody` and `KeeperRelaunchBrake`
//! (`aterm_spec::derive`), and the server (`crate::server`) run the same code
//! over real sockets and a real kernel.
//!
//! A record's life (per master, keyed by its `st_rdev`):
//!
//! ```text
//!   REGISTER ─▶ Claimed{claimants} ──last claimant dies──▶ classify (§5.4)
//!                  ▲    │ RELEASE ─▶ closed                  │
//!                  │    │                     handoff ◀──────┤ a live holder / PENDING
//!                  │    │                     quit, clean ◀──┤ ─▶ closed
//!                  │    │                     crash ◀────────┘ ─▶ Orphaned
//!   REGISTER ◀── Offered{to} ◀── HELLO/ADOPT, holder scan empty, shell alive
//! ```
//!
//! The keeper KEEPS its copy through an offer (duplicate, never move: F9). An
//! offer whose recipient dies before it registers goes back to Orphaned.

use std::collections::BTreeMap;

use crate::wire::{Birth, CAP_LINK, CAP_SENDS_BYE, MarkerRef, MasterHeader, PeerClass, RefuseCode};

/// A master's identity: its `st_rdev`.
pub type Rdev = u64;
/// A connection, numbered by the caller.
pub type ConnId = u64;

/// Hard caps (§5.7): masters held, connections, claimants per master.
pub const MAX_MASTERS: usize = 1024;
/// See [`MAX_MASTERS`].
pub const MAX_CONNECTIONS: usize = 16;
/// See [`MAX_MASTERS`]. Two: the window and its update successor.
pub const MAX_CLAIMANTS: usize = 2;
/// How long a peer's death waits for its missing half of the evidence (the
/// EOF, or the kernel's exit status) before it is judged on what arrived.
pub const DEATH_GRACE_MS: u64 = 1_000;
/// How long a PENDING names its successor without that successor registering.
pub const PENDING_TTL_MS: u64 = 30_000;
/// A relaunch whose window has not said HELLO by then has failed (§5.3 step 6;
/// `open`'s exit status is no evidence, §11 M8).
pub const HELLO_DEADLINE_MS: u64 = 15_000;
/// A relaunched window alive this long resets the brake's streak.
pub const HEALTHY_MS: u64 = 30 * 60_000;
/// How long after the keeper runs `open` an App HELLO is still taken as that
/// relaunch answering, even past [`HELLO_DEADLINE_MS`]: a build that boots
/// slower than the deadline is a failed relaunch at the deadline AND the
/// keeper's own launch when it finally says HELLO — never a manual launch
/// that resets the brake (which would let a slow-booting crash loop relaunch
/// forever). §6.2's five-minute "died early" window.
pub const LATE_HELLO_MS: u64 = 5 * 60_000;
/// How often a record held for a handoff is re-scanned with no PENDING alive:
/// the successor that holds it may die, or may never register.
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
    /// The scan could not decide (a table it needed was unreadable, or the scan
    /// itself failed): treated as held, never as free.
    Unknown,
}

/// The three questions the core asks of the world. The server answers them from
/// the kernel; the Tier-1 runner answers them from the model's state.
pub trait KeeperEnv {
    /// Who, other than the keeper, holds a descriptor for `rdev`.
    fn holders(&mut self, rdev: Rdev) -> Holders;
    /// Whether `pid` is still the shell born at `birth`.
    fn shell_alive(&mut self, pid: u32, birth: Birth) -> bool;
    /// Whether `pid` is still the process born at `birth` (or, with no birth
    /// record, whether it names any live process of ours).
    fn pid_alive(&mut self, pid: u32, birth: Option<Birth>) -> bool;
    /// What the crash marker `marker` names for the window `pid` says now
    /// (§5.4 row 3's cross-check). A world that cannot read markers answers
    /// [`MarkerEvidence::Unknown`]: no evidence, and the classifier then judges
    /// as it did before the cross-check.
    fn marker(&mut self, pid: u32, marker: &MarkerRef) -> MarkerEvidence {
        let _ = (pid, marker);
        MarkerEvidence::Unknown
    }
}

/// What a window's crash marker says (`aterm-gui`'s `crash_signal`: created
/// empty at every start, `flock`ed by its owner for the owner's whole life,
/// unlinked by the owner's exit path — `exit(3)`'s `atexit` handler and
/// `clean_exit_now` — and written only by the fatal-signal handler).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerEvidence {
    /// No marker named, or none that could be read: no evidence.
    Unknown,
    /// The marker is gone: its owner's exit path ran and unlinked it (or
    /// another start's sweep removed a dead one — which is why a Released
    /// marker alone never makes a quit: the wait status must say `exit(0)`
    /// too).
    Released,
    /// The marker is still there and its lock is free: its owner died without
    /// running its exit path (a signal, SIGKILL, jetsam). Also a consumed
    /// `.seen` marker: another start already read it as such a death.
    Dead,
    /// The marker is still there and someone holds its lock: its owner, or a
    /// forked child that inherited the descriptor. No evidence of an end.
    Held,
}

/// A decision the caller carries out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Out {
    /// Keep the descriptor that arrived with this REGISTER as the custody copy.
    Keep(Rdev),
    /// Close the descriptor that arrived with this REGISTER: the keeper already
    /// holds a copy of that master (a claim that moved).
    DropDuplicate(Rdev),
    /// Close the custody copy of this master: its record is gone.
    Close(Rdev),
    /// Send WELCOME with this many OFFERs to follow.
    Welcome { conn: ConnId, offers: u16 },
    /// Send an OFFER of this master, with a duplicate of the custody copy.
    Offer { conn: ConnId, rdev: Rdev },
    /// Send ORPHANS(n): a live sibling may ADOPT.
    Orphans { conn: ConnId, n: u16 },
    /// Send REFUSED.
    Refused {
        conn: ConnId,
        code: RefuseCode,
        rdev: Rdev,
    },
    /// Start watching this pid's exit (a PENDING successor).
    WatchPid(u32),
    /// Relaunch the app (`/usr/bin/open -a <own bundle>`).
    Relaunch,
}

/// The §5.4 verdict on one dead peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A live successor still holds the masters, or an unexpired PENDING names
    /// a live pid: hold, and wait for it to register or die.
    Handoff,
    /// BYE read before EOF: close its masters now.
    Quit,
    /// No BYE arrived, but the window ended with `exit(0)` AND its crash
    /// marker is gone — its exit path ran and removed it: a quit whose BYE was
    /// lost (the link was reconnecting, the write did not land). Close, as a
    /// quit: a clean quit is never read as a crash.
    CleanExit,
    /// A peer that never spoke BYE-capable AKP1 ended with `exit(0)`: a
    /// pre-keeper build reached by rollback. Close, as today.
    CleanPreKeeper,
    /// Anything else: a signal, a non-zero exit, a lost BYE. Orphan and
    /// relaunch under the brake.
    Crash,
}

/// The evidence the classifier reads, in its table's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeathEvidence {
    /// A live successor holds a master this peer claimed, or an unexpired
    /// PENDING names a live pid.
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
pub fn exited_zero(status: i32) -> bool {
    // WIFEXITED: the low seven bits are zero; WEXITSTATUS: bits 8..16.
    status & 0x7f == 0 && (status >> 8) & 0xff == 0
}

/// THE CLASSIFIER (§5.4), evidence read in the table's order. Unknown evidence
/// resolves toward today's behaviour, except for a keeper-aware peer that died
/// without BYE — the case the keeper exists for.
///
/// ROW 3's CROSS-CHECK, the crash marker. A quit and a crash are told apart by
/// two independent witnesses that must AGREE before a missing BYE is read as a
/// quit: the kernel's wait status says `exit(0)`, and the window's own crash
/// marker is gone (its exit path unlinked it). Either alone is not enough:
/// an unwinding panic exits 101 through the same `atexit` that removes the
/// marker, and another start's sweep can remove a SIGKILLed window's dead
/// marker; a bare `_exit(0)` that skipped the exit path leaves the marker —
/// a crash is never read as a quit. A pre-keeper peer (no BYE capability)
/// that ended with `exit(0)` is closed as before, unless its marker says it
/// never ran its exit path. `KeeperDeathJudgement` (`aterm_spec::derive`) is
/// the machine; its mutants are this function without the marker (a lost BYE
/// resurrects a clean quit) and with the marker trusted alone (a panic read
/// as a quit).
#[must_use]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "PtyKeeperCustody",
        action = "KeeperHonoursBye",
        project = "aterm_keeper::KeeperCore::custody_view"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
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

/// THE RELAUNCH BRAKE, a writer/reader pair (§6.2): [`RelaunchBrake::decide`]
/// READS the streak to choose relaunch-or-hold and WRITES the decision back —
/// a relaunch counts toward the streak, a spent streak is recorded as a HOLD in
/// its own field, never as a deadline sentinel a later read could take as
/// "already elapsed" (the `retry_after = 0` crash loop, as a relaunch storm).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelaunchBrake {
    streak: u32,
    held: bool,
}

impl RelaunchBrake {
    /// A fresh brake: nothing spent. (The brake machine's environment actions
    /// are waived here: no brake code runs for them.)
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "KeeperRelaunchBrake",
            action = "PickRepeatedDeaths",
            reason = "The environment's pick of a scenario; no brake code runs."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "KeeperRelaunchBrake",
            action = "PickBootTrial",
            reason = "As PickRepeatedDeaths."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "KeeperRelaunchBrake",
            action = "Death",
            reason = "A window's non-clean end; the brake is consulted at Decide (KeeperCore's \
                      peer_died and resolve_holds call RelaunchBrake::decide)."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "KeeperRelaunchBrake",
            action = "CleanQuit",
            reason = "A window's quit; decided at Decide with clean = true."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
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
    pub fn streak(&self) -> u32 {
        self.streak
    }

    /// Whether the brake is holding (only a manual launch lifts it).
    #[must_use]
    pub fn held(&self) -> bool {
        self.held
    }

    /// Decide at a death: a quit (`clean`) relaunches nothing; otherwise
    /// relaunch after the streak's backoff while it has room, else hold.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
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

    /// The relaunched window lived [`HEALTHY_MS`]: the streak resets.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "KeeperRelaunchBrake",
            action = "Healthy",
            project = "aterm_keeper::RelaunchBrake::streak"
        )
    )]
    pub fn healthy(&mut self) {
        self.streak = 0;
    }

    /// A launch the keeper did not cause said HELLO: the brake resets.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
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
    Held { since: u64 },
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
    pub claimants: Vec<ConnId>,
    pub state: RecordState,
}

/// One connection's peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub pid: u32,
    pub birth: Option<Birth>,
    pub class: Option<PeerClass>,
    pub caps: u8,
    pub bye: bool,
    /// When EOF was read.
    pub eof_at: Option<u64>,
    /// When the exit status arrived, and what it was.
    pub exit: Option<(u64, i32)>,
    /// When HELLO arrived.
    pub hello_at: Option<u64>,
    /// This window is one the keeper relaunched.
    pub relaunched: bool,
    /// The window's crash marker, once HELLO named one the keeper found held
    /// by a live owner ([`KeeperCore::name_marker`]).
    pub marker: Option<MarkerRef>,
    /// Offers made to this connection went back to Orphaned when its EOF was
    /// read ([`KeeperCore::eof`]): its death, when judged, owes those orphans
    /// a window unless the process quit.
    pub returned_offers: bool,
}

impl Peer {
    fn is_app(&self) -> bool {
        self.class == Some(PeerClass::App)
    }
    fn connected(&self) -> bool {
        self.eof_at.is_none() && self.exit.is_none()
    }
}

/// A PENDING: the update successor a window named before its grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingSuccessor {
    pub pid: u32,
    pub birth: Option<Birth>,
    pub expires: u64,
    pub from: ConnId,
    pub dead: bool,
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
    /// Records closed on `exit(0)` with the crash marker gone (a lost BYE).
    pub clean_exit: u64,
    /// Records closed on a pre-keeper peer's `exit(0)`.
    pub clean_pre_keeper: u64,
    /// Records a crash left (orphaned, or closed because the shell was gone).
    pub crash: u64,
    /// Records held for a handoff.
    pub handoff: u64,
    /// Orphans closed because their shell died (never offered, never listed).
    pub pruned: u64,
}

/// The keeper's side of the custody model for two named masters (the Tier-1
/// projection): whether it holds each, and whether each is an orphan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CustodyView {
    pub holds_a: bool,
    pub holds_b: bool,
    pub orphan_a: bool,
    pub orphan_b: bool,
}

/// THE KEEPER. See the module docs.
#[derive(Clone, Debug)]
pub struct KeeperCore {
    records: BTreeMap<Rdev, Record>,
    peers: BTreeMap<ConnId, Peer>,
    pending: Option<PendingSuccessor>,
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
}

impl KeeperCore {
    /// A keeper with an empty table. `can_relaunch`: whether a relaunch has a
    /// bundle to open; without one the keeper holds orphans for the next
    /// manual launch and never relaunches.
    ///
    /// The custody machine's actions that are not the keeper's own code — the
    /// window's, the update's, launchd's — are waived here, each with where its
    /// keeper half is.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "PtyKeeperCustody",
            action = "Quit",
            reason = "The window writes BYE on its final-exit path (aterm-gui's keeper_link::bye, beside \
                      lib.rs's exit(0)); the keeper only records it (KeeperCore::bye), which this \
                      runner drives, and judges the end at the EOF and exit (KeeperHonoursBye)."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "PtyKeeperCustody",
            action = "StartSuccessor",
            reason = "The seamless update's own step (app_update_handoff); the keeper's half is \
                      KeeperCore::pending, driven here as the PENDING frame."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "PtyKeeperCustody",
            action = "Rollback",
            reason = "The window proves its successor dead (app_update_handoff); the keeper sees \
                      only the successor's exit status, driven here through KeeperCore::peer_exit."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "PtyKeeperCustody",
            action = "AdoptA",
            reason = "The recovering window's admission and reader (aterm-gui's \
                      keeper_link::admit_recovered_master); \
                      the keeper sees its REGISTER, driven here."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "PtyKeeperCustody",
            action = "AdoptB",
            reason = "As AdoptA."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "PtyKeeperCustody",
            action = "KeeperCrashes",
            reason = "The keeper process dies; the kernel closes every custody copy. The runner \
                      drops the core."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
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
            pending: None,
            brake: RelaunchBrake::new(),
            relaunch: RelaunchState::Idle,
            last_relaunch_at: None,
            can_relaunch,
            relaunches: 0,
            last_hold_scan: 0,
            ends: EndCounts::default(),
            last_verdict: None,
        }
    }

    /// How the keeper has judged the ends it saw.
    #[must_use]
    pub fn ends(&self) -> EndCounts {
        self.ends
    }

    /// The newest verdict the classifier gave (the `KeeperDeathJudgement`
    /// projection).
    #[must_use]
    pub fn last_verdict(&self) -> Option<Verdict> {
        self.last_verdict
    }

    /// The records, for status and tests.
    #[must_use]
    pub fn records(&self) -> &BTreeMap<Rdev, Record> {
        &self.records
    }

    /// The peers, for status and tests.
    #[must_use]
    pub fn peers(&self) -> &BTreeMap<ConnId, Peer> {
        &self.peers
    }

    /// The brake, for status and tests.
    #[must_use]
    pub fn brake(&self) -> &RelaunchBrake {
        &self.brake
    }

    /// The relaunch lane, for status and tests.
    #[must_use]
    pub fn relaunch_state(&self) -> RelaunchState {
        self.relaunch
    }

    /// How many relaunches this keeper has made.
    #[must_use]
    pub fn relaunches(&self) -> u64 {
        self.relaunches
    }

    /// The pending successor, if any.
    #[must_use]
    pub fn pending_successor(&self) -> Option<PendingSuccessor> {
        self.pending
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

    /// Orphans on offer now.
    #[must_use]
    pub fn orphan_count(&self) -> usize {
        self.records
            .values()
            .filter(|r| r.state == RecordState::Orphaned)
            .count()
    }

    /// A connection was accepted, from a same-uid peer the kernel named.
    /// `false`: refused (the connection cap), and the caller drops it.
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

    /// The window on `conn` named its crash marker in its HELLO. The keeper
    /// keeps it only when the marker is there NOW with its lock held — the
    /// window is alive and holds it — so that its later absence can only mean
    /// the window's exit path removed it. A marker that is not held (a wrong
    /// directory, a start armed without the lock, a stranger's claim) is no
    /// evidence and is dropped: the window is judged as one that named none.
    /// `true` when kept.
    ///
    /// `KeeperDeathJudgement`'s environment — the window's build and end, the
    /// evidence degrading, the shell's own end — is waived here, beside the
    /// keeper's recording of the one piece of it the window names.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "PickP3Window",
            reason = "The environment's pick of a window build (a P3 window names no marker); no keeper code runs."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "PickPreByeWindow",
            reason = "As PickP3Window (a window from before BYE: no CAP_SENDS_BYE, no marker)."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "QuitWithBye",
            reason = "The window's final-exit path (aterm-gui's keeper_link::bye, then exit(0), whose atexit \
                      unlinks the marker); the keeper records the BYE (KeeperCore::bye) and judges at Judge."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "QuitByeLost",
            reason = "As QuitWithBye with the BYE not written; the keeper sees only the EOF and the exit."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "Killed",
            reason = "The kernel ends the window with no exit path (SIGKILL, jetsam, a fatal signal); the \
                      marker stays, its lock freed. The keeper sees the EOF and the exit status."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "PanicExit",
            reason = "An unwinding panic: exit(101) through the atexit that unlinks the marker."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "StatusLost",
            reason = "The exit watch could not be armed (ESRCH at the connect); the keeper judges on the \
                      EOF and a dead pid after DEATH_GRACE_MS, with no status."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "MarkerSwept",
            reason = "Another aterm start's install sweep removes a dead, empty marker (aterm-gui's \
                      crash_signal::markers::sweep)."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
            machine = "KeeperDeathJudgement",
            action = "MarkerInherited",
            reason = "A forked child still holds the marker's descriptor and lock."
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::spec_unmodeled(
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

    /// HELLO. A window's boot HELLO is answered with WELCOME and the offers
    /// (§5.3 step 7); a status client's, and a window's link reconnecting
    /// ([`CAP_LINK`]), with WELCOME alone. A window's HELLO while a
    /// relaunch is in flight is that relaunch answering; any other is a manual
    /// launch, which resets the brake.
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
            // A HELLO that arrives after its relaunch's deadline is still the
            // keeper's own launch: it counts toward the streak (its early
            // death is decided like any relaunched window's) and never resets
            // the brake.
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

    /// THE OFFER RULE: every Orphaned record whose master no live process
    /// holds (the holder scan) and whose shell is still the one registered is
    /// offered to `conn` and marked Offered to it. The keeper KEEPS its copy. A
    /// record whose shell is gone is closed: there is nothing left to keep.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "OfferA",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "OfferB",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    fn offers_for(&mut self, conn: ConnId, env: &mut dyn KeeperEnv) -> Vec<Out> {
        let mut offers = Vec::new();
        let mut closes = Vec::new();
        let orphans: Vec<Rdev> = self
            .records
            .iter()
            .filter(|(_, r)| r.state == RecordState::Orphaned)
            .map(|(rdev, _)| *rdev)
            .collect();
        for rdev in orphans {
            let Some(rec) = self.records.get(&rdev) else {
                continue;
            };
            if !env.shell_alive(rec.header.shell_pid, rec.header.shell_birth) {
                self.records.remove(&rdev);
                closes.push(Out::Close(rdev));
                continue;
            }
            if env.holders(rdev) != Holders::None {
                continue;
            }
            if let Some(rec) = self.records.get_mut(&rdev) {
                rec.state = RecordState::Offered { to: conn };
                offers.push(Out::Offer { conn, rdev });
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

    /// REGISTER: a claim on a master, proved by possession — the descriptor's
    /// `st_rdev` (`fd_rdev`, read by the caller with `fstat`) must be the one
    /// the header names. A first registration keeps the descriptor as the
    /// custody copy; a claim on a master already held moves the claim and drops
    /// the duplicate (the keeper already holds the same open file description).
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "RegisterA",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "RegisterB",
            project = "aterm_keeper::KeeperCore::custody_view"
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
        let pid = match self.peers.get(&conn) {
            Some(p) if p.is_app() && p.connected() => p.pid,
            _ => return refuse(RefuseCode::WrongClass),
        };
        if fd_rdev != rdev {
            return refuse(RefuseCode::RdevMismatch);
        }
        // The successor a PENDING named has registered: the update landed.
        if self.pending.is_some_and(|p| p.pid == pid) {
            self.pending = None;
        }
        // A claimant of the SAME process whose link dropped (EOF read, process
        // alive) is replaced by its reconnected link, never counted twice.
        let stale: Vec<ConnId> = self
            .peers
            .iter()
            .filter(|(c, p)| **c != conn && p.pid == pid && p.eof_at.is_some())
            .map(|(c, _)| *c)
            .collect();
        if let Some(rec) = self.records.get_mut(&rdev) {
            rec.claimants.retain(|c| !stale.contains(c));
            if !rec.claimants.contains(&conn) {
                if rec.claimants.len() >= MAX_CLAIMANTS {
                    return refuse(RefuseCode::TooManyClaimants);
                }
                rec.claimants.push(conn);
            }
            rec.state = RecordState::Claimed;
            rec.header = header;
            if !tag.is_empty() {
                rec.tag = tag;
            }
            return vec![Out::DropDuplicate(rdev)];
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
            },
        );
        vec![Out::Keep(rdev)]
    }

    /// META: the newest Repaint-rung scalar state of a master this connection
    /// claims. Replaces the previous one; never decoded.
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

    /// RELEASE: the tab or pane closed (or a recovering window refused the
    /// offer). The record goes and the custody copy closes. Only a claimant,
    /// or the connection it was offered to, may release it.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "CloseTabA",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "CloseTabB",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "OutgoingReleasesA",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "SuccessorClosesTabA",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    pub fn release(&mut self, conn: ConnId, rdev: Rdev) -> Vec<Out> {
        let allowed = self.records.get(&rdev).is_some_and(|rec| {
            rec.claimants.contains(&conn) || rec.state == RecordState::Offered { to: conn }
        });
        if !allowed {
            return vec![Out::Refused {
                conn,
                code: RefuseCode::NotClaimant,
                rdev,
            }];
        }
        // TWO CLAIMANTS (an update's outgoing window and its successor, both
        // registered between the successor's Commit and the outgoing window's
        // `_exit`): a RELEASE from one ends only ITS claim while another
        // PROCESS still claims the master. The outgoing window's late
        // `Session::drop` must not close the copy the successor's shell lives
        // on — the successor crashing next would lose it with no keeper fault
        // (`PtyKeeperCustody`'s `OutgoingReleasesA`, whose mutant this was in
        // P2). A successor that closes the tab while the outgoing window still
        // claims it leaves the record to that claimant's end, which the
        // classifier judges with the shell already hung up (F3): closed.
        // Claimants of the SAME process (a link that dropped and reconnected)
        // are one claimant.
        let pid_of = |c: &ConnId| self.peers.get(c).map(|p| p.pid);
        let releaser = pid_of(&conn);
        let others_hold = self.records.get(&rdev).is_some_and(|rec| {
            rec.claimants
                .iter()
                .any(|c| *c != conn && (releaser.is_none() || pid_of(c) != releaser))
        });
        if others_hold {
            if let Some(rec) = self.records.get_mut(&rdev) {
                rec.claimants
                    .retain(|c| *c != conn && (releaser.is_none() || pid_of(c) != releaser));
            }
            return Vec::new();
        }
        self.records.remove(&rdev);
        vec![Out::Close(rdev)]
    }

    /// BYE: quit intent. The peer's end is judged when its EOF and exit arrive.
    ///
    /// The intent is the PROCESS's, whichever of its links carried it: a
    /// window whose link dropped and reconnected (the same pid and birth) quit
    /// on every one of them, so the dead link's end, judged at the same exit,
    /// never reads as a crash that brings the window back (round-seven update
    /// audit, finding 65; `PtyKeeperCustody`'s `RelaunchedWindowQuits`, whose
    /// mutant marks only the link that carried it).
    pub fn bye(&mut self, conn: ConnId) {
        let Some(who) = self.peers.get(&conn).map(|p| (p.pid, p.birth)) else {
            return;
        };
        for p in self.peers.values_mut() {
            if (p.pid, p.birth) == who {
                p.bye = true;
            }
        }
    }

    /// PENDING: the update successor's kernel pid, sent before the grant. The
    /// caller watches its exit ([`Out::WatchPid`]).
    pub fn pending(&mut self, conn: ConnId, pid: u32, now: u64, birth: Option<Birth>) -> Vec<Out> {
        match self.peers.get(&conn) {
            Some(p) if p.is_app() => {}
            _ => {
                return vec![Out::Refused {
                    conn,
                    code: RefuseCode::WrongClass,
                    rdev: 0,
                }];
            }
        }
        self.pending = Some(PendingSuccessor {
            pid,
            birth,
            expires: now.saturating_add(PENDING_TTL_MS),
            from: conn,
            dead: false,
        });
        vec![Out::WatchPid(pid)]
    }

    /// The connection read EOF. Alone it decides nothing about a CLAIM: the
    /// peer's death is judged when the kernel's exit status arrives too (a
    /// registered window's link that dropped while it lives is `LinkDrops`,
    /// whose mutant orphans here).
    ///
    /// An OFFER, though, was made to the CONNECTION, and nothing registers on
    /// a connection that is gone: every record offered to `conn` is an orphan
    /// again now, its leader watched again (round-seven update audit, finding
    /// 65; `RelaunchedLinkDrops`, whose mutant leaves the offer with the dead
    /// link). Left Offered to a dead connection while its window lives, no
    /// ADOPT and no later launch could be offered it, and the window's
    /// eventual end read it back as a fresh orphan. A window that took the
    /// descriptor before its link dropped still holds it, so the holder scan
    /// keeps a second offer away ([`Self::offers_for`]), and its reconnected
    /// link may REGISTER it ([`Self::register`] proves possession, not the
    /// offer).
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "LinkDrops",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "RelaunchedLinkDrops",
            project = "aterm_keeper::KeeperCore::custody_view"
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
                }
            }
        }
        out.extend(self.settle(now, env));
        out
    }

    /// The kernel delivered `pid`'s `wait(2)` status (the exit watch). The
    /// caller has read every frame `pid` wrote before it (the server's
    /// `drain_pid`): a claim read after this is refused ([`Self::register`]).
    pub fn peer_exit(
        &mut self,
        pid: u32,
        status: i32,
        now: u64,
        env: &mut dyn KeeperEnv,
    ) -> Vec<Out> {
        for p in self.peers.values_mut() {
            if p.pid == pid && p.exit.is_none() {
                p.exit = Some((now, status));
            }
        }
        if let Some(pending) = self.pending.as_mut()
            && pending.pid == pid
        {
            pending.dead = true;
        }
        let mut out = self.settle(now, env);
        // Any exit may be the end of the process a held record waited on (a
        // successor whose PENDING already expired, or was never received).
        if self.pending.is_none() && self.has_holds() {
            self.last_hold_scan = now;
            out.extend(self.resolve_holds(now, env));
        }
        // …and of an orphan's shell (the keeper watches each orphan's session
        // leader: [`Out::WatchPid`] when it is orphaned).
        out.extend(self.prune_dead_orphans(env));
        out
    }

    /// STALE ORPHANS: every Orphaned record whose shell is no longer the
    /// process it registered is closed now — there is nothing left to keep,
    /// the copy's close hangs up whatever else still sits on the slave, as the
    /// window closing an exited shell's tab would, and `aterm keeper status`
    /// never lists it nor an offer carry it. Run on every exit the keeper sees
    /// (it watches each orphan's leader) and when a watch could not be armed
    /// because the leader was already gone.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
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

    /// The clock: judge deaths whose grace ran out, resolve holds, and run the
    /// relaunch lane.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "Relaunch",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    pub fn tick(&mut self, now: u64, env: &mut dyn KeeperEnv) -> Vec<Out> {
        let mut out = self.settle(now, env);
        if let Some(p) = self.pending
            && !p.dead
            && now >= p.expires
        {
            self.pending = None;
            self.last_hold_scan = now;
            out.extend(self.resolve_holds(now, env));
        }
        if self.pending.is_none()
            && self.has_holds()
            && now >= self.last_hold_scan.saturating_add(HOLD_RESCAN_MS)
        {
            self.last_hold_scan = now;
            out.extend(self.resolve_holds(now, env));
        }
        out.extend(self.run_relaunch(now));
        out
    }

    /// Whether a window is connected and alive (a relaunch would only race it).
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
                    // No window answered: a failed relaunch.
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

    /// Ask the brake, and turn its answer into the lane's next state.
    fn after_decision(&mut self, now: u64, clean: bool) -> RelaunchState {
        match self.brake.decide(now, clean) {
            BrakeDecision::Nothing => RelaunchState::Idle,
            BrakeDecision::RelaunchAt(at) => RelaunchState::Due { at },
            BrakeDecision::Hold => RelaunchState::Held,
        }
    }

    /// Judge every peer whose death evidence is complete, or whose grace ran
    /// out: a peer is dead when its EOF and its exit status have both arrived,
    /// or one has and [`DEATH_GRACE_MS`] passed without the other while the
    /// process no longer lives. A peer whose link dropped while its process
    /// lives keeps its claims — an EOF alone is never a death (the
    /// `BuggyOfferOnEof` mutant).
    fn settle(&mut self, now: u64, env: &mut dyn KeeperEnv) -> Vec<Out> {
        let mut out = Vec::new();
        let ready: Vec<ConnId> = self
            .peers
            .iter()
            .filter(|(_, p)| match (p.eof_at, p.exit) {
                (Some(_), Some(_)) => true,
                (Some(eof), None) => {
                    now >= eof.saturating_add(DEATH_GRACE_MS) && !env.pid_alive(p.pid, p.birth)
                }
                (None, Some((at, _))) => now >= at.saturating_add(DEATH_GRACE_MS),
                (None, None) => false,
            })
            .map(|(c, _)| *c)
            .collect();
        for conn in ready {
            out.extend(self.peer_died(conn, now, env));
        }
        if self
            .pending
            .is_some_and(|p| p.dead && !self.peers.values().any(|q| q.pid == p.pid))
        {
            // A successor that died before it ever connected (exit 74, F11).
            self.pending = None;
            out.extend(self.resolve_holds(now, env));
        }
        out
    }

    /// One peer is gone: drop it from every claim; judge the records it was the
    /// last claimant of; return offers made to it to Orphaned.
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "WindowCrashes",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "RelaunchedWindowCrashes",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "RelaunchedWindowQuits",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "SuccessorCrashes",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "Commit",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "OutgoingDrained",
            project = "aterm_keeper::KeeperCore::custody_view"
        )
    )]
    fn peer_died(&mut self, conn: ConnId, now: u64, env: &mut dyn KeeperEnv) -> Vec<Out> {
        let Some(peer) = self.peers.remove(&conn) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut orphaned_any = false;
        // Offers returned to Orphaned, here or at this connection's EOF: the
        // window died before it registered them. They owe a window only when
        // it did not quit (a BYE, [`Self::bye`]).
        let mut returned_offers = peer.returned_offers;
        let mut last: Vec<Rdev> = Vec::new();
        for (rdev, rec) in &mut self.records {
            if rec.state == (RecordState::Offered { to: conn }) {
                rec.state = RecordState::Orphaned;
                returned_offers = true;
                out.push(Out::WatchPid(rec.header.shell_pid));
                continue;
            }
            if let Some(i) = rec.claimants.iter().position(|c| *c == conn) {
                rec.claimants.remove(i);
                if rec.claimants.is_empty() {
                    last.push(*rdev);
                }
            }
        }
        if !peer.is_app() {
            return out;
        }
        if returned_offers && !peer.bye {
            orphaned_any = true;
        }
        let pending_live = self.pending.is_some_and(|p| {
            !p.dead && now < p.expires && p.pid != peer.pid && env.pid_alive(p.pid, p.birth)
        });
        let status = peer.exit.map(|(_, s)| s);
        // The marker is read once per death, after it (the process is gone:
        // its exit path, if it ran, has already unlinked the marker).
        let marker = match (&peer.marker, last.is_empty()) {
            (Some(m), false) => env.marker(peer.pid, m),
            _ => MarkerEvidence::Unknown,
        };
        let mut verdicts = Vec::new();
        for rdev in last {
            let successor_holds =
                pending_live || matches!(env.holders(rdev), Holders::Held(_) | Holders::Unknown);
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
                    if let Some(rec) = self.records.get_mut(&rdev) {
                        rec.state = RecordState::Held { since: now };
                    }
                }
                Verdict::Quit | Verdict::CleanExit | Verdict::CleanPreKeeper => {
                    match verdict {
                        Verdict::Quit => self.ends.quit += 1,
                        Verdict::CleanExit => self.ends.clean_exit += 1,
                        _ => self.ends.clean_pre_keeper += 1,
                    }
                    self.records.remove(&rdev);
                    out.push(Out::Close(rdev));
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
                            // Its leader's exit prunes it ([`Self::prune_dead_orphans`]).
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
        // The brake is asked once per death that left orphans (or of the
        // window the keeper relaunched, whose end counts either way); a quit
        // relaunches nothing.
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
        out
    }

    /// A PENDING ended (its successor died, or it expired): every Held record
    /// no live process holds is an orphan now (F11: a crash mid-update).
    #[cfg_attr(
        any(test, feature = "spec-anchors"),
        aterm_spec::refines(
            machine = "PtyKeeperCustody",
            action = "SuccessorFailStops",
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
        for rdev in held {
            if env.holders(rdev) != Holders::None {
                continue;
            }
            let alive = self
                .records
                .get(&rdev)
                .is_some_and(|r| env.shell_alive(r.header.shell_pid, r.header.shell_birth));
            if alive {
                if let Some(rec) = self.records.get_mut(&rdev) {
                    rec.state = RecordState::Orphaned;
                    out.push(Out::WatchPid(rec.header.shell_pid));
                }
                orphaned = true;
            } else {
                self.records.remove(&rdev);
                out.push(Out::Close(rdev));
            }
        }
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

    /// A connection that never said HELLO, or a status client, left: forget it
    /// (a window's end is judged by [`Self::eof`] and [`Self::peer_exit`]).
    pub fn forget_if_passive(&mut self, conn: ConnId) {
        if self.peers.get(&conn).is_some_and(|p| !p.is_app()) {
            self.peers.remove(&conn);
        }
    }

    /// The status report: `key=value` lines, masters by rdev and state, never a
    /// tag's or a META's contents.
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
                "ends quit={} clean_exit={} clean_pre_keeper={} crash={} handoff={} pruned={}",
                self.ends.quit,
                self.ends.clean_exit,
                self.ends.clean_pre_keeper,
                self.ends.crash,
                self.ends.handoff,
                self.ends.pruned
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
mod tests {
    use super::*;

    /// A world the tests describe directly.
    #[derive(Default)]
    struct World {
        held: BTreeMap<Rdev, Vec<u32>>,
        dead_shells: Vec<u32>,
        live_pids: Vec<u32>,
        /// What each window's marker says, by the window's pid.
        markers: BTreeMap<u32, MarkerEvidence>,
    }

    impl KeeperEnv for World {
        fn holders(&mut self, rdev: Rdev) -> Holders {
            match self.held.get(&rdev) {
                Some(p) if !p.is_empty() => Holders::Held(p.clone()),
                _ => Holders::None,
            }
        }
        fn shell_alive(&mut self, pid: u32, _birth: Birth) -> bool {
            !self.dead_shells.contains(&pid)
        }
        fn pid_alive(&mut self, pid: u32, _birth: Option<Birth>) -> bool {
            self.live_pids.contains(&pid)
        }
        fn marker(&mut self, pid: u32, _marker: &MarkerRef) -> MarkerEvidence {
            self.markers
                .get(&pid)
                .copied()
                .unwrap_or(MarkerEvidence::Unknown)
        }
    }

    fn hdr(rdev: Rdev, shell: u32) -> MasterHeader {
        MasterHeader {
            rdev,
            shell_pid: shell,
            shell_birth: Birth::default(),
            local_id: rdev,
        }
    }

    fn window(k: &mut KeeperCore, w: &mut World, conn: ConnId, pid: u32, now: u64) -> Vec<Out> {
        assert!(k.accept(conn, pid, None));
        w.live_pids.push(pid);
        k.hello(conn, PeerClass::App, CAP_SENDS_BYE, now, w)
    }

    #[test]
    fn the_classifier_reads_its_table_in_order() {
        let e = |successor_holds, bye, sends_bye, status| DeathEvidence {
            successor_holds,
            bye,
            sends_bye,
            status,
            marker: MarkerEvidence::Unknown,
        };
        let m = |sends_bye, status, marker| DeathEvidence {
            successor_holds: false,
            bye: false,
            sends_bye,
            status,
            marker,
        };
        // Row 1 beats everything.
        assert_eq!(
            classify_death(e(true, true, true, Some(0))),
            Verdict::Handoff
        );
        assert_eq!(classify_death(e(false, true, true, Some(9))), Verdict::Quit);
        // A pre-keeper build's exit(0), and only exit(0).
        assert_eq!(
            classify_death(e(false, false, false, Some(0))),
            Verdict::CleanPreKeeper
        );
        assert_eq!(
            classify_death(e(false, false, false, Some(0x0100))),
            Verdict::Crash
        );
        assert_eq!(
            classify_death(e(false, false, false, Some(9))),
            Verdict::Crash
        );
        // A keeper-aware peer's exit(0) with no BYE is a lost BYE: a crash.
        assert_eq!(
            classify_death(e(false, false, true, Some(0))),
            Verdict::Crash
        );
        // Unknown status, keeper-aware, no BYE: the case the keeper exists for.
        assert_eq!(classify_death(e(false, false, true, None)), Verdict::Crash);
        // ROW 3's CROSS-CHECK. exit(0) with the marker gone: the exit path ran,
        // the BYE was lost — a quit, for a keeper-aware peer and a pre-keeper
        // one alike.
        use MarkerEvidence::{Dead, Held, Released, Unknown};
        assert_eq!(
            classify_death(m(true, Some(0), Released)),
            Verdict::CleanExit
        );
        assert_eq!(
            classify_death(m(false, Some(0), Released)),
            Verdict::CleanExit
        );
        // Either witness alone is not a quit: a released marker with an
        // unwinding panic's 101, a SIGKILL, or no status; exit(0) with the
        // marker held, unread, or left behind by a death that skipped its exit
        // path.
        for (status, marker) in [
            (Some(101 << 8), Released),
            (Some(9), Released),
            (None, Released),
            (Some(0), Held),
            (Some(0), Unknown),
            (Some(0), Dead),
            (Some(9), Dead),
        ] {
            assert_eq!(
                classify_death(m(true, status, marker)),
                Verdict::Crash,
                "{status:?} {marker:?}"
            );
        }
        // A pre-keeper exit(0) is closed on no marker evidence as before, but
        // never over a marker that says the exit path did not run.
        assert_eq!(
            classify_death(m(false, Some(0), Unknown)),
            Verdict::CleanPreKeeper
        );
        assert_eq!(classify_death(m(false, Some(0), Dead)), Verdict::Crash);
        // BYE and a handoff still come first.
        assert_eq!(
            classify_death(DeathEvidence {
                bye: true,
                ..m(true, Some(9), Dead)
            }),
            Verdict::Quit
        );
        assert!(exited_zero(0));
        assert!(!exited_zero(0x0700));
        assert!(!exited_zero(0x9));
        assert!(!exited_zero(101 << 8));
    }

    #[test]
    fn the_brake_backs_off_then_holds_until_a_manual_launch() {
        let mut b = RelaunchBrake::new();
        assert_eq!(b.decide(100, true), BrakeDecision::Nothing, "a quit");
        assert_eq!(b.decide(100, false), BrakeDecision::RelaunchAt(100));
        assert_eq!(b.decide(200, false), BrakeDecision::RelaunchAt(10_200));
        assert_eq!(b.decide(300, false), BrakeDecision::RelaunchAt(60_300));
        assert_eq!(b.decide(400, false), BrakeDecision::Hold);
        assert!(b.held());
        assert_eq!(b.decide(500, false), BrakeDecision::Hold, "still held");
        b.manual_launch();
        assert_eq!((b.streak(), b.held()), (0, false));
        assert_eq!(b.decide(600, false), BrakeDecision::RelaunchAt(600));
        b.healthy();
        assert_eq!(b.streak(), 0);
    }

    #[test]
    fn a_crash_orphans_and_a_relaunch_offers_keeping_the_copy() {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        assert_eq!(k.register(1, hdr(10, 900), vec![], 10), vec![Out::Keep(10)]);
        assert_eq!(k.register(1, hdr(11, 901), vec![], 11), vec![Out::Keep(11)]);
        // SIGKILL: EOF and the status.
        w.live_pids.retain(|p| *p != 500);
        k.eof(1, 5, &mut w);
        k.peer_exit(500, 9, 6, &mut w);
        assert_eq!(k.orphan_count(), 2);
        assert_eq!(k.tick(7, &mut w), vec![Out::Relaunch]);
        let outs = window(&mut k, &mut w, 2, 600, 8);
        assert_eq!(
            outs,
            vec![
                Out::Welcome { conn: 2, offers: 2 },
                Out::Offer { conn: 2, rdev: 10 },
                Out::Offer { conn: 2, rdev: 11 },
            ]
        );
        assert!(k.records().contains_key(&10), "the keeper keeps its copy");
        // The new window claims both: the duplicates are dropped.
        assert_eq!(
            k.register(2, hdr(10, 900), vec![], 10),
            vec![Out::DropDuplicate(10)]
        );
        assert_eq!(k.records()[&10].state, RecordState::Claimed);
        assert!(k.peers()[&2].relaunched);
    }

    #[test]
    fn an_eof_alone_is_not_a_death() {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        k.register(1, hdr(10, 900), vec![], 10);
        k.eof(1, 5, &mut w);
        assert!(k.tick(5_000, &mut w).is_empty(), "the process lives");
        assert_eq!(k.records()[&10].state, RecordState::Claimed);
        // It dies later: now it is judged.
        w.live_pids.clear();
        k.peer_exit(500, 9, 6_000, &mut w);
        assert_eq!(k.records()[&10].state, RecordState::Orphaned);
    }

    #[test]
    fn a_quit_closes_and_relaunches_nothing() {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        k.register(1, hdr(10, 900), vec![], 10);
        k.bye(1);
        k.eof(1, 5, &mut w);
        let out = k.peer_exit(500, 0, 5, &mut w);
        assert_eq!(out, vec![Out::Close(10)]);
        assert!(k.tick(100_000, &mut w).is_empty());
        assert_eq!(k.relaunches(), 0);
    }

    #[test]
    fn a_pre_keeper_clean_exit_closes() {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        assert!(k.accept(1, 500, None));
        w.live_pids.push(500);
        k.hello(1, PeerClass::App, 0, 0, &mut w);
        k.register(1, hdr(10, 900), vec![], 10);
        k.eof(1, 5, &mut w);
        assert_eq!(k.peer_exit(500, 0, 5, &mut w), vec![Out::Close(10)]);
    }

    #[test]
    fn an_update_is_a_hold_and_a_failed_successor_orphans() {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        k.register(1, hdr(10, 900), vec![], 10);
        w.live_pids.push(700);
        assert_eq!(k.pending(1, 700, 1, None), vec![Out::WatchPid(700)]);
        // The window crashes mid-update: a hold while the successor lives.
        w.live_pids.retain(|p| *p != 500);
        k.eof(1, 2, &mut w);
        k.peer_exit(500, 9, 2, &mut w);
        assert!(matches!(k.records()[&10].state, RecordState::Held { .. }));
        assert!(
            k.tick(3, &mut w).is_empty(),
            "no relaunch over a live successor"
        );
        // The successor fail-stops (exit 74): orphans, and a relaunch.
        w.live_pids.retain(|p| *p != 700);
        k.peer_exit(700, 74 << 8, 4, &mut w);
        assert_eq!(k.records()[&10].state, RecordState::Orphaned);
        assert_eq!(k.tick(5, &mut w), vec![Out::Relaunch]);
    }

    #[test]
    fn a_committed_successor_holding_the_master_is_a_hold() {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        k.register(1, hdr(10, 900), vec![], 10);
        // PENDING lost; the holder scan alone finds the successor holding it.
        w.held.insert(10, vec![700]);
        w.live_pids.retain(|p| *p != 500);
        k.eof(1, 2, &mut w);
        k.peer_exit(500, 0, 2, &mut w);
        assert!(matches!(k.records()[&10].state, RecordState::Held { .. }));
        // Nothing is offered to anyone while it is held.
        let outs = window(&mut k, &mut w, 2, 600, 3);
        assert_eq!(outs, vec![Out::Welcome { conn: 2, offers: 0 }]);
    }

    #[test]
    fn possession_is_proved_and_caps_hold() {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        assert_eq!(
            k.register(1, hdr(10, 900), vec![], 11),
            vec![
                Out::Refused {
                    conn: 1,
                    code: RefuseCode::RdevMismatch,
                    rdev: 10
                },
                Out::DropDuplicate(10)
            ]
        );
        window(&mut k, &mut w, 2, 501, 0);
        window(&mut k, &mut w, 3, 502, 0);
        k.register(1, hdr(10, 900), vec![], 10);
        k.register(2, hdr(10, 900), vec![], 10);
        assert!(matches!(
            k.register(3, hdr(10, 900), vec![], 10)[0],
            Out::Refused {
                code: RefuseCode::TooManyClaimants,
                ..
            }
        ));
        // A status client may not register.
        assert!(k.accept(9, 503, None));
        k.hello(9, PeerClass::Status, 0, 0, &mut w);
        assert!(matches!(
            k.register(9, hdr(12, 900), vec![], 12)[0],
            Out::Refused {
                code: RefuseCode::WrongClass,
                ..
            }
        ));
        // Only a claimant releases.
        assert!(matches!(k.release(9, 10)[0], Out::Refused { .. }));
        // Two claimants (an update's outgoing window and its successor): the
        // first RELEASE ends only its own claim — the copy stays for the
        // other — and the last one closes it.
        assert_eq!(
            k.release(1, 10),
            vec![],
            "the other claimant still holds it"
        );
        assert_eq!(k.records()[&10].claimants, vec![2]);
        assert!(
            matches!(k.release(1, 10)[0], Out::Refused { .. }),
            "no claim left"
        );
        assert_eq!(k.release(2, 10), vec![Out::Close(10)]);
    }

    /// P2's open finding (the design's "What P3 needs"): the outgoing window
    /// of an update releases a master AFTER its successor registered it, and
    /// the successor then crashes. The custody copy must still be there, so
    /// the shell is orphaned for the next window — not hung up with no keeper
    /// fault.
    #[test]
    fn a_late_release_from_the_outgoing_window_keeps_the_successors_copy() {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        assert_eq!(k.register(1, hdr(10, 900), vec![], 10), vec![Out::Keep(10)]);
        k.pending(1, 600, 1, None);
        w.live_pids.push(600);
        window(&mut k, &mut w, 2, 600, 2);
        assert_eq!(
            k.register(2, hdr(10, 900), vec![], 10),
            vec![Out::DropDuplicate(10)]
        );
        // The outgoing window's late Session::drop.
        assert_eq!(k.release(1, 10), vec![]);
        // It `_exit`s: its stream ends.
        w.live_pids.retain(|p| *p != 500);
        assert!(k.eof(1, 3, &mut w).is_empty());
        assert!(k.peer_exit(500, 0, 3, &mut w).is_empty());
        assert!(k.records().contains_key(&10), "the successor's copy stays");
        // The successor crashes: the shell is an orphan, not lost.
        w.live_pids.retain(|p| *p != 600);
        k.eof(2, 4, &mut w);
        k.peer_exit(600, 9, 4, &mut w);
        assert_eq!(k.records()[&10].state, RecordState::Orphaned);
    }

    /// The same process twice (a link that dropped and came back, its old
    /// connection not yet judged) is ONE claimant: its release closes.
    #[test]
    fn a_release_from_a_reconnected_link_is_the_last_claim() {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        k.register(1, hdr(10, 900), vec![], 10);
        assert!(k.accept(2, 500, None));
        k.hello(2, PeerClass::App, CAP_SENDS_BYE | CAP_LINK, 1, &mut w);
        // Registered again from the new link before the old one's EOF.
        k.register(2, hdr(10, 900), vec![], 10);
        assert_eq!(k.records()[&10].claimants, vec![1, 2]);
        assert_eq!(k.release(2, 10), vec![Out::Close(10)]);
    }

    #[test]
    fn a_dead_shell_is_closed_not_offered() {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        k.register(1, hdr(10, 900), vec![], 10);
        k.register(1, hdr(11, 901), vec![], 11);
        w.live_pids.clear();
        k.eof(1, 1, &mut w);
        k.peer_exit(500, 9, 1, &mut w);
        w.dead_shells.push(900);
        let outs = window(&mut k, &mut w, 2, 600, 2);
        assert_eq!(
            outs,
            vec![
                Out::Close(10),
                Out::Welcome { conn: 2, offers: 1 },
                Out::Offer { conn: 2, rdev: 11 }
            ]
        );
    }

    /// STALE ORPHANS: an orphan whose shell dies is closed on its leader's
    /// exit — the keeper asked to watch it when it orphaned it — without
    /// waiting for a HELLO, and `status` no longer lists it.
    #[test]
    fn an_orphan_whose_shell_dies_is_pruned_at_once() {
        let mut k = KeeperCore::new(false);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        k.register(1, hdr(10, 900), vec![], 10);
        k.register(1, hdr(11, 901), vec![], 11);
        w.live_pids.clear();
        k.eof(1, 1, &mut w);
        let outs = k.peer_exit(500, 9, 1, &mut w);
        assert!(outs.contains(&Out::WatchPid(900)), "{outs:?}");
        assert!(outs.contains(&Out::WatchPid(901)), "{outs:?}");
        assert_eq!(k.orphan_count(), 2);
        // Shell 900 dies: its exit prunes its record, and only its.
        w.dead_shells.push(900);
        let outs = k.peer_exit(900, 1, 5, &mut w);
        assert_eq!(outs, vec![Out::Close(10)]);
        assert_eq!(k.orphan_count(), 1);
        assert_eq!(k.ends().pruned, 1);
        let status = k.status_lines().join("\n");
        assert!(!status.contains("shell=900"), "{status}");
        assert!(status.contains("shell=901"), "{status}");
        assert!(status.contains("pruned=1"), "{status}");
        // A watch that could not be armed (the leader already gone) is the
        // same prune, asked directly.
        w.dead_shells.push(901);
        assert_eq!(k.prune_dead_orphans(&mut w), vec![Out::Close(11)]);
        assert_eq!(k.records().len(), 0);
    }

    /// ROW 3's CROSS-CHECK through the core: a window that named a HELD marker
    /// at HELLO and ended with `exit(0)`, no BYE and its marker gone is a quit;
    /// the same end with the marker still there is a crash; a marker that was
    /// not held at HELLO is not evidence at all.
    #[test]
    fn a_lost_bye_with_the_marker_gone_is_a_quit() {
        let marker = MarkerRef {
            nanos: 7,
            dir: b"/logs".to_vec(),
        };
        let run = |at_hello: MarkerEvidence, at_death: MarkerEvidence, status: i32| {
            let mut k = KeeperCore::new(true);
            let mut w = World::default();
            window(&mut k, &mut w, 1, 500, 0);
            w.markers.insert(500, at_hello);
            let kept = k.name_marker(1, marker.clone(), &mut w);
            assert_eq!(kept, at_hello == MarkerEvidence::Held);
            k.register(1, hdr(10, 900), vec![], 10);
            w.live_pids.clear();
            w.markers.insert(500, at_death);
            k.eof(1, 1, &mut w);
            k.peer_exit(500, status, 1, &mut w);
            (
                k.last_verdict(),
                k.records().contains_key(&10),
                k.relaunch_state(),
            )
        };
        use MarkerEvidence::{Dead, Held, Released};
        assert_eq!(
            run(Held, Released, 0),
            (Some(Verdict::CleanExit), false, RelaunchState::Idle),
            "a clean quit is never a crash, and relaunches nothing"
        );
        assert_eq!(run(Held, Dead, 9).0, Some(Verdict::Crash));
        assert_eq!(
            run(Held, Released, 101 << 8).0,
            Some(Verdict::Crash),
            "a panic's exit path also removes the marker: a crash, never a quit"
        );
        assert_eq!(
            run(Released, Released, 0).0,
            Some(Verdict::Crash),
            "a marker not held at HELLO proves nothing"
        );
        assert_eq!(run(Dead, Released, 0).0, Some(Verdict::Crash));
    }

    #[test]
    fn the_relaunch_lane_waits_for_hello_and_backs_off() {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        k.register(1, hdr(10, 900), vec![], 10);
        w.live_pids.clear();
        k.eof(1, 0, &mut w);
        k.peer_exit(500, 9, 0, &mut w);
        assert_eq!(k.tick(0, &mut w), vec![Out::Relaunch]);
        // No HELLO within 15 s: a failed relaunch; the next waits 10 s.
        assert!(k.tick(14_999, &mut w).is_empty());
        assert!(k.tick(15_000, &mut w).is_empty());
        assert_eq!(k.relaunch_state(), RelaunchState::Due { at: 25_000 });
        assert!(k.tick(24_999, &mut w).is_empty());
        assert_eq!(k.tick(25_000, &mut w), vec![Out::Relaunch]);
        assert!(k.tick(40_000, &mut w).is_empty());
        assert_eq!(k.relaunch_state(), RelaunchState::Due { at: 100_000 });
        assert_eq!(k.tick(100_000, &mut w), vec![Out::Relaunch]);
        assert!(k.tick(115_000, &mut w).is_empty());
        assert_eq!(
            k.relaunch_state(),
            RelaunchState::Held,
            "the streak is spent"
        );
        assert!(
            k.tick(10_000_000, &mut w).is_empty(),
            "held until a manual launch"
        );
        // A manual launch is offered the orphan and resets the brake.
        let outs = window(&mut k, &mut w, 2, 600, 10_000_001);
        assert!(outs.contains(&Out::Offer { conn: 2, rdev: 10 }));
        assert_eq!(k.brake().streak(), 0);
    }

    #[test]
    fn a_keeper_without_a_bundle_never_relaunches() {
        let mut k = KeeperCore::new(false);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        k.register(1, hdr(10, 900), vec![], 10);
        w.live_pids.clear();
        k.eof(1, 0, &mut w);
        k.peer_exit(500, 9, 0, &mut w);
        assert!(k.tick(1_000_000, &mut w).is_empty());
        assert_eq!(k.orphan_count(), 1, "held for the next manual launch");
    }

    #[test]
    fn an_offer_whose_recipient_dies_is_an_orphan_again() {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 500, 0);
        k.register(1, hdr(10, 900), vec![], 10);
        w.live_pids.clear();
        k.eof(1, 0, &mut w);
        k.peer_exit(500, 9, 0, &mut w);
        window(&mut k, &mut w, 2, 600, 1);
        assert_eq!(k.records()[&10].state, RecordState::Offered { to: 2 });
        w.live_pids.clear();
        k.eof(2, 2, &mut w);
        k.peer_exit(600, 9, 2, &mut w);
        assert_eq!(k.records()[&10].state, RecordState::Orphaned);
        assert!(
            matches!(k.relaunch_state(), RelaunchState::Due { .. }),
            "a recipient that crashed before it registered still owes the orphan a window"
        );
    }

    /// An offer is made to a CONNECTION (round-seven update audit, finding
    /// 65): when that connection drops while its window lives — the window's
    /// boot HELLO timed out, or the keeper's own send of the OFFER failed —
    /// the offer returns to Orphaned at the EOF, so an ADOPT from the same
    /// window's next link (or the next launch) is offered it again; it never
    /// stays Offered to a dead connection for the window's lifetime. And the
    /// window's later QUIT, whose BYE arrives on its new link, relaunches
    /// nothing: a BYE is the process's, whichever of its links carried it.
    #[test]
    fn an_offer_whose_link_drops_while_its_window_lives_is_offered_again() {
        let mut k = KeeperCore::new(true);
        let mut w = World::default();
        window(&mut k, &mut w, 1, 10, 0);
        k.register(1, hdr(10, 900), vec![], 10);
        w.live_pids.clear();
        k.eof(1, 0, &mut w);
        k.peer_exit(10, 9, 0, &mut w);
        assert_eq!(k.records()[&10].state, RecordState::Orphaned);
        // The keeper relaunches; the new window's HELLO is offered the orphan.
        assert_eq!(k.tick(0, &mut w), vec![Out::Relaunch]);
        let outs = window(&mut k, &mut w, 2, 20, 1);
        assert!(outs.contains(&Out::Offer { conn: 2, rdev: 10 }), "{outs:?}");
        assert_eq!(k.records()[&10].state, RecordState::Offered { to: 2 });
        // Its link drops before it registers; the window lives on.
        let outs = k.eof(2, 2, &mut w);
        assert!(
            outs.contains(&Out::WatchPid(900)),
            "the orphan's leader is watched again: {outs:?}"
        );
        assert!(k.tick(2 + DEATH_GRACE_MS + 1, &mut w).is_empty());
        assert_eq!(
            k.records()[&10].state,
            RecordState::Orphaned,
            "no offer stands to a connection that is gone"
        );
        // The same window's link reconnects, and asks for the offers.
        assert!(k.accept(3, 20, None));
        assert_eq!(
            k.hello(3, PeerClass::App, CAP_SENDS_BYE | CAP_LINK, 3, &mut w),
            vec![Out::Welcome { conn: 3, offers: 0 }]
        );
        let outs = k.adopt(3, &mut w);
        assert!(outs.contains(&Out::Offer { conn: 3, rdev: 10 }), "{outs:?}");
        // The person quits that window: BYE on the new link, then its end.
        k.bye(3);
        let now = 100_000;
        k.eof(3, now, &mut w);
        w.live_pids.clear();
        k.peer_exit(20, 0, now, &mut w);
        for t in [now, now + 1_000_000, now + 10_000_000] {
            assert!(
                !k.tick(t, &mut w).contains(&Out::Relaunch),
                "a window the person quit is never brought back"
            );
        }
        assert!(
            !matches!(k.relaunch_state(), RelaunchState::Due { .. }),
            "{:?}",
            k.relaunch_state()
        );
    }
}

#[cfg(test)]
mod late_hello_tests {
    use super::*;

    struct Alive(Vec<u32>);
    impl KeeperEnv for Alive {
        fn holders(&mut self, _: Rdev) -> Holders {
            Holders::None
        }
        fn shell_alive(&mut self, _: u32, _: Birth) -> bool {
            true
        }
        fn pid_alive(&mut self, pid: u32, _: Option<Birth>) -> bool {
            self.0.contains(&pid)
        }
    }

    fn hdr(rdev: Rdev) -> MasterHeader {
        MasterHeader {
            rdev,
            shell_pid: 900,
            shell_birth: Birth::default(),
            local_id: rdev,
        }
    }

    /// A build that boots slower than HELLO_DEADLINE_MS and crashes after its
    /// HELLO: every relaunch's late HELLO must still be that relaunch
    /// answering, never a manual launch that resets the brake — or the brake
    /// never holds and the keeper relaunches it forever.
    #[test]
    fn a_slow_booting_crash_loop_is_still_braked() {
        let mut k = KeeperCore::new(true);
        let mut w = Alive(vec![500]);
        assert!(k.accept(1, 500, None));
        k.hello(1, PeerClass::App, CAP_SENDS_BYE, 0, &mut w);
        k.register(1, hdr(10), vec![], 10);
        w.0.clear();
        k.eof(1, 0, &mut w);
        k.peer_exit(500, 9, 0, &mut w);
        let mut now = 0u64;
        let mut relaunches = 0;
        // Far more cycles than the streak allows.
        for conn in 2u64..22 {
            // Run the clock until the lane relaunches (or holds for good).
            let mut fired = false;
            for _ in 0..200 {
                if k.tick(now, &mut w).contains(&Out::Relaunch) {
                    fired = true;
                    break;
                }
                now += 1_000;
            }
            if !fired {
                break;
            }
            relaunches += 1;
            // The window says HELLO 20 s later: after the 15 s deadline.
            now += 20_000;
            k.tick(now, &mut w);
            let pid = 600 + u32::try_from(conn).expect("small");
            assert!(k.accept(conn, pid, None));
            w.0.push(pid);
            k.hello(conn, PeerClass::App, CAP_SENDS_BYE, now, &mut w);
            k.register(conn, hdr(10), vec![], 10);
            // It crashes 5 s after its HELLO.
            now += 5_000;
            w.0.clear();
            k.eof(conn, now, &mut w);
            k.peer_exit(pid, 9, now, &mut w);
        }
        assert!(
            relaunches <= MAX_STREAK as usize + 1,
            "relaunched {relaunches} times: the brake never held"
        );
        assert_eq!(k.orphan_count(), 1, "the orphan is still kept");
    }
}
