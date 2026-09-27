// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A best-effort wake for the running agent harness after a managed agent twin moves.
//!
//! The CLI publishes only after a mutating pass has returned and the FINAL
//! target of an agent twin the live upgrade moves — `agents/claude`, and
//! `agents/codex` (the harness's Codex branch) — differs from the one it
//! found under the store lock.
//! The window watches the dedicated notice directory for an atomic replacement
//! of this marker; it re-reads the twin before acting, and its minute timer remains the
//! fallback if the hint cannot be written or observed. No release fact or
//! authority is carried in the marker itself.

use std::path::PathBuf;

use crate::store::{Layout, ToolName};

/// The notice directory has no pass progress, status or download files in it:
/// an active download cannot repeatedly wake the idle harness host.
pub const NOTICE_DIR: &str = "activation-notices";

/// One fixed child of that directory, replaced after a change of any twin in
/// [`AGENTS`] (named for the first of them, which the window has watched
/// since before Codex joined).
pub const CLAUDE_MARKER: &str = "claude";

/// The agents whose twins the live upgrade moves sessions onto: a pass that
/// moves either one wakes the window's host.
pub const AGENTS: [&str; 2] = ["claude", "codex"];

/// Create the notice directory before a pass can start writing progress. Its
/// creation is a one-time event; errors are only a lost hint. The window's
/// host calls it too, for a prefix that already exists, so its watch sits on
/// the directory itself from the start — never on an ancestor it would have
/// to look at again on a timer until the first install (the philosophy
/// review of 2026-09-25).
pub fn prepare(layout: &Layout) {
    let _ = layout.ensure_dir(&layout.prefix.join(NOTICE_DIR));
}

/// The marker the window watches. It is a wake hint, never a version source.
#[must_use]
pub fn marker_path(layout: &Layout) -> PathBuf {
    layout.prefix.join(NOTICE_DIR).join(CLAUDE_MARKER)
}

/// What the front-of-PATH managed twin of `agent` actually runs at this
/// instant. A missing or pending twin has no target.
#[must_use]
pub(crate) fn agent_target(layout: &Layout, agent: &str) -> Option<PathBuf> {
    let tool = ToolName::new(agent)?;
    crate::platform::resolve_shim(&layout.agent_shim(&tool))
}

/// What the front-of-PATH managed Claude twin actually runs at this instant.
#[cfg(test)]
fn claude_target(layout: &Layout) -> Option<PathBuf> {
    agent_target(layout, "claude")
}

/// The live target of every twin in [`AGENTS`], in that order: what a pass
/// snapshots under the store lock and compares after ([`note_if_changed`]).
#[must_use]
pub(crate) fn agent_targets(layout: &Layout) -> Vec<Option<PathBuf>> {
    AGENTS.iter().map(|a| agent_target(layout, a)).collect()
}

/// Publish one coalescible hint when a completed pass changed a live target
/// of [`AGENTS`] (`before`: [`agent_targets`] as the pass found them). An
/// unrelated pass or a transaction rolled back to its original twins writes
/// nothing. A failed hint does not turn a successful install into a failure:
/// the host still looks at a worker's start.
pub(crate) fn note_if_changed(layout: &Layout, before: &[Option<PathBuf>]) {
    if agent_targets(layout) == before {
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
        note_if_changed(&layout, &[None, None]);
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
        note_if_changed(&layout, &[None, None]);
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
        let before = agent_targets(&layout);
        crate::platform::install_twin_to_env(&twin, &new, &crate::shim_env::ShimEnv::NONE, "")
            .unwrap();
        crate::platform::install_twin_to_env(&twin, &old, &crate::shim_env::ShimEnv::NONE, "")
            .unwrap();
        assert_eq!(agent_targets(&layout), before);
        note_if_changed(&layout, &before);
        assert!(
            !marker_path(&layout).exists(),
            "a pass that restored its original live twin cannot wake the host"
        );
        let _ = std::fs::remove_dir_all(prefix);
    }

    /// THE CODEX BRANCH IS WOKEN TOO: a pass that moves only `agents/codex`
    /// replaces the one marker the window watches, so a running Codex
    /// session's worker asks at once whether its session has somewhere to
    /// go. NEGATIVE CONTROL: the same twin laid again moves nothing.
    #[test]
    fn a_codex_twin_change_replaces_the_marker_too() {
        let prefix = std::env::temp_dir().join(format!(
            "atpkg-codex-activation-notice-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&prefix);
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
        assert_eq!(before, vec![None, Some(old.clone())]);
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
        let _ = std::fs::remove_dir_all(prefix);
    }
}
