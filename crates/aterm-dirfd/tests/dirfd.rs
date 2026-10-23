// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-dirfd` against `std`: the same system calls made through `std::fs` (and, for the flagged renames, their documented semantics).

#![cfg(unix)]

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};

use aterm_dirfd as mine;

/// Every operation reduces to this, so comparing two runs compares everything
/// an implementation could get wrong: the verdict, the exact errno, and any
/// value read back.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Unit,
    Stat {
        mode: u32,
        nlink: u64,
        is_dir: bool,
        is_file: bool,
        /// See [`Identities`]: the `(st_dev, st_ino)` pair, interned per run.
        identity: usize,
    },
    /// Directory entries, sorted — two independent `readdir` streams over the
    /// same directory need not agree on ORDER, only on contents.
    Names(Vec<String>),
    /// An open succeeded; the identity of what was opened is compared through a
    /// follow-up `fstat` rather than a meaningless descriptor number.
    Opened {
        mode: u32,
        nlink: u64,
        identity: usize,
    },
    Failed(i32),
}

/// Interned `(st_dev, st_ino)` pairs, so inode identity can be compared across
/// two DIFFERENT fixture trees.
#[derive(Default)]
struct Identities(Vec<(u64, u64)>);

impl Identities {
    fn token(&mut self, dev: u64, ino: u64) -> usize {
        if let Some(i) = self.0.iter().position(|&p| p == (dev, ino)) {
            return i;
        }
        self.0.push((dev, ino));
        self.0.len() - 1
    }
}

fn mine_flags_dir() -> mine::OFlags {
    mine::OFlags::RDONLY
        | mine::OFlags::DIRECTORY
        | mine::OFlags::NOFOLLOW
        | mine::OFlags::NONBLOCK
        | mine::OFlags::CLOEXEC
}

fn mine_flags_file() -> mine::OFlags {
    mine::OFlags::RDONLY | mine::OFlags::NOFOLLOW | mine::OFlags::NONBLOCK | mine::OFlags::CLOEXEC
}

/// One step of a script, written once and executed by both sides.
#[derive(Debug, Clone)]
enum Op {
    OpenDir(&'static str),
    OpenFile(&'static str),
    /// `O_WRONLY | O_CREAT | O_EXCL` with 0600 — the private-file create.
    CreateExclusive(&'static str),
    Fstat(&'static str),
    Mkdir(&'static str),
    Unlink(&'static str),
    Rmdir(&'static str),
    Rename(&'static str, &'static str),
    RenameNoReplace(&'static str, &'static str),
    Link(&'static str, &'static str),
    Chmod(&'static str, u32),
    FsyncDir,
    ReadDir,
}

fn run_mine(root: &OwnedFd, op: &Op, ids: &mut Identities) -> Outcome {
    use mine::{AtFlags, Mode, RenameFlags};
    let unit = |r: mine::Result<()>| match r {
        Ok(()) => Outcome::Unit,
        Err(e) => Outcome::Failed(e.raw_os_error()),
    };
    match *op {
        Op::OpenDir(name) => match mine::openat(root, name, mine_flags_dir(), Mode::empty()) {
            Ok(fd) => match mine::fstat(&fd) {
                Ok(s) => Outcome::Opened {
                    mode: s.st_mode,
                    nlink: s.st_nlink,
                    identity: ids.token(s.st_dev, s.st_ino),
                },
                Err(e) => Outcome::Failed(e.raw_os_error()),
            },
            Err(e) => Outcome::Failed(e.raw_os_error()),
        },
        Op::OpenFile(name) => match mine::openat(root, name, mine_flags_file(), Mode::empty()) {
            Ok(fd) => match mine::fstat(&fd) {
                Ok(s) => Outcome::Opened {
                    mode: s.st_mode,
                    nlink: s.st_nlink,
                    identity: ids.token(s.st_dev, s.st_ino),
                },
                Err(e) => Outcome::Failed(e.raw_os_error()),
            },
            Err(e) => Outcome::Failed(e.raw_os_error()),
        },
        Op::CreateExclusive(name) => {
            let flags = mine::OFlags::WRONLY
                | mine::OFlags::CREATE
                | mine::OFlags::EXCL
                | mine::OFlags::NOFOLLOW
                | mine::OFlags::NONBLOCK
                | mine::OFlags::CLOEXEC;
            match mine::openat(root, name, flags, Mode::RUSR | Mode::WUSR) {
                Ok(fd) => match mine::fstat(&fd) {
                    Ok(s) => Outcome::Opened {
                        mode: s.st_mode,
                        nlink: s.st_nlink,
                        identity: ids.token(s.st_dev, s.st_ino),
                    },
                    Err(e) => Outcome::Failed(e.raw_os_error()),
                },
                Err(e) => Outcome::Failed(e.raw_os_error()),
            }
        }
        Op::Fstat(name) => match mine::openat(root, name, mine_flags_file(), Mode::empty()) {
            Ok(fd) => match mine::fstat(&fd) {
                Ok(s) => Outcome::Stat {
                    mode: s.st_mode,
                    nlink: s.st_nlink,
                    is_dir: mine::FileType::from_raw_mode(s.st_mode).is_dir(),
                    is_file: mine::FileType::from_raw_mode(s.st_mode).is_file(),
                    identity: ids.token(s.st_dev, s.st_ino),
                },
                Err(e) => Outcome::Failed(e.raw_os_error()),
            },
            Err(e) => Outcome::Failed(e.raw_os_error()),
        },
        Op::Mkdir(name) => unit(mine::mkdirat(root, name, Mode::RWXU)),
        Op::Unlink(name) => unit(mine::unlinkat(root, name, AtFlags::empty())),
        Op::Rmdir(name) => unit(mine::unlinkat(root, name, AtFlags::REMOVEDIR)),
        Op::Rename(from, to) => unit(mine::renameat(root, from, root, to)),
        Op::RenameNoReplace(from, to) => unit(mine::renameat_with(
            root,
            from,
            root,
            to,
            RenameFlags::NOREPLACE,
        )),
        Op::Link(from, to) => unit(mine::linkat(root, from, root, to, AtFlags::empty())),
        Op::Chmod(name, bits) => match mine::openat(root, name, mine_flags_file(), Mode::empty()) {
            Ok(fd) => {
                let mut m = mine::Mode::empty();
                if bits & 0o400 != 0 {
                    m |= mine::Mode::RUSR;
                }
                if bits & 0o200 != 0 {
                    m |= mine::Mode::WUSR;
                }
                if bits & 0o100 != 0 {
                    m |= mine::Mode::XUSR;
                }
                unit(mine::fchmod(&fd, m))
            }
            Err(e) => Outcome::Failed(e.raw_os_error()),
        },
        Op::FsyncDir => unit(mine::fsync(root)),
        Op::ReadDir => match mine::Dir::read_from(root) {
            Ok(dir) => {
                let mut names = Vec::new();
                for entry in dir {
                    match entry {
                        Ok(e) => names
                            .push(String::from_utf8_lossy(e.file_name().to_bytes()).into_owned()),
                        Err(e) => return Outcome::Failed(e.raw_os_error()),
                    }
                }
                names.sort();
                Outcome::Names(names)
            }
            Err(e) => Outcome::Failed(e.raw_os_error()),
        },
    }
}

fn errno(e: &std::io::Error) -> Outcome {
    Outcome::Failed(e.raw_os_error().expect("an OS error"))
}

/// `std` opens with `O_CLOEXEC` itself; the rest of the flag set is the same.
fn reference_open(path: &Path, directory: bool) -> std::io::Result<std::fs::File> {
    let mut flags = libc::O_NOFOLLOW | libc::O_NONBLOCK;
    if directory {
        flags |= libc::O_DIRECTORY;
    }
    OpenOptions::new().read(true).custom_flags(flags).open(path)
}

fn opened(file: std::io::Result<std::fs::File>, ids: &mut Identities) -> Outcome {
    match file.and_then(|f| f.metadata()) {
        Ok(m) => Outcome::Opened {
            mode: m.mode(),
            nlink: m.nlink(),
            identity: ids.token(m.dev(), m.ino()),
        },
        Err(e) => errno(&e),
    }
}

/// The reference: the same operation through `std::fs`, on a path under `root`.
fn run_reference(root: &Path, op: &Op, ids: &mut Identities) -> Outcome {
    let unit = |r: std::io::Result<()>| match r {
        Ok(()) => Outcome::Unit,
        Err(e) => errno(&e),
    };
    let exists = |name: &str| std::fs::symlink_metadata(root.join(name)).is_ok();
    match *op {
        Op::OpenDir(name) => opened(reference_open(&root.join(name), true), ids),
        Op::OpenFile(name) => opened(reference_open(&root.join(name), false), ids),
        Op::CreateExclusive(name) => opened(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(root.join(name)),
            ids,
        ),
        Op::Fstat(name) => {
            match reference_open(&root.join(name), false).and_then(|f| f.metadata()) {
                Ok(m) => Outcome::Stat {
                    mode: m.mode(),
                    nlink: m.nlink(),
                    is_dir: m.is_dir(),
                    is_file: m.is_file(),
                    identity: ids.token(m.dev(), m.ino()),
                },
                Err(e) => errno(&e),
            }
        }
        Op::Mkdir(name) => unit(
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(root.join(name)),
        ),
        Op::Unlink(name) => unit(std::fs::remove_file(root.join(name))),
        Op::Rmdir(name) => unit(std::fs::remove_dir(root.join(name))),
        Op::Rename(from, to) => unit(std::fs::rename(root.join(from), root.join(to))),
        Op::RenameNoReplace(from, to) => {
            if !exists(from) {
                Outcome::Failed(libc::ENOENT)
            } else if exists(to) {
                Outcome::Failed(libc::EEXIST)
            } else {
                unit(std::fs::rename(root.join(from), root.join(to)))
            }
        }
        Op::Link(from, to) => unit(std::fs::hard_link(root.join(from), root.join(to))),
        Op::Chmod(name, bits) => match reference_open(&root.join(name), false) {
            Ok(f) => unit(f.set_permissions(std::fs::Permissions::from_mode(bits))),
            Err(e) => errno(&e),
        },
        #[expect(
            clippy::disallowed_methods,
            reason = "the reference model's own directory flush; the test compares results, not tiers"
        )]
        Op::FsyncDir => unit(reference_open(root, true).and_then(|f| f.sync_all())),
        Op::ReadDir => match std::fs::read_dir(root) {
            Ok(entries) => {
                let mut names = vec![".".to_string(), "..".to_string()];
                for entry in entries {
                    match entry {
                        Ok(e) => names.push(e.file_name().to_string_lossy().into_owned()),
                        Err(e) => return errno(&e),
                    }
                }
                names.sort();
                Outcome::Names(names)
            }
            Err(e) => errno(&e),
        },
    }
}

/// Build the identical starting tree both sides are driven over.
fn build_tree(root: &Path) {
    use std::os::unix::fs::symlink;
    std::fs::create_dir_all(root.join("dir")).unwrap();
    std::fs::create_dir_all(root.join("dir/nested")).unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    std::fs::write(root.join("file"), b"contents").unwrap();
    std::fs::write(root.join("dir/inner"), b"inner").unwrap();
    std::fs::write(root.join("twolinks"), b"shared").unwrap();
    std::fs::hard_link(root.join("twolinks"), root.join("twolinks-b")).unwrap();
    symlink("dir", root.join("link-to-dir")).unwrap();
    symlink("file", root.join("link-to-file")).unwrap();
    symlink("nowhere", root.join("dangling")).unwrap();
}

/// A full recursive description of a tree: what an operation LEFT BEHIND, not
/// merely what it returned.
fn snapshot(root: &Path) -> Vec<String> {
    use std::os::unix::fs::MetadataExt as _;
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            let kind = if meta.is_dir() {
                "dir"
            } else if meta.file_type().is_symlink() {
                "symlink"
            } else {
                "file"
            };
            out.push(format!(
                "{} {kind} mode={:o} nlink={} size={}",
                rel.display(),
                meta.mode() & 0o7777,
                meta.nlink(),
                if meta.is_dir() { 0 } else { meta.len() },
            ));
            if meta.is_dir() {
                stack.push(path);
            }
        }
    }
    out.sort();
    out
}

fn open_root_mine(path: &Path) -> OwnedFd {
    mine::openat(mine::CWD, path, mine_flags_dir(), mine::Mode::empty()).unwrap()
}

/// Run one script through both sides against two identical trees and
/// require agreement at EVERY step, plus an identical tree at the end.
fn compare_script(label: &str, script: &[Op]) {
    let a = aterm_tempfile::tempdir().unwrap();
    let b = aterm_tempfile::tempdir().unwrap();
    let (pa, pb): (PathBuf, PathBuf) = (a.path().join("t"), b.path().join("t"));
    std::fs::create_dir(&pa).unwrap();
    std::fs::create_dir(&pb).unwrap();
    build_tree(&pa);
    build_tree(&pb);
    assert_eq!(
        snapshot(&pa),
        snapshot(&pb),
        "{label}: the two fixtures must start identical"
    );

    let fa = open_root_mine(&pa);
    let (mut failures, mut successes) = (0usize, 0usize);
    let (mut ia, mut ib) = (Identities::default(), Identities::default());
    for (i, op) in script.iter().enumerate() {
        let ra = run_mine(&fa, op, &mut ia);
        let rb = run_reference(&pb, op, &mut ib);
        if matches!(ra, Outcome::Failed(_)) {
            failures += 1;
        } else {
            successes += 1;
        }
        assert_eq!(ra, rb, "{label}: step {i} {op:?} disagreed");
        assert_eq!(
            snapshot(&pa),
            snapshot(&pb),
            "{label}: step {i} {op:?} left different trees behind",
        );
    }
    assert!(
        failures > 0 && successes > 0,
        "{label}: script is vacuous — {successes} successes, {failures} failures",
    );
}

/// Opens, including every way one is supposed to FAIL: a symlink under
/// `O_NOFOLLOW`, a file opened `O_DIRECTORY`, a directory opened as a file, an
/// absent name, and an exclusive create over an existing name.
#[test]
fn opens_agree_including_every_refusal() {
    compare_script(
        "opens",
        &[
            Op::OpenDir("dir"),
            Op::OpenDir("empty"),
            Op::OpenDir("file"),
            Op::OpenDir("link-to-dir"),
            Op::OpenDir("dangling"),
            Op::OpenDir("missing"),
            Op::OpenFile("file"),
            Op::OpenFile("twolinks"),
            Op::OpenFile("link-to-file"),
            Op::OpenFile("dangling"),
            Op::OpenFile("missing"),
            Op::OpenFile("dir"),
            Op::CreateExclusive("fresh"),
            Op::CreateExclusive("fresh"),
            Op::CreateExclusive("file"),
            Op::CreateExclusive("link-to-file"),
        ],
    );
}

/// `fstat` fields, the type predicates built on them, and — the part that
/// matters most — inode IDENTITY.
#[test]
fn stats_agree_on_every_field_read_including_inode_identity() {
    compare_script(
        "stats",
        &[
            Op::Fstat("file"),
            Op::Fstat("twolinks"),
            Op::Fstat("twolinks-b"),
            Op::Fstat("dir"),
            Op::Fstat("empty"),
            Op::Fstat("link-to-file"),
            Op::Fstat("missing"),
            Op::Fstat("file"),
        ],
    );
}

/// The relational check above cannot see a change to how the raw platform
/// fields are WIDENED, because both sides of it are produced by the same
/// conversion.
#[test]
fn identity_fields_agree_bit_for_bit() {
    let dir = aterm_tempfile::tempdir().unwrap();
    build_tree(dir.path());
    let fd = open_root_mine(dir.path());
    for name in [
        "file",
        "twolinks",
        "twolinks-b",
        "dir",
        "empty",
        "dir/inner",
    ] {
        let mine_stat = mine::openat(&fd, name, mine_flags_file(), mine::Mode::empty())
            .and_then(|f| mine::fstat(&f))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let reference = std::fs::symlink_metadata(dir.path().join(name))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            (mine_stat.st_dev, mine_stat.st_ino),
            (reference.dev(), reference.ino()),
            "{name}: st_dev/st_ino must match std exactly",
        );
    }
    let id = |name: &str| {
        let s = mine::openat(&fd, name, mine_flags_file(), mine::Mode::empty())
            .and_then(|f| mine::fstat(&f))
            .unwrap();
        (s.st_dev, s.st_ino)
    };
    assert_eq!(id("twolinks"), id("twolinks-b"));
    assert_ne!(id("twolinks"), id("file"));
}

/// Creation and removal, including the errors: rmdir on a non-empty directory,
/// rmdir on a file, unlink on a directory, unlink on an absent name.
#[test]
fn creates_and_removals_agree_including_every_error() {
    compare_script(
        "mutations",
        &[
            Op::Mkdir("new"),
            Op::Mkdir("new"),
            Op::Mkdir("file"),
            Op::Rmdir("new"),
            Op::Rmdir("new"),
            Op::Rmdir("dir"),
            Op::Rmdir("file"),
            Op::Rmdir("empty"),
            Op::Unlink("file"),
            Op::Unlink("file"),
            Op::Unlink("dir"),
            Op::Unlink("dangling"),
            Op::Unlink("twolinks-b"),
            Op::Fstat("twolinks"),
        ],
    );
}

/// Renames, links and mode changes — including the plain rename's CLOBBERING
/// behaviour, which is exactly what `NOREPLACE` exists to avoid.
#[test]
fn renames_links_and_modes_agree() {
    compare_script(
        "renames",
        &[
            Op::Rename("file", "renamed"),
            Op::Rename("missing", "nope"),
            Op::Rename("renamed", "twolinks"),
            Op::Link("twolinks", "third"),
            Op::Link("twolinks", "third"),
            Op::Link("missing", "nope"),
            Op::Chmod("third", 0o400),
            Op::Chmod("third", 0o600),
            Op::Chmod("missing", 0o600),
            Op::FsyncDir,
        ],
    );
}

/// The non-clobbering publish — the platform-specific rename semantics, which
/// are implemented per-platform rather than emulated.
#[test]
fn flagged_renames_agree() {
    compare_script(
        "renameat_with",
        &[
            Op::RenameNoReplace("file", "published"),
            Op::RenameNoReplace("published", "twolinks"),
            Op::RenameNoReplace("missing", "nope"),
            Op::RenameNoReplace("dir", "empty"),
        ],
    );
}

/// Directory listings, before and after mutation.
#[test]
fn directory_listings_agree() {
    compare_script(
        "readdir",
        &[
            Op::ReadDir,
            Op::Mkdir("zz"),
            Op::ReadDir,
            Op::Unlink("file"),
            Op::Unlink("file"),
            Op::Rmdir("dir"),
            Op::ReadDir,
            Op::Rmdir("zz"),
            Op::ReadDir,
        ],
    );
}

/// TWO live streams over ONE descriptor must each see the WHOLE directory.
#[test]
fn two_interleaved_streams_over_one_descriptor_each_see_every_entry() {
    const N: usize = 2000;
    let a = aterm_tempfile::tempdir().unwrap();
    for i in 0..N {
        std::fs::File::create(a.path().join(format!("entry-{i:05}"))).unwrap();
    }

    let fa = open_root_mine(a.path());
    let mut left = mine::Dir::read_from(&fa).unwrap();
    let mut right = mine::Dir::read_from(&fa).unwrap();
    let (mut ln, mut rn): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    loop {
        let l = left.next();
        let r = right.next();
        if l.is_none() && r.is_none() {
            break;
        }
        if let Some(e) = l {
            ln.push(String::from_utf8_lossy(e.unwrap().file_name().to_bytes()).into_owned());
        }
        if let Some(e) = r {
            rn.push(String::from_utf8_lossy(e.unwrap().file_name().to_bytes()).into_owned());
        }
    }
    ln.sort();
    rn.sort();

    for (label, got) in [("stream 1", &ln), ("stream 2", &rn)] {
        assert_eq!(
            got.len(),
            N + 2,
            "{label} saw {} of {} entries — the two streams are sharing one file offset",
            got.len(),
            N + 2,
        );
    }
    assert_eq!(ln, rn, "the two streams must agree");
    assert_eq!(
        Outcome::Names(ln),
        run_reference(a.path(), &Op::ReadDir, &mut Identities::default()),
        "the streams must see what std sees"
    );
}

/// A stream over a directory that has been REMOVED reads as empty rather than
/// erroring: a retention sweep that has already emptied and unlinked a
/// directory must not then report a failure for it.
#[test]
fn a_removed_directory_reads_as_empty() {
    let base = aterm_tempfile::tempdir().unwrap();
    let doomed = base.path().join("m");
    std::fs::create_dir(&doomed).unwrap();
    std::fs::write(doomed.join("child"), b"x").unwrap();
    let fd = open_root_mine(&doomed);
    std::fs::remove_file(doomed.join("child")).unwrap();
    std::fs::remove_dir(&doomed).unwrap();
    assert_eq!(
        run_mine(&fd, &Op::ReadDir, &mut Identities::default()),
        Outcome::Names(Vec::new()),
        "a removed directory should read as empty",
    );
}

/// Reading a directory must not consume the caller's descriptor.
#[test]
fn reading_a_directory_leaves_the_callers_descriptor_usable() {
    let dir = aterm_tempfile::tempdir().unwrap();
    build_tree(dir.path());
    let ids = &mut Identities::default();
    let fd = open_root_mine(dir.path());
    for _ in 0..3 {
        let _ = run_mine(&fd, &Op::ReadDir, ids);
    }
    assert_eq!(
        run_mine(&fd, &Op::ReadDir, ids),
        run_mine(&fd, &Op::ReadDir, ids),
        "repeated reads through the same descriptor",
    );
    assert!(
        matches!(
            run_mine(&fd, &Op::OpenFile("file"), ids),
            Outcome::Opened { .. }
        ),
        "the descriptor must still be usable after reading the directory",
    );
}

/// THE property. A retained directory descriptor keeps addressing the inode it
/// was opened on, even after the pathname it came from has been repointed at a
/// different directory.
#[test]
fn a_retained_descriptor_never_follows_a_swapped_pathname() {
    let base = aterm_tempfile::tempdir().unwrap();
    let real = base.path().join("target");
    let decoy = base.path().join("decoy");
    std::fs::create_dir(&real).unwrap();
    std::fs::create_dir(&decoy).unwrap();

    let pinned = open_root_mine(&real);
    std::fs::rename(&real, base.path().join("moved-away")).unwrap();
    std::fs::rename(&decoy, &real).unwrap();

    let outcome = run_mine(
        &pinned,
        &Op::CreateExclusive("witness"),
        &mut Identities::default(),
    );
    assert!(
        matches!(outcome, Outcome::Opened { .. }),
        "the pinned create should succeed, got {outcome:?}",
    );
    assert!(
        base.path().join("moved-away/witness").exists(),
        "the write must land in the pinned inode",
    );
    assert!(
        !real.join("witness").exists(),
        "the write must NOT follow the pathname to the decoy",
    );
}

/// `ENOENT` must be the platform's number, because `remove_file_if_exists`
/// matches on it to decide "already gone" versus "the filesystem refused me".
#[test]
fn the_not_found_errno_is_the_same_constant() {
    assert_eq!(mine::Errno::NOENT.raw_os_error(), libc::ENOENT);
    let dir = aterm_tempfile::tempdir().unwrap();
    let fa = open_root_mine(dir.path());
    let err = mine::unlinkat(&fa, "nothing-here", mine::AtFlags::empty()).unwrap_err();
    assert_eq!(err, mine::Errno::NOENT);
}

/// A name with an interior NUL must be REFUSED, not silently truncated at the
/// NUL — truncation would address a different file.
#[test]
fn an_interior_nul_is_refused_rather_than_truncated() {
    use std::os::unix::ffi::OsStrExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("real"), b"x").unwrap();
    let fd = open_root_mine(dir.path());
    let evil = OsStr::from_bytes(b"real\0ignored");
    let err = mine::openat(&fd, evil, mine_flags_file(), mine::Mode::empty()).unwrap_err();
    assert_eq!(err, mine::Errno::INVAL);
}

/// Long names: the wrapper keeps short names off the heap, and the boundary
/// between that fast path and the heap fallback must not change behaviour.
#[test]
fn names_across_the_stack_buffer_boundary_agree() {
    let a = aterm_tempfile::tempdir().unwrap();
    let b = aterm_tempfile::tempdir().unwrap();
    let fa = open_root_mine(a.path());
    for len in [1usize, 2, 200, 254, 255, 511, 512, 513, 900] {
        let name = "n".repeat(len);
        let ra = mine::openat(
            &fa,
            name.as_str(),
            mine::OFlags::WRONLY
                | mine::OFlags::CREATE
                | mine::OFlags::EXCL
                | mine::OFlags::CLOEXEC,
            mine::Mode::RUSR | mine::Mode::WUSR,
        )
        .map(|_| ())
        .map_err(|e| e.raw_os_error());
        let rb = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(b.path().join(&name))
            .map(|_| ())
            .map_err(|e| e.raw_os_error().expect("an OS error"));
        assert_eq!(ra, rb, "name of length {len}");
    }
}
