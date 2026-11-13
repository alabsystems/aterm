// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS application firewall items via `socketfilterfw`.

use super::{Ctx, Item};
use crate::model::{Probe, Read};
use crate::port::{Cmd, System};
use serde::Deserialize;

/// `socketfilterfw`.
pub const SOCKETFILTERFW: &str = "/usr/libexec/ApplicationFirewall/socketfilterfw";

/// The `[firewall]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// `--setglobalstate`.
    pub enabled: Option<bool>,
    /// `--setstealthmode`.
    pub stealth: Option<bool>,
    /// `--setallowsigned` (built-in software).
    pub allow_signed: Option<bool>,
    /// `--setallowsignedapp` (signed downloads).
    pub allow_signed_downloaded: Option<bool>,
    /// Refused whatever its value.
    pub block_all: Option<bool>,
}

/// The items `table` asks for.
/// # Errors
/// `block_all` named, with the reason.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let _ = ctx;
    if table.block_all.is_some() {
        return Err(
            "[firewall] block_all is refused: blocking all incoming connections \
                    refuses the SSH connection the controller reaches this Mac on, so no \
                    later run could undo it"
                .to_owned(),
        );
    }
    let rows = [
        (Setting::Enabled, table.enabled),
        (Setting::Stealth, table.stealth),
        (Setting::AllowSigned, table.allow_signed),
        (
            Setting::AllowSignedDownloaded,
            table.allow_signed_downloaded,
        ),
    ];
    Ok(rows
        .into_iter()
        .filter_map(|(setting, wanted)| {
            wanted.map(|on| Box::new(Switch { setting, on }) as Box<dyn Item>)
        })
        .collect())
}

/// One `socketfilterfw` switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// The firewall itself.
    Enabled,
    /// Stealth mode (no answer to probes and pings).
    Stealth,
    /// Allow built-in signed software.
    AllowSigned,
    /// Allow downloaded signed software.
    AllowSignedDownloaded,
}

impl Setting {
    const fn get(self) -> &'static str {
        match self {
            Self::Enabled => "--getglobalstate",
            Self::Stealth => "--getstealthmode",
            Self::AllowSigned | Self::AllowSignedDownloaded => "--getallowsigned",
        }
    }

    const fn set(self) -> &'static str {
        match self {
            Self::Enabled => "--setglobalstate",
            Self::Stealth => "--setstealthmode",
            Self::AllowSigned => "--setallowsigned",
            Self::AllowSignedDownloaded => "--setallowsignedapp",
        }
    }

    const fn id(self) -> &'static str {
        match self {
            Self::Enabled => "firewall.enabled",
            Self::Stealth => "firewall.stealth",
            Self::AllowSigned => "firewall.allow-signed",
            Self::AllowSignedDownloaded => "firewall.allow-signed-downloaded",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Enabled => "Firewall",
            Self::Stealth => "Firewall stealth mode",
            Self::AllowSigned => "Firewall: allow built-in signed software",
            Self::AllowSignedDownloaded => "Firewall: allow downloaded signed software",
        }
    }

    /// The setting in `socketfilterfw`'s answer to [`Setting::get`].
    #[must_use]
    pub fn parse(self, text: &str) -> Read<bool> {
        let found = match self {
            Self::Enabled => text.lines().find_map(|l| {
                let n = l.split_once("(State = ")?.1.split_once(')')?.0;
                crate::parse_uint::<u32>(n.trim()).map(|n| n != 0)
            }),
            Self::Stealth => text.lines().find_map(|l| {
                let state = l.trim().strip_prefix("Firewall stealth mode is ")?;
                word(state, "on", "off")
            }),
            Self::AllowSigned => signed_line(text, "Automatically allow built-in signed software"),
            Self::AllowSignedDownloaded => {
                signed_line(text, "Automatically allow downloaded signed software")
            }
        };
        found.map_or_else(
            || {
                Read::Unusable(format!(
                    "socketfilterfw {} answered {:?}",
                    self.get(),
                    text.trim()
                ))
            },
            Read::Value,
        )
    }
}

fn signed_line(text: &str, prefix: &str) -> Option<bool> {
    text.lines().find_map(|l| {
        let state = l.trim().strip_prefix(prefix)?.trim().trim_end_matches('.');
        word(state, "ENABLED", "DISABLED")
    })
}

fn word(text: &str, on: &str, off: &str) -> Option<bool> {
    match text.trim() {
        t if t == on => Some(true),
        t if t == off => Some(false),
        _ => None,
    }
}

const fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

struct Switch {
    setting: Setting,
    on: bool,
}

impl Switch {
    fn set(&self, on: bool) -> Cmd {
        Cmd::new(SOCKETFILTERFW).args([self.setting.set(), on_off(on)])
    }
}

impl Item for Switch {
    fn id(&self) -> String {
        self.setting.id().to_owned()
    }

    fn family(&self) -> &'static str {
        "firewall"
    }

    fn title(&self) -> String {
        format!("{} {}", self.setting.title(), on_off(self.on))
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let cmd = Cmd::new(SOCKETFILTERFW).arg(self.setting.get());
        let read = match sys.run(&cmd) {
            Ok(r) if r.success() => self.setting.parse(&r.out()),
            Ok(r) => Read::Unusable(format!("{}: {}", cmd.display(), r.describe())),
            Err(e) => Read::Unusable(format!("{}: {e}", cmd.display())),
        };
        match read {
            Read::Value(now) if now == self.on => Probe::Ok {
                now: on_off(now).to_owned(),
            },
            Read::Value(now) => Probe::Change {
                from: on_off(now).to_owned(),
                to: on_off(self.on).to_owned(),
            },
            Read::Absent => Probe::Unusable {
                why: format!("{} gave no answer", cmd.display()),
            },
            Read::Unusable(why) => Probe::Unusable { why },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let cmd = self.set(self.on);
        let ran = sys
            .run(&cmd)
            .map_err(|e| format!("{}: {e}", cmd.display()))?;
        if ran.success() {
            Ok(())
        } else {
            let said = format!("{} {}", ran.out().trim(), ran.err());
            let how = ran
                .status
                .map_or_else(|| "killed by a signal".to_owned(), |s| format!("exit {s}"));
            Err(format!("{}: {how}: {}", cmd.display(), said.trim()))
        }
    }

    fn revert(&self) -> Option<String> {
        Some(self.set(!self.on).display())
    }
}

#[cfg(test)]
#[path = "firewall_tests.rs"]
mod tests;
