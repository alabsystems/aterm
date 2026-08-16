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
        // No root-key env var, and there is no longer one to set: the anchor is the
        // compiled-in paper-master keyset (`aterm_update_core::pins::PAPER_MASTER_PUBKEYS`
        // via `atpkg::PKG_TRUST_ANCHORS`) and nothing ambient can supply or swap it.
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
    //
    // THE PROPERTY UNDER TEST IS TERMINATION, not the exit code, and that distinction
    // now matters: this tree ships with `PAPER_MASTER_PUBKEYS` empty, so the manager is
    // INERT and `update` refuses (nonzero) instead of no-op-ing (zero). Either way the
    // child must exit promptly — the GUI clears its busy state on the child exiting, and
    // a hang is what strands Settings. `run_with_deadline` panics if it does not, so
    // reaching this line IS the assertion; asserting a specific code here would only pin
    // whether the fleet happens to be armed.
    let update = run_with_deadline(&home, &config_home, &registry, &["update"]);
    assert!(
        update.code().is_some(),
        "the child exited on its own rather than being signalled"
    );

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

/// The conventional help spellings, at the real process edge: they exit 0 on the help
/// surface instead of the unknown-verb error path — `atpkg` rides PATH as an argv0
/// alias, so `--help` is the first thing a shell (or an AI) tries. (`sync`, once
/// asserted here as an update alias, is deleted; the unknown-verb hint's coherence
/// with the dispatch is pinned by cli.rs's own `verb_hint_matches_dispatch`.)
#[test]
fn help_spellings_dispatch_as_advertised() {
    let root = std::env::temp_dir().join(format!("atpkg-verb-dispatch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let config_home = root.join("config");
    let registry = root.join("registry");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&registry).unwrap();

    for help in [["help"], ["-h"], ["--help"]] {
        let status = run_with_deadline(&home, &config_home, &registry, &help);
        assert!(status.success(), "atpkg {} exits 0", help[0]);
    }

    let _ = std::fs::remove_dir_all(root);
}
