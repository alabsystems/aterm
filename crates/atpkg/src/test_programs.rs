// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Content-addressed fake toolchain binaries pre-assessed once per test process.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Materializes `body` as an executable file and runs it once to pay first-exec assessment.
///
/// Off Apple a child process writes it (`aterm_tempfile::written_by_a_child`): Linux refuses to
/// exec a file anything holds open for writing (`ETXTBSY`), and a child another test thread
/// spawns while this process holds the file open keeps that writer until its own exec.
pub(crate) fn assessed(name: &str, body: &str) -> PathBuf {
    use std::hash::{Hash as _, Hasher as _};
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
        let _ = std::fs::remove_file(&tmp);
        #[cfg(not(target_vendor = "apple"))]
        aterm_tempfile::written_by_a_child(&tmp, body.as_bytes(), 0o755).unwrap();
        #[cfg(target_vendor = "apple")]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::write(&tmp, body).unwrap();
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::rename(&tmp, &path).unwrap();
    }
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

/// Canonical `target/<profile>/atpkg-reroute-programs` directory for [`assessed`] binaries.
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
