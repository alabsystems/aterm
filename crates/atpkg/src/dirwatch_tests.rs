// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const NOW: Option<Duration> = Some(Duration::ZERO);

#[test]
fn a_directory_entry_names_its_watch_alone() {
    let root = crate::testdir::dir("atpkg-dirwatch-dir-");
    let (a, b) = (root.join("a"), root.join("b"));
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let watcher = Watcher::new().unwrap();
    let wa = watcher.add_dir(&a).unwrap();
    let wb = watcher.add_dir(&b).unwrap();
    assert_ne!(wa, wb);
    assert!(
        watcher.wait(NOW).unwrap().is_empty(),
        "CONTROL: nothing yet"
    );
    std::fs::write(root.join("staged"), b"x").unwrap();
    std::fs::rename(root.join("staged"), a.join("row")).unwrap();
    assert_eq!(watcher.wait(NOW).unwrap(), [wa]);
    std::fs::remove_file(a.join("row")).unwrap();
    assert_eq!(watcher.wait(NOW).unwrap(), [wa]);
    std::fs::remove_dir(&b).unwrap();
    assert_eq!(watcher.wait(NOW).unwrap(), [wb], "the directory itself");
}

#[test]
fn a_file_written_in_place_names_the_file_and_not_its_directory() {
    use std::io::Write as _;

    let root = crate::testdir::dir("atpkg-dirwatch-file-");
    let row = root.join("4242.json");
    std::fs::write(&row, b"").unwrap();
    let watcher = Watcher::new().unwrap();
    let dir = watcher.add_dir(&root).unwrap();
    let file = watcher.add_file(&row).unwrap();
    let mut out = std::fs::OpenOptions::new().append(true).open(&row).unwrap();
    out.write_all(b"{\"pid\":4242}").unwrap();
    drop(out);
    assert_eq!(watcher.wait(NOW).unwrap(), [file], "not {dir:?}");
    let mut out = std::fs::OpenOptions::new().append(true).open(&row).unwrap();
    out.write_all(b"\n").unwrap();
    drop(out);
    assert_eq!(watcher.wait(NOW).unwrap(), [file]);
}

#[test]
fn permission_changes_name_the_watched_file_and_directory() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = crate::testdir::dir("atpkg-dirwatch-permissions-");
    let parent = root.join("source");
    std::fs::create_dir(&parent).unwrap();
    let path = parent.join("input");
    std::fs::write(&path, b"inert input").unwrap();
    let mode = |path: &Path, value| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(value)).unwrap();
    };
    mode(&parent, 0o700);
    mode(&path, 0o600);
    let watcher = Watcher::new().unwrap();
    let directory = watcher.add_dir(&parent).unwrap();
    let file = watcher.add_file(&path).unwrap();
    for (path, value, expected) in [
        (&path, 0o400, file),
        (&path, 0o600, file),
        (&parent, 0o500, directory),
        (&parent, 0o700, directory),
    ] {
        mode(path, value);
        assert_eq!(
            watcher.wait(Some(aterm_spec::wait::MUST_HAPPEN)).unwrap(),
            [expected],
            "an input's permissions can change its availability without replacing it"
        );
    }
}

#[test]
fn a_read_only_sample_does_not_notify_attribute_watchers() {
    let root = crate::testdir::dir("atpkg-dirwatch-read-only-");
    let path = root.join("input");
    std::fs::write(&path, b"inert input").unwrap();
    std::fs::File::open(&path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_accessed(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
        )
        .unwrap();
    let watcher = Watcher::new().unwrap();
    watcher.add_dir(&root).unwrap();
    watcher.add_file(&path).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"inert input");
    assert!(
        watcher.wait(NOW).unwrap().is_empty(),
        "a local read must not keep waking its own input watch"
    );
}

#[test]
fn an_edge_after_registration_is_kept_for_a_later_wait() {
    let root = crate::testdir::dir("atpkg-dirwatch-kept-");
    std::fs::write(root.join("before"), b"").unwrap();
    let watcher = Watcher::new().unwrap();
    let id = watcher.add_dir(&root).unwrap();
    assert!(
        watcher.wait(NOW).unwrap().is_empty(),
        "before: not reported"
    );
    std::fs::write(root.join("after"), b"").unwrap();
    assert_eq!(
        watcher.wait(Some(aterm_spec::wait::MUST_HAPPEN)).unwrap(),
        [id]
    );
}

#[test]
fn a_removed_watch_names_nothing() {
    let root = crate::testdir::dir("atpkg-dirwatch-removed-");
    let (a, b) = (root.join("a"), root.join("b"));
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let watcher = Watcher::new().unwrap();
    let wa = watcher.add_dir(&a).unwrap();
    let wb = watcher.add_dir(&b).unwrap();
    watcher.remove(wa);
    watcher.remove(wa);
    std::fs::write(a.join("x"), b"").unwrap();
    std::fs::write(b.join("x"), b"").unwrap();
    assert_eq!(watcher.wait(NOW).unwrap(), [wb]);
    let again = watcher.add_dir(&a).unwrap();
    assert!(again != wa && again != wb, "ids are never reused");
    std::fs::write(a.join("y"), b"").unwrap();
    assert_eq!(watcher.wait(NOW).unwrap(), [again], "CONTROL: kept");
}

#[test]
fn a_watch_whose_object_is_removed_is_gone() {
    let root = crate::testdir::dir("atpkg-dirwatch-gone-");
    let (dir, kept, row) = (root.join("dir"), root.join("kept"), root.join("row"));
    std::fs::create_dir(&dir).unwrap();
    std::fs::create_dir(&kept).unwrap();
    std::fs::write(&row, b"").unwrap();
    let watcher = Watcher::new().unwrap();
    let wd = watcher.add_dir(&dir).unwrap();
    let wk = watcher.add_dir(&kept).unwrap();
    let wf = watcher.add_file(&row).unwrap();
    assert!(!watcher.is_gone(wd) && !watcher.is_gone(wk) && !watcher.is_gone(wf));
    std::fs::remove_dir(&dir).unwrap();
    std::fs::write(kept.join("x"), b"").unwrap();
    std::fs::remove_file(&row).unwrap();
    assert_eq!(
        watcher.wait(Some(aterm_spec::wait::MUST_HAPPEN)).unwrap(),
        [wd, wk, wf]
    );
    assert!(watcher.is_gone(wd), "the removed directory");
    assert!(watcher.is_gone(wf), "the removed file");
    assert!(
        !watcher.is_gone(wk),
        "CONTROL: an entry changed, not the object"
    );
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("y"), b"").unwrap();
    assert!(
        watcher.wait(NOW).unwrap().is_empty(),
        "the new directory is not watched"
    );
    assert!(watcher.is_gone(wd));
    watcher.remove(wk);
    assert!(watcher.is_gone(wk), "a removed watch");
    assert!(watcher.is_gone(WatchId(u64::MAX)), "an id never handed out");
}

#[test]
fn one_directory_watched_twice_keeps_the_other_watch() {
    let root = crate::testdir::dir("atpkg-dirwatch-twice-");
    let watcher = Watcher::new().unwrap();
    let first = watcher.add_dir(&root).unwrap();
    let second = watcher.add_dir(&root).unwrap();
    std::fs::write(root.join("x"), b"").unwrap();
    assert_eq!(watcher.wait(NOW).unwrap(), [first, second]);
    watcher.remove(first);
    std::fs::write(root.join("y"), b"").unwrap();
    assert_eq!(watcher.wait(NOW).unwrap(), [second]);
}

#[test]
fn a_wake_ends_the_wait() {
    let watcher = std::sync::Arc::new(Watcher::new().unwrap());
    let waiting = || {
        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = std::sync::Arc::clone(&watcher);
        std::thread::spawn(move || tx.send(watcher.wait(None).unwrap()));
        rx
    };
    watcher.wake().unwrap();
    let kept = waiting()
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("a wake given before the wait ends it");
    assert!(kept.is_empty());
    let rx = waiting();
    watcher.wake().unwrap();
    let woke = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the woken wait returns");
    assert!(woke.is_empty());
    watcher.wake().unwrap();
    assert!(watcher.wait(Some(Duration::MAX)).unwrap().is_empty());
}

#[test]
fn a_symlink_or_the_wrong_kind_is_refused() {
    let root = crate::testdir::dir("atpkg-dirwatch-kind-");
    let (dir, file) = (root.join("dir"), root.join("file"));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(&file, b"").unwrap();
    std::os::unix::fs::symlink(&dir, root.join("dir-link")).unwrap();
    std::os::unix::fs::symlink(&file, root.join("file-link")).unwrap();
    let watcher = Watcher::new().unwrap();
    assert!(watcher.add_dir(&root.join("dir-link")).is_err());
    assert!(watcher.add_file(&root.join("file-link")).is_err());
    assert!(watcher.add_dir(&file).is_err());
    assert!(watcher.add_file(&dir).is_err());
    assert!(watcher.add_dir(&root.join("missing")).is_err());
    assert!(watcher.add_dir(&dir).is_ok() && watcher.add_file(&file).is_ok());
}
