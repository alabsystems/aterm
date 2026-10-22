// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mount-table enumeration, volume space queries, and pure path-to-mount lookups.
//!
//! [`table`] lists mounts without querying remote filesystems; [`mount_of`],
//! [`mounts_under`], [`Table::is_under`], and [`Table::is_mount_point`] resolve
//! paths lexically across macOS firmlinks and Windows volume GUIDs/`subst` drives.
//! [`read`] queries only local, non-automounted volumes, and [`file_id`] returns
//! device and inode/file-ID identity.

use std::path::{Component, Path, PathBuf};

use crate::paths;

/// Maximum mounts returned by [`table`]; longer tables mark [`Table::complete`] false.
pub const MAX_MOUNTS: usize = 256;

/// Maximum bytes read from `/proc/self/mountinfo` on Linux.
pub const MOUNTINFO_CAP: usize = 64 * 1024;

/// macOS's data volume, where every firmlinked root lives.
pub const DATA_VOLUME: &str = "/System/Volumes/Data";

/// Linux filesystem types treated as local (`statvfs`-safe).
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
    /// True for a local device (`MNT_LOCAL` on macOS, [`LOCAL_FSTYPES`] on Linux,
    /// `\Device\HarddiskVolume<n>` on Windows).
    pub local: bool,
    /// Mounted by the automounter (macOS `MNT_AUTOMOUNTED`, or `autofs`).
    pub automounted: bool,
    /// True for removable media (`MNT_REMOVABLE` on macOS, `/media` or `/run/media`
    /// on Linux, optical/floppy devices on Windows).
    pub removable: bool,
    /// Mounted read-only.
    pub read_only: bool,
    /// Shared free-space pool identifier: APFS container (`disk3`) or Windows
    /// volume GUID name (`Volume{...}`).
    pub container: Option<String>,
}

impl Mount {
    /// Free-space deduplication key ([`VolumeKey::Volume`] or [`VolumeKey::Container`]), if any.
    #[must_use]
    pub fn key(&self) -> Option<VolumeKey> {
        let c = self.container.as_deref()?;
        Some(match volume_guid(c) {
            Some(guid) => VolumeKey::Volume(guid.to_ascii_lowercase()),
            None => VolumeKey::Container(c.to_owned()),
        })
    }
}

/// Extract `{guid}` from a `Volume{guid}` name.
fn volume_guid(name: &str) -> Option<&str> {
    let guid = name
        .get(6..)
        .filter(|_| name[..6].eq_ignore_ascii_case("Volume"))?;
    (guid.len() == 38 && guid.starts_with('{') && guid.ends_with('}')).then_some(guid)
}

/// Device and file identifier (`st_dev`/`st_ino` on Unix; 64-bit volume serial
/// and 128-bit file ID from `FILE_ID_INFO` on Windows).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId {
    /// The device: `st_dev`; the 64-bit volume serial on Windows.
    pub dev: u64,
    /// The file on it: `st_ino`; the 128-bit file id on Windows.
    pub ino: u128,
}

/// Read [`FileId`] for `path` (following symlinks/reparse points only when `follow` is true).
///
/// Returns `None` if missing, inaccessible, or on a filesystem without stable file IDs.
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

/// Read [`FileId`] from an open Windows handle via `GetFileInformationByHandleEx(FileIdInfo)`.
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
    /// True when the full mount table was read without truncation or error.
    pub complete: bool,
    /// macOS firmlinks mapping root paths (`/Users`) to data-volume paths.
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
    /// Free inodes, if tracked by the filesystem (`None` on Windows).
    pub files_free: Option<u64>,
    /// Mount point device ID (`st_dev` on Unix, volume root serial on Windows).
    pub dev: u64,
    /// Mounted read-only.
    pub read_only: bool,
}

/// Identifier for a shared free-space pool.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VolumeKey {
    /// An APFS container (`disk3`).
    Container(String),
    /// A file system's id (Linux `f_fsid`; macOS the mount point's `st_dev`).
    Fsid(u64),
    /// Lowercase Windows volume GUID (`{...}`).
    Volume(String),
}

impl VolumeKey {
    /// Canonical string key (`container:<c>`, `fsid:<id>`, or `volume:<guid>`).
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
    /// OS error code (`errno` or Win32 error) when the query failed.
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
    /// Translate `path` to its macOS data-volume path if under a firmlinked root.
    #[must_use]
    pub fn on_data(&self, path: &Path) -> Option<PathBuf> {
        self.firmlinks
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .and_then(|(root, data)| path.strip_prefix(root).ok().map(|rest| data.join(rest)))
    }

    /// Return `path` mapped through macOS firmlinks if applicable.
    fn placed(&self, path: &Path) -> PathBuf {
        self.on_data(path).unwrap_or_else(|| path.to_path_buf())
    }

    /// Check whether `path` is at or under `dir`, resolving macOS firmlinks and
    /// Windows volume mounts/`subst` drives. Returns `None` if placement is ambiguous.
    #[must_use]
    pub fn is_under(&self, path: &Path, dir: &Path) -> Option<bool> {
        under_in(path, dir, self, cfg!(windows))
    }

    /// Expand 8.3 short components in `path` via [`long_name`] when its prefix lies on a
    /// local, non-automounted Windows mount; returns `path` unchanged elsewhere or on failure.
    #[must_use]
    pub fn long_spelling<'p>(&self, path: &'p Path) -> std::borrow::Cow<'p, Path> {
        long_spelling_in(path, self, cfg!(windows), long_name)
    }

    /// Like [`Self::long_spelling`], using `expand` to resolve the short-named prefix.
    #[must_use]
    pub fn long_spelling_by<'p>(
        &self,
        path: &'p Path,
        expand: impl FnOnce(&Path) -> Option<PathBuf>,
    ) -> std::borrow::Cow<'p, Path> {
        long_spelling_in(path, self, cfg!(windows), expand)
    }

    /// Check whether `path` is a mount point in this table (`None` if placement is ambiguous).
    #[must_use]
    pub fn is_mount_point(&self, path: &Path) -> Option<bool> {
        point_in(path, self, cfg!(windows))
    }

    /// All mount points in this table (including firmlink aliases on macOS), sorted and deduplicated.
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

/// Longest-prefix mount in `t` containing `path` (pure, without touching the filesystem).
#[must_use]
pub fn mount_of<'a>(path: &Path, t: &'a Table) -> Option<&'a Mount> {
    mount_in(path, t, cfg!(windows))
}

/// All mounts in `t` at or lexically under `dir` (conservative when `dir` or table is ambiguous).
pub fn mounts_under<'a>(dir: &Path, t: &'a Table) -> impl Iterator<Item = &'a Mount> + 'a {
    mounts_in(dir, t, cfg!(windows))
}

/// Platform-selectable implementation of [`mount_of`].
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

/// Platform-selectable implementation of [`mounts_under`].
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

/// Platform-selectable implementation of [`Table::long_spelling_by`].
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

/// Resolve 8.3 short names in an existing path via `GetLongPathNameW` on Windows (`None` elsewhere).
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

/// Check whether placed Windows key `path` is under `dir` across all volume mount spellings.
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

/// Verbatim [`paths::windows_key`] for a mount-manager path (`NETWORK` maps to itself).
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

/// Deepest non-`subst` mount containing `key`, and the relative suffix beneath it.
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

/// Maximum alias spellings or folder-mount hops followed for a single path.
const SPELLINGS: usize = 64;

/// All canonical Windows keys naming the same volume location as `key`.
fn spellings(key: &str, mounts: &[Mount]) -> Option<Vec<String>> {
    let (m, rest) = holder(key, mounts, false)?;
    let mut out = Vec::new();
    spell(volume_of(m)?, rest, mounts, 0, &mut out)?;
    Some(out)
}

/// Recursively collect mount-point spellings for `rest` on volume `name`.
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

/// Join a Windows mount key `base` with relative suffix `rest`.
fn joined(base: &str, rest: &str) -> String {
    match (base.is_empty() || base.ends_with('\\'), rest.is_empty()) {
        (_, true) => base.to_owned(),
        (true, false) => format!("{base}{rest}"),
        (false, false) => format!("{base}\\{rest}"),
    }
}

/// Normalize `path` into a Windows key, resolving `Volume{...}` roots and `subst` drives.
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

/// Synthetic `fstype` for a Windows `subst` drive mapping.
pub const SUBST: &str = "subst";

/// Maximum `subst` redirection hops followed when placing a Windows path.
pub const SUBST_HOPS: usize = 8;

/// Synthetic mount point covering all Windows UNC paths.
pub const NETWORK: &str = r"\\";

/// Absolute, and no `.` or `..` component.
fn plain_absolute(p: &Path) -> bool {
    p.is_absolute()
        && !p
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
}

/// Extract the APFS container name (`disk3`) from a mount source like `/dev/disk3s5`.
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

/// Parse `/usr/share/firmlinks` tab-separated lines into `(root, data_volume_path)` pairs.
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

/// Parse Linux `/proc/self/mountinfo` text into a [`Table`].
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
        let removable = on.starts_with("/media") || on.starts_with("/run/media");
        mounts.push(Mount {
            removable,
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

/// Split a NUL-separated `MULTI_SZ` UTF-16 buffer into slices.
#[cfg(any(windows, test))]
pub(crate) fn multi_sz(buf: &[u16]) -> Vec<&[u16]> {
    buf.split(|&c| c == 0)
        .take_while(|s| !s.is_empty())
        .collect()
}

/// Classification of a Windows `QueryDosDeviceW` target path.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Device<'a> {
    /// Local `\Device\HarddiskVolume<n>` volume.
    Volume,
    /// Optical or floppy drive (`\Device\CdRom<n>`, `\Device\Floppy<n>`).
    Removable,
    /// `\??\C:\work`: a `subst` letter onto the drive path it carries.
    Subst(&'a str),
    /// Network redirector, virtual filesystem, or unclassified device.
    Remote,
}

/// Classify an NT object-manager device target string.
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
    /// NT device path (`\Device\HarddiskVolume3`), if readable.
    pub(crate) device: Option<String>,
    /// Mounted paths (`C:\`, `C:\mnt\data\`), if readable.
    pub(crate) paths: Option<Vec<PathBuf>>,
}

/// Assemble a Windows [`Table`] from enumerated volumes and per-session drive letters.
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
        removable: false,
        read_only: false,
        container: None,
    }];
    let in_session = |letter: char| {
        letters
            .iter()
            .find(|(l, _)| l.eq_ignore_ascii_case(&letter))
            .map(|(_, t)| t.as_deref())
    };
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
                let global = if root {
                    v.device.as_deref()
                } else {
                    owner(letter)
                };
                match in_session(letter) {
                    Some(Some(t)) if global.is_some_and(|d| d.eq_ignore_ascii_case(t)) => {}
                    // Skip mounts shadowed by a per-session `subst` or network drive.
                    Some(Some(_)) if root || global.is_some() => continue,
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
                removable: class == Some(Device::Removable),
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
            removable: class == Device::Removable,
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

/// Leading ASCII drive letter of `p`, if any.
#[cfg(any(windows, test))]
fn drive_letter(p: &Path) -> Option<char> {
    match p.to_str()?.as_bytes() {
        [l, b':', b'\\', ..] if l.is_ascii_alphabetic() => Some(char::from(l.to_ascii_uppercase())),
        _ => None,
    }
}

/// Decode a `GetLogicalDrives` bitmask into uppercase drive letters (`None` if zero).
#[cfg(any(windows, test))]
pub(crate) fn drive_letters(mask: u32) -> Option<Vec<char>> {
    let letters: Vec<char> = (b'A'..=b'Z')
        .zip(0..26u32)
        .filter(|(_, bit)| mask & (1 << bit) != 0)
        .map(|(l, _)| char::from(l))
        .collect();
    (!letters.is_empty()).then_some(letters)
}

/// Strip trailing backslash from non-root Windows mount points.
#[cfg(any(windows, test))]
fn mount_point(p: &Path) -> PathBuf {
    match p.to_str() {
        Some(s) if letter_root(p).is_none() && s.len() > 1 => {
            PathBuf::from(s.trim_end_matches('\\'))
        }
        _ => p.to_path_buf(),
    }
}

/// Read the machine's current mount table without querying remote filesystems.
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

/// Query capacity, free space, and device ID for a local, non-automounted [`Mount`].
///
/// # Errors
///
/// Returns [`Unread`] if the mount is remote, automounted, refused by the OS, or unsupported.
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

/// Check whether `m` is backed by a fixed disk (`DRIVE_FIXED`) on Windows (`None` if unknown).
#[cfg(windows)]
#[must_use]
pub fn is_fixed(m: &Mount) -> Option<bool> {
    if !m.local || m.fstype == SUBST {
        return Some(false);
    }
    arm::drive_type(m).map(|kind| kind == crate::win_paths::DRIVE_FIXED)
}

/// Resolve `path` in `t` via [`mount_of`] and [`read`] its volume.
///
/// # Errors
///
/// Returns [`Unread::NoMount`] if unmounted in `t`, or the error from [`read`].
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

    // `<sys/mount.h>` `getfsstat` (`getfsstat$INODE64` on x86_64 macOS).
    unsafe extern "C" {
        #[cfg_attr(target_arch = "x86_64", link_name = "getfsstat$INODE64")]
        fn getfsstat(
            buf: *mut libc::statfs,
            bufsize: libc::c_int,
            mode: libc::c_int,
        ) -> libc::c_int;
    }

    const MNT_NOWAIT: libc::c_int = 2;
    const MNT_RDONLY: u32 = 0x0000_0001;
    const MNT_LOCAL: u32 = 0x0000_1000;
    const MNT_REMOVABLE: u32 = 0x0000_0200;
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
        let room = count.saturating_add(4).min(super::MAX_MOUNTS + 1);
        let mut buf = vec![blank; room];
        let Ok(bytes) = libc::c_int::try_from(std::mem::size_of_val(buf.as_slice())) else {
            return Table {
                firmlinks,
                ..Table::default()
            };
        };
        // SAFETY: `buf` has `bytes` writable bytes; `MNT_NOWAIT` does not block on mounts.
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
                    removable: r.f_flags & MNT_REMOVABLE != 0,
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

    /// Verbatim volume GUID root or mount path for querying `m`.
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
