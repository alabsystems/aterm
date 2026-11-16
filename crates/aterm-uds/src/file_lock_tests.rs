// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn existing_only_lock_never_recreates_a_removed_slot() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("slot");
    assert_eq!(
        LockFile::take_existing(&path, Instant::now(), "busy")
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert!(!path.exists());
    std::fs::write(&path, b"retained").unwrap();
    let held = LockFile::take_existing(&path, Instant::now(), "busy").unwrap();
    assert_eq!(
        LockFile::take_existing(&path, Instant::now(), "busy")
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"retained");
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        LockFile::take_existing(&path, Instant::now(), "busy")
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert!(!path.exists());
    drop(held);
}

/// One holder at a time: a second take while the first holds it waits until its deadline and
/// says `busy`, and takes it once the first let go; a missing file is made 0600, one there is
/// taken whatever its mode, and a folder is no lock file.
#[test]
fn a_lock_file_has_one_holder() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("record.lock");
    let first = LockFile::take(&path, Instant::now(), "busy").unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let refused = LockFile::take(&path, Instant::now(), "busy").unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(refused.to_string(), "busy");
    drop(first);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    drop(LockFile::take(&path, Instant::now(), "busy").unwrap());
    assert!(
        LockFile::take(dir.path(), Instant::now(), "busy").is_err(),
        "a folder is no lock file"
    );
}

/// Shared holders hold a lock file together, two in one process as two processes' would, and
/// none beside an exclusive holder: `try_take` finds a shared holder at once and says `None`,
/// waiting for nothing, and takes it once every shared holder let go; while it holds it, a
/// share waits until its deadline and says `busy`. A missing file is made 0600, and a folder is
/// no lock file to either.
/// NEGATIVE CONTROL, observed: with `share` taking the exclusive lock (`Hold::Exclusive`), the
/// second share is refused: `` called `Result::unwrap()` on an `Err` value: Custom { kind:
/// WouldBlock, error: "busy" } ``.
#[test]
fn shared_holders_hold_a_lock_file_together_and_never_beside_an_exclusive_one() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("name.looking");
    let first = LockFile::share(&path, Instant::now(), "busy").unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let second = LockFile::share(&path, Instant::now(), "busy").unwrap();
    assert!(LockFile::try_take(&path).unwrap().is_none());
    drop(first);
    assert!(
        LockFile::try_take(&path).unwrap().is_none(),
        "one shared holder is left"
    );
    drop(second);
    let alone = LockFile::try_take(&path)
        .unwrap()
        .expect("no holder is left");
    let refused = LockFile::share(&path, Instant::now(), "busy").unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(refused.to_string(), "busy");
    assert!(LockFile::try_take(&path).unwrap().is_none());
    drop(alone);
    drop(LockFile::share(&path, Instant::now(), "busy").unwrap());
    assert!(LockFile::share(dir.path(), Instant::now(), "busy").is_err());
    assert!(LockFile::try_take(dir.path()).is_err());
}

/// A link at the path is never followed (`O_NOFOLLOW`): a take through a link to a regular
/// file there fails with `ELOOP`, and that file is never locked through it, so a take of the
/// file itself, while the take through the link is held, gets it at once.
#[test]
fn a_link_at_the_path_is_never_followed_to_the_file_it_names() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::write(&target, b"").unwrap();
    let linked = dir.path().join("linked.lock");
    std::os::unix::fs::symlink(&target, &linked).unwrap();
    let through = LockFile::take(&linked, Instant::now(), "busy");
    let direct = LockFile::take(&target, Instant::now(), "busy");
    assert!(
        direct.is_ok(),
        "the target was locked through the link: {direct:?}"
    );
    assert_eq!(
        through.as_ref().map_err(io::Error::raw_os_error).err(),
        Some(Some(libc::ELOOP)),
        "{through:?}"
    );
}

/// A FIFO at the path is never waited on (`O_NONBLOCK`): the take returns at once, refused as
/// no regular file, where a read-only open without that flag waits for a writer that never
/// comes. The take is a one-minute hang detector away from the test.
#[test]
fn a_fifo_at_the_path_is_refused_at_once() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let fifo = dir.path().join("fifo.lock");
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `c` is a valid NUL-terminated path in the test's own scratch folder.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");
    let (tx, rx) = std::sync::mpsc::channel();
    let at = fifo.clone();
    std::thread::spawn(move || {
        let _ = tx.send(LockFile::take(&at, Instant::now(), "busy").map(drop));
    });
    let taken = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the take of a FIFO returned (a hang detector)");
    assert_eq!(taken.map_err(|e| e.kind()), Err(io::ErrorKind::InvalidData));
}

/// A thread inside [`takes_nothing`] is refused a take before the lock is tried, and each try,
/// refused or not, counts on that thread alone ([`takes_tried_here`]): a fresh thread has tried
/// none, and the rule ends as `takes_nothing` returns. NEGATIVE CONTROL: the same take outside
/// the rule gets the lock, so the refusal is the rule's and not the file's.
#[test]
fn a_thread_that_takes_nothing_is_refused_each_take_and_each_try_counts() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("watched.lock");
    let here = takes_tried_here();
    std::thread::spawn(move || {
        assert_eq!(takes_tried_here(), 0, "a fresh thread has tried no take");
        let refused = takes_nothing(|| LockFile::take(&path, Instant::now(), "busy").map(drop));
        assert_eq!(
            refused.map_err(|e| e.kind()),
            Err(io::ErrorKind::PermissionDenied)
        );
        assert_eq!(takes_tried_here(), 1, "a refused take is a try");
        let held = LockFile::take(&path, Instant::now(), "busy").expect("a take outside the rule");
        assert_eq!(takes_tried_here(), 2);
        drop(held);
    })
    .join()
    .unwrap();
    assert_eq!(
        takes_tried_here(),
        here,
        "another thread's tries are its own"
    );
}

/// The shared lease and the take that never waits are takes too: inside [`takes_nothing`] a
/// [`LockFile::share`] and a [`LockFile::try_take`] are each refused before the lock is tried
/// and each counts as a try, so no holder, shared or exclusive, is left behind them. Outside the
/// rule the same share gets the lock, and the same try_take, once the share let go, gets it.
/// NEGATIVE CONTROLS, observed: with `try_take` not asking `refused_here`, the try_take inside
/// the rule takes the lock: `` left: Ok(true) right: Err(PermissionDenied) ``; with the refusal
/// asked by `lock_exclusive_with_clock` alone and not by `lock_with_clock`, the share inside it
/// does: `` left: Ok(()) right: Err(PermissionDenied) ``.
#[test]
fn a_thread_that_takes_nothing_is_refused_a_share_and_a_try_take() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("name.looking");
    std::thread::spawn(move || {
        let refused = takes_nothing(|| LockFile::share(&path, Instant::now(), "busy").map(drop));
        assert_eq!(
            refused.map_err(|e| e.kind()),
            Err(io::ErrorKind::PermissionDenied)
        );
        assert_eq!(takes_tried_here(), 1, "a refused share is a try");
        let refused = takes_nothing(|| LockFile::try_take(&path).map(|held| held.is_some()));
        assert_eq!(
            refused.map_err(|e| e.kind()),
            Err(io::ErrorKind::PermissionDenied),
            "a try_take inside the rule"
        );
        assert_eq!(takes_tried_here(), 2, "a refused try_take is a try");
        drop(LockFile::share(&path, Instant::now(), "busy").expect("a share outside the rule"));
        let alone = LockFile::try_take(&path).expect("a try_take outside the rule");
        assert!(alone.is_some(), "nothing the refused takes tried holds it");
        assert_eq!(takes_tried_here(), 4);
    })
    .join()
    .unwrap();
}
