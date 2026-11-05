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
    /// Whether the developer tools are installed now ([`aterm_types::clt_shim::developer_git`]).
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
                aterm_types::clt_shim::developer_git
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
            present: aterm_types::clt_shim::developer_git,
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
    pub(crate) fn settle(&self, installing: bool) {
        if let Some(dir) = &self.dir {
            refresh(dir, !(self.present)(), installing, self.exe.as_deref());
        }
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
        self.settle(false);
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
        .filter(|dir| is_wanted(dir) && !aterm_types::clt_shim::developer_git())
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
mod tests {
    use super::*;

    /// `entries` joined as this host's PATH joins them (`:`, `;` on Windows).
    fn path(entries: &[&str]) -> String {
        entries.join(if cfg!(windows) { ";" } else { ":" })
    }

    #[test]
    fn the_shim_sits_ahead_of_usr_bin_and_behind_a_person_s_own_tools() {
        // Homebrew's git, earlier on PATH, still wins; the shim only fronts `/usr/bin`.
        let got = on_path(
            &path(&["/opt/homebrew/bin", "/usr/bin", "/bin", "/usr/sbin"]),
            "/S",
        );
        assert_eq!(
            got,
            path(&["/opt/homebrew/bin", "/S", "/usr/bin", "/bin", "/usr/sbin"])
        );
    }

    #[test]
    fn with_no_usr_bin_it_fronts_bin() {
        assert_eq!(
            on_path(&path(&["/usr/local/bin", "/bin"]), "/S"),
            path(&["/usr/local/bin", "/S", "/bin"])
        );
    }

    #[test]
    fn applying_it_twice_changes_nothing_more() {
        let once = on_path(&path(&["/opt/homebrew/bin", "/usr/bin"]), "/S");
        assert_eq!(on_path(&once, "/S"), once);
    }

    #[test]
    fn with_no_system_bin_the_shim_is_not_added() {
        // Nothing to intercept, so the shim stays off PATH (and a stale copy is dropped).
        assert_eq!(
            on_path(&path(&["/opt/homebrew/bin", "/S"]), "/S"),
            "/opt/homebrew/bin"
        );
    }

    /// A spawn decides the shim's place from whether it is wanted at that moment: a tab opened
    /// while it is wanted gets it ahead of `/usr/bin` (behind the person's own earlier entries),
    /// whatever PATH the window composed at its launch, and the variable naming it; one opened
    /// after nothing wants it gets a PATH without it and no variable.
    #[test]
    fn tabs_opened_while_it_is_wanted_get_it_and_after_it_is_not_do_not() {
        let var = aterm_types::clt_shim::SHIM_ENV;
        let composed = || {
            vec![
                ("TERM".to_owned(), "xterm-256color".to_owned()),
                (
                    "PATH".to_owned(),
                    "/opt/homebrew/bin:/usr/bin:/bin".to_owned(),
                ),
            ]
        };
        let get = |env: &[(String, String)], key: &str| {
            env.iter()
                .rev()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };
        let nothing = |_: &str| None;
        let mut wanted = composed();
        settle_session_path(&mut wanted, &nothing, Some("/S"), false);
        assert_eq!(
            get(&wanted, "PATH").as_deref(),
            Some("/opt/homebrew/bin:/S:/usr/bin:/bin")
        );
        assert_eq!(get(&wanted, var).as_deref(), Some("/S"));
        for key in ["PATH", var] {
            assert_eq!(wanted.iter().filter(|(name, _)| name == key).count(), 1);
        }

        // No PATH composed at launch: the inherited one carries it.
        let mut bare = vec![("TERM".to_owned(), "xterm-256color".to_owned())];
        let inherited = |key: &str| (key == "PATH").then(|| "/usr/bin:/bin".to_owned());
        settle_session_path(&mut bare, &inherited, Some("/S"), false);
        assert_eq!(get(&bare, "PATH").as_deref(), Some("/S:/usr/bin:/bin"));
        assert_eq!(get(&bare, var).as_deref(), Some("/S"));

        // Not wanted: a copy left on the PATH is dropped, and a clean PATH is left as it was.
        let shim = aterm_types::clt_shim::shim_dir().unwrap();
        let shim = shim.to_str().unwrap();
        let mut stale = vec![("PATH".to_owned(), format!("{shim}:/usr/bin"))];
        settle_session_path(&mut stale, &nothing, None, true);
        assert_eq!(get(&stale, "PATH").as_deref(), Some("/usr/bin"));
        assert_eq!(get(&stale, var), None, "no variable where none was");
        let mut clean = composed();
        settle_session_path(&mut clean, &nothing, None, true);
        assert_eq!(clean, composed());
        let mut none = vec![("TERM".to_owned(), "xterm-256color".to_owned())];
        let clean_path = |key: &str| (key == "PATH").then(|| "/usr/bin".to_owned());
        settle_session_path(&mut none, &clean_path, None, true);
        assert_eq!(
            get(&none, "PATH"),
            None,
            "an inherited clean PATH is not restated"
        );

        // A window started from a shell that had the shim: its variable is emptied, and the
        // directory it names is dropped from the PATH.
        let mut heir = vec![("TERM".to_owned(), "xterm-256color".to_owned())];
        let from_a_shim_shell = |key: &str| match key {
            "PATH" => Some("/opt/homebrew/bin:/T:/usr/bin".to_owned()),
            key if key == var => Some("/T".to_owned()),
            _ => None,
        };
        settle_session_path(&mut heir, &from_a_shim_shell, None, true);
        assert_eq!(
            get(&heir, "PATH").as_deref(),
            Some("/opt/homebrew/bin:/usr/bin")
        );
        assert_eq!(get(&heir, var).as_deref(), Some(""));
    }

    /// While the tools are missing the shim stays wanted, whatever becomes of an install: only its
    /// installing mark, which changes a shim's line, follows the install. Once they exist both
    /// marks go at once, and the directory, with its links, stays for the processes that reach
    /// it; a next launch keeps it too, its developer directory's `xcrun` linked onto that launch's
    /// executable, so a process that inherited `DEVELOPER_DIR` from before still reaches a shim.
    /// NEGATIVE CONTROL: on a Mac with the tools a launch lays nothing at all.
    #[cfg(unix)]
    #[test]
    fn the_shim_stays_until_the_tools_exist_and_only_then_comes_off_new_shells() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static PRESENT: AtomicBool = AtomicBool::new(false);
        let tmp = aterm_tempfile::tempdir().unwrap();
        let dir = tmp.path().join("clt-shim");
        let installing = || aterm_types::clt_shim::installing_mark(&dir).is_file();
        let shim = Shim::at(dir.clone(), || PRESENT.load(Ordering::SeqCst));
        shim.reconcile();
        assert!(
            shim.laid() && shim.wanted(),
            "from the launch, with no pass"
        );
        assert!(!installing());
        shim.settle(true);
        assert!(shim.wanted() && installing());
        shim.settle(false);
        assert!(shim.wanted(), "a Stop, or an unfinished install, leaves it");
        assert!(!installing());
        let links = std::fs::read_dir(&dir).unwrap().count();
        PRESENT.store(true, Ordering::SeqCst);
        shim.settle(true);
        assert!(!shim.wanted() && !installing(), "the tools exist");
        assert!(shim.laid());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), links);
        let xcrun = aterm_types::clt_shim::developer_dir(&dir)
            .join(aterm_types::clt_shim::DEVELOPER_DIR_BIN)
            .join("xcrun");
        std::fs::remove_file(&xcrun).unwrap();
        shim.reconcile();
        assert!(
            shim.laid() && !shim.wanted(),
            "the next launch keeps it, unwanted"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), links);
        assert_eq!(
            std::fs::read_link(&xcrun).unwrap(),
            std::env::current_exe().unwrap(),
            "its developer directory's xcrun, linked onto this launch's executable"
        );

        let owner = tmp.path().join("owner").join("clt-shim");
        let with_the_tools = Shim::at(owner.clone(), || true);
        with_the_tools.reconcile();
        assert!(!owner.exists(), "the tools are here: nothing is laid");
    }

    /// Apple's real `/usr/bin/git` with `DEVELOPER_DIR` naming the directory [`lay`] lays runs that
    /// directory's `xcrun`, whose target here is a stand-in that notes it ran; the real one is
    /// `aterm`, whose shim prints a line and exits. Named before the directory is laid (as a
    /// process start names it) the stub fails plainly, with Apple's own error and no dialog.
    /// Measured against the real stub, so only on a Mac whose tools answer in place of its dialog.
    /// NEGATIVE CONTROL: without the variable the same stub never reaches the stand-in.
    #[cfg(target_os = "macos")]
    #[test]
    fn apples_real_stub_reaches_the_laid_developer_dir_and_never_its_install_route() {
        use std::os::unix::fs::PermissionsExt as _;
        if !aterm_types::clt_shim::developer_git() {
            eprintln!("this Mac has no developer tools: its stub would raise Apple's dialog");
            return;
        }
        let tmp = aterm_tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let noted = root.join("noted");
        let stand_in = root.join("stand-in");
        std::fs::write(
            &stand_in,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\nexit 69\n",
                noted.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
        let shim = root.join("clt-shim");
        let developer = aterm_types::clt_shim::developer_dir(&shim);
        let git = |dir: Option<&Path>| {
            let mut git = aterm_uds::spawnfd::command("/usr/bin/git");
            git.arg("--version")
                .env_remove(aterm_types::clt_shim::DEVELOPER_DIR_ENV);
            if let Some(dir) = dir {
                git.env(aterm_types::clt_shim::DEVELOPER_DIR_ENV, dir);
            }
            git.output().unwrap()
        };
        let unlaid = git(Some(&developer));
        assert_eq!(unlaid.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&unlaid.stderr).contains("DEVELOPER_DIR"),
            "{unlaid:?}"
        );
        lay(&shim, &stand_in, &["git"]).unwrap();
        let laid = git(Some(&developer));
        assert_eq!(laid.status.code(), Some(69), "{laid:?}");
        assert_eq!(
            std::fs::read_to_string(&noted).unwrap(),
            "git --version\n",
            "Apple's stub called the laid xcrun with the tool and its words"
        );
        std::fs::remove_file(&noted).unwrap();
        assert!(git(None).status.success(), "the tools' own git");
        assert!(!noted.exists(), "no variable, no stand-in");
    }

    /// A spawn hands `DEVELOPER_DIR` on while the shim is wanted, a person's own excepted, and
    /// empties one this process kept from before the tools arrived, only once they did.
    /// NEGATIVE CONTROL: with the tools still missing and the directory not wanted (its laying
    /// failed), the inherited one stays, as an empty one would send Apple's stubs to their dialog.
    #[test]
    fn a_spawn_names_the_developer_dir_only_while_the_shim_is_wanted() {
        let key = aterm_types::clt_shim::DEVELOPER_DIR_ENV;
        let get = |env: &[(String, String)]| {
            env.iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        };
        let ours = "/s/clt-shim/developer";
        let with = |value: &'static str| move |k: &str| (k == key).then(|| value.to_owned());
        let nothing = |_: &str| None;

        let mut env = Vec::new();
        settle_session_path(&mut env, &nothing, Some("/s/clt-shim"), false);
        assert_eq!(get(&env).as_deref(), Some(ours));

        let mut env = Vec::new();
        settle_session_path(&mut env, &with("/opt/xcode"), Some("/s/clt-shim"), false);
        assert_eq!(get(&env), None, "a person's own stands");

        let mut env = Vec::new();
        settle_session_path(&mut env, &with(ours), None, true);
        assert_eq!(get(&env).as_deref(), Some(""), "the tools arrived");

        let mut env = Vec::new();
        settle_session_path(&mut env, &with(ours), None, false);
        assert_eq!(get(&env), None, "still missing: the inherited one stays");

        for inherited in ["/opt/xcode", ""] {
            for present in [false, true] {
                let mut env = Vec::new();
                settle_session_path(&mut env, &with(inherited), None, present);
                assert_eq!(get(&env), None, "{inherited:?}");
            }
        }
        let mut env = Vec::new();
        settle_session_path(&mut env, &nothing, None, true);
        assert_eq!(get(&env), None, "the owner's Mac gets nothing");
    }

    /// A launch never takes the shim directory from under a live shell, whether it adopted that
    /// shell (a seamless hand-off to an updated aterm, the PTY keeper after a crash) or the shell
    /// outlived the window on its own (a tmux server started from a tab): a bash there that hashed
    /// `<shim>/git` while the install ran would fail `No such file or directory` (127). The launch
    /// takes only the mark. Stand-ins only: the shim's `git` and the next directory's are scripts
    /// of this test's own.
    #[cfg(unix)]
    #[test]
    fn a_launch_leaves_a_live_shells_shim_runnable() {
        use std::io::{BufRead as _, Read as _, Write as _};
        let tmp = aterm_tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let shim = root.join("clt-shim");
        let real = root.join("real");
        let aterm = root.join("aterm");
        std::fs::create_dir_all(&shim).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(aterm_types::clt_shim::wanted_mark(&shim), b"").unwrap();
        let script = |path: &Path, body: &str| {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        script(&aterm, "echo shim");
        std::os::unix::fs::symlink(&aterm, shim.join("git")).unwrap();
        script(&real.join("git"), "echo real");
        let mut child = aterm_uds::spawnfd::command("/bin/bash")
            .args([
                "--noprofile",
                "--norc",
                "-c",
                "git; read _; git; echo rc=$?",
            ])
            .env_clear()
            .env("PATH", format!("{}:{}", shim.display(), real.display()))
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut first = String::new();
        stdout.read_line(&mut first).unwrap();
        assert_eq!(first.trim(), "shim", "the install under way");
        // The successor's launch with the tools there, its predecessor's shells adopted alive.
        let kept = Shim::at(shim.clone(), || true).onto(aterm.clone());
        kept.reconcile();
        assert!(!kept.wanted(), "new shells no longer get it");
        assert!(kept.laid(), "the adopted shells' directory stays");
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"\n").unwrap();
        drop(stdin);
        let mut rest = String::new();
        stdout.read_to_string(&mut rest).unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            rest,
            "shim\nrc=0\n",
            "an adopted bash keeps its hashed git: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A bash a person opened while the shim was wanted, which ran `git` then (and so hashed it
    /// at the shim's path), keeps working once the tools arrive: the window takes the want away
    /// and leaves the directory, so that bash's next `git` reaches the shim, which runs the
    /// tools' own `git`. Before, the directory went with the want and that `git` failed `No such
    /// file or directory` (exit 127) until `hash -r`. Stand-ins only: the shim's `git` (which
    /// runs the real one once the tools are there, as `aterm_cli::clt_shim` does) and the real
    /// one are scripts of this test's own.
    #[cfg(unix)]
    #[test]
    fn a_bash_that_ran_git_while_the_shim_was_laid_runs_git_once_the_tools_arrive() {
        use std::io::{BufRead as _, Read as _, Write as _};
        let tmp = aterm_tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let shim = root.join("clt-shim");
        let real = root.join("real");
        let arrived = root.join("arrived");
        std::fs::create_dir_all(&shim).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(aterm_types::clt_shim::wanted_mark(&shim), b"").unwrap();
        let script = |path: &Path, body: &str| {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        script(
            &shim.join("git"),
            &format!(
                "[ -e '{}' ] && exec '{}' \"$@\"\necho shim",
                arrived.display(),
                real.join("git").display()
            ),
        );
        script(&real.join("git"), "echo real");
        let mut child = aterm_uds::spawnfd::command("/bin/bash")
            .args([
                "--noprofile",
                "--norc",
                "-c",
                "git; read _; git; echo rc=$?",
            ])
            .env_clear()
            .env("PATH", format!("{}:{}", shim.display(), real.display()))
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .stderr(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut first = String::new();
        stdout.read_line(&mut first).unwrap();
        assert_eq!(first.trim(), "shim", "the tools still installing");
        // The tools arrived: the window settles the shim.
        std::fs::write(&arrived, b"").unwrap();
        let kept = Shim::at(shim.clone(), || true);
        kept.settle(true);
        assert!(!kept.wanted(), "new shells no longer get it");
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"\n").unwrap();
        drop(stdin);
        let mut rest = String::new();
        stdout.read_to_string(&mut rest).unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            rest,
            "real\nrc=0\n",
            "the shell's hashed git, once the tools arrived: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
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
