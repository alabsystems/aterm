// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The starter baseline's `[power.ac]` and `[updates]` tables, written once: `aterm machines
//! init` writes them into `baselines/default.toml` (`aterm_machines::controller::templates`
//! splices [`STARTER_POWER_AC_TOML`] and [`STARTER_UPDATES_TOML`] in with `concat!`, so the file's
//! bytes and their SHA history stay as they were), and the System Resources page's macOS
//! configuration section judges this Mac against them ([`starter_power_ac`],
//! [`starter_updates`]). The text is the one source; the typed values are read from it.

/// One power profile's wanted settings: `[power.ac]` or `[power.battery]`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PowerProfile {
    /// System sleep, minutes; 0 = never.
    pub sleep: Option<u32>,
    /// Display sleep, minutes; 0 = never.
    pub display_sleep: Option<u32>,
    /// Disk sleep, minutes; 0 = never.
    pub disk_sleep: Option<u32>,
    /// `powernap`.
    pub power_nap: Option<bool>,
    /// `womp`.
    pub wake_on_lan: Option<bool>,
    /// `autorestart`, only where `pmset -g cap` lists it.
    pub restart_after_power_loss: Option<bool>,
    /// `standby`.
    pub standby: Option<bool>,
    /// `tcpkeepalive`.
    pub tcp_keepalive: Option<bool>,
}

/// The `[updates]` table's wanted settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Updates {
    /// Check for updates (`softwareupdate --schedule`).
    pub automatic_check: Option<bool>,
    /// `AutomaticDownload`.
    pub automatic_download: Option<bool>,
    /// `AutomaticallyInstallMacOSUpdates`.
    pub automatic_macos_install: Option<bool>,
    /// `CriticalUpdateInstall` and `ConfigDataInstall`.
    pub critical: Option<bool>,
}

/// The starter baseline's `[power.ac]` table, as `baselines/default.toml` holds it (a literal
/// `concat!` takes).
#[doc(hidden)]
#[macro_export]
macro_rules! starter_power_ac_toml {
    () => {
        "[power.ac]
sleep = 0
disk_sleep = 0
display_sleep = 15
power_nap = false
wake_on_lan = true
restart_after_power_loss = true
"
    };
}

/// The starter baseline's `[updates]` table, as `baselines/default.toml` holds it.
#[doc(hidden)]
#[macro_export]
macro_rules! starter_updates_toml {
    () => {
        "[updates]
automatic_check = true
automatic_download = true
automatic_macos_install = false
critical = true
"
    };
}

/// The starter baseline's `[power.ac]` table's text.
pub const STARTER_POWER_AC_TOML: &str = starter_power_ac_toml!();

/// The starter baseline's `[updates]` table's text.
pub const STARTER_UPDATES_TOML: &str = starter_updates_toml!();

/// Each `key = value` line of a table's text, its header and blank lines left out.
fn fields(text: &str) -> impl Iterator<Item = (&str, &str)> {
    text.lines()
        .filter(|line| !line.starts_with('[') && !line.trim().is_empty())
        .filter_map(|line| line.split_once(" = "))
}

fn switch(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// The starter baseline's `[power.ac]`, read from [`STARTER_POWER_AC_TOML`].
#[must_use]
pub fn starter_power_ac() -> PowerProfile {
    let mut profile = PowerProfile::default();
    for (key, value) in fields(STARTER_POWER_AC_TOML) {
        match key {
            "sleep" => profile.sleep = value.parse().ok(),
            "display_sleep" => profile.display_sleep = value.parse().ok(),
            "disk_sleep" => profile.disk_sleep = value.parse().ok(),
            "power_nap" => profile.power_nap = switch(value),
            "wake_on_lan" => profile.wake_on_lan = switch(value),
            "restart_after_power_loss" => profile.restart_after_power_loss = switch(value),
            "standby" => profile.standby = switch(value),
            "tcp_keepalive" => profile.tcp_keepalive = switch(value),
            _ => {}
        }
    }
    profile
}

/// The starter baseline's `[updates]`, read from [`STARTER_UPDATES_TOML`].
#[must_use]
pub fn starter_updates() -> Updates {
    let mut updates = Updates::default();
    for (key, value) in fields(STARTER_UPDATES_TOML) {
        match key {
            "automatic_check" => updates.automatic_check = switch(value),
            "automatic_download" => updates.automatic_download = switch(value),
            "automatic_macos_install" => updates.automatic_macos_install = switch(value),
            "critical" => updates.critical = switch(value),
            _ => {}
        }
    }
    updates
}
