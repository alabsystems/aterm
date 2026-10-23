// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BROKER'S OWN HEALTH: what a serving `aterm link broker` watches in the astream broker it runs, the one cure it applies itself.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often the serving loop looks for a poisoned log.
pub const POLL: Duration = Duration::from_secs(2);

/// How often a serving broker has astream rewrite `<log>.health` (the module docs).
pub const HEALTH_EVERY: Duration = Duration::from_secs(30);

/// The exit status of a broker that left its poisoned log for its supervisor to reopen.
pub const POISONED_EXIT: u8 = 75;

/// The word every line the broker writes about a poisoned log names the condition by (the exit's, the `stay` seam's, a stop's over one).
pub const POISONED_WORDS: &str = "poisoned";
/// The words that OPEN [`poisoned_line`] right after `<log>: `, the condition and the exit together, which doctor picks the remedy of a quoted `broker.err` line by (`enable.rs`'s `quoted_broker_fix`).
pub const POISONED_EXIT_WORDS: &str = "poisoned, exiting 75";
/// The words of the line a broker no `aterm fabric on` job runs writes, which doctor tells it by.
pub const UNSUPERVISED_WORDS: &str = "no `aterm fabric on` job runs this broker";

/// The one line a broker leaves when it exits over a poisoned log, naming astream's `reason` on one line.
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

/// `text` on one line: a line break in astream's reason would split the one
/// line `broker.err`'s reader quotes.
fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// The flushes a log has issued since it opened ([`astream_broker::FlushCounts`]),
/// as one phrase.
fn flushes(f: &astream_broker::FlushCounts) -> String {
    format!(
        "flushes since it opened: {} full, {} barrier, {} skipped, {} failed",
        f.full, f.barrier, f.skipped, f.failed
    )
}

/// The BROKER section's `health` row and doctor's `health:` line, from the broker's own `Hello` (entry 0x04, [`astream_broker::HelloHealth`]).
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

/// The word [`in_band`] opens a poisoned log's row with, which doctor and the tests read it by.
pub const POISONED_ROW_WORD: &str = "POISONED";

/// What the log holds and who is attached ([`astream_broker::BrokerHealth`], R5), as one phrase.
#[must_use]
pub fn holds(h: &astream_broker::BrokerHealth) -> String {
    format!(
        "{} record(s) in {}, {} subject(s), {} pending will(s), {} group commit(s); {} \
         connection(s), {} parked tail(s)",
        h.retained_records,
        bytes(h.retained_bytes),
        h.distinct_subjects,
        h.pending_wills,
        h.group_commits,
        h.active_conns,
        h.parked_tails,
    )
}

/// Whether `read` — a `<log>.health` as astream's reader found it.
#[must_use]
pub fn is_current(read: &astream_broker::HealthRead, serving: Option<u32>) -> bool {
    read.writer == astream_broker::HealthWriter::Serving && serving == Some(read.health.pid)
}

/// The BROKER section's `holds` row, at `now_ms`, from astream's snapshot beside the log.
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

/// `n` bytes as a person reads them: `512 B`, `3.1 KiB`, `2.0 MiB`, rounded to
/// a tenth, in integers.
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

/// Whether THIS process runs as an `aterm fabric on` job.
#[must_use]
pub fn supervised() -> bool {
    #[cfg(target_os = "linux")]
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok();
    #[cfg(not(target_os = "linux"))]
    let cgroup: Option<String> = None;
    supervised_by(
        std::env::var_os("XPC_SERVICE_NAME").as_deref(),
        cgroup.as_deref(),
    )
}

/// [`supervised`]'s rule on what the process was told: launchd's service name,
/// and the text of `/proc/self/cgroup` (one `<id>:<controllers>:<path>` line a
/// hierarchy).
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

/// Whether `name` is a label `aterm fabric on` gives a broker job: the plain
/// [`crate::enable::LABEL`] (the default root) or `LABEL.<cksum>` (any other).
fn is_job_label(name: &str) -> bool {
    use crate::enable::LABEL;
    name == LABEL
        || name
            .strip_prefix(LABEL)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|sum| !sum.is_empty() && sum.chars().all(|c| c.is_ascii_digit()))
}

/// What the broker does when it finds its log poisoned: exit (the cure, and the only behaviour of a shipped build), or.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnPoison {
    #[default]
    Exit,
    Stay,
}

/// THE BROKER'S FAULT SEAM, `$ATERM_LINK_BROKER_FAULT`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Seam {
    /// Fail every full flush, from before anything is served.
    pub sync: bool,
    /// The extra escalation filters.
    pub escalate: Vec<String>,
    pub on_poison: OnPoison,
}

/// The seam's variable.
pub const FAULT_SEAM: &str = "ATERM_LINK_BROKER_FAULT";

/// What the marker that makes `sync` one-shot adds to the log's name.
pub const FAULT_FIRED_SUFFIX: &str = ".aterm-fault-fired";

impl Seam {
    /// The seam as this process's environment sets it for `log` — the default (nothing) in a release build, or with the variable unset.
    #[must_use]
    pub fn from_env(log: &Path) -> Self {
        let Some(raw) = aterm_types::dev_seam!(FAULT_SEAM) else {
            return Self::default();
        };
        Self::parse(&raw.to_string_lossy(), fired_marker(log).exists())
    }

    /// The words of `raw`; `sync` only while it has not `fired`. An unknown
    /// word arms nothing.
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

    /// The rules the open adds to this build's escalation table.
    #[must_use]
    pub fn escalation(&self) -> Vec<astream_broker::Escalate> {
        self.escalate
            .iter()
            .map(|f| astream_broker::Escalate::Subject(f.clone()))
            .collect()
    }

    /// Arm `sync` on `broker`, once: the marker beside `log` first, so a broker that exits over the fault never arms it again.
    pub fn arm(&self, broker: &astream_broker::Broker, log: &Path) {
        if !self.sync {
            return;
        }
        let marker = fired_marker(log);
        if let Err(e) = std::fs::write(&marker, b"1\n") {
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
            sync: true,
            truncate: false,
        });
    }
}

/// `<log>.aterm-fault-fired`.
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
mod tests {
    use super::*;

    /// **THE EXIT LINE SAYS WHAT DOCTOR READS FIRST AND ASTREAM'S REASON
    /// LAST, ON ONE LINE, AND WHO REOPENS THE LOG.** Supervised, the
    /// supervisor; with no `aterm fabric on` job, the line says so and names
    /// the command — unless something else restarts it, which this build
    /// cannot see.
    #[test]
    fn the_exit_line_names_astreams_reason_and_who_reopens_the_log() {
        let reason = "a flush failed (EIO)\nbadly";
        let supervised = poisoned_line("/r/bus.log", reason, None);
        assert_eq!(
            supervised,
            "aterm link broker: /r/bus.log: poisoned, exiting 75 so the supervisor reopens it \
             (astream: a flush failed (EIO) badly)"
        );
        let argv = "aterm link broker /r/bus.sock /r/bus.log --secret-file /r/mint.secret";
        let alone = poisoned_line("/r/bus.log", reason, Some(argv));
        assert_eq!(
            alone,
            format!(
                "aterm link broker: /r/bus.log: poisoned, exiting 75; no `aterm fabric on` job \
                 runs this broker, so unless something else restarts it, restart it as `{argv}` \
                 (astream: a flush failed (EIO) badly)"
            )
        );
        assert!(!alone.contains("so the supervisor reopens it"), "{alone}");
        assert!(!alone.contains("no supervisor"), "{alone}");
        assert!(!supervised.contains("restart it as"), "{supervised}");
        assert!(!supervised.contains(UNSUPERVISED_WORDS), "{supervised}");
        for line in [&supervised, &alone] {
            assert!(!line.contains('\n'), "{line}");
            assert!(
                line.starts_with(&format!(
                    "aterm link broker: /r/bus.log: {POISONED_EXIT_WORDS}"
                )),
                "{line}"
            );
            assert!(
                line.ends_with("(astream: a flush failed (EIO) badly)"),
                "{line}"
            );
        }
    }

    /// **A SUPERVISOR IS READ OFF THE PROCESS, AND ONLY `aterm fabric on`'S
    /// JOB COUNTS.** launchd's service name and systemd's unit cgroup name the
    /// job: the default root's plain label and a per-root `LABEL.<cksum>` are
    /// supervised.
    #[test]
    fn a_supervisor_is_the_jobs_own_word() {
        use crate::enable::LABEL;
        let xpc = |s: &str| supervised_by(Some(OsStr::new(s)), None);
        let cgroup = |s: &str| supervised_by(None, Some(s));
        assert!(xpc(LABEL));
        assert!(xpc(&format!("{LABEL}.4294967295")));
        assert!(cgroup(&format!(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/{LABEL}.service\n"
        )));
        assert!(cgroup(&format!(
            "12:pids:/user.slice\n1:name=systemd:/user.slice/user-1000.slice/user@1000.service/\
             {LABEL}.123.service\n"
        )));
        assert!(!xpc("0"));
        assert!(!xpc("application.systems.alab.aterm.12345.67890"));
        assert!(!xpc(&format!("{LABEL}-other")));
        assert!(!xpc(&format!("{LABEL}.")));
        assert!(!xpc(&format!("{LABEL}.x1")));
        assert!(!cgroup(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/other.service\n"
        ));
        assert!(!cgroup(&format!(
            "0::/user.slice/user-1000.slice/session-2.scope/{LABEL}\n"
        )));
        assert!(!supervised_by(None, None));
    }

    /// **THE SEAM'S `sync` IS ONE-SHOT, ITS SWITCHES ARE NOT.** With the
    /// marker absent `sync` is armed; once it exists (the broker that armed it
    /// wrote it) the restarted broker arms nothing, while `escalate=` and
    /// `stay` still hold.
    #[test]
    fn the_seam_arms_its_fault_once() {
        let first = Seam::parse("sync,escalate=/f/*/in/> stay escalate=/x bogus", false);
        assert!(first.sync);
        assert_eq!(
            first.escalate,
            vec!["/f/*/in/>".to_string(), "/x".to_string()]
        );
        assert_eq!(first.on_poison, OnPoison::Stay);
        assert_eq!(first.escalation().len(), 2);
        let again = Seam::parse("sync,escalate=/f/*/in/> stay", true);
        assert!(!again.sync, "a fired fault is not armed again");
        assert_eq!(again.escalate, vec!["/f/*/in/>".to_string()]);
        assert_eq!(again.on_poison, OnPoison::Stay);
        assert_eq!(Seam::parse("bogus syncs stays", false), Seam::default());
    }

    /// **ARMING WRITES THE MARKER FIRST, AND THE FAULT IS ASTREAM'S OWN.** On a
    /// real broker at Relaxed, with the seam's escalation naming one subject:
    /// before arming, an escalated publish is acked and nothing is poisoned
    /// (the negative control); armed, an ordinary publish is still acked — a
    /// relaxed batch flushes nothing — and the next escalated one's full flush
    /// fails, which poisons the log; the marker is beside it, so the seam
    /// read again for the same log arms nothing.
    #[test]
    fn the_armed_seam_poisons_a_relaxed_log_at_its_first_escalated_record() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("bhs")
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
        let served = broker.serve(&sock).expect("serve");
        let mut c = astream_broker::Client::connect(&sock).expect("connect");
        c.publish(1, 1, "/e/a", b"escalated, healthy")
            .expect("acked");
        assert_eq!(broker.poisoned(), None);
        seam.arm(&broker, &log);
        assert!(fired_marker(&log).exists(), "the marker is written");
        c.publish(1, 2, "/o/a", b"ordinary")
            .expect("a relaxed batch flushes nothing");
        assert_eq!(broker.poisoned(), None);
        let e = c
            .publish(1, 3, "/e/a", b"escalated")
            .expect_err("its full flush failed");
        assert!(e.to_string().contains("durability flush failed"), "{e}");
        let why = broker.poisoned().expect("the log is poisoned");
        assert!(!why.is_empty());
        let refused = c.publish(1, 4, "/o/b", b"after").expect_err("poisoned");
        assert!(
            astream_broker::broker_refusal(&refused).is_some_and(|r| r.is_poisoned()),
            "{refused}"
        );
        assert!(!Seam::parse("sync", fired_marker(&log).exists()).sync);
        drop(c);
        drop(served);
        assert_eq!(broker.poisoned().as_deref(), Some(why.as_str()));
    }

    /// **THE FACTS ARE ASTREAM'S OWN, IN BAND AND ON DISK**, read off a real
    /// broker kept the way the serving loop keeps it (Relaxed, astream's
    /// `<log>.health` every [`HEALTH_EVERY`]) through astream's own readers —
    /// a fresh `Hello`'s entry 0x04 and `read_health` — and said by this
    /// module's rows.
    #[test]
    fn the_facts_name_what_astream_decided() {
        use astream_broker::{HealthWriter, ServedReason, Tier};
        let dir = aterm_tempfile::Builder::new()
            .prefix("bhf")
            .tempdir_in("/tmp")
            .expect("scratch");
        let log = dir.path().join("b.log");
        let sock = dir.path().join("b.sock");
        let seam = Seam::parse("escalate=/e/>", false);
        let mut opts = astream_broker::OpenOptions::new(astream_broker::Durability::Relaxed);
        opts.escalation = astream_broker::Escalation::new(seam.escalation());
        let broker =
            astream_broker::Broker::open_opts(&log, opts, || Ok(astream_broker::Prepared::Nothing))
                .expect("open");
        broker
            .keep_health(HEALTH_EVERY)
            .expect("astream writes the sidecar at once");
        let served = broker.serve(&sock).expect("serve");
        let me = std::process::id();
        let mut c = astream_broker::Client::connect(&sock).expect("connect");
        c.publish(1, 1, "/o/a", b"before").expect("acked");
        let hello = |c: &mut astream_broker::Client<std::os::unix::net::UnixStream>| {
            c.hello().expect("hello");
            (
                c.broker_health()
                    .cloned()
                    .expect("this pin's broker sends entry 0x04"),
                c.served_durability().expect("and the tier"),
            )
        };
        let mut probe = astream_broker::Client::connect(&sock).expect("probe");

        let (healthy, tier) = hello(&mut probe);
        assert_eq!(healthy.poisoned, None);
        assert_eq!(healthy.flushes.failed, 0);
        assert_eq!(tier.served, Tier::Relaxed);
        assert!(in_band(&healthy).starts_with("ok, not poisoned — flushes since it opened: "));
        broker.write_health().expect("a snapshot after the publish");
        let read = astream_broker::read_health(&log)
            .expect("read")
            .expect("a sidecar");
        assert_eq!(read.writer, HealthWriter::Serving);
        assert_eq!(read.health.pid, me);
        assert_eq!(
            (
                read.health.retained_records,
                read.health.distinct_subjects,
                read.health.active_conns,
                read.health.poisoned.as_deref(),
            ),
            (1, 1, 2, None),
            "{read:?}"
        );
        assert!(is_current(&read, Some(me)));
        assert!(
            !is_current(&read, Some(me.wrapping_add(1))),
            "another pid serves the socket"
        );
        assert!(!is_current(&read, None), "no pid serves the socket");
        let row = snapshot_row(&read, Some(me), read.health.written_at_ms);
        assert!(
            row.starts_with("1 record(s) in ") && row.contains("2 connection(s)"),
            "{row}"
        );

        broker.inject_faults(astream_broker::InjectedFaults {
            sync: true,
            truncate: false,
        });
        c.publish(1, 2, "/e/a", b"escalated")
            .expect_err("its full flush failed");
        let (sick, tier) = hello(&mut probe);
        let why = sick.poisoned.as_deref().expect("poisoned, in band");
        assert!(why.contains("a flush failed"), "{why}");
        assert_eq!(sick.flushes.failed, 1);
        assert_eq!(
            (tier.served, tier.reason),
            (Tier::Strict, ServedReason::LatchedAfterFlushFailure)
        );
        let row = in_band(&sick);
        assert!(
            row.starts_with(&format!("{POISONED_ROW_WORD} (")) && row.contains(", 1 failed"),
            "{row}"
        );
        let read = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "astream's sidecar to name the poison",
            || {
                astream_broker::read_health(&log)
                    .ok()
                    .flatten()
                    .filter(|r| r.health.poisoned.is_some())
            },
        );
        assert_eq!(read.health.served.served, Tier::Strict);
        assert_eq!(read.health.flushes.failed, 1);
        assert!(is_current(&read, Some(me)), "still served, poisoned");

        drop(c);
        drop(probe);
        drop(served);
        let read = astream_broker::read_health(&log)
            .expect("read")
            .expect("the last word stays");
        assert_eq!(read.writer, HealthWriter::Gone);
        assert!(
            !is_current(&read, Some(me)),
            "a gone writer is never current"
        );
        let row = snapshot_row(&read, Some(me), read.health.written_at_ms);
        assert!(
            row.starts_with(&format!(
                "- (astream's <log>.health is the last word of pid {me}, which no longer serves \
                 the log, written 0s ago: "
            )) && row.contains(&format!("that broker's log was {POISONED_ROW_WORD} (")),
            "{row}"
        );
    }

    /// The rows' arithmetic and whose-word wording: bytes in tenths, rounded
    /// in integers; a snapshot whose writer the reader cannot tell is never
    /// current, and its row says so.
    #[test]
    fn a_snapshot_says_whose_word_it_is() {
        use astream_broker::{BrokerHealth, HealthRead, HealthWriter};
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(3 * 1024 * 1024 + 100_000), "3.1 MiB");
        assert_eq!(bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
        let mut health = BrokerHealth::default();
        health.pid = 7;
        health.written_at_ms = 1_000;
        health.retained_records = 3;
        let read = |writer| HealthRead {
            health: health.clone(),
            writer,
        };
        assert!(is_current(&read(HealthWriter::Serving), Some(7)));
        assert!(!is_current(&read(HealthWriter::Serving), Some(8)));
        assert!(!is_current(&read(HealthWriter::Unknown), Some(7)));
        let unknown = snapshot_row(&read(HealthWriter::Unknown), Some(7), 61_000);
        assert_eq!(
            unknown,
            "- (astream's <log>.health is pid 7's, and this host cannot tell whether it still \
             serves the log, written 1m ago: 3 record(s) in 0 B, 0 subject(s), 0 pending \
             will(s), 0 group commit(s); 0 connection(s), 0 parked tail(s))"
        );
        let other = snapshot_row(&read(HealthWriter::Serving), Some(8), 1_000);
        assert!(
            other.contains("is not the pid serving the socket"),
            "{other}"
        );
        assert!(!other.contains(POISONED_ROW_WORD), "{other}");
    }

    /// **A HEALTHY LOG READS UNPOISONED AFTER ITS LISTENERS GO** — the
    /// negative control of the look the stop path makes after the drop: a
    /// relaxed log closed cleanly is not reported poisoned.
    #[test]
    fn a_healthy_log_reads_unpoisoned_after_its_listeners_go() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("bhh")
            .tempdir_in("/tmp")
            .expect("scratch");
        let log = dir.path().join("b.log");
        let sock = dir.path().join("b.sock");
        let opts = astream_broker::OpenOptions::new(astream_broker::Durability::Relaxed);
        let broker =
            astream_broker::Broker::open_opts(&log, opts, || Ok(astream_broker::Prepared::Nothing))
                .expect("open");
        let served = broker.serve(&sock).expect("serve");
        let mut c = astream_broker::Client::connect(&sock).expect("connect");
        c.publish(1, 1, "/o/a", b"ordinary").expect("acked");
        drop(c);
        drop(served);
        assert_eq!(broker.poisoned(), None);
    }
}
