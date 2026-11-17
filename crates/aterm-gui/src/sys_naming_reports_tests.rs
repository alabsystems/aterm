// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn name(id: usize) -> String {
    format!("wait-{id:032x}.sample.txt")
}

fn write(directory: &Path, id: usize, when: SystemTime) {
    let path = directory.join(name(id));
    std::fs::write(&path, b"private stacks").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(when))
        .unwrap();
}

#[test]
fn automatic_reports_disappear_on_success_failure_and_cancellation() {
    let root = aterm_tempfile::tempdir().unwrap();
    for error in [
        None,
        Some(io::ErrorKind::PermissionDenied),
        Some(io::ErrorKind::Interrupted),
    ] {
        let mut staged = None;
        let result = observe(root.path(), false, SystemTime::now(), |directory| {
            staged = Some(directory.to_owned());
            std::fs::write(
                directory.join(name(1)),
                if error.is_some() {
                    b"".as_slice()
                } else {
                    b"complete stacks".as_slice()
                },
            )?;
            match error {
                Some(kind) => Err(io::Error::from(kind)),
                None => Ok("timer"),
            }
        });
        assert_eq!(result.as_ref().err().map(io::Error::kind), error);
        if error.is_none() {
            assert_eq!(result.unwrap(), "timer");
        }
        assert!(!staged.unwrap().exists());
        assert!(!root.path().join(name(1)).exists());
    }
}

#[test]
fn explicit_reports_survive_success_and_collection_failure() {
    let root = aterm_tempfile::tempdir().unwrap();
    for (id, error) in [(1, None), (2, Some(io::ErrorKind::TimedOut))] {
        let mut staged = None;
        let result = observe(root.path(), true, SystemTime::now(), |directory| {
            staged = Some(directory.to_owned());
            std::fs::write(directory.join(name(id)), b"retained stacks")?;
            error.map_or(Ok(()), |kind| Err(io::Error::from(kind)))
        });
        assert_eq!(result.err().map(|error| error.kind()), error);
        assert!(!staged.unwrap().exists());
        assert_eq!(
            std::fs::read(root.path().join(name(id))).unwrap(),
            b"retained stacks"
        );
    }
}

#[test]
fn retention_bounds_completed_reports_without_touching_staging_or_other_entries() {
    let root = aterm_tempfile::tempdir().unwrap();
    let now = SystemTime::UNIX_EPOCH + AGE * 2;
    for id in 0..KEEP + 2 {
        write(
            root.path(),
            id,
            now - Duration::from_secs((KEEP + 2 - id) as u64),
        );
    }
    write(root.path(), 400, now - AGE);
    // A wall-clock rollback does not expire future files; the count still bounds them.
    write(root.path(), 500, now + Duration::from_secs(3600));
    let staging = root.path().join(".ubd").join("active.noindex");
    std::fs::create_dir_all(&staging).unwrap();
    write(&staging, 600, now - AGE);
    std::fs::write(root.path().join("notes.txt"), b"keep").unwrap();
    std::fs::write(root.path().join("wait-short.sample.txt"), b"keep").unwrap();
    std::fs::create_dir(root.path().join(name(700))).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("notes.txt", root.path().join(name(800))).unwrap();

    observe(root.path(), false, now, |_| Ok(())).unwrap();
    for id in 0..3 {
        assert!(!root.path().join(name(id)).exists());
    }
    for id in 3..KEEP + 2 {
        assert!(root.path().join(name(id)).is_file());
    }
    assert!(!root.path().join(name(400)).exists());
    assert!(root.path().join(name(500)).is_file());
    assert!(staging.join(name(600)).is_file());
    assert_eq!(
        std::fs::read(root.path().join("notes.txt")).unwrap(),
        b"keep"
    );
    assert!(root.path().join("wait-short.sample.txt").is_file());
    assert!(root.path().join(name(700)).is_dir());
    #[cfg(unix)]
    assert!(
        std::fs::symlink_metadata(root.path().join(name(800)))
            .unwrap()
            .is_symlink()
    );
}

#[test]
fn retention_never_replaces_an_existing_report() {
    let root = aterm_tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(name(1)), b"original").unwrap();
    let result = observe(root.path(), true, SystemTime::now(), |directory| {
        std::fs::write(directory.join(name(1)), b"collision")
    });
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read(root.path().join(name(1))).unwrap(),
        b"original"
    );
}

#[cfg(unix)]
#[test]
fn abandoned_staging_is_collected_but_active_and_unowned_directories_survive() {
    let root = aterm_tempfile::tempdir().unwrap();
    let staging = root.path().join(".ubd");
    std::fs::create_dir(&staging).unwrap();
    let abandoned = aterm_tempfile::Builder::new()
        .prefix("wait-")
        .suffix(".noindex")
        .tempdir_in(&staging)
        .unwrap();
    let owner = claim(abandoned.path()).unwrap();
    write(abandoned.path(), 1, SystemTime::now());
    let abandoned = abandoned.keep();
    // This is the process-death boundary: the directory remains and its OS lock closes.
    drop(owner);
    let unowned = staging.join("wait-unowned.noindex");
    std::fs::create_dir(&unowned).unwrap();
    std::fs::write(unowned.join("keep"), b"unrelated").unwrap();
    observe(root.path(), false, SystemTime::now(), |active| {
        assert!(!abandoned.exists());
        write(active, 2, SystemTime::now());
        scavenge(&staging)?;
        assert!(
            active.join(name(2)).is_file(),
            "the live lock excludes collection"
        );
        assert!(unowned.join("keep").is_file());
        Ok(())
    })
    .unwrap();
    assert!(unowned.join("keep").is_file());
}
