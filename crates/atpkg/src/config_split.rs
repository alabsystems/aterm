// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Which `aterm.toml` a shell's aterm and the app read, when they are not the same file.
//!
//! On macOS an app opened from Finder, the Dock or `open` runs in launchd's environment, which
//! has no `XDG_CONFIG_HOME` unless someone ran `launchctl setenv`. A shell that exports one makes
//! every aterm it starts (the CLI, `aterm pkg`, `aterm update`, a terminal session, a window
//! opened as `aterm --window`) read `$XDG_CONFIG_HOME/aterm/aterm.toml`, while the app reads
//! `~/.config/aterm/aterm.toml`, and Settings writes that one (round 13 of the install audits:
//! `[update] enabled = false` set in Settings left the shell's `aterm update status` saying
//! nothing of it, and the reverse). Nothing here moves or rewrites either file. The split is
//! said where a person looks (`aterm explain-config`, `aterm doctor`), with the one step that
//! makes the two one folder while one side does not exist yet: a symlink, undone by removing
//! it, which the person runs.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The two aterm config folders, when they differ.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    /// The folder this process reads (`$XDG_CONFIG_HOME/aterm`).
    pub here: PathBuf,
    /// The folder the app launchd starts reads.
    pub app: PathBuf,
    /// launchd's `XDG_CONFIG_HOME`, when it has one.
    pub app_xdg: Option<PathBuf>,
    /// The keys whose values differ between the two `aterm.toml` files, dotted
    /// (`update.enabled`), when both exist and parse.
    pub differs: Vec<String>,
    /// Whether both `aterm.toml` files exist and parse, so [`Split::differs`] compared them.
    pub compared: bool,
    /// The symlink that makes the two one folder, as `(link, target)`: offered only while
    /// `link` does not exist, so it replaces nothing.
    pub unify: Option<(PathBuf, PathBuf)>,
}

/// The aterm config folder under `xdg` (`XDG_CONFIG_HOME`) and `home`, as
/// [`aterm_types::dirs::resolve_aterm_config_dir`] picks it.
fn folder(xdg: Option<&OsStr>, home: &Path) -> Option<PathBuf> {
    aterm_types::dirs::resolve_aterm_config_dir(xdg, None, Some(home.as_os_str()))
}

/// Whether paths identify the same config location, including a shared missing suffix
/// under aliased parent directories.
fn same_folder(
    a: &Path,
    b: &Path,
    exists: &impl Fn(&Path) -> std::io::Result<bool>,
) -> Result<bool, String> {
    if aterm_types::dirs::same_directory(a, b) {
        return Ok(true);
    }
    if a.file_name() == b.file_name()
        && !probe_exists(a, exists)?
        && !probe_exists(b, exists)?
        && let Some((a, b)) = a.parent().zip(b.parent())
    {
        return same_folder(a, b, exists);
    }
    Ok(false)
}

/// The split between this process's `XDG_CONFIG_HOME` (`here_xdg`) and launchd's (`app_xdg`)
/// under `home`, or `Ok(None)` when both read one folder.
/// # Errors
/// A configuration path cannot be resolved or its existence cannot be read.
pub fn split(
    here_xdg: Option<&OsStr>,
    app_xdg: Option<&OsStr>,
    home: &Path,
) -> Result<Option<Split>, String> {
    split_with(here_xdg, app_xdg, home, &directory_exists)
}

fn directory_exists(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn probe_exists(
    path: &Path,
    exists: &impl Fn(&Path) -> std::io::Result<bool>,
) -> Result<bool, String> {
    exists(path)
        .map_err(|error| format!("read configuration directory {}: {error}", path.display()))
}

fn split_with(
    here_xdg: Option<&OsStr>,
    app_xdg: Option<&OsStr>,
    home: &Path,
    exists: &impl Fn(&Path) -> std::io::Result<bool>,
) -> Result<Option<Split>, String> {
    let here = folder(here_xdg, home)
        .ok_or_else(|| "this process's configuration directory is unavailable".to_string())?;
    let app = folder(app_xdg, home)
        .ok_or_else(|| "the app's configuration directory is unavailable".to_string())?;
    if same_folder(&here, &app, exists)? {
        return Ok(None);
    }
    let unify = match (probe_exists(&here, exists)?, probe_exists(&app, exists)?) {
        (true, true) => None,
        (_, false) => Some((app.clone(), here.clone())),
        (false, true) => Some((here.clone(), app.clone())),
    };
    let differs = differing_keys(&here.join("aterm.toml"), &app.join("aterm.toml"));
    Ok(Some(Split {
        here,
        app,
        app_xdg: app_xdg.filter(|xdg| !xdg.is_empty()).map(PathBuf::from),
        compared: differs.is_some(),
        differs: differs.unwrap_or_default(),
        unify,
    }))
}

/// The dotted keys whose values differ between two `aterm.toml` files; `None` unless both
/// exist and parse.
fn differing_keys(a: &Path, b: &Path) -> Option<Vec<String>> {
    let read = |path: &Path| -> Option<aterm_toml::Table> {
        let text = crate::metadata_io::read_bounded_regular_utf8_follow_final_links(
            path,
            crate::config::MAX_PACKAGES_CONFIG_BYTES,
        )
        .ok()?;
        aterm_toml::from_str(&text).ok()
    };
    let (a, b) = (read(a)?, read(b)?);
    let mut keys = Vec::new();
    diff_tables("", &a, &b, &mut keys);
    keys.sort();
    Some(keys)
}

fn diff_tables(prefix: &str, a: &aterm_toml::Table, b: &aterm_toml::Table, out: &mut Vec<String>) {
    let mut names: Vec<&String> = a.keys().chain(b.keys()).collect();
    names.sort();
    names.dedup();
    for name in names {
        let key = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        match (a.get(name), b.get(name)) {
            (Some(aterm_toml::Value::Table(a)), Some(aterm_toml::Value::Table(b))) => {
                diff_tables(&key, a, b, out);
            }
            (a, b) if a != b => out.push(key),
            _ => {}
        }
    }
}

/// How long `launchctl getenv` gets to answer.
#[cfg(target_os = "macos")]
const LAUNCHCTL_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// Combined group-signal/reap polling after an unanswered read-only query.
#[cfg(target_os = "macos")]
const LAUNCHCTL_CLEANUP: std::time::Duration = std::time::Duration::from_millis(250);

/// launchd's `XDG_CONFIG_HOME` for this account, the one an app opened from Finder inherits;
/// `None` when a successful query reports no value.
#[cfg(target_os = "macos")]
fn launchd_xdg_config_home() -> Result<Option<std::ffi::OsString>, String> {
    let mut cmd = aterm_uds::spawnfd::command("/bin/launchctl");
    cmd.args(["getenv", "XDG_CONFIG_HOME"]);
    let (out, ()) = match aterm_uds::deadline::output_until_complete_stdout_retained(
        &mut cmd,
        std::time::Instant::now() + LAUNCHCTL_BUDGET,
        LAUNCHCTL_CLEANUP,
        (),
    ) {
        Ok(answer) => answer,
        Err(failure) => {
            let (error, custody) = failure.into_parts();
            // Keep any dispatched child owned until a positive reap.
            custody.reap_in_background();
            return Err(format!("read launchd's XDG_CONFIG_HOME: {error}"));
        }
    };
    launchd_value(out)
}

/// Decode only the complete stdout of a successful query, preserving native path bytes.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn launchd_value(out: std::process::Output) -> Result<Option<std::ffi::OsString>, String> {
    use std::os::unix::ffi::OsStringExt as _;
    if !out.status.success() {
        return Err(format!(
            "launchctl getenv XDG_CONFIG_HOME failed: {}",
            out.status
        ));
    }
    let mut value = out.stdout;
    while value.last().is_some_and(|b| *b == b'\n') {
        value.pop();
    }
    Ok((!value.is_empty()).then(|| std::ffi::OsString::from_vec(value)))
}

/// The split for this process on macOS: its `XDG_CONFIG_HOME` against launchd's. Elsewhere,
/// and in a test binary ([`aterm_types::dirs::test_root`]), `Ok(None)`: a desktop session there
/// starts apps with the session's environment, and a test never asks the host's launchd.
/// # Errors
/// On macOS, the home directory cannot be resolved, or launchd's environment query fails
/// or does not complete, or a configuration directory's existence cannot be read.
pub fn of_process() -> Result<Option<Split>, String> {
    #[cfg(target_os = "macos")]
    {
        if aterm_types::dirs::test_root().is_some() {
            return Ok(None);
        }
        let home = aterm_types::dirs::home_dir()
            .ok_or_else(|| "the home directory is unavailable".to_string())?;
        let here = std::env::var_os("XDG_CONFIG_HOME");
        let app = launchd_xdg_config_home()?;
        split(here.as_deref(), app.as_deref(), &home)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(None)
    }
}

impl Split {
    /// `path` as a person reads it under `home`: `~/…`, else whole.
    fn shown(path: &Path, home: &Path) -> String {
        path.strip_prefix(home).map_or_else(
            |_| path.display().to_string(),
            |rest| {
                let rel: Vec<_> = rest
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect();
                if rel.is_empty() {
                    "~".to_owned()
                } else {
                    format!("~/{}", rel.join("/"))
                }
            },
        )
    }

    /// The paragraph a person reads: where each reads, what differs, and the step that makes
    /// them one (or that only a merge by hand does).
    #[must_use]
    pub fn said(&self, home: &Path) -> String {
        let here = Self::shown(&self.here.join("aterm.toml"), home);
        let app = Self::shown(&self.app.join("aterm.toml"), home);
        let why = match &self.app_xdg {
            Some(xdg) => format!(
                "launchd gives apps XDG_CONFIG_HOME={}",
                Self::shown(xdg, home)
            ),
            None => "launchd gives apps no XDG_CONFIG_HOME".to_owned(),
        };
        let mut said = format!(
            "this aterm reads {here}, but the app opened from Finder, the Dock or `open` reads \
             {app} ({why}), and Settings writes that one: a setting in either file is not seen \
             by the aterm that reads the other"
        );
        if self.compared && self.differs.is_empty() {
            said.push_str("; the two say the same today");
        } else if !self.differs.is_empty() {
            let shown: Vec<&str> = self.differs.iter().take(8).map(String::as_str).collect();
            let more = self.differs.len().saturating_sub(shown.len());
            said.push_str(&format!(
                "; they differ in {}{}",
                shown.join(", "),
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                }
            ));
        }
        match (&self.unify, self.unify_command()) {
            (Some((link, _)), Some(command)) => said.push_str(&format!(
                ". To make them one folder: {command} (undo: rm {})",
                shell_word(link),
            )),
            _ => said.push_str(&format!(
                ". Both exist, so nothing links them: merge what you keep into one file, then \
                 link the other's folder to it ({} and {})",
                Self::shown(&self.here, home),
                Self::shown(&self.app, home)
            )),
        }
        said
    }

    /// The command that makes the two one folder: the target and the link's parent made when
    /// they are missing, then the link.
    #[must_use]
    pub fn unify_command(&self) -> Option<String> {
        let (link, target) = self.unify.as_ref()?;
        let parent = link.parent()?;
        let mut steps = Vec::new();
        if !target.is_dir() {
            steps.push(format!("mkdir -p {}", shell_word(target)));
        }
        if !parent.is_dir() {
            steps.push(format!("mkdir -p {}", shell_word(parent)));
        }
        steps.push(format!("ln -s {} {}", shell_word(target), shell_word(link)));
        Some(steps.join(" && "))
    }
}

/// `path` as one shell word: bare when it holds only characters no shell treats specially,
/// else single-quoted.
fn shell_word(path: &Path) -> String {
    let text = path.to_string_lossy();
    if !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+,@%".contains(c))
    {
        text.into_owned()
    } else {
        crate::platform::sh_quote_str(&text)
    }
}

#[cfg(test)]
#[path = "config_split_tests.rs"]
mod tests;
