// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A best-effort wake for the running agent harness after a managed agent twin moves.

use std::path::PathBuf;

use crate::store::Layout;

/// The notice directory has no pass progress, status or download files in it.
pub const NOTICE_DIR: &str = "activation-notices";

/// The first marker: replaced after a change of Claude Code's or Codex's twin, which every
/// supervised session looks at ([`marker_name`]).
pub const CLAUDE_MARKER: &str = "claude";

/// The marker a moved twin of `program` replaces: Claude Code's and Codex's share
/// [`CLAUDE_MARKER`]; every other program's is its own name.
#[must_use]
pub fn marker_name(program: &str) -> &str {
    match program {
        "claude" | "codex" => CLAUDE_MARKER,
        other => other,
    }
}

/// Create the notice directory before a pass can start writing progress.
pub fn prepare(layout: &Layout) {
    let _ = layout.ensure_dir(&layout.prefix.join(NOTICE_DIR));
}

/// The first marker the window watches ([`CLAUDE_MARKER`]).
#[must_use]
pub fn marker_path(layout: &Layout) -> PathBuf {
    marker_path_of(layout, "claude")
}

/// The marker a moved twin of `program` replaces under `layout` ([`marker_name`]).
#[must_use]
pub fn marker_path_of(layout: &Layout, program: &str) -> PathBuf {
    layout.prefix.join(NOTICE_DIR).join(marker_name(program))
}

/// What the front-of-PATH managed twin of `agent` actually runs at this instant.
#[must_use]
pub(crate) fn agent_target(layout: &Layout, agent: &str) -> Option<PathBuf> {
    crate::activate::twin_target(layout, agent)
}

#[cfg(test)]
fn claude_target(layout: &Layout) -> Option<PathBuf> {
    agent_target(layout, "claude")
}

fn live_programs() -> impl Iterator<Item = &'static str> {
    crate::vendor_direct::VENDORS
        .iter()
        .filter(|row| row.live_upgrade)
        .map(|row| row.program)
}

/// The live target of the twin of every agent the live upgrade moves sessions onto
/// ([`live_programs`]), in table order: what a pass snapshots under the store lock and compares
/// after ([`note_if_changed`]).
#[must_use]
pub(crate) fn agent_targets(layout: &Layout) -> Vec<Option<PathBuf>> {
    live_programs()
        .map(|program| agent_target(layout, program))
        .collect()
}

/// Publish one coalescible hint per marker whose twin a completed pass changed ([`marker_name`];
/// `before`: the targets [`agent_targets`] read as the pass found them, in its order, a missing
/// entry read as no twin).
pub(crate) fn note_if_changed(layout: &Layout, before: &[Option<PathBuf>]) {
    let after = agent_targets(layout);
    let mut published: Vec<&str> = Vec::new();
    for (at, program) in live_programs().enumerate() {
        let was = before.get(at).cloned().flatten();
        let marker = marker_name(program);
        if after.get(at).cloned().flatten() != was && !published.contains(&marker) {
            published.push(marker);
            let _ = publish(layout, marker);
        }
    }
}

fn publish(layout: &Layout, marker: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = layout.prefix.join(NOTICE_DIR);
    layout.ensure_dir(&dir)?;
    let tmp = dir.join(format!(
        "{marker}.tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let staged = file.write_all(b"1\n");
    drop(file);
    if let Err(error) = staged {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    std::fs::rename(&tmp, dir.join(marker)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
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
        crate::platform::install_twin_to_env(&twin, &old, &crate::shim_env::ShimEnv::NONE, "")
            .unwrap();
        let before = agent_targets(&layout);
        assert_eq!(before, vec![None, Some(old.clone()), None, None]);
        crate::platform::install_twin_to_env(&twin, &old, &crate::shim_env::ShimEnv::NONE, "")
            .unwrap();
        note_if_changed(&layout, &before);
        assert!(!marker_path(&layout).exists(), "the same twin wakes nobody");
        crate::platform::install_twin_to_env(&twin, &new, &crate::shim_env::ShimEnv::NONE, "")
            .unwrap();
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
        crate::platform::install_twin_to_env(&muse, &old, &crate::shim_env::ShimEnv::NONE, "")
            .unwrap();
        let before = agent_targets(&layout);
        assert_eq!(before, vec![None, None, None, Some(old.clone())]);
        crate::platform::install_twin_to_env(&muse, &old, &crate::shim_env::ShimEnv::NONE, "")
            .unwrap();
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
        crate::platform::install_twin_to_env(&muse, &new, &crate::shim_env::ShimEnv::NONE, "")
            .unwrap();
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
        crate::platform::install_twin_to_env(&agy, &old, &crate::shim_env::ShimEnv::NONE, "")
            .unwrap();
        let before = agent_targets(&layout);
        assert_eq!(before, vec![None, None, Some(old.clone()), None]);
        crate::platform::install_twin_to_env(&agy, &old, &crate::shim_env::ShimEnv::NONE, "")
            .unwrap();
        note_if_changed(&layout, &before);
        let own = marker_path_of(&layout, "agy");
        assert_eq!(own, prefix.join(NOTICE_DIR).join("agy"));
        assert!(!own.exists(), "the same twin wakes nobody");
        crate::platform::install_twin_to_env(&agy, &new, &crate::shim_env::ShimEnv::NONE, "")
            .unwrap();
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
}
