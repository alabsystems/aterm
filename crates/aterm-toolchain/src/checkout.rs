// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHEN A CHECKOUT IS GONE, AND WHERE NOTHING LOOKS — ONE RULE for its readers:
//! atpkg's Rust build cache sweep (`atpkg::build_cache::reclaim`, which lets a gone
//! checkout's build dir go) and the window's disk watch (`aterm_agent`'s
//! `harness::disk::discover`, which never walks where [`never_looked_at`] says no
//! look goes). Std only, and nothing here names either of them.
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
//! * a checkout where volumes are mounted ([`VOLUME_ROOTS`]) is never looked
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
    /// The device of `$HOME`'s volume (`st_dev`).
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
#[cfg(not(target_os = "macos"))]
pub const HOME_SKIP: &[&str] = &["Library", "Applications"];

/// Where volumes are mounted, and come and go: a checkout under one is
/// never looked at ([`gone`]). On macOS a removable or network volume is
/// privacy-guarded too.
#[cfg(target_os = "macos")]
pub const VOLUME_ROOTS: &[&str] = &["/Volumes"];

/// See the macOS list: where removable volumes are mounted.
#[cfg(not(target_os = "macos"))]
pub const VOLUME_ROOTS: &[&str] = &["/media", "/run/media", "/mnt"];

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

/// The device of the entry at `p`, a link not followed.
fn device_of(p: &Path) -> std::io::Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::symlink_metadata(p).map(|m| m.dev())
    }
    #[cfg(not(unix))]
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
        .map_or(0, Iterator::count);
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
        .any(|m| m.as_path() != ancestor && m.starts_with(ancestor) && checkout.starts_with(m));
    (!mounted_between).then(|| {
        format!(
            "neither it nor the folder that held it is there, and {} — on the home volume, \
             with no volume mounted between — is: removed with the checkout it was in",
            ancestor.display()
        )
    })
}

/// The place `checkout` lies in that is never looked into, if it lies in
/// one: under a [`VOLUME_ROOTS`] entry, or in a [`HOME_SKIP`] folder of
/// `home`. Lexical — nothing is looked at — and, on macOS, blind to ASCII
/// case, as the file system is (`~/documents` is `~/Documents`). The disk
/// watch asks it of any path it would read from the window, a Claude Code
/// directory too.
#[must_use]
pub fn never_looked_at(checkout: &Path, home: Option<&Path>) -> Option<String> {
    if let Some(root) = VOLUME_ROOTS
        .iter()
        .find(|r| below(checkout, Path::new(r)).is_some())
    {
        return Some(format!("where volumes are mounted ({root})"));
    }
    let mut rest = below(checkout, home?)?;
    let Some(Component::Normal(first)) = rest.next() else {
        return None;
    };
    HOME_SKIP
        .iter()
        .find(|s| same_name(first, s))
        .map(|s| format!("in a folder of $HOME that is never looked into (~/{s})"))
}

/// The components of `path` past `root`, when `root` is a prefix of it
/// component by component ([`same_name`]).
fn below<'a>(path: &'a Path, root: &Path) -> Option<std::path::Components<'a>> {
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
    Some(rest)
}

/// One path component against a name: blind to ASCII case on macOS (its
/// file systems are, by default), exact elsewhere.
fn same_name(component: &std::ffi::OsStr, name: &str) -> bool {
    if cfg!(target_os = "macos") {
        component
            .to_str()
            .is_some_and(|c| c.eq_ignore_ascii_case(name))
    } else {
        component == name
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
        assert!(
            fs.gone(&format!("{}/x/ws", VOLUME_ROOTS[0]), Some(&placing(&["/"])))
                .is_err()
        );
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
}
