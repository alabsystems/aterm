// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Identifies the running copy of aterm and any other copies in usual install locations.

use std::path::{Path, PathBuf};

/// Why a running copy is inert ([`Running::InertApp`]).
pub use crate::install_posture::InstallPosture;

/// The macOS classification, shared with the updater's bundle admission.
#[cfg(target_os = "macos")]
pub use crate::bundle::posture_from;

/// The running copy's [`InstallPosture`] ([`crate::bundle::resolve_layout`]).
#[cfg(target_os = "macos")]
#[must_use]
pub fn running_posture() -> Option<InstallPosture> {
    let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    Some(posture_from(&exe))
}

/// What the running copy is — decides whether the other-copy clause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Running {
    /// An installed `.app` the updater may replace (`bundle::resolve` semantics).
    InstalledApp,
    /// An `.app` the updater never touches.
    InertApp,
    /// Not a bundle: a bare executable.
    Binary,
}

/// One `aterm.app` found at a usual install location that is not the running one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtherCopy {
    /// The `.app` root.
    pub path: PathBuf,
    /// Its `CFBundleShortVersionString`.
    pub version: Option<String>,
}

/// The answer: the running copy, what kind of thing it is, and the other copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhichCopy {
    /// macOS `.app` root containing the executable.
    pub running: PathBuf,
    /// See [`Running`].
    pub kind: Running,
    /// Other `aterm.app` bundles in the usual places, in probe order.
    pub others: Vec<OtherCopy>,
}

impl WhichCopy {
    /// The running copy's row value.
    #[must_use]
    pub fn running_detail(&self) -> String {
        match self.kind {
            Running::InertApp => format!(
                "{} (the updater leaves this copy alone)",
                self.running.display()
            ),
            Running::InstalledApp | Running::Binary => self.running.display().to_string(),
        }
    }

    /// One other copy's row value.
    #[must_use]
    pub fn other_detail(&self, other: &OtherCopy) -> String {
        let version = other.version.as_deref().unwrap_or("version unknown");
        let mut detail = format!("{} ({version})", other.path.display());
        if self.kind == Running::InstalledApp {
            detail.push_str(" \u{2014} the updater leaves it alone");
        }
        detail
    }

    /// The report as printed lines.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![format!("running: {}", self.running_detail())];
        out.extend(
            self.others
                .iter()
                .map(|other| format!("another copy: {}", self.other_detail(other))),
        );
        out
    }
}

/// Observe this process.
#[must_use]
pub fn observe() -> Option<WhichCopy> {
    let exe = std::env::current_exe().ok()?;
    #[cfg(unix)]
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    Some(observe_from(&exe))
}

#[cfg(target_os = "macos")]
fn observe_from(exe: &Path) -> WhichCopy {
    survey(exe, &usual_app_locations())
}

/// [`observe`]'s `kind` alone, without probing for other copies ([`Running::Binary`]).
#[must_use]
pub fn running_kind() -> Running {
    let Ok(exe) = std::env::current_exe() else {
        return Running::Binary;
    };
    #[cfg(unix)]
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    kind_of(&exe)
}

/// [`running_kind`]'s pure core, for a given executable path.
#[must_use]
pub fn kind_of(exe: &Path) -> Running {
    #[cfg(target_os = "macos")]
    {
        classify(exe).1
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = exe;
        Running::Binary
    }
}

/// The running copy's root and kind.
#[cfg(target_os = "macos")]
fn classify(exe: &Path) -> (PathBuf, Running) {
    match crate::bundle::layout_of(exe) {
        Some(bundle) => {
            let owned = crate::bundle::resolve_from(exe)
                .is_some_and(|b| !crate::bundle::is_dev_marked(&b.app_root));
            (
                bundle.app_root,
                if owned {
                    Running::InstalledApp
                } else {
                    Running::InertApp
                },
            )
        }
        None => (exe.to_path_buf(), Running::Binary),
    }
}

/// Off macOS there is no bundle and no updater lane.
#[cfg(not(target_os = "macos"))]
fn observe_from(exe: &Path) -> WhichCopy {
    WhichCopy {
        running: exe.to_path_buf(),
        kind: Running::Binary,
        others: Vec::new(),
    }
}

/// Pure core of [`observe`].
#[cfg(target_os = "macos")]
#[must_use]
pub fn survey(exe: &Path, candidates: &[PathBuf]) -> WhichCopy {
    let (running, kind) = classify(exe);
    let mut others: Vec<OtherCopy> = Vec::new();
    for candidate in candidates {
        if !is_real_dir(candidate) {
            continue;
        }
        if crate::install::same_install_root(candidate, &running)
            || others
                .iter()
                .any(|other| crate::install::same_install_root(candidate, &other.path))
        {
            continue;
        }
        others.push(OtherCopy {
            path: candidate.clone(),
            version: plist_short_version(candidate),
        });
    }
    WhichCopy {
        running,
        kind,
        others,
    }
}

/// A directory that is really there, not a symlink to one.
#[cfg(target_os = "macos")]
fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir())
}

/// The most Caskroom version directories the report looks inside.
#[cfg(target_os = "macos")]
const MAX_CASKROOM_ENTRIES: usize = 16;

/// The longest version string another copy's plist may contribute.
#[cfg(any(target_os = "macos", test))]
const MAX_VERSION_CHARS: usize = 32;

#[cfg(any(target_os = "macos", test))]
#[must_use]
fn plausible_version(version: &str) -> Option<&str> {
    let ok = !version.is_empty()
        && version.len() <= MAX_VERSION_CHARS
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'));
    ok.then_some(version)
}

/// `CFBundleShortVersionString` from a bundle's XML `Info.plist` ([`plausible_version`]).
#[cfg(target_os = "macos")]
fn plist_short_version(app_root: &Path) -> Option<String> {
    let text = crate::read_ledger_text(&app_root.join("Contents/Info.plist"))?;
    crate::manifest::xml_plist_string(&text, "<key>CFBundleShortVersionString</key>")
        .and_then(plausible_version)
        .map(str::to_string)
}

/// The usual places an `aterm.app` lands.
#[cfg(target_os = "macos")]
#[must_use]
pub fn usual_app_locations() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let brew = std::env::var_os("HOMEBREW_PREFIX").map(PathBuf::from);
    app_locations(home.as_deref(), brew.as_deref())
}

#[cfg(target_os = "macos")]
fn app_locations(home: Option<&Path>, homebrew_prefix: Option<&Path>) -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from("/Applications/aterm.app")];
    if let Some(home) = home {
        out.push(home.join("Applications").join("aterm.app"));
    }
    let mut prefixes = vec![PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")];
    if let Some(prefix) = homebrew_prefix
        && !prefixes.iter().any(|p| p == prefix)
    {
        prefixes.push(prefix.to_path_buf());
    }
    for prefix in prefixes {
        out.extend(caskroom_bundles(&prefix.join("Caskroom").join("aterm")));
    }
    out
}

/// `<caskroom>/<version>/aterm.app` for each version directory present ([`MAX_CASKROOM_ENTRIES`]).
#[cfg(target_os = "macos")]
fn caskroom_bundles(caskroom: &Path) -> Vec<PathBuf> {
    if !is_real_dir(caskroom) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(caskroom) else {
        return Vec::new();
    };
    let mut versions: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .map(|entry| entry.path())
        .collect();
    versions.sort();
    versions.truncate(MAX_CASKROOM_ENTRIES);
    versions
        .into_iter()
        .map(|version| version.join("aterm.app"))
        .filter(|app| is_real_dir(app))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_version_shaped_string_is_admitted() {
        assert_eq!(plausible_version("0.60.0"), Some("0.60.0"));
        assert_eq!(
            plausible_version("0.61.0-rc.1+b1787"),
            Some("0.61.0-rc.1+b1787")
        );
        assert_eq!(plausible_version(""), None);
        assert_eq!(plausible_version("0.60.0\u{1b}[31m"), None);
        assert_eq!(plausible_version("0.60.0 (dev)"), None);
        assert!(plausible_version(&"9".repeat(MAX_VERSION_CHARS)).is_some());
        assert_eq!(plausible_version(&"9".repeat(MAX_VERSION_CHARS + 1)), None);
    }

    #[test]
    fn this_process_names_its_own_executable() {
        let report = observe().expect("current_exe resolves under test");
        assert!(report.running.is_absolute(), "{report:?}");
        let exe = std::env::current_exe().unwrap();
        #[cfg(unix)]
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        assert_eq!(report.kind, Running::Binary);
        assert_eq!(report.running, exe);
    }

    #[cfg(target_os = "macos")]
    mod survey {
        use super::super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static SEQ: AtomicUsize = AtomicUsize::new(0);

        fn temp_root() -> PathBuf {
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("aterm-which-copy-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            root
        }

        fn make_bundle(app: &Path, plist_body: Option<&str>) -> PathBuf {
            let macos = app.join("Contents").join("MacOS");
            std::fs::create_dir_all(&macos).unwrap();
            let exe = macos.join("aterm");
            std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
            if let Some(body) = plist_body {
                std::fs::write(
                    app.join("Contents").join("Info.plist"),
                    format!(
                        "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>{body}</dict></plist>"
                    ),
                )
                .unwrap();
            }
            exe
        }

        fn version_plist(version: &str) -> String {
            format!(
                "<key>CFBundleVersion</key><string>1700000000</string>\
                 <key>CFBundleShortVersionString</key><string>{version}</string>"
            )
        }

        #[test]
        fn the_running_bundle_is_named_and_the_other_copies_carry_their_versions() {
            let root = temp_root();
            let running_app = root.join("Applications").join("aterm.app");
            let running_exe = make_bundle(&running_app, Some(&version_plist("0.61.0")));
            let home_app = root.join("home").join("Applications").join("aterm.app");
            make_bundle(&home_app, Some(&version_plist("0.60.0")));
            let cask_app = root
                .join("Caskroom")
                .join("aterm")
                .join("0.59.0")
                .join("aterm.app");
            make_bundle(&cask_app, None);
            let missing = root.join("nowhere").join("aterm.app");
            let file = root.join("file.app");
            std::fs::write(&file, b"not a bundle").unwrap();
            let link = root.join("link.app");
            std::os::unix::fs::symlink(&running_app, &link).unwrap();
            let elsewhere = root.join("elsewhere").join("aterm.app");
            make_bundle(&elsewhere, Some(&version_plist("0.1.0")));
            let planted = root.join("planted.app");
            std::os::unix::fs::symlink(&elsewhere, &planted).unwrap();
            let odd = root.join("odd").join("aterm.app");
            make_bundle(&odd, Some(&version_plist("0.60.0\u{1b}[31m")));

            let report = survey(
                &running_exe,
                &[
                    link.clone(),
                    planted,
                    missing,
                    file,
                    home_app.clone(),
                    cask_app.clone(),
                    home_app.clone(),
                    odd.clone(),
                ],
            );
            assert_eq!(report.running, running_app);
            assert_eq!(report.kind, Running::InstalledApp);
            assert_eq!(
                report.others,
                vec![
                    OtherCopy {
                        path: home_app.clone(),
                        version: Some("0.60.0".to_string()),
                    },
                    OtherCopy {
                        path: cask_app.clone(),
                        version: None,
                    },
                    OtherCopy {
                        path: odd.clone(),
                        version: None,
                    },
                ]
            );
            assert_eq!(report.lines().len(), 4, "the running copy, then each other");
            let _ = std::fs::remove_dir_all(root);
        }

        #[test]
        fn a_bare_binary_names_its_path_and_makes_no_updater_promise() {
            let root = temp_root();
            let exe = root.join("target").join("release").join("aterm");
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
            let installed = root.join("Applications").join("aterm.app");
            make_bundle(&installed, Some(&version_plist("0.60.0")));

            let report = survey(&exe, std::slice::from_ref(&installed));
            assert_eq!(report.running, exe);
            assert_eq!(report.kind, Running::Binary);
            let lines = report.lines();
            assert_eq!(lines.len(), 2, "{lines:?}");
            assert!(
                lines.iter().all(|line| !line.contains("updater")),
                "{lines:?}"
            );
            let _ = std::fs::remove_dir_all(root);
        }

        #[test]
        fn an_inert_bundle_says_the_updater_leaves_it_alone() {
            let root = temp_root();
            let dev_app = root.join("dist").join("aterm.app");
            let dev_exe = make_bundle(
                &dev_app,
                Some(&format!(
                    "{}<key>ATermDevBuild</key><string>true</string>",
                    version_plist("0.61.0")
                )),
            );
            let report = survey(&dev_exe, &[]);
            assert_eq!(report.running, dev_app);
            assert_eq!(report.kind, Running::InertApp);
            let mounted = survey(
                Path::new("/Volumes/aterm/aterm.app/Contents/MacOS/aterm"),
                &[],
            );
            assert_eq!(mounted.running, Path::new("/Volumes/aterm/aterm.app"));
            assert_eq!(mounted.kind, Running::InertApp);
            let _ = std::fs::remove_dir_all(root);
        }

        #[test]
        fn the_usual_places_include_every_caskroom_bundle_once() {
            let root = temp_root();
            let caskroom = root.join("Caskroom").join("aterm");
            let newer = caskroom.join("0.61.0").join("aterm.app");
            let older = caskroom.join("0.60.0").join("aterm.app");
            make_bundle(&newer, None);
            make_bundle(&older, None);
            std::fs::create_dir_all(caskroom.join("0.58.0")).unwrap();
            let outside = root.join("outside").join("aterm.app");
            make_bundle(&outside, None);
            std::os::unix::fs::symlink(outside.parent().unwrap(), caskroom.join("0.57.0")).unwrap();
            std::fs::create_dir_all(caskroom.join("0.56.0")).unwrap();
            std::os::unix::fs::symlink(&outside, caskroom.join("0.56.0").join("aterm.app"))
                .unwrap();
            assert_eq!(
                caskroom_bundles(&caskroom),
                vec![older.clone(), newer.clone()]
            );
            assert!(caskroom_bundles(&root.join("no-caskroom")).is_empty());
            let linked_caskroom = root.join("linked-caskroom");
            std::os::unix::fs::symlink(&caskroom, &linked_caskroom).unwrap();
            assert!(caskroom_bundles(&linked_caskroom).is_empty());
            let crowded = root.join("crowded");
            for i in 0..(MAX_CASKROOM_ENTRIES + 4) {
                make_bundle(&crowded.join(format!("1.{i:03}.0")).join("aterm.app"), None);
            }
            assert_eq!(caskroom_bundles(&crowded).len(), MAX_CASKROOM_ENTRIES);

            let home = root.join("home");
            let places = app_locations(Some(&home), Some(&root));
            assert_eq!(places[0], Path::new("/Applications/aterm.app"));
            assert_eq!(places[1], home.join("Applications").join("aterm.app"));
            assert!(
                places.contains(&older) && places.contains(&newer),
                "{places:?}"
            );
            let defaults = app_locations(None, None);
            let repeated = app_locations(None, Some(Path::new("/opt/homebrew")));
            assert_eq!(defaults, repeated);
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
