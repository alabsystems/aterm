// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Time zone and network time configuration via `systemsetup` and `/etc/localtime`.

use super::{Ctx, Item};
use crate::model::Probe;
use crate::port::{Cmd, Ran, System};
use serde::Deserialize;
use std::path::Path;
use std::sync::Mutex;

/// `systemsetup`.
pub const SYSTEMSETUP: &str = "/usr/sbin/systemsetup";

/// Where the zone files live (a link to the current tz database on 27).
pub const ZONEINFO: &str = "/var/db/timezone/zoneinfo";

const NOT_ROOT: &str = "You need administrator access";

/// The table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// An IANA zone (`America/Los_Angeles`); read back from `/etc/localtime`.
    #[serde(default)]
    pub zone: Option<String>,
    /// `systemsetup -setusingnetworktime`.
    #[serde(default)]
    pub network_time: Option<bool>,
    /// `systemsetup -setnetworktimeserver`.
    #[serde(default)]
    pub server: Option<String>,
}

/// The items `table` asks for.
/// # Errors
/// One line naming a zone or server that is not a name.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let _ = ctx;
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    if let Some(zone) = &table.zone {
        let plain = !zone.is_empty()
            && !zone.starts_with(['/', '-'])
            && !zone
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            && zone
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'));
        if !plain {
            return Err(format!(
                "[time] zone {zone:?} is not a time zone name (like \"America/Los_Angeles\")"
            ));
        }
        items.push(Box::new(Setting::new(Kind::Zone(zone.clone()))));
    }
    if let Some(on) = table.network_time {
        items.push(Box::new(Setting::new(Kind::NetworkTime(on))));
    }
    if let Some(server) = &table.server {
        let plain = !server.is_empty()
            && !server.starts_with('-')
            && server
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'));
        if !plain {
            return Err(format!(
                "[time] server {server:?} is not a host name or address"
            ));
        }
        items.push(Box::new(Setting::new(Kind::Server(server.clone()))));
    }
    Ok(items)
}

/// Extract the IANA zone name following `zoneinfo/` in a `/etc/localtime` link target.
#[must_use]
pub fn zone_of_link(target: &str) -> Option<String> {
    let parts: Vec<&str> = target.trim().split('/').collect();
    let at = parts.iter().rposition(|p| p.starts_with("zoneinfo"))?;
    let zone = parts[at + 1..].join("/");
    (!zone.is_empty()).then_some(zone)
}

/// The value after `<label>: ` in a `systemsetup -get…` answer.
#[must_use]
pub fn systemsetup_value(text: &str, label: &str) -> Option<String> {
    let prefix = format!("{label}: ");
    text.lines()
        .find_map(|l| l.trim().strip_prefix(&prefix).map(|v| v.trim().to_owned()))
        .filter(|v| !v.is_empty())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Zone(String),
    NetworkTime(bool),
    Server(String),
}

struct Setting {
    kind: Kind,
    previous: Mutex<Option<String>>,
}

impl Setting {
    const fn new(kind: Kind) -> Self {
        Self {
            kind,
            previous: Mutex::new(None),
        }
    }

    fn set(&self, value: &str) -> Cmd {
        let verb = match self.kind {
            Kind::Zone(_) => "-settimezone",
            Kind::NetworkTime(_) => "-setusingnetworktime",
            Kind::Server(_) => "-setnetworktimeserver",
        };
        Cmd::new(SYSTEMSETUP).args([verb, value])
    }

    fn wanted(&self) -> String {
        match &self.kind {
            Kind::Zone(z) | Kind::Server(z) => z.clone(),
            Kind::NetworkTime(on) => on_off(*on).to_owned(),
        }
    }

    fn read(&self, sys: &dyn System) -> Result<String, String> {
        match &self.kind {
            Kind::Zone(_) => {
                let cmd = Cmd::new("/usr/bin/readlink").arg("/etc/localtime");
                let out = out_of(sys, &cmd)?;
                zone_of_link(&out)
                    .ok_or_else(|| format!("{}: {:?} names no zone", cmd.display(), out.trim()))
            }
            Kind::NetworkTime(_) => {
                let cmd = Cmd::new(SYSTEMSETUP).arg("-getusingnetworktime");
                let out = out_of(sys, &cmd)?;
                match systemsetup_value(&out, "Network Time").as_deref() {
                    Some("On") => Ok("on".to_owned()),
                    Some("Off") => Ok("off".to_owned()),
                    _ => Err(format!("{} answered {:?}", cmd.display(), out.trim())),
                }
            }
            Kind::Server(_) => {
                let cmd = Cmd::new(SYSTEMSETUP).arg("-getnetworktimeserver");
                let out = out_of(sys, &cmd)?;
                systemsetup_value(&out, "Network Time Server")
                    .ok_or_else(|| format!("{} answered {:?}", cmd.display(), out.trim()))
            }
        }
    }

    fn same(&self, now: &str) -> bool {
        match &self.kind {
            Kind::Server(s) => now.eq_ignore_ascii_case(s),
            _ => now == self.wanted(),
        }
    }
}

const fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn out_of(sys: &dyn System, cmd: &Cmd) -> Result<String, String> {
    let ran: Ran = sys
        .run(cmd)
        .map_err(|e| format!("{}: {e}", cmd.display()))?;
    let out = ran.out();
    if out.contains(NOT_ROOT) {
        return Err(format!(
            "{}: systemsetup needs root ({})",
            cmd.display(),
            out.trim()
        ));
    }
    if !ran.success() {
        let said = format!("{} {}", out.trim(), ran.err());
        let how = ran
            .status
            .map_or_else(|| "killed by a signal".to_owned(), |s| format!("exit {s}"));
        return Err(format!("{}: {how}: {}", cmd.display(), said.trim()));
    }
    Ok(out)
}

impl Item for Setting {
    fn id(&self) -> String {
        match self.kind {
            Kind::Zone(_) => "time.zone",
            Kind::NetworkTime(_) => "time.network-time",
            Kind::Server(_) => "time.server",
        }
        .to_owned()
    }

    fn family(&self) -> &'static str {
        "time"
    }

    fn title(&self) -> String {
        match &self.kind {
            Kind::Zone(z) => format!("Time zone {z}"),
            Kind::NetworkTime(on) => format!("Set time automatically {}", on_off(*on)),
            Kind::Server(s) => format!("Network time server {s}"),
        }
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let now = match self.read(sys) {
            Ok(now) => now,
            Err(why) => return Probe::Unusable { why },
        };
        if self.same(&now) {
            return Probe::Ok { now };
        }
        if let Kind::Zone(zone) = &self.kind {
            let file = Path::new(ZONEINFO).join(zone);
            match sys.stat(&file) {
                Ok(s) if !s.dir => {}
                Ok(_) | Err(_) => {
                    return Probe::Unusable {
                        why: format!(
                            "this Mac has no time zone {zone} ({} is not a zone file)",
                            file.display()
                        ),
                    };
                }
            }
        }
        if let Ok(mut slot) = self.previous.lock() {
            *slot = Some(now.clone());
        }
        Probe::Change {
            from: now,
            to: self.wanted(),
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        out_of(sys, &self.set(&self.wanted())).map(drop)
    }

    fn revert(&self) -> Option<String> {
        let previous = self.previous.lock().ok()?.clone()?;
        Some(self.set(&previous).display())
    }
}

#[cfg(test)]
#[path = "time_tests.rs"]
mod tests;
