// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WORLD AND ITS SCHEDULER: one fabric around one real bridge, driven by
//! a seed through a FAULT phase and then a SETTLE phase, and judged by an
//! oracle that reads only what a person could: the bus, and the endpoint's
//! rings, queues and holds.
//!
//! ## The world
//!
//! Fleet [`FLEET`], this node [`NODE`] hosting two sessions, and up to four as
//! tabs open and close; a PEER node that posts to them from outside (a
//! producer of its own on the bus); a person, [`PERSON`], who halts the fleet
//! and is posted to (the person lane, which aterm's escalation table makes
//! durable at its ack). The sessions post to each other — mail that leaves
//! through the outbox, lands on this node's own lane and comes back in
//! through the inbox group — ask each other, and decide those asks, which
//! owes a receipt. With the BUS MIRROR on ([`Opts::mirror`], half the seeds),
//! their session logs grow, and the bridge mirrors them onto their `ev` faces.
//!
//! ## The scheduler
//!
//! Every step draws one action from the seed: a pass of the bridge's loop, a
//! pump of one of its readers, mail from the peer, a post, a decision, a halt
//! or its lift, a tab opening or closing (`session-created`,
//! `session-exited`), a session-log record, a kill (now, or armed to land at
//! the bridge's k-th I/O step), a relaunch, a broker restart ([`bus::Stop`])
//! and its start, a refused batch (the node's next publishes and group
//! commits rolled back, `ERR_REFUSED`), the Ordered horizon's flush, a jump
//! of the clock, and — in a world whose broker holds acks
//! ([`Opts::hold_acks`]) — a flush landing that sends the acks it held
//! ([`bus::Broker::answer_held`]).
//! Then the SETTLE phase: no more faults, the broker up, a bridge running, and
//! the loop and its readers run until nothing moves and presence and the
//! mirror have caught up, or ten minutes have passed (half an hour of the
//! world's clock is the hang detector).
//!
//! ## One device or two
//!
//! What a power loss can take depends on where the log and the state dir
//! live, and [`Opts::seed`] alternates the two layouts. ONE DEVICE (odd
//! seeds, aterm's own layout): the two lose power together, in a power cut
//! of the whole machine ([`Opts::power_cuts`]) — the broker's lossy restart,
//! the state dir's image (which may tear a file the cut did not find flushed,
//! [`Opts::torn_device`]: the bridge sets it aside, finding F1), aterm gone
//! with its sessions (the relaunch mints new sids) and a new boot. TWO
//! DEVICES (even seeds): the log loses power ALONE
//! ([`Opts::bus_losses`]) while aterm, the bridge and the state dir live on —
//! a log on another volume, a Lossy flush failure: the move the endpoint's
//! lane 3 exists for. Both at once is finding F2's world — a landed post
//! the log lost on its own, its in-flight entry brought back by a later cut —
//! decided (the next boot's first bridge retires an earlier boot's entries
//! unpublished) and held as a committed sweep of its own (`super::tests`'
//! `both_ways`).
//!
//! ## The oracle
//!
//! * NO DUPLICATE, at every step: no session's ring holds one message twice;
//!   the bus holds no post twice.
//! * NO SENDER LEFT WAITING BY AN IDLE LOOP, at every step: whenever the
//!   bridge parks with nothing queued, the broker holds no answer to a
//!   session's post on its connections, still in its batch's flush
//!   ([`Process::idle`]) — the loop read it waiting first, rather than leave
//!   the sender's `outbox sent` to the idle backstop's grace (aterm-0).
//! * NO LOSS, once settled: every message on the bus for a session it hosts is
//!   in that session's ring; every post is retired, every retirement that
//!   says `off=<n>` names the record that carries it (unless a power-loss cut
//!   took that record, which the law allows only for an unescalated one: the
//!   broker's own law check covers that), and none is retired unlanded.
//! * PER-SENDER ORDER, once settled: one sender's posts sit on the bus in the
//!   order it queued them.
//! * RECEIPTS EXACTLY ONCE: no decision is answered twice on the bus; once
//!   settled every owed receipt is retired, and one retired `off=<n>` is there.
//! * HALTS: once settled, a halt whose newest row stands holds every session;
//!   with no halt ever on, none is held.
//! * NO ALARM: the bridge never raised `cap-compromised` (a sequence it
//!   reused) nor refused its own mail as `forged-self`.
//! * PRESENCE, once settled: every hosted session's row on the bus says
//!   `live`; no row of a session that left still does, unless the bridge's
//!   contract leaves that ghost to the operator (finding F4,
//!   [`World::presence_gaps`]). A post to a session that left before it was
//!   routed may be retired `unroutable`.
//! * THE MIRROR (`LogMirror`'s laws, read off the bus as a reader reads it):
//!   at every step no `ms` of a session log is two records in the reader's
//!   view; once settled every hosted session's log landed through its `ms`
//!   head and every `ms` up to it is on its `ev` face or covered — across
//!   every move of the log, whose cuts the mirror must say again; and in a
//!   world whose log never moved, no record is on the bus twice even before
//!   the reader's dedup (a killed batch is republished at its pinned
//!   sequences). Completeness, not the rest, ends where a power cut tore the
//!   mirror's pin: the bridge keeps it, the mirror stops for good (its key
//!   and lifetime total, rulings Q6 and Q13, have no other copy), and a stop
//!   for the pin with no tear behind it is a violation.
//! * THE BROKER'S LAW: every restart is held to astream's P2-P7
//!   ([`bus::law_check`]; P1 holds by construction, P8 is not modelled).
//! * LIVENESS: the settle phase ends, the bridge never panics — after a power
//!   cut that tore a state file too (finding F1: the bridge sets it aside;
//!   [`Mutant::RefuseTornAfterPowerCut`], the old refusal, wedges).
//! * WHAT WAS SET ASIDE IS TOLD, NEVER MORE, once settled: the bridges told
//!   aterm (`link set-aside`) of no more torn files than the state dir's
//!   quarantine holds — counting none of the whole in-flight entries an
//!   earlier boot left, which the next boot's first bridge retired there
//!   (`retired.`, finding F2) and tells nobody of.

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

/// One rule of the bridge switched off, for a negative control
/// ([`crate::fx::Fx::mutant`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Mutant {
    /// The attach reads no lineage: a moved generation is another log.
    IgnoreLineage,
    /// A post left queued by a kill takes a fresh sequence on its retry
    /// instead of the one pinned to it.
    FreshSeqOnRetry,
    /// A receipt left owed by a kill takes a fresh sequence on its retry.
    FreshReceiptSeq,
    /// A sender's next post goes out before its last one is answered.
    PipelineSender,
    /// The inbox commit the bridge owed kept across a move that took what it
    /// names (the reader-resume clamp's absence, `FabricExactlyOnce`'s `Buggy = 5`).
    KeepOwedCommitAcrossMove,
    /// The incarnation lease trusted across boots (the lease's own defect 1,
    /// `crate::state::IncSeam`).
    LeaseAcrossBoots,
    /// The bus mirror's pinned batch kept across a move of the log: its slots
    /// landed although the move took them.
    KeepMirrorPinAcrossMove,
    /// No ghost sweep: a session row whose instance died unannounced stays
    /// `state=live`.
    NoGhostSweep,
    /// `session-exited` publishes no `state=exited` row.
    SkipExitedRow,
    /// A relaunched bridge does not recover the mirror's pinned batch: it
    /// takes the same records afresh, at fresh sequences.
    FreshMirrorTake,
    /// The attach records no witnessed will: only its own incarnation's rows
    /// are its to retire, and a predecessor's ghosts stand.
    IgnoreWitnessedWill,
    /// The inbox group is committed through a record BEFORE it is delivered
    /// (`FabricExactlyOnce`'s `Buggy = 2`, `CommitEarly`).
    CommitBeforeDelivery,
    /// A drain peeks the outbox with a post still in flight, its answer not
    /// read first (`Bridge::drain_outbox`'s exactly-once wait, which aterm-0
    /// keeps: the loop reads acks only where a reply must be judged).
    PeekPastOwedPost,
    /// Every read of an owed ack waits for it, as before aterm-0
    /// (`Bridge::try_settle_one`): the loop is correct and never goes on
    /// without an answer — the control of the counts that say it does.
    WaitForEveryAck,
    /// At an idle moment the loop reads an owed POST's answer only once it
    /// has arrived, as aterm-0's first cut did (`Bridge::settle_owed`), and
    /// parks with a sender's `outbox sent` left to the idle backstop's grace.
    IdlePostLeftToBackstop,
    /// A file a power cut tore is refused where it is read, at every attach of
    /// every later boot, as before finding F1's fix
    /// (`crate::state::StateDir::set_aside_torn` never runs).
    RefuseTornAfterPowerCut,
    /// The sweep judges a file whole when its reader takes it, as finding F1's
    /// first fix did (`crate::state::StateDir::set_aside_torn_by_the_reader_alone`):
    /// an in-flight entry cut before its witness, or inside its body at an even
    /// length, still reads — and a reply's is then refused at every attach.
    SweepByTheReaderAlone,
    /// The first bridge of a boot after an unclean stop leaves every whole
    /// in-flight entry where it is, and the attach publishes each again at its
    /// sequence, as before finding F2's decision
    /// (`crate::state::StateDir::retire_in_flight_of_an_earlier_boot` never
    /// runs): an entry a power cut brought back after its record landed, and
    /// the log on its own then lost, lands behind its sender's later posts.
    RepublishInFlightOfAnEarlierBoot,
}

/// What one run is.
#[derive(Clone, Debug)]
pub(crate) struct Opts {
    pub(crate) seed: u64,
    /// Steps of the fault phase.
    pub(crate) steps: usize,
    /// The tier, or `None` for the seed's choice.
    pub(crate) tier: Option<Tier>,
    pub(crate) ep: EpRules,
    /// The escalation table the BROKER holds (the oracle always holds aterm's).
    pub(crate) table: Escalation,
    pub(crate) arm: ArmMutant,
    pub(crate) mutants: BTreeSet<Mutant>,
    pub(crate) power_cuts: bool,
    /// The broker's log may lose power ALONE ([`Stop::PowerLoss`] while the
    /// bridge and its state dir live on): a log on another volume than the
    /// state dir, or a Lossy flush failure — the move the endpoint's lane 3
    /// exists for.
    pub(crate) bus_losses: bool,
    /// How often a power cut is drawn, against a pass's 40 (default 1).
    pub(crate) cut_weight: u64,
    /// The state dir's device keeps every write (a control).
    pub(crate) lossless_device: bool,
    /// The state dir's device may tear an unflushed file (POSIX's worst
    /// case, [`Device::tears`]): on in every committed world (finding F1's
    /// law), off only for a control that needs every image whole.
    pub(crate) torn_device: bool,
    /// THE BUS MIRROR: `[fabric] log = "turns"`, the sessions' logs mirrored
    /// onto their `ev` faces.
    pub(crate) mirror: bool,
    /// HELD ACKS (aterm-0): the broker answers some acks late, after the
    /// records they name are readable ([`bus`]'s module doc), so the bridge's
    /// loop, which reads an ack only once it has arrived wherever nothing must
    /// be judged, meets every order of a delivery and its own post's ack.
    pub(crate) hold_acks: bool,
}

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
            // ONE DEVICE OR TWO, alternating by seed: an odd seed's log and
            // state dir share a device (aterm's layout: they lose power
            // together, in a power cut of the whole machine), an even seed's
            // log is on a device of its own (it loses power alone). Both at
            // once is finding F2's world, a committed sweep of its own
            // ([`super::tests`]' `both_ways`).
            power_cuts: seed % 2 == 1,
            bus_losses: seed.is_multiple_of(2),
            cut_weight: 1,
            lossless_device: false,
            // POSIX'S WORST CASE, EVERYWHERE: a power cut may tear any file it
            // did not find flushed (finding F1, [`Device::tears`]), and the
            // bridge sets what it tore aside rather than wedge on it.
            torn_device: true,
            // THE MIRROR ON OR OFF, alternating by pairs of seeds, so each
            // device layout runs with it both ways: off is the default
            // (nothing asked, taken, pinned or published), and a bridge that
            // runs the mirror runs another loop (`Bridge::mirror_then_round`).
            mirror: (seed / 2) % 2 == 1,
            // HELD ACKS ON OR OFF, alternating by fours, so each layout and
            // each mirror setting runs both ways: a broker that answers as it
            // serves, and one that answers some acks after their records are
            // readable.
            hold_acks: (seed / 4) % 2 == 1,
        }
    }

    /// `self` with one bridge rule switched off.
    pub(crate) fn with(mut self, m: Mutant) -> Self {
        self.mutants.insert(m);
        self
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
    /// What the model's tap followed: offsets opened, steps it was asked to
    /// explain, and the model actions that explained them.
    pub(crate) model: (u64, u64, BTreeSet<&'static str>),
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
    /// Ghost rows the bridge's contract leaves to the operator (finding F4).
    pub(crate) ghosts_left: u64,
    /// `ms` of the live sessions' logs the bus held once settled.
    pub(crate) mirrored: u64,
    /// Torn state files a bridge moved into the state dir's quarantine after a
    /// power cut (`crate::state::StateDir::set_aside_torn`), counted once
    /// settled.
    pub(crate) set_aside: u64,
    /// Of those, how many the bridges told aterm of (`link set-aside`): never
    /// more than were set aside (a bridge killed between the move and the
    /// telling tells fewer).
    pub(crate) set_aside_told: u64,
    /// Whole in-flight entries the first bridge of a boot retired into the
    /// quarantine after an unclean stop, never published again
    /// (`crate::state::StateDir::retire_in_flight_of_an_earlier_boot`, finding
    /// F2), counted once settled.
    pub(crate) retired: u64,
    /// HELD ACKS ([`Opts::hold_acks`], [`bus::Broker::held_counts`]): answers
    /// the broker held, reads that must not wait which found one not yet
    /// there, and records a reader was handed before their publish's ack.
    pub(crate) acks_held: u64,
    pub(crate) tries_not_yet: u64,
    pub(crate) seen_before_ack: u64,
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
    /// Whether a power cut tore `logpin`, the mirror's pin: the one tear the
    /// bridge keeps where it is (it holds the node's alias key and lifetime
    /// total, rulings Q6 and Q13), so the mirror stops for good
    /// ([`World::mirror_stopped_by_its_torn_pin`]).
    logpin_torn: bool,
    /// The next local id and sid generation aterm mints.
    next_local: u64,
    epoch: u64,
    marker: u64,
    sent: BTreeMap<String, Sent>,
    /// Every halt row the person published, in order: `(marker, on)`.
    halt_rows: Vec<(String, bool)>,
    peer_seq: u64,
    person_seq: u64,
    violations: Vec<String>,
    stats: Stats,
    /// The lowest offset a broker restart cut back to: an offset at or above
    /// it may name another record now than when it was handed out.
    lost_floor: Option<u64>,
    /// TIER-1 OF `FabricExactlyOnce`: every inbox offset the world's messages
    /// took, followed through the model after every step ([`super::conform`]).
    tap: conform::Tap,
}

/// The producer id of `principal` (astream's derivation).
fn producer(principal: &str) -> u64 {
    astream_cap::producer_id_of(principal)
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
        let mut device = Device::new(&state_dir);
        device.lossless = opts.lossless_device;
        device.tears = opts.torn_device;
        // The node is provisioned before any bridge runs (`fabric on`), durably.
        device.seed(&state_dir.join("node"), format!("{NODE}\n").as_bytes());
        let broker = bus::Broker::new(tier, opts.table.clone());
        lock(&broker.state).mutant = opts.arm;
        if opts.hold_acks {
            // A stream of its own, so the holds shift no other draw.
            broker.hold_acks(Rng::new(opts.seed ^ 0x0a0c_4e1d_ac45));
        }
        for p in [NODE, PEER, PERSON] {
            broker.name_producer(producer(p), p);
        }
        let core = Core::new(opts.seed, broker, device);
        *lock(&core.mutants) = opts.mutants.clone();
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
            logpin_torn: false,
            next_local: 1,
            epoch: 1,
            marker: 0,
            sent: BTreeMap::new(),
            halt_rows: Vec::new(),
            peer_seq: 0,
            person_seq: 0,
            violations: Vec::new(),
            lost_floor: None,
            tap: conform::Tap::new(aterm_models_session::fabric_exactly_once_model()),
            stats: Stats::default(),
        };
        w.note(format!("world seed={} tier={tier:?}", w.opts.seed));
        w.open_sessions();
        w
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
    /// Two are always kept, so the sessions still have someone to post to.
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
    /// one, and the push lane's line that accompanies a timeline record.
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
    fn collect_notes(&self) {
        for n in self.core.broker.take_notes() {
            self.core.note(n);
        }
        let reqs = std::mem::take(&mut lock(&self.ep.state).requests);
        for r in reqs {
            self.core.note(format!("ctl {r}"));
        }
    }

    // ---------------------------------------------------------------------
    // the bridge process
    // ---------------------------------------------------------------------

    /// Launch a bridge, as aterm's supervisor does.
    fn launch(&mut self) {
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
    /// is dropped by the caller), or a real panic, which is a violation.
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

    // ---------------------------------------------------------------------
    // the world's own actions
    // ---------------------------------------------------------------------

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
    /// person.
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
    /// what the law lets it, the state dir keeps what the device lets it,
    /// aterm and its sessions are gone, and the next boot begins.
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
                self.logpin_torn = true;
            }
            self.note(n);
        }
        // aterm is gone with its sessions; the relaunch mints new ones.
        self.ep.power_cut();
        self.boot = self.boot.wrapping_add(1).max(1);
        self.epoch += 1;
        self.core.broker.start();
        self.open_sessions();
    }

    /// ONE STEP OF THE FAULT PHASE.
    fn fault_step(&mut self) {
        let up = lock(&self.core.broker.state).up;
        let readable = self.core.readable();
        let alive = self.live.is_some();
        // (weight, action) for every action possible now.
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
                // The next publishes of this node's producer, and the
                // bridge's group commits, are refused, as a batch whose
                // append or flush failed is: not known to be permanent, and
                // the publish behind it still lands.
                let n = 1 + u32::try_from(self.rng.below(2)).unwrap_or(0);
                lock(&self.core.broker.state).refuse_next = n;
                self.note(format!(
                    "broker refuses the next {n} of the node's publishes and commits"
                ));
            }
            15 => self.log_record(),
            16 => self.open_one(),
            17 => self.exit_one(),
            // A batch's flush lands between two of the bridge's passes: the
            // acks the broker held are sent, for a try to find.
            18 => {
                self.core.broker.answer_held(false);
            }
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
        if !lock(&self.core.broker.state).up {
            self.core.broker.start();
        }
        // No more reordering: what the broker held is answered, and it holds
        // nothing from now on.
        self.core.broker.answer_held(true);
        let started = self.core.elapsed();
        let mut last = self.fingerprint();
        let mut still = 0;
        // A HANG DETECTOR, in virtual time: half an hour of the world's clock
        // with work still owed is a bridge that will not do it (every retry
        // it makes parks at most `RECONNECT_MAX`), and in steps, for a loop
        // that spins without parking.
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
                // Quiet for a minute and a half of the world's clock with no
                // post or receipt owed — and presence and the mirror caught
                // up, or ten minutes gone: a ghost row waits out the bridge's
                // five-minute patience and a sweep past it, and what has not
                // converged by then the oracle says.
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

    // ---------------------------------------------------------------------
    // the oracle
    // ---------------------------------------------------------------------

    /// The text markers of the bus's records on this node's and the person's
    /// lanes: `marker -> [(offset, subject, body)]`.
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
            // ONLY A COMMIT OWED ON THE LOG THE BROKER SERVES NOW: one the bridge
            // accounted on a log a move has since replaced names that log's
            // records — at the offset, perhaps one the move took — and the next
            // attach clamps it before anything is committed. Read as this
            // offset's, it was a commit owed through a record nobody delivered.
            owed: self
                .live
                .as_ref()
                .and_then(|l| l.bridge.owed_inbox_commit())
                .and_then(|(upto, on)| on.is_none_or(|on| on == ident).then_some(upto)),
            sessions,
        }
    }

    /// The last presence row of every session face under this node, `sid ->
    /// (state=, inc=)`: the face is a last-value one, so the row at the
    /// highest offset is what every reader takes for the session.
    fn presence_rows(&self) -> BTreeMap<String, (String, Option<u64>)> {
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
            out.insert(sid.to_string(), (state, inc));
        }
        out
    }

    /// THE INCARNATIONS WHOSE ROWS THE RUNNING BRIDGE MAY RETIRE, by the
    /// contract (`adoptable`'s doc) and read off the BUS, never asked of the
    /// bridge, so a bridge that narrows its own rule or loses its bookkeeping
    /// is judged by the rule it was meant to keep: its own incarnation — the
    /// newest node row, `live`, since every attach takes a fresh one — and the
    /// incarnation whose WILL was the newest node row when that attach read the
    /// face, the row just before the attach's first.
    fn retirable_incarnations(&self) -> BTreeSet<u64> {
        let mut out = BTreeSet::new();
        if self.live.is_none() {
            return out;
        }
        let face = crate::subject::node_face(FLEET, NODE, "presence");
        let rows: Vec<(String, Option<u64>)> = self
            .core
            .broker
            .records()
            .into_iter()
            .filter(|(_, subject, _, _)| *subject == face)
            .map(|(_, _, body, _)| {
                let (b, _) = Body::decode(&body);
                let state = b.unknown.get("state").cloned().unwrap_or_default();
                (state, b.unknown.get("inc").and_then(|i| i.parse().ok()))
            })
            .collect();
        let Some((state, Some(me))) = rows.last().cloned() else {
            return out;
        };
        if state != "live" {
            return out;
        }
        out.insert(me);
        let first = rows.iter().position(|(_, inc)| *inc == Some(me));
        if let Some((state, Some(dead))) = first
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| rows.get(i))
        {
            if crate::glance::is_will(state) {
                out.insert(*dead);
            }
        }
        out
    }

    /// PRESENCE, once settled: every hosted session's row says `live`, and no
    /// row of a session that left — its tab closed, or its instance gone in a
    /// power cut with nobody to say so — still does, unless the bridge's own
    /// contract leaves it to the operator: a ghost row is retired only by the
    /// incarnation that wrote it or the one that witnessed that incarnation's
    /// will ([`World::retirable_incarnations`], the contract `adoptable`
    /// keeps, so a live sibling sharing the node id is never touched), and one
    /// whose witnesses all died inside the five-minute patience is `aterm
    /// fabric doctor --retire-ghosts`'s (finding F4). The second list is those,
    /// by sid.
    fn presence_gaps(&self) -> (Vec<String>, Vec<String>) {
        let rows = self.presence_rows();
        let retirable = self.retirable_incarnations();
        let s = lock(&self.ep.state);
        let mut bad = Vec::new();
        let mut left = Vec::new();
        for session in s.sessions.values() {
            let row = rows.get(&session.sid).map(|(state, _)| state.as_str());
            if row != Some("live") {
                bad.push(format!(
                    "presence: {} is hosted and its row says {row:?}",
                    session.sid
                ));
            }
        }
        for session in &s.gone {
            let Some((state, inc)) = rows.get(&session.sid) else {
                continue;
            };
            if state != "live" {
                continue;
            }
            if inc.is_some_and(|inc| retirable.contains(&inc)) {
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
    /// `(ep, seq)` as every reader does: per `ms`, the distinct records that
    /// hold it, and the covers.
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
    /// `ReaderViewAtMostOnce`): no `ms` of a session's log is two records in
    /// the reader's view.
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

    /// Whether the mirror stopped FOR GOOD, as the rulings have it, because a
    /// power cut tore its pin: the newest bridge said `link mirror stopped
    /// reason=pin`, and a power cut tore `logpin`. The pin holds the node's
    /// alias key and lifetime total (rulings Q6, Q13), which no reboot ends
    /// and nothing else holds, so the bridge keeps a torn one where it is and
    /// the mirror refuses it rather than mint a fresh key and count from zero
    /// (`crate::state::StateDir::set_aside_torn` never moves it). Completeness
    /// is then not the mirror's law; its at-most-once laws still are. A stop
    /// for the pin with no tear behind it is a violation
    /// ([`World::mirror_stop_unexplained`]).
    fn mirror_stopped_by_its_torn_pin(&self) -> bool {
        self.logpin_torn && lock(&self.ep.state).mirror_said == "stopped reason=pin"
    }

    /// A mirror stopped for its pin when no power cut tore the pin: the bridge
    /// refused one it wrote whole.
    fn mirror_stop_unexplained(&self) -> Option<String> {
        let said = lock(&self.ep.state).mirror_said.clone();
        (said == "stopped reason=pin" && !self.logpin_torn)
            .then(|| "mirror: stopped for its pin, which no power cut tore".to_string())
    }

    /// THE MIRROR, once settled (`LogMirror`'s `QuiescentMirrorComplete`):
    /// every hosted session's log landed through its `ms` head, and every
    /// `ms` up to it is a record in the reader's view or covered by a gap —
    /// across every move of the log, whose cuts the mirror must say again.
    /// Not once a torn pin stopped it ([`World::mirror_stopped_by_its_torn_pin`]).
    fn mirror_gaps(&mut self) -> Vec<String> {
        if self.mirror_stopped_by_its_torn_pin() {
            return Vec::new();
        }
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
    /// record of a session log is on its `ev` face twice before the reader's
    /// dedup. A bridge killed at any step of a batch leaves the batch pinned,
    /// and its successor republishes it at the pinned sequences, which the
    /// broker dedups; a batch taken again at fresh sequences would put a
    /// second copy on the bus. (A move legitimately does: its cursors start
    /// over, and what the new log still holds below the fork is sent again,
    /// one copy each reader dedups.)
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
    /// bridge's patience) and the mirror.
    fn converged(&mut self) -> bool {
        self.presence_gaps().0.is_empty() && self.mirror_gaps().is_empty()
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
        // NO SENDER LEFT WAITING BY AN IDLE LOOP: whenever the bridge parked
        // with nothing queued, no post of a session's was a record whose ack
        // the broker still held ([`Process::idle`]) — its `outbox sent`, which
        // a `post --wait` and the mail-landed wake wait for, would have
        // waited for the idle backstop's grace with the loop asleep.
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
        // NO LOSS: every message on the bus for a live session is in its ring.
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
        // POSTS: retired, and where they said.
        for session in s.sessions.values() {
            for p in &session.posts {
                let marker = String::from_utf8_lossy(&p.body).into_owned();
                match p.retired.as_deref() {
                    None => bad.push(format!(
                        "post {} #{} {marker} never retired",
                        session.sid, p.id
                    )),
                    // Posted to a session that left before the bridge routed
                    // it: no presence row says it is anywhere.
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
                            // A cut took it: lawful only if unescalated, which
                            // the broker's law check holds.
                            (Some(_), None) => {}
                            // Retired at an offset a later cut took: the endpoint
                            // forgot it (`forget_bus_offsets`), and the post is on
                            // the bus once (the dup check), wherever it is now.
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
        // PER-SENDER ORDER.
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
        // RECEIPTS: at most one answer per decision on the bus, and each one
        // names the ask it answers — an `ask` on the decider's own lane — on
        // THIS log: an answer whose `re=` a move took names another record.
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
        // HALTS.
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
        // NO ALARM.
        for (_, subject, body, _) in self.core.broker.records() {
            if !subject.starts_with(&format!("/f/{FLEET}/pub/{NODE}/")) {
                continue;
            }
            let text = crate::pct::decode(&String::from_utf8_lossy(&body));
            for alarm in ["cap-compromised", "forged-self"] {
                if text.contains(alarm) {
                    bad.push(format!("alarm: {subject}: {text}"));
                }
            }
        }
        let (presence, left) = self.presence_gaps();
        bad.extend(presence);
        self.stats.ghosts_left = left.len() as u64;
        bad.extend(self.mirror_gaps());
        bad.extend(self.mirror_stop_unexplained());
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
        // THE QUARANTINE, read as the owner reads it: a torn file under its
        // kind's name, a retired in-flight entry under `retired.` (F2).
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
        self.stats.set_aside_told = lock(&self.ep.state).set_aside_told;
        if self.stats.set_aside_told > self.stats.set_aside {
            bad.push(format!(
                "set-aside: the bridges told aterm of {} torn files, and the quarantine holds {}",
                self.stats.set_aside_told, self.stats.set_aside
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

/// RUN ONE SEED: the fault phase, the settle phase, the oracle.
pub(crate) fn run(opts: &Opts) -> Outcome {
    let mut w = World::new(opts.clone());
    w.launch();
    for _ in 0..opts.steps {
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
    }
    let trace = w.core.trace();
    // End the bridge before the world goes: its threads, its lanes.
    if let Some(live) = w.live.take() {
        live.process.kill();
    }
    (
        w.stats.acks_held,
        w.stats.tries_not_yet,
        w.stats.seen_before_ack,
    ) = w.core.broker.held_counts();
    Outcome {
        seed: opts.seed,
        tier: w.tier,
        violations: w.violations,
        trace,
        stats: w.stats,
        model: (w.tap.opened, w.tap.checked, w.tap.fired.clone()),
    }
}

/// Whether `text` is one of the world's message markers (`m<digits>`).
fn is_marker(text: &str) -> bool {
    text.strip_prefix('m')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}
