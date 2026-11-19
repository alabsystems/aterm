// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-profile power management (`pmset -c` for AC and `pmset -b` for battery).

use super::{Ctx, Item};
use crate::model::Probe;
use crate::port::{Cmd, Ran, System};
use aterm_types::macos_setup::baseline::PowerProfile;
use aterm_types::macos_setup::power::{self, Unread};
pub use aterm_types::macos_setup::power::{Setting, Source, parse_cap, parse_custom, render};
use serde::Deserialize;
use std::sync::Mutex;

/// `pmset`: a root tool only this crate runs (census `machines-root-door`).
pub const PMSET: &str = "/usr/bin/pmset";

/// One power profile (`pmset -c` for AC, `pmset -b` for battery).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// System sleep, minutes; 0 = never.
    #[serde(default)]
    pub sleep: Option<u32>,
    /// Display sleep, minutes; 0 = never.
    #[serde(default)]
    pub display_sleep: Option<u32>,
    /// Disk sleep, minutes; 0 = never.
    #[serde(default)]
    pub disk_sleep: Option<u32>,
    /// `powernap`.
    #[serde(default)]
    pub power_nap: Option<bool>,
    /// `womp`.
    #[serde(default)]
    pub wake_on_lan: Option<bool>,
    /// `autorestart`, only where `pmset -g cap` lists it.
    #[serde(default)]
    pub restart_after_power_loss: Option<bool>,
    /// `standby`.
    #[serde(default)]
    pub standby: Option<bool>,
    /// `tcpkeepalive`.
    #[serde(default)]
    pub tcp_keepalive: Option<bool>,
}

/// The table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// `[power.ac]`.
    #[serde(default)]
    pub ac: Option<Profile>,
    /// `[power.battery]`: a Mac without a battery reports it NotApplicable.
    #[serde(default)]
    pub battery: Option<Profile>,
}

/// The items `table` asks for: one per profile and setting.
///
/// # Errors
/// Never today; the signature is the family contract.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let _ = ctx;
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    for (source, profile) in [(Source::Ac, &table.ac), (Source::Battery, &table.battery)] {
        let Some(p) = profile else { continue };
        for (setting, wanted) in power::wanted(&PowerProfile::from(p)) {
            items.push(Box::new(Power {
                source,
                setting,
                wanted,
                previous: Mutex::new(None),
            }));
        }
    }
    Ok(items)
}

impl From<&Profile> for PowerProfile {
    fn from(p: &Profile) -> Self {
        Self {
            sleep: p.sleep,
            display_sleep: p.display_sleep,
            disk_sleep: p.disk_sleep,
            power_nap: p.power_nap,
            wake_on_lan: p.wake_on_lan,
            restart_after_power_loss: p.restart_after_power_loss,
            standby: p.standby,
            tcp_keepalive: p.tcp_keepalive,
        }
    }
}

/// [`power::Reads`] answered from the port's [`System`]: the items' reads, and `aterm machines
/// macos-reading`'s for the window.
pub(crate) struct Port<'a>(pub(crate) &'a dyn System);

impl power::Reads for Port<'_> {
    fn pmset(&self, args: &[&str]) -> Result<String, String> {
        let cmd = Cmd::new(PMSET).args(args.iter().map(|arg| (*arg).to_owned()));
        match self.0.run(&cmd) {
            Ok(r) if r.success() => Ok(r.out()),
            Ok(r) => Err(format!("{}: {}", cmd.display(), r.describe())),
            Err(e) => Err(format!("{}: {e}", cmd.display())),
        }
    }
}

/// What a shared reading's [`Unread`] is as a probe.
pub(crate) fn unread_probe(unread: Unread) -> Probe {
    match unread {
        Unread::NotApplicable(why) => Probe::NotApplicable { why },
        Unread::Unusable(why) => Probe::Unusable { why },
    }
}

struct Power {
    source: Source,
    setting: &'static Setting,
    wanted: u64,
    /// The value the last probe that planned a change read: the revert's.
    previous: Mutex<Option<u64>>,
}

impl Power {
    fn write(&self, value: u64) -> Cmd {
        Cmd::new(PMSET).args([
            self.source.flag().to_owned(),
            self.setting.key.to_owned(),
            value.to_string(),
        ])
    }

    fn read(&self, sys: &dyn System) -> Result<u64, Probe> {
        power::read(&Port(sys), self.source, self.setting).map_err(unread_probe)
    }
}

impl Item for Power {
    fn id(&self) -> String {
        format!("power.{}.{}", self.source.id(), self.setting.id)
    }

    fn family(&self) -> &'static str {
        "power"
    }

    fn title(&self) -> String {
        format!(
            "{} on {}: {}",
            self.setting.title,
            self.source.section(),
            render(self.setting.kind, self.wanted)
        )
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let now = match self.read(sys) {
            Ok(v) => v,
            Err(probe) => return probe,
        };
        if now == self.wanted {
            return Probe::Ok {
                now: render(self.setting.kind, now),
            };
        }
        if let Ok(mut slot) = self.previous.lock() {
            *slot = Some(now);
        }
        Probe::Change {
            from: render(self.setting.kind, now),
            to: render(self.setting.kind, self.wanted),
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let cmd = self.write(self.wanted);
        let ran: Ran = sys
            .run(&cmd)
            .map_err(|e| format!("{}: {e}", cmd.display()))?;
        if ran.success() {
            Ok(())
        } else {
            Err(format!("{}: {}", cmd.display(), ran.describe()))
        }
    }

    fn revert(&self) -> Option<String> {
        let previous = (*self.previous.lock().ok()?)?;
        Some(self.write(previous).display())
    }
}

#[cfg(test)]
#[path = "power_tests.rs"]
mod tests;
