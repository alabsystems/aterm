// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The no-follow reads on a test's own scratch folder: a link anywhere on a path is refused and
//! never walked, a FIFO's reader and its event-only open return at once refused, a watch hears a
//! write.

use std::os::fd::AsFd as _;
use std::path::PathBuf;

use super::*;

/// A scratch folder spelled without a link (the temp folder is under `/var`, a link to
/// `/private/var`), and the folder a link in it points to, which holds `secret`.
fn scratch() -> (aterm_tempfile::TempDir, PathBuf) {
    let dir = aterm_tempfile::TempDir::new().expect("a scratch folder");
    let root = std::fs::canonicalize(dir.path()).expect("its own spelling");
    std::fs::create_dir(root.join("real")).expect("a folder");
    std::fs::create_dir(root.join("elsewhere")).expect("a folder");
    std::fs::write(root.join("elsewhere/secret"), b"x").expect("a file");
    std::fs::write(root.join("real/file"), b"line\n").expect("a file");
    std::os::unix::fs::symlink(root.join("elsewhere"), root.join("chain")).expect("a link");
    std::os::unix::fs::symlink(root.join("real/file"), root.join("real/last")).expect("a link");
    (dir, root)
}

/// EVERY CALL REFUSES A LINK ON THE PATH OR AT ITS END. NEGATIVE CONTROL: the same file through
/// no link opens, and std's following open reads through the link.
#[test]
fn a_link_anywhere_on_the_path_is_refused_and_never_walked() {
    let (_dir, root) = scratch();
    let through = root.join("chain/secret");
    let last = root.join("real/last");
    for path in [&through, &last] {
        assert_eq!(
            kind_of(path).expect("read"),
            Kind::Link,
            "{}",
            path.display()
        );
        assert!(is_link(
            &open_evtonly(path, Kind::File).expect_err("refused")
        ));
        assert!(is_link(&open_reader(path).expect_err("refused")));
    }
    assert!(
        std::fs::read(&through).is_ok(),
        "NEGATIVE CONTROL: a following open reads through the link"
    );
    assert_eq!(
        kind_of(&root.join("elsewhere/secret")).expect("read"),
        Kind::File
    );
    assert_eq!(kind_of(&root.join("real")).expect("read"), Kind::Dir);
    assert!(open_evtonly(&root.join("real"), Kind::Dir).is_ok());
    assert!(open_reader(&root.join("real/file")).is_ok());
}

/// NOTHING THERE IS MISSING, through a missing folder or a file named as one.
#[test]
fn nothing_there_reads_missing() {
    let (_dir, root) = scratch();
    for path in [
        root.join("nope"),
        root.join("nope/deeper"),
        root.join("real/file/below"),
    ] {
        assert_eq!(
            kind_of(&path).expect("read"),
            Kind::Missing,
            "{}",
            path.display()
        );
        assert!(is_missing(&open_reader(&path).expect_err("refused")));
    }
}

/// A FIFO NAMED AS A FILE: the reader's open returns at once (no writer is ever opened) and
/// refuses it as not a regular file; a folder is refused so too. Each read is a one-minute hang
/// detector away from the test.
#[test]
fn the_reader_refuses_a_fifo_at_once_and_a_folder() {
    let (_dir, root) = scratch();
    let fifo = root.join("pipe");
    let c = std::ffi::CString::new(fifo.to_str().expect("utf-8")).expect("no NUL");
    // SAFETY: `c` is a valid NUL-terminated path in the test's own scratch folder.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert_eq!(kind_of(&fifo).expect("read"), Kind::Other);
    let (tx, rx) = std::sync::mpsc::channel();
    let at = fifo.clone();
    std::thread::spawn(move || {
        let _ = tx.send(open_reader(&at).map(drop));
    });
    let read = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the FIFO's reader returned (a hang detector)");
    assert!(not_regular(&read.expect_err("refused")));
    assert!(not_regular(
        &open_reader(&root.join("real")).expect_err("refused")
    ));
}

/// A FIFO ASKED FOR A WATCH, as a folder or as a file: the event-only open returns at once (an
/// open of a FIFO with no writer would wait for one) and refuses it as not what was asked for.
/// NEGATIVE CONTROL: a folder asked as a folder and a file as a file open; each asked as the other
/// is refused. Each open is a one-minute hang detector away from the test.
#[test]
fn an_event_only_open_of_a_fifo_returns_at_once_refused() {
    let (_dir, root) = scratch();
    let fifo = root.join("pipe");
    let c = std::ffi::CString::new(fifo.to_str().expect("utf-8")).expect("no NUL");
    // SAFETY: `c` is a valid NUL-terminated path in the test's own scratch folder.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    for want in [Kind::Dir, Kind::File] {
        let (tx, rx) = std::sync::mpsc::channel();
        let at = fifo.clone();
        std::thread::spawn(move || {
            let _ = tx.send(open_evtonly(&at, want).map(drop));
        });
        let opened = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the FIFO's event-only open returned (a hang detector)");
        assert!(not_regular(&opened.expect_err("refused")), "{want:?}");
    }
    assert!(open_evtonly(&root.join("real"), Kind::Dir).is_ok());
    assert!(open_evtonly(&root.join("real/file"), Kind::File).is_ok());
    assert!(not_regular(
        &open_evtonly(&root.join("real"), Kind::File).expect_err("refused")
    ));
    assert!(not_regular(
        &open_evtonly(&root.join("real/file"), Kind::Dir).expect_err("refused")
    ));
}

/// A WATCH HEARS A WRITE IN ITS FOLDER and a file's going, and the bell ends a wait. The waits
/// are one-minute hang detectors.
#[test]
fn a_watch_hears_its_folder_change_and_its_file_go() {
    let (_dir, root) = scratch();
    let watcher = Watcher::new().expect("a kqueue");
    let dir = open_evtonly(&root.join("real"), Kind::Dir).expect("the folder");
    let file = open_evtonly(&root.join("real/file"), Kind::File).expect("the file");
    watcher.add(dir.as_fd(), 7).expect("watched");
    watcher.add(file.as_fd(), 8).expect("watched");
    std::fs::write(root.join("real/new"), b"x").expect("a write");
    let fired = until_fired(&watcher, 7);
    assert!(!fired.gone);
    std::fs::remove_file(root.join("real/file")).expect("removed");
    let fired = until_fired(&watcher, 8);
    assert!(fired.gone, "the file's watch says it left its name");
    watcher.ring().expect("rung");
    assert!(
        watcher
            .wait(Duration::from_secs(60))
            .expect("woken")
            .is_empty(),
        "the bell returns an empty wait"
    );
}

fn until_fired(watcher: &Watcher, id: u64) -> Fired {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!left.is_zero(), "watch {id} fired (a hang detector)");
        if let Some(fired) = watcher
            .wait(left)
            .expect("a wait")
            .into_iter()
            .find(|f| f.id == id)
        {
            return fired;
        }
    }
}

/// WHAT A READER NAMES: its device, inode and size, and a new inode at the same name.
#[test]
fn identity_names_the_inode_and_its_size() {
    let (_dir, root) = scratch();
    let path = root.join("real/file");
    let first = identity(open_reader(&path).expect("opened").as_fd()).expect("read");
    assert_eq!(first.size, 5);
    std::fs::remove_file(&path).expect("removed");
    std::fs::write(&path, b"longer line\n").expect("written again");
    let second = identity(open_reader(&path).expect("opened").as_fd()).expect("read");
    assert_eq!(second.size, 12);
    assert_eq!(first.dev, second.dev);
    assert_ne!(
        first.ino, second.ino,
        "a new file at the name is a new inode"
    );
}
