// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Resolves the `aterm` executable launched by integration test fixtures.

use std::path::{Path, PathBuf};

/// Returns the canonical path of the executable named by `ATERM_TEST_EXECUTABLE`, or of this
/// test's snapshot of this package's `aterm` ([`snapshot`]) when unset.
///
/// Canonical because aterm names itself by its physical path (`current_exe`, `Image::at`), and a
/// release clone reaches its target through a symlink (`target -> target.noindex`): a test that
/// compared a spelling through the link failed only in a release's verify.
pub fn aterm_executable() -> PathBuf {
    let Some(selected) = std::env::var_os("ATERM_TEST_EXECUTABLE") else {
        static SNAPSHOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        return SNAPSHOT
            .get_or_init(|| {
                let built = aterm_tempfile::bin_exe("aterm");
                let built = std::fs::canonicalize(&built)
                    .unwrap_or_else(|error| panic!("the package's aterm {built:?}: {error}"));
                snapshot(&built)
            })
            .clone();
    };
    let selected = PathBuf::from(selected);
    assert!(
        selected.is_absolute(),
        "ATERM_TEST_EXECUTABLE must name an absolute owned artifact: {selected:?}"
    );
    let metadata = std::fs::metadata(&selected)
        .unwrap_or_else(|error| panic!("ATERM_TEST_EXECUTABLE {selected:?}: {error}"));
    assert!(
        metadata.is_file(),
        "ATERM_TEST_EXECUTABLE must name a regular file: {selected:?}"
    );
    std::fs::canonicalize(&selected)
        .unwrap_or_else(|error| panic!("ATERM_TEST_EXECUTABLE {selected:?}: {error}"))
}

/// `built`, copied once into a folder beside it (`.ubd/aterm-under-test.noindex/<content>/`,
/// named by its length and its modification time), canonical. Every instance a test starts runs
/// that copy, whose name nothing writes again, and every process of the test (a stand-in the test
/// binary runs as included) finds the same copy.
///
/// `built` is Cargo's uplifted copy of the binary (`<target>/debug/aterm`), which every Cargo
/// command on the target directory writes again as a new file, even when the binary is fresh. An
/// instance started from it then runs an image its own path no longer names: aterm's process
/// observation (`native_act::Process::observe`) rightly refuses an image whose path names another
/// file, so its harness cannot vouch for itself as a native operation's actor and refuses the
/// operation, and its relaunch helper (`<path> harness native-launch`) runs another build. A
/// parallel suite's Cargo command did exactly that under a running fabric_world instance.
///
/// A copy is published whole (made under a private name, then linked to its own name, which a
/// second publisher's link leaves as it is) and never replaced. Each use marks its folder; a
/// folder no use has marked for a day is removed.
fn snapshot(built: &Path) -> PathBuf {
    use std::time::{Duration, Instant, SystemTime};
    const UNUSED: Duration = Duration::from_secs(24 * 60 * 60);
    let root = built
        .parent()
        .expect("the binary's folder")
        .join(".ubd")
        .join("aterm-under-test.noindex");
    let root = root.as_path();
    std::fs::create_dir_all(root).unwrap_or_else(|error| panic!("{root:?}: {error}"));
    let name = built.file_name().expect("the binary's file name");
    let deadline = Instant::now() + Duration::from_secs(60);
    // A copy taken while Cargo writes `built` again is refused by the stamp check: the same
    // file, unchanged, before and after, and the copy as long as it.
    let (folder, copy) = loop {
        let before = stamp(built);
        if let Some((_, _, len, Some(modified))) = before {
            let folder = root.join(content_key(len, modified));
            let copy = folder.join(name);
            if stamp(&copy).is_some_and(|published| published.2 == len) {
                break (folder, copy);
            }
            std::fs::create_dir_all(&folder).unwrap_or_else(|error| panic!("{folder:?}: {error}"));
            let private = folder.join(format!(
                ".{}.{}",
                std::process::id(),
                name.to_string_lossy()
            ));
            let _ = std::fs::remove_file(&private);
            let copied = std::fs::copy(built, &private).ok();
            if before == stamp(built) && copied == Some(len) {
                match std::fs::hard_link(&private, &copy) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("publish {copy:?}: {error}"),
                }
            }
            let _ = std::fs::remove_file(&private);
            if stamp(&copy).is_some_and(|published| published.2 == len) {
                break (folder, copy);
            }
        }
        assert!(
            Instant::now() < deadline,
            "{built:?} did not hold still for one copy within a minute"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let used = folder.join(".used");
    let marked = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&used)
        .and_then(|file| file.set_modified(SystemTime::now()));
    marked.unwrap_or_else(|error| panic!("mark {used:?}: {error}"));
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let other = entry.path();
        let last_use = std::fs::metadata(other.join(".used"))
            .or_else(|_| entry.metadata())
            .and_then(|meta| meta.modified());
        if other != folder && last_use.is_ok_and(|at| at.elapsed().is_ok_and(|age| age > UNUSED)) {
            let _ = std::fs::remove_dir_all(other);
        }
    }
    std::fs::canonicalize(&copy).unwrap_or_else(|error| panic!("{copy:?}: {error}"))
}

/// The folder name of a build's content: its length and its modification time in nanoseconds,
/// in hex. Cargo's uplift keeps the length and the time of the binary it copies, so each copy of
/// one build has one name.
fn content_key(len: u64, modified: std::time::SystemTime) -> String {
    let nanos = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("{len:x}-{nanos:x}")
}

/// What says a file is the same, unchanged: its device and inode, its length and its
/// modification time.
fn stamp(path: &Path) -> Option<(u64, u64, u64, Option<std::time::SystemTime>)> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let (dev, ino) = {
        use std::os::unix::fs::MetadataExt as _;
        (meta.dev(), meta.ino())
    };
    #[cfg(not(unix))]
    let (dev, ino) = (0, 0);
    meta.is_file()
        .then(|| (dev, ino, meta.len(), meta.modified().ok()))
}
