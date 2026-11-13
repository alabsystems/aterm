// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-profile power management (`pmset -c` for AC and `pmset -b` for battery).

use super::{Ctx, Item};
use crate::model::Probe;
use crate::port::{Cmd, Ran, System};
use serde::Deserialize;
use std::sync::Mutex;

/// `pmset`.
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
        let minutes = |m: Option<u32>| m.map(u64::from);
        let switch = |b: Option<bool>| b.map(u64::from);
        let wanted = [
            (&SLEEP, minutes(p.sleep)),
            (&DISPLAY_SLEEP, minutes(p.display_sleep)),
            (&DISK_SLEEP, minutes(p.disk_sleep)),
            (&POWER_NAP, switch(p.power_nap)),
            (&WAKE_ON_LAN, switch(p.wake_on_lan)),
            (
                &RESTART_AFTER_POWER_LOSS,
                switch(p.restart_after_power_loss),
            ),
            (&STANDBY, switch(p.standby)),
            (&TCP_KEEPALIVE, switch(p.tcp_keepalive)),
        ];
        for (setting, value) in wanted {
            if let Some(value) = value {
                items.push(Box::new(Power {
                    source,
                    setting,
                    wanted: value,
                    previous: Mutex::new(None),
                }));
            }
        }
    }
    Ok(items)
}

/// A power source, as `pmset` names its profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// On a charger or a desktop's mains.
    Ac,
    /// On battery.
    Battery,
}

impl Source {
    /// `pmset`'s profile flag.
    #[must_use]
    pub const fn flag(self) -> &'static str {
        match self {
            Self::Ac => "-c",
            Self::Battery => "-b",
        }
    }

    /// The section header `pmset -g custom` prints, without its colon.
    #[must_use]
    pub const fn section(self) -> &'static str {
        match self {
            Self::Ac => "AC Power",
            Self::Battery => "Battery Power",
        }
    }

    const fn id(self) -> &'static str {
        match self {
            Self::Ac => "ac",
            Self::Battery => "battery",
        }
    }
}

/// How a setting's value reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Minutes, 0 = never.
    Minutes,
    /// 0 or 1.
    Switch,
}

/// One baseline field and the `pmset` key behind it.
#[derive(Debug)]
struct Setting {
    /// The id's last part (the field, kebab-cased).
    id: &'static str,
    /// The `pmset` key.
    key: &'static str,
    kind: Kind,
    title: &'static str,
}

const SLEEP: Setting = Setting {
    id: "sleep",
    key: "sleep",
    kind: Kind::Minutes,
    title: "system sleep",
};
const DISPLAY_SLEEP: Setting = Setting {
    id: "display-sleep",
    key: "displaysleep",
    kind: Kind::Minutes,
    title: "display sleep",
};
const DISK_SLEEP: Setting = Setting {
    id: "disk-sleep",
    key: "disksleep",
    kind: Kind::Minutes,
    title: "disk sleep",
};
const POWER_NAP: Setting = Setting {
    id: "power-nap",
    key: "powernap",
    kind: Kind::Switch,
    title: "Power Nap",
};
const WAKE_ON_LAN: Setting = Setting {
    id: "wake-on-lan",
    key: "womp",
    kind: Kind::Switch,
    title: "wake for network access",
};
const RESTART_AFTER_POWER_LOSS: Setting = Setting {
    id: "restart-after-power-loss",
    key: "autorestart",
    kind: Kind::Switch,
    title: "restart after a power failure",
};
const STANDBY: Setting = Setting {
    id: "standby",
    key: "standby",
    kind: Kind::Switch,
    title: "standby",
};
const TCP_KEEPALIVE: Setting = Setting {
    id: "tcp-keepalive",
    key: "tcpkeepalive",
    kind: Kind::Switch,
    title: "TCP keepalive while asleep",
};

/// Parse `pmset -g custom` output into `(section_name, Vec<(key, value)>)` pairs.
#[must_use]
pub fn parse_custom(text: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut out: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for l in text.lines() {
        if l.trim().is_empty() {
            continue;
        }
        if !l.starts_with([' ', '\t']) {
            if let Some(name) = l.trim_end().strip_suffix(':') {
                out.push((name.to_owned(), Vec::new()));
            }
            continue;
        }
        let row = l.trim();
        let Some((key, value)) = row.rsplit_once(char::is_whitespace) else {
            continue;
        };
        if let Some((_, rows)) = out.last_mut() {
            rows.push((key.trim_end().to_owned(), value.to_owned()));
        }
    }
    out
}

/// The capabilities `pmset -g cap` lists (one indented key per line).
#[must_use]
pub fn parse_cap(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.starts_with([' ', '\t']))
        .map(|l| l.trim().to_owned())
        .filter(|k| !k.is_empty())
        .collect()
}

fn render(kind: Kind, value: u64) -> String {
    match (kind, value) {
        (Kind::Minutes, 0) => "never".to_owned(),
        (Kind::Minutes, 1) => "1 minute".to_owned(),
        (Kind::Minutes, m) => format!("{m} minutes"),
        (Kind::Switch, 0) => "off".to_owned(),
        (Kind::Switch, 1) => "on".to_owned(),
        (Kind::Switch, n) => n.to_string(),
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
        let cmd = Cmd::new(PMSET).args(["-g", "custom"]);
        let text = tool_out(sys, &cmd)?;
        let sections = parse_custom(&text);
        let Some((_, rows)) = sections
            .iter()
            .find(|(name, _)| name == self.source.section())
        else {
            return Err(Probe::NotApplicable {
                why: format!(
                    "this Mac has no {} profile (`pmset -g custom` prints no {} section)",
                    self.source.id(),
                    self.source.section()
                ),
            });
        };
        if self.setting.key == RESTART_AFTER_POWER_LOSS.key {
            let cap = tool_out(sys, &Cmd::new(PMSET).args(["-g", "cap"]))?;
            if !parse_cap(&cap).iter().any(|k| k == "autorestart") {
                return Err(Probe::NotApplicable {
                    why: "`pmset -g cap` does not list autorestart: this Mac has no restart \
                          after a power failure (Apple silicon laptops and VMs do not)"
                        .to_owned(),
                });
            }
        }
        let Some((_, value)) = rows.iter().find(|(k, _)| k == self.setting.key) else {
            return Err(Probe::NotApplicable {
                why: format!(
                    "this Mac's {} profile has no {} (`pmset -g custom` does not print it)",
                    self.source.section(),
                    self.setting.key
                ),
            });
        };
        crate::parse_uint(value).ok_or_else(|| Probe::Unusable {
            why: format!(
                "`pmset -g custom` gave {} {value:?}, not a number",
                self.setting.key
            ),
        })
    }
}

fn tool_out(sys: &dyn System, cmd: &Cmd) -> Result<String, Probe> {
    match sys.run(cmd) {
        Ok(r) if r.success() => Ok(r.out()),
        Ok(r) => Err(Probe::Unusable {
            why: format!("{}: {}", cmd.display(), r.describe()),
        }),
        Err(e) => Err(Probe::Unusable {
            why: format!("{}: {e}", cmd.display()),
        }),
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
