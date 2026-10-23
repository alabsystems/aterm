// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The owner-only rule on real files: `open_nonblocking`, `owner_private` and
//! `owner_private_dir` (claim cap.secret-file-owner-only, through astream-cap's capfile).

use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use crate::{euid, open_nonblocking, owner_private, owner_private_dir, Links, NotPrivate};

/// A wait for something that must happen: a hang detector, not a latency budget.
const HANG: Duration = Duration::from_secs(60);

/// A scratch directory, 0700, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "astream-sys-owner-{tag}-{}-{n}",
            std::process::id()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .expect("an exclusive scratch directory");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(dir)
    }

    fn file(&self, name: &str, mode: u32) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, b"secret").unwrap();
        chmod(&path, mode);
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn judge(path: &Path) -> Result<std::fs::Metadata, NotPrivate> {
    let file = open_nonblocking(path, Links::Refuse).expect("open");
    owner_private(&file).expect("judged")
}

/// A file of ours at 0600, 0400 or 0700 passes and hands back the descriptor's
/// metadata; any group or other bit is refused as `Mode`, naming the bits. The same
/// file and bytes, only the mode moved: each pass is the control of the refusals.
#[test]
fn owner_private_passes_owner_only_modes_and_names_any_other() {
    let scratch = Scratch::new("mode");
    let path = scratch.file("k", 0o600);
    for (mode, ok) in [
        (0o600, true),
        (0o400, true),
        (0o700, true),
        (0o640, false),
        (0o604, false),
        (0o644, false),
        (0o660, false),
    ] {
        chmod(&path, mode);
        match judge(&path) {
            Ok(meta) => {
                assert!(ok, "{mode:o} passed");
                assert_eq!(meta.len(), 6);
            }
            Err(refused) => {
                assert!(!ok, "{mode:o}: {refused}");
                assert_eq!(refused, NotPrivate::Mode(mode));
                assert_eq!(refused.to_string(), format!("is mode {mode:04o}"));
            }
        }
    }
}

/// The kind is the first clause: a directory is not a file to `owner_private`, and a
/// file is not a directory to `owner_private_dir`, each private by its mode. A
/// directory of ours at 0700 passes `owner_private_dir`, at 0755 is refused as `Mode`.
#[test]
fn the_kind_is_judged_first_and_a_directory_has_its_own_judge() {
    let scratch = Scratch::new("kind");
    let file = scratch.file("k", 0o600);
    let sub = scratch.0.join("sub");
    std::fs::DirBuilder::new().mode(0o700).create(&sub).unwrap();
    chmod(&sub, 0o700);
    let opened = open_nonblocking(&sub, Links::Refuse).expect("a directory opens");
    assert_eq!(
        owner_private(&opened).unwrap().unwrap_err(),
        NotPrivate::Kind("a directory")
    );
    assert!(owner_private_dir(&opened).unwrap().is_ok());
    let opened = open_nonblocking(&file, Links::Refuse).unwrap();
    assert_eq!(
        owner_private_dir(&opened).unwrap().unwrap_err(),
        NotPrivate::Kind("a regular file")
    );
    chmod(&sub, 0o755);
    let opened = open_nonblocking(&sub, Links::Refuse).unwrap();
    assert_eq!(
        owner_private_dir(&opened).unwrap().unwrap_err(),
        NotPrivate::Mode(0o755)
    );
}

/// Another user's file is refused as `Owner`, before its mode: `/` is root's (0755),
/// so a reader that is not root is refused for the owner and root is refused for the
/// mode. Either way it never passes.
#[test]
fn another_users_directory_is_refused_for_its_owner() {
    let root = open_nonblocking(Path::new("/"), Links::Refuse).expect("/ opens");
    let refused = owner_private_dir(&root).unwrap().unwrap_err();
    if euid() == 0 {
        assert_eq!(refused, NotPrivate::Mode(0o755));
    } else {
        assert_eq!(refused, NotPrivate::Owner(0));
        assert_eq!(refused.to_string(), "is owned by uid 0, not this user");
    }
}

/// A FIFO at the path opens AT ONCE, non-blocking, and is refused as one: a plain open
/// would wait for a writer that never comes. The open runs on its own thread behind
/// the one-minute hang detector, so a blocking open fails the test rather than hanging
/// it.
#[test]
fn a_fifo_is_refused_without_waiting_for_a_writer() {
    let scratch = Scratch::new("fifo");
    let fifo = scratch.0.join("k");
    let made = std::process::Command::new("mkfifo")
        .arg("-m")
        .arg("600")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo");
    let (tx, rx) = mpsc::channel();
    let at = fifo.clone();
    std::thread::spawn(move || {
        let opened = open_nonblocking(&at, Links::Refuse).map(|f| owner_private(&f));
        let _ = tx.send(opened);
    });
    let judged = rx
        .recv_timeout(HANG)
        .expect("a non-blocking open of a FIFO returns");
    assert_eq!(
        judged.expect("opened").expect("judged").unwrap_err(),
        NotPrivate::Kind("a FIFO")
    );
}

/// `Links::Refuse` refuses a symlink at the last component and opens nothing through
/// it; `Links::Follow` opens the file it names (the control).
#[test]
fn a_link_is_refused_or_followed_as_asked() {
    let scratch = Scratch::new("link");
    let real = scratch.file("real", 0o600);
    let link = scratch.0.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(open_nonblocking(&link, Links::Refuse).is_err());
    let followed = open_nonblocking(&link, Links::Follow).expect("followed");
    assert!(owner_private(&followed).unwrap().is_ok());
}

/// On macOS an access-list entry that ALLOWS is refused as `Acl` on a 0600 file and a
/// 0700 directory, both private by their mode; `chmod -N` clears it and the same file
/// passes. A DENY entry takes nothing away and passes: the negative control.
#[cfg(target_vendor = "apple")]
#[test]
fn an_access_list_that_allows_others_is_refused() {
    let acl = |args: &[&str], path: &Path| {
        let ran = std::process::Command::new("/bin/chmod")
            .args(args)
            .arg(path)
            .status()
            .expect("run chmod");
        assert!(ran.success(), "chmod {args:?}");
    };
    let scratch = Scratch::new("acl");
    let path = scratch.file("k", 0o600);
    acl(&["+a", "everyone allow read"], &path);
    assert_eq!(judge(&path).unwrap_err(), NotPrivate::Acl);
    acl(&["-N"], &path);
    assert!(judge(&path).is_ok());
    acl(&["+a", "everyone deny write"], &path);
    assert!(judge(&path).is_ok(), "a deny entry passes");

    let sub = scratch.0.join("sub");
    std::fs::DirBuilder::new().mode(0o700).create(&sub).unwrap();
    acl(&["+a", "everyone allow list"], &sub);
    let opened = open_nonblocking(&sub, Links::Refuse).unwrap();
    assert_eq!(
        owner_private_dir(&opened).unwrap().unwrap_err(),
        NotPrivate::Acl
    );
}

/// On macOS the access list is judged BEFORE the mode: a 0755 directory and a 0644 file,
/// each open by its mode bits as well, that carry an `everyone allow read` entry are `Acl`,
/// never `Mode`. A caller that narrows the mode it is told of (aterm-uds's
/// `ensure_private_dir`) relies on it: a `Mode` answer must mean every other clause held,
/// or narrowing the mode would leave the allow entry in place and call the directory
/// private. Control: once `chmod -N` clears the entry, the same directory and file are
/// `Mode`, naming their bits.
#[cfg(target_vendor = "apple")]
#[test]
fn the_access_list_is_judged_before_the_mode() {
    let acl = |args: &[&str], path: &Path| {
        let ran = std::process::Command::new("/bin/chmod")
            .args(args)
            .arg(path)
            .status()
            .expect("run chmod");
        assert!(ran.success(), "chmod {args:?}");
    };
    let scratch = Scratch::new("order");
    let path = scratch.file("k", 0o644);
    let sub = scratch.0.join("sub");
    std::fs::DirBuilder::new().mode(0o700).create(&sub).unwrap();
    chmod(&sub, 0o755);
    for at in [&path, &sub] {
        acl(&["+a", "everyone allow read"], at);
    }
    assert_eq!(judge(&path).unwrap_err(), NotPrivate::Acl);
    let opened = open_nonblocking(&sub, Links::Refuse).unwrap();
    assert_eq!(
        owner_private_dir(&opened).unwrap().unwrap_err(),
        NotPrivate::Acl
    );
    for at in [&path, &sub] {
        acl(&["-N"], at);
    }
    assert_eq!(judge(&path).unwrap_err(), NotPrivate::Mode(0o644));
    let opened = open_nonblocking(&sub, Links::Refuse).unwrap();
    assert_eq!(
        owner_private_dir(&opened).unwrap().unwrap_err(),
        NotPrivate::Mode(0o755)
    );
}
