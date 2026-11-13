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
    if crate::store::ToolName::new(marker).is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid activation marker name",
        ));
    }
    if std::fs::symlink_metadata(&layout.prefix).is_ok_and(|m| !m.file_type().is_dir()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "prefix is not a real directory",
        ));
    }
    let dir = layout.prefix.join(NOTICE_DIR);
    if std::fs::symlink_metadata(&dir).is_ok_and(|m| !m.file_type().is_dir()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "activation notice directory is not a real directory",
        ));
    }
    layout.ensure_dir(&dir)?;
    if !std::fs::symlink_metadata(&dir)?.file_type().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "activation notice directory is not a real directory",
        ));
    }
    aterm_uds::atomic_file::write(&dir.join(marker), b"1\n", 0o600)
}

#[cfg(test)]
#[path = "activation_notice_tests.rs"]
mod tests;
