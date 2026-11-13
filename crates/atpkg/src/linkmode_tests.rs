// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// `ay`'s shim as this platform names it (`ay.cmd` on Windows).
fn ay_shim(layout: &Layout) -> PathBuf {
    layout.shim(&crate::store::ToolName::new("ay").expect("a tool name"))
}

#[test]
fn guarded_link_replaces_only_the_still_selected_checkout() {
    let root = crate::testdir::dir("atpkg-guarded-link-");
    let layout = Layout {
        prefix: root.join("pkg"),
    };
    let old = checkout("guard-old", &["ay"]);
    let new = checkout("guard-new", &["ay"]);
    let manual = checkout("guard-manual", &["ay"]);
    let bins = vec![PathBuf::from("target/release/ay")];
    let mut guard = crate::lock::try_lock_store(&layout).unwrap();
    link(&layout, "ay", &old, &bins).unwrap();
    let alias = PathBuf::from(format!("{}/.", old.display()));
    let before = fs::read(layout.link_marker("ay")).unwrap();
    assert!(
        link_if_checkout(
            &mut guard,
            &layout,
            "ay",
            &new,
            &bins,
            &alias,
            &layout.prefix
        )
        .is_err()
    );
    assert_eq!(fs::read(layout.link_marker("ay")).unwrap(), before);
    let mut marker = read_marker_for_program(&layout, "ay").unwrap();
    marker.checkout = alias.display().to_string();
    write_marker(&layout.link_marker("ay"), &marker).unwrap();
    assert!(
        link_if_checkout(&mut guard, &layout, "ay", &new, &bins, &old, &layout.prefix).is_err()
    );
    link_if_checkout(
        &mut guard,
        &layout,
        "ay",
        &new,
        &bins,
        &alias,
        &layout.prefix,
    )
    .unwrap()
    .apply()
    .unwrap();
    assert_eq!(linked_checkout(&layout, "ay"), Some(new.to_path_buf()));
    link(&layout, "ay", &manual, &bins).unwrap();
    let marker = fs::read(layout.link_marker("ay")).unwrap();
    let shim = fs::read(ay_shim(&layout)).unwrap();
    assert!(
        link_if_checkout(&mut guard, &layout, "ay", &new, &bins, &old, &layout.prefix).is_err()
    );
    assert_eq!(fs::read(layout.link_marker("ay")).unwrap(), marker);
    assert_eq!(fs::read(ay_shim(&layout)).unwrap(), shim);
    let valid = read_marker_for_program(&layout, "ay").unwrap();
    for kind in ["schema", "program", "relative", "malformed"] {
        let mut invalid = valid.clone();
        match kind {
            "schema" => invalid.schema = 2,
            "program" => invalid.program = "other".into(),
            "relative" => invalid.checkout = "relative".into(),
            _ => {}
        }
        let bytes = if kind == "malformed" {
            "not a marker".into()
        } else {
            serialize_marker(&invalid).unwrap()
        };
        fs::write(layout.link_marker("ay"), bytes.as_bytes()).unwrap();
        assert!(!is_linked(&layout, "ay"), "{kind}");
        assert!(linked_checkout_checked(&layout, "ay").is_err(), "{kind}");
        assert!(
            link_if_checkout(
                &mut guard,
                &layout,
                "ay",
                &new,
                &bins,
                &manual,
                &layout.prefix
            )
            .is_err(),
            "{kind}"
        );
        assert_eq!(
            fs::read(layout.link_marker("ay")).unwrap(),
            bytes.as_bytes()
        );
        assert_eq!(fs::read(ay_shim(&layout)).unwrap(), shim);
    }
}

#[test]
fn guarded_link_requires_its_store_lock_and_original_bin_mapping() {
    let root = crate::testdir::dir("atpkg-guarded-owner-");
    let layout = Layout {
        prefix: root.join("pkg"),
    };
    let old = checkout("guard-mapping", &["ay", "aux"]);
    let new = checkout("guard-mapping-new", &["ay"]);
    let bins = vec![PathBuf::from("target/release/ay")];
    let both = vec![
        PathBuf::from("target/release/ay"),
        PathBuf::from("target/release/aux"),
    ];
    link(&layout, "ay", &old, &both).unwrap();
    let marker = fs::read(layout.link_marker("ay")).unwrap();
    let mut guard = crate::lock::try_lock_store(&layout).unwrap();
    assert_eq!(guard.layout().prefix, layout.prefix);
    assert!(
        link_if_checkout(&mut guard, &layout, "ay", &new, &bins, &old, &layout.prefix).is_err()
    );
    let shim = fs::read(ay_shim(&layout)).unwrap();
    assert!(
        link_if_checkout(&mut guard, &layout, "ay", &new, &both, &old, &layout.prefix).is_err()
    );
    assert_eq!(fs::read(layout.link_marker("ay")).unwrap(), marker);
    assert_eq!(fs::read(ay_shim(&layout)).unwrap(), shim);
    fs::write(new.join("target/release/aux"), b"fixture\n").unwrap();
    let outcome = link_if_checkout(&mut guard, &layout, "ay", &new, &both, &old, &layout.prefix)
        .unwrap()
        .apply()
        .unwrap();
    assert_eq!(outcome.linked, ["ay", "aux"]);
    assert_eq!(linked_checkout(&layout, "ay"), Some(new.to_path_buf()));
    unlink(&layout, "ay").unwrap();
    assert!(
        link_if_checkout(&mut guard, &layout, "ay", &new, &bins, &new, &layout.prefix).is_err()
    );
    assert!(!layout.link_marker("ay").exists());
    assert!(!ay_shim(&layout).exists());
}

#[test]
fn guarded_link_prefix_refuses_a_different_store() {
    let root = crate::testdir::dir("atpkg-guarded-prefix-");
    let layout = Layout {
        prefix: root.join("a"),
    };
    let other = Layout {
        prefix: root.join("b"),
    };
    let mut guard = crate::lock::try_lock_store(&layout).unwrap();
    let mut other_guard = crate::lock::try_lock_store(&other).unwrap();
    assert!(check_link_prefix(&guard.layout(), &layout.prefix).is_ok());
    assert!(check_link_prefix(&guard.layout(), &other.prefix).is_err());
    let old = checkout("guard-prefix-old", &["ay"]);
    let new = checkout("guard-prefix-new", &["ay"]);
    let bins = [PathBuf::from("target/release/ay")];
    link(&layout, "ay", &old, &bins).unwrap();
    let marker = fs::read(layout.link_marker("ay")).unwrap();
    assert!(
        link_if_checkout(
            &mut other_guard,
            &layout,
            "ay",
            &new,
            &bins,
            &old,
            &layout.prefix
        )
        .is_err()
    );
    assert!(link_if_checkout(&mut guard, &other, "ay", &new, &bins, &old, &layout.prefix).is_err());
    assert_eq!(fs::read(layout.link_marker("ay")).unwrap(), marker);
}
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-link-{label}-"))
}

/// `bin/<name>` for a name the test knows is admissible.
fn shim_of(layout: &Layout, name: &str) -> PathBuf {
    layout.shim(&crate::store::ToolName::new(name).unwrap())
}

/// A fake checkout with `target/release/<bins>`.
fn checkout(label: &str, bins: &[&str]) -> crate::testdir::Dir {
    let d = crate::testdir::dir(&format!("atpkg-checkout-{label}-"));
    fs::create_dir_all(d.join("target/release")).unwrap();
    for b in bins {
        fs::write(d.join("target/release").join(b), b"#!/bin/true\n").unwrap();
    }
    d
}

#[test]
fn the_exe_suffix_is_stripped_without_ever_slicing_mid_character() {
    assert_eq!(strip_exe_suffix("日本語", ".exe"), "日本語");
    assert_eq!(strip_exe_suffix("日本語.exe", ".exe"), "日本語");
    assert_eq!(strip_exe_suffix("日本語.EXE", ".exe"), "日本語");
    assert_eq!(strip_exe_suffix("é", ".exe"), "é");
    assert_eq!(strip_exe_suffix("éxe", ".exe"), "éxe");
    assert_eq!(strip_exe_suffix("é.exe", ".exe"), "é");
    assert_eq!(strip_exe_suffix("ay.exe", ".exe"), "ay");
    assert_eq!(strip_exe_suffix("ay.ExE", ".exe"), "ay");
    assert_eq!(strip_exe_suffix("ay", ".exe"), "ay");
    assert_eq!(strip_exe_suffix("ay", ""), "ay");
    assert_eq!(strip_exe_suffix("ay.exe", ""), "ay.exe");
    assert_eq!(strip_exe_suffix(".exe", ".exe"), ".exe");
    assert_eq!(bin_tool_name(Path::new("target/release/ay")), Some("ay"));
}

#[test]
fn nothing_to_link_names_the_unbuilt_path_and_the_refused_name() {
    let l = layout("nobins");
    let co = checkout("nobins", &[]);
    let bins = [
        PathBuf::from("target/release/ay"),
        PathBuf::from("target/release/sudo"),
    ];
    let err = link(&l, "ay", &co, &bins).unwrap_err();
    let want = co.join("target/release/ay");
    assert!(
        matches!(&err, LinkError::NoBins { missing, refused }
            if *missing == [want.clone()] && *refused == ["sudo".to_string()]),
        "{err:?}"
    );
    assert!(!shim_of(&l, "ay").exists());
}
#[cfg(unix)]
#[test]
fn a_link_whose_marker_cannot_be_written_rolls_back_and_leaves_no_temp() {
    let l = layout("markerfail");
    let co = checkout("markerfail", &["ay"]);
    let bins = [PathBuf::from("target/release/ay")];
    fs::create_dir_all(l.links_dir().join("ay").join("occupied")).unwrap();

    let out = link(&l, "ay", &co, &bins);
    assert!(
        out.is_err(),
        "a marker that cannot be written must fail the link"
    );
    assert!(
        !shim_of(&l, "ay").exists(),
        "the shim this call created must be rolled back — an UNTRACKED dev shim is \
         worse than no link at all"
    );
    let litter: Vec<String> = fs::read_dir(l.links_dir())
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| n.starts_with(".link.tmp-"))
        .collect();
    assert!(
        litter.is_empty(),
        "no marker temp may survive a failed write, found {litter:?}"
    );
    assert!(
        !linked_programs(&l)
            .iter()
            .any(|p| p.starts_with(".link.tmp-")),
        "a leaked temp must never be reported as a dev-linked program"
    );

    let _ = fs::remove_dir_all(&l.prefix);
    let _ = fs::remove_dir_all(&co);
}

#[cfg(unix)]
#[test]
fn a_failed_link_restores_the_shim_it_overwrote() {
    let l = layout("overwrite");
    let co = checkout("overwrite", &["ay"]);
    let bins = [PathBuf::from("target/release/ay")];
    fs::create_dir_all(l.bin_dir()).unwrap();
    let store_bin = l.prefix.join("store-build").join("ay");
    fs::create_dir_all(store_bin.parent().unwrap()).unwrap();
    fs::write(&store_bin, b"#!/bin/true\n").unwrap();
    let shim = shim_of(&l, "ay");
    crate::platform::install_shim_to(&shim, &store_bin).unwrap();
    let before = fs::read(&shim).unwrap();
    let mode_before = fs::metadata(&shim).unwrap().permissions().mode() & 0o777;

    fs::create_dir_all(l.links_dir().join("ay").join("occupied")).unwrap();

    let out = link(&l, "ay", &co, &bins);
    assert!(
        out.is_err(),
        "a marker that cannot be written must fail the link"
    );
    assert_eq!(
        fs::read(&shim).unwrap(),
        before,
        "the OVERWRITTEN store shim must be restored byte for byte, never left \
         pointing into the dev checkout"
    );
    assert_eq!(
        fs::metadata(&shim).unwrap().permissions().mode() & 0o777,
        mode_before,
        "the restored shim keeps its mode"
    );
    assert_eq!(
        crate::platform::resolve_shim(&shim).as_deref(),
        Some(store_bin.as_path()),
        "the restored shim forwards to the STORE build, never to the checkout"
    );
    assert!(
        !is_linked(&l, "ay"),
        "no marker, so no dev link is recorded"
    );
    let litter: Vec<String> = fs::read_dir(l.bin_dir())
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| n.contains(".atpkg-rollback-"))
        .collect();
    assert!(
        litter.is_empty(),
        "the rollback must leave no temp behind, found {litter:?}"
    );

    let _ = fs::remove_dir_all(&l.prefix);
    let _ = fs::remove_dir_all(&co);
}

#[test]
fn link_and_unlink_round_trip() {
    let l = layout("rt");
    let co = checkout("rt", &["ay", "ny"]);
    let bins = [
        PathBuf::from("target/release/ay"),
        PathBuf::from("target/release/ny"),
    ];
    let out = link(&l, "ay", &co, &bins).unwrap();
    assert_eq!(out.linked, vec!["ay".to_string(), "ny".to_string()]);
    assert_eq!(
        crate::platform::resolve_shim(&shim_of(&l, "ay")).unwrap(),
        co.join("target/release/ay")
    );
    assert!(is_linked(&l, "ay"));
    #[cfg(unix)]
    {
        let mode = fs::metadata(l.link_marker("ay"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    unlink(&l, "ay").unwrap();
    assert!(!is_linked(&l, "ay"));
    assert!(
        fs::symlink_metadata(shim_of(&l, "ay")).is_err(),
        "shim removed"
    );
    assert!(
        fs::symlink_metadata(shim_of(&l, "ny")).is_err(),
        "shim removed"
    );
    let _ = fs::remove_dir_all(&l.prefix);
    let _ = fs::remove_dir_all(&co);
}

#[test]
fn link_and_unlink_sweep_the_front_of_path_agents_twin() {
    let l = layout("agents-twin");
    let co = checkout("agents-twin", &["claude"]);
    let claude = crate::store::ToolName::new("claude").unwrap();
    let store_bin = l.build_dir("claude", 2_026_091_001).join("bin");
    fs::create_dir_all(&store_bin).unwrap();
    let target = store_bin.join(claude.exe_file());
    fs::write(&target, b"#!/bin/true\n").unwrap();
    let lay_twin = || {
        l.ensure_dir(&l.agents_dir()).unwrap();
        crate::platform::install_shim_to(&l.agent_shim(&claude), &target).unwrap();
    };
    l.ensure_dir(&l.bin_dir()).unwrap();
    crate::platform::install_shim_to(&shim_of(&l, "claude"), &target).unwrap();
    lay_twin();

    let bins = [PathBuf::from("target/release/claude")];
    link(&l, "claude", &co, &bins).unwrap();
    assert_eq!(
        crate::platform::resolve_shim(&shim_of(&l, "claude")).unwrap(),
        co.join("target/release/claude"),
        "fixture: bin/claude is the dev link"
    );
    assert_eq!(
        crate::platform::resolve_shim(&l.agent_shim(&claude)),
        Some(co.join("target/release/claude")),
        "the twin at the front of PATH runs the dev link"
    );
    assert_eq!(
        crate::platform::resolve_shim(&shim_of(&l, "claude")),
        Some(co.join("target/release/claude")),
        "bin/ stays the link as laid"
    );
    #[cfg(unix)]
    assert!(
        !fs::read_to_string(l.agent_shim(&claude))
            .unwrap()
            .contains("__selfupdate"),
        "a dev link's twin hands no verb to atpkg's self-update"
    );
    crate::activate::reconcile_agents(&l);
    crate::activate::sweep_agents_dir(&l);
    assert!(fs::symlink_metadata(l.agent_shim(&claude)).is_ok());
    let stray = checkout("agents-twin-stray", &["claude"]);
    let stray_bin = stray.join("target/release/claude");
    crate::platform::install_shim_to(&shim_of(&l, "claude"), &stray_bin).unwrap();
    crate::platform::install_shim_to(&l.agent_shim(&claude), &stray_bin).unwrap();
    crate::activate::sweep_agents_dir(&l);
    assert!(
        fs::symlink_metadata(l.agent_shim(&claude)).is_err(),
        "a twin onto an unrecorded checkout is swept"
    );
    crate::activate::reconcile_agents(&l);
    assert!(
        fs::symlink_metadata(l.agent_shim(&claude)).is_err(),
        "and never laid"
    );
    crate::platform::install_shim_to(&shim_of(&l, "claude"), &co.join("target/release/claude"))
        .unwrap();
    let _ = fs::remove_dir_all(&stray);

    lay_twin();
    unlink(&l, "claude").unwrap();
    assert!(
        fs::symlink_metadata(shim_of(&l, "claude")).is_err(),
        "fixture: the dev shim is gone"
    );
    assert!(
        fs::symlink_metadata(l.agent_shim(&claude)).is_err(),
        "agents/ does not outlive the bin/ shim that vouched for it"
    );
    let _ = fs::remove_dir_all(&l.prefix);
    let _ = fs::remove_dir_all(&co);
}

#[test]
fn sensitive_bin_name_is_refused_a_link() {
    let l = layout("sensitive");
    let co = checkout("sensitive", &["ay", "ssh"]);
    let bins = [
        PathBuf::from("target/release/ay"),
        PathBuf::from("target/release/ssh"),
    ];
    let out = link(&l, "ay", &co, &bins).unwrap();
    assert_eq!(out.linked, vec!["ay".to_string()]);
    assert_eq!(out.refused, vec!["ssh".to_string()]);
    assert!(crate::store::ToolName::new("ssh").is_none());
    assert!(
        fs::symlink_metadata(
            l.bin_dir()
                .join(format!("ssh{}", crate::platform::SHIM_SUFFIX))
        )
        .is_err(),
        "sensitive name never shimmed"
    );
    let _ = fs::remove_dir_all(&l.prefix);
    let _ = fs::remove_dir_all(&co);
}

#[test]
fn unlink_only_removes_links_into_its_checkout() {
    let l = layout("scoped");
    let co = checkout("scoped", &["ay"]);
    link(&l, "ay", &co, &[PathBuf::from("target/release/ay")]).unwrap();
    let store_target = l.build_dir("ay", 18).join("bin/ay");
    fs::create_dir_all(store_target.parent().unwrap()).unwrap();
    fs::write(&store_target, b"#!/bin/true\n").unwrap();
    crate::platform::install_shim_to(&shim_of(&l, "ay"), &store_target).unwrap();
    unlink(&l, "ay").unwrap();
    assert_eq!(
        crate::platform::resolve_shim(&shim_of(&l, "ay")).unwrap(),
        store_target
    );
    let _ = fs::remove_dir_all(&l.prefix);
    let _ = fs::remove_dir_all(&co);
}

#[test]
fn link_preserves_a_pre_existing_pin() {
    let l = layout("pin");
    crate::pin::set_pinned(&l, "ay", true).unwrap();
    let before = fs::read(l.prefix.join("pins")).unwrap();
    let co = checkout("pin", &["ay"]);
    link(&l, "ay", &co, &[PathBuf::from("target/release/ay")]).unwrap();
    unlink(&l, "ay").unwrap();
    let after = fs::read(l.prefix.join("pins")).unwrap();
    assert_eq!(
        before, after,
        "pin file is byte-identical across a link→unlink cycle"
    );
    assert!(crate::pin::is_pinned(&l, "ay").unwrap());
    let _ = fs::remove_dir_all(&l.prefix);
    let _ = fs::remove_dir_all(&co);
}

#[test]
fn refresh_relinks_newly_built_bins() {
    let l = layout("refresh");
    let co = checkout("refresh", &["ay"]);
    let bins = [
        PathBuf::from("target/release/ay"),
        PathBuf::from("target/release/ny"),
    ];
    let out = link(&l, "ay", &co, &bins).unwrap();
    assert_eq!(out.linked, vec!["ay".to_string()], "ny not built yet");
    fs::write(co.join("target/release/ny"), b"#!/bin/true\n").unwrap();
    let out2 = refresh(&l, "ay").unwrap();
    assert!(
        out2.linked.contains(&"ny".to_string()),
        "refresh picks up the new bin"
    );
    assert_eq!(
        crate::platform::resolve_shim(&shim_of(&l, "ny")).unwrap(),
        co.join("target/release/ny")
    );
    let _ = fs::remove_dir_all(&l.prefix);
    let _ = fs::remove_dir_all(&co);
}

#[test]
fn link_marker_bin_count_is_rejected_before_shim_mutation() {
    let l = layout("bin-cap");
    let co = checkout("bin-cap", &["ay"]);
    let bins = vec![PathBuf::from("target/release/ay"); MAX_LINK_MARKER_BINS + 1];
    assert!(link(&l, "ay", &co, &bins).is_err());
    assert!(
        crate::platform::resolve_shim(&shim_of(&l, "ay")).is_none(),
        "an unrecordable request must not mutate shims"
    );
    let _ = fs::remove_dir_all(&l.prefix);
    let _ = fs::remove_dir_all(&co);
}

#[test]
fn linked_program_enumeration_accepts_exact_cap_and_rejects_one_more() {
    let l = layout("entry-cap");
    fs::create_dir_all(l.links_dir()).unwrap();
    for index in 0..MAX_LINKED_PROGRAMS {
        fs::write(l.links_dir().join(format!("p{index:03}")), b"x").unwrap();
    }
    assert_eq!(
        linked_programs_checked(&l).unwrap().len(),
        MAX_LINKED_PROGRAMS
    );
    fs::write(l.links_dir().join("overflow"), b"x").unwrap();
    let error = linked_programs_checked(&l).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        linked_programs(&l).is_empty(),
        "compatibility wrapper fails closed instead of returning a partial set"
    );
    let _ = fs::remove_dir_all(&l.prefix);
}

#[cfg(unix)]
#[test]
fn fifo_and_retargetable_link_markers_are_explicit_errors() {
    use std::os::unix::ffi::OsStrExt as _;

    let l = layout("marker-special");
    fs::create_dir_all(l.links_dir()).unwrap();
    let marker = l.link_marker("ay");
    let marker_c = std::ffi::CString::new(marker.as_os_str().as_bytes()).unwrap();
    // SAFETY: `marker_c` is a live NUL-terminated path in our private fixture.
    assert_eq!(unsafe { libc::mkfifo(marker_c.as_ptr(), 0o600) }, 0);
    let fifo_error = linked_checkout_checked(&l, "ay").unwrap_err();
    assert_eq!(fifo_error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!is_linked(&l, "ay"));
    fs::remove_file(&marker).unwrap();

    let first = l.prefix.join("first.toml");
    let second = l.prefix.join("second.toml");
    let marker_text = |checkout: &str| {
        aterm_toml::to_string(&LinkMarker {
            schema: 1,
            program: "ay".to_string(),
            checkout: checkout.to_string(),
            bins: Vec::new(),
            linked_at: String::new(),
        })
        .unwrap()
    };
    fs::write(&first, marker_text("/first")).unwrap();
    fs::write(&second, marker_text("/second")).unwrap();
    std::os::unix::fs::symlink(&first, &marker).unwrap();
    assert!(linked_checkout_checked(&l, "ay").is_err());
    fs::remove_file(&marker).unwrap();
    std::os::unix::fs::symlink(&second, &marker).unwrap();
    assert!(
        linked_checkout_checked(&l, "ay").is_err(),
        "retargeting the link never makes a link-like marker admissible"
    );
    let _ = fs::remove_dir_all(&l.prefix);
}

#[cfg(unix)]
#[test]
fn link_directory_symlink_is_rejected_instead_of_followed() {
    let l = layout("directory-link");
    let foreign = layout("directory-link-foreign");
    fs::create_dir_all(foreign.links_dir()).unwrap();
    fs::write(foreign.links_dir().join("ay"), b"foreign").unwrap();
    std::os::unix::fs::symlink(foreign.links_dir(), l.links_dir()).unwrap();
    let error = linked_programs_checked(&l).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    let _ = fs::remove_dir_all(&l.prefix);
    let _ = fs::remove_dir_all(&foreign.prefix);
}

#[test]
fn link_rejects_dot_or_dash_programs_and_non_relative_bins() {
    let l = layout("dot-dash-and-rel-bins");
    let co = checkout("dot-dash-co", &["ay"]);
    for bad_prog in [".link.tmp-123", ".hidden", "-flag", ""] {
        assert!(matches!(
            link(&l, bad_prog, &co, &[PathBuf::from("target/release/ay")]),
            Err(LinkError::BadName(_))
        ));
        assert!(!is_linked(&l, bad_prog));
    }
    for bad_bin in [
        PathBuf::from("../outside/ay"),
        PathBuf::from("/abs/path/ay"),
        PathBuf::from(""),
    ] {
        assert!(
            link(&l, "ay", &co, std::slice::from_ref(&bad_bin)).is_err(),
            "non-relative bin {} must be rejected",
            bad_bin.display()
        );
    }

    link(&l, "ay", &co, &[PathBuf::from("target/release/ay")]).unwrap();
    fs::write(l.links_dir().join(".link.tmp-999-0"), b"partial").unwrap();
    fs::write(l.links_dir().join(".DS_Store"), b"junk").unwrap();
    assert_eq!(linked_programs_checked(&l).unwrap(), vec!["ay".to_string()]);

    let mut invalid = read_marker_for_program(&l, "ay").unwrap();
    invalid.bins = vec!["../outside/ay".to_string()];
    write_marker(&l.link_marker("ay"), &invalid).unwrap();
    assert!(!is_linked(&l, "ay"));

    let _ = fs::remove_dir_all(&l.prefix);
    let _ = fs::remove_dir_all(&co);
}
