// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The git-and-friends shim directory the window keeps on a Mac from aterm's first launch until
//! its Command Line Tools exist (System State design §6.3).
//!
//! While the tools are missing, whatever becomes of an install of them, a launch lays a directory
//! of symlinks, one per [`aterm_types::clt_shim::SHIM_TOOLS`] name that exists as a `/usr/bin`
//! stub, each onto the one `aterm` binary, and marks it wanted ([`Shim::settle`],
//! [`aterm_types::clt_shim::wanted_mark`]). Every shell the window spawns while it is wanted gets
//! that directory ahead of `/usr/bin` on its PATH and named in
//! [`aterm_types::clt_shim::SHIM_ENV`], and its developer directory in `DEVELOPER_DIR`
//! ([`aterm_types::clt_shim::developer_dir`]), decided at the spawn ([`settle_session_path`]); the
//! shell integration moves the directory back ahead of `/usr/bin` once the profile files have run,
//! since a login shell's `path_helper` puts `/usr/bin` first. A login shell or an agent's tool
//! shell that loads no integration keeps the variable, which Apple's stubs read first. So a tab
//! or an agent that runs `git` reaches `aterm`, which fails plainly ([`aterm_cli::clt_shim`])
//! instead of raising macOS's install dialog. A second mark
//! ([`aterm_types::clt_shim::installing_mark`]) says whether an install is on its way, for the
//! shim's one line. The wanted mark goes the moment the tools exist, and a shell spawned after
//! that gets neither. The directory itself stays once laid, and each launch links it again onto
//! its own `aterm` ([`Shim::for_launch`]): a process that outlives the window (a tmux server, a
//! `nohup` job, a service started from a tab) keeps the PATH entry and the `DEVELOPER_DIR` it
//! inherited, and a bash there may have hashed `<shim>/git`. Through the kept links each of them
//! reaches a shim, which hands the call to Apple's own `/usr/bin/<tool>` once the tools exist;
//! with the developer directory gone, Apple's stubs there would fail
//! `missing DEVELOPER_DIR path` instead, and a hashed `git` `No such file or directory`, on a Mac
//! that has the tools, until that process ends.
//!
//! The owner's Mac, which already had the tools at aterm's first launch, never lays it, leaving
//! every shell's PATH untouched.

use std::path::{Path, PathBuf};

/// The shim directory this window keeps, and how it reads whether the tools are installed.
pub(crate) struct Shim {
    /// Where it is laid: this account's ([`aterm_types::clt_shim::shim_dir`]), a test's own, or
    /// nowhere (a test that lays none, or no state home).
    dir: Option<PathBuf>,
    /// Whether Git and the system compiler are installed now ([`crate::clt_install::developer_tools_ready`]).
    present: fn() -> bool,
    /// The program its links point at: this process's own executable, or a test's stand-in.
    exe: Option<PathBuf>,
}

impl Default for Shim {
    /// A shim laid nowhere (a test's, or a headless one's that needs none). A test's reads the
    /// tools missing, as no test reads this Mac's; one that needs them there says so
    /// ([`Self::at`]).
    fn default() -> Self {
        Self {
            dir: None,
            present: if cfg!(test) {
                (|| false) as fn() -> bool
            } else {
                crate::clt_install::developer_tools_ready
            },
            exe: None,
        }
    }
}

impl Shim {
    /// This account's shim, as a launch keeps it, reconciled ahead of the launch's first shell
    /// ([`Self::reconcile`]); none in a test, which never touches the account's state, nor off
    /// macOS, where `/usr/bin` holds no install stubs. The window keeps the one this returns
    /// ([`crate::workstation_setup::Host::for_launch`]), so no launch keeps one unreconciled.
    #[must_use]
    pub(crate) fn for_launch() -> Self {
        let shim = Self {
            dir: if cfg!(all(target_os = "macos", not(test))) {
                aterm_types::clt_shim::shim_dir()
            } else {
                None
            },
            present: crate::clt_install::developer_tools_ready,
            exe: None,
        };
        shim.reconcile();
        shim
    }

    /// A shim laid at `dir`, the tools installed as `present` says (a test's; only a Unix host
    /// lays one).
    #[cfg(all(test, unix))]
    pub(crate) const fn at(dir: PathBuf, present: fn() -> bool) -> Self {
        Self {
            dir: Some(dir),
            present,
            exe: None,
        }
    }

    /// This shim with its links onto `exe` in place of this process's executable (a test's).
    #[cfg(all(test, unix))]
    pub(crate) fn onto(self, exe: PathBuf) -> Self {
        Self {
            exe: Some(exe),
            ..self
        }
    }

    /// Whether a directory is kept here and the tools are missing now.
    pub(crate) fn tools_missing(&self) -> bool {
        self.dir.is_some() && !self.tools_present()
    }

    /// Whether the tools are installed now, as this shim reads them (a stat and a readlink), laid
    /// or not.
    pub(crate) fn tools_present(&self) -> bool {
        (self.present)()
    }

    /// While the tools are missing, lay the directory and mark it wanted, with its installing mark
    /// saying whether an install is on its way (`installing`); once they exist take both marks
    /// away, at once, and leave the directory where it is. Best effort: a directory that could
    /// not be laid leaves nothing wanted, and a process whose start named its developer directory
    /// keeps that name ([`settle_session_path`]), which on macOS 26.6 makes Apple's stubs fail
    /// `missing DEVELOPER_DIR path` (measured with the tools installed; a Mac without them is not).
    /// Returns the installing word, reading it only while the tools are missing or no directory
    /// is kept; once this shim finds the tools, its word is false and both marks go without that
    /// reading. With no directory, the owner's word is kept for its watch bookkeeping.
    pub(crate) fn settle(&self, installing: impl FnOnce() -> bool) -> bool {
        let Some(dir) = &self.dir else {
            return installing();
        };
        let missing = !(self.present)();
        let installing = missing && installing();
        refresh(dir, missing, installing, self.exe.as_deref());
        installing
    }

    /// This shim as a launch keeps it, ahead of the launch's first shell. The marks an earlier
    /// launch left go, as nothing installs the tools yet. A directory an earlier launch laid
    /// stays, its links (the developer directory's `xcrun` among them) made again onto this
    /// launch's executable, so a process that inherited it from an earlier launch, however long
    /// it lives, still reaches a shim that hands the call on once the tools exist. With the tools
    /// missing it is laid, and wanted, at once.
    pub(crate) fn reconcile(&self) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_file(aterm_types::clt_shim::wanted_mark(dir));
            let _ = std::fs::remove_file(aterm_types::clt_shim::installing_mark(dir));
            if dir.is_dir() && (self.present)() {
                let _ = relay(dir, self.exe.as_deref());
            }
        }
        self.settle(|| false);
    }

    /// Whether the directory is laid now.
    #[cfg(all(test, unix))]
    pub(crate) fn laid(&self) -> bool {
        self.dir.as_deref().is_some_and(std::path::Path::is_dir)
    }

    /// Whether new shells get it now: laid, and marked wanted.
    #[cfg(all(test, unix))]
    pub(crate) fn wanted(&self) -> bool {
        self.dir.as_deref().is_some_and(is_wanted)
    }
}

/// Whether `dir` is laid and marked wanted.
fn is_wanted(dir: &std::path::Path) -> bool {
    dir.is_dir()
        && std::fs::symlink_metadata(aterm_types::clt_shim::wanted_mark(dir))
            .is_ok_and(|m| m.is_file())
}

#[cfg(unix)]
fn write_mark(path: &Path) {
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Ok(file) = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        && file.metadata().is_ok_and(|m| m.is_file())
    {
        let _ = file.set_len(0);
    }
}

/// Lay `dir` and mark it wanted while the tools are `missing`, with the installing mark as
/// `installing` says, or take both marks away once they exist (the directory stays for the shells
/// that have it). Each link is `name -> aterm_exe`; the set is
/// [`aterm_types::clt_shim::SHIM_TOOLS`] filtered to the names that exist as `/usr/bin` stubs now.
/// The wanted mark is written only once the links are there, so a spawn that reads it finds them.
#[cfg(unix)]
fn refresh(dir: &Path, missing: bool, installing: bool, exe: Option<&Path>) {
    let wanted = aterm_types::clt_shim::wanted_mark(dir);
    let on_its_way = aterm_types::clt_shim::installing_mark(dir);
    if !missing {
        let _ = std::fs::remove_file(&wanted);
        let _ = std::fs::remove_file(&on_its_way);
        return;
    }
    if installing {
        write_mark(&on_its_way);
    } else {
        let _ = std::fs::remove_file(&on_its_way);
    }
    if is_wanted(dir) {
        return;
    }
    if relay(dir, exe).is_ok() {
        write_mark(&wanted);
    }
}

#[cfg(not(unix))]
fn refresh(_dir: &Path, _missing: bool, _installing: bool, _exe: Option<&Path>) {}

/// Lay `dir` onto `exe`, else this process's own executable ([`lay`]), the set of tools this
/// Mac's `/usr/bin` holds stubs for.
#[cfg(unix)]
fn relay(dir: &Path, exe: Option<&Path>) -> std::io::Result<()> {
    let aterm_exe = match exe {
        Some(exe) => exe.to_owned(),
        None => std::env::current_exe()?,
    };
    let tools = aterm_types::clt_shim::runtime_tools(usr_bin_stub_exists);
    lay(dir, &aterm_exe, &tools)
}

#[cfg(not(unix))]
fn relay(_dir: &Path, _exe: Option<&Path>) -> std::io::Result<()> {
    Ok(())
}

/// This account's shim directory while it is wanted: what a spawn puts on its shell's PATH and
/// names in [`aterm_types::clt_shim::SHIM_ENV`] ([`settle_session_path`]), and what a warm
/// spare's spawn shares with a cold tab's.
#[must_use]
pub(crate) fn wanted_dir() -> Option<String> {
    aterm_types::clt_shim::shim_dir()
        .filter(|dir| is_wanted(dir) && !crate::clt_install::developer_tools_ready())
        .and_then(|dir| dir.to_str().map(str::to_owned))
}

/// Settle the shim's place in the environment a spawn hands its shell (`env_add`, over what
/// `inherited` reads of this process's): with the directory wanted now (`wanted`), on PATH
/// ahead of `/usr/bin` and behind the person's own earlier entries ([`on_path`]), and named in
/// [`aterm_types::clt_shim::SHIM_ENV`]; without it, a copy left on that PATH (a window started
/// from a shell that had it) is dropped and the variable is emptied. `DEVELOPER_DIR` follows: the
/// shim's developer directory while it is wanted, unless a person's own stands; once the tools
/// are `present`, empty (which Apple's stubs read as unset) in place of one this process kept from
/// before they arrived. With them missing and the directory not wanted (it could not be laid),
/// an inherited one stays: it keeps Apple's stubs off their dialog route, where an empty one
/// would send them there. Decided at each spawn, so a tab opened while the shim is wanted gets
/// it, and one opened after nothing wants it does not.
pub(crate) fn settle_session_path(
    env_add: &mut Vec<(String, String)>,
    inherited: &dyn Fn(&str) -> Option<String>,
    wanted: Option<&str>,
    present: bool,
) {
    let var = aterm_types::clt_shim::SHIM_ENV;
    let given = |env_add: &[(String, String)], key: &str| {
        env_add
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
            .or_else(|| inherited(key))
    };
    let named = given(env_add, var).filter(|value| !value.is_empty());
    let set = |env_add: &mut Vec<(String, String)>, key: &str, value: String| {
        env_add.retain(|(name, _)| name != key);
        env_add.push((key.to_owned(), value));
    };
    if let Some(base) = given(env_add, "PATH") {
        let path = if let Some(shim) = wanted {
            on_path(&base, shim)
        } else {
            let stale: Vec<String> = aterm_types::clt_shim::shim_dir()
                .and_then(|dir| dir.to_str().map(str::to_owned))
                .into_iter()
                .chain(named.clone())
                .collect();
            let sep = if cfg!(windows) { ';' } else { ':' };
            base.split(sep)
                .filter(|entry| !stale.iter().any(|shim| shim == entry))
                .collect::<Vec<_>>()
                .join(&sep.to_string())
        };
        if path != base {
            set(env_add, "PATH", path);
        }
    }
    match wanted {
        Some(shim) => set(env_add, var, shim.to_owned()),
        None if named.is_some() => set(env_add, var, String::new()),
        None => {}
    }
    let developer = aterm_types::clt_shim::DEVELOPER_DIR_ENV;
    let inherited_ours_or_none = given(env_add, developer)
        .filter(|value| !value.is_empty())
        .map(|value| aterm_types::clt_shim::is_shim_developer_dir(Path::new(&value)));
    match (wanted, inherited_ours_or_none) {
        (Some(shim), None | Some(true)) => {
            let dir = aterm_types::clt_shim::developer_dir(Path::new(shim));
            if let Some(dir) = dir.to_str() {
                set(env_add, developer, dir.to_owned());
            }
        }
        (None, Some(true)) if present => set(env_add, developer, String::new()),
        _ => {}
    }
}

/// Whether `/usr/bin/<tool>` exists (the `xcode-select` stub a new Mac carries).
#[cfg(unix)]
fn usr_bin_stub_exists(tool: &str) -> bool {
    Path::new("/usr/bin").join(tool).exists()
}

/// Create `dir` and link each of `tools` onto `aterm_exe`, replacing a stale link, and its developer
/// directory's `xcrun` the same way ([`aterm_types::clt_shim::developer_dir`]). Extra links a
/// shrunken tool set left behind are removed, so the directory holds exactly `tools` and that.
#[cfg(unix)]
fn lay(dir: &Path, aterm_exe: &Path, tools: &[&str]) -> std::io::Result<()> {
    use std::os::unix::fs::symlink;
    let developer_bin =
        aterm_types::clt_shim::developer_dir(dir).join(aterm_types::clt_shim::DEVELOPER_DIR_BIN);
    std::fs::create_dir_all(&developer_bin)?;
    // Replace a link that points elsewhere (a moved bundle); leave a correct one.
    let link_onto_aterm = |link: &Path| -> std::io::Result<()> {
        if std::fs::read_link(link).ok().as_deref() != Some(aterm_exe) {
            let _ = std::fs::remove_file(link);
            symlink(aterm_exe, link)?;
        }
        Ok(())
    };
    link_onto_aterm(&developer_bin.join("xcrun"))?;
    for tool in tools {
        link_onto_aterm(&dir.join(tool))?;
    }
    // Drop a link for a name no longer in the set.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            // Its developer directory is the one thing here that is no link.
            if let Some(name) = entry.file_name().to_str()
                && !tools.contains(&name)
                && !entry.file_type().is_ok_and(|kind| kind.is_dir())
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
#[path = "clt_shim_tests.rs"]
mod tests;
