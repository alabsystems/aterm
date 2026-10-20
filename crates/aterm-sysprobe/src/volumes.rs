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
//!   `/proc/self/mountinfo` on Linux). A table cut at [`MAX_MOUNTS`] (or at
//!   [`MOUNTINFO_CAP`] bytes) says so ([`Table::complete`]): a fence that
//!   needs every mount reads a cut table as proving nothing.
//! * [`mount_of`] and [`mounts_under`] are PURE: they touch no path. On macOS
//!   a firmlinked root (`/Users`, `/private`, …, from `/usr/share/firmlinks`)
//!   is read in its data-volume spelling first.
//! * [`read`] asks the file system only through a mount point the table calls
//!   local and not automounted (one `statfs`/`statvfs`), so a hung network
//!   mount cannot block a reader. macOS's `getfsstat` keeps a free figure of
//!   its own, which can be stale; it is never used as one.
//!
//! APFS: every volume of one container reports the container's free space, so
//! a set of readings dedupes by [`VolumeKey::Container`]; `free` is what a
//! write can use now (`f_bavail`), purgeable space not counted.

use std::path::{Component, Path, PathBuf};

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
    /// On a local device (macOS `MNT_LOCAL`; Linux [`LOCAL_FSTYPES`]).
    pub local: bool,
    /// Mounted by the automounter (macOS `MNT_AUTOMOUNTED`, or `autofs`).
    pub automounted: bool,
    /// Mounted read-only.
    pub read_only: bool,
    /// The APFS container (`disk3` of `/dev/disk3s5`); `None` elsewhere.
    pub container: Option<String>,
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
    /// Inodes free, where the file system counts them.
    pub files_free: Option<u64>,
    /// The mount point's device (`st_dev`): the removal fence's.
    pub dev: u64,
    /// Mounted read-only.
    pub read_only: bool,
}

/// Which free space a reading is of: an APFS container's (every volume in it
/// shares one), else one file system's.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VolumeKey {
    /// An APFS container (`disk3`).
    Container(String),
    /// A file system's id (Linux `f_fsid`; macOS the mount point's `st_dev`).
    Fsid(u64),
}

impl VolumeKey {
    /// A short spelling for a line or a file's key (`container:disk3`,
    /// `fsid:16777234`).
    #[must_use]
    pub fn spelled(&self) -> String {
        match self {
            VolumeKey::Container(c) => format!("container:{c}"),
            VolumeKey::Fsid(f) => format!("fsid:{f}"),
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
    /// The system refused the read (an errno).
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

    /// `path` as the file system places it: its data-volume spelling where
    /// a firmlink names it ([`Self::on_data`]), else itself. A mount point is
    /// listed as it was mounted (`/Volumes/Recovery` lies under the `/Volumes`
    /// firmlink), so both sides of a comparison are read this way.
    fn placed(&self, path: &Path) -> PathBuf {
        self.on_data(path).unwrap_or_else(|| path.to_path_buf())
    }

    /// Every mount point as a walk meets it: each one's own spelling and, for
    /// one on macOS's data volume, its firmlinked form (`/System/Volumes/Data/
    /// home` and `/home`), sorted, each once. What a walk never enters or
    /// `stat`s (`aterm_agent::harness::disk::discover`'s `Roots::mounts`).
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
/// where a firmlink names it ([`Table::on_data`]). PURE: no path is
/// touched, so a stalled mount cannot block it. `None` for a relative path,
/// one that walks up through `..`, or a table holding no mount of it.
#[must_use]
pub fn mount_of<'a>(path: &Path, t: &'a Table) -> Option<&'a Mount> {
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

/// Every mount of `t` AT or UNDER `dir` (lexical, both spellings of a
/// firmlinked path). PURE. A whole-directory removal reads this as "a volume
/// is mounted inside": a Linux bind mount shares its source's `st_dev`, so a
/// device fence alone cannot see it. A relative `dir` (or one with `..`)
/// answers every mount: nothing is ruled out on a path nobody can place.
pub fn mounts_under<'a>(dir: &Path, t: &'a Table) -> impl Iterator<Item = &'a Mount> + 'a {
    let plain = plain_absolute(dir);
    let dir = t.placed(dir);
    t.mounts
        .iter()
        .filter(move |m| !plain || t.placed(&m.on).starts_with(&dir))
}

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

/// The machine's mount table now, read without asking any file system:
/// `getfsstat(MNT_NOWAIT)` and `/usr/share/firmlinks` on macOS, one bounded
/// read of `/proc/self/mountinfo` on Linux, an empty INCOMPLETE table
/// elsewhere (and where the read fails).
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
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Table::default()
    }
}

/// One volume read now, through its mount point `m` — only one the table
/// calls local and not automounted ([`Unread`] otherwise): one `statfs`
/// (macOS) or `statvfs` (Linux) and one `stat` for its device.
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
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        arm::read(m)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(Unread::Unsupported)
    }
}

/// [`read`] of the volume holding `path` in `t` ([`mount_of`]).
///
/// # Errors
///
/// As [`read`]; [`Unread::NoMount`] when no mount of `t` holds it, and
/// [`Unread::Unsupported`] where there is no reader.
pub fn read_path(path: &Path, t: &Table) -> Result<Reading, Unread> {
    if cfg!(not(any(target_os = "macos", target_os = "linux"))) {
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
