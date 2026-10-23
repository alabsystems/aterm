// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Readers for `[update] enabled` and `[update] auto_apply` in `aterm.toml`, defaulting
//! to `true` when absent, unreadable, or malformed.

use std::path::{Path, PathBuf};

const MAX_CONFIG_BYTES: u64 = 512 * 1024;

/// Canonical UI label for `[update] enabled`.
pub const CHECK_LABEL: &str = "Check for updates automatically";
/// Canonical UI label for `[update] auto_apply`.
pub const INSTALL_LABEL: &str = "Install updates automatically";

/// Status string explaining why a downloaded build is held when `[update] auto_apply` is off.
#[must_use]
pub fn install_switch_off() -> String {
    format!("\u{201c}{INSTALL_LABEL}\u{201d} is off")
}

/// Resolve the user `aterm.toml` path (`$XDG_CONFIG_HOME`, `%APPDATA%` on Windows, or `~/.config`).
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    // `aterm_types::dirs::config_home`: `$XDG_CONFIG_HOME`, or a test binary's own root.
    if let Some(x) = aterm_types::dirs::config_home() {
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

/// The `[update]` key the updater reads. The table's other keys are the GUI's; `auto_apply`
/// is read on its own by [`parse_update_auto_apply`], so a typo in one cannot flip the other.
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

/// Parse `[update] enabled` from `aterm.toml` text (`false` only when explicitly set to `false`).
#[must_use]
pub fn parse_update_enabled(text: &str) -> bool {
    match aterm_toml::from_str::<UpdateOnly>(text) {
        Ok(root) => root.update.and_then(|u| u.enabled).unwrap_or(true),
        Err(e) => {
            crate::warn(&update_parse_notice(text, &e));
            true
        }
    }
}

fn update_parse_notice(text: &str, e: &aterm_toml::Error) -> String {
    let what = if aterm_toml::from_str::<aterm_toml::Table>(text).is_err() {
        "aterm.toml does not parse"
    } else {
        "[update] in aterm.toml has a value of the wrong type"
    };
    format!("{what} — automatic updates stay on: {e}")
}

/// Read [`parse_update_enabled`] from `path` (`true` if absent, non-regular, oversized, or non-UTF-8).
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

/// Parse `[update] auto_apply` from `aterm.toml` text (`false` only when explicitly set to `false`).
#[must_use]
pub fn parse_update_auto_apply(text: &str) -> bool {
    aterm_toml::from_str::<AutoApplyOnly>(text)
        .ok()
        .and_then(|root| root.update)
        .and_then(|u| u.auto_apply)
        .unwrap_or(true)
}

/// Read [`parse_update_auto_apply`] from the user config (`true` if absent, oversized, or unreadable).
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

/// Cached `[update] enabled` setting for this process.
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

    #[test]
    fn auto_apply_is_on_unless_the_table_says_false() {
        assert!(parse_update_auto_apply(""));
        assert!(parse_update_auto_apply("[update]\nenabled = false\n"));
        assert!(!parse_update_auto_apply("[update]\nauto_apply = false\n"));
        assert!(parse_update_auto_apply("[update]\nauto_apply = \"no\"\n"));
        assert!(!parse_update_enabled(
            "[update]\nenabled = false\nauto_apply = \"no\"\n"
        ));
        assert!(!parse_update_auto_apply(
            "[update]\nenabled = \"no\"\nauto_apply = false\n"
        ));
    }

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
