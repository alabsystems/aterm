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

/// Bounded records for the survey's read-only fleet path. A failed identity read says
/// unknown, never dead; boot or start mismatch is positive evidence of a dead identity.
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
            let alive = match identity(pid) {
                Some(current) => {
                    if current == (boot.to_owned(), start) {
                        "1"
                    } else {
                        "0"
                    }
                }
                None if !aterm_uds::process::pid_alive(pid)
                    || aterm_uds::process::boot_id().is_some_and(|current| current != boot) =>
                {
                    "0"
                }
                None => "-",
            };
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
