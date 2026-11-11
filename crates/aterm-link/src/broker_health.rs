// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Broker health monitoring and poisoned-log handling.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often the serving loop looks for a poisoned log.
pub const POLL: Duration = Duration::from_secs(2);

const FAST_POLL: Duration = Duration::from_millis(100);

/// The serving loop's wake in this process: [`POLL`], or [`FAST_POLL`] at test pace.
pub(crate) fn poll() -> Duration {
    if crate::bridge::fast_timers() {
        FAST_POLL
    } else {
        POLL
    }
}

/// How often a serving broker has astream rewrite `<log>.health`.
pub const HEALTH_EVERY: Duration = Duration::from_secs(30);

/// Exit status of a broker that left its poisoned log for its supervisor to reopen.
pub const POISONED_EXIT: u8 = 75;

/// Word naming a poisoned log condition.
pub const POISONED_WORDS: &str = "poisoned";
/// Prefix of [`poisoned_line`] after `<log>: `.
pub const POISONED_EXIT_WORDS: &str = "poisoned, exiting 75";
/// Phrase emitted when no `aterm fabric on` job runs the broker.
pub const UNSUPERVISED_WORDS: &str = "no `aterm fabric on` job runs this broker";
/// What [`replaced_line`] says of a supervised broker's restart.
const SUPERVISOR_STARTS_WORDS: &str = "so the supervisor starts that one";

/// Format the diagnostic line emitted when a broker exits over a poisoned log.
#[must_use]
pub fn poisoned_line(log: &str, reason: &str, restart: Option<&str>) -> String {
    let head = format!("aterm link broker: {log}: {POISONED_EXIT_WORDS}");
    let why = one_line(reason);
    match restart {
        None => format!("{head} so the supervisor reopens it (astream: {why})"),
        Some(argv) => format!(
            "{head}; {UNSUPERVISED_WORDS}, so unless something else restarts it, restart it as \
             `{argv}` (astream: {why})"
        ),
    }
}

/// THE LINE A BROKER EXITS WITH WHEN ITS IMAGE WAS REPLACED (fix unit E5, E2E-6's residual),
/// after `aterm link broker: `: the broker exits either way, and the line says who starts the
/// new build, the supervisor (`restart` none, [`supervised`]) or, with no `aterm fabric on`
/// job running it, the command to start it again (`restart`, as [`poisoned_line`] gives it).
#[must_use]
pub fn replaced_line(path: &str, restart: Option<&str>) -> String {
    match restart {
        None => format!("{path} was replaced by another build; exiting {SUPERVISOR_STARTS_WORDS}"),
        Some(argv) => format!(
            "{path} was replaced by another build; exiting, and {UNSUPERVISED_WORDS}, so unless \
             something else restarts it, start it again as `{argv}`"
        ),
    }
}

fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

fn flushes(f: &astream_broker::FlushCounts) -> String {
    format!(
        "flushes since it opened: {} full, {} barrier, {} skipped, {} failed",
        f.full, f.barrier, f.skipped, f.failed
    )
}

/// Format the `health` row from the broker's `Hello` (`0x04`).
#[must_use]
pub fn in_band(h: &astream_broker::HelloHealth) -> String {
    match &h.poisoned {
        Some(why) => format!(
            "{} ({}): its log refuses every write until it is reopened — {}",
            POISONED_ROW_WORD,
            crate::render::safe(why, 256),
            flushes(&h.flushes)
        ),
        None => format!("ok, not poisoned — {}", flushes(&h.flushes)),
    }
}

/// Prefix of [`in_band`] for a poisoned log.
pub const POISONED_ROW_WORD: &str = "POISONED";

/// Format what the log holds, what of it the broker keeps decoded in memory ([`residency`]),
/// who is attached, and the flushes the broker's process has made (astream's
/// `process_flushes`, `astream_sys::flush_tally` as the snapshot took it: calls, so a refused
/// barrier and the full flush that answered it are one of each). A snapshot an older broker
/// wrote may carry no tally or no residency, and the row says so ([`NO_PROCESS_FLUSHES`],
/// [`NO_RESIDENCY`]), never zeros. Then how long those flushes took and the slow ones
/// ([`flush_times`] of astream's `process_flush_times`), or [`NO_FLUSH_TIMES`].
#[must_use]
pub fn holds(h: &astream_broker::BrokerHealth) -> String {
    let flushed = match &h.process_flushes {
        Some(t) => format!(
            "{} full, {} barrier and {} plain flush(es) by its process since it started",
            t.full, t.barrier, t.plain
        ),
        None => NO_PROCESS_FLUSHES.to_string(),
    };
    let flushed = format!(
        "{flushed}; {}",
        h.process_flush_times
            .as_ref()
            .map_or_else(|| NO_FLUSH_TIMES.to_string(), flush_times)
    );
    let resident = h.resident.map_or_else(
        || NO_RESIDENCY.to_string(),
        |r| residency(r.records, r.bytes, h.retained_records),
    );
    format!(
        "{} record(s) in {} ({resident}), {} subject(s), {} pending will(s), {} group \
         commit(s); {} connection(s), {} parked tail(s); {flushed}",
        h.retained_records,
        bytes(h.retained_bytes),
        h.distinct_subjects,
        h.pending_wills,
        h.group_commits,
        h.active_conns,
        h.parked_tails,
    )
}

/// `resident=<records>/<bytes> of <retained>`: of the `retained` records, the `records` the
/// broker keeps decoded in memory and the bytes its residency budget charges them (subject and
/// body, plus a fixed overhead each: astream's `Broker::residency`). It reads the rest back
/// from the log file when a reader asks for them. The broker's start line and [`holds`] say it.
#[must_use]
pub fn residency(records: u64, charged: u64, retained: u64) -> String {
    format!("resident={records}/{} of {retained}", bytes(charged))
}

/// [`residency`] of `broker` now, as its own `Broker::residency` counts it.
#[must_use]
pub fn residency_of(broker: &astream_broker::Broker) -> String {
    let (records, charged, retained) = broker.residency();
    let count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    residency(count(records), count(charged), count(retained))
}

/// [`holds`]'s residency for a snapshot that carries none: one an astream older than this
/// build's wrote.
pub const NO_RESIDENCY: &str = "its residency not in the snapshot";

/// The end of [`holds`] for a snapshot that carries no process tally: one an astream older
/// than this build's wrote.
pub const NO_PROCESS_FLUSHES: &str =
    "its process's flushes not in the snapshot (a broker older than this build)";

/// The end of [`holds`] for a snapshot that carries no flush times: one an astream older than
/// this build's wrote.
pub const NO_FLUSH_TIMES: &str =
    "its process's flush times not in the snapshot (a broker older than this build)";

/// How long a broker process's flushes took (astream's `process_flush_times`,
/// `astream_sys::flush_times` as the snapshot took it): each kind's total, how many were
/// slower than 1 ms (`astream_sys::SLOW_FLUSH_NS`), how many of those its ring kept and, when it
/// keeps one, the slowest of them, so a stall another process saw can be lined up with the ring
/// (`fabric --json`'s `process_flush_times` lists each, on the clock it names).
#[must_use]
pub fn flush_times(t: &astream_sys::FlushTimes) -> String {
    let ms = |ns: u64| ns as f64 / 1e6;
    let slowest = t
        .slow
        .iter()
        .max_by_key(|f| f.ns())
        .map_or_else(String::new, |f| {
            format!(", the slowest a {:.1} ms {}", ms(f.ns()), f.kind.word())
        });
    format!(
        "its flushes took {:.1} ms full, {:.1} ms barrier and {:.1} ms plain; {} slower than 1 \
         ms, {} kept{slowest}",
        ms(t.full_ns),
        ms(t.barrier_ns),
        ms(t.plain_ns),
        t.slow_count,
        t.slow.len(),
    )
}

/// Whether `read` reflects the broker currently serving the socket.
#[must_use]
pub fn is_current(read: &astream_broker::HealthRead, serving: Option<u32>) -> bool {
    read.writer == astream_broker::HealthWriter::Serving && serving == Some(read.health.pid)
}

/// Format the `holds` row at `now_ms` from `<log>.health`.
#[must_use]
pub fn snapshot_row(
    read: &astream_broker::HealthRead,
    serving: Option<u32>,
    now_ms: u64,
) -> String {
    use astream_broker::HealthWriter;
    let h = &read.health;
    let written = crate::fabric::age(now_ms.saturating_sub(h.written_at_ms));
    if is_current(read, serving) {
        return format!(
            "{} (astream's <log>.health, written {written} ago)",
            holds(h)
        );
    }
    let whose = match read.writer {
        HealthWriter::Gone => format!(
            "the last word of pid {}, which no longer serves the log",
            h.pid
        ),
        HealthWriter::Serving => format!(
            "pid {}'s, which serves the log but is not the pid serving the socket",
            h.pid
        ),
        HealthWriter::Unknown => format!(
            "pid {}'s, and this host cannot tell whether it still serves the log",
            h.pid
        ),
    };
    let poison = h.poisoned.as_deref().map_or_else(String::new, |why| {
        format!(
            "; that broker's log was {POISONED_ROW_WORD} ({})",
            crate::render::safe(why, 256)
        )
    });
    format!(
        "- (astream's <log>.health is {whose}, written {written} ago: {}{poison})",
        holds(h)
    )
}

fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut scale: u128 = 1024;
    let mut unit = UNITS[0];
    for next in &UNITS[1..] {
        if u128::from(n) / scale < 1024 {
            break;
        }
        scale *= 1024;
        unit = next;
    }
    let tenths = (u128::from(n) * 10 + scale / 2) / scale;
    format!("{}.{} {unit}", tenths / 10, tenths % 10)
}

/// Whether this process runs as an `aterm fabric on` job.
#[must_use]
pub fn supervised() -> bool {
    #[cfg(target_os = "linux")]
    let cgroup =
        aterm_uds::atomic_file::read_regular_text(std::path::Path::new("/proc/self/cgroup"), 65536)
            .ok();
    #[cfg(not(target_os = "linux"))]
    let cgroup: Option<String> = None;
    supervised_by(
        std::env::var_os("XPC_SERVICE_NAME").as_deref(),
        cgroup.as_deref(),
    )
}

fn supervised_by(xpc_service: Option<&OsStr>, cgroup: Option<&str>) -> bool {
    let launchd = xpc_service
        .and_then(OsStr::to_str)
        .is_some_and(is_job_label);
    let systemd = cgroup.is_some_and(|text| {
        text.lines().any(|line| {
            line.rsplit('/')
                .next()
                .and_then(|unit| unit.strip_suffix(".service"))
                .is_some_and(is_job_label)
        })
    });
    launchd || systemd
}

fn is_job_label(name: &str) -> bool {
    use crate::enable::LABEL;
    name == LABEL
        || name
            .strip_prefix(LABEL)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|sum| !sum.is_empty() && sum.chars().all(|c| c.is_ascii_digit()))
}

/// Action taken when the broker finds its log poisoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnPoison {
    #[default]
    Exit,
    Stay,
}

/// Broker fault-injection seam (`$ATERM_LINK_BROKER_FAULT`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Seam {
    /// Fail every full flush, from before anything is served.
    pub sync: bool,
    /// The extra escalation filters.
    pub escalate: Vec<String>,
    pub on_poison: OnPoison,
}

/// Environment variable for [`Seam`].
pub const FAULT_SEAM: &str = "ATERM_LINK_BROKER_FAULT";

/// Filename suffix for the one-shot `sync` fault marker.
pub const FAULT_FIRED_SUFFIX: &str = ".aterm-fault-fired";

impl Seam {
    /// Read the fault seam from the environment for `log`.
    #[must_use]
    pub fn from_env(log: &Path) -> Self {
        let Some(raw) = aterm_types::dev_seam!(FAULT_SEAM) else {
            return Self::default();
        };
        Self::parse(&raw.to_string_lossy(), fired_marker(log).exists())
    }

    fn parse(raw: &str, fired: bool) -> Self {
        let mut seam = Self::default();
        for word in raw.split([' ', ',']).filter(|w| !w.is_empty()) {
            match word.split_once('=') {
                Some(("escalate", filter)) => seam.escalate.push(filter.to_string()),
                _ if word == "sync" => seam.sync = !fired,
                _ if word == "stay" => seam.on_poison = OnPoison::Stay,
                _ => {}
            }
        }
        seam
    }

    /// Escalation rules configured by this seam.
    #[must_use]
    pub fn escalation(&self) -> Vec<astream_broker::Escalate> {
        self.escalate
            .iter()
            .map(|f| astream_broker::Escalate::Subject(f.clone()))
            .collect()
    }

    /// Arm the one-shot `sync` fault on `broker`.
    pub fn arm(&self, broker: &astream_broker::Broker, log: &Path) {
        if !self.sync {
            return;
        }
        let marker = fired_marker(log);
        let armed = crate::enable::create_temp_beside(&marker, 0o600).and_then(|(tmp, mut f)| {
            use std::io::Write as _;
            let res = f.write_all(b"1\n").and_then(|()| {
                drop(f);
                std::fs::rename(&tmp, &marker)
            });
            if res.is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
            res
        });
        if let Err(e) = armed {
            eprintln!(
                "aterm link broker: {FAULT_SEAM}: not arming sync: {}: {e}",
                marker.display()
            );
            return;
        }
        eprintln!(
            "aterm link broker: {FAULT_SEAM}: arming sync (one shot): every full flush fails"
        );
        broker.inject_faults(astream_broker::InjectedFaults {
            write: false,
            sync: true,
            truncate: false,
        });
    }
}

fn fired_marker(log: &Path) -> PathBuf {
    let mut name = log.as_os_str().to_os_string();
    name.push(FAULT_FIRED_SUFFIX);
    PathBuf::from(name)
}

/// ASTREAM'S OWN REASON for a poisoned log, read off a real broker the seam poisoned (Relaxed, `sync` armed, one escalated record).
#[cfg(test)]
pub(crate) fn astream_poisoned_reason() -> String {
    let dir = aterm_tempfile::Builder::new()
        .prefix("bhr")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let log = dir.path().join("b.log");
    let sock = dir.path().join("b.sock");
    let seam = Seam::parse("sync escalate=/e/>", false);
    let mut opts = astream_broker::OpenOptions::new(astream_broker::Durability::Relaxed);
    opts.escalation = astream_broker::Escalation::new(seam.escalation());
    let broker =
        astream_broker::Broker::open_opts(&log, opts, || Ok(astream_broker::Prepared::Nothing))
            .expect("open");
    let _served = broker.serve(&sock).expect("serve");
    seam.arm(&broker, &log);
    let mut c = astream_broker::Client::connect(&sock).expect("connect");
    let _ = c
        .publish(1, 1, "/e/a", b"escalated")
        .expect_err("its full flush failed");
    broker.poisoned().expect("the log is poisoned")
}

#[cfg(test)]
#[path = "broker_health_tests.rs"]
mod tests;
