// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Negative legacy-reader fence. Marker presence never grants owner or Slot
//! authority, including malformed, unreadable, linked or pending marker files.

use super::*;

pub const OWNER_MARKER_SUFFIX: &str = ".aterm-owner";
pub const OWNER_MARKER_TEXT: &[u8] = b"ATERM-CODEX-AUTH-OWNER 1\n";

pub(super) fn legacy_reader_allowed(home: &Path) -> bool {
    legacy_file_reader_allowed(&auth_path_in_home(home))
}

/// Negative marker fence for an already resolved exact auth file, including
/// custom basenames. True grants no source, Slot, ownership or readiness
/// authority; callers still need their own admission and startup gate.
pub fn legacy_file_reader_allowed(auth: &Path) -> bool {
    let Some(name) = auth.file_name() else {
        return false;
    };
    [OWNER_MARKER_SUFFIX, ".aterm-slot", ".aterm-slot-state"].iter().all(|suffix| {
        let mut marker = name.to_os_string();
        marker.push(suffix);
        matches!(std::fs::symlink_metadata(auth.with_file_name(marker)), Err(error) if error.kind()==std::io::ErrorKind::NotFound)
    })
}

#[cfg(test)]
#[path = "codex_app_login_custody_tests.rs"]
mod tests;
