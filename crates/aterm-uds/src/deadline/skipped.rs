// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Included by `silent.rs` and by aterm-verify (which takes no dependencies), which finds these
// lines in each test binary's output to name the skip. Items only: no inner doc, no `use`.

/// The one line a test prints when it cannot run here because macOS's trust
/// service did not answer.
pub const SKIPPED: &str =
    "skipped here: macOS's trust service (trustd) did not answer; nothing was verified";

/// The one line a test prints when it cannot run here because macOS's
/// disk-image service did not answer (`hdiutil` timed out).
pub const DISK_IMAGE_SKIPPED: &str =
    "skipped here: macOS's disk-image service (hdiutil) did not answer; nothing was verified";
