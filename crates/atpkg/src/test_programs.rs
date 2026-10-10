// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Test programs the unit tests exec: one file per distinct body, laid once and run once
//! before any test's bound measures it — shared by `reroute`'s and `upstream_identity`'s
//! tests, which both put fake `cargo`/`rustc`/`rustup` programs on a PATH.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// THE PROGRAM WHOSE BODY IS `body`: one file per distinct body, shared by every test
/// and every run of the suite, and executed once per process, unbounded — never a
/// fresh file per test.
///
/// The root cause of `reroute`'s tests' load flake (4–8 of 8 runs under load): a file this
/// process writes carries `com.apple.provenance`, and its FIRST `exec` waits on
/// `syspolicyd`'s assessment — measured on m7 2026-09-24, 0.4 s idle, 13 s with the
/// assessor busy and more than 60 s under a heavy build, at 0% CPU — while any later
/// exec of the SAME file, through any symlink to it, costs nothing (the verdict is
/// cached on the file). Every test wrote its fakes afresh, so every run paid one
/// assessment per fake inside a 60 s bound. Now a program is content-addressed beside the
/// test binary ([`programs_dir`]) and laid by rename, so one written by an earlier run is
/// reused as it stands and a new one is assessed here, where no bound measures it; a test
/// places SYMLINKS to it.
///
/// ONE LAYER AT A TIME. Every test thread of the binary lays through the same temporary
/// name, so the lock is held from the check to the first run: measured 2026-09-30 when
/// `upstream_identity`'s copy of this helper laid without it, two tests laying the same
/// body from a cold directory renamed one temporary file from under each other — `NotFound`
/// at the `set_permissions` or the `rename` — in 21 of 30 cold runs.
pub(crate) fn assessed(name: &str, body: &str) -> PathBuf {
    use std::hash::{Hash as _, Hasher as _};
    use std::os::unix::fs::PermissionsExt as _;
    static RAN: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    let mut hasher = std::hash::DefaultHasher::new();
    body.hash(&mut hasher);
    let path = programs_dir().join(format!("{name}-{:016x}", hasher.finish()));
    let mut ran = RAN.lock().unwrap_or_else(|e| e.into_inner());
    if ran.contains(&path) {
        return path;
    }
    if std::fs::read(&path).ok().as_deref() != Some(body.as_bytes()) {
        let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
        std::fs::write(&tmp, body).unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
    }
    // The assessment, paid here and not inside a test's bound (free when an earlier
    // run's file is reused).
    let status = std::process::Command::new(&path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    assert!(
        status.is_ok(),
        "the test program {} did not run: {status:?}",
        path.display()
    );
    ran.push(path.clone());
    path
}

/// Where the [`assessed`] programs live: `target/<profile>/atpkg-reroute-programs`,
/// beside this test binary's `deps/` — shared by every run of this checkout's suite and
/// by no other user's. A fixed name under the temp dir is that only where `$TMPDIR` is
/// per-user (macOS); on Linux it is the shared `/tmp`, where a second user's run could
/// not write it and a directory someone else made first would be exec'd from.
pub(crate) fn programs_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("a test binary lives in target/<profile>/deps")
        .join("atpkg-reroute-programs");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
