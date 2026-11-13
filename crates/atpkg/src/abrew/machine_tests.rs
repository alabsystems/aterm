// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn bottle_tags_never_name_a_newer_os_or_another_architecture() {
    let tags = Machine::mac("arm64", "14.6", None, false).bottle_tags();
    assert_eq!(
        tags,
        [
            "arm64_sonoma",
            "arm64_ventura",
            "arm64_monterey",
            "arm64_big_sur"
        ]
    );
    assert_eq!(
        Machine::mac("x86_64", "14.6", None, false).bottle_tags()[0],
        "sonoma"
    );
    assert!(
        Machine::mac("arm64", "10.15", None, false)
            .bottle_tags()
            .is_empty()
    );
    assert!(
        Machine::mac("mips", "26.0", None, false)
            .bottle_tags()
            .is_empty()
    );
    assert!(
        Machine::mac("arm64", "next", None, false)
            .bottle_tags()
            .is_empty()
    );
}

#[test]
fn bottle_tag_symbols_follow_brew_tag() {
    assert_eq!(bottle_tag_symbol("sequoia", "intel"), "sequoia");
    assert_eq!(bottle_tag_symbol("sequoia", "arm"), "arm64_sequoia");
    assert_eq!(bottle_tag_symbol("linux", "intel"), "x86_64_linux");
    assert_eq!(bottle_tag_symbol("macos", "arm"), "arm64_macos");
    assert_eq!(bottle_tag_symbol("all", "all"), "all");
    assert_eq!(
        bottle_tag_from_symbol("bogus").as_deref(),
        Some("x86_64_bogus")
    );
    assert_eq!(
        bottle_tag_from_symbol("arm64_tahoe").as_deref(),
        Some("arm64_tahoe")
    );
    assert_eq!(bottle_tag_from_symbol("tahoe").as_deref(), Some("tahoe"));
    assert_eq!(bottle_tag_from_symbol("all").as_deref(), Some("all"));
    assert_eq!(bottle_tag_from_symbol("foo-bar"), None);
    assert_eq!(
        simulated_bottle_tag(Some("linux"), Some("intel")),
        "x86_64_linux"
    );
    assert_eq!(bottle_tag_combinations(Some("all"), Some("intel")).len(), 8);
    assert_eq!(bottle_tag_combinations(None, Some("all")).len(), 2);
}

#[test]
fn simulated_machines_carry_brew_variation_keys() {
    assert_eq!(
        Machine::simulated(Some("linux"), Some("arm")).variation_keys(),
        Some(vec!["arm64_linux".to_string()])
    );
    assert_eq!(
        Machine::simulated(Some("linux"), Some("intel")).variation_keys(),
        Some(vec!["x86_64_linux".to_string()])
    );
    assert_eq!(
        Machine::simulated(Some("sequoia"), Some("intel")).variation_keys(),
        Some(vec!["x86_64_sequoia".to_string(), "sequoia".to_string()])
    );
    // Generic macos has no release, hence no variation keys
    // and the base API record applies.
    assert_eq!(
        Machine::simulated(Some("macos"), Some("arm")).variation_keys(),
        None
    );
}

#[test]
fn variation_keys_name_exactly_this_system() {
    assert_eq!(
        Machine::mac("arm64", "27.0.1", None, true).variation_keys(),
        Some(vec!["arm64_golden_gate".to_string()])
    );
    assert_eq!(
        Machine::mac("x86_64", "15.2", None, true).variation_keys(),
        Some(vec!["x86_64_sequoia".to_string(), "sequoia".to_string()])
    );
    assert_eq!(
        Machine::mac("arm64", "40.0", None, true).variation_keys(),
        None
    );
    assert_eq!(
        Machine::mac("x86_64", "10.15.7", None, true).variation_keys(),
        Some(vec!["x86_64_catalina".to_string(), "catalina".to_string()])
    );
    assert_eq!(
        Machine::mac("x86_64", "10.14", None, true).variation_keys(),
        None,
        "Mojave is not Catalina"
    );
}

#[test]
fn a_modelled_machine_is_one_the_probes_could_report() {
    let intel = Machine::modelled("x86_64", "15.5", None, false).unwrap();
    assert_eq!(
        intel.describe(),
        "x86_64, macOS 15.5, no Xcode, no Command Line Tools"
    );
    assert!(Machine::modelled("arm64", "40.0", Some("40.1"), true).is_ok());
    for (arch, macos, xcode) in [
        ("aarch64", "27.0", None),
        ("mips", "27.0", None),
        ("arm64", "garbage", None),
        ("arm64", "", None),
        ("arm64", "27.0", Some("26.x")),
    ] {
        assert!(
            Machine::modelled(arch, macos, xcode, true).is_err(),
            "{arch} {macos} {xcode:?}"
        );
    }
}

/// A version plist the way an Xcode bundle carries it.
#[cfg(unix)]
fn version_plist(version: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
         \t<key>BuildVersion</key>\n\t<string>7</string>\n\
         \t<key>CFBundleShortVersionString</key>\n\t<string>{version}</string>\n\
         \t<key>CFBundleVersion</key>\n\t<string>24000</string>\n</dict>\n</plist>\n"
    )
}

/// aterm's shim directory in `DEVELOPER_DIR` reads as no choice at all.
/// NEGATIVE CONTROL: any other directory there wins over the selection.
#[test]
fn the_shims_developer_dir_is_no_choice_of_tools() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let selected = tmp.path().join("selected");
    std::fs::create_dir_all(&selected).unwrap();
    let active = |env: &str| {
        DeveloperDir {
            developer_dir_env: Some(env.into()),
            select_link: tmp.path().join("no-link"),
            defaults: vec![selected.clone()],
        }
        .active()
    };
    assert_eq!(active("/s/clt-shim/developer"), Some(selected.clone()));
    assert_eq!(active("/s/own"), Some("/s/own".into()));
}

/// The Xcode version is read off the selected developer directory's
/// bundle, no process run: the link `xcode-select` writes, overridden by
/// `DEVELOPER_DIR` (a bundle or its `Contents/Developer`), else the
/// default bundle. NEGATIVE CONTROL: the Command Line Tools selected, a
/// missing or non-numeric plist and an unset selection with no default
/// Xcode all read as no Xcode, and so does a developer directory that is
/// not a bundle's `Contents/Developer` even with a version plist beside
/// it (a folder named otherwise, or a `Developer` outside `Contents`).
#[cfg(unix)]
#[test]
fn the_xcode_version_is_read_from_the_selected_bundle() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path();
    let xcode = root.join("Xcode-26.1.app");
    std::fs::create_dir_all(xcode.join("Contents/Developer")).unwrap();
    std::fs::write(xcode.join("Contents/version.plist"), version_plist("26.1")).unwrap();
    let beta = root.join("Xcode-beta.app");
    std::fs::create_dir_all(beta.join("Contents/Developer")).unwrap();
    std::fs::write(beta.join("Contents/version.plist"), version_plist("27.0")).unwrap();
    let clt = root.join("CommandLineTools");
    std::fs::create_dir_all(&clt).unwrap();
    let link = root.join("xcode_select_link");
    let absent = root.join("absent/Contents/Developer");
    let probe = |env: Option<&std::path::Path>, default: &std::path::Path| {
        DeveloperDir {
            developer_dir_env: env.map(|p| p.as_os_str().to_owned()),
            select_link: link.clone(),
            defaults: vec![default.to_path_buf()],
        }
        .bundle_version()
    };
    std::os::unix::fs::symlink(xcode.join("Contents/Developer"), &link).unwrap();
    assert_eq!(probe(None, &absent).as_deref(), Some("26.1"));
    assert_eq!(
        probe(Some(&beta.join("Contents/Developer")), &absent).as_deref(),
        Some("27.0")
    );
    assert_eq!(probe(Some(&beta), &absent).as_deref(), Some("27.0"));
    assert_eq!(probe(Some(&clt), &absent), None);
    let tools = root.join("Library/Developer/CommandLineTools");
    std::fs::create_dir_all(&tools).unwrap();
    std::fs::write(
        root.join("Library/Developer/version.plist"),
        version_plist("16.4"),
    )
    .unwrap();
    let loose = root.join("Odd.app/Resources/Developer");
    std::fs::create_dir_all(&loose).unwrap();
    std::fs::write(
        root.join("Odd.app/Resources/version.plist"),
        version_plist("16.4"),
    )
    .unwrap();
    for not_a_bundle in [&tools, &loose] {
        assert_eq!(
            probe(Some(not_a_bundle), &absent),
            None,
            "{}",
            not_a_bundle.display()
        );
    }
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&clt, &link).unwrap();
    assert_eq!(probe(None, &xcode.join("Contents/Developer")), None);
    std::fs::remove_file(&link).unwrap();
    assert_eq!(
        probe(None, &xcode.join("Contents/Developer")).as_deref(),
        Some("26.1"),
        "nothing selected: the default bundle"
    );
    assert_eq!(probe(None, &absent), None);
    std::fs::write(xcode.join("Contents/version.plist"), version_plist("26.x")).unwrap();
    assert_eq!(probe(None, &xcode.join("Contents/Developer")), None);
    std::fs::remove_file(xcode.join("Contents/version.plist")).unwrap();
    assert_eq!(probe(None, &xcode.join("Contents/Developer")), None);
}

/// On this Mac the probe agrees with `xcodebuild -version` for the
/// selected developer directory. An Xcode's own `xcodebuild`, run from
/// inside its bundle, names the version the probe reads. With the Command
/// Line Tools selected, the `/usr/bin` shim refuses ("requires Xcode")
/// and the probe reads no Xcode: the shim runs only once that directory
/// holds the tools (its `clang`), so it cannot raise Apple's install
/// dialog. With no developer tools at all the comparison ends COULD NOT
/// RUN, never a pass.
#[cfg(target_os = "macos")]
#[test]
fn the_probe_agrees_with_the_selected_xcode() {
    let probe = DeveloperDir {
        developer_dir_env: None,
        ..DeveloperDir::this()
    };
    let read = probe.bundle_version();
    let Some(developer) = probe.active() else {
        aterm_uds::host::could_not_run_without(&["xcodebuild"]);
        return;
    };
    let own = developer.join("usr/bin/xcodebuild");
    let (xcodebuild, is_xcode) = if own.is_file() {
        (own, true)
    } else if probe.tool("clang").is_some() {
        (std::path::PathBuf::from("/usr/bin/xcodebuild"), false)
    } else {
        aterm_uds::host::could_not_run_without(&["xcodebuild"]);
        return;
    };
    let out = aterm_uds::spawnfd::command(&xcodebuild)
        .arg("-version")
        .env_remove("DEVELOPER_DIR")
        .output()
        .expect("xcodebuild runs");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !is_xcode {
        assert!(
            !out.status.success(),
            "{} names an Xcode the probe did not: {said}",
            developer.display()
        );
        assert_eq!(read, None, "{} is no Xcode", developer.display());
        return;
    }
    assert!(out.status.success(), "{said}");
    let first = said.lines().next().unwrap_or_default().to_owned();
    assert_eq!(first.strip_prefix("Xcode "), read.as_deref(), "{first}");
}

/// A developer tool is the selected directory's own, found without
/// running the `/usr/bin` shim: the Command Line Tools' `usr/bin`, an
/// Xcode's default toolchain (`DEVELOPER_DIR` naming the bundle), and
/// with nothing selected the first default there. NEGATIVE CONTROL: a
/// selection whose directory holds no such tool, one whose tool is not
/// executable, a `DEVELOPER_DIR` over a link that does hold one, and no
/// selection and no default each find none, where the shim would raise
/// Apple's install dialog.
#[cfg(unix)]
#[test]
fn a_developer_tool_is_found_only_in_the_selected_directory() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path();
    let put = |path: &std::path::Path, mode: u32| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    let clt = root.join("CommandLineTools");
    put(&clt.join("usr/bin/install_name_tool"), 0o755);
    let xcode = root.join("Xcode.app");
    let in_xcode = xcode
        .join("Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/install_name_tool");
    put(&in_xcode, 0o755);
    let bare = root.join("Bare");
    std::fs::create_dir_all(bare.join("usr/bin")).unwrap();
    let unusable = root.join("Unusable");
    put(&unusable.join("usr/bin/install_name_tool"), 0o644);
    let link = root.join("xcode_select_link");
    let dir = |env: Option<&std::path::Path>, defaults: &[&std::path::Path]| DeveloperDir {
        developer_dir_env: env.map(|p| p.as_os_str().to_owned()),
        select_link: link.clone(),
        defaults: defaults.iter().map(|d| d.to_path_buf()).collect(),
    };
    let tool = |d: DeveloperDir| d.tool("install_name_tool");
    std::os::unix::fs::symlink(&clt, &link).unwrap();
    assert_eq!(
        tool(dir(None, &[])),
        Some(clt.join("usr/bin/install_name_tool"))
    );
    assert_eq!(tool(dir(Some(&xcode), &[])), Some(in_xcode.clone()));
    assert_eq!(tool(dir(Some(&bare), &[])), None, "DEVELOPER_DIR wins");
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&bare, &link).unwrap();
    assert_eq!(tool(dir(None, &[&clt])), None, "the selection wins");
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&unusable, &link).unwrap();
    assert_eq!(tool(dir(None, &[])), None, "not executable");
    std::fs::remove_file(&link).unwrap();
    let absent = root.join("absent");
    assert_eq!(
        tool(dir(None, &[&absent, &clt])),
        Some(clt.join("usr/bin/install_name_tool")),
        "nothing selected: the first default there"
    );
    assert_eq!(tool(dir(None, &[&absent])), None, "no developer tools");
}

#[test]
fn the_newest_mac_is_the_api_default_system() {
    let newest = Machine::newest_mac();
    assert_eq!(newest.arch(), "arm64");
    assert_eq!(newest.macos_major(), Some(RELEASES[0].0));
    assert_eq!(newest.xcode_version(), newest.macos_version());
    assert!(newest.has_clt());
    assert_eq!(
        newest.describe(),
        "arm64, macOS 27.0, Xcode 27.0, Command Line Tools"
    );
}

#[test]
fn a_release_runs_the_xcode_released_with_it() {
    for (macos, xcode) in [
        ("27.0", Some("27.0")),
        ("26.1", Some("26.0")),
        ("15.5", Some("16.0")),
        ("14.6", Some("15.0")),
        ("13.5", Some("14.1")),
        ("12.7", Some("13.1")),
        ("11.7", Some("12.2")),
        ("10.15.7", Some("11.0")),
        ("10.14", None),
        ("10", None),
        ("30.0", Some("30.0")),
        ("20.0", None),
        ("9.3", None),
        ("+27.0", None),
        ("+30.0", None),
        ("garbage", None),
    ] {
        assert_eq!(release_xcode(macos).as_deref(), xcode, "{macos}");
    }
}
