// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use std::io;
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::Path;
use std::time::Duration;

use super::super::win::tests::{assert_owner_only, junction, open_to_rewrite, scratch};
use super::super::{PrivateFileError, create_private_dir, read_private, write_private};
use super::{DirectoryView, PrivateDirectory};
use crate::acl::{Descriptor, Parts, Sid};

/// The folder at `path` opened as a caller might, to hand [`PrivateDirectory::from_file`].
fn opened(path: &Path) -> std::fs::File {
    const READ_CONTROL: u32 = 0x0002_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
    std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
        .share_mode(0x7)
        .custom_flags(
            crate::acl::FILE_FLAG_BACKUP_SEMANTICS | crate::acl::FILE_FLAG_OPEN_REPARSE_POINT,
        )
        .open(path)
        .expect("the folder opens")
}

fn child_names(view: &DirectoryView<'_>) -> Vec<String> {
    let mut names: Vec<String> = view
        .names()
        .unwrap()
        .collect::<io::Result<Vec<_>>>()
        .unwrap()
        .into_iter()
        .map(|name| name.into_string().unwrap())
        .filter(|name| name != "." && name != "..")
        .collect();
    names.sort();
    names
}

/// An opened folder is the folder its operations reach, whatever its name names since: a write
/// through one renamed away lands there, never in the private folder made at its old name; and
/// `is_at` says which name names it now.
#[test]
fn an_opened_private_folder_never_reopens_its_replaced_name() {
    let (root, path) = scratch();
    let moved = root.path().join("original");
    let original = opened(&path);
    std::fs::rename(&path, &moved).unwrap();
    create_private_dir(&path).unwrap();
    let directory = PrivateDirectory::from_file(original).unwrap();
    directory
        .view()
        .unwrap()
        .write("owned", b"the same original folder")
        .unwrap();
    assert!(!path.join("owned").exists());
    assert_eq!(
        read_private(&moved, "owned", 64).unwrap(),
        b"the same original folder"
    );
    assert_owner_only(&moved.join("owned"), "");
    assert!(directory.is_at(&moved).unwrap());
    assert!(!directory.is_at(&path).unwrap());
    assert!(!directory.is_at(&root.path().join("missing")).unwrap());
    let ordinary = root.path().join("ordinary-file");
    std::fs::write(&ordinary, b"x").unwrap();
    assert!(PrivateDirectory::from_file(opened(&ordinary)).is_err());
}

/// Reads, writes, the listing, removal and named locks all use the opened folder: the folder
/// renamed and a new private one made at its name, each keeps its own rows and its own locks.
/// (Windows renames no folder while a file in it is open, so the locks are taken after.)
#[test]
fn reads_writes_listing_removal_and_locks_stay_on_the_opened_folder() {
    let (root, path) = scratch();
    let retired = root.path().join("retired");
    let held = PrivateDirectory::open(&path).unwrap();
    held.view().unwrap().write("row", b"old").unwrap();
    std::fs::rename(&path, &retired).unwrap();
    create_private_dir(&path).unwrap();
    let replacement = PrivateDirectory::open(&path).unwrap();
    replacement.view().unwrap().write("row", b"new").unwrap();
    assert!(!held.is_at(&path).unwrap());
    assert!(held.is_at(&retired).unwrap());

    let view = held.view().unwrap();
    assert_eq!(view.read("row", 100).unwrap(), b"old");
    view.write("old-only", b"bound").unwrap();
    assert_eq!(child_names(&view), ["old-only", "row"]);
    assert!(replacement.view().unwrap().read("old-only", 100).is_err());
    view.remove("row").unwrap();
    assert!(matches!(
        view.read("row", 100),
        Err(PrivateFileError::Missing)
    ));
    assert_eq!(
        replacement.view().unwrap().read("row", 100).unwrap(),
        b"new"
    );

    let keeper = held.lock(".keeper", Duration::ZERO, "busy").unwrap();
    assert_owner_only(&retired.join(".keeper"), "");
    assert_eq!(
        held.lock(".keeper", Duration::ZERO, "busy")
            .err()
            .map(|e| e.kind()),
        Some(io::ErrorKind::WouldBlock),
        "a lock held excludes a second"
    );
    let other = replacement
        .lock(".keeper", Duration::ZERO, "busy")
        .expect("the other folder's lock is another file");
    drop((keeper, other));
    drop(
        held.lock(".keeper", Duration::ZERO, "busy")
            .expect("let go"),
    );
}

/// A waiter for a held lock is told once, then takes the lock when it is let go.
#[test]
fn a_waiter_takes_the_lock_once_let_go() {
    let (_root, path) = scratch();
    let directory = PrivateDirectory::open(&path).unwrap();
    let first = directory.lock(".lock", Duration::ZERO, "busy").unwrap();
    let (told, heard) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| directory.wait_lock(".lock", move || told.send(()).unwrap()));
        heard
            .recv_timeout(Duration::from_secs(60))
            .expect("the waiter found the lock held");
        drop(first);
        let held = waiter.join().unwrap().expect("taken once let go");
        assert!(directory.lock(".lock", Duration::ZERO, "busy").is_err());
        drop(held);
    });
}

/// NEGATIVE CONTROLS: a junction is no folder of its own, a folder another account may read is
/// no private one (a view taken after the change is refused, and so is every lock), a name in
/// it that is a junction, a second link or a folder is refused, and a name of more than one
/// component, or one a Windows path reads as another file, is refused before anything opens.
/// NEGATIVE CONTROL (2026-11-18): `view` admitting a folder whatever its verdict: the junction
/// opens (and an ordinary file's handle is taken for a folder, in the first test here).
#[test]
fn admission_is_judged_again_and_children_refuse_links_and_folders() {
    let (root, path) = scratch();
    let link = root.path().join("alias");
    junction(&link, &path);
    assert!(PrivateDirectory::open(&link).is_err());
    let plain = root.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    assert!(PrivateDirectory::open(&plain).is_err(), "not private");

    let directory = PrivateDirectory::open(&path).unwrap();
    let view = directory.view().unwrap();
    view.write("row", b"private").unwrap();
    std::fs::hard_link(path.join("row"), path.join("second-link")).unwrap();
    assert!(view.read("row", 100).is_err());
    junction(&path.join("inner"), &plain);
    assert!(matches!(
        view.read("inner", 100),
        Err(PrivateFileError::Symlink)
    ));
    assert!(directory.lock("inner", Duration::ZERO, "busy").is_err());
    std::fs::create_dir(path.join("folder")).unwrap();
    assert!(view.read("folder", 100).is_err());
    assert!(directory.lock("folder", Duration::ZERO, "busy").is_err());
    for name in [
        "",
        ".",
        "..",
        "..\\escape",
        "nested/row",
        "a:b",
        "NUL",
        "nul\0tail",
    ] {
        assert!(view.read(name, 100).is_err(), "{name:?}");
        assert!(view.write(name, b"bytes").is_err(), "{name:?}");
        assert!(view.remove(name).is_err(), "{name:?}");
        assert!(
            directory.lock(name, Duration::ZERO, "busy").is_err(),
            "{name:?}"
        );
    }

    let me = Sid::token_user().unwrap();
    Descriptor::from_sddl(&format!(
        "D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)(A;OICI;FR;;;WD)"
    ))
    .unwrap()
    .apply(open_to_rewrite(&path), Parts::DACL | Parts::PROTECTED_DACL)
    .unwrap();
    assert!(directory.view().is_err());
    assert!(directory.lock("new-lock", Duration::ZERO, "busy").is_err());
    assert!(PrivateDirectory::open(&path).is_err());
    assert!(!path.join("new-lock").exists(), "no lock made");
    std::fs::remove_dir(path.join("inner")).unwrap();
}

/// A held write keeps its stages: a refused `check` publishes nothing and leaves no temporary;
/// a folder flush that fails after the rename is the inner error, with the bytes published; one
/// that is unavailable is accepted by the plain write. On Windows the folder's flush is only
/// noted, so each write records the file's flush and the folder's.
#[test]
fn a_held_write_preserves_its_preparation_and_folder_flush_stages() {
    let (_root, path) = scratch();
    let directory = PrivateDirectory::open(&path).unwrap();
    let view = directory.view().unwrap();
    view.write_entry_landed("row", b"before", || Ok(()))
        .unwrap()
        .unwrap();
    let recording = aterm_durable::seam::Recording::start();
    let refused = view.write_entry_landed("row", b"not published", || {
        Err(io::Error::from(io::ErrorKind::StorageFull))
    });
    assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::StorageFull);
    assert_eq!(view.read("row", 100).unwrap(), b"before");
    assert_eq!(recording.take(), [aterm_durable::Flush::Durable]);
    for errno in [aterm_durable::seam::ENOTSUP, aterm_durable::seam::EIO] {
        let bytes = format!("published {errno}");
        let failure = aterm_durable::seam::FailingDirs::with(errno);
        let landed = view
            .write_entry_landed("row", bytes.as_bytes(), || Ok(()))
            .expect("temporary preparation and rename succeed");
        drop(failure);
        assert_eq!(landed.unwrap_err().raw_os_error(), Some(errno));
        assert_eq!(view.read("row", 100).unwrap(), bytes.as_bytes());
        assert_eq!(recording.take(), [aterm_durable::Flush::Durable; 2]);
    }
    let unavailable = aterm_durable::seam::FailingDirs::with(aterm_durable::seam::ENOTSUP);
    view.write("row", b"best effort caller").unwrap();
    drop(unavailable);
    assert_eq!(view.read("row", 100).unwrap(), b"best effort caller");
    assert_eq!(recording.take(), [aterm_durable::Flush::Durable; 2]);
    view.write("row", b"noted").unwrap();
    assert_eq!(recording.take(), [aterm_durable::Flush::Durable; 2]);
    assert_eq!(child_names(&view), ["row"], "no failed temporary remains");
    assert_owner_only(&path.join("row"), "");
}

/// A lock file made through a view is the user's under the private list from its creation, and
/// one that is there is opened, never replaced.
#[test]
fn a_lock_made_through_a_view_is_private_from_its_creation() {
    let (_root, path) = scratch();
    let directory = PrivateDirectory::open(&path).unwrap();
    let view = directory.view().unwrap();
    drop(view.open_lock(".lock").unwrap());
    assert_owner_only(&path.join(".lock"), "");
    let before = aterm_dirfd::file_id(view.open_lock(".lock").unwrap()).unwrap();
    let again = aterm_dirfd::file_id(view.open_lock(".lock").unwrap()).unwrap();
    assert_eq!(before, again, "the same file");
    write_private(&path, "data", b"d").unwrap();
    let (_file, metadata) = view.open_private("data").unwrap();
    assert_eq!(metadata.len(), 1);
}
