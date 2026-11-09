// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SIMULATED BROKER HELD TO ASTREAM'S OWN (sim item B01): a differential
//! test of [`super::bus`]'s model against a real `astream_broker::Broker`.
//!
//! The simulation's exactly-once claims (the seeded worlds' laws,
//! `FabricExactlyOnce`'s conformance in [`super::conform`]) rest on the model
//! answering the bridge as astream does. This test plays one script on both,
//! request by request, and compares the answers, so a pin bump of
//! `vendor/astream` the model does not follow fails here.
//!
//! ## The two arms
//!
//! * REAL: `Broker::open_opts` on a scratch directory, opened as aterm opens
//!   its own: [`crate::durability::Durability::options`] (the tier, the
//!   100 ms Ordered horizon, the escalation table on the weak tiers, aterm's
//!   16 MiB residency budget), `Boot::Known` (a new boot after each power
//!   loss; `Boot::Unknown` after a [`Step::RebootUnnamed`]) and `Volume::Boot`
//!   (so a same-boot unclean stop in a named boot keeps the generation),
//!   unguarded, and aterm's rules set at every open (the
//!   world's, `Opts::rules`), or for the journal of a world whose broker was
//!   unretained, never: what `aterm link broker` serves when setting the rules
//!   fails. Its retention tick's interval is a year, and the tick setting the
//!   rules asks for at once is waited for before anything is served. Served on
//!   a scratch Unix socket and read with astream's codec. The SEALED arm seals
//!   its log under a key, as `aterm link broker` seals one beside its
//!   `rest.key`; the COLD arm (critic item F06) keeps a residency budget of 0
//!   in place of aterm's, so every record a read takes below the head comes
//!   back from the file through astream's paged cold tier, as most of the
//!   history the shipped broker serves does.
//! * MODEL: `bus::Broker::new` with the same table, retained as the real arm
//!   is, each connection a raw [`bus::SimStream`]. No held acks, race or
//!   mutant, and a rolled-back batch only on the refinement arm (below); its
//!   tails write what they match before a retention lands
//!   (`bus::State::eager_tails`), as astream's tail thread does, and an open
//!   that sets the rules takes astream's first tick (`bus::Broker::open_tick`).
//!
//! ## Retention's steps (sim item B05)
//!
//! A [`Step::Tick`] runs the tick on both arms at a time on the run's
//! [`Clock`], an hour past the wall clock, so astream's own first tick at an
//! open (by the wall clock) never stamps later than a script; a
//! [`Step::Cut`] is `retain_before_carrying_state` on both, after a tick so
//! both plan on one clock; a [`Step::StartUnretained`] opens setting no
//! rules, and a [`Step::StartWith`] rules changed outside aterm's (`world::changed_rules`). Each is compared by the
//! base it leaves, and what follows by every answer below. The replay ticks
//! before every stop, so what an open finds was stamped by a script's tick,
//! and compares a world's deliveries where the model owes one
//! ([`Step::DeliverAny`]): a world's tail reads lazily, and the replay's ticks
//! move retention off the world's timeline.
//!
//! ## What is compared
//!
//! Each decoded answer after one projection ([`project`]): a `PublishAck`'s
//! offset and `deduped`; a `Delivery`, a `Budget` and a `Gap` whole; a
//! refusal's code and astream-defined prefix class ([`class`]), never its
//! words; a `Mark`'s `next`, `head`, `resume` and `oldest`, and its `durable`
//! exactly on Strict and by [`Law`] on Ordered and Relaxed (monotone, at most
//! the head, at least one past every escalated ack); a `Hello`'s generation,
//! lineage, served tier, request set, retention rules and whether it names its log's
//! device (critic item F07), never its nonce, log id, health or device number. A tail's `Mark`s are skipped: the model sends none.
//! Since the log ids differ, a conditional `Retire` naming the model's log is
//! sent to the real arm naming the real log at the same generation
//! (`Real::on_this_log`); one naming any other log goes as it is.
//!
//! The scripts are hand-written ones per verb and edge ([`scripts`],
//! [`long`]) and the door's journal of the seeded [`WORLDS`] ([`replay`]):
//! what real bridges sent, on every tier with the mirror off, onto a retained
//! broker and onto an unretained one, with the world's retention ticks, cuts
//! and days passing (`scripts::retention` is retention's own hand script).
//! Every connection is ended before a restart, so a will fires at a point the
//! script decides. A real will is waited for by `BrokerStats::wills_settled`,
//! a real tail by its reader thread, the tick an open asks for by
//! `BrokerStats::retention_ticks`; each wait is a one-minute hang detector.
//!
//! Negative controls: the model with an [`ArmMutant`] must diverge: a
//! forgotten dedup or will fence, an altered payload, each drift this test
//! found and `bus.rs` fixed, and retention's: a `Fetch` that clamps below the
//! floor, a cut that keeps the dedup keys below it, a tail never told a gap,
//! a told reader's `Fetch` refused as any other or read without its notice
//! ([`drift`]).
//!
//! ## The refinement arm (critic item F10)
//!
//! The model is the bridge's adversary: a seed picks any cut at or above the
//! durable head that P2-P7 allow (`bus::State::allowed_cuts`,
//! [`bus::law_check`]) and rolls back the node's batches when a world says so
//! (`bus::State::refuse_next`). The file arms cannot check that the real store
//! stays inside that law, as a file keeps every record at a power loss and no
//! write of one fails. The REFINEMENT ARM ([`run_refining`]) replays the
//! door's journal of each world of [`REFINED`] (power cuts, bus losses and
//! rolled-back batches on, unretained) on a real broker whose log is on
//! astream's simulated device (`Broker::open_sim`, a test seam of astream's),
//! every answer compared as above, and adds three checks:
//!
//! * at each power loss, the images the device could be left in
//!   (`SimDevice::power_loss_images`: every one while an epoch's subsets fit
//!   [`IMAGES`], a seeded sample of them past that) are opened as the next boot
//!   opens it, and each must keep a head among the model's allowed cuts, the
//!   model's records below it, and an identity and lineage `law_check` admits
//!   ([`refine`]);
//! * each batch the model rolls back, a publish's, a group commit's or a will's
//!   a connection's end fires, is one whose real write fails half-way
//!   (`InjectedFaults::write`), answered as the model answers it
//!   ([`answer_refused`], [`end`]);
//! * after every step the real head is the model's, so nothing of a rolled-back
//!   batch is visible.
//!
//! Ordered keeps no horizon there, on both arms (`bus::State::no_horizon`): a
//! horizon's full flush is the wall clock's, and the model's replay never runs
//! one, so it could only make the real store keep more than the law asks; with
//! none, what an image loses is the script's doing alone and the same each run.
//! Its control ([`ArmMutant::AckIsDurable`]) is a model whose allowed cuts keep
//! every acked record: on Ordered (a barrier taken for a full flush) and on
//! Relaxed (a page-cache write taken for one) the images below the head must
//! fall outside it.
//!
//! ## WHAT IT DOES NOT COVER
//!
//! * A GUARDED broker: `Attach` is a `Mark` on both arms and nothing is
//!   checked; the table's principal rule (`h-`) applies on a real broker only
//!   to a producer a bound grant attached, and the refusals a capability draws
//!   (expired, poisoned, unauthorized) are not drawn.
//! * RETENTION AT SCALED BYTES: a world whose rules are scaled to a
//!   few records is not replayed (the real broker's writer asks for the ticks a
//!   class past its room needs, by the wall clock, not the run's), so a byte
//!   give-up is held to astream by astream's own test of the planner it
//!   exports (`retention_planner`), not here. Nor the `cut: ` retire refusal:
//!   aterm makes no uncarried cut.
//! * A TAIL THAT HAS YET TO READ WHEN A CUT LANDS: the worlds explore it (told
//!   the gap instead of handed the records); both arms here write eagerly.
//! * POWER-LOSS CUTS ON A REAL DISK: the file arms' power loss keeps the whole log;
//!   the cuts are the refinement arm's, on astream's simulated device, whose images
//!   are a block-level contract (fcntl(2)'s barrier order, a volatile block landing
//!   as it reads at the loss, never as an earlier write of it left it), at most
//!   [`IMAGES`] for each prefix of the barrier epochs and a seeded sample past
//!   that (none and all always among it), so a loss that needs particular blocks
//!   among many is tried only if the sample lands on it. Nor the Ordered horizon's
//!   full flush between batches: the refinement arm keeps none (above). A recovery
//!   astream settles as ANOTHER LOG (no record kept: a fresh id at generation 0)
//!   is one [`bus::law_check`] admits (`Restart::another_log`); the model never
//!   mints one, so no seeded world explores the bridge's answer to it
//!   (`LogIdentity::continues` answers none for another log, and the reader
//!   forgets its offsets).
//! * AN UNCLEAN STOP IN A BOOT THE OPEN CANNOT NAME: aterm opens in
//!   `Boot::Unknown` on a host whose kernel names no boot
//!   (`durability::open_with`), and astream then takes every unclean stop of a
//!   weak tier for a power loss ('it served in a boot it could not name') and
//!   moves the generation. P6 admits no move at a crash, so that is outside the
//!   model's law: a script follows a [`Step::RebootUnnamed`] with clean stops
//!   alone until the next power loss (the runner refuses another), and the
//!   worlds' power losses reopen in a named boot. The two unnamed reopens of
//!   `scripts::restarts` are compared.
//! * A BATCH ASTREAM ROLLS BACK THAT NO FAULT CAN NAME: the refinement arm fails the
//!   real write of each batch the model rolls back that a script sends alone or a
//!   connection's end fires; never a will an open fires (astream fires them inside
//!   its open, before a fault can be armed: [`held_over_the_open`]), and a script
//!   pipelining a write while one is owed is refused. A failed FULL FLUSH (on a
//!   weak tier it poisons the log) is astream's own test (`poisoned_log`), not here.
//! * A WILL LIVE AT A RESTART: astream's connection threads race its
//!   shutdown; the model fires each producer's latest will at the next open,
//!   one outcome the race allows, and the scripts end every connection first.
//! * TIME: a tail's catch-up, idle and durable marks and the liveness bound
//!   they arm, the Ordered horizon's timer (judged only by [`Law`]), the
//!   retention tick by its own wall clock (the scripts' ticks are the run's).
//! * `Last`'s SCAN BOUND: a round of astream's visits at most `LAST_SCAN_MAX`
//!   (65,536) index entries from the filter's literal prefix; one that stops
//!   on an entry the filter matches answers a short page resuming there, and
//!   past `LAST_RESUME_ROUNDS` (64) rounds the answer is an error. The model
//!   scans without a bound; no script's index holds 65,536 entries.
//! * A replica's refusals, and verbs the bridge never sends (`ForkSubscribe`,
//!   `ProcessAndProduce`, `Replicate`).
//! * Concurrency: one request at a time; batches and the order of an ack
//!   against a tail's read are the held-ack worlds'.
//! * A conditional `Retire` inside a GROUP-COMMIT BATCH: astream answers one
//!   naming a record staged in the same batch moot, one for an absent subject
//!   at the head readers saw before the batch, and one whose subject a retire
//!   not yet visible took at that retire's offset; the model commits each
//!   request alone. Only a client that guesses an offset before its ack names
//!   an uncommitted record, and the bridge sends each retire alone with no ack
//!   owed, so a pipelined script never names one subject twice or draws a
//!   moot answer for an absent one.
//! * Off Apple, the Ordered tier: astream serves it Strict (`NotApple`).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use astream_broker::proto::{
    decode_response, encode_request, read_frame, write_frame, Request, Response,
};
use astream_broker::{Boot, Volume, ACK_SEQ_BASE};

use super::bus::{self, ArmMutant, Escalation, Event, Stop, Tier};
use super::device::Device;
use super::{lock, Core, Process};
use crate::durability::Durability;

/// The hang detector: a wait for what must happen.
const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The boot both arms run in until the first power loss ([`Step::PowerLoss`], [`Step::Reboot`]).
const BOOT: [u8; 16] = *b"aterm-sim-B01-bt";

/// The boot the real arm serves in after `losses` power losses: [`BOOT`], then a new one after each,
/// which its open names as `aterm link broker`'s does (`Boot::Detect`, the kernel's boot session):
/// an unclean stop in the boot it served in keeps the generation, one across a power loss moves it
/// on a weak tier, as the model's do.
fn boot_after(losses: u64) -> Boot {
    let mut id = BOOT;
    if losses > 0 {
        id[8..].copy_from_slice(&losses.to_be_bytes());
    }
    Boot::Known(id)
}

/// The refinement arm's device blocks, in bytes: the smallest the crash simulation sweeps, so a
/// write tears at the most places.
const BLOCK: usize = 16;

/// The images [`refine`] reopens at most for each prefix of a loss's barrier epochs: every subset
/// of up to six blocks, a sample past that (`SimDevice::power_loss_images`).
const IMAGES: usize = 64;

/// The connection a world producer's writes ride ([`Event::Publish`]): the
/// journal's connections count from 1.
const WORLD: u64 = 0;

/// One step of a script, played on both arms.
#[derive(Clone, Debug)]
pub(crate) enum Step {
    /// Open connection `c`.
    Dial(u64),
    /// Send one request on `c` and read its whole answer (none for a subscribe: `c` is a tail from then on).
    Send(u64, Request),
    /// Send every request on `c` before reading any answer, then read each.
    Pipeline(u64, Vec<Request>),
    /// Read the next delivery of tail `c`.
    Deliver(u64),
    /// A world's delivery replayed ([`replay`]): the next frame of tail `c` when the model owes
    /// one, compared with the real tail's; none when it owes none, the real tail not read. A
    /// world's tail reads lazily and the arms' eagerly, and the replay's own ticks move
    /// retention off the world's timeline, so the world's count of frames is no claim.
    DeliverAny(u64),
    /// The client drops `c`.
    End(u64),
    /// Every connection ends, then the broker stops.
    Stop(Stop),
    /// The broker opens again.
    Start,
    /// Every connection ends, the machine loses power, and the broker opens again in the next boot,
    /// every record kept.
    Reboot,
    /// A [`Step::Reboot`] whose open cannot name its boot (`Boot::Unknown`: aterm's open on a host
    /// whose kernel names none, `durability::open_with`), nor can any open after it until the next
    /// power loss. Astream then takes every unclean stop for a power loss, so a script follows it
    /// with clean stops alone until that loss (WHAT IT DOES NOT COVER).
    RebootUnnamed,
    /// A [`Step::Reboot`]'s stop alone, a [`Step::Start`] to follow (a journal's `Stop(PowerLoss)`): every
    /// connection ends and the machine loses power, the model keeping every record. On a simulated
    /// device the images the loss could leave (at most [`IMAGES`] for each prefix of its barrier
    /// epochs, a seeded sample past that) are first reopened and held to the model's law
    /// ([`refine`]).
    PowerLoss,
    /// The model is told producer `p` is a node's (`bus::Broker::name_producer`), which its
    /// rolled-back batches pick by; the real arm binds no principal.
    Name(u64, String),
    /// The model rolls back the next this-many of a node's publishes and group commits
    /// (`bus::State::refuse_next`), and the real arm's write fails for each one it refuses
    /// (`astream_broker::InjectedFaults::write`, [`answer_refused`]).
    Refuse(u32),
    /// The broker opens again setting no rules: what `aterm link broker` serves when setting them fails.
    StartUnretained,
    /// The broker opens again setting these rules, not its own: a change no aterm build makes
    /// (`world::changed_rules`).
    StartWith(astream_broker::Retention),
    /// A retention tick at this many ms past the run's clock ([`Clock`]).
    Tick(u64),
    /// A cut at this offset, carrying state (`retain_before_carrying_state`); a [`Step::Tick`] comes first.
    Cut(u64),
    /// The broker's rate limit set now, until the next open (astream's `Broker::set_rate_limit`,
    /// critic item E06): every open sets aterm's own ([`crate::broker_rules::rate_limit`]).
    RateLimit(Option<astream_broker::RateLimit>),
    /// The publishes each arm's rate limit refused so far (`BrokerStats::rate_limited`), compared.
    RateLimited,
}

/// THE RUN'S CLOCK for its retention ticks: an hour past the wall clock when the run began,
/// so every tick a script asks for is later than any the real broker takes by the wall
/// clock itself (its first tick, asked for at once when its rules are set at an open), and
/// `retention::stamp`'s `now.max(last)` takes the script's time on both arms.
struct Clock(u64);

impl Clock {
    fn new() -> Self {
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        Self(wall.saturating_add(3_600_000))
    }

    fn at(&self, rel: u64) -> u64 {
        self.0.saturating_add(rel)
    }
}

/// A script that ran the same on both arms, or the first answer that did not.
type Verdict = Result<(), String>;

/// THE DURABLE LAW one arm's `Mark`s are held to on Ordered and Relaxed.
#[derive(Debug, Default)]
struct Law {
    /// The last durable head a `Mark` told.
    seen: u64,
    /// One past the newest escalated record acked so far.
    floor: u64,
}

impl Law {
    fn judge(&mut self, durable: Option<u64>, head: u64) -> Result<(), String> {
        let Some(d) = durable else {
            return Err("a Mark with no durable head".to_string());
        };
        if d > head {
            return Err(format!("durable {d} above the head {head}"));
        }
        if d < self.seen {
            return Err(format!("durable {d} below an earlier {}", self.seen));
        }
        if d < self.floor {
            return Err(format!(
                "durable {d} below an escalated record's ack, {}",
                self.floor
            ));
        }
        self.seen = d;
        Ok(())
    }
}

/// The REAL arm: astream's broker on a scratch directory.
struct Real {
    conns: BTreeMap<u64, RealConn>,
    up: Option<(astream_broker::Broker, astream_broker::BrokerHandle)>,
    durability: Durability,
    /// The rules every open sets, as `aterm link broker` opens its own (`None`: the broker
    /// it serves when setting them fails).
    retain: Option<astream_broker::Retention>,
    /// The key the log is sealed under, opened keyed and bound as `aterm link broker` opens a log
    /// with its `rest.key` (`None`: plaintext).
    at_rest: Option<[u8; 32]>,
    /// The bytes of decoded records every open keeps resident in place of aterm's own
    /// [`crate::durability::RESIDENT_BUDGET`], which [`Durability::options`] sets (`None`:
    /// aterm's). The cold arm's 0 keeps the newest record alone, so every read below the head
    /// comes from the log file through astream's paged cold tier.
    resident_budget: Option<usize>,
    /// The records earlier opens read back cold (`Broker::cold_reads`, which counts from each
    /// open): with the open that serves now's, [`Real::cold_reads`].
    cold_before: u64,
    boot: Boot,
    /// The power losses so far ([`boot_after`]).
    losses: u64,
    /// Producers whose `/a/will` record the log holds: what an open fires,
    /// one firing each.
    will_producers: BTreeSet<u64>,
    /// Wills queued to this broker's writer since its open: what
    /// `wills_settled` must reach before the log is read again.
    queued: u64,
    /// The REFINEMENT ARM's device (critic item F10): the log lives on astream's simulated
    /// device in place of the scratch directory, and each power loss's images are reopened
    /// and held to the model's law ([`refine`]). `None`: the log is a file.
    device: Option<astream_broker::SimDevice>,
    /// What the refinement arm found so far.
    refined: Refined,
    dir: Scratch,
}

/// What the refinement arm saw over one run ([`refine`], [`answer_refused`]).
#[derive(Debug, Default)]
struct Refined {
    /// Power losses whose images were reopened.
    losses: u64,
    /// Images reopened, every one a restart the model's law admits.
    images: u64,
    /// Images that lost acked records (the generation moved, or another log).
    lost: u64,
    /// Images astream opened as ANOTHER LOG (a fresh id: none of the old one's records kept).
    another_log: u64,
    /// Losses whose images were a sample, not every image the device allows.
    sampled: u64,
    /// Publishes and group commits the model rolled back and the real arm's write failed.
    refused: u64,
}

struct RealConn {
    stream: UnixStream,
    /// The producer of the will this connection holds, once one registered.
    will: Option<u64>,
    /// A tail's frames, read by a thread of its own.
    tail: Option<mpsc::Receiver<io::Result<Option<Vec<u8>>>>>,
    reader: Option<std::thread::JoinHandle<()>>,
    keep: Arc<AtomicBool>,
}

/// Unknown reader completion preserves the fixture even during assertion unwind.
struct Scratch {
    dir: Option<aterm_tempfile::TempDir>,
    keep: Arc<AtomicBool>,
}

impl Scratch {
    fn path(&self) -> &std::path::Path {
        self.dir.as_ref().expect("the fixture directory").path()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.keep.load(Ordering::Relaxed) {
            return;
        }
        if let Some(dir) = self.dir.take() {
            eprintln!(
                "KEPT broker fixture with unknown reader custody: {}",
                dir.path().display()
            );
            let _ = dir.keep();
        }
    }
}

impl RealConn {
    fn unjoined(&self, why: &str) {
        self.keep.store(true, Ordering::Relaxed);
        if !std::thread::panicking() {
            panic!("the broker tail reader could not be joined: {why}");
        }
        eprintln!("broker tail reader custody unknown: {why}");
    }
}

impl Drop for RealConn {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        let Some(reader) = self.reader.take() else {
            return;
        };
        let (complete, completion) = mpsc::channel();
        let joining = std::thread::Builder::new().spawn(move || {
            let _ = complete.send(reader.join());
        });
        match joining {
            Ok(joining) => drop(joining),
            Err(e) => {
                self.unjoined(&format!("start the join observer: {e}"));
                return;
            }
        }
        match completion.recv_timeout(HANG) {
            Ok(Ok(())) => {}
            Ok(Err(panic)) => {
                self.keep.store(true, Ordering::Relaxed);
                if !std::thread::panicking() {
                    std::panic::resume_unwind(panic);
                }
            }
            Err(e) => self.unjoined(&e.to_string()),
        }
    }
}

impl Real {
    fn new(
        tier: Tier,
        retain: Option<astream_broker::Retention>,
        at_rest: Option<[u8; 32]>,
        resident_budget: Option<usize>,
    ) -> Self {
        let mut real = Self::new_unopened(tier, retain, at_rest, resident_budget);
        real.open();
        real
    }

    fn new_unopened(
        tier: Tier,
        retain: Option<astream_broker::Retention>,
        at_rest: Option<[u8; 32]>,
        resident_budget: Option<usize>,
    ) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-b01")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("a scratch dir");
        Self {
            dir: Scratch {
                dir: Some(dir),
                keep: Arc::new(AtomicBool::new(false)),
            },
            durability: durability(tier),
            retain,
            boot: boot_after(0),
            losses: 0,
            at_rest,
            resident_budget,
            cold_before: 0,
            up: None,
            conns: BTreeMap::new(),
            will_producers: BTreeSet::new(),
            queued: 0,
            device: None,
            refined: Refined::default(),
        }
    }

    /// The REFINEMENT ARM (critic item F10): aterm's broker of `tier` with its log on a fresh
    /// simulated device of [`BLOCK`]-byte blocks, its socket in a scratch directory; unretained,
    /// since retention's rewrite is a file's and refused on the device.
    fn on_device(tier: Tier) -> Self {
        let mut real = Self::new_unopened(tier, None, None, None);
        real.device = Some(astream_broker::SimDevice::new(BLOCK));
        real.open();
        real
    }

    fn sock(&self) -> std::path::PathBuf {
        self.dir.path().join("b.sock")
    }

    fn open(&mut self) {
        let rules = self.retain.clone();
        self.open_setting(rules);
    }

    /// Whether the log has astream's binding beside it.
    fn bound_at_rest(&self) -> bool {
        self.dir.path().join("bus.log.seal").exists()
    }

    /// The files of the log that hold a subject in the clear, every script's starting `/f/f/`.
    fn holding_subjects(&self) -> Vec<String> {
        let mut holding: Vec<String> = std::fs::read_dir(self.dir.path())
            .expect("the scratch dir")
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let bytes = std::fs::read(entry.path()).ok()?;
                (name.starts_with("bus.log") && bytes.windows(5).any(|w| w == b"/f/f/"))
                    .then_some(name)
            })
            .collect();
        holding.sort();
        holding
    }

    /// What every open of this arm takes in `boot`: aterm's options ([`Durability::options`])
    /// with this arm's budget and seal, and on the refinement arm an Ordered tier with no
    /// horizon (`bus::State::no_horizon`).
    fn options(&self, boot: Boot) -> astream_broker::OpenOptions {
        let mut opts = self.durability.options();
        if let Some(budget) = self.resident_budget {
            opts.resident_budget = Some(budget);
        }
        if self.device.is_some() && self.durability == Durability::Ordered {
            opts.durability = astream_broker::Durability::Ordered { horizon: None };
        }
        opts.boot = boot;
        opts.volume = Volume::Boot;
        opts.require_bound_at_rest = self.at_rest.is_some();
        opts.at_rest_key = self.at_rest;
        opts
    }

    /// Open again setting `rules`, or none.
    fn open_setting(&mut self, rules: Option<astream_broker::Retention>) {
        let opts = self.options(self.boot);
        let broker = match &self.device {
            Some(device) => astream_broker::Broker::open_sim(device, opts),
            None => {
                astream_broker::Broker::open_opts(self.dir.path().join("bus.log"), opts, || {
                    Ok(astream_broker::Prepared::Nothing)
                })
            }
        }
        .expect("the real broker opens");
        // The tick's own clock is the wall clock: a year between its ticks, so the only
        // ones it takes by itself are the one setting the rules asks for at once and the ones
        // a batch past a class's room asks for (none under aterm's rules at their bytes).
        broker.set_retention_interval(Duration::from_secs(365 * 24 * 60 * 60));
        // As `aterm link broker` and the fabric world harness do at every open, before
        // anything is served: astream retains and advertises only the rules set since an open.
        if let Some(rules) = rules {
            broker
                .set_retention(rules)
                .expect("aterm's retention rules are set");
            // Its first tick, asked for at once (the interval's ask before it is the same
            // one): taken before anything is served, so it stamps only what the open found,
            // as the model's `open_tick` does.
            let deadline = Instant::now() + HANG;
            while {
                let stats = broker.stats();
                stats.retention_ticks + stats.retention_skips == 0
            } {
                assert!(
                    Instant::now() < deadline,
                    "the real broker never took its first retention tick"
                );
                aterm_uds::deadline::poll_yield();
            }
        }
        // And its rate limit, as `aterm link broker` sets it at every open (critic item E06).
        crate::broker_rules::limit(&broker).expect("aterm's rate limit is set");
        let handle = broker.serve(self.sock()).expect("the real broker serves");
        self.up = Some((broker, handle));
        self.queued = self.will_producers.len() as u64;
        self.settle();
    }

    fn broker(&self) -> &astream_broker::Broker {
        &self.up.as_ref().expect("the real broker is up").0
    }

    /// The records this arm read back from its log file because its budget left them cold,
    /// over every open so far (`Broker::cold_reads`): 0 for a broker that holds them all.
    fn cold_reads(&self) -> u64 {
        self.cold_before
            + self
                .up
                .as_ref()
                .map_or(0, |(broker, _)| broker.cold_reads())
    }

    /// Wait until the writer has settled every will queued to it.
    fn settle(&self) {
        let deadline = Instant::now() + HANG;
        while self.broker().stats().wills_settled < self.queued {
            assert!(
                Instant::now() < deadline,
                "the real broker never settled {} wills",
                self.queued
            );
            aterm_uds::deadline::poll_yield();
        }
    }

    fn dial(&mut self, c: u64) {
        let stream = UnixStream::connect(self.sock()).expect("dial the real broker");
        stream
            .set_read_timeout(Some(HANG))
            .expect("bound the reads");
        self.conns.insert(
            c,
            RealConn {
                stream,
                will: None,
                tail: None,
                reader: None,
                keep: Arc::clone(&self.dir.keep),
            },
        );
    }

    /// `req` as this arm takes it: a conditional `Retire` that names the model's log
    /// ([`bus::LOG_ID`], what a script or a world's bridge read off the model's `Hello`)
    /// names this broker's log at the same generation, which the `Hello`s are compared on.
    fn on_this_log(&self, req: &Request) -> Request {
        match req {
            Request::Retire {
                producer_id,
                subject,
                offset,
                log,
            } if log.id == bus::LOG_ID => Request::Retire {
                producer_id: *producer_id,
                subject: subject.clone(),
                offset: *offset,
                log: astream_broker::LogIdentity {
                    id: self.broker().log_identity().id,
                    generation: log.generation,
                },
            },
            other => other.clone(),
        }
    }

    fn write(&mut self, c: u64, reqs: &[Request]) {
        let reqs: Vec<Request> = reqs.iter().map(|r| self.on_this_log(r)).collect();
        let conn = self.conns.get_mut(&c).expect("a live real connection");
        let mut bytes = Vec::new();
        for r in &reqs {
            write_frame(&mut bytes, &encode_request(r)).expect("frame to memory");
        }
        conn.stream
            .write_all(&bytes)
            .expect("write to the real broker");
    }

    /// The next answer on `c`. A simulated device shares no device with any file, so the
    /// refinement arm's `Hello` names none (astream's `Backing::device`), where the model's
    /// names the Apple device it simulates: that arm's `Hello` is read as naming one, every
    /// other word of it compared as the file arms' are.
    fn read(&mut self, c: u64) -> Response {
        let conn = self.conns.get_mut(&c).expect("a live real connection");
        let mut resp = match read_frame(&mut conn.stream) {
            Ok(Some(frame)) => decode_response(&frame).expect("a frame astream decodes"),
            Ok(None) => panic!("the real broker closed connection {c}"),
            Err(e) => panic!("no answer from the real broker on {c} in {HANG:?}: {e}"),
        };
        if let (Some(_), Response::Nonce { ext, .. }) = (&self.device, &mut resp) {
            ext.device.get_or_insert(astream_broker::LogDevice {
                dev: 0,
                flush_drains_device: true,
            });
        }
        resp
    }

    /// `c` is a tail now: a thread reads its frames.
    fn tail(&mut self, c: u64) {
        let conn = self.conns.get_mut(&c).expect("a live real connection");
        assert!(
            conn.tail.is_none() && conn.reader.is_none(),
            "already a tail"
        );
        let mut reader = conn.stream.try_clone().expect("clone the tail");
        let _ = reader.set_read_timeout(None);
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || loop {
            let frame = read_frame(&mut reader);
            let end = !matches!(frame, Ok(Some(_)));
            if tx.send(frame).is_err() || end {
                return;
            }
        });
        conn.reader = Some(worker);
        conn.tail = Some(rx);
    }

    /// The next delivery of tail `c`, skipping its marks; `None` at its end.
    fn deliver(&mut self, c: u64) -> Option<Response> {
        let deadline = Instant::now() + HANG;
        let rx = self
            .conns
            .get(&c)
            .and_then(|conn| conn.tail.as_ref())
            .expect("a real tail");
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .unwrap_or_else(|| {
                    panic!("the real broker delivered nothing on tail {c} in {HANG:?}")
                });
            match rx.recv_timeout(remaining) {
                Ok(Ok(Some(frame))) => match decode_response(&frame) {
                    Some(Response::Mark { .. }) => {}
                    other => return other,
                },
                Ok(Ok(None) | Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => return None,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("the real broker delivered nothing on tail {c} in {HANG:?}")
                }
            }
        }
    }

    fn end(&mut self, c: u64) {
        let Some(conn) = self.conns.remove(&c) else {
            return;
        };
        let will = conn.will.is_some();
        drop(conn);
        if will {
            self.queued += 1;
            self.settle();
        }
    }

    fn end_all(&mut self) {
        let ids: Vec<u64> = self.conns.keys().copied().collect();
        for c in ids {
            self.end(c);
        }
    }

    fn stop(&mut self, how: Stop) {
        self.end_all();
        let (broker, mut handle) = self.up.take().expect("the real broker is up");
        if how != Stop::Clean {
            broker.arm_unclean_stop();
        }
        handle.shutdown();
        self.cold_before += broker.cold_reads();
        drop(broker);
    }

    /// The machine loses power: the broker stops as a killed process does, and its next open
    /// is in the next boot ([`boot_after`]), or, not `named`, in one it cannot name
    /// ([`Step::RebootUnnamed`]). The device keeps every record, as a file does; the images a
    /// power loss could leave are [`refine`]'s.
    fn lose_power(&mut self, named: bool) {
        self.stop(Stop::Crash);
        self.losses += 1;
        self.boot = if named {
            boot_after(self.losses)
        } else {
            Boot::Unknown
        };
    }

    /// A retention tick at `now_ms`; the log's base after it.
    fn tick(&self, now_ms: u64) -> u64 {
        let b = self.broker();
        b.retention_tick_at(now_ms)
            .expect("the real broker's retention tick");
        b.base()
    }

    /// A carried cut at `at`; the log's base after it.
    fn cut(&self, at: u64) -> u64 {
        let b = self.broker();
        b.retain_before_carrying_state(at)
            .expect("the real broker's carried cut");
        b.base()
    }
}

/// The MODEL arm: the simulated broker, on a world of its own.
struct Model {
    _dir: aterm_tempfile::TempDir,
    core: Arc<Core>,
    process: Arc<Process>,
    conns: BTreeMap<u64, bus::SimStream>,
}

impl Model {
    fn new(tier: Tier, retain: Option<astream_broker::Retention>, mutant: ArmMutant) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-b01-sim")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("a scratch dir");
        let broker = bus::Broker::new(tier, table(tier));
        lock(&broker.state).mutant = mutant;
        broker.retain(retain);
        lock(&broker.state).eager_tails = true;
        let core = Core::new(1, broker, Device::new(dir.path()));
        let process = Process::new(Arc::clone(&core), 1);
        Self {
            _dir: dir,
            core,
            process,
            conns: BTreeMap::new(),
        }
    }

    fn dial(&mut self, c: u64) {
        let stream = bus::dial_stream(&self.process).expect("dial the model");
        self.conns.insert(c, stream);
    }

    fn write(&mut self, c: u64, reqs: &[Request]) {
        let stream = self.conns.get_mut(&c).expect("a live model connection");
        let mut bytes = Vec::new();
        for r in reqs {
            write_frame(&mut bytes, &encode_request(r)).expect("frame to memory");
        }
        stream.write_all(&bytes).expect("write to the model");
    }

    fn read(&mut self, c: u64) -> Response {
        let stream = self.conns.get_mut(&c).expect("a live model connection");
        match read_frame(stream) {
            Ok(Some(frame)) => decode_response(&frame).expect("a frame astream decodes"),
            Ok(None) => panic!("the model closed connection {c}"),
            Err(e) => panic!("no answer from the model on {c}: {e}"),
        }
    }

    /// The next frame of tail `c`, `Ok(None)` at its end, or what the model
    /// said instead of either.
    fn deliver(&mut self, c: u64) -> Result<Option<Response>, String> {
        let stream = self.conns.get_mut(&c).expect("a model tail");
        match read_frame(stream) {
            Ok(Some(frame)) => Ok(decode_response(&frame)),
            Ok(None) => Ok(None),
            Err(e) => Err(format!("nothing owed ({e})")),
        }
    }

    fn end(&mut self, c: u64) {
        drop(self.conns.remove(&c));
    }

    fn end_all(&mut self) {
        let ids: Vec<u64> = self.conns.keys().copied().collect();
        for c in ids {
            self.end(c);
        }
    }

    fn stop(&mut self, how: Stop) {
        self.end_all();
        self.core.broker.stop(how, &mut super::Rng::new(0));
    }

    /// The machine loses power, every record kept.
    fn lose_power(&mut self) {
        self.end_all();
        self.core.broker.stop_keeping_all(Stop::PowerLoss);
    }

    /// A retention tick at `now_ms`; the log's base after it.
    fn tick(&self, now_ms: u64) -> u64 {
        self.core.broker.tick(now_ms);
        self.core.broker.base()
    }

    /// A carried cut at `at`; the log's base after it.
    fn cut(&self, at: u64) -> u64 {
        self.core.broker.cut_to(at);
        self.core.broker.base()
    }
}

/// aterm's durability word for a tier of the model.
fn durability(tier: Tier) -> Durability {
    match tier {
        Tier::Strict => Durability::Strict,
        Tier::Ordered => Durability::Ordered,
        Tier::Relaxed => Durability::Relaxed,
    }
}

/// The table aterm opens a broker of this tier with, as the model reads it.
fn table(tier: Tier) -> Escalation {
    Escalation::of(&durability(tier).options().escalation)
}

/// Whether the record `req` writes is escalated on the real arm: its subject
/// or group rules (an unguarded broker binds no principal).
fn escalated(table: &Escalation, req: &Request) -> bool {
    match req {
        Request::Publish { subject, .. } => table.escalates(subject, &bus::Kind::Plain, None),
        Request::Commit { group, upto } => table.escalates(
            "",
            &bus::Kind::Commit {
                group: group.clone(),
                upto: *upto,
            },
            None,
        ),
        _ => false,
    }
}

/// Whether `resp` is the last frame of the answer to `req`.
fn ends(req: &Request, resp: &Response) -> bool {
    match req {
        Request::Last { .. } | Request::Fetch { .. } => {
            matches!(resp, Response::Mark { .. } | Response::Error { .. })
        }
        _ => true,
    }
}

/// Whether `req` turns its connection into a tail.
fn subscribes(req: &Request) -> bool {
    matches!(
        req,
        Request::Subscribe { .. } | Request::SubscribeGroup { .. }
    )
}

/// The refusal class astream defines a prefix for, or none.
fn class(msg: &str) -> &'static str {
    use astream_broker::proto::{
        LOG_POISONED_PREFIX, RATE_LIMITED_PREFIX, RETAINED_FROM_PREFIX, RETIRE_CHARGED_PREFIX,
        RETIRE_CUT_PREFIX, RETIRE_LOG_PREFIX, RETIRE_WILL_PREFIX,
    };
    [
        RATE_LIMITED_PREFIX,
        RETIRE_CHARGED_PREFIX,
        RETIRE_WILL_PREFIX,
        RETIRE_LOG_PREFIX,
        RETIRE_CUT_PREFIX,
        RETAINED_FROM_PREFIX,
        LOG_POISONED_PREFIX,
    ]
    .into_iter()
    .find(|p| msg.starts_with(p))
    .unwrap_or("")
}

/// One answer as the comparison reads it, with a `Mark`'s
/// durable head apart for the law.
fn project(tier: Tier, resp: &Response) -> (String, Option<(Option<u64>, u64)>) {
    match resp {
        Response::PublishAck { offset, deduped } => {
            (format!("ack @{offset} deduped={deduped}"), None)
        }
        Response::Delivery {
            offset,
            subject,
            body,
        } => (format!("delivery @{offset} {subject} {body:?}"), None),
        // A rate-limited refusal by the bucket that refused it too (`(byte rate exceeded)`): its
        // wait is each arm's clock's, and only the model's stands still.
        Response::Error { code, msg } => (
            format!(
                "error {code} {:?}{}",
                class(msg),
                if *code == astream_broker::proto::ERR_RATE_LIMITED {
                    msg.rfind(" (").map_or("", |i| &msg[i..])
                } else {
                    ""
                }
            ),
            None,
        ),
        Response::Mark {
            next,
            head,
            resume,
            durable,
            oldest,
        } => {
            let exact = if tier == Tier::Strict {
                format!(" durable={durable:?}")
            } else {
                String::new()
            };
            (
                format!("mark next={next} head={head} resume={resume:?} oldest={oldest:?}{exact}"),
                (tier != Tier::Strict).then_some((*durable, *head)),
            )
        }
        Response::Nonce { log, ext, .. } => (
            format!(
                "hello generation={:?} served={:?} lineage={:?} requests={:?} device={} \
                 retention={:?}",
                log.as_ref().map(|l| l.generation),
                ext.durability,
                ext.lineage,
                ext.requests,
                // That it names one: the real arm's number is its scratch log's device, the model's
                // none's, and the drain is the platform's on the real arm and Apple's on the model.
                ext.device.is_some(),
                ext.retention
                    .as_deref()
                    .map(|rules| astream_broker::Retention::decode(rules).ok_or(rules))
            ),
            None,
        ),
        Response::Budget {
            subjects,
            reserved,
            max,
        } => (
            format!("budget subjects={subjects} reserved={reserved} max={max}"),
            None,
        ),
        Response::Gap(g) => (
            format!("gap from={} to={} records={:?}", g.from, g.to, g.records),
            None,
        ),
        // No script asks for them (a bridge never does): named whole if one ever comes.
        Response::Stats(stats) => (format!("stats {stats:?}"), None),
    }
}

/// THE DIFFERENTIAL: `steps` on both arms at `tier`, each retained by aterm's rules, the
/// model with `mutant`.
fn run(tier: Tier, mutant: ArmMutant, steps: &[Step]) -> Verdict {
    run_on(
        tier,
        Some(crate::broker_rules::default_rules()),
        mutant,
        steps,
    )
}

/// [`run`] on two brokers each retained by `retain`, or not.
fn run_on(
    tier: Tier,
    retain: Option<astream_broker::Retention>,
    mutant: ArmMutant,
    steps: &[Step],
) -> Verdict {
    run_keyed(tier, retain, None, mutant, steps)
}

/// [`run_on`], the real broker's log sealed under `at_rest`. The model keeps no bytes at rest, so
/// whatever a keyed log answers differently from it is the seal's doing.
fn run_keyed(
    tier: Tier,
    retain: Option<astream_broker::Retention>,
    at_rest: Option<[u8; 32]>,
    mutant: ArmMutant,
    steps: &[Step],
) -> Verdict {
    run_keeping(tier, retain, at_rest, None, mutant, steps).map(drop)
}

/// [`run_keyed`], the real broker keeping `resident_budget` bytes of decoded records resident
/// in place of aterm's budget (`None`: aterm's), handing back the real broker's files and its
/// cold reads to look at.
fn run_keeping(
    tier: Tier,
    retain: Option<astream_broker::Retention>,
    at_rest: Option<[u8; 32]>,
    resident_budget: Option<usize>,
    mutant: ArmMutant,
    steps: &[Step],
) -> Result<Real, String> {
    let real = Real::new(tier, retain.clone(), at_rest, resident_budget);
    run_arms(tier, real, retain, mutant, steps)
}

/// THE REFINEMENT ARM'S RUN (critic item F10): `steps` on aterm's broker of `tier` with its log
/// on a simulated device ([`Real::on_device`]) and on the model, unretained, compared answer by
/// answer as every run is, Ordered keeping no horizon on both (`bus::State::no_horizon`); at
/// each power loss the images the device could be left in (at most [`IMAGES`] for each prefix
/// of the barrier epochs, a sample past that) are reopened and held to the model's law
/// ([`refine`]), and each batch the model rolls back is a write the real arm fails
/// ([`answer_refused`]). What it found.
fn run_refining(tier: Tier, mutant: ArmMutant, steps: &[Step]) -> Result<Refined, String> {
    run_arms(tier, Real::on_device(tier), None, mutant, steps).map(|real| real.refined)
}

/// The steps of a run on `real` and on the model retained by `retain`, or not.
fn run_arms(
    tier: Tier,
    mut real: Real,
    retain: Option<astream_broker::Retention>,
    mutant: ArmMutant,
    steps: &[Step],
) -> Result<Real, String> {
    let table = table(tier);
    let clock = Clock::new();
    let mut model = Model::new(tier, retain, mutant);
    // The refinement arm's Ordered keeps no horizon, as its real broker does ([`Real::options`]).
    lock(&model.core.broker.state).no_horizon = real.device.is_some();
    let mut laws = [Law::default(), Law::default()];
    let mut answers = 0u64;
    for (i, step) in steps.iter().enumerate() {
        let at = |what: &str| format!("{tier} step {i} {step:?}: {what}");
        match step {
            Step::Dial(c) => {
                real.dial(*c);
                model.dial(*c);
            }
            Step::Send(c, req) => {
                let refusing = owed(&model) > 0
                    && matches!(req, Request::Publish { .. } | Request::Commit { .. });
                if refusing {
                    answer_refused(&mut real, &mut model, *c, req, &table, &mut laws, tier)
                } else {
                    answer(
                        &mut real,
                        &mut model,
                        *c,
                        std::slice::from_ref(req),
                        &table,
                        &mut laws,
                        tier,
                    )
                }
                .map_err(|e| at(&e))?;
                answers += 1;
            }
            Step::Pipeline(c, reqs) => {
                if owed(&model) > 0
                    && reqs
                        .iter()
                        .any(|r| matches!(r, Request::Publish { .. } | Request::Commit { .. }))
                {
                    return Err(at(
                        "a pipelined write while the model owes a rolled-back batch: no fault \
                         the real arm arms fails one write of a pipeline",
                    ));
                }
                answer(&mut real, &mut model, *c, reqs, &table, &mut laws, tier)
                    .map_err(|e| at(&e))?;
                answers += reqs.len() as u64;
            }
            Step::Deliver(c) => {
                let r = Ok(real.deliver(*c).map(|r| project(tier, &r).0));
                let m = model.deliver(*c).map(|m| m.map(|m| project(tier, &m).0));
                if r != m {
                    return Err(at(&format!("real {r:?}, model {m:?}")));
                }
                answers += 1;
            }
            Step::DeliverAny(c) => {
                let m = model.deliver(*c).map(|m| m.map(|m| project(tier, &m).0));
                if m.is_ok() {
                    let r = Ok(real.deliver(*c).map(|r| project(tier, &r).0));
                    if r != m {
                        return Err(at(&format!("real {r:?}, model {m:?}")));
                    }
                    answers += 1;
                }
            }
            Step::End(c) => end(&mut real, &mut model, *c),
            Step::Stop(how) => {
                assert_ne!(*how, Stop::PowerLoss, "a power loss is a PowerLoss step");
                assert!(
                    *how == Stop::Clean || !matches!(real.boot, Boot::Unknown),
                    "an unclean stop in a boot the open cannot name moves a weak tier's \
                     generation, which the model's law does not admit (WHAT IT DOES NOT COVER)"
                );
                end_all(&mut real, &mut model);
                real.stop(*how);
                model.stop(*how);
            }
            Step::Start => held_over_the_open(&model, || {
                real.open();
                model.core.broker.start();
                model.core.broker.open_tick();
            }),
            Step::PowerLoss | Step::Reboot | Step::RebootUnnamed => {
                let named = !matches!(step, Step::RebootUnnamed);
                lose_power(&mut real, &mut model, tier, i, named).map_err(|e| at(&e))?;
                if !matches!(step, Step::PowerLoss) {
                    held_over_the_open(&model, || {
                        real.open();
                        model.core.broker.start();
                        model.core.broker.open_tick();
                    });
                }
            }
            Step::Refuse(n) => model.core.broker.refuse_next(*n),
            Step::Name(producer, principal) => {
                model.core.broker.name_producer(*producer, principal);
            }
            Step::StartUnretained => held_over_the_open(&model, || {
                real.open_setting(None);
                model.core.broker.start_unretained();
            }),
            Step::StartWith(rules) => held_over_the_open(&model, || {
                real.open_setting(Some(rules.clone()));
                model.core.broker.start_with(rules.clone());
                model.core.broker.open_tick();
            }),
            Step::Tick(rel) => {
                let now = clock.at(*rel);
                let (r, m) = (real.tick(now), model.tick(now));
                if r != m {
                    return Err(at(&format!(
                        "the tick left the real base {r}, the model's {m}"
                    )));
                }
            }
            Step::Cut(off) => {
                let (r, m) = (real.cut(*off), model.cut(*off));
                if r != m {
                    return Err(at(&format!(
                        "the cut left the real base {r}, the model's {m}"
                    )));
                }
            }
            Step::RateLimit(limit) => {
                real.broker()
                    .set_rate_limit(*limit)
                    .expect("the real broker takes the limit");
                model.core.broker.set_rate_limit(*limit);
            }
            Step::RateLimited => {
                let (r, m) = (
                    real.broker().stats().rate_limited,
                    model.core.broker.rate_limited(),
                );
                if r != m {
                    return Err(at(&format!(
                        "the real broker refused {r} publishes for the rate, the model {m}"
                    )));
                }
                answers += 1;
            }
        }
        // Between steps the refinement arm's log holds what the model's does, so nothing of a
        // rolled-back batch (a publish's, a commit's, a fired will's) is visible past one.
        if real.device.is_some() && real.up.is_some() {
            let (r, m) = (
                real.broker().head(),
                lock(&model.core.broker.state).log.len() as u64,
            );
            if r != m {
                return Err(at(&format!("the real head is {r}, the model's {m}")));
            }
        }
    }
    assert!(answers > 0, "a script that compared nothing");
    Ok(real)
}

/// Send `reqs` on `c` on both arms, read each answer whole, and compare.
fn answer(
    real: &mut Real,
    model: &mut Model,
    c: u64,
    reqs: &[Request],
    table: &Escalation,
    laws: &mut [Law; 2],
    tier: Tier,
) -> Result<(), String> {
    real.write(c, reqs);
    model.write(c, reqs);
    for req in reqs {
        if subscribes(req) {
            real.tail(c);
            continue;
        }
        loop {
            let r = real.read(c);
            let m = model.read(c);
            compare(real, c, req, &r, &m, table, laws, tier)?;
            if ends(req, &r) {
                break;
            }
        }
    }
    Ok(())
}

/// A BATCH THE MODEL ROLLS BACK, ON BOTH ARMS (critic item F10): `req` goes to the model first,
/// and when the model spends one of its `refuse_next` on it the real arm's next write fails
/// half-way (`InjectedFaults::write`) while `req` is served there, as a disk that fills does;
/// then every answer is compared as [`answer`] compares them, and after a refusal the real
/// head must be the model's: nothing of the rolled-back batch visible.
fn answer_refused(
    real: &mut Real,
    model: &mut Model,
    c: u64,
    req: &Request,
    table: &Escalation,
    laws: &mut [Law; 2],
    tier: Tier,
) -> Result<(), String> {
    let before = owed(model);
    model.write(c, std::slice::from_ref(req));
    let mut modelled = Vec::new();
    loop {
        let m = model.read(c);
        let end = ends(req, &m);
        modelled.push(m);
        if end {
            break;
        }
    }
    let refused = owed(model) < before;
    if refused {
        real.broker().inject_faults(astream_broker::InjectedFaults {
            write: true,
            sync: false,
            truncate: false,
        });
    }
    real.write(c, std::slice::from_ref(req));
    let mut answered = Vec::new();
    loop {
        let r = real.read(c);
        let end = ends(req, &r);
        answered.push(r);
        if end {
            break;
        }
    }
    if refused {
        real.broker()
            .inject_faults(astream_broker::InjectedFaults::default());
        real.refined.refused += 1;
    }
    if answered.len() != modelled.len() {
        return Err(format!(
            "{req:?}: real {answered:?}, model {modelled:?} (refused={refused})"
        ));
    }
    for (r, m) in answered.iter().zip(&modelled) {
        compare(real, c, req, r, m, table, laws, tier)?;
    }
    let (real_head, model_head) = (
        real.broker().head(),
        lock(&model.core.broker.state).log.len() as u64,
    );
    if refused && real_head != model_head {
        return Err(format!(
            "{req:?}: rolled back, the real head is {real_head} and the model's {model_head}"
        ));
    }
    Ok(())
}

/// One answer `r` of the real arm and `m` of the model to `req` on `c`, compared ([`project`]),
/// each `Mark`'s durable head held to its arm's [`Law`].
#[expect(
    clippy::too_many_arguments,
    reason = "the two answers, the request, its connection and the run's table, laws and tier"
)]
fn compare(
    real: &mut Real,
    c: u64,
    req: &Request,
    r: &Response,
    m: &Response,
    table: &Escalation,
    laws: &mut [Law; 2],
    tier: Tier,
) -> Result<(), String> {
    let (rp, rd) = project(tier, r);
    let (mp, md) = project(tier, m);
    if rp != mp {
        return Err(format!("{req:?}: real {rp}, model {mp}"));
    }
    for (who, (law, d)) in ["real", "model"]
        .into_iter()
        .zip(laws.iter_mut().zip([rd, md]))
    {
        if let Some((durable, head)) = d {
            law.judge(durable, head)
                .map_err(|e| format!("{req:?}: the {who} arm's {e}"))?;
        }
    }
    if let Response::PublishAck { offset, .. } = r {
        if escalated(table, req) {
            for law in laws.iter_mut() {
                law.floor = law.floor.max(offset + 1);
            }
        }
    }
    if let (Request::Will { producer_id, .. }, Response::Mark { .. }) = (req, r) {
        if let Some(conn) = real.conns.get_mut(&c) {
            conn.will = Some(*producer_id);
        }
        real.will_producers.insert(*producer_id);
    }
    Ok(())
}

/// The rolled-back batches the model still owes (`bus::State::refuse_next`).
fn owed(model: &Model) -> u32 {
    lock(&model.core.broker.state).refuse_next
}

/// The connection `c` ends on both arms, the model first: when the will it fires there is a batch
/// the model rolls back, the real arm's write fails while its will fires (critic item F10).
fn end(real: &mut Real, model: &mut Model, c: u64) {
    let before = owed(model);
    model.end(c);
    let refused = owed(model) < before;
    if refused {
        real.broker().inject_faults(astream_broker::InjectedFaults {
            write: true,
            sync: false,
            truncate: false,
        });
    }
    real.end(c);
    if refused {
        real.broker()
            .inject_faults(astream_broker::InjectedFaults::default());
        real.refined.refused += 1;
    }
}

/// Every connection ends on both arms, in order ([`end`]).
fn end_all(real: &mut Real, model: &mut Model) {
    let ids: BTreeSet<u64> = real
        .conns
        .keys()
        .chain(model.conns.keys())
        .copied()
        .collect();
    for c in ids {
        end(real, model, c);
    }
}

/// `open` opens both arms, a rolled-back batch the model owes held over it: astream fires the
/// wills a log holds inside its open, before any fault can be armed on the broker, so no will an
/// open fires is one the model rolls back here (critic item F10), as the worlds' own model may.
fn held_over_the_open(model: &Model, open: impl FnOnce()) {
    let owed = std::mem::take(&mut lock(&model.core.broker.state).refuse_next);
    open();
    lock(&model.core.broker.state).refuse_next = owed;
}

/// The machine loses power on both arms: every connection ends, the real broker stops as a
/// killed process does and the model keeps every record, each to open again in another boot
/// (one the real arm's open cannot name unless `named`). On the refinement arm the images its
/// device could be left in ([`refine`]: at most [`IMAGES`] for each prefix of the barrier epochs,
/// a sample past that) are first held to the model's law.
fn lose_power(
    real: &mut Real,
    model: &mut Model,
    tier: Tier,
    step: usize,
    named: bool,
) -> Result<(), String> {
    end_all(real, model);
    let before = real.broker().log_identity();
    let head = real.broker().head();
    real.lose_power(named);
    if let Some(device) = real.device.clone() {
        let opts = real.options(real.boot);
        refine(
            &device,
            opts,
            before,
            head,
            model,
            tier,
            step,
            &mut real.refined,
        )?;
    }
    model.lose_power();
    Ok(())
}

/// THE REFINEMENT AT ONE POWER LOSS (critic item F10): the real store's recoveries stay inside
/// the model's law. The model is the bridge's adversary, a spec-level over-approximation whose
/// seed chooses any cut at or above the durable head P2-P7 allow (`bus::State::allowed_cuts`,
/// [`bus::law_check`]); here the direction it cannot check itself: the images astream's
/// simulated device could be left in after this loss (`SimDevice::power_loss_images`: every
/// one while they fit [`IMAGES`] for each prefix of the barrier epochs since the last full
/// flush, a sample seeded by the step beyond that, none and all always among it) are each
/// opened as the next boot opens it (`BrokerLog::open_sim`, aterm's options, in the boot it
/// opens in next), and each must
///
/// * open (P1: the model always opens);
/// * keep a head among the model's allowed cuts;
/// * hold, below it, the model's records: each subject, and each visible record's body;
/// * be a restart [`bus::law_check`] admits, its identity and lineage as astream settled them.
///
/// The arms must agree before the loss on the head and the generation, as their answers did.
#[expect(
    clippy::too_many_arguments,
    reason = "the device and how it opens, what the real arm told, the model, and the tally"
)]
fn refine(
    device: &astream_broker::SimDevice,
    opts: astream_broker::OpenOptions,
    before: astream_broker::LogIdentity,
    head: u64,
    model: &Model,
    tier: Tier,
    step: usize,
    refined: &mut Refined,
) -> Result<(), String> {
    let (cuts, oracle, mut restart) = {
        let s = lock(&model.core.broker.state);
        let restart = bus::Restart {
            stop: Stop::PowerLoss,
            tier,
            before: s.log.clone(),
            durable_before: s.durable,
            kept: 0,
            generation_before: s.generation,
            generation_after: s.generation,
            lineage_after: Vec::new(),
            another_log: false,
        };
        (s.allowed_cuts(Stop::PowerLoss), s.oracle.clone(), restart)
    };
    let modelled = restart.before.len() as u64;
    if head != modelled || before.generation != restart.generation_before {
        return Err(format!(
            "before the loss the real arm stood at head {head} under {before}, the model at \
             head {modelled} in generation {}",
            restart.generation_before
        ));
    }
    let (images, every) = device.power_loss_images(IMAGES, 0xF10 ^ step as u64);
    refined.losses += 1;
    refined.sampled += u64::from(!every);
    for (n, image) in images.iter().enumerate() {
        let log = astream_broker::BrokerLog::open_sim(image, opts.clone())
            .map_err(|e| format!("P1: the next boot refused power-loss image {n}: {e}"))?;
        let kept = log.head().0;
        let now = log.identity();
        let at =
            |what: String| format!("power-loss image {n} (kept {kept} of {head}, {now}): {what}");
        if !cuts.contains(&kept) {
            return Err(at(format!(
                "no cut the model's law allows ({:?}..={:?})",
                cuts.first(),
                cuts.last()
            )));
        }
        for rec in log.read_from(astream_wire::Offset::ZERO) {
            let at_off = usize::try_from(rec.seq.0).unwrap_or(usize::MAX);
            let Some(m) = restart.before.get(at_off) else {
                return Err(at(format!("a record @{} past the model's log", rec.seq.0)));
            };
            if rec.subject != m.subject || (!m.hidden() && rec.body != m.body) {
                return Err(at(format!(
                    "@{}: {} {:?}, the model's {} {:?}",
                    rec.seq.0, rec.subject, rec.body, m.subject, m.body
                )));
            }
        }
        restart.kept = kept;
        restart.generation_after = now.generation;
        restart.lineage_after = log.lineage().steps().to_vec();
        restart.another_log = now.id != before.id;
        let bad = bus::law_check(&restart, &oracle);
        if !bad.is_empty() {
            return Err(at(format!("the model's law: {bad:?}")));
        }
        refined.images += 1;
        refined.lost += u64::from(kept < head);
        refined.another_log += u64::from(restart.another_log);
    }
    Ok(())
}

/// THE DOOR'S JOURNAL of a world, as a script: each connection, request,
/// delivery and end as it happened, a world producer's writes on a connection
/// of their own, and the restarts.
fn replay(journal: &[Event]) -> Vec<Step> {
    // The world's ticks at their distance from its first, on the run's clock ([`Clock`]).
    // A tick at the last time first, before every stop (so what an open finds was stamped
    // by a script's tick, and the real broker's own first tick at the open, by the wall
    // clock, stamps only what the open itself wrote, at the last stamp's time, as the
    // model's `open_tick` does) and before every cut (which plans by the last stamp's
    // clock).
    let t0 = journal.iter().find_map(|e| match e {
        Event::Tick(now) => Some(*now),
        _ => None,
    });
    let mut last = 0u64;
    let mut steps = vec![Step::Tick(0)];
    let mut world_dialled = false;
    for e in journal {
        match e {
            Event::Dial(c) => steps.push(Step::Dial(*c)),
            Event::Request(c, req) => steps.push(Step::Send(*c, req.clone())),
            Event::Deliver(c) => steps.push(Step::DeliverAny(*c)),
            Event::End(c) => steps.push(Step::End(*c)),
            Event::Publish {
                producer,
                seq,
                subject,
                body,
            } => {
                if !world_dialled {
                    steps.push(Step::Dial(WORLD));
                    world_dialled = true;
                }
                steps.push(Step::Send(
                    WORLD,
                    Request::Publish {
                        producer_id: *producer,
                        producer_seq: *seq,
                        subject: subject.clone(),
                        body: body.clone(),
                    },
                ));
            }
            Event::Stop(how) => {
                steps.push(Step::Tick(last));
                steps.push(if *how == Stop::PowerLoss {
                    Step::PowerLoss
                } else {
                    Step::Stop(*how)
                });
                world_dialled = false;
            }
            Event::Refuse(n) => steps.push(Step::Refuse(*n)),
            // A node's name alone: the real arm binds no principal (it is unguarded), so the model
            // learns none its table's principal rule reads (`h-`); a node's is what its rolled-back
            // batches are picked by (`bus::State::refuse_next`).
            Event::Name(producer, principal) => {
                if principal.starts_with("n-") {
                    steps.push(Step::Name(*producer, principal.clone()));
                }
            }
            Event::Start => steps.push(Step::Start),
            Event::StartUnretained => steps.push(Step::StartUnretained),
            Event::StartWith(rules) => steps.push(Step::StartWith(rules.clone())),
            Event::Tick(now) => {
                last = now.saturating_sub(t0.unwrap_or(*now));
                steps.push(Step::Tick(last));
            }
            Event::Cut(at) => {
                steps.push(Step::Tick(last));
                steps.push(Step::Cut(*at));
            }
        }
    }
    steps
}

/// The hand-written scripts.
pub(crate) mod scripts {
    use super::{Request, Step, ACK_SEQ_BASE};
    use astream_broker::{incarnation_base, will_seq, RETIRE_SUBJECT};

    /// A node's producer, and another's.
    pub(crate) const P: u64 = 7;
    pub(crate) const Q: u64 = 8;

    pub(crate) const PRES_A: &str = "/f/f/pub/n-sim/s-a/presence";
    pub(crate) const PRES_B: &str = "/f/f/pub/n-sim/s-b/presence";
    pub(crate) const PRES_C: &str = "/f/f/pub/n-sim/s-c/presence";
    pub(crate) const NODE: &str = "/f/f/pub/n-sim/node/presence";
    pub(crate) const LANE: &str = "/f/f/in/n-sim/s-a/n-sim/say";
    pub(crate) const LANE_B: &str = "/f/f/in/n-sim/s-b/n-sim/say";
    /// Escalated by aterm's table on the weak tiers.
    pub(crate) const HALT: &str = "/f/f/fleet/halt";
    pub(crate) const PERSON: &str = "/f/f/in/p/alice/n-sim/say";
    pub(crate) const INBOX: &str = "/f/f/in/n-sim/>";
    pub(crate) const GROUP: &str = "/f/f/cur/n-sim";
    /// Escalated by aterm's table on the weak tiers.
    pub(crate) const PERSON_GROUP: &str = "/f/f/cur/p/alice";

    pub(crate) fn publish(producer: u64, seq: u64, subject: &str, body: &str) -> Request {
        Request::Publish {
            producer_id: producer,
            producer_seq: seq,
            subject: subject.to_string(),
            body: body.as_bytes().to_vec(),
        }
    }

    /// Malformed UTF-8 remains an ordinary, byte-exact broker payload.
    pub(crate) fn binary_payload() -> Vec<Step> {
        vec![
            Step::Dial(1),
            Step::Send(
                1,
                Request::Publish {
                    producer_id: P,
                    producer_seq: seq(1, 1),
                    subject: LANE.to_string(),
                    body: vec![0xff],
                },
            ),
            Step::Send(1, fetch(0, LANE, 1)),
        ]
    }

    pub(crate) fn will(producer: u64, seq: u64, subject: &str, body: &str) -> Request {
        Request::Will {
            producer_id: producer,
            producer_seq: seq,
            subject: subject.to_string(),
            body: body.as_bytes().to_vec(),
        }
    }

    /// `/a/retire` of `target`, at `seq` (a client's is 0).
    pub(crate) fn retire_at(producer: u64, seq: u64, target: &str) -> Request {
        Request::Publish {
            producer_id: producer,
            producer_seq: seq,
            subject: RETIRE_SUBJECT.to_string(),
            body: target.as_bytes().to_vec(),
        }
    }

    pub(crate) fn retire(producer: u64, target: &str) -> Request {
        retire_at(producer, 0, target)
    }

    /// The conditional `Retire` of `target` if its newest record is at `offset` on log `id`, generation `generation`.
    pub(crate) fn retire_on(
        producer: u64,
        target: &str,
        offset: u64,
        id: [u8; 16],
        generation: u64,
    ) -> Request {
        Request::Retire {
            producer_id: producer,
            subject: target.to_string(),
            offset,
            log: astream_broker::LogIdentity { id, generation },
        }
    }

    /// [`retire_on`] the log each arm serves ([`super::bus::LOG_ID`] on both, `Real::on_this_log`).
    pub(crate) fn retire_if(producer: u64, target: &str, offset: u64, generation: u64) -> Request {
        retire_on(producer, target, offset, super::bus::LOG_ID, generation)
    }

    pub(crate) fn fetch(from: u64, filter: &str, max: u32) -> Request {
        Request::Fetch {
            from_offset: from,
            filter: filter.to_string(),
            max,
        }
    }

    pub(crate) fn last(filter: &str, after: &str, max: u32) -> Request {
        Request::Last {
            filter: filter.to_string(),
            after: after.to_string(),
            max,
        }
    }

    pub(crate) fn commit(group: &str, upto: u64) -> Request {
        Request::Commit {
            group: group.to_string(),
            upto,
        }
    }

    pub(crate) fn budget(producer: u64) -> Request {
        Request::Budget {
            producer_id: producer,
        }
    }

    /// Incarnation `inc`'s `k`-th sequence.
    pub(crate) fn seq(inc: u64, k: u64) -> u64 {
        incarnation_base(inc).expect("an incarnation") + k
    }

    /// Incarnation `inc`'s will sequence.
    pub(crate) fn will_at(inc: u64) -> u64 {
        will_seq(inc).expect("an incarnation")
    }

    /// Everything readable, as a reader and a roster see it.
    fn look(c: u64) -> Vec<Step> {
        vec![
            Step::Send(c, fetch(0, "/>", 1000)),
            Step::Send(c, last("/>", "", 1000)),
            Step::Send(c, budget(P)),
            Step::Send(c, budget(Q)),
        ]
    }

    /// PUBLISH: a resend, pipelined publishes, the reserved and malformed subjects, the ack half,
    /// and the gap fence on a connection that asked for it.
    pub(crate) fn publishing() -> Vec<Step> {
        let mut s = vec![
            Step::Dial(1),
            Step::Send(1, Request::Hello),
            Step::Send(1, publish(P, seq(1, 1), PRES_A, "live")),
            Step::Send(1, publish(P, seq(1, 1), PRES_A, "live")),
            Step::Send(1, publish(P, seq(1, 1), PRES_A, "other")),
            Step::Pipeline(
                1,
                vec![
                    publish(P, seq(1, 2), LANE, "m1"),
                    publish(P, seq(1, 3), LANE_B, "m2"),
                    publish(P, seq(1, 2), LANE, "m1"),
                    publish(Q, seq(1, 1), HALT, "halt"),
                    publish(Q, seq(1, 2), PERSON, "hi"),
                    publish(P, seq(1, 4), LANE, "m3"),
                ],
            ),
            Step::Send(1, publish(P, seq(1, 5), "/a/commit", "x")),
            Step::Send(1, publish(P, seq(1, 5), "/a/will", "x")),
            Step::Send(1, publish(P, seq(1, 5), "/a/bind", "x")),
            Step::Send(1, publish(P, seq(1, 5), "/a/custom", "x")),
            Step::Send(1, publish(P, seq(1, 6), "no-slash", "x")),
            Step::Send(1, publish(P, seq(1, 6), "/f/f/*/x", "x")),
            Step::Send(1, publish(P, seq(1, 6), "/f//x", "x")),
            Step::Send(
                1,
                publish(P, seq(1, 6), &format!("/f/{}", "x".repeat(4100)), "x"),
            ),
            Step::Send(1, fetch(0, &format!("/f/{}", "x".repeat(4100)), 1)),
            Step::Send(1, publish(P, ACK_SEQ_BASE | 3, LANE, "ack")),
            Step::Send(1, publish(P, ACK_SEQ_BASE | 3, LANE, "ack")),
            Step::Send(1, fetch(0, "/>", 100)),
            Step::Send(1, fetch(2, "/f/f/in/>", 1)),
            Step::Send(1, fetch(0, "/f/f/in/>", 0)),
            Step::Send(1, fetch(1000, "/>", 10)),
            Step::Send(1, fetch(0, "no-slash", 10)),
            Step::Send(1, fetch(0, "/f/f/pub/*/s-a/>", 100)),
            Step::Send(1, last("/>", "", 100)),
            Step::Send(1, last("/f/f/>", "", 2)),
            Step::Send(1, last("/f/f/>", PRES_A, 2)),
            Step::Send(1, last("/f/f/in/n-sim/>", "", 2)),
            Step::Send(1, last(PRES_A, "", 1)),
            Step::Send(1, last(PRES_A, PRES_A, 1)),
            Step::Send(1, last("/>", "", 0)),
            Step::Send(1, last("bad", "", 10)),
            Step::Send(1, last("/f/f/pub/*/*/presence", "/a", 10)),
            Step::Send(1, Request::Keepalive),
            Step::Send(1, Request::IdleMarks),
            Step::Send(1, Request::GapNotices),
            Step::Send(
                1,
                Request::Attach {
                    grant: "rw:/>".to_string(),
                    proof: vec![0; 32],
                },
            ),
            Step::Send(1, budget(P)),
            Step::Send(1, budget(Q)),
            Step::Send(1, budget(99)),
            Step::End(1),
            Step::Dial(2),
            Step::Send(2, Request::Hello),
            // The fence, on the connection that asks for it (connection 1 never did, so its
            // refused publishes above fence nothing): a refused publish holds back the next of
            // its producer until the lift, and the next lands after it.
            Step::Send(2, Request::FenceGaps),
            Step::Send(2, publish(P, seq(1, 7), "no-slash", "x")),
            Step::Send(2, publish(P, seq(1, 8), LANE, "fenced")),
            Step::Send(2, Request::ClearGapFences),
            Step::Send(2, publish(P, seq(1, 8), LANE, "lifted")),
            // The settle (critic item F07): answered in send order with the durable head after
            // its full flush, on a clean log too, and pipelined behind a publish.
            Step::Send(2, Request::Settle),
            Step::Send(2, Request::Settle),
            Step::Pipeline(
                2,
                vec![publish(P, seq(1, 9), LANE, "settled"), Request::Settle],
            ),
        ];
        s.extend(look(2));
        s
    }

    /// THE RATE LIMIT (critic item E06): a producer's bucket spent, another producer's whole, a
    /// record past the byte burst, and the fence a refusal latches on a connection that asked,
    /// what a refusal behind it counts, and the retry at its own sequence after the lift. A byte
    /// bucket refilled at a byte a second, so neither arm's clock moves an answer: the first
    /// publish leaves 63 of its 150 bytes, and the next needs 87.
    pub(crate) fn rate_limited() -> Vec<Step> {
        const R: u64 = 9;
        let row = |c: char| c.to_string().repeat(60);
        let limit = astream_broker::RateLimit::records(1_000, 1_000)
            .and_then(|l| l.with_bytes(1, 150))
            .expect("a byte bucket astream takes");
        let mut s = vec![
            Step::Dial(1),
            Step::Send(1, Request::Hello),
            Step::Send(1, Request::FenceGaps),
            Step::RateLimit(Some(limit)),
            // Landed, refused for the rate (the fence latched at its sequence), and gap-fenced:
            // the last one counted refused for the rate too, ahead of the fence.
            Step::Pipeline(
                1,
                vec![
                    publish(P, seq(1, 1), LANE, &row('a')),
                    publish(P, seq(1, 2), LANE, &row('b')),
                    publish(P, seq(1, 3), LANE, &row('c')),
                ],
            ),
            Step::RateLimited,
            // Another producer's bucket is its own.
            Step::Send(1, publish(Q, seq(1, 1), LANE_B, &row('q'))),
            // A record past the byte burst, refused at every try; a retire, never limited; a
            // malformed subject, refused for itself and never counted.
            Step::Send(1, publish(R, seq(1, 1), LANE, &"x".repeat(200))),
            Step::Send(1, publish(R, seq(1, 1), LANE, &"x".repeat(200))),
            Step::Send(1, retire(P, LANE)),
            Step::Send(1, publish(Q, seq(1, 2), "no-slash", "x")),
            Step::RateLimited,
            // The lift alone lands nothing: the refused post again, still past the bucket,
            // latches the fence again.
            Step::Send(1, Request::ClearGapFences),
            Step::Send(1, publish(P, seq(1, 2), LANE, &row('b'))),
            Step::Send(1, publish(P, seq(1, 3), LANE, &row('c'))),
            // Unlimited, and no lift: the refused post at its own sequence lands and takes its
            // fence down (an attach's replay holds the fences, and retries it so), then the next,
            // then a resend the dedup answers; another producer's fence still stands until the lift.
            Step::RateLimit(None),
            Step::Send(1, publish(P, seq(1, 2), LANE, &row('b'))),
            Step::Send(1, publish(P, seq(1, 3), LANE, &row('c'))),
            Step::Send(1, publish(P, seq(1, 2), LANE, &row('b'))),
            Step::Send(1, publish(R, seq(1, 2), LANE, "behind its fence")),
            Step::Send(1, Request::ClearGapFences),
            Step::Send(1, publish(R, seq(1, 1), LANE, &"x".repeat(200))),
            Step::RateLimited,
            // An open sets aterm's own limit again, which none of this reaches.
            Step::Stop(super::Stop::Clean),
            Step::Start,
            Step::Dial(2),
            Step::Send(2, Request::Hello),
            Step::Send(2, publish(P, seq(2, 1), LANE, &row('d'))),
            Step::Send(2, publish(P, seq(2, 2), LANE, &row('e'))),
            Step::RateLimited,
        ];
        s.extend(look(2));
        s
    }

    /// `/a/retire`: absent, charged to this producer, to another, at a seq other than 0, a reserved or malformed target, and while a will targets the subject.
    pub(crate) fn retiring() -> Vec<Step> {
        let mut s = vec![
            Step::Dial(1),
            Step::Send(1, Request::Hello),
            Step::Send(1, publish(P, seq(1, 1), PRES_A, "live")),
            Step::Send(1, publish(Q, seq(1, 1), PRES_B, "live")),
            Step::Send(1, publish(P, seq(1, 2), PRES_B, "live")),
            Step::Send(1, budget(P)),
            Step::Send(1, budget(Q)),
            Step::Send(1, retire(P, PRES_A)),
            Step::Send(1, retire(P, PRES_A)),
            Step::Send(1, retire(P, PRES_B)),
            Step::Send(1, retire(P, "/f/f/never/written")),
            Step::Send(1, retire(P, "/a/commit")),
            Step::Send(1, retire(P, "/a/custom")),
            Step::Send(1, retire(P, "/a")),
            Step::Send(1, retire(P, "no-slash")),
            Step::Send(1, retire(P, "/f/f/*")),
            Step::Send(1, retire_at(Q, 9, PRES_B)),
            Step::Send(1, budget(P)),
            Step::Send(1, budget(Q)),
            Step::Send(1, publish(Q, seq(1, 2), PRES_A, "back")),
            Step::Send(1, budget(P)),
            Step::Send(1, budget(Q)),
            Step::Send(1, retire(P, PRES_A)),
            Step::Send(1, publish(P, seq(1, 3), PRES_C, "live")),
            Step::Dial(2),
            Step::Send(2, will(P, will_at(1), PRES_C, "gone")),
            Step::Send(1, retire(P, PRES_C)),
            Step::End(2),
            Step::Send(1, retire(P, PRES_C)),
            Step::Send(1, publish(P, seq(2, 1), LANE, "m")),
            Step::Dial(3),
            Step::Send(3, will(P, will_at(2), LANE, "gone")),
            Step::Send(1, retire(P, LANE)),
            Step::Send(1, publish(P, seq(3, 1), NODE, "live")),
            Step::Send(1, retire(P, LANE)),
            Step::End(3),
        ];
        s.extend(look(1));
        s.extend([
            Step::Stop(super::Stop::Clean),
            Step::Start,
            Step::Dial(4),
            Step::Send(4, Request::Hello),
        ]);
        s.extend(look(4));
        s
    }

    /// THE CONDITIONAL `Retire` the bridge gives a departed session's subjects back by: retired at
    /// the newest record it names; moot for an absent subject or a newer record; refused when it
    /// names another log, another generation, an offset past the newest record or, for an absent
    /// subject, at or above the head; charged to another producer; while a will can still
    /// append; a reserved, malformed or overlong subject; pipelined; across a restart and a move.
    /// The offsets in the comments are what each arm answers.
    pub(crate) fn conditional() -> Vec<Step> {
        let foreign = [0x11; 16];
        let mut s = vec![
            Step::Dial(1),
            Step::Send(1, Request::Hello),
            Step::Send(1, publish(P, seq(1, 1), PRES_A, "exited")), // @0
            Step::Send(1, publish(Q, seq(1, 1), PRES_B, "live")),   // @1
            Step::Send(1, publish(P, seq(1, 2), LANE, "m1")),       // @2
            Step::Send(1, publish(P, seq(1, 3), PRES_C, "live")),   // @3
            Step::Send(1, budget(P)),
            Step::Send(1, budget(Q)),
            Step::Send(1, retire_if(P, PRES_A, 0, 0)), // retired @4
            Step::Send(1, retire_if(P, PRES_A, 0, 0)), // moot: absent, at the head 5
            Step::Send(1, retire_if(P, PRES_A, 5, 0)), // absent, at the head: log:
            Step::Send(1, retire_if(P, PRES_B, 1, 0)), // charged to Q
            Step::Send(1, retire_if(Q, PRES_B, 0, 0)), // moot: a newer record @1
            Step::Send(1, publish(P, seq(1, 4), LANE, "m2")), // @5
            Step::Send(1, retire_if(P, LANE, 2, 0)),   // moot @5
            Step::Send(1, retire_if(P, LANE, 6, 0)),   // past the newest record: log:
            Step::Send(1, retire_if(P, LANE, 5, 1)),   // another generation: log:
            Step::Send(1, retire_on(P, LANE, 5, foreign, 0)), // another log: log:
            Step::Send(1, retire_if(P, "/a/commit", 0, 0)),
            Step::Send(1, retire_if(P, "/a/retire", 0, 0)),
            Step::Send(1, retire_if(P, "/a", 0, 0)),
            Step::Send(1, retire_if(P, "no-slash", 0, 0)),
            Step::Send(1, retire_if(P, "/f/f/*", 0, 0)),
            Step::Send(1, retire_if(P, &format!("/f/{}", "x".repeat(4100)), 0, 0)),
            Step::Dial(2),
            Step::Send(2, will(P, will_at(2), PRES_C, "gone")), // @6
            Step::Send(1, retire_if(P, PRES_C, 3, 0)),          // will:
            Step::Send(1, retire_if(Q, PRES_C, 3, 0)),          // charged to P
            Step::End(2),                                       // fires @7
            Step::Send(1, retire_if(P, PRES_C, 3, 0)),          // moot @7
            Step::Send(1, retire_if(P, PRES_C, 7, 0)),          // retired @8
            // Pipelined, only answers a group-commit batch cannot change (the module doc's
            // WHAT IT DOES NOT COVER): no subject twice, no moot answer for an absent one.
            Step::Pipeline(
                1,
                vec![
                    retire_if(P, LANE, 5, 0),            // retired @9
                    publish(P, seq(2, 1), LANE_B, "m3"), // @10
                    retire_if(P, PRES_B, 1, 0),          // charged to Q
                    retire_if(Q, PRES_B, 0, 0),          // moot @1
                ],
            ),
            Step::Send(1, retire_if(P, LANE, 5, 0)), // moot: absent, at the head 11
            Step::Send(1, budget(P)),
            Step::Send(1, budget(Q)),
        ];
        s.extend(look(1));
        s.extend([
            Step::Stop(super::Stop::Clean),
            Step::Start,
            Step::Dial(3),
            Step::Send(3, Request::Hello),
            Step::Send(3, retire_if(P, PRES_A, 0, 0)),
            Step::Send(3, publish(P, seq(2, 2), LANE, "m4")), // @11
            Step::Send(3, budget(P)),
        ]);
        s.extend(look(3));
        s.extend([
            Step::Reboot,
            Step::Dial(4),
            Step::Send(4, Request::Hello),
            Step::Send(4, retire_if(P, LANE, 11, 0)), // the log moved: log:
            Step::Send(4, retire_if(P, LANE, 11, 1)), // retired @12
        ]);
        s.extend(look(4));
        s
    }

    /// WILLS: registered with its subject reserved.
    pub(crate) fn wills() -> Vec<Step> {
        let mut s = vec![
            Step::Dial(1),
            Step::Send(1, Request::Hello),
            Step::Send(1, will(P, will_at(1), NODE, "away 1")),
            Step::Send(1, budget(P)),
            Step::Send(1, will(Q, will_at(1), NODE, "away q")),
            Step::Send(1, will(P, will_at(1), "/a/will", "x")),
            Step::Send(1, will(P, will_at(1), "/a/retire", "x")),
            Step::Send(1, will(P, will_at(1), "no-slash", "x")),
            Step::Send(1, will(P, ACK_SEQ_BASE | 1, NODE, "x")),
            Step::Send(1, budget(P)),
            Step::Send(1, publish(P, seq(1, 1), NODE, "live 1")),
            Step::Send(1, budget(P)),
            Step::End(1),
            Step::Dial(2),
            Step::Send(2, fetch(0, "/>", 100)),
            Step::Send(2, will(P, will_at(2), NODE, "away 2")),
            Step::Dial(3),
            Step::Send(3, publish(P, seq(3, 1), NODE, "live 3")),
            Step::End(2),
            Step::Send(3, fetch(0, "/>", 100)),
            Step::Send(3, will(P, will_at(4), NODE, "away 4")),
            Step::Send(3, publish(P, ACK_SEQ_BASE | 77, LANE, "ack")),
            Step::End(3),
            Step::Dial(4),
            Step::Send(4, fetch(0, "/>", 100)),
            Step::Send(4, will(P, will_at(5), NODE, "first")),
            Step::Send(4, will(P, will_at(5), NODE, "second")),
            Step::Dial(5),
            Step::Send(5, will(Q, will_at(1), PRES_B, "q gone")),
            Step::Send(4, budget(Q)),
            Step::End(4),
            Step::End(5),
            Step::Dial(6),
            Step::Send(6, budget(Q)),
            Step::Send(6, will(P, will_at(6), NODE, "away 6")),
            Step::Dial(7),
            Step::Send(7, publish(P, will_at(6), NODE, "away 6")),
            Step::End(6),
            Step::Dial(9),
            Step::Send(9, will(Q, will_at(3), PRES_B, "q again")),
            Step::Send(
                9,
                Request::Subscribe {
                    from_offset: 0,
                    filter: "no-slash".to_string(),
                },
            ),
            Step::Deliver(9),
            Step::Deliver(9),
            Step::End(9),
        ];
        s.extend(look(7));
        s.extend([
            Step::Send(7, will(P, will_at(7), NODE, "away 7")),
            Step::Stop(super::Stop::Crash),
            Step::Start,
            Step::Dial(8),
            Step::Send(8, Request::Hello),
        ]);
        s.extend(look(8));
        s
    }

    /// COMMITS max-folded, and a group's resume on a tail, across a restart.
    pub(crate) fn groups() -> Vec<Step> {
        let mut s = vec![
            Step::Dial(1),
            Step::Send(1, Request::Hello),
            Step::Pipeline(
                1,
                vec![
                    publish(P, seq(1, 1), LANE, "m1"),
                    publish(P, seq(1, 2), LANE_B, "m2"),
                    publish(P, seq(1, 3), PRES_A, "live"),
                    publish(P, seq(1, 4), LANE, "m3"),
                    publish(P, seq(1, 5), LANE_B, "m4"),
                ],
            ),
            Step::Send(1, commit(GROUP, 1)),
            Step::Send(1, commit(GROUP, 0)),
            Step::Send(1, commit(PERSON_GROUP, 2)),
            Step::Send(1, publish(Q, seq(1, 1), PERSON, "to alice")),
            Step::Dial(2),
            Step::Send(2, Request::IdleMarks),
            Step::Send(
                2,
                Request::SubscribeGroup {
                    group: GROUP.to_string(),
                    filter: INBOX.to_string(),
                },
            ),
            Step::Deliver(2),
            Step::Deliver(2),
            Step::Send(1, publish(P, seq(1, 6), LANE, "m5")),
            Step::Deliver(2),
            Step::Send(1, commit(GROUP, 4)),
            Step::Dial(3),
            Step::Send(
                3,
                Request::Subscribe {
                    from_offset: 0,
                    filter: "/>".to_string(),
                },
            ),
            Step::Deliver(3),
            Step::Deliver(3),
            Step::Deliver(3),
            Step::Deliver(3),
            Step::Deliver(3),
            Step::Deliver(3),
            Step::Deliver(3),
            Step::Stop(super::Stop::Clean),
            Step::Start,
            Step::Dial(4),
            Step::Send(4, Request::Hello),
            Step::Send(4, commit(GROUP, 3)),
            Step::Dial(5),
            Step::Send(
                5,
                Request::SubscribeGroup {
                    group: GROUP.to_string(),
                    filter: INBOX.to_string(),
                },
            ),
            Step::Deliver(5),
            Step::Stop(super::Stop::Crash),
            Step::Start,
            Step::Dial(6),
            Step::Send(
                6,
                Request::SubscribeGroup {
                    group: PERSON_GROUP.to_string(),
                    filter: "/f/f/in/p/alice/>".to_string(),
                },
            ),
            Step::Deliver(6),
        ];
        s.extend([Step::Dial(7)]);
        s.extend(look(7));
        s
    }

    /// A HELD COMMIT RIDING A BURST (critic items B08 and C13): the bridge sends a round's
    /// held inbox commit ahead of its next publish, or of its bus mirror's slots, as one burst
    /// in one write (`Link::send_burst`), which astream takes into one batch. Each burst here
    /// is a commit, then what it rides: one commit that moves the group; one max-folded below
    /// it, beside a resend the dedup answers; one beside a publish its subject refuses, the
    /// commit landing all the same; and one ahead of three mirror slots on a session's `ev`
    /// face, one of them a resend. Then a group tail resumes past the last commit that moved
    /// the group, before and after a restart.
    pub(crate) fn corked() -> Vec<Step> {
        let group = || Request::SubscribeGroup {
            group: GROUP.to_string(),
            filter: INBOX.to_string(),
        };
        let mut s = vec![
            Step::Dial(1),
            Step::Send(1, Request::Hello),
            Step::Send(1, publish(Q, seq(1, 1), LANE, "ask 1")),
            Step::Send(1, publish(Q, seq(1, 2), LANE_B, "ask 2")),
            Step::Pipeline(
                1,
                vec![commit(GROUP, 0), publish(P, seq(1, 1), LANE_B, "reply 1")],
            ),
            Step::Pipeline(
                1,
                vec![commit(GROUP, 3), publish(P, seq(1, 2), LANE, "reply 2")],
            ),
            Step::Pipeline(
                1,
                vec![commit(GROUP, 1), publish(P, seq(1, 2), LANE, "reply 2")],
            ),
            Step::Pipeline(
                1,
                vec![commit(GROUP, 5), publish(P, seq(1, 3), "/f//x", "x")],
            ),
            Step::Dial(2),
            Step::Send(2, Request::IdleMarks),
            Step::Send(2, group()),
            Step::Send(1, publish(P, seq(1, 3), LANE, "after")),
            Step::Deliver(2),
            Step::Pipeline(
                1,
                vec![
                    commit(GROUP, 6),
                    publish(P, seq(1, 4), EV, "snap 1"),
                    publish(P, seq(1, 5), EV, "snap 2"),
                    publish(P, seq(1, 4), EV, "snap 1"),
                ],
            ),
            Step::Stop(super::Stop::Clean),
            Step::Start,
            Step::Dial(3),
            Step::Send(3, group()),
            Step::Deliver(3),
        ];
        s.push(Step::Dial(4));
        s.extend(look(4));
        s
    }

    /// RESTARTS: a clean close, an unclean stop in the same boot, a reopen after a power loss in the
    /// next boot, and two in boots the open cannot name ([`Step::RebootUnnamed`]): the first over a
    /// log a named boot's open recorded, the second over one an unnamed boot's open recorded, which
    /// astream must take for another boot too.
    pub(crate) fn restarts() -> Vec<Step> {
        let mut s = vec![
            Step::Dial(1),
            Step::Send(1, Request::Hello),
            Step::Send(1, publish(P, seq(1, 1), PRES_A, "live")),
            Step::Send(1, publish(P, seq(1, 2), LANE, "m1")),
            Step::Send(1, will(P, will_at(1), PRES_A, "gone")),
            Step::Stop(super::Stop::Clean),
            Step::Start,
            Step::Dial(2),
            Step::Send(2, Request::Hello),
            Step::Send(2, publish(P, seq(2, 1), PRES_A, "live 2")),
            Step::Send(2, publish(P, seq(1, 2), LANE, "m1")),
            Step::Stop(super::Stop::Crash),
            Step::Start,
            Step::Dial(3),
            Step::Send(3, Request::Hello),
            Step::Send(3, publish(P, seq(2, 2), LANE, "m2")),
            Step::Send(3, will(P, will_at(2), PRES_A, "gone 2")),
            Step::Reboot,
            Step::Dial(4),
            Step::Send(4, Request::Hello),
            Step::Send(4, publish(P, seq(2, 2), LANE, "m2")),
            Step::Send(4, publish(P, seq(3, 1), LANE, "m3")),
            Step::Stop(super::Stop::Clean),
            Step::Start,
            Step::Dial(5),
            Step::Send(5, Request::Hello),
        ];
        s.extend(look(5));
        s.extend([
            Step::RebootUnnamed,
            Step::Dial(6),
            Step::Send(6, Request::Hello),
            Step::Send(6, publish(P, seq(3, 2), LANE, "m4")),
            Step::Stop(super::Stop::Clean),
            Step::Start,
            Step::Dial(7),
            Step::Send(7, Request::Hello),
            Step::Send(7, publish(P, seq(3, 3), LANE, "m5")),
            Step::RebootUnnamed,
            Step::Dial(8),
            Step::Send(8, Request::Hello),
        ]);
        s.extend(look(8));
        s
    }

    /// One session's bulk stream (the prefix rule's under aterm's rules).
    pub(crate) const EV: &str = "/f/f/pub/n-sim/s-a/ev";
    /// The group whose read gives this node's mail up under aterm's rules
    /// (`broker_rules::MAIL_READER` expanded for `LANE`).
    pub(crate) const MAIL_GROUP: &str = "/f/f/cur/n-sim/node/inbox";
    /// A receipt on `LANE`'s session's lane, which the changed rules keep the last of only.
    pub(crate) const ACK: &str = "/f/f/in/n-sim/s-a/n-sim/ack";
    /// A day on the run's clock ([`super::Clock`]).
    const DAY: u64 = 24 * 60 * 60 * 1000;

    /// RETENTION'S CUTS (sim item B05), aterm's rules on both arms: a carried cut that takes
    /// the stream and presence and carries the mail and the halt; a `Fetch` below the floor
    /// (refused `retained-from=`), a head query, a fetch of the held mail and `Last` of a
    /// row the cut took; a resend whose dedup key the cut dropped (appended anew); a read
    /// mail given up a day on; a reader told the gaps below its fetches (paged, and refused
    /// where its filter holds nothing below the oldest); a tail told the gap below it and
    /// handed the held mail; the week the age rule gives the unread mail, the halt and the
    /// stream up; and an open that sets the rules, one that sets none, and one that sets
    /// them again; then an open under changed rules and the one after it, back on aterm's,
    /// which leave the mail carried below the base below its oldest (`Step::StartWith`).
    pub(crate) fn retention() -> Vec<Step> {
        let mut s = vec![
            Step::Dial(1),
            Step::Send(1, Request::Hello),
            Step::Pipeline(
                1,
                vec![
                    publish(P, seq(1, 1), EV, "e1"),
                    publish(P, seq(1, 2), EV, "e2"),
                    publish(P, seq(1, 3), PRES_A, "live"),
                    publish(P, seq(1, 4), LANE, "m1"),
                    publish(P, seq(1, 5), LANE_B, "m2"),
                    publish(P, seq(1, 6), HALT, "on"),
                    publish(P, seq(1, 7), EV, "e3"),
                    publish(P, seq(1, 8), LANE, "m3"),
                ],
            ),
            Step::Tick(0),
            Step::Tick(0),
            Step::Cut(6),
            Step::Send(1, fetch(0, EV, 10)),
            Step::Send(1, fetch(0, EV, 0)),
            Step::Send(1, fetch(0, INBOX, 10)),
            Step::Send(1, last(PRES_A, "", 10)),
            Step::Send(1, publish(P, seq(1, 2), EV, "e2")),
            Step::Send(1, commit(MAIL_GROUP, 3)),
            Step::Tick(DAY),
            Step::Send(1, fetch(0, LANE, 10)),
            Step::Send(1, fetch(4, LANE_B, 10)),
            // A reader told what retention gave up (`BrokerLog::fetch_told`): a filter whose
            // subjects have different floors reads what it still holds below its oldest,
            // the notice ahead of the page, page by page; one holding none there is refused.
            Step::Dial(7),
            Step::Send(7, Request::GapNotices),
            Step::Send(7, fetch(0, "/f/f/>", 10)),
            Step::Send(7, fetch(0, "/f/f/>", 1)),
            Step::Send(7, fetch(4, "/f/f/>", 1)),
            Step::Send(7, fetch(0, INBOX, 10)),
            Step::Send(7, fetch(0, EV, 10)),
            Step::Send(7, fetch(0, EV, 0)),
            Step::Dial(2),
            Step::Send(2, Request::GapNotices),
            Step::Send(
                2,
                Request::Subscribe {
                    from_offset: 0,
                    filter: INBOX.to_string(),
                },
            ),
            Step::Deliver(2),
            Step::Deliver(2),
            Step::Deliver(2),
            Step::Tick(8 * DAY),
            Step::Send(1, fetch(0, INBOX, 10)),
            Step::Send(1, fetch(0, "/f/f/fleet/>", 10)),
            Step::Send(7, fetch(0, "/f/f/>", 10)),
            Step::Tick(8 * DAY),
            Step::Stop(super::Stop::Clean),
            Step::Start,
            Step::Dial(3),
            Step::Send(3, Request::Hello),
            Step::Send(3, fetch(0, LANE_B, 10)),
            Step::Tick(8 * DAY),
            Step::Stop(super::Stop::Clean),
            Step::StartUnretained,
            Step::Dial(4),
            Step::Send(4, Request::Hello),
            Step::Send(4, fetch(0, LANE_B, 10)),
            Step::Send(4, publish(P, seq(2, 1), EV, "e4")),
            Step::Tick(8 * DAY),
            Step::Stop(super::Stop::Clean),
            Step::Start,
            Step::Tick(9 * DAY),
            Step::Dial(5),
            Step::Send(5, Request::Hello),
        ];
        s.extend(look(5));
        // An open under changed rules and the one after it on aterm's (`world::changed_rules`):
        // they give the receipt carried below the base up, and the open setting aterm's again
        // takes the mail class whole from the base alone, so the mail carried below it lies
        // below every lane's oldest: refused to a plain read, read by a told one and by a tail.
        let older = super::super::world::changed_rules(false);
        s.extend([
            Step::Pipeline(
                5,
                vec![
                    publish(P, seq(3, 1), LANE, "m4"),
                    publish(P, seq(3, 2), ACK, "r1"),
                    publish(P, seq(3, 3), LANE_B, "m5"),
                    publish(P, seq(3, 4), EV, "e5"),
                ],
            ),
            Step::Tick(10 * DAY),
            Step::Cut(u64::MAX),
            Step::Stop(super::Stop::Clean),
            Step::StartWith(older),
            Step::Tick(10 * DAY),
            Step::Dial(9),
            Step::Send(9, Request::Hello),
            Step::Send(9, fetch(0, INBOX, 10)),
            Step::Send(9, fetch(0, ACK, 10)),
            Step::Stop(super::Stop::Clean),
            Step::Start,
            Step::Tick(10 * DAY),
            Step::Dial(10),
            Step::Send(10, Request::Hello),
            Step::Send(10, fetch(0, LANE, 10)),
            Step::Send(10, Request::GapNotices),
            Step::Send(10, fetch(0, LANE, 10)),
            Step::Send(10, fetch(0, INBOX, 10)),
            Step::Dial(11),
            Step::Send(11, Request::GapNotices),
            Step::Send(
                11,
                Request::Subscribe {
                    from_offset: 0,
                    filter: INBOX.to_string(),
                },
            ),
            Step::DeliverAny(11),
            Step::DeliverAny(11),
            Step::DeliverAny(11),
        ]);
        s
    }
}

/// The scripts that need a long log.
pub(crate) mod long {
    use super::scripts::{budget, fetch, last, publish, retire, seq, will, will_at, NODE, P, Q};
    use super::{Request, Step};
    use astream_broker::broker::FETCH_SCAN_MAX;
    use astream_broker::store::MAX_SUBJECTS_PER_PRODUCER;

    /// How many publishes one pipelined step carries.
    const CHUNK: u64 = 1024;

    fn row(i: u64) -> String {
        format!("/f/f/pub/n-sim/s-{i}/presence")
    }

    /// `n` publishes of `producer`'s, the `k`-th (from `first`) by `make`.
    fn pipelined(c: u64, first: u64, n: u64, make: impl Fn(u64) -> Request) -> Vec<Step> {
        let mut steps = Vec::new();
        let mut k = first;
        while k < first + n {
            let end = (k + CHUNK).min(first + n);
            steps.push(Step::Pipeline(c, (k..end).map(&make).collect()));
            k = end;
        }
        steps
    }

    /// THE SUBJECT BUDGET.
    pub(crate) fn budgets() -> Vec<Step> {
        let max = MAX_SUBJECTS_PER_PRODUCER as u64;
        let mut s = vec![
            Step::Dial(1),
            Step::Send(1, Request::Hello),
            Step::Dial(2),
            Step::Send(2, will(P, will_at(9), NODE, "away")),
        ];
        s.extend(pipelined(1, 0, max - 1, |i| {
            publish(P, seq(1, i + 1), &row(i), "live")
        }));
        s.extend([
            Step::Send(1, budget(P)),
            Step::Send(1, publish(P, seq(1, max), &row(max), "live")),
            Step::Send(1, publish(P, seq(1, max + 1), &row(0), "again")),
            Step::Send(1, publish(Q, seq(1, 1), &row(max), "q's")),
            Step::Send(1, publish(P, seq(1, max + 2), &row(max), "p's")),
            Step::Dial(3),
            Step::Send(3, will(P, will_at(9), &row(max + 1), "away")),
            Step::Send(1, publish(P, seq(1, max + 3), NODE, "live")),
            Step::Send(1, budget(P)),
            Step::Send(1, publish(P, seq(1, max + 4), &row(max + 1), "live")),
            Step::Send(1, retire(P, &row(0))),
            Step::Send(1, budget(P)),
            Step::Send(1, publish(P, seq(1, max + 5), &row(max + 1), "live")),
            Step::End(2),
            Step::End(3),
            Step::Send(1, budget(P)),
            Step::Send(1, budget(Q)),
            Step::Send(1, last("/f/f/pub/n-sim/s-4095/>", "", 10)),
            // More subjects than `LAST_PAGE_MAX`: one page, cut at it, resuming.
            Step::Send(1, last("/>", "", u32::MAX)),
        ]);
        s
    }

    /// `FETCH`'S SCAN BOUND.
    pub(crate) fn scan_bound() -> Vec<Step> {
        let n = FETCH_SCAN_MAX as u64 + 5;
        let lane = super::scripts::LANE;
        let pres = super::scripts::PRES_A;
        // A log past the scan bound is more records than any producer's burst (aterm's
        // `broker_rules::RATE_BURST_RECORDS`), published faster than the model's clock, which
        // stands still, refills it: unlimited.
        let mut s = vec![
            Step::Dial(1),
            Step::Send(1, Request::Hello),
            Step::RateLimit(None),
        ];
        s.extend(pipelined(1, 1, n, |k| publish(P, seq(1, k), lane, "m")));
        s.extend([
            Step::Send(1, publish(P, seq(1, n + 1), pres, "live")),
            Step::Send(1, fetch(0, pres, 10)),
            Step::Send(1, fetch(FETCH_SCAN_MAX as u64, pres, 10)),
            Step::Send(1, fetch(FETCH_SCAN_MAX as u64 - 3, lane, 2)),
            Step::Send(1, fetch(0, "/>", 0)),
            Step::Send(1, last("/>", "", 10)),
        ]);
        s
    }
}

/// THE WORLDS whose journals are replayed, `(seed, tier)`: on every tier, one
/// with the mirror off, one mirroring onto a retained broker and one onto an
/// unretained broker ([`super::world::Opts::seed`]'s alternations), as
/// [`what_real_bridges_sent_astream_answers_as_the_model_did`] checks.
const WORLDS: &[(u64, Tier)] = &[
    (1, Tier::Strict),
    (7, Tier::Strict),
    (19, Tier::Strict),
    (4, Tier::Ordered),
    (2, Tier::Ordered),
    (22, Tier::Ordered),
    (5, Tier::Relaxed),
    (3, Tier::Relaxed),
    (18, Tier::Relaxed),
];

/// A world of [`WORLDS`], on one device that never loses power, a broker that
/// answers every ack at once and rolls back no batch, and a disk that never
/// fails: what a restart may lose, when an ack is read and a batch astream
/// could not commit are the law check's and the seeded worlds'. (A batch the
/// world's broker rolled back is one the real arm commits, so the log the
/// journal's deliveries were read from is not the one replayed.)
fn journalled_opts(seed: u64, tier: Tier) -> super::world::Opts {
    super::world::Opts {
        tier: Some(tier),
        // Retained at the shipped rules where `seed / 16` is even: six of the nine [`WORLDS`],
        // the three at seeds 16 and over (one a tier) not. Never scaled: a scaled world's
        // ticks the real broker's writer asks for by the wall clock, never the run's.
        retained: (seed / 16).is_multiple_of(2),
        scaled: false,
        window: None,
        power_cuts: false,
        bus_losses: false,
        hold_acks: false,
        refused_batches: false,
        write_errors: false,
        journal: true,
        ..super::world::Opts::seed(seed)
    }
}

fn journalled(seed: u64, tier: Tier) -> super::world::Outcome {
    super::world::run(&journalled_opts(seed, tier))
}

/// Whether astream's broker serves `tier` as asked on this host: off Apple it serves an
/// Ordered log Strict (`NotApple`: no barrier to order by), a tier the Strict comparison holds
/// the model to, while the model's device keeps the barrier it simulates everywhere.
fn served_here(tier: Tier) -> bool {
    tier != Tier::Ordered || cfg!(target_vendor = "apple")
}

/// **THE MODEL ANSWERS EVERY SCRIPT AS ASTREAM'S BROKER DOES**, on every
/// tier the host serves: each verb the bridge sends and each edge astream defines for it.
#[test]
fn the_model_answers_every_script_as_astreams_broker_does() {
    let mut failed = Vec::new();
    for tier in [Tier::Strict, Tier::Ordered, Tier::Relaxed]
        .into_iter()
        .filter(|&tier| served_here(tier))
    {
        for (name, script) in every_script() {
            if let Err(e) = run(tier, ArmMutant::None, &script) {
                failed.push(format!("{name}: {e}"));
            }
        }
    }
    assert!(
        failed.is_empty(),
        "the model diverged:\n{}",
        failed.join("\n")
    );
}

/// Each verb the bridge sends and each edge astream defines for it.
fn every_script() -> [(&'static str, Vec<Step>); 10] {
    [
        ("publishing", scripts::publishing()),
        ("the rate limit", scripts::rate_limited()),
        ("binary payload", scripts::binary_payload()),
        ("retiring", scripts::retiring()),
        ("retiring conditionally", scripts::conditional()),
        ("wills", scripts::wills()),
        ("groups", scripts::groups()),
        ("a commit and a publish in one write", scripts::corked()),
        ("restarts", scripts::restarts()),
        ("retention", scripts::retention()),
    ]
}

/// The key the sealed arms' logs are sealed under.
const KEY: [u8; 32] = *b"conform-broker-at-rest-key-32-by";

/// **A SEALED LOG ANSWERS AS A PLAINTEXT ONE**: every hand-written script, each where its
/// plaintext run plays it ([`every_script`] on every tier the host serves, [`long_scripts`] on
/// Relaxed), the real broker's log sealed at rest as `aterm link broker` opens one with its
/// `rest.key`, retained by aterm's rules. The seal lives below everything a client can see, and
/// each run's files show it was there: a `.seal`, and no subject in the clear in the log or a
/// sidecar. NEGATIVE CONTROLS: the plaintext arm of a script has no `.seal` and holds its
/// subjects, and a model that forgets its dedup, or its subject budget, is still caught through
/// the keyed arm, so the arm compares, at the budget too.
#[test]
fn a_sealed_log_answers_every_script_as_the_model_does() {
    let arm = |tier, key, mutant, script: &[Step]| {
        run_keeping(
            tier,
            Some(crate::broker_rules::default_rules()),
            key,
            None,
            mutant,
            script,
        )
    };
    let runs = [Tier::Strict, Tier::Ordered, Tier::Relaxed]
        .into_iter()
        .filter(|&tier| served_here(tier))
        .flat_map(|tier| every_script().map(|(name, script)| (tier, name, script)))
        .chain(long_scripts().map(|(name, script)| (Tier::Relaxed, name, script)));
    let mut failed = Vec::new();
    for (tier, name, script) in runs {
        match arm(tier, Some(KEY), ArmMutant::None, &script) {
            Err(e) => failed.push(format!("{name}: {e}")),
            Ok(real) if !real.bound_at_rest() => {
                failed.push(format!("{name} at {tier}: no `.seal`: nothing sealed it"));
            }
            Ok(real) => {
                let holding = real.holding_subjects();
                if !holding.is_empty() {
                    failed.push(format!("{name} at {tier}: in the clear in {holding:?}"));
                }
            }
        }
    }
    assert!(
        failed.is_empty(),
        "the model diverged from a sealed log:\n{}",
        failed.join("\n")
    );
    let plain = arm(Tier::Relaxed, None, ArmMutant::None, &scripts::publishing())
        .expect("the plaintext arm");
    assert!(!plain.bound_at_rest(), "CONTROL: no key, no `.seal`");
    assert_eq!(
        plain.holding_subjects(),
        ["bus.log"],
        "CONTROL: the plaintext log holds its subjects"
    );
    assert!(
        arm(
            Tier::Relaxed,
            Some(KEY),
            ArmMutant::FreshDedup,
            &scripts::publishing()
        )
        .is_err(),
        "CONTROL: the keyed arm answered a forgetful model's resend as astream answers it"
    );
    assert!(
        arm(
            Tier::Relaxed,
            Some(KEY),
            ArmMutant::PublishPastTheBudget,
            &long::budgets()
        )
        .is_err(),
        "CONTROL: the keyed arm answered a model with no subject budget as astream answers it"
    );
}

/// THE COLD ARM'S resident budget: the newest record alone stays decoded, so every record a
/// read takes below a log's head comes back from the file through astream's paged cold tier.
const COLD: Option<usize> = Some(0);

/// The hand-written scripts that read no record below their log's head, the one record a
/// budget of 0 keeps resident: the scripts the cold arm reads nothing back for.
const HEAD_ONLY: &[&str] = &["binary payload"];

/// **A COLD LOG ANSWERS AS A RESIDENT ONE** (critic item F06). The shipped broker keeps 16 MiB of
/// decoded records resident ([`crate::durability::RESIDENT_BUDGET`]) of up to 352 MiB its rules
/// retain, so most of the history it serves it reads back from its log file through astream's
/// paged cold tier, which no run above reaches: the longest script, [`long::scan_bound`], stays
/// under aterm's budget. Here the real broker keeps none resident ([`COLD`]) and plays what the
/// resident arms play, retained as they are: every hand-written script, plaintext and sealed
/// under [`KEY`] (each sealed run's log has its `.seal`), on the tiers its resident runs play
/// (every tier the host serves for [`every_script`], Relaxed for [`long_scripts`]), the door's
/// journal of each of the [`WORLDS`] at its world's rules, and the reclaim scenario's on every
/// tier the host serves. Residency is on no answer, so the model answers each as it answers a
/// resident log.
///
/// The arm provably reached the cold tier: every run of a script that reads below its head
/// (each but [`HEAD_ONLY`]: a `Fetch` or a `Last` page, a group's resume, a tail's catch-up, the
/// records retention's tick weighs under aterm's hold rules), and of every journal, read
/// records back cold over the run (`Broker::cold_reads` over every open, [`Real::cold_reads`]),
/// and no [`HEAD_ONLY`] run read any. The count is the whole run's: the harness's waits and
/// `RateLimited`'s comparison poll `Broker::stats`, which reads no record (astream's latest-will
/// map, critic item F19), so every cold read is a script's request, delivery, tick or cut
/// being served, whether in its step or by the broker's own threads after it (a tail catching
/// up after a subscribe).
/// NEGATIVE CONTROLS: a model that forgets its dedup (`FreshDedup`), or its subject budget
/// (`PublishPastTheBudget`, on the subject budget's long script), is still caught on the cold
/// arm, and at aterm's own budget a script the cold arm reads back records for reads none, so
/// the cold reads are the budget's doing.
#[test]
fn a_cold_log_answers_every_script_as_the_model_does() {
    let rules = || Some(crate::broker_rules::default_rules());
    let served: Vec<Tier> = [Tier::Strict, Tier::Ordered, Tier::Relaxed]
        .into_iter()
        .filter(|&tier| served_here(tier))
        .collect();
    let scripts: Vec<_> = [None, Some(KEY)]
        .into_iter()
        .flat_map(|key| {
            served
                .iter()
                .flat_map(move |&tier| {
                    every_script().map(move |(name, script)| (tier, key, name, script))
                })
                .chain(long_scripts().map(move |(name, script)| (Tier::Relaxed, key, name, script)))
        })
        .collect();
    let mut runs = aterm_spec::check::each(&scripts, |(tier, key, name, script)| {
        let what = format!(
            "{name} at {tier}{}",
            if key.is_some() { ", sealed" } else { "" }
        );
        let verdict =
            run_keeping(*tier, rules(), *key, COLD, ArmMutant::None, script).and_then(|real| {
                if key.is_some() && !real.bound_at_rest() {
                    Err("no `.seal`: nothing sealed it".to_string())
                } else {
                    Ok(real.cold_reads())
                }
            });
        (what, !HEAD_ONLY.contains(name), verdict)
    });
    let worlds: Vec<(u64, super::world::Opts)> = WORLDS
        .iter()
        .filter(|&&(_, tier)| served_here(tier))
        .map(|&(seed, tier)| (seed, journalled_opts(seed, tier)))
        .collect();
    runs.extend(aterm_spec::check::each(&worlds, |(seed, opts)| {
        let tier = opts.tier.expect("a journalled world names its tier");
        let steps = replay(&super::world::run(opts).journal);
        let verdict = run_keeping(tier, opts.rules(), None, COLD, ArmMutant::None, &steps)
            .map(|real| real.cold_reads());
        (format!("seed {seed}'s journal at {tier}"), true, verdict)
    }));
    runs.extend(served.iter().map(|&tier| {
        let opts = super::world::Opts {
            journal: true,
            ..super::world::Opts::reclaiming(5, tier)
        };
        let steps = replay(&super::world::run_reclaim(&opts, 3, true).journal);
        let verdict = run_keeping(tier, opts.rules(), None, COLD, ArmMutant::None, &steps)
            .map(|real| real.cold_reads());
        (format!("the reclaim journal at {tier}"), true, verdict)
    }));
    let mut cold = 0;
    let mut failed = Vec::new();
    let mut fewest: Option<(u64, &str)> = None;
    for (what, below_the_head, verdict) in &runs {
        match *verdict {
            Err(ref e) => failed.push(format!("{what}: {e}")),
            Ok(0) if *below_the_head => failed.push(format!(
                "{what}: read no record back cold, so no answer came from the cold tier"
            )),
            Ok(reads) if !*below_the_head && reads > 0 => failed.push(format!(
                "{what}: read {reads} records back cold, so it reads below its head"
            )),
            Ok(reads) => {
                cold += reads;
                if *below_the_head && fewest.is_none_or(|(least, _)| reads < least) {
                    fewest = Some((reads, what));
                }
            }
        }
    }
    eprintln!(
        "the cold arm read {cold} records back cold over {} runs (the fewest {fewest:?})",
        runs.len()
    );
    assert!(
        failed.is_empty(),
        "the model diverged from a cold log:\n{}",
        failed.join("\n")
    );
    assert!(cold > 0, "the cold arm read nothing back cold");
    let resident = run_keeping(
        Tier::Relaxed,
        rules(),
        None,
        None,
        ArmMutant::None,
        &scripts::publishing(),
    )
    .expect("the resident arm");
    assert_eq!(
        resident.cold_reads(),
        0,
        "CONTROL: at aterm's budget the publishing script reads records back cold"
    );
    assert!(
        run_keeping(
            Tier::Relaxed,
            rules(),
            None,
            COLD,
            ArmMutant::FreshDedup,
            &scripts::publishing()
        )
        .is_err(),
        "CONTROL: the cold arm answered a forgetful model's resend as astream answers it"
    );
    assert!(
        run_keeping(
            Tier::Relaxed,
            rules(),
            None,
            COLD,
            ArmMutant::PublishPastTheBudget,
            &long::budgets()
        )
        .is_err(),
        "CONTROL: the cold arm answered a model with no subject budget as astream answers it"
    );
}

/// The scripts that need a long log ([`long`]), played on Relaxed alone: the bounds are the same
/// on every tier, and a Strict flush per batch would only cost time.
fn long_scripts() -> [(&'static str, Vec<Step>); 2] {
    [
        ("budgets", long::budgets()),
        ("scan bound", long::scan_bound()),
    ]
}

/// **THE MODEL HOLDS ASTREAM'S SUBJECT BUDGET AND FETCH SCAN BOUND**, on a
/// log long enough to reach each, on Relaxed ([`long_scripts`]).
#[test]
fn the_model_holds_astreams_subject_budget_and_fetch_scan_bound() {
    let mut failed = Vec::new();
    for (name, script) in long_scripts() {
        if let Err(e) = run(Tier::Relaxed, ArmMutant::None, &script) {
            failed.push(format!("{name}: {e}"));
        }
    }
    assert!(
        failed.is_empty(),
        "the model diverged:\n{}",
        failed.join("\n")
    );
}

/// **WHAT REAL BRIDGES SENT, ASTREAM ANSWERS AS THE MODEL DID**: the door's
/// journal of each of the [`WORLDS`], replayed on both arms, each retained as
/// the world's broker was.
#[test]
fn what_real_bridges_sent_astream_answers_as_the_model_did() {
    let worlds: Vec<(u64, super::world::Opts)> = WORLDS
        .iter()
        .map(|&(seed, tier)| (seed, journalled_opts(seed, tier)))
        .collect();
    for tier in [Tier::Strict, Tier::Ordered, Tier::Relaxed] {
        for (mirror, retained, what) in [
            (false, None, "the mirror off"),
            (true, Some(true), "a mirror onto a retained broker"),
            (true, Some(false), "a mirror onto an unretained broker"),
        ] {
            assert!(
                worlds.iter().any(|(_, o)| o.tier == Some(tier)
                    && o.mirror == mirror
                    && retained.is_none_or(|r| o.retained == r)),
                "no journalled world on {tier} with {what}"
            );
        }
    }
    let replayed = aterm_spec::check::each(&worlds, |(seed, opts)| {
        let tier = opts.tier.expect("a journalled world names its tier");
        let out = super::world::run(opts);
        assert!(
            out.violations.is_empty(),
            "seed {seed}'s world broke a law: {:?}",
            out.violations
        );
        let steps = replay(&out.journal);
        assert!(
            steps.iter().any(|s| matches!(s, Step::Stop(_))),
            "seed {seed}'s world never restarted its broker"
        );
        if !served_here(tier) {
            return (0, 0, Ok(()));
        }
        let requests = steps.iter().filter(|s| matches!(s, Step::Send(..))).count();
        let cuts = steps.iter().filter(|s| matches!(s, Step::Cut(_))).count();
        let verdict = run_on(tier, opts.rules(), ArmMutant::None, &steps)
            .map_err(|e| format!("seed {seed}: {e}"));
        (requests, cuts, verdict)
    });
    let requests: usize = replayed.iter().map(|(n, _, _)| n).sum();
    let cuts: usize = replayed.iter().map(|(_, c, _)| c).sum();
    eprintln!("replayed {requests} requests real bridges sent, and {cuts} cuts");
    assert!(
        cuts > 0,
        "no replayed world cut its broker's log (sim item B05)"
    );
    let failed: Vec<&String> = replayed
        .iter()
        .filter_map(|(_, _, v)| v.as_ref().err())
        .collect();
    assert!(
        failed.is_empty(),
        "the model diverged:\n{}",
        failed
            .iter()
            .map(|e| e.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// THE WORLDS whose journals the refinement arm replays (critic item F10), `(seed, tier)`: two a
/// tier, each with power cuts, bus losses and rolled-back batches, on an unretained broker
/// ([`refined_opts`]).
const REFINED: &[(u64, Tier)] = &[
    (1, Tier::Strict),
    (5, Tier::Strict),
    (2, Tier::Ordered),
    (6, Tier::Ordered),
    (3, Tier::Relaxed),
    (7, Tier::Relaxed),
];

/// A world of [`REFINED`]: [`journalled_opts`]' with what the refinement arm holds the real store
/// to switched on — the machine losing power, the broker alone losing it, batches the model rolls
/// back — and unretained (retention's rewrite is a file's, refused on a simulated device).
fn refined_opts(seed: u64, tier: Tier) -> super::world::Opts {
    super::world::Opts {
        power_cuts: true,
        bus_losses: true,
        refused_batches: true,
        retained: false,
        ..journalled_opts(seed, tier)
    }
}

/// **THE REAL STORE'S RECOVERIES AND ROLLED-BACK BATCHES STAY INSIDE THE MODEL'S LAW** (critic
/// item F10): the door's journal of each of the [`REFINED`] worlds, replayed on the refinement arm
/// ([`run_refining`]): every answer compared as every run's are, the images each power loss could
/// leave on astream's simulated device (at most [`IMAGES`] for each prefix of its barrier epochs, a
/// seeded sample past that) reopened and held to the model's law ([`refine`]), and each batch the
/// model rolled back a write the real arm failed, nothing of it visible ([`answer_refused`]). Not
/// vacuous: every world loses power, an Ordered and a Relaxed log (neither keeps a horizon there,
/// so their losses are the journal's alone) each lose acked records in some image, and some batch
/// is refused.
#[test]
fn the_real_stores_recoveries_and_rolled_back_batches_stay_inside_the_models_law() {
    let worlds: Vec<(u64, Tier)> = REFINED.to_vec();
    let refined = aterm_spec::check::each(&worlds, |&(seed, tier)| {
        let out = super::world::run(&refined_opts(seed, tier));
        assert!(
            out.violations.is_empty(),
            "seed {seed}'s world broke a law: {:?}",
            out.violations
        );
        let steps = replay(&out.journal);
        assert!(
            !steps.iter().any(|s| matches!(s, Step::Cut(_))),
            "seed {seed}: an unretained world cut its log"
        );
        let verdict = run_refining(tier, ArmMutant::None, &steps)
            .map_err(|e| format!("seed {seed} at {tier}: {e}"));
        (seed, tier, verdict)
    });
    let mut failed = Vec::new();
    let mut lost = BTreeMap::<String, u64>::new();
    let mut refused = 0u64;
    for (seed, tier, verdict) in &refined {
        match verdict {
            Ok(r) => {
                eprintln!("seed {seed} at {tier}: {r:?}");
                assert!(r.losses > 0, "seed {seed} at {tier}: no power loss");
                *lost.entry(tier.to_string()).or_default() += r.lost;
                refused += r.refused;
            }
            Err(e) => failed.push(e.as_str()),
        }
    }
    assert!(
        failed.is_empty(),
        "the real store stepped outside the model:\n{}",
        failed.join("\n")
    );
    for tier in [Tier::Ordered, Tier::Relaxed] {
        assert!(
            lost.get(&tier.to_string()).is_some_and(|&n| n > 0),
            "no {tier} image lost an acked record: the refinement judged no cut there"
        );
    }
    assert!(refused > 0, "no batch was rolled back on the real arm");
}

/// CONTROL for the refinement arm: a model whose allowed cuts keep every acked record
/// ([`ArmMutant::AckIsDurable`]) omits the images a real device leaves below the head, and on the
/// first world of each weak tier the refinement arm must find the real store outside it: on
/// Ordered a barrier taken for a full flush, on Relaxed a page-cache write taken for one.
#[test]
fn control_a_model_that_takes_every_ack_for_durable_has_the_real_store_outside_it() {
    for weak in [Tier::Ordered, Tier::Relaxed] {
        let &(seed, tier) = REFINED
            .iter()
            .find(|(_, tier)| *tier == weak)
            .expect("a world of each weak tier");
        let steps = replay(&super::world::run(&refined_opts(seed, tier)).journal);
        let Err(err) = run_refining(tier, ArmMutant::AckIsDurable, &steps) else {
            panic!("CONTROL AckIsDurable at {tier}: the real store stayed inside a model that loses nothing");
        };
        assert!(
            err.contains("no cut the model's law allows"),
            "CONTROL AckIsDurable at {tier} failed for another reason: {err}"
        );
    }
}

/// **WHAT A RECLAIMING BRIDGE SENT, ASTREAM ANSWERS AS THE MODEL DID** (sim item B02):
/// the door's journal of the reclaim scenario ([`super::world::run_reclaim`]: three
/// sessions leave, the bridge gives their lanes and rows back, and one is revived just
/// ahead of its row's retire, a successor's row written first) replayed on both arms on
/// every tier the host serves, each conditional `Retire` answered alike, retired or moot.
#[test]
fn what_a_reclaiming_bridge_sent_astream_answers_as_the_model_did() {
    for tier in [Tier::Strict, Tier::Ordered, Tier::Relaxed] {
        let opts = super::world::Opts {
            journal: true,
            ..super::world::Opts::reclaiming(5, tier)
        };
        let out = super::world::run_reclaim(&opts, 3, true);
        assert!(
            out.violations.is_empty() && out.revived.is_some(),
            "{tier}: the reclaim scenario: {:#?}",
            out.violations
        );
        let steps = replay(&out.journal);
        let retires = steps
            .iter()
            .filter(|s| matches!(s, Step::Send(_, Request::Retire { .. })))
            .count();
        assert!(
            retires >= 6,
            "{tier}: the journal holds {retires} conditional retires, not three sessions' lanes and rows"
        );
        if served_here(tier) {
            run_on(tier, opts.rules(), ArmMutant::None, &steps)
                .unwrap_or_else(|e| panic!("the model diverged: {e}"));
        }
    }
}

/// THE DRIFT each fix in `bus.rs` this test found once undid, as an
/// [`ArmMutant`], with the script that must catch it and what it gets wrong.
fn drift() -> Vec<(ArmMutant, Vec<Step>, &'static str)> {
    vec![
        (
            ArmMutant::FetchClampsBelowTheFloor,
            scripts::retention(),
            "a Fetch below the floor that clamps instead of `retained-from=`",
        ),
        (
            ArmMutant::KeepsDedupBelowTheCut,
            scripts::retention(),
            "a cut that keeps the dedup keys below it",
        ),
        (
            ArmMutant::NoGapNotices,
            scripts::retention(),
            "a tail never told what retention gave up below it",
        ),
        (
            ArmMutant::FetchNeverTold,
            scripts::retention(),
            "a told reader's Fetch refused below the oldest where retention holds its records",
        ),
        (
            ArmMutant::FetchToldNoNotice,
            scripts::retention(),
            "a told Fetch below the oldest with no gap notice ahead of its page",
        ),
        (
            ArmMutant::HelloNoLineage,
            scripts::publishing(),
            "a never-moved log's Hello with no lineage",
        ),
        (
            ArmMutant::RefuseEveryA,
            scripts::publishing(),
            "a publish to an /a/ subject not astream's own",
        ),
        (
            ArmMutant::NoNameBound,
            scripts::publishing(),
            "a name past MAX_NAME_LEN",
        ),
        (
            ArmMutant::FetchNamesNoOldest,
            scripts::publishing(),
            "Fetch's oldest offset",
        ),
        (
            ArmMutant::LastResumesOnlyBeforeMore,
            scripts::publishing(),
            "Last's resume after a full last page",
        ),
        (
            ArmMutant::RetireOnlyAtZero,
            scripts::retiring(),
            "a retire at a sequence other than 0",
        ),
        (
            ArmMutant::RetireIgnoresWills,
            scripts::retiring(),
            "a retire while a will targets the subject",
        ),
        (
            ArmMutant::RetireIgnoresCharge,
            scripts::retiring(),
            "a retire of another producer's subject",
        ),
        (
            ArmMutant::RetireAnyTarget,
            scripts::retiring(),
            "a retire of a reserved or malformed target",
        ),
        (
            ArmMutant::RetireIgnoresItsCondition,
            scripts::conditional(),
            "a conditional retire of a record newer than it names, or on another log",
        ),
        (
            ArmMutant::WillTakesAnySubject,
            scripts::wills(),
            "a will on a reserved or malformed subject",
        ),
        (
            ArmMutant::WillOfASecondProducer,
            scripts::wills(),
            "a second producer's will on one connection",
        ),
        (
            ArmMutant::WillAckHalfRefused,
            scripts::wills(),
            "the code refusing a will in the ack half",
        ),
        (
            ArmMutant::BudgetReservesNothing,
            scripts::wills(),
            "a will's reservation in Budget",
        ),
        (
            ArmMutant::RefusedSubscribeLives,
            scripts::wills(),
            "the end of a refused subscribe's connection",
        ),
        (
            ArmMutant::WillReservesNothing,
            long::budgets(),
            "a will's reservation against the subject budget",
        ),
        (
            ArmMutant::PublishPastTheBudget,
            long::budgets(),
            "a publish's new subject past the budget",
        ),
        (
            ArmMutant::WillPastTheBudget,
            long::budgets(),
            "a will's new subject past the budget",
        ),
        (
            ArmMutant::LastPastItsPage,
            long::budgets(),
            "a Last page past LAST_PAGE_MAX",
        ),
        (
            ArmMutant::FetchUnbounded,
            long::scan_bound(),
            "Fetch's scan bound",
        ),
        (
            ArmMutant::Unretained,
            scripts::publishing(),
            "a retained broker's Hello with no retention",
        ),
        (
            ArmMutant::NoGapFence,
            scripts::publishing(),
            "a publish refused behind its producer's refused one",
        ),
        (
            ArmMutant::FenceUnasked,
            scripts::publishing(),
            "a publish fenced on a connection that never asked",
        ),
        (
            ArmMutant::NoRateLimit,
            scripts::rate_limited(),
            "a publish past its producer's bucket",
        ),
        (
            ArmMutant::RateSkipsTheFenced,
            scripts::rate_limited(),
            "a publish behind a fence the rate limit never counted",
        ),
        (
            ArmMutant::RateLatchesNoFence,
            scripts::rate_limited(),
            "a refusal for the rate that fenced nothing",
        ),
    ]
}

/// **NEGATIVE CONTROL: A MODEL THAT FORGETS ITS DEDUP OR ITS WILL FENCE,
/// CHANGES A BINARY PAYLOAD, OR FALLS BACK TO ANY DRIFT THIS TEST FOUND, IS
/// CAUGHT** by the script that holds it ([`drift`]), and a forgotten dedup by
/// the journal of a world.
#[test]
fn a_model_that_forgets_its_dedup_or_its_will_fence_is_caught() {
    let mut controls = vec![
        (ArmMutant::FreshDedup, scripts::publishing(), "a resend"),
        (ArmMutant::NoWillFence, scripts::wills(), "a fenced will"),
        (
            ArmMutant::AlterPayload,
            scripts::binary_payload(),
            "a changed binary payload",
        ),
    ];
    controls.extend(drift());
    let verdicts = aterm_spec::check::each(&controls, |(mutant, script, _)| {
        run(Tier::Relaxed, *mutant, script)
    });
    let mut missed = Vec::new();
    for ((mutant, _, what), verdict) in controls.iter().zip(verdicts) {
        match verdict {
            Err(e) => eprintln!("CONTROL {mutant:?} caught: {e}"),
            Ok(()) => missed.push(format!("{mutant:?}: {what}")),
        }
    }
    assert!(
        missed.is_empty(),
        "CONTROL: answered as astream answers it:\n  {}",
        missed.join("\n  ")
    );
    let out = journalled(3, Tier::Relaxed);
    let caught = run(Tier::Relaxed, ArmMutant::FreshDedup, &replay(&out.journal));
    assert!(
        caught.is_err(),
        "CONTROL FreshDedup: no resend in a world's journal"
    );
}
