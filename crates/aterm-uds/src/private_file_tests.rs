// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use std::io;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{
    KeptParse, OwnedRead, PrivateDir, PrivateFileError, PrivateLock, PrivateRead, check_owned_dir,
    create_private_dir, ensure_private_dir, judge_private_dir, open_private_rw, read_owned_into,
    read_private, read_private_into, remove_private, write_private, write_private_new,
};

/// The busy text the lock tests hand [`PrivateLock::take`].
const BUSY: &str = "another holder has it";

/// A waiter for a held lock is told once, sleeps, and takes the lock when its holder lets go:
/// the holder is released from the `held` hook's thread only after the waiter said it found
/// the lock held, and the waiter's lock then excludes a try.
/// NEGATIVE CONTROL: answering the first try's `WouldBlock` as an error (`=> return
/// Err(..)`) fails "the waiter takes it once let go"; never calling `held` hangs the
/// release, and the one-minute hang detector fails it.
#[test]
fn a_waiter_takes_a_held_lock_once_it_is_let_go() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    let first = PrivateLock::take(&dir, ".lock", Duration::ZERO, BUSY).unwrap();
    let (told, heard) = std::sync::mpsc::channel();
    let waiter = {
        let dir = dir.clone();
        std::thread::spawn(move || {
            PrivateLock::wait_for(&dir, ".lock", move || told.send(()).unwrap())
        })
    };
    heard
        .recv_timeout(Duration::from_secs(60))
        .expect("the waiter found the lock held");
    drop(first);
    let held = waiter
        .join()
        .unwrap()
        .expect("the waiter takes it once let go");
    assert_eq!(
        PrivateLock::take(&dir, ".lock", Duration::ZERO, BUSY)
            .err()
            .map(|e| e.kind()),
        Some(io::ErrorKind::WouldBlock),
        "the waiter holds it"
    );
    drop(held);
    let free = PrivateLock::wait_for(&dir, ".lock", || panic!("a free lock is never held"));
    assert!(free.is_ok());
}

#[test]
fn lock_wait_stops_when_hook_or_backoff_exhausts_the_budget() {
    for release_in_hook in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir_in(root.path(), "state", 0o700);
        let first = std::cell::RefCell::new(Some(
            PrivateLock::take(&dir, ".lock", Duration::ZERO, BUSY).unwrap(),
        ));
        let (second, _) = super::open_private(&dir, ".lock").unwrap();
        let start = Instant::now();
        let deadline = start + Duration::from_secs(60);
        let clock = std::cell::Cell::new(start);
        let pauses = std::cell::Cell::new(0);
        let refused = crate::file_lock::lock_exclusive_with_clock(
            &second,
            Some(deadline),
            BUSY,
            || {
                if release_in_hook {
                    drop(first.borrow_mut().take());
                    clock.set(deadline);
                }
            },
            || clock.get(),
            |_| {
                pauses.set(pauses.get() + 1);
                drop(first.borrow_mut().take());
                clock.set(deadline);
            },
        )
        .unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(refused.to_string(), BUSY);
        assert_eq!(pauses.get(), usize::from(!release_in_hook));
        assert!(first.borrow().is_none());
        let _free = PrivateLock::take(&dir, ".lock", Duration::ZERO, BUSY).unwrap();
    }
}

#[test]
fn exclusive_wait_observes_contention_and_acquires_after_release() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    let mut first =
        Some(PrivateLock::take(&dir, ".lock", std::time::Duration::ZERO, BUSY).unwrap());
    let (second, _) = super::open_private(&dir, ".lock").unwrap();
    let mut told = 0;
    crate::file_lock::lock_exclusive_until_with(
        &second,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
        BUSY,
        || {
            told += 1;
            drop(first.take());
        },
    )
    .unwrap();
    assert_eq!(told, 1, "only actual lock contention is waited for");
    let mut retried = false;
    let refused = crate::file_lock::lock_exclusive_until_with(
        &super::open_private(&dir, ".lock").unwrap().0,
        std::time::Instant::now(),
        BUSY,
        || retried = true,
    )
    .unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::WouldBlock);
    assert_eq!(refused.to_string(), BUSY);
    assert!(!retried, "an expired deadline does not retry a held lock");
    second.unlock().unwrap();
}

/// With one lock held, a second take that does not wait is `WouldBlock` with the busy
/// text; once the first is dropped, a second take succeeds.
#[test]
fn private_lock_excludes_a_second_holder() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    let held = PrivateLock::take(&dir, ".lock", std::time::Duration::ZERO, BUSY).unwrap();
    let refused = PrivateLock::take(&dir, ".lock", std::time::Duration::ZERO, BUSY)
        .err()
        .expect("a second holder is refused");
    assert_eq!(refused.kind(), std::io::ErrorKind::WouldBlock);
    assert_eq!(refused.to_string(), BUSY);
    drop(held);
    let _again = PrivateLock::take(&dir, ".lock", std::time::Duration::ZERO, BUSY).unwrap();
}

/// A symlink at the lock's name is refused, and so is a FIFO, whose open returns at
/// once (`O_NONBLOCK`).
#[test]
fn private_lock_refuses_a_link_and_a_fifo() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    file_in(&dir, "real", b"", 0o600);
    std::os::unix::fs::symlink(dir.join("real"), dir.join("link.lock")).unwrap();
    assert!(PrivateLock::take(&dir, "link.lock", std::time::Duration::ZERO, BUSY).is_err());
    // A dangling link is refused too, and its target is never created.
    let outside = root.path().join("outside.lock");
    std::os::unix::fs::symlink(&outside, dir.join("dangling.lock")).unwrap();
    assert!(PrivateLock::take(&dir, "dangling.lock", std::time::Duration::ZERO, BUSY).is_err());
    assert!(!outside.exists());
    let fifo = dir.join("fifo.lock");
    let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a NUL-terminated path that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0, "mkfifo");
    let refused = PrivateLock::take(&dir, "fifo.lock", std::time::Duration::ZERO, BUSY)
        .err()
        .expect("a FIFO is no lock");
    assert_eq!(refused.kind(), std::io::ErrorKind::InvalidData);
}

/// A name of more than one path component is `InvalidInput` before anything is opened:
/// no lock file is made below `dir`, beside it, or at an absolute path.
#[test]
fn private_lock_refuses_a_name_of_more_than_one_component() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    let absolute = root.path().join("absolute.lock");
    let absolute_name = absolute.to_str().unwrap();
    for name in ["sub/x.lock", "../beside.lock", "..", ".", "", absolute_name] {
        assert_eq!(
            PrivateLock::take(&dir, name, std::time::Duration::ZERO, BUSY)
                .err()
                .expect("refused")
                .kind(),
            std::io::ErrorKind::InvalidInput,
            "{name:?}"
        );
    }
    assert!(!dir.join("sub").exists());
    assert!(!root.path().join("beside.lock").exists());
    assert!(!absolute.exists());
}

/// `take_with` tells its hook each time it finds the lock held, and takes it once the
/// holder lets go: here the hook itself drops the first holder. Its busy text is the
/// caller's, made at run time.
#[test]
fn private_lock_take_with_tells_its_hook_and_takes_a_freed_lock() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    let mut first =
        Some(PrivateLock::take(&dir, ".lock", std::time::Duration::ZERO, BUSY).unwrap());
    let mut told = 0;
    let _second = PrivateLock::take_with(
        &dir,
        ".lock",
        std::time::Duration::from_secs(60),
        "unused",
        || {
            told += 1;
            drop(first.take());
        },
    )
    .unwrap();
    assert_eq!(told, 1);
    let busy = format!("busy in {}", dir.display());
    let refused = PrivateLock::take_with(&dir, ".lock", std::time::Duration::ZERO, &busy, || {})
        .err()
        .expect("held by the second");
    assert_eq!(refused.kind(), std::io::ErrorKind::WouldBlock);
    assert_eq!(refused.to_string(), busy);
}

/// A free lock is taken whatever the wait, one no clock can represent included.
#[test]
fn private_lock_takes_a_free_lock_with_any_wait() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    let _held = PrivateLock::take(&dir, ".lock", std::time::Duration::MAX, BUSY).unwrap();
}

/// A missing file is created 0600; a 0644 file, or a file in a 0755 directory, is not
/// private.
#[test]
fn open_private_rw_creates_0600_and_refuses_loose() {
    use std::io::{Seek as _, Write as _};
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    let mut file = open_private_rw(&dir, "journal").unwrap();
    assert_eq!(mode_of(&dir.join("journal")), 0o600);
    assert_eq!(file.stream_position().unwrap(), 0);
    file.write_all(b"step").unwrap();
    drop(file);
    let again = open_private_rw(&dir, "journal").unwrap();
    assert_eq!(again.metadata().unwrap().len(), 4);
    file_in(&dir, "loose", b"x", 0o644);
    assert!(matches!(
        open_private_rw(&dir, "loose"),
        Err(PrivateFileError::NotPrivate(_))
    ));
    let open = dir_in(root.path(), "open", 0o755);
    assert!(matches!(
        open_private_rw(&open, "journal"),
        Err(PrivateFileError::NotPrivate(_))
    ));
    assert!(!open.join("journal").exists());
    // An existing private file in a directory loosened later is refused too.
    file_in(&open, "existing", b"x", 0o600);
    assert!(matches!(
        open_private_rw(&open, "existing"),
        Err(PrivateFileError::NotPrivate(_))
    ));
    // A link in the file's place, a dangling one included, is refused, and the
    // creating open never makes its target.
    let outside = root.path().join("outside");
    std::os::unix::fs::symlink(&outside, dir.join("linked")).unwrap();
    assert!(matches!(
        open_private_rw(&dir, "linked"),
        Err(PrivateFileError::Symlink)
    ));
    assert!(!outside.exists());
}

/// Under a umask that clears an owner bit (`0o277`), the journal file and the lock file
/// are still made exactly 0600, so a second open of each, read-write, succeeds; an
/// existing file's mode is left as it was. The umask is process-wide, so the body runs
/// alone in a child of this test binary, which is waited on by a hang detector.
#[test]
fn files_made_under_a_narrow_umask_are_exactly_0600() {
    const NAME: &str = "private_file::tests::files_made_under_a_narrow_umask_are_exactly_0600";
    const STAGE_ENV: &str = "ATERM_UDS_TEST_NARROW_UMASK";
    if std::env::var_os(STAGE_ENV).is_some() {
        // The directory first: under this umask a new one would be 0500.
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = dir_in(root.path(), "state", 0o700);
        // SAFETY: `umask(2)` swaps this process's file-creation mask and cannot fail;
        // this child runs this one test, so no other create sees the mask.
        unsafe { libc::umask(0o277) };
        drop(open_private_rw(&dir, "journal").unwrap());
        assert_eq!(mode_of(&dir.join("journal")), 0o600);
        drop(open_private_rw(&dir, "journal").expect("the second read-write open"));
        drop(PrivateLock::take(&dir, ".lock", std::time::Duration::ZERO, BUSY).unwrap());
        assert_eq!(mode_of(&dir.join(".lock")), 0o600);
        drop(
            PrivateLock::take(&dir, ".lock", std::time::Duration::ZERO, BUSY)
                .expect("the second take"),
        );
        // An existing file is opened as it stands: never chmodded.
        file_in(&dir, "narrow", b"x", 0o400);
        assert!(open_private_rw(&dir, "narrow").is_err());
        assert_eq!(mode_of(&dir.join("narrow")), 0o400);
        println!("STAGED narrow umask ok");
        return;
    }
    let out = crate::deadline::ends_within(
        crate::spawnfd::command(std::env::current_exe().expect("the test binary"))
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(STAGE_ENV, "1"),
        crate::deadline::ECHO_HANG,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("STAGED narrow umask ok"),
        "{:?}\nstdout: {stdout}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Group and other read bits read, and say so; a write bit on the file or the
/// directory, or a symlink, is refused.
#[test]
fn read_owned_into_accepts_read_bits_only() {
    use std::os::unix::fs::MetadataExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let mut buf = [0u8; 64];
    let shared = dir_in(root.path(), "shared", 0o755);
    file_in(&shared, "token", b"legacy", 0o644);
    let meta = std::fs::metadata(shared.join("token")).unwrap();
    assert_eq!(
        read_owned_into(&shared, "token", 64, &mut buf).unwrap(),
        OwnedRead {
            len: 6,
            dev: meta.dev(),
            ino: meta.ino(),
            loose: true,
        }
    );
    assert_eq!(&buf[..6], b"legacy");
    let private = dir_in(root.path(), "private", 0o700);
    file_in(&private, "token", b"tight", 0o600);
    let read = read_owned_into(&private, "token", 64, &mut buf).unwrap();
    assert_eq!((read.len, read.loose), (5, false));
    // A search bit lets nobody read: neither refused nor loose. A group read bit is.
    let searchable = dir_in(root.path(), "searchable", 0o711);
    file_in(&searchable, "token", b"tight", 0o600);
    let read = read_owned_into(&searchable, "token", 64, &mut buf).unwrap();
    assert_eq!((read.len, read.loose), (5, false));
    file_in(&private, "group-read", b"group", 0o640);
    let read = read_owned_into(&private, "group-read", 64, &mut buf).unwrap();
    assert_eq!((read.len, read.loose), (5, true));
    // A write bit is refused by name: which bits, on what, and the mode judged.
    let mut refused = |dir: &Path, name: &str| match read_owned_into(dir, name, 64, &mut buf) {
        Err(PrivateFileError::Writable(writable)) => {
            assert_eq!(
                writable.to_string(),
                PrivateFileError::Writable(writable).to_string()
            );
            (writable.dir(), writable.mode(), writable.cause())
        }
        other => panic!("{name} in {}: {other:?}", dir.display()),
    };
    file_in(&shared, "group-write", b"x", 0o664);
    assert_eq!(
        refused(&shared, "group-write"),
        (false, 0o664, "group-writable")
    );
    file_in(&private, "both-write", b"x", 0o666);
    assert_eq!(
        refused(&private, "both-write"),
        (false, 0o666, "group- and world-writable")
    );
    // umask 002's directory: only its group may write, and its group may be the
    // owner's alone, which no NSS lookup proves complete; still refused, as that.
    let grouped = dir_in(root.path(), "grouped", 0o775);
    file_in(&grouped, "token", b"x", 0o644);
    assert_eq!(refused(&grouped, "token"), (true, 0o775, "group-writable"));
    let world = dir_in(root.path(), "world", 0o700);
    file_in(&world, "token", b"x", 0o600);
    std::fs::set_permissions(&world, std::fs::Permissions::from_mode(0o757)).unwrap();
    assert_eq!(refused(&world, "token"), (true, 0o757, "world-writable"));
    std::os::unix::fs::symlink(private.join("token"), shared.join("link")).unwrap();
    assert!(matches!(
        read_owned_into(&shared, "link", 64, &mut buf),
        Err(PrivateFileError::Symlink)
    ));
    // astream-sys's rule judges an access list before the mode: a loose file that one
    // opens to others is refused for the list, not read as loose.
    #[cfg(target_os = "macos")]
    {
        let token = shared.join("token");
        chmod(&[
            "+a".as_ref(),
            "everyone allow write".as_ref(),
            token.as_os_str(),
        ]);
        match read_owned_into(&shared, "token", 64, &mut buf) {
            Err(PrivateFileError::NotPrivate(why)) => {
                assert!(why.contains("chmod -N"), "{why}");
            }
            other => panic!("an access list that allows others: {other:?}"),
        }
    }
}

fn dir_in(root: &Path, name: &str, mode: u32) -> PathBuf {
    let dir = root.join(name);
    std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
    dir
}

fn file_in(dir: &Path, name: &str, bytes: &[u8], mode: u32) {
    use std::io::Write as _;
    let path = dir.join(name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    file.write_all(bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn mode_of(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

#[test]
fn a_private_file_reads_into_the_buffer_with_its_identity() {
    use std::os::unix::fs::MetadataExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    file_in(&dir, "token", b"eleven byte", 0o600);
    let mut buf = [0u8; 64];
    let read = read_private_into(&dir, "token", 32, &mut buf).unwrap();
    let meta = std::fs::metadata(dir.join("token")).unwrap();
    assert_eq!(
        read,
        PrivateRead {
            len: 11,
            dev: meta.dev(),
            ino: meta.ino(),
        }
    );
    assert_eq!(&buf[..read.len], b"eleven byte");
}

#[test]
fn a_file_past_the_cap_is_malformed() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    file_in(&dir, "token", &[7; 9], 0o600);
    let mut buf = [0u8; 64];
    assert!(matches!(
        read_private_into(&dir, "token", 8, &mut buf),
        Err(PrivateFileError::Malformed(
            "longer than such a file can be"
        ))
    ));
    assert!(buf.iter().all(|&b| b == 0), "{buf:?}");
    let mut short = [0u8; 8];
    assert!(matches!(
        read_private_into(&dir, "token", 64, &mut short),
        Err(PrivateFileError::Malformed(
            "longer than such a file can be"
        ))
    ));
    assert_eq!(short, [0; 8]);
    assert_eq!(
        read_private_into(&dir, "token", 9, &mut buf).unwrap().len,
        9
    );
}

#[test]
fn a_loose_file_or_directory_is_not_private() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut buf = [0u8; 64];
    let dir = dir_in(root.path(), "state", 0o700);
    file_in(&dir, "token", b"x", 0o644);
    assert!(matches!(
        read_private_into(&dir, "token", 64, &mut buf),
        Err(PrivateFileError::NotPrivate(_))
    ));
    let loose = dir_in(root.path(), "loose", 0o755);
    file_in(&loose, "token", b"x", 0o600);
    assert!(matches!(
        read_private_into(&loose, "token", 64, &mut buf),
        Err(PrivateFileError::NotPrivate(_))
    ));
}

/// A [`super::JudgedDir`] reads and opens each file as `read_private` and `open_private`
/// do, refusals included, in a private, an open, a linked and a missing directory.
#[test]
fn a_judged_directory_reads_as_read_private_does() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = dir_in(root.path(), "state", 0o700);
    file_in(&state, "token", b"x", 0o600);
    file_in(&state, "loose", b"x", 0o644);
    std::os::unix::fs::symlink(state.join("token"), state.join("link")).unwrap();
    let open = dir_in(root.path(), "open", 0o755);
    file_in(&open, "token", b"x", 0o600);
    let linked = root.path().join("linked");
    std::os::unix::fs::symlink(&state, &linked).unwrap();
    file_in(root.path(), "plain", b"x", 0o600);
    let said = |read: Result<Vec<u8>, PrivateFileError>| match read {
        Ok(bytes) => format!("{bytes:?}"),
        Err(e) => format!("{e:?}"),
    };
    let mut verdicts = std::collections::BTreeSet::new();
    let (plain, gone) = (root.path().join("plain"), root.path().join("gone"));
    for dir in [state, open, linked, plain, gone] {
        let judged = super::JudgedDir::judge(&dir).unwrap();
        for name in ["token", "loose", "link", "missing", "sub/token"] {
            let want = said(read_private(&dir, name, 64));
            assert_eq!(said(judged.read(name, 64)), want, "{dir:?} {name}");
            assert_eq!(
                judged.open(name).is_ok(),
                super::open_private(&dir, name).is_ok(),
                "{dir:?} {name}"
            );
            verdicts.insert(want);
        }
    }
    // A read, then refusals for a loose file, an open directory, a link, a missing file,
    // a name of two components and a directory that is a file.
    assert_eq!(verdicts.len(), 7, "{verdicts:?}");
}

/// A private judgement refuses every directory [`super::check_private_dir`] refuses, as it does.
///
/// NEGATIVE CONTROL (2026-11-05): with `check_dir_verdict` gone from `JudgedDir::private`, the
/// first directory that is not private is taken (`left: Ok(()), right: Err(..)`).
#[test]
fn a_private_judgement_refuses_as_check_private_dir_does() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = dir_in(root.path(), "state", 0o700);
    let linked = root.path().join("linked");
    std::os::unix::fs::symlink(&state, &linked).unwrap();
    file_in(root.path(), "plain", b"x", 0o600);
    let said = |answer: io::Result<()>| answer.map_err(|error| error.to_string());
    assert!(super::JudgedDir::private(&state).is_ok());
    for dir in [
        dir_in(root.path(), "open", 0o755),
        linked,
        root.path().join("plain"),
        root.path().join("gone"),
    ] {
        let want = said(super::check_private_dir(&dir));
        assert!(want.is_err(), "{dir:?}");
        assert_eq!(
            said(super::JudgedDir::private(&dir).map(drop)),
            want,
            "{dir:?}"
        );
    }
}

/// A verdict is the directory's when it was judged: opens under it do not judge the
/// directory again, and the next verdict sees what changed.
#[test]
fn a_judged_directory_is_judged_once() {
    let root = aterm_tempfile::tempdir().unwrap();
    let state = dir_in(root.path(), "state", 0o700);
    file_in(&state, "token", b"x", 0o600);
    let judged = super::JudgedDir::judge(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(judged.read("token", 64).unwrap(), b"x");
    assert!(matches!(
        super::JudgedDir::judge(&state).unwrap().read("token", 64),
        Err(PrivateFileError::NotPrivate(super::DIR_OPEN))
    ));
}

#[test]
fn a_name_of_more_than_one_component_is_malformed() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    let sub = dir_in(&dir, "sub", 0o700);
    file_in(&sub, "token", b"x", 0o600);
    let mut buf = [0u8; 64];
    for name in ["sub/token", "../state/sub/token", "..", ".", "", "a\0b"] {
        assert!(
            matches!(
                read_private_into(&dir, name, 64, &mut buf),
                Err(PrivateFileError::Malformed(_))
            ),
            "{name:?}"
        );
    }
    assert_eq!(
        read_private_into(&sub, "token", 64, &mut buf).unwrap().len,
        1
    );
}

#[test]
fn a_name_of_more_than_one_component_is_neither_written_nor_removed() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    file_in(root.path(), "beside", b"x", 0o600);
    for name in ["sub/token", "../token", "..", ".", "", "a\0b"] {
        assert_eq!(
            write_private(&dir, name, b"t").unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput,
            "{name:?}"
        );
    }
    assert!(!dir.join("sub").exists());
    assert!(!root.path().join("token").exists());
    assert_eq!(
        remove_private(&dir, "../beside").unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert!(root.path().join("beside").exists());
    write_private(&dir, "token", b"t").unwrap();
    assert!(remove_private(&dir, "token").unwrap());
}

#[test]
fn a_file_beneath_no_directory_was_never_there() {
    let root = aterm_tempfile::tempdir().unwrap();
    file_in(root.path(), "state", b"x", 0o600);
    let file = root.path().join("state");
    assert!(!remove_private(&file, "token").unwrap());
    assert!(!remove_private(&file.join("sub"), "token").unwrap());
    assert!(!remove_private(&root.path().join("missing"), "token").unwrap());
    assert!(file.exists());
}

/// NEGATIVE CONTROL: `remove_private` and `check_owned_dir` refuse a symlinked directory
/// without unlinking entries inside the symlink target.
#[test]
fn remove_private_and_check_owned_dir_refuse_symlinked_directory() {
    let root = aterm_tempfile::tempdir().unwrap();
    let outside = dir_in(root.path(), "outside", 0o700);
    file_in(&outside, "kept.entry", b"kept", 0o600);
    let linked = root.path().join("state-link");
    std::os::unix::fs::symlink(&outside, &linked).unwrap();

    assert_eq!(
        check_owned_dir(&linked).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        remove_private(&linked, "kept.entry").unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(std::fs::read(outside.join("kept.entry")).unwrap(), b"kept");
}

#[test]
fn read_private_with_no_cap_reads_the_file() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    file_in(&dir, "token", b"whole", 0o600);
    assert_eq!(read_private(&dir, "token", usize::MAX).unwrap(), b"whole");
}

/// On macOS a new directory that inherits its parent's access list is no private one:
/// `create_private_dir` refuses it, and accepts it once `chmod -N` removes the list.
#[cfg(target_os = "macos")]
#[test]
fn create_private_dir_refuses_an_inherited_access_list() {
    let root = aterm_tempfile::tempdir().unwrap();
    let parent = dir_in(root.path(), "parent", 0o700);
    let child = parent.join("child");
    chmod(&[
        "+a".as_ref(),
        "everyone allow list,search,directory_inherit".as_ref(),
        parent.as_os_str(),
    ]);
    assert_eq!(
        create_private_dir(&child).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    chmod(&["-N".as_ref(), parent.as_os_str(), child.as_os_str()]);
    create_private_dir(&child).unwrap();
    assert_eq!(mode_of(&child), 0o700);
}

/// `chmod <args>`, which must succeed.
#[cfg(target_os = "macos")]
fn chmod(args: &[&std::ffi::OsStr]) {
    let ran = crate::spawnfd::command("/bin/chmod")
        .args(args)
        .status()
        .expect("run chmod");
    assert!(ran.success(), "chmod {args:?}");
}

/// On macOS an access list that allows anyone anything makes a 0700 directory no
/// private one: `judge_private_dir` and `ensure_private_dir` refuse it, naming
/// `chmod -N`, and leave it as it was.
#[cfg(target_os = "macos")]
#[test]
fn ensure_private_dir_refuses_an_access_list_that_allows_others() {
    let root = aterm_tempfile::tempdir().unwrap();
    let granted = dir_in(root.path(), "granted", 0o700);
    chmod(&[
        "+a".as_ref(),
        "everyone allow read".as_ref(),
        granted.as_os_str(),
    ]);
    for refusal in [
        judge_private_dir(&granted).unwrap_err(),
        ensure_private_dir(&granted).unwrap_err(),
    ] {
        assert_eq!(refusal.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(refusal.raw_os_error().is_none(), "{refusal}");
        assert!(refusal.to_string().contains("chmod -N"), "{refusal}");
    }
    assert_eq!(mode_of(&granted), 0o700);

    // Open by its mode bits AND by an access list: refused for the list, its mode
    // left as it was. astream-sys judges the list before the mode. Were the mode
    // judged first, `judge_private_dir` would answer `Open(0o755)`, so
    // `aterm fabric on --dry-run` would promise only a `chmod 700`, and
    // `ensure_private_dir` would narrow the owner's directory to 0700 before its
    // second look found the list and refused it: the mode changed for nothing.
    // Both are caught here, by the `unwrap_err` and by the mode check below.
    let wide = dir_in(root.path(), "wide", 0o755);
    chmod(&[
        "+a".as_ref(),
        "everyone allow read".as_ref(),
        wide.as_os_str(),
    ]);
    for refusal in [
        judge_private_dir(&wide).unwrap_err(),
        ensure_private_dir(&wide).unwrap_err(),
    ] {
        assert_eq!(refusal.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(refusal.to_string().contains("chmod -N"), "{refusal}");
    }
    assert_eq!(mode_of(&wide), 0o755, "a refused directory is not narrowed");

    let parent = dir_in(root.path(), "parent", 0o700);
    chmod(&[
        "+a".as_ref(),
        "everyone allow list,search,directory_inherit".as_ref(),
        parent.as_os_str(),
    ]);
    let child = parent.join("child");
    assert_eq!(
        ensure_private_dir(&child).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );

    let denied = dir_in(root.path(), "denied", 0o700);
    chmod(&[
        "+a".as_ref(),
        "everyone deny add_file".as_ref(),
        denied.as_os_str(),
    ]);
    assert_eq!(judge_private_dir(&denied).unwrap(), PrivateDir::Private);
    assert_eq!(ensure_private_dir(&denied).unwrap(), None);
}

#[test]
fn judge_private_dir_says_what_ensure_would_do_and_changes_nothing() {
    let root = aterm_tempfile::tempdir().unwrap();
    let fresh = root.path().join("fresh");
    assert_eq!(judge_private_dir(&fresh).unwrap(), PrivateDir::Missing);
    assert!(!fresh.exists());
    assert_eq!(ensure_private_dir(&fresh).unwrap(), None);
    assert_eq!(judge_private_dir(&fresh).unwrap(), PrivateDir::Private);

    let open = dir_in(root.path(), "open", 0o755);
    assert_eq!(judge_private_dir(&open).unwrap(), PrivateDir::Open(0o755));
    assert_eq!(mode_of(&open), 0o755);
    assert_eq!(ensure_private_dir(&open).unwrap(), Some(0o755));
    assert_eq!(mode_of(&open), 0o700);

    let link = root.path().join("link");
    std::os::unix::fs::symlink(&fresh, &link).unwrap();
    let refusal = judge_private_dir(&link).unwrap_err();
    assert_eq!(refusal.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(refusal.raw_os_error().is_none(), "{refusal}");
}

#[test]
fn ensure_private_dir_refuses_one_its_narrowing_left_open() {
    let root = aterm_tempfile::tempdir().unwrap();
    let open = dir_in(root.path(), "open", 0o755);
    // A volume that ignores modes: the chmod succeeds and changes nothing.
    let refusal = super::ensure_private_dir_by(&open, super::make_0700, |_| Ok(())).unwrap_err();
    assert_eq!(refusal.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(refusal.to_string().contains("is not 0700"), "{refusal}");
    assert_eq!(judge_private_dir(&open).unwrap(), PrivateDir::Open(0o755));
    assert_eq!(ensure_private_dir(&open).unwrap(), Some(0o755));
}

#[test]
fn ensure_private_dir_finds_one_removed_as_it_was_made_private_gone() {
    let root = aterm_tempfile::tempdir().unwrap();
    let open = dir_in(root.path(), "open", 0o755);
    for dir in [open, root.path().join("missing")] {
        let gone =
            super::ensure_private_dir_by(&dir, super::make_0700, |dir| std::fs::remove_dir(dir))
                .unwrap_err();
        assert_eq!(gone.kind(), std::io::ErrorKind::NotFound, "{gone}");
        assert!(gone.to_string().contains("was removed"), "{gone}");
        assert_eq!(judge_private_dir(&dir).unwrap(), PrivateDir::Missing);
    }
}

#[test]
fn ensure_private_dir_judges_one_another_writer_made_first() {
    let root = aterm_tempfile::tempdir().unwrap();
    for (name, mode, was) in [("private", 0o700, None), ("open", 0o755, Some(0o755))] {
        let dir = root.path().join(name);
        let made = super::ensure_private_dir_by(
            &dir,
            |dir| {
                dir_in(root.path(), name, mode);
                super::make_0700(dir)
            },
            super::narrow_0700,
        );
        assert_eq!(made.unwrap(), was, "{name}");
        assert_eq!(mode_of(&dir), 0o700, "{name}");
    }

    let gone = root.path().join("gone");
    let refusal = super::ensure_private_dir_by(
        &gone,
        |_| Err(std::io::ErrorKind::AlreadyExists.into()),
        super::narrow_0700,
    )
    .unwrap_err();
    assert_eq!(refusal.kind(), std::io::ErrorKind::NotFound, "{refusal}");
    assert!(refusal.to_string().contains("was removed"), "{refusal}");

    let target = dir_in(root.path(), "target", 0o755);
    let link = root.path().join("link");
    let refusal = super::ensure_private_dir_by(
        &link,
        |dir| {
            std::os::unix::fs::symlink(&target, dir).unwrap();
            super::make_0700(dir)
        },
        super::narrow_0700,
    )
    .unwrap_err();
    assert_eq!(
        refusal.kind(),
        std::io::ErrorKind::PermissionDenied,
        "{refusal}"
    );
    assert!(refusal.to_string().contains("is a symlink"), "{refusal}");
    assert_eq!(mode_of(&target), 0o755);
}

#[test]
fn ensure_private_dir_names_the_directory_once_in_a_bare_refusal() {
    use std::io::ErrorKind;
    let root = aterm_tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a-file"), b"x").unwrap();
    std::os::unix::fs::symlink(root.path().join("nowhere"), root.path().join("dangling")).unwrap();
    let unwritable = dir_in(root.path(), "unwritable", 0o500);
    for (odd, kind) in [
        ("a-file/state", ErrorKind::NotADirectory),
        ("dangling/state", ErrorKind::AlreadyExists),
        ("unwritable/state", ErrorKind::PermissionDenied),
        // `.` hides `gone` from `Path::parent`: the make finds no parent.
        ("gone/.", ErrorKind::NotFound),
    ] {
        let dir = root.path().join(odd);
        let refusal = ensure_private_dir(&dir).unwrap_err();
        assert_eq!(refusal.kind(), kind, "{odd}: {refusal}");
        let shown = dir.display().to_string();
        assert_eq!(refusal.to_string().matches(&shown).count(), 1, "{refusal}");
    }
    std::fs::set_permissions(&unwritable, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn a_fifo_or_an_access_list_in_the_files_place_is_refused() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    file_in(&dir, "token", b"x", 0o600);
    let made = crate::spawnfd::command("/usr/bin/mkfifo")
        .arg("-m")
        .arg("600")
        .arg(dir.join("fifo"))
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo");
    let (tx, rx) = std::sync::mpsc::channel();
    let at = dir.clone();
    std::thread::spawn(move || {
        let _ = tx.send(read_private(&at, "fifo", 64).map(|_| ()));
    });
    let refused = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("a FIFO is judged without waiting for a writer");
    assert!(
        matches!(
            refused,
            Err(PrivateFileError::Malformed("not a regular file"))
        ),
        "{refused:?}"
    );
    assert_eq!(read_private(&dir, "token", 64).unwrap(), b"x");

    #[cfg(target_os = "macos")]
    {
        let token = dir.join("token");
        chmod(&[
            "+a".as_ref(),
            "everyone allow read".as_ref(),
            token.as_os_str(),
        ]);
        match read_private(&dir, "token", 64) {
            Err(PrivateFileError::NotPrivate(why)) => {
                assert!(why.contains("chmod -N"), "{why}")
            }
            other => panic!("an access list that allows others: {other:?}"),
        }
        chmod(&["-N".as_ref(), token.as_os_str()]);
        assert_eq!(read_private(&dir, "token", 64).unwrap(), b"x");
    }
}

#[test]
fn a_symlink_in_the_files_place_is_refused() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    file_in(&dir, "real", b"x", 0o600);
    std::os::unix::fs::symlink(dir.join("real"), dir.join("token")).unwrap();
    let mut buf = [0u8; 64];
    assert!(matches!(
        read_private_into(&dir, "token", 64, &mut buf),
        Err(PrivateFileError::Symlink)
    ));
}

#[test]
fn create_private_dir_makes_or_accepts_and_never_narrows() {
    let root = aterm_tempfile::tempdir().unwrap();
    let fresh = root.path().join("fresh");
    create_private_dir(&fresh).unwrap();
    assert!(std::fs::symlink_metadata(&fresh).unwrap().is_dir());
    assert_eq!(mode_of(&fresh), 0o700);
    create_private_dir(&fresh).unwrap();

    let loose = dir_in(root.path(), "loose", 0o755);
    assert!(create_private_dir(&loose).is_err());
    assert_eq!(mode_of(&loose), 0o755);

    let link = root.path().join("link");
    std::os::unix::fs::symlink(&fresh, &link).unwrap();
    assert!(create_private_dir(&link).is_err());

    let orphan = root.path().join("absent").join("child");
    assert_eq!(
        create_private_dir(&orphan).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
}

/// A NAME REPLACED UNDER AN OPEN READ IS NO SECOND LINK: the descriptor of a file an atomic
/// write renamed over has no link left, and reads the whole file as it stood at the open.
#[test]
fn a_file_replaced_after_its_open_reads_as_it_stood_and_a_second_link_stays_refused() {
    use std::os::unix::fs::MetadataExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    let open = |how| {
        let file = match how {
            super::Opening::ReadWrite => std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(dir.join("book")),
            _ => astream_sys::open_nonblocking(&dir.join("book"), astream_sys::Links::Refuse),
        };
        file.unwrap()
    };
    write_private(&dir, "book", b"old").unwrap();
    let read = open(super::Opening::Read);
    write_private(&dir, "book", b"new").unwrap();
    let (file, meta, _) =
        super::judge_opened(read, super::Opening::Read).expect("a read of the replaced name");
    assert_eq!(meta.nlink(), 0, "the rename left the open file no link");
    assert_eq!(super::read_capped(file, 64).unwrap(), b"old");
    assert_eq!(read_private(&dir, "book", 64).unwrap(), b"new");

    let written = open(super::Opening::ReadWrite);
    write_private(&dir, "book", b"newer").unwrap();
    assert!(matches!(
        super::judge_opened(written, super::Opening::ReadWrite),
        Err(PrivateFileError::Missing)
    ));

    std::fs::hard_link(dir.join("book"), dir.join("copy")).unwrap();
    for how in [super::Opening::Read, super::Opening::ReadWrite] {
        assert!(matches!(
            super::judge_opened(open(how), how),
            Err(PrivateFileError::NotPrivate(super::ANOTHER_LINK))
        ));
    }
    assert!(matches!(
        read_private(&dir, "book", 64),
        Err(PrivateFileError::NotPrivate(super::ANOTHER_LINK))
    ));
}

/// A new private file is made whole, `prepare` seeing it before it has its name, and one that
/// is there stays: writing a secret twice can strand nothing it protects.
#[test]
fn a_new_private_file_never_replaces_one_and_leaves_no_temporary() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("d");
    create_private_dir(&dir).unwrap();
    let names = || {
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    };
    let mut prepared = false;
    assert!(
        write_private_new(&dir, "key", b"one", |file| {
            prepared =
                !dir.join("key").exists() && file.metadata()?.permissions().mode() & 0o777 == 0o600;
            Ok(())
        })
        .unwrap()
    );
    assert!(prepared, "prepare ran on a 0600 file with no name yet");
    assert_eq!(read_private(&dir, "key", 64).unwrap(), b"one");
    assert!(!write_private_new(&dir, "key", b"two", |_| Ok(())).unwrap());
    assert_eq!(read_private(&dir, "key", 64).unwrap(), b"one", "kept");
    assert_eq!(names(), ["key"], "no temporary left");

    let refused =
        write_private_new(&dir, "other", b"x", |_| Err(std::io::Error::other("no"))).unwrap_err();
    assert_eq!(refused.to_string(), "no");
    assert_eq!(names(), ["key"], "a failed prepare leaves no name");
    assert_eq!(
        write_private_new(&dir, "a/b", b"x", |_| Ok(()))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
}

/// Equal bytes reuse their parse even after the file was replaced; a same-length write
/// into that file is read and parsed again. A different file evicts the one parse kept.
#[test]
fn a_kept_parse_of_unchanged_bytes_is_not_parsed_again() {
    use std::io::Write as _;
    use std::os::unix::fs::MetadataExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    write_private(&dir, "book", b"first").unwrap();
    let kept = KeptParse::new(1);
    let parses = std::cell::Cell::new(0);
    let parse = |bytes: &[u8]| {
        parses.set(parses.get() + 1);
        bytes.to_vec()
    };
    assert_eq!(kept.read(&dir, "book", 64, parse).unwrap(), b"first");
    assert_eq!(kept.read(&dir, "book", 64, parse).unwrap(), b"first");
    assert_eq!(parses.get(), 1, "a file nothing changed is parsed once");
    let original = std::fs::File::open(dir.join("book")).unwrap();
    let initial = original.metadata().unwrap();
    write_private(&dir, "book", b"first").unwrap();
    let replaced = std::fs::metadata(dir.join("book")).unwrap();
    assert_ne!(
        (initial.dev(), initial.ino()),
        (replaced.dev(), replaced.ino()),
        "the same bytes are in a different file"
    );
    assert_eq!(kept.read(&dir, "book", 64, parse).unwrap(), b"first");
    assert_eq!(parses.get(), 1, "equal bytes reuse their parse");
    std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join("book"))
        .unwrap()
        .write_all(b"again")
        .unwrap();
    assert_eq!(
        std::fs::metadata(dir.join("book")).unwrap().ino(),
        replaced.ino()
    );
    assert_eq!(
        kept.read(&dir, "book", 64, parse).unwrap(),
        b"again",
        "a same-length in-place rewrite is read again"
    );
    assert_eq!(parses.get(), 2);
    write_private(&dir, "other", b"other").unwrap();
    assert_eq!(kept.read(&dir, "other", 64, parse).unwrap(), b"other");
    assert_eq!(parses.get(), 3);
    assert_eq!(kept.read(&dir, "book", 64, parse).unwrap(), b"again");
    assert_eq!(parses.get(), 4, "the evicted file is parsed again");
}

/// A kept parse is handed back only after the file was opened and judged again: once its
/// directory is no longer private the read is refused, though the file stands as it was.
/// A warm parse also obeys each read's byte cap before invoking the parser.
#[test]
fn a_kept_parse_is_judged_at_every_read() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = dir_in(root.path(), "state", 0o700);
    write_private(&dir, "book", b"held").unwrap();
    let kept = KeptParse::new(4);
    assert_eq!(
        kept.read(&dir, "book", 64, <[u8]>::to_vec).unwrap(),
        b"held"
    );
    assert!(matches!(
        kept.read(&dir, "book", 3, |_| panic!(
            "an oversized file is not parsed"
        )),
        Err(PrivateFileError::Malformed(_))
    ));
    assert_eq!(kept.read(&dir, "book", 4, <[u8]>::to_vec).unwrap(), b"held");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        kept.read(&dir, "book", 64, <[u8]>::to_vec),
        Err(PrivateFileError::NotPrivate(_))
    ));
}

/// A directory judgement's answer: its verdict, or its error by kind and raw errno.
#[cfg(target_os = "macos")]
type Said = Result<super::DirIs, (io::ErrorKind, Option<i32>)>;

#[cfg(target_os = "macos")]
fn said_of(judged: io::Result<super::DirIs>) -> Said {
    judged.map_err(|error| (error.kind(), error.raw_os_error()))
}

/// The answer the OS error `errno` gives.
#[cfg(target_os = "macos")]
fn refused(errno: i32) -> Said {
    Err((io::Error::from_raw_os_error(errno).kind(), Some(errno)))
}

/// `path` judged by [`super::dir_is`], with what that judged and opened
/// ([`super::tally::directories`]), and by [`super::dir_is_opened`], on a thread of their own
/// behind the one-minute hang detector (a FIFO's non-blocking open returns at once; a blocking
/// one would hang here).
#[cfg(target_os = "macos")]
fn judged_both_ways(path: &Path) -> (Said, super::tally::Directories, Said) {
    let at = path.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (by_path, seen) = super::tally::directories(|| said_of(super::dir_is(&at)));
        let _ = tx.send((by_path, seen, said_of(super::dir_is_opened(&at))));
    });
    rx.recv_timeout(Duration::from_secs(60))
        .expect("a judgement returns")
}

/// The modes and access lists a macOS law's fixtures changed, put back before its tempdir
/// drops (a 0000 or 0300 directory, and a deny entry, block the removal): `put_back` at the
/// law's end, asserted, and `Drop` as best it can where the law panicked first.
#[cfg(target_os = "macos")]
#[derive(Default)]
struct PutBack {
    modes: Vec<PathBuf>,
    lists: Vec<PathBuf>,
    done: bool,
}

#[cfg(target_os = "macos")]
impl PutBack {
    /// Each changed mode made 0700, then each list removed (`chmod -N`): whether every one was.
    fn put_back(&mut self) -> bool {
        self.done = true;
        let mut all = true;
        for path in &self.modes {
            all &= std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).is_ok();
        }
        for path in &self.lists {
            all &= crate::spawnfd::command("/bin/chmod")
                .arg("-N")
                .arg(path)
                .status()
                .is_ok_and(|ran| ran.success());
        }
        all
    }
}

#[cfg(target_os = "macos")]
impl Drop for PutBack {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.put_back();
        }
    }
}

/// The principals of the 20-entry list: 12 users and 8 groups every Mac has, each a distinct
/// qualifier, so `chmod +a` adds an entry for each rather than merging it into one already
/// there (file_cmds chmod_acl.c merges an entry of the same qualifier and tag). astream-sys's
/// law A3 builds its long list from the same principals.
#[cfg(target_os = "macos")]
const DISTINCT: [&str; 20] = [
    "user:root",
    "user:daemon",
    "user:nobody",
    "user:_www",
    "user:_uucp",
    "user:_lp",
    "user:_postfix",
    "user:_sshd",
    "user:_spotlight",
    "user:_windowserver",
    "user:_coreaudiod",
    "user:_locationd",
    "group:staff",
    "group:admin",
    "group:wheel",
    "group:everyone",
    "group:tty",
    "group:operator",
    "group:kmem",
    "group:_lpadmin",
];

/// How many entries `/bin/ls -lde` lists for `path` (its ` <n>: ` lines).
#[cfg(target_os = "macos")]
fn entries_listed(path: &Path) -> usize {
    let listed = crate::spawnfd::command("/bin/ls")
        .arg("-lde")
        .arg(path)
        .output()
        .expect("run ls");
    assert!(listed.status.success(), "ls -lde {}", path.display());
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter(|line| {
            line.trim_start()
                .split_once(": ")
                .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .count()
}

/// LAW U1: a directory is judged by its path as by its open. Over astream-sys's law A3
/// fixtures (the socket bound in a short tempdir of `/tmp`, since a path in the test's
/// tempdir may outrun `sun_path`) and a mode-0000 file, each row's
/// [`super::dir_is`] answers its absolute expectation and [`super::dir_is_opened`]'s answer (a
/// verdict, or an error by kind and raw errno), and judges the path once
/// ([`super::tally::directories`]). On macOS arm64 it opens the path only where the path's
/// attributes cannot decide (a lookup through a file or a locked directory, a file named with
/// a trailing slash, a directory without its owner's read or search bit or with an access
/// list, and devfs's `/dev/fd` node of a held directory, which shows every search bit clear);
/// on any other Mac every judgement opens it.
/// Rows whose answer differs for root branch on `euid() == 0`.
///
/// CONTROL: the 20-entry list holds at least 17 entries (`ls -lde`) before it is judged, so
/// its stat passes Libc's first 16-entry buffer (Libc-1752.120.2 sys/statx_np.c:34).
///
/// NEGATIVE CONTROLS, each applied by hand once to private_file.rs and reverted: `dir_is`
/// answering `DirIs::Private` for `DirByPath::Undecided` fails every row it leaves to the open
/// (the 0300 row answers `Private` where its open is refused `EACCES`); `DirIs::of` mapping
/// `NotPrivate::Kind` to `DirIs::Others` fails the file, FIFO, `/dev/null`, socket and 0000
/// file rows; `dir_is` calling `dir_is_opened` whatever the path's attributes answered fails
/// the count on every decided row; `dir_is_opened` answering the open's error where
/// `kind_and_owner_of` refuses (`Err(_) => Err(e)`) fails the socket and 0000 file rows,
/// whose open is refused (`EOPNOTSUPP`; `EACCES` where not root) though each is `NotDir`.
#[cfg(target_os = "macos")]
#[test]
fn a_directory_is_judged_by_its_path_as_by_its_open() {
    use super::DirIs;
    use std::os::fd::AsRawFd as _;
    let as_root = astream_sys::euid() == 0;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut put_back = PutBack::default();
    let base = scratch.path();
    let at = |name: &str| base.join(name);
    let slashed = |path: &Path| {
        let mut slashed = path.as_os_str().to_owned();
        slashed.push("/");
        PathBuf::from(slashed)
    };

    file_in(base, "f", b"", 0o600);
    let file = at("f");
    // Removing a file needs only its directory's write bit, so the 0000 file needs no put-back.
    file_in(base, "f000", b"", 0o000);
    // A socket's name has a much smaller cap than a filesystem path on macOS.
    let sockets = aterm_tempfile::Builder::new()
        .prefix("u1.")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = sockets.path().join("s");
    let listening = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    for (name, mode) in [
        ("d700", 0o700),
        ("d500", 0o500),
        ("d755", 0o755),
        ("d750", 0o750),
        ("d711", 0o711),
        ("h733", 0o733),
    ] {
        dir_in(base, name, mode);
    }
    for (name, mode) in [("d300", 0o300), ("d000", 0o000)] {
        put_back.modes.push(at(name));
        dir_in(base, name, mode);
    }
    std::os::unix::fs::symlink(at("d700"), at("l")).unwrap();
    std::os::unix::fs::symlink(at("nothing"), at("dangling")).unwrap();
    let made = crate::spawnfd::command("/usr/bin/mkfifo")
        .args(["-m", "600"])
        .arg(at("q"))
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo");
    let locked = dir_in(base, "locked", 0o700);
    dir_in(&locked, "inner", 0o700);
    put_back.modes.push(locked.clone());
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    for (name, mode, entry) in [
        ("allow-list", 0o700, "everyone allow list"),
        ("allow-read", 0o755, "everyone allow read"),
        ("deny-delete", 0o700, "everyone deny delete"),
        ("deny-list", 0o700, "everyone deny list"),
        ("h700-allow", 0o700, "everyone allow list"),
    ] {
        let dir = dir_in(base, name, mode);
        put_back.lists.push(dir.clone());
        chmod(&["+a".as_ref(), entry.as_ref(), dir.as_os_str()]);
    }
    let long = dir_in(base, "deny-20", 0o700);
    put_back.lists.push(long.clone());
    for (who, right) in DISTINCT
        .iter()
        .zip(["delete", "writeattr", "writeextattr"].iter().cycle())
    {
        let entry = format!("{who} deny {right}");
        chmod(&["+a".as_ref(), entry.as_ref(), long.as_os_str()]);
    }
    let listed = entries_listed(&long);
    assert!(
        listed >= 17,
        "CONTROL: the long list holds {listed} entries, not 17 or more"
    );
    let held_733 =
        astream_sys::open_nonblocking(&at("h733"), astream_sys::Links::Refuse).expect("hold h733");
    let held_listed = astream_sys::open_nonblocking(&at("h700-allow"), astream_sys::Links::Refuse)
        .expect("hold h700-allow");
    let node = |held: &std::fs::File| PathBuf::from(format!("/dev/fd/{}", held.as_raw_fd()));

    let private_unless_root = |errno| {
        if as_root {
            Ok(DirIs::Private)
        } else {
            refused(errno)
        }
    };
    // Each path, the answer both routes give, and whether its judgement opens it on macOS arm64.
    let rows: Vec<(PathBuf, Said, bool)> = vec![
        (at("missing"), Ok(DirIs::Missing), false),
        (at("missing").join("sub"), Ok(DirIs::Missing), false),
        (file.clone(), Ok(DirIs::NotDir), false),
        (file.join("sub"), refused(libc::ENOTDIR), true),
        // Each one's open is refused (the 0000 file's only where not root); where it is, the
        // kind (`kind_and_owner_of`), not the refusal, is the answer.
        (at("f000"), Ok(DirIs::NotDir), false),
        (socket.clone(), Ok(DirIs::NotDir), false),
        (at("l"), Ok(DirIs::Symlink), false),
        (at("dangling"), Ok(DirIs::Symlink), false),
        (at("q"), Ok(DirIs::NotDir), false),
        (PathBuf::from("/dev/null"), Ok(DirIs::NotDir), false),
        (at("d700"), Ok(DirIs::Private), false),
        (at("d500"), Ok(DirIs::Private), false),
        (at("d755"), Ok(DirIs::Open(0o755)), false),
        (at("d750"), Ok(DirIs::Open(0o750)), false),
        (at("d711"), Ok(DirIs::Open(0o711)), false),
        (at("d300"), private_unless_root(libc::EACCES), true),
        (at("d000"), private_unless_root(libc::EACCES), true),
        (at("allow-list"), Ok(DirIs::Acl), true),
        (at("allow-read"), Ok(DirIs::Acl), true),
        (at("deny-delete"), Ok(DirIs::Private), true),
        (at("deny-list"), private_unless_root(libc::EACCES), true),
        (long.clone(), Ok(DirIs::Private), true),
        // Root searches the locked directory, so its lookup succeeds and decides.
        (
            locked.join("inner"),
            private_unless_root(libc::EACCES),
            !as_root,
        ),
        (
            PathBuf::from("/"),
            Ok(if as_root {
                DirIs::Open(0o755)
            } else {
                DirIs::Others
            }),
            false,
        ),
        (node(&held_733), Ok(DirIs::Open(0o733)), true),
        (node(&held_listed), Ok(DirIs::Acl), true),
        (slashed(&at("l")), Ok(DirIs::Private), false),
        (slashed(&file), refused(libc::ENOTDIR), true),
    ];
    let mut wrong = Vec::new();
    let mut seen = Vec::new();
    for (path, expected, opens) in &rows {
        let (by_path, counted, opened) = judged_both_ways(path);
        let shown = path.display();
        seen.push(format!("{shown}: {by_path:?} / {opened:?} / {counted:?}"));
        if by_path != *expected {
            wrong.push(format!(
                "{shown}: answered {by_path:?}, expected {expected:?}"
            ));
        }
        if by_path != opened {
            wrong.push(format!("{shown}: answered {by_path:?}, opened {opened:?}"));
        }
        let opens = *opens || !cfg!(target_arch = "aarch64");
        let want = super::tally::Directories {
            judged: vec![(path.clone(), 1)],
            opened: opens.then(|| (path.clone(), 1)).into_iter().collect(),
        };
        if counted != want {
            wrong.push(format!(
                "{shown}: judged and opened {counted:?}, expected {want:?}"
            ));
        }
    }
    drop((held_733, held_listed, listening));
    assert!(
        put_back.put_back(),
        "every fixture's mode and list put back"
    );
    assert!(wrong.is_empty(), "U1: {wrong:#?}\nevery row: {seen:#?}");
}

/// LAW U2: a private read judges its directory without opening it. On a 0700 directory with
/// no access list, `read_private`, `JudgedDir::judge` followed by two opens, and
/// `write_private` each judge the directory once and open it to judge never
/// ([`super::tally::directories`]); the write's own flush of the directory is no judgement.
///
/// CONTROL: the same read under an `everyone deny delete` entry, a list the path's
/// attributes leave to the open, returns the file's bytes and opens the directory once.
///
/// NEGATIVE CONTROL, applied by hand once to private_file.rs and reverted: `dir_is` calling
/// `dir_is_opened` whatever the path's attributes answered fails the read's count.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn a_private_read_judges_its_directory_without_opening_it() {
    use super::tally::{self, Directories};
    let root = aterm_tempfile::tempdir().unwrap();
    let mut put_back = PutBack::default();
    let dir = dir_in(root.path(), "state", 0o700);
    file_in(&dir, "token", b"held", 0o600);
    file_in(&dir, "other", b"also", 0o600);
    let judged_once = || Directories {
        judged: vec![(dir.clone(), 1)],
        opened: Vec::new(),
    };
    let (read, seen) = tally::directories(|| read_private(&dir, "token", 64));
    assert_eq!(
        (read.unwrap(), seen),
        (b"held".to_vec(), judged_once()),
        "read_private"
    );
    let (opened, seen) = tally::directories(|| {
        let judged = super::JudgedDir::judge(&dir).unwrap();
        (judged.open("token").is_ok(), judged.open("other").is_ok())
    });
    assert_eq!(
        (opened, seen),
        ((true, true), judged_once()),
        "JudgedDir::judge and two opens"
    );
    let (wrote, seen) = tally::directories(|| write_private(&dir, "written", b"new"));
    wrote.unwrap();
    assert_eq!(seen, judged_once(), "write_private");

    put_back.lists.push(dir.clone());
    chmod(&[
        "+a".as_ref(),
        "everyone deny delete".as_ref(),
        dir.as_os_str(),
    ]);
    let (read, seen) = tally::directories(|| read_private(&dir, "token", 64));
    let control = (read.map_err(|error| error.to_string()), seen);
    assert!(put_back.put_back(), "the directory's access list put back");
    assert_eq!(
        control,
        (
            Ok(b"held".to_vec()),
            Directories {
                judged: vec![(dir.clone(), 1)],
                opened: vec![(dir.clone(), 1)],
            }
        ),
        "CONTROL: a listed directory is opened to be judged"
    );
}
