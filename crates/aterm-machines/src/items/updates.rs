// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS Software Update [`Pref`] items and [`Schedule`] check.

use super::defaults::{Pref, Target, Want};
use super::{Ctx, Item};
use crate::model::{Effect, PValue, Probe};
use crate::port::{Cmd, System};
use serde::Deserialize;

/// The domain, under `/Library/Preferences`.
pub const DOMAIN: &str = "com.apple.SoftwareUpdate";

/// The `[updates]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// Check for updates, read and turned on with `softwareupdate --schedule` (macOS 27 keeps
    /// no preference key for it); turning it off is blocked.
    pub automatic_check: Option<bool>,
    /// `AutomaticDownload`.
    pub automatic_download: Option<bool>,
    /// `AutomaticallyInstallMacOSUpdates`.
    pub automatic_macos_install: Option<bool>,
    /// `CriticalUpdateInstall` and `ConfigDataInstall` (security responses, XProtect).
    pub critical: Option<bool>,
}

/// The items `table` asks for, one per key, in the order System Settings lists them.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let _ = ctx;
    let rows: [(Option<bool>, &str, &str, &str); 4] = [
        (
            table.automatic_download,
            "updates.automatic-download",
            "AutomaticDownload",
            "download new updates when available",
        ),
        (
            table.automatic_macos_install,
            "updates.automatic-macos-install",
            "AutomaticallyInstallMacOSUpdates",
            "install macOS updates",
        ),
        (
            table.critical,
            "updates.critical",
            "CriticalUpdateInstall",
            "install Security Responses and system files",
        ),
        (
            table.critical,
            "updates.config-data",
            "ConfigDataInstall",
            "install XProtect and other configuration data",
        ),
    ];
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    if let Some(on) = table.automatic_check {
        items.push(Box::new(Schedule { on }));
    }
    for (wanted, id, key, what) in rows {
        let Some(on) = wanted else {
            continue;
        };
        let state = if on { "on" } else { "off" };
        let title = format!("Software Update: {what} {state} ({DOMAIN} {key})");
        items.push(Box::new(
            Pref::new(
                "updates",
                id.to_owned(),
                title,
                Target::system(DOMAIN, key),
                Want::Value(PValue::Bool(on)),
            )
            .felt(Effect::Now)
            .provenance(
                "macOS 27.0 (26A428) VM: written as root, read back as a boolean, deleted; held by 27.0.1 (26A434) after System Settings (AutomaticCheckEnabled absent there)",
            ),
        ));
    }
    Ok(items)
}

/// Why checking for updates is not turned off on a Mac running `os_version`.
fn off_blocked(os_version: &str) -> String {
    let why = "checking for updates is not turned off: `softwareupdate --schedule off` \
               changed nothing on macOS 27.0 and the next check removed AutomaticCheckEnabled"
        .to_owned();
    match os_version {
        "27.0" => why,
        "" => format!("{why}; this Mac's macOS version was not read"),
        other => format!("{why}; this Mac runs macOS {other}, where that was not measured"),
    }
}

/// `softwareupdate`, whose `--schedule` reads and sets checking for updates.
const SOFTWAREUPDATE: &str = "/usr/sbin/softwareupdate";

/// Checking for updates, as `softwareupdate --schedule` reports it.
struct Schedule {
    on: bool,
}

impl Schedule {
    const fn word(on: bool) -> &'static str {
        if on { "on" } else { "off" }
    }
}

impl Item for Schedule {
    fn id(&self) -> String {
        "updates.automatic-check".into()
    }

    fn family(&self) -> &'static str {
        "updates"
    }

    fn title(&self) -> String {
        format!(
            "Software Update: check for updates {} (softwareupdate --schedule)",
            Self::word(self.on)
        )
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        let ran = match sys.run(&Cmd::new(SOFTWAREUPDATE).arg("--schedule")) {
            Ok(ran) if ran.success() => ran,
            Ok(ran) => {
                return Probe::Unusable {
                    why: format!("softwareupdate --schedule: {}", ran.describe()),
                };
            }
            Err(e) => {
                return Probe::Unusable {
                    why: format!("softwareupdate --schedule: {e}"),
                };
            }
        };
        let text = ran.out();
        let now = if text.contains("is turned on") {
            true
        } else if text.contains("is turned off") {
            false
        } else {
            return Probe::Unusable {
                why: format!("softwareupdate --schedule printed {:?}", text.trim()),
            };
        };
        match (now, self.on) {
            (now, on) if now == on => Probe::Ok {
                now: Self::word(now).into(),
            },
            (false, true) => Probe::Change {
                from: "off".into(),
                to: "on".into(),
            },
            (_, _) => Probe::Blocked {
                why: off_blocked(&ctx.facts.os_version),
            },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        match sys.run(&Cmd::new(SOFTWAREUPDATE).args(["--schedule", "on"])) {
            Ok(ran) if ran.success() => Ok(()),
            Ok(ran) => Err(format!("softwareupdate --schedule on: {}", ran.describe())),
            Err(e) => Err(format!("softwareupdate --schedule on: {e}")),
        }
    }
}

#[cfg(test)]
#[path = "updates_tests.rs"]
mod tests;
