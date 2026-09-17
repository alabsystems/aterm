// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The rustup VIEW through the untracked lane (`atpkg::seam`), against the REAL
//! `atpkg` binary this build produced: the view's `bin/` is one copy-on-write CLONE per
//! tool of the live trust build. Every file a TRACKED process creates carries
//! com.apple.provenance (`crate::provenance`, measured 2026-09-12), and a toolchain run
//! from tagged files tags what it writes. The view used to be hard links, and the 0.86.0
//! app built it in-process on every pass, re-tagging the clean `trust` bundle ITSELF (all
//! 21 executables, 2026-09-15). A clone never writes the store — but the clone's own files
//! would be tagged if this process laid them. So the view is built by a launchd job, like
//! the bundle is staged and the shims are laid, and this test proves both properties:
//! the store's files stay CLEAN whoever lays the view, and the VIEW's files stay clean
//! when the lane lays them, while an in-process refresh from this same process tags the
//! view (never the store) whenever this process is tracked. Under an untracked shell every
//! half is clean and the test says which case it exercised — the tag cannot be minted by
//! hand.
//!
//! macOS only: the tag and launchd exist nowhere else.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};

use atpkg::lay::{Lane, TrackedPolicy};
use atpkg::provenance::{carries_provenance, measure_tracked};
use atpkg::seam::{refresh_view_with, view_dir};
use atpkg::store::Layout;

fn scratch(label: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "atpkg-untracked-view-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn helper_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_atpkg"))
}

/// A store with one trust build — two Trust-named tools, a `lib/` — and
/// `store/trust/current` pointing at it, the shape `refresh_view` reads. The FILES are
/// laid by a launchd job (`launchd_write`): written by this process they would carry
/// the tag from birth whenever it is tracked, and the property under test is whether
/// the LANE adds one.
fn fake_store(prefix: &Path, build: u64, scratch: &Path) -> PathBuf {
    let dir = prefix.join("store").join("trust").join(build.to_string());
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    launchd_write(
        &dir.join("lib").join("libtrust.dylib"),
        "driver",
        false,
        scratch,
    );
    for tool in ["trustc", "targo"] {
        launchd_write(
            &dir.join("bin").join(tool),
            &format!("#!/bin/sh\necho {tool}\n"),
            true,
            scratch,
        );
    }
    std::os::unix::fs::symlink(&dir, prefix.join("store").join("trust").join("current")).unwrap();
    dir
}

/// Write `content` at `path` from a launchd-spawned `/bin/sh` — an untracked process,
/// so the file is clean whatever this process is (the same lane the store's own
/// `untracked_stage` test uses to mint a clean copy).
fn launchd_write(path: &Path, content: &str, executable: bool, scratch: &Path) {
    let done = scratch.join(format!(
        "write.{}.done",
        path.file_name().unwrap().to_string_lossy()
    ));
    let label = format!(
        "systems.alab.atpkg.test.view-write.{}.{}",
        std::process::id(),
        path.file_name().unwrap().to_string_lossy()
    );
    let mode = if executable { "755" } else { "644" };
    let out = std::process::Command::new("/bin/launchctl")
        .args(["submit", "-l", &label, "--", "/bin/sh", "-c"])
        .arg("printf '%s' \"$1\" > \"$2\" && chmod \"$3\" \"$2\"; : > \"$4\"")
        .arg("x")
        .arg(content)
        .arg(path)
        .arg(mode)
        .arg(&done)
        .output()
        .expect("launchctl runs");
    assert!(
        out.status.success(),
        "launchctl submit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let started = std::time::Instant::now();
    while !done.exists() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the launchd write of {} did not finish",
            path.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = std::process::Command::new("/bin/launchctl")
        .args(["remove", &label])
        .output();
    assert!(
        !carries_provenance(path),
        "PRECONDITION: a file a launchd job wrote is clean: {}",
        path.display()
    );
}

fn identity(path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt as _;
    let m = std::fs::metadata(path).unwrap();
    (m.dev(), m.ino())
}

/// The bytes of `path`.
fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

/// The lane builds the same view the in-process body builds — clones of the store's
/// files, the stock names beside them — and neither the store nor the view is tagged; the
/// in-process control from this process tags the VIEW whenever this process is tracked,
/// and never the store.
#[test]
fn the_view_lane_clones_the_store_without_tagging_either() {
    let d = scratch("lane");
    let layout = Layout {
        prefix: d.join("prefix"),
    };
    std::fs::create_dir_all(&layout.prefix).unwrap();
    let build = fake_store(&layout.prefix, 8595, &d);
    let tracked = measure_tracked(&d).unwrap_or(false);

    let refreshed = refresh_view_with(
        &layout,
        "trust",
        true,
        &Lane::Helper(helper_exe()),
        TrackedPolicy::Refuse,
    )
    .unwrap_or_else(|e| panic!("the view lane must run on this macOS: {e}"));
    assert_eq!(refreshed.build, build);
    assert_eq!(refreshed.tools, 2);
    assert!(
        refreshed.stock.contains(&("rustc", "trustc")),
        "the stock name is laid beside its Trust tool: {:?}",
        refreshed.stock
    );
    assert!(refreshed.changed, "a first build is a change");
    let bin = view_dir(&layout, "trust").join("bin");
    let store_trustc = build.join("bin").join("trustc");
    assert_ne!(
        identity(&bin.join("trustc")),
        identity(&store_trustc),
        "a clone is its own inode, never the store's"
    );
    assert_eq!(bytes(&bin.join("trustc")), bytes(&store_trustc));
    assert_eq!(
        bytes(&bin.join("rustc")),
        bytes(&store_trustc),
        "the stock name holds the Trust tool's bytes"
    );
    assert!(
        !carries_provenance(&store_trustc),
        "the lane does not tag the store"
    );
    assert!(
        !carries_provenance(&build.join("lib").join("libtrust.dylib")),
        "nor the store's lib/"
    );
    assert!(
        !carries_provenance(&bin.join("trustc")) && !carries_provenance(&bin.join("rustc")),
        "and the lane's clones are clean"
    );
    assert!(
        !carries_provenance(
            &view_dir(&layout, "trust")
                .join("lib")
                .join("libtrust.dylib")
        ),
        "including the view's lib/"
    );
    // A rebuild that produces the same set is silent.
    let again = refresh_view_with(
        &layout,
        "trust",
        true,
        &Lane::Helper(helper_exe()),
        TrackedPolicy::Refuse,
    )
    .unwrap();
    assert!(!again.changed, "the same set again is not a change");

    // THE CONTROL: the in-process body, from THIS process.
    let c = scratch("in-process");
    let control = Layout {
        prefix: c.join("prefix"),
    };
    std::fs::create_dir_all(&control.prefix).unwrap();
    let cbuild = fake_store(&control.prefix, 8595, &c);
    refresh_view_with(
        &control,
        "trust",
        true,
        &Lane::Unavailable(String::from("a test harness")),
        TrackedPolicy::Allow,
    )
    .unwrap();
    assert!(
        !carries_provenance(&cbuild.join("bin").join("trustc")),
        "an in-process CLONE never writes the store, tracked or not — the hard link 0.86.0 \
         shipped did"
    );
    let control_view_tagged =
        carries_provenance(&view_dir(&control, "trust").join("bin").join("trustc"));
    if tracked {
        assert!(
            control_view_tagged,
            "the clones a tracked process lays are tagged — why the lane lays them"
        );
    }
    eprintln!(
        "test process tracked={tracked}: the lane left the store and its clones clean; the \
         in-process control left the store clean and tagged its clones={control_view_tagged}"
    );
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&c);
}
