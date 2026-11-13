// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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

    fn temp_root() -> crate::paths::ScratchDir {
        crate::paths::scratch_dir("which-copy")
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
        std::os::unix::fs::symlink(&outside, caskroom.join("0.56.0").join("aterm.app")).unwrap();
        assert_eq!(
            caskroom_bundles(&caskroom),
            vec![older.clone(), newer.clone()]
        );
        assert!(caskroom_bundles(&root.join("no-caskroom")).is_empty());
        let linked_caskroom = root.join("linked-caskroom");
        std::os::unix::fs::symlink(&caskroom, &linked_caskroom).unwrap();
        assert!(caskroom_bundles(&linked_caskroom).is_empty());
        let crowded = root.join("crowded");
        std::fs::create_dir_all(crowded.join(".metadata")).unwrap();
        for i in 0..(MAX_CASKROOM_ENTRIES + 4) {
            make_bundle(&crowded.join(format!("1.{i:03}.0")).join("aterm.app"), None);
        }
        assert_eq!(caskroom_bundles(&crowded).len(), MAX_CASKROOM_ENTRIES);

        let alab_cask = root
            .join("Caskroom")
            .join("alab")
            .join("0.62.0")
            .join("aterm.app");
        make_bundle(&alab_cask, Some(&version_plist("0.62.0")));
        let custom_install = root.join("custom-install");
        let custom_app = custom_install.join("aterm.app");
        make_bundle(&custom_app, Some(&version_plist("0.63.0")));

        let home = root.join("home");
        let places = app_locations(Some(&home), Some(&root), Some(&custom_install));
        assert_eq!(places[0], Path::new("/Applications/aterm.app"));
        assert_eq!(places[1], home.join("Applications").join("aterm.app"));
        assert_eq!(places[2], custom_app);
        assert!(
            places.contains(&older) && places.contains(&newer) && places.contains(&alab_cask),
            "{places:?}"
        );
        let running_exe = make_bundle(
            &root.join("Applications").join("aterm.app"),
            Some(&version_plist("0.64.0")),
        );
        let surveyed = survey(&running_exe, &places);
        assert!(
            surveyed.others.contains(&OtherCopy {
                path: custom_app,
                version: Some("0.63.0".to_string()),
            }),
            "{surveyed:?}"
        );
        assert!(
            surveyed.others.contains(&OtherCopy {
                path: alab_cask,
                version: Some("0.62.0".to_string()),
            }),
            "{surveyed:?}"
        );
        let defaults = app_locations(None, None, None);
        let repeated = app_locations(
            None,
            Some(Path::new("/opt/homebrew")),
            Some(Path::new("/Applications")),
        );
        let empty_install = app_locations(None, None, Some(Path::new("")));
        assert_eq!(defaults, repeated);
        assert_eq!(defaults, empty_install);
        let empty_home = app_locations(Some(Path::new("")), None, None);
        let relative_home = app_locations(Some(Path::new("relative-home")), None, None);
        assert_eq!(defaults, empty_home);
        assert_eq!(defaults, relative_home);
        let relative_install = app_locations(None, None, Some(Path::new("relative-install")));
        assert!(relative_install.contains(&PathBuf::from("relative-install/aterm.app")));
        assert_eq!(
            homebrew_prefixes(None),
            homebrew_prefixes(Some(Path::new("")))
        );
        assert_eq!(
            homebrew_prefixes(None),
            homebrew_prefixes(Some(Path::new("relative-brew")))
        );
        let repeated_home =
            app_locations(Some(&home), Some(&root), Some(&home.join("Applications")));
        let home_app = home.join("Applications").join("aterm.app");
        assert_eq!(
            repeated_home
                .iter()
                .filter(|path| path.as_path() == home_app.as_path())
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
