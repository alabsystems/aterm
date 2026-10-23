// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whether another account could replace what an install directory holds: the walk
//! [`super::Context::destination`] makes before it trusts a target, and the same rule the
//! installer's embedded verifier applies (tools/install.sh `check_install_path`), held equal to it
//! by `crates/aterm-update/tests/install_sh`.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Why another account could replace what `dir` holds, or `Ok` when none can: from `/` down, each
/// component a real directory of root's or this user's and, until a private directory of this
/// user's, writable by no other account; each refusal names the component and, for a directory of
/// this user's, the remedy.
pub fn refusal(dir: &Path) -> Result<(), String> {
    let me = super::uid();
    let mut private_ancestor = false;
    for ancestor in dir.ancestors().collect::<Vec<_>>().into_iter().rev() {
        let md = fs::symlink_metadata(ancestor).map_err(|e| e.to_string())?;
        let why = if md.file_type().is_symlink() {
            Some("is a symlink".to_string())
        } else if !md.is_dir() {
            Some("isn\u{2019}t a directory".to_string())
        } else if ![0, me].contains(&md.uid()) {
            Some("belongs to another user".to_string())
        } else if !private_ancestor
            && aterm_uds::shared_write::others_can_write_at(ancestor).map_err(|e| e.to_string())?
        {
            Some(if md.uid() == me {
                format!(
                    "is writable by other users \u{2014} run: chmod go-w {}",
                    ancestor.display()
                )
            } else {
                "is writable by other users".to_string()
            })
        } else {
            None
        };
        if let Some(why) = why {
            return Err(format!("{} {why}", ancestor.display()));
        }
        private_ancestor |= md.uid() == me && md.mode() & 0o077 == 0;
    }
    Ok(())
}
