// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! VOLUMES — the machine's mount table, a fresh reading of one volume, and the
//! pure lookups between a path and its mount (design
//! `docs/DESIGN-disk-awareness-2026-10-17.md` §2.1, under the module name the
//! system-awareness design §10.1 fixed).
//!
//! Platform reads only: no timer, no thread, no policy. Where a volume STANDS
//! (its lines, its level, its room) is `aterm_agent::harness::disk`'s.
//!
//! * [`table`] lists every mount without asking any file system
//!   (`getfsstat(MNT_NOWAIT)` on macOS, one bounded read of
//!   `/proc/self/mountinfo` on Linux, the mount manager and the object
//!   manager on Windows). A table cut at [`MAX_MOUNTS`] (or at
//!   [`MOUNTINFO_CAP`] bytes) says so ([`Table::complete`]): a fence that
//!   needs every mount reads a cut table as proving nothing.
//! * [`mount_of`] and [`mounts_under`] are PURE: they touch no path. On macOS
//!   a firmlinked root (`/Users`, `/private`, …, from `/usr/share/firmlinks`)
//!   is read in its data-volume spelling first. On Windows both sides are
//!   compared by [`crate::paths::windows_key`] (verbatim or plain, any case),
//!   a `\\?\Volume{…}\` path read through its volume's mount first, a `subst`
//!   letter followed to where it points.
//! * [`Table::is_under`] and [`Table::is_mount_point`] are the same rule for
//!   two paths, and for a path and the mount points: on Windows every
//!   comparison of two paths from two sources goes through them (a `subst`
//!   letter, a volume's second mount point, a share that may be the local
//!   disk again: what the table-less [`crate::paths::is_under`] cannot
//!   relate).
//! * [`read`] asks the file system only through a mount point the table calls
//!   local and not automounted (one `statfs`/`statvfs`; on Windows one
//!   `GetDiskFreeSpaceExW` and one `GetVolumeInformationW` on the volume's
//!   GUID root), so a hung network mount cannot block a reader. macOS's
//!   `getfsstat` keeps a free figure of its own, which can be stale; it is
//!   never used as one.
//! * [`file_id`] is one file's identity (`st_dev`/`st_ino`; on Windows the
//!   `FILE_ID_INFO` serial and id): the one device and inode every fence on
//!   the disk path compares.
//!
//! APFS: every volume of one container reports the container's free space, so
//! a set of readings dedupes by [`VolumeKey::Container`]; `free` is what a
//! write can use now (`f_bavail`), purgeable space not counted.
//!
//! Windows: a volume is keyed by its GUID ([`VolumeKey::Volume`]), which the
//! mount manager keeps across boots and which two volumes never share (a
//! cloned disk keeps its serial). `free` is free to this caller, its quota
//! honoured; storage the system reserves is not counted; a shadow copy keeps
//! a freed file's blocks as an APFS snapshot does, so a removal can free less
//! than it counted.

use std::path::{Component, Path, PathBuf};

use crate::paths;

/// The most mounts one [`table`] lists; a longer table is cut there and
/// marked incomplete ([`Table::complete`]).
pub const MAX_MOUNTS: usize = 256;

/// The most bytes of `/proc/self/mountinfo` one [`table`] reads (Linux): the
/// crate's 4 KiB [`crate::READ_CAP`] is too small for a mount table. A table
/// cut here is incomplete.
pub const MOUNTINFO_CAP: usize = 64 * 1024;

/// macOS's data volume, where every firmlinked root lives.
pub const DATA_VOLUME: &str = "/System/Volumes/Data";

/// The file systems the Linux arm calls LOCAL (an allowlist: a network or FUSE
/// type is [`Unread::Remote`] and never `statvfs`ed).
pub const LOCAL_FSTYPES: &[&str] = &[
    "ext2", "ext3", "ext4", "xfs", "btrfs", "zfs", "f2fs", "tmpfs", "overlay", "vfat", "exfat",
    "ntfs3",
];

/// One mount, as the table lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    /// Where it is mounted.
    pub on: PathBuf,
    /// What is mounted (`/dev/disk3s5`, `map auto_home`, a server's share).
    pub from: String,
    /// Its file system type (`apfs`, `autofs`, `ext4`).
    pub fstype: String,
    /// On a local device (macOS `MNT_LOCAL`; Linux [`LOCAL_FSTYPES`];
    /// Windows a `\Device\HarddiskVolume<n>`). Local is not FIXED: on Windows
    /// a USB stick's volume is a `HarddiskVolume` too (`DRIVE_REMOVABLE` to
    /// `GetDriveTypeW`), and so is an attached VHD(X) wherever its backing
    /// file lives, a share included. A removal that must stay on fixed disks
    /// asks that itself: nothing in this table does.
    pub local: bool,
    /// Mounted by the automounter (macOS `MNT_AUTOMOUNTED`, or `autofs`).
    pub automounted: bool,
    /// Mounted read-only.
    pub read_only: bool,
    /// What every mount sharing one free figure names alike: the APFS
    /// container (`disk3` of `/dev/disk3s5`), or on Windows the volume's
    /// GUID name (`Volume{6140fc3b-…}`: `\\?\` before it and `\` after it is
    /// its root) — for each mount point of the volume and for a `subst`
    /// letter onto it. `None` elsewhere ([`Mount::key`]).
    pub container: Option<String>,
}

impl Mount {
    /// The key of the free space this mount reads, where the table names
    /// it: a Windows volume GUID name (`Volume{…}`) is
    /// [`VolumeKey::Volume`], any other container [`VolumeKey::Container`].
    /// `None` without a container: a reading keys by [`VolumeKey::Fsid`].
    #[must_use]
    pub fn key(&self) -> Option<VolumeKey> {
        let c = self.container.as_deref()?;
        Some(match volume_guid(c) {
            Some(guid) => VolumeKey::Volume(guid.to_ascii_lowercase()),
            None => VolumeKey::Container(c.to_owned()),
        })
    }
}

/// The GUID (`{6140fc3b-…}`) of a Windows volume name (`Volume{6140fc3b-…}`),
/// else `None`.
fn volume_guid(name: &str) -> Option<&str> {
    let guid = name
        .get(6..)
        .filter(|_| name[..6].eq_ignore_ascii_case("Volume"))?;
    (guid.len() == 38 && guid.starts_with('{') && guid.ends_with('}')).then_some(guid)
}

/// One file's identity: the device it lives on and its number there
/// ([`file_id`]). Unix: `st_dev` and `st_ino`. Windows: `FILE_ID_INFO`'s
/// 64-bit volume serial and 128-bit file id (ReFS and Dev Drive ids do not
/// fit 64 bits; an NTFS id's low 64 bits are its file reference number) —
/// never the 32-bit serial `GetFileInformationByHandle` answers, so no two
/// widths ever meet in one comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId {
    /// The device: `st_dev`; the 64-bit volume serial on Windows.
    pub dev: u64,
    /// The file on it: `st_ino`; the 128-bit file id on Windows.
    pub ino: u128,
}

/// The identity of the file at `path` — of a link itself unless `follow`
/// — read now: `stat`/`lstat` on unix; on Windows one open for
/// `FILE_READ_ATTRIBUTES` (every sharing mode granted, a directory too,
/// `FILE_FLAG_OPEN_REPARSE_POINT` unless `follow`) and
/// `GetFileInformationByHandleEx(FileIdInfo)`, any spelling (verbatim,
/// plain, any case, past `MAX_PATH`). `None` where it cannot be read: the
/// path is missing or refused, its volume keeps no file ids (FAT and exFAT:
/// never a narrower stand-in), or the platform has no reader. A caller
/// reads `None` as UNKNOWN: in a removal fence it refuses, never "any
/// device".
///
/// Every device and identity a Windows disk fence compares comes from here
/// (or from `file_id_of_handle`, Windows only, on a handle already open), so
/// two of them always mean the same width.
#[must_use]
pub fn file_id(path: &Path, follow: bool) -> Option<FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let m = if follow {
            std::fs::metadata(path)
        } else {
            std::fs::symlink_metadata(path)
        }
        .ok()?;
        Some(FileId {
            dev: m.dev(),
            ino: u128::from(m.ino()),
        })
    }
    #[cfg(windows)]
    {
        crate::win_paths::file_id(path, follow).ok()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, follow);
        None
    }
}

/// The identity of the file `handle` has open ([`file_id`]):
/// `GetFileInformationByHandleEx(FileIdInfo)`. `None` where its volume keeps
/// no file ids (FAT, exFAT) or the query fails.
#[cfg(windows)]
#[must_use]
pub fn file_id_of_handle(handle: std::os::windows::io::BorrowedHandle<'_>) -> Option<FileId> {
    crate::win_paths::file_id_of_handle(handle).ok()
}

/// The mount table ([`table`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    /// Every mount read, at most [`MAX_MOUNTS`].
    pub mounts: Vec<Mount>,
    /// Every mount was read: nothing was cut and the read did not fail. A
    /// fence that must see every mount point reads `false` as "proves
    /// nothing".
    pub complete: bool,
    /// macOS's firmlinks: each root (`/Users`) and its data-volume spelling
    /// (`/System/Volumes/Data/Users`). Empty elsewhere.
    pub firmlinks: Vec<(PathBuf, PathBuf)>,
}

/// One volume's figures, read now ([`read`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The mount point read.
    pub mount: PathBuf,
    /// Which volume's free space this is ([`VolumeKey`]).
    pub key: VolumeKey,
    /// Its size in bytes.
    pub total: u64,
    /// Bytes free to an unprivileged writer now (`f_bavail`).
    pub free: u64,
    /// Inodes free, where the file system counts them (never on Windows:
    /// NTFS and ReFS have no fixed budget).
    pub files_free: Option<u64>,
    /// The mount point's device (`st_dev`; on Windows the [`file_id`]
    /// serial of the volume's root): the removal fence's.
    pub dev: u64,
    /// Mounted read-only.
    pub read_only: bool,
}

/// Which free space a reading is of: an APFS container's (every volume in it
/// shares one), a Windows volume's, else one file system's.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VolumeKey {
    /// An APFS container (`disk3`).
    Container(String),
    /// A file system's id (Linux `f_fsid`; macOS the mount point's `st_dev`).
    Fsid(u64),
    /// A Windows volume, by the GUID the mount manager names it with
    /// (`{6140fc3b-044f-43d7-b060-cb98170c7a63}` of
    /// `\\?\Volume{6140fc3b-…}\`), lowercase: kept across boots and letter
    /// changes, and unique where two volumes share a serial.
    Volume(String),
}

impl VolumeKey {
    /// A short spelling for a line or a file's key (`container:disk3`,
    /// `fsid:16777234`, `volume:{6140fc3b-044f-43d7-b060-cb98170c7a63}`).
    #[must_use]
    pub fn spelled(&self) -> String {
        match self {
            VolumeKey::Container(c) => format!("container:{c}"),
            VolumeKey::Fsid(f) => format!("fsid:{f}"),
            VolumeKey::Volume(g) => format!("volume:{g}"),
        }
    }
}

/// Why a volume was not read ([`read`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unread {
    /// A network or FUSE mount: never asked, a stalled one answers by blocking.
    Remote,
    /// Mounted by the automounter: asking it may mount it, or block.
    Automounted,
    /// No mount of the table holds the path.
    NoMount,
    /// The system refused the read (an errno; on Windows the Win32 error,
    /// `ERROR_NOT_READY` for a drive with no medium).
    Refused(i32),
    /// This platform has no reader.
    Unsupported,
}

impl Unread {
    /// The wire word (`why=` of a reply).
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Unread::Remote => "remote",
            Unread::Automounted => "automounted",
            Unread::NoMount => "no-mount",
            Unread::Refused(_) => "unreadable",
            Unread::Unsupported => "unsupported",
        }
    }
}

impl Table {
    /// The data-volume spelling of `path` when it lies under a firmlinked
    /// root (`/Users//x` → `/System/Volumes/Data/Users//x`), else `None`.
    /// Lexical.
    #[must_use]
    pub fn on_data(&self, path: &Path) -> Option<PathBuf> {
        self.firmlinks
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .and_then(|(root, data)| path.strip_prefix(root).ok().map(|rest| data.join(rest)))
    }

    /// `path` as the file system places it (unix): its data-volume spelling
    /// where a firmlink names it ([`Self::on_data`]), else itself. A mount
    /// point is listed as it was mounted (`/Volumes/Recovery` lies under the
    /// `/Volumes` firmlink), so both sides of a comparison are read this
    /// way. Windows places by [`windows_placed`].
    fn placed(&self, path: &Path) -> PathBuf {
        self.on_data(path).unwrap_or_else(|| path.to_path_buf())
    }

    /// Whether `path` is `dir` or lies under it, as this table places both:
    /// on Windows THE rule for comparing two paths from two sources (a
    /// process's working directory or image against a build directory, a
    /// session's directory against a mount), where a false "not under" reads
    /// as "nobody uses it":
    ///
    /// * any spelling ([`paths::windows_key`]): verbatim or plain, any case;
    /// * a `subst` letter followed to the directory it names (`S:\x` of
    ///   `subst S: C:\work` lies under `C:\work`), a `\\?\Volume{…}\` path
    ///   read through its volume's mount;
    /// * through every mount point of a volume: `D:\x` lies under `C:\mnt\d`
    ///   where `D:`'s volume is mounted at that folder too, and so does
    ///   `C:\mnt\d\x` under `D:\`.
    ///
    /// `None` where it cannot be placed, which a caller reads as IN USE /
    /// unknown: either side refused by [`paths::windows_key`] (an 8.3 short
    /// name among them); a drive letter the table does not list (a `subst`
    /// made since it was read); a path on a share or a network letter that
    /// does not lie lexically under the other — a share can be the local
    /// disk again (`\\localhost\C$\work`, a `net use` onto one), which only
    /// its server knows; a table that is not [`Self::complete`], where a
    /// mount point it missed may relate the two. Still lexical: a junction
    /// or a symbolic link (canonicalize a recorded path first, or compare
    /// [`file_id`]s).
    ///
    /// Elsewhere `Path::starts_with`, as [`paths::is_under`]: always placed.
    #[must_use]
    pub fn is_under(&self, path: &Path, dir: &Path) -> Option<bool> {
        under_in(path, dir, self, cfg!(windows))
    }

    /// `path` in a spelling [`paths::windows_key`] places: THE one rule for a
    /// path a process or a shell reported with an 8.3 short name in it
    /// (`C:\Users\ANDREW~1\x`: a process started in that spelling keeps it as
    /// its working directory and image, a short `%TEMP%` hands it to every
    /// child), which the key refuses and so places nowhere. Where the key
    /// refuses `path` only for such names ([`paths::short_spelled`]) and
    /// this table places the part before them on a mount it calls local and
    /// not automounted, the file system is asked for the long spelling of
    /// the part through the LAST short name ([`long_name`]:
    /// `GetLongPathNameW`, each short name replaced by the long name its own
    /// directory lists for it, with the system's error boxes off), and the
    /// names after it are appended as they are — they need not exist, so a
    /// directory a tab names before it is made, or after it is removed,
    /// places too (`\\?\C:\Users\Andrew Yates\x\not-yet`).
    ///
    /// LEXICAL, as the key is: a link among the short names keeps its own
    /// long name, never its target's, so a short spelling places exactly
    /// where its long spelling would — a working directory inside a junction
    /// under a candidate is under the candidate either way — and whatever
    /// reads a long spelling by identity besides (a process's handle to its
    /// directory, the file mapped at a module's base, a `canonicalize` before
    /// a volume read) reads the short one so too.
    ///
    /// Otherwise `path` as it is, which the key still refuses, so its caller
    /// holds it as unknown: the part before its short names on a share or an
    /// automounted volume (never asked: a stalled one answers by blocking) or
    /// placed nowhere, a part through the short names that is gone or
    /// refuses the listing, a long name the key refuses too. A path the key
    /// places comes back as it is, untouched. Elsewhere always `path`: no
    /// other platform has short names.
    ///
    /// Only the part before the first short name is placed before the file
    /// system is asked: a link between it and the last short name can lead
    /// to a share, where the call waits as long as the server does. A caller
    /// that must not wait on one asks through [`Self::long_spelling_by`]
    /// under a deadline of its own (the path-user scan's naming helper).
    #[must_use]
    pub fn long_spelling<'p>(&self, path: &'p Path) -> std::borrow::Cow<'p, Path> {
        long_spelling_in(path, self, cfg!(windows), long_name)
    }

    /// [`Self::long_spelling`], with `expand` asked in place of [`long_name`]
    /// for the long spelling of the part through the last short name (an
    /// existing verbatim path, [`paths::ShortSpelled::through`]) — the same
    /// rule, the file system's call run where the caller says: the path-user
    /// scan runs [`long_name`] on its naming helper under its deadline, so a
    /// share that stalls holds one process, never the scan. `None` from
    /// `expand` leaves `path` as it is: unplaced, so held.
    #[must_use]
    pub fn long_spelling_by<'p>(
        &self,
        path: &'p Path,
        expand: impl FnOnce(&Path) -> Option<PathBuf>,
    ) -> std::borrow::Cow<'p, Path> {
        long_spelling_in(path, self, cfg!(windows), expand)
    }

    /// Whether `path` is one of this table's mount points: where a walk
    /// stops. On Windows in any spelling, as [`Self::is_under`] places it —
    /// a folder mount point verbatim or plain, any case, through a `subst`
    /// letter or through another mount point of the volume it is on; a
    /// `subst` letter is no mount point (it names a directory); `None` where
    /// it cannot be placed, or is not found in a table that is not
    /// [`Self::complete`]. Elsewhere one of [`Self::points`], spelled alike.
    #[must_use]
    pub fn is_mount_point(&self, path: &Path) -> Option<bool> {
        point_in(path, self, cfg!(windows))
    }

    /// Every mount point as a walk meets it: each one's own spelling and, for
    /// one on macOS's data volume, its firmlinked form (`/System/Volumes/Data/
    /// home` and `/home`), sorted, each once. What a walk never enters or
    /// `stat`s (`aterm_agent::harness::disk::discover`'s `Roots::mounts`). On
    /// Windows each as the mount manager spells it (`C:\`, `C:\mnt\data`),
    /// with the network's [`NETWORK`] and every `subst` letter: a path a walk
    /// meets is spelled otherwise (`\\?\C:\…`, another case), so it is
    /// compared by [`Self::is_mount_point`], never by `Path` equality.
    #[must_use]
    pub fn points(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self.mounts.iter().map(|m| m.on.clone()).collect();
        let firmlinked: Vec<PathBuf> = out
            .iter()
            .filter_map(|p| p.strip_prefix(DATA_VOLUME).ok())
            .filter(|rel| !rel.as_os_str().is_empty())
            .map(|rel| Path::new("/").join(rel))
            .collect();
        out.extend(firmlinked);
        out.sort();
        out.dedup();
        out
    }
}

/// The mount holding `path` in `t`: the longest mount point that is a prefix
/// of it, component by component, each side read in its data-volume spelling
/// where a firmlink names it ([`Table::on_data`]) — on Windows each side by
/// its [`paths::windows_key`], so `\\?\C:\Users\x` and `c:\USERS\x\` place
/// on `C:\` alike, a `\\?\Volume{…}\` path on that volume's mount and a path
/// on a `subst` letter where the letter points. PURE: no path is touched, so
/// a stalled mount cannot block it. `None` for a relative path, one that
/// walks up through `..`, a spelling Windows cannot place
/// ([`paths::windows_key`]), a table holding no mount of it, or a Windows
/// table holding a mount point it cannot key (the path may lie on that
/// point's volume, and never on the one above it).
#[must_use]
pub fn mount_of<'a>(path: &Path, t: &'a Table) -> Option<&'a Mount> {
    mount_in(path, t, cfg!(windows))
}

/// Every mount of `t` AT or UNDER `dir` (lexical, both spellings of a
/// firmlinked path; on Windows any spelling, through every mount point of a
/// volume, as [`Table::is_under`] places it). PURE. A whole-directory
/// removal reads this as "a volume is mounted inside": a Linux bind mount
/// shares its source's `st_dev`, and a Windows folder mount point is a
/// reparse point a walk must never cross, so a device fence alone cannot
/// see either. A relative `dir` (or one with `..`, or a Windows spelling
/// that cannot be placed) answers every mount, and on Windows every folder
/// mount point not proven outside it is listed (one that cannot be keyed,
/// or any in a table that is not complete): nothing is ruled out on a path
/// nobody can place.
pub fn mounts_under<'a>(dir: &Path, t: &'a Table) -> impl Iterator<Item = &'a Mount> + 'a {
    mounts_in(dir, t, cfg!(windows))
}

/// [`mount_of`] by the rule of Windows (`windows`) or of unix, so the pure
/// tests hold the Windows rule on every platform. Windows: the mount
/// [`holder`] of the path's [`windows_placed`] key — never a `subst` letter
/// (a path on it is placed where the letter points), and none at all while
/// a mount point cannot be keyed (the path may lie on that point's volume).
pub(crate) fn mount_in<'a>(path: &Path, t: &'a Table, windows: bool) -> Option<&'a Mount> {
    if windows {
        let key = windows_placed(path, &t.mounts)?;
        return holder(&key, &t.mounts, false).map(|(m, _)| m);
    }
    if !plain_absolute(path) {
        return None;
    }
    let path = t.placed(path);
    t.mounts
        .iter()
        .map(|m| (m, t.placed(&m.on)))
        .filter(|(_, on)| path.starts_with(on))
        .max_by_key(|(_, on)| on.components().count())
        .map(|(m, _)| m)
}

/// [`mounts_under`] by the rule of Windows (`windows`) or of unix. Windows:
/// a `subst` letter is no volume mounted anywhere, never listed; a root (a
/// drive letter, [`NETWORK`]) is inside nothing but itself; a folder mount
/// point is listed unless [`under_keys`] says it is not inside `dir` (so an
/// unkeyed point, or an incomplete table, lists it).
pub(crate) fn mounts_in<'a>(
    dir: &Path,
    t: &'a Table,
    windows: bool,
) -> Box<dyn Iterator<Item = &'a Mount> + 'a> {
    if windows {
        let dir = windows_placed(dir, &t.mounts);
        return Box::new(
            t.mounts
                .iter()
                .filter(|m| m.fstype != SUBST)
                .filter(move |m| {
                    let (Some(dir), Some(on)) = (&dir, point_key(&m.on)) else {
                        return true;
                    };
                    if on.ends_with('\\') {
                        paths::key_under(&on, dir)
                    } else {
                        under_keys(&on, dir, t) != Some(false)
                    }
                }),
        );
    }
    let plain = plain_absolute(dir);
    let dir = t.placed(dir);
    Box::new(
        t.mounts
            .iter()
            .filter(move |m| !plain || t.placed(&m.on).starts_with(&dir)),
    )
}

/// [`Table::long_spelling`] by the rule of Windows (`windows`) or of unix,
/// asking `expand` for the long spelling of the part through the last short
/// name, so the pure tests hold the rule on every platform.
pub(crate) fn long_spelling_in<'p>(
    path: &'p Path,
    t: &Table,
    windows: bool,
    expand: impl FnOnce(&Path) -> Option<PathBuf>,
) -> std::borrow::Cow<'p, Path> {
    use std::borrow::Cow;
    if !windows {
        return Cow::Borrowed(path);
    }
    let Some(short) = paths::short_spelled(path) else {
        return Cow::Borrowed(path);
    };
    if !mount_in(short.lead, t, true).is_some_and(|m| m.local && !m.automounted) {
        return Cow::Borrowed(path);
    }
    let Some(long) = expand(&short.through) else {
        return Cow::Borrowed(path);
    };
    let Some(long) = long.to_str() else {
        return Cow::Borrowed(path);
    };
    let mut long = long.trim_end_matches('\\').to_owned();
    for name in &short.rest {
        long.push('\\');
        long.push_str(name);
    }
    let long = PathBuf::from(long);
    if paths::windows_key(&long).is_some() {
        Cow::Owned(long)
    } else {
        Cow::Borrowed(path)
    }
}

/// The long spelling the file system gives the existing `path`, whose names
/// may be 8.3 short names ([`Table::long_spelling`]'s one call, public for a
/// caller that runs it under a deadline of its own through
/// [`Table::long_spelling_by`]). On Windows `GetLongPathNameW` with the
/// system's error boxes off: each name replaced by the long name its own
/// directory lists for it (`\\?\C:\Users\ANDREW~1` reads `\\?\C:\Users\Andrew
/// Yates`), the spelling otherwise kept — a verbatim prefix stays, and a link
/// is NOT followed (a junction answers its own long name). Every directory on
/// the way is listed, so a link among them to a share waits on its server.
/// `None` when the call fails (a name gone, a listing refused, an empty
/// drive), and always elsewhere: no other platform has short names.
#[must_use]
pub fn long_name(path: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        crate::win_paths::long_path(path).ok()
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

/// [`Table::is_under`] by the rule of Windows (`windows`) or of unix.
pub(crate) fn under_in(path: &Path, dir: &Path, t: &Table, windows: bool) -> Option<bool> {
    if !windows {
        return Some(path.starts_with(dir));
    }
    under_keys(
        &windows_placed(path, &t.mounts)?,
        &windows_placed(dir, &t.mounts)?,
        t,
    )
}

/// [`Table::is_under`] of two [`windows_placed`] keys: lexically under, or
/// a [`spellings`] of `path` under one of `dir`'s; `None` where those
/// cannot all be named, or `t` is not complete.
fn under_keys(path: &str, dir: &str, t: &Table) -> Option<bool> {
    if paths::key_under(path, dir) {
        return Some(true);
    }
    if !t.complete {
        return None;
    }
    let (path, dir) = (spellings(path, &t.mounts)?, spellings(dir, &t.mounts)?);
    Some(
        path.iter()
            .any(|p| dir.iter().any(|d| paths::key_under(p, d))),
    )
}

/// [`Table::is_mount_point`] by the rule of Windows (`windows`) or of unix.
pub(crate) fn point_in(path: &Path, t: &Table, windows: bool) -> Option<bool> {
    if !windows {
        return Some(t.points().iter().any(|p| p == path));
    }
    let key = windows_placed(path, &t.mounts)?;
    let points = t
        .mounts
        .iter()
        .filter(|m| m.fstype != SUBST)
        .map(|m| point_key(&m.on))
        .collect::<Option<Vec<String>>>()?;
    if points.contains(&key) {
        return Some(true);
    }
    if !t.complete {
        return None;
    }
    Some(
        spellings(&key, &t.mounts)?
            .iter()
            .any(|s| points.contains(s)),
    )
}

/// The [`paths::windows_key`] of a Windows mount point as the mount manager
/// lists it (`C:\`, `C:\mnt\data`), read VERBATIM: those are the names on
/// disk, a folder named `data.` included, which a plain spelling would
/// trim. [`NETWORK`] keys as itself. `None` where it cannot be keyed.
fn point_key(on: &Path) -> Option<String> {
    let s = on.to_str()?;
    if s == NETWORK {
        return Some(NETWORK.to_owned());
    }
    if s.starts_with(r"\\?\") || s.starts_with(r"\??\") {
        return paths::windows_key(on);
    }
    paths::windows_key(Path::new(&format!(r"\\?\{s}")))
}

/// The mount of the Windows `mounts` holding the placed key `key`: the
/// deepest mount point it is at or under (strictly under when `strict`),
/// never a `subst` letter, with the rest of `key` below that point
/// (`USERS\X` of `C:\USERS\X` on `C:\`). `None` where no mount holds it, or
/// where any mount point cannot be keyed ([`point_key`]): the key may lie
/// on that point's volume, and is then on no other.
fn holder<'a, 'k>(key: &'k str, mounts: &'a [Mount], strict: bool) -> Option<(&'a Mount, &'k str)> {
    let mut best: Option<(&Mount, usize)> = None;
    for m in mounts.iter().filter(|m| m.fstype != SUBST) {
        let on = point_key(&m.on)?;
        if paths::key_under(key, &on)
            && !(strict && on.len() == key.len())
            && best.is_none_or(|(_, len)| on.len() >= len)
        {
            best = Some((m, on.len()));
        }
    }
    let (m, len) = best?;
    Some((m, key[len..].trim_start_matches('\\')))
}

/// The most spellings [`spellings`] names one location by, and the most
/// folder mount points it follows up from one: more is a volume mounted
/// inside itself, and is not placed.
const SPELLINGS: usize = 64;

/// Every key the Windows namespace spells the location of the placed key
/// `key` by: each mount point of its volume (a drive letter, each folder
/// mount point) joined with its rest below the one holding it, and, for a
/// folder mount point, each spelling of that folder on the volume holding
/// it — `C:\MNT\D\X` and `D:\X` both, where `D:`'s volume is mounted at
/// `C:\mnt\d` too. `None` where they cannot all be named: no mount holds
/// it, or one with no volume GUID does (a share, a network letter, a letter
/// whose volume was not read: what else names it only its server knows), a
/// mount point cannot be keyed, or there are more than [`SPELLINGS`].
fn spellings(key: &str, mounts: &[Mount]) -> Option<Vec<String>> {
    let (m, rest) = holder(key, mounts, false)?;
    let mut out = Vec::new();
    spell(volume_of(m)?, rest, mounts, 0, &mut out)?;
    Some(out)
}

/// [`spellings`] of `rest` below the root of volume `name`, `depth` folder
/// mount points up from where it started.
fn spell(
    name: &str,
    rest: &str,
    mounts: &[Mount],
    depth: usize,
    out: &mut Vec<String>,
) -> Option<()> {
    if depth > SPELLINGS {
        return None;
    }
    let points = mounts.iter().filter(|m| {
        m.fstype != SUBST && volume_of(m).is_some_and(|v| v.eq_ignore_ascii_case(name))
    });
    for m in points {
        let on = point_key(&m.on)?;
        let spelled = joined(&on, rest);
        if !out.contains(&spelled) {
            if out.len() == SPELLINGS {
                return None;
            }
            out.push(spelled);
        }
        if !on.ends_with('\\') {
            // A folder mount point: a folder of the volume holding it, which
            // may itself be spelled more ways.
            let (above, at) = holder(&on, mounts, true)?;
            spell(volume_of(above)?, &joined(at, rest), mounts, depth + 1, out)?;
        }
    }
    Some(())
}

/// The volume GUID name (`Volume{…}`) whose mount point `m` is, else `None`.
fn volume_of(m: &Mount) -> Option<&str> {
    m.container.as_deref().filter(|c| volume_guid(c).is_some())
}

/// The key `base` with the relative key `rest` below it (`C:\` and `X` is
/// `C:\X`; `C:\A` and `X` is `C:\A\X`; either alone where the other is
/// empty).
fn joined(base: &str, rest: &str) -> String {
    match (base.is_empty() || base.ends_with('\\'), rest.is_empty()) {
        (_, true) => base.to_owned(),
        (true, false) => format!("{base}{rest}"),
        (false, false) => format!("{base}\\{rest}"),
    }
}

/// [`paths::windows_key`] of `path` as the Windows table places it:
///
/// * a volume GUID path (`\\?\Volume{G}\x`, which names no letter) is
///   spelled first through a mount point of volume `G` in `mounts`
///   (`\\?\C:\x`; never a `subst` letter, which names a directory of the
///   volume, not its root);
/// * a path on a `subst` letter is the path the letter names (`S:\x` of
///   `subst S: C:\work` is `C:\work\x`), letter after letter, at most
///   [`SUBST_HOPS`] of them;
/// * [`NETWORK`], the network's synthetic mount point, keys as itself, so
///   every UNC path lies under it.
///
/// `None` where it cannot be placed: [`paths::windows_key`]'s refusals, a
/// volume GUID path whose volume `mounts` mounts nowhere, a `subst` chain
/// longer than [`SUBST_HOPS`].
fn windows_placed(path: &Path, mounts: &[Mount]) -> Option<String> {
    let s = path.to_str()?;
    if s == NETWORK {
        return Some(NETWORK.to_owned());
    }
    let guid_path = s
        .strip_prefix(r"\\?\")
        .or_else(|| s.strip_prefix(r"\??\"))
        .and_then(|v| {
            let (name, rest) = v.split_once('\\').unwrap_or((v, ""));
            volume_guid(name).map(|_| (name, rest))
        });
    let mut key = match guid_path {
        Some((name, rest)) => {
            let on = mounts.iter().filter(|m| m.fstype != SUBST).find(|m| {
                m.container
                    .as_deref()
                    .is_some_and(|c| c.eq_ignore_ascii_case(name))
            })?;
            let on = on.on.to_str()?.trim_end_matches('\\');
            paths::windows_key(Path::new(&format!(r"\\?\{on}\{rest}")))?
        }
        None => paths::windows_key(path)?,
    };
    for _ in 0..SUBST_HOPS {
        let Some((on, target)) = mounts.iter().filter(|m| m.fstype == SUBST).find_map(|m| {
            let on = paths::windows_key(&m.on)?;
            paths::key_under(&key, &on).then_some((on, m.from.as_str()))
        }) else {
            return Some(key);
        };
        // The object manager's target (`\??\C:\work`) is literal: verbatim.
        let target = paths::windows_key(Path::new(target))?;
        let rest = key[on.len()..].trim_start_matches('\\');
        key = match (rest.is_empty(), target.ends_with('\\')) {
            (true, _) => target,
            (false, true) => format!("{target}{rest}"),
            (false, false) => format!("{target}\\{rest}"),
        };
    }
    None
}

/// The `fstype` of a Windows table's `subst` letter: its `from` is the
/// object manager's target (`\??\C:\work`).
pub const SUBST: &str = "subst";

/// The most `subst` letters one Windows placement follows ([`mount_of`],
/// [`mounts_under`]): a longer chain is not placed.
pub const SUBST_HOPS: usize = 8;

/// The mount point of the Windows table's synthetic network mount: every
/// UNC path (`\\srv\share\x`, `\\?\UNC\srv\share\x`) lies under it, so a
/// caller reads [`Unread::Remote`] for one, never asking the server.
pub const NETWORK: &str = r"\\";

/// Absolute, and no `.` or `..` component.
fn plain_absolute(p: &Path) -> bool {
    p.is_absolute()
        && !p
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
}

/// The APFS container a mount's source names: `disk3` of `/dev/disk3s5`, of
/// `/dev/disk3s1s1` (a sealed system snapshot) and of
/// `com.apple.os.update-…@/dev/disk3s1`. `None` for any other source.
#[must_use]
pub fn apfs_container(from: &str) -> Option<String> {
    let dev = from.rsplit('@').next()?;
    let name = dev.strip_prefix("/dev/").unwrap_or(dev);
    let rest = name.strip_prefix("disk")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with('s') {
        return None;
    }
    Some(format!("disk{digits}"))
}

/// `/usr/share/firmlinks`' lines (`/Users\tUsers`): each root and its
/// data-volume spelling. A line that is not two fields, or whose root is not
/// absolute, is left out.
#[must_use]
pub fn parse_firmlinks(text: &str) -> Vec<(PathBuf, PathBuf)> {
    text.lines()
        .filter_map(|l| {
            let (root, rel) = l.split_once('\t')?;
            let (root, rel) = (Path::new(root.trim()), rel.trim());
            (root.is_absolute() && plain_absolute(root) && !rel.is_empty() && !rel.starts_with('/'))
                .then(|| (root.to_path_buf(), Path::new(DATA_VOLUME).join(rel)))
        })
        .collect()
}

/// A mountinfo field with its octal escapes (`\040` for a space) decoded.
#[must_use]
pub fn unescape_mount(field: &str) -> String {
    let b = field.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let octal = b
            .get(i + 1..i + 4)
            .filter(|d| b[i] == b'\\' && d.iter().all(|c| (b'0'..=b'7').contains(c)))
            .map(|d| d.iter().fold(0u32, |a, c| a * 8 + u32::from(c - b'0')));
        match octal.and_then(|v| u8::try_from(v).ok()) {
            Some(v) => {
                out.push(v);
                i += 4;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `/proc/self/mountinfo`'s `text` as a [`Table`]. `cut`: the read stopped at
/// [`MOUNTINFO_CAP`]. A torn last line (no newline: the read was cut inside
/// it) is dropped; a cut text, a line that does not parse, or more than
/// [`MAX_MOUNTS`] mounts makes the table incomplete. PURE.
#[must_use]
pub fn parse_mountinfo(text: &str, cut: bool) -> Table {
    let whole = if text.ends_with('\n') {
        text
    } else {
        &text[..text.rfind('\n').map_or(0, |i| i + 1)]
    };
    let mut complete = !cut && whole.len() == text.len();
    let mut mounts = Vec::new();
    for line in whole.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // `<id> <parent> <maj:min> <root> <point> <opts> [optional…] - <type> <source> <super>`
        let fields: Vec<&str> = line.split(' ').collect();
        let Some(dash) = fields.iter().position(|f| *f == "-") else {
            complete = false;
            continue;
        };
        let (Some(point), Some(opts), Some(fstype)) =
            (fields.get(4), fields.get(5), fields.get(dash + 1))
        else {
            complete = false;
            continue;
        };
        let on = PathBuf::from(unescape_mount(point));
        if !on.is_absolute() {
            complete = false;
            continue;
        }
        if mounts.len() == MAX_MOUNTS {
            complete = false;
            break;
        }
        let fstype = unescape_mount(fstype);
        let automounted = fstype == "autofs";
        mounts.push(Mount {
            on,
            from: fields
                .get(dash + 2)
                .map_or_else(String::new, |s| unescape_mount(s)),
            local: !automounted && LOCAL_FSTYPES.contains(&fstype.as_str()),
            automounted,
            read_only: opts.split(',').any(|o| o == "ro"),
            fstype,
            container: None,
        });
    }
    Table {
        mounts,
        complete,
        firmlinks: Vec::new(),
    }
}

/// The strings of a `MULTI_SZ` buffer (each NUL-terminated, the list ended
/// by an empty one or by the buffer). PURE.
#[cfg(any(windows, test))]
pub(crate) fn multi_sz(buf: &[u16]) -> Vec<&[u16]> {
    buf.split(|&c| c == 0)
        .take_while(|s| !s.is_empty())
        .collect()
}

/// What a Windows DOS device name points at, as `QueryDosDeviceW` spells
/// its target ([`device_class`]).
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Device<'a> {
    /// `\Device\HarddiskVolume<n>`: a partition, an attached VHD(X), a
    /// Storage Spaces, BitLocker or Dev Drive volume. LOCAL.
    Volume,
    /// `\Device\CdRom<n>`, `\Device\Floppy<n>`: removable media, where a
    /// question may wait for a medium. Never read ([`Unread::Automounted`]).
    Removable,
    /// `\??\C:\work`: a `subst` letter onto the drive path it carries.
    Subst(&'a str),
    /// Anything else: a network redirector (`\Device\LanmanRedirector\…`,
    /// `\Device\Mup\…`, WebDAV's, RDP's `RdpDr`, WSL's `P9Rdr`), a user-mode
    /// file system, a `subst` onto a share, a device nobody has measured.
    /// Never asked ([`Unread::Remote`]): a stalled one answers by blocking.
    Remote,
}

/// The class of the object-manager target `target` (an allowlist: only a
/// numbered `\Device\HarddiskVolume` is local). PURE.
#[cfg(any(windows, test))]
pub(crate) fn device_class(target: &str) -> Device<'_> {
    let numbered = |prefix: &str| {
        target
            .get(..prefix.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
            && target.len() > prefix.len()
            && target.as_bytes()[prefix.len()..]
                .iter()
                .all(u8::is_ascii_digit)
    };
    if numbered(r"\Device\HarddiskVolume") {
        return Device::Volume;
    }
    if numbered(r"\Device\CdRom") || numbered(r"\Device\Floppy") {
        return Device::Removable;
    }
    match target.strip_prefix(r"\??\") {
        Some(dir) if matches!(dir.as_bytes(), [d, b':', b'\\', ..] if d.is_ascii_alphabetic()) => {
            Device::Subst(dir)
        }
        _ => Device::Remote,
    }
}

/// One volume as the Windows mount manager lists it ([`windows_table`]).
#[cfg(any(windows, test))]
#[derive(Clone, Debug, Default)]
pub(crate) struct VolumeSeen {
    /// Its GUID name, `Volume{6140fc3b-…}`.
    pub(crate) name: String,
    /// The NT device it is (`\Device\HarddiskVolume3`); `None` when that
    /// could not be read.
    pub(crate) device: Option<String>,
    /// Every path it is mounted at (`C:\`, `C:\mnt\data\`); `None` when
    /// they could not be read.
    pub(crate) paths: Option<Vec<PathBuf>>,
}

/// The Windows [`Table`] of what the mount manager and the object manager
/// said: `volumes`, each with its device and mount points; `letters`, every
/// drive letter this process sees with its target in this session (`None`:
/// unread); `complete`: the volume enumeration finished. PURE.
///
/// * Each mount point of a volume is a mount: `from` its device, `fstype`
///   empty (no file system is asked), `container` its GUID name, LOCAL for a
///   `\Device\HarddiskVolume<n>`, automounted for removable media
///   ([`device_class`]).
/// * A letter whose target in this session is not its volume's device (a
///   per-session `subst` or `net use` over it) is read as that target, and
///   so is every folder mount point spelled through it (`D:\mnt\x\` of
///   another volume lies on the share this session's `D:` is). A folder
///   mount point spelled through a letter no volume read here owns is kept,
///   and the table is incomplete.
/// * Every other letter is a mount of its own, from its target: a volume's
///   letter (LOCAL), removable media, a [`SUBST`] letter (local and
///   keyed as what it points at; never placed on: [`mount_of`] follows it),
///   or a remote one.
/// * The synthetic [`NETWORK`] mount holds every UNC path, remote.
/// * A volume or letter that could not be read, or more than
///   [`MAX_MOUNTS`] mounts, makes the table incomplete: a fence that needs
///   every mount (a folder mount point missed inside a build directory)
///   reads it as proving nothing.
#[cfg(any(windows, test))]
pub(crate) fn windows_table(
    volumes: &[VolumeSeen],
    letters: &[(char, Option<String>)],
    complete: bool,
) -> Table {
    let mut complete = complete;
    let mut mounts = vec![Mount {
        on: PathBuf::from(NETWORK),
        from: "UNC".to_owned(),
        fstype: String::new(),
        local: false,
        automounted: false,
        read_only: false,
        container: None,
    }];
    let in_session = |letter: char| {
        letters
            .iter()
            .find(|(l, _)| l.eq_ignore_ascii_case(&letter))
            .map(|(_, t)| t.as_deref())
    };
    // The device a drive letter names for every session: the volume's whose
    // mount points hold its root.
    let owner = |letter: char| {
        volumes
            .iter()
            .find(|w| {
                w.paths
                    .as_ref()
                    .is_some_and(|ps| ps.iter().any(|q| letter_root(q) == Some(letter)))
            })
            .and_then(|w| w.device.as_deref())
    };
    for v in volumes {
        let Some(points) = &v.paths else {
            complete = false;
            continue;
        };
        let class = v.device.as_deref().map(device_class);
        if class.is_none() && !points.is_empty() {
            complete = false;
        }
        for p in points {
            let root = letter_root(p).is_some();
            if let Some(letter) = drive_letter(p) {
                // A root is this volume's letter; a folder mount point is
                // spelled through the letter of the volume it is a folder of.
                let global = if root {
                    v.device.as_deref()
                } else {
                    owner(letter)
                };
                match in_session(letter) {
                    Some(Some(t)) if global.is_some_and(|d| d.eq_ignore_ascii_case(t)) => {}
                    // This session's letter names something else (a
                    // per-session `subst` or `net use` over it): the letter
                    // is read below as it points, and so is every folder
                    // spelled through it.
                    Some(Some(_)) if root || global.is_some() => continue,
                    // A folder spelled through a letter no volume read here
                    // owns, or a letter's target unread: kept, and the
                    // table proves nothing.
                    Some(_) => complete = false,
                    None => {}
                }
            }
            mounts.push(Mount {
                on: mount_point(p),
                from: v.device.clone().unwrap_or_else(|| v.name.clone()),
                fstype: String::new(),
                local: class == Some(Device::Volume),
                automounted: class == Some(Device::Removable),
                read_only: false,
                container: Some(v.name.clone()),
            });
        }
    }
    for (letter, target) in letters {
        let on = PathBuf::from(format!("{}:\\", letter.to_ascii_uppercase()));
        if mounts.iter().any(|m| m.on == on) {
            continue;
        }
        let Some(target) = target else {
            complete = false;
            continue;
        };
        let class = device_class(target);
        mounts.push(Mount {
            on,
            from: target.clone(),
            fstype: if matches!(class, Device::Subst(_)) {
                SUBST.to_owned()
            } else {
                String::new()
            },
            local: class == Device::Volume,
            automounted: class == Device::Removable,
            read_only: false,
            container: volumes
                .iter()
                .find(|v| {
                    class == Device::Volume
                        && v.device
                            .as_deref()
                            .is_some_and(|d| d.eq_ignore_ascii_case(target))
                })
                .map(|v| v.name.clone()),
        });
    }
    if mounts.len() > MAX_MOUNTS {
        mounts.truncate(MAX_MOUNTS);
        complete = false;
    }
    let mut t = Table {
        mounts,
        complete,
        firmlinks: Vec::new(),
    };
    // A `subst` letter reads as the mount it points into.
    for i in 0..t.mounts.len() {
        if t.mounts[i].fstype != SUBST {
            continue;
        }
        let into = mount_in(Path::new(&t.mounts[i].from), &t, true).cloned();
        let m = &mut t.mounts[i];
        (m.local, m.automounted, m.container) = into.map_or((false, false, None), |into| {
            (into.local, into.automounted, into.container)
        });
    }
    t
}

/// The drive letter of a drive's root (`C` of `C:\`), else `None`.
#[cfg(any(windows, test))]
fn letter_root(p: &Path) -> Option<char> {
    match p.to_str()?.as_bytes() {
        [l, b':', b'\\'] if l.is_ascii_alphabetic() => Some(char::from(l.to_ascii_uppercase())),
        _ => None,
    }
}

/// The drive letter a path is spelled through (`C` of `C:\` and of
/// `C:\mnt\data\`), else `None`.
#[cfg(any(windows, test))]
fn drive_letter(p: &Path) -> Option<char> {
    match p.to_str()?.as_bytes() {
        [l, b':', b'\\', ..] if l.is_ascii_alphabetic() => Some(char::from(l.to_ascii_uppercase())),
        _ => None,
    }
}

/// The drive letters of `GetLogicalDrives`' mask (bit 0 `A:`), or `None`
/// for a mask naming none: that call answers 0 only when it fails (a booted
/// Windows always has its system drive), and a table read without its
/// letters misses every per-session `subst` and `net use`. PURE.
#[cfg(any(windows, test))]
pub(crate) fn drive_letters(mask: u32) -> Option<Vec<char>> {
    let letters: Vec<char> = (b'A'..=b'Z')
        .zip(0..26u32)
        .filter(|(_, bit)| mask & (1 << bit) != 0)
        .map(|(l, _)| char::from(l))
        .collect();
    (!letters.is_empty()).then_some(letters)
}

/// A mount point as the mount manager spells it (`C:\mnt\data\`), without
/// its trailing separator unless it is a drive's root (`C:\`).
#[cfg(any(windows, test))]
fn mount_point(p: &Path) -> PathBuf {
    match p.to_str() {
        Some(s) if letter_root(p).is_none() && s.len() > 1 => {
            PathBuf::from(s.trim_end_matches('\\'))
        }
        _ => p.to_path_buf(),
    }
}

/// The machine's mount table now, read without asking any file system:
/// `getfsstat(MNT_NOWAIT)` and `/usr/share/firmlinks` on macOS, one bounded
/// read of `/proc/self/mountinfo` on Linux; on Windows the mount manager
/// (`FindFirstVolumeW`/`FindNextVolumeW`, `GetVolumePathNamesForVolumeNameW`
/// for every drive letter and folder mount point) and the object manager
/// (`GetLogicalDrives`, `QueryDosDeviceW` for each volume's device and each
/// letter's target: `subst` and network letters). There a volume's mount
/// points are LOCAL only for a `\Device\HarddiskVolume<n>`, removable media
/// are automounted, every other device is remote, a `subst` letter is
/// followed to where it points ([`SUBST`]), every UNC path lies under the
/// synthetic remote [`NETWORK`] mount, and anything unread leaves the table
/// incomplete. An empty INCOMPLETE table elsewhere (and where the read
/// fails).
#[must_use]
pub fn table() -> Table {
    #[cfg(target_os = "macos")]
    {
        arm::table()
    }
    #[cfg(target_os = "linux")]
    {
        match read_capped(Path::new("/proc/self/mountinfo"), MOUNTINFO_CAP) {
            Some((text, cut)) => parse_mountinfo(&text, cut),
            None => Table::default(),
        }
    }
    #[cfg(windows)]
    {
        arm::table()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        Table::default()
    }
}

/// One volume read now, through its mount point `m` — only one the table
/// calls local and not automounted ([`Unread`] otherwise): one `statfs`
/// (macOS) or `statvfs` (Linux) and one `stat` for its device. On Windows,
/// through the volume's GUID root (`\\?\Volume{…}\`: no letter, no `subst`,
/// no `MAX_PATH`) with the system's error boxes off: `GetDiskFreeSpaceExW`
/// (`free` is free to this caller), `GetVolumeInformationW` (read-only) and
/// the root's [`file_id`] for its device — a volume that keeps no file ids
/// (FAT, exFAT) is refused rather than given a device of another width.
///
/// # Errors
///
/// The mount is remote or automounted, the system refused the read, or this
/// platform has no reader.
pub fn read(m: &Mount) -> Result<Reading, Unread> {
    if m.automounted || m.fstype == "autofs" {
        return Err(Unread::Automounted);
    }
    if !m.local {
        return Err(Unread::Remote);
    }
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    {
        arm::read(m)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        Err(Unread::Unsupported)
    }
}

/// Is the volume `m` mounts a FIXED disk (`GetDriveTypeW` on its root, the
/// volume's GUID root where the table names one, else its own mount point,
/// verbatim)? Windows only: the table's [`Mount::local`] is a local DEVICE,
/// and a USB stick's or an attached VHD's volume is one too; a removal that
/// must stay on fixed disks asks this as well. `Some(false)` for a mount the
/// table calls remote (the synthetic [`NETWORK`] one included), for a
/// [`SUBST`] letter (an alias, never a volume of its own: [`mount_of`]
/// follows it to the volume, which is asked instead) and for every other
/// kind (removable, optical, RAM disk, remote); `None` when the kind could
/// not be named — which a removal reads as refused.
#[cfg(windows)]
#[must_use]
pub fn is_fixed(m: &Mount) -> Option<bool> {
    if !m.local || m.fstype == SUBST {
        return Some(false);
    }
    arm::drive_type(m).map(|kind| kind == crate::win_paths::DRIVE_FIXED)
}

/// [`read`] of the volume holding `path` in `t` ([`mount_of`]).
///
/// # Errors
///
/// As [`read`]; [`Unread::NoMount`] when no mount of `t` holds it, and
/// [`Unread::Unsupported`] where there is no reader.
pub fn read_path(path: &Path, t: &Table) -> Result<Reading, Unread> {
    if cfg!(not(any(target_os = "macos", target_os = "linux", windows))) {
        return Err(Unread::Unsupported);
    }
    read(mount_of(path, t).ok_or(Unread::NoMount)?)
}

/// At most `cap` bytes of `path`, and whether the read stopped at the cap.
#[cfg(target_os = "linux")]
fn read_capped(path: &Path, cap: usize) -> Option<(String, bool)> {
    use std::io::Read as _;
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(cap.min(16 * 1024));
    // One byte past the cap says whether there was more.
    let n = f
        .take(u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut buf)
        .ok()?;
    let cut = n > cap;
    buf.truncate(cap);
    Some((String::from_utf8_lossy(&buf).into_owned(), cut))
}

#[cfg(target_os = "macos")]
mod arm {
    use super::{Mount, Reading, Table, Unread, VolumeKey, apfs_container, parse_firmlinks};
    use std::path::{Path, PathBuf};

    // `<sys/mount.h>`: `int getfsstat(struct statfs *buf, int bufsize, int mode)`.
    // Declared here because the first-party `libc` does not carry it; Intel
    // spells the 64-bit-inode `statfs` it fills as `getfsstat$INODE64`, as
    // `libc::statfs` itself is `statfs$INODE64` there (`spawn_folder.rs`
    // declares it the same way).
    unsafe extern "C" {
        #[cfg_attr(target_arch = "x86_64", link_name = "getfsstat$INODE64")]
        fn getfsstat(
            buf: *mut libc::statfs,
            bufsize: libc::c_int,
            mode: libc::c_int,
        ) -> libc::c_int;
    }

    /// `<sys/mount.h>`.
    const MNT_NOWAIT: libc::c_int = 2;
    const MNT_RDONLY: u32 = 0x0000_0001;
    const MNT_LOCAL: u32 = 0x0000_1000;
    const MNT_AUTOMOUNTED: u32 = 0x0040_0000;

    /// A NUL-terminated `c_char` array as text.
    fn text(chars: &[libc::c_char]) -> String {
        let bytes: Vec<u8> = chars
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c.to_ne_bytes()[0])
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub(super) fn table() -> Table {
        let firmlinks = crate::read_bounded(Path::new("/usr/share/firmlinks"))
            .map(|t| parse_firmlinks(&t))
            .unwrap_or_default();
        // SAFETY: a null buffer asks only for the count; nothing is written.
        let Ok(count) = usize::try_from(unsafe { getfsstat(std::ptr::null_mut(), 0, MNT_NOWAIT) })
        else {
            return Table {
                firmlinks,
                ..Table::default()
            };
        };
        // SAFETY: `statfs` is integers and arrays of them: all-zero is a value.
        let blank: libc::statfs = unsafe { std::mem::zeroed() };
        // Room for a few mounted between the two calls, and one past the cap
        // to tell a table longer than it.
        let room = count.saturating_add(4).min(super::MAX_MOUNTS + 1);
        let mut buf = vec![blank; room];
        let Ok(bytes) = libc::c_int::try_from(std::mem::size_of_val(buf.as_slice())) else {
            return Table {
                firmlinks,
                ..Table::default()
            };
        };
        // SAFETY: `buf` is `bytes` writable bytes of whole `statfs` records,
        // and getfsstat writes at most that many, answering how many it wrote.
        // MNT_NOWAIT asks no file system, so a stalled mount cannot block it.
        let written = unsafe { getfsstat(buf.as_mut_ptr(), bytes, MNT_NOWAIT) };
        let Ok(written) = usize::try_from(written) else {
            return Table {
                firmlinks,
                ..Table::default()
            };
        };
        let mounts: Vec<Mount> = buf
            .iter()
            .take(written.min(super::MAX_MOUNTS))
            .map(|r| {
                let from = text(&r.f_mntfromname);
                Mount {
                    on: PathBuf::from(text(&r.f_mntonname)),
                    container: apfs_container(&from),
                    from,
                    fstype: text(&r.f_fstypename),
                    local: r.f_flags & MNT_LOCAL != 0,
                    automounted: r.f_flags & MNT_AUTOMOUNTED != 0,
                    read_only: r.f_flags & MNT_RDONLY != 0,
                }
            })
            .filter(|m| m.on.is_absolute())
            .collect();
        Table {
            complete: written <= super::MAX_MOUNTS && written < room && mounts.len() == written,
            mounts,
            firmlinks,
        }
    }

    pub(super) fn read(m: &Mount) -> Result<Reading, Unread> {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::MetadataExt as _;
        let c = std::ffi::CString::new(m.on.as_os_str().as_bytes())
            .map_err(|_| Unread::Refused(libc::EINVAL))?;
        // SAFETY: `statfs` is integers and arrays of them: all-zero is a value.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is NUL-terminated and `st` is a writable `statfs`.
        if unsafe { libc::statfs(c.as_ptr(), &raw mut st) } != 0 {
            return Err(Unread::Refused(
                std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
            ));
        }
        let dev = std::fs::metadata(&m.on)
            .map_err(|e| Unread::Refused(e.raw_os_error().unwrap_or(0)))?
            .dev();
        let bsize = u64::from(st.f_bsize);
        Ok(Reading {
            mount: m.on.clone(),
            key: m
                .container
                .clone()
                .map_or(VolumeKey::Fsid(dev), VolumeKey::Container),
            total: st.f_blocks.saturating_mul(bsize),
            free: st.f_bavail.saturating_mul(bsize),
            files_free: Some(st.f_ffree),
            dev,
            read_only: m.read_only || st.f_flags & MNT_RDONLY != 0,
        })
    }
}

#[cfg(target_os = "linux")]
mod arm {
    use super::{Mount, Reading, Unread, VolumeKey};

    #[cfg_attr(
        target_pointer_width = "64",
        expect(
            clippy::useless_conversion,
            reason = "statvfs's c_ulong and fsblkcnt_t fields are u64 on LP64 and u32 on 32-bit"
        )
    )]
    pub(super) fn read(m: &Mount) -> Result<Reading, Unread> {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::MetadataExt as _;
        let c = std::ffi::CString::new(m.on.as_os_str().as_bytes())
            .map_err(|_| Unread::Refused(libc::EINVAL))?;
        // SAFETY: `statvfs` is integers (its padding private): all-zero is a value.
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is NUL-terminated and `st` is a writable `statvfs`.
        if unsafe { libc::statvfs(c.as_ptr(), &raw mut st) } != 0 {
            return Err(Unread::Refused(
                std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
            ));
        }
        let dev = std::fs::metadata(&m.on)
            .map_err(|e| Unread::Refused(e.raw_os_error().unwrap_or(0)))?
            .dev();
        let frsize = u64::from(st.f_frsize);
        Ok(Reading {
            mount: m.on.clone(),
            key: VolumeKey::Fsid(u64::from(st.f_fsid)),
            total: u64::from(st.f_blocks).saturating_mul(frsize),
            free: u64::from(st.f_bavail).saturating_mul(frsize),
            files_free: Some(u64::from(st.f_favail)),
            dev,
            read_only: m.read_only,
        })
    }
}

#[cfg(windows)]
mod arm {
    use super::{
        Mount, Reading, Table, Unread, VolumeKey, VolumeSeen, drive_letters, volume_guid,
        windows_table,
    };
    use crate::win_paths;
    use std::path::PathBuf;

    pub(super) fn table() -> Table {
        let _quiet = win_paths::quiet();
        let (names, complete) = win_paths::volume_names();
        let volumes: Vec<VolumeSeen> = names
            .iter()
            .map(|full| {
                // `\\?\Volume{…}\`: the object manager knows it as `Volume{…}`.
                let name = full
                    .trim_start_matches(r"\\?\")
                    .trim_end_matches('\\')
                    .to_owned();
                VolumeSeen {
                    device: win_paths::dos_device(&name),
                    paths: win_paths::volume_path_names(full),
                    name,
                }
            })
            .collect();
        let drives = drive_letters(win_paths::logical_drives());
        let letters: Vec<(char, Option<String>)> = drives
            .iter()
            .flatten()
            .map(|&l| (l, win_paths::dos_device(&format!("{l}:"))))
            .collect();
        windows_table(&volumes, &letters, complete && drives.is_some())
    }

    /// The root every reading of `m` goes through: its volume's GUID root
    /// where the table names one, else its own mount point, verbatim.
    fn root(m: &Mount) -> PathBuf {
        match m.container.as_deref().filter(|c| volume_guid(c).is_some()) {
            Some(name) => PathBuf::from(format!(r"\\?\{name}\")),
            None => {
                let on = m.on.to_string_lossy();
                let on = on.trim_end_matches('\\');
                if on.starts_with(r"\\?\") {
                    PathBuf::from(format!(r"{on}\"))
                } else {
                    PathBuf::from(format!(r"\\?\{on}\"))
                }
            }
        }
    }

    /// `GetDriveTypeW` of `m`'s root ([`super::is_fixed`]).
    pub(super) fn drive_type(m: &Mount) -> Option<u32> {
        win_paths::drive_type(&root(m))
    }

    pub(super) fn read(m: &Mount) -> Result<Reading, Unread> {
        let _quiet = win_paths::quiet();
        let root = root(m);
        let (free, total) = win_paths::disk_free(&root).map_err(Unread::Refused)?;
        let (flags, _fstype) = win_paths::volume_info(&root).map_err(Unread::Refused)?;
        let dev = win_paths::file_id(&root, true)
            .map_err(Unread::Refused)?
            .dev;
        Ok(Reading {
            mount: m.on.clone(),
            key: m.key().unwrap_or(VolumeKey::Fsid(dev)),
            total,
            free,
            files_free: None,
            dev,
            read_only: m.read_only || flags & win_paths::FILE_READ_ONLY_VOLUME != 0,
        })
    }
}

#[cfg(test)]
#[path = "volumes_tests.rs"]
mod tests;
