// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHEN A CHECKOUT IS GONE, AND WHERE NOTHING LOOKS — ONE RULE for its readers:
//! atpkg's Rust build cache sweep (`atpkg::build_cache::reclaim`, which lets a gone
//! checkout's build dir go) and the window's disk watch (`aterm_agent`'s
//! `harness::disk::discover`, which never walks where [`never_looked_at`] says no
//! look goes). Std only (on Windows, `aterm_sysprobe`'s file identity and path key
//! too), and nothing here names either of them.
//!
//! A checkout is GONE only when it is SEEN to be ([`gone`]): its folder is
//! there without a `Cargo.toml` (or Trust's `Targo.toml`, [`MANIFESTS`]), or
//! the folder that held it is there without it. Nothing else is a sight, so
//! nothing else lets a cache go:
//!
//! * a `Cargo.toml` that cannot be read (a folder this process may not
//!   enter: `EACCES`, a privacy refusal's `EPERM`) is not an absent one;
//! * a checkout whose folder AND the folder above it are both missing is
//!   away, not removed (a volume not mounted now: `/Volumes/<name>` goes
//!   with it), or a tree moved;
//! * a checkout where volumes are mounted ([`VOLUME_ROOTS`]; on Windows any
//!   drive but `$HOME`'s, where every letter is such a place) is never looked
//!   at: a volume's root has the mount folder above it whether or not the
//!   volume is there, and on macOS a removable or network volume is a
//!   privacy-guarded place as well — and a stalled network mount answers a
//!   `stat` by blocking;
//! * a checkout in one of `$HOME`'s folders the system's privacy consent
//!   guards ([`HOME_SKIP`]: `~/Desktop`, `~/Documents`, `~/Downloads` …) is
//!   never looked at either: a `stat` there from the window can put a
//!   privacy prompt on the owner's screen at a moment nobody asked for
//!   anything. Nor is one reached through a link below `$HOME`, which could
//!   lead there.
//!
//! Reading any error on `Cargo.toml` as "removed" would delete the build
//! cache (tens of GB, a full rebuild) of a checkout behind `chmod 000`, on an
//! unmounted volume or at a volume's root. A checkout removed together with
//! the folder that held it keeps its cache — unless the caller can PLACE it
//! ([`Placing`], [`gone_placed`]): under `$HOME`, reached through no link, its
//! nearest existing folder on the home volume's device and not empty (an empty
//! one is what a custom mount point reads while its volume is unplugged), and
//! no mount point of a complete mount table between that folder and it. Then
//! no volume can be unmounted there, so "away" cannot be the reason: a workspace nested in a
//! removed checkout (`~/Developer/codex-alab-r3/codex-rs`, both gone) is
//! gone. Measured 2026-10-17: seven such mirrors were kept for good.

use std::path::{Component, Path, PathBuf};

/// What lets [`gone_placed`] call a workspace gone whose folder and the
/// folder above it are both missing: the home volume's device, and the mount
/// points of a COMPLETE mount table (each spelling a walk meets, firmlinked
/// forms included). A caller that has no complete table passes none, and the
/// rule keeps such a cache, as [`gone`] does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placing {
    /// The device of `$HOME`'s volume (`st_dev`; on Windows the volume serial of
    /// `aterm_sysprobe::volumes::file_id`, the one width every device fence reads).
    pub home_dev: u64,
    /// Every mount point of a mount table read whole.
    pub mounts: Vec<PathBuf>,
}

/// The manifests that make a folder a workspace: Cargo's, and Trust's native one.
pub const MANIFESTS: &[&str] = &["Cargo.toml", "Targo.toml"];

/// Directories of `$HOME` never walked by the disk watch and never looked
/// into for a checkout ([`gone`]): the system's own trees — and, on macOS,
/// the folders the system's privacy consent (TCC) guards: a look from the
/// window's process into `~/Desktop`, `~/Documents` or `~/Downloads` (or the
/// media folders, or `~/Library`'s iCloud Drive) can raise a privacy prompt
/// on the owner's screen, or fail with `Operation not permitted`, at a
/// moment nobody asked for anything.
#[cfg(target_os = "macos")]
pub const HOME_SKIP: &[&str] = &[
    "Library",
    "Applications",
    "Desktop",
    "Documents",
    "Downloads",
    "Movies",
    "Music",
    "Pictures",
];

/// See the macOS list: the system's own trees.
#[cfg(not(any(target_os = "macos", windows)))]
pub const HOME_SKIP: &[&str] = &["Library", "Applications"];

/// Windows: the profile's own trees — `AppData` (every program's state and
/// caches, Claude Code's and aterm's among them) — and every folder a cloud
/// client syncs, `OneDrive` and `OneDrive - <organization>` (an entry ending
/// in `*` names every folder whose name starts with the rest,
/// [`home_skip`]): a look there can make the client fetch a file nobody
/// asked for. Any other folder whose attributes say a look would fetch it
/// (recall on open or on data access, offline) or that is a reparse point
/// other than a link is passed over by the walk that meets it, by its
/// attributes (`aterm_agent::harness::disk::discover`).
#[cfg(windows)]
pub const HOME_SKIP: &[&str] = &["AppData", "OneDrive*"];

/// The [`HOME_SKIP`] entry `name` (a folder's name as a directory lists it)
/// is, if any: blind to ASCII case on macOS, as its file systems are by
/// default; on Windows blind to case as NTFS folds it (the one fold of
/// `aterm_sysprobe::paths`), an entry ending in `*` matching every name it
/// begins; exact elsewhere.
#[must_use]
pub fn home_skip(name: &std::ffi::OsStr) -> Option<&'static str> {
    HOME_SKIP.iter().copied().find(|s| same_name(name, s))
}

/// Where volumes are mounted, and come and go: a checkout under one is
/// never looked at ([`gone`]). On macOS a removable or network volume is
/// privacy-guarded too.
#[cfg(target_os = "macos")]
pub const VOLUME_ROOTS: &[&str] = &["/Volumes"];

/// See the macOS list: where removable volumes are mounted.
#[cfg(not(any(target_os = "macos", windows)))]
pub const VOLUME_ROOTS: &[&str] = &["/media", "/run/media", "/mnt"];

/// Windows has no folder where volumes come and go: every drive letter is
/// one (a USB stick's or a DVD's letter names another medium tomorrow, a
/// `subst` or network letter another folder), so no folder is listed here,
/// and a checkout on any drive but `$HOME`'s own is never looked at
/// ([`off_home_drive`]).
#[cfg(windows)]
pub const VOLUME_ROOTS: &[&str] = &[];

/// Is the checkout at `checkout` SEEN to be gone — no longer a workspace?
/// `Ok` with the sight, `Err` with why it is not called gone. `home` is the
/// `$HOME` whose guarded folders ([`HOME_SKIP`]) are never looked into.
///
/// # Errors
///
/// Why the checkout is not called gone: it is there, it was not looked at,
/// or what was looked at could not be read.
pub fn gone(checkout: &Path, home: Option<&Path>) -> Result<String, String> {
    gone_placed(checkout, home, None)
}

/// [`gone`], and where `placing` is given, a workspace whose folder and the
/// folder above it are both missing is gone too when it can be PLACED on the
/// home volume ([`Placing`], the module docs): under `home`, outside every
/// place never looked into, every folder on the way below `home` a real
/// folder (no link), its nearest existing ancestor on `placing.home_dev`, and
/// no mount point of `placing.mounts` on the path between that ancestor and
/// it.
///
/// # Errors
///
/// As [`gone`].
pub fn gone_placed(
    checkout: &Path,
    home: Option<&Path>,
    placing: Option<&Placing>,
) -> Result<String, String> {
    gone_with(
        checkout,
        home,
        placing,
        &mut |p| std::fs::symlink_metadata(p).map(|m| m.is_dir()),
        &mut |p| device_of(p),
        &mut |p| empty_folder(p),
    )
}

/// Entries an empty folder may hold and still read as empty: what Finder
/// leaves in a folder it showed.
const FINDER_LITTER: &[&str] = &[".DS_Store", ".localized"];

/// Whether the folder at `p` holds nothing but [`FINDER_LITTER`]: what a
/// volume's mount point reads while the volume is not mounted.
fn empty_folder(p: &Path) -> std::io::Result<bool> {
    for entry in std::fs::read_dir(p)? {
        let name = entry?.file_name();
        if !FINDER_LITTER.iter().any(|l| name == *l) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The device of the entry at `p`, a link not followed: `st_dev`; on Windows
/// the one identity export's volume serial
/// (`aterm_sysprobe::volumes::file_id`, the width [`Placing::home_dev`] is
/// read in), an error where it cannot be read (a FAT volume keeps no file
/// ids: never a narrower stand-in).
fn device_of(p: &Path) -> std::io::Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::symlink_metadata(p).map(|m| m.dev())
    }
    #[cfg(windows)]
    {
        aterm_sysprobe::volumes::file_id(p, false)
            .map(|id| id.dev)
            .ok_or_else(|| std::io::Error::other("its file identity could not be read"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = p;
        Err(std::io::Error::other("no device numbers here"))
    }
}

/// [`gone`], every look at the file system through `stat` — the path's
/// entry, a link not followed at the last component: whether it is a
/// directory, or the error. Nothing is looked at before the path is known
/// to lie outside every place never looked into, and each folder on the way
/// below `home` is looked at before anything under it, so no link is
/// followed there.
///
/// # Errors
///
/// As [`gone`].
pub fn gone_by(
    checkout: &Path,
    home: Option<&Path>,
    stat: &mut dyn FnMut(&Path) -> std::io::Result<bool>,
) -> Result<String, String> {
    gone_with(
        checkout,
        home,
        None,
        stat,
        &mut |_| Err(std::io::Error::other("not placed")),
        &mut |_| Err(std::io::Error::other("not placed")),
    )
}

/// [`gone_placed`], every look through `stat` (as [`gone_by`]), every
/// device read through `dev` (the entry's, a link not followed) and every
/// folder's emptiness through `empty` (it holds nothing but what Finder
/// leaves).
///
/// # Errors
///
/// As [`gone`].
pub fn gone_with(
    checkout: &Path,
    home: Option<&Path>,
    placing: Option<&Placing>,
    stat: &mut dyn FnMut(&Path) -> std::io::Result<bool>,
    dev: &mut dyn FnMut(&Path) -> std::io::Result<u64>,
    empty: &mut dyn FnMut(&Path) -> std::io::Result<bool>,
) -> Result<String, String> {
    let shown = checkout.display();
    if !checkout.is_absolute() || checkout.components().any(|c| c == Component::ParentDir) {
        return Err(format!(
            "its checkout {shown} is not an absolute path, so it is not looked at"
        ));
    }
    if let Some(place) = never_looked_at(checkout, home) {
        return Err(format!(
            "its checkout {shown} is {place}, never looked into, so it is not called gone"
        ));
    }
    let unread = |what: &Path, e: &std::io::Error| {
        format!(
            "{} could not be read ({e}), so its checkout is not called gone",
            what.display()
        )
    };
    let below_home = home
        .and_then(|h| below(checkout, h))
        .map_or(0, |names| names.len());
    let mut way: Vec<&Path> = checkout.ancestors().take(below_home).collect();
    way.reverse();
    // The nearest folder of the way that is there (each looked at before
    // anything under it, so none is reached through a link).
    let mut nearest: Option<&Path> = None;
    for folder in way {
        match stat(folder) {
            Ok(true) => nearest = Some(folder),
            Ok(false) => {
                return Err(format!(
                    "{} is not a folder (a link is not followed), so its checkout is not \
                     called gone",
                    folder.display()
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(unread(folder, &e)),
        }
    }
    for name in MANIFESTS {
        let manifest = checkout.join(name);
        match stat(&manifest) {
            Ok(_) => return Err(format!("its checkout {shown} is there")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(unread(&manifest, &e)),
        }
    }
    match stat(checkout) {
        Ok(true) => return Ok(format!("its folder {shown} is there without a Cargo.toml")),
        Ok(false) => return Err(format!("its checkout {shown} is not a folder")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(unread(checkout, &e)),
    }
    let Some(parent) = checkout.parent() else {
        return Err(format!("its checkout {shown} has no folder above it"));
    };
    match stat(parent) {
        Ok(true) => Ok(format!(
            "the folder that held it, {}, is there without it",
            parent.display()
        )),
        Ok(false) => Err(format!("{} is not a folder", parent.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let away = format!(
                "neither its checkout {shown} nor the folder that held it is there ({e}) — away \
                 (a volume not mounted now), not seen removed"
            );
            match (placing, home) {
                (Some(placing), Some(home)) if below_home > 0 => {
                    placed_gone(checkout, home, nearest, placing, (stat, dev, empty)).ok_or(away)
                }
                _ => Err(away),
            }
        }
        Err(e) => Err(format!(
            "neither its checkout {shown} nor the folder that held it is there ({e}) — away \
             (a volume not mounted now), not seen removed"
        )),
    }
}

/// The looks [`placed_gone`] makes: `stat`, `dev` and `empty` of
/// [`gone_with`].
type Looks<'a> = (
    &'a mut dyn FnMut(&Path) -> std::io::Result<bool>,
    &'a mut dyn FnMut(&Path) -> std::io::Result<u64>,
    &'a mut dyn FnMut(&Path) -> std::io::Result<bool>,
);

/// The sight of a workspace nested in a removed checkout ([`gone_placed`]):
/// its nearest existing ancestor (`nearest`, a real folder below `home`, or
/// `home` itself) is on the home volume's device, holds something (an EMPTY
/// folder is what a volume's custom mount point — `~/Developer/ext` — reads
/// while the volume is unplugged: its workspaces are away, not removed), and
/// no mount point lies on the path between that ancestor and the workspace.
/// `None`: it cannot be placed, and it is kept.
fn placed_gone(
    checkout: &Path,
    home: &Path,
    nearest: Option<&Path>,
    placing: &Placing,
    (stat, dev, empty): Looks<'_>,
) -> Option<String> {
    let ancestor = match nearest {
        Some(folder) => folder,
        None => {
            // Nothing below `$HOME` is there: `$HOME` itself, a real folder.
            stat(home).ok().filter(|is_dir| *is_dir)?;
            home
        }
    };
    if dev(ancestor).ok()? != placing.home_dev {
        return None;
    }
    if empty(ancestor).unwrap_or(true) {
        return None;
    }
    let mounted_between = placing
        .mounts
        .iter()
        .any(|m| mounted_between(m, ancestor, checkout));
    (!mounted_between).then(|| {
        format!(
            "neither it nor the folder that held it is there, and {} — on the home volume, \
             with no volume mounted between — is: removed with the checkout it was in",
            ancestor.display()
        )
    })
}

/// Is the mount point `m` (of a complete table, [`Placing::mounts`]) on the
/// path between `ancestor` and `checkout` — under `ancestor` but not it, and
/// at or above `checkout`? On Windows by the one rule of
/// `aterm_sysprobe::paths` (the table lists `C:\mnt\data` plain, a walk meets
/// `\\?\C:\…` in any case). A ROOT is never between: a drive letter's
/// (`D:\`, a `subst` letter's `S:\` too) and the network's synthetic `\\`
/// lie under no folder, and a volume that is also mounted inside the path
/// is listed again at that folder, which is placed. A folder mount point
/// that rule cannot place against either (one on another drive: only the
/// mount table could relate the two) is counted as between, so the
/// workspace is kept.
fn mounted_between(m: &Path, ancestor: &Path, checkout: &Path) -> bool {
    #[cfg(windows)]
    {
        use aterm_sysprobe::paths::{is_under, windows_key};
        if m.as_os_str() == aterm_sysprobe::volumes::NETWORK {
            return false;
        }
        let Some(point) = windows_key(m) else {
            return true;
        };
        // A key ends in its separator only at a root (`D:\`, `\\SRV\SHARE\`).
        if point.ends_with('\\') {
            return false;
        }
        let same = windows_key(ancestor).is_some_and(|a| a == point);
        !same && is_under(m, ancestor) != Some(false) && is_under(checkout, m) != Some(false)
    }
    #[cfg(not(windows))]
    {
        m != ancestor && m.starts_with(ancestor) && checkout.starts_with(m)
    }
}

/// The place `checkout` lies in that is never looked into, if it lies in
/// one: under a [`VOLUME_ROOTS`] entry (on Windows on any drive but
/// `$HOME`'s, [`off_home_drive`]), or in a [`HOME_SKIP`] folder of `home`.
/// Lexical — nothing is looked at — and blind to case as the file system
/// is: ASCII case on macOS (`~/documents` is `~/Documents`), NTFS's on
/// Windows, where a verbatim and a plain spelling of one path read alike
/// (`\\?\C:\Users\x\appdata` is `C:\Users\X\AppData`). The disk watch asks it
/// of any path it would read from the window, a Claude Code directory too.
#[must_use]
pub fn never_looked_at(checkout: &Path, home: Option<&Path>) -> Option<String> {
    if let Some(root) = VOLUME_ROOTS
        .iter()
        .find(|r| below(checkout, Path::new(r)).is_some())
    {
        return Some(format!("where volumes are mounted ({root})"));
    }
    #[cfg(windows)]
    if let Some(place) = off_home_drive(checkout, home) {
        return Some(place);
    }
    let rest = below(checkout, home?)?;
    let first = rest.first()?;
    home_skip(first).map(|s| format!("in a folder of $HOME that is never looked into (~/{s})"))
}

/// Windows' [`VOLUME_ROOTS`]: the place `checkout` lies in when it is not on
/// `$HOME`'s own drive (or share) — by the root of the key of
/// `aterm_sysprobe::paths` (`C:\`, `\\SRV\SHARE\`), any spelling — or when
/// there is no `$HOME` to name that drive, or either path cannot be keyed.
/// `$HOME`'s drive holds the profile, a fixed disk; any other letter may be
/// a removable medium, and another medium at the same letter tomorrow would
/// show the folder that held a checkout "there without it".
#[cfg(windows)]
fn off_home_drive(checkout: &Path, home: Option<&Path>) -> Option<String> {
    // `windows_under` places two paths only under one root.
    let on_home_drive =
        home.is_some_and(|h| aterm_sysprobe::paths::windows_under(checkout, h).is_some());
    (!on_home_drive).then(|| {
        "where volumes come and go (a drive other than $HOME's, or no $HOME to name its drive)"
            .to_owned()
    })
}

/// The names of `path` past `root`, when `root` is a prefix of it component
/// by component ([`same_name`]); on Windows by the key of
/// `aterm_sysprobe::paths` (a verbatim prefix stripped, case folded as NTFS
/// folds it: those names come back folded), `None` where either cannot be
/// keyed.
fn below(path: &Path, root: &Path) -> Option<Vec<std::ffi::OsString>> {
    #[cfg(windows)]
    {
        let (key, root) = (
            aterm_sysprobe::paths::windows_key(path)?,
            aterm_sysprobe::paths::windows_key(root)?,
        );
        if !aterm_sysprobe::paths::key_under(&key, &root) {
            return None;
        }
        let rest = key.get(root.len()..)?.trim_start_matches('\\');
        Some(
            rest.split('\\')
                .filter(|n| !n.is_empty())
                .map(std::ffi::OsString::from)
                .collect(),
        )
    }
    #[cfg(not(windows))]
    {
        let mut rest = path.components();
        for want in root.components() {
            let got = rest.next()?;
            let same = match (got, want) {
                (Component::Normal(a), Component::Normal(b)) => {
                    b.to_str().is_some_and(|b| same_name(a, b))
                }
                (a, b) => a == b,
            };
            if !same {
                return None;
            }
        }
        Some(rest.map(|c| c.as_os_str().to_os_string()).collect())
    }
}

/// One path component against a name: blind to ASCII case on macOS (its
/// file systems are, by default); on Windows blind to case as NTFS folds it
/// (each side keyed by `aterm_sysprobe::paths::windows_key` as a name of one
/// folder), a `name` ending in `*` matching every component it begins
/// ([`HOME_SKIP`]'s `OneDrive*`); exact elsewhere.
fn same_name(component: &std::ffi::OsStr, name: &str) -> bool {
    #[cfg(windows)]
    {
        // A name keyed as the one folder of a drive root, verbatim (so a
        // trailing dot or space is part of it): `C:\` and its folded name.
        let fold = |n: &std::ffi::OsStr| {
            let mut p = std::ffi::OsString::from(r"\\?\C:\");
            p.push(n);
            aterm_sysprobe::paths::windows_key(Path::new(&p))
                .and_then(|k| k.get(3..).map(str::to_owned))
                .filter(|k| !k.is_empty() && !k.contains('\\'))
        };
        let (pattern, prefix) = match name.strip_suffix('*') {
            Some(head) => (head, true),
            None => (name, false),
        };
        let (Some(got), Some(want)) = (fold(component), fold(pattern.as_ref())) else {
            return false;
        };
        if prefix {
            got.starts_with(&want)
        } else {
            got == want
        }
    }
    #[cfg(not(windows))]
    {
        if cfg!(target_os = "macos") {
            component
                .to_str()
                .is_some_and(|c| c.eq_ignore_ascii_case(name))
        } else {
            component == name
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// What the fixture's `/`-rooted paths are rooted at: a drive on Windows,
    /// where a path with no prefix is not absolute.
    const ROOT: &str = if cfg!(windows) { "C:" } else { "" };

    /// The fixture path `p` (`/`-rooted), absolute here.
    fn at(p: &str) -> PathBuf {
        PathBuf::from(format!("{ROOT}{p}"))
    }

    /// A file system of named folders (`true`), links (`false`) and their
    /// devices; anything else is missing.
    struct Fs {
        entries: BTreeMap<PathBuf, (bool, u64)>,
    }

    impl Fs {
        fn new(entries: &[(&str, bool, u64)]) -> Self {
            Self {
                entries: entries
                    .iter()
                    .map(|(p, dir, dev)| (at(p), (*dir, *dev)))
                    .collect(),
            }
        }

        fn gone(&self, checkout: &str, placing: Option<&Placing>) -> Result<String, String> {
            let found = |p: &Path| {
                self.entries
                    .get(p)
                    .copied()
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "missing"))
            };
            gone_with(
                &at(checkout),
                Some(&at("/h")),
                placing,
                &mut |p| found(p).map(|(dir, _)| dir),
                &mut |p| found(p).map(|(_, dev)| dev),
                &mut |p| {
                    found(p)?;
                    Ok(!self.entries.keys().any(|e| e.parent() == Some(p)))
                },
            )
        }
    }

    fn placing(mounts: &[&str]) -> Placing {
        Placing {
            home_dev: 1,
            mounts: mounts.iter().map(|m| at(m)).collect(),
        }
    }

    /// A WORKSPACE NESTED IN A REMOVED CHECKOUT IS GONE once it can be placed:
    /// `~/dev/codex-r3/codex-rs` with `~/dev/codex-r3` missing and `~/dev`
    /// there on the home volume. NEGATIVE CONTROLS, each kept: no placing (the
    /// rule as it was), a mount point on the path between, the nearest folder
    /// on another device, a link on the way, a `$HOME` folder never looked
    /// into, and a volume root.
    #[test]
    fn a_workspace_nested_in_a_removed_checkout_is_gone_only_where_it_can_be_placed() {
        let fs = Fs::new(&[
            ("/h", true, 1),
            ("/h/dev", true, 1),
            ("/h/dev/other", true, 1),
            ("/h/ln", false, 1),
            ("/h/dev/ext", true, 1),
        ]);
        let ws = "/h/dev/codex-r3/codex-rs";
        assert!(fs.gone(ws, None).is_err(), "unplaced: away, kept");
        let seen = fs.gone(ws, Some(&placing(&["/", "/System/Volumes/Data"])));
        assert!(
            seen.as_deref().is_ok_and(|s| s.contains("/h/dev")),
            "{seen:?}"
        );
        for between in ["/h/dev/codex-r3", "/h/dev/codex-r3/codex-rs"] {
            assert!(
                fs.gone(ws, Some(&placing(&["/", between]))).is_err(),
                "a mount at {between}"
            );
        }
        let other = Placing {
            home_dev: 2,
            ..placing(&["/"])
        };
        assert!(fs.gone(ws, Some(&other)).is_err(), "another device");
        assert!(
            fs.gone("/h/ln/x/ws", Some(&placing(&["/"]))).is_err(),
            "through a link"
        );
        let skip = format!("/h/{}/r/ws", HOME_SKIP[0]);
        assert!(fs.gone(&skip, Some(&placing(&["/"]))).is_err());
        // Windows lists no volume folder: its letters are
        // `on_windows_a_root_is_never_between_and_only_homes_drive_is_looked_at`'s.
        if let Some(root) = VOLUME_ROOTS.first() {
            assert!(
                fs.gone(&format!("{root}/x/ws"), Some(&placing(&["/"])))
                    .is_err()
            );
        }
        // Nothing below `$HOME` there: placed against `$HOME` itself.
        assert!(fs.gone("/h/gone/a/ws", Some(&placing(&["/"]))).is_ok());
        // Its nearest folder EMPTY — a custom mount point (`~/dev/ext`) whose
        // volume is unplugged: away, kept. NEGATIVE CONTROL: the same depth
        // under the non-empty `~/dev` is gone.
        assert!(
            fs.gone("/h/dev/ext/proj/ws", Some(&placing(&["/"])))
                .is_err_and(|e| e.contains("away")),
            "an empty mount-point folder keeps it"
        );
        assert!(fs.gone("/h/dev/proj/ws", Some(&placing(&["/"]))).is_ok());
        // The old sights still read as they did.
        assert!(
            fs.gone("/h/dev/ws", None)
                .is_ok_and(|s| s.contains("there without it"))
        );
    }

    /// WINDOWS' FOLDERS NEVER LOOKED INTO: `AppData` and every `OneDrive*`, in
    /// any case NTFS folds alike, and a path spelled verbatim (`\\?\C:\…`, as
    /// `std::fs::canonicalize` answers) against a plain `$HOME` in another
    /// case; and any drive but `$HOME`'s. NEGATIVE CONTROLS: a sibling that
    /// only begins like `AppData`, a folder that is neither, and a path off
    /// `$HOME` on its drive.
    #[cfg(windows)]
    #[test]
    fn appdata_and_every_onedrive_are_never_looked_into_in_any_spelling() {
        use std::ffi::OsStr;
        for name in [
            "AppData",
            "appdata",
            "APPDATA",
            "OneDrive",
            "onedrive",
            "OneDrive - Contoso",
            "ONEDRIVE-Personal",
            // NTFS folds the dotless i to `I`: one name with `OneDrive`.
            "OneDr\u{131}ve",
        ] {
            assert!(home_skip(OsStr::new(name)).is_some(), "{name}");
        }
        for name in ["AppDataX", "Documents", "One", "MyOneDrive", "aterm"] {
            assert!(home_skip(OsStr::new(name)).is_none(), "{name}");
        }
        let home = Path::new(r"C:\Users\M6-AN");
        for path in [
            r"\\?\C:\Users\m6-an\appdata\Local\claude",
            r"C:\users\m6-an\AppData",
            r"c:\Users\m6-an\OneDrive - Contoso\repo",
            r"\\?\C:\USERS\M6-AN\onedrive\repo\target",
            r"D:\Users\m6-an\AppData\x",
        ] {
            assert!(
                never_looked_at(Path::new(path), Some(home)).is_some(),
                "{path}"
            );
        }
        for path in [
            r"\\?\C:\Users\m6-an\aterm\target",
            r"C:\Users\m6-an\AppDataX\x",
            r"C:\Elsewhere\AppData\x",
        ] {
            assert!(
                never_looked_at(Path::new(path), Some(home)).is_none(),
                "{path}"
            );
        }
    }

    /// WINDOWS PLACES A NESTED WORKSPACE AS UNIX DOES: the home's device and
    /// its folders' are the one identity export's volume serial (the stub
    /// this replaced named no device, so nothing was ever placed), a verbatim
    /// spelling of the workspace is below a plain `$HOME`, and a folder mount
    /// point the table lists plain (`C:\…`) is between when the walk spells
    /// it verbatim. NEGATIVE CONTROLS: a missing folder has no device; a
    /// mount point between keeps it; another device keeps it.
    #[cfg(windows)]
    #[test]
    fn a_windows_workspace_nested_in_a_removed_checkout_is_placed_by_its_volume_serial() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let home = scratch.path().join("Home");
        std::fs::create_dir_all(home.join("dev").join("other")).expect("fixture");
        let dev = device_of(&home).expect("a device");
        assert_eq!(
            Some(dev),
            aterm_sysprobe::volumes::file_id(&home, false).map(|id| id.dev)
        );
        assert!(device_of(&home.join("missing")).is_err());
        let canonical = std::fs::canonicalize(&home).expect("canonical");
        // `$HOME` as a person types it: plain, another case.
        let plain = PathBuf::from(
            aterm_sysprobe::paths::plain(&canonical)
                .to_string_lossy()
                .to_lowercase(),
        );
        let ws = canonical.join("dev").join("gone-r3").join("ws");
        let placing = |mounts: Vec<PathBuf>, home_dev: u64| Placing { home_dev, mounts };
        // The drive's root as the mount manager lists it (`C:\`).
        let Some(Component::Prefix(prefix)) = canonical.components().next() else {
            panic!("a canonical path starts at its drive: {canonical:?}");
        };
        let std::path::Prefix::VerbatimDisk(letter) = prefix.kind() else {
            panic!("a drive letter: {canonical:?}");
        };
        let root = PathBuf::from(format!("{}:\\", char::from(letter)));
        let seen = gone_placed(&ws, Some(&plain), Some(&placing(vec![root.clone()], dev)));
        assert!(seen.is_ok(), "{seen:?}");
        let between =
            aterm_sysprobe::paths::plain(&canonical.join("dev").join("gone-r3")).into_owned();
        let kept = gone_placed(
            &ws,
            Some(&plain),
            Some(&placing(vec![root.clone(), between], dev)),
        );
        assert!(kept.is_err(), "a mount point between keeps it: {kept:?}");
        let kept = gone_placed(&ws, Some(&plain), Some(&placing(vec![root], dev ^ 1)));
        assert!(kept.is_err(), "another device keeps it: {kept:?}");
    }

    /// WINDOWS: A ROOT IS NEVER BETWEEN, AND ONLY `$HOME`'S DRIVE IS LOOKED AT.
    /// Another drive letter's root, a `subst` letter's and the network's `\\`
    /// in the table leave a nested workspace placeable (every second letter
    /// read as "between" kept it on any machine with two). A checkout on any
    /// other letter, or with no `$HOME` to name its drive, is never looked
    /// at: another medium at that letter would show the folder that held it
    /// "there without it". NEGATIVE CONTROLS: a folder mount point on another
    /// drive keeps the workspace (only the mount table could relate it), and
    /// a checkout on `$HOME`'s drive is looked at.
    #[cfg(windows)]
    #[test]
    fn on_windows_a_root_is_never_between_and_only_homes_drive_is_looked_at() {
        let scratch = aterm_tempfile::tempdir().expect("scratch");
        let home = std::fs::canonicalize(scratch.path())
            .expect("canonical")
            .join("Home");
        std::fs::create_dir_all(home.join("dev").join("other")).expect("fixture");
        let dev = device_of(&home).expect("a device");
        let ws = home.join("dev").join("gone-r3").join("ws");
        let Some(Component::Prefix(prefix)) = home.components().next() else {
            panic!("a canonical path starts at its drive: {home:?}");
        };
        let std::path::Prefix::VerbatimDisk(letter) = prefix.kind() else {
            panic!("a drive letter: {home:?}");
        };
        let letter = char::from(letter).to_ascii_uppercase();
        let [other, subst] = match letter {
            'D' => ['E', 'S'],
            'S' => ['D', 'T'],
            _ => ['D', 'S'],
        };
        let root = |l: char| PathBuf::from(format!("{l}:\\"));
        let placing = |mounts: Vec<PathBuf>| Placing {
            home_dev: dev,
            mounts,
        };
        let many = placing(vec![
            root(letter),
            root(other),
            root(subst),
            PathBuf::from(aterm_sysprobe::volumes::NETWORK),
        ]);
        let seen = gone_placed(&ws, Some(&home), Some(&many));
        assert!(seen.is_ok(), "a second letter is not between: {seen:?}");
        let foreign = placing(vec![
            root(letter),
            PathBuf::from(format!(r"{other}:\mnt\x")),
        ]);
        let kept = gone_placed(&ws, Some(&home), Some(&foreign));
        assert!(kept.is_err(), "another drive's folder point: {kept:?}");

        assert_eq!(never_looked_at(&ws, Some(&home)), None);
        let elsewhere = PathBuf::from(format!(r"{other}:\work\proj"));
        let refused = gone(&elsewhere, Some(&home));
        assert!(
            refused
                .as_ref()
                .is_err_and(|why| why.contains("a drive other than $HOME's")),
            "{refused:?}"
        );
        assert!(never_looked_at(&ws, None).is_some(), "no $HOME, no drive");
        let share = Path::new(r"\\srv\share\proj");
        assert!(never_looked_at(share, Some(&home)).is_some());
    }
}
