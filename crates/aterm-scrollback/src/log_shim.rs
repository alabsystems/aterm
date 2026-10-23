// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Strict-gate log shims.

/// Log a pre-composed message at warn level.
#[inline(never)]
pub(crate) fn warn_str(msg: &str) {
    aterm_log::warn!("{msg}");
}
