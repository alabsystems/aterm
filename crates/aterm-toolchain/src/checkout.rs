// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Checkout liveness and skipped-path rules for build cache reclamation and disk discovery.

use std::path::{Component, Path, PathBuf};

/// Home volume device and complete mount table for [`gone_placed`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placing {
    /// Home volume device identifier.
    pub home_dev: u64,
    /// Every mount point from a complete mount table.
    pub mounts: Vec<PathBuf>,
}

/// Workspace manifest filenames.
pub const MANIFESTS: &[&str] = &["Cargo.toml", "Targo.toml"];

/// Home subdirectories skipped by the disk watch and checkout liveness checks.
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

/// Home subdirectories skipped by the disk watch and checkout liveness checks.
#[cfg(not(any(target_os = "macos", windows)))]
pub const HOME_SKIP: &[&str] = &["Library", "Applications"];

/// Home subdirectories skipped by the disk watch and checkout liveness checks.
#[cfg(windows)]
pub const HOME_SKIP: &[&str] = &["AppData", "OneDrive*"];

/// Returns the matching [`HOME_SKIP`] entry for `name`, if any.
#[inline]
#[must_use]
pub fn home_skip(name: &std::ffi::OsStr) -> Option<&'static str> {
    HOME_SKIP.iter().copied().find(|s| same_name(name, s))
}

/// Mount root directories where removable volumes come and go.
#[cfg(target_os = "macos")]
pub const VOLUME_ROOTS: &[&str] = &["/Volumes"];

/// Mount root directories where removable volumes come and go.
#[cfg(not(any(target_os = "macos", windows)))]
pub const VOLUME_ROOTS: &[&str] = &["/media", "/run/media", "/mnt"];

/// Mount root directories where removable volumes come and go.
#[cfg(windows)]
pub const VOLUME_ROOTS: &[&str] = &[];

/// Checks whether `checkout` is seen to be gone and no longer a workspace.
///
/// # Errors
///
/// Why the checkout is not called gone: it is there, it was not looked at, or what
/// was looked at could not be read.
#[inline]
pub fn gone(checkout: &Path, home: Option<&Path>) -> Result<String, String> {
    gone_placed(checkout, home, None)
}

/// Like [`gone`], also treating a nested workspace as gone when [`Placing`] places its
/// nearest existing ancestor, not empty, on the home volume with no mount between.
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

const FINDER_LITTER: &[&str] = &[".DS_Store", ".localized"];

fn empty_folder(p: &Path) -> std::io::Result<bool> {
    for entry in std::fs::read_dir(p)? {
        let name = entry?.file_name();
        if !FINDER_LITTER.iter().any(|l| name == *l) {
            return Ok(false);
        }
    }
    Ok(true)
}

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

/// Like [`gone`], reading filesystem entries through `stat` without following a final link.
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

/// Like [`gone_placed`], reading entries through `stat`, devices through `dev`, and
/// folder emptiness through `empty`.
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

type Looks<'a> = (
    &'a mut dyn FnMut(&Path) -> std::io::Result<bool>,
    &'a mut dyn FnMut(&Path) -> std::io::Result<u64>,
    &'a mut dyn FnMut(&Path) -> std::io::Result<bool>,
);

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

/// Returns the reason `checkout` is never looked into, if it lies under [`VOLUME_ROOTS`]
/// or a [`HOME_SKIP`] folder of `home`.
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
#[cfg(windows)]
fn off_home_drive(checkout: &Path, home: Option<&Path>) -> Option<String> {
    let on_home_drive =
        home.is_some_and(|h| aterm_sysprobe::paths::windows_under(checkout, h).is_some());
    (!on_home_drive).then(|| {
        "where volumes come and go (a drive other than $HOME's, or no $HOME to name its drive)"
            .to_owned()
    })
}

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

fn same_name(component: &std::ffi::OsStr, name: &str) -> bool {
    #[cfg(windows)]
    {
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

    const ROOT: &str = if cfg!(windows) { "C:" } else { "" };

    fn at(p: &str) -> PathBuf {
        PathBuf::from(format!("{ROOT}{p}"))
    }

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
        if let Some(root) = VOLUME_ROOTS.first() {
            assert!(
                fs.gone(&format!("{root}/x/ws"), Some(&placing(&["/"])))
                    .is_err()
            );
        }
        assert!(fs.gone("/h/gone/a/ws", Some(&placing(&["/"]))).is_ok());
        assert!(
            fs.gone("/h/dev/ext/proj/ws", Some(&placing(&["/"])))
                .is_err_and(|e| e.contains("away")),
            "an empty mount-point folder keeps it"
        );
        assert!(fs.gone("/h/dev/proj/ws", Some(&placing(&["/"]))).is_ok());
        assert!(
            fs.gone("/h/dev/ws", None)
                .is_ok_and(|s| s.contains("there without it"))
        );
    }

    /// WINDOWS' FOLDERS NEVER LOOKED INTO: `AppData` and every `OneDrive*`, in
    /// any case NTFS folds alike, and a path spelled verbatim (`\\?\C:\…`, as
    /// `std::fs::canonicalize` answers) against a plain `$HOME` in another
    /// case; and any drive but `$HOME`'s.
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
    /// it verbatim.
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
        let plain = PathBuf::from(
            aterm_sysprobe::paths::plain(&canonical)
                .to_string_lossy()
                .to_lowercase(),
        );
        let ws = canonical.join("dev").join("gone-r3").join("ws");
        let placing = |mounts: Vec<PathBuf>, home_dev: u64| Placing { home_dev, mounts };
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
