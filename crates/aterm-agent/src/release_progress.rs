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

    fn current_record(padding: usize) -> String {
        let pid = std::process::id();
        let (boot, start) = identity(pid).unwrap();
        aterm_json::json!({"holder":{"pid":pid,"boot":boot,"start":start},
            "rows":[],"padding":"x".repeat(padding)})
        .to_string()
    }

    #[test]
    fn survey_omits_oversize_valid_json_and_growth_after_admission() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let registry = dir.path().join(DIRECTORY);
        std::fs::create_dir(&registry).unwrap();
        let path = registry.join("line.json");
        let small = current_record(0);
        std::fs::write(&path, &small).unwrap();
        assert_eq!(survey(dir.path()).len(), 1);
        let (held, metadata) = aterm_uds::regular_file::open(&path).unwrap();
        let large = current_record(MAX_RECORD_BYTES as usize);
        assert!(aterm_json::from_str::<aterm_json::Value>(&large).is_ok());
        std::fs::write(&path, large).unwrap();
        assert!(read_record(held, metadata.len()).is_none());
        assert!(survey(dir.path()).is_empty());
        std::fs::write(&path, small).unwrap();
        assert_eq!(survey(dir.path()).len(), 1);
    }

    #[test]
    fn admitted_record_does_not_reopen_a_replaced_path() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let registry = dir.path().join(DIRECTORY);
        std::fs::create_dir(&registry).unwrap();
        let path = registry.join("line.json");
        let old = current_record(0);
        std::fs::write(&path, &old).unwrap();
        let (held, metadata) = aterm_uds::regular_file::open(&path).unwrap();
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, current_record(MAX_RECORD_BYTES as usize)).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert_eq!(read_record(held, metadata.len()).unwrap(), old.as_bytes());
        assert!(survey(dir.path()).is_empty());
    }

    #[test]
    fn survey_refuses_final_aliases_directories_and_fifos() {
        use std::os::unix::ffi::OsStrExt as _;
        if !aterm_uds::test_binary::isolated_lifecycle(
            "release_progress::tests::survey_refuses_final_aliases_directories_and_fifos",
        ) {
            return;
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        let registry = dir.path().join(DIRECTORY);
        std::fs::create_dir(&registry).unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, current_record(0)).unwrap();
        std::os::unix::fs::symlink(&target, registry.join("alias.json")).unwrap();
        std::fs::create_dir(registry.join("directory.json")).unwrap();
        let fifo = registry.join("fifo.json");
        let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: the C string is terminated and live; this creates only a FIFO
        // in the test's owned directory, with no reader or writer to wait for.
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
        assert!(survey(dir.path()).is_empty());
        std::fs::write(registry.join("valid.json"), current_record(0)).unwrap();
        assert_eq!(survey(dir.path()).len(), 1);
    }
}
