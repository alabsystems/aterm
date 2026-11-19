// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Release progress read by fleet surveys, with kernel process identity checked on the host.

use std::path::Path;

use aterm_ctl::release_holder::MAX_LEASE_S;

/// The local registry is data, never executable input. A dead holder's record is kept while its
/// lease may still run for a reader, so a peer that asks can tell its death from a failed fleet
/// read, and is removed once it cannot ([`Standing::DeadLapsed`], [`collect`]).
pub use aterm_ctl::release_holder::DIRECTORY;

const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;

/// The most bytes of rows [`survey`] returns, each row's newline counted: a quarter of the 256
/// KiB a fleet survey reply holds, so however many records the registry keeps, its rows leave
/// the reply room for its account, repository and session rows.
pub const SURVEY_BYTES_MAX: usize = 64 * 1024;

// The descriptor has already passed regular_file::open. The actual byte limit
// still applies if that same file grows after its admission metadata was read.
fn read_record(file: std::fs::File, observed_len: u64) -> Option<Vec<u8>> {
    use std::io::Read as _;
    if observed_len > MAX_RECORD_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_RECORD_BYTES).then_some(bytes)
}

/// Process identity, excluding pid reuse and a previous boot.
#[must_use]
pub fn identity(pid: u32) -> Option<(String, u64)> {
    let boot = aterm_uds::process::boot_id()?;
    let start = aterm_uds::process::read_process_start(i32::try_from(pid).ok()?)?;
    Some((boot, start.boot.0))
}

/// What this host's kernel establishes about one recorded holder identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Judged {
    /// The pid runs, in the recorded boot, with the recorded start: the very process.
    Alive,
    /// Positive kernel evidence that the identity runs nothing: another boot, no process with
    /// the pid (`ESRCH`), an exited one holding it, or another start in the same boot.
    Dead,
    /// A malformed identity, or a read the kernel did not answer (another user's process, a
    /// withheld boot id). Never evidence of death.
    Unknown,
}

impl Judged {
    /// Whether it runs: `None` where the kernel did not say.
    #[must_use]
    pub const fn alive(self) -> Option<bool> {
        match self {
            Self::Alive => Some(true),
            Self::Dead => Some(false),
            Self::Unknown => None,
        }
    }
}

/// Judge `(pid, boot, start)` against this host's kernel. A pid names at most one running
/// process, and a process keeps its boot and start for life, so any of: a different current
/// boot, an absent or exited pid, or a different start in the same boot, proves the recorded
/// process gone. Every failed or refused read is [`Judged::Unknown`].
#[must_use]
pub fn judge(pid: u32, boot: &str, start: u64) -> Judged {
    judge_with(
        pid,
        boot,
        start,
        &Kernel {
            boot_id: &aterm_uds::process::boot_id,
            presence: &aterm_uds::process::process_presence,
            start_of: &|pid| aterm_uds::process::read_process_start(pid).map(|read| read.boot.0),
        },
    )
}

/// The kernel reads [`judge`] combines: this boot's id, a pid's presence, and its process start.
struct Kernel<'a> {
    boot_id: &'a dyn Fn() -> Option<String>,
    presence: &'a dyn Fn(i32) -> aterm_uds::process::ProcessPresence,
    start_of: &'a dyn Fn(i32) -> Option<u64>,
}

fn judge_with(pid: u32, boot: &str, start: u64, kernel: &Kernel<'_>) -> Judged {
    use aterm_uds::process::ProcessPresence;
    let Some(pid) = i32::try_from(pid).ok().filter(|&pid| pid > 1) else {
        return Judged::Unknown;
    };
    match (kernel.boot_id)() {
        Some(now) if now != boot => return Judged::Dead,
        Some(_) => {}
        None => return Judged::Unknown,
    }
    match (kernel.presence)(pid) {
        ProcessPresence::Absent | ProcessPresence::Exited => Judged::Dead,
        ProcessPresence::Unknown => Judged::Unknown,
        ProcessPresence::Present => match (kernel.start_of)(pid) {
            Some(read) if read == start => Judged::Alive,
            Some(_) => Judged::Dead,
            None => Judged::Unknown,
        },
    }
}

/// The `release_holder` row answering a survey's `holder=`, judged now, echoing only the
/// identity asked ([`aterm_ctl::release_holder`]).
#[must_use]
pub fn holder_row(holder: &aterm_ctl::release_holder::Identity) -> String {
    holder.row(judge(holder.pid, &holder.boot, holder.start).alive())
}

/// Where one record of the registry stands at `now`, by this host's kernel and the lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Standing {
    /// Its holder is the very process running ([`Judged::Alive`]).
    Live,
    /// This kernel did not say whether its holder runs ([`Judged::Unknown`]): never evidence of
    /// death, so the record is kept.
    Unknown,
    /// Its holder is proven dead ([`Judged::Dead`]) and its lease may still run for a reader, who
    /// may then still ask this host about it.
    DeadLeased,
    /// Its holder is proven dead and its last renewal lies more than a whole lease
    /// ([`MAX_LEASE_S`]) before `now`, or that far after it: every cutter then takes the line
    /// without asking about this holder (`line_claim`'s `admit`), and a peer's `holder=` read is
    /// answered from the kernel alone ([`holder_row`]), so no reader needs the record.
    DeadLapsed,
}

impl Standing {
    /// The survey's `alive=` word for a record that stands so; `None` for one not surveyed.
    const fn word(self) -> Option<&'static str> {
        match self {
            Self::Live => Some("1"),
            Self::Unknown => Some("-"),
            Self::DeadLeased => Some("0"),
            Self::DeadLapsed => None,
        }
    }
}

/// Where `record`, a registry file last written at unix second `mtime`, stands at `now`: its
/// holder judged by this kernel ([`judge`]), and a proven death [`Standing::DeadLapsed`] once
/// its last renewal, the later of the record's `at` and `mtime`, lies more than [`MAX_LEASE_S`]
/// from `now` either way. The file's time stands where the record's `at` is missing or older:
/// its holder ran when it last wrote the record, whatever renewal the record names. A death with
/// neither time is [`Standing::DeadLeased`]. `None` where the record names no holder identity.
#[must_use]
pub fn standing(record: &aterm_json::Value, mtime: Option<u64>, now: u64) -> Option<Standing> {
    let holder = &record["holder"];
    let pid = u32::try_from(holder["pid"].as_u64()?).ok()?;
    let judged = judge(pid, holder["boot"].as_str()?, holder["start"].as_u64()?);
    let last = match (record["at"].as_u64(), mtime) {
        (Some(at), Some(mtime)) => Some(at.max(mtime)),
        (at, mtime) => at.or(mtime),
    };
    let lapsed = |last: u64| {
        now.saturating_sub(last) > MAX_LEASE_S || last.saturating_sub(now) > MAX_LEASE_S
    };
    Some(match judged {
        Judged::Alive => Standing::Live,
        Judged::Unknown => Standing::Unknown,
        Judged::Dead if last.is_some_and(lapsed) => Standing::DeadLapsed,
        Judged::Dead => Standing::DeadLeased,
    })
}

/// The registry file at `path` as admitted: its record, which file it was ([`Identity`]) and the
/// unix second it was last written. `None` for anything but a regular file within
/// [`MAX_RECORD_BYTES`] holding JSON (a link, folder or FIFO is refused unopened).
///
/// [`Identity`]: aterm_uds::line_log::Identity
fn read_at(
    path: &Path,
) -> Option<(
    aterm_json::Value,
    aterm_uds::line_log::Identity,
    Option<u64>,
)> {
    let (file, metadata) = aterm_uds::regular_file::open(path).ok()?;
    let identity = aterm_uds::line_log::identity_of(&file).ok()?;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|at| at.as_secs());
    let bytes = read_record(file, metadata.len())?;
    Some((aterm_json::from_slice(&bytes).ok()?, identity, mtime))
}

/// Bounded records for the survey's read-only fleet path, judged at `now` ([`standing`]): one
/// `release_line` row for each record but those of a [`Standing::DeadLapsed`] holder, live
/// holders first, then unknown, then dead, each newest renewal first, as many as fit in
/// [`SURVEY_BYTES_MAX`]. A failed identity read says unknown, never dead ([`judge`]).
#[must_use]
pub fn survey(state: &Path, now: u64) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(state.join(DIRECTORY)) else {
        return Vec::new();
    };
    let mut rows: Vec<(Standing, u64, String)> = entries
        .filter_map(|entry| {
            let (mut record, _, mtime) = read_at(&entry.ok()?.path())?;
            let standing = standing(&record, mtime, now)?;
            let alive = standing.word()?;
            let at = record["at"].as_u64().unwrap_or(0);
            // The ref retains every outcome. The fleet read needs identity and the latest
            // boundary only, and must fit the bounded survey reply even on a large test graph.
            record
                .as_object_mut()?
                .insert("rows".into(), aterm_json::json!([]));
            Some((
                standing,
                at,
                format!(
                    "release_line alive={alive} data={}",
                    aterm_codec::pct::encode(&record.to_string())
                ),
            ))
        })
        .collect();
    rows.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    let mut room = SURVEY_BYTES_MAX;
    rows.into_iter()
        .filter_map(|(_, _, row)| {
            let cost = row.len() + 1;
            (cost <= room).then(|| {
                room -= cost;
                row
            })
        })
        .collect()
}

/// The release pumps' `release_pump` rows ([`aterm_ctl::release_pump`]) of their records under
/// `state` (one per clone, in [`aterm_ctl::release_pump::DIRECTORY`]), at `now`, the newest
/// [`aterm_ctl::release_pump::MAX_RECORDS`] by their last look, newest first, each pid judged by
/// this host's kernel ([`judge`]; not said where the record names no boot and start). A file
/// that reads as no record answers no row: a link, folder or FIFO is refused unopened, and a
/// file past [`aterm_ctl::release_pump::MAX_RECORD_BYTES`] is no record.
#[must_use]
pub fn pump_rows(state: &Path, now: u64) -> Vec<String> {
    let dir = state.join(aterm_ctl::release_pump::DIRECTORY);
    if !std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut records: Vec<aterm_ctl::release_pump::Record> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
        .filter_map(|entry| pump_record(&entry.path()))
        .collect();
    records.sort_by(|a, b| b.at.cmp(&a.at).then(a.pid.cmp(&b.pid)));
    records.truncate(aterm_ctl::release_pump::MAX_RECORDS);
    records
        .iter()
        .map(|record| {
            let alive = match (&record.boot, record.start) {
                (Some(boot), Some(start)) => judge(record.pid, boot, start).alive(),
                _ => None,
            };
            record.row(alive, now)
        })
        .collect()
}

/// The pump record at `path`; `None` where it reads as none ([`pump_rows`]).
fn pump_record(path: &Path) -> Option<aterm_ctl::release_pump::Record> {
    use std::io::Read as _;
    let limit = aterm_ctl::release_pump::MAX_RECORD_BYTES;
    let (file, metadata) = aterm_uds::regular_file::open(path).ok()?;
    if metadata.len() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > limit {
        return None;
    }
    aterm_ctl::release_pump::Record::from_json(std::str::from_utf8(&bytes).ok()?)
}

/// Remove each record of the registry under `state` whose holder is [`Standing::DeadLapsed`] at
/// `now`, by this host's local writers alone (a cut taking its line, the window's sweep), never
/// on a path a fleet request reaches. Only a regular file named `*.json` that reads whole as a
/// record is removed (no folder, link, FIFO or unparsable or oversize file; a
/// `write_atomic_durable` stage is a dot-named folder), and only while its name still holds the
/// very file judged. How many it removed.
pub fn collect(state: &Path, now: u64) -> usize {
    let dir = state.join(DIRECTORY);
    if !std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return false;
            };
            if name.starts_with('.') || !name.ends_with(".json") {
                return false;
            }
            let path = entry.path();
            let Some((record, judged, mtime)) = read_at(&path) else {
                return false;
            };
            if standing(&record, mtime, now) != Some(Standing::DeadLapsed) {
                return false;
            }
            let same = aterm_uds::line_log::identity_at(&path).is_ok_and(|now| {
                (now.dev, now.ino, now.len) == (judged.dev, judged.ino, judged.len)
            });
            same && std::fs::remove_file(&path).is_ok()
        })
        .count()
}

#[cfg(all(
    test,
    any(target_vendor = "apple", target_os = "linux", target_os = "android")
))]
#[path = "release_progress_tests.rs"]
mod tests;
