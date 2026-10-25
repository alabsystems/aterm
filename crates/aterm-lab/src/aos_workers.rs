// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aos-search --threads` and `--workers` (design §11.4–§11.7): the thread [`Spread`], the
//! coordinator shell around the pure [`Coordinator`] (spawning, one reader per worker into one
//! bounded channel, the clock, kills and reaping) and the hidden worker verb `aos-worker`. Every
//! thread, process and clock of the protocol lives here; the core reads none.

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Write};
use std::ops::ControlFlow;
use std::process::{ChildStderr, ChildStdin};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use astream_wire::frame::{
    FrameError, HEADER_SIZE, decode_frame_header_bytes, encode_frame_header,
};
use astream_wire::hash::crc32_ieee;
use aterm_aos::Digest;
use aterm_effects::aos_momentum::dist::{
    self, Action, Binary, Claim, Coordinator, Event, ExecIdentity, FrameFault, JobEnd, MAX_FRAME,
    Msg, RawClaim, Refusal, Sender, Trip, job_id, run_job,
};
use aterm_effects::aos_momentum::lease::{
    Counts, LIMITS_V1, Limits, Nanos, Outcome, Token, WorkerId, WorkerState,
};
use aterm_effects::aos_momentum::m4::{Evaluator, Job, Spread};
use aterm_effects::aos_momentum::plan::{PLAN_LEN, SchedulePlan};
use aterm_uds::spawnfd::{Child, Rusage, Stdio};

use crate::lab_build::LAB_BUILD;
use crate::stop;

/// The hidden pre-dispatch verb a worker runs as (beside `slot-hold`).
pub const WORKER_VERB: &str = "aos-worker";

/// The protocol version a worker is spawned with: `aos-worker --proto 1`.
pub const PROTO: &str = "1";

/// The domain of the digest of a binary's own file (the identity's `executable`).
pub const EXECUTABLE_DOMAIN: &[u8] = b"aterm-aos-m5b-executable-v1\0";

/// The most bytes of a worker's stderr kept.
const STDERR_TAIL: usize = 64 * 1024;

/// How long the coordinator waits on its channel before it looks for an interrupt.
const POLL: Duration = Duration::from_millis(100);

/// How long shutdown waits for a worker whose pipe has closed to become reapable.
const REAP_WAIT: Duration = Duration::from_secs(1);

/// A debug fault's delay between result and lie. Which search phase sees it depends on the
/// workload; this delay is not a causal shutdown barrier.
const POST_LIE_DELAY: Duration = Duration::from_millis(500);

/// The shell's clock is `Instant`'s, measured from one origin, so the core is never handed an
/// earlier `now`; a `ClockBackwards` would be a defect here.
const MONOTONIC: &str = "the shell's clock is monotonic";

fn nanos(d: Duration) -> Nanos {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// A [`Spread`] that times each of its calls (measured, never scored).
pub struct Timed<'a> {
    inner: &'a dyn Spread,
    calls: Mutex<Vec<(usize, Duration)>>,
}

impl<'a> Timed<'a> {
    /// `inner`, timed.
    #[must_use]
    pub fn new(inner: &'a dyn Spread) -> Timed<'a> {
        Timed {
            inner,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Each call's task count and wall time, in call order.
    #[must_use]
    pub fn calls(&self) -> Vec<(usize, Duration)> {
        self.calls.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl Spread for Timed<'_> {
    fn run(&self, n: usize, job: &(dyn Fn(usize) + Sync)) {
        let started = Instant::now();
        self.inner.run(n, job);
        self.calls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((n, started.elapsed()));
    }
}

/// This many threads: the host's parallelism, at least one.
#[must_use]
pub fn available() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

/// Fills `buf` from `r`: the bytes read, short only at the end of the stream.
fn fill(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(buf.get_mut(got..).unwrap_or_default()) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

fn corrupt(detail: impl Into<String>) -> FrameFault {
    FrameFault::Corrupt {
        detail: detail.into(),
    }
}

/// One frame's payload from `r` (design §11.3 step 2: the length is bounded before the payload is
/// read, and the CRC is checked), or `None` at a clean end between frames.
/// # Errors
/// [`FrameFault::Oversize`] or [`FrameFault::Corrupt`].
pub fn read_frame(r: &mut impl Read) -> Result<Option<Vec<u8>>, FrameFault> {
    let mut hdr = [0u8; HEADER_SIZE];
    let got = fill(r, &mut hdr).map_err(|e| corrupt(e.to_string()))?;
    if got == 0 {
        return Ok(None);
    }
    if got < HEADER_SIZE {
        return Err(corrupt("the stream ended inside a frame header"));
    }
    let len = u64::from(u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]));
    if len > u64::try_from(MAX_FRAME).unwrap_or(u64::MAX) {
        return Err(FrameFault::Oversize { len });
    }
    let (len, crc) = decode_frame_header_bytes(&hdr).map_err(|e| corrupt(e.as_str()))?;
    let mut payload = vec![0u8; len];
    if fill(r, &mut payload).map_err(|e| corrupt(e.to_string()))? < len {
        return Err(corrupt("the stream ended inside a frame"));
    }
    if crc32_ieee(&payload) != crc {
        return Err(corrupt(FrameError::ChecksumMismatch.as_str()));
    }
    Ok(Some(payload))
}

/// `payload` as one frame.
/// # Errors
/// A payload over astream's own bound.
pub fn frame_bytes(payload: &[u8]) -> io::Result<Vec<u8>> {
    let hdr = encode_frame_header(payload).map_err(io::Error::other)?;
    let mut out = Vec::with_capacity(HEADER_SIZE + payload.len());
    out.extend_from_slice(&hdr);
    out.extend_from_slice(payload);
    Ok(out)
}

/// The step worker `w`'s frame `payload` takes, as `--frames` says it: `G.S.A.K` for item `K` of
/// search `G`'s slot `S` on attempt `A`, `G.S.A.end` for that lease's result or stop, `hello.W`
/// for worker `W`'s hello, and `-` for a payload that is no worker's message. A run takes each
/// step at most once, so one that trades frames without finishing names none it has not named.
fn frame_step(w: WorkerId, payload: &[u8]) -> String {
    match Msg::decode(payload, Sender::Worker) {
        Ok(Msg::Hello { .. }) => format!("hello.{}", w.0),
        Ok(Msg::Progress { token, k }) => {
            format!("{}.{}.{}.{k}", token.search, token.slot, token.attempt)
        }
        Ok(Msg::Result { token, .. } | Msg::Cancelled { token, .. }) => {
            format!("{}.{}.{}.end", token.search, token.slot, token.attempt)
        }
        Ok(Msg::Lease { .. } | Msg::Cancel { .. }) | Err(_) => "-".to_owned(),
    }
}

/// What this binary contributes to its [`ExecIdentity`]: the toolchain and profile `build.rs`
/// stamped, the digest of its own file, and the faults armed.
/// # Errors
/// The executable could not be found or read.
pub fn binary(faults: &[String]) -> Result<Binary, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot name this executable: {e}"))?;
    let bytes = std::fs::read(&exe).map_err(|e| format!("cannot read {}: {e}", exe.display()))?;
    Ok(Binary {
        toolchain: env!("ATERM_LAB_TOOLCHAIN").to_owned(),
        build_profile: format!(
            "{} effects-debug-assertions={}",
            env!("ATERM_LAB_PROFILE"),
            dist::DEBUG_ASSERTIONS
        ),
        executable: Digest::of(EXECUTABLE_DOMAIN, &[&bytes]),
        faults: faults.to_vec(),
    })
}

/// A test fault's behaviour (design §11.7), on the lease it is keyed by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultKind {
    /// SIGKILL itself after item K.
    Kill(u16),
    /// Stop after item K: no progress, no reply, `Cancel` ignored.
    Hang(u16),
    /// Stop after item K until `Cancel`, then ignore it: the remaining items, a progress frame
    /// each, and the result.
    Slow(u16),
    /// No progress after item 0 until `Cancel`, then the full result.
    Late,
    /// As [`FaultKind::Late`], with a lie in the result.
    LateLie,
    /// The result twice.
    Dup,
    /// The result, then a different one.
    DupLie,
    /// The result, then a different one [`POST_LIE_DELAY`] later.
    PostLie,
    /// An admitted plan claimed refused at item 0.
    LieRefusal,
    /// `T` lowered by one on every row of an admitted claim, `P` or `Z` with it, so the claim keeps
    /// its shape and is (or stays) the claimed best, which step 5 judges in full.
    LieBest,
    /// `T` and `P` raised by 200 on every row of an admitted claim, so on the six-line
    /// plans file another plan is claimed best and this claim is reached only by the dominance
    /// check.
    Inflate,
    /// A frame whose CRC fails, after item 0.
    Garbage,
    /// The progress frame for item 0, repeated.
    Flood,
}

/// A test fault and what it is keyed by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// `name@G.S.A[:K]`: the lease of search serial G, slot S, attempt A, whichever worker
    /// draws it.
    At {
        kind: FaultKind,
        /// The search serial, from 1.
        search: u32,
        slot: u32,
        /// The attempt, from 1.
        attempt: u8,
    },
    /// `foreign@W`: worker W's hello names a suffixed build.
    Foreign {
        /// The worker's spawn index.
        worker: u32,
    },
}

impl Fault {
    /// The fault `spec` names.
    /// # Errors
    /// Why the spec is not one.
    pub fn parse(spec: &str) -> Result<Fault, String> {
        let bad = |why: &str| format!("--fault {spec}: {why} (aos-search --help)");
        let (name, key) = spec
            .split_once('@')
            .ok_or_else(|| bad("expected NAME@KEY"))?;
        if name == "foreign" {
            let worker = key
                .parse()
                .map_err(|_| bad("foreign@W takes a worker index"))?;
            return Ok(Fault::Foreign { worker });
        }
        let (lease, k) = match key.split_once(':') {
            Some((lease, k)) => (
                lease,
                Some(k.parse::<u16>().map_err(|_| bad("K is an item ordinal"))?),
            ),
            None => (key, None),
        };
        let mut parts = lease.split('.');
        let (Some(g), Some(s), Some(a), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(bad("the key is G.S.A: search serial, slot, attempt"));
        };
        let search = g.parse().map_err(|_| bad("G is a search serial"))?;
        let slot = s.parse().map_err(|_| bad("S is a slot"))?;
        let attempt = a.parse().map_err(|_| bad("A is an attempt"))?;
        if search == 0 || attempt == 0 {
            return Err(bad("G and A count from 1"));
        }
        let kind = match (name, k) {
            ("kill", Some(k)) => FaultKind::Kill(k),
            ("hang", Some(k)) => FaultKind::Hang(k),
            ("slow", Some(k)) => FaultKind::Slow(k),
            ("kill" | "hang" | "slow", None) => return Err(bad("this fault takes :K")),
            (_, Some(_)) => return Err(bad("only kill, hang and slow take :K")),
            ("late", None) => FaultKind::Late,
            ("late-lie", None) => FaultKind::LateLie,
            ("dup", None) => FaultKind::Dup,
            ("dup-lie", None) => FaultKind::DupLie,
            ("post-lie", None) => FaultKind::PostLie,
            ("lie-refusal", None) => FaultKind::LieRefusal,
            ("lie-best", None) => FaultKind::LieBest,
            ("inflate", None) => FaultKind::Inflate,
            ("garbage", None) => FaultKind::Garbage,
            ("flood", None) => FaultKind::Flood,
            _ => return Err(bad("unknown fault")),
        };
        Ok(Fault::At {
            kind,
            search,
            slot,
            attempt,
        })
    }

    fn on(&self, t: &Token) -> Option<FaultKind> {
        match *self {
            Fault::At {
                kind,
                search,
                slot,
                attempt,
            } if (search, slot, attempt) == (t.search, t.slot, t.attempt) => Some(kind),
            Fault::At { .. } | Fault::Foreign { .. } => None,
        }
    }
}

/// `aos-worker`'s arguments: `--proto 1`, `--worker N`, and (debug builds) each `--fault SPEC`.
#[derive(Debug)]
struct WorkerArgs {
    index: u32,
    faults: Vec<(String, Fault)>,
}

impl WorkerArgs {
    fn parse(args: &[String]) -> Result<WorkerArgs, String> {
        let mut proto = None;
        let mut index = None;
        let mut faults = Vec::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut value = || {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{a} needs a value"))
            };
            match a.as_str() {
                "--proto" => proto = Some(value()?),
                "--worker" => {
                    index = Some(value()?.parse().map_err(|_| "--worker takes an index")?);
                }
                "--fault" if cfg!(debug_assertions) => {
                    let spec = value()?;
                    let fault = Fault::parse(&spec)?;
                    faults.push((spec, fault));
                }
                other => return Err(format!("unexpected '{other}'")),
            }
        }
        if proto.as_deref() != Some(PROTO) {
            return Err(format!("this build speaks --proto {PROTO} only"));
        }
        Ok(WorkerArgs {
            index: index.ok_or("--worker is required")?,
            faults,
        })
    }
}

/// `aos-worker` (hidden, pre-dispatch): one worker, configured by argv and frames alone.
pub fn worker_main(args: &[String]) -> i32 {
    match worker(args) {
        Ok(()) => 0,
        Err((code, why)) => {
            eprintln!("aos-worker: {why}");
            code
        }
    }
}

/// What the stdin reader forwards to the worker's main thread.
enum Inbound {
    Lease {
        token: Token,
        job: Digest,
        kind: u8,
        plan: [u8; PLAN_LEN],
    },
    Cancel(Token),
}

/// The lifeline and the inbound frames: a thread that forwards leases and cancels and ends the
/// process when stdin closes (the coordinator is gone) or carries a frame no coordinator sends.
fn inbound() -> Receiver<Inbound> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        loop {
            let m = match read_frame(&mut stdin) {
                Ok(None) => std::process::exit(0),
                Ok(Some(p)) => match Msg::decode(&p, Sender::Coordinator) {
                    Ok(Msg::Lease {
                        token,
                        job,
                        kind,
                        plan,
                    }) => Inbound::Lease {
                        token,
                        job,
                        kind,
                        plan,
                    },
                    Ok(Msg::Cancel { token }) => Inbound::Cancel(token),
                    Ok(other) => {
                        eprintln!("aos-worker: the coordinator sent tag {}", other.tag());
                        std::process::exit(3)
                    }
                    Err(f) => {
                        eprintln!("aos-worker: the coordinator's frame: {f}");
                        std::process::exit(3)
                    }
                },
                Err(f) => {
                    eprintln!("aos-worker: the coordinator's frame: {f}");
                    std::process::exit(3)
                }
            };
            if tx.send(m).is_err() {
                return;
            }
        }
    });
    rx
}

/// The worker's side of the pipes.
struct Io {
    out: io::StdoutLock<'static>,
    rx: Receiver<Inbound>,
    /// Leases that arrived while a job ran.
    queued: VecDeque<Inbound>,
    /// Each finished lease and the rows it judged, so a later `Cancel` is acknowledged.
    done: Vec<(Token, u16)>,
    faults: Vec<(String, Fault)>,
}

type Exit = (i32, String);

impl Io {
    fn send(&mut self, m: &Msg) -> Result<(), Exit> {
        let bytes = frame_bytes(&m.encode()).map_err(|e| (3, e.to_string()))?;
        self.raw(&bytes)
    }

    fn raw(&mut self, bytes: &[u8]) -> Result<(), Exit> {
        // A closed stdout means the coordinator is gone: the lifeline, not a fault.
        if self
            .out
            .write_all(bytes)
            .and_then(|()| self.out.flush())
            .is_err()
        {
            std::process::exit(0);
        }
        Ok(())
    }

    fn fired(&self, kind: FaultKind) {
        for (spec, f) in &self.faults {
            if matches!(f, Fault::At { kind: k, .. } if *k == kind) {
                eprintln!("aos-worker fault={spec} fired");
            }
        }
    }

    /// Acknowledges a `Cancel` for a lease this worker finished; one it never held is ignored.
    fn ack(&mut self, t: Token) -> Result<(), Exit> {
        if let Some(&(_, at)) = self.done.iter().find(|(d, _)| *d == t) {
            self.send(&Msg::Cancelled { token: t, at })?;
        }
        Ok(())
    }

    /// Takes what arrived without waiting: whether `token` was cancelled.
    fn poll(&mut self, token: Token) -> Result<bool, Exit> {
        let mut cancelled = false;
        loop {
            match self.rx.try_recv() {
                Ok(Inbound::Cancel(t)) if t == token => cancelled = true,
                Ok(Inbound::Cancel(t)) => self.ack(t)?,
                Ok(lease) => self.queued.push_back(lease),
                Err(TryRecvError::Empty) => return Ok(cancelled),
                Err(TryRecvError::Disconnected) => std::process::exit(0),
            }
        }
    }

    /// Waits for `token`'s `Cancel`.
    fn await_cancel(&mut self, token: Token) -> Result<(), Exit> {
        loop {
            match self.rx.recv() {
                Ok(Inbound::Cancel(t)) if t == token => return Ok(()),
                Ok(Inbound::Cancel(t)) => self.ack(t)?,
                Ok(lease) => self.queued.push_back(lease),
                Err(_) => std::process::exit(0),
            }
        }
    }
}

/// Parks for good: the stdin reader ends the process when the coordinator closes it or is gone.
fn hang() -> ! {
    loop {
        std::thread::park();
    }
}

fn worker(args: &[String]) -> Result<(), Exit> {
    let opts = WorkerArgs::parse(args).map_err(|e| (2, e))?;
    let rx = inbound();
    let started = Instant::now();
    let ev = Evaluator::v1(LAB_BUILD, started)
        .map_err(|e| (3, format!("the pinned policy is refused: {e}")))?;
    let evaluator_ns = nanos(started.elapsed());
    let specs: Vec<String> = opts.faults.iter().map(|(s, _)| s.clone()).collect();
    let mut identity = ExecIdentity::of(&ev, binary(&specs).map_err(|e| (3, e))?);
    let exec = identity.digest();
    let mut io = Io {
        out: io::stdout().lock(),
        rx,
        queued: VecDeque::new(),
        done: Vec::new(),
        faults: opts.faults,
    };
    for (spec, f) in &io.faults {
        if *f == (Fault::Foreign { worker: opts.index }) {
            eprintln!("aos-worker fault={spec} fired");
            identity.build.push_str("-foreign");
        }
    }
    let pid = std::process::id();
    io.send(&Msg::Hello {
        identity: Box::new(identity),
        pid,
        evaluator_ns,
    })?;
    eprintln!("aos-worker pid={pid}");
    loop {
        let next = match io.queued.pop_front() {
            Some(m) => m,
            None => io.rx.recv().map_err(|_| (0, "stdin closed".to_owned()))?,
        };
        match next {
            Inbound::Cancel(t) => io.ack(t)?,
            Inbound::Lease {
                token,
                job,
                kind,
                plan,
            } => run_lease(&ev, &exec, &mut io, token, job, kind, plan)?,
        }
    }
}

/// One lease: the job, its faults, and its reply.
fn run_lease(
    ev: &Evaluator,
    exec: &Digest,
    io: &mut Io,
    token: Token,
    job: Digest,
    kind: u8,
    plan: [u8; PLAN_LEN],
) -> Result<(), Exit> {
    let plan = SchedulePlan::decode(&plan).map_err(|e| (3, format!("the lease's plan: {e}")))?;
    if job != job_id(exec, kind, plan) {
        return Err((
            3,
            "the lease's job id is not this worker's for its plan".to_owned(),
        ));
    }
    let fault = io.faults.iter().find_map(|(_, f)| f.on(&token));
    let stepped_before = ev.stepped();
    let started = Instant::now();
    let mut cancelled = false;
    let mut failed: Option<Exit> = None;
    let end = run_job(
        ev,
        plan,
        &mut |k| match keep(io, token, k, fault, &mut cancelled) {
            Ok(flow) => flow,
            Err(e) => {
                failed = Some(e);
                ControlFlow::Break(())
            }
        },
    );
    if let Some(e) = failed {
        return Err(e);
    }
    let judge_ns = nanos(started.elapsed());
    let stepped = ev.stepped().saturating_sub(stepped_before);
    match end.map_err(|f| (3, format!("the run's shape: {f}")))? {
        JobEnd::Stopped { at } => io.send(&Msg::Cancelled { token, at })?,
        JobEnd::Claimed(claim) => {
            let at = u16::try_from(claim.len()).unwrap_or(u16::MAX);
            let result = |claim: RawClaim| Msg::Result {
                token,
                job,
                claim,
                stepped,
                judge_ns,
            };
            let honest = RawClaim::from(claim);
            match fault {
                Some(f @ (FaultKind::Late | FaultKind::LateLie)) => {
                    if !cancelled {
                        io.await_cancel(token)?;
                    }
                    let sent = if f == FaultKind::LateLie {
                        lie(honest, f)
                    } else {
                        honest
                    };
                    io.send(&result(sent))?;
                }
                Some(FaultKind::Dup) => {
                    io.fired(FaultKind::Dup);
                    io.send(&result(honest.clone()))?;
                    io.send(&result(honest))?;
                }
                Some(f @ (FaultKind::DupLie | FaultKind::PostLie)) => {
                    io.fired(f);
                    io.send(&result(honest.clone()))?;
                    if f == FaultKind::PostLie {
                        let _ = stop::sleep(POST_LIE_DELAY);
                    }
                    io.send(&result(lie(honest, f)))?;
                }
                Some(f @ (FaultKind::LieRefusal | FaultKind::LieBest | FaultKind::Inflate)) => {
                    io.fired(f);
                    io.send(&result(lie(honest, f)))?;
                }
                _ => io.send(&result(honest))?,
            }
            io.done.push((token, at));
        }
    }
    Ok(())
}

/// `run_job`'s `keep` after item `k`: what arrived, the faults, then progress, or a stop on
/// `Cancel`.
fn keep(
    io: &mut Io,
    token: Token,
    k: usize,
    fault: Option<FaultKind>,
    cancelled: &mut bool,
) -> Result<ControlFlow<()>, Exit> {
    let k = u16::try_from(k).unwrap_or(u16::MAX);
    *cancelled |= io.poll(token)?;
    let progress = Msg::Progress { token, k };
    match fault {
        Some(FaultKind::Kill(n)) if k == n => {
            io.fired(FaultKind::Kill(n));
            aterm_uds::process::kill_self_now()
        }
        Some(FaultKind::Hang(n)) if k == n => {
            io.fired(FaultKind::Hang(n));
            hang()
        }
        Some(FaultKind::Slow(n)) if k == n && !*cancelled => {
            io.fired(FaultKind::Slow(n));
            io.await_cancel(token)?;
            *cancelled = true;
        }
        Some(FaultKind::Garbage) if k == 0 => {
            io.fired(FaultKind::Garbage);
            let mut bytes = frame_bytes(&progress.encode()).map_err(|e| (3, e.to_string()))?;
            if let Some(b) = bytes.last_mut() {
                *b ^= 0xff;
            }
            io.raw(&bytes)?;
            hang()
        }
        Some(FaultKind::Flood) if k == 0 => {
            io.fired(FaultKind::Flood);
            for _ in 0..64 {
                io.send(&progress)?;
            }
            hang()
        }
        Some(f @ (FaultKind::Late | FaultKind::LateLie)) if k >= 1 => {
            if k == 1 {
                io.fired(f);
            }
            return Ok(ControlFlow::Continue(()));
        }
        _ => {}
    }
    let ignores_cancel = matches!(
        fault,
        Some(FaultKind::Slow(_) | FaultKind::Late | FaultKind::LateLie)
    );
    if *cancelled && !ignores_cancel {
        return Ok(ControlFlow::Break(()));
    }
    io.send(&progress)?;
    Ok(ControlFlow::Continue(()))
}

/// The honest claim, altered as the fault lies.
#[cfg(debug_assertions)]
fn lie(honest: RawClaim, fault: FaultKind) -> RawClaim {
    let kind = honest.kind();
    match fault {
        FaultKind::LieRefusal => honest.altered(dist::ClaimKind::Refused, |rows| {
            rows.truncate(1);
            if let Some(r) = rows.last_mut() {
                r.stranded_light =
                    aterm_effects::aos_momentum::bundle::RowVerdict::fail("HotHasWake", 0);
            }
        }),
        FaultKind::LieBest => honest.altered(kind, |rows| {
            for r in rows {
                if r.presented >= 1 && r.ticks >= 1 {
                    r.ticks -= 1;
                    r.presented -= 1;
                } else if r.wasted >= 1 && r.ticks >= 1 {
                    r.ticks -= 1;
                    r.wasted -= 1;
                }
            }
        }),
        _ => {
            // Inflate lifts the claim past the next plan's (gap0 claims 6,394 against Shipped's
            // 8,441 over 17 rows); every other lie differs by one.
            let by = if fault == FaultKind::Inflate { 200 } else { 1 };
            honest.altered(kind, |rows| {
                for r in rows {
                    r.ticks = r.ticks.saturating_add(by);
                    r.presented = r.presented.saturating_add(by);
                }
            })
        }
    }
}

/// Release builds arm no fault, so no claim is ever altered.
#[cfg(not(debug_assertions))]
fn lie(honest: RawClaim, _: FaultKind) -> RawClaim {
    honest
}

/// What `--workers` was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolArgs {
    pub workers: usize,
    /// The lease window, when a debug build overrides it.
    pub lease_ms: Option<u64>,
    /// The faults armed on every worker (debug builds).
    pub faults: Vec<String>,
    /// `--frames` (debug builds): each worker frame taken is said on stderr with its step
    /// ([`frame_step`]), so the process tests' hang detector waits for the run's next new step,
    /// not for a whole run a loaded host stretches.
    pub frames: bool,
}

impl PoolArgs {
    /// The ledger's limits: `LIMITS_V1`, with this pool's size and window.
    #[must_use]
    pub fn limits(&self) -> Limits {
        Limits {
            max_workers: self.workers,
            window_ns: self
                .lease_ms
                .map_or(LIMITS_V1.window_ns, |ms| ms.saturating_mul(1_000_000)),
            ..LIMITS_V1
        }
    }
}

/// A worker's stderr: the last [`STDERR_TAIL`] bytes, and the faults it said fired.
#[derive(Debug, Default)]
struct Tail {
    bytes: VecDeque<u8>,
    line: Vec<u8>,
    fired: BTreeMap<String, u64>,
}

impl Tail {
    fn take(&mut self, chunk: &[u8]) {
        for &b in chunk {
            if self.bytes.len() == STDERR_TAIL {
                self.bytes.pop_front();
            }
            self.bytes.push_back(b);
            if b == b'\n' {
                let line = String::from_utf8_lossy(&self.line);
                if let Some(spec) = line
                    .strip_prefix("aos-worker fault=")
                    .and_then(|s| s.strip_suffix(" fired"))
                {
                    *self.fired.entry(spec.to_owned()).or_insert(0) += 1;
                }
                self.line.clear();
            } else if self.line.len() < 512 {
                self.line.push(b);
            }
        }
    }
}

/// One spawned worker process.
struct Proc {
    child: Child,
    stdin: Option<ChildStdin>,
    pid: u32,
    spawned_at: Nanos,
    killed: bool,
    eof: bool,
    usage: Option<Rusage>,
    tail: Arc<Mutex<Tail>>,
}

/// The coordinator's measured transport figures (never scored).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Wire {
    /// Frames read from workers.
    pub frames_in: u64,
    /// Their bytes, headers included.
    pub bytes_in: u64,
    /// Frames written to workers.
    pub frames_out: u64,
    /// Their bytes, headers included.
    pub bytes_out: u64,
    /// The most events queued at once in the shared channel.
    pub queue_max: usize,
    /// Time inside the core (decode, ledger, fold of a frame), in nanoseconds.
    pub core_ns: u64,
}

/// What a pool did, once it is shut down.
#[derive(Clone, Debug, Default)]
pub struct PoolReport {
    pub spawned: usize,
    pub reaped: usize,
    /// Workers the coordinator killed.
    pub killed: usize,
    pub counts: Counts,
    /// Each worker's peak RSS in bytes, where it was reaped with accounting.
    pub peaks: Vec<u64>,
    /// User plus system CPU of the reaped workers.
    pub cpu: Duration,
    /// Each armed fault that a worker said fired, and how often.
    pub fired: BTreeMap<String, u64>,
    /// The foreign workers and the first field each differs in.
    pub foreign: Vec<(u32, &'static str)>,
    /// Spawn to the last hello, when every worker said hello.
    pub pool_ns: Option<Nanos>,
    pub wire: Wire,
    /// Every accepted job's round trip.
    pub trips: Vec<Trip>,
    /// Why the command's distributed work is void, if it is.
    pub refusal: Option<Refusal>,
    /// The signal that interrupted the command, if one did.
    pub interrupted: Option<i32>,
    /// The tail of each worker's stderr whose process ended with a failing status.
    pub stderr: Vec<(u32, String)>,
}

/// The coordinator shell: the workers, their readers, the channel and the clock.
pub struct Pool {
    args: PoolArgs,
    origin: Instant,
    core: Option<Coordinator>,
    procs: Vec<Proc>,
    rx: Receiver<(WorkerId, Event)>,
    queued: Arc<AtomicUsize>,
    wire: Wire,
    pool_ns: Option<Nanos>,
    interrupted: Option<i32>,
    closing: bool,
    report: Option<PoolReport>,
}

fn read_worker(
    w: WorkerId,
    mut out: impl Read,
    tx: SyncSender<(WorkerId, Event)>,
    queued: Arc<AtomicUsize>,
) {
    loop {
        let ev = match read_frame(&mut out) {
            Ok(Some(p)) => Event::Frame(p),
            Ok(None) => Event::Eof,
            Err(f) => Event::Broken(f),
        };
        let last = !matches!(ev, Event::Frame(_));
        queued.fetch_add(1, Ordering::SeqCst);
        if tx.send((w, ev)).is_err() || last {
            return;
        }
    }
}

fn drain_stderr(mut err: ChildStderr, tail: Arc<Mutex<Tail>>) {
    let mut buf = [0u8; 4096];
    loop {
        match err.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => tail
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take(buf.get(..n).unwrap_or_default()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

impl Pool {
    /// Spawns `args.workers` workers of this executable (design §11.6), before the coordinator
    /// builds its own evaluator, with `origin` as the clock's zero.
    /// # Errors
    /// The executable cannot be named, or a spawn failed (the ones spawned are killed and reaped
    /// when the pool drops).
    pub fn spawn(args: &PoolArgs, origin: Instant) -> io::Result<Pool> {
        let exe = std::env::current_exe()?;
        let n = args.workers;
        let (tx, rx) = mpsc::sync_channel(n.saturating_mul(2).max(1));
        let queued = Arc::new(AtomicUsize::new(0));
        let mut pool = Pool {
            args: args.clone(),
            origin,
            core: None,
            procs: Vec::with_capacity(n),
            rx,
            queued: Arc::clone(&queued),
            wire: Wire::default(),
            pool_ns: None,
            interrupted: None,
            closing: false,
            report: None,
        };
        for i in 0..n {
            let mut cmd = aterm_uds::spawnfd::command(&exe);
            cmd.args([WORKER_VERB, "--proto", PROTO, "--worker", &i.to_string()]);
            for f in &args.faults {
                cmd.args(["--fault", f]);
            }
            cmd.env_clear()
                .current_dir("/")
                .process_group(0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = cmd.spawn()?;
            let spawned_at = nanos(origin.elapsed());
            let w = WorkerId(u32::try_from(i).unwrap_or(u32::MAX));
            let tail = Arc::new(Mutex::new(Tail::default()));
            if let Some(out) = child.stdout.take() {
                let (tx, queued) = (tx.clone(), Arc::clone(&queued));
                std::thread::spawn(move || read_worker(w, out, tx, queued));
            }
            if let Some(err) = child.stderr.take() {
                let tail = Arc::clone(&tail);
                std::thread::spawn(move || drain_stderr(err, tail));
            }
            pool.procs.push(Proc {
                stdin: child.stdin.take(),
                pid: child.id(),
                child,
                spawned_at,
                killed: false,
                eof: false,
                usage: None,
                tail,
            });
        }
        Ok(pool)
    }

    fn now(&self) -> Nanos {
        nanos(self.origin.elapsed())
    }

    /// Admits the spawned workers to a coordinator of `ev` in this binary: each joins at the time
    /// it was spawned, so its hello is timed from its spawn.
    /// # Errors
    /// This executable cannot be read, or the clock ran backwards (a defect).
    pub fn admit(&mut self, ev: &Evaluator) -> Result<(), String> {
        let identity = ExecIdentity::of(ev, binary(&self.args.faults)?);
        let mut core = Coordinator::new(identity, self.args.limits(), ev.policy().items());
        for (i, p) in self.procs.iter().enumerate() {
            let w = core
                .join(p.spawned_at)
                .map_err(|e| format!("the coordinator's clock: {e:?}"))?
                .ok_or("more workers than the ledger admits")?;
            if usize::try_from(w.0).ok() != Some(i) {
                return Err("worker ids out of spawn order".to_owned());
            }
        }
        self.core = Some(core);
        Ok(())
    }

    /// Runs one search's jobs to completion (every job claimed, no drain open): the accepted
    /// claims by slot, or why the command's distributed work is void.
    /// # Errors
    /// The [`Refusal`] that voided it.
    pub fn run(&mut self, jobs: &[Job]) -> Result<Vec<(u32, Claim)>, Refusal> {
        let now = self.now();
        let Some(core) = self.core.as_mut() else {
            return Err(Refusal::Interrupted);
        };
        let acts = core.begin(jobs, now).expect(MONOTONIC);
        self.act(acts);
        self.pump(|c| matches!(c.ledger().outcome(), Outcome::Complete | Outcome::Void(_)));
        let Some(core) = self.core.as_mut() else {
            return Err(Refusal::Interrupted);
        };
        match core.ledger().outcome() {
            Outcome::Complete => Ok(core.take_claims()),
            _ => Err(core.refusal().cloned().unwrap_or(Refusal::Interrupted)),
        }
    }

    /// The current search's serial (0 before the first).
    #[must_use]
    pub fn serial(&self) -> u32 {
        self.core.as_ref().map_or(0, |c| c.ledger().search())
    }

    /// Voids the command's distributed work for `why`, found after a search (step 5).
    pub fn void(&mut self, why: Refusal) {
        let now = self.now();
        if let Some(core) = self.core.as_mut() {
            let acts = core.void(why, now).expect(MONOTONIC);
            self.act(acts);
        }
    }

    /// Voids the command for an interrupt that has arrived and is not yet noted; whether one had.
    /// An interrupt between pumps (during step 5, the held-out read or a bundle's write) is seen
    /// at the next one, or at shutdown.
    fn note_interrupt(&mut self) -> bool {
        if self.interrupted.is_none()
            && let Err(stop::Stopped(sig)) = stop::check()
        {
            self.interrupted = Some(sig);
            self.void(Refusal::Interrupted);
            return true;
        }
        false
    }

    /// Feeds events and ticks until `done` holds of the core, voiding on an interrupt first.
    /// Each iteration takes at most the events queued when it woke, then ticks.
    fn pump(&mut self, done: impl Fn(&Coordinator) -> bool) {
        loop {
            if self.note_interrupt() {
                continue;
            }
            let Some(core) = self.core.as_ref() else {
                return;
            };
            if done(core) {
                return;
            }
            let now = self.now();
            let wait = core
                .ledger()
                .next_deadline()
                .map_or(POLL, |d| Duration::from_nanos(d.saturating_sub(now)))
                .min(POLL);
            match self.rx.recv_timeout(wait) {
                Ok((w, ev)) => {
                    let more = self.queued.load(Ordering::SeqCst).saturating_sub(1);
                    self.feed(w, ev);
                    for _ in 0..more {
                        match self.rx.try_recv() {
                            Ok((w, ev)) => self.feed(w, ev),
                            Err(_) => break,
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    // Every reader has ended; only the ledger's deadlines can move it now.
                    let _ = stop::sleep(wait);
                }
            }
            let now = self.now();
            if let Some(core) = self.core.as_mut() {
                let acts = core.tick(now).expect(MONOTONIC);
                self.act(acts);
            }
        }
    }

    fn feed(&mut self, w: WorkerId, ev: Event) {
        let queued = self.queued.fetch_sub(1, Ordering::SeqCst);
        self.wire.queue_max = self.wire.queue_max.max(queued);
        let i = usize::try_from(w.0).unwrap_or(usize::MAX);
        match &ev {
            Event::Frame(p) => {
                self.wire.frames_in += 1;
                self.wire.bytes_in = self
                    .wire
                    .bytes_in
                    .saturating_add(u64::try_from(p.len() + HEADER_SIZE).unwrap_or(u64::MAX));
                if self.args.frames {
                    eprintln!("aos-workers frame from={} step={}", w.0, frame_step(w, p));
                }
            }
            Event::Eof | Event::Broken(_) => {
                if let Some(p) = self.procs.get_mut(i) {
                    p.eof = true;
                }
                // After the pipes close, a reader's end is the lifeline at work, not a fault.
                if self.closing {
                    return;
                }
            }
        }
        let now = self.now();
        let Some(core) = self.core.as_mut() else {
            return;
        };
        let was_joining = matches!(core.ledger().worker(w), Some(WorkerState::Joining { .. }));
        let started = Instant::now();
        let acts = core.on(w, ev, now).expect(MONOTONIC);
        self.wire.core_ns = self.wire.core_ns.saturating_add(nanos(started.elapsed()));
        // An accepted hello: the worker left `Joining` for `Idle` (and may already hold a lease).
        let admitted = !matches!(
            core.ledger().worker(w),
            Some(WorkerState::Joining { .. } | WorkerState::Gone(_)) | None
        );
        if was_joining
            && admitted
            && let Some(p) = self.procs.get(i)
        {
            eprintln!("aos-workers worker={} pid={}", w.0, p.pid);
        }
        if self.pool_ns.is_none()
            && !core
                .ledger()
                .workers()
                .iter()
                .any(|s| matches!(s, WorkerState::Joining { .. }))
        {
            self.pool_ns = Some(now);
        }
        self.act(acts);
    }

    fn act(&mut self, acts: Vec<Action>) {
        for a in acts {
            match a {
                Action::Send(w, payload) => {
                    let Ok(bytes) = frame_bytes(&payload) else {
                        continue;
                    };
                    let i = usize::try_from(w.0).unwrap_or(usize::MAX);
                    if let Some(stdin) = self.procs.get_mut(i).and_then(|p| p.stdin.as_mut()) {
                        self.wire.frames_out += 1;
                        self.wire.bytes_out = self
                            .wire
                            .bytes_out
                            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                        // A worker that died is seen by its reader's end of stream.
                        let _ = stdin.write_all(&bytes).and_then(|()| stdin.flush());
                    }
                }
                Action::Kill(w) => self.kill(usize::try_from(w.0).unwrap_or(usize::MAX)),
            }
        }
    }

    fn kill(&mut self, i: usize) {
        if let Some(p) = self.procs.get_mut(i)
            && !p.killed
            && p.usage.is_none()
        {
            let _ = p.child.kill();
            p.killed = true;
            drop(p.stdin.take());
        }
    }

    /// Ends the pool: lets every drain end (by an ack, a stale result or the ledger's drain
    /// timeout), closes every worker's stdin, reads each to its end within the grace, kills the
    /// rest and reaps them all.
    pub fn shutdown(&mut self) -> PoolReport {
        if let Some(r) = &self.report {
            return r.clone();
        }
        self.pump(|c| c.ledger().quiescent());
        self.closing = true;
        for p in &mut self.procs {
            drop(p.stdin.take());
        }
        let grace = Duration::from_nanos(self.args.limits().grace_ns);
        let until = Instant::now() + grace;
        while self.procs.iter().any(|p| !p.eof && !p.killed) {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match self.rx.recv_timeout(left.min(POLL)) {
                Ok((w, ev)) => self.feed(w, ev),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        self.note_interrupt();
        // A worker whose pipe has closed is exiting; it becomes reapable a moment after its fds
        // close, so it is waited for, briefly, rather than killed and counted `killed`.
        let reap_by = Instant::now() + REAP_WAIT;
        let mut report = PoolReport {
            spawned: self.procs.len(),
            ..PoolReport::default()
        };
        for i in 0..self.procs.len() {
            let Some(p) = self.procs.get_mut(i) else {
                continue;
            };
            if p.usage.is_none() {
                let mut exited = matches!(p.child.try_wait_with_rusage(), Ok(Some(_)));
                while !exited && p.eof && !p.killed && Instant::now() < reap_by {
                    std::thread::sleep(Duration::from_millis(5));
                    exited = matches!(p.child.try_wait_with_rusage(), Ok(Some(_)));
                }
                if !exited {
                    let _ = p.child.kill();
                    p.killed = true;
                }
            }
            if let Ok(u) = p.child.wait_with_rusage() {
                p.usage = Some(u);
            }
            if p.killed {
                report.killed += 1;
            }
            if let Some(u) = p.usage {
                report.reaped += 1;
                report.peaks.push(u.maxrss);
                report.cpu = report.cpu.saturating_add(u.cpu);
                if !u.status.success() && !p.killed {
                    let tail = p.tail.lock().unwrap_or_else(|e| e.into_inner());
                    let text: Vec<u8> = tail.bytes.iter().copied().collect();
                    report.stderr.push((
                        u32::try_from(i).unwrap_or(u32::MAX),
                        String::from_utf8_lossy(&text).into_owned(),
                    ));
                }
            }
            let tail = p.tail.lock().unwrap_or_else(|e| e.into_inner());
            for (spec, n) in &tail.fired {
                *report.fired.entry(spec.clone()).or_insert(0) += n;
            }
        }
        if let Some(core) = &self.core {
            report.counts = core.ledger().counts();
            report.foreign = core.foreign().iter().map(|(w, f)| (w.0, *f)).collect();
            report.trips = core.trips().to_vec();
            report.refusal = core.refusal().cloned();
        }
        report.pool_ns = self.pool_ns;
        report.wire = self.wire;
        report.interrupted = self.interrupted;
        self.report = Some(report.clone());
        report
    }
}

impl Drop for Pool {
    /// The guard: dropping a `Child` does not kill it, so every exit path kills and reaps.
    fn drop(&mut self) {
        if self.report.is_none() {
            for p in &mut self.procs {
                drop(p.stdin.take());
                if p.usage.is_none() {
                    let _ = p.child.kill();
                    let _ = p.child.wait_with_rusage();
                }
            }
        }
    }
}

/// `xs`' p50, p95 and p99 by nearest rank, in milliseconds.
#[must_use]
pub fn percentiles(xs: &[Nanos]) -> Option<(f64, f64, f64)> {
    let mut v = xs.to_vec();
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let at = |p: usize| {
        let rank = (p * v.len()).div_ceil(100).max(1);
        let ns = v.get(rank - 1).copied().unwrap_or(0);
        ns as f64 / 1e6
    };
    Some((at(50), at(95), at(99)))
}

#[cfg(test)]
#[path = "aos_workers_tests.rs"]
mod tests;
