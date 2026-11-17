// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows half of `aterm-dirfd` on real NTFS trees under the temp directory.
//!
//! STOCK EXCEPTION: Windows only, and Trust has no Windows std, so only stock Rust builds this (the
//! Windows lane's guest pins its version: `aterm-lab/src/windows_guest.rs`).

#![cfg(windows)]

use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use aterm_dirfd::acl::{self, Descriptor, NotOwnerOnly, Parts, Sid};
use aterm_dirfd::{AtFlags, Dir, Errno, FileId, Mode, OFlags, OwnedFd};

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

#[test]
fn relative_opens_stats_and_listings_agree_with_std() {
    let tmp = scratch();
    let root = canon(tmp.path());
    write(&root.join("a.txt"), "hello");
    std::fs::hard_link(root.join("a.txt"), root.join("hard.txt")).expect("hard link");
    std::fs::create_dir(root.join("sub")).expect("sub");
    write(&root.join("sub").join("b.bin"), "abc");

    let pinned = aterm_dirfd::pin(&root).expect("pinned");
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
    assert_eq!(a_st.id(), hard_st.id());
    assert_eq!(a_st.st_nlink, 2);
    assert_eq!(a_st.st_dev, root_st.st_dev);
    assert_ne!(a_st.st_ino, root_st.st_ino);
    assert_eq!(aterm_dirfd::file_id(&a).expect("id"), a_st.id());
    assert!(a_st.file_type().is_file());
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
    let listed_a = entry(&pinned, "a.txt");
    assert_eq!(listed_a.ino(), a_st.st_ino);
    assert_eq!(listed_a.size(), 5);
    assert!(listed_a.file_type().is_file());
    assert!(entry(&pinned, "sub").file_type().is_dir());
    let std_meta = std::fs::metadata(root.join("a.txt")).expect("std");
    assert_eq!(a_st.modified(), Some(std_meta.modified().expect("mtime")));
    assert_eq!(std_meta.len(), a_st.size);
    let mut text = String::new();
    std::fs::File::from(a)
        .read_to_string(&mut text)
        .expect("read through the handle");
    assert_eq!(text, "hello");

    let again = aterm_dirfd::openat(&pinned, ".", dir_flags(), Mode::empty()).expect("reopen");
    assert_eq!(aterm_dirfd::file_id(&again).expect("id"), root_st.id());
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
    assert!(aterm_dirfd::openat(&pinned, "new.lock", flags, Mode::empty()).is_ok());
    assert_eq!(
        aterm_dirfd::openat(&pinned, "new.lock", flags | OFlags::EXCL, Mode::empty()).err(),
        Some(Errno(183))
    );
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
/// this process's user — or, elevated, `BUILTIN\Administrators`.
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
    let listing = aterm_dirfd::openat(&pinned, "d", dir_flags(), Mode::empty()).expect("d");
    assert_eq!(
        aterm_dirfd::owner(&listing).err(),
        Some(Errno::ACCESS_DENIED)
    );
    for flags in [OFlags::PATH | OFlags::RDWR, OFlags::PATH | OFlags::CREATE] {
        assert_eq!(
            aterm_dirfd::openat(&pinned, "new", flags, Mode::empty()).err(),
            Some(Errno::INVAL)
        );
    }
    drop(holder);

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
    assert_eq!(
        aterm_dirfd::unlinkat(&pinned, "empty", AtFlags::empty(), id("empty")),
        Err(Errno::ISDIR)
    );
    assert!(root.join("empty").is_dir());
    assert_eq!(
        aterm_dirfd::unlinkat(&pinned, "f.txt", AtFlags::REMOVEDIR, id("f.txt")),
        Err(Errno::NOTDIR)
    );
    assert_eq!(read(&root.join("f.txt")), "f");
    assert_eq!(
        aterm_dirfd::unlinkat(&pinned, "full", AtFlags::REMOVEDIR, id("full")),
        Err(Errno::NOTEMPTY)
    );
    assert_eq!(read(&root.join("full").join("x")), "x");
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
    assert!(gone(&path));
    assert!(listed(&dir).is_empty());
    holder
        .write_all(b" and more")
        .expect("the holder still writes");
    assert_eq!(holder.metadata().expect("holder meta").len(), 19);
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
    assert_eq!(
        aterm_dirfd::unlinkat(&pinned, "busy.rlib", AtFlags::empty(), id),
        Err(Errno::SHARING_VIOLATION)
    );
    assert_eq!(read(&path), "busy");
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
    let j = entry(&tfd, "j");
    assert!(j.file_type().is_link(), "{j:?}");
    assert!(!j.file_type().is_dir());
    assert_eq!(j.reparse_tag(), IO_REPARSE_TAG_MOUNT_POINT);
    assert_eq!(
        aterm_dirfd::openat(&tfd, "j", dir_flags(), Mode::empty()).err(),
        Some(Errno::LOOP)
    );
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
    assert_eq!(aterm_dirfd::pin(&t.join("j")).err(), Some(Errno::LOOP));
    let id = FileId {
        dev: itself_st.st_dev,
        ino: j.ino(),
    };
    assert_eq!(
        aterm_dirfd::unlinkat(&tfd, "j", AtFlags::REMOVEDIR, id),
        Err(Errno::NOTDIR)
    );
    drop(tfd);
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
    mount_point::make(&t.join("mnt"), mount_point::NO_VOLUME);
    mount_point::make(&t.join("j"), &format!(r"\??\{}", plain(&outside)));

    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let tfd = aterm_dirfd::openat(&pinned, "t", dir_flags(), Mode::empty()).expect("t");
    let dev = aterm_dirfd::file_id(&tfd).expect("identity").dev;
    let (mnt, j) = (entry(&tfd, "mnt"), entry(&tfd, "j"));
    for listed in [&mnt, &j] {
        assert!(listed.file_type().is_link(), "{listed:?}");
        assert_eq!(listed.reparse_tag(), IO_REPARSE_TAG_MOUNT_POINT);
    }
    assert_eq!(
        aterm_dirfd::openat(&tfd, "mnt", dir_flags(), Mode::empty()).err(),
        Some(Errno::LOOP)
    );
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
    let said = std::io::Error::from(Errno::MOUNT_POINT);
    assert_eq!(said.kind(), std::io::ErrorKind::Other);
    assert!(said.to_string().contains("volume is mounted"), "{said}");
    drop(tfd);
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
    let itself = aterm_dirfd::openat(&tfd, "placeholder.dat", OFlags::RDONLY, Mode::empty())
        .expect("itself");
    assert!(
        aterm_dirfd::fstat(&itself)
            .expect("stat")
            .file_type()
            .is_foreign_reparse()
    );
    drop((itself, tfd));
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
/// sync client leaves what it uploaded.
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

    for under in [&sync, &inc] {
        assert_eq!(
            aterm_dirfd::pin(under).err(),
            Some(Errno::FOREIGN_REPARSE),
            "{}",
            under.display()
        );
    }
    assert_eq!(aterm_dirfd::seam::thread_placeholder_mode(), mode);
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
    let exposed = aterm_dirfd::expose_placeholders().expect("exposed");
    assert_eq!(aterm_dirfd::seam::thread_placeholder_mode(), 2);
    drop(exposed);
    assert_eq!(aterm_dirfd::seam::thread_placeholder_mode(), mode);

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
/// it, the volume's own root).
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
    assert_eq!(
        read(r"\\?\volume{6140fc3b-044f-43d7-b060-cb98170c7a63}"),
        Some("{6140fc3b-044f-43d7-b060-cb98170c7a63}".to_owned())
    );
    for not_one in [
        r"\\?\C:\Users\x",
        r"\\?\Volume{6140fc3b-044f-43d7-b060-cb98170c7a6}\",
        r"\\?\Volume{6140fc3b-044f-43d7-b060-cb98170c7a6g}\",
        r"\\?\Volume{6140fc3b-044f-43d7-b060-cb98170c7a63\",
        r"\\?\Volume{6140fc3b0044f-43d7-b060-cb98170c7a63}\",
        r"\\?\Volume{6140fc3b-044f-43d7-b060-cb98170c7a63}extra",
    ] {
        assert_eq!(read(not_one), None, "{not_one}");
    }
}

/// Turn off the last-write-time updates the writes through `file` make (`SetFileTime` with
/// 0xFFFFFFFF_FFFFFFFF as the last write): a write through it leaves that time as it was, as a
/// write in the clock step of the write before it does, and the call itself changes nothing of
/// the file.
fn keep_write_time(file: &std::fs::File) {
    use std::os::windows::io::AsRawHandle as _;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// Each time a `FILETIME` (two `DWORD`s, low first: one little-endian `u64`), or null to
        /// leave it as it is.
        fn SetFileTime(
            file: *mut core::ffi::c_void,
            creation: *const u64,
            access: *const u64,
            write: *const u64,
        ) -> i32;
    }
    let off = u64::MAX;
    // SAFETY: `file` keeps its handle open across the call; the creation and access times are
    // null (left as they are), and `off` is a readable `FILETIME`'s eight bytes that outlive the
    // call.
    let ok = unsafe {
        SetFileTime(
            file.as_raw_handle(),
            std::ptr::null(),
            std::ptr::null(),
            &raw const off,
        )
    };
    assert_ne!(ok, 0, "SetFileTime: {}", std::io::Error::last_os_error());
}

/// A WRITE THE TIMES DO NOT SHOW MOVES THE CHANGE-JOURNAL COUNTER: a write of as many bytes
/// through a handle that leaves the file's last-write time as it was (as a write in the clock
/// step of the write before it leaves it), read while that handle is still open and with no other
/// change made to the file, reads a greater `usn` in the same journal through a facts-only
/// handle; the handle's close then writes a record of its own. CONTROLS: the handle opened and
/// its time updates turned off, nothing written, reads the listed `usn`; and after the write
/// fstat's size and last write read as the listing's, so only the counter tells it. A directory
/// opened as itself has a counter too. The temp volume is NTFS, which keeps a change journal.
#[test]
fn a_write_the_times_do_not_show_moves_the_change_journal_counter() {
    let tmp = scratch();
    let root = canon(tmp.path());
    let path = root.join("notes.md");
    write(&path, "one");
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let facts = || {
        aterm_dirfd::openat(
            &pinned,
            "notes.md",
            OFlags::PATH | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .expect("facts")
    };
    let listed = aterm_dirfd::usn(facts())
        .expect("read")
        .expect("the temp volume keeps a change journal");
    let stat = aterm_dirfd::fstat(facts()).expect("stat");
    assert_eq!(aterm_dirfd::usn(facts()), Ok(Some(listed)), "unchanged");
    let mut held = std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("opened to write");
    keep_write_time(&held);
    assert_eq!(
        aterm_dirfd::usn(facts()),
        Ok(Some(listed)),
        "opened, its time updates off, nothing written"
    );
    held.write_all(b"two").expect("written");
    let written = aterm_dirfd::fstat(facts()).expect("stat");
    assert_eq!(
        (written.size, written.last_write),
        (stat.size, stat.last_write),
        "the write's size and time"
    );
    let moved = aterm_dirfd::usn(facts()).expect("read").expect("a journal");
    assert_eq!(moved.journal, listed.journal);
    assert!(moved.usn > listed.usn, "{moved:?} after {listed:?}");
    drop(held);
    let closed = aterm_dirfd::usn(facts()).expect("read").expect("a journal");
    assert!(
        closed.usn > moved.usn,
        "the close: {closed:?} after {moved:?}"
    );
    assert_eq!(read(&path), "two");
    let folder = aterm_dirfd::usn(&pinned).expect("read").expect("a journal");
    assert_eq!(folder.journal, listed.journal);
}

/// A REWRITE WITH ITS TIME PUT BACK MOVES THE CHANGE-JOURNAL COUNTER: a rewrite of as many bytes
/// whose modified time is then put back where the write before left it (as a copy that keeps a
/// file's times leaves it) reads a greater `usn` in the same journal, through a facts-only
/// handle; the put-back is itself a change the journal records. CONTROL: fstat's size and last
/// write read the rewrite as no change.
#[test]
fn a_rewrite_with_its_time_put_back_moves_the_change_journal_counter() {
    let tmp = scratch();
    let root = canon(tmp.path());
    let path = root.join("notes.md");
    write(&path, "one");
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let facts = || {
        aterm_dirfd::openat(
            &pinned,
            "notes.md",
            OFlags::PATH | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .expect("facts")
    };
    let listed = aterm_dirfd::usn(facts())
        .expect("read")
        .expect("the temp volume keeps a change journal");
    let stat = aterm_dirfd::fstat(facts()).expect("stat");
    let modified = std::fs::symlink_metadata(&path)
        .expect("metadata")
        .modified()
        .expect("modified");
    write(&path, "two");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("opened")
        .set_modified(modified)
        .expect("modified time put back");
    let rewritten = aterm_dirfd::fstat(facts()).expect("stat");
    assert_eq!(
        (rewritten.size, rewritten.last_write),
        (stat.size, stat.last_write),
        "the rewrite's size and time"
    );
    let moved = aterm_dirfd::usn(facts()).expect("read").expect("a journal");
    assert_eq!(moved.journal, listed.journal);
    assert!(moved.usn > listed.usn, "{moved:?} after {listed:?}");
}

#[test]
fn a_name_swapped_after_the_listing_is_refused_and_both_files_stay() {
    let tmp = scratch();
    let root = canon(tmp.path());
    write(&root.join("x"), "listed");
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let dev = aterm_dirfd::file_id(&pinned).expect("root").dev;
    let listed_x = entry(&pinned, "x");
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
    let enumerated = FileId {
        dev: aterm_dirfd::file_id(&pinned).expect("root").dev,
        ino: entry(&pinned, "d").ino(),
    };
    std::fs::rename(root.join("d"), root.join("d-aside")).expect("renamed aside");
    std::fs::create_dir(root.join("d")).expect("another d");
    write(&root.join("d").join("swapped-in.txt"), "not ours");
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
    aterm_dirfd::seam::legacy_classes(true);
    let removed = aterm_dirfd::unlinkat(&pinned, "d", AtFlags::REMOVEDIR, id);
    aterm_dirfd::seam::legacy_classes(false);
    removed.expect("delete pending");
    assert!(aterm_dirfd::fstat(&held).expect("stat").delete_pending);
    assert_eq!(
        aterm_dirfd::openat(&pinned, "d", dir_flags(), Mode::empty()).err(),
        Some(Errno::DELETE_PENDING)
    );
    assert_eq!(
        aterm_dirfd::openat(&held, "new", OFlags::RDWR | OFlags::CREATE, Mode::empty()).err(),
        Some(Errno::DELETE_PENDING)
    );
    let listing: Vec<_> = Dir::read_from(&held)
        .expect("a listing of a delete-pending directory")
        .collect();
    assert!(listing.is_empty(), "{listing:?}");
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
    assert_eq!(
        aterm_dirfd::seam::listing_class(&dir),
        "FileIdBothDirectoryInfo"
    );
    assert_eq!(legacy, extended);
    let dev = aterm_dirfd::file_id(&pinned).expect("root").dev;
    let id = |name: &str| FileId {
        dev,
        ino: entry(&pinned, name).ino(),
    };
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

#[test]
fn pin_walks_every_spelling_of_one_directory_and_refuses_the_rest() {
    let tmp = scratch();
    let root = canon(tmp.path());
    write(&root.join("f.txt"), "f");
    let id = aterm_dirfd::file_id(aterm_dirfd::pin(&root).expect("verbatim")).expect("id");
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
    let drive = &plain.to_str().expect("UTF-8")[..3];
    let drive_root = aterm_dirfd::pin(Path::new(drive)).expect("drive root");
    assert_eq!(aterm_dirfd::seam::is_volume_root(&drive_root), Ok(true));
    let first = plain
        .components()
        .nth(2)
        .expect("the temp directory is below the drive root");
    let top = aterm_dirfd::pin(&Path::new(drive).join(first)).expect("top-level directory");
    assert_eq!(aterm_dirfd::seam::is_volume_root(&top), Ok(false));
    let below = aterm_dirfd::pin(&root).expect("pinned");
    assert_eq!(aterm_dirfd::seam::is_volume_root(&below), Ok(false));
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
    let volume = run("mountvol", &[drive, "/L"]);
    let volume = volume.trim();
    assert!(volume.starts_with(r"\\?\Volume{"), "{volume:?}");
    let by_guid = PathBuf::from(format!("{volume}{}", &plain.to_str().expect("UTF-8")[3..]));
    assert_eq!(
        aterm_dirfd::file_id(aterm_dirfd::pin(&by_guid).expect("by GUID")).expect("id"),
        id
    );
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
    for (target, volume) in [
        (r"\Device\HarddiskVolume3", true),
        (r"\device\HarddiskVolume12", true),
        (r"\Device\CdRom0", true),
        (r"\??\C:\Users\me\src", false),
        (r"\??\C:\", false),
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
    assert!(Errno::NOENT.is_noent() && !Errno::NOENT.is_inval());
    assert!(Errno::INVAL.is_inval() && !Errno::INVAL.is_noent());
    assert_eq!(std::io::Error::from(Errno::NOENT).raw_os_error(), Some(2));
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
    let not_found: Vec<i32> = (0..=0xFFFF)
        .filter(|&code| std::io::Error::from(Errno(code)).kind() == ErrorKind::NotFound)
        .collect();
    assert_eq!(not_found, [2]);
    for own in [
        Errno::CHANGED,
        Errno::FOREIGN_REPARSE,
        Errno::NOT_VOLUME_ROOT,
        Errno::MOUNT_POINT,
        Errno::PLACEHOLDERS_HIDDEN,
        Errno::NOT_OWNER,
    ] {
        let e = std::io::Error::from(own);
        assert_eq!(
            e.get_ref().and_then(|e| e.downcast_ref::<Errno>()),
            Some(&own)
        );
        assert!(!e.to_string().contains("os error"), "{e}");
    }
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
        Errno::NOT_OWNER,
        Errno::EXIST,
        Errno::NOSYS,
    ];
    let mut unique = codes.to_vec();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), codes.len());
    assert_eq!(
        std::io::Error::from(Errno::EXIST).kind(),
        ErrorKind::AlreadyExists
    );
    assert_eq!(
        std::io::Error::from(Errno::NOSYS).kind(),
        ErrorKind::Unsupported
    );
    assert_eq!(
        std::io::Error::from(Errno::NOT_OWNER).kind(),
        ErrorKind::PermissionDenied
    );
}

/// This token's user.
fn me() -> Sid {
    Sid::token_user().expect("this token's user")
}

/// The owner-only descriptor's text for `me`: a file's, or a folder's (its entries passed on).
fn owner_only_text(me: &Sid, folder: bool) -> String {
    let passed = if folder { "OICI" } else { "" };
    format!("O:{me}D:P(A;{passed};FA;;;{me})(A;{passed};FA;;;SY)")
}

/// The descriptor of `name` under `dir`, read off a facts-only open of it.
fn descriptor_of(dir: &OwnedFd, name: &str) -> Descriptor {
    let facts = aterm_dirfd::openat(dir, name, OFlags::PATH | OFlags::NOFOLLOW, Mode::empty())
        .expect("a facts-only open");
    Descriptor::of(&facts, Parts::OWNER | Parts::DACL).expect("its descriptor")
}

/// `name`'s owner and access list as descriptor text.
fn text_of(dir: &OwnedFd, name: &str) -> String {
    descriptor_of(dir, name)
        .to_sddl(Parts::OWNER | Parts::DACL)
        .expect("as text")
}

/// The owner-only rule's verdict on `name`.
fn verdict(dir: &OwnedFd, name: &str) -> Result<(), NotOwnerOnly> {
    let facts = aterm_dirfd::openat(dir, name, OFlags::PATH | OFlags::NOFOLLOW, Mode::empty())
        .expect("a facts-only open");
    acl::owner_only(&facts, &me()).expect("its descriptor reads")
}

/// Whether `name`'s access list is its folder's alone: not protected, and every entry inherited.
fn takes_folders_entries(dir: &OwnedFd, name: &str) -> bool {
    let descriptor = descriptor_of(dir, name);
    !descriptor.dacl_protected().expect("its control bits")
        && descriptor
            .entries()
            .expect("its entries")
            .is_some_and(|entries| {
                !entries.is_empty() && entries.iter().all(|e| e.flags & acl::INHERITED != 0)
            })
}

/// Whether `name` is owned by `me` under a protected list of exactly two entries, `me`'s and
/// SYSTEM's, each allowing everything (a folder's passed on to what is made inside, `OICI`), read
/// entry by entry: a list written in place carries the auto-inherited bit (`D:PAI`) a list made
/// at creation does not, and the rule reads no control bit but protection.
fn exactly_owner_only(dir: &OwnedFd, name: &str, me: &Sid, folder: bool) -> bool {
    const OBJECT_AND_CONTAINER_INHERIT: u8 = 0x3;
    let descriptor = descriptor_of(dir, name);
    let flags = if folder {
        OBJECT_AND_CONTAINER_INHERIT
    } else {
        0
    };
    let allow = |sid: Sid| acl::Entry {
        kind: acl::ACCESS_ALLOWED,
        flags,
        mask: acl::FILE_ALL_ACCESS,
        sid: Some(sid),
    };
    descriptor.owner().expect("its owner").as_ref() == Some(me)
        && descriptor.dacl_protected().expect("its control bits")
        && descriptor.entries().expect("its entries")
            == Some(vec![
                allow(me.clone()),
                allow(Sid::parse("SY").expect("SYSTEM")),
            ])
}

/// A file `openat` makes and a folder `mkdirat` makes with a private mode (no group or other bit)
/// are owner-only from their making: owned by this user under a protected list of exactly the
/// user and SYSTEM (a folder's passed on, `OICI`), with no entry from their folder, a private file
/// inside a private folder too. With any other mode each takes its folder's entries, and a file
/// that exists, opened with `CREATE` and a private mode, keeps its own.
#[test]
fn a_private_mode_makes_owner_only_and_another_takes_the_folders_entries() {
    let tmp = scratch();
    let root = canon(tmp.path());
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let me = me();
    let create = OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW;
    for (name, mode) in [
        ("rw", Mode::RUSR | Mode::WUSR),
        ("ro", Mode::RUSR),
        ("none", Mode::empty()),
    ] {
        drop(aterm_dirfd::openat(&pinned, name, create, mode).expect("made"));
        assert_eq!(
            text_of(&pinned, name),
            owner_only_text(&me, false),
            "{name}"
        );
        assert_eq!(verdict(&pinned, name), Ok(()), "{name}");
    }
    aterm_dirfd::mkdirat(&pinned, "vault", Mode::RWXU).expect("made");
    assert_eq!(text_of(&pinned, "vault"), owner_only_text(&me, true));
    assert_eq!(verdict(&pinned, "vault"), Ok(()));
    for (name, mode) in [
        ("group", Mode::RUSR | Mode::WUSR | Mode::RGRP),
        ("other", Mode::RUSR | Mode::WUSR | Mode::WOTH),
        ("exec", Mode::RWXU | Mode::XOTH),
    ] {
        drop(aterm_dirfd::openat(&pinned, name, create, mode).expect("made"));
        assert!(takes_folders_entries(&pinned, name), "{name}");
    }
    let shared = Mode::RWXU | Mode::RGRP | Mode::XGRP | Mode::ROTH | Mode::XOTH;
    aterm_dirfd::mkdirat(&pinned, "shared", shared).expect("made");
    assert!(takes_folders_entries(&pinned, "shared"));
    let flags = OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW;
    drop(aterm_dirfd::openat(&pinned, "group", flags, Mode::RUSR | Mode::WUSR).expect("opened"));
    assert!(takes_folders_entries(&pinned, "group"));
    let vault = aterm_dirfd::openat(&pinned, "vault", dir_flags(), Mode::empty()).expect("vault");
    drop(aterm_dirfd::openat(&vault, "key", create, Mode::RUSR | Mode::WUSR).expect("made"));
    assert_eq!(text_of(&vault, "key"), owner_only_text(&me, false));
    aterm_dirfd::mkdirat(&vault, "inner", Mode::RWXU).expect("made");
    assert_eq!(text_of(&vault, "inner"), owner_only_text(&me, true));
    drop(aterm_dirfd::openat(&vault, "loose", create, Mode::RUSR | Mode::RGRP).expect("made"));
    assert!(takes_folders_entries(&vault, "loose"));
    assert_eq!(verdict(&vault, "loose"), Err(NotOwnerOnly::Unprotected));
}

/// `mkdirat`, `fstatat` and `linkat` agree with each other and with the handles: a folder made is
/// there as a folder with the identity its open reads; a second name of a file, in another folder,
/// is the same file (one identity, two links, one content); a taken name is refused (`EXIST`) and
/// kept. `fstatat` reads `.` as the directory itself and a junction as itself, and refuses
/// `REMOVEDIR`, as `linkat` does.
#[test]
fn mkdirat_linkat_and_fstatat_round_trip() {
    let tmp = scratch();
    let root = canon(tmp.path());
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let none = AtFlags::empty();

    aterm_dirfd::mkdirat(&pinned, "sub", Mode::RWXU).expect("made");
    assert!(root.join("sub").is_dir());
    let sub = aterm_dirfd::openat(&pinned, "sub", dir_flags(), Mode::empty()).expect("sub");
    let made = aterm_dirfd::fstatat(&pinned, "sub", AtFlags::SYMLINK_NOFOLLOW).expect("stat");
    assert!(made.file_type().is_dir());
    assert_eq!(made.id(), aterm_dirfd::file_id(&sub).expect("its identity"));
    assert_eq!(
        aterm_dirfd::mkdirat(&pinned, "sub", Mode::RWXU),
        Err(Errno::EXIST)
    );
    write(&root.join("plain"), "x");
    assert_eq!(
        aterm_dirfd::mkdirat(&pinned, "plain", Mode::RWXU),
        Err(Errno::EXIST)
    );
    assert_eq!(
        aterm_dirfd::mkdirat(&pinned, ".", Mode::RWXU),
        Err(Errno::INVAL)
    );

    write(&root.join("a"), "hello");
    let a = aterm_dirfd::openat(
        &pinned,
        "a",
        OFlags::RDONLY | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .expect("a");
    let by_name = aterm_dirfd::fstatat(&pinned, "a", none).expect("stat");
    assert_eq!(by_name.id(), aterm_dirfd::fstat(&a).expect("fstat").id());
    assert!(by_name.file_type().is_file());
    assert_eq!((by_name.size, by_name.st_nlink), (5, 1));
    assert_eq!(
        aterm_dirfd::fstatat(&pinned, ".", none)
            .expect("itself")
            .id(),
        aterm_dirfd::file_id(&pinned).expect("its identity")
    );
    assert_eq!(
        aterm_dirfd::fstatat(&pinned, "missing", none).err(),
        Some(Errno::NOENT)
    );
    assert_eq!(
        aterm_dirfd::fstatat(&pinned, "a", AtFlags::REMOVEDIR).err(),
        Some(Errno::INVAL)
    );

    aterm_dirfd::linkat(&pinned, "a", &sub, "b", none).expect("linked");
    let b = aterm_dirfd::fstatat(&sub, "b", none).expect("stat");
    assert_eq!(b.id(), by_name.id());
    assert_eq!(b.st_nlink, 2);
    assert_eq!(aterm_dirfd::fstat(&a).expect("fstat").st_nlink, 2);
    assert_eq!(read(&root.join("sub").join("b")), "hello");
    write(&root.join("sub").join("b"), "world");
    assert_eq!(read(&root.join("a")), "world");
    write(&root.join("taken"), "kept");
    assert_eq!(
        aterm_dirfd::linkat(&pinned, "a", &pinned, "taken", none),
        Err(Errno::EXIST)
    );
    assert_eq!(read(&root.join("taken")), "kept");
    assert_eq!(
        aterm_dirfd::linkat(&pinned, "a", &sub, "b", none),
        Err(Errno::EXIST)
    );
    assert_eq!(
        aterm_dirfd::linkat(&pinned, "missing", &pinned, "c", none),
        Err(Errno::NOENT)
    );
    assert_eq!(
        aterm_dirfd::linkat(&pinned, "a", &pinned, "c", AtFlags::REMOVEDIR),
        Err(Errno::INVAL)
    );
    assert_eq!(
        aterm_dirfd::linkat(&pinned, "sub", &pinned, "sub2", none),
        Err(Errno::ISDIR)
    );
    assert!(gone(&root.join("sub2")) && gone(&root.join("c")));
    assert_eq!(aterm_dirfd::fstat(&a).expect("fstat").st_nlink, 2);

    std::fs::create_dir(root.join("target")).expect("target");
    junction(&root.join("j"), &root.join("target"));
    let j = aterm_dirfd::fstatat(&pinned, "j", none).expect("stat");
    assert!(j.file_type().is_link(), "{j:?}");
    assert_ne!(
        j.id(),
        aterm_dirfd::fstatat(&pinned, "target", none)
            .expect("stat")
            .id()
    );
}

/// A `NOREPLACE` rename refuses a name a file, a folder or a junction takes, with `EXIST` (std's
/// `AlreadyExists`), and leaves both entries as they were, the junction's target untouched; into
/// a free name, in its own folder or another, it moves the entry, its identity kept, a folder too.
#[test]
fn a_noreplace_rename_refuses_a_taken_name_and_takes_a_free_one() {
    use aterm_dirfd::RenameFlags;
    let tmp = scratch();
    let root = canon(tmp.path());
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let none = AtFlags::empty();
    write(&root.join("from"), "mine");
    write(&root.join("file"), "theirs");
    std::fs::create_dir(root.join("folder")).expect("folder");
    std::fs::create_dir(root.join("target")).expect("target");
    write(&root.join("target").join("inside"), "inside");
    junction(&root.join("junction"), &root.join("target"));
    let from = aterm_dirfd::fstatat(&pinned, "from", none)
        .expect("stat")
        .id();
    for taken in ["file", "folder", "junction"] {
        let refused =
            aterm_dirfd::renameat_with(&pinned, "from", &pinned, taken, RenameFlags::NOREPLACE);
        assert_eq!(refused, Err(Errno::EXIST), "{taken}");
        assert_eq!(
            std::io::Error::from(refused.unwrap_err()).kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(read(&root.join("from")), "mine", "{taken}");
    }
    assert_eq!(read(&root.join("file")), "theirs");
    assert!(root.join("folder").is_dir());
    assert!(
        aterm_dirfd::fstatat(&pinned, "junction", none)
            .expect("stat")
            .file_type()
            .is_link()
    );
    assert_eq!(read(&root.join("target").join("inside")), "inside");
    assert!(!root.join("target").join("from").exists());

    aterm_dirfd::renameat_with(&pinned, "from", &pinned, "moved", RenameFlags::NOREPLACE)
        .expect("moved");
    assert!(gone(&root.join("from")));
    assert_eq!(read(&root.join("moved")), "mine");
    assert_eq!(
        aterm_dirfd::fstatat(&pinned, "moved", none)
            .expect("stat")
            .id(),
        from
    );
    std::fs::create_dir(root.join("other")).expect("other");
    let other = aterm_dirfd::openat(&pinned, "other", dir_flags(), Mode::empty()).expect("other");
    aterm_dirfd::renameat_with(&pinned, "moved", &other, "there", RenameFlags::NOREPLACE)
        .expect("moved across");
    assert!(gone(&root.join("moved")));
    assert_eq!(read(&root.join("other").join("there")), "mine");
    assert_eq!(
        aterm_dirfd::fstatat(&other, "there", none)
            .expect("stat")
            .id(),
        from
    );
    aterm_dirfd::renameat_with(&pinned, "folder", &other, "folder", RenameFlags::NOREPLACE)
        .expect("a folder moved");
    assert!(gone(&root.join("folder")) && root.join("other").join("folder").is_dir());
    assert_eq!(
        aterm_dirfd::renameat_with(&pinned, "missing", &pinned, "x", RenameFlags::NOREPLACE),
        Err(Errno::NOENT)
    );
    assert_eq!(
        aterm_dirfd::renameat_with(&pinned, ".", &pinned, "x", RenameFlags::NOREPLACE),
        Err(Errno::INVAL)
    );
    assert!(gone(&root.join("x")));
}

/// A rename over a name replaces the entry there even while a reader holds it open, as
/// `rename(2)` does (POSIX semantics): the reader reads the old file's bytes on to their end and
/// again from the start, its file now nameless; the name opens the new file, its identity the
/// moved one's, and the old name is gone. With no flags `renameat_with` is the same rename, and a
/// read-only entry is replaced too. An entry held open without delete sharing, at either name, is
/// not moved or replaced (`SHARING_VIOLATION`); a folder is not replaced (`ISDIR`); a folder with
/// a file open inside is not moved (`ACCESS_DENIED`) until it closes; each refusal leaves both
/// names as they were.
#[test]
fn a_rename_replaces_an_open_file_whose_reader_reads_the_old_bytes_on() {
    use std::io::Seek as _;
    use std::os::windows::fs::OpenOptionsExt as _;
    let tmp = scratch();
    let root = canon(tmp.path());
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let none = AtFlags::empty();
    write(&root.join("old"), "old bytes, all of them");
    write(&root.join("new"), "new bytes");
    let new = aterm_dirfd::fstatat(&pinned, "new", none)
        .expect("stat")
        .id();
    let mut reader = std::fs::File::open(root.join("old")).expect("a reader");
    let mut head = [0u8; 4];
    reader.read_exact(&mut head).expect("read");
    assert_eq!(&head, b"old ");

    aterm_dirfd::renameat(&pinned, "new", &pinned, "old").expect("replaced while open");
    let mut rest = String::new();
    reader.read_to_string(&mut rest).expect("read on");
    assert_eq!(rest, "bytes, all of them");
    reader.rewind().expect("rewound");
    let mut whole = String::new();
    reader.read_to_string(&mut whole).expect("read again");
    assert_eq!(whole, "old bytes, all of them");
    assert_eq!(aterm_dirfd::fstat(&reader).expect("fstat").st_nlink, 0);
    assert_eq!(read(&root.join("old")), "new bytes");
    assert!(gone(&root.join("new")));
    assert_eq!(
        aterm_dirfd::fstatat(&pinned, "old", none)
            .expect("stat")
            .id(),
        new
    );

    write(&root.join("next"), "next");
    aterm_dirfd::renameat_with(
        &pinned,
        "next",
        &pinned,
        "old",
        aterm_dirfd::RenameFlags::empty(),
    )
    .expect("replaced");
    assert_eq!(read(&root.join("old")), "next");
    assert!(gone(&root.join("next")));

    write(&root.join("ro"), "read-only");
    let mut permissions = std::fs::metadata(root.join("ro"))
        .expect("metadata")
        .permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(root.join("ro"), permissions).expect("read-only");
    write(&root.join("src"), "over it");
    aterm_dirfd::renameat(&pinned, "src", &pinned, "ro").expect("a read-only entry replaced");
    assert_eq!(read(&root.join("ro")), "over it");

    const FILE_SHARE_READ: u32 = 0x1;
    write(&root.join("held"), "held");
    write(&root.join("onto"), "onto");
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(root.join("held"))
        .expect("held without delete sharing");
    assert_eq!(
        aterm_dirfd::renameat(&pinned, "onto", &pinned, "held"),
        Err(Errno::SHARING_VIOLATION)
    );
    assert_eq!(
        aterm_dirfd::renameat(&pinned, "held", &pinned, "elsewhere"),
        Err(Errno::SHARING_VIOLATION)
    );
    assert!(gone(&root.join("elsewhere")));
    drop(holder);
    assert_eq!(read(&root.join("held")), "held");
    assert_eq!(read(&root.join("onto")), "onto");

    std::fs::create_dir(root.join("empty")).expect("a folder");
    assert_eq!(
        aterm_dirfd::renameat(&pinned, "onto", &pinned, "empty"),
        Err(Errno::ISDIR)
    );
    assert!(root.join("empty").is_dir());
    std::fs::create_dir(root.join("busy")).expect("a folder");
    write(&root.join("busy").join("open"), "open");
    let inside = std::fs::File::open(root.join("busy").join("open")).expect("open inside");
    assert_eq!(
        aterm_dirfd::renameat(&pinned, "busy", &pinned, "moved"),
        Err(Errno::ACCESS_DENIED)
    );
    drop(inside);
    assert!(root.join("busy").is_dir() && gone(&root.join("moved")));
    aterm_dirfd::renameat(&pinned, "busy", &pinned, "moved").expect("moved once nothing is open");
    assert_eq!(read(&root.join("moved").join("open")), "open");
}

/// NTFS has no atomic exchange of two names: `EXCHANGE`, alone or with `NOREPLACE`, is refused
/// (`NOSYS`, std's `Unsupported`) and both entries stay as they were.
#[test]
fn an_exchange_is_refused_and_moves_nothing() {
    use aterm_dirfd::RenameFlags;
    let tmp = scratch();
    let root = canon(tmp.path());
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    write(&root.join("one"), "one");
    write(&root.join("two"), "two");
    for flags in [
        RenameFlags::EXCHANGE,
        RenameFlags::EXCHANGE | RenameFlags::NOREPLACE,
    ] {
        assert_eq!(
            aterm_dirfd::renameat_with(&pinned, "one", &pinned, "two", flags),
            Err(Errno::NOSYS),
            "{flags:?}"
        );
    }
    assert_eq!(read(&root.join("one")), "one");
    assert_eq!(read(&root.join("two")), "two");
}

/// `fchmod` to a private mode makes what the handle is open on owner-only in place, through the
/// handle and no name: a file, whose handle still writes after, and a folder, whose entries are
/// then passed on. A mode that grants the group or others anything is refused (`NOSYS`) and
/// changes nothing.
#[test]
fn fchmod_to_a_private_mode_makes_a_file_and_a_folder_owner_only_in_place() {
    let tmp = scratch();
    let root = canon(tmp.path());
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let me = me();
    let create = OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW;
    let public = Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH;

    let file = aterm_dirfd::openat(&pinned, "file", create, public).expect("made");
    assert!(takes_folders_entries(&pinned, "file"));
    aterm_dirfd::fchmod(&file, Mode::RUSR | Mode::WUSR).expect("narrowed");
    assert!(exactly_owner_only(&pinned, "file", &me, false));
    assert_eq!(verdict(&pinned, "file"), Ok(()));
    std::fs::File::from(file)
        .write_all(b"still writes")
        .expect("written");
    assert_eq!(read(&root.join("file")), "still writes");

    aterm_dirfd::mkdirat(&pinned, "folder", Mode::RWXU | Mode::RGRP | Mode::XGRP).expect("made");
    assert!(takes_folders_entries(&pinned, "folder"));
    let folder = aterm_dirfd::openat(&pinned, "folder", dir_flags(), Mode::empty()).expect("open");
    drop(aterm_dirfd::openat(&folder, "loose", create, public).expect("made"));
    drop(aterm_dirfd::openat(&folder, "own", create, Mode::RUSR | Mode::WUSR).expect("made"));
    aterm_dirfd::fchmod(&folder, Mode::RWXU).expect("narrowed");
    assert!(exactly_owner_only(&pinned, "folder", &me, true));
    assert_eq!(verdict(&pinned, "folder"), Ok(()));
    let system = Sid::parse("SY").expect("SYSTEM");
    let loose = descriptor_of(&folder, "loose")
        .entries()
        .expect("its entries")
        .expect("a list");
    assert!(takes_folders_entries(&folder, "loose"));
    assert!(
        loose
            .iter()
            .all(|e| e.sid.as_ref() == Some(&me) || e.sid.as_ref() == Some(&system)),
        "{loose:?}"
    );
    assert_eq!(text_of(&folder, "own"), owner_only_text(&me, false));

    let left = aterm_dirfd::openat(&pinned, "left", create, public).expect("made");
    for mode in [public, Mode::RUSR | Mode::WUSR | Mode::XOTH] {
        assert_eq!(
            aterm_dirfd::fchmod(&left, mode),
            Err(Errno::NOSYS),
            "{mode:?}"
        );
    }
    assert!(takes_folders_entries(&pinned, "left"));
}

/// `fchmod` of what another account owns is refused (`NOT_OWNER`, read as `PermissionDenied`, as
/// unix's `EPERM` for a chmod by anyone but the owner) and changes nothing, never taking it over:
/// Windows' own program and folder, which `TrustedInstaller` owns (this unelevated user can give
/// nothing another owner, so the system's own stand in for a planted file), keep their owner and
/// list.
/// NEGATIVE CONTROL (2026-11-16): `fchmod` writing a file another account owns as it did before
/// (reopened with `WRITE_OWNER` to give it to the user): `cmd.exe: Err(Errno(5))` (the reopen's
/// `ERROR_ACCESS_DENIED`, the takeover tried), not `NOT_OWNER`.
#[test]
fn fchmod_of_what_another_account_owns_is_refused_and_changes_nothing() {
    use std::os::windows::io::{AsHandle as _, BorrowedHandle};
    let windows = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"));
    let me = me();
    let program = std::fs::File::open(windows.join("System32").join("cmd.exe")).expect("cmd.exe");
    let folder = {
        use std::os::windows::fs::OpenOptionsExt as _;
        const READ_CONTROL: u32 = 0x0002_0000;
        const FILE_READ_ATTRIBUTES: u32 = 0x0080;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        std::fs::OpenOptions::new()
            .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&windows)
            .expect("Windows' folder")
    };
    let parts = Parts::OWNER | Parts::DACL;
    let text = |fd: BorrowedHandle<'_>| {
        Descriptor::of(fd, parts)
            .and_then(|descriptor| descriptor.to_sddl(parts))
            .expect("its descriptor")
    };
    for (what, fd, mode) in [
        ("cmd.exe", program.as_handle(), Mode::RUSR | Mode::WUSR),
        ("Windows' folder", folder.as_handle(), Mode::RWXU),
    ] {
        let owner = Descriptor::of(fd, Parts::OWNER)
            .and_then(|descriptor| descriptor.owner())
            .expect("its owner")
            .expect("an owner");
        assert_ne!(owner, me, "the control: {what} is {owner}'s");
        let before = text(fd);
        let refused = aterm_dirfd::fchmod(fd, mode);
        assert_eq!(refused, Err(Errno::NOT_OWNER), "{what}: {refused:?}");
        assert_eq!(text(fd), before, "{what} is left as it was");
    }
}

/// Where the file system has no POSIX rename (the legacy class, forced on this thread), a
/// `NOREPLACE` rename still refuses a taken name with `EXIST`, and a rename still replaces a name
/// nothing holds open; one a reader holds open is not replaced, and both names stay.
#[test]
fn the_legacy_rename_class_refuses_a_taken_name_and_replaces_a_closed_one() {
    use aterm_dirfd::RenameFlags;
    struct Legacy;
    impl Drop for Legacy {
        fn drop(&mut self) {
            aterm_dirfd::seam::legacy_classes(false);
        }
    }
    let tmp = scratch();
    let root = canon(tmp.path());
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    write(&root.join("from"), "from");
    write(&root.join("to"), "to");
    aterm_dirfd::seam::legacy_classes(true);
    let _legacy = Legacy;
    assert_eq!(
        aterm_dirfd::renameat_with(&pinned, "from", &pinned, "to", RenameFlags::NOREPLACE),
        Err(Errno::EXIST)
    );
    assert_eq!(read(&root.join("to")), "to");
    let reader = std::fs::File::open(root.join("to")).expect("a reader");
    assert_eq!(
        aterm_dirfd::renameat(&pinned, "from", &pinned, "to"),
        Err(Errno::ACCESS_DENIED)
    );
    assert_eq!(read(&root.join("from")), "from");
    drop(reader);
    aterm_dirfd::renameat(&pinned, "from", &pinned, "to").expect("replaced");
    assert_eq!(read(&root.join("to")), "from");
    assert!(gone(&root.join("from")));
}

/// A third-party reparse tag set on a file: a stand-in for a cloud placeholder
/// (a non-link reparse point) that needs no sync provider.
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

    /// A volume GUID name no volume carries.
    pub(super) const NO_VOLUME: &str = r"\??\Volume{00000000-0000-0000-0000-000000000000}\";
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    /// Make `path` (a new, empty folder) an `IO_REPARSE_TAG_MOUNT_POINT` reparse point onto the NT name `target`.
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

/// The identity of the entry `name` under `fd`, as its folder lists it.
fn listed_id(fd: &OwnedFd, name: &str) -> FileId {
    FileId {
        dev: aterm_dirfd::file_id(fd).expect("the folder's id").dev,
        ino: entry(fd, name).ino(),
    }
}

/// `renameat_judged` moves an entry (a file, or a folder and all it holds) through a handle open
/// on it relative to its folder, to a name relative to another folder's handle that `mkdirat`
/// made, and never over an entry there; a name that no longer carries the identity judged, or
/// any flag word but `NOREPLACE` alone, moves nothing.
///
/// Negative controls: the taken destination (`EXIST`), the swapped name (`CHANGED`) and the
/// refused flag words (`INVAL`) each leave both entries as they were; a rename that replaced
/// would leave `stage/b.txt` reading "from".
#[test]
fn renameat_moves_an_entry_by_handle_and_never_over_another() {
    use aterm_dirfd::RenameFlags;
    let tmp = scratch();
    let root = canon(tmp.path());
    let from = root.join("from");
    std::fs::create_dir_all(from.join("sub")).expect("from/sub");
    write(&from.join("a.txt"), "a");
    write(&from.join("sub").join("inner.txt"), "inner");
    write(&from.join("b.txt"), "from");
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    let from_fd = aterm_dirfd::openat(&pinned, "from", dir_flags(), Mode::empty()).expect("from");

    aterm_dirfd::mkdirat(&pinned, "stage", Mode::RWXU).expect("made");
    assert_eq!(
        aterm_dirfd::mkdirat(&pinned, "stage", Mode::RWXU),
        Err(Errno::EXIST),
        "a taken name is never made again"
    );
    assert_eq!(
        aterm_dirfd::mkdirat(&pinned, ".", Mode::RWXU),
        Err(Errno::INVAL)
    );
    let stage = aterm_dirfd::openat(&pinned, "stage", dir_flags(), Mode::empty()).expect("stage");
    write(&root.join("stage").join("b.txt"), "theirs");

    let a = listed_id(&from_fd, "a.txt");
    aterm_dirfd::renameat_judged(
        &from_fd,
        "a.txt",
        &stage,
        "a.txt",
        RenameFlags::NOREPLACE,
        a,
    )
    .expect("a file moves");
    assert!(gone(&from.join("a.txt")));
    assert_eq!(read(&root.join("stage").join("a.txt")), "a");
    assert_eq!(listed_id(&stage, "a.txt"), a, "the same file, renamed");

    let sub = listed_id(&from_fd, "sub");
    aterm_dirfd::renameat_judged(&from_fd, "sub", &stage, "sub", RenameFlags::NOREPLACE, sub)
        .expect("a folder moves whole");
    assert!(gone(&from.join("sub")));
    assert_eq!(
        read(&root.join("stage").join("sub").join("inner.txt")),
        "inner"
    );

    let b = listed_id(&from_fd, "b.txt");
    assert_eq!(
        aterm_dirfd::renameat_judged(
            &from_fd,
            "b.txt",
            &stage,
            "b.txt",
            RenameFlags::NOREPLACE,
            b
        ),
        Err(Errno::EXIST)
    );
    assert_eq!(read(&from.join("b.txt")), "from");
    assert_eq!(read(&root.join("stage").join("b.txt")), "theirs");

    assert_eq!(
        aterm_dirfd::renameat_judged(
            &from_fd,
            "b.txt",
            &stage,
            "c.txt",
            RenameFlags::NOREPLACE,
            a
        ),
        Err(Errno::CHANGED),
        "b.txt is not the file judged"
    );
    for flags in [
        RenameFlags::empty(),
        RenameFlags::EXCHANGE,
        RenameFlags::NOREPLACE | RenameFlags::EXCHANGE,
    ] {
        assert_eq!(
            aterm_dirfd::renameat_judged(&from_fd, "b.txt", &stage, "c.txt", flags, b),
            Err(Errno::INVAL),
            "{flags:?}"
        );
    }
    assert_eq!(read(&from.join("b.txt")), "from");
    assert!(gone(&root.join("stage").join("c.txt")));
    aterm_dirfd::renameat_judged(
        &from_fd,
        "b.txt",
        &stage,
        "c.txt",
        RenameFlags::NOREPLACE,
        b,
    )
    .expect("a free name takes it");
    assert_eq!(read(&root.join("stage").join("c.txt")), "from");
}

/// NTFS refuses to rename a folder while a file under it is open, even one opened with every
/// sharing right and locked as cargo locks a profile's `.cargo-lock`: the premise that keeps a
/// whole removal on Windows from staging a profile whose locks the pass holds. Once the handle
/// closes, the same rename goes. A folder mounted on a volume is never renamed either.
///
/// Negative control: the rename after the handle closed succeeds, so the refusal is the open
/// handle's.
#[test]
fn renameat_of_a_folder_with_a_file_open_under_it_is_refused() {
    use aterm_dirfd::RenameFlags;
    let tmp = scratch();
    let root = canon(tmp.path());
    let profile = root.join("debug");
    std::fs::create_dir_all(profile.join("deps")).expect("debug/deps");
    write(&profile.join(".cargo-lock"), "");
    write(&profile.join("deps").join("lay.rlib"), "rlib");
    let pinned = aterm_dirfd::pin(&root).expect("pinned");
    aterm_dirfd::mkdirat(&pinned, "stage", Mode::RWXU).expect("made");
    let stage = aterm_dirfd::openat(&pinned, "stage", dir_flags(), Mode::empty()).expect("stage");
    let id = listed_id(&pinned, "debug");

    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(profile.join(".cargo-lock"))
        .expect("the lock");
    #[expect(
        clippy::disallowed_methods,
        reason = "a test's build: released by unlock before its handle closes"
    )]
    let taken = lock.try_lock();
    taken.expect("uncontended");
    let refused = aterm_dirfd::renameat_judged(
        &pinned,
        "debug",
        &stage,
        "debug",
        RenameFlags::NOREPLACE,
        id,
    )
    .expect_err("a file open under it keeps the folder where it is");
    assert!(
        [Errno::ACCESS_DENIED, Errno::SHARING_VIOLATION].contains(&refused),
        "{refused:?}"
    );
    assert_eq!(read(&profile.join("deps").join("lay.rlib")), "rlib");
    assert!(gone(&root.join("stage").join("debug")));
    let _ = lock.unlock();
    drop(lock);

    aterm_dirfd::renameat_judged(
        &pinned,
        "debug",
        &stage,
        "debug",
        RenameFlags::NOREPLACE,
        id,
    )
    .expect("let go, it moves");
    assert!(gone(&profile));
    assert_eq!(
        read(
            &root
                .join("stage")
                .join("debug")
                .join("deps")
                .join("lay.rlib")
        ),
        "rlib"
    );

    let mounted = root.join("mounted");
    mount_point::make(&mounted, mount_point::NO_VOLUME);
    let id = listed_id(&pinned, "mounted");
    assert_eq!(
        aterm_dirfd::renameat_judged(
            &pinned,
            "mounted",
            &stage,
            "mounted",
            RenameFlags::NOREPLACE,
            id
        ),
        Err(Errno::MOUNT_POINT)
    );
    assert!(std::fs::symlink_metadata(&mounted).is_ok());
}
