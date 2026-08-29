// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The differential test: `rustix` as the ORACLE for `aterm-dirfd`.
//!
//! This is security machinery. `pinned_dir.rs` exists because a `PathBuf` is a
//! claim about one instant, and a same-uid process can rename an ancestor and
//! leave a symlink at the old pathname before a worker writes. So the tests here
//! are not "does `openat` open a file"; they are:
//!
//! * **Same answer, same errno.** Every operation is scripted through BOTH
//!   implementations against two IDENTICAL directory trees, and the comparison
//!   covers the result, the exact `errno` of every failure, AND a full recursive
//!   snapshot of the tree afterwards (type, permission bits, link count, size).
//!   An implementation that succeeded where the oracle failed, or that left a
//!   different filesystem behind, fails here.
//! * **Inode identity, the basis of every TOCTOU check.** `same_identity()` in
//!   `pinned_dir.rs` reads exactly two fields — `st_dev` and `st_ino` — and
//!   every "is this still the same directory / child / file" guard is built on
//!   them. The two fixture trees have different absolute inode numbers, so they
//!   are compared RELATIONALLY: each run interns the `(dev, ino)` pairs it sees
//!   and reports the first-seen index, so both implementations must agree that
//!   two hard links to one inode are the SAME identity and that two files are
//!   DIFFERENT ones. `identity_fields_agree_bit_for_bit` pins the absolute
//!   values on top of that, over one shared descriptor.
//! * **The TOCTOU property itself.** A retained directory descriptor must keep
//!   addressing the inode it was opened on, even after the pathname it came from
//!   has been made to point somewhere else. That is checked directly, on both
//!   implementations, with the swap performed between the open and the use.
//! * **`Dir::read_from` must not steal the caller's descriptor, and must not
//!   SHARE its offset.** `fdopendir` takes ownership of what it is handed and
//!   `closedir` closes it, so a naive implementation silently closes the
//!   retained pin; a `dup`-based one keeps the pin alive but shares its file
//!   offset, so two streams over one pin split the directory between them. Both
//!   are tested — the second by interleaving two live streams entry by entry
//!   over a directory big enough to cross the `readdir` buffer.
//!
//! Both implementations run against real filesystems, because every property
//! being tested — link counts, rename atomicity, the resolution window — only
//! exists on one.

use std::ffi::OsStr;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

use aterm_dirfd as mine;

// ---------------------------------------------------------------------------
// Comparable results
// ---------------------------------------------------------------------------

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
///
/// The absolute numbers necessarily differ between the trees, but the RELATION
/// must not: two hard links to one inode have to intern to the same token in
/// both runs, two distinct files to different ones, and re-opening a name has
/// to return the token it returned before. That relation is the whole of
/// `same_identity()`, and nothing else in this file was comparing it.
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

fn oracle_flags_dir() -> rustix::fs::OFlags {
    rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC
}

fn mine_flags_file() -> mine::OFlags {
    mine::OFlags::RDONLY | mine::OFlags::NOFOLLOW | mine::OFlags::NONBLOCK | mine::OFlags::CLOEXEC
}

fn oracle_flags_file() -> rustix::fs::OFlags {
    rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC
}

// ---------------------------------------------------------------------------
// The scripted operations
// ---------------------------------------------------------------------------

/// One step of a script, written once and executed by both implementations.
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
    RenameExchange(&'static str, &'static str),
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
        Op::RenameExchange(from, to) => unit(mine::renameat_with(
            root,
            from,
            root,
            to,
            RenameFlags::EXCHANGE,
        )),
        Op::Link(from, to) => unit(mine::linkat(root, from, root, to, AtFlags::empty())),
        Op::Chmod(name, bits) => match mine::openat(root, name, mine_flags_file(), Mode::empty()) {
            Ok(fd) => {
                let mode = mine::Mode::empty();
                let _ = mode;
                // Reconstruct the requested bits from the shared constants so
                // both arms ask for exactly the same permission set.
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

#[allow(
    clippy::unnecessary_cast,
    reason = "rustix's `Stat` field widths are PLATFORM-DEPENDENT, and the widening \
              casts here are what make one expression compile on all of them. \
              `st_ino` already happens to be `u64` on macOS, so the lint fires on \
              THIS host only; taking the cast off to satisfy it would break the \
              targets where the field is narrower — the same reason its `st_dev` \
              and `st_nlink` neighbours are cast and are not flagged."
)]
fn run_oracle(root: &OwnedFd, op: &Op, ids: &mut Identities) -> Outcome {
    use rustix::fs::{AtFlags, Mode, RenameFlags};
    let unit = |r: rustix::io::Result<()>| match r {
        Ok(()) => Outcome::Unit,
        Err(e) => Outcome::Failed(e.raw_os_error()),
    };
    let stat_of = |fd: &OwnedFd, ids: &mut Identities| match rustix::fs::fstat(fd) {
        Ok(s) => Outcome::Opened {
            mode: u32::from(s.st_mode),
            nlink: s.st_nlink as u64,
            identity: ids.token(s.st_dev as u64, s.st_ino),
        },
        Err(e) => Outcome::Failed(e.raw_os_error()),
    };
    match *op {
        Op::OpenDir(name) => {
            match rustix::fs::openat(root, name, oracle_flags_dir(), Mode::empty()) {
                Ok(fd) => stat_of(&fd, ids),
                Err(e) => Outcome::Failed(e.raw_os_error()),
            }
        }
        Op::OpenFile(name) => {
            match rustix::fs::openat(root, name, oracle_flags_file(), Mode::empty()) {
                Ok(fd) => stat_of(&fd, ids),
                Err(e) => Outcome::Failed(e.raw_os_error()),
            }
        }
        Op::CreateExclusive(name) => {
            let flags = rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC;
            match rustix::fs::openat(root, name, flags, Mode::RUSR | Mode::WUSR) {
                Ok(fd) => stat_of(&fd, ids),
                Err(e) => Outcome::Failed(e.raw_os_error()),
            }
        }
        Op::Fstat(name) => match rustix::fs::openat(root, name, oracle_flags_file(), Mode::empty())
        {
            Ok(fd) => match rustix::fs::fstat(&fd) {
                Ok(s) => Outcome::Stat {
                    mode: u32::from(s.st_mode),
                    nlink: s.st_nlink as u64,
                    is_dir: rustix::fs::FileType::from_raw_mode(s.st_mode).is_dir(),
                    is_file: rustix::fs::FileType::from_raw_mode(s.st_mode).is_file(),
                    identity: ids.token(s.st_dev as u64, s.st_ino),
                },
                Err(e) => Outcome::Failed(e.raw_os_error()),
            },
            Err(e) => Outcome::Failed(e.raw_os_error()),
        },
        Op::Mkdir(name) => unit(rustix::fs::mkdirat(root, name, Mode::RWXU)),
        Op::Unlink(name) => unit(rustix::fs::unlinkat(root, name, AtFlags::empty())),
        Op::Rmdir(name) => unit(rustix::fs::unlinkat(root, name, AtFlags::REMOVEDIR)),
        Op::Rename(from, to) => unit(rustix::fs::renameat(root, from, root, to)),
        Op::RenameNoReplace(from, to) => unit(rustix::fs::renameat_with(
            root,
            from,
            root,
            to,
            RenameFlags::NOREPLACE,
        )),
        Op::RenameExchange(from, to) => unit(rustix::fs::renameat_with(
            root,
            from,
            root,
            to,
            RenameFlags::EXCHANGE,
        )),
        Op::Link(from, to) => unit(rustix::fs::linkat(root, from, root, to, AtFlags::empty())),
        Op::Chmod(name, bits) => {
            match rustix::fs::openat(root, name, oracle_flags_file(), Mode::empty()) {
                Ok(fd) => {
                    let mut m = Mode::empty();
                    if bits & 0o400 != 0 {
                        m |= Mode::RUSR;
                    }
                    if bits & 0o200 != 0 {
                        m |= Mode::WUSR;
                    }
                    if bits & 0o100 != 0 {
                        m |= Mode::XUSR;
                    }
                    unit(rustix::fs::fchmod(&fd, m))
                }
                Err(e) => Outcome::Failed(e.raw_os_error()),
            }
        }
        Op::FsyncDir => unit(rustix::fs::fsync(root)),
        Op::ReadDir => match rustix::fs::Dir::read_from(root) {
            Ok(mut dir) => {
                let mut names = Vec::new();
                for entry in &mut dir {
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

// ---------------------------------------------------------------------------
// Fixtures and snapshots
// ---------------------------------------------------------------------------

/// Build the identical starting tree both implementations are driven over.
///
/// It deliberately includes the awkward cases: a symlink pointing at a
/// directory and one pointing at a file (both of which `O_NOFOLLOW` must
/// refuse), a dangling symlink, a non-empty directory, and a file with two
/// links (so a link-count assertion means something).
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

fn open_root_oracle(path: &Path) -> OwnedFd {
    rustix::fs::openat(
        rustix::fs::CWD,
        path,
        oracle_flags_dir(),
        rustix::fs::Mode::empty(),
    )
    .unwrap()
}

/// Run one script through both implementations against two identical trees and
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
    let fb = open_root_oracle(&pb);
    // Non-vacuity: a script that only ever failed (or only ever succeeded)
    // would compare two implementations agreeing about nothing interesting.
    // Every script here must exercise BOTH verdicts.
    let (mut failures, mut successes) = (0usize, 0usize);
    // One identity table PER RUN: the trees have different inode numbers, so
    // what must match is the pattern of sameness, not the numbers.
    let (mut ia, mut ib) = (Identities::default(), Identities::default());
    for (i, op) in script.iter().enumerate() {
        let ra = run_mine(&fa, op, &mut ia);
        let rb = run_oracle(&fb, op, &mut ib);
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

// ---------------------------------------------------------------------------
// The scripts
// ---------------------------------------------------------------------------

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
///
/// The order is deliberate. `twolinks` and `twolinks-b` are two names for one
/// inode, so both implementations must report the SAME identity token for them
/// and a DIFFERENT one for `file`; re-stating `file` at the end must return the
/// token it returned first. That relation is exactly what `same_identity()`
/// decides every TOCTOU question with.
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
/// conversion. So `st_dev` and `st_ino` are also compared absolutely, over one
/// shared descriptor and one shared tree, where the numbers must be equal and
/// not merely consistent. `st_dev` is `i32` on Darwin and the widening is a
/// cast; this is the test that notices if that cast ever changes meaning.
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
        let oracle_stat =
            rustix::fs::openat(&fd, name, oracle_flags_file(), rustix::fs::Mode::empty())
                .and_then(|f| rustix::fs::fstat(&f))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            (mine_stat.st_dev, mine_stat.st_ino),
            (oracle_stat.st_dev as u64, oracle_stat.st_ino as u64),
            "{name}: st_dev/st_ino must match the oracle exactly",
        );
    }
    // ...and the pair really does discriminate: the hard links share it, the
    // distinct files do not.
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

/// The non-clobbering publish and the atomic swap — the two platform-specific
/// rename semantics, which are implemented per-platform rather than emulated.
#[test]
fn flagged_renames_agree_on_both_semantics() {
    compare_script(
        "renameat_with",
        &[
            // NOREPLACE must SUCCEED into a free name...
            Op::RenameNoReplace("file", "published"),
            // ...and REFUSE an occupied one, leaving both names in place.
            Op::RenameNoReplace("published", "twolinks"),
            Op::RenameNoReplace("missing", "nope"),
            Op::RenameNoReplace("dir", "empty"),
            // EXCHANGE swaps two existing names atomically.
            Op::RenameExchange("published", "twolinks"),
            Op::RenameExchange("dir", "empty"),
            // ...and fails when a side is absent.
            Op::RenameExchange("published", "missing"),
            Op::RenameExchange("missing", "published"),
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
            // Failures interleaved with the listings: a refused mutation must
            // leave the directory contents exactly as they were, in both.
            Op::Unlink("file"),
            Op::Rmdir("dir"),
            Op::ReadDir,
            Op::Rmdir("zz"),
            Op::ReadDir,
        ],
    );
}

/// A big directory, so the listing crosses whatever buffer either
/// implementation reads in.
#[test]
fn large_directory_listings_agree() {
    let a = aterm_tempfile::tempdir().unwrap();
    let b = aterm_tempfile::tempdir().unwrap();
    for root in [a.path(), b.path()] {
        for i in 0..500 {
            std::fs::write(root.join(format!("entry-{i:04}")), b"x").unwrap();
        }
    }
    let fa = open_root_mine(a.path());
    let fb = open_root_oracle(b.path());
    let ra = run_mine(&fa, &Op::ReadDir, &mut Identities::default());
    let rb = run_oracle(&fb, &Op::ReadDir, &mut Identities::default());
    assert_eq!(ra, rb);
    let Outcome::Names(names) = ra else {
        panic!("expected a listing, got {ra:?}")
    };
    // 500 entries plus `.` and `..`, which both implementations must return
    // (the callers filter them, and a wrapper that filtered for them would
    // silently change what those filters mean).
    assert_eq!(names.len(), 502, "entries plus . and ..");
    assert!(names.contains(&".".to_string()));
    assert!(names.contains(&"..".to_string()));
}

/// TWO live streams over ONE descriptor must each see the WHOLE directory.
///
/// This is the test that a `dup`-based `Dir::read_from` fails. A duplicate
/// shares the original's open file description and therefore its offset, so two
/// streams built that way consume one shared position: each returns roughly
/// half the entries, with no error anywhere, and a second `read_from` that
/// rewinds restarts a stream already in flight. The callers are exactly the
/// shape that triggers it — a pinned directory is `Clone` over one
/// `Arc<OwnedFd>` and is read from more than one place.
///
/// The directory is deliberately larger than any plausible `readdir` buffer, and
/// the two streams are advanced ONE ENTRY AT A TIME so they are genuinely
/// interleaved rather than merely both alive. Measured against the `dup`
/// implementation this replaced: 986 and 1016 of 2002.
#[test]
fn two_interleaved_streams_over_one_descriptor_each_see_every_entry() {
    const N: usize = 2000;
    let a = aterm_tempfile::tempdir().unwrap();
    let b = aterm_tempfile::tempdir().unwrap();
    for root in [a.path(), b.path()] {
        for i in 0..N {
            std::fs::write(root.join(format!("entry-{i:05}")), b"x").unwrap();
        }
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

    // The oracle, interleaved the same way, over the identical second tree.
    let fb = open_root_oracle(b.path());
    let mut oleft = rustix::fs::Dir::read_from(&fb).unwrap();
    let mut oright = rustix::fs::Dir::read_from(&fb).unwrap();
    let (mut oln, mut orn): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    loop {
        let l = oleft.read();
        let r = oright.read();
        if l.is_none() && r.is_none() {
            break;
        }
        if let Some(e) = l {
            oln.push(String::from_utf8_lossy(e.unwrap().file_name().to_bytes()).into_owned());
        }
        if let Some(e) = r {
            orn.push(String::from_utf8_lossy(e.unwrap().file_name().to_bytes()).into_owned());
        }
    }
    oln.sort();
    orn.sort();

    // N entries plus `.` and `..`, in EVERY one of the four streams.
    for (label, got) in [
        ("aterm-dirfd stream 1", &ln),
        ("aterm-dirfd stream 2", &rn),
        ("rustix stream 1", &oln),
        ("rustix stream 2", &orn),
    ] {
        assert_eq!(
            got.len(),
            N + 2,
            "{label} saw {} of {} entries — the two streams are sharing one file offset",
            got.len(),
            N + 2,
        );
    }
    assert_eq!(ln, rn, "the two aterm-dirfd streams must agree");
    assert_eq!(ln, oln, "aterm-dirfd and rustix must agree");
    assert_eq!(oln, orn, "the two rustix streams must agree");
}

/// A stream over a directory that has been REMOVED reads as empty rather than
/// erroring — `rustix` treats a vanished `"."` that way deliberately, and a
/// retention sweep that has already emptied and unlinked a directory must not
/// then report a failure for it.
#[test]
fn a_removed_directory_reads_as_empty_in_both() {
    let base = aterm_tempfile::tempdir().unwrap();
    for oracle in [false, true] {
        let doomed = base.path().join(if oracle { "o" } else { "m" });
        std::fs::create_dir(&doomed).unwrap();
        std::fs::write(doomed.join("child"), b"x").unwrap();
        let names = if oracle {
            let fd = open_root_oracle(&doomed);
            std::fs::remove_file(doomed.join("child")).unwrap();
            std::fs::remove_dir(&doomed).unwrap();
            run_oracle(&fd, &Op::ReadDir, &mut Identities::default())
        } else {
            let fd = open_root_mine(&doomed);
            std::fs::remove_file(doomed.join("child")).unwrap();
            std::fs::remove_dir(&doomed).unwrap();
            run_mine(&fd, &Op::ReadDir, &mut Identities::default())
        };
        assert_eq!(
            names,
            Outcome::Names(Vec::new()),
            "{}: a removed directory should read as empty",
            if oracle { "oracle" } else { "aterm-dirfd" },
        );
    }
}

// ---------------------------------------------------------------------------
// The properties the callers actually depend on
// ---------------------------------------------------------------------------

/// Reading a directory must not consume the caller's descriptor.
///
/// `fdopendir` takes ownership of the descriptor it is handed and `closedir`
/// closes it. An implementation that passes the caller's fd straight through
/// would close the retained pin the whole design rests on — and would then fail
/// in a way that looks like a filesystem error, not a bug. Both implementations
/// must keep the descriptor usable afterwards.
#[test]
fn reading_a_directory_leaves_the_callers_descriptor_usable() {
    for oracle in [false, true] {
        let dir = aterm_tempfile::tempdir().unwrap();
        build_tree(dir.path());
        let ids = &mut Identities::default();
        if oracle {
            let fd = open_root_oracle(dir.path());
            for _ in 0..3 {
                let _ = run_oracle(&fd, &Op::ReadDir, ids);
            }
            assert_eq!(
                run_oracle(&fd, &Op::ReadDir, ids),
                run_oracle(&fd, &Op::ReadDir, ids),
                "oracle: repeated reads through the same descriptor",
            );
            assert!(matches!(
                run_oracle(&fd, &Op::OpenFile("file"), ids),
                Outcome::Opened { .. }
            ));
        } else {
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
    }
}

/// THE property. A retained directory descriptor keeps addressing the inode it
/// was opened on, even after the pathname it came from has been repointed at a
/// different directory. Both implementations must write into the ORIGINAL.
///
/// This is the attack `pinned_dir` exists to defeat: another same-uid process
/// renames an ancestor and drops a replacement (or a symlink) at the old name
/// between the check and the use.
#[test]
fn a_retained_descriptor_never_follows_a_swapped_pathname() {
    for oracle in [false, true] {
        let base = aterm_tempfile::tempdir().unwrap();
        let real = base.path().join("target");
        let decoy = base.path().join("decoy");
        std::fs::create_dir(&real).unwrap();
        std::fs::create_dir(&decoy).unwrap();

        // Pin the real directory, then swap the pathname out from under it.
        let pinned = if oracle {
            open_root_oracle(&real)
        } else {
            open_root_mine(&real)
        };
        std::fs::rename(&real, base.path().join("moved-away")).unwrap();
        std::fs::rename(&decoy, &real).unwrap();

        // Writing through the pin must land in the ORIGINAL directory, which is
        // now called `moved-away` — never in the decoy now sitting at `target`.
        let op = Op::CreateExclusive("witness");
        let ids = &mut Identities::default();
        let outcome = if oracle {
            run_oracle(&pinned, &op, ids)
        } else {
            run_mine(&pinned, &op, ids)
        };
        assert!(
            matches!(outcome, Outcome::Opened { .. }),
            "{}: the pinned create should succeed, got {outcome:?}",
            if oracle { "oracle" } else { "aterm-dirfd" },
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
}

/// `ENOENT` must be the same number in both, because `remove_file_if_exists`
/// matches on it to decide "already gone" versus "the filesystem refused me".
#[test]
fn the_not_found_errno_is_the_same_constant() {
    assert_eq!(
        mine::Errno::NOENT.raw_os_error(),
        rustix::io::Errno::NOENT.raw_os_error(),
    );
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
    // The oracle refuses it too; the point is that NEITHER opens `real`.
    assert!(rustix::fs::openat(&fd, evil, oracle_flags_file(), rustix::fs::Mode::empty()).is_err());
}

/// Long names: the wrapper keeps short names off the heap, and the boundary
/// between that fast path and the heap fallback must not change behaviour.
#[test]
fn names_across_the_stack_buffer_boundary_agree() {
    let a = aterm_tempfile::tempdir().unwrap();
    let b = aterm_tempfile::tempdir().unwrap();
    let fa = open_root_mine(a.path());
    let fb = open_root_oracle(b.path());
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
        let rb = rustix::fs::openat(
            &fb,
            name.as_str(),
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .map(|_| ())
        .map_err(|e| e.raw_os_error());
        assert_eq!(ra, rb, "name of length {len}");
    }
}
