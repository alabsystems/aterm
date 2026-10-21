// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BROKER'S HEALTH ON DISK: `<log>.health`, the operator's snapshot of a running
//! broker (`REQUIREMENTS` R5) beside the log it serves, in a format astream owns.
//!
//! # Why a file, and why astream's
//!
//! A broker's in-process snapshot ([`Broker::stats`](crate::Broker::stats),
//! [`Broker::flush_counts`](crate::Broker::flush_counts),
//! [`Broker::poisoned`](crate::Broker::poisoned)) is readable only by the process that
//! embeds it, and a second process cannot open the log to read its own: the log is
//! exclusively locked while it is served (`asb stats` on a live log met exactly that
//! refusal). The two facts every reader needs at once — is the log POISONED, and what
//! have its flushes done — ride the answer to every `Hello` (the extension block's tag
//! 0x04, [`HelloHealth`](crate::HelloHealth)). The rest of R5 — what the log holds, who
//! is attached, what is parked — is an operator's, read on the host, so it goes in a
//! file beside the log, mode 0600: only the user the broker runs as reads it (an embedder
//! that keeps its log in a 0700 directory, as aterm does, adds that directory's rule),
//! which is R5's read grant on one machine, and no request tag is spent. (An
//! attach-gated request for it remains possible, and is not built.)
//!
//! An embedder that wrote its own file of these facts would invent a format, a pid
//! convention and a staleness rule astream never sees; this is the one format, and
//! [`read_health`] is the one reader.
//!
//! # Who wrote it, and is that broker still serving
//!
//! Every snapshot names the writer's `pid` and when it was written. A pid alone cannot
//! say whether the snapshot is CURRENT: the broker may have died since, and its pid
//! been reused. So the writer LOCKS each file it writes (an advisory exclusive lock,
//! taken on the fresh file before it is renamed into place) and keeps that lock until
//! it replaces the file again, the log closes, or the process dies — when the kernel
//! drops it. [`read_health`] asks for a shared lock on the file it read:
//!
//! - refused: a live process holds the file it read, and that process is the broker
//!   that still has the log open — [`HealthWriter::Serving`];
//! - granted, on the file still at the name: no process holds it, so the broker that
//!   wrote it closed the log or died — [`HealthWriter::Gone`], its last word;
//! - granted on a file the writer has since replaced: read again (bounded);
//! - no advisory locks here (another platform, a filesystem without them):
//!   [`HealthWriter::Unknown`] — judge by `pid` and `written_at_ms`.
//!
//! The reader's probe never blocks the writer, which locks only fresh files and never
//! asks for the lock on a file it already published.
//!
//! # Replaced, never edited; no flush
//!
//! Each snapshot is written whole to `<log>.health.tmp` (created fresh, mode 0600, never
//! through a link planted at the name), then renamed over `<log>.health`: a reader sees
//! one snapshot or the one before it, never a mix. It is NOT flushed: it describes a
//! running process and is rewritten every period, and a power loss that leaves it short
//! or stale leaves a broker that rewrites it at its next open — a file that does not
//! parse reads as an error, never as facts. A full flush per period would tax every
//! other writer on the volume for a snapshot nobody needs to survive a reboot.

use crate::logid::LogIdentity;
use crate::proto::{ServedInfo, ServedReason, Tier};
use crate::store::FlushCounts;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// What the sidecar's name adds to the log's: `<log>.health`.
pub const HEALTH_SUFFIX: &str = ".health";

/// The sidecar's first line, and its format's version. A later format is a later line:
/// this build reads it as a file it does not know, never as facts.
const HEADER: &str = "astream-broker-health 1";

/// The most a reader takes from the file: a snapshot is a few hundred bytes, and a
/// planted multi-gigabyte file must not be read into memory.
const READ_MAX: u64 = 64 * 1024;

/// How many times [`read_health`] reads again when the file it read was replaced
/// before its lock probe. A writer replaces it once a period, so one retry almost
/// always settles it; the bound makes a writer replacing it without pause read
/// [`HealthWriter::Unknown`], never a loop.
const READ_ATTEMPTS: usize = 8;

/// The health sidecar beside `log`: `<log>.health`.
#[must_use]
pub fn health_path(log: &Path) -> PathBuf {
    let mut p = log.to_path_buf();
    p.as_mut_os_string().push(HEALTH_SUFFIX);
    p
}

/// A broker's health: the `REQUIREMENTS` R5 snapshot an operator reads, as the broker
/// wrote it to `<log>.health` ([`Broker::health`](crate::Broker::health) takes it in
/// process). Each count is exact when taken; the snapshot is not one instant across
/// the log's lock and the counters beside it, which no reader of it needs.
///
/// `non_exhaustive`: a later build may add a fact. Build one with `default()` and set
/// the fields.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerHealth {
    /// The process that wrote it ([`std::process::id`]).
    pub pid: u32,
    /// When it was written, Unix milliseconds ([`Broker::now_ms`](crate::Broker::now_ms)).
    pub written_at_ms: u64,
    /// The log it describes, at the generation it was written under.
    pub identity: LogIdentity,
    /// The tier asked for, the tier served, and why they differ.
    pub served: ServedInfo,
    /// The lowest retained offset.
    pub base: u64,
    /// The log's head: the offset the next commit takes.
    pub head: u64,
    /// The subscriber-visible head ([`Broker::visible_head`](crate::Broker::visible_head)).
    pub visible_head: u64,
    /// The durable head ([`Broker::durable_head`](crate::Broker::durable_head)).
    pub durable_head: u64,
    /// Retained records, and the bytes of their frames on disk.
    pub retained_records: u64,
    pub retained_bytes: u64,
    /// Distinct visible subjects in the last-value index.
    pub distinct_subjects: u64,
    /// Durably bound producer ids.
    pub bound_producers: u64,
    /// Last wills that have neither fired nor been fenced.
    pub pending_wills: u64,
    /// Consumer groups with a durable committed offset.
    pub group_commits: u64,
    /// The log is a replication target.
    pub replica: bool,
    /// Open client connections.
    pub active_conns: u64,
    /// Subscriber tails parked waiting for a record.
    pub parked_tails: u64,
    /// The flushes the log has issued since it opened ([`FlushCounts`]).
    pub flushes: FlushCounts,
    /// `Some(reason)` while the log is POISONED: it refuses every write until it is
    /// reopened ([`Broker::poisoned`](crate::Broker::poisoned)).
    pub poisoned: Option<String>,
}

impl Default for BrokerHealth {
    fn default() -> Self {
        BrokerHealth {
            pid: 0,
            written_at_ms: 0,
            identity: LogIdentity {
                id: [0; 16],
                generation: 0,
            },
            served: ServedInfo {
                requested: Tier::Strict,
                served: Tier::Strict,
                reason: ServedReason::None,
                horizon_ms: None,
                rules: 0,
            },
            base: 0,
            head: 0,
            visible_head: 0,
            durable_head: 0,
            retained_records: 0,
            retained_bytes: 0,
            distinct_subjects: 0,
            bound_producers: 0,
            pending_wills: 0,
            group_commits: 0,
            replica: false,
            active_conns: 0,
            parked_tails: 0,
            flushes: FlushCounts::default(),
            poisoned: None,
        }
    }
}

/// Whether the broker that wrote a `<log>.health` still serves its log
/// ([`read_health`]; the module docs say how it is told).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthWriter {
    /// The process that wrote the file still holds it, and so still has the log open:
    /// the facts are the serving broker's own, as of `written_at_ms`.
    Serving,
    /// No process holds the file: the broker that wrote it closed the log or died. The
    /// facts are its last word, not the state of any broker now.
    Gone,
    /// The reader cannot tell: advisory locks are not kept here (another platform, or a
    /// filesystem without them), or the file was replaced under every look.
    Unknown,
}

/// A `<log>.health` as [`read_health`] found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthRead {
    /// The snapshot.
    pub health: BrokerHealth,
    /// Whether its writer still serves the log.
    pub writer: HealthWriter,
}

/// A tier as the file names it.
fn tier_word(t: Tier) -> String {
    match t {
        Tier::Strict => "strict".to_string(),
        Tier::Ordered => "ordered".to_string(),
        Tier::Relaxed => "relaxed".to_string(),
        Tier::Other(c) => format!("tier-{c}"),
    }
}

fn parse_tier(s: &str) -> Option<Tier> {
    match s {
        "strict" => Some(Tier::Strict),
        "ordered" => Some(Tier::Ordered),
        "relaxed" => Some(Tier::Relaxed),
        _ => match s.strip_prefix("tier-") {
            Some(c) => match c.parse::<u8>() {
                Ok(c) => Some(Tier::from_code(c)),
                Err(_) => None,
            },
            None => None,
        },
    }
}

/// A served reason as the file names it.
fn reason_word(r: ServedReason) -> String {
    match r {
        ServedReason::None => "none".to_string(),
        ServedReason::NotApple => "not-apple".to_string(),
        ServedReason::NotBootVolume => "not-boot-volume".to_string(),
        ServedReason::BarrierRefused => "barrier-refused".to_string(),
        ServedReason::LatchedAfterBarrierError => "latched-after-barrier-error".to_string(),
        ServedReason::LatchedAfterFlushFailure => "latched-after-flush-failure".to_string(),
        other => format!("reason-{}", other.code()),
    }
}

fn parse_reason(s: &str) -> Option<ServedReason> {
    match s {
        "none" => Some(ServedReason::None),
        "not-apple" => Some(ServedReason::NotApple),
        "not-boot-volume" => Some(ServedReason::NotBootVolume),
        "barrier-refused" => Some(ServedReason::BarrierRefused),
        "latched-after-barrier-error" => Some(ServedReason::LatchedAfterBarrierError),
        "latched-after-flush-failure" => Some(ServedReason::LatchedAfterFlushFailure),
        _ => match s.strip_prefix("reason-") {
            Some(c) => match c.parse::<u8>() {
                Ok(c) => Some(ServedReason::from_code(c)),
                Err(_) => None,
            },
            None => None,
        },
    }
}

/// `text` on one line: a line break in a poison's reason would end the value and start
/// a key of its own.
fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

impl BrokerHealth {
    /// The counters, by their key in the file.
    fn counts(&self) -> [(&'static str, u64); 18] {
        [
            ("written_at_ms", self.written_at_ms),
            ("base", self.base),
            ("head", self.head),
            ("visible_head", self.visible_head),
            ("durable_head", self.durable_head),
            ("records", self.retained_records),
            ("bytes", self.retained_bytes),
            ("subjects", self.distinct_subjects),
            ("producers", self.bound_producers),
            ("pending_wills", self.pending_wills),
            ("group_commits", self.group_commits),
            ("connections", self.active_conns),
            ("parked_tails", self.parked_tails),
            ("flush_barrier", self.flushes.barrier),
            ("flush_full", self.flushes.full),
            ("flush_skipped", self.flushes.skipped),
            ("flush_failed", self.flushes.failed),
            ("rules", u64::from(self.served.rules)),
        ]
    }

    /// The file's text: its first line, `astream-broker-health 1` (the format's
    /// version), then one `key=value` line each, the poison's reason (when there is one)
    /// last. Every key [`parse`](Self::parse) needs is always written.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(512);
        out.push_str(HEADER);
        out.push('\n');
        let mut kv = |k: &str, v: &str| {
            out.push_str(k);
            out.push('=');
            out.push_str(v);
            out.push('\n');
        };
        kv("pid", &self.pid.to_string());
        kv("log", &self.identity.to_string());
        kv("requested", &tier_word(self.served.requested));
        kv("served", &tier_word(self.served.served));
        kv("served_reason", &reason_word(self.served.reason));
        match self.served.horizon_ms {
            Some(ms) => kv("horizon_ms", &ms.to_string()),
            None => kv("horizon_ms", "-"),
        }
        kv("replica", if self.replica { "yes" } else { "no" });
        for (k, v) in self.counts() {
            kv(k, &v.to_string());
        }
        match &self.poisoned {
            Some(why) => {
                kv("poisoned", "yes");
                kv("reason", &one_line(why));
            }
            None => kv("poisoned", "no"),
        }
        out
    }

    /// The snapshot in `text`, or `None` for text that is not one this build reads:
    /// another first line (another format, or a file cut short), a key it writes
    /// missing, or a value that does not parse. Keys it does not know are skipped, so a
    /// later build that adds one is still read.
    #[must_use]
    pub fn parse(text: &str) -> Option<BrokerHealth> {
        let mut lines = text.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let mut h = BrokerHealth::default();
        // Every key `render` writes but `reason` (present exactly when poisoned).
        let mut seen = 0u32;
        let mut poisoned = None;
        let mut reason = None;
        let mut horizon = None;
        for line in lines {
            let Some((k, v)) = line.split_once('=') else {
                return None;
            };
            let num = || v.parse::<u64>().ok();
            let bit = match k {
                "pid" => {
                    h.pid = v.parse().ok()?;
                    0
                }
                "log" => {
                    h.identity = LogIdentity::parse(v)?;
                    1
                }
                "requested" => {
                    h.served.requested = parse_tier(v)?;
                    2
                }
                "served" => {
                    h.served.served = parse_tier(v)?;
                    3
                }
                "served_reason" => {
                    h.served.reason = parse_reason(v)?;
                    4
                }
                "horizon_ms" => {
                    horizon = Some(if v == "-" {
                        None
                    } else {
                        Some(v.parse::<u32>().ok()?)
                    });
                    5
                }
                "replica" => {
                    h.replica = match v {
                        "yes" => true,
                        "no" => false,
                        _ => return None,
                    };
                    6
                }
                "written_at_ms" => {
                    h.written_at_ms = num()?;
                    7
                }
                "base" => {
                    h.base = num()?;
                    8
                }
                "head" => {
                    h.head = num()?;
                    9
                }
                "visible_head" => {
                    h.visible_head = num()?;
                    10
                }
                "durable_head" => {
                    h.durable_head = num()?;
                    11
                }
                "records" => {
                    h.retained_records = num()?;
                    12
                }
                "bytes" => {
                    h.retained_bytes = num()?;
                    13
                }
                "subjects" => {
                    h.distinct_subjects = num()?;
                    14
                }
                "producers" => {
                    h.bound_producers = num()?;
                    15
                }
                "pending_wills" => {
                    h.pending_wills = num()?;
                    16
                }
                "group_commits" => {
                    h.group_commits = num()?;
                    17
                }
                "connections" => {
                    h.active_conns = num()?;
                    18
                }
                "parked_tails" => {
                    h.parked_tails = num()?;
                    19
                }
                "flush_barrier" => {
                    h.flushes.barrier = num()?;
                    20
                }
                "flush_full" => {
                    h.flushes.full = num()?;
                    21
                }
                "flush_skipped" => {
                    h.flushes.skipped = num()?;
                    22
                }
                "flush_failed" => {
                    h.flushes.failed = num()?;
                    23
                }
                "rules" => {
                    h.served.rules = v.parse().ok()?;
                    24
                }
                "poisoned" => {
                    poisoned = Some(match v {
                        "yes" => true,
                        "no" => false,
                        _ => return None,
                    });
                    25
                }
                "reason" => {
                    reason = Some(v.to_string());
                    continue;
                }
                _ => continue,
            };
            seen |= 1u32 << bit;
        }
        if seen != (1u32 << 26) - 1 {
            return None;
        }
        // A horizon belongs to Ordered alone, as on the wire.
        h.served.horizon_ms = match (h.served.requested, horizon) {
            (Tier::Ordered, Some(ms)) => ms,
            _ => None,
        };
        h.poisoned = match (poisoned, reason) {
            (Some(true), Some(why)) => Some(why),
            (Some(false), None) => None,
            _ => return None,
        };
        Some(h)
    }
}

/// Read the health sidecar beside `log` and say whether the broker that wrote it still
/// serves the log (the module docs say how it is told). `Ok(None)` when there is none —
/// no broker that keeps one has served this log.
///
/// # Errors
///
/// The file could not be read, or it is not a snapshot this build reads
/// (`InvalidData`, naming the file): another format, or one a power loss cut short.
pub fn read_health(log: impl AsRef<Path>) -> io::Result<Option<HealthRead>> {
    let path = health_path(log.as_ref());
    let mut replaced = None;
    for _ in 0..READ_ATTEMPTS {
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut text = String::new();
        Read::by_ref(&mut file)
            .take(READ_MAX)
            .read_to_string(&mut text)?;
        let Some(health) = BrokerHealth::parse(&text) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{}: not a broker health snapshot this build reads (first line {:?})",
                    path.display(),
                    text.lines().next().unwrap_or("")
                ),
            ));
        };
        // The probe is taken on the very handle the snapshot was read through.
        match probe_writer(&file, &path)? {
            Some(writer) => return Ok(Some(HealthRead { health, writer })),
            None => replaced = Some(health),
        }
    }
    // Replaced under every look: a writer is alive and busy, but which snapshot it
    // holds this reader cannot say.
    match replaced {
        Some(health) => Ok(Some(HealthRead {
            health,
            writer: HealthWriter::Unknown,
        })),
        None => Ok(None),
    }
}

/// Probe the writer of `file`, the snapshot read at `path`: `Some(writer)` when the
/// probe could tell, `None` when the file was replaced at the name in between (read
/// again).
#[cfg(unix)]
fn probe_writer(file: &File, path: &Path) -> io::Result<Option<HealthWriter>> {
    use std::os::unix::fs::MetadataExt;
    match file.try_lock_shared() {
        Err(std::fs::TryLockError::WouldBlock) => Ok(Some(HealthWriter::Serving)),
        Err(std::fs::TryLockError::Error(e)) if e.kind() == io::ErrorKind::Unsupported => {
            Ok(Some(HealthWriter::Unknown))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e),
        Ok(()) => {
            let _ = file.unlock();
            // Granted: nobody holds THIS file. Gone only if it is still the one at the
            // name — a writer that replaced it between the open and the probe let it go
            // on purpose, and holds the new one.
            let held = file.metadata()?;
            match std::fs::metadata(path) {
                Ok(now) if now.dev() == held.dev() && now.ino() == held.ino() => {
                    Ok(Some(HealthWriter::Gone))
                }
                Ok(_) => Ok(None),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        }
    }
}

/// No advisory-lock probe off Unix: the writer takes no lock there (a Windows lock is
/// mandatory and would refuse the reader its read), so who wrote it cannot be told.
#[cfg(not(unix))]
fn probe_writer(_file: &File, _path: &Path) -> io::Result<Option<HealthWriter>> {
    Ok(Some(HealthWriter::Unknown))
}

/// The writer's side of `<log>.health`: the file it last published, still held under
/// its lock. One per broker that keeps the sidecar, behind the broker's own mutex.
pub(crate) struct HealthFile {
    path: PathBuf,
    tmp: PathBuf,
    /// The published file, locked: what tells a reader this broker still serves.
    held: Option<File>,
}

impl HealthFile {
    pub(crate) fn new(log: &Path) -> HealthFile {
        let path = health_path(log);
        let mut tmp = path.clone();
        tmp.as_mut_os_string().push(".tmp");
        HealthFile {
            path,
            tmp,
            held: None,
        }
    }

    /// Publish `health`: written whole to a fresh temp beside the sidecar (0600, never
    /// through a planted link), locked, renamed over the sidecar; only then is the file
    /// it replaces let go. On an error the temp is removed and the published file, and
    /// its lock, stay as they were.
    pub(crate) fn write(&mut self, health: &BrokerHealth) -> io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = crate::store::create_fresh(&self.tmp, &mut opts)?;
        let res = lock_fresh(&file)
            .and_then(|()| file.write_all(health.render().as_bytes()))
            .and_then(|()| crate::store::rename_path(&self.tmp, &self.path));
        match res {
            Ok(()) => {
                if let Some(old) = self.held.replace(file) {
                    release(old);
                }
                Ok(())
            }
            Err(e) => {
                drop(file);
                let _ = crate::store::remove_file_path(&self.tmp);
                Err(e)
            }
        }
    }

    /// Let go of the published file: from now on a reader finds its writer
    /// [`HealthWriter::Gone`]. Called before the log's own lock is released, so no
    /// reader is ever told a broker serves a log it has closed.
    pub(crate) fn release(&mut self) {
        if let Some(old) = self.held.take() {
            release(old);
        }
    }
}

/// Lock a fresh file exclusively, for as long as it is published. A filesystem without
/// advisory locks is tolerated (the reader then reads [`HealthWriter::Unknown`]); off
/// Unix nothing is locked (see [`probe_writer`]).
#[cfg(unix)]
fn lock_fresh(file: &File) -> io::Result<()> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(std::fs::TryLockError::Error(e)) if e.kind() == io::ErrorKind::Unsupported => Ok(()),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
        // Nobody else has this inode yet: created fresh, and not yet at the name.
        Err(std::fs::TryLockError::WouldBlock) => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "a fresh health file was already locked",
        )),
    }
}

#[cfg(not(unix))]
fn lock_fresh(_file: &File) -> io::Result<()> {
    Ok(())
}

/// Unlock explicitly before the close: `close(2)` releases a `flock(2)` only when the
/// open file description's last reference goes, and a `fork` in another thread holds
/// one until its `execve` (the log's own lock is released the same way).
fn release(file: File) {
    let _ = file.unlock();
    drop(file);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BrokerHealth {
        BrokerHealth {
            pid: 4242,
            written_at_ms: 1_760_000_000_000,
            identity: LogIdentity {
                id: [0xab; 16],
                generation: 3,
            },
            served: ServedInfo {
                requested: Tier::Ordered,
                served: Tier::Strict,
                reason: ServedReason::LatchedAfterFlushFailure,
                horizon_ms: Some(1000),
                rules: 2,
            },
            base: 1,
            head: 9,
            visible_head: 8,
            durable_head: 7,
            retained_records: 8,
            retained_bytes: 812,
            distinct_subjects: 4,
            bound_producers: 2,
            pending_wills: 1,
            group_commits: 3,
            replica: false,
            active_conns: 5,
            parked_tails: 2,
            flushes: FlushCounts {
                barrier: 6,
                full: 1,
                skipped: 2,
                failed: 1,
            },
            poisoned: Some("a flush failed (EIO)\nwith records".to_string()),
        }
    }

    /// The file round-trips every fact, the poison's reason on one line; a healthy log
    /// writes `poisoned=no` and no reason. A later build's extra key is skipped. NEGATIVE
    /// CONTROLS: another first line, a missing key, a value that does not parse, a
    /// poison flag without its reason, and a reason without the flag are not snapshots.
    #[test]
    fn the_snapshot_round_trips_and_refuses_what_it_is_not() {
        let h = sample();
        let text = h.render();
        let mut want = h.clone();
        want.poisoned = Some("a flush failed (EIO) with records".to_string());
        assert_eq!(BrokerHealth::parse(&text), Some(want));
        let mut healthy = sample();
        healthy.poisoned = None;
        let ht = healthy.render();
        assert!(
            ht.contains("\npoisoned=no\n") && !ht.contains("\nreason="),
            "{ht}"
        );
        assert_eq!(BrokerHealth::parse(&ht), Some(healthy.clone()));
        assert_eq!(
            BrokerHealth::parse(&format!("{ht}later_key=7\n")),
            Some(healthy.clone()),
            "a later build's key is skipped"
        );

        assert_eq!(
            BrokerHealth::parse(&ht.replace(HEADER, "astream-broker-health 2")),
            None
        );
        let dropped: String = ht
            .lines()
            .filter(|l| !l.starts_with("parked_tails="))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(BrokerHealth::parse(&dropped), None, "a key missing");
        assert_eq!(
            BrokerHealth::parse(&ht.replace("records=8", "records=eight")),
            None
        );
        assert_eq!(
            BrokerHealth::parse(&ht.replace("poisoned=no", "poisoned=yes")),
            None,
            "the flag without its reason"
        );
        assert_eq!(
            BrokerHealth::parse(&format!("{ht}reason=x\n")),
            None,
            "a reason without the flag"
        );
        assert_eq!(BrokerHealth::parse(""), None);
    }

    /// The writer HOLDS what it publishes: while it does, the reader is told
    /// [`HealthWriter::Serving`]; once it lets go — the log closed, or the process
    /// died — the same file reads [`HealthWriter::Gone`]. A rewrite replaces the
    /// snapshot whole and still reads Serving (the lock follows the new file). No file
    /// at all reads `None`, and a file that is not a snapshot is an error naming it.
    /// The NEGATIVE CONTROL is the Gone reading: the same bytes, nobody holding them.
    #[cfg(unix)]
    #[test]
    fn the_reader_tells_a_serving_writer_from_a_gone_one() {
        let dir = crate::test_scratch::Scratch::new("health");
        let log = dir.path().join("bus.log");
        assert_eq!(read_health(&log).unwrap(), None, "no sidecar yet");

        let mut w = HealthFile::new(&log);
        let mut h = sample();
        w.write(&h).unwrap();
        let read = read_health(&log).unwrap().expect("a sidecar");
        assert_eq!(read.writer, HealthWriter::Serving);
        assert_eq!(read.health.pid, 4242);
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(health_path(&log))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "0600: the broker's user alone");
        }

        h.retained_records = 99;
        w.write(&h).unwrap();
        let read = read_health(&log).unwrap().expect("a sidecar");
        assert_eq!(
            (read.writer, read.health.retained_records),
            (HealthWriter::Serving, 99)
        );
        assert!(
            !std::path::Path::new(&w.tmp).exists(),
            "the temp is renamed, never left"
        );

        w.release();
        let read = read_health(&log).unwrap().expect("the last word stays");
        assert_eq!(read.writer, HealthWriter::Gone);
        assert_eq!(read.health.retained_records, 99);

        std::fs::write(health_path(&log), "not a snapshot\n").unwrap();
        let err = read_health(&log).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("bus.log.health"), "{err}");
    }

    /// A link planted at the temp's name is not written through: the temp is created
    /// fresh, so the file the link names is untouched.
    #[cfg(unix)]
    #[test]
    fn a_planted_link_at_the_temp_is_not_written_through() {
        let dir = crate::test_scratch::Scratch::new("health-link");
        let log = dir.path().join("bus.log");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep").unwrap();
        let w = HealthFile::new(&log);
        std::os::unix::fs::symlink(&victim, &w.tmp).unwrap();
        let mut w = w;
        w.write(&sample()).unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(
            read_health(&log).unwrap().map(|r| r.writer),
            Some(HealthWriter::Serving)
        );
    }
}
