// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn tab_marking_flips_both_receipt_shapes() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let dir = tmp.path();
    let receipt = dir.join("INSTALL_RECEIPT.json");
    // Abrew's string shape.
    std::fs::write(
        &receipt,
        "{\n  \"name\": \"x\",\n  \"installed_on_request\": \"true\"\n}\n",
    )
    .unwrap();
    assert_eq!(set_installed_on_request(dir, true).unwrap(), Some(true));
    assert!(
        std::fs::read_to_string(&receipt)
            .unwrap()
            .contains("\"installed_on_request\": \"true\"")
    );
    assert_eq!(set_installed_on_request(dir, false).unwrap(), Some(true));
    assert!(
        std::fs::read_to_string(&receipt)
            .unwrap()
            .contains("\"installed_on_request\": \"false\"")
    );
    // Brew's bool shape keeps its shape.
    std::fs::write(&receipt, "{\"installed_on_request\":false}").unwrap();
    assert_eq!(set_installed_on_request(dir, true).unwrap(), Some(false));
    assert_eq!(
        std::fs::read_to_string(&receipt).unwrap(),
        "{\"installed_on_request\":true}"
    );
    // Absent key and absent file both read as no tab.
    std::fs::write(&receipt, "{\"name\":\"x\"}").unwrap();
    assert_eq!(set_installed_on_request(dir, true).unwrap(), None);
    std::fs::remove_file(&receipt).unwrap();
    assert_eq!(set_installed_on_request(dir, true).unwrap(), None);
}

#[test]
fn receipt_runtime_deps_distinguish_absent_from_recorded() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let p = Prefix::at(&tmp.path().join("prefix"));
    let dir = tmp.path().join("keg");
    std::fs::create_dir(&dir).unwrap();
    // No receipt, then no key: brew's nil tab deps.
    assert_eq!(receipt_runtime_deps(&dir), None);
    p.write_receipt(&dir, &[("name", "x")]).unwrap();
    assert_eq!(receipt_runtime_deps(&dir), None);
    // A recorded empty closure is Some([]), and entries keep order.
    p.write_receipt_raw(&dir, &[("name", "x")], &[("runtime_dependencies", "[]")])
        .unwrap();
    assert_eq!(receipt_runtime_deps(&dir), Some(Vec::new()));
    p.write_receipt_raw(
        &dir,
        &[("name", "x")],
        &[(
            "runtime_dependencies",
            "[{\"full_name\": \"b\"}, {\"full_name\": \"a\"}]",
        )],
    )
    .unwrap();
    assert_eq!(
        receipt_runtime_deps(&dir),
        Some(vec!["b".to_string(), "a".to_string()])
    );
    // The cask hash shape splits by kind; a formula array
    // (or nothing) reads as no cask deps.
    assert_eq!(
        receipt_cask_runtime_deps(&dir),
        (Vec::<String>::new(), Vec::<String>::new())
    );
    p.write_receipt_raw(
        &dir,
        &[("token", "x")],
        &[(
            "runtime_dependencies",
            "{\"cask\": [{\"full_name\": \"other\"}], \"formula\": [{\"full_name\": \"git\"}]}",
        )],
    )
    .unwrap();
    assert_eq!(
        receipt_cask_runtime_deps(&dir),
        (vec!["git".to_string()], vec!["other".to_string()])
    );
}

#[test]
#[cfg(unix)]
fn real_brew_prefix_overlap_is_detected() {
    let tmp = aterm_tempfile::TempDir::new().unwrap();
    let brew = tmp.path().join("brew");
    let own = tmp.path().join("own");
    std::fs::create_dir(&brew).unwrap();
    std::fs::create_dir(&own).unwrap();
    let roots = [brew.as_path()];
    assert!(overlaps_brew_roots(&brew, &roots));
    assert!(overlaps_brew_roots(&brew.join("new/abrew"), &roots));
    assert!(overlaps_brew_roots(tmp.path(), &roots));
    assert!(!overlaps_brew_roots(&own.join("new/abrew"), &roots));
    assert!(overlaps_brew_roots(&own.join("../brew"), &roots));
    let alias = tmp.path().join("alias");
    symlink_dir(&brew, &alias).unwrap();
    assert!(overlaps_brew_roots(&alias.join("new/abrew"), &roots));
    assert!(overlaps_brew_roots(&brew.join("new"), &[alias.as_path()]));
    let dangling = tmp.path().join("dangling");
    symlink_dir(&tmp.path().join("absent"), &dangling).unwrap();
    assert!(overlaps_brew_roots(&dangling.join("new"), &roots));
    let file = tmp.path().join("file");
    std::fs::write(&file, b"owner file").unwrap();
    assert!(overlaps_brew_roots(&file.join("new"), &roots));
    assert!(overlaps_brew_roots(&own, &[dangling.as_path()]));
    assert_eq!(std::fs::read(&file).unwrap(), b"owner file");
    assert_eq!(std::fs::read_dir(&brew).unwrap().count(), 0);
}

#[test]
#[cfg(unix)]
fn cask_pin_reads_brews_pinned_casks_symlink() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let dir = p.cask_dir("widget", "2.0");
    std::fs::create_dir_all(&dir).unwrap();
    p.write_receipt(
        &dir,
        &[
            ("installed_by", "abrew"),
            ("token", "widget"),
            ("version", "2.0"),
        ],
    )
    .unwrap();
    assert!(!p.cask_is_pinned("widget"));
    let pins = p.root.join("var/homebrew/pinned_casks");
    std::fs::create_dir_all(&pins).unwrap();
    symlink_dir(
        std::path::Path::new("../../../Caskroom/widget/2.0"),
        &pins.join("widget"),
    )
    .unwrap();
    assert_eq!(p.cask_pinned_version("widget").as_deref(), Some("2.0"));
    assert!(p.cask_is_pinned("widget"));
    // A pin at a payload we do not own does not count.
    std::fs::remove_file(pins.join("widget")).unwrap();
    symlink_dir(std::path::Path::new("/tmp"), &pins.join("widget")).unwrap();
    assert!(!p.cask_is_pinned("widget"));
    assert!(!p.cask_pin_link_present("widget"));
    // Neither pin_cask nor unpin_cask removes a foreign symlink or file.
    assert!(p.pin_cask("widget").is_err());
    assert!(p.unpin_cask("widget").is_err());
    assert_eq!(
        std::fs::read_link(pins.join("widget")).unwrap(),
        std::path::Path::new("/tmp")
    );
    std::fs::remove_file(pins.join("widget")).unwrap();
    std::fs::write(pins.join("widget"), b"foreign").unwrap();
    assert!(p.pin_cask("widget").is_err());
    assert!(p.unpin_cask("widget").is_err());
    assert_eq!(std::fs::read(pins.join("widget")).unwrap(), b"foreign");
    std::fs::remove_file(pins.join("widget")).unwrap();
    // A symlink mentioning Caskroom/<token>/ that resolves outside or to an
    // unowned live directory is not treated as ours.
    let unowned = p.cask_dir("widget", "unowned");
    std::fs::create_dir_all(&unowned).unwrap();
    for bad_target in [
        std::path::PathBuf::from("../../../Caskroom/widget/unowned"),
        std::path::PathBuf::from("../../../Caskroom/widget/1.0/../../other"),
        std::path::PathBuf::from("/other/prefix/Caskroom/widget/2.0"),
    ] {
        symlink_dir(&bad_target, &pins.join("widget")).unwrap();
        assert!(!p.cask_is_pinned("widget"), "{bad_target:?}");
        assert!(!p.cask_pin_link_present("widget"), "{bad_target:?}");
        assert!(p.pin_cask("widget").is_err(), "{bad_target:?}");
        assert!(p.unpin_cask("widget").is_err(), "{bad_target:?}");
        std::fs::remove_file(pins.join("widget")).unwrap();
    }
    std::fs::remove_dir_all(&unowned).unwrap();
    // An owned dangling cask pin is recognized and cleaned up by both pin_cask and unpin_cask.
    symlink_dir(
        std::path::Path::new("../../../Caskroom/widget/1.0"),
        &pins.join("widget"),
    )
    .unwrap();
    assert!(!p.cask_is_pinned("widget"));
    assert!(p.cask_pin_link_dangling_ours("widget"));
    assert!(p.cask_pin_link_present("widget"));
    p.pin_cask("widget").unwrap();
    assert_eq!(p.cask_pinned_version("widget").as_deref(), Some("2.0"));
    assert!(p.cask_is_pinned("widget"));
    p.unpin_cask("widget").unwrap();
    assert!(!p.cask_is_pinned("widget"));
    assert!(std::fs::symlink_metadata(&pins).is_err());
    // Dangling pin removal when the cask itself was uninstalled.
    std::fs::create_dir_all(&pins).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    symlink_dir(
        std::path::Path::new("../../../Caskroom/widget/2.0"),
        &pins.join("widget"),
    )
    .unwrap();
    assert!(p.cask_pin_link_present("widget"));
    p.unpin_cask("widget").unwrap();
    assert!(std::fs::symlink_metadata(&pins).is_err());
    // Aliased var/homebrew base is refused before touching external trees.
    let outside = tmp.path().join("outside-var");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::remove_dir_all(p.root.join("var/homebrew")).unwrap();
    symlink_dir(&outside, &p.root.join("var/homebrew")).unwrap();
    assert!(p.pin_cask("widget").is_err());
    assert!(p.unpin_cask("widget").is_err());
}

#[test]
#[cfg(unix)]
fn relative_opt_and_pin_links_resolve() {
    // Brew writes opt/pin links relative (`../Cellar/...`);
    // abrew's own are absolute. Both spellings must read.
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let keg = p.keg("widget", "1.0");
    std::fs::create_dir_all(&keg).unwrap();
    p.write_receipt(
        &keg,
        &[
            ("installed_by", "abrew"),
            ("name", "widget"),
            ("version", "1.0"),
        ],
    )
    .unwrap();
    let opt_dir = p.root.join("opt");
    std::fs::create_dir_all(&opt_dir).unwrap();
    symlink_dir(
        std::path::Path::new("../Cellar/widget/1.0"),
        &opt_dir.join("widget"),
    )
    .unwrap();
    assert_eq!(p.optlinked_version("widget").as_deref(), Some("1.0"));
    let pins = p.root.join("Pinned");
    std::fs::create_dir_all(&pins).unwrap();
    symlink_dir(
        std::path::Path::new("../Cellar/widget/1.0"),
        &pins.join("widget"),
    )
    .unwrap();
    assert_eq!(p.pinned_version("widget").as_deref(), Some("1.0"));
    assert!(p.is_pinned("widget"));
}

#[test]
#[cfg(unix)]
fn unpin_removes_our_dangling_pin_but_not_foreign_entries() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let dir = p.keg("tool", "1.0");
    std::fs::create_dir_all(&dir).unwrap();
    p.write_receipt(
        &dir,
        &[
            ("installed_by", "abrew"),
            ("name", "tool"),
            ("version", "1.0"),
        ],
    )
    .unwrap();
    p.pin("tool").unwrap();
    assert!(p.is_pinned("tool"));
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(!p.is_pinned("tool"));
    p.unpin("tool").unwrap();
    assert!(std::fs::symlink_metadata(p.root.join("Pinned/tool")).is_err());
    std::fs::write(p.root.join("Pinned/foreign"), b"keep").unwrap();
    assert!(p.unpin("foreign").is_err());
    assert_eq!(
        std::fs::read(p.root.join("Pinned/foreign")).unwrap(),
        b"keep"
    );
    symlink_dir(std::path::Path::new("/tmp"), &p.root.join("Pinned/outward")).unwrap();
    assert!(p.unpin("outward").is_err());
    assert!(std::fs::symlink_metadata(p.root.join("Pinned/outward")).is_ok());

    std::fs::create_dir_all(&dir).unwrap();
    p.write_receipt(
        &dir,
        &[
            ("installed_by", "abrew"),
            ("name", "tool"),
            ("version", "1.0"),
        ],
    )
    .unwrap();
    let live = p.keg("tool", "unowned");
    std::fs::create_dir_all(&live).unwrap();
    std::fs::write(live.join("owner-note"), b"keep").unwrap();
    let version_link = p.keg("tool", "linked");
    let absent = tmp.path().join("foreign-missing");
    symlink_dir(&absent, &version_link).unwrap();
    let marker = p.root.join("Pinned/tool");
    let mut version_slash = version_link.as_os_str().to_os_string();
    version_slash.push("/");
    let targets = [
        live.clone(),
        p.cellar().join("tool/../../foreign-missing"),
        p.keg("tool", "missing").join("nested"),
        version_link.clone(),
        PathBuf::from(version_slash),
        version_link.join("."),
    ];
    for live_alias in [false, true] {
        if live_alias {
            std::fs::create_dir_all(&absent).unwrap();
            std::fs::write(absent.join("owner-note"), b"keep").unwrap();
            std::fs::write(
                absent.join("INSTALL_RECEIPT.json"),
                r#"{"installed_by":"abrew","name":"tool","version":"linked"}"#,
            )
            .unwrap();
        }
        for target in &targets {
            symlink_dir(target, &marker).unwrap();
            assert!(p.pinned_version("tool").is_none(), "{target:?}");
            assert!(!p.pin_link_dangling_ours("tool"), "{target:?}");
            assert!(p.pin("tool").is_err(), "{target:?}");
            assert!(p.unpin("tool").is_err(), "{target:?}");
            assert_eq!(std::fs::read_link(&marker).unwrap(), *target);
            std::fs::remove_file(&marker).unwrap();
        }
    }
    assert_eq!(std::fs::read(live.join("owner-note")).unwrap(), b"keep");
    assert_eq!(std::fs::read(absent.join("owner-note")).unwrap(), b"keep");
    assert_eq!(std::fs::read_link(&version_link).unwrap(), absent);
    std::fs::remove_file(&version_link).unwrap();
    std::fs::remove_dir_all(p.cellar().join("tool")).unwrap();
    for version in ["1.0", "1,2"] {
        symlink_dir(&p.keg("tool", version), &marker).unwrap();
        assert!(p.pin_link_dangling_ours("tool"));
        p.unpin("tool").unwrap();
        assert!(std::fs::symlink_metadata(&marker).is_err());
    }
}

#[test]
fn full_name_uses_receipt_then_official_tap() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let dir = p.keg("tool", "1.0");
    std::fs::create_dir_all(&dir).unwrap();
    p.write_receipt(
        &dir,
        &[
            ("installed_by", "abrew"),
            ("name", "tool"),
            ("version", "1.0"),
            ("full_name", "someone/tap/tool"),
        ],
    )
    .unwrap();
    let inst = p.installed_one(Kind::Formula, "tool").unwrap();
    assert_eq!(p.full_name(&inst), "someone/tap/tool");
    let dir = p.keg("wget", "1.25");
    std::fs::create_dir_all(&dir).unwrap();
    p.write_receipt(
        &dir,
        &[
            ("installed_by", "abrew"),
            ("name", "wget"),
            ("version", "1.25"),
            ("full_name", "wget"),
        ],
    )
    .unwrap();
    let inst = p.installed_one(Kind::Formula, "wget").unwrap();
    assert_eq!(p.full_name(&inst), "homebrew/core/wget");
    let dir = p.cask_dir("firefox", "128.0");
    std::fs::create_dir_all(&dir).unwrap();
    p.write_receipt(
        &dir,
        &[
            ("installed_by", "abrew"),
            ("token", "firefox"),
            ("version", "128.0"),
        ],
    )
    .unwrap();
    let inst = p.installed_one(Kind::Cask, "firefox").unwrap();
    assert_eq!(p.full_name(&inst), "homebrew/cask/firefox");
    // A brew tab records the origin tap at source.tap.
    let dir = p.keg("taptool", "3.0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("INSTALL_RECEIPT.json"),
        r#"{"installed_by":"abrew","name":"taptool","version":"3.0","source":{"tap":"someone/tap"}}"#,
    )
    .unwrap();
    let inst = p.installed_one(Kind::Formula, "taptool").unwrap();
    assert_eq!(p.full_name(&inst), "someone/tap/taptool");
}

#[test]
fn staging_dirs_are_invisible_to_the_installed_census() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let dir = p.keg("hello", "2.12.3");
    std::fs::create_dir_all(&dir).unwrap();
    p.write_receipt(
        &dir,
        &[
            ("installed_by", "abrew"),
            ("name", "hello"),
            ("version", "2.12.3"),
        ],
    )
    .unwrap();
    let staged = aterm_tempfile::TempDir::new_in(p.cellar()).unwrap().keep();
    std::fs::create_dir_all(staged.join("go/1.27.3")).unwrap();
    let checked = p.installed_checked(Kind::Formula).unwrap();
    assert_eq!(checked.len(), 1);
    assert_eq!(checked[0].name, "hello");
    assert!(p.installed_checked(Kind::Cask).unwrap().is_empty());
    let stray = p.cellar().join("hello").join(".tmp1234_deadbeef_0");
    std::fs::create_dir_all(&stray).unwrap();
    let checked = p.installed_checked(Kind::Formula).unwrap();
    assert_eq!(checked[0].versions, vec!["2.12.3".to_string()]);
    // A brew-layout caskroom keeps its metadata beside the
    // installed version; the checked census must skip it.
    let cask_dir = p.cask_dir("demo", "1.0");
    std::fs::create_dir_all(&cask_dir).unwrap();
    p.write_receipt(
        &cask_dir,
        &[
            ("installed_by", "abrew"),
            ("token", "demo"),
            ("version", "1.0"),
        ],
    )
    .unwrap();
    std::fs::create_dir_all(p.caskroom().join("demo/.metadata/1.0")).unwrap();
    let casks = p.installed_checked(Kind::Cask).unwrap();
    assert_eq!(casks.len(), 1);
    assert_eq!(casks[0].versions, vec!["1.0".to_string()]);
}

/// macOS and Linux only: the restore publishes by an atomic directory
/// rename ([`publish_directory`]), which no other platform offers.
#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn stranded_reinstall_aside_is_restored_without_discarding_other_payloads() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    let aside = |receipt: &[(&str, &str)], marker: &str| {
        let staged = aterm_tempfile::TempDir::new_in(p.cellar()).unwrap().keep();
        let payload = staged.join("payload");
        std::fs::create_dir_all(&payload).unwrap();
        p.write_receipt(&payload, receipt).unwrap();
        std::fs::write(payload.join("MARKER"), marker).unwrap();
        staged
    };
    let mut foreign = Vec::new();
    for base in [p.cellar(), p.caskroom(), p.cache(), p.cache().join("api")] {
        let dir = base.join(".tmp-owner-work");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("KEEP"), b"owner bytes").unwrap();
        foreign.push(dir);
    }
    let staged = aside(
        &[
            ("installed_by", "abrew"),
            ("name", "hello"),
            ("version", "2.12.3"),
        ],
        "old-keg",
    );
    p.restore_stale_asides();
    assert!(staged.exists());
    assert!(!staged.join("payload").exists());
    for dir in foreign {
        assert_eq!(std::fs::read(dir.join("KEEP")).unwrap(), b"owner bytes");
    }
    let live = p.keg("hello", "2.12.3");
    assert_eq!(
        std::fs::read_to_string(live.join("MARKER")).unwrap(),
        "old-keg"
    );
    let checked = p.installed_checked(Kind::Formula).unwrap();
    assert_eq!(checked.len(), 1);
    assert_eq!(checked[0].name, "hello");
    std::fs::write(live.join("MARKER"), "new-keg").unwrap();
    let staged = aside(
        &[
            ("installed_by", "abrew"),
            ("name", "hello"),
            ("version", "2.12.3"),
        ],
        "old-keg",
    );
    p.restore_stale_asides();
    assert!(staged.exists());
    assert_eq!(
        std::fs::read(staged.join("payload/MARKER")).unwrap(),
        b"old-keg"
    );
    assert_eq!(
        std::fs::read_to_string(live.join("MARKER")).unwrap(),
        "new-keg"
    );
    let staged = aside(
        &[
            ("installed_by", "another"),
            ("name", "evil"),
            ("version", "9.9"),
        ],
        "foreign",
    );
    p.restore_stale_asides();
    assert!(staged.exists());
    assert_eq!(
        std::fs::read(staged.join("payload/MARKER")).unwrap(),
        b"foreign"
    );
    assert!(!p.keg("evil", "9.9").exists());
}

#[test]
fn installed_scan_and_pin_round_trip() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let p = Prefix::at(tmp.path());
    p.ensure().unwrap();
    for version in ["1.24.5", "1.25.0"] {
        let dir = p.keg("wget", version);
        std::fs::create_dir_all(&dir).unwrap();
        p.write_receipt(
            &dir,
            &[
                ("installed_by", "abrew"),
                ("name", "wget"),
                ("version", version),
            ],
        )
        .unwrap();
    }
    for (version, receipt) in [
        ("3.0", None),
        (
            "4.0",
            Some(r#"{"installed_by":"another","name":"wget","version":"4.0"}"#),
        ),
        (
            "5.0",
            Some(r#"{"installed_by":"abrew","name":"different","version":"5.0"}"#),
        ),
        (
            "6.0",
            Some(r#"{"installed_by":"abrew","name":"wget","version":"7.0"}"#),
        ),
        ("8.0", Some("not json")),
    ] {
        let dir = p.keg("wget", version);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(receipt) = receipt {
            std::fs::write(dir.join("INSTALL_RECEIPT.json"), receipt).unwrap();
        }
    }
    let cask = p.cask_dir("demo", "1.0");
    std::fs::create_dir_all(&cask).unwrap();
    p.write_receipt(
        &cask,
        &[
            ("installed_by", "abrew"),
            ("token", "demo"),
            ("version", "1.0"),
        ],
    )
    .unwrap();
    assert_eq!(p.installed(Kind::Cask)[0].name, "demo");
    #[cfg(unix)]
    {
        let linked = p.keg("wget", "9.0");
        std::fs::create_dir_all(&linked).unwrap();
        let outside = tmp.path().join("foreign-receipt");
        std::fs::write(
            &outside,
            r#"{"installed_by":"abrew","name":"wget","version":"9.0"}"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, linked.join("INSTALL_RECEIPT.json")).unwrap();
    }
    let got = p.installed(Kind::Formula);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].version, "1.25.0");
    assert_eq!(got[0].versions.len(), 2);
    #[cfg(unix)]
    {
        p.pin("wget").unwrap();
        assert_eq!(p.pinned_version("wget").as_deref(), Some("1.25.0"));
        assert!(p.is_pinned("wget"));
        p.unpin("wget").unwrap();
        assert!(!p.is_pinned("wget"));
        let marker = p.root.join("Pinned/wget");
        std::fs::write(&marker, b"owner bytes").unwrap();
        assert!(p.pin("wget").is_err());
        assert!(p.unpin("wget").is_err());
        assert_eq!(std::fs::read(&marker).unwrap(), b"owner bytes");
        std::fs::remove_file(&marker).unwrap();
        std::os::unix::fs::symlink(p.keg("wget", "missing"), &marker).unwrap();
        assert!(!p.is_pinned("wget"));
        p.pin("wget").unwrap();
        assert_eq!(p.pinned_version("wget").as_deref(), Some("1.25.0"));
        p.unpin("wget").unwrap();
        let foreign = tmp.path().join("owner-note");
        std::fs::write(&foreign, b"keep").unwrap();
        std::os::unix::fs::symlink(&foreign, p.root.join("state.json")).unwrap();
        p.rebuild_state().unwrap();
        assert!(!p.root.join("state.json").is_symlink());
        assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
        let receipt = p.keg("wget", "1.25.0").join("INSTALL_RECEIPT.json");
        std::fs::remove_file(&receipt).unwrap();
        std::os::unix::fs::symlink(&foreign, &receipt).unwrap();
        p.write_receipt(
            &p.keg("wget", "1.25.0"),
            &[
                ("installed_by", "abrew"),
                ("name", "wget"),
                ("version", "1.25.0"),
            ],
        )
        .unwrap();
        assert!(!receipt.is_symlink());
        assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
        for bad in ["../owner-note", "..", ".", "", foreign.to_str().unwrap()] {
            assert!(p.pin(bad).is_err(), "{bad:?}");
            assert!(p.unpin(bad).is_err(), "{bad:?}");
            assert!(p.pinned_version(bad).is_none());
        }
        assert_eq!(std::fs::read(&foreign).unwrap(), b"keep");
        let original = tmp.path().join("original-package");
        std::fs::rename(p.cellar().join("wget"), &original).unwrap();
        std::os::unix::fs::symlink(&original, p.cellar().join("wget")).unwrap();
        std::os::unix::fs::symlink(p.keg("wget", "1.25.0"), &marker).unwrap();
        assert!(p.installed(Kind::Formula).is_empty());
        assert!(p.pinned_version("wget").is_none());
        assert!(p.pin("wget").is_err());
        assert!(p.unpin("wget").is_err());
        assert!(marker.is_symlink());
        assert!(original.join("1.25.0/INSTALL_RECEIPT.json").is_file());
    }
}

#[cfg(unix)]
#[test]
fn linked_layout_bases_never_admit_or_mutate_a_foreign_tree() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(outside.join("wget/1.0")).unwrap();
    std::fs::write(outside.join("owner-note"), b"keep").unwrap();
    std::fs::write(
        outside.join("wget/1.0/INSTALL_RECEIPT.json"),
        r#"{"installed_by":"abrew","name":"wget","version":"1.0"}"#,
    )
    .unwrap();
    let aside = outside.join(".abrew-reinstall-wget-2.0/payload");
    std::fs::create_dir_all(&aside).unwrap();
    std::fs::write(
        aside.join("INSTALL_RECEIPT.json"),
        r#"{"installed_by":"abrew","name":"wget","version":"2.0"}"#,
    )
    .unwrap();
    for relative in ["", "Cellar", "Pinned", "Library", "cache/api"] {
        let root = tmp
            .path()
            .join(format!("prefix-{}", relative.replace('/', "-")));
        let prefix = Prefix::at(&root);
        if relative.is_empty() {
            std::os::unix::fs::symlink(&outside, &root).unwrap();
        } else {
            std::fs::create_dir_all(root.join(relative).parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&outside, root.join(relative)).unwrap();
        }
        prefix.restore_stale_asides();
        assert!(!outside.join("wget/2.0").exists(), "{relative}");
        assert!(aside.join("INSTALL_RECEIPT.json").is_file(), "{relative}");
        assert!(prefix.ensure().is_err(), "{relative}");
        assert!(prefix.pin("wget").is_err(), "{relative}");
        assert!(prefix.unpin("wget").is_err(), "{relative}");
        assert!(Lock::within(&prefix, std::time::Duration::ZERO).is_err());
        assert!(!root.join("state.json").exists());
        assert!(!root.join("abrew.lock").exists());
        if relative.is_empty() || relative == "Cellar" {
            assert!(prefix.installed(Kind::Formula).is_empty());
        }
        assert_eq!(std::fs::read(outside.join("owner-note")).unwrap(), b"keep");
        assert!(outside.join("wget/1.0/INSTALL_RECEIPT.json").is_file());
    }
}

#[test]
fn the_writer_lock_never_reclaims_a_live_holder_or_unlinks_its_rendezvous() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let prefix = Prefix::at(tmp.path());
    let first = Lock::acquire(&prefix).unwrap();
    first
        ._file
        .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
        .unwrap();
    assert!(Lock::within(&prefix, std::time::Duration::ZERO).is_err());
    let path = prefix.root.join("abrew.lock");
    assert!(path.is_file());
    drop(first);
    let successor = Lock::acquire(&prefix).unwrap();
    assert!(Lock::within(&prefix, std::time::Duration::ZERO).is_err());
    drop(successor);
    assert!(
        path.is_file(),
        "the lock inode remains the next writer's rendezvous"
    );
    drop(Lock::acquire(&prefix).unwrap());
    let legacy = prefix.root.join(".abrew-lock.d");
    std::fs::create_dir(&legacy).unwrap();
    std::fs::write(legacy.join("owner-note"), b"keep").unwrap();
    assert!(Lock::within(&prefix, std::time::Duration::ZERO).is_err());
    assert_eq!(std::fs::read(legacy.join("owner-note")).unwrap(), b"keep");
    std::fs::remove_file(legacy.join("owner-note")).unwrap();
    std::fs::remove_dir(&legacy).unwrap();
    drop(Lock::acquire(&prefix).unwrap());
    #[cfg(unix)]
    {
        std::fs::remove_file(&path).unwrap();
        let foreign = tmp.path().join("foreign");
        std::fs::write(&foreign, b"owner bytes").unwrap();
        std::os::unix::fs::symlink(&foreign, &path).unwrap();
        assert!(Lock::within(&prefix, std::time::Duration::ZERO).is_err());
        assert_eq!(std::fs::read(&foreign).unwrap(), b"owner bytes");
        assert!(path.is_symlink());
    }
}
