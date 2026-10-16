// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Fake programs (`cargo`/`rustc`/`rustup`) that `reroute`'s and `upstream_identity`'s
//! tests put on a PATH, laid and run once before any test's bound measures them.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The program whose body is `body`: one content-addressed file per distinct body, shared
/// by every test and run, and executed once per process outside any test's bound; tests
/// place symlinks to it.
///
/// On macOS a file this process writes carries `com.apple.provenance`, and its first
/// `exec` waits on `syspolicyd` (0.4 s idle, over 60 s under a heavy build); later execs
/// of the same file, through any symlink, are free.
///
/// The lock is held from the check to the first run: every test thread lays through the
/// same temporary name, and two unlocked layers rename it from under each other.
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
    // Pay the first-exec assessment here, outside any test's bound.
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

/// Where the [`assessed`] programs live: `target/<profile>/atpkg-reroute-programs`, shared
/// by every run of this checkout's suite and by no other user's (a fixed name under
/// `/tmp` is shared across users on Linux). Canonical, because `target` may be a link.
pub(crate) fn programs_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("a test binary lives in target/<profile>/deps")
        .join("atpkg-reroute-programs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}
