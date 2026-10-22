// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows half of `aterm-dirfd` on real NTFS trees under the temp
//! directory.
//!
//! This is the machinery the disk reclaim deletes build caches with, so the
//! tests are the ways a delete goes wrong on Windows, each staged for real:
//!
//! * **Same facts as std.** Listings, identities, sizes and link counts read
//!   through relative handles agree with `std::fs` over the same tree.
//! * **Nothing is reached through a link.** A junction inside the tree that
//!   points OUTSIDE it is refused by `NOFOLLOW`, refused by `pin`, lists
//!   empty when opened as itself, and is removed AS ITSELF by the tree delete,
//!   which leaves its target's contents untouched. A reparse point that is not
//!   a link (a third-party tag stands in for a cloud placeholder) is kept, and
//!   so is a folder a volume is mounted on (a junction's tag onto a volume's
//!   GUID name), which removing would unmount.
//! * **A real cloud sync root.** Registered on a scratch folder
//!   (`CfRegisterSyncRoot`, no provider, no administrator) with placeholders
//!   made under it: in the default mode, which disguises them as plain
//!   files, every call still sees them as the reparse points they are — a
//!   placeholder is kept, the root refuses `pin` — and the thread's own mode
//!   is put back after each.
//! * **A volume by its own name.** A handle's volume reads as its GUID, the
//!   name two volumes never share where they can share a serial.
//! * **Deletes as unix deletes.** A read-only file goes; a file another handle
//!   holds open WITH delete sharing is unlinked at once (POSIX semantics); one
//!   held WITHOUT delete sharing is refused with a sharing violation and
//!   stays, and the refusal comes back from the one call.
//! * **Identity, not names.** A name swapped between the listing and the
//!   delete, or between the listing and the descent, is refused, and both
//!   survive. A directory is never unlinked as a file, nor a file as a
//!   directory.
//! * **Only "not found" is gone.** A missing drive, volume, path or share
//!   never converts to `io::ErrorKind::NotFound`; neither does a name under a
//!   directory being deleted (delete-pending, by either disposition).
//! * **Local volume roots only.** `pin` refuses a share before opening
//!   anything, and starts only at a volume's own root.
//! * **Long paths.** A tree deeper than `MAX_PATH` is pinned (in either
//!   spelling), listed and deleted although this machine runs with
//!   `LongPathsEnabled=0`.
//!
//! The struct layouts the FFI declares by hand are pinned at compile time in
//! `src/windows/ffi.rs`; a build of this file is a build of those assertions.
//! This file declares no FFI: what it needs beyond the API is
//! `aterm_dirfd::seam`.

#![cfg(windows)]

use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use aterm_dirfd::{AtFlags, Dir, Errno, FileId, Mode, OFlags, OwnedFd};

// Fixtures

/// A scratch directory on the temp volume (NTFS on every machine this runs on).
fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::tempdir().expect("a scratch directory")
}

/// `path`'s canonical, verbatim (`\\?\C:\…`) spelling, as the reclaim gets it.
fn canon(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).expect("canonical")
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("written");
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read")
}

fn gone(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

/// The directory read-only open the reclaim's walk makes.
fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW
}

/// `path`'s plain spelling (`C:\…`): the verbatim prefix dropped.
fn plain(path: &Path) -> String {
    let text = path.to_str().expect("test paths are UTF-8");
    text.strip_prefix(r"\\?\").unwrap_or(text).to_owned()
}

/// What `program args…` prints, spawned through `aterm_uds::spawnfd::command`
/// like every child.
fn run(program: &str, args: &[&str]) -> String {
    let out = aterm_uds::spawnfd::command(program)
        .args(args)
        .output()
        .expect("the program runs");
    assert!(out.status.success(), "{program} {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `link` made a junction to `target` with `cmd /C mklink /J`. `mklink`
/// refuses the verbatim spelling, so the plain one is passed.
fn junction(link: &Path, target: &Path) {
    run("cmd", &["/C", "mklink", "/J", &plain(link), &plain(target)]);
}

/// The names `Dir` lists under `fd`, sorted.
fn listed(fd: &OwnedFd) -> Vec<String> {
    let mut names: Vec<String> = Dir::read_from(fd)
        .expect("listing")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_str()
                .expect("UTF-8")
                .to_owned()
        })
        .collect();
    names.sort();
    names
}

/// The names `std::fs::read_dir` lists at `path`, sorted.
fn std_listed(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .expect("std listing")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .into_string()
                .expect("UTF-8")
        })
        .collect();
    names.sort();
    names
}

/// The listed entry `name` under `fd`.
fn entry(fd: &OwnedFd, name: &str) -> aterm_dirfd::DirEntry {
    Dir::read_from(fd)
        .expect("listing")
        .map(|e| e.expect("entry"))
        .find(|e| e.file_name() == name)
        .unwrap_or_else(|| panic!("{name} listed"))
}

/// The tree delete the disk reclaim makes, in miniature: open `name` under
/// `parent` as a directory without following it and check it is `expect`;
/// remove every listed child (a directory by recursion, anything else by
/// `unlinkat` with the listed identity); then the emptied directory by name.
/// A link or a file at `name` is unlinked as itself. Every refusal is recorded
/// with its code and the rest still tried.
fn remove_tree(parent: &OwnedFd, name: &OsStr, expect: FileId, kept: &mut Vec<(OsString, Errno)>) {
    let dir = match aterm_dirfd::openat(parent, name, dir_flags(), Mode::empty()) {
        Ok(dir) => dir,
        Err(Errno::NOTDIR | Errno::LOOP) => {
            if let Err(e) = aterm_dirfd::unlinkat(parent, name, AtFlags::empty(), expect) {
                kept.push((name.to_owned(), e));
            }
            return;
        }
        Err(e) => return kept.push((name.to_owned(), e)),
    };
    let st = aterm_dirfd::fstat(&dir).expect("stat");
    if st.id() != expect {
        return kept.push((name.to_owned(), Errno::CHANGED));
    }
    let before = kept.len();
    for child in Dir::read_from(&dir).expect("listing") {
        let child = child.expect("entry");
        let id = FileId {
            dev: st.st_dev,
            ino: child.ino(),
        };
        if child.file_type().is_dir() {
            remove_tree(&dir, child.file_name(), id, kept);
        } else if let Err(e) = aterm_dirfd::unlinkat(&dir, child.file_name(), AtFlags::empty(), id)
        {
            kept.push((child.file_name().to_owned(), e));
        }
    }
    match aterm_dirfd::unlinkat(parent, name, AtFlags::REMOVEDIR, expect) {
        Ok(()) => {}
        // Not empty because a child was kept: that child's code is on record.
        Err(Errno::NOTEMPTY) if kept.len() > before => {}
        Err(e) => kept.push((name.to_owned(), e)),
    }
}

/// `remove_tree` of the directory `name` directly under the pinned `root`.
fn remove_named(root: &OwnedFd, name: &str) -> Vec<(OsString, Errno)> {
    let fd = aterm_dirfd::openat(root, name, dir_flags(), Mode::empty()).expect("opened");
    let id = aterm_dirfd::file_id(&fd).expect("identity");
    drop(fd);
    let mut kept = Vec::new();
    remove_tree(root, OsStr::new(name), id, &mut kept);
    kept
}

// Opens, stats, listings

#[test]
fn relative_opens_stats_and_listings_agree_with_std() {
    let tmp = scratch();
    let root = canon(tmp.path());
    write(&root.join("a.txt"), "hello");
    std::fs::hard_link(root.join("a.txt"), root.join("hard.txt")).expect("hard link");
    std::fs::create_dir(root.join("sub")).expect("sub");
    write(&root.join("sub").join("b.bin"), "abc");

    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    // The listing is std's, without `.` and `..`.
    assert_eq!(listed(&pinned), std_listed(&root));
    assert_eq!(listed(&pinned), ["a.txt", "hard.txt", "sub"]);

    let root_st = aterm_dirfd::fstat(&pinned).expect("root stat");
    assert!(root_st.file_type().is_dir());
    let a = aterm_dirfd::openat(
        &pinned,
        "a.txt",
        OFlags::RDONLY | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .expect("a.txt");
    let a_st = aterm_dirfd::fstat(&a).expect("a stat");
    let hard =
        aterm_dirfd::openat(&pinned, "hard.txt", OFlags::RDONLY, Mode::empty()).expect("hard");
    let hard_st = aterm_dirfd::fstat(&hard).expect("hard stat");
    // Two names, one file: one identity, two links, on the directory's volume.
    assert_eq!(a_st.id(), hard_st.id());
    assert_eq!(a_st.st_nlink, 2);
    assert_eq!(a_st.st_dev, root_st.st_dev);
    assert_ne!(a_st.st_ino, root_st.st_ino);
    assert_eq!(aterm_dirfd::file_id(&a).expect("id"), a_st.id());
    assert!(a_st.file_type().is_file());
    // `dev` is the volume's 64-bit serial, whose low half is the 32-bit
    // serial `vol` prints ("Volume Serial Number is 76C4-8470").
    let drive = &plain(&root)[..2];
    let vol = run("cmd", &["/C", "vol", drive]);
    let serial = vol
        .split_whitespace()
        .find_map(|word| {
            let (high, low) = word.split_once('-')?;
            (high.len() == 4 && low.len() == 4)
                .then(|| u32::from_str_radix(&format!("{high}{low}"), 16).ok())
                .flatten()
        })
        .unwrap_or_else(|| panic!("a serial in {vol:?}"));
    assert_eq!(a_st.st_dev as u32, serial, "{:#x} vs {vol}", a_st.st_dev);
    assert_eq!(a_st.size, 5);
    assert!(
        a_st.allocation >= a_st.size || a_st.allocation == 0,
        "{a_st:?}"
    );
    assert_eq!(a_st.reparse_tag, 0);
    assert!(!a_st.delete_pending);
    // The listing carries the same identity and size.
    let listed_a = entry(&pinned, "a.txt");
    assert_eq!(listed_a.ino(), a_st.st_ino);
    assert_eq!(listed_a.size(), 5);
    assert!(listed_a.file_type().is_file());
    assert!(entry(&pinned, "sub").file_type().is_dir());
    // The handle's write time is std's (both read through a handle).
    let std_meta = std::fs::metadata(root.join("a.txt")).expect("std");
    assert_eq!(a_st.modified(), Some(std_meta.modified().expect("mtime")));
    assert_eq!(std_meta.len(), a_st.size);
    // A handle becomes a std File.
    let mut text = String::new();
    std::fs::File::from(a)
        .read_to_string(&mut text)
        .expect("read through the handle");
    assert_eq!(text, "hello");

    // `.` is the directory itself.
    let again = aterm_dirfd::openat(&pinned, ".", dir_flags(), Mode::empty()).expect("reopen");
    assert_eq!(aterm_dirfd::file_id(&again).expect("id"), root_st.id());
    // The unix refusals, as Windows codes.
    let sub = aterm_dirfd::openat(&pinned, "sub", dir_flags(), Mode::empty()).expect("sub");
    assert_eq!(listed(&sub), ["b.bin"]);
    assert_eq!(
        aterm_dirfd::openat(&pinned, "a.txt", dir_flags(), Mode::empty()).err(),
        Some(Errno::NOTDIR)
    );
    let missing = aterm_dirfd::openat(&pinned, "missing", OFlags::RDONLY, Mode::empty()).err();
    assert_eq!(missing, Some(Errno::NOENT));
    assert!(missing.is_some_and(Errno::is_noent));
    assert_eq!(
        aterm_dirfd::openat(&pinned, "sub", OFlags::RDWR, Mode::empty()).err(),
        Some(Errno::ISDIR)
    );
    // A name is one component, or nothing is opened.
    for bad in ["", "..", r"sub\b.bin", "sub/b.bin", "a.txt:stream", "a\0b"] {
        assert_eq!(
            aterm_dirfd::openat(&pinned, bad, OFlags::RDONLY, Mode::empty()).err(),
            Some(Errno::INVAL),
            "{bad:?}"
        );
    }
}

#[test]
fn two_listings_over_one_handle_each_see_every_entry() {
    let tmp = scratch();
    let root = canon(tmp.path());
    // Enough names that one query cannot hold them all.
    let mut want: Vec<String> = (0..1500)
        .map(|i| format!("entry-{i:04}-{}", "x".repeat(40)))
        .collect();
    for name in &want {
        write(&root.join(name), "");
    }
    want.sort();
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let mut one = Dir::read_from(&pinned).expect("one");
    let mut two = Dir::read_from(&pinned).expect("two");
    let (mut seen_one, mut seen_two) = (Vec::new(), Vec::new());
    loop {
        let a = one.next();
        let b = two.next();
        if a.is_none() && b.is_none() {
            break;
        }
        seen_one.extend(a.map(|e| e.expect("entry").file_name().to_string_lossy().into_owned()));
        seen_two.extend(b.map(|e| e.expect("entry").file_name().to_string_lossy().into_owned()));
    }
    seen_one.sort();
    seen_two.sort();
    assert_eq!(seen_one, want);
    assert_eq!(seen_two, want);
    // The caller's handle still resolves names.
    assert!(aterm_dirfd::openat(&pinned, &want[0], OFlags::RDONLY, Mode::empty()).is_ok());
}

#[test]
fn create_opens_and_exclusive_creates() {
    let tmp = scratch();
    let root = canon(tmp.path());
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let flags = OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW;
    let made =
        aterm_dirfd::openat(&pinned, "new.lock", flags, Mode::RUSR | Mode::WUSR).expect("created");
    std::fs::File::from(made).write_all(b"x").expect("written");
    assert_eq!(read(&root.join("new.lock")), "x");
    // Open-or-create opens what is there.
    assert!(aterm_dirfd::openat(&pinned, "new.lock", flags, Mode::empty()).is_ok());
    // Exclusive create refuses it (STATUS_OBJECT_NAME_COLLISION: ERROR_ALREADY_EXISTS).
    assert_eq!(
        aterm_dirfd::openat(&pinned, "new.lock", flags | OFlags::EXCL, Mode::empty()).err(),
        Some(Errno(183))
    );
    // A directory is never created.
    assert_eq!(
        aterm_dirfd::openat(
            &pinned,
            "d",
            OFlags::DIRECTORY | OFlags::CREATE,
            Mode::empty()
        )
        .err(),
        Some(Errno::INVAL)
    );
}

#[test]
fn a_lock_another_handle_holds_is_seen_through_a_relative_open() {
    let tmp = scratch();
    let root = canon(tmp.path());
    let lock = root.join(".cargo-lock");
    write(&lock, "");
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock)
        .expect("holder");
    #[expect(
        clippy::disallowed_methods,
        reason = "the stand-in cargo's lock, released by unlock below before its close"
    )]
    holder.lock().expect("held");
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let probe = std::fs::File::from(
        aterm_dirfd::openat(
            &pinned,
            ".cargo-lock",
            OFlags::RDWR | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .expect("probe"),
    );
    #[expect(
        clippy::disallowed_methods,
        reason = "a probe: it fails here, and is unlocked below on success before its close"
    )]
    let busy = probe.try_lock();
    assert!(
        matches!(busy, Err(std::fs::TryLockError::WouldBlock)),
        "{busy:?}"
    );
    holder.unlock().expect("released");
    #[expect(
        clippy::disallowed_methods,
        reason = "a probe, unlocked on the next line before its close"
    )]
    probe.try_lock().expect("free now");
    probe.unlock().expect("released");
}

/// A facts-only open (`PATH`) reads a file nobody may read the data of (held
/// without read sharing) and a directory, and the owner through either is
/// this process's user — or, elevated, `BUILTIN\Administrators`. A system
/// file's owner (`TrustedInstaller`, a service SID) is neither. `PATH` with a
/// write mode or `CREATE` is refused.
#[test]
fn a_facts_only_open_reads_any_entry_and_its_owner() {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_SHARE_WRITE: u32 = 0x2;

    let tmp = scratch();
    let root = canon(tmp.path());
    let path = root.join("busy.rlib");
    write(&path, "busy");
    std::fs::create_dir(root.join("d")).expect("d");
    let holder = std::fs::OpenOptions::new()
        .write(true)
        .share_mode(FILE_SHARE_WRITE)
        .open(&path)
        .expect("holder");
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    // A data open is refused by the holder; the facts-only open is not.
    assert_eq!(
        aterm_dirfd::openat(&pinned, "busy.rlib", OFlags::RDONLY, Mode::empty()).err(),
        Some(Errno::SHARING_VIOLATION)
    );
    let file = aterm_dirfd::openat(
        &pinned,
        "busy.rlib",
        OFlags::PATH | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .expect("facts");
    assert_eq!(aterm_dirfd::fstat(&file).expect("stat").size, 4);
    let dir = aterm_dirfd::openat(
        &pinned,
        "d",
        OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .expect("facts of a directory");
    let me = aterm_dirfd::Sid::current_user().expect("this process's user");
    let admins = aterm_dirfd::Sid::administrators();
    assert_eq!(admins.to_string(), "S-1-5-32-544");
    assert!(me.to_string().starts_with("S-1-5-"), "{me}");
    for fd in [&file, &dir] {
        let owner = aterm_dirfd::owner(fd).expect("owner");
        assert!(
            owner == me || owner == admins,
            "{owner} is neither {me} nor {admins}"
        );
    }
    // A directory's listing open carries no READ_CONTROL: its owner is not
    // read through it.
    let listing = aterm_dirfd::openat(&pinned, "d", dir_flags(), Mode::empty()).expect("d");
    assert_eq!(
        aterm_dirfd::owner(&listing).err(),
        Some(Errno::ACCESS_DENIED)
    );
    // PATH asks no write right and creates nothing.
    for flags in [OFlags::PATH | OFlags::RDWR, OFlags::PATH | OFlags::CREATE] {
        assert_eq!(
            aterm_dirfd::openat(&pinned, "new", flags, Mode::empty()).err(),
            Some(Errno::INVAL)
        );
    }
    drop(holder);

    // ntdll.dll belongs to TrustedInstaller (a service SID, S-1-5-80-…).
    let system = canon(
        &std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
            .join("System32"),
    );
    let system = aterm_dirfd::pin(&system).expect("System32");
    let ntdll = aterm_dirfd::openat(&system, "ntdll.dll", OFlags::PATH, Mode::empty())
        .expect("ntdll's facts");
    let theirs = aterm_dirfd::owner(&ntdll).expect("ntdll's owner");
    assert!(theirs != me && theirs != admins, "{theirs}");
    assert!(theirs.to_string().starts_with("S-1-5-80-"), "{theirs}");
}

// Deletion

#[test]
fn a_tree_with_a_read_only_file_is_deleted() {
    let tmp = scratch();
    let root = canon(tmp.path());
    let t = root.join("t");
    std::fs::create_dir_all(t.join("d").join("e")).expect("tree");
    std::fs::create_dir(t.join("empty")).expect("empty dir");
    write(&t.join("d").join("e").join("f.txt"), "f");
    let ro = t.join("d").join("ro.txt");
    write(&ro, "read-only");
    let mut perms = std::fs::metadata(&ro).expect("meta").permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&ro, perms).expect("read-only");

    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let kept = remove_named(&pinned, "t");
    assert!(kept.is_empty(), "{kept:?}");
    assert!(gone(&t));
}

#[test]
fn each_unlink_removes_only_the_kind_its_flags_name() {
    let tmp = scratch();
    let root = canon(tmp.path());
    std::fs::create_dir(root.join("empty")).expect("empty");
    std::fs::create_dir(root.join("full")).expect("full");
    write(&root.join("full").join("x"), "x");
    write(&root.join("f.txt"), "f");
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let dev = aterm_dirfd::file_id(&pinned).expect("root").dev;
    let id = |name: &str| FileId {
        dev,
        ino: entry(&pinned, name).ino(),
    };
    // A real directory is not unlinked as a file, even empty — a POSIX
    // delete through a non-directory open would take it, `unlink(2)` does not.
    assert_eq!(
        aterm_dirfd::unlinkat(&pinned, "empty", AtFlags::empty(), id("empty")),
        Err(Errno::ISDIR)
    );
    assert!(root.join("empty").is_dir());
    // A file is not removed as a directory.
    assert_eq!(
        aterm_dirfd::unlinkat(&pinned, "f.txt", AtFlags::REMOVEDIR, id("f.txt")),
        Err(Errno::NOTDIR)
    );
    assert_eq!(read(&root.join("f.txt")), "f");
    // A directory with something in it stays.
    assert_eq!(
        aterm_dirfd::unlinkat(&pinned, "full", AtFlags::REMOVEDIR, id("full")),
        Err(Errno::NOTEMPTY)
    );
    assert_eq!(read(&root.join("full").join("x")), "x");
    // Each by its own flags goes.
    aterm_dirfd::unlinkat(&pinned, "empty", AtFlags::REMOVEDIR, id("empty")).expect("rmdir");
    aterm_dirfd::unlinkat(&pinned, "f.txt", AtFlags::empty(), id("f.txt")).expect("unlink");
    assert!(gone(&root.join("empty")) && gone(&root.join("f.txt")));
}

#[test]
fn a_file_held_open_with_delete_sharing_is_unlinked_at_once() {
    let tmp = scratch();
    let root = canon(tmp.path());
    let d = root.join("d");
    std::fs::create_dir(&d).expect("d");
    let path = d.join("held.log");
    // std opens with FILE_SHARE_READ | WRITE | DELETE.
    let mut holder = std::fs::File::create(&path).expect("holder");
    holder.write_all(b"still mine").expect("written");

    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let dir = aterm_dirfd::openat(&pinned, "d", dir_flags(), Mode::empty()).expect("d");
    let dir_id = aterm_dirfd::file_id(&dir).expect("id");
    let listed_held = entry(&dir, "held.log");
    let id = FileId {
        dev: dir_id.dev,
        ino: listed_held.ino(),
    };
    aterm_dirfd::unlinkat(&dir, "held.log", AtFlags::empty(), id).expect("unlinked while held");
    // The name is gone now, not at the holder's close (POSIX semantics)...
    assert!(gone(&path));
    assert!(listed(&dir).is_empty());
    // ...the holder keeps its file...
    holder
        .write_all(b" and more")
        .expect("the holder still writes");
    assert_eq!(holder.metadata().expect("holder meta").len(), 19);
    // ...and the emptied directory goes too, the file still open inside it.
    drop(dir);
    aterm_dirfd::unlinkat(&pinned, "d", AtFlags::REMOVEDIR, dir_id).expect("rmdir");
    assert!(gone(&d));
}

#[test]
fn a_file_held_open_without_delete_sharing_is_refused_and_stays() {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_SHARE_READ_WRITE: u32 = 0x1 | 0x2;

    let tmp = scratch();
    let root = canon(tmp.path());
    let path = root.join("busy.rlib");
    write(&path, "busy");
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ_WRITE)
        .open(&path)
        .expect("holder");
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let id = FileId {
        dev: aterm_dirfd::file_id(&pinned).expect("root").dev,
        ino: entry(&pinned, "busy.rlib").ino(),
    };
    // Refused by the open, returned from this one call: no retry, no wait.
    assert_eq!(
        aterm_dirfd::unlinkat(&pinned, "busy.rlib", AtFlags::empty(), id),
        Err(Errno::SHARING_VIOLATION)
    );
    assert_eq!(read(&path), "busy");
    // A tree delete records it and removes the rest, leaving the parent.
    std::fs::create_dir(root.join("t")).expect("t");
    write(&root.join("t").join("free.txt"), "free");
    let inner = root.join("t").join("busy.txt");
    write(&inner, "busy");
    let inner_holder = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ_WRITE)
        .open(&inner)
        .expect("inner holder");
    let kept = remove_named(&pinned, "t");
    assert_eq!(
        kept,
        [(OsString::from("busy.txt"), Errno::SHARING_VIOLATION)]
    );
    assert!(gone(&root.join("t").join("free.txt")));
    assert_eq!(read(&inner), "busy");
    // Once the holders let go, the deletes go through.
    drop((holder, inner_holder));
    aterm_dirfd::unlinkat(&pinned, "busy.rlib", AtFlags::empty(), id).expect("unlinked");
    assert!(remove_named(&pinned, "t").is_empty());
    assert!(gone(&root.join("t")));
}

#[test]
fn a_junction_inside_the_tree_is_removed_as_a_link_and_its_target_survives() {
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

    let tmp = scratch();
    let root = canon(tmp.path());
    let outside = root.join("outside");
    std::fs::create_dir(&outside).expect("outside");
    write(&outside.join("keep.txt"), "precious");
    let t = root.join("t");
    std::fs::create_dir(&t).expect("t");
    write(&t.join("inner.txt"), "inner");
    junction(&t.join("j"), &outside);

    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let tfd = aterm_dirfd::openat(&pinned, "t", dir_flags(), Mode::empty()).expect("t");
    // Listed as a link, by its tag.
    let j = entry(&tfd, "j");
    assert!(j.file_type().is_link(), "{j:?}");
    assert!(!j.file_type().is_dir());
    assert_eq!(j.reparse_tag(), IO_REPARSE_TAG_MOUNT_POINT);
    // Refused by NOFOLLOW, as ELOOP.
    assert_eq!(
        aterm_dirfd::openat(&tfd, "j", dir_flags(), Mode::empty()).err(),
        Some(Errno::LOOP)
    );
    // Opened without NOFOLLOW it is the junction itself: its own tag, and none
    // of its target's entries.
    let itself = aterm_dirfd::openat(&tfd, "j", OFlags::DIRECTORY, Mode::empty()).expect("itself");
    let itself_st = aterm_dirfd::fstat(&itself).expect("stat");
    assert_eq!(itself_st.reparse_tag, IO_REPARSE_TAG_MOUNT_POINT);
    assert_eq!(itself_st.st_ino, j.ino());
    assert_ne!(
        itself_st.st_ino,
        aterm_dirfd::fstat(aterm_dirfd::pin(&outside).expect("outside"))
            .expect("stat")
            .st_ino
    );
    assert!(listed(&itself).is_empty());
    drop(itself);
    // pin never walks through it.
    assert_eq!(aterm_dirfd::pin(&t.join("j")).err(), Some(Errno::LOOP));
    // rmdir of a link is ENOTDIR, as on unix.
    let id = FileId {
        dev: itself_st.st_dev,
        ino: j.ino(),
    };
    assert_eq!(
        aterm_dirfd::unlinkat(&tfd, "j", AtFlags::REMOVEDIR, id),
        Err(Errno::NOTDIR)
    );
    drop(tfd);
    // The tree delete removes the link as itself; the target keeps everything.
    let kept = remove_named(&pinned, "t");
    assert!(kept.is_empty(), "{kept:?}");
    assert!(gone(&t));
    assert_eq!(read(&outside.join("keep.txt")), "precious");
    assert_eq!(std_listed(&outside), ["keep.txt"]);
}

#[test]
fn a_volume_mount_point_is_kept_while_a_junction_of_its_tag_goes_as_a_link() {
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

    let tmp = scratch();
    let root = canon(tmp.path());
    let outside = root.join("outside");
    std::fs::create_dir(&outside).expect("outside");
    write(&outside.join("keep.txt"), "precious");
    let t = root.join("t");
    std::fs::create_dir(&t).expect("t");
    write(&t.join("inner.txt"), "inner");
    // A folder a volume is mounted on (a volume no machine has, so nothing is
    // mounted or reachable through it), and a junction: one tag, a volume's
    // GUID name or a path for the target.
    mount_point::make(&t.join("mnt"), mount_point::NO_VOLUME);
    mount_point::make(&t.join("j"), &format!(r"\??\{}", plain(&outside)));

    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let tfd = aterm_dirfd::openat(&pinned, "t", dir_flags(), Mode::empty()).expect("t");
    let dev = aterm_dirfd::file_id(&tfd).expect("identity").dev;
    let (mnt, j) = (entry(&tfd, "mnt"), entry(&tfd, "j"));
    // Listed alike: a link, by its tag, never entered.
    for listed in [&mnt, &j] {
        assert!(listed.file_type().is_link(), "{listed:?}");
        assert_eq!(listed.reparse_tag(), IO_REPARSE_TAG_MOUNT_POINT);
    }
    assert_eq!(
        aterm_dirfd::openat(&tfd, "mnt", dir_flags(), Mode::empty()).err(),
        Some(Errno::LOOP)
    );
    // Unlinked apart: the mount point is refused and stays; the junction goes
    // as itself and its target keeps everything.
    let id = |listed: &aterm_dirfd::DirEntry| FileId {
        dev,
        ino: listed.ino(),
    };
    assert_eq!(
        aterm_dirfd::unlinkat(&tfd, "mnt", AtFlags::empty(), id(&mnt)),
        Err(Errno::MOUNT_POINT)
    );
    assert!(!gone(&t.join("mnt")));
    assert_eq!(
        aterm_dirfd::unlinkat(&tfd, "j", AtFlags::empty(), id(&j)),
        Ok(())
    );
    assert!(gone(&t.join("j")));
    assert_eq!(read(&outside.join("keep.txt")), "precious");
    // Said as what it is, and never "gone".
    let said = std::io::Error::from(Errno::MOUNT_POINT);
    assert_eq!(said.kind(), std::io::ErrorKind::Other);
    assert!(said.to_string().contains("volume is mounted"), "{said}");
    drop(tfd);
    // The tree delete keeps it, and the directory holding it.
    let kept = remove_named(&pinned, "t");
    assert_eq!(kept, [(OsString::from("mnt"), Errno::MOUNT_POINT)]);
    assert!(gone(&t.join("inner.txt")));
    assert!(!gone(&t.join("mnt")));
}

#[test]
fn a_reparse_point_that_is_not_a_link_is_kept() {
    let tmp = scratch();
    let root = canon(tmp.path());
    let t = root.join("t");
    std::fs::create_dir(&t).expect("t");
    write(&t.join("plain.txt"), "plain");
    let placeholder = t.join("placeholder.dat");
    write(&placeholder, "");
    foreign::tag(&placeholder);

    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let tfd = aterm_dirfd::openat(&pinned, "t", dir_flags(), Mode::empty()).expect("t");
    let listed_placeholder = entry(&tfd, "placeholder.dat");
    assert!(listed_placeholder.file_type().is_foreign_reparse());
    assert_eq!(listed_placeholder.reparse_tag(), foreign::TAG);
    assert_eq!(
        aterm_dirfd::openat(
            &tfd,
            "placeholder.dat",
            OFlags::RDONLY | OFlags::NOFOLLOW,
            Mode::empty()
        )
        .err(),
        Some(Errno::FOREIGN_REPARSE)
    );
    // Opened as itself without NOFOLLOW.
    let itself = aterm_dirfd::openat(&tfd, "placeholder.dat", OFlags::RDONLY, Mode::empty())
        .expect("itself");
    assert!(
        aterm_dirfd::fstat(&itself)
            .expect("stat")
            .file_type()
            .is_foreign_reparse()
    );
    drop((itself, tfd));
    // The tree delete keeps it, and the directory holding it.
    let kept = remove_named(&pinned, "t");
    assert_eq!(
        kept,
        [(OsString::from("placeholder.dat"), Errno::FOREIGN_REPARSE)]
    );
    assert!(!gone(&placeholder));
    assert!(gone(&t.join("plain.txt")));
}

/// `IO_REPARSE_TAG_CLOUD`'s family: `0x9000_x01A`, the nibble `x` a flag
/// set (`IO_REPARSE_TAG_CLOUD_1` … `_F`).
fn is_cloud_tag(tag: u32) -> bool {
    tag & 0xFFFF_0FFF == 0x9000_001A
}

/// `FILE_ATTRIBUTE_REPARSE_POINT`.
const REPARSE_POINT: u32 = 0x0400;

/// A REAL SYNC ROOT, ITS PLACEHOLDERS KEPT. A scratch folder registered as a
/// cloud sync root (`CfRegisterSyncRoot`, as OneDrive registers its folder;
/// no provider connected) holds a cargo-shaped tree made placeholders, as a
/// sync client leaves what it uploaded. This test thread runs in the default
/// placeholder mode, which disguises a placeholder inside a sync root as a
/// plain file — yet every call sees each as the reparse point of the cloud
/// tag it is: listed and read through a handle as one, refused by
/// `NOFOLLOW`, kept by `unlinkat` (`FOREIGN_REPARSE`, the file intact), and
/// `pin` refuses the root and every path under it. The thread's own mode is
/// back after each call. NEGATIVE CONTROL: left disguised
/// (`seam::leave_placeholders_disguised`), the same placeholder reads as a
/// plain file with tag 0 and `pin` walks through the root — what the delete
/// would have trusted. The root is unregistered on drop (`seam::SyncRoot`),
/// and nothing of it is left: read through exposed handles, the root, the
/// folders and the file under it carry no reparse point, and the file keeps
/// its bytes. (`CfRegisterSyncRoot` writes no registry entry here, measured
/// 2026-10-20, so the registry has nothing to say of residue: what a root
/// left registered leaves is the reparse point on its folder.)
#[test]
fn a_real_sync_roots_placeholders_are_kept_and_the_root_is_never_pinned() {
    let tmp = scratch();
    let root = canon(tmp.path());
    let sync = root.join("sync");
    let inc = sync
        .join("proj")
        .join("target")
        .join("debug")
        .join("incremental");
    std::fs::create_dir_all(&inc).expect("the tree");
    let file = inc.join("query-cache.bin");
    write(&file, "synced bytes");
    let synced = aterm_dirfd::seam::SyncRoot::register(&sync).expect("a sync root");
    for made in [
        sync.join("proj"),
        sync.join("proj").join("target"),
        sync.join("proj").join("target").join("debug"),
        inc.clone(),
        file.clone(),
    ] {
        synced.convert(&made).expect("a placeholder");
    }
    let mode = aterm_dirfd::seam::thread_placeholder_mode();
    assert_eq!(mode, 0, "the test thread runs in the process's default");

    // The root and every path under it refuse `pin`.
    for under in [&sync, &inc] {
        assert_eq!(
            aterm_dirfd::pin(under).err(),
            Some(Errno::FOREIGN_REPARSE),
            "{}",
            under.display()
        );
    }
    assert_eq!(aterm_dirfd::seam::thread_placeholder_mode(), mode);
    // Reached as itself, one component at a time (no NOFOLLOW).
    let pinned = aterm_dirfd::pin(&root).expect("the scratch root");
    let mut fd = pinned;
    for name in ["sync", "proj", "target", "debug", "incremental"] {
        fd = aterm_dirfd::openat(&fd, name, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())
            .expect(name);
        let st = aterm_dirfd::fstat(&fd).expect("stat");
        assert!(st.file_type().is_foreign_reparse(), "{name}: {st:?}");
        assert!(
            is_cloud_tag(st.reparse_tag),
            "{name}: {:#x}",
            st.reparse_tag
        );
    }
    let listed_file = entry(&fd, "query-cache.bin");
    assert!(
        listed_file.file_type().is_foreign_reparse(),
        "{listed_file:?}"
    );
    assert!(is_cloud_tag(listed_file.reparse_tag()), "{listed_file:?}");
    assert_eq!(
        aterm_dirfd::openat(
            &fd,
            "query-cache.bin",
            OFlags::RDONLY | OFlags::NOFOLLOW,
            Mode::empty()
        )
        .err(),
        Some(Errno::FOREIGN_REPARSE)
    );
    let dev = aterm_dirfd::file_id(&fd).expect("identity").dev;
    let id = FileId {
        dev,
        ino: listed_file.ino(),
    };
    assert_eq!(
        aterm_dirfd::unlinkat(&fd, "query-cache.bin", AtFlags::empty(), id).err(),
        Some(Errno::FOREIGN_REPARSE)
    );
    assert_eq!(read(&file), "synced bytes", "the synced file stays");
    assert_eq!(aterm_dirfd::seam::thread_placeholder_mode(), mode);
    // The guard itself: exposed while it lives, the old mode back after.
    let exposed = aterm_dirfd::expose_placeholders().expect("exposed");
    assert_eq!(aterm_dirfd::seam::thread_placeholder_mode(), 2);
    drop(exposed);
    assert_eq!(aterm_dirfd::seam::thread_placeholder_mode(), mode);

    // NEGATIVE CONTROL: the default mode disguises all of it.
    aterm_dirfd::seam::leave_placeholders_disguised(true);
    let disguised = aterm_dirfd::fstat(
        aterm_dirfd::openat(&fd, "query-cache.bin", OFlags::PATH, Mode::empty()).expect("opened"),
    )
    .expect("stat");
    let walked = aterm_dirfd::pin(&inc).is_ok();
    aterm_dirfd::seam::leave_placeholders_disguised(false);
    assert_eq!(disguised.reparse_tag, 0, "{disguised:?}");
    assert!(disguised.file_type().is_file(), "{disguised:?}");
    assert!(walked, "pin walks through a disguised sync root");
    drop(fd);

    drop(synced);
    // No residue: every level of the tree reads plain through exposed handles.
    let sync_fd = aterm_dirfd::pin(&sync).expect("the root a plain folder again");
    let inc_fd = aterm_dirfd::pin(&inc).expect("a plain folder again");
    let file_fd = aterm_dirfd::openat(&inc_fd, "query-cache.bin", OFlags::PATH, Mode::empty())
        .expect("the file opened");
    for (what, fd) in [("root", &sync_fd), ("folder", &inc_fd), ("file", &file_fd)] {
        let st = aterm_dirfd::fstat(fd).expect("stat");
        assert_eq!(
            (st.reparse_tag, st.attributes & REPARSE_POINT),
            (0, 0),
            "{what}: {st:?}"
        );
    }
    assert_eq!(read(&file), "synced bytes", "the file keeps its bytes");
}

/// A HANDLE NAMES ITS VOLUME BY ITS GUID: `volume_guid` reads `{…}`, lowercase,
/// the same through any handle on the volume (the scratch root, a file in
/// it, the volume's own root). The reading of the system's answer takes only
/// a `\\?\Volume{…}\` path with a GUID of the right shape, in any case;
/// NEGATIVE CONTROLS: a drive-letter path, a GUID too short, one with a
/// letter that is no hexadecimal digit, one missing its brace.
#[test]
fn a_handle_names_its_volume_by_its_guid() {
    let tmp = scratch();
    let root = canon(tmp.path());
    write(&root.join("f.txt"), "f");
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let guid = aterm_dirfd::volume_guid(&pinned).expect("the volume's GUID");
    assert_eq!(guid.len(), 38, "{guid}");
    assert!(guid.starts_with('{') && guid.ends_with('}'), "{guid}");
    assert_eq!(guid, guid.to_ascii_lowercase());
    let file = aterm_dirfd::openat(&pinned, "f.txt", OFlags::PATH, Mode::empty()).expect("file");
    assert_eq!(aterm_dirfd::volume_guid(&file), Ok(guid.clone()));
    let drive = Path::new(&plain(&root)[..3]).to_path_buf();
    let volume_root = aterm_dirfd::pin(&drive).expect("the volume's root");
    assert_eq!(aterm_dirfd::volume_guid(&volume_root), Ok(guid));

    use aterm_dirfd::seam::guid_of_volume_path as read;
    assert_eq!(
        read(r"\\?\Volume{6140FC3B-044f-43d7-b060-cb98170c7a63}\Users\x").as_deref(),
        Some("{6140fc3b-044f-43d7-b060-cb98170c7a63}")
    );
    assert_eq!(
        read(r"\\?\volume{6140fc3b-044f-43d7-b060-cb98170c7a63}\").as_deref(),
        Some("{6140fc3b-044f-43d7-b060-cb98170c7a63}")
    );
    for not_one in [
        r"\\?\C:\Users\x",
        r"\\?\Volume{6140fc3b-044f-43d7-b060-cb98170c7a6}\",
        r"\\?\Volume{6140fc3b-044f-43d7-b060-cb98170c7a6g}\",
        r"\\?\Volume{6140fc3b-044f-43d7-b060-cb98170c7a63\",
        r"\\?\Volume{6140fc3b0044f-43d7-b060-cb98170c7a63}\",
    ] {
        assert_eq!(read(not_one), None, "{not_one}");
    }
}

#[test]
fn a_name_swapped_after_the_listing_is_refused_and_both_files_stay() {
    let tmp = scratch();
    let root = canon(tmp.path());
    write(&root.join("x"), "listed");
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let dev = aterm_dirfd::file_id(&pinned).expect("root").dev;
    let listed_x = entry(&pinned, "x");
    // Between the listing and the delete, another file takes the name.
    std::fs::rename(root.join("x"), root.join("y")).expect("renamed aside");
    write(&root.join("x"), "swapped in");
    let expect = FileId {
        dev,
        ino: listed_x.ino(),
    };
    assert_eq!(
        aterm_dirfd::unlinkat(&pinned, "x", AtFlags::empty(), expect),
        Err(Errno::CHANGED)
    );
    assert_eq!(read(&root.join("x")), "swapped in");
    assert_eq!(read(&root.join("y")), "listed");
    // The same for a directory.
    std::fs::create_dir(root.join("d")).expect("d");
    let d_id = FileId {
        dev,
        ino: entry(&pinned, "d").ino(),
    };
    std::fs::rename(root.join("d"), root.join("d-aside")).expect("renamed aside");
    std::fs::create_dir(root.join("d")).expect("another d");
    assert_eq!(
        aterm_dirfd::unlinkat(&pinned, "d", AtFlags::REMOVEDIR, d_id),
        Err(Errno::CHANGED)
    );
    assert!(root.join("d").is_dir() && root.join("d-aside").is_dir());
    // And a vanished name is gone, ERROR_FILE_NOT_FOUND.
    assert_eq!(
        aterm_dirfd::unlinkat(&pinned, "never", AtFlags::empty(), expect),
        Err(Errno::NOENT)
    );
}

#[test]
fn a_directory_swapped_before_the_descent_is_not_entered() {
    let tmp = scratch();
    let root = canon(tmp.path());
    std::fs::create_dir(root.join("d")).expect("d");
    write(&root.join("d").join("listed.txt"), "listed");
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    // The identity the listing gave...
    let enumerated = FileId {
        dev: aterm_dirfd::file_id(&pinned).expect("root").dev,
        ino: entry(&pinned, "d").ino(),
    };
    // ...and, before the walk opens the name, another directory has it.
    std::fs::rename(root.join("d"), root.join("d-aside")).expect("renamed aside");
    std::fs::create_dir(root.join("d")).expect("another d");
    write(&root.join("d").join("swapped-in.txt"), "not ours");
    // The walk checks the opened child against the ENUMERATED identity before
    // descending (module docs, "A tree delete"), so nothing in either is touched.
    let mut kept = Vec::new();
    remove_tree(&pinned, OsStr::new("d"), enumerated, &mut kept);
    assert_eq!(kept, [(OsString::from("d"), Errno::CHANGED)]);
    assert_eq!(read(&root.join("d").join("swapped-in.txt")), "not ours");
    assert_eq!(read(&root.join("d-aside").join("listed.txt")), "listed");
}

#[test]
fn a_directory_deleted_under_its_handle_lists_as_empty() {
    let tmp = scratch();
    let root = canon(tmp.path());
    std::fs::create_dir(root.join("d")).expect("d");
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let d = aterm_dirfd::openat(&pinned, "d", dir_flags(), Mode::empty()).expect("d");
    let id = aterm_dirfd::file_id(&d).expect("id");
    aterm_dirfd::unlinkat(&pinned, "d", AtFlags::REMOVEDIR, id).expect("removed");
    assert!(gone(&root.join("d")));
    let listing: Vec<_> = Dir::read_from(&d).expect("a listing").collect();
    assert!(listing.is_empty(), "{listing:?}");
    // Nothing can be made in it: a name under it is delete-pending, not
    // absent (so never read as "gone").
    assert_eq!(
        aterm_dirfd::openat(&d, "new", OFlags::RDWR | OFlags::CREATE, Mode::empty()).err(),
        Some(Errno::DELETE_PENDING)
    );
}

#[test]
fn a_directory_whose_legacy_delete_is_pending_lists_as_empty() {
    let tmp = scratch();
    let root = canon(tmp.path());
    std::fs::create_dir(root.join("d")).expect("d");
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let held = aterm_dirfd::openat(&pinned, "d", dir_flags(), Mode::empty()).expect("d");
    let id = aterm_dirfd::file_id(&held).expect("id");
    // The legacy disposition: deleted when the last handle closes, and
    // `held` keeps it open — delete-pending.
    aterm_dirfd::seam::legacy_classes(true);
    let removed = aterm_dirfd::unlinkat(&pinned, "d", AtFlags::REMOVEDIR, id);
    aterm_dirfd::seam::legacy_classes(false);
    removed.expect("delete pending");
    assert!(aterm_dirfd::fstat(&held).expect("stat").delete_pending);
    // Its name is refused as delete-pending, never as absent...
    assert_eq!(
        aterm_dirfd::openat(&pinned, "d", dir_flags(), Mode::empty()).err(),
        Some(Errno::DELETE_PENDING)
    );
    // ...nothing can be made in it...
    assert_eq!(
        aterm_dirfd::openat(&held, "new", OFlags::RDWR | OFlags::CREATE, Mode::empty()).err(),
        Some(Errno::DELETE_PENDING)
    );
    // ...and a listing over the held handle is not an error: NTFS re-opens
    // it by the empty relative name (measured 2026-10-19), and it lists
    // nothing, as a removed directory does.
    let listing: Vec<_> = Dir::read_from(&held)
        .expect("a listing of a delete-pending directory")
        .collect();
    assert!(listing.is_empty(), "{listing:?}");
    // The last handle closes; the directory goes.
    drop(held);
    assert!(gone(&root.join("d")));
}

#[test]
fn a_tree_past_max_path_is_pinned_listed_and_deleted() {
    let tmp = scratch();
    let root = canon(tmp.path());
    let mut deep = root.join("t");
    for level in 0..6 {
        deep.push(format!("level-{level}-{}", "d".repeat(48)));
    }
    std::fs::create_dir_all(&deep).expect("deep tree");
    let file = deep.join("artifact.rlib");
    write(&file, "deep");
    assert!(
        file.as_os_str().len() > 320,
        "{} chars",
        file.as_os_str().len()
    );

    let leaf = aterm_dirfd::pin(&deep).expect("pinned past MAX_PATH");
    assert_eq!(listed(&leaf), ["artifact.rlib"]);
    // The plain spelling too, which a Win32 open of the whole path would
    // refuse with LongPathsEnabled=0: only the root goes to Win32, verbatim.
    let plain_deep = PathBuf::from(plain(&deep));
    assert!(!plain_deep.as_os_str().to_string_lossy().starts_with(r"\\"));
    let leaf_again = aterm_dirfd::pin(&plain_deep).expect("plain spelling pinned past MAX_PATH");
    assert_eq!(
        aterm_dirfd::file_id(&leaf_again).expect("id"),
        aterm_dirfd::file_id(&leaf).expect("id")
    );
    drop(leaf_again);
    let made = aterm_dirfd::openat(
        &leaf,
        "more.d",
        OFlags::WRONLY | OFlags::CREATE,
        Mode::empty(),
    )
    .expect("created past MAX_PATH");
    drop(made);
    assert!(deep.join("more.d").is_file());
    drop(leaf);
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let kept = remove_named(&pinned, "t");
    assert!(kept.is_empty(), "{kept:?}");
    assert!(gone(&root.join("t")));
}

#[test]
fn the_legacy_classes_list_and_delete_as_the_extended_ones() {
    let tmp = scratch();
    let root = canon(tmp.path());
    write(&root.join("a.txt"), "a");
    std::fs::create_dir(root.join("sub")).expect("sub");
    let ro = root.join("ro.txt");
    write(&ro, "ro");
    let mut perms = std::fs::metadata(&ro).expect("meta").permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&ro, perms).expect("read-only");
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let mut dir = Dir::read_from(&pinned).expect("listing");
    let extended: Vec<_> = dir.by_ref().map(|e| e.expect("entry")).collect();
    assert_eq!(
        aterm_dirfd::seam::listing_class(&dir),
        "FileIdExtdDirectoryInfo"
    );

    aterm_dirfd::seam::legacy_classes(true);
    let mut dir = Dir::read_from(&pinned).expect("listing");
    let legacy: Vec<_> = dir.by_ref().map(|e| e.expect("entry")).collect();
    // Read with the other class, so the equality below compares two readings.
    assert_eq!(
        aterm_dirfd::seam::listing_class(&dir),
        "FileIdBothDirectoryInfo"
    );
    // FileIdBothDirectoryInfo: the same names, ids (NTFS's 64-bit reference,
    // zero-extended), kinds and sizes.
    assert_eq!(legacy, extended);
    let dev = aterm_dirfd::file_id(&pinned).expect("root").dev;
    let id = |name: &str| FileId {
        dev,
        ino: entry(&pinned, name).ino(),
    };
    // The legacy disposition deletes a plain file, and refuses a read-only one.
    aterm_dirfd::unlinkat(&pinned, "a.txt", AtFlags::empty(), id("a.txt")).expect("unlinked");
    assert!(gone(&root.join("a.txt")));
    assert_eq!(
        aterm_dirfd::unlinkat(&pinned, "ro.txt", AtFlags::empty(), id("ro.txt")),
        Err(Errno::ACCESS_DENIED)
    );
    assert_eq!(read(&ro), "ro");
    aterm_dirfd::unlinkat(&pinned, "sub", AtFlags::REMOVEDIR, id("sub")).expect("rmdir");
    assert!(gone(&root.join("sub")));
    aterm_dirfd::seam::legacy_classes(false);
}

// pin, and errors

#[test]
fn pin_walks_every_spelling_of_one_directory_and_refuses_the_rest() {
    let tmp = scratch();
    let root = canon(tmp.path());
    write(&root.join("f.txt"), "f");
    let id = aterm_dirfd::file_id(aterm_dirfd::pin(&root).expect("verbatim")).expect("id");
    // The plain spelling reaches the same directory.
    let plain = PathBuf::from(
        root.to_str()
            .expect("UTF-8")
            .strip_prefix(r"\\?\")
            .expect("verbatim"),
    );
    assert_eq!(
        aterm_dirfd::file_id(aterm_dirfd::pin(&plain).expect("plain")).expect("id"),
        id
    );
    // The bare drive root pins too, and is its volume's root.
    let drive = &plain.to_str().expect("UTF-8")[..3];
    let drive_root = aterm_dirfd::pin(Path::new(drive)).expect("drive root");
    assert_eq!(aterm_dirfd::seam::is_volume_root(&drive_root), Ok(true));
    // A directory below it is not (the check `pin` makes on the root it
    // opens, which a `subst` letter's root fails): one whose volume path fits
    // the check's buffer (`\Users`), and one that does not.
    let first = plain
        .components()
        .nth(2)
        .expect("the temp directory is below the drive root");
    let top = aterm_dirfd::pin(&Path::new(drive).join(first)).expect("top-level directory");
    assert_eq!(aterm_dirfd::seam::is_volume_root(&top), Ok(false));
    let below = aterm_dirfd::pin(&root).expect("pinned");
    assert_eq!(aterm_dirfd::seam::is_volume_root(&below), Ok(false));
    // The device check `pin` makes before opening anything, on live DOS
    // names: the drive passes; a device with a name under it (as a network
    // drive's share under its redirector), an alias of another DOS name (as
    // a `subst` letter's `\??\C:\dir`) and an empty target do not.
    assert_eq!(aterm_dirfd::seam::volume_device(&drive[..2]), Ok(()));
    for (name, target) in [
        ("CON", r"\Device\ConDrv\Console"),
        ("AUX", r"\DosDevices\COM1"),
        ("GLOBALROOT", ""),
    ] {
        assert_eq!(
            aterm_dirfd::seam::volume_device(name),
            Err(Errno::NOT_VOLUME_ROOT),
            "{name} ({target})"
        );
    }
    // So does the volume-GUID spelling, from the mount manager's own name.
    let volume = run("mountvol", &[drive, "/L"]);
    let volume = volume.trim();
    assert!(volume.starts_with(r"\\?\Volume{"), "{volume:?}");
    let by_guid = PathBuf::from(format!("{volume}{}", &plain.to_str().expect("UTF-8")[3..]));
    assert_eq!(
        aterm_dirfd::file_id(aterm_dirfd::pin(&by_guid).expect("by GUID")).expect("id"),
        id
    );
    // A drive or volume that is not there is not "gone" (that is
    // ERROR_FILE_NOT_FOUND, said of a name in a directory only).
    for letter in b'A'..=b'Z' {
        let root = format!("{}:\\", char::from(letter));
        let pinned = aterm_dirfd::pin(Path::new(&root));
        assert_ne!(pinned.as_ref().err(), Some(&Errno::NOENT), "{root}");
    }
    assert_eq!(
        aterm_dirfd::pin(Path::new(
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\x"
        ))
        .err(),
        Some(Errno::PATH_NOT_FOUND)
    );
    // A share is never walked, and nothing is opened to find that out: an
    // unroutable host (TEST-NET-1) would hold an open for its SMB timeout.
    for share in [
        r"\\192.0.2.1\share\x",
        r"\\?\UNC\192.0.2.1\share\x",
        r"\\localhost\c$\Windows",
    ] {
        assert_eq!(
            aterm_dirfd::pin(Path::new(share)).err(),
            Some(Errno::NOT_VOLUME_ROOT),
            "{share}"
        );
    }
    // The device check, over targets as `QueryDosDeviceW` spells them.
    for (target, volume) in [
        (r"\Device\HarddiskVolume3", true),
        (r"\device\HarddiskVolume12", true),
        (r"\Device\CdRom0", true),
        // A subst letter: a path, resolved component by component.
        (r"\??\C:\Users\me\src", false),
        (r"\??\C:\", false),
        // Mapped network drives: a share under a redirector.
        (
            r"\Device\LanmanRedirector\;Z:0000000000012345\server\share",
            false,
        ),
        (
            r"\Device\Mup\;LanmanRedirector\;Y:0000000000012345\server\share",
            false,
        ),
        (
            r"\Device\WebDavRedirector\;W:0000000000012345\host\DavWWWRoot",
            false,
        ),
        (
            r"\Device\HarddiskDmVolumes\PhysicalDmVolumes\BlockVolume1",
            false,
        ),
        (r"\Device\", false),
        (r"\Device", false),
        (r"\DosDevices\C:", false),
        ("", false),
    ] {
        assert_eq!(
            aterm_dirfd::seam::is_volume_device(target),
            volume,
            "{target:?}"
        );
    }

    let refused = [
        PathBuf::from(r"relative\dir"),
        PathBuf::from(&drive[..2]).join("drive-relative"),
        // Spelled, not `join`ed: pushing `..` onto a verbatim path pops.
        PathBuf::from(format!(r"{}\..\x", root.display())),
        PathBuf::from(r"\\.\C:\Windows"),
        PathBuf::from(format!(r"{}\with/slash", root.display())),
        plain.join("trailing-dot."),
    ];
    for path in refused {
        assert_eq!(
            aterm_dirfd::pin(&path).err(),
            Some(Errno::INVAL),
            "{path:?}"
        );
    }
    assert_eq!(
        aterm_dirfd::pin(&root.join("f.txt")).err(),
        Some(Errno::NOTDIR)
    );
    assert_eq!(
        aterm_dirfd::pin(&root.join("missing")).err(),
        Some(Errno::NOENT)
    );
}

#[test]
fn errors_keep_gone_apart_from_everything_else() {
    use std::io::ErrorKind;

    assert_eq!(
        std::io::Error::from(Errno::NOENT).kind(),
        ErrorKind::NotFound
    );
    assert_eq!(std::io::Error::from(Errno::NOENT).raw_os_error(), Some(2));
    // std would call these NotFound: a missing path, drive, network path or
    // share. None is "this name is gone"; each keeps its code.
    const ERROR_INVALID_DRIVE: i32 = 15;
    const ERROR_BAD_NETPATH: i32 = 53;
    const ERROR_BAD_NET_NAME: i32 = 67;
    for code in [
        Errno::PATH_NOT_FOUND,
        Errno(ERROR_INVALID_DRIVE),
        Errno(ERROR_BAD_NETPATH),
        Errno(ERROR_BAD_NET_NAME),
    ] {
        assert_eq!(
            std::io::Error::from_raw_os_error(code.0).kind(),
            ErrorKind::NotFound,
            "std's reading of {code:?}, which this guards against"
        );
        let e = std::io::Error::from(code);
        assert_ne!(e.kind(), ErrorKind::NotFound, "{code:?}");
        assert_eq!(
            e.get_ref().and_then(|e| e.downcast_ref::<Errno>()),
            Some(&code)
        );
        assert!(!code.is_noent());
    }
    // And whatever else std's table files under NotFound: of every 16-bit
    // system code, only ERROR_FILE_NOT_FOUND converts to it.
    let not_found: Vec<i32> = (0..=0xFFFF)
        .filter(|&code| std::io::Error::from(Errno(code)).kind() == ErrorKind::NotFound)
        .collect();
    assert_eq!(not_found, [2]);
    // This crate's own codes say what they are and survive the conversion.
    for own in [
        Errno::CHANGED,
        Errno::FOREIGN_REPARSE,
        Errno::NOT_VOLUME_ROOT,
        Errno::MOUNT_POINT,
        Errno::PLACEHOLDERS_HIDDEN,
    ] {
        let e = std::io::Error::from(own);
        assert_eq!(
            e.get_ref().and_then(|e| e.downcast_ref::<Errno>()),
            Some(&own)
        );
        assert!(!e.to_string().contains("os error"), "{e}");
    }
    // The refusals a delete meets stay distinct codes.
    let codes = [
        Errno::NOENT,
        Errno::PATH_NOT_FOUND,
        Errno::ACCESS_DENIED,
        Errno::SHARING_VIOLATION,
        Errno::NOTEMPTY,
        Errno::DELETE_PENDING,
        Errno::LOOP,
        Errno::FOREIGN_REPARSE,
        Errno::CHANGED,
        Errno::NOT_VOLUME_ROOT,
        Errno::MOUNT_POINT,
        Errno::PLACEHOLDERS_HIDDEN,
    ];
    let mut unique = codes.to_vec();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), codes.len());
}

/// A third-party reparse tag set on a file: a stand-in for a cloud placeholder
/// (a non-link reparse point) that needs no sync provider. The FSCTL is the
/// crate's (`seam::set_reparse_tag`), so this file declares no FFI.
mod foreign {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::path::Path;

    /// A non-Microsoft (bit 31 clear), non-surrogate (bit 29 clear) tag.
    pub(super) const TAG: u32 = 0x0000_7A7E;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    /// The tag's owner, any GUID: a third party's reparse point carries one.
    const GUID: [u8; 16] = [
        0x7a, 0x7e, 0x5a, 0x1e, 0xd1, 0x4f, 0x4c, 0x8e, 0x9a, 0x51, 0x0e, 0x2b, 0x3c, 0x4d, 0x5e,
        0x6f,
    ];

    /// Set [`TAG`] on the file at `path`, with an empty payload.
    pub(super) fn tag(path: &Path) {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .expect("opened for the tag");
        aterm_dirfd::seam::set_reparse_tag(&file, TAG, GUID).expect("FSCTL_SET_REPARSE_POINT");
    }
}

mod mount_point {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::path::Path;

    /// A volume GUID name no volume carries: the target of a stand-in
    /// volume mount point, which nothing can ever reach.
    pub(super) const NO_VOLUME: &str = r"\??\Volume{00000000-0000-0000-0000-000000000000}\";
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    /// Make `path` (a new, empty folder) an `IO_REPARSE_TAG_MOUNT_POINT`
    /// reparse point onto the NT name `target`.
    pub(super) fn make(path: &Path, target: &str) {
        std::fs::create_dir(path).expect("the folder");
        let dir = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .expect("opened for the reparse point");
        aterm_dirfd::seam::set_mount_point(&dir, target).expect("FSCTL_SET_REPARSE_POINT");
    }
}
