// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A home's content as the conformance searches keep its states and put them back
//! (`log_entries/held_conformance_tests.rs`, `log_entries/waiting_conformance_tests.rs`): each
//! directory and regular file under the home, with its mode, and each file's bytes; and what a
//! lab last read or wrote of its home ([`Known`]), with each file's metadata then, so that a read
//! opens only the files that changed since.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A home's content: each directory and regular file by its path under the home, with its mode,
/// and each file's bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Content {
    pub(crate) dirs: BTreeMap<PathBuf, u32>,
    pub(crate) files: BTreeMap<PathBuf, (u32, Vec<u8>)>,
}

impl Content {
    /// What is under `home` now, every file read.
    pub(crate) fn of(home: &Path) -> Self {
        Known::default().now(home).content
    }

    /// Make `home`, which holds `self`, hold `to`: remove each file and directory `to` has not,
    /// make each directory it has and `self` has not or has with another mode, and write each
    /// file whose mode or bytes differ.
    pub(crate) fn turn_into(&self, to: &Self, home: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        for path in self
            .files
            .keys()
            .filter(|path| !to.files.contains_key(*path))
        {
            std::fs::remove_file(home.join(path)).unwrap();
        }
        for path in self
            .dirs
            .keys()
            .rev()
            .filter(|path| !to.dirs.contains_key(*path))
        {
            std::fs::remove_dir(home.join(path)).unwrap();
        }
        for (path, &mode) in &to.dirs {
            if self.dirs.get(path) != Some(&mode) {
                crate::fsx::ensure_private_dir(&home.join(path)).unwrap();
                std::fs::set_permissions(home.join(path), std::fs::Permissions::from_mode(mode))
                    .unwrap();
            }
        }
        for (path, file) in &to.files {
            let was = self.files.get(path);
            if was != Some(file) {
                std::fs::write(home.join(path), &file.1).unwrap();
                // A write into a file there keeps its mode.
                if was.map(|(mode, _)| *mode) != Some(file.0) {
                    std::fs::set_permissions(
                        home.join(path),
                        std::fs::Permissions::from_mode(file.0),
                    )
                    .unwrap();
                }
            }
        }
    }
}

/// A regular file's metadata as a lab last read or wrote it: its inode, size and mode, and its
/// modification and change times to the nanosecond. A write into the file moves both times and a
/// rename over it changes its inode, except within one tick of the filesystem's clock: a rewrite
/// in place at the same length, or a new file given a freed inode, keeps the stamp of the file it
/// replaced. A file whose stamp is as it was and was stamped before the tick its stamp was taken
/// in ([`Known::taken`]) holds the bytes it held then, as git's index trusts only a file it did
/// not stamp within its own tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    ino: u64,
    size: u64,
    mode: u32,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        Self {
            ino: meta.ino(),
            size: meta.size(),
            mode: meta.mode(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        }
    }

    /// When the file last changed: the later of its two times.
    fn changed(&self) -> (i64, i64) {
        self.mtime.max(self.ctime)
    }
}

/// A file beside a home whose write tells the time by the clock of the home's filesystem, which
/// stamps the home's files: a write's times come from that clock at that filesystem's
/// granularity, which the system's own clock does not show.
#[derive(Debug)]
struct Clock {
    /// A private folder in the home's parent, removed with the clock.
    _dir: aterm_tempfile::TempDir,
    file: PathBuf,
    /// Whether that folder is on the home's filesystem (a home that is a mount point's is not).
    on_home: bool,
}

impl Clock {
    fn beside(home: &Path) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        let dir = aterm_tempfile::Builder::new()
            .prefix(".home-clock")
            .tempdir_in(home.parent().expect("a home has a parent"))
            .unwrap();
        let on_home =
            std::fs::metadata(dir.path()).unwrap().dev() == std::fs::metadata(home).unwrap().dev();
        let file = dir.path().join("tick");
        Self {
            _dir: dir,
            file,
            on_home,
        }
    }

    /// The time a write takes now on the home's filesystem: no file written there later changed
    /// before it. `None` where the clock's folder is on another filesystem.
    fn now(&self) -> Option<(i64, i64)> {
        self.on_home.then(|| {
            std::fs::write(&self.file, [0]).unwrap();
            Stamp::of(&std::fs::metadata(&self.file).unwrap()).changed()
        })
    }
}

/// A home as a lab last read or wrote it: its content, and each file's [`Stamp`] then. A lab
/// keeps one of each home it reads, as the clock it reads is that home's.
#[derive(Debug, Default)]
pub(crate) struct Known {
    pub(crate) content: Content,
    stamps: BTreeMap<PathBuf, Stamp>,
    /// The home's filesystem's time ([`Clock::now`]) once the stamps were taken: a file changed
    /// at that time or later may have been written again in that same tick, which its stamp
    /// would not show. `None` where no clock was read, and then no stamp is trusted.
    taken: Option<(i64, i64)>,
    /// The home's clock, made at the first read or write of it.
    clock: Option<Clock>,
}

/// What a read of a home found: its content and each file's [`Stamp`].
#[derive(Default)]
struct Seen {
    content: Content,
    stamps: BTreeMap<PathBuf, Stamp>,
}

impl Known {
    /// What `home` holds now, every directory and file and its metadata read from the disk: a
    /// file's bytes too, unless its [`Stamp`] is the one `self` has of it, taken in a later tick
    /// than the file last changed in, beside the bytes it held then.
    fn now(&self, home: &Path) -> Seen {
        let mut now = Seen::default();
        self.read(home, Path::new(""), &mut now);
        now
    }

    fn read(&self, dir: &Path, at: &Path, now: &mut Seen) {
        use std::os::unix::fs::PermissionsExt as _;
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = at.join(entry.file_name());
            let kind = entry.file_type().unwrap();
            let meta = entry.metadata().unwrap();
            let mode = meta.permissions().mode() & 0o7777;
            if kind.is_dir() {
                now.content.dirs.insert(path.clone(), mode);
                self.read(&entry.path(), &path, now);
            } else if kind.is_file() {
                let stamp = Stamp::of(&meta);
                let settled = self.taken.is_some_and(|taken| stamp.changed() < taken);
                let bytes = match self.content.files.get(&path) {
                    Some((_, bytes)) if settled && self.stamps.get(&path) == Some(&stamp) => {
                        bytes.clone()
                    }
                    _ => std::fs::read(entry.path()).unwrap(),
                };
                now.content.files.insert(path.clone(), (mode, bytes));
                now.stamps.insert(path, stamp);
            } else {
                panic!(
                    "{}: a home holds only directories and regular files",
                    path.display()
                );
            }
        }
    }

    /// Know `content` and `stamps` of `home`, taken now by its filesystem's clock.
    fn take(&mut self, home: &Path, content: Content, stamps: BTreeMap<PathBuf, Stamp>) {
        self.content = content;
        self.stamps = stamps;
        self.taken = self.clock.get_or_insert_with(|| Clock::beside(home)).now();
    }

    /// Read `home` ([`Known::now`]): what it holds now, which `self` then knows.
    pub(crate) fn refresh(&mut self, home: &Path) -> &Content {
        let Seen { content, stamps } = self.now(home);
        self.take(home, content, stamps);
        &self.content
    }

    /// Make `home` hold `to`, writing what differs from what it holds now ([`Known::now`],
    /// [`Content::turn_into`]); `self` then knows it holds `to`, each file written by its metadata
    /// after the write.
    pub(crate) fn put(&mut self, to: &Content, home: &Path) {
        let now = self.now(home);
        now.content.turn_into(to, home);
        let stamps = to
            .files
            .iter()
            .map(|(path, file)| {
                let stamp = match now.stamps.get(path) {
                    Some(&stamp) if now.content.files.get(path) == Some(file) => stamp,
                    _ => Stamp::of(&std::fs::symlink_metadata(home.join(path)).unwrap()),
                };
                (path.clone(), stamp)
            })
            .collect();
        self.take(home, to.clone(), stamps);
    }
}

/// A file a home's clock stamped in the tick a lab took its stamps in is read again, though its
/// stamp is unchanged; one stamped in an earlier tick is not. The lab here knows other bytes than
/// the file holds, as it would after a rewrite in place within one tick of a coarse clock, and its
/// `taken` stands where such a clock would have put it, or a second later. A file written after
/// the clock was read changed no earlier than its reading, which is what lets a stamp taken
/// before it stand for the file's bytes.
/// NEGATIVE CONTROL: with every matching stamp trusted (`settled` always true), the read in the
/// clock's tick answers the bytes the lab knew: `` a file stamped in the tick its stamp was taken
/// in is read again ``.
#[test]
fn a_file_stamped_in_the_tick_its_stamp_was_taken_in_is_read_again() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    crate::fsx::ensure_private_dir(&home).unwrap();
    let clock = Clock::beside(&home);
    let read = clock
        .now()
        .expect("a home in the temporary folder has its clock beside it");
    std::fs::write(home.join("records"), b"written").unwrap();
    let stamp = Stamp::of(&std::fs::metadata(home.join("records")).unwrap());
    assert!(
        stamp.changed() >= read,
        "a file written after the clock was read changed no earlier: {stamp:?} {read:?}"
    );
    let mut known = Known::default();
    known.refresh(&home);
    assert_eq!(known.stamps[Path::new("records")], stamp);
    known
        .content
        .files
        .insert(PathBuf::from("records"), (0o600, b"stale!!".to_vec()));
    let knowing = |taken| Known {
        content: known.content.clone(),
        stamps: known.stamps.clone(),
        taken: Some(taken),
        clock: None,
    };
    assert_eq!(
        knowing(stamp.changed()).now(&home).content.files[Path::new("records")].1,
        b"written",
        "a file stamped in the tick its stamp was taken in is read again"
    );
    let (seconds, nanos) = stamp.changed();
    assert_eq!(
        knowing((seconds + 1, nanos)).now(&home).content.files[Path::new("records")].1,
        b"stale!!",
        "a file stamped before that tick is not read again"
    );
}
