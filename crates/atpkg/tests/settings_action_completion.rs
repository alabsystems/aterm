// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Process-level regression for the two native Settings Packages actions.
//!
//! The GUI waits for the co-located atpkg child before clearing its busy state.
//! Plant writerless FIFOs at the action-reachable config, floor, and offline
//! registry manifest paths, then prove both real child verbs terminate under a
//! deadline. Unit tests cover the other local metadata seams independently.

#![cfg(unix)]

use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

fn make_fifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt as _;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `path` is a live NUL-terminated pathname in this test's private
    // fixture, and `mkfifo` retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
}

fn run_with_deadline(
    home: &Path,
    config_home: &Path,
    registry: &Path,
    args: &[&str],
) -> ExitStatus {
    let mut child = Command::new(env!("CARGO_BIN_EXE_atpkg"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", config_home)
        .env("ATPKG_ROOTKEY_OVERRIDE", "invalid-but-nonempty-test-anchor")
        .env("ATPKG_REGISTRY", format!("dir:{}", registry.display()))
        .env_remove("ATPKG_DISABLE")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn dev atpkg child");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().expect("poll atpkg child") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "atpkg {} did not complete before the deadline",
                args.join(" ")
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn native_settings_package_actions_complete_with_hostile_metadata_files() {
    let root = std::env::temp_dir().join(format!("atpkg-settings-actions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let config_home = root.join("config");
    let registry = root.join("registry");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&registry).unwrap();

    // Both actions consume this same config through config::cached().
    make_fifo(&config_home.join("aterm/aterm.toml"));

    // Check & Update short-circuits cleanly on an empty store after the config
    // admission returns defaults; it must not wait for a FIFO writer.
    let update = run_with_deadline(&home, &config_home, &registry, &["update"]);
    assert!(update.success(), "empty-store update is a successful no-op");

    // Install ALab Toolset reaches the floor and dir-registry index. Both are
    // FIFOs here; the child should reject them and exit nonzero, never remain
    // alive and strand Settings in its busy state.
    let prefix = home
        .join("Library")
        .join("Application Support")
        .join("aterm")
        .join("pkg");
    make_fifo(&prefix.join("index_build.floor"));
    make_fifo(&registry.join("index.toml"));
    std::fs::write(registry.join("index.toml.sig"), b"invalid").unwrap();
    let install = run_with_deadline(
        &home,
        &config_home,
        &registry,
        &["install", "--default-set"],
    );
    assert!(!install.success(), "hostile registry input fails closed");

    let _ = std::fs::remove_dir_all(root);
}
