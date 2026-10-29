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
///
/// # Errors
/// `path` is not an owned safe file, is not UTF-8 or valid TOML, or `[swarm]` fails
/// [`enabled_in_table`].
#[cfg(unix)]
pub fn enabled_at(path: &Path) -> Result<bool> {
    let bytes = crate::fsx::read_owned(path, 1024 * 1024)
        .map_err(|_| Error::new("aterm.toml is not an owned safe file"))?;
    let Some(bytes) = bytes else {
        return Ok(true);
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| Error::new("aterm.toml is not UTF-8"))?;
    enabled_in_toml(text)
}

/// Whether `home`'s `aterm.toml` leaves swarm participation on.
///
/// # Errors
/// The resolved `aterm.toml` fails [`enabled_at`].
#[cfg(unix)]
pub fn enabled_in_home(home: &Path) -> Result<bool> {
    let Some(path) = aterm_types::dirs::resolve_aterm_config_path(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("APPDATA").as_deref(),
        Some(home.as_os_str()),
    ) else {
        return Ok(true);
    };
    enabled_at(&path)
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
mod tests {
    use super::*;

    /// Absent `[swarm]` and `[swarm] enabled = true` leave swarm participation on;
    /// `[swarm] enabled = false` turns it off; malformed sections or keys fail rather than
    /// silently defaulting on.
    /// NEGATIVE CONTROL: ignoring `[swarm]` would return `Ok(true)` for `[swarm] enabled = false`.
    #[test]
    fn swarm_table_parses_default_true_explicit_false_and_rejects_bad_shapes() {
        assert_eq!(enabled_in_toml(""), Ok(true));
        assert_eq!(enabled_in_toml("[commons]\nenabled = true\n"), Ok(true));
        assert_eq!(enabled_in_toml("[swarm]\n"), Ok(true));
        assert_eq!(enabled_in_toml("[swarm]\nenabled = true\n"), Ok(true));
        assert_eq!(enabled_in_toml("[swarm]\nenabled = false\n"), Ok(false));
        assert!(enabled_in_toml("swarm = false\n").is_err());
        assert!(enabled_in_toml("[swarm]\nenabled = \"off\"\n").is_err());
        assert!(enabled_in_toml("[swarm]\nother = true\n").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn enabled_at_reads_missing_as_on_and_rejects_symlinks() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let config = scratch.path().join("aterm.toml");
        assert_eq!(enabled_at(&config), Ok(true), "missing file defaults on");
        std::fs::write(&config, "[swarm]\nenabled = false\n").unwrap();
        assert_eq!(enabled_at(&config), Ok(false));
        let link = scratch.path().join("linked.toml");
        std::os::unix::fs::symlink(&config, &link).unwrap();
        assert!(enabled_at(&link).is_err(), "symlink rejected");
    }
}
