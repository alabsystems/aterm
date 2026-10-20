// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The source rule, the marker, the store window, the twin an ALab build gets and the index
//! lane end to end, each with the negative control that keeps today's vendor lane.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;
use crate::flow::Fetcher;
use crate::select::Candidate;
use crate::sig::testkit;

const ARM_MAC: &str = "aarch64-apple-darwin";
pub(crate) const BUILD: u64 = 2_026_101_601;

/// Write `root` as a Codex package: layout 1, entrypoint `bin/codex` holding `exe`, and
/// `alab-build.json` when `alab`.
fn package(root: &Path, version: &str, exe: &[u8], alab: bool) {
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin/codex"), exe).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            root.join("bin/codex"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    std::fs::write(root.join(PACKAGE_JSON), package_json(version)).unwrap();
    if alab {
        std::fs::write(root.join(ALAB_BUILD_JSON), br#"{"source":"0"}"#).unwrap();
    }
}

fn package_json(version: &str) -> String {
    package_json_naming(version, "bin/codex")
}

/// [`package_json`] with `entry` its entrypoint.
fn package_json_naming(version: &str, entry: &str) -> String {
    format!(
        r#"{{"layoutVersion":1,"version":"{version}","target":"aarch64-apple-darwin","entrypoint":"{entry}"}}"#
    )
}

/// `bin/codex` as this host's shim runs it (`bin/codex.exe` on Windows): what a pack the
/// index lane installs here holds, and its manifest names.
fn host_entry() -> String {
    format!("bin/codex{}", crate::platform::EXE_SUFFIX)
}

/// A complete, active store build of codex at `build`, an ALab package when `alab`.
fn store_build(l: &Layout, build: u64, alab: bool) -> PathBuf {
    let dir = l.build_dir("codex", build);
    package(
        &dir,
        &format!("0.164.0-dev.{build}+gabc"),
        b"#!/bin/sh\nexit 0\n",
        alab,
    );
    crate::store::mark_build_ready(&dir).unwrap();
    crate::activate::activate_build(l, &dir).unwrap();
    crate::activate::install_shims(l, &dir, &["codex".to_string()], crate::Aliases::Off).unwrap();
    dir
}

fn scratch(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-alab-{label}-"))
}

// ------------------------------------------------------------------- the rule, pure

/// Every arm of the source rule, in its order: a link beats everything, a person's vendor
/// choice beats the channel, an index pin beats a kept build, and nothing at all is the
/// vendor lane exactly as before.
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

/// The store window: above every legacy pin of the vendor's bytes (the live index's
/// `codex = 2026101501` on repo `aterm` among them), below every vendor id; both edges out.
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

/// A signed-shape index naming `codex` from `repo`, its stable channel pinning `pin` (or
/// nothing), with `extra` appended to the channel table.
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

/// Only a pin from ALab's repo, on a target ALab builds for, inside the window and
/// unrevoked routes here. The negative controls are today's facts: the live index's legacy
/// row (`codex = 2026101501` on repo `aterm`), no pin, another target.
#[test]
fn a_pin_routes_only_from_alab_repo_on_alab_targets() {
    let good = parsed(&index_text("codex-alab", Some(BUILD), ""));
    assert_eq!(index_pin(&good, "codex", ARM_MAC), Some(BUILD));
    // The live index as it stands: a legacy vendor manifest on the `aterm` repo.
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
    // Programs ALab does not build are never routed, whatever the index says.
    let claude = parsed(
        &index_text("codex-alab", Some(BUILD), "").replace("[programs.codex]", "[programs.claude]"),
    );
    assert_eq!(index_pin(&claude, "claude", ARM_MAC), None);
    assert_eq!(index_pin(&good, "ay", ARM_MAC), None);
}

/// A pin outside the window, yanked, or under its floor names nothing here: the installed
/// build stays as the rest of the rule decides.
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
    // A per-target overlay is the pin this target reads.
    let overlaid = parsed(&index_text(
        "codex-alab",
        None,
        &format!("pin_by_target = {{ \"{ARM_MAC}\" = {{ codex = {BUILD} }} }}\n"),
    ));
    assert_eq!(index_pin(&overlaid, "codex", ARM_MAC), Some(BUILD));
    assert_eq!(index_pin(&overlaid, "codex", "x86_64-apple-darwin"), None);
}

// ------------------------------------------------------------------- the marker

/// The marker: layout 1, an entrypoint inside the root that is a regular file, and
/// `alab-build.json` a regular file beside it. Each missing half reads not-ALab.
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
    // The vendor's own package: no build record.
    let vendor = d.join("vendor");
    package(&vendor, "0.164.0", b"x", false);
    assert!(!is_alab_package(&vendor));
    // A build record that is not a regular file.
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
    // No entrypoint file.
    let no_entry = d.join("no-entry");
    package(&no_entry, "0.1.0-dev.1+g0", b"x", true);
    std::fs::remove_file(no_entry.join("bin/codex")).unwrap();
    assert!(!is_alab_package(&no_entry));
    // Manifests the reader refuses.
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
    // An empty entrypoint names the root itself, never a regular file.
    std::fs::write(
        alab.join(PACKAGE_JSON),
        r#"{"layoutVersion":1,"entrypoint":""}"#,
    )
    .unwrap();
    assert!(!is_alab_package(&alab));
}

/// The executable form reads the package root its entrypoint names, from the binary or its
/// directory's parent, through links to it, and never a stray manifest above that names
/// another file.
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

/// A store build is ALab's by its marker alone, for a program ALab builds, at an id that is
/// not a vendor one — never by its version.
#[test]
fn a_store_build_is_alab_by_its_marker_and_never_at_a_vendor_id() {
    let l = scratch("classify");
    store_build(&l, BUILD, true);
    assert!(build_is_alab(&l, "codex", BUILD));
    assert_eq!(active_alab_build(&l, "codex", ARM_MAC), Some(BUILD));
    // The same tree under a vendor id is a vendor build, whatever it carries.
    let vendor_id = crate::vendor_direct::Version::parse("0.165.0")
        .unwrap()
        .build_id();
    let vdir = l.build_dir("codex", vendor_id);
    package(&vdir, "0.165.0", b"x", true);
    crate::store::mark_build_ready(&vdir).unwrap();
    assert!(!build_is_alab(&l, "codex", vendor_id));
    // A legacy index build of the vendor's bytes (no record) is not ALab's.
    let legacy = l.build_dir("codex", 2_026_101_501);
    package(&legacy, "0.157.1", b"x", false);
    crate::store::mark_build_ready(&legacy).unwrap();
    assert!(!build_is_alab(&l, "codex", 2_026_101_501));
    assert_eq!(retained_alab_below(&l, "codex", BUILD + 1), vec![BUILD]);
    assert!(retained_alab_below(&l, "codex", BUILD).is_empty());
    // A program ALab does not build is never classified ALab.
    assert!(!build_is_alab(&l, "claude", BUILD));
}

// ------------------------------------------------------------------- the rule over a store

/// The rule over a real store: an active ALab build with no pin is kept, the pin takes it to
/// the index lane, the person's choice and a link come first, and a vendor-only machine is
/// the vendor lane under the live index.
#[test]
fn the_source_over_a_store_reads_the_link_the_choice_the_pin_and_the_build() {
    let l = scratch("source");
    let good = parsed(&index_text("codex-alab", Some(BUILD + 1), ""));
    let live = parsed(&index_text("aterm", Some(2_026_101_501), ""));
    // Nothing installed: the pin, else the vendor.
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
    // On a target ALab does not build for, its build cannot run: the vendor lane judges it
    // as it judged any build before this table (a prefix carried to an Intel Mac).
    assert_eq!(
        source(&l, "codex", "x86_64-apple-darwin", Some(&good)),
        Source::VendorDirect
    );
    assert_eq!(active_alab_build(&l, "codex", "x86_64-apple-darwin"), None);
    assert!(index_owns(&l, "codex", ARM_MAC, Some(&good)));
    // A person's choice, and taking it back.
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
    // A choice is per ALab-built program; another program's file means nothing.
    std::fs::write(vendor_choice_path(&l, "claude"), b"x").unwrap();
    assert!(!vendor_chosen(&l, "claude"));
    // The channel the planners hold reads the same.
    let ch = good.channel_for("stable", ARM_MAC).unwrap();
    assert_eq!(
        source_in(&l, &good, &ch, "codex", ARM_MAC),
        Source::Index { build: BUILD + 1 }
    );
}

/// The planner hands codex to the index lane only where the rule does; the vendor programs
/// it does not own stay out, as before ([`crate::plan_groups`] is unchanged).
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

/// Whose build a row names: ALab's for a row the index lane pinned in the window or one
/// [`crate::state::alab_kept`] wrote (kept, or rolled back onto), the vendor's otherwise —
/// the live index's legacy pin (`2026101501`, index 71) and a row whose build is not ALab's
/// among the controls.
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

/// `[packages.links] codex = "<pkgdir>"` links the package's entrypoint; a cargo checkout
/// and a program ALab does not build keep the default.
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

// ------------------------------------------------------------------- the twin

/// The last `exec '<path>' "$@"` line of a twin: what codex-alab's live upgrade and the
/// harness read.
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

/// The symlink inode at `entry` (never followed): a new one means a rename landed there.
#[cfg(unix)]
fn link_ino(entry: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(entry).unwrap().ino()
}

/// An index-sourced ALab build's twin execs `store/codex/exec/codex`, the stable path whose
/// OWN directory takes the rename when the next build lands — what codex-alab's live
/// upgrade watches (argv0's parent, non-recursively). It carries the self-update block,
/// reads back as the build it runs, survives the sweep, and needs no re-lay when the next
/// build lands. A primary that turns to the vendor's bytes (a person's choice) never
/// re-points it, so a running codex-alab never crosses onto them. The vendor control: a
/// non-ALab build's twin execs its store path, byte for byte as before.
#[cfg(unix)]
#[test]
fn an_alab_twin_execs_an_entry_its_own_directory_re_points() {
    let l = scratch("twin");
    let first = store_build(&l, BUILD, true);
    let entry = crate::activate::exec_dir(&l, "codex").join("codex");
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
    // The next build: the entry is replaced by a rename in its own directory (a new link
    // inode there), argv0 now resolves to the new build's file, and the twin's bytes stay.
    let second = store_build(&l, BUILD + 1, true);
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
    // The harness's reader resolves the stable path to ALab bytes.
    assert!(exe_is_alab(&entry));

    // A person's vendor build becomes the primary: its twin names the vendor build, and the
    // entry a running codex-alab watches still names the ALab build.
    let vendor_id = crate::vendor_direct::Version::parse("0.165.0")
        .unwrap()
        .build_id();
    let vendor = l.build_dir("codex", vendor_id);
    package(&vendor, "0.165.0", b"#!/bin/sh\nexit 0\n", false);
    crate::store::mark_build_ready(&vendor).unwrap();
    crate::activate::activate_build(&l, &vendor).unwrap();
    crate::activate::install_shims(&l, &vendor, &["codex".to_string()], crate::Aliases::Off)
        .unwrap();
    assert_eq!(
        exec_line(&twin(&l)),
        format!("exec '{}' \"$@\"", vendor.join("bin/codex").display())
    );
    assert_eq!(
        std::fs::read_link(&entry).unwrap(),
        second.join("bin/codex")
    );

    // Negative control: the vendor's bytes (no record) get today's twin, and no entry.
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

/// AD-4: a dev link of an ALab package execs the same `store/codex/exec/codex` entry, which
/// a re-link re-points by a rename in its own directory, so a running codex-alab sees its
/// launch path name another build; the twin carries no self-update block (a link is the
/// developer's). The control: a dev link of anything else gets today's twin.
#[cfg(unix)]
#[test]
fn an_alab_dev_link_twin_execs_an_entry_a_relink_re_points() {
    let l = scratch("devlink");
    let d = crate::testdir::dir("atpkg-alab-devlink-pkgs-");
    let (a, b) = (d.join("a"), d.join("b"));
    package(&a, "0.164.0-dev.1+ga", b"a", true);
    package(&b, "0.164.0-dev.2+gb", b"b", true);
    let a = std::path::absolute(&a).unwrap();
    let b = std::path::absolute(&b).unwrap();
    crate::link(&l, "codex", &a, &[PathBuf::from("bin/codex")]).unwrap();
    let entry = crate::activate::exec_dir(&l, "codex").join("codex");
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
    crate::activate::sweep_agents_dir(&l);
    assert!(twin(&l).exists());

    // Negative control: a cargo checkout's link.
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

// ------------------------------------------------------------------- the index lane

/// A fetcher serving one signed index, signed pkg manifests and archives, recording which
/// repo each manifest was asked of.
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

/// One fetcher for a pass: the ALab channel's index, manifests and packs from `index`, and
/// every vendor document and payload from `vendor` (the vendor lane's test world). Unix,
/// as its one user (cli's `alab_tests`) is.
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

/// An ALab pack's archive: `codex-package.json`, `alab-build.json` and `bin/codex` holding
/// `exe`, as `alab/package.sh` lays them ([`host_entry`]).
pub(crate) fn pack_archive(build: u64, exe: &[u8], alab: bool) -> Vec<u8> {
    let entry = host_entry();
    let manifest = package_json_naming(&format!("0.164.0-dev.{build}+gabc"), &entry);
    let mut entries: Vec<(&str, u8, &str, &[u8], u32)> = vec![
        ("bin/", b'5', "", &[][..], 0o755),
        (&entry, b'0', "", exe, 0o755),
        (PACKAGE_JSON, b'0', "", manifest.as_bytes(), 0o644),
    ];
    if alab {
        entries.push((ALAB_BUILD_JSON, b'0', "", br#"{"source":"0"}"#, 0o644));
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

/// P1 in the client: a fresh store under an index pinning ALab's codex installs the pack
/// through the index lane — the manifest asked of ALab's repo, the pack gate passed, the
/// shim and the twin laid. The negative controls take the vendor lane, as today, and never
/// ask the index lane for a manifest: the live index's legacy row, and another target.
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

/// P4 in the client: a pack that is not an ALab package is refused by the pack gate and
/// nothing is laid; on macOS so is a pack whose Mach-O does not verify.
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

/// P4's tamper cases on an ALab pack: a flipped byte in the archive, a manifest signed by a
/// key on no roster, and an index signed by one — each refused with nothing laid.
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

    // An index signed off the roster pins nothing: the vendor lane, never the pack.
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

/// The real strict check on real bytes: an ad hoc signature passes (codex-alab's form), the
/// same binary unsigned is a verdict against it, and a text file is no pass.
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

/// A person's choice may create its own marker, but never write through a link to another
/// file. Both the marker and the linked target below belong to this exclusive fixture.
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
