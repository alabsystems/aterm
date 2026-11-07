// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test fixtures of the files aterm reads only when no other user may write them.

use std::path::Path;

/// `std::fs::write` for a file a test means aterm to read under an owner-only rule (`aterm.toml`:
/// [`aterm_types::dirs::aterm_config_refusal`]; a private record: no group or other bit at all):
/// `contents` whole, mode 0600 whatever the umask ([`aterm_uds::atomic_file::write`]). A plain
/// write gives a new file the umask's mode, group-writable under a user-private-group login's
/// 002, and the reader refuses that file, so the test would take the refused path instead of the
/// one it names.
pub(crate) fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    aterm_uds::atomic_file::write(path.as_ref(), contents.as_ref(), 0o600)
}
