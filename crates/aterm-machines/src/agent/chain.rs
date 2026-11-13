// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The path-chain check the root agent runs before it acts (design §3.4).

use crate::port::System;
use std::path::{Component, Path, PathBuf};

/// Check `exe` (the path this process was started as) and its chain.
/// # Errors
/// One line naming the first component that is not root's alone.
pub fn check(sys: &dyn System, exe: &Path) -> Result<(), String> {
    if !exe.is_absolute() || has_dots(exe) {
        return Err(format!("{} is not a plain absolute path", exe.display()));
    }
    let st = sys
        .stat(exe)
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    let file = if st.symlink {
        if st.uid != 0 {
            return Err(format!(
                "{} is a link owned by uid {}, not root",
                exe.display(),
                st.uid
            ));
        }
        let parent = exe
            .parent()
            .ok_or_else(|| format!("{} has no parent", exe.display()))?;
        dirs(sys, parent)?;
        let target = sys
            .read_link(exe)
            .map_err(|e| format!("{}: {e}", exe.display()))?;
        let target: PathBuf = if target.is_absolute() {
            target
        } else {
            parent.join(target)
        };
        if has_dots(&target) {
            return Err(format!(
                "{} points through `.` or `..` ({})",
                exe.display(),
                target.display()
            ));
        }
        target
    } else {
        exe.to_path_buf()
    };
    let st = sys
        .stat(&file)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    if st.symlink {
        return Err(format!(
            "{} is a link to a link; only bin/aterm may be a link",
            file.display()
        ));
    }
    if st.dir {
        return Err(format!("{} is a directory", file.display()));
    }
    owned_by_root(&file, st.uid, st.mode)?;
    let parent = file
        .parent()
        .ok_or_else(|| format!("{} has no parent", file.display()))?;
    dirs(sys, parent)
}

fn dirs(sys: &dyn System, dir: &Path) -> Result<(), String> {
    for d in dir.ancestors() {
        let st = sys.stat(d).map_err(|e| format!("{}: {e}", d.display()))?;
        if st.symlink {
            return Err(format!("{} is a symbolic link", d.display()));
        }
        if !st.dir {
            return Err(format!("{} is not a directory", d.display()));
        }
        owned_by_root(d, st.uid, st.mode)?;
    }
    Ok(())
}

fn owned_by_root(path: &Path, uid: u32, mode: u32) -> Result<(), String> {
    if uid != 0 {
        return Err(format!(
            "{} is owned by uid {uid}, not root",
            path.display()
        ));
    }
    if mode & 0o022 != 0 {
        return Err(format!(
            "{} is writable by its group or others (mode {mode:o})",
            path.display()
        ));
    }
    Ok(())
}

fn has_dots(p: &Path) -> bool {
    p.components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
}

#[cfg(test)]
#[path = "chain_tests.rs"]
mod tests;
