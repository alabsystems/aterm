// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::activate::{activate_build, install_shims};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn tool(name: &str) -> ToolName {
    ToolName::new(name).unwrap()
}

#[cfg(unix)]
use aterm_uds::deadline::once_released;

fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-ops-{label}-"))
}

fn install(layout: &Layout, program: &str, build: u64) -> PathBuf {
    let dir = layout.build_dir(program, build);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(
        dir.join("bin").join(tool(program).exe_file()),
        b"#!/bin/true\n",
    )
    .unwrap();
    install_shims(
        layout,
        &dir,
        &[program.to_string()],
        crate::activate::Aliases::Off,
    )
    .unwrap();
    activate_build(layout, &dir).unwrap();
    crate::store::mark_build_ready(&dir).unwrap();
    dir
}

/// An interrupted uninstall (^C, SIGTERM, an EACCES in the walk) leaves no `<n>.ready`
/// marker beside a half-emptied tree.
#[cfg(unix)]
#[test]
fn an_interrupted_uninstall_leaves_no_build_still_vouched_for() {
    let l = layout("uninstall-interrupted");
    let keep = install(&l, "trust", 8590);
    let doomed = install(&l, "trust", 8595);
    assert!(crate::store::build_is_complete(&keep));
    assert!(crate::store::build_is_complete(&doomed));
    std::fs::set_permissions(keep.join("bin"), std::fs::Permissions::from_mode(0o500)).unwrap();
    let outcome = uninstall(&l, "trust");
    let aborted = keep.join("bin").join(tool("trust").exe_file()).is_file();
    std::fs::set_permissions(keep.join("bin"), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        aborted && outcome.is_err(),
        "the fixture must make the removal abort partway, or it proves nothing: \
         aborted={aborted} outcome={outcome:?}"
    );
    for (n, dir) in [(8590u64, &keep), (8595, &doomed)] {
        assert!(
            !crate::store::build_is_complete(dir),
            "build {n} must not still be vouched for after an aborted uninstall"
        );
    }
    assert!(
        !list_installed(&l).iter().any(|(p, _)| p == "trust"),
        "and nothing reports trust as installed — so rollback cannot select the wreck"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn partial_build_without_marker_is_not_listed() {
    let l = layout("partial");
    let dir = l.build_dir("ay", 18);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin/ay"), b"partial").unwrap();
    assert!(!crate::store::build_is_complete(&dir));
    assert!(
        list_installed(&l).is_empty(),
        "partial (marker-less) build is not installed"
    );
    crate::store::mark_build_ready(&dir).unwrap();
    assert_eq!(list_installed(&l), vec![("ay".into(), 18)]);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn store_build_of_is_anchored_to_the_prefix() {
    let prefix = Path::new("/p");
    assert_eq!(
        store_build_of(prefix, Path::new("/p/store/ay/18/bin/ay")),
        Some(("ay".to_string(), 18))
    );
    assert_eq!(
        store_build_of(prefix, Path::new("/p/store/ay/18")),
        Some(("ay".to_string(), 18))
    );
    assert_eq!(
        store_build_of(prefix, Path::new("/other/store/ay/18")),
        None
    );
    assert_eq!(
        program_build_of_target(Path::new("/other/store/ay/18")),
        Some(("ay".to_string(), 18)),
        "the unanchored parser is why the anchored one has to exist"
    );
    assert_eq!(store_build_of(prefix, Path::new("/p/store/ay")), None);
    assert_eq!(store_build_of(prefix, Path::new("/p/store/ay/head")), None);
    assert_eq!(store_build_of(prefix, Path::new("/p/store/../ay/18")), None);

    let canonical = Path::new("/p/store/trust/18.noindex/bin/trustc");
    assert_eq!(
        store_build_of(prefix, canonical),
        Some(("trust".to_string(), 18)),
        "a build laid aside, as a canonical path names it"
    );
    assert_eq!(
        program_build_of_target(canonical),
        Some(("trust".to_string(), 18))
    );
    for name in [
        ".noindex",
        "18.noindex.noindex",
        "18.noindexx",
        "+18",
        "018",
        "+18.noindex",
    ] {
        let path = Path::new("/p/store/trust").join(name);
        assert_eq!(store_build_of(prefix, &path), None, "{name}");
        assert_eq!(program_build_of_target(&path), None, "{name}");
    }
}

#[cfg(unix)]
#[test]
fn relative_store_links_use_the_actual_build_sidecar_and_presence() {
    use std::os::unix::fs::symlink;

    let l = layout("relative-store-link");
    let physical = l.prefix.join("store/ay/17.noindex");
    std::fs::create_dir_all(physical.join("bin")).unwrap();
    std::fs::create_dir_all(l.bin_dir()).unwrap();
    let file = physical.join("bin/ay");
    std::fs::write(&file, b"original target").unwrap();
    symlink("17.noindex", l.prefix.join("store/ay/17")).unwrap();
    let declared = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    crate::shim_env::write_sidecar(&l.build_dir("ay", 17), &declared).unwrap();
    let raw = Path::new("../store/ay/17/bin/ay");
    symlink(raw, l.shim(&tool("ay"))).unwrap();
    let target = l.bin_dir().join(raw);
    let selected = managed(&l, "ay").unwrap();
    assert_eq!(selected.target, target);
    assert_eq!(selected.env, declared);
    assert_eq!(which(&l, "ay"), Some(target.clone()));
    assert_eq!(
        store_build_of(&l.prefix, &target),
        Some(("ay".to_string(), 17))
    );
    assert_eq!(
        store_build_of(&l.prefix.join("bin/.."), &file),
        Some(("ay".to_string(), 17))
    );
    assert_eq!(active_build_of(&l, "ay"), Some(17));
    assert_eq!(active_builds(&l).get("ay"), Some(&17));

    std::fs::remove_file(&file).unwrap();
    assert_eq!(which(&l, "ay"), Some(target.clone()));
    assert_eq!(store_build_of(&l.prefix, &target), None);
    assert_eq!(active_build_of(&l, "ay"), None);
    assert!(!active_builds(&l).contains_key("ay"));
}

#[cfg(unix)]
#[test]
fn dot_parent_classification_never_borrows_a_lexically_named_store_sidecar() {
    use std::os::unix::fs::symlink;

    let l = layout("relative-store-escape");
    let local = l.build_dir("ay", 17);
    std::fs::create_dir_all(local.join("bin")).unwrap();
    std::fs::write(local.join("bin/ay"), b"wrong local target").unwrap();
    let declared = crate::shim_env::ShimEnv::admit(&["AY_QUIET=1".to_string()]).unwrap();
    crate::shim_env::write_sidecar(&local, &declared).unwrap();
    std::fs::create_dir_all(l.bin_dir()).unwrap();
    let foreign = crate::testdir::dir("atpkg-native-link-foreign-");
    std::fs::create_dir_all(foreign.join("deep/nested")).unwrap();
    let foreign_bin = foreign.join("store/ay/17/bin");
    std::fs::create_dir_all(&foreign_bin).unwrap();
    std::fs::write(foreign_bin.join("ay"), b"actual foreign target").unwrap();
    symlink(foreign.join("deep/nested"), l.bin_dir().join("slot")).unwrap();
    let raw = Path::new("slot/../../store/ay/17/bin/ay");
    symlink(raw, l.shim(&tool("ay"))).unwrap();
    let selected = managed(&l, "ay").unwrap();
    assert_eq!(selected.target, l.bin_dir().join(raw));
    assert_eq!(
        std::fs::read(&selected.target).unwrap(),
        b"actual foreign target"
    );
    assert_eq!(store_build_of(&l.prefix, &selected.target), None);
    assert_eq!(selected.env, crate::shim_env::ShimEnv::NONE);
    // A parent traversal after the apparent build is not a store member,
    // either, even though the old shape-only parser had already found it.
    assert_eq!(
        store_build_of(&l.prefix, &local.join("../../../../missing")),
        None
    );
}

#[test]
fn a_shim_pointing_at_a_deleted_build_is_not_live() {
    let l = layout("dangling");
    install(&l, "ay", 17);
    assert_eq!(
        active_builds(&l).get("ay").copied(),
        Some(17),
        "precondition: a real install is live"
    );
    assert_eq!(active_build_of(&l, "ay"), Some(17));
    std::fs::remove_dir_all(l.build_dir("ay", 17)).unwrap();
    assert!(
        crate::platform::resolve_shim(&l.shim(&tool("ay"))).is_some(),
        "the shim itself must still resolve — otherwise this test proves nothing"
    );
    assert!(
        !active_builds(&l).contains_key("ay"),
        "a shim whose target no longer exists must not be reported live"
    );
    assert_eq!(active_build_of(&l, "ay"), None);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// A target that is there but cannot be stat'd (EACCES, EPERM, EIO) is still live: only
/// the permission to look is removed, so the answer must not change.
#[cfg(unix)]
#[test]
fn a_shim_whose_target_cannot_be_stat_ed_is_still_live() {
    let l = layout("unstattable");
    install(&l, "ay", 17);
    assert_eq!(
        active_builds(&l).get("ay").copied(),
        Some(17),
        "precondition: a real install is live"
    );
    let target = crate::platform::resolve_shim(&l.bin_dir().join("ay")).unwrap();
    let holder = target.parent().unwrap().to_path_buf();
    let saved = std::fs::metadata(&holder).unwrap().permissions();
    std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o000)).unwrap();
    let stat = std::fs::metadata(&target);
    if stat.is_ok() {
        std::fs::set_permissions(&holder, saved).unwrap();
        let _ = std::fs::remove_dir_all(&l.prefix);
        aterm_uds::host::could_not_run(
            "a stat through a mode-0 folder succeeded: root, or a file system that ignores modes",
        );
    }
    assert_eq!(
        stat.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied,
        "the fixture must fail the stat for the reason under test"
    );
    let live = active_builds(&l).get("ay").copied();
    let live_one = active_build_of(&l, "ay");
    std::fs::set_permissions(&holder, saved).unwrap();
    let _ = std::fs::remove_dir_all(&l.prefix);
    assert_eq!(
        live,
        Some(17),
        "a target we merely could not LOOK at is not a target that is gone"
    );
    assert_eq!(live_one, Some(17));
}

#[test]
fn active_builds_reflects_the_shim_not_a_staged_inactive_build() {
    let l = layout("active");
    install(&l, "ay", 17);
    let staged = l.build_dir("ay", 18);
    std::fs::create_dir_all(staged.join("bin")).unwrap();
    std::fs::write(staged.join("bin/ay"), b"#!/bin/true\n").unwrap();
    crate::store::mark_build_ready(&staged).unwrap();

    assert!(list_installed(&l).contains(&("ay".to_string(), 18)));
    assert_eq!(active_builds(&l).get("ay").copied(), Some(17));
    assert_eq!(active_build_of(&l, "ay"), Some(17));
    assert_eq!(active_build_of(&l, "nope"), None);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn active_tools_lists_only_this_builds_shims() {
    let l = layout("active-tools");
    install(&l, "ay", 18);
    install(&l, "ny", 9);
    assert_eq!(active_tools(&l, "ay", 18), vec![tool("ay")]);
    assert_eq!(active_tools(&l, "ny", 9), vec![tool("ny")]);
    assert!(active_tools(&l, "ay", 17).is_empty());
    assert!(active_tools(&l, "nope", 1).is_empty());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn list_installed_reads_only_the_canonical_build_name() {
    let l = layout("canonical-build-names");
    install(&l, "ay", 18);
    for lookalike in ["+19", "019"] {
        let dir = l.prefix.join("store").join("ay").join(lookalike);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(
            dir.join("bin").join(tool("ay").exe_file()),
            b"#!/bin/true\n",
        )
        .unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
        assert!(
            crate::store::build_is_complete(&dir),
            "{lookalike} must look every bit as complete as a real build, or the \
             fixture proves nothing"
        );
    }
    assert_eq!(
        list_installed(&l),
        vec![("ay".into(), 18)],
        "a build-number lookalike is not an installed build"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[cfg(unix)]
#[test]
fn sdk_custody_refuses_uninstall_before_shims_or_payload_mutate() {
    let l = layout("sdk-custody-uninstall");
    let build = install(&l, "ay", 18);
    install(&l, "ny", 9);
    let subject = crate::lease::Subject::build("ay", 18).unwrap();
    let lease =
        crate::lease::take(&l.prefix, &subject, "SDK reader", std::time::Duration::ZERO).unwrap();
    let child = lease.try_clone_descriptor().unwrap();
    drop(lease);
    assert_eq!(
        uninstall(&l, "ay").unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(which(&l, "ay").is_some() && crate::store::build_is_complete(&build));
    drop(child);
    once_released(|| uninstall(&l, "ay"));
    assert!(which(&l, "ay").is_none() && !build.exists());
    assert!(which(&l, "ny").is_some());
}

#[cfg(unix)]
#[test]
fn sdk_custody_ordinary_discard_and_matching_gc_guard_conserve_the_sdk() {
    let l = layout("sdk-custody-discard");
    let build = install(&l, "ay", 18);
    let subject = crate::lease::Subject::build("ay", 18).unwrap();
    let lease =
        crate::lease::take(&l.prefix, &subject, "SDK reader", std::time::Duration::ZERO).unwrap();
    let child = lease.try_clone_descriptor().unwrap();
    drop(lease);
    crate::store::discard_build(&build);
    assert!(crate::store::build_is_complete(&build));
    let wrong = crate::compat::sdk_mutation_guard(&l.prefix, "ay", 19).unwrap();
    crate::store::discard_build_reclaimed(&build, &wrong);
    assert!(crate::store::build_is_complete(&build));
    drop(wrong);
    drop(child);
    let guard = once_released(|| crate::compat::sdk_mutation_guard(&l.prefix, "ay", 18));
    crate::store::discard_build_reclaimed(&build, &guard);
    assert!(
        !build.exists(),
        "the actual original GC guard must not be recursively acquired"
    );
}

#[test]
fn sdk_custody_keeps_unrepresentable_non_sdk_names_working() {
    let l = layout("sdk-custody-ordinary-name");
    let build = install(&l, "a.b", 18);
    assert!(crate::lease::Subject::build("a.b", 18).is_none());
    uninstall(&l, "a.b").unwrap();
    assert!(!build.exists());
    let build = install(&l, "go.tool", 19);
    crate::store::discard_build(&build);
    assert!(!build.exists());
}

#[test]
fn uninstall_removes_shim_store_and_channel_link() {
    let l = layout("uninstall");
    install(&l, "ay", 18);
    install(&l, "ny", 9);
    assert!(which(&l, "ay").is_some());

    uninstall(&l, "ay").unwrap();
    assert!(which(&l, "ny").is_some(), "another program is untouched");
    assert!(l.build_dir("ny", 9).exists());

    assert!(which(&l, "ay").is_none(), "shim removed");
    assert!(!l.build_dir("ay", 18).exists(), "store tree reclaimed");
    assert!(
        !l.prefix.join("store/ay").exists(),
        "program store dir removed"
    );
    assert!(std::fs::symlink_metadata(l.program_current("ay")).is_err());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn uninstall_rejects_path_traversal_names() {
    let l = layout("uninstall-traversal");

    let name = format!("atpkg-victim-do-not-delete-{}", std::process::id());
    let victim = l.prefix.parent().unwrap().join(&name);
    let _ = std::fs::remove_dir_all(&victim);
    std::fs::create_dir_all(victim.join("keep")).unwrap();

    let traversal = format!("../../{name}");
    let err = uninstall(&l, &traversal).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(
        victim.join("keep").exists(),
        "out-of-prefix directory must NOT be deleted by a traversal program name"
    );

    let err = uninstall(&l, "..").unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(
        l.prefix.exists(),
        "managed prefix must survive `uninstall ..`"
    );

    for bad in ["", ".", "a/b", "a\\b", "x\0y"] {
        assert_eq!(
            uninstall(&l, bad).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    for drive in ["C:", "C:x", "C:.."] {
        let escapes = !l.prefix.join("store").join(drive).starts_with(&l.prefix);
        assert_eq!(
            escapes,
            cfg!(windows),
            "{drive:?}: the join leaves the prefix"
        );
        assert_eq!(uninstall_name_shape(drive).is_err(), escapes, "{drive:?}");
    }

    let _ = std::fs::remove_dir_all(&victim);
    let _ = std::fs::remove_dir_all(&l.prefix);
}

/// [`exec_env`] is the build's declaration when it has one, whatever the shim as laid
/// says; else the shim's own exports (add-only); nothing for a plain shim over a build
/// declaring none, a name with no shim, or a name no shim could have.
#[cfg(unix)]
#[test]
fn exec_env_is_what_the_build_declares_else_what_the_shim_exports() {
    let l = layout("exec-env");
    let dir = install(&l, "ay", 17);
    let none = crate::shim_env::ShimEnv::NONE;
    let laid = crate::shim_env::ShimEnv::admit(&["AY_QUIET=1".to_string()]).unwrap();
    let declared = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".to_string()]).unwrap();
    assert_eq!(
        exec_env(&l, "ay"),
        none,
        "a plain shim over a build declaring nothing"
    );

    crate::activate::install_tools_env(
        &l,
        &dir,
        &[tool("ay")],
        crate::activate::Aliases::Off,
        &laid,
    )
    .unwrap();
    assert_eq!(
        exec_env(&l, "ay"),
        laid,
        "no declaration: the shim's own exports stand"
    );

    crate::shim_env::write_sidecar(&dir, &declared).unwrap();
    assert_eq!(
        exec_env(&l, "ay"),
        declared,
        "a declaration wins over the shim as laid"
    );
    crate::activate::install_tools(&l, &dir, &[tool("ay")], crate::activate::Aliases::Off).unwrap();
    assert_eq!(
        crate::platform::shim_env_of(&l.shim(&tool("ay"))),
        none,
        "fixture: the drifted shape — the shim laid plain"
    );
    assert_eq!(exec_env(&l, "ay"), declared, "…and over a shim laid plain");

    assert_eq!(exec_env(&l, "absent"), none);
    assert_eq!(exec_env(&l, "../ay"), none);
    let _ = std::fs::remove_dir_all(&l.prefix);
}
