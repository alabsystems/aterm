// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Which copy of aterm ITSELF is running — story S12 of
//! `docs/DESIGN-which-copy-runs-2026-08-27.md`.
//!
//! `aterm.app` has the same three sources every provisioned program has: a DMG
//! drag-install, the Homebrew cask (`auto_updates true`: brew never upgrades it), or
//! `tools/install.sh`. The updater updates the RUNNING bundle's location only; a
//! second copy elsewhere (`~/Applications`, a dev `dist/aterm.app`, a Caskroom
//! leftover) is not its owner and is left alone (`install::trial_owned_by`). So
//! `aterm --version` and Settings ▸ About name the path of the running copy and, when
//! another `aterm.app` sits in one of the usual places, say so in ONE sentence — the
//! same words on both surfaces, spelled here and nowhere else:
//!
//! ```text
//! running: /Applications/aterm.app
//! another copy: /Users//ana/Applications/aterm.app (0.60.0) — not the one running; the updater updates only this one
//! ```
//!
//! Reuse, not reimplementation: the running bundle is [`crate::bundle::layout_of`]
//! (the shape) and [`crate::bundle::resolve_from`] + the dev mark (whether the
//! updater owns it); "is that candidate the same install?" is the updater's own
//! [`crate::install::same_install_root`] — the duplicate-copy identity its trial
//! ownership rule uses — and the other copy's version is
//! [`crate::manifest::xml_plist_string`] over the bounded `read_ledger_text` reader.
//! Nothing here spawns a helper: a version read is one small file.

use std::path::{Path, PathBuf};

/// WHY a running copy is inert, not merely THAT it is. [`Running::InertApp`]
/// deliberately collapses three situations the updater treats identically — a
/// dev-marked build, a launch from a mounted disk image, and an App-Translocated
/// launch — because the updater's answer to all three is the same: do not touch
/// it. A person's is not. Two of the three are a mistake they can undo in one
/// drag, and the third is not a fault at all.
///
/// Re-exported here rather than reimplemented: the classification is the pure
/// path logic that sits beside [`crate::bundle::resolve_from`], so the reason
/// shown to a human and the refusal acted on by the updater are the same
/// judgement and cannot drift apart.
/// macOS-only, like the module it comes from: `bundle` is `#[cfg(target_os =
/// "macos")]` because the whole notion of an `.app` that can be translocated is,
/// and every other `crate::bundle` use in this file is gated the same way.
#[cfg(target_os = "macos")]
pub use crate::bundle::{InstallPosture, posture_from};

/// What the running copy IS — decides whether the updater's promise ("the updater
/// updates only this one") may be made at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Running {
    /// An installed `.app` the updater may replace (`bundle::resolve` semantics).
    InstalledApp,
    /// An `.app` the updater never touches: dev-marked (`ATermDevBuild`), or launched
    /// from a mounted disk image / App Translocation.
    InertApp,
    /// Not a bundle: a bare executable — every Linux/Windows launch, and a macOS
    /// `cargo run` / `target/release` / store binary.
    Binary,
}

/// One `aterm.app` found at a usual install location that is NOT the running one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtherCopy {
    /// The `.app` root.
    pub path: PathBuf,
    /// Its `CFBundleShortVersionString`; `None` when the plist is missing, binary, or
    /// lacks the key (the row then says `version unknown` rather than guessing).
    pub version: Option<String>,
}

/// The answer: the running copy, what kind of thing it is, and the other copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhichCopy {
    /// macOS `.app` root containing the executable; otherwise the executable path.
    pub running: PathBuf,
    /// See [`Running`].
    pub kind: Running,
    /// Other `aterm.app` bundles in the usual places, in probe order.
    pub others: Vec<OtherCopy>,
}

impl WhichCopy {
    /// The running copy's row value: the path — plus, for a bundle the updater never
    /// touches, the reason the "updates only this one" promise is absent.
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

    /// One other copy's row value:
    /// `<path> (<version>) — not the one running; the updater updates only this one`.
    /// The updater clause is made only when the running copy is an installed bundle
    /// the updater owns — from a bare binary or an inert bundle it updates neither.
    #[must_use]
    pub fn other_detail(&self, other: &OtherCopy) -> String {
        let version = other.version.as_deref().unwrap_or("version unknown");
        let mut detail = format!(
            "{} ({version}) \u{2014} not the one running",
            other.path.display()
        );
        if self.kind == Running::InstalledApp {
            detail.push_str("; the updater updates only this one");
        }
        detail
    }

    /// The report as printed lines: `running: …`, then `another copy: …` per other
    /// copy. `aterm --version` prints these after its identity line; About shows the
    /// same values under the `running` / `another copy` rows.
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

/// Observe THIS process: the copy it runs from and, on macOS, every other `aterm.app`
/// in the usual places. `None` only when the executable path itself is unknown.
#[must_use]
pub fn observe() -> Option<WhichCopy> {
    let exe = std::env::current_exe().ok()?;
    // Deliberate symlink-shim resolution, exactly as `bundle::resolve_layout` does: a
    // `~/.local/bin/aterm` launcher names the bundle (or store binary) it points into,
    // not itself. Unix only: on Windows `canonicalize` answers a verbatim `\\?\C:\…`
    // spelling nobody types, and there is no shim to see through.
    #[cfg(unix)]
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    Some(observe_from(&exe))
}

#[cfg(target_os = "macos")]
fn observe_from(exe: &Path) -> WhichCopy {
    survey(exe, &usual_app_locations())
}

/// Off macOS there is no bundle and no updater lane: the executable path is the
/// whole answer.
#[cfg(not(target_os = "macos"))]
fn observe_from(exe: &Path) -> WhichCopy {
    WhichCopy {
        running: exe.to_path_buf(),
        kind: Running::Binary,
        others: Vec::new(),
    }
}

/// Pure core of [`observe`]: classify `exe` and probe `candidates` (`.app` roots).
/// A candidate that does not exist, is not a REAL directory (a symlink planted at a
/// usual place is never followed out of it — not read, not reported; so a symlinked
/// spelling of the running bundle is not a second copy either), resolves to the
/// running bundle (`same_install_root`), or duplicates an earlier candidate is
/// skipped.
#[cfg(target_os = "macos")]
#[must_use]
pub fn survey(exe: &Path, candidates: &[PathBuf]) -> WhichCopy {
    let (running, kind) = match crate::bundle::layout_of(exe) {
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
    };
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

/// A directory that is really there — not a symlink to one. Every probe of another
/// copy goes through this, so the report never follows a link planted in
/// `~/Applications` or a Caskroom out to somewhere else on the disk.
#[cfg(target_os = "macos")]
fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir())
}

/// The most Caskroom version directories the report will look inside. A Caskroom
/// with more than this many `aterm` versions is not a shape `brew` produces; the
/// report names the first of them, sorted, and stops.
#[cfg(target_os = "macos")]
const MAX_CASKROOM_ENTRIES: usize = 16;

/// The longest version string another copy's plist may contribute, and the only
/// characters it may carry — a version is `MAJOR.MINOR.PATCH` with at most a
/// pre-release or build tag, so anything else (an escape sequence, a paragraph) is
/// not a version and reads as unknown rather than reaching the terminal.
#[cfg(any(target_os = "macos", test))]
const MAX_VERSION_CHARS: usize = 32;

/// `version` if it is shaped like one ([`MAX_VERSION_CHARS`] of `[0-9A-Za-z.+-]`),
/// else `None`. Pure; every platform's tests pin the rule.
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

/// `CFBundleShortVersionString` off a bundle's XML `Info.plist` (release bundles are
/// emitted as XML; anything else reads as unknown). Bounded read through the ledger
/// reader (a plist over its cap is unknown, never unbounded work), no helper spawned,
/// and the value is admitted only when it is shaped like a version
/// ([`plausible_version`]) — the report prints it into a terminal.
#[cfg(target_os = "macos")]
fn plist_short_version(app_root: &Path) -> Option<String> {
    let text = crate::read_ledger_text(&app_root.join("Contents/Info.plist"))?;
    crate::manifest::xml_plist_string(&text, "<key>CFBundleShortVersionString</key>")
        .and_then(plausible_version)
        .map(str::to_string)
}

/// The usual places an `aterm.app` lands: `/Applications`, `~/Applications`, and the
/// Homebrew Caskroom (`brew install --cask alabsystems/tap/aterm` stages
/// `<prefix>/Caskroom/aterm/<version>/aterm.app` before moving it; a leftover there
/// is a copy). `HOME` and `HOMEBREW_PREFIX` come from the environment, exactly as
/// the staging root's own derivation reads `HOME`.
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
    // Apple silicon, Intel, then a configured prefix that is neither.
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

/// `<caskroom>/<version>/aterm.app` for every version directory present — real
/// directories only, never a symlink out of the Caskroom, at most
/// [`MAX_CASKROOM_ENTRIES`] of them — sorted so the report is stable. A missing
/// Caskroom is simply no copies.
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

    fn copy(kind: Running, others: Vec<OtherCopy>) -> WhichCopy {
        WhichCopy {
            running: PathBuf::from("/Applications/aterm.app"),
            kind,
            others,
        }
    }

    fn other(path: &str, version: Option<&str>) -> OtherCopy {
        OtherCopy {
            path: PathBuf::from(path),
            version: version.map(str::to_string),
        }
    }

    /// THE sentences, spelled exactly — `aterm --version` and About both print these.
    #[test]
    fn the_report_is_spelled_exactly() {
        let installed = copy(
            Running::InstalledApp,
            vec![
                other("/Users//ana/Applications/aterm.app", Some("0.60.0")),
                other("/opt/homebrew/Caskroom/aterm/0.59.0/aterm.app", None),
            ],
        );
        assert_eq!(
            installed.lines(),
            vec![
                "running: /Applications/aterm.app".to_string(),
                "another copy: /Users//ana/Applications/aterm.app (0.60.0) \u{2014} not the one \
                 running; the updater updates only this one"
                    .to_string(),
                "another copy: /opt/homebrew/Caskroom/aterm/0.59.0/aterm.app (version unknown) \
                 \u{2014} not the one running; the updater updates only this one"
                    .to_string(),
            ]
        );
        // Alone: the one line.
        assert_eq!(
            copy(Running::InstalledApp, Vec::new()).lines(),
            vec!["running: /Applications/aterm.app".to_string()]
        );
    }

    /// The updater clause is a PROMISE about the running copy, so it is made only
    /// when the updater owns it: a bare binary or an inert bundle updates neither.
    #[test]
    fn the_updater_clause_is_made_only_for_an_installed_bundle() {
        let others = vec![other("/Users//ana/Applications/aterm.app", Some("0.60.0"))];
        let binary = WhichCopy {
            running: PathBuf::from("/Users//ana/aterm/target/release/aterm"),
            kind: Running::Binary,
            others: others.clone(),
        };
        assert_eq!(
            binary.lines(),
            vec![
                "running: /Users//ana/aterm/target/release/aterm".to_string(),
                "another copy: /Users//ana/Applications/aterm.app (0.60.0) \u{2014} not the one \
                 running"
                    .to_string(),
            ]
        );
        let inert = WhichCopy {
            running: PathBuf::from("/Volumes/aterm/aterm.app"),
            kind: Running::InertApp,
            others,
        };
        assert_eq!(
            inert.lines(),
            vec![
                "running: /Volumes/aterm/aterm.app (the updater leaves this copy alone)"
                    .to_string(),
                "another copy: /Users//ana/Applications/aterm.app (0.60.0) \u{2014} not the one \
                 running"
                    .to_string(),
            ]
        );
    }

    /// Another copy's version reaches the terminal, so only a version-shaped value
    /// is admitted; anything else reads as unknown.
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

    /// Every platform answers with at least the executable path.
    #[test]
    fn this_process_names_its_own_executable() {
        let report = observe().expect("current_exe resolves under test");
        assert!(report.running.is_absolute(), "{report:?}");
        let exe = std::env::current_exe().unwrap();
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        // A test binary is never a `.app`, so the running path IS the executable.
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

        /// A fake bundle: the `Contents/MacOS/aterm` layout, plus an XML plist
        /// when `plist_body` is given.
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

        /// The running bundle is named by its `.app` root; every other candidate that
        /// exists is listed with the version its own plist carries — or `None` when
        /// it carries none — in probe order; a candidate that is missing, a plain
        /// file, or a symlink to the running bundle is not a second copy.
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
            // A symlink planted at a usual place pointing at a REAL other bundle is
            // never followed: not read, not reported.
            let elsewhere = root.join("elsewhere").join("aterm.app");
            make_bundle(&elsewhere, Some(&version_plist("0.1.0")));
            let planted = root.join("planted.app");
            std::os::unix::fs::symlink(&elsewhere, &planted).unwrap();
            // A version that is not shaped like one reads as unknown.
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
                    // The same install spelled twice is one copy.
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
            assert_eq!(
                report.lines(),
                vec![
                    format!("running: {}", running_app.display()),
                    format!(
                        "another copy: {} (0.60.0) \u{2014} not the one running; the updater \
                         updates only this one",
                        home_app.display()
                    ),
                    format!(
                        "another copy: {} (version unknown) \u{2014} not the one running; the \
                         updater updates only this one",
                        cask_app.display()
                    ),
                    format!(
                        "another copy: {} (version unknown) \u{2014} not the one running; the \
                         updater updates only this one",
                        odd.display()
                    ),
                ]
            );
            let _ = std::fs::remove_dir_all(root);
        }

        /// A bare binary is named by its own path and makes no updater promise; the
        /// installed copies beside it are still reported.
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
            assert_eq!(
                report.lines(),
                vec![
                    format!("running: {}", exe.display()),
                    format!(
                        "another copy: {} (0.60.0) \u{2014} not the one running",
                        installed.display()
                    ),
                ]
            );
            let _ = std::fs::remove_dir_all(root);
        }

        /// A dev-marked bundle and a mounted-image launch are bundles the updater
        /// never touches: named, and honest about it.
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
            assert_eq!(
                report.running_detail(),
                format!("{} (the updater leaves this copy alone)", dev_app.display())
            );
            // The shape alone classifies a DMG launch; no filesystem needed.
            let mounted = survey(
                Path::new("/Volumes/aterm/aterm.app/Contents/MacOS/aterm"),
                &[],
            );
            assert_eq!(mounted.running, Path::new("/Volumes/aterm/aterm.app"));
            assert_eq!(mounted.kind, Running::InertApp);
            let _ = std::fs::remove_dir_all(root);
        }

        /// The Caskroom contributes one candidate per versioned bundle, sorted; the
        /// fixed places come first, and a configured prefix that repeats a default
        /// is not probed twice.
        #[test]
        fn the_usual_places_include_every_caskroom_bundle_once() {
            let root = temp_root();
            let caskroom = root.join("Caskroom").join("aterm");
            let newer = caskroom.join("0.61.0").join("aterm.app");
            let older = caskroom.join("0.60.0").join("aterm.app");
            make_bundle(&newer, None);
            make_bundle(&older, None);
            // A version directory with no bundle inside contributes nothing.
            std::fs::create_dir_all(caskroom.join("0.58.0")).unwrap();
            // A symlinked version directory, or a symlinked bundle inside a real one,
            // is never followed out of the Caskroom.
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
            // A Caskroom that is itself a symlink is not probed.
            let linked_caskroom = root.join("linked-caskroom");
            std::os::unix::fs::symlink(&caskroom, &linked_caskroom).unwrap();
            assert!(caskroom_bundles(&linked_caskroom).is_empty());
            // The walk is bounded: past the cap, the rest is not looked at.
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
            // A default prefix named again as HOMEBREW_PREFIX is one prefix.
            let defaults = app_locations(None, None);
            let repeated = app_locations(None, Some(Path::new("/opt/homebrew")));
            assert_eq!(defaults, repeated);
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
