// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS Software Update [`Pref`] items and [`Schedule`] check.

use super::defaults::{Exports, Pref, Target, Want};
use super::{Ctx, Item};
use crate::model::{Effect, PValue, Probe, Read};
use crate::port::{Cmd, System};
use serde::Deserialize;
use std::cell::RefCell;

use aterm_types::macos_setup::baseline::Updates;
use aterm_types::macos_setup::updates;
pub use aterm_types::macos_setup::updates::DOMAIN;

/// `softwareupdate`, whose `--schedule` reads and sets checking for updates: a root tool only
/// this crate runs (census `machines-root-door`).
pub use super::developer::SOFTWAREUPDATE;

/// [`updates::Reads`] answered from the port's [`System`] as this module's items read it:
/// `softwareupdate --schedule` as [`Schedule`] reads it, and each key through one
/// [`Exports`] of [`DOMAIN`] ([`Target::system`], `defaults export` with its path checks and
/// its empty-domain confirmation), judged as [`Pref`] judges it ([`held`]). `aterm machines
/// macos-reading` reads through it, as the person: the domain's file is root's own and
/// world-readable.
pub(crate) struct Port<'a> {
    sys: &'a dyn System,
    exports: RefCell<Exports>,
}

impl<'a> Port<'a> {
    /// A reading of `sys`, its domain read once at the first key asked.
    pub(crate) fn new(sys: &'a dyn System) -> Self {
        Self {
            sys,
            exports: RefCell::new(Exports::default()),
        }
    }
}

impl updates::Reads for Port<'_> {
    fn schedule(&self) -> Result<String, String> {
        let cmd = Cmd::new(SOFTWAREUPDATE).arg("--schedule");
        match self.sys.run(&cmd) {
            Ok(ran) if ran.success() => Ok(ran.out()),
            Ok(ran) => Err(format!("softwareupdate --schedule: {}", ran.describe())),
            Err(e) => Err(format!("softwareupdate --schedule: {e}")),
        }
    }

    fn pref(&self, key: &str) -> Result<updates::Held, String> {
        held(
            &self
                .exports
                .borrow_mut()
                .read(self.sys, &Target::system(DOMAIN, key)),
        )
    }
}

/// What `read` holds, as [`Pref`]'s `Want::Value(PValue::Bool(_))` judges it: a value that
/// satisfies a switch ([`PValue::same`]: a boolean, or the `1`/`0` integer) is that switch, no
/// value is unset, and any other value satisfies neither.
///
/// # Errors
/// The read's reason where it was unusable.
pub(crate) fn held(read: &Read<PValue>) -> Result<updates::Held, String> {
    match read {
        Read::Unusable(why) => Err(why.clone()),
        Read::Absent => Ok(updates::Held::Unset),
        Read::Value(value) => Ok([true, false]
            .into_iter()
            .find(|on| value.same(&PValue::Bool(*on)))
            .map_or_else(
                || updates::Held::Other(value.to_string()),
                updates::Held::Switch,
            )),
    }
}

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
    let wanted = Updates::from(table);
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    if let Some(on) = table.automatic_check {
        items.push(Box::new(Schedule { on }));
    }
    for key in updates::KEYS {
        let Some(on) = key.field.of(&wanted) else {
            continue;
        };
        let state = if on { "on" } else { "off" };
        let title = format!(
            "Software Update: {} {state} ({DOMAIN} {})",
            key.what, key.key
        );
        items.push(Box::new(
            Pref::new(
                "updates",
                key.id.to_owned(),
                title,
                Target::system(DOMAIN, key.key),
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

impl From<&Table> for Updates {
    fn from(table: &Table) -> Self {
        Self {
            automatic_check: table.automatic_check,
            automatic_download: table.automatic_download,
            automatic_macos_install: table.automatic_macos_install,
            critical: table.critical,
        }
    }
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
        let Some(now) = updates::schedule(&text) else {
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
