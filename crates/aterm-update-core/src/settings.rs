// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `[update] enabled` — "Check for updates automatically" — read by the updater itself;
//! and `[update] auto_apply` for the headless `aterm update` verbs, which say whether a
//! downloaded build installs by itself.
//!
//! THE ONE SWITCH (2026-09-23). `$ATERM_NO_AUTO_UPDATE` was the only way to turn the
//! app updater off, and an environment variable reaches only the processes one shell
//! launched: never the window a Dock click or a login item starts, never a nested
//! instance past the deny-list. The owner's rule is that a person's controls are
//! Settings, not environment ("NOT ENV VARS those are for development"), so the switch
//! is a key in the SAME `aterm.toml` the window reads — Settings ▸ Terminal ▸ Updates
//! writes it — and every process that runs the updater (the window, a terminal session, the
//! `aterm update` verbs) reads it here, identically.
//!
//! Only the `[update]` table is deserialized, on its own (atpkg's one-table wrapper
//! discipline), so a typo in another table cannot flip it. A file that is missing,
//! unreadable or malformed reads as the default, ON: an updater a typo switched off is
//! a machine that silently stops receiving security fixes, and the window names a
//! config it could not read on its own.

use std::path::{Path, PathBuf};

/// The same admission budget the native config service and atpkg's reader use.
const MAX_CONFIG_BYTES: u64 = 512 * 1024;

/// The path of the user config file — the GUI's resolution
/// (`app_config::config_path`), mirrored as atpkg's `config::config_path` mirrors it:
/// `$XDG_CONFIG_HOME/aterm/aterm.toml`, else (Windows) `%APPDATA%\aterm\aterm.toml`,
/// else `$HOME/.config/aterm/aterm.toml`.
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("aterm").join("aterm.toml"));
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA").filter(|a| !a.is_empty()) {
        return Some(PathBuf::from(appdata).join("aterm").join("aterm.toml"));
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".config/aterm/aterm.toml"))
}

/// The `[update]` key the updater reads. Every other key of the table (owner, repo,
/// auto_apply, require_team_id) is the GUI's — `auto_apply` is also read on its own by
/// [`parse_update_auto_apply`], never beside this one, so a typo in either key cannot
/// flip the other.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct UpdateTable {
    enabled: Option<bool>,
}

/// The `[update]` table alone; every other table of `aterm.toml` is skipped unchecked.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct UpdateOnly {
    update: Option<UpdateTable>,
}

/// `[update] enabled` out of full `aterm.toml` text: `false` only when the table
/// parses and says so; absent, or a table that does not parse, is ON (module docs).
#[must_use]
pub fn parse_update_enabled(text: &str) -> bool {
    match aterm_toml::from_str::<UpdateOnly>(text) {
        Ok(root) => root.update.and_then(|u| u.enabled).unwrap_or(true),
        Err(e) => {
            crate::warn(&format!(
                "ignoring a malformed [update] table in aterm.toml — automatic updates \
                 stay on: {e}"
            ));
            true
        }
    }
}

/// [`parse_update_enabled`] over the file at `path`; a file that is absent, not a
/// regular file, over the budget or not UTF-8 is ON.
#[must_use]
pub fn update_enabled_at(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return true;
    };
    if !meta.is_file() || meta.len() > MAX_CONFIG_BYTES {
        return true;
    }
    std::fs::read_to_string(path).map_or(true, |text| parse_update_enabled(&text))
}

/// `[update] auto_apply` alone.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct AutoApplyTable {
    auto_apply: Option<bool>,
}

/// The `[update]` table, for `auto_apply` alone.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct AutoApplyOnly {
    update: Option<AutoApplyTable>,
}

/// `[update] auto_apply` out of full `aterm.toml` text — whether a downloaded build
/// installs by itself (the window's automatic lane, within a minute): `false` only when
/// the table parses and says so, the default ON otherwise, exactly as the window reads it
/// (`app_config::update_auto_apply_setting`).
#[must_use]
pub fn parse_update_auto_apply(text: &str) -> bool {
    aterm_toml::from_str::<AutoApplyOnly>(text)
        .ok()
        .and_then(|root| root.update)
        .and_then(|u| u.auto_apply)
        .unwrap_or(true)
}

/// [`parse_update_auto_apply`] over the user config, read now: the one-shot `aterm
/// update status|check` says what the window will do with a downloaded build. A file
/// that is absent, not a regular file, over the budget or not UTF-8 is ON.
#[must_use]
pub fn update_auto_apply() -> bool {
    let Some(path) = config_path() else {
        return true;
    };
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_CONFIG_BYTES => {
            std::fs::read_to_string(&path).map_or(true, |text| parse_update_auto_apply(&text))
        }
        _ => true,
    }
}

/// "Check for updates automatically" for THIS process: read once, so a change made in
/// Settings applies from the next launch (Settings says so), and every lane of one
/// process agrees for its whole life.
#[must_use]
pub fn update_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| config_path().is_none_or(|path| update_enabled_at(&path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_is_on_and_false_is_off() {
        assert!(parse_update_enabled(""));
        assert!(parse_update_enabled("font_px = 12.0\n"));
        assert!(parse_update_enabled("[update]\nauto_apply = false\n"));
        assert!(parse_update_enabled("[update]\nenabled = true\n"));
        assert!(!parse_update_enabled("[update]\nenabled = false\n"));
    }

    /// `[update] auto_apply` is ON unless the table says `false`, and each key is read
    /// alone: a malformed `auto_apply` never flips `enabled`, nor the other way round.
    #[test]
    fn auto_apply_is_on_unless_the_table_says_false() {
        assert!(parse_update_auto_apply(""));
        assert!(parse_update_auto_apply("[update]\nenabled = false\n"));
        assert!(!parse_update_auto_apply("[update]\nauto_apply = false\n"));
        assert!(parse_update_auto_apply("[update]\nauto_apply = \"no\"\n"));
        // Each key is read alone: a typo in one never flips the other.
        assert!(!parse_update_enabled(
            "[update]\nenabled = false\nauto_apply = \"no\"\n"
        ));
        assert!(!parse_update_auto_apply(
            "[update]\nenabled = \"no\"\nauto_apply = false\n"
        ));
    }

    /// Only `[update]` is type-checked: a typo in another table leaves an explicit
    /// `enabled = false` standing, and a malformed `[update]` is ON, never a panic.
    #[test]
    fn only_the_update_table_is_read() {
        assert!(!parse_update_enabled(
            "[packages]\nenabled = \"yes\"\n[update]\nenabled = false\n"
        ));
        assert!(parse_update_enabled("[update]\nenabled = \"no\"\n"));
        assert!(parse_update_enabled("[update\nbroken"));
    }

    #[test]
    fn a_missing_or_foreign_file_is_on() {
        let dir = std::env::temp_dir().join(format!("aterm-upd-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(update_enabled_at(&dir.join("absent.toml")));
        assert!(update_enabled_at(&dir), "a directory is not a config file");
        let off = dir.join("aterm.toml");
        std::fs::write(&off, "[update]\nenabled = false\n").unwrap();
        assert!(!update_enabled_at(&off));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
