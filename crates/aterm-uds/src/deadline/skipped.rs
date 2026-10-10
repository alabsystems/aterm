// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// ONE FILE, TWO CRATES: `silent.rs` includes it, and so does aterm-verify,
// which takes no dependencies (its Cargo.toml says why) and reads this line out
// of every test binary's output to name the skip in its verdict. Items only: no
// inner doc, no `use`.

/// The one line a test prints when it cannot run here because macOS's trust
/// service did not answer.
pub const SKIPPED: &str =
    "skipped here: macOS's trust service (trustd) did not answer; nothing was verified";

/// The one line a test prints when it cannot run here because macOS's
/// disk-image service did not answer (`hdiutil` timed out).
pub const DISK_IMAGE_SKIPPED: &str =
    "skipped here: macOS's disk-image service (hdiutil) did not answer; nothing was verified";
