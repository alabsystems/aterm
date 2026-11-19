// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Backup exclusions and build directories, shared with the temporary-directory allocator.

pub use aterm_backup::{Backups, validate_build_dir};

#[cfg(test)]
#[cfg(unix)]
#[path = "backup_tests.rs"]
mod tests;
