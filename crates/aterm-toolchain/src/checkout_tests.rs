// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    let between = aterm_sysprobe::paths::plain(&canonical.join("dev").join("gone-r3")).into_owned();
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
