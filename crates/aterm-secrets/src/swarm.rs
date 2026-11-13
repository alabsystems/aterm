// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `[swarm] enabled` master switch in `aterm.toml`
//! (`docs/DESIGN-init-on-by-default-2026-10-25.md` §8.5,
//! `docs/DESIGN-alab-swarm-2026-10-27.md` §9).

use crate::{Error, Result};
#[cfg(unix)]
use std::path::Path;

/// What a converge or setup row says where `[swarm] enabled = false` turns swarm participation off.
pub const SWARM_OFF: &str = "off: [swarm] enabled = false";

/// Whether `table` leaves swarm participation on: `true` when `[swarm]` or `enabled` is absent.
///
/// # Errors
/// `[swarm]` is not a table, names an unknown setting, or gives `enabled` a non-boolean value.
pub fn enabled_in_table(table: &aterm_toml::Table) -> Result<bool> {
    let Some(swarm) = table.get("swarm") else {
        return Ok(true);
    };
    let Some(swarm) = swarm.as_table() else {
        return Err(Error::new("invalid swarm configuration section"));
    };
    if swarm.keys().any(|key| key != "enabled") {
        return Err(Error::new("unknown swarm configuration setting"));
    }
    match swarm.get("enabled") {
        None => Ok(true),
        Some(aterm_toml::Value::Boolean(enabled)) => Ok(*enabled),
        Some(_) => Err(Error::new("swarm enabled setting must be boolean")),
    }
}

/// Whether a parsed `aterm.toml` [`aterm_toml::Value`] leaves swarm participation on.
///
/// # Errors
/// `value` is not a table, or `[swarm]` fails [`enabled_in_table`].
pub fn enabled_in_value(value: &aterm_toml::Value) -> Result<bool> {
    let table = value
        .as_table()
        .ok_or_else(|| Error::new("invalid swarm configuration"))?;
    enabled_in_table(table)
}

/// Whether `text` (an `aterm.toml` document) leaves swarm participation on.
///
/// # Errors
/// `text` is not valid TOML, or `[swarm]` fails [`enabled_in_table`].
pub fn enabled_in_toml(text: &str) -> Result<bool> {
    let table: aterm_toml::Table =
        aterm_toml::from_str(text).map_err(|why| Error::new(format!("invalid TOML: {why}")))?;
    enabled_in_table(&table)
}

/// Whether the `aterm.toml` at `path` leaves swarm participation on (`true` when `path` is absent).
/// It reads the file by the one rule the window's settings read it by too
/// ([`crate::fsx::read_aterm_config`], [`aterm_types::dirs::aterm_config_refusal`]): a symlink
/// (a dotfiles checkout, home-manager) is followed, and the file it ends at must be this user's
/// or root's, with no group or other write bits, so every reader of the switch hears what the
/// window hears, and a file one of them refuses the others refuse too.
///
/// # Errors
/// `path` (or the file a symlink there names) is not a regular file this user or root owns and
/// others cannot write, is not UTF-8 or valid TOML, or `[swarm]` fails [`enabled_in_table`].
#[cfg(unix)]
pub fn enabled_at(path: &Path) -> Result<bool> {
    let bytes = crate::fsx::read_aterm_config(path, 1024 * 1024)?;
    let Some(bytes) = bytes else {
        return Ok(true);
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::new(format!("{}: not UTF-8 text", path.display())))?;
    enabled_in_toml(text).map_err(|why| Error::new(format!("{}: {why}", path.display())))
}

/// What a row or a log line says where `[swarm] enabled` cannot be read (`why`, an
/// [`enabled_at`] error): in plain words, that aterm cannot read its settings file, and that its
/// automatic sharing waits until it can.
#[must_use]
pub fn unreadable(why: &impl std::fmt::Display) -> String {
    format!(
        "aterm cannot read its settings file ({why}), so it shares nothing on its own until it can"
    )
}

/// Whether `home`'s `aterm.toml` leaves swarm participation on.
///
/// # Errors
/// The resolved `aterm.toml` fails [`enabled_at`].
#[cfg(unix)]
pub fn enabled_in_home(home: &Path) -> Result<bool> {
    match config_in_home(home) {
        Some(path) => enabled_at(&path),
        None => Ok(true),
    }
}

/// The `aterm.toml` [`enabled_in_home`] reads for `home`: this process's `XDG_CONFIG_HOME` or
/// `APPDATA` only where `home` is the live home or holds it.
#[cfg(unix)]
#[must_use]
pub fn config_in_home(home: &Path) -> Option<std::path::PathBuf> {
    let matches_live_home = aterm_types::dirs::home_dir().as_deref() == Some(home);
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|val| matches_live_home || Path::new(val).starts_with(home));
    let appdata = std::env::var_os("APPDATA")
        .filter(|val| matches_live_home || Path::new(val).starts_with(home));
    aterm_types::dirs::resolve_aterm_config_path(
        xdg.as_deref(),
        appdata.as_deref(),
        Some(home.as_os_str()),
    )
}

/// What a file's metadata says of its contents without reading them: its device and inode, its
/// length, and its modification and change times to the nanosecond, so an edit in place and an
/// atomic replace each move it.
#[cfg(unix)]
type Stamp = (u64, u64, u64, i64, i64, i64, i64);

/// The [`Stamp`] of the file `path` resolves to, a symlink followed as [`enabled_at`] follows it:
/// an edit of the file a dotfiles link names moves it.
#[cfg(unix)]
fn stamp_of(path: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).ok()?;
    Some((
        meta.dev(),
        meta.ino(),
        meta.size(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    ))
}

/// `home`'s `[swarm] enabled` for a loop that asks it every turn (a running subscriber, the
/// approver beside it): [`enabled_in_home`]'s answer, with its `aterm.toml` read and parsed again
/// only once the metadata of the file it resolves to moved ([`Stamp`]: an edit, a replace, the
/// file or its symlink's target coming or going) or the path it resolves to did, so a turn costs
/// one `stat`.
#[cfg(unix)]
#[derive(Debug)]
pub struct Switch {
    home: std::path::PathBuf,
    /// The path and stamp of the last read, and what it said.
    read: Option<(Option<std::path::PathBuf>, Option<Stamp>, Result<bool>)>,
}

#[cfg(unix)]
impl Switch {
    /// The switch of `home`'s `aterm.toml`, not read yet.
    #[must_use]
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            read: None,
        }
    }

    /// Whether `home`'s `aterm.toml` leaves swarm participation on now ([`enabled_in_home`]),
    /// read again only where the file moved since the last read.
    ///
    /// # Errors
    /// The resolved `aterm.toml` fails [`enabled_at`].
    pub fn enabled(&mut self) -> Result<bool> {
        let path = config_in_home(&self.home);
        let stamp = path.as_deref().and_then(stamp_of);
        if let Some((read_path, read_stamp, said)) = &self.read
            && *read_path == path
            && *read_stamp == stamp
        {
            return said.clone();
        }
        let said = match &path {
            Some(path) => enabled_at(path),
            None => Ok(true),
        };
        self.read = Some((path, stamp, said.clone()));
        said
    }

    /// [`Self::enabled`], saying in `log` once per change, never once per turn, that the file
    /// cannot be read ([`unreadable`]): a read that fails as the last one did says nothing.
    ///
    /// # Errors
    /// The resolved `aterm.toml` fails [`enabled_at`].
    pub fn enabled_saying(&mut self, log: &dyn Fn(&str)) -> Result<bool> {
        let before = self.read.as_ref().map(|(_, _, said)| said.clone());
        let said = self.enabled();
        if let Err(why) = &said
            && before.as_ref() != Some(&said)
        {
            log(&unreadable(why));
        }
        said
    }
}

/// Whether this process's `aterm.toml` ([`aterm_types::dirs::aterm_config_path`]) leaves swarm
/// participation on.
///
/// # Errors
/// The resolved `aterm.toml` fails [`enabled_at`].
#[cfg(unix)]
pub fn enabled_live() -> Result<bool> {
    let Some(path) = aterm_types::dirs::aterm_config_path() else {
        return Ok(true);
    };
    enabled_at(&path)
}

#[cfg(test)]
#[path = "swarm_tests.rs"]
mod tests;
