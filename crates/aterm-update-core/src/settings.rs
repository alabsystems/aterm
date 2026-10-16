// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `[update] enabled` ("Check for updates automatically"), read by the updater itself, and
//! `[update] auto_apply`, whether a downloaded build installs by itself.
//!
//! The switch is a key in `aterm.toml` (Settings ▸ Software Update writes it), not an
//! environment variable, which would never reach a window started from the Dock or a login
//! item. Every process that runs the updater reads it here. Only the `[update]` table is
//! deserialized, so a typo in another table cannot flip it, and a missing, unreadable or
//! malformed file reads as on: an updater a typo switched off silently stops receiving
//! security fixes.

use std::path::{Path, PathBuf};

/// The same admission budget the native config service and atpkg's reader use.
const MAX_CONFIG_BYTES: u64 = 512 * 1024;

/// The user config file, resolved as the GUI's `app_config::config_path` does:
/// `$XDG_CONFIG_HOME/aterm/aterm.toml`, else (Windows) `%APPDATA%\aterm\aterm.toml`, else
/// `$HOME/.config/aterm/aterm.toml`.
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

/// `[update] enabled` out of full `aterm.toml` text: `false` only when the table parses and
/// says so.
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

/// The log line for text [`parse_update_enabled`] could not read, telling a file that does not
/// parse apart from a mistyped `[update]` value.
fn update_parse_notice(text: &str, e: &aterm_toml::Error) -> String {
    let what = if aterm_toml::from_str::<aterm_toml::Table>(text).is_err() {
        "aterm.toml does not parse"
    } else {
        "[update] in aterm.toml has a value of the wrong type"
    };
    format!("{what} — automatic updates stay on: {e}")
}

/// [`parse_update_enabled`] over the file at `path`; an absent, non-regular, oversized or
/// non-UTF-8 file is on.
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

/// `[update] auto_apply` out of full `aterm.toml` text: whether a downloaded build installs
/// by itself. `false` only when the table parses and says so, as the window reads it
/// (`app_config::update_auto_apply_setting`).
#[must_use]
pub fn parse_update_auto_apply(text: &str) -> bool {
    aterm_toml::from_str::<AutoApplyOnly>(text)
        .ok()
        .and_then(|root| root.update)
        .and_then(|u| u.auto_apply)
        .unwrap_or(true)
}

/// [`parse_update_auto_apply`] over the user config, read now (for `aterm update
/// status|check`). An absent, non-regular, oversized or non-UTF-8 file is on.
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

/// "Check for updates automatically" for this process, read once: a change in Settings
/// applies from the next launch, and every lane of one process agrees for its whole life.
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

    /// `auto_apply` is on unless the table says `false`, and a malformed key never flips the
    /// other.
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

    /// Only `[update]` is type-checked: a typo in another table leaves an explicit
    /// `enabled = false` standing, and a malformed `[update]` is on, never a panic.
    #[test]
    fn only_the_update_table_is_read() {
        assert!(!parse_update_enabled(
            "[packages]\nenabled = \"yes\"\n[update]\nenabled = false\n"
        ));
        assert!(parse_update_enabled("[update]\nenabled = \"no\"\n"));
        assert!(parse_update_enabled("[update\nbroken"));
    }

    #[test]
    fn the_parse_notice_names_the_whole_file_or_the_update_table() {
        let notice = |text: &str| {
            let e = aterm_toml::from_str::<UpdateOnly>(text).expect_err("fixture must fail");
            update_parse_notice(text, &e)
        };
        let syntax = notice("font_px =\n[update]\nenabled = false\n");
        assert!(
            syntax.starts_with("aterm.toml does not parse — automatic updates stay on: "),
            "{syntax}"
        );
        let typed = notice("[update]\nenabled = \"no\"\n");
        assert!(
            typed.starts_with(
                "[update] in aterm.toml has a value of the wrong type — automatic updates stay on: "
            ),
            "{typed}"
        );
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
