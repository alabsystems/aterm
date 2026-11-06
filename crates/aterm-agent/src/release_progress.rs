// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Release progress read by fleet surveys, with kernel process identity checked on the host.

use std::path::Path;

/// The local registry is data, never executable input. A dead record is retained so a peer
/// can distinguish death from a failed fleet read.
pub const DIRECTORY: &str = "release-lines";

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
            let metadata = std::fs::symlink_metadata(&path).ok()?;
            if !metadata.is_file() || metadata.len() > 4 * 1024 * 1024 {
                return None;
            }
            let bytes = std::fs::read(path).ok()?;
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
mod tests {
    use super::*;

    #[test]
    fn survey_checks_kernel_identity_instead_of_trusting_a_recent_record() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let registry = dir.path().join(DIRECTORY);
        std::fs::create_dir(&registry).unwrap();
        let pid = std::process::id();
        let (boot, start) = identity(pid).unwrap();
        let path = registry.join("0.156.0.json");
        let record = |start| aterm_json::json!({"holder": {"pid": pid, "boot": boot, "start": start}, "rows": ["red"]});
        std::fs::write(&path, record(start).to_string()).unwrap();
        assert!(survey(dir.path())[0].starts_with("release_line alive=1 "));
        // Negative control: the same pid and a recent row cannot revive another start.
        std::fs::write(&path, record(start.wrapping_add(1)).to_string()).unwrap();
        assert!(survey(dir.path())[0].starts_with("release_line alive=0 "));
    }
}
