// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The git-and-friends shim directory the window keeps on a Mac whose Command Line Tools are
//! installing (System State design §6.3).
//!
//! While a setup pass is installing Apple's Command Line Tools ([`crate::clt_install`]) and they
//! are missing, the window lays a directory of symlinks — one per
//! [`aterm_types::clt_shim::SHIM_TOOLS`] name that exists as a `/usr/bin` stub — each onto the
//! one `aterm` binary, and marks it wanted ([`Shim::settle`],
//! [`aterm_types::clt_shim::wanted_mark`]). Every shell the window spawns while it is wanted gets
//! that directory ahead of `/usr/bin` on its PATH and named in
//! [`aterm_types::clt_shim::SHIM_ENV`], decided at the spawn ([`settle_session_path`]); the
//! shell integration moves it back ahead of `/usr/bin` once the profile files have run, since a
//! login shell's `path_helper` puts `/usr/bin` first. So a tab opened during the install reaches
//! `aterm` when it runs `git`, which says the tools are on their way ([`aterm_cli::clt_shim`])
//! instead of raising macOS's install dialog. The mark goes the moment nothing installs the
//! tools or they arrived (a person's Stop, the Developer group turned off, an install that ended
//! unfinished, an account that may not install them, the tools installed), and a shell spawned
//! after that gets neither the PATH entry nor the variable. The directory itself stays until the
//! next launch removes it ([`Shim::reconcile`]), or longer when that launch adopted live shells:
//! a shell that already ran `git` through it (bash hashes the path) still reaches a shim, which
//! hands the call to Apple's own `/usr/bin/git` (with the tools there, or once nothing installs
//! them).
//!
//! The owner's Mac, which already has the tools, never lays it, leaving every shell's PATH
//! untouched.

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

/// The shim directory this window keeps, and how it reads whether the tools are installed.
pub(crate) struct Shim {
    /// Where it is laid: this account's ([`aterm_types::clt_shim::shim_dir`]), a test's own, or
    /// nowhere (a test that lays none, or no state home).
    dir: Option<PathBuf>,
    /// Whether the developer tools are installed now ([`aterm_types::clt_shim::developer_git`]).
    present: fn() -> bool,
}

impl Default for Shim {
    /// A shim laid nowhere.
    fn default() -> Self {
        Self {
            dir: None,
            present: aterm_types::clt_shim::developer_git,
        }
    }
}

impl Shim {
    /// This account's shim, as a launch keeps it; none in a test, which never touches the
    /// account's state.
    pub(crate) fn for_launch() -> Self {
        Self {
            dir: if cfg!(test) {
                None
            } else {
                aterm_types::clt_shim::shim_dir()
            },
            present: aterm_types::clt_shim::developer_git,
        }
    }

    /// A shim laid at `dir`, the tools installed as `present` says (a test's; only a Unix host
    /// lays one).
    #[cfg(all(test, unix))]
    pub(crate) const fn at(dir: PathBuf, present: fn() -> bool) -> Self {
        Self {
            dir: Some(dir),
            present,
        }
    }

    /// Lay the directory and mark it wanted when `wanted` (a pass is installing the Command Line
    /// Tools now) and they are missing; else take the mark away, at once, and leave the directory
    /// to the next launch. Best effort: a Mac that cannot lay it keeps raising Apple's own dialog,
    /// as it did before.
    pub(crate) fn settle(&self, wanted: bool) {
        if let Some(dir) = &self.dir {
            refresh(dir, wanted && !(self.present)());
        }
    }

    /// This shim as a launch keeps it: no pass wants the tools yet, so the mark an earlier launch
    /// left (one that ended mid-install, stopped, or crashed) goes, and no shell is told the tools
    /// are on their way while nothing installs them. The directory goes with it only when this
    /// launch `adopted` no live shells (a seamless update's hand-off, the PTY keeper after a
    /// crash): an adopted shell may have it on its PATH, and a bash there hashed `<shim>/git`,
    /// which would fail `No such file or directory` (127) with the directory gone. Its shim
    /// hands the call to Apple's own tool instead, until a pass installs the tools again.
    pub(crate) fn reconcile(&self, adopted: bool) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_file(aterm_types::clt_shim::wanted_mark(dir));
            if !adopted {
                // We own this directory; removing it drops our symlinks, never their targets.
                let _ = std::fs::remove_dir_all(dir);
            }
        }
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
    dir.is_dir() && aterm_types::clt_shim::wanted_mark(dir).is_file()
}

/// Lay `dir` and mark it wanted when `active`, or take the mark away when not (the directory
/// stays for the shells that have it). Each link is `name -> aterm_exe`; the set is
/// [`aterm_types::clt_shim::SHIM_TOOLS`] filtered to the names that exist as `/usr/bin` stubs
/// now. The mark is written only once the links are there, so a spawn that reads it finds them.
#[cfg(unix)]
fn refresh(dir: &Path, active: bool) {
    let mark = aterm_types::clt_shim::wanted_mark(dir);
    if !active {
        let _ = std::fs::remove_file(&mark);
        return;
    }
    let Ok(aterm_exe) = std::env::current_exe() else {
        return;
    };
    let tools = aterm_types::clt_shim::runtime_tools(usr_bin_stub_exists);
    if lay(dir, &aterm_exe, &tools).is_ok() {
        let _ = std::fs::write(&mark, b"");
    }
}

#[cfg(not(unix))]
fn refresh(_dir: &std::path::Path, _active: bool) {}

/// This account's shim directory while it is wanted: what a spawn puts on its shell's PATH and
/// names in [`aterm_types::clt_shim::SHIM_ENV`] ([`settle_session_path`]), and what a warm
/// spare's spawn shares with a cold tab's.
#[must_use]
pub(crate) fn wanted_dir() -> Option<String> {
    aterm_types::clt_shim::shim_dir()
        .filter(|dir| is_wanted(dir))
        .and_then(|dir| dir.to_str().map(str::to_owned))
}

/// Settle the shim's place in the environment a spawn hands its shell (`env_add`, over what
/// `inherited` reads of this process's): with the directory wanted now (`wanted`), on PATH
/// ahead of `/usr/bin` and behind the person's own earlier entries ([`on_path`]), and named in
/// [`aterm_types::clt_shim::SHIM_ENV`]; without it, a copy left on that PATH (a window started
/// from a shell that had it) is dropped and the variable is emptied. Decided at each spawn, so a
/// tab opened while the shim is wanted gets it, and one opened after nothing wants it does not.
pub(crate) fn settle_session_path(
    env_add: &mut Vec<(String, String)>,
    inherited: &dyn Fn(&str) -> Option<String>,
    wanted: Option<&str>,
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
}

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
        settle_session_path(&mut wanted, &nothing, Some("/S"));
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
        settle_session_path(&mut bare, &inherited, Some("/S"));
        assert_eq!(get(&bare, "PATH").as_deref(), Some("/S:/usr/bin:/bin"));
        assert_eq!(get(&bare, var).as_deref(), Some("/S"));

        // Not wanted: a copy left on the PATH is dropped, and a clean PATH is left as it was.
        let shim = aterm_types::clt_shim::shim_dir().unwrap();
        let shim = shim.to_str().unwrap();
        let mut stale = vec![("PATH".to_owned(), format!("{shim}:/usr/bin"))];
        settle_session_path(&mut stale, &nothing, None);
        assert_eq!(get(&stale, "PATH").as_deref(), Some("/usr/bin"));
        assert_eq!(get(&stale, var), None, "no variable where none was");
        let mut clean = composed();
        settle_session_path(&mut clean, &nothing, None);
        assert_eq!(clean, composed());
        let mut none = vec![("TERM".to_owned(), "xterm-256color".to_owned())];
        let clean_path = |key: &str| (key == "PATH").then(|| "/usr/bin".to_owned());
        settle_session_path(&mut none, &clean_path, None);
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
        settle_session_path(&mut heir, &from_a_shim_shell, None);
        assert_eq!(
            get(&heir, "PATH").as_deref(),
            Some("/opt/homebrew/bin:/usr/bin")
        );
        assert_eq!(get(&heir, var).as_deref(), Some(""));
    }

    /// A person's Stop (or the Developer group turned off) with the tools still missing takes
    /// the shim off new shells at once, and leaves the directory, with its links, to the next
    /// launch, so a shell that already reaches it is handed to Apple's own tool, not to a
    /// missing file. A launch then removes the directory and its mark.
    #[cfg(unix)]
    #[test]
    fn a_stop_takes_the_shim_off_new_shells_and_leaves_the_directory_to_the_next_launch() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let dir = tmp.path().join("clt-shim");
        let shim = Shim::at(dir.clone(), || false);
        shim.settle(true);
        assert!(
            shim.laid() && shim.wanted(),
            "a pass wants the missing tools"
        );
        let links = std::fs::read_dir(&dir).unwrap().count();
        shim.settle(false);
        assert!(!shim.wanted(), "new shells no longer get it");
        assert!(
            shim.laid(),
            "the directory stays for the shells that have it"
        );
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            links,
            "its links too"
        );
        shim.reconcile(false);
        assert!(!shim.laid() && !shim.wanted(), "the next launch removes it");
        assert!(!aterm_types::clt_shim::wanted_mark(&dir).exists());
    }

    /// A launch that adopts live shells (a seamless hand-off to an updated aterm, or the PTY
    /// keeper after a crash: "the same live shell") must not take their shim directory from under
    /// them: a bash there that hashed `<shim>/git` while the install ran would fail `No such file
    /// or directory` (127). That launch takes only the mark. Stand-ins only: the shim's `git` and
    /// the next directory's are scripts of this test's own.
    #[cfg(unix)]
    #[test]
    fn a_launch_that_adopts_live_shells_leaves_their_shim_runnable() {
        use std::io::{BufRead as _, Read as _, Write as _};
        let tmp = aterm_tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let shim = root.join("clt-shim");
        let real = root.join("real");
        std::fs::create_dir_all(&shim).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(aterm_types::clt_shim::wanted_mark(&shim), b"").unwrap();
        let script = |path: &Path, body: &str| {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        script(&shim.join("git"), "echo shim");
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
        // The successor's launch, its predecessor's shells adopted alive.
        let kept = Shim::at(shim.clone(), || false);
        kept.reconcile(true);
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
        // The tools arrived: the window settles the want.
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
