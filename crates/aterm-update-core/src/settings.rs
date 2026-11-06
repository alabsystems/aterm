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
    update_enabled_in(text).unwrap_or_else(|e| {
        crate::warn(&update_parse_notice(text, &e));
        true
    })
}

/// [`parse_update_enabled`] without its warning: what `text` says of `[update] enabled`, or why
/// it says nothing.
fn update_enabled_in(text: &str) -> Result<bool, aterm_toml::Error> {
    aterm_toml::from_str::<UpdateOnly>(text).map(|root| {
        root.update
            .and_then(|update| update.enabled)
            .unwrap_or(true)
    })
}

fn update_parse_notice(text: &str, e: &aterm_toml::Error) -> String {
    let what = if aterm_toml::from_str::<aterm_toml::Table>(text).is_err() {
        "aterm.toml does not parse"
    } else {
        "[update] in aterm.toml has a value of the wrong type"
    };
    format!("{what} — automatic updates stay on: {e}")
}

fn read_config_file(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = opts.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_CONFIG_BYTES {
        return None;
    }
    let mut text =
        String::with_capacity(usize::try_from(meta.len()).unwrap_or(0).saturating_add(1));
    file.take(MAX_CONFIG_BYTES.saturating_add(1))
        .read_to_string(&mut text)
        .ok()?;
    (text.len() as u64 <= MAX_CONFIG_BYTES).then_some(text)
}

/// Read [`parse_update_enabled`] from `path` (`true` if absent, non-regular, oversized, or non-UTF-8).
#[must_use]
pub fn update_enabled_at(path: &Path) -> bool {
    read_config_file(path).is_none_or(|text| parse_update_enabled(&text))
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
    config_path()
        .and_then(|path| read_config_file(&path))
        .is_none_or(|text| parse_update_auto_apply(&text))
}

/// Cached `[update] enabled` setting for this process, read at its first call (which warns of a
/// config that does not parse) and kept until the next launch, whatever the file says later.
#[must_use]
pub fn update_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| config_path().is_none_or(|path| update_enabled_at(&path)))
}

/// `[update] enabled` as the user config holds it now, read again at each call
/// ([`update_enabled_saved_at`] of [`config_path`]): a long-lived process compares it with
/// [`update_enabled`] to learn that the setting changed since it started.
#[must_use]
pub fn update_enabled_saved() -> bool {
    update_enabled_saved_at(config_path().as_deref())
}

/// [`update_enabled_at`] of `path` (`true` with none), but quiet: a config that does not parse
/// reads as on without a warning, since a caller that reads it again and again would repeat
/// the one warning [`update_enabled`]'s first read gave.
fn update_enabled_saved_at(path: Option<&Path>) -> bool {
    path.and_then(read_config_file)
        .is_none_or(|text| update_enabled_in(&text).unwrap_or(true))
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
        let dir = crate::test_scratch_dir("upd-settings");
        assert!(update_enabled_at(&dir.path().join("absent.toml")));
        assert!(
            update_enabled_at(dir.path()),
            "a directory is not a config file"
        );
        let off = dir.path().join("aterm.toml");
        std::fs::write(&off, "[update]\nenabled = false\n").unwrap();
        assert!(!update_enabled_at(&off));
    }

    /// THE SAVED SETTING IS READ AGAIN AT EACH CALL, AS THE PARSER READS IT: it follows the file
    /// as the file changes, and reads every text, one that does not parse included, as
    /// `parse_update_enabled` does (which alone warns).
    #[test]
    fn the_saved_setting_follows_the_file_and_reads_as_the_parser_does() {
        for text in [
            "",
            "[update]\nenabled = false\n",
            "[update]\nenabled = true\n",
            "[update]\nenabled = \"no\"\n",
            "[update\nbroken",
        ] {
            assert_eq!(
                update_enabled_in(text).unwrap_or(true),
                parse_update_enabled(text),
                "{text:?}"
            );
        }
        let dir = crate::test_scratch_dir("upd-settings-saved");
        let path = dir.path().join("aterm.toml");
        assert!(update_enabled_saved_at(None), "no config path reads as on");
        assert!(update_enabled_saved_at(Some(&path)), "no file reads as on");
        std::fs::write(&path, "[update]\nenabled = false\n").unwrap();
        assert!(!update_enabled_saved_at(Some(&path)), "turned off");
        std::fs::write(&path, "[update]\nenabled = true\n").unwrap();
        assert!(update_enabled_saved_at(Some(&path)), "turned on again");
        std::fs::write(&path, "[update\nbroken").unwrap();
        assert!(
            update_enabled_saved_at(Some(&path)),
            "a broken file reads as on"
        );
    }
}
