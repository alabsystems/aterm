// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Time zone and network time through `systemsetup` (design §5.1).
//!
//! Measured in the macOS 27.0 VM: `systemsetup -settimezone`, `-setusingnetworktime` and
//! `-setnetworktimeserver` (and their `-get…`) work as root over SSH, and also from a root
//! launchd job that could not open the system TCC database, so none of them needs Full
//! Disk Access. Not as root, every verb prints `You need administrator access to run this
//! tool... exiting!` and exits **0**, so a run is judged by its words and its read-back,
//! never by its exit status alone. A set also writes an `### Error:-99 …
//! InternetServices.m` line to standard error and exits 0; that line is noise. An unknown
//! zone answers `Mars/Base is not a valid timezone.` and exits 1.
//!
//! The zone is read back from `/etc/localtime`'s link (`/var/db/timezone/zoneinfo/GMT` in
//! the VM), the network-time state and server from `systemsetup -get…`.

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

/// What `systemsetup` prints, exit 0, when it is not run as root.
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
///
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

/// The zone a `/etc/localtime` link target names: what follows its `zoneinfo` directory
/// (`/var/db/timezone/zoneinfo/America/Los_Angeles` is `America/Los_Angeles`).
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

    /// The `systemsetup` run that sets `value` (the zone, `on`/`off`, the server).
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

    /// The current value, in the form [`Setting::set`] takes.
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
            // Host names are case-insensitive.
            Kind::Server(s) => now.eq_ignore_ascii_case(s),
            _ => now == self.wanted(),
        }
    }
}

const fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// `cmd`'s standard output, refused when it exited nonzero or `systemsetup` said it is not
/// root (it exits 0 then).
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
            // A plan must not promise a zone this Mac does not have.
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
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::model::{HostFacts, Mode, Outcome};
    use crate::port::{Fake, Stat};

    fn ctx() -> Ctx {
        Ctx {
            facts: HostFacts::default(),
            users: Vec::new(),
            console: None,
            host_name: "lab-07".into(),
            cache_dir: "/c".into(),
            disrupt: true,
        }
    }

    /// What `systemsetup -settimezone` writes to standard error in the VM, exit 0.
    const NOISE: &str = "2026-10-16 23:07:26.027 systemsetup[2456:28578] ### Error:-99 File:/AppleInternal/Library/BuildRoots/4~CV23ugBcSuPoqTCv2d4ySbycsdbOG37Qm_PyGCo/Library/Caches/com.apple.xbs/TemporaryDirectory.ALqQnw/Sources/Admin/InternetServices.m Line:395";

    fn set_ok(stdout: &str) -> Ran {
        Ran {
            status: Some(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: NOISE.as_bytes().to_vec(),
        }
    }

    fn zone_file() -> Stat {
        Stat {
            uid: 0,
            gid: 0,
            mode: 0o644,
            dir: false,
            symlink: false,
            len: 2_852,
        }
    }

    #[test]
    fn the_zone_is_what_follows_the_zoneinfo_directory_in_the_link() {
        assert_eq!(
            zone_of_link("/var/db/timezone/zoneinfo/GMT\n").as_deref(),
            Some("GMT")
        );
        assert_eq!(
            zone_of_link("/var/db/timezone/zoneinfo/America/Los_Angeles").as_deref(),
            Some("America/Los_Angeles")
        );
        assert_eq!(
            zone_of_link("/usr/share/zoneinfo.default/Europe/London").as_deref(),
            Some("Europe/London")
        );
        assert_eq!(zone_of_link("/var/db/timezone/zoneinfo/"), None);
        assert_eq!(zone_of_link("/etc/elsewhere"), None);
    }

    #[test]
    fn setting_the_zone_runs_systemsetup_and_reads_back_the_link() {
        let sys = Fake::root();
        sys.on(
            "/usr/bin/readlink /etc/localtime",
            Ran::ok("/var/db/timezone/zoneinfo/GMT\n"),
        )
        .on(
            "/usr/bin/readlink /etc/localtime",
            Ran::ok("/var/db/timezone/zoneinfo/America/Los_Angeles\n"),
        )
        .on(
            "/usr/sbin/systemsetup -settimezone America/Los_Angeles",
            set_ok("Set TimeZone: America/Los_Angeles\n"),
        )
        .stat_of("/var/db/timezone/zoneinfo/America/Los_Angeles", zone_file());
        let t = Table {
            zone: Some("America/Los_Angeles".into()),
            ..Table::default()
        };
        let items = expand(&t, &ctx()).unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:#?}");
        assert_eq!(rows[0].from.as_deref(), Some("GMT"));
        assert_eq!(
            rows[0].revert.as_deref(),
            Some("/usr/sbin/systemsetup -settimezone GMT")
        );
    }

    #[test]
    fn a_zone_this_mac_lacks_is_caught_by_the_plan_not_by_the_apply() {
        let sys = Fake::root();
        sys.on(
            "/usr/bin/readlink /etc/localtime",
            Ran::ok("/var/db/timezone/zoneinfo/GMT\n"),
        );
        let t = Table {
            zone: Some("Mars/Base".into()),
            ..Table::default()
        };
        let items = expand(&t, &ctx()).unwrap();
        let rows = engine::run(&items, Mode::Plan, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Unknown);
        assert!(
            rows[0]
                .why
                .as_deref()
                .unwrap()
                .contains("no time zone Mars/Base")
        );
        assert!(sys.calls().iter().all(|c| !c.contains("-settimezone")));
    }

    #[test]
    fn a_zone_or_server_that_is_not_a_name_is_refused_when_the_baseline_expands() {
        for zone in ["", "/etc/passwd", "../x", "America//X", "Amer ica/X", "-x"] {
            let t = Table {
                zone: Some(zone.into()),
                ..Table::default()
            };
            assert!(expand(&t, &ctx()).is_err(), "{zone:?}");
        }
        for server in ["", "-x", "time apple com", "a;b"] {
            let t = Table {
                server: Some(server.into()),
                ..Table::default()
            };
            assert!(expand(&t, &ctx()).is_err(), "{server:?}");
        }
        let t = Table {
            zone: Some("Etc/GMT+8".into()),
            server: Some("time.apple.com".into()),
            network_time: Some(true),
        };
        assert_eq!(expand(&t, &ctx()).unwrap().len(), 3);
    }

    #[test]
    fn network_time_and_its_server_read_systemsetups_own_lines() {
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/systemsetup -getusingnetworktime",
            Ran::ok("Network Time: On\n"),
        )
        .on(
            "/usr/sbin/systemsetup -getnetworktimeserver",
            Ran::ok("Network Time Server: time.apple.com\n"),
        );
        let t = Table {
            network_time: Some(true),
            server: Some("Time.Apple.com".into()),
            ..Table::default()
        };
        let items = expand(&t, &ctx()).unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert!(rows.iter().all(|r| r.outcome == Outcome::Ok), "{rows:#?}");
        assert_eq!(rows[0].to.as_deref(), Some("on"));
    }

    #[test]
    fn turning_network_time_off_reverts_by_turning_it_back_on() {
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/systemsetup -getusingnetworktime",
            Ran::ok("Network Time: On\n"),
        )
        .on(
            "/usr/sbin/systemsetup -getusingnetworktime",
            Ran::ok("Network Time: Off\n"),
        )
        .on(
            "/usr/sbin/systemsetup -setusingnetworktime off",
            set_ok("setUsingNetworkTime: Off\n"),
        );
        let t = Table {
            network_time: Some(false),
            ..Table::default()
        };
        let items = expand(&t, &ctx()).unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Changed);
        assert_eq!(
            rows[0].revert.as_deref(),
            Some("/usr/sbin/systemsetup -setusingnetworktime on")
        );
    }

    #[test]
    fn systemsetup_refusing_a_non_root_caller_with_exit_0_is_not_read_as_an_answer() {
        // `systemsetup -gettimezone` as the admin, not root, in the VM: exit 0.
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/systemsetup -getusingnetworktime",
            Ran::ok("You need administrator access to run this tool... exiting!\n"),
        );
        let t = Table {
            network_time: Some(true),
            ..Table::default()
        };
        let items = expand(&t, &ctx()).unwrap();
        let rows = engine::run(&items, Mode::Apply, &sys, &ctx(), &mut NoJournal);
        assert_eq!(rows[0].outcome, Outcome::Unknown);
        assert!(rows[0].why.as_deref().unwrap().contains("needs root"));
    }
}
