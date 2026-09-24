// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A best-effort wake for the running Claude harness after its managed twin moves.
//!
//! The CLI publishes only after a mutating pass has returned and the FINAL
//! `agents/claude` target differs from the one it found under the store lock.
//! The window watches the dedicated notice directory for an atomic replacement
//! of this marker; it re-reads the twin before acting, and its minute timer remains the
//! fallback if the hint cannot be written or observed. No release fact or
//! authority is carried in the marker itself.

use std::path::{Path, PathBuf};

use crate::store::{Layout, ToolName};

/// The notice directory has no pass progress, status or download files in it:
/// an active download cannot repeatedly wake the idle harness host.
pub const NOTICE_DIR: &str = "activation-notices";

/// One fixed child of that directory, replaced after a Claude twin change.
pub const CLAUDE_MARKER: &str = "claude";

/// Create the notice directory before a pass can start writing progress. Its
/// creation is a one-time event; errors are only a lost hint.
pub(crate) fn prepare(layout: &Layout) {
    let _ = layout.ensure_dir(&layout.prefix.join(NOTICE_DIR));
}

/// The marker the window watches. It is a wake hint, never a version source.
#[must_use]
pub fn marker_path(layout: &Layout) -> PathBuf {
    layout.prefix.join(NOTICE_DIR).join(CLAUDE_MARKER)
}

/// What the front-of-PATH managed Claude twin actually runs at this instant.
/// A missing or pending twin has no target.
#[must_use]
pub(crate) fn claude_target(layout: &Layout) -> Option<PathBuf> {
    let tool = ToolName::new("claude")?;
    crate::platform::resolve_shim(&layout.agent_shim(&tool))
}

/// Publish one coalescible hint when a completed pass changed the live target.
/// An unrelated pass or a transaction rolled back to its original twin writes
/// nothing. A failed hint does not turn a successful install into a failure:
/// the host still sweeps on its bounded fallback.
pub(crate) fn note_if_changed(layout: &Layout, before: Option<&Path>) {
    if claude_target(layout).as_deref() == before {
        return;
    }
    let _ = publish(layout);
}

fn publish(layout: &Layout) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = layout.prefix.join(NOTICE_DIR);
    layout.ensure_dir(&dir)?;
    let tmp = dir.join(format!(
        "{CLAUDE_MARKER}.tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    // `create_new` refuses a planted link rather than following it. One
    // process owns the store lock, and a stale temp can at worst lose this
    // hint; the next minute's sweep remains authoritative.
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
    std::fs::rename(&tmp, marker_path(layout)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_final_claude_target_change_replaces_the_marker() {
        use std::os::unix::fs::MetadataExt as _;

        let prefix = std::env::temp_dir().join(format!(
            "atpkg-claude-activation-notice-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&prefix);
        std::fs::create_dir_all(&prefix).unwrap();
        let layout = Layout {
            prefix: prefix.clone(),
        };
        prepare(&layout);
        let marker = marker_path(&layout);
        note_if_changed(&layout, None);
        assert!(!marker.exists(), "an unchanged absent twin emits no hint");

        // The twin reader, not a caller-supplied claim, decides the change.
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
        note_if_changed(&layout, None);
        let first = std::fs::metadata(&marker).unwrap().ino();
        note_if_changed(&layout, claude_target(&layout).as_deref());
        assert_eq!(std::fs::metadata(&marker).unwrap().ino(), first);

        let newer = layout.build_dir("claude", 43);
        std::fs::create_dir_all(newer.join("bin")).unwrap();
        std::fs::write(newer.join("bin/claude"), b"#!/bin/sh\nexit 0\n").unwrap();
        let before = claude_target(&layout);
        crate::platform::install_twin_to_env(
            &layout.agent_shim(&tool),
            &newer.join("bin/claude"),
            &crate::shim_env::ShimEnv::NONE,
            "",
        )
        .unwrap();
        note_if_changed(&layout, before.as_deref());
        assert_ne!(std::fs::metadata(&marker).unwrap().ino(), first);
        let _ = std::fs::remove_dir_all(prefix);
    }

    #[test]
    fn a_rolled_back_claude_twin_emits_no_notice() {
        let prefix = std::env::temp_dir().join(format!(
            "atpkg-claude-activation-rollback-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&prefix);
        let layout = Layout {
            prefix: prefix.clone(),
        };
        prepare(&layout);
        let tool = ToolName::new("claude").unwrap();
        std::fs::create_dir_all(layout.agents_dir()).unwrap();
        let old = layout.build_dir("claude", 42).join("bin/claude");
        let new = layout.build_dir("claude", 43).join("bin/claude");
        for target in [&old, &new] {
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, b"#!/bin/sh\nexit 0\n").unwrap();
        }
        let twin = layout.agent_shim(&tool);
        crate::platform::install_twin_to_env(&twin, &old, &crate::shim_env::ShimEnv::NONE, "")
            .unwrap();
        let before = claude_target(&layout);
        crate::platform::install_twin_to_env(&twin, &new, &crate::shim_env::ShimEnv::NONE, "")
            .unwrap();
        crate::platform::install_twin_to_env(&twin, &old, &crate::shim_env::ShimEnv::NONE, "")
            .unwrap();
        assert_eq!(claude_target(&layout), before);
        note_if_changed(&layout, before.as_deref());
        assert!(
            !marker_path(&layout).exists(),
            "a pass that restored its original live twin cannot wake the host"
        );
        let _ = std::fs::remove_dir_all(prefix);
    }
}
