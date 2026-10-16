// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHEN A CELLS CACHE'S CHECKOUT IS GONE — ONE RULE, shared as SOURCE by
//! its readers: the window's disk watch (`aterm_agent`'s
//! `harness::disk::discover`, which below the floor deletes a gone
//! checkout's cache whole, unattended) and atpkg's Rust build cache sweep
//! (`build_cache::reclaim`), which include this file by path — the same rule
//! with no dependency edge between the crates. Std only, and nothing here
//! names any of them. The cells cache (`~/.cache/aterm/cells`) is a layout
//! nothing writes any more (`verify cells` builds under its run's target
//! dir); the rule reclaims what machines still hold of it.
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
//! Reading any error on `Cargo.toml` as "removed" would delete the cells
//! cache (5-46 GB, a full rebuild) of a checkout behind `chmod 000`, on an
//! unmounted volume or at a volume's root. A checkout removed together with
//! the folder that held it keeps its cache; that is the safe direction.

use std::path::{Component, Path};

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

/// The checkout a cells cache's `.checkout` marker names, read by BOTH
/// sweeps from its bytes EXACTLY as the cells cache was written: the
/// checkout's canonical path, nothing around it. Never trimmed and never
/// decoded lossily — either turns a live checkout's path (a folder whose
/// name ends in a space or U+00A0; on Linux, a name that is not UTF-8) into
/// a sibling's that is missing beside the folder holding both, which
/// [`gone`] then SEES gone. A marker ending in a line break names no
/// checkout: none was written so, and read either way it could name a
/// missing sibling (`echo`'s `proj\n` beside `proj`) — such a cache is kept
/// by both sweeps, as one with no marker is. Nor does one that is not an
/// absolute path.
#[must_use]
pub fn marker_checkout(bytes: &[u8]) -> Option<std::path::PathBuf> {
    if bytes.last().is_some_and(|b| matches!(b, b'\n' | b'\r')) {
        return None;
    }
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStrExt as _;
        std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    };
    #[cfg(not(unix))]
    let path = std::path::PathBuf::from(std::str::from_utf8(bytes).ok()?);
    path.is_absolute().then_some(path)
}

/// Is the checkout at `checkout` SEEN to be gone — no longer a workspace?
/// `Ok` with the sight, `Err` with why it is not called gone. `home` is the
/// `$HOME` whose guarded folders ([`HOME_SKIP`]) are never looked into.
///
/// # Errors
///
/// Why the checkout is not called gone: it is there, it was not looked at,
/// or what was looked at could not be read.
pub fn gone(checkout: &Path, home: Option<&Path>) -> Result<String, String> {
    gone_by(checkout, home, &mut |p| {
        std::fs::symlink_metadata(p).map(|m| m.is_dir())
    })
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
    for folder in way {
        match stat(folder) {
            Ok(true) => {}
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
        Err(e) => Err(format!(
            "neither its checkout {shown} nor the folder that held it is there ({e}) — away \
             (a volume not mounted now), not seen removed"
        )),
    }
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
