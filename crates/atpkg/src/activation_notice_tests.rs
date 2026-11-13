// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use super::*;
use crate::store::ToolName;

#[test]
fn only_a_final_claude_target_change_replaces_the_marker() {
    use std::os::unix::fs::MetadataExt as _;

    let prefix = crate::testdir::dir("atpkg-claude-activation-notice-");
    let layout = Layout {
        prefix: prefix.clone(),
    };
    prepare(&layout);
    let marker = marker_path(&layout);
    note_if_changed(&layout, &[None, None, None, None]);
    assert!(!marker.exists(), "an unchanged absent twin emits no hint");

    let build = layout.build_dir("claude", 42);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::write(build.join("bin/claude"), b"#!/bin/sh\nexit 0\n").unwrap();
    let tool = ToolName::new("claude").unwrap();
    std::fs::create_dir_all(layout.agents_dir()).unwrap();
    crate::platform::install_twin_to_env(
        &layout.agent_shim(&tool),
        &build.join("bin/claude"),
        &crate::shim_env::ShimEnv::NONE,
        "",
    )
    .unwrap();
    assert_eq!(claude_target(&layout), Some(build.join("bin/claude")));
    note_if_changed(&layout, &[None, None, None, None]);
    let first = std::fs::metadata(&marker).unwrap().ino();
    note_if_changed(&layout, &agent_targets(&layout));
    assert_eq!(std::fs::metadata(&marker).unwrap().ino(), first);

    let newer = layout.build_dir("claude", 43);
    std::fs::create_dir_all(newer.join("bin")).unwrap();
    std::fs::write(newer.join("bin/claude"), b"#!/bin/sh\nexit 0\n").unwrap();
    let before = agent_targets(&layout);
    crate::platform::install_twin_to_env(
        &layout.agent_shim(&tool),
        &newer.join("bin/claude"),
        &crate::shim_env::ShimEnv::NONE,
        "",
    )
    .unwrap();
    note_if_changed(&layout, &before);
    assert_ne!(std::fs::metadata(&marker).unwrap().ino(), first);
    let _ = std::fs::remove_dir_all(&prefix);
}

#[test]
fn a_codex_twin_change_replaces_the_marker_too() {
    let prefix = crate::testdir::dir("atpkg-codex-activation-notice-");
    let layout = Layout {
        prefix: prefix.clone(),
    };
    prepare(&layout);
    let tool = ToolName::new("codex").unwrap();
    std::fs::create_dir_all(layout.agents_dir()).unwrap();
    let old = layout.build_dir("codex", 42).join("bin/codex");
    let new = layout.build_dir("codex", 43).join("bin/codex");
    for target in [&old, &new] {
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, b"#!/bin/sh\nexit 0\n").unwrap();
    }
    let twin = layout.agent_shim(&tool);
    crate::platform::install_twin_to_env(&twin, &old, &crate::shim_env::ShimEnv::NONE, "").unwrap();
    let before = agent_targets(&layout);
    assert_eq!(before, vec![None, Some(old.clone()), None, None]);
    crate::platform::install_twin_to_env(&twin, &old, &crate::shim_env::ShimEnv::NONE, "").unwrap();
    note_if_changed(&layout, &before);
    assert!(!marker_path(&layout).exists(), "the same twin wakes nobody");
    crate::platform::install_twin_to_env(&twin, &new, &crate::shim_env::ShimEnv::NONE, "").unwrap();
    note_if_changed(&layout, &before);
    assert!(
        marker_path(&layout).exists(),
        "a moved Codex twin wakes the host"
    );
    let _ = std::fs::remove_dir_all(&prefix);
}

#[test]
fn a_muse_twin_change_replaces_the_marker() {
    use std::os::unix::fs::MetadataExt as _;

    let prefix = crate::testdir::dir("atpkg-muse-activation-notice-");
    let layout = Layout {
        prefix: prefix.clone(),
    };
    prepare(&layout);
    assert!(
        crate::vendor_direct::spec("muse").is_some_and(|row| row.live_upgrade),
        "the Muse Code row is the live upgrade's"
    );
    assert!(
        crate::vendor_direct::spec("gemini").is_none(),
        "no row lands a Gemini CLI"
    );
    std::fs::create_dir_all(layout.agents_dir()).unwrap();
    let old = layout
        .build_dir("muse", 2_001_004_010_006_427_002)
        .join("bin/muse");
    let new = layout
        .build_dir("muse", 2_001_004_011_006_500_002)
        .join("bin/muse");
    let gemini = layout
        .build_dir("gemini", 1_000_000_000_046_000_000)
        .join("bin/gemini");
    for target in [&old, &new, &gemini] {
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, b"#!/bin/sh\nexit 0\n").unwrap();
    }
    let muse = layout.agent_shim(&ToolName::new("muse").unwrap());
    crate::platform::install_twin_to_env(&muse, &old, &crate::shim_env::ShimEnv::NONE, "").unwrap();
    let before = agent_targets(&layout);
    assert_eq!(before, vec![None, None, None, Some(old.clone())]);
    crate::platform::install_twin_to_env(&muse, &old, &crate::shim_env::ShimEnv::NONE, "").unwrap();
    note_if_changed(&layout, &before);
    let own = marker_path_of(&layout, "muse");
    assert_eq!(own, prefix.join(NOTICE_DIR).join("muse"));
    assert!(!own.exists(), "the same twin wakes nobody");
    let gemini_twin = layout.agent_shim(&ToolName::new("gemini").unwrap());
    crate::platform::install_twin_to_env(
        &gemini_twin,
        &gemini,
        &crate::shim_env::ShimEnv::NONE,
        "",
    )
    .unwrap();
    note_if_changed(&layout, &before);
    assert!(
        !own.exists() && !marker_path(&layout).exists(),
        "a twin no live upgrade moves wakes nobody"
    );
    assert!(
        !prefix.join(NOTICE_DIR).join("gemini").exists(),
        "and has no marker"
    );
    crate::platform::install_twin_to_env(&muse, &new, &crate::shim_env::ShimEnv::NONE, "").unwrap();
    note_if_changed(&layout, &before);
    assert!(
        own.exists(),
        "a moved Muse Code twin wakes the host for Muse Code"
    );
    let meta = std::fs::metadata(&own).unwrap();
    assert_eq!(meta.mode() & 0o777, 0o600, "the hint is the owner's alone");
    assert!(
        !marker_path(&layout).exists(),
        "a Muse landing never replaces the marker every session looks at"
    );
    let _ = std::fs::remove_dir_all(&prefix);
}

#[test]
fn an_agy_twin_change_replaces_its_own_marker() {
    use std::os::unix::fs::MetadataExt as _;

    assert_eq!(marker_name("agy"), "agy");
    assert!(
        crate::vendor_direct::spec("agy").is_some_and(|row| row.live_upgrade),
        "the agy row is the live upgrade's"
    );
    let prefix = crate::testdir::dir("atpkg-agy-activation-notice-");
    let layout = Layout {
        prefix: prefix.clone(),
    };
    prepare(&layout);
    std::fs::create_dir_all(layout.agents_dir()).unwrap();
    let old = layout
        .build_dir("agy", 1_000_001_000_004_000_000)
        .join("bin/agy");
    let new = layout
        .build_dir("agy", 1_000_001_000_004_000_001)
        .join("bin/agy");
    for target in [&old, &new] {
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, b"#!/bin/sh\nexit 0\n").unwrap();
    }
    let agy = layout.agent_shim(&ToolName::new("agy").unwrap());
    crate::platform::install_twin_to_env(&agy, &old, &crate::shim_env::ShimEnv::NONE, "").unwrap();
    let before = agent_targets(&layout);
    assert_eq!(before, vec![None, None, Some(old.clone()), None]);
    crate::platform::install_twin_to_env(&agy, &old, &crate::shim_env::ShimEnv::NONE, "").unwrap();
    note_if_changed(&layout, &before);
    let own = marker_path_of(&layout, "agy");
    assert_eq!(own, prefix.join(NOTICE_DIR).join("agy"));
    assert!(!own.exists(), "the same twin wakes nobody");
    crate::platform::install_twin_to_env(&agy, &new, &crate::shim_env::ShimEnv::NONE, "").unwrap();
    note_if_changed(&layout, &before);
    assert!(own.exists(), "a moved agy twin wakes the host for agy");
    assert_eq!(
        std::fs::metadata(&own).unwrap().mode() & 0o777,
        0o600,
        "the hint is the owner's alone"
    );
    assert!(
        !marker_path(&layout).exists() && !marker_path_of(&layout, "muse").exists(),
        "an agy landing replaces neither the first marker nor Muse Code's"
    );
    let _ = std::fs::remove_dir_all(&prefix);
}

/// NEGATIVE CONTROL: `publish` refuses invalid marker names and symlinked notice directories
/// without writing into the symlink target, while publishing `0600` markers into a real folder.
#[test]
fn publish_refuses_bad_marker_names_and_symlinked_notice_dirs() {
    let prefix = crate::testdir::dir("atpkg-activation-notice-symlink-");
    let layout = Layout {
        prefix: prefix.clone(),
    };
    prepare(&layout);
    assert!(publish(&layout, "../escape").is_err());
    assert!(publish(&layout, "sh").is_err());

    let outside = prefix.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let notice = prefix.join(NOTICE_DIR);
    std::fs::remove_dir(&notice).unwrap();
    std::os::unix::fs::symlink(&outside, &notice).unwrap();
    assert!(publish(&layout, "claude").is_err());
    assert!(!outside.join("claude").exists());

    std::fs::remove_file(&notice).unwrap();
    assert!(publish(&layout, "claude").is_ok());
    assert!(marker_path_of(&layout, "claude").exists());
    let _ = std::fs::remove_dir_all(&prefix);
}

#[test]
fn a_pass_moving_two_twins_replaces_each_ones_marker() {
    use std::os::unix::fs::MetadataExt as _;

    assert_eq!(marker_name("claude"), CLAUDE_MARKER);
    assert_eq!(marker_name("codex"), CLAUDE_MARKER);
    assert_eq!(marker_name("muse"), "muse");
    let prefix = crate::testdir::dir("atpkg-two-activation-notices-");
    let layout = Layout {
        prefix: prefix.clone(),
    };
    prepare(&layout);
    std::fs::create_dir_all(layout.agents_dir()).unwrap();
    let build = |program: &str, id: u64| {
        let bin = layout.build_dir(program, id).join("bin").join(program);
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"#!/bin/sh\nexit 0\n").unwrap();
        bin
    };
    let lay = |program: &str, bin: &Path| {
        crate::platform::install_twin_to_env(
            &layout.agent_shim(&ToolName::new(program).unwrap()),
            bin,
            &crate::shim_env::ShimEnv::NONE,
            "",
        )
        .unwrap();
    };
    lay("claude", &build("claude", 42));
    lay("muse", &build("muse", 2_001_004_010_006_427_002));
    let before = agent_targets(&layout);
    lay("claude", &build("claude", 43));
    lay("muse", &build("muse", 2_001_004_011_006_500_002));
    note_if_changed(&layout, &before);
    let (first, own) = (marker_path(&layout), marker_path_of(&layout, "muse"));
    assert!(first.exists() && own.exists(), "both replaced");
    let (first_ino, own_ino) = (
        std::fs::metadata(&first).unwrap().ino(),
        std::fs::metadata(&own).unwrap().ino(),
    );
    let before = agent_targets(&layout);
    lay("claude", &build("claude", 44));
    note_if_changed(&layout, &before);
    assert_ne!(std::fs::metadata(&first).unwrap().ino(), first_ino);
    assert_eq!(
        std::fs::metadata(&own).unwrap().ino(),
        own_ino,
        "a Claude Code landing leaves Muse Code's marker"
    );
    let _ = std::fs::remove_dir_all(&prefix);
}
