// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The path-chain check the root agent runs before it acts (design §3.4): its executable and
//! every directory from there up to `/` must be root-owned and writable by nobody else, so
//! no other account could have replaced what sudo just ran as root.
//!
//! The executable may be named through one link, the stable `bin/aterm`, which must itself
//! be root-owned; the link's target is checked the same way, and no other link is allowed
//! anywhere in either chain.

use crate::port::System;
use std::path::{Component, Path, PathBuf};

/// Check `exe` (the path this process was started as) and its chain.
///
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

/// `dir` and every directory above it: real directories, root's, writable by root alone.
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
mod tests {
    use super::*;
    use crate::port::{Fake, Stat};

    fn dir(uid: u32, mode: u32) -> Stat {
        Stat {
            uid,
            gid: 0,
            mode,
            dir: true,
            symlink: false,
            len: 0,
        }
    }

    fn file(uid: u32, mode: u32) -> Stat {
        Stat {
            uid,
            gid: 0,
            mode,
            dir: false,
            symlink: false,
            len: 1,
        }
    }

    /// The installed layout as `stat -f "%Su:%Sg %Sp"` showed it in the VM: `/` and
    /// `/Library` root:wheel 0755, everything the agent made root:wheel 0755.
    fn installed() -> Fake {
        let fake = Fake::root();
        for d in [
            "/",
            "/Library",
            "/Library/Management",
            "/Library/Management/aterm",
            "/Library/Management/aterm/bin",
            "/Library/Management/aterm/agent",
        ] {
            fake.stat_of(d, dir(0, 0o755));
        }
        fake.stat_of(
            "/Library/Management/aterm/agent/aterm-machines-dev",
            file(0, 0o755),
        );
        fake.link(
            "/Library/Management/aterm/bin/aterm",
            "/Library/Management/aterm/agent/aterm-machines-dev",
        );
        fake
    }

    const EXE: &str = "/Library/Management/aterm/bin/aterm";

    #[test]
    fn the_installed_agent_through_its_root_link_passes() {
        check(&installed(), Path::new(EXE)).unwrap();
        check(
            &installed(),
            Path::new("/Library/Management/aterm/agent/aterm-machines-dev"),
        )
        .unwrap();
    }

    #[test]
    fn a_directory_another_account_may_write_fails_the_chain() {
        let fake = installed();
        fake.stat_of("/Library/Management", dir(0, 0o775));
        let err = check(&fake, Path::new(EXE)).unwrap_err();
        assert!(err.contains("/Library/Management is writable"), "{err}");
        let fake = installed();
        fake.stat_of("/Library/Management/aterm/agent", dir(501, 0o755));
        let err = check(&fake, Path::new(EXE)).unwrap_err();
        assert!(err.contains("owned by uid 501"), "{err}");
    }

    #[test]
    fn an_executable_others_could_replace_fails_the_chain() {
        let fake = installed();
        fake.stat_of(
            "/Library/Management/aterm/agent/aterm-machines-dev",
            file(0, 0o775),
        );
        assert!(check(&fake, Path::new(EXE)).is_err());
        let fake = installed();
        fake.stat_of(
            "/Library/Management/aterm/agent/aterm-machines-dev",
            file(501, 0o755),
        );
        assert!(check(&fake, Path::new(EXE)).is_err());
    }

    #[test]
    fn a_link_anywhere_but_bin_aterm_fails_the_chain() {
        let fake = installed();
        fake.stat_of(
            "/Library/Management/aterm/agent",
            Stat {
                symlink: true,
                dir: false,
                ..dir(0, 0o755)
            },
        );
        let err = check(&fake, Path::new(EXE)).unwrap_err();
        assert!(err.contains("symbolic link"), "{err}");
        let fake = installed();
        fake.stat_of(
            EXE,
            Stat {
                uid: 501,
                gid: 20,
                mode: 0o755,
                dir: false,
                symlink: true,
                len: 0,
            },
        );
        let err = check(&fake, Path::new(EXE)).unwrap_err();
        assert!(err.contains("link owned by uid 501"), "{err}");
    }

    #[test]
    fn a_link_through_dot_dot_fails_the_chain() {
        let fake = installed();
        fake.link(EXE, "../../../../Users//admin/aterm");
        assert!(check(&fake, Path::new(EXE)).is_err());
    }
}
