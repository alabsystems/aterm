// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The git-and-friends shim directory the window keeps on a Mac whose Command Line Tools are
//! installing (System State design §6.3).
//!
//! While a setup pass wants Apple's Command Line Tools ([`crate::clt_install`]), the window lays
//! a directory of symlinks — one per [`aterm_types::clt_shim::SHIM_TOOLS`] name that exists as a
//! `/usr/bin` stub — each onto the one `aterm` binary, and puts that directory ahead of
//! `/usr/bin` in the shells it spawns. A shell that runs `git` then reaches `aterm`, which says
//! the tools are on their way ([`aterm_cli::clt_shim`]) instead of raising macOS's install
//! dialog. The directory is torn down the moment no pass wants the tools (they arrived, or the
//! run was stopped), so a later shell gets a clean PATH.
//!
//! The directory's own existence is the signal: [`on_path`] adds it to a session's PATH only
//! when it is there, and the owner's Mac — which already has the tools, so no pass ever wants
//! them — never lays it, leaving every shell's PATH untouched.

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

/// The shim directory under this Mac's state home (the `ATERM_STATE_HOME` dev seam isolates it in
/// a guest or a test), or `None` where there is no state home to anchor it to.
#[must_use]
pub(crate) fn shim_dir() -> Option<PathBuf> {
    aterm_types::dirs::state_dir().map(|dir| dir.join("clt-shim"))
}

/// Lay the shim directory when `active` (a pass wants the Command Line Tools), or tear it down
/// when not. Each link is `name -> aterm_exe`; the set is [`aterm_types::clt_shim::SHIM_TOOLS`]
/// filtered to the names that exist as `/usr/bin` stubs now. Best effort: a Mac that cannot lay
/// it simply keeps raising Apple's own dialog, as it did before.
#[cfg(unix)]
pub(crate) fn refresh(active: bool) {
    let Some(dir) = shim_dir() else {
        return;
    };
    if !active {
        // We own this directory; removing it drops our symlinks, never their targets.
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let Ok(aterm_exe) = std::env::current_exe() else {
        return;
    };
    let tools = aterm_types::clt_shim::runtime_tools(usr_bin_stub_exists);
    let _ = lay(&dir, &aterm_exe, &tools);
}

#[cfg(not(unix))]
pub(crate) fn refresh(_active: bool) {}

/// Whether `/usr/bin/<tool>` exists (the `xcode-select` stub a new Mac carries).
#[cfg(unix)]
fn usr_bin_stub_exists(tool: &str) -> bool {
    Path::new("/usr/bin").join(tool).exists()
}

/// Create `dir` and link each of `tools` onto `aterm_exe`, replacing a stale link. Extra links a
/// shrunken tool set left behind are removed, so the directory holds exactly `tools`.
#[cfg(unix)]
fn lay(dir: &Path, aterm_exe: &Path, tools: &[&str]) -> std::io::Result<()> {
    use std::os::unix::fs::symlink;
    std::fs::create_dir_all(dir)?;
    for tool in tools {
        let link = dir.join(tool);
        // Replace a link that points elsewhere (a moved bundle); leave a correct one.
        if std::fs::read_link(&link).ok().as_deref() != Some(aterm_exe) {
            let _ = std::fs::remove_file(&link);
            symlink(aterm_exe, &link)?;
        }
    }
    // Drop a link for a name no longer in the set.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str()
                && !tools.contains(&name)
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}

/// `path` with `shim` placed immediately ahead of the first system bin (`/usr/bin`, else
/// `/bin`), so the shim intercepts only what those directories' `xcode-select` stubs would have
/// served — a tool a person put earlier on PATH (Homebrew's `git`) still wins. Idempotent: a
/// prior copy of `shim` is dropped first. A PATH with no system bin is returned with any prior
/// copy dropped and nothing added (the stubs are unreachable, so the shim would do nothing).
#[must_use]
pub(crate) fn on_path(path: &str, shim: &str) -> String {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let mut entries: Vec<&str> = path.split(sep).collect();
    entries.retain(|entry| *entry != shim);
    let at = entries
        .iter()
        .position(|entry| *entry == "/usr/bin")
        .or_else(|| entries.iter().position(|entry| *entry == "/bin"));
    if let Some(at) = at {
        entries.insert(at, shim);
    }
    entries.join(&sep.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shim_sits_ahead_of_usr_bin_and_behind_a_person_s_own_tools() {
        // Homebrew's git, earlier on PATH, still wins; the shim only fronts `/usr/bin`.
        let got = on_path("/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin", "/S");
        assert_eq!(got, "/opt/homebrew/bin:/S:/usr/bin:/bin:/usr/sbin");
    }

    #[test]
    fn with_no_usr_bin_it_fronts_bin() {
        assert_eq!(
            on_path("/usr/local/bin:/bin", "/S"),
            "/usr/local/bin:/S:/bin"
        );
    }

    #[test]
    fn applying_it_twice_changes_nothing_more() {
        let once = on_path("/opt/homebrew/bin:/usr/bin", "/S");
        assert_eq!(on_path(&once, "/S"), once);
    }

    #[test]
    fn with_no_system_bin_the_shim_is_not_added() {
        // Nothing to intercept, so the shim stays off PATH (and a stale copy is dropped).
        assert_eq!(on_path("/opt/homebrew/bin:/S", "/S"), "/opt/homebrew/bin");
    }

    #[cfg(unix)]
    #[test]
    fn lay_links_each_tool_onto_aterm_then_teardown_removes_them() {
        let tmp = std::env::temp_dir().join(format!("aterm-clt-shim-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dir = tmp.join("clt-shim");
        let aterm_exe = tmp.join("aterm");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(&aterm_exe, b"#!/bin/sh\n").unwrap();

        lay(&dir, &aterm_exe, &["git", "cc"]).unwrap();
        assert_eq!(std::fs::read_link(dir.join("git")).unwrap(), aterm_exe);
        assert_eq!(std::fs::read_link(dir.join("cc")).unwrap(), aterm_exe);

        // A shrunken set drops the link it no longer owns.
        lay(&dir, &aterm_exe, &["git"]).unwrap();
        assert!(dir.join("git").exists());
        assert!(dir.join("cc").symlink_metadata().is_err());

        // Teardown removes our links, never the binary they point at.
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!dir.exists());
        assert!(aterm_exe.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
