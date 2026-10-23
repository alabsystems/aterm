// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic fabric simulation world and fault/settle scheduler.
//!
//! Drives [`FLEET`], [`NODE`], and [`PERSON`] sessions from [`Opts::seed`] across bus mirrors
//! ([`Opts::mirror`]), broker restarts ([`bus::Stop`]), held acks ([`Opts::hold_acks`],
//! [`bus::Broker::answer_held`]), disk write errors ([`Opts::write_errors`], [`Device::fill`]),
//! whole-machine [`Opts::power_cuts`] ([`Opts::torn_device`]), and isolated [`Opts::bus_losses`].

use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use super::bus::{self, ArmMutant, Escalation, Stop, Tier};
use super::conform;
use super::device::Device;
use super::endpoint::{Endpoint, EpRules};
use super::{lock, Core, Process, Rng, SimKill};
use crate::body::Body;
use crate::bridge::{Bridge, Config};
use crate::ctl::Ctl;
use crate::fx::Fx;
use crate::mailbox::Mailbox;
use crate::transport::Backoff;

/// The fleet.
pub(crate) const FLEET: &str = "sim";
/// This node.
pub(crate) const NODE: &str = "n-51a0000000000001";
/// The peer node, a producer of the world's own.
pub(crate) const PEER: &str = "n-9ee7000000000002";
/// The person.
pub(crate) const PERSON: &str = "h-alice";

/// One rule of the bridge switched off, for a negative control ([`crate::fx::Fx::mutant`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Mutant {
    /// The attach reads no lineage.
    IgnoreLineage,
    /// A post left queued by a kill takes a fresh sequence on its retry instead of the one pinned.
    FreshSeqOnRetry,
    /// A receipt left owed by a kill takes a fresh sequence on its retry.
    FreshReceiptSeq,
    /// A sender's next post goes out before its last one is answered.
    PipelineSender,
    /// The inbox commit the bridge owed kept across a move that took what it names.
    KeepOwedCommitAcrossMove,
    /// The incarnation lease trusted across boots.
    LeaseAcrossBoots,
    /// The bus mirror's pinned batch kept across a move of the log.
    KeepMirrorPinAcrossMove,
    /// No ghost sweep: a session row whose instance died unannounced stays `state=live`.
    NoGhostSweep,
    /// `session-exited` publishes no `state=exited` row.
    SkipExitedRow,
    /// A relaunched bridge does not recover the mirror's pinned batch.
    FreshMirrorTake,
    /// The attach records no witnessed.
    IgnoreWitnessedWill,
    /// A will's incarnation is trusted without matching its writer's mark.
    WitnessByIncarnationAlone,
    /// The sweep reads no row's state-dir mark, before the fix decision.
    ReadNoMark,
    /// The sweep takes every incarnation below its own for an earlier bridge of its state dir.
    RetireEveryEarlierIncarnation,
    /// The inbox group is committed through a record before it is delivered.
    CommitBeforeDelivery,
    /// A drain peeks the outbox with a post still in flight, its answer not read first.
    PeekPastOwedPost,
    /// Every read of an owed ack waits for it, as before aterm-0 (`Bridge::try_settle_one`).
    WaitForEveryAck,
    /// At an idle moment the loop reads an owed post's answer only once it has arrived.
    IdlePostLeftToBackstop,
    /// A file a power cut tore is refused where it is read, at every attach of every later boot.
    RefuseTornAfterPowerCut,
    /// The sweep judges a file whole when its reader takes it.
    SweepByTheReaderAlone,
    /// The first bridge of a boot after an unclean stop leaves every whole in-flight entry.
    RepublishInFlightOfAnEarlierBoot,
    /// The first bridge of a boot after an unclean stop leaves a torn mirror pin where it.
    KeepTornPin,
    /// The alias key has no durable copy, as before `logkey`.
    KeyInThePinAlone,
    /// The total is read off the pin alone, as before `logfloor`.
    TotalInThePinAlone,
    /// A bridge tells aterm what its start moved into the quarantine in place.
    TellMovedNotHeld,
    /// An offset of ours below the self-lane window's oldest is added to it, as before sim item S7.
    RememberBelowTheWindow,
    /// A recovery step a write error stopped.
    BeginRunPastAFailedRecovery,
    /// The mirror judges its pin at the hello, the recovery that reconciles it complete.
    JudgePinBeforeRecovery,
    /// An own record whose `acked` write or in-flight entry's unlink failed is delivered.
    DeliverPastAStandingEntry,
    /// The mirror's floor is trusted as it reads, as before sim item S7.
    TrustTheFloorThatReads,
    /// An attach that could not write the state dir's witness of the log.
    AttachPastAnUnwrittenWitness,
    /// An in-flight entry the attach published again is cleared as it lands, as before sim item S7.
    DeliverBeforeTheDrain,
    /// A bridge whose recovery after an unclean stop is owed stops its run cleanly all the same.
    CleanStopOverAnOwedRecovery,
    /// Each try of the recovery's moves publishes a copy of every file again.
    CopyOnEveryTry,
}

/// What one run.
#[derive(Clone, Debug)]
pub(crate) struct Opts {
    pub(crate) seed: u64,
    /// Steps of the fault phase.
    pub(crate) steps: usize,
    /// The tier, or `None` for the seed's choice.
    pub(crate) tier: Option<Tier>,
    pub(crate) ep: EpRules,
    /// The escalation table the broker holds (the oracle always holds aterm's).
    pub(crate) table: Escalation,
    pub(crate) arm: ArmMutant,
    pub(crate) mutants: BTreeSet<Mutant>,
    pub(crate) power_cuts: bool,
    /// The broker's log may lose power alone.
    pub(crate) bus_losses: bool,
    /// How often a power cut is drawn, against a pass's 40 (default 1).
    pub(crate) cut_weight: u64,
    /// The state dir's device keeps every write (a control).
    pub(crate) lossless_device: bool,
    /// The state dir's device may tear an unflushed file (POSIX's worst case, [`Device::tears`]).
    pub(crate) torn_device: bool,
    /// The bus mirror: `[fabric] log = "turns"`, the sessions' logs mirrored onto their `ev` faces.
    pub(crate) mirror: bool,
    /// Held acks (aterm-0): the broker answers some acks late.
    pub(crate) hold_acks: bool,
    /// A sibling under this node's id.
    pub(crate) sibling: bool,
    /// A sibling that races an attach ([`bus::Race`]).
    pub(crate) race: bool,
    /// The sibling takes the current attach's incarnation.
    pub(crate) alias: bool,
    /// WRITE errors (sim item S7, [`Device::arm_write_errors`]).
    pub(crate) write_errors: bool,
    /// Aterm quits and comes back, in the same boot ([`World::quit`]).
    pub(crate) quits: bool,
    /// The door's journal ([`bus::Event`]).
    pub(crate) journal: bool,
}

/// The sibling's one session ([`Opts::sibling`]).
pub(crate) const SIBLING_SID: &str = "s-sibling";

impl Opts {
    /// The committed configuration for `seed`.
    pub(crate) fn seed(seed: u64) -> Self {
        Self {
            seed,
            steps: 400,
            tier: None,
            ep: EpRules::default(),
            table: Escalation::aterm(),
            arm: ArmMutant::None,
            mutants: BTreeSet::new(),
            power_cuts: seed % 2 == 1,
            bus_losses: seed.is_multiple_of(2),
            cut_weight: 1,
            lossless_device: false,
            torn_device: true,
            mirror: (seed / 2) % 2 == 1,
            hold_acks: (seed / 4) % 2 == 1,
            sibling: false,
            race: false,
            alias: false,
            write_errors: (seed / 8) % 2 == 1,
            quits: false,
            journal: false,
        }
    }

    /// `self` with one bridge rule switched.
    pub(crate) fn with(mut self, m: Mutant) -> Self {
        self.mutants.insert(m);
        self
    }

    /// The committed configuration for `seed`.
    pub(crate) fn with_a_sibling(seed: u64) -> Self {
        Self {
            sibling: true,
            power_cuts: false,
            bus_losses: true,
            ..Self::seed(seed)
        }
    }

    /// The committed configuration for `seed`.
    pub(crate) fn with_a_racing_sibling(seed: u64) -> Self {
        Self {
            sibling: false,
            race: true,
            ..Self::with_a_sibling(seed)
        }
    }

    pub(crate) fn with_an_aliasing_sibling(seed: u64) -> Self {
        Self {
            alias: true,
            steps: 0,
            tier: Some(Tier::Strict),
            hold_acks: false,
            mirror: false,
            ..Self::with_a_racing_sibling(seed)
        }
    }
}

/// What one run found.
#[derive(Debug)]
pub(crate) struct Outcome {
    pub(crate) seed: u64,
    pub(crate) tier: Tier,
    pub(crate) violations: Vec<String>,
    pub(crate) trace: Vec<String>,
    pub(crate) stats: Stats,
    /// What the model's tap followed.
    pub(crate) model: (u64, u64, BTreeSet<&'static str>),
    /// The broker's journal ([`Opts::journal`]).
    pub(crate) journal: Vec<bus::Event>,
}

/// How much one run exercised, so a run that did nothing cannot pass.
#[derive(Clone, Debug, Default)]
pub(crate) struct Stats {
    pub(crate) passes: u64,
    pub(crate) pumps: u64,
    pub(crate) kills: u64,
    pub(crate) relaunches: u64,
    pub(crate) broker_restarts: u64,
    pub(crate) moves: u64,
    pub(crate) lost_records: u64,
    pub(crate) power_cuts: u64,
    pub(crate) mail: u64,
    pub(crate) posts: u64,
    pub(crate) receipts: u64,
    pub(crate) halts: u64,
    pub(crate) delivered: u64,
    pub(crate) exits: u64,
    pub(crate) log_records: u64,
    /// Ghost rows the bridge retired (`presence-retired … reason=unhosted`).
    pub(crate) ghosts_retired: u64,
    /// Ghost rows no running bridge may retire, left to the operator.
    pub(crate) ghosts_left: u64,
    /// The sibling ([`Opts::sibling`]), once settled, 1 or 0.
    pub(crate) sibling_kept: u64,
    pub(crate) sibling_retired: u64,
    /// The racing sibling ([`Opts::race`]), 1 or 0.
    pub(crate) sibling_raced: u64,
    /// The alias injected a live sibling at the current incarnation and the real bridge's first.
    pub(crate) sibling_alias_collided: u64,
    /// The specific same-dir session observed live before the first bridge died.
    pub(crate) alias_ghost_retired: u64,
    /// `ms` of the live sessions' logs the bus held once settled.
    pub(crate) mirrored: u64,
    /// Torn state files a bridge moved into the state dir's quarantine after a power cut.
    pub(crate) set_aside: u64,
    /// Of those, how many the bridges told aterm of (`link set-aside`).
    pub(crate) set_aside_told: u64,
    /// Power cuts that tore `logpin`, the mirror's pin.
    pub(crate) pins_torn: u64,
    /// Of the torn files set aside.
    pub(crate) pins_set_aside: u64,
    /// Whole in-flight entries the first bridge of a boot retired into the quarantine.
    pub(crate) retired: u64,
    /// Held acks ([`Opts::hold_acks`], [`bus::Broker::held_counts`]).
    pub(crate) acks_held: u64,
    pub(crate) tries_not_yet: u64,
    pub(crate) seen_before_ack: u64,
    /// WRITE errors the device injected ([`Opts::write_errors`], [`super::device::WriteErrors`]).
    pub(crate) writes_failed: u64,
    pub(crate) unlinks_failed: u64,
    pub(crate) flushes_failed: u64,
    pub(crate) renamed_unflushed: u64,
    pub(crate) appends_cut: u64,
    pub(crate) disk_full_refused: u64,
    pub(crate) disk_full_spells: u64,
    /// Times the bridge said its link down for a state-dir write the disk refused.
    pub(crate) held_for_the_disk: u64,
    /// Quits ([`Opts::quits`]).
    pub(crate) quits: u64,
    pub(crate) clean_stops: u64,
}

/// One message the world made, by its marker (the text it carries).
#[derive(Clone, Debug)]
struct Sent {
    /// The session it is for (`None`: the person's lane).
    to: Option<String>,
    /// A post's sender and id.
    post: Option<(String, u64)>,
}

/// One session's `ev` face as a reader sees it ([`World::mirror_faces`]).
#[derive(Debug, Default)]
struct Face {
    /// Per `ms`, the distinct `(ep, seq)` records that hold it.
    recs: BTreeMap<u64, BTreeSet<(u32, u64)>>,
    /// Every cover: `(first ms, how many)`.
    covers: Vec<(u64, u64)>,
    /// How many bus records carry each `(ep, seq)`, before the reader's dedup.
    raw: BTreeMap<(u32, u64), usize>,
}

impl Face {
    fn holds(&self, ms: u64) -> bool {
        self.recs.contains_key(&ms)
            || self
                .covers
                .iter()
                .any(|&(lo, n)| ms >= lo && ms < lo.saturating_add(n))
    }
}

/// One bridge process, running.
struct Live {
    bridge: Bridge,
    process: Arc<Process>,
    backoff: Backoff,
    mailbox: Arc<Mailbox>,
}

/// THE WORLD.
struct World {
    opts: Opts,
    rng: Rng,
    tier: Tier,
    _dir: aterm_tempfile::TempDir,
    cfg: Config,
    core: Arc<Core>,
    ep: Arc<Endpoint>,
    live: Option<Live>,
    next_pid: u64,
    /// Which boot this is (a power cut starts the next).
    boot: u8,
    /// The next local id and sid generation aterm mints.
    next_local: u64,
    epoch: u64,
    marker: u64,
    sent: BTreeMap<String, Sent>,
    /// Every halt row the person published, in order.
    halt_rows: Vec<(String, bool)>,
    peer_seq: u64,
    person_seq: u64,
    violations: Vec<String>,
    stats: Stats,
    /// The lowest offset a broker restart cut back.
    lost_floor: Option<u64>,
    /// What a sibling's rows carry past the fields every row.
    sibling_mark: String,
    /// This directory's mark under the injected current boot, for the oracle.
    mark: Option<String>,
    /// The sibling ([`Opts::sibling`]).
    sibling_inc: Option<u64>,
    sibling_attaches_at: Option<usize>,
    sibling_dies_at: Option<usize>,
    sibling_dead: bool,
    /// The finite alias fixture's actual departed B1 session.
    alias_ghost: Option<String>,
    /// Tier-1 of `FabricExactlyOnce`.
    tap: conform::Tap,
    /// The quarantine's names when the running bridge's process was launched.
    quarantine_at_launch: BTreeSet<String>,
}

/// The producer id of `principal` (astream's derivation).
fn producer(principal: &str) -> u64 {
    astream_cap::producer_id_of(principal)
}

/// A real directory's mark under the WORLD'S kernel boot, never the test host's.
fn boot_mark(path: &std::path::Path, boot: u8) -> Option<String> {
    let state = crate::state::StateDir::open(path).expect("the world's state dir");
    state.set_inc_seam(crate::state::IncSeam {
        boot: Some(Some(crate::durability::test_boot(boot))),
        ..crate::state::IncSeam::default()
    });
    state.mark(NODE)
}

impl World {
    fn new(opts: Opts) -> Self {
        let mut rng = Rng::new(opts.seed);
        let tier = opts.tier.unwrap_or(match rng.below(3) {
            0 => Tier::Strict,
            1 => Tier::Ordered,
            _ => Tier::Relaxed,
        });
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-sim")
            .tempdir_in("/tmp")
            .expect("a scratch dir");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).expect("the state dir");
        let copy = dir.path().join("copy");
        std::fs::create_dir_all(&copy).expect("the copy's dir");
        let sibling_mark = boot_mark(&copy, if opts.alias { 2 } else { 1 })
            .map(|mark| format!(" {}={mark}", crate::state::PRESENCE_MARK))
            .unwrap_or_default();
        let mark = boot_mark(&state_dir, 1);
        let mut device = Device::new(&state_dir);
        device.lossless = opts.lossless_device;
        device.tears = opts.torn_device;
        device.seed(&state_dir.join("node"), format!("{NODE}\n").as_bytes());
        let table = if tier == Tier::Strict {
            Escalation::none()
        } else {
            opts.table.clone()
        };
        let broker = bus::Broker::new(tier, table);
        lock(&broker.state).mutant = opts.arm;
        if opts.journal {
            broker.keep_journal();
        }
        if opts.race {
            lock(&broker.state).race = Some(bus::Race {
                refuse_will: if opts.alias { 2 } else { 3 },
                node_face: crate::subject::node_face(FLEET, NODE, "presence"),
                session_face: crate::subject::session_face(FLEET, NODE, SIBLING_SID, "presence"),
                extra: sibling_mark.clone(),
                alias: opts.alias,
                ..bus::Race::default()
            });
        }
        if opts.hold_acks {
            // A stream of its own, so the holds shift no other draw.
            broker.hold_acks(Rng::new(opts.seed ^ 0x0a0c_4e1d_ac45));
        }
        for p in [NODE, PEER, PERSON] {
            broker.name_producer(producer(p), p);
        }
        let core = Core::new(opts.seed, broker, device);
        *lock(&core.mutants) = opts.mutants.clone();
        if opts.write_errors {
            let mut errors = Rng::new(opts.seed ^ 0x0e10_e5bc_0057);
            let per = [20, 50, 120][usize::try_from(errors.below(3)).unwrap_or(0)];
            core.device.arm_write_errors(errors, per);
        }
        let ep = Endpoint::new(opts.ep, opts.mirror);
        let cfg = Config {
            fleet: FLEET.to_string(),
            broker: "sim".to_string(),
            cap_files: Vec::new(),
            state_dir: state_dir.to_string_lossy().into_owned(),
            accept_from: Vec::new(),
            sock: None,
            token: None,
            presence: crate::presence::Mode::default(),
            receipts: true,
            log: if opts.mirror {
                crate::mirror::LogMode::Turns
            } else {
                crate::mirror::LogMode::Off
            },
        };
        let mut w = World {
            opts,
            rng,
            tier,
            _dir: dir,
            cfg,
            core,
            ep,
            live: None,
            next_pid: 1,
            boot: 1,
            next_local: 1,
            epoch: 1,
            marker: 0,
            sent: BTreeMap::new(),
            halt_rows: Vec::new(),
            peer_seq: 0,
            person_seq: 0,
            violations: Vec::new(),
            lost_floor: None,
            sibling_mark,
            mark,
            sibling_inc: None,
            sibling_attaches_at: None,
            sibling_dies_at: None,
            sibling_dead: false,
            alias_ghost: None,
            tap: conform::Tap::new(aterm_models_session::fabric_exactly_once_model()),
            quarantine_at_launch: BTreeSet::new(),
            stats: Stats::default(),
        };
        if w.opts.sibling {
            let mut fate = Rng::new(w.opts.seed ^ 0x5b1b_11e6_dead_0001);
            let steps = w.opts.steps.max(16) as u64;
            let at = steps - steps / 8 + fate.below(steps / 16);
            w.sibling_attaches_at = usize::try_from(at).ok();
            if !fate.chance(1, 4) {
                w.sibling_dies_at = usize::try_from(at + 1 + fate.below(steps / 16)).ok();
            }
        }
        w.note(format!("world seed={} tier={tier:?}", w.opts.seed));
        w.open_sessions();
        w
    }

    /// THE SIBLING ATTACHES ([`Opts::sibling`]), at its step of the fault
    fn sibling_attaches(&mut self) {
        if self.sibling_inc.is_some() {
            return;
        }
        if !lock(&self.core.broker.state).up {
            self.sibling_attaches_at = self.sibling_attaches_at.map(|at| at + 1);
            self.sibling_dies_at = self.sibling_dies_at.map(|at| at + 1);
            return;
        }
        let inc = self.core.broker.max_incarnation(producer(NODE)) + 1;
        let Some(base) = astream_broker::incarnation_base(inc) else {
            return;
        };
        let node_face = crate::subject::node_face(FLEET, NODE, "presence");
        let session_face = crate::subject::session_face(FLEET, NODE, SIBLING_SID, "presence");
        let rows = [
            (
                node_face,
                format!(
                    "v=1 t=1 state=live inc={inc} fabric=connected{}",
                    self.sibling_mark
                ),
            ),
            (
                session_face,
                format!(
                    "v=1 t=1 inc={inc} epoch=- gen=- state=live hold=0 attention=-{}",
                    self.sibling_mark
                ),
            ),
        ];
        for (n, (subject, body)) in rows.iter().enumerate() {
            let seq = base + 1 + n as u64;
            if self
                .core
                .broker
                .publish_as(producer(NODE), seq, subject, body.as_bytes())
                .is_none()
            {
                return;
            }
        }
        let mut s = lock(&self.core.broker.state);
        s.durable = s.log.len() as u64;
        drop(s);
        self.sibling_inc = Some(inc);
        self.note(format!("sibling attaches inc={inc} {SIBLING_SID}"));
    }

    /// THE SIBLING DIES ([`Opts::sibling`]): its connection ends, and its will
    fn sibling_dies(&mut self) {
        if self.sibling_dead {
            return;
        }
        let Some(inc) = self.sibling_inc else {
            self.sibling_dies_at = self.sibling_dies_at.map(|at| at + 1);
            return;
        };
        if !lock(&self.core.broker.state).up {
            self.sibling_dies_at = self.sibling_dies_at.map(|at| at + 1);
            return;
        }
        self.sibling_dead = true;
        let Some(seq) = astream_broker::will_seq(inc) else {
            return;
        };
        if self.core.broker.fences(producer(NODE), seq) {
            self.note(format!("sibling dies inc={inc}, its will fenced"));
            return;
        }
        let face = crate::subject::node_face(FLEET, NODE, "presence");
        let body = format!(
            "v=1 t=1 state={} inc={inc} fabric=disconnected{}",
            crate::glance::AWAY,
            self.sibling_mark
        );
        if self
            .core
            .broker
            .publish_as(producer(NODE), seq, &face, body.as_bytes())
            .is_some()
        {
            let mut s = lock(&self.core.broker.state);
            s.durable = s.log.len() as u64;
        }
        self.note(format!("sibling dies inc={inc}, its will fired"));
    }

    fn note(&self, line: impl Into<String>) {
        self.core.note(line);
    }

    /// aterm's sessions of this boot: two, with sids of this epoch.
    fn open_sessions(&mut self) {
        for name in ["a", "b"] {
            let local = self.next_local;
            self.next_local += 1;
            let sid = format!("s-{name}{}", self.epoch);
            self.ep.create(local, &sid, self.stamp());
            self.note(format!("session {local} {sid}"));
        }
    }

    /// The world's clock as a session log stamps a record.
    fn stamp(&self) -> aterm_slog::Stamp {
        let mono = u64::try_from(self.core.elapsed().as_millis()).unwrap_or(u64::MAX);
        aterm_slog::Stamp::new(self.core.wall_ms(), mono)
    }

    /// A tab opens: a session joins, and aterm says so on the push lane.
    fn open_one(&mut self) {
        if self.ep.locals().len() >= 4 {
            return;
        }
        let local = self.next_local;
        self.next_local += 1;
        let sid = format!("s-n{local}e{}", self.epoch);
        self.ep.create(local, &sid, self.stamp());
        self.note(format!("session {local} {sid} opens"));
        self.push_event(&format!("EVENT * session-created {sid}"));
    }

    /// A tab closes: a session leaves, and aterm says so on the push lane.
    fn exit_one(&mut self) {
        let locals = self.ep.locals();
        if locals.len() <= 2 {
            return;
        }
        let Some(local) = self.rng.pick(&locals).copied() else {
            return;
        };
        let Some(sid) = self.ep.exit(local) else {
            return;
        };
        self.stats.exits += 1;
        self.note(format!("session {local} {sid} exits"));
        self.push_event(&format!("EVENT {local} session-exited {sid}"));
    }

    /// A session's log grows: a bus-class record (three in four) or a local
    fn log_record(&mut self) {
        let locals = self.ep.locals();
        let Some(local) = self.rng.pick(&locals).copied() else {
            return;
        };
        let bus = self.rng.below(4) != 0;
        if self.ep.append_log(local, bus, self.stamp()) {
            self.stats.log_records += 1;
            self.push_event(&format!("EVENT {local} human"));
        }
    }

    fn next_marker(&mut self) -> String {
        self.marker += 1;
        format!("m{}", self.marker)
    }

    /// Move the broker's and the endpoint's notes into the trace.
    fn collect_notes(&mut self) {
        for n in self.core.broker.take_notes() {
            self.core.note(n);
        }
        let reqs = std::mem::take(&mut lock(&self.ep.state).requests);
        for r in reqs {
            // The link said down for a state-dir write the disk refused
            if r.starts_with("link down reason=error") {
                self.stats.held_for_the_disk += 1;
            }
            self.core.note(format!("ctl {r}"));
        }
    }

    /// Launch a bridge, as aterm's supervisor does.
    fn copies_since_launch(&mut self) {
        let dir = std::path::Path::new(&self.cfg.state_dir).join(crate::state::QUARANTINE);
        let now: BTreeSet<String> = std::fs::read_dir(&dir)
            .map(|dir| {
                dir.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let made: Vec<String> = now
            .difference(&self.quarantine_at_launch)
            .cloned()
            .collect();
        for v in quarantine_copies(&dir, &made) {
            if !self.violations.contains(&v) {
                self.violations.push(v);
            }
        }
        self.quarantine_at_launch = now;
    }

    fn launch(&mut self) {
        self.copies_since_launch();
        let pid = self.next_pid;
        self.next_pid += 1;
        let process = Process::new(Arc::clone(&self.core), pid);
        let (near, far) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
        self.ep.serve(far);
        let ctl = Ctl::from_stream(near).expect("the verb lane");
        self.note(format!("launch pid={pid} boot={}", self.boot));
        let fx = Fx::sim(Arc::clone(&process));
        let built = std::panic::catch_unwind(AssertUnwindSafe(|| {
            Bridge::simulated(self.cfg.clone(), fx, ctl)
        }));
        let mut bridge = match built {
            Ok(Ok(bridge)) => bridge,
            Ok(Err(e)) => {
                self.violations
                    .push(format!("the bridge could not be built: {e}"));
                return;
            }
            Err(payload) => {
                self.absorb(payload, &process);
                return;
            }
        };
        self.stats.relaunches += 1;
        let lease = u8::from(self.opts.mutants.contains(&Mutant::LeaseAcrossBoots));
        bridge.simulate_boot(crate::durability::test_boot(self.boot), lease);
        if self.opts.mutants.contains(&Mutant::IgnoreLineage) {
            bridge.simulate_ignore_lineage();
        }
        let mailbox = bridge.mailbox_handle();
        let started = std::panic::catch_unwind(AssertUnwindSafe(|| bridge.start()));
        match started {
            Ok(None) => {
                self.live = Some(Live {
                    bridge,
                    process,
                    backoff: Backoff::default(),
                    mailbox,
                });
            }
            Ok(Some(ended)) => {
                self.violations
                    .push(format!("the bridge ended before its loop: {ended:?}"));
            }
            Err(payload) => {
                drop(bridge);
                self.absorb(payload, &process);
            }
        }
    }

    /// A step unwound: a simulated kill (the process is dead and its bridge
    fn absorb(&mut self, payload: Box<dyn std::any::Any + Send>, process: &Arc<Process>) {
        if payload.is::<SimKill>() {
            self.stats.kills += 1;
            return;
        }
        process.kill();
        let msg = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
            .unwrap_or_else(|| "a non-string panic".to_string());
        self.violations.push(format!("the bridge panicked: {msg}"));
    }

    /// One pass of the bridge's loop.
    fn pass(&mut self) {
        let Some(mut live) = self.live.take() else {
            return;
        };
        self.stats.passes += 1;
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| live.bridge.pass(&mut live.backoff)));
        match r {
            Ok(None) => self.live = Some(live),
            Ok(Some(ended)) => {
                self.note(format!("bridge ended: {ended:?}"));
                live.process.kill();
            }
            Err(payload) => {
                let process = Arc::clone(&live.process);
                drop(live);
                self.absorb(payload, &process);
            }
        }
    }

    /// Kill the bridge now (a `kill -9` between two of its steps).
    fn kill(&mut self) {
        if let Some(live) = self.live.take() {
            self.note(format!("kill pid={} now", live.process.pid));
            live.process.kill();
            drop(live);
            self.stats.kills += 1;
        }
    }

    /// Queue one of aterm's push-lane lines for the running bridge.
    fn push_event(&self, line: &str) {
        if let Some(live) = &self.live {
            self.note(format!("event {line}"));
            live.mailbox.push_event(line.to_string());
        }
    }

    fn sessions(&self) -> Vec<(u64, String)> {
        self.ep
            .locals()
            .into_iter()
            .filter_map(|l| self.ep.sid(l).map(|s| (l, s)))
            .collect()
    }

    /// The peer posts a note to one of this node's sessions.
    fn peer_mail(&mut self) {
        let sessions = self.sessions();
        let Some((_, sid)) = self.rng.pick(&sessions).cloned() else {
            return;
        };
        let marker = self.next_marker();
        self.peer_seq += 1;
        let subject = crate::subject::post_lane(FLEET, NODE, &sid, PEER, "note");
        let body = format!("v=1 t={} text={marker}", self.core.wall_ms());
        let landed =
            self.core
                .broker
                .publish_as(producer(PEER), self.peer_seq, &subject, body.as_bytes());
        self.note(format!("peer mail {marker} to {sid}: {landed:?}"));
        if landed.is_some() {
            self.stats.mail += 1;
            self.sent.insert(
                marker,
                Sent {
                    to: Some(sid),
                    post: None,
                },
            );
        }
    }

    /// A session posts: to the other session (a note or an ask), or to the
    fn post(&mut self) {
        let sessions = self.sessions();
        if sessions.len() < 2 {
            return;
        }
        let n = sessions.len() as u64;
        let i = usize::try_from(self.rng.below(n)).unwrap_or(0);
        let j = (i + 1 + usize::try_from(self.rng.below(n - 1)).unwrap_or(0)) % sessions.len();
        let (local, from) = sessions[i].clone();
        let to_sid = sessions[j].1.clone();
        let (to, kind, target) = match self.rng.below(6) {
            0 => (PERSON.to_string(), "note", None),
            1 | 2 => (format!("@{to_sid}"), "ask", Some(to_sid)),
            _ => (format!("@{to_sid}"), "note", Some(to_sid)),
        };
        let marker = self.next_marker();
        let Some(id) = self.ep.post(local, &to, kind, &marker) else {
            return;
        };
        self.stats.posts += 1;
        self.note(format!("post {from} #{id} {marker} to={to} kind={kind}"));
        self.sent.insert(
            marker,
            Sent {
                to: target,
                post: Some((from, id)),
            },
        );
        self.push_event(&format!("EVENT {local} post {id}"));
    }

    /// A session marks its rows seen, deciding its newest ask now and then.
    fn seen(&mut self) {
        let sessions = self.sessions();
        let Some((local, sid)) = self.rng.pick(&sessions).cloned() else {
            return;
        };
        let verdict = match self.rng.below(3) {
            0 => None,
            1 => Some("handled"),
            _ => Some("refused"),
        };
        let Some(line) = self.ep.seen(local, verdict) else {
            return;
        };
        if line.contains("verdict=") {
            self.stats.receipts += 1;
        }
        self.note(format!("seen {sid}: {line}"));
        self.push_event(&line);
    }

    /// The person halts the fleet, or lifts the halt.
    fn halt(&mut self) {
        let on = !self.halt_rows.last().is_some_and(|(_, on)| *on);
        let marker = self.next_marker();
        self.person_seq += 1;
        let subject = crate::subject::person_halt(FLEET, PERSON);
        let body = format!(
            "v=1 t={} state={} reason={marker}",
            self.core.wall_ms(),
            if on { "on" } else { "off" }
        );
        let landed = self.core.broker.publish_as(
            producer(PERSON),
            self.person_seq,
            &subject,
            body.as_bytes(),
        );
        self.note(format!(
            "halt {} {marker}: {landed:?}",
            if on { "on" } else { "off" }
        ));
        if landed.is_some() {
            self.stats.halts += 1;
            self.halt_rows.push((marker, on));
        }
    }

    /// The broker stops, as `stop` says, and the law is checked on what it kept.
    fn broker_stop(&mut self, stop: Stop) {
        if !lock(&self.core.broker.state).up {
            return;
        }
        let mut rng = self.rng.fork(0xb0);
        let r = self.core.broker.stop(stop, &mut rng);
        self.stats.broker_restarts += 1;
        if r.generation_after != r.generation_before {
            self.stats.moves += 1;
        }
        self.stats.lost_records += r.before.len() as u64 - r.kept;
        if r.kept < r.before.len() as u64 {
            self.lost_floor = Some(self.lost_floor.map_or(r.kept, |f| f.min(r.kept)));
        }
        let oracle = lock(&self.core.broker.state).oracle.clone();
        for v in bus::law_check(&r, &oracle) {
            self.violations.push(format!("broker law: {v}"));
        }
    }

    /// A POWER CUT of the whole machine: the bridge dies, the broker loses
    fn power_cut(&mut self) {
        self.stats.power_cuts += 1;
        self.note("power cut");
        self.kill();
        if lock(&self.core.broker.state).up {
            self.broker_stop(Stop::PowerLoss);
        }
        let mut rng = self.rng.fork(0xdc);
        for n in self.core.device.power_cut(&mut rng) {
            if n.starts_with("power-cut logpin ") && n.ends_with(" torn") {
                self.stats.pins_torn += 1;
            }
            self.note(n);
        }
        self.ep.power_cut();
        self.boot = self.boot.wrapping_add(1).max(1);
        self.mark = boot_mark(std::path::Path::new(&self.cfg.state_dir), self.boot);
        self.epoch += 1;
        self.core.broker.start();
        self.open_sessions();
    }

    /// ATERM QUITS AND COMES BACK, in the same boot ([`Opts::quits`]): its
    fn quit(&mut self) {
        let Some(live) = &self.live else {
            return;
        };
        self.stats.quits += 1;
        self.note("aterm quits");
        live.mailbox
            .note_bye(aterm_types::fabric_grammar::Bye::Quit);
        self.push_event("EVENT * bye quit");
        let acked = lock(&self.ep.state).byes_acked;
        for _ in 0..8 {
            if self.live.is_none() || lock(&self.ep.state).byes_acked > acked {
                break;
            }
            self.pass();
            self.collect_notes();
        }
        if let Some(live) = &self.live {
            live.mailbox.push_aterm_closed();
        }
        for _ in 0..4 {
            if self.live.is_none() {
                break;
            }
            self.pass();
            self.collect_notes();
        }
        self.kill();
        let run = crate::state::StateDir::peek(&self.cfg.state_dir).last_run();
        if matches!(run, Ok(Some(run)) if run.clean) {
            self.stats.clean_stops += 1;
        }
        self.ep.power_cut();
        self.epoch += 1;
        self.open_sessions();
    }

    /// ONE STEP OF THE FAULT PHASE.
    fn fault_step(&mut self) {
        let up = lock(&self.core.broker.state).up;
        let readable = self.core.readable();
        let alive = self.live.is_some();
        let mut menu: Vec<(u64, u8)> =
            vec![(6, 2), (6, 3), (4, 4), (2, 5), (2, 13), (1, 16), (1, 17)];
        if self.opts.mirror {
            menu.push((6, 15));
        }
        if alive {
            menu.push((40, 0));
            menu.push((2, 6));
            menu.push((2, 7));
        } else {
            menu.push((12, 8));
        }
        if !readable.is_empty() {
            menu.push((25, 1));
        }
        if up {
            menu.push((1, 9));
            menu.push((1, 14));
            if self.tier == Tier::Ordered {
                menu.push((2, 12));
            }
        } else {
            menu.push((8, 10));
        }
        if self.opts.power_cuts {
            menu.push((self.opts.cut_weight, 11));
        }
        if self.opts.hold_acks {
            menu.push((4, 18));
        }
        if alive && self.opts.quits {
            menu.push((1, 20));
        }
        if self.opts.write_errors {
            // The disk fills, or has room again (an emptied full disk sooner
            menu.push((if self.core.device.is_full() { 4 } else { 1 }, 19));
        }
        let total: u64 = menu.iter().map(|(w, _)| w).sum();
        let mut draw = self.rng.below(total);
        let mut action = 0;
        for (w, a) in &menu {
            if draw < *w {
                action = *a;
                break;
            }
            draw -= w;
        }
        match action {
            0 => self.pass(),
            1 => {
                let pick = self.rng.pick(&readable).map(|(c, _)| *c);
                if let Some(conn) = pick {
                    self.stats.pumps += 1;
                    self.core.pump(conn);
                }
            }
            2 => self.peer_mail(),
            3 => self.post(),
            4 => self.seen(),
            5 => self.halt(),
            6 => self.kill(),
            7 => {
                if let Some(live) = &self.live {
                    let k = 1 + self.rng.below(12);
                    self.note(format!("arm kill pid={} in {k}", live.process.pid));
                    self.core.arm_kill(live.process.pid, k);
                }
            }
            8 => self.launch(),
            9 => {
                let kinds = if self.opts.bus_losses { 3 } else { 2 };
                let stop = match self.rng.below(kinds) {
                    0 => Stop::Clean,
                    1 => Stop::Crash,
                    _ => Stop::PowerLoss,
                };
                self.broker_stop(stop);
            }
            10 => self.core.broker.start(),
            11 => self.power_cut(),
            12 => self.core.broker.horizon(),
            14 => {
                let n = 1 + u32::try_from(self.rng.below(2)).unwrap_or(0);
                lock(&self.core.broker.state).refuse_next = n;
                self.note(format!(
                    "broker refuses the next {n} of the node's publishes and commits"
                ));
            }
            15 => self.log_record(),
            16 => self.open_one(),
            17 => self.exit_one(),
            18 => {
                self.core.broker.answer_held(false);
            }
            19 => {
                let full = !self.core.device.is_full();
                self.core.device.fill(full);
                if full {
                    self.stats.disk_full_spells += 1;
                }
                self.note(if full { "disk full" } else { "disk has room" });
            }
            20 => self.quit(),
            _ => {
                let d = Duration::from_millis(100 + self.rng.below(20_000));
                self.note(format!("time +{}ms", d.as_millis()));
                self.core.advance(d);
            }
        }
        self.collect_notes();
        self.check_always();
    }

    /// A fingerprint of everything that can still move.
    fn fingerprint(&self) -> (usize, usize, usize, usize, usize) {
        let head = lock(&self.core.broker.state).log.len();
        let s = lock(&self.ep.state);
        let rows: usize = s.sessions.values().map(|x| x.rows.len()).sum();
        let open_posts: usize = s
            .sessions
            .values()
            .map(|x| x.posts.iter().filter(|p| p.retired.is_none()).count())
            .sum();
        let open_receipts: usize = s
            .sessions
            .values()
            .map(|x| x.receipts.iter().filter(|r| r.retired.is_none()).count())
            .sum();
        let holds = s
            .sessions
            .values()
            .filter(|x| x.fleet_hold.is_some())
            .count();
        (head, rows, open_posts, open_receipts, holds)
    }

    /// THE SETTLE PHASE: no more faults; run until nothing moves.
    fn settle(&mut self) {
        self.core.disarm_kill();
        self.note("settle");
        self.core.device.clean_disk();
        if !lock(&self.core.broker.state).up {
            self.core.broker.start();
        }
        self.core.broker.answer_held(true);
        let started = self.core.elapsed();
        let mut last = self.fingerprint();
        let mut still = 0;
        let budget = Duration::from_secs(1800);
        for _ in 0..20_000 {
            if self.core.elapsed().saturating_sub(started) > budget {
                break;
            }
            if self.live.is_none() {
                self.launch();
                continue;
            }
            let readable = self.core.readable();
            if let Some(&(conn, _)) = readable.first() {
                self.stats.pumps += 1;
                self.core.pump(conn);
            } else {
                self.pass();
            }
            self.collect_notes();
            self.check_always();
            let now = self.fingerprint();
            let waited = self.core.elapsed().saturating_sub(started);
            if now == last && readable.is_empty() {
                still += 1;
                if still >= 60
                    && waited >= Duration::from_secs(90)
                    && now.2 == 0
                    && now.3 == 0
                    && (waited >= Duration::from_secs(600) || self.converged())
                {
                    return;
                }
            } else {
                still = 0;
                last = now;
            }
        }
        let (head, rows, posts, receipts, holds) = self.fingerprint();
        self.violations.push(format!(
            "liveness: the world did not settle (head {head}, rows {rows}, posts queued {posts}, \
             receipts owed {receipts}, holds {holds})"
        ));
    }

    /// The text markers of the bus's records on this node's and the person's
    fn bus_markers(&self) -> BTreeMap<String, Vec<(u64, String, Body)>> {
        let mut out: BTreeMap<String, Vec<(u64, String, Body)>> = BTreeMap::new();
        for (off, subject, body, _) in self.core.broker.records() {
            if !subject.starts_with(&format!("/f/{FLEET}/in/")) {
                continue;
            }
            let (body, _) = Body::decode(&body);
            if is_marker(&body.text) {
                out.entry(body.text.clone())
                    .or_default()
                    .push((off, subject, body));
            }
        }
        out
    }

    /// What the model's tap reads of the world now ([`conform::View`]).
    fn view(&self) -> conform::View {
        let lane = format!("/f/{FLEET}/in/{NODE}/");
        let (records, ident) = {
            let s = lock(&self.core.broker.state);
            let records = s
                .log
                .iter()
                .map(|r| {
                    let rest = r.subject.strip_prefix(lane.as_str())?;
                    let sid = rest.split('/').next()?;
                    let (body, _) = Body::decode(&r.body);
                    Some((sid.to_string(), body.text))
                })
                .collect();
            let ident = astream_broker::LogIdentity {
                id: s.id,
                generation: s.generation,
            };
            (records, ident)
        };
        let group = crate::subject::inbox_group(FLEET, NODE);
        let sessions = lock(&self.ep.state)
            .sessions
            .values()
            .map(|s| {
                (
                    s.sid.clone(),
                    conform::SessionView {
                        dedup: s.dedup.clone(),
                        rows: s.rows.iter().map(|r| (r.off, r.text.clone())).collect(),
                        log: s.bus_log.clone(),
                    },
                )
            })
            .collect();
        conform::View {
            records,
            log: ident.to_string(),
            generation: ident.generation,
            group_start: self.core.broker.group_start(&group),
            alive: self.live.is_some(),
            owed: self
                .live
                .as_ref()
                .and_then(|l| l.bridge.owed_inbox_commit())
                .and_then(|(upto, on)| on.is_none_or(|on| on == ident).then_some(upto)),
            sessions,
        }
    }

    /// The last presence row of every session face under this node, `sid ->
    fn presence_rows(&self) -> BTreeMap<String, (String, Option<u64>, Option<String>)> {
        let prefix = format!("/f/{FLEET}/pub/{NODE}/");
        let mut out = BTreeMap::new();
        for (_, subject, body, _) in self.core.broker.records() {
            let Some(sid) = subject
                .strip_prefix(prefix.as_str())
                .and_then(|rest| rest.strip_suffix("/presence"))
                .filter(|sid| *sid != "node" && !sid.contains('/'))
            else {
                continue;
            };
            let (b, _) = Body::decode(&body);
            let state = b.unknown.get("state").cloned().unwrap_or_default();
            let inc = b.unknown.get("inc").and_then(|i| i.parse().ok());
            let mark = b.unknown.get(crate::state::PRESENCE_MARK).cloned();
            out.insert(sid.to_string(), (state, inc, mark));
        }
        out
    }

    /// THE RUNNING BRIDGE, AS THE BUS SHOWS IT, and what it must be able to
    fn running_bridge(&self) -> Option<(u64, Option<(u64, u64)>)> {
        self.live.as_ref()?;
        let rows = self.node_rows();
        let owner = self
            .mark
            .as_deref()
            .and_then(|mark| u64::from_str_radix(mark, 16).ok())?;
        let (state, me, _) = rows
            .iter()
            .rev()
            .find(|(_, _, mark)| *mark == Some(owner))
            .cloned()?;
        let me = me?;
        (state == "live").then(|| (me, witnessed_by(&rows, me)))
    }

    /// THE SIBLING'S INCARNATION, once it attached: the scheduled one's
    fn sibling(&self) -> Option<u64> {
        self.sibling_inc.or_else(|| {
            lock(&self.core.broker.state)
                .race
                .as_ref()
                .and_then(|race| race.raced)
        })
    }

    /// Every row of this node's own presence face, in log order: `(state=,
    fn node_rows(&self) -> Vec<(String, Option<u64>, Option<u64>)> {
        let face = crate::subject::node_face(FLEET, NODE, "presence");
        self.core
            .broker
            .records()
            .into_iter()
            .filter(|(_, subject, _, _)| *subject == face)
            .map(|(_, _, body, _)| {
                let (b, _) = Body::decode(&body);
                let state = b.unknown.get("state").cloned().unwrap_or_default();
                let inc = b.unknown.get("inc").and_then(|i| i.parse().ok());
                let mark = b
                    .unknown
                    .get(crate::state::PRESENCE_MARK)
                    .and_then(|mark| u64::from_str_radix(mark, 16).ok());
                (state, inc, mark)
            })
            .collect()
    }

    /// PRESENCE, once settled: every hosted session's row says `live`, and no
    fn presence_gaps(&self) -> (Vec<String>, Vec<String>) {
        let rows = self.presence_rows();
        let running = self.running_bridge();
        let s = lock(&self.ep.state);
        let mut bad = Vec::new();
        let mut left = Vec::new();
        if let (Some(sibling), Some((state, inc, _))) = (self.sibling(), rows.get(SIBLING_SID)) {
            let owner = self
                .sibling_mark
                .strip_prefix(" statedir=")
                .and_then(|mark| u64::from_str_radix(mark, 16).ok());
            let node = self.node_rows();
            let own_mark = self
                .mark
                .as_deref()
                .and_then(|mark| u64::from_str_radix(mark, 16).ok());
            let ours = node
                .iter()
                .filter(|(state, _, mark)| {
                    state == "live" && own_mark.is_some() && *mark == own_mark
                })
                .filter_map(|(_, i, _)| *i)
                .collect::<BTreeSet<u64>>();
            let witnessed = ours.iter().any(|i| {
                owner.is_some_and(|mark| witnessed_by(&node, *i) == Some((sibling, mark)))
            });
            if state != "live" && !witnessed {
                bad.push(format!(
                    "presence: the sibling's {SIBLING_SID} (inc={inc:?}) was retired, and no \
                     attach here witnessed its will: its row says {state:?}"
                ));
            }
            if state == "live"
                && running
                    .is_some_and(|(_, w)| owner.is_some_and(|mark| w == Some((sibling, mark))))
            {
                bad.push(format!(
                    "presence: the sibling's {SIBLING_SID} is dead and its row (inc={inc:?}) \
                     still says live, a ghost the running bridge may retire"
                ));
            }
        }
        for session in s.sessions.values() {
            let row = rows.get(&session.sid).map(|(state, _, _)| state.as_str());
            if row != Some("live") {
                bad.push(format!(
                    "presence: {} is hosted and its row says {row:?}",
                    session.sid
                ));
            }
        }
        for session in &s.gone {
            let Some((state, inc, mark)) = rows.get(&session.sid) else {
                continue;
            };
            if state != "live" {
                continue;
            }
            if inc.is_some_and(|inc| {
                running.is_some_and(|(me, witnessed)| {
                    (inc <= me && self.mark.is_some() && mark == &self.mark)
                        || mark
                            .as_deref()
                            .and_then(|mark| u64::from_str_radix(mark, 16).ok())
                            .is_some_and(|mark| witnessed == Some((inc, mark)))
                })
            }) {
                bad.push(format!(
                    "presence: {} is gone and its row (inc={inc:?}) still says live, a ghost \
                     the running bridge may retire",
                    session.sid
                ));
            } else {
                left.push(session.sid.clone());
            }
        }
        (bad, left)
    }

    /// THE READER'S VIEW of every session's `ev` face, decoded and deduped by
    fn mirror_faces(&self) -> BTreeMap<String, Face> {
        let prefix = format!("/f/{FLEET}/pub/{NODE}/");
        let mut out: BTreeMap<String, Face> = BTreeMap::new();
        for (_, subject, body, _) in self.core.broker.records() {
            let Some(sid) = subject
                .strip_prefix(prefix.as_str())
                .and_then(|rest| rest.strip_suffix("/ev"))
                .filter(|sid| *sid != "node" && !sid.contains('/'))
            else {
                continue;
            };
            let Ok(b) = aterm_slog::decode_bus(&String::from_utf8_lossy(&body)) else {
                continue;
            };
            let Some(ms) = b.ms else { continue };
            let face = out.entry(sid.to_string()).or_default();
            match b.rec.f.get_u(aterm_slog::Key::N) {
                Some(n) if b.rec.op == aterm_slog::Op::Gap => face.covers.push((ms, n)),
                _ => {
                    face.recs.entry(ms).or_default().insert((b.ep, b.rec.seq));
                    *face.raw.entry((b.ep, b.rec.seq)).or_default() += 1;
                }
            }
        }
        out
    }

    /// THE MIRROR'S AT-MOST-ONCE, at every step (`LogMirror`'s
    fn mirror_twice(&self) -> Vec<String> {
        let mut bad = Vec::new();
        for (sid, face) in self.mirror_faces() {
            for (ms, ids) in &face.recs {
                if ids.len() > 1 {
                    bad.push(format!(
                        "mirror: {sid}'s ms {ms} is {} records in the reader's view {ids:?}",
                        ids.len()
                    ));
                }
            }
        }
        bad
    }

    /// THE MIRROR NEVER STOPS FOR ITS PIN. Every atomic batch pin or send
    fn mirror_stopped_for_its_pin(&self) -> Option<String> {
        let n = lock(&self.ep.state).pin_stops;
        (n > 0).then(|| format!("mirror: stopped for its pin {n} times"))
    }

    /// Across every boot, at every step: the node's takes
    fn mirror_key_and_total(&self) -> Vec<String> {
        let mut bad = Vec::new();
        let keys = lock(&self.ep.state).take_keys.clone();
        if keys.len() > 1 {
            bad.push(format!(
                "mirror: the node's takes named {} alias keys {keys:?}",
                keys.len()
            ));
        }
        if let Some(total) = self
            .live
            .as_ref()
            .and_then(|live| live.bridge.mirror_engaged_total())
        {
            let counted = self.core.device.pin_total();
            if total < counted {
                bad.push(format!(
                    "mirror total: the running bridge counts {total} bytes, below the {counted} \
                     a pin of the node was written with"
                ));
            }
        }
        bad
    }

    /// THE MIRROR, once settled (`LogMirror`'s `QuiescentMirrorComplete`):
    fn mirror_gaps(&mut self) -> Vec<String> {
        let faces = self.mirror_faces();
        let s = lock(&self.ep.state);
        let mut bad = Vec::new();
        let mut mirrored = 0;
        for session in s.sessions.values() {
            let Some(log) = session.log.as_ref() else {
                continue;
            };
            let head = log.ms_head();
            if !log.mirror_complete() {
                bad.push(format!(
                    "mirror: {} landed {:?} of ms head {head}",
                    session.sid,
                    log.mirror_landed()
                ));
            }
            let face = faces.get(&session.sid);
            let missing: Vec<u64> = (1..=head)
                .filter(|ms| !face.is_some_and(|f| f.holds(*ms)))
                .collect();
            mirrored += head - missing.len() as u64;
            if let Some(first) = missing.first() {
                bad.push(format!(
                    "mirror: {}'s ms {first} ({} of {head} in all) is neither on the bus nor \
                     covered, landed {:?}",
                    session.sid,
                    missing.len(),
                    log.mirror_landed()
                ));
            }
        }
        drop(s);
        self.stats.mirrored = mirrored;
        bad
    }

    /// THE MIRROR'S PIN, once settled, in a world whose log never moved: no
    fn mirror_twice_raw(&self) -> Vec<String> {
        let mut bad = Vec::new();
        for (sid, face) in self.mirror_faces() {
            if let Some(((ep, seq), n)) = face.raw.iter().find(|(_, n)| **n > 1) {
                bad.push(format!(
                    "mirror: {sid}'s record {ep}:{seq} is on the bus {n} times, and the log \
                     never moved"
                ));
            }
        }
        bad
    }

    /// Whether what settles slowly has: presence (a ghost waits out the
    fn converged(&mut self) -> bool {
        let (bad, left) = self.presence_gaps();
        bad.is_empty() && left.is_empty() && self.mirror_gaps().is_empty()
    }

    /// One step of the world, through the model's tap.
    fn observe_model(&mut self) {
        let view = self.view();
        let markers: BTreeSet<String> = self
            .sent
            .iter()
            .filter(|(_, s)| s.to.is_some())
            .map(|(m, _)| m.clone())
            .collect();
        let before = self.tap.violations.len();
        self.tap.observe(&view, &markers);
        let new: Vec<String> = self.tap.violations[before..].to_vec();
        self.violations.extend(new);
    }

    /// THE CHECKS THAT HOLD AT EVERY STEP.
    fn check_always(&mut self) {
        self.observe_model();
        let mut bad = Vec::new();
        {
            let s = lock(&self.ep.state);
            for session in s.sessions.values().chain(s.gone.iter()) {
                let mut seen: BTreeMap<&str, u64> = BTreeMap::new();
                for row in &session.rows {
                    if is_marker(&row.text) {
                        let n = seen.entry(row.text.as_str()).or_default();
                        *n += 1;
                        if *n == 2 {
                            bad.push(format!(
                                "dup delivery: {} holds {} twice (rows @{})",
                                session.sid, row.text, row.off
                            ));
                        }
                    }
                }
            }
        }
        if self.opts.mirror {
            bad.extend(self.mirror_twice());
            bad.extend(self.mirror_key_and_total());
        }
        for (marker, records) in self.bus_markers() {
            if self.sent.get(&marker).is_some_and(|s| s.post.is_some()) && records.len() > 1 {
                let offs: Vec<u64> = records.iter().map(|r| r.0).collect();
                bad.push(format!(
                    "dup post: {marker} is on the bus {} times {offs:?}",
                    records.len()
                ));
            }
        }
        let idle_held = std::mem::take(&mut *lock(&self.core.idle_held));
        for (off, subject, body) in idle_held {
            let (body, _) = Body::decode(&body);
            if subject.starts_with(&format!("/f/{FLEET}/in/"))
                && self.sent.get(&body.text).is_some_and(|s| s.post.is_some())
            {
                bad.push(format!(
                    "idle: the loop parked while the broker held the answer to post {} @{off}",
                    body.text
                ));
            }
        }
        for b in bad {
            if !self.violations.contains(&b) {
                self.violations.push(b);
            }
        }
    }

    /// THE CHECKS THAT HOLD ONCE SETTLED.
    fn check_settled(&mut self) {
        let mut bad = Vec::new();
        let markers = self.bus_markers();
        let s = lock(&self.ep.state);
        let live: BTreeMap<&str, &super::endpoint::Session> =
            s.sessions.values().map(|x| (x.sid.as_str(), x)).collect();
        for (marker, sent) in &self.sent {
            let Some(to) = sent.to.as_deref() else {
                continue;
            };
            let Some(session) = live.get(to) else {
                continue;
            };
            if markers.contains_key(marker) && !session.rows.iter().any(|r| r.text == *marker) {
                bad.push(format!(
                    "lost: {marker} is on the bus for {to} and not in its ring"
                ));
            }
        }
        for session in s.sessions.values() {
            for p in &session.posts {
                let marker = String::from_utf8_lossy(&p.body).into_owned();
                match p.retired.as_deref() {
                    None => bad.push(format!(
                        "post {} #{} {marker} never retired",
                        session.sid, p.id
                    )),
                    Some(r)
                        if r.contains("reason=unroutable")
                            && p.to.strip_prefix('@').is_some_and(|to| {
                                s.gone.iter().any(|g| g.sid == to)
                                    && !s.sessions.values().any(|l| l.sid == to)
                            }) => {}
                    Some(r) if r.starts_with("off=-") => {
                        bad.push(format!(
                            "post {} #{} {marker} retired unlanded: {r}",
                            session.sid, p.id
                        ));
                    }
                    Some(r) => {
                        let off: Option<u64> = r
                            .split_whitespace()
                            .find_map(|t| t.strip_prefix("off="))
                            .and_then(|n| n.parse().ok());
                        let on_bus = markers.get(&marker);
                        match (off, on_bus) {
                            (Some(off), Some(records)) if records.iter().any(|x| x.0 == off) => {}
                            (Some(_), None) => {}
                            (Some(off), Some(_)) if self.lost_floor.is_some_and(|f| off >= f) => {}
                            _ => bad.push(format!(
                                "post {} #{} {marker} retired `{r}` but the bus has it at {:?}",
                                session.sid,
                                p.id,
                                on_bus.map(|v| v.iter().map(|x| x.0).collect::<Vec<_>>())
                            )),
                        }
                    }
                }
            }
            for r in &session.receipts {
                if r.retired.is_none() {
                    bad.push(format!(
                        "receipt {} rid={} never retired",
                        session.sid, r.rid
                    ));
                }
            }
        }
        let mut by_sender: BTreeMap<&str, Vec<(u64, u64)>> = BTreeMap::new();
        for (marker, sent) in &self.sent {
            let Some((from, id)) = &sent.post else {
                continue;
            };
            if let Some(records) = markers.get(marker) {
                if let Some(first) = records.first() {
                    by_sender
                        .entry(from.as_str())
                        .or_default()
                        .push((first.0, *id));
                }
            }
        }
        for (from, mut posts) in by_sender {
            posts.sort_unstable();
            if posts.windows(2).any(|w| w[0].1 > w[1].1) {
                bad.push(format!(
                    "order: {from}'s posts on the bus out of order: {posts:?}"
                ));
            }
        }
        let records = self.core.broker.records();
        let by_off: BTreeMap<u64, &String> = records.iter().map(|(o, s, _, _)| (*o, s)).collect();
        let mut answers: BTreeMap<(u64, String), u64> = BTreeMap::new();
        for (off, subject, body, _) in &records {
            if !subject.ends_with("/ack") {
                continue;
            }
            let (body, _) = Body::decode(body);
            if let (Some(re), Some(from)) = (body.re, body.from.clone()) {
                let asked = by_off.get(&re).is_some_and(|s| {
                    s.starts_with(&format!("/f/{FLEET}/in/{NODE}/{from}/")) && s.ends_with("/ask")
                });
                if !asked {
                    bad.push(format!(
                        "receipt: the answer @{off} from {from} names @{re}, which is not its \
                         ask (@{re} is {:?})",
                        by_off.get(&re)
                    ));
                }
                *answers.entry((re, from)).or_default() += 1;
            }
        }
        for ((re, from), n) in answers {
            if n > 1 {
                bad.push(format!(
                    "receipt: {from}'s decision on @{re} answered {n} times"
                ));
            }
        }
        let standing = {
            let records = self.core.broker.records();
            let halt = crate::subject::person_halt(FLEET, PERSON);
            records
                .iter()
                .rfind(|(_, subject, _, _)| *subject == halt)
                .map(|(_, _, body, _)| {
                    let (b, _) = Body::decode(body);
                    b.unknown.get("state").is_some_and(|v| v == "on")
                })
        };
        let ever_on = self.halt_rows.iter().any(|(_, on)| *on);
        for session in s.sessions.values() {
            if standing == Some(true) && session.fleet_hold.is_none() {
                bad.push(format!("halt: {} runs under a standing halt", session.sid));
            }
            if !ever_on && session.fleet_hold.is_some() {
                bad.push(format!(
                    "halt: {} is held with no halt ever on",
                    session.sid
                ));
            }
        }
        drop(s);
        let alias_collision = lock(&self.core.broker.state)
            .race
            .as_ref()
            .filter(|race| race.alias)
            .and_then(|race| race.collision);
        for (_, subject, body, _) in self.core.broker.records() {
            if !subject.starts_with(&format!("/f/{FLEET}/pub/{NODE}/")) {
                continue;
            }
            let text = crate::pct::decode(&String::from_utf8_lossy(&body));
            for alarm in ["cap-compromised", "forged-self"] {
                if text.contains(alarm) {
                    let (parsed, _) = Body::decode(&body);
                    if alias_collision.is_some_and(|(seq, off)| {
                        alarm == "cap-compromised"
                            && subject == crate::subject::node_face(FLEET, NODE, "ev")
                            && parsed.unknown.get("ev").map(|ev| crate::pct::decode(ev))
                                == Some(format!(
                                    "cap-compromised seq={seq} off={off} reason=deduped"
                                ))
                    }) {
                        continue;
                    }
                    bad.push(format!("alarm: {subject}: {text}"));
                }
            }
        }
        let (presence, left) = self.presence_gaps();
        bad.extend(presence);
        self.stats.ghosts_left = left.len() as u64;
        let sibling = self.sibling();
        let sibling_row = sibling
            .and_then(|_| self.presence_rows().remove(SIBLING_SID))
            .map(|(state, _, _)| state);
        let outranked = sibling.is_some_and(|sibling| {
            self.node_rows()
                .iter()
                .any(|(_, inc, _)| *inc > Some(sibling))
        });
        self.stats.sibling_kept = u64::from(outranked && sibling_row.as_deref() == Some("live"));
        self.stats.sibling_retired = u64::from(sibling_row.is_some_and(|state| state != "live"));
        self.stats.sibling_raced = u64::from(self.opts.race && sibling.is_some());
        self.stats.sibling_alias_collided = u64::from(self.core.broker.alias_collision_completed());
        self.stats.alias_ghost_retired = u64::from(self.alias_ghost.as_ref().is_some_and(|sid| {
            self.presence_rows()
                .get(sid)
                .is_some_and(|(state, _, _)| state == "exited")
        }));
        bad.extend(self.mirror_gaps());
        bad.extend(self.mirror_stopped_for_its_pin());
        if self.stats.moves == 0 {
            bad.extend(self.mirror_twice_raw());
        }
        self.stats.ghosts_retired = self
            .core
            .broker
            .records()
            .iter()
            .filter(|(_, subject, body, _)| {
                subject.starts_with(&format!("/f/{FLEET}/pub/{NODE}/"))
                    && crate::pct::decode(&String::from_utf8_lossy(body))
                        .contains("presence-retired")
            })
            .count() as u64;
        let quarantined: Vec<String> = std::fs::read_dir(
            std::path::Path::new(&self.cfg.state_dir).join(crate::state::QUARANTINE),
        )
        .map(|dir| {
            dir.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
        self.stats.retired = quarantined
            .iter()
            .filter(|n| n.starts_with(crate::state::RETIRED_PREFIX))
            .count() as u64;
        self.stats.set_aside = quarantined.len() as u64 - self.stats.retired;
        self.stats.pins_set_aside = quarantined
            .iter()
            .filter(|n| *n == crate::state::LOGPIN || n.starts_with("logpin."))
            .count() as u64;
        self.stats.set_aside_told = lock(&self.ep.state).set_aside_told;
        if self.stats.set_aside_told > self.stats.set_aside {
            bad.push(format!(
                "set-aside: the bridges told aterm of {} torn files, and the quarantine holds {}",
                self.stats.set_aside_told, self.stats.set_aside
            ));
        }
        let held_said = lock(&self.ep.state).held_said;
        if lock(&self.ep.state).rules.lane >= 5 && held_said != Some(self.stats.set_aside) {
            bad.push(format!(
                "quarantine: the newest bridge told aterm it holds {held_said:?} torn files, and \
                 it holds {}",
                self.stats.set_aside
            ));
        }
        self.stats.delivered = {
            let s = lock(&self.ep.state);
            s.sessions
                .values()
                .chain(s.gone.iter())
                .map(|x| x.rows.len() as u64)
                .sum()
        };
        for b in bad {
            if !self.violations.contains(&b) {
                self.violations.push(b);
            }
        }
    }
}

/// The name a quarantined file's copies share: the one its source's path
fn quarantine_base(name: &str) -> &str {
    let parts = match name.split('.').next() {
        Some("logpin") => 1,
        Some("inflight" | "keys" | "unhandled") => 2,
        Some("sent" | "acks" | "retired") => 3,
        _ => return name,
    };
    name.match_indices('.')
        .nth(parts - 1)
        .map_or(name, |(at, _)| &name[..at])
}

/// THE QUARANTINE'S SECOND COPIES ([`World::copies_since_launch`]): each pair of
fn quarantine_copies(dir: &std::path::Path, names: &[String]) -> Vec<String> {
    let mut by_base: BTreeMap<&str, Vec<(&str, Vec<u8>)>> = BTreeMap::new();
    for name in names {
        let path = dir.join(name);
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path) {
            by_base
                .entry(quarantine_base(name))
                .or_default()
                .push((name.as_str(), bytes));
        }
    }
    let mut bad = Vec::new();
    for copies in by_base.values() {
        for (i, (a, bytes)) in copies.iter().enumerate() {
            for (b, other) in &copies[i + 1..] {
                if bytes == other {
                    bad.push(format!(
                        "quarantine copies: {a} and {b} hold the same bytes, one file moved twice"
                    ));
                }
            }
        }
    }
    bad
}

/// Run one seed: the fault phase, the settle phase, the oracle.
pub(crate) fn run(opts: &Opts) -> Outcome {
    let mut w = World::new(opts.clone());
    if opts.alias {
        // exit_one preserves two sessions, so give the real first bridge a
        w.open_one();
    }
    w.launch();
    if opts.alias {
        w.settle();
        let before = w.presence_rows();
        let first_inc = w.running_bridge().map(|(inc, _)| inc);
        w.kill();
        w.exit_one();
        let departed = lock(&w.ep.state)
            .gone
            .last()
            .map(|session| session.sid.clone());
        w.alias_ghost = departed.filter(|sid| {
            before.get(sid).is_some_and(|(state, inc, mark)| {
                state == "live" && inc.is_some() && *inc == first_inc && mark == &w.mark
            })
        });
        w.launch();
    }
    for step in 0..opts.steps {
        if w.sibling_attaches_at == Some(step) {
            w.sibling_attaches();
        }
        if w.sibling_dies_at == Some(step) {
            w.sibling_dies();
        }
        w.fault_step();
        if w.violations
            .iter()
            .any(|v| v.starts_with("the bridge panicked"))
        {
            break;
        }
    }
    if !w
        .violations
        .iter()
        .any(|v| v.starts_with("the bridge panicked"))
    {
        w.settle();
        w.collect_notes();
        w.check_settled();
        w.copies_since_launch();
    }
    let trace = w.core.trace();
    if let Some(live) = w.live.take() {
        live.process.kill();
    }
    (
        w.stats.acks_held,
        w.stats.tries_not_yet,
        w.stats.seen_before_ack,
    ) = w.core.broker.held_counts();
    let errors = w.core.device.write_errors();
    w.stats.writes_failed = errors.writes;
    w.stats.unlinks_failed = errors.unlinks;
    w.stats.flushes_failed = errors.flushes;
    w.stats.renamed_unflushed = errors.renamed_unflushed;
    w.stats.appends_cut = errors.appends_cut;
    w.stats.disk_full_refused = errors.disk_full;
    Outcome {
        seed: opts.seed,
        tier: w.tier,
        violations: w.violations,
        trace,
        stats: w.stats,
        model: (w.tap.opened, w.tap.checked, w.tap.fired.clone()),
        journal: w.core.broker.take_journal(),
    }
}

/// Whether `text` is one of the world's message markers (`m<digits>`).
fn is_marker(text: &str) -> bool {
    text.strip_prefix('m')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The incarnation and writer mark whose WILL was the newest node row when `inc`'s attach read
fn witnessed_by(rows: &[(String, Option<u64>, Option<u64>)], inc: u64) -> Option<(u64, u64)> {
    let first = rows.iter().position(|(_, i, _)| *i == Some(inc))?;
    let (state, dead, mark) = rows.get(first.checked_sub(1)?)?;
    crate::glance::is_will(state)
        .then(|| dead.zip(*mark))
        .flatten()
}
