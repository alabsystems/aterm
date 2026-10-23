// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for the `alab` module.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;
use crate::flow::Fetcher;
use crate::select::Candidate;
use crate::sig::testkit;

const ARM_MAC: &str = "aarch64-apple-darwin";
pub(crate) const BUILD: u64 = 2_026_101_601;

/// Write `root` as a Codex package whose entrypoint is `bin/codex`.
fn package(root: &Path, version: &str, exe: &[u8], alab: bool) {
    package_at(root, version, exe, alab, "bin/codex");
}

/// [`package`] with `entry` its entrypoint ([`host_entry`]: what a shim of codex runs here).
fn package_at(root: &Path, version: &str, exe: &[u8], alab: bool, entry: &str) {
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join(entry), exe).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(root.join(entry), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(root.join(PACKAGE_JSON), package_json_naming(version, entry)).unwrap();
    if alab {
        std::fs::write(root.join(ALAB_BUILD_JSON), br#"{"source":"0"}"#).unwrap();
    }
}

/// A Codex package's manifest, `entry` its entrypoint.
fn package_json_naming(version: &str, entry: &str) -> String {
    format!(
        r#"{{"layoutVersion":1,"version":"{version}","target":"aarch64-apple-darwin","entrypoint":"{entry}"}}"#
    )
}

/// `bin/codex` as this host's shim runs it (`bin/codex.exe` on Windows).
fn host_entry() -> String {
    format!("bin/codex{}", crate::platform::EXE_SUFFIX)
}

/// A complete, active store build of codex at `build`, an ALab package when `alab`.
fn store_build(l: &Layout, build: u64, alab: bool) -> PathBuf {
    store_build_with(l, build, alab, b"#!/bin/sh\nexit 0\n")
}

fn store_build_with(l: &Layout, build: u64, alab: bool, exe: &[u8]) -> PathBuf {
    let dir = l.build_dir("codex", build);
    package_at(
        &dir,
        &format!("0.164.0-dev.{build}+gabc"),
        exe,
        alab,
        &host_entry(),
    );
    crate::store::mark_build_ready(&dir).unwrap();
    crate::activate::activate_build(l, &dir).unwrap();
    crate::activate::install_shims(l, &dir, &["codex".to_string()], crate::Aliases::Off).unwrap();
    dir
}

fn scratch(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-alab-{label}-"))
}

#[test]
fn the_source_rule_takes_its_arms_in_order() {
    let all = Facts {
        linked: true,
        vendor_chosen: true,
        index_pin: Some(BUILD),
        installed_alab: Some(BUILD - 1),
    };
    assert_eq!(rule(&all), Source::Linked);
    let unlinked = Facts {
        linked: false,
        ..all
    };
    assert_eq!(rule(&unlinked), Source::VendorDirect, "a person's choice");
    let chosen_by_nobody = Facts {
        vendor_chosen: false,
        ..unlinked
    };
    assert_eq!(rule(&chosen_by_nobody), Source::Index { build: BUILD });
    let unpinned = Facts {
        index_pin: None,
        ..chosen_by_nobody
    };
    assert_eq!(rule(&unpinned), Source::KeepAlab { build: BUILD - 1 });
    assert_eq!(rule(&Facts::default()), Source::VendorDirect);
    assert!(Source::Index { build: BUILD }.index_lane());
    for other in [
        Source::Linked,
        Source::KeepAlab { build: BUILD },
        Source::VendorDirect,
    ] {
        assert!(!other.index_lane(), "{other:?}");
    }
}

#[test]
fn the_alab_store_window_is_strictly_between_the_legacy_pins_and_the_vendor_ids() {
    let base = crate::vendor_direct::VENDOR_BUILD_BASE;
    for legacy in [crate::vendor_direct::LEGACY_CEILING, 2_026_101_501, 0] {
        assert!(!in_store_window(legacy), "{legacy}");
    }
    assert!(in_store_window(LEGACY_PIN_CEILING + 1));
    assert!(in_store_window(BUILD));
    assert!(in_store_window(base - 1));
    assert!(!in_store_window(base));
}

/// A signed-shape index naming `codex` from `repo`, its stable channel pinning `pin`.
fn index_text(repo: &str, pin: Option<u64>, extra: &str) -> String {
    let pins = pin.map_or_else(String::new, |b| format!(", codex = {b}"));
    format!(
        "schema = 2\nindex_build = 72\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
         machine_id = \"{}\"\nroster_seq = {}\n\
         [programs.ay]\nrepo = \"ay\"\n[programs.codex]\nrepo = \"{repo}\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\n{extra}\
         pin = {{ ay = 18{pins} }}\n",
        testkit::MACHINE_ID,
        testkit::SEQ,
    )
}

fn parsed(text: &str) -> Index {
    crate::manifest::parse_index(&testkit::machine_signed(text.as_bytes().to_vec())).unwrap()
}

#[test]
fn a_pin_routes_only_from_alab_repo_on_alab_targets() {
    let good = parsed(&index_text("codex-alab", Some(BUILD), ""));
    assert_eq!(index_pin(&good, "codex", ARM_MAC), Some(BUILD));
    let live = parsed(&index_text("aterm", Some(2_026_101_501), ""));
    assert_eq!(index_pin(&live, "codex", ARM_MAC), None);
    let unpinned = parsed(&index_text("codex-alab", None, ""));
    assert_eq!(index_pin(&unpinned, "codex", ARM_MAC), None);
    for other in [
        "x86_64-apple-darwin",
        "aarch64-unknown-linux-gnu",
        "x86_64-pc-windows-msvc",
    ] {
        assert_eq!(index_pin(&good, "codex", other), None, "{other}");
    }
    let claude = parsed(
        &index_text("codex-alab", Some(BUILD), "").replace("[programs.codex]", "[programs.claude]"),
    );
    assert_eq!(index_pin(&claude, "claude", ARM_MAC), None);
    assert_eq!(index_pin(&good, "ay", ARM_MAC), None);
}

#[test]
fn codex_old_is_the_vendors_lane_whatever_the_index_names() {
    let l = scratch("codex-old");
    let text = index_text("codex-alab", Some(BUILD), "")
        .replace(
            "[[channels]]",
            "[programs.codex-old]\nrepo = \"codex-alab\"\n[[channels]]",
        )
        .replace(
            &format!("codex = {BUILD}"),
            &format!("codex = {BUILD}, codex-old = {BUILD}"),
        );
    let index = parsed(&text);
    assert_eq!(
        index.program("codex-old").map(|p| p.repo.as_str()),
        Some("codex-alab"),
        "PRECONDITION: the index names it from ALab's repo"
    );
    assert_eq!(index_pin(&index, "codex", ARM_MAC), Some(BUILD));
    assert_eq!(index_pin(&index, "codex-old", ARM_MAC), None);
    assert_eq!(
        source(&l, "codex", ARM_MAC, Some(&index)),
        Source::Index { build: BUILD }
    );
    assert_eq!(
        source(&l, "codex-old", ARM_MAC, Some(&index)),
        Source::VendorDirect
    );
    assert!(!index_owns(&l, "codex-old", ARM_MAC, Some(&index)));
    std::fs::create_dir_all(l.prefix.join("sources")).unwrap();
    std::fs::write(vendor_choice_path(&l, "codex-old"), b"").unwrap();
    assert!(!vendor_chosen(&l, "codex-old"));
    let ch = index.channel_for("stable", ARM_MAC).unwrap();
    let members: Vec<String> = crate::apply::plan_groups_here(&l, &index, &ch, ARM_MAC)
        .into_iter()
        .flat_map(|g| g.members)
        .collect();
    assert_eq!(members, ["ay", "codex"]);
}

#[test]
fn a_pin_outside_the_window_or_revoked_routes_nothing() {
    for bad in [
        crate::vendor_direct::LEGACY_CEILING,
        2_026_101_501,
        crate::vendor_direct::VENDOR_BUILD_BASE,
        crate::vendor_direct::VENDOR_BUILD_BASE + 1,
        12_321,
    ] {
        let index = parsed(&index_text("codex-alab", Some(bad), ""));
        assert_eq!(index_pin(&index, "codex", ARM_MAC), None, "{bad}");
    }
    let yanked = parsed(&index_text(
        "codex-alab",
        Some(BUILD),
        &format!("yanked = [\"codex@{BUILD}\"]\n"),
    ));
    assert_eq!(index_pin(&yanked, "codex", ARM_MAC), None);
    let floored = parsed(&index_text(
        "codex-alab",
        Some(BUILD),
        &format!("min_build_by_program = {{ codex = {} }}\n", BUILD + 1),
    ));
    assert_eq!(index_pin(&floored, "codex", ARM_MAC), None);
    let overlaid = parsed(&index_text(
        "codex-alab",
        None,
        &format!("pin_by_target = {{ \"{ARM_MAC}\" = {{ codex = {BUILD} }} }}\n"),
    ));
    assert_eq!(index_pin(&overlaid, "codex", ARM_MAC), Some(BUILD));
    assert_eq!(index_pin(&overlaid, "codex", "x86_64-apple-darwin"), None);
}

#[test]
fn the_marker_is_the_package_layout_and_the_alab_build_record() {
    let d = crate::testdir::dir("atpkg-alab-marker-");
    let alab = d.join("alab");
    package(&alab, "0.164.0-dev.12+gabc", b"x", true);
    assert!(is_alab_package(&alab));
    assert_eq!(
        package_version(&alab).as_deref(),
        Some("0.164.0-dev.12+gabc")
    );
    assert_eq!(entrypoint_of(&alab), Some(PathBuf::from("bin/codex")));
    let vendor = d.join("vendor");
    package(&vendor, "0.164.0", b"x", false);
    assert!(!is_alab_package(&vendor));
    let dir_marker = d.join("dir-marker");
    package(&dir_marker, "0.1.0-dev.1+g0", b"x", false);
    std::fs::create_dir_all(dir_marker.join(ALAB_BUILD_JSON)).unwrap();
    assert!(!is_alab_package(&dir_marker));
    #[cfg(unix)]
    {
        let linked_marker = d.join("linked-marker");
        package(&linked_marker, "0.1.0-dev.1+g0", b"x", false);
        std::os::unix::fs::symlink(
            alab.join(ALAB_BUILD_JSON),
            linked_marker.join(ALAB_BUILD_JSON),
        )
        .unwrap();
        assert!(
            !is_alab_package(&linked_marker),
            "a symlinked record is no record"
        );
    }
    let no_entry = d.join("no-entry");
    package(&no_entry, "0.1.0-dev.1+g0", b"x", true);
    std::fs::remove_file(no_entry.join("bin/codex")).unwrap();
    assert!(!is_alab_package(&no_entry));
    for bad in [
        r#"{"layoutVersion":2,"entrypoint":"bin/codex"}"#,
        r#"{"layoutVersion":1,"entrypoint":"../bin/codex"}"#,
        r#"{"layoutVersion":1,"entrypoint":"/bin/codex"}"#,
        r#"{"layoutVersion":1}"#,
        "not json",
    ] {
        assert_eq!(package_entrypoint(bad), None, "{bad}");
        std::fs::write(alab.join(PACKAGE_JSON), bad).unwrap();
        assert!(!is_alab_package(&alab), "{bad}");
    }
    assert_eq!(
        package_entrypoint(r#"{"layoutVersion":1,"entrypoint":"bin/codex"}"#),
        Some(PathBuf::from("bin/codex"))
    );
    std::fs::write(
        alab.join(PACKAGE_JSON),
        r#"{"layoutVersion":1,"entrypoint":""}"#,
    )
    .unwrap();
    assert!(!is_alab_package(&alab));
}

#[test]
fn the_exe_form_finds_the_root_whose_entrypoint_is_the_exe() {
    let d = crate::testdir::dir("atpkg-alab-exe-");
    let root = d.join("pkg");
    package(&root, "0.164.0-dev.12+gabc", b"x", true);
    assert!(exe_is_alab(&root.join("bin/codex")));
    #[cfg(unix)]
    {
        let link = d.join("link-to-codex");
        std::os::unix::fs::symlink(root.join("bin/codex"), &link).unwrap();
        assert!(
            exe_is_alab(&link),
            "a link to the entrypoint is the entrypoint"
        );
    }
    std::fs::write(root.join("bin/other"), b"x").unwrap();
    assert!(!exe_is_alab(&root.join("bin/other")), "not the entrypoint");
    assert!(!exe_is_alab(&d.join("missing")));
    std::fs::remove_file(root.join(ALAB_BUILD_JSON)).unwrap();
    assert!(!exe_is_alab(&root.join("bin/codex")));
}

/// The upstream commit a test record names.
pub(crate) const UPSTREAM_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

/// An `alab-build.json` whose `upstream` field is `upstream`.
pub(crate) fn upstream_record(upstream: &str) -> String {
    format!(r#"{{"source":"0","version":"0.164.0-dev.12+gabc","upstream":{upstream}}}"#)
}

/// The field as the packager writes it.
pub(crate) fn full_upstream() -> String {
    format!(
        r#"{{"release":"rust-v0.163.0","commits_since_release":241,"commit":"{UPSTREAM_SHA}","commit_date":"2026-10-15T18:02:11Z"}}"#
    )
}

#[test]
fn the_upstream_is_read_off_the_record_whole_or_not_at_all() {
    let parsed = parse_upstream(&upstream_record(&full_upstream())).expect("the field");
    assert_eq!(
        parsed,
        Upstream {
            release: "rust-v0.163.0".into(),
            commit: UPSTREAM_SHA.into(),
            since: Some(241),
            day: Some("2026-10-15".into()),
        }
    );
    assert_eq!(
        parsed.words(),
        "based on upstream rust-v0.163.0+g0123456789ab"
    );
    assert_eq!(
        parsed.release_line("openai/codex"),
        "Based on upstream openai/codex rust-v0.163.0 + 241 commits (0123456789ab, 2026-10-15)"
    );
    let bare = format!(
        r#"{{"release":"rust-v0.163.0","commit":"{}"}}"#,
        UPSTREAM_SHA.to_ascii_uppercase()
    );
    let bare = parse_upstream(&upstream_record(&bare)).expect("release and commit alone");
    assert_eq!(
        bare,
        Upstream {
            since: None,
            day: None,
            ..parsed.clone()
        }
    );
    assert_eq!(bare.words(), parsed.words());
    assert_eq!(
        bare.release_line("openai/codex"),
        "Based on upstream openai/codex rust-v0.163.0 (0123456789ab)"
    );
    let dated = format!(
        r#"{{"release":"rust-v0.163.0","commit":"{UPSTREAM_SHA}","commit_date":"2026-10-20T13:50:13Z"}}"#
    );
    assert_eq!(
        parse_upstream(&upstream_record(&dated)).map(|u| u.release_line("openai/codex")),
        Some("Based on upstream openai/codex rust-v0.163.0 (0123456789ab, 2026-10-20)".into())
    );
    let one = format!(
        r#"{{"release":"rust-v0.163.0","commits_since_release":1,"commit":"{UPSTREAM_SHA}"}}"#
    );
    assert_eq!(
        parse_upstream(&upstream_record(&one)).map(|u| u.release_line("openai/codex")),
        Some("Based on upstream openai/codex rust-v0.163.0 + 1 commit (0123456789ab)".into())
    );
    assert_eq!(parse_upstream(r#"{"source":"0"}"#), None);
    let sha = UPSTREAM_SHA;
    let long = "v".repeat(129);
    for field in [
        r#""rust-v0.163.0""#.to_string(),
        "null".to_string(),
        "{}".to_string(),
        r#"{"release":"rust-v0.163.0"}"#.to_string(),
        format!(r#"{{"commit":"{sha}"}}"#),
        format!(r#"{{"release":"","commit":"{sha}"}}"#),
        format!(r#"{{"release":"rust v0.163.0","commit":"{sha}"}}"#),
        format!(r#"{{"release":"rust-v0.163.0\u001b[2J","commit":"{sha}"}}"#),
        format!(r#"{{"release":"{long}","commit":"{sha}"}}"#),
        format!(r#"{{"release":163,"commit":"{sha}"}}"#),
        r#"{"release":"rust-v0.163.0","commit":"0123456789ab"}"#.to_string(),
        format!(r#"{{"release":"rust-v0.163.0","commit":"{sha}0"}}"#),
        format!(
            r#"{{"release":"rust-v0.163.0","commit":"{}"}}"#,
            "g".repeat(40)
        ),
        r#"{"release":"rust-v0.163.0","commit":1}"#.to_string(),
        format!(r#"{{"release":"rust-v0.163.0","commit":"{sha}","commits_since_release":"241"}}"#),
        format!(r#"{{"release":"rust-v0.163.0","commit":"{sha}","commits_since_release":-1}}"#),
        format!(r#"{{"release":"rust-v0.163.0","commit":"{sha}","commits_since_release":2.5}}"#),
        format!(r#"{{"release":"rust-v0.163.0","commit":"{sha}","commits_since_release":null}}"#),
        format!(r#"{{"release":"rust-v0.163.0","commit":"{sha}","commit_date":"2026-10-20"}}"#),
        format!(
            r#"{{"release":"rust-v0.163.0","commit":"{sha}","commit_date":"2026-10-20T13:50:13+07:00"}}"#
        ),
        format!(
            r#"{{"release":"rust-v0.163.0","commit":"{sha}","commit_date":"2026-1O-20T13:50:13Z"}}"#
        ),
        format!(r#"{{"release":"rust-v0.163.0","commit":"{sha}","commit_date":20261020}}"#),
        format!(r#"{{"release":"rust-v0.163.0","commit":"{sha}","commit_date":null}}"#),
    ] {
        assert_eq!(parse_upstream(&upstream_record(&field)), None, "{field}");
    }
    let whole = upstream_record(&full_upstream());
    for text in [
        "not json",
        "",
        "[]",
        &whole[..whole.len() - 1],
        r#""upstream""#,
    ] {
        assert_eq!(parse_upstream(text), None, "{text}");
    }
}

#[test]
fn a_package_s_upstream_is_its_record_s_and_an_older_package_has_none() {
    let d = crate::testdir::dir("atpkg-alab-upstream-");
    let root = d.join("pkg");
    package(&root, "0.164.0-dev.12+gabc", b"x", true);
    assert_eq!(upstream_of(&root), None, "a record from before the field");
    assert_eq!(upstream_of_exe(&root.join("bin/codex")), None);
    std::fs::write(
        root.join(ALAB_BUILD_JSON),
        upstream_record(&full_upstream()),
    )
    .unwrap();
    let words = |u: Option<Upstream>| u.map(|u| u.words());
    let said = Some("based on upstream rust-v0.163.0+g0123456789ab".to_string());
    assert_eq!(words(upstream_of(&root)), said);
    assert_eq!(words(upstream_of_exe(&root.join("bin/codex"))), said);
    std::fs::write(root.join("bin/other"), b"x").unwrap();
    assert_eq!(
        upstream_of_exe(&root.join("bin/other")),
        None,
        "not the entrypoint"
    );
    #[cfg(unix)]
    {
        let link = d.join("link-to-codex");
        std::os::unix::fs::symlink(root.join("bin/codex"), &link).unwrap();
        assert_eq!(
            words(upstream_of_exe(&link)),
            said,
            "a link to the entrypoint"
        );
        let linked = d.join("linked-record");
        package(&linked, "0.164.0-dev.12+gabc", b"x", false);
        std::os::unix::fs::symlink(root.join(ALAB_BUILD_JSON), linked.join(ALAB_BUILD_JSON))
            .unwrap();
        assert_eq!(upstream_of(&linked), None, "a record through a link");
    }
    let vendor = d.join("vendor");
    package(&vendor, "0.164.0", b"x", false);
    std::fs::write(vendor.join("upstream.json"), full_upstream()).unwrap();
    assert_eq!(upstream_of(&vendor), None);
    for bad in [
        b"\xff\xfe{\"upstream\":{}}".to_vec(),
        format!(
            "{}{}",
            " ".repeat(MAX_PACKAGE_JSON_BYTES),
            upstream_record(&full_upstream())
        )
        .into_bytes(),
        br#"{"upstream":{"release":"rust-v0.163.0","commit":"#.to_vec(),
    ] {
        std::fs::write(root.join(ALAB_BUILD_JSON), &bad).unwrap();
        assert_eq!(
            upstream_of(&root),
            None,
            "{}",
            String::from_utf8_lossy(&bad)
        );
    }

    let l = scratch("upstream");
    let dir = store_build(&l, BUILD, true);
    assert_eq!(
        program_upstream(&l, "codex"),
        None,
        "a record from before the field"
    );
    std::fs::write(dir.join(ALAB_BUILD_JSON), upstream_record(&full_upstream())).unwrap();
    assert_eq!(words(program_upstream(&l, "codex")), said);
    assert_eq!(program_upstream(&l, "claude"), None);
}

#[test]
fn a_store_build_is_alab_by_its_marker_and_never_at_a_vendor_id() {
    let l = scratch("classify");
    store_build(&l, BUILD, true);
    assert!(build_is_alab(&l, "codex", BUILD));
    assert_eq!(active_alab_build(&l, "codex", ARM_MAC), Some(BUILD));
    let vendor_id = crate::vendor_direct::Version::parse("0.165.0")
        .unwrap()
        .build_id();
    let vdir = l.build_dir("codex", vendor_id);
    package(&vdir, "0.165.0", b"x", true);
    crate::store::mark_build_ready(&vdir).unwrap();
    assert!(!build_is_alab(&l, "codex", vendor_id));
    let legacy = l.build_dir("codex", 2_026_101_501);
    package(&legacy, "0.157.1", b"x", false);
    crate::store::mark_build_ready(&legacy).unwrap();
    assert!(!build_is_alab(&l, "codex", 2_026_101_501));
    assert_eq!(retained_alab_below(&l, "codex", BUILD + 1), vec![BUILD]);
    assert!(retained_alab_below(&l, "codex", BUILD).is_empty());
    assert!(!build_is_alab(&l, "claude", BUILD));
}

#[test]
fn the_source_over_a_store_reads_the_link_the_choice_the_pin_and_the_build() {
    let l = scratch("source");
    let good = parsed(&index_text("codex-alab", Some(BUILD + 1), ""));
    let live = parsed(&index_text("aterm", Some(2_026_101_501), ""));
    assert_eq!(
        source(&l, "codex", ARM_MAC, Some(&good)),
        Source::Index { build: BUILD + 1 }
    );
    assert_eq!(
        source(&l, "codex", ARM_MAC, Some(&live)),
        Source::VendorDirect
    );
    assert_eq!(source(&l, "codex", ARM_MAC, None), Source::VendorDirect);
    assert_eq!(
        source(&l, "codex", "x86_64-apple-darwin", Some(&good)),
        Source::VendorDirect
    );
    store_build(&l, BUILD, true);
    assert_eq!(
        source(&l, "codex", ARM_MAC, None),
        Source::KeepAlab { build: BUILD },
        "an index that does not verify keeps the ALab build"
    );
    assert_eq!(
        source(&l, "codex", ARM_MAC, Some(&live)),
        Source::KeepAlab { build: BUILD },
        "the legacy row never moves it to the vendor"
    );
    assert_eq!(
        source(&l, "codex", "x86_64-apple-darwin", Some(&good)),
        Source::VendorDirect
    );
    assert_eq!(active_alab_build(&l, "codex", "x86_64-apple-darwin"), None);
    assert!(index_owns(&l, "codex", ARM_MAC, Some(&good)));
    choose_vendor(&l, "codex").unwrap();
    assert!(vendor_chosen(&l, "codex"));
    assert_eq!(
        source(&l, "codex", ARM_MAC, Some(&good)),
        Source::VendorDirect
    );
    assert!(clear_vendor_choice(&l, "codex"));
    assert!(!clear_vendor_choice(&l, "codex"));
    assert_eq!(
        source(&l, "codex", ARM_MAC, Some(&good)),
        Source::Index { build: BUILD + 1 }
    );
    std::fs::write(vendor_choice_path(&l, "claude"), b"x").unwrap();
    assert!(!vendor_chosen(&l, "claude"));
    let ch = good.channel_for("stable", ARM_MAC).unwrap();
    assert_eq!(
        source_in(&l, &good, &ch, "codex", ARM_MAC),
        Source::Index { build: BUILD + 1 }
    );
}

#[test]
fn the_planner_takes_codex_only_where_the_rule_hands_it_over() {
    let l = scratch("plan");
    let good = parsed(&index_text("codex-alab", Some(BUILD), ""));
    let live = parsed(&index_text("aterm", Some(2_026_101_501), ""));
    let members = |index: &Index, triple: &str| -> Vec<String> {
        let ch = index.channel_for("stable", triple).unwrap();
        crate::apply::plan_groups_here(&l, index, &ch, triple)
            .into_iter()
            .flat_map(|g| g.members)
            .collect()
    };
    assert_eq!(members(&good, ARM_MAC), ["ay", "codex"]);
    assert_eq!(
        members(&live, ARM_MAC),
        ["ay"],
        "the legacy pin stays ignored"
    );
    assert_eq!(members(&good, "x86_64-apple-darwin"), ["ay"]);
    let ch = good.channel_for("stable", ARM_MAC).unwrap();
    let plain: Vec<String> = crate::plan_groups(&good, &ch)
        .into_iter()
        .flat_map(|g| g.members)
        .collect();
    assert_eq!(
        plain,
        ["ay"],
        "the index-only planner never takes a vendor program"
    );
    choose_vendor(&l, "codex").unwrap();
    assert_eq!(members(&good, ARM_MAC), ["ay"], "a person chose the vendor");
}

#[test]
fn an_alab_row_is_named_alab_build_and_a_vendor_row_its_vendor() {
    let alab =
        |state: &str| crate::state::source_words("codex", state, 0).starts_with("ALab build");
    let words = format!("ALab build 0.164.0-dev.12+gabc (build {BUILD})");
    for state in [
        crate::state::managed(BUILD, 72),
        crate::state::alab_kept(BUILD, &format!("{words}; kept")),
        crate::state::alab_kept(
            BUILD,
            &format!("{words}; rolled back from ALab build {}", BUILD + 1),
        ),
        crate::state::alab_kept(BUILD, "rolled back from a revoked build"),
    ] {
        assert!(alab(&state), "{state}");
    }
    for state in [
        crate::state::vendor_managed("0.165.0", "OpenAI"),
        crate::state::managed(2_026_092_301, 46),
        crate::state::managed(2_026_101_501, 71),
        crate::state::vendor_kept("2026101501", &format!("rolled back from {words}")),
        crate::state::vendor_kept("0.165.0", &words),
    ] {
        assert!(!alab(&state), "{state}");
    }
    assert!(
        !crate::state::source_words("claude", &crate::state::managed(BUILD, 72), 0)
            .starts_with("ALab build")
    );
    let l = scratch("words");
    store_build(&l, BUILD, true);
    assert_eq!(
        build_words(&l, "codex", BUILD),
        format!("ALab build 0.164.0-dev.{BUILD}+gabc (build {BUILD})")
    );
}

#[test]
fn a_package_directory_links_its_entrypoint() {
    let d = crate::testdir::dir("atpkg-alab-linkbin-");
    package(&d.join("pkg"), "0.164.0-dev.1+g0", b"x", true);
    assert_eq!(
        package_link_bin("codex", &d.join("pkg")),
        Some(PathBuf::from("bin/codex"))
    );
    assert_eq!(package_link_bin("claude", &d.join("pkg")), None);
    std::fs::create_dir_all(d.join("cargo")).unwrap();
    assert_eq!(package_link_bin("codex", &d.join("cargo")), None);
}

/// The last `exec '<path>' "$@"` line of a twin.
#[cfg(unix)]
fn exec_line(twin: &Path) -> String {
    std::fs::read_to_string(twin)
        .unwrap()
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap()
        .to_string()
}

#[cfg(unix)]
fn twin(l: &Layout) -> PathBuf {
    l.agent_shim(&crate::store::ToolName::new("codex").unwrap())
}

/// The symlink inode at `entry` (never followed).
#[cfg(unix)]
fn link_ino(entry: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(entry).unwrap().ino()
}

#[cfg(unix)]
const LAUNCH_PROBE: &[u8] = b"#!/bin/sh\nprintf '%s|%s|%s|%s|%s|%s\\n' \"$0\" \"${DISABLE_AUTOUPDATER-unset}\" \"${ATPKG_MANAGED_LAUNCH_PID-unset}\" \"${ATPKG_MANAGED_LAUNCH_PATH-unset}\" \"${ATPKG_MANAGED_LAUNCH_TARGET-unset}\" \"$$\" >> \"$ATPKG_LAUNCH_LOG\"\n";

#[cfg(unix)]
fn observed_launch(shim: &Path, log: &Path, target: &Path, count: usize, env: &str) {
    observed_launch_with_hint(shim, log, target, count, env, None);
}

#[cfg(unix)]
fn observed_launch_with_hint(
    shim: &Path,
    log: &Path,
    target: &Path,
    count: usize,
    env: &str,
    managed: Option<&Path>,
) {
    let mut command = aterm_uds::spawnfd::command("/bin/sh");
    command
        .arg(shim)
        .arg("--version")
        .env_clear()
        .env("ATPKG_LAUNCH_LOG", log);
    let result = aterm_uds::deadline::outputs_until(
        &mut [&mut command],
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .pop()
    .expect("one owned shell")
    .expect("owned shell completes");
    assert!(result.status.success(), "{result:?}");
    let written = std::fs::read_to_string(log).unwrap();
    assert_eq!(
        written.lines().count(),
        count,
        "one target ran per invocation"
    );
    let fields: Vec<_> = written.lines().last().unwrap().split('|').collect();
    assert_eq!(fields.len(), 6);
    assert_eq!(fields[0], target.to_str().unwrap());
    assert_eq!(fields[1], env);
    assert!(fields[5].parse::<u32>().is_ok_and(|pid| pid != 0));
    if let Some(managed) = managed {
        assert_eq!(
            fields[2], fields[5],
            "exec retains the exporting shell's pid"
        );
        assert_eq!(fields[3], managed.to_str().unwrap());
        assert_eq!(fields[4], target.to_str().unwrap());
    } else {
        assert_eq!(&fields[2..5], &["unset", "unset", "unset"]);
    }
}

#[cfg(unix)]
#[test]
fn an_alab_twin_execs_an_entry_its_own_directory_re_points() {
    let l = scratch("twin");
    let first = store_build_with(&l, BUILD, true, LAUNCH_PROBE);
    let env = crate::shim_env::ShimEnv::admit(&[
        "DISABLE_AUTOUPDATER=1".to_string(),
        "ATPKG_MANAGED_LAUNCH_EXTRA=kept".to_string(),
    ])
    .unwrap();
    crate::activate::install_shims_env(
        &l,
        &first,
        &["codex".to_string()],
        crate::Aliases::Off,
        &env,
    )
    .unwrap();
    let entry = crate::activate::exec_dir(&l, "codex").join("codex");
    let primary = l.shim(&crate::store::ToolName::new("codex").unwrap());
    let log = l.prefix.join("launches");
    let first_bytes = std::fs::read(first.join("bin/codex")).unwrap();
    observed_launch(&primary, &log, &entry, 1, "1");
    observed_launch(&twin(&l), &log, &entry, 2, "1");
    assert_eq!(crate::platform::shim_env_of(&primary), env);
    assert_eq!(
        crate::platform::resolve_shim(&primary),
        Some(first.join("bin/codex"))
    );
    assert_eq!(
        exec_line(&twin(&l)),
        format!("exec '{}' \"$@\"", entry.display())
    );
    assert_eq!(std::fs::read_link(&entry).unwrap(), first.join("bin/codex"));
    let body = std::fs::read_to_string(twin(&l)).unwrap();
    assert!(body.contains(crate::selfupdate::HIDDEN_VERB), "{body}");
    assert_eq!(
        crate::activate::twin_target(&l, "codex"),
        Some(first.join("bin/codex"))
    );
    assert!(crate::cli::agent_twin_current(&l, "codex", BUILD));
    crate::activate::sweep_agents_dir(&l);
    assert!(twin(&l).exists(), "the sweep keeps an ALab twin");
    let (before, ino) = (std::fs::read(twin(&l)).unwrap(), link_ino(&entry));
    let second = store_build_with(&l, BUILD + 1, true, LAUNCH_PROBE);
    crate::activate::install_shims_env(
        &l,
        &second,
        &["codex".to_string()],
        crate::Aliases::Off,
        &env,
    )
    .unwrap();
    observed_launch(&primary, &log, &entry, 3, "1");
    observed_launch(&twin(&l), &log, &entry, 4, "1");
    assert_eq!(std::fs::read(first.join("bin/codex")).unwrap(), first_bytes);
    assert_eq!(
        crate::platform::resolve_shim(&primary),
        Some(second.join("bin/codex"))
    );
    assert_eq!(std::fs::read(twin(&l)).unwrap(), before);
    assert_ne!(link_ino(&entry), ino, "the entry was re-laid in exec/");
    assert_eq!(
        std::fs::canonicalize(&entry).unwrap(),
        std::fs::canonicalize(second.join("bin/codex")).unwrap()
    );
    assert_eq!(
        crate::activate::twin_target(&l, "codex"),
        Some(second.join("bin/codex"))
    );
    assert!(crate::cli::agent_twin_current(&l, "codex", BUILD + 1));
    assert!(exe_is_alab(&entry));
    std::fs::remove_file(&entry).unwrap();
    observed_launch_with_hint(
        &primary,
        &log,
        &second.join("bin/codex"),
        5,
        "1",
        Some(&entry),
    );
    // The entry can exist but be unusable, or its parent can refuse the next lay.
    // Both raw routes retain the admitted logical entry, including a fallback twin.
    std::fs::write(&entry, b"not executable").unwrap();
    observed_launch_with_hint(
        &primary,
        &log,
        &second.join("bin/codex"),
        6,
        "1",
        Some(&entry),
    );
    std::fs::remove_file(&entry).unwrap();
    let exec_dir = crate::activate::exec_dir(&l, "codex");
    std::fs::remove_dir(&exec_dir).unwrap();
    std::fs::write(&exec_dir, b"cannot lay a child here").unwrap();
    crate::activate::reconcile_agents(&l);
    observed_launch_with_hint(
        &primary,
        &log,
        &second.join("bin/codex"),
        7,
        "1",
        Some(&entry),
    );
    observed_launch_with_hint(
        &twin(&l),
        &log,
        &second.join("bin/codex"),
        8,
        "1",
        Some(&entry),
    );
    std::fs::remove_file(&exec_dir).unwrap();
    let primary_before = std::fs::read(&primary).unwrap();
    let primary_ino = {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(&primary).unwrap().ino()
    };
    crate::activate::reconcile_agents(&l);
    observed_launch(&primary, &log, &entry, 9, "1");
    assert_eq!(
        crate::platform::shim_env_of(&primary),
        env,
        "unrelated persistent keys survive a read/render/reconcile round trip"
    );
    assert_eq!(std::fs::read(&primary).unwrap(), primary_before);
    {
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(std::fs::metadata(&primary).unwrap().ino(), primary_ino);
    }

    let vendor_id = crate::vendor_direct::Version::parse("0.165.0")
        .unwrap()
        .build_id();
    let vendor = l.build_dir("codex", vendor_id);
    package(&vendor, "0.165.0", LAUNCH_PROBE, false);
    crate::store::mark_build_ready(&vendor).unwrap();
    crate::activate::activate_build(&l, &vendor).unwrap();
    crate::activate::install_shims(&l, &vendor, &["codex".to_string()], crate::Aliases::Off)
        .unwrap();
    observed_launch(&primary, &log, &vendor.join("bin/codex"), 10, "unset");
    observed_launch(&twin(&l), &log, &vendor.join("bin/codex"), 11, "unset");
    assert_eq!(
        exec_line(&twin(&l)),
        format!("exec '{}' \"$@\"", vendor.join("bin/codex").display())
    );
    assert_eq!(
        std::fs::read_link(&entry).unwrap(),
        second.join("bin/codex")
    );

    let foreign = crate::testdir::dir("atpkg-alab-unmanaged-");
    package(&foreign, "0.164.0-dev.9+gforeign", LAUNCH_PROBE, true);
    crate::platform::install_shim_to(&primary, &foreign.join("bin/codex")).unwrap();
    let unmanaged = std::fs::read(&primary).unwrap();
    crate::activate::reconcile_agents(&l);
    assert_eq!(std::fs::read(&primary).unwrap(), unmanaged);
    assert!(!twin(&l).exists());
    assert_eq!(
        std::fs::read_link(&entry).unwrap(),
        second.join("bin/codex")
    );
    observed_launch(&primary, &log, &foreign.join("bin/codex"), 12, "unset");

    let v = scratch("twin-vendor");
    let dir = store_build(&v, BUILD, false);
    assert_eq!(
        exec_line(&twin(&v)),
        format!("exec '{}' \"$@\"", dir.join("bin/codex").display())
    );
    assert_eq!(
        crate::activate::twin_target(&v, "codex"),
        Some(dir.join("bin/codex"))
    );
    assert!(std::fs::symlink_metadata(crate::activate::exec_dir(&v, "codex")).is_err());
}

#[cfg(unix)]
#[test]
fn an_alab_dev_link_twin_execs_an_entry_a_relink_re_points() {
    let l = scratch("devlink");
    let d = crate::testdir::dir("atpkg-alab-devlink-pkgs-");
    let (a, b) = (d.join("a"), d.join("b"));
    package(&a, "0.164.0-dev.1+ga", LAUNCH_PROBE, true);
    package(&b, "0.164.0-dev.2+gb", LAUNCH_PROBE, true);
    let a = std::path::absolute(&a).unwrap();
    let b = std::path::absolute(&b).unwrap();
    crate::link(&l, "codex", &a, &[PathBuf::from("bin/codex")]).unwrap();
    let entry = crate::activate::exec_dir(&l, "codex").join("codex");
    let primary = l.shim(&crate::store::ToolName::new("codex").unwrap());
    let log = l.prefix.join("launches");
    let a_bytes = std::fs::read(a.join("bin/codex")).unwrap();
    observed_launch(&primary, &log, &entry, 1, "unset");
    observed_launch(&twin(&l), &log, &entry, 2, "unset");
    assert_eq!(
        crate::platform::resolve_shim(&primary),
        Some(a.join("bin/codex"))
    );
    assert_eq!(
        exec_line(&twin(&l)),
        format!("exec '{}' \"$@\"", entry.display())
    );
    assert_eq!(std::fs::read_link(&entry).unwrap(), a.join("bin/codex"));
    let body = std::fs::read_to_string(twin(&l)).unwrap();
    assert!(!body.contains(crate::selfupdate::HIDDEN_VERB), "{body}");
    assert_eq!(
        crate::activate::twin_target(&l, "codex"),
        Some(a.join("bin/codex"))
    );
    let (before, ino) = (std::fs::read(twin(&l)).unwrap(), link_ino(&entry));
    crate::link(&l, "codex", &b, &[PathBuf::from("bin/codex")]).unwrap();
    observed_launch(&primary, &log, &entry, 3, "unset");
    observed_launch(&twin(&l), &log, &entry, 4, "unset");
    assert_eq!(std::fs::read(a.join("bin/codex")).unwrap(), a_bytes);
    assert_eq!(
        crate::platform::resolve_shim(&primary),
        Some(b.join("bin/codex"))
    );
    assert_eq!(
        std::fs::read_link(&entry).unwrap(),
        b.join("bin/codex"),
        "the re-link re-points it"
    );
    assert_ne!(link_ino(&entry), ino, "by a rename in exec/");
    assert_eq!(std::fs::read(twin(&l)).unwrap(), before);
    assert_eq!(
        crate::activate::twin_target(&l, "codex"),
        Some(b.join("bin/codex"))
    );
    std::fs::remove_file(&entry).unwrap();
    observed_launch_with_hint(
        &primary,
        &log,
        &b.join("bin/codex"),
        5,
        "unset",
        Some(&entry),
    );
    crate::activate::reconcile_agents(&l);
    observed_launch(&primary, &log, &entry, 6, "unset");
    crate::activate::sweep_agents_dir(&l);
    assert!(twin(&l).exists());
    crate::linkmode::unlink(&l, "codex").unwrap();
    assert!(!primary.exists() && !twin(&l).exists());
    assert!(!crate::linkmode::is_linked(&l, "codex"));

    let v = scratch("devlink-plain");
    let checkout = std::path::absolute(d.join("cargo")).unwrap();
    std::fs::create_dir_all(checkout.join("target/release")).unwrap();
    std::fs::write(checkout.join("target/release/codex"), b"x").unwrap();
    crate::link(
        &v,
        "codex",
        &checkout,
        &[PathBuf::from("target/release/codex")],
    )
    .unwrap();
    assert_eq!(
        exec_line(&twin(&v)),
        format!(
            "exec '{}' \"$@\"",
            checkout.join("target/release/codex").display()
        )
    );
}

/// A fetcher serving one signed index.
#[derive(Default)]
pub(crate) struct Channel {
    index: Vec<u8>,
    pkgs: BTreeMap<u64, (Vec<u8>, Vec<u8>)>,
    archives: BTreeMap<String, Vec<u8>>,
    pub(crate) asked: std::cell::RefCell<Vec<String>>,
}

impl Fetcher for Channel {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        let (roster_bytes, roster_sig) = testkit::published_roster();
        Ok(vec![Candidate {
            label: "v0".into(),
            index_bytes: self.index.clone(),
            sig: testkit::sign(&testkit::MACHINE_SEED, &self.index),
            roster_bytes,
            roster_sig,
        }])
    }
    fn pkg_manifest(
        &self,
        repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.asked
            .borrow_mut()
            .push(format!("{repo}/{program}@{build}"));
        self.pkgs
            .get(&build)
            .cloned()
            .ok_or_else(|| "no such manifest".into())
    }
    fn download(&self, _repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        let bytes = self.archives.get(asset).ok_or("no such asset")?;
        std::fs::write(dest, bytes).map_err(|e| e.to_string())
    }
}

/// One fetcher for a pass.
#[cfg(unix)]
pub(crate) struct Both<'a> {
    pub(crate) index: &'a Channel,
    pub(crate) vendor: &'a crate::vendor_direct::lane::world::Fake,
}

#[cfg(unix)]
impl Fetcher for Both<'_> {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        self.index.index_candidates()
    }
    fn pkg_manifest(&self, r: &str, p: &str, b: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.index.pkg_manifest(r, p, b)
    }
    fn download(&self, r: &str, a: &str, d: &Path) -> Result<(), String> {
        self.index.download(r, a, d)
    }
    fn vendor_get(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        if_none_match: Option<&str>,
    ) -> Result<crate::flow::VendorGet, crate::flow::VendorFetchError> {
        self.vendor.vendor_get(program, url, cap, if_none_match)
    }
    fn vendor_head(
        &self,
        program: &str,
        url: &str,
        cap: u64,
        if_none_match: Option<&str>,
    ) -> Result<crate::flow::VendorGet, crate::flow::VendorFetchError> {
        self.vendor.vendor_head(program, url, cap, if_none_match)
    }
    fn vendor_content_length(
        &self,
        program: &str,
        url: &str,
    ) -> Result<u64, crate::flow::VendorFetchError> {
        self.vendor.vendor_content_length(program, url)
    }
    fn vendor_download(
        &self,
        program: &str,
        url: &str,
        dest: &Path,
        cap: u64,
    ) -> Result<(), crate::flow::VendorFetchError> {
        self.vendor.vendor_download(program, url, dest, cap)
    }
}

/// An ALab pack's archive ([`host_entry`]).
pub(crate) fn pack_archive(build: u64, exe: &[u8], alab: bool) -> Vec<u8> {
    pack_archive_recording(build, exe, alab.then_some(&br#"{"source":"0"}"#[..]))
}

/// [`pack_archive`] whose `alab-build.json` holds `record` (none: no record, so no ALab package).
fn pack_archive_recording(build: u64, exe: &[u8], record: Option<&[u8]>) -> Vec<u8> {
    let entry = host_entry();
    let manifest = package_json_naming(&format!("0.164.0-dev.{build}+gabc"), &entry);
    let mut entries: Vec<(&str, u8, &str, &[u8], u32)> = vec![
        ("bin/", b'5', "", &[][..], 0o755),
        (&entry, b'0', "", exe, 0o755),
        (PACKAGE_JSON, b'0', "", manifest.as_bytes(), 0o644),
    ];
    if let Some(record) = record {
        entries.push((ALAB_BUILD_JSON, b'0', "", record, 0o644));
    }
    crate::extract::fixtures::zstd_bytes(&crate::extract::fixtures::tar_bytes(&entries))
}

/// The bytes every darwin test pack runs: a copy of an Apple binary signed AD HOC, as
/// `alab/package.sh` signs codex-alab, which `codesign --verify --strict` passes; elsewhere
/// any bytes (no platform gate there).
pub(crate) fn signed_exe() -> Vec<u8> {
    if cfg!(target_os = "macos") {
        codesigned(&["--force", "--sign", "-"])
    } else {
        b"\x7fELF-test".to_vec()
    }
}

/// A copy of `/usr/bin/true` after `codesign <args>` ran on it.
fn codesigned(args: &[&str]) -> Vec<u8> {
    let d = crate::testdir::dir("atpkg-alab-exe-");
    let exe = d.join("codex");
    std::fs::copy("/usr/bin/true", &exe).unwrap();
    let out = aterm_uds::spawnfd::command(aterm_update_core::codesign::CODESIGN)
        .args(args)
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::read(&exe).unwrap()
}

/// The channel publishing codex `build` from `repo` with `archive` as its one asset.
pub(crate) fn channel(repo: &str, build: u64, archive: Vec<u8>) -> Channel {
    let asset = format!("codex-{build}.tar.zst");
    let probe = crate::testdir::dir("atpkg-alab-probe-");
    let file = probe.join(&asset);
    std::fs::write(&file, &archive).unwrap();
    let sha = crate::tree::file_sha256(&file).unwrap();
    let root = crate::extract::extract_tar_zst_tree(
        &file,
        &probe.join("tree"),
        1 << 30,
        10_000,
        crate::extract::ExtractOptions::default(),
        crate::extract::FrameChecksum::IfPresent,
    )
    .map_or_else(|_| "0".repeat(64), |t| t.root());
    let pkg = format!(
        "schema = 2\nprogram = \"codex\"\nversion = \"0.164.0-dev.{build}+gabc\"\n\
         build_number = {build}\nexposes = [\"codex\"]\n\
         [[artifact]]\ntarget = \"{ARM_MAC}\"\nkind = \"binary\"\nasset = \"{asset}\"\n\
         sha256 = \"{sha}\"\ntree_root = \"{root}\"\nsize = {}\n\
         [artifact.cost]\ndisk_installed = 1048576\n",
        archive.len()
    );
    let mut ch = Channel {
        index: index_text(repo, Some(build), "").into_bytes(),
        ..Channel::default()
    };
    ch.pkgs.insert(
        build,
        (
            pkg.clone().into_bytes(),
            testkit::sign(&testkit::MACHINE_SEED, pkg.as_bytes()),
        ),
    );
    ch.archives.insert(asset, archive);
    ch
}

fn install(
    l: &Layout,
    f: &dyn Fetcher,
    triple: &str,
) -> Result<crate::InstallReport, crate::FlowError> {
    crate::flow::install(
        f,
        l,
        &testkit::anchor(),
        &crate::flow::InstallRequest {
            channel: "stable",
            program: "codex",
            triple,
            installed: crate::ops::active_builds(l).get("codex").copied(),
        },
        crate::sig::BuildFloor::default(),
        testkit::NOW,
    )
}

#[test]
fn the_index_lane_installs_an_alab_pack_and_the_legacy_row_stays_vendor() {
    let l = scratch("flow");
    let f = channel(
        "codex-alab",
        BUILD,
        pack_archive(BUILD, &signed_exe(), true),
    );
    let report = install(&l, &f, ARM_MAC).expect("the ALab pack installs");
    assert_eq!(report.build, BUILD);
    assert!(
        report.vendor.is_none(),
        "an index install, not a vendor one"
    );
    assert_eq!(
        f.asked.borrow().as_slice(),
        [format!("codex-alab/codex@{BUILD}")]
    );
    assert_eq!(active_alab_build(&l, "codex", ARM_MAC), Some(BUILD));
    assert!(exe_is_alab(&l.build_dir("codex", BUILD).join(host_entry())));
    #[cfg(unix)]
    assert!(exec_line(&twin(&l)).contains("/store/codex/exec/codex"));

    for (label, repo, triple) in [
        ("legacy", "aterm", ARM_MAC),
        ("intel", "codex-alab", "x86_64-apple-darwin"),
    ] {
        let v = scratch(label);
        let f = channel(repo, BUILD, pack_archive(BUILD, &signed_exe(), true));
        let e = install(&v, &f, triple).expect_err("no vendor host answers this fetcher");
        assert!(
            matches!(
                e,
                crate::FlowError::VendorUnreachable(_)
                    | crate::FlowError::Vendor(_)
                    | crate::FlowError::VendorUnavailable(_)
            ),
            "{label}: {e:?}"
        );
        assert!(
            f.asked.borrow().is_empty(),
            "{label}: the index lane was asked"
        );
        assert!(crate::ops::active_builds(&v).is_empty(), "{label}");
    }
}

#[test]
fn the_pack_gate_refuses_a_pack_that_is_not_alab_or_not_signed() {
    let l = scratch("gate-marker");
    let f = channel(
        "codex-alab",
        BUILD,
        pack_archive(BUILD, &signed_exe(), false),
    );
    let e = install(&l, &f, ARM_MAC).expect_err("no build record");
    assert!(e.to_string().contains("not an ALab package"), "{e}");
    assert!(crate::ops::active_builds(&l).is_empty());
    assert!(!l.build_dir("codex", BUILD).exists());
    if cfg!(target_os = "macos") {
        let l = scratch("gate-unsigned");
        let exe = codesigned(&["--remove-signature"]);
        let f = channel("codex-alab", BUILD, pack_archive(BUILD, &exe, true));
        let e = install(&l, &f, ARM_MAC).expect_err("an unsigned Mach-O");
        assert!(e.to_string().contains("codesign --verify --strict"), "{e}");
        assert!(crate::ops::active_builds(&l).is_empty());
    }
}

#[test]
fn an_alab_pack_this_aterm_is_too_old_for_is_never_laid() {
    let this = aterm_version_words(this_aterm());
    for (label, record, refused) in [
        (
            "newer",
            r#"{"source":"0","aterm":{"min":"999.0.0"}}"#.to_string(),
            Some("999.0.0"),
        ),
        (
            "unread",
            r#"{"source":"0","aterm":{"min":"soon"}}"#.to_string(),
            Some("no shape"),
        ),
        (
            "this",
            format!(r#"{{"source":"0","aterm":{{"min":"{this}"}}}}"#),
            None,
        ),
    ] {
        let l = scratch(&format!("pairing-{label}"));
        let archive = pack_archive_recording(BUILD, &signed_exe(), Some(record.as_bytes()));
        let f = channel("codex-alab", BUILD, archive);
        match refused {
            Some(said) => {
                let e = install(&l, &f, ARM_MAC).expect_err(label);
                assert!(
                    e.to_string().contains("alab pairing") && e.to_string().contains(said),
                    "{label}: {e}"
                );
                assert!(crate::ops::active_builds(&l).is_empty(), "{label}");
                assert!(!l.build_dir("codex", BUILD).exists(), "{label}");
            }
            None => {
                let report = install(&l, &f, ARM_MAC).expect(label);
                assert_eq!(report.build, BUILD, "{label}");
                assert_eq!(active_alab_build(&l, "codex", ARM_MAC), Some(BUILD));
            }
        }
    }
}

#[test]
fn the_pairing_reads_the_records_aterm_min_and_names_no_floor_of_its_own() {
    let d = crate::testdir::dir("atpkg-alab-pairing-");
    let root = d.join("pkg");
    package(&root, "0.164.0-dev.1+gabc", b"#!/bin/sh\nexit 0\n", true);
    assert_eq!(pairing_floor(&root), Ok(None));
    for aterm in [(0, 0, 0), (0, 109, 9), (0, 110, 0), this_aterm()] {
        assert_eq!(pairs_with(&root, aterm), Ok(()), "{aterm:?}");
    }
    for (record, floor, below) in [
        (r#"{"aterm":{"min":"0.100.0"}}"#, (0, 100, 0), (0, 99, 99)),
        (
            r#"{"aterm":{"min":"0.112.3+g0123abc"}}"#,
            (0, 112, 3),
            (0, 112, 2),
        ),
    ] {
        std::fs::write(root.join(ALAB_BUILD_JSON), record).unwrap();
        assert_eq!(pairing_floor(&root), Ok(Some(floor)), "{record}");
        assert_eq!(pairs_with(&root, floor), Ok(()), "{record}");
        let e = pairs_with(&root, below).expect_err(record);
        assert!(
            e.contains(&format!("needs aterm {}", aterm_version_words(floor)))
                && e.contains(&aterm_version_words(below)),
            "{e}"
        );
    }
    for bad in [
        r#"{"aterm":"0.110.0"}"#,
        r#"{"aterm":{"min":"0.110"}}"#,
        "not json",
    ] {
        std::fs::write(root.join(ALAB_BUILD_JSON), bad).unwrap();
        assert!(pairing_floor(&root).is_err(), "{bad}");
        assert!(pairs_with(&root, (99, 0, 0)).is_err(), "{bad}");
    }
    std::fs::remove_file(root.join(ALAB_BUILD_JSON)).unwrap();
    assert!(
        pairs_with(&root, (99, 0, 0)).is_err(),
        "no record, no ALab package"
    );
    assert_eq!(parse_aterm_version("0.110.0"), Some((0, 110, 0)));
    assert_eq!(
        parse_aterm_version("0.110.0+g0123456789"),
        Some((0, 110, 0))
    );
    assert_eq!(parse_aterm_version("1.2.3-dev.4"), Some((1, 2, 3)));
    for bad in ["", "0.110", "0.110.0.1", "v0.110.0", "0.x.0", "0..0"] {
        assert_eq!(parse_aterm_version(bad), None, "{bad:?}");
    }
    assert_eq!(
        Some(this_aterm()),
        parse_aterm_version(env!("CARGO_PKG_VERSION")),
        "this aterm is the workspace's version"
    );
}

#[test]
fn the_producer_refuses_a_pairing_no_client_reads() {
    let d = crate::testdir::dir("atpkg-alab-producer-pairing-");
    let exposes = vec!["codex".to_string()];
    let this = aterm_version_words(this_aterm());
    for (label, record, refused) in [
        ("none", r#"{"source":"0"}"#.to_string(), false),
        (
            "this",
            format!(r#"{{"source":"0","aterm":{{"min":"{this}"}}}}"#),
            false,
        ),
        (
            "newer",
            r#"{"source":"0","aterm":{"min":"999.0.0"}}"#.to_string(),
            false,
        ),
        (
            "no object",
            r#"{"source":"0","aterm":"0.110.0"}"#.to_string(),
            true,
        ),
        (
            "unread",
            r#"{"source":"0","aterm":{"min":"soon"}}"#.to_string(),
            true,
        ),
    ] {
        let root = d.join(label.replace(' ', "-"));
        package_at(
            &root,
            "0.164.0-dev.1+gabc",
            &signed_exe(),
            true,
            &host_entry(),
        );
        std::fs::write(root.join(ALAB_BUILD_JSON), &record).unwrap();
        match crate::install::check_alab_pack(&root, &exposes) {
            Ok(()) => assert!(!refused, "{label}: the producer passed {record}"),
            Err(e) => {
                assert!(refused, "{label}: {e}");
                let e = e.to_string();
                assert!(
                    e.contains("alab pairing") && e.contains("no shape"),
                    "{label}: {e}"
                );
            }
        }
    }
}

#[test]
fn a_tampered_pack_or_signature_is_refused_and_nothing_is_laid() {
    let archive = pack_archive(BUILD, &signed_exe(), true);
    let mut flipped = channel("codex-alab", BUILD, archive.clone());
    let asset = format!("codex-{BUILD}.tar.zst");
    let bytes = flipped.archives.get_mut(&asset).unwrap();
    let at = bytes.len() / 2;
    bytes[at] ^= 0xff;
    let l = scratch("tamper-archive");
    let e = install(&l, &flipped, ARM_MAC).expect_err("a flipped byte");
    assert!(e.to_string().contains("sha256"), "{e}");
    assert!(crate::ops::active_builds(&l).is_empty());

    let mut forged = channel("codex-alab", BUILD, archive.clone());
    let (body, _) = forged.pkgs.remove(&BUILD).unwrap();
    let sig = testkit::sign(&testkit::OUTSIDER_SEED, &body);
    forged.pkgs.insert(BUILD, (body, sig));
    let l = scratch("tamper-pkg");
    install(&l, &forged, ARM_MAC).expect_err("a manifest no rostered machine signed");
    assert!(crate::ops::active_builds(&l).is_empty());

    struct Outsider(Channel);
    impl Fetcher for Outsider {
        fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
            let mut c = self.0.index_candidates()?;
            for c in &mut c {
                c.sig = testkit::sign(&testkit::OUTSIDER_SEED, &c.index_bytes);
            }
            Ok(c)
        }
        fn pkg_manifest(&self, r: &str, p: &str, b: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
            self.0.pkg_manifest(r, p, b)
        }
        fn download(&self, r: &str, a: &str, d: &Path) -> Result<(), String> {
            self.0.download(r, a, d)
        }
    }
    let outsider = Outsider(channel("codex-alab", BUILD, archive));
    let l = scratch("tamper-index");
    install(&l, &outsider, ARM_MAC).expect_err("no verified index, no vendor host");
    assert!(outsider.0.asked.borrow().is_empty());
    assert!(crate::ops::active_builds(&l).is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn the_strict_check_passes_a_valid_signature_and_refuses_a_modified_one() {
    let d = crate::testdir::dir("atpkg-alab-strict-");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let good = d.join("good");
    std::fs::write(&good, signed_exe()).unwrap();
    crate::install::codesign_strict_check(&good, deadline).expect("an ad hoc signature");
    let bad = d.join("bad");
    std::fs::write(&bad, codesigned(&["--remove-signature"])).unwrap();
    let e = crate::install::codesign_strict_check(&bad, deadline).expect_err("unsigned");
    assert!(e.is_verdict(), "{e}");
    let text = d.join("text");
    std::fs::write(&text, b"#!/bin/sh\nexit 0\n").unwrap();
    crate::install::codesign_strict_check(&text, deadline).expect_err("not code");
}

#[cfg(unix)]
#[test]
fn a_vendor_choice_never_writes_through_a_link() {
    let l = scratch("choice-link");
    choose_vendor(&l, "codex").expect("a new ordinary choice");
    assert!(vendor_chosen(&l, "codex"));
    let marker = vendor_choice_path(&l, "codex");
    let ordinary = std::fs::read(&marker).unwrap();
    choose_vendor(&l, "codex").expect("an already recorded ordinary choice");
    assert_eq!(std::fs::read(&marker).unwrap(), ordinary);
    assert!(clear_vendor_choice(&l, "codex"));

    let target = l.prefix.join("private-notes");
    let notes = b"fixture notes, not a vendor-choice marker\n";
    std::fs::write(&target, notes).unwrap();
    std::os::unix::fs::symlink(&target, &marker).unwrap();
    assert!(!vendor_chosen(&l, "codex"), "a link is no recorded choice");
    let result = choose_vendor(&l, "codex");
    assert_eq!(
        std::fs::read(&target).unwrap(),
        notes,
        "recording a choice must preserve the file the marker links to"
    );
    assert!(result.is_err(), "an existing linked marker must be refused");
    assert!(!vendor_chosen(&l, "codex"));
    assert_eq!(std::fs::read_link(&marker).unwrap(), target);
}
