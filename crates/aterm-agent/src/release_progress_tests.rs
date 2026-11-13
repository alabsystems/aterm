// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
