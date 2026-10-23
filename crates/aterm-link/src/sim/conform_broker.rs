// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SIMULATED BROKER HELD TO ASTREAM'S OWN (sim item B01).

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

/// The boot both arms run in, until a [`Step::Reboot`].
const BOOT: [u8; 16] = *b"aterm-sim-B01-bt";

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
    /// The client drops `c`.
    End(u64),
    /// Every connection ends, then the broker stops.
    Stop(Stop),
    /// The broker opens again.
    Start,
    /// Every connection ends, the broker stops uncleanly, and it opens again in a boot it cannot name, every record kept.
    Reboot,
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
    boot: Boot,
    /// Producers whose `/a/will` record the log holds: what an open fires,
    /// one firing each.
    will_producers: BTreeSet<u64>,
    /// Wills queued to this broker's writer since its open: what
    /// `wills_settled` must reach before the log is read again.
    queued: u64,
    dir: Scratch,
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
    fn new(tier: Tier) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-b01")
            .tempdir_in("/tmp")
            .expect("a scratch dir");
        let mut real = Self {
            dir: Scratch {
                dir: Some(dir),
                keep: Arc::new(AtomicBool::new(false)),
            },
            durability: durability(tier),
            boot: Boot::Known(BOOT),
            up: None,
            conns: BTreeMap::new(),
            will_producers: BTreeSet::new(),
            queued: 0,
        };
        real.open();
        real
    }

    fn sock(&self) -> std::path::PathBuf {
        self.dir.path().join("b.sock")
    }

    fn open(&mut self) {
        let mut opts = self.durability.options();
        opts.boot = self.boot;
        opts.volume = Volume::Boot;
        let broker =
            astream_broker::Broker::open_opts(self.dir.path().join("bus.log"), opts, || {
                Ok(astream_broker::Prepared::Nothing)
            })
            .expect("the real broker opens");
        let handle = broker.serve(self.sock()).expect("the real broker serves");
        self.up = Some((broker, handle));
        self.queued = self.will_producers.len() as u64;
        self.settle();
    }

    fn broker(&self) -> &astream_broker::Broker {
        &self.up.as_ref().expect("the real broker is up").0
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
            std::thread::yield_now();
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

    fn write(&mut self, c: u64, reqs: &[Request]) {
        let conn = self.conns.get_mut(&c).expect("a live real connection");
        let mut bytes = Vec::new();
        for r in reqs {
            write_frame(&mut bytes, &encode_request(r)).expect("frame to memory");
        }
        conn.stream
            .write_all(&bytes)
            .expect("write to the real broker");
    }

    fn read(&mut self, c: u64) -> Response {
        let conn = self.conns.get_mut(&c).expect("a live real connection");
        match read_frame(&mut conn.stream) {
            Ok(Some(frame)) => decode_response(&frame).expect("a frame astream decodes"),
            Ok(None) => panic!("the real broker closed connection {c}"),
            Err(e) => panic!("no answer from the real broker on {c} in {HANG:?}: {e}"),
        }
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
        drop(broker);
    }

    fn reboot(&mut self) {
        self.stop(Stop::Crash);
        self.boot = Boot::Unknown;
        self.open();
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
    fn new(tier: Tier, mutant: ArmMutant) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-b01-sim")
            .tempdir_in("/tmp")
            .expect("a scratch dir");
        let broker = bus::Broker::new(tier, table(tier));
        lock(&broker.state).mutant = mutant;
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

    fn reboot(&mut self) {
        self.end_all();
        self.core.broker.stop_keeping_all(Stop::PowerLoss);
        self.core.broker.start();
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
        LOG_POISONED_PREFIX, RETAINED_FROM_PREFIX, RETIRE_CHARGED_PREFIX, RETIRE_CUT_PREFIX,
        RETIRE_LOG_PREFIX, RETIRE_WILL_PREFIX,
    };
    [
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
        Response::Error { code, msg } => (format!("error {code} {:?}", class(msg)), None),
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
                "hello generation={:?} served={:?} lineage={:?} requests={:?} retention={:?}",
                log.as_ref().map(|l| l.generation),
                ext.durability,
                ext.lineage,
                ext.requests,
                ext.retention
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
    }
}

/// THE DIFFERENTIAL: `steps` on both arms at `tier`, the model with `mutant`.
fn run(tier: Tier, mutant: ArmMutant, steps: &[Step]) -> Verdict {
    let table = table(tier);
    let mut real = Real::new(tier);
    let mut model = Model::new(tier, mutant);
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
                answer(
                    &mut real,
                    &mut model,
                    *c,
                    std::slice::from_ref(req),
                    &table,
                    &mut laws,
                    tier,
                )
                .map_err(|e| at(&e))?;
                answers += 1;
            }
            Step::Pipeline(c, reqs) => {
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
            Step::End(c) => {
                real.end(*c);
                model.end(*c);
            }
            Step::Stop(how) => {
                assert_ne!(*how, Stop::PowerLoss, "a power loss is a Reboot step");
                real.stop(*how);
                model.stop(*how);
            }
            Step::Start => {
                real.open();
                model.core.broker.start();
            }
            Step::Reboot => {
                real.reboot();
                model.reboot();
            }
        }
    }
    assert!(answers > 0, "a script that compared nothing");
    Ok(())
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
            let (rp, rd) = project(tier, &r);
            let (mp, md) = project(tier, &m);
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
            if let (Request::Will { producer_id, .. }, Response::Mark { .. }) = (req, &r) {
                if let Some(conn) = real.conns.get_mut(&c) {
                    conn.will = Some(*producer_id);
                }
                real.will_producers.insert(*producer_id);
            }
            if ends(req, &r) {
                break;
            }
        }
    }
    Ok(())
}

/// THE DOOR'S JOURNAL of a world, as a script: each connection, request,
/// delivery and end as it happened, a world producer's writes on a connection
/// of their own, and the restarts.
fn replay(journal: &[Event]) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut world_dialled = false;
    for e in journal {
        match e {
            Event::Dial(c) => steps.push(Step::Dial(*c)),
            Event::Request(c, req) => steps.push(Step::Send(*c, req.clone())),
            Event::Deliver(c) => steps.push(Step::Deliver(*c)),
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
                steps.push(Step::Stop(*how));
                world_dialled = false;
            }
            Event::Start => steps.push(Step::Start),
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

    /// PUBLISH: a resend, pipelined publishes, the reserved and malformed subjects, the ack half.
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

    /// RESTARTS: a clean close, an unclean stop in the same boot, and a reopen in a boot the open cannot name.
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
        s.extend([Step::Reboot, Step::Dial(6), Step::Send(6, Request::Hello)]);
        s.extend(look(6));
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
        ]);
        s
    }

    /// `FETCH`'S SCAN BOUND.
    pub(crate) fn scan_bound() -> Vec<Step> {
        let n = FETCH_SCAN_MAX as u64 + 5;
        let lane = super::scripts::LANE;
        let pres = super::scripts::PRES_A;
        let mut s = vec![Step::Dial(1), Step::Send(1, Request::Hello)];
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

/// THE WORLDS whose journals are replayed: one per tier, and each with the
/// mirror off and on ([`super::world::Opts::seed`]'s alternation), on one
/// device that never loses power, a broker that answers every ack at once,
/// and a disk that never fails: what a restart may lose and when an ack is
/// read are the law check's and the held-ack worlds'.
fn journalled(seed: u64, tier: Tier) -> super::world::Outcome {
    let opts = super::world::Opts {
        tier: Some(tier),
        power_cuts: false,
        bus_losses: false,
        hold_acks: false,
        write_errors: false,
        journal: true,
        ..super::world::Opts::seed(seed)
    };
    super::world::run(&opts)
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
        for (name, script) in [
            ("publishing", scripts::publishing()),
            ("binary payload", scripts::binary_payload()),
            ("retiring", scripts::retiring()),
            ("wills", scripts::wills()),
            ("groups", scripts::groups()),
            ("restarts", scripts::restarts()),
        ] {
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

/// **THE MODEL HOLDS ASTREAM'S SUBJECT BUDGET AND FETCH SCAN BOUND**, on a
/// log long enough to reach each (Relaxed: the bounds are the same on every
/// tier, and a Strict flush per batch would only cost time).
#[test]
fn the_model_holds_astreams_subject_budget_and_fetch_scan_bound() {
    let mut failed = Vec::new();
    for (name, script) in [
        ("budgets", long::budgets()),
        ("scan bound", long::scan_bound()),
    ] {
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
/// journal of seeded worlds, replayed on both arms.
#[test]
fn what_real_bridges_sent_astream_answers_as_the_model_did() {
    let mut failed = Vec::new();
    let mut requests = 0usize;
    for (seed, tier) in [
        (1, Tier::Strict),
        (2, Tier::Ordered),
        (3, Tier::Relaxed),
        (4, Tier::Ordered),
        (6, Tier::Relaxed),
        (7, Tier::Strict),
    ] {
        let out = journalled(seed, tier);
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
            continue;
        }
        requests += steps.iter().filter(|s| matches!(s, Step::Send(..))).count();
        if let Err(e) = run(tier, ArmMutant::None, &steps) {
            failed.push(format!("seed {seed}: {e}"));
        }
    }
    eprintln!("replayed {requests} requests real bridges sent");
    assert!(
        failed.is_empty(),
        "the model diverged:\n{}",
        failed.join("\n")
    );
}

/// **NEGATIVE CONTROL: A MODEL THAT FORGETS ITS DEDUP OR ITS WILL FENCE, OR
/// CHANGES A BINARY PAYLOAD, IS CAUGHT** by the scripts that hold it
/// ([`ArmMutant::FreshDedup`], [`ArmMutant::NoWillFence`],
/// [`ArmMutant::AlterPayload`]), and by the journal of a world.
#[test]
fn a_model_that_forgets_its_dedup_or_its_will_fence_is_caught() {
    for (mutant, script, what) in [
        (ArmMutant::FreshDedup, scripts::publishing(), "a resend"),
        (ArmMutant::NoWillFence, scripts::wills(), "a fenced will"),
        (
            ArmMutant::AlterPayload,
            scripts::binary_payload(),
            "a changed binary payload",
        ),
    ] {
        let caught = run(Tier::Relaxed, mutant, &script);
        assert!(
            caught.is_err(),
            "CONTROL {mutant:?}: {what} was answered as astream answers it"
        );
        eprintln!("CONTROL {mutant:?} caught: {}", caught.unwrap_err());
    }
    let out = journalled(3, Tier::Relaxed);
    let caught = run(Tier::Relaxed, ArmMutant::FreshDedup, &replay(&out.journal));
    assert!(
        caught.is_err(),
        "CONTROL FreshDedup: no resend in a world's journal"
    );
}
