// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::{
    fs::File,
    os::fd::{AsRawFd as _, FromRawFd as _},
    os::unix::fs::MetadataExt as _,
};

const ROLE: &str = "ATERM_ORIGINAL_ROOT_STORE_CAPTURE_TEST";
const EXACT: &str = "conn::resource_run::early::tests::original_store_startup_capture_survives_number_reuse_and_rejects_late_adoption";

#[test]
fn original_store_startup_capture_survives_number_reuse_and_rejects_late_adoption() {
    if std::env::var_os(ROLE).is_some() {
        return child();
    }
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args(["--exact", EXACT, "--nocapture"])
        .env(ROLE, "1");
    let output =
        aterm_uds::deadline::output_within(&mut command, std::time::Duration::from_secs(60))
            .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("original-startup-store-captured"));
}

fn child() {
    unsafe extern "C" {
        fn dup2(old: i32, new: i32) -> i32;
    }
    let root = crate::test_scratch_dir("original-store-startup");
    let foreign_root = crate::test_scratch_dir("foreign-store-startup");
    let mut original = File::open(root.path()).unwrap();
    let mut lower = Vec::new();
    while original.as_raw_fd() < 6 {
        lower.push(original);
        original = File::open(root.path()).unwrap();
    }
    let source = original.as_raw_fd();
    let original_id = original.metadata().unwrap().ino();
    let foreign = File::open(foreign_root.path()).unwrap();
    let arguments = [
        "resource-run",
        "--accounted",
        "--cpu-units",
        "1",
        "--memory-bytes",
        "1",
        "--timeout-ms",
        "1000",
        "--job",
        "workload",
        "--expect-original-store-fd",
        &source.to_string(),
        "--",
        "targo",
        "--resource-workload-v1",
    ]
    .map(OsString::from)
    .to_vec();
    // SAFETY: original is held, with no callback or concurrent closer.
    let captured = unsafe { capture_original_root_store(&arguments) }.unwrap();
    // SAFETY: the same original source is still owned and held here.
    let for_changed_arguments = unsafe { capture_original_root_store(&arguments) }.unwrap();
    let mut changed = arguments[1..].to_vec();
    let option = changed
        .iter()
        .position(|word| word == "--expect-original-store-fd")
        .unwrap();
    changed.drain(option..option + 2);
    assert!(parse_with_original_store(changed, for_changed_arguments).is_err());
    // Simulate startup code replacing the raw inherited number before parse.
    // SAFETY: both FDs are owned here; replacing original changes no other owner.
    assert_eq!(unsafe { dup2(foreign.as_raw_fd(), source) }, source);
    assert_ne!(original.metadata().unwrap().ino(), original_id);
    let parsed = parse_with_original_store(arguments[1..].to_vec(), captured)
        .unwrap()
        .unwrap();
    assert_eq!(
        parsed.expected_store.unwrap().metadata().unwrap().ino(),
        original_id
    );
    assert!(parse_with_original_store(arguments[1..].to_vec(), None).is_err());

    // A missing original at entry is permanently unavailable even when later
    // callbacks open a directory at exactly that raw number.
    drop(original);
    // SAFETY: this isolated child has no closer or FD-opening callback here.
    assert!(unsafe { capture_original_root_store(&arguments) }.is_err());
    // SAFETY: source was closed above and foreign remains held.
    assert_eq!(unsafe { dup2(foreign.as_raw_fd(), source) }, source);
    // SAFETY: dup2 just created this otherwise-unowned number.
    let replacement = unsafe { File::from_raw_fd(source) };
    assert!(replacement.metadata().unwrap().is_dir());
    assert!(parse_with_original_store(arguments[1..].to_vec(), None).is_err());
    println!("original-startup-store-captured");
}
