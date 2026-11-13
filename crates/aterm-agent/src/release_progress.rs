// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Release progress read by fleet surveys, with kernel process identity checked on the host.

use std::path::Path;

/// The local registry is data, never executable input. A dead record is retained so a peer
/// can distinguish death from a failed fleet read.
pub const DIRECTORY: &str = "release-lines";

const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;

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
    /// The survey's `alive=` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Alive => "1",
            Self::Dead => "0",
            Self::Unknown => "-",
        }
    }

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

/// Bounded records for the survey's read-only fleet path. A failed identity read says
/// unknown, never dead ([`judge`]).
#[must_use]
pub fn survey(state: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(state.join(DIRECTORY)) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let (file, metadata) = aterm_uds::regular_file::open(&path).ok()?;
            let bytes = read_record(file, metadata.len())?;
            let mut record: aterm_json::Value = aterm_json::from_slice(&bytes).ok()?;
            let pid = u32::try_from(record["holder"]["pid"].as_u64()?).ok()?;
            let boot = record["holder"]["boot"].as_str()?;
            let start = record["holder"]["start"].as_u64()?;
            let alive = judge(pid, boot, start).word();
            // The ref retains every outcome. The fleet read needs identity and the latest
            // boundary only, and must fit the bounded survey reply even on a large test graph.
            record
                .as_object_mut()?
                .insert("rows".into(), aterm_json::json!([]));
            Some(format!(
                "release_line alive={alive} data={}",
                aterm_codec::pct::encode(&record.to_string())
            ))
        })
        .collect()
}

#[cfg(all(
    test,
    any(target_vendor = "apple", target_os = "linux", target_os = "android")
))]
#[path = "release_progress_tests.rs"]
mod tests;
