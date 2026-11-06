// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SHIPPED BROKER'S MEMORY OVER A SEEDED LOG (F09). C27 asks an always-on broker to stay
//! under 50 MB, and D10e met it in the bench's process, which serves astream's broker beside its
//! own clients and tables, so the number was never the broker's alone. With its broker in a
//! process of its own (`run/outside.rs`) and a `backlog=`, the attach world's log is seeded
//! here before that broker starts: `backlog` filler records for each attach population, of the
//! backlog's kind ([`backlog::fill_subject`], on a node no world has), written through astream's
//! broker opened as `aterm link broker` opens it (`aterm_link::durability::open_escalating`,
//! sealed under the root's key, retained by aterm's rules, with no rate limit, since the seed is
//! history and not traffic), and closed. The shipped broker then opens that log, and the bench
//! reads the broker's process by its pid (`aterm_sysprobe::procs::processes`): its physical
//! footprint (`ri_phys_footprint`), its resident pages (`ri_resident_size`) and the largest
//! footprint it has had (`ri_lifetime_max_phys_footprint`), sampled through its open and for
//! [`SETTLE`] after it, a sweep of the process table and [`SAMPLE_PAUSE`] a reading (the report
//! gives the gaps the readings lie apart, [`gaps`]), then read where the world gets to. Two more
//! opens of this build's broker say where a peak comes from: one of an empty log (the process
//! alone), one of the same records seeded plaintext. Last, this process times reads of records
//! the residency budget leaves cold, sealed and plaintext ([`ColdReader`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aterm_json::{Value, json};

use super::{backlog, mib};
use crate::harness::{self, FLEET, SECRET};
pub use crate::spec::COLD_RECORDS;
use crate::stats;

/// The pause after each of the open's sampler's readings of the broker's process. A reading is
/// a sweep of the whole process table, which is what `aterm_sysprobe` reads a pid by, so the
/// readings lie the sweep's time and this pause apart: what the report gives is their gaps as
/// measured ([`gaps`]), not this.
pub const SAMPLE_PAUSE: Duration = Duration::from_millis(10);

/// How long the sampler goes on after the broker's `Hello` is answered: what the open leaves
/// held once it has handed back what it held only while it opened.
pub const SETTLE: Duration = Duration::from_secs(2);

/// The pause after each of the world's sampler's readings of the broker's process, from the
/// world's boot to its last read: the most it held between two reads, which its lifetime peak
/// cannot say once the open's is above it. Longer than the open's, so its sweeps take less of the
/// machine the world's stamps are timed on; as there, the readings lie a sweep and this pause
/// apart, and the report gives their gaps ([`gaps`]).
pub const WORLD_SAMPLE_PAUSE: Duration = Duration::from_millis(50);

/// How long past the last restart's reading the world waits before its last read: one
/// retention interval and [`TICK_SLACK`]. The broker's retention tick waits the interval from
/// the end of its last tick (astream's `ticker_loop`), so one starts inside the wait; the
/// shipped broker counts its ticks nowhere a client reads (`BrokerStats::retention_ticks` is
/// in-process), so the tick is the interval's, not a count's.
pub const TICK_WAIT: Duration = astream_broker::RETENTION_INTERVAL.saturating_add(TICK_SLACK);

/// The wait past one retention interval ([`TICK_WAIT`]): a tick that cuts nothing plans in
/// milliseconds, so this is a broker's thread at priority 4 starting late, not the tick's work.
pub const TICK_SLACK: Duration = Duration::from_secs(10);

/// The node the seeded records name: `n-<16 hex>`, as every world's node is, so each record is
/// [`crate::spec::FILL_RECORD_BYTES`]; no world's node is this one.
pub const SEED_NODE: &str = "n-0000000000000f09";

/// The filler producer the seed publishes under.
const SEED_PRODUCER: u64 = backlog::FILL_PRODUCER | 0x0f09_0000;

/// Cold-read passes over each log, the two logs' passes alternating.
pub const COLD_PASSES: usize = 6;

/// The most records one fetch of a cold-read pass asks for.
pub const COLD_PAGE: u32 = 4096;

/// One reading of the broker's process.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Reading {
    /// When it was read, ms after the broker was started.
    pub at_ms: f64,
    /// `ri_phys_footprint`, bytes.
    pub footprint: Option<u64>,
    /// `ri_resident_size`, bytes: its pages in memory, the file-backed pages it maps included.
    pub resident: Option<u64>,
    /// `ri_lifetime_max_phys_footprint`, bytes: the largest footprint since it started.
    pub peak: Option<u64>,
}

impl Reading {
    /// `pid` now, `at_ms` counted from `started`; every count `None` where the table has no such
    /// process or does not say.
    #[must_use]
    pub fn of(pid: u32, started: Instant) -> Self {
        let mine = aterm_sysprobe::procs::processes()
            .and_then(|sweep| sweep.procs.into_iter().find(|p| p.pid == pid));
        let at = mine
            .as_ref()
            .and_then(|p| p.read_at)
            .unwrap_or_else(Instant::now);
        Self {
            at_ms: at.saturating_duration_since(started).as_secs_f64() * 1e3,
            footprint: mine.as_ref().and_then(|p| p.footprint),
            resident: mine.as_ref().and_then(|p| p.resident),
            peak: mine.as_ref().and_then(|p| p.peak_footprint),
        }
    }

    /// As the report's JSON carries it.
    #[must_use]
    pub fn json(self) -> Value {
        json!({
            "at_ms": stats::round3(self.at_ms),
            "footprint_bytes": self.footprint,
            "resident_bytes": self.resident,
            "peak_footprint_bytes": self.peak,
        })
    }
}

/// Reads the broker's process over and over, pausing after each reading, on a thread of its own.
pub struct Sampler {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Vec<Reading>>>,
}

impl Sampler {
    /// Start sampling `pid` with [`SAMPLE_PAUSE`] after each reading, its times counted from
    /// `started`.
    #[must_use]
    pub fn start(pid: u32, started: Instant) -> Self {
        Self::pausing(pid, started, SAMPLE_PAUSE)
    }

    /// Start sampling `pid` with `pause` after each reading (a sweep of the process table), its
    /// times counted from `started`.
    #[must_use]
    pub fn pausing(pid: u32, started: Instant, pause: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut samples = Vec::new();
            while !stopped.load(Ordering::Relaxed) {
                samples.push(Reading::of(pid, started));
                std::thread::sleep(pause);
            }
            samples
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }

    /// Stop the worker and return every reading it took.
    ///
    /// # Panics
    /// The worker handle is missing or its thread panicked.
    #[must_use]
    pub fn finish(mut self) -> Vec<Reading> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread
            .take()
            .expect("the sampler owns its worker")
            .join()
            .expect("the broker's sampler")
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The gaps between `samples`' consecutive readings, ms: how far apart a sampler's readings
/// lay, a sweep's time and its pause, so how long a transient its readings missed could last.
/// `None` for fewer than two readings.
#[must_use]
pub fn gaps(samples: &[Reading]) -> Option<stats::Summary> {
    let gaps: Vec<f64> = samples
        .windows(2)
        .map(|w| w[1].at_ms - w[0].at_ms)
        .collect();
    stats::summarize(&gaps)
}

/// [`gaps`] as the report's JSON carries them: the median and the largest, ms.
#[must_use]
pub fn gaps_json(samples: &[Reading]) -> Value {
    gaps(samples).map_or(
        Value::Null,
        |g| json!({ "p50": stats::round3(g.p50), "max": stats::round3(g.max) }),
    )
}

/// [`gaps`] as the report's text gives them: `<p50> / <max>`, ms, `–` for none.
#[must_use]
pub fn gaps_md(samples: &[Reading]) -> String {
    gaps(samples).map_or_else(
        || "–".to_string(),
        |g| format!("{:.1} / {:.1}", g.p50, g.max),
    )
}

/// Of `samples`, those taken after `after_ms` and up to `until_ms`: how many, and the largest
/// footprint among them with when it was read.
#[must_use]
pub fn most_between(
    samples: &[Reading],
    after_ms: f64,
    until_ms: f64,
) -> (usize, Option<(f64, u64)>) {
    let these: Vec<&Reading> = samples
        .iter()
        .filter(|r| r.at_ms > after_ms && r.at_ms <= until_ms)
        .collect();
    let most = these
        .iter()
        .filter_map(|r| r.footprint.map(|f| (r.at_ms, f)))
        .max_by_key(|&(_, f)| f);
    (these.len(), most)
}

/// One read of the served broker: where the world had got to, its process, and its health
/// snapshot when there was one.
pub type Read = (String, Reading, Option<Health>);

/// What the broker's health snapshot (`<log>.health`) said of its memory when read: the
/// records it keeps decoded and the bytes its budget charges them, of those it retains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Health {
    pub resident_records: Option<u64>,
    pub resident_bytes: Option<u64>,
    pub retained: u64,
    /// How old the snapshot was, ms: the broker rewrites it every 30 s
    /// (`aterm_link::broker_health::HEALTH_EVERY`).
    pub age_ms: u64,
}

impl Health {
    /// The snapshot beside `log` now; `None` where there is none or it does not parse.
    #[must_use]
    pub fn of(log: &Path) -> Option<Self> {
        let read = astream_broker::read_health(log).ok().flatten()?;
        let h = read.health;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        Some(Self {
            resident_records: h.resident.map(|r| r.records),
            resident_bytes: h.resident.map(|r| r.bytes),
            retained: h.retained_records,
            age_ms: now.saturating_sub(h.written_at_ms),
        })
    }

    #[must_use]
    pub fn json(self) -> Value {
        json!({
            "resident_records": self.resident_records,
            "resident_bytes": self.resident_bytes,
            "retained_records": self.retained,
            "age_ms": self.age_ms,
        })
    }
}

/// One open of `aterm link broker`, sampled.
#[derive(Debug, Clone)]
pub struct Open {
    /// When it wrote `listening`, ms after it was started.
    pub listening_ms: f64,
    /// When this process's `Hello` to it was answered, ms after it was started.
    pub hello_ms: f64,
    /// Its process when the `Hello` was answered.
    pub at_hello: Reading,
    /// Its process [`SETTLE`] later.
    pub settled: Reading,
    /// Every reading the sampler took, from its start to [`SETTLE`] after the `Hello`.
    pub samples: Vec<Reading>,
    /// Its health snapshot when the `Hello` was answered.
    pub health: Option<Health>,
}

impl Open {
    /// Sample `pid` from its start (`started`) until it has answered a `Hello` on `sock` and
    /// [`SETTLE`] has passed; `listening` is when it wrote its `listening` line, and `log` the log
    /// it serves.
    ///
    /// # Panics
    /// When the `Hello` is not answered.
    #[must_use]
    pub fn finish(
        sampler: Sampler,
        pid: u32,
        started: Instant,
        listening: Instant,
        sock: &str,
        log: &Path,
    ) -> Self {
        let mut hello = harness::broker_client(sock);
        hello
            .hello()
            .unwrap_or_else(|e| panic!("the broker on {sock} did not answer a Hello: {e}"));
        let hello_ms = started.elapsed().as_secs_f64() * 1e3;
        drop(hello);
        let at_hello = Reading::of(pid, started);
        let health = Health::of(log);
        std::thread::sleep(SETTLE);
        let settled = Reading::of(pid, started);
        let samples = sampler.finish();
        Self {
            listening_ms: listening.saturating_duration_since(started).as_secs_f64() * 1e3,
            hello_ms,
            at_hello,
            settled,
            samples,
            health,
        }
    }

    /// The largest footprint the sampler saw before the broker wrote `listening` (its open), and
    /// when.
    #[must_use]
    pub fn most_in_open(&self) -> Option<(f64, u64)> {
        self.samples
            .iter()
            .filter(|r| r.at_ms <= self.listening_ms)
            .filter_map(|r| r.footprint.map(|f| (r.at_ms, f)))
            .max_by_key(|&(_, f)| f)
    }

    /// The largest footprint the sampler saw at all, and when.
    #[must_use]
    pub fn most_sampled(&self) -> Option<(f64, u64)> {
        self.samples
            .iter()
            .filter_map(|r| r.footprint.map(|f| (r.at_ms, f)))
            .max_by_key(|&(_, f)| f)
    }

    #[must_use]
    pub fn json(&self) -> Value {
        let most = |m: Option<(f64, u64)>| {
            m.map_or(
                Value::Null,
                |(at, f)| json!({ "at_ms": stats::round3(at), "footprint_bytes": f }),
            )
        };
        json!({
            "spawn_to_listening_ms": stats::round3(self.listening_ms),
            "spawn_to_hello_ms": stats::round3(self.hello_ms),
            "at_hello": self.at_hello.json(),
            "settled": self.settled.json(),
            "settle_ms": u64::try_from(SETTLE.as_millis()).unwrap_or(u64::MAX),
            "most_sampled_in_open": most(self.most_in_open()),
            "most_sampled": most(self.most_sampled()),
            "sample_pause_ms": u64::try_from(SAMPLE_PAUSE.as_millis()).unwrap_or(u64::MAX),
            "sample_gap_ms": gaps_json(&self.samples),
            // [at_ms, footprint, resident, peak] per sample.
            "samples": self
                .samples
                .iter()
                .map(|r| json!([stats::round3(r.at_ms), r.footprint, r.resident, r.peak]))
                .collect::<Vec<_>>(),
            "health": self.health.map_or(Value::Null, Health::json),
        })
    }
}

/// What a seed wrote.
#[derive(Debug, Clone, Copy)]
pub struct Seeded {
    pub records: usize,
    pub fill_ms: f64,
    /// The records the seeding broker retained when it closed: the seed's and its rules'.
    pub retained: usize,
    /// The log's bytes once closed.
    pub log_bytes: u64,
}

impl Seeded {
    #[must_use]
    pub fn json(self) -> Value {
        json!({
            "records": self.records,
            "fill_ms": stats::round3(self.fill_ms),
            "retained_records": self.retained,
            "log_bytes": self.log_bytes,
        })
    }
}

/// The filler's subject in a seeded log.
#[must_use]
pub fn subject() -> String {
    backlog::fill_subject(SEED_NODE)
}

/// Write `records` filler records to the log at `log` (sealed under `key`, plaintext without
/// one) through astream's broker opened as `aterm link broker` opens it on `tier`, with aterm's
/// retention rules and no rate limit, and close it.
///
/// # Panics
/// When the log does not open, serve or take every record.
#[must_use]
pub fn seed(
    log: &Path,
    key: Option<[u8; 32]>,
    tier: aterm_link::durability::Durability,
    records: usize,
) -> Seeded {
    let opened = aterm_link::durability::open_escalating(
        log,
        Some(SECRET.to_vec()),
        tier,
        aterm_link::durability::this_boot(),
        Vec::new(),
        key,
    )
    .unwrap_or_else(|e| panic!("open {} to seed it: {e}", log.display()));
    let broker = opened.broker;
    aterm_link::broker_rules::retain(&broker).expect("the default retention rules");
    let sock = beside(log, "seed.sock");
    let mut handle = broker.serve(&sock).expect("serve the seeding broker");
    let mut god = harness::broker_client(&sock);
    let cap = astream_cap::mint(SECRET, &format!("/f/{FLEET}/>")).expect("mint the god cap");
    god.attach(&cap.filter, &cap.tag)
        .expect("attach the god cap");
    let t0 = Instant::now();
    backlog::fill_records(&mut god, SEED_PRODUCER, &subject(), records)
        .unwrap_or_else(|e| panic!("seed {records} records into {}: {e}", log.display()));
    let fill_ms = t0.elapsed().as_secs_f64() * 1e3;
    drop(god);
    let (_, _, retained) = broker.residency();
    handle.shutdown();
    drop(broker);
    let _ = std::fs::remove_file(&sock);
    Seeded {
        records,
        fill_ms,
        retained,
        log_bytes: std::fs::metadata(log).map_or(0, |m| m.len()),
    }
}

/// `name` in `log`'s directory.
fn beside(log: &Path, name: &str) -> PathBuf {
    log.with_file_name(name)
}

/// One seeded log opened in this process at a residency budget of 0, served on a socket of its
/// own, with a reader connected: every record it serves is read back from the file, so each
/// fetch's records are cold reads (`Broker::cold_reads`). No retention rules are set, so no tick
/// walks the log beside the reads.
pub struct ColdReader {
    pub sealed: bool,
    /// The open, ms: every retained record scanned (unsealed, under a key).
    pub open_ms: f64,
    broker: astream_broker::Broker,
    handle: astream_broker::BrokerHandle,
    client: astream_broker::Client,
    subject: String,
    /// Each pass's time, ms.
    pub pass_ms: Vec<f64>,
    /// The records each pass read cold.
    pub cold: Vec<u64>,
}

impl ColdReader {
    /// Open `log` under `key` (plaintext without one) on `tier`, at a budget of 0.
    ///
    /// # Panics
    /// When it does not open or serve.
    #[must_use]
    pub fn open(
        log: &Path,
        key: Option<[u8; 32]>,
        tier: aterm_link::durability::Durability,
    ) -> Self {
        let mut opts = tier.options();
        opts.guard = Some(SECRET.to_vec());
        opts.require_bound_at_rest = key.is_some();
        opts.at_rest_key = key;
        opts.resident_budget = Some(0);
        let t0 = Instant::now();
        let broker =
            astream_broker::Broker::open_opts(log, opts, || Ok(astream_broker::Prepared::Nothing))
                .unwrap_or_else(|e| panic!("open {} at budget 0: {e}", log.display()));
        let open_ms = t0.elapsed().as_secs_f64() * 1e3;
        let sock = beside(log, "cold.sock");
        let handle = broker.serve(&sock).expect("serve the cold reader's broker");
        let mut client = harness::broker_client(&sock);
        let cap = astream_cap::mint(SECRET, &format!("/f/{FLEET}/>")).expect("mint the god cap");
        client
            .attach(&cap.filter, &cap.tag)
            .expect("attach the god cap");
        Self {
            sealed: key.is_some(),
            open_ms,
            broker,
            handle,
            client,
            subject: subject(),
            pass_ms: Vec::new(),
            cold: Vec::new(),
        }
    }

    /// Fetch the first [`COLD_RECORDS`] seeded records, timed, and count the cold reads.
    ///
    /// # Panics
    /// When the log holds fewer (a seed `spec::parse` refuses before the run), or the pass read
    /// fewer of them cold than it fetched: then the budget kept some decoded, and the time is not
    /// the cold reads'.
    pub fn pass(&mut self) {
        let before = self.broker.cold_reads();
        let t0 = Instant::now();
        let mut from = self.broker.base();
        let mut got = 0;
        while got < COLD_RECORDS {
            let want = u32::try_from(COLD_RECORDS - got)
                .unwrap_or(u32::MAX)
                .min(COLD_PAGE);
            let (records, (next, head)) = self
                .client
                .fetch(from, &self.subject, want)
                .unwrap_or_else(|e| panic!("fetch {want} from {from}: {e}"));
            assert!(
                !records.is_empty() || next < head,
                "the log holds {got} seeded records past its base, not {COLD_RECORDS}"
            );
            got += records.len();
            from = next;
        }
        let ms = t0.elapsed().as_secs_f64() * 1e3;
        let cold = self.broker.cold_reads().saturating_sub(before);
        assert!(
            cold >= got as u64,
            "a pass fetched {got} records and read {cold} cold: the budget kept some decoded"
        );
        self.pass_ms.push(ms);
        self.cold.push(cold);
    }

    /// Each pass's nanoseconds a record.
    #[must_use]
    pub fn ns_per_record(&self) -> Vec<f64> {
        self.pass_ms
            .iter()
            .map(|ms| ms * 1e6 / COLD_RECORDS as f64)
            .collect()
    }

    #[must_use]
    pub fn json(&self) -> Value {
        json!({
            "sealed": self.sealed,
            "open_ms": stats::round3(self.open_ms),
            "retained_records": self.broker.residency().2,
            "pass_ms": self.pass_ms.iter().map(|&m| stats::round3(m)).collect::<Vec<_>>(),
            "cold_reads_per_pass": self.cold.clone(),
            "ns_per_record": stats::to_json(&self.ns_per_record()),
        })
    }
}

impl Drop for ColdReader {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

/// [`COLD_PASSES`] passes over each of `readers`, alternating which goes first.
pub fn cold_passes(readers: &mut [ColdReader; 2]) {
    for pass in 0..COLD_PASSES {
        let order = if pass % 2 == 0 { [0, 1] } else { [1, 0] };
        for i in order {
            readers[i].pass();
        }
    }
}

/// `bytes` in MB (10^6, C27's unit) to a tenth, `–` for none.
#[must_use]
pub fn mb(bytes: Option<u64>) -> String {
    bytes.map_or_else(|| "–".to_string(), |b| format!("{:.1}", b as f64 / 1e6))
}

/// The report's table of opens: `opens` are `(what, records, log bytes, open)`.
#[must_use]
pub fn opens_table(opens: &[(&str, usize, u64, &Open)]) -> Vec<String> {
    let mut md = vec![
        "| open | seeded records | log (MiB) | start -> `listening` (ms) | samples | their gaps: \
         p50 / max (ms) | most in the open (MiB, at ms) | at `Hello`: footprint (MiB) | resident \
         (MiB) | peak (MiB) | peak (MB) | 2 s later: footprint (MiB) | peak (MiB) |"
            .to_string(),
        "|---|---:|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|".to_string(),
    ];
    for (what, records, bytes, open) in opens {
        let most = open.most_in_open().map_or_else(
            || "–".to_string(),
            |(at, f)| format!("{} at {at:.0}", mib(Some(f))),
        );
        md.push(format!(
            "| {what} | {records} | {} | {:.1} | {} | {} | {most} | {} | {} | {} | {} | {} | {} |",
            mib(Some(*bytes)),
            open.listening_ms,
            open.samples.len(),
            gaps_md(&open.samples),
            mib(open.at_hello.footprint),
            mib(open.at_hello.resident),
            mib(open.at_hello.peak),
            mb(open.at_hello.peak),
            mib(open.settled.footprint),
            mib(open.settled.peak),
        ));
    }
    md.push(String::new());
    md
}

/// The report's table of the served broker's readings, `(read, reading, health)`, each with the
/// most the world's `sampled` saw since the read before it (none before the first).
#[must_use]
pub fn readings_table(readings: &[Read], sampled: &[Reading]) -> Vec<String> {
    let mut md = vec![
        "| read | at (s) | footprint (MiB) | footprint (MB) | resident (MiB) | peak (MiB) | peak \
         (MB) | samples since the read before | most of them (MiB) | (MB) | health: resident \
         records | resident (MiB) | retained records | snapshot age (s) |"
            .to_string(),
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|".to_string(),
    ];
    let mut before: Option<f64> = None;
    for (read, r, h) in readings {
        let (n, most) = before.map_or((0, None), |lo| most_between(sampled, lo, r.at_ms));
        let most = most.map(|(_, f)| f);
        let n = before.map_or_else(|| "–".to_string(), |_| n.to_string());
        before = Some(r.at_ms);
        let (rr, rb, ret, age) = h.map_or_else(
            || {
                (
                    "–".to_string(),
                    "–".to_string(),
                    "–".to_string(),
                    "–".to_string(),
                )
            },
            |h| {
                (
                    h.resident_records
                        .map_or_else(|| "–".to_string(), |n| n.to_string()),
                    mib(h.resident_bytes),
                    h.retained.to_string(),
                    format!("{:.1}", h.age_ms as f64 / 1e3),
                )
            },
        );
        md.push(format!(
            "| {read} | {:.1} | {} | {} | {} | {} | {} | {n} | {} | {} | {rr} | {rb} | {ret} | \
             {age} |",
            r.at_ms / 1e3,
            mib(r.footprint),
            mb(r.footprint),
            mib(r.resident),
            mib(r.peak),
            mb(r.peak),
            mib(most),
            mb(most),
        ));
    }
    md.push(String::new());
    md
}

/// The report's table of the cold reads.
#[must_use]
pub fn cold_table(readers: &[ColdReader; 2]) -> Vec<String> {
    let mut md = vec![
        "| log | open at budget 0 (ms) | passes | records a pass | cold reads a pass | ns a record: \
         p50 | min | max |"
            .to_string(),
        "|---|---:|---:|---:|---|---:|---:|---:|".to_string(),
    ];
    for r in readers {
        let ns = r.ns_per_record();
        let s = stats::summarize(&ns);
        let lo = ns.iter().copied().fold(f64::INFINITY, f64::min);
        let cold: Vec<String> = r.cold.iter().map(u64::to_string).collect();
        md.push(format!(
            "| {} | {:.1} | {} | {COLD_RECORDS} | {} | {} | {:.1} | {} |",
            if r.sealed { "sealed" } else { "plaintext" },
            r.open_ms,
            r.pass_ms.len(),
            cold.join(", "),
            s.map_or_else(|| "–".to_string(), |s| format!("{:.1}", s.p50)),
            lo,
            s.map_or_else(|| "–".to_string(), |s| format!("{:.1}", s.max)),
        ));
    }
    md.push(String::new());
    md
}

#[cfg(test)]
mod tests {
    #[test]
    fn dropping_a_sampler_stops_and_joins_its_worker() {
        use super::*;

        let sampler =
            Sampler::pausing(std::process::id(), Instant::now(), Duration::from_millis(1));
        let stop = Arc::clone(&sampler.stop);
        drop(sampler);
        // Also stop a broken implementation before failing, so the negative control
        // cannot leave an unbounded detached sampler behind.
        let stopped = stop.swap(true, Ordering::Relaxed);
        assert!(stopped, "dropping the sampler must stop its actual worker");
        assert_eq!(Arc::strong_count(&stop), 1, "the worker has been joined");
    }

    #[test]
    fn the_gaps_are_between_the_readings_not_the_pause() {
        use super::*;

        let at = |at_ms| Reading {
            at_ms,
            ..Reading::default()
        };
        // A 10 ms pause after sweeps of 8, 9 and 12 ms: the readings lie 18 to 22 ms apart.
        let readings = [at(0.0), at(18.0), at(37.0), at(59.0)];
        let g = gaps(&readings).expect("three gaps");
        assert_eq!((g.n, g.p50, g.max), (3, 19.0, 22.0));
        assert_eq!(gaps_md(&readings), "19.0 / 22.0");
        assert!(gaps(&readings[..1]).is_none() && gaps_md(&[]) == "–");
    }
}
