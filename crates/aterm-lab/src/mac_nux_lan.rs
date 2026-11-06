// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab mac nux`'s judgement of aterm's LAN on a new Mac, for a build whose own LAN traffic goes
//! through the root LAN broker (`docs/DESIGN-lan-broker-2026-11-05.md`): macOS's Local Network
//! alerts that name aterm (target 0, [`count_alerts`]), the broker's own rows ([`broker_cells`]:
//! launchd's view of `systems.alab.aterm-lan`, `aterm lan status`, the broker's log), and the
//! `first-launch-vault` row ([`first_launch_vault`]), whose app finds a lab vault in its own
//! guest with no press.
//!
//! The broker runs as root, so its own socket use is invisible to the lane's unprivileged reads
//! and macOS never asks about it; its rows read launchd and its log instead. What macOS asks
//! about is read twice: nehelper's `Showing local network alert for <bundle id>` line in the
//! unified log ([`nehelper_alerts`]) and, under `--reader`, the alert's own words
//! ([`reader_alerts`]). Each names whose question it is, so the lane's own traffic (a `tart
//! exec`'d command is `tart-guest-agent`'s, which macOS asked about once on 2026-11-05 when a
//! probe reached another guest) is named and never counted as aterm's.

use std::collections::BTreeMap;

use aterm_uds::lan::{LABEL, LOG_PATH};

use super::{Ask, GUEST_NUX, Guest, LOCAL_VAULT, LOCAL_VAULT_PORT, Row, Value, phrase, squeeze};
use crate::Fail;
use crate::mac::STEP;
use crate::mac_nux_guest::{Words, local_network_asker};

/// What nehelper logs as it raises macOS's Local Network question, before the asking identity:
/// `Showing local network alert for <bundle id>`.
pub const NEHELPER_SHOWING: &str = "local network alert for ";

/// The unified log's predicate for nehelper's showings ([`NEHELPER_SHOWING`]).
pub const NEHELPER_PREDICATE: &str = "subsystem == \"com.apple.networkextension\" AND eventMessage CONTAINS \"local network alert for\"";

/// The name macOS's question gives the release ("Allow “aterm” to find devices on local
/// networks?"). An `--app` bundle may carry another ([`app_name`]); both count as aterm's.
pub const ATERM_NAME: &str = "aterm";

/// The name macOS's question gives the app at `info_plist`'s values: its `CFBundleDisplayName`,
/// else its `CFBundleName` (what PlistBuddy printed for each, `""` for one it has not). `None`
/// when it has neither. Pure.
#[must_use]
pub fn app_name(display: &str, bundle: &str) -> Option<String> {
    [display, bundle]
        .into_iter()
        .map(str::trim)
        .find(|n| !n.is_empty())
        .map(str::to_owned)
}

/// Whether a bundle identity nehelper names is aterm's: the release's `com.aterm.aterm`, or any
/// other `com.aterm.` build (`--app` may be one). Pure.
#[must_use]
pub fn is_aterm_identity(id: &str) -> bool {
    id == "com.aterm.aterm" || id.starts_with("com.aterm.")
}

/// nehelper's showings in a `log show --style ndjson` record: when (epoch ms, from the event's
/// `timestamp`) and the identity it asked about, in order. Pure.
#[must_use]
pub fn nehelper_alerts(log: &str) -> Vec<(Option<u64>, String)> {
    log.lines()
        .filter_map(|line| {
            let message = crate::mac::event_message(line);
            let (_, rest) = message.split_once(NEHELPER_SHOWING)?;
            let id = rest
                .split_whitespace()
                .next()?
                .trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'));
            if id.is_empty() {
                return None;
            }
            let at_ms = aterm_json::from_str::<aterm_json::Value>(line)
                .ok()
                .and_then(|v| {
                    v.get("timestamp")?
                        .as_str()
                        .and_then(super::log_timestamp_ms)
                });
            Some((at_ms, id.to_owned()))
        })
        .collect()
}

/// The alerts the reader read: each time a process's reading came to hold macOS's Local Network
/// question, when, the process, and the app it names ([`local_network_asker`]), in order. The
/// reader writes a process's reading again only when it changed, or when that process's windows
/// went and came back, so a reading that still holds the question after one that held it (its
/// words changed around it) is the same alert, and one with the very words of the last is the
/// alert shown again. Pure.
#[must_use]
pub fn reader_alerts(read: &[Words]) -> Vec<(u64, i32, String)> {
    let mut last: BTreeMap<i32, &str> = BTreeMap::new();
    let mut out = Vec::new();
    for w in read {
        let asker = local_network_asker(&w.words);
        let before = last.insert(w.pid, &w.words);
        let Some(asker) = asker else { continue };
        let same_alert = before
            .is_some_and(|b| b != w.words && local_network_asker(b).as_deref() == Some(&asker));
        if !same_alert {
            out.push((w.at_ms, w.pid, asker));
        }
    }
    out
}

/// The Local Network alerts a row read, split by whose they were.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Alerts {
    /// aterm's alerts: the larger of nehelper's (one per aterm identity) and the reader's.
    pub aterm: usize,
    /// Each reading of one of aterm's alerts, as `<source> <who> <ms after the open> ms`.
    pub aterm_seen: Vec<String>,
    /// Each alert that named another program, the same way: never counted.
    pub others: Vec<String>,
    /// nehelper's showings of an aterm identity whose time the lane could not read: whether each
    /// was this launch's is unknown, so none counts, and a row with one and no counted alert
    /// leaves the count NOT MEASURED.
    pub untimed: Vec<String>,
}

/// [`Alerts`] since `open_ms` (epoch ms) from nehelper's showings (`shown`, [`nehelper_alerts`])
/// and the reader's alerts (`read`, [`reader_alerts`]; empty with no reader). The reader's alert
/// is aterm's when it names [`ATERM_NAME`] or the staged app's own name (`app`, [`app_name`]).
///
/// De-duplication, so each alert counts once however many of its connections macOS held back:
/// macOS asks one identity its question once and holds every later use on that alert until a
/// person answers it, so nehelper's showings count one alert per aterm identity; the reader
/// counts each time the question came up in a window ([`reader_alerts`]). The two read the same
/// alerts by different means (the reader runs only under `--reader`), so aterm's count is the
/// larger of the two, never their sum: one alert both read is one. Pure.
#[must_use]
pub fn count_alerts(
    shown: &[(Option<u64>, String)],
    read: &[Words],
    open_ms: u64,
    app: Option<&str>,
) -> Alerts {
    let after = |at: u64| at.saturating_sub(open_ms);
    let mut a = Alerts::default();
    let mut ids: Vec<&str> = Vec::new();
    for (at, id) in shown {
        let aterms = is_aterm_identity(id);
        let Some(at) = *at else {
            let seen = format!("nehelper {id} at an unread time");
            if aterms {
                a.untimed.push(seen);
            } else {
                a.others.push(seen);
            }
            continue;
        };
        if at < open_ms {
            continue;
        }
        let seen = format!("nehelper {id} {} ms", after(at));
        if aterms {
            if !ids.contains(&id.as_str()) {
                ids.push(id);
            }
            a.aterm_seen.push(seen);
        } else {
            a.others.push(seen);
        }
    }
    let alerts = reader_alerts(read);
    let mut by_reader = 0;
    for (at, pid, asker) in alerts.iter().filter(|(at, _, _)| *at >= open_ms) {
        let seen = format!("reader “{asker}” (pid {pid}) {} ms", after(*at));
        if asker == ATERM_NAME || app == Some(asker.as_str()) {
            by_reader += 1;
            a.aterm_seen.push(seen);
        } else {
            a.others.push(seen);
        }
    }
    a.aterm = ids.len().max(by_reader);
    a
}

/// The row's Local Network verdict since `open_ms` (epoch ms): nehelper's showings from the
/// guest's unified log and the reader's alerts (`read`, `None` when no reader ran), counted as
/// [`count_alerts`] counts them, with the staged app's own name read from its `Info.plist` in the
/// guest. The row fails on any alert of aterm's; a showing of aterm's at an unreadable time with
/// none counted leaves the count NOT MEASURED.
pub(super) fn alert_cells(
    g: &Guest,
    row: &mut Row,
    open_ms: u64,
    read: Option<&[Words]>,
) -> Result<(), Fail> {
    let since_s = g.clock_ms()?.saturating_sub(open_ms) / 1000 + 5;
    let log = g.sh(
        &format!(
            "/usr/bin/log show --last {since_s}s --info --style ndjson --predicate '{NEHELPER_PREDICATE}' \
             2>/dev/null || true"
        ),
        STEP,
    )?;
    let plist = format!("{}/Contents/Info.plist", g.app);
    let name = |key: &str| {
        g.run(
            &format!("/usr/libexec/PlistBuddy -c 'Print :{key}' '{plist}' 2>/dev/null || true"),
            STEP,
        )
    };
    let app = app_name(&name("CFBundleDisplayName")?, &name("CFBundleName")?);
    let a = count_alerts(
        &nehelper_alerts(&log),
        read.unwrap_or(&[]),
        open_ms,
        app.as_deref(),
    );
    row.cell(
        "aterm's Local Network alerts (target 0)",
        if a.aterm == 0 && !a.untimed.is_empty() {
            Value::Missing(format!(
                "nehelper showed aterm's alert at a time the lane could not read, so whether it \
                 was this launch's is unknown: {}",
                squeeze(&a.untimed.join("; "), 200)
            ))
        } else {
            Value::Count(a.aterm as u64)
        },
    );
    let sources = if read.is_some() {
        "nehelper's log and the reader's words"
    } else {
        "nehelper's log (no reader: --reader also reads each alert's words)"
    };
    row.cell(
        "aterm's Local Network alerts, as read",
        Value::Text({
            let seen: Vec<&str> = a
                .aterm_seen
                .iter()
                .chain(&a.untimed)
                .map(String::as_str)
                .collect();
            if seen.is_empty() {
                format!("none, by {sources}")
            } else {
                format!("{} (by {sources})", seen.join("; "))
            }
        }),
    );
    row.cell(
        "Local Network alerts not aterm's (never counted)",
        Value::Text(if a.others.is_empty() {
            "none".to_owned()
        } else {
            a.others.join("; ")
        }),
    );
    if a.aterm > 0 {
        row.fail(format!(
            "macOS asked its Local Network question about aterm {} time(s): {}",
            a.aterm,
            squeeze(&a.aterm_seen.join("; "), 200)
        ));
    }
    Ok(())
}

/// Whose process a Local Network hold was, by its place in the process tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whose {
    /// aterm's: a process running an aterm bundle's executable, or one under it, that the lane
    /// did not start.
    Aterm,
    /// The lane's own: under `tart-guest-agent` (a `tart exec`'d command, an aterm CLI the lane
    /// ran included), whose Local Network use macOS charges to the agent, never to aterm.
    Lane,
    /// Another program's.
    Other,
    /// Gone before the lane read the process table: whose it was is unknown.
    Gone,
}

/// What a command line holds when it runs an aterm bundle's executable: the app's
/// (`/Applications`, `~/Applications`) or the broker's image's.
const ATERM_EXECUTABLE: &str = "aterm.app/Contents/MacOS/aterm";

/// What the guest agent's command line holds.
const GUEST_AGENT: &str = "tart-guest-agent";

/// The process table `ps -axww -o pid=,ppid=,command=` printed: each pid's parent and command.
/// Pure.
#[must_use]
pub fn ps_tree(ps: &str) -> BTreeMap<u32, (u32, String)> {
    ps.lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let rest = rest.trim_start();
            let (ppid, command) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            Some((
                pid.parse().ok()?,
                (ppid.parse().ok()?, command.trim().to_owned()),
            ))
        })
        .collect()
}

/// [`Whose`] `pid` is in `tree` ([`ps_tree`]): the lane's when it or any ancestor is the guest
/// agent, else aterm's when it or any ancestor runs an aterm executable, else another's. Pure.
#[must_use]
pub fn whose(tree: &BTreeMap<u32, (u32, String)>, pid: u32) -> Whose {
    if !tree.contains_key(&pid) {
        return Whose::Gone;
    }
    let mut chain = Vec::new();
    let mut at = pid;
    // The tree's depth bounds the walk, so a table read mid-change cannot loop it.
    while let Some((parent, command)) = tree.get(&at) {
        chain.push(command.as_str());
        if *parent <= 1 || chain.len() > tree.len() {
            break;
        }
        at = *parent;
    }
    if chain.iter().any(|c| c.contains(GUEST_AGENT)) {
        Whose::Lane
    } else if chain.iter().any(|c| c.contains(ATERM_EXECUTABLE)) {
        Whose::Aterm
    } else {
        Whose::Other
    }
}

/// One launchd job's state as `launchctl print system/<label>` reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Daemon {
    pub state: String,
    pub pid: Option<u32>,
    pub program: String,
    /// How many times launchd started it since it was bootstrapped.
    pub runs: Option<u32>,
    pub last_exit: String,
}

impl Daemon {
    /// Whether it runs now.
    #[must_use]
    pub fn running(&self) -> bool {
        self.state == "running"
    }
}

/// [`Daemon`] from `launchctl print`'s top-level `key = value` lines (one tab in; a nested
/// block's are deeper); `None` when launchd has no such service. Pure.
#[must_use]
pub fn read_launchctl(text: &str) -> Option<Daemon> {
    if !text
        .lines()
        .any(|l| l.trim_end().ends_with("= {") && !l.starts_with('\t'))
    {
        return None;
    }
    let mut d = Daemon::default();
    for line in text.lines() {
        let Some(top) = line.strip_prefix('\t').filter(|l| !l.starts_with('\t')) else {
            continue;
        };
        let Some((key, value)) = top.split_once(" = ") else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "state" => d.state = value.to_owned(),
            "pid" => d.pid = value.parse().ok(),
            "program" => d.program = value.to_owned(),
            "runs" => d.runs = value.parse().ok(),
            "last exit code" => d.last_exit = value.to_owned(),
            _ => {}
        }
    }
    Some(d)
}

/// How many of the broker's processes ended since launchd bootstrapped it: each start but the
/// one running now (0 with no broker). Pure.
#[must_use]
pub fn broker_exits(broker: Option<&Daemon>) -> usize {
    broker.map_or(0, |d| {
        usize::try_from(d.runs.unwrap_or(0))
            .unwrap_or(usize::MAX)
            .saturating_sub(usize::from(d.running()))
    })
}

/// What `aterm lan status` said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanStatus {
    /// A broker answered: `broker protocol=<n> build=<id> socket=<path>`.
    Serving {
        protocol: u32,
        build: String,
        socket: String,
    },
    /// No broker is installed, and `aterm lan status` served itself under macOS's allowance
    /// (the design's §5 direct path): the allowance, in its words.
    Direct(String),
    /// The build has the verb, and no broker answered: why, in its words.
    Not(String),
    /// The build has no `lan` verb: no broker is part of it.
    NoVerb,
}

/// What `aterm lan status` says before the allowance its own process served itself under.
const DIRECT_SAYS: &str = "aterm makes its own LAN connections as ";

/// [`LanStatus`] over `aterm lan status`'s output. A build with no `lan` verb says so in one of
/// two wordings: with no terminal on stdin (a `tart exec`'d command) the word goes to the window
/// parser, which quotes it (`unknown command 'lan'`); with one, the CLI does not. Pure.
#[must_use]
pub fn read_lan_status(out: &str) -> LanStatus {
    if out.contains("unknown command 'lan'") || out.contains("unknown command lan") {
        return LanStatus::NoVerb;
    }
    if let Some((_, rest)) = out.split_once(DIRECT_SAYS) {
        let words = rest
            .split_once(" (socket=")
            .map_or(rest, |(words, _)| words);
        return LanStatus::Direct(squeeze(words.trim(), 200));
    }
    for line in out.lines() {
        let Some(rest) = line.trim().strip_prefix("broker ") else {
            continue;
        };
        let field = |key: &str| {
            rest.split_whitespace()
                .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
        };
        let socket = rest
            .split_once(" socket=")
            .map(|(_, s)| s.trim().to_owned());
        if let (Some(protocol), Some(build), Some(socket)) = (
            field("protocol").and_then(|p| p.parse().ok()),
            field("build"),
            socket,
        ) {
            return LanStatus::Serving {
                protocol,
                build: build.to_owned(),
                socket,
            };
        }
    }
    LanStatus::Not(squeeze(out.trim(), 200))
}

/// How many of the broker log's last lines a row shows.
const LOG_TAIL: usize = 12;

/// The broker's rows: launchd's view of it (`print`, `launchctl print system/<LABEL>` as root,
/// read beside the elevation's process accounting), `aterm lan status` as the account at the
/// console, and its log's last [`LOG_TAIL`] lines (read as root). On a build with no `lan` verb
/// each is NOT MEASURED. With the verb, the row fails when no broker serves although setup's
/// elevation ran its root half (`elevated`), which installs it.
pub(super) fn broker_cells(
    g: &Guest,
    row: &mut Row,
    print: &str,
    elevated: bool,
) -> Result<(), Fail> {
    let said = g.run(&format!("{} lan status 2>&1", g.aterm), STEP)?;
    let status = read_lan_status(&said);
    if status == LanStatus::NoVerb {
        let why = "this build has no LAN broker (`aterm lan status`: unknown command)";
        for cell in ["LAN broker", "aterm lan status", "LAN broker log"] {
            row.cell(cell, Value::Missing(why.into()));
        }
        return Ok(());
    }
    row.cell(
        "LAN broker",
        Value::Text(match read_launchctl(print) {
            None => format!(
                "not loaded ({})",
                squeeze(
                    print
                        .lines()
                        .next()
                        .unwrap_or("launchctl said nothing")
                        .trim(),
                    120
                )
            ),
            Some(d) => format!(
                "state={} pid={} program={} runs={} last exit={}",
                d.state,
                d.pid.map_or_else(|| "-".to_owned(), |p| p.to_string()),
                d.program,
                d.runs.map_or_else(|| "?".to_owned(), |r| r.to_string()),
                d.last_exit
            ),
        }),
    );
    row.cell(
        "aterm lan status",
        Value::Text(match &status {
            LanStatus::Serving {
                protocol,
                build,
                socket,
            } => format!("serving: protocol={protocol} build={build} socket={socket}"),
            LanStatus::Direct(allowed) => format!("no broker; served directly: {allowed}"),
            LanStatus::Not(why) => format!("no broker: {why}"),
            LanStatus::NoVerb => unreachable!("handled above"),
        }),
    );
    let log = g.run(
        &g.as_root(&format!(
            "/usr/bin/tail -n {LOG_TAIL} '{LOG_PATH}' 2>/dev/null || true"
        )),
        STEP,
    )?;
    let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    row.cell(
        "LAN broker log",
        Value::Text(if lines.is_empty() {
            format!("{LOG_PATH} empty or absent")
        } else {
            squeeze(&lines.join(" | "), 600)
        }),
    );
    if elevated && !matches!(status, LanStatus::Serving { .. }) {
        row.fail(format!(
            "setup's elevation ran its root half, and no LAN broker serves ({LABEL})"
        ));
    }
    Ok(())
}

/// The variable the vault look reads for `first-launch-vault`: the Bonjour type every vault
/// browse of the look browses, through the broker, in place of `_alab-vault._tcp`
/// (`aterm_secrets::discover::vault_service`: a value that is no `_alab-lab-<nonce>._tcp`
/// browses for no vault). Set, it marks the process a lab's, so A12 refuses a live-vault join,
/// and the look logs each vault instance it heard to `aterm.log` on a line naming the service and
/// the instance, which [`vault_heard`] reads. The GUI app gets the variable from its launchd
/// session: the row sets it with `launchctl setenv` in the account's GUI domain before the open,
/// since an app `open` starts inherits no environment from the lane's shell.
pub use aterm_secrets::discover::LAB_VAULT_SERVICE_ENV;

/// The instance name the lab vault is registered under: the one the vault look takes for the
/// ALab vault (`aterm_secrets::discover`'s `alab-secrets` or a collision rename of it).
pub const LAB_VAULT_INSTANCE: &str = "alab-secrets";

/// `aterm.log`'s lines that name `service` (the lab vault service) and the lab vault's instance:
/// the vault look heard the lab vault. Pure.
#[must_use]
pub fn vault_heard<'a>(log: &'a str, service: &str) -> Vec<&'a str> {
    log.lines()
        .filter(|l| l.contains(service) && l.contains(LAB_VAULT_INSTANCE))
        .collect()
}

/// `aterm.log`'s lines where the vault look heard an instance of the live `_alab-vault._tcp`:
/// none on an app whose look browses the lab vault service alone. Pure.
#[must_use]
pub fn live_vault_heard(log: &str) -> Vec<&str> {
    let live = format!(" on {}", super::ALAB_SERVICE);
    log.lines()
        .filter(|l| l.contains("vault look: heard ") && l.trim_end().ends_with(&live))
        .collect()
}

/// The guest commands that serve the lab vault as its own identity (`home`): a server on every
/// address at `port` (so the advert resolves to the guest's LAN address) that advertises nothing
/// itself, a vault named [`LOCAL_VAULT`] made on it, then a registration of it under the lab
/// `service` as [`LAB_VAULT_INSTANCE`] (`dns-sd -R`, in the background while the row runs).
///
/// Everything the lane itself sends the server goes to 127.0.0.1: this runs before setup's
/// elevation installs the broker, and on a broker build aterm has no direct path to a LAN
/// address, so a `secrets create` dialled at one fails; a `curl` to one from `tart exec` would
/// put the guest agent's own Local Network alert on screen for the whole launch. Only the app's
/// browse and dial, through the broker, cross the LAN. A wait on the server answering is a
/// one-minute hang detector.
fn lab_vault_setup(aterm: &str, home: &str, port: u16, service: &str) -> String {
    let local = format!("http://127.0.0.1:{port}");
    format!(
        "set -e; A={home}; rm -rf $A; mkdir -p $A/seed; printf nux-probe-value > $A/seed/NUX_PROBE; export HOME=$A; \
         ({aterm} secrets serve --listen 0.0.0.0:{port} --no-advertise --no-keep-awake --store $A/served </dev/null >$A/serve.log 2>&1 &); \
         n=0; until curl -s -o /dev/null --max-time 1 {local}/; do \
         n=$((n+1)); [ $n -gt 600 ] && {{ echo 'the lab vault did not answer within 60 s'; cat $A/serve.log; exit 1; }}; sleep 0.1; done; \
         {aterm} secrets create '{local}/v1/{LOCAL_VAULT}' --import $A/seed --no-agent --name nux-seed > $A/create.out 2>&1 || {{ cat $A/create.out; exit 1; }}; \
         (/usr/bin/dns-sd -R {LAB_VAULT_INSTANCE} {service} local {port} </dev/null >$A/register.log 2>&1 &); echo served"
    )
}

/// `first-launch-vault`: a first launch whose app finds a lab vault served in its own guest, by
/// its own browse through the broker, with no press (`docs/DESIGN-lan-broker-2026-11-05.md` §7).
///
/// The guest never reaches the live ALab (A12, the lane's packet filter) and tart's NAT keeps
/// guests apart, so the vault is the guest's own: served at the guest's address and registered
/// under a lab vault service made for the run (`_alab-lab-<nonce>._tcp`, which no ALab Mac
/// registers or browses), named to the app by [`LAB_VAULT_SERVICE_ENV`] in its GUI session. On an
/// app whose executable does not name that variable the row is NOT RUN before anything is served:
/// that build's look would browse the live `_alab-vault._tcp`. Otherwise it is `first-launch`'s
/// row, then the vault verdict: the app's `aterm.log` names the lab vault it heard
/// ([`vault_heard`]) and no instance of the live `_alab-vault._tcp` ([`live_vault_heard`]). The
/// broker logs no request it answered, only one it refused, so the row shows its log's lines
/// that name the lab service: a refused browse or resolve of it.
pub(super) fn first_launch_vault(g: &Guest, ask: &Ask, row: &mut Row) -> Result<(), Fail> {
    let named = g.run(
        &format!(
            "/usr/bin/grep -q -a -F {LAB_VAULT_SERVICE_ENV} '{}' && echo named || echo absent",
            g.aterm
        ),
        STEP,
    )?;
    if named.trim() != "named" {
        return Err(crate::mac::not_run(format!(
            "the app's executable names no {LAB_VAULT_SERVICE_ENV}, so its vault look would \
             browse the live _alab-vault._tcp: the vault found with no press is NOT MEASURED \
             on a build that does not read that knob"
        )));
    }
    let mut bytes = [0u8; 6];
    aterm_uds::rand::fill(&mut bytes).map_err(|e| Fail::die(format!("random bytes: {e}")))?;
    let service = phrase::lab_service_of(bytes);
    let addresses = phrase::inet_addresses(&g.run("/sbin/ifconfig -a 2>/dev/null || true", STEP)?);
    let Some(lan) = phrase::lan_address(&addresses) else {
        return Err(Fail::die(
            "the guest has no address on tart's network for the lab vault to listen at",
        ));
    };
    let listen = format!("{lan}:{LOCAL_VAULT_PORT}");
    let served = g.sh(
        &lab_vault_setup(
            &g.aterm,
            &format!("{GUEST_NUX}/lab-vault"),
            LOCAL_VAULT_PORT,
            &service,
        ),
        STEP + STEP,
    )?;
    if served.trim().lines().last() != Some("served") {
        return Err(Fail::die(format!(
            "the lab vault did not come up: {}",
            squeeze(served.trim(), 300)
        )));
    }
    let set = g.sh(
        &g.as_root(&format!(
            "/bin/launchctl asuser {uid} /bin/launchctl setenv {LAB_VAULT_SERVICE_ENV} {service} && \
             /bin/launchctl asuser {uid} /bin/launchctl getenv {LAB_VAULT_SERVICE_ENV}",
            uid = g.uid
        )),
        STEP,
    )?;
    if set.trim() != service {
        return Err(Fail::die(format!(
            "the GUI session's {LAB_VAULT_SERVICE_ENV} did not read back: {}",
            squeeze(set.trim(), 120)
        )));
    }
    row.cell(
        "lab vault",
        Value::Text(format!(
            "{LAB_VAULT_INSTANCE} under {service} at {listen} ({LAB_VAULT_SERVICE_ENV} in gui/{})",
            g.uid
        )),
    );
    super::first_launch(g, ask, row)?;
    let log = g.log()?;
    let heard = vault_heard(&log, &service);
    let live = live_vault_heard(&log);
    let broker_log = g.run(
        &g.as_root(&format!(
            "/usr/bin/grep -F -- '{service}' '{LOG_PATH}' 2>/dev/null || true"
        )),
        STEP,
    )?;
    row.cell(
        "the lab vault found by the app's own browse, no press",
        if heard.is_empty() {
            Value::Text("not found: aterm.log names no lab vault it heard".into())
        } else {
            Value::Text(squeeze(&heard.join(" | "), 300))
        },
    );
    row.cell(
        "the broker's refusals of the lab service",
        Value::Text(if broker_log.trim().is_empty() {
            format!("none: {LOG_PATH} names no {service}")
        } else {
            squeeze(broker_log.trim(), 300)
        }),
    );
    if !live.is_empty() {
        row.fail(format!(
            "the app's vault look heard the live {}: {}",
            super::ALAB_SERVICE,
            squeeze(&live.join(" | "), 300)
        ));
    }
    if heard.is_empty() {
        row.fail(format!(
            "the app's vault look never heard the lab vault ({LAB_VAULT_INSTANCE} under {service})"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ndjson(ts: &str, message: &str) -> String {
        format!(
            "{{\"timestamp\":\"{ts}\",\"subsystem\":\"com.apple.networkextension\",\"eventMessage\":\"{message}\"}}\n"
        )
    }

    fn words(at_ms: u64, pid: i32, words: &str) -> Words {
        Words {
            at_ms,
            pid,
            owner: "UserNotificationCenter".into(),
            words: words.into(),
        }
    }

    const ATERM_ASKS: &str = "AXStaticText \"Allow “aterm” to find devices on local networks?\"";
    const AGENT_ASKS: &str =
        "AXStaticText \"Allow “tart-guest-agent” to find devices on local networks?\"";

    /// An aterm alert counts; the guest agent's (a lane's own `tart exec` traffic) never does,
    /// and each alert counts once however both sources read it.
    #[test]
    fn only_aterms_own_local_network_alerts_count_and_each_once() {
        let log = format!(
            "{}{}{}{}",
            ndjson(
                "2026-11-05 10:00:01.000000-0800",
                "Showing local network alert for com.aterm.aterm"
            ),
            ndjson(
                "2026-11-05 10:00:01.500000-0800",
                "Showing local network alert for com.aterm.aterm"
            ),
            ndjson(
                "2026-11-05 10:00:02.000000-0800",
                "Showing local network alert for com.cirruslabs.tart-guest-agent"
            ),
            "garbage\n"
        );
        let shown = nehelper_alerts(&log);
        assert_eq!(shown.len(), 3);
        assert_eq!(shown[0].1, "com.aterm.aterm");
        let open = shown[0].0.unwrap() - 1000;
        let read = [words(open + 1200, 300, ATERM_ASKS)];
        let a = count_alerts(&shown, &read, open, Some("aterm"));
        assert_eq!(a.aterm, 1, "one alert, read by both, shown twice: {a:?}");
        assert_eq!(a.aterm_seen.len(), 3);
        assert_eq!(
            a.others,
            ["nehelper com.cirruslabs.tart-guest-agent 2000 ms"]
        );
        // NEGATIVE CONTROL: the guest agent's alert alone, by both sources, counts nothing.
        let agent = nehelper_alerts(&ndjson(
            "2026-11-05 10:00:02.000000-0800",
            "Showing local network alert for com.cirruslabs.tart-guest-agent",
        ));
        let a = count_alerts(&agent, &[words(open + 2100, 301, AGENT_ASKS)], open, None);
        assert_eq!(a.aterm, 0, "{a:?}");
        assert_eq!(a.others.len(), 2);
        // Before the open is not the launch's.
        let a = count_alerts(&shown, &[], shown[2].0.unwrap() + 1, None);
        assert_eq!(a.aterm, 0);
        // A showing at an unreadable time is never counted, and is named apart.
        let untimed = [(None, "com.aterm.aterm".to_owned())];
        let a = count_alerts(&untimed, &[], open, None);
        assert_eq!((a.aterm, a.untimed.len()), (0, 1), "{a:?}");
        assert_eq!(
            nehelper_alerts(&ndjson(
                "not a time",
                "Showing local network alert for com.aterm.aterm"
            )),
            untimed
        );
        assert!(is_aterm_identity("com.aterm.aterm-dev"));
        assert!(!is_aterm_identity("com.atermx"));
    }

    /// The reader's alerts: words that change around the question are the same alert, the very
    /// words again (the window went and came back) a new one, another process a new one.
    #[test]
    fn the_readers_alerts_are_each_time_the_question_came_up() {
        let read = [
            words(100, 300, "AXStaticText \"Background Items Added\""),
            words(200, 300, ATERM_ASKS),
            words(300, 300, &format!("{ATERM_ASKS}; AXButton \"Allow\"")),
            words(400, 300, ATERM_ASKS),
            words(500, 300, ATERM_ASKS),
            words(600, 301, ATERM_ASKS),
        ];
        let alerts = reader_alerts(&read);
        assert_eq!(
            alerts
                .iter()
                .map(|(at, pid, _)| (*at, *pid))
                .collect::<Vec<_>>(),
            [(200, 300), (500, 300), (600, 301)]
        );
        assert_eq!(count_alerts(&[], &read, 0, None).aterm, 3);
        // An --app bundle with its own name: its alert is aterm's by that name.
        let dev = [words(
            100,
            400,
            "AXStaticText \"Allow “aterm (dev)” to find devices on local networks?\"",
        )];
        assert_eq!(count_alerts(&[], &dev, 0, None).aterm, 0);
        assert_eq!(count_alerts(&[], &dev, 0, Some("aterm (dev)")).aterm, 1);
        assert_eq!(
            app_name("", "aterm (dev)\n").as_deref(),
            Some("aterm (dev)")
        );
        assert_eq!(app_name("Shown", "aterm").as_deref(), Some("Shown"));
        assert_eq!(app_name(" ", ""), None);
        assert!(
            reader_alerts(&read[..1]).is_empty(),
            "no question, no alert"
        );
    }

    /// A hold of a process under the guest agent is the lane's, even an aterm CLI the lane ran;
    /// the app and its children are aterm's.
    #[test]
    fn a_holds_process_is_placed_by_its_ancestors() {
        let tree = ps_tree(
            "    1     0 /sbin/launchd\n\
             \x20 210     1 /usr/local/bin/tart-guest-agent --run-daemon\n\
             \x20 400   210 /bin/sh -c nc 192.168.64.117 22\n\
             \x20 401   400 /usr/bin/nc 192.168.64.117 22\n\
             \x20 500     1 /Applications/aterm.app/Contents/MacOS/aterm\n\
             \x20 501   500 /usr/bin/dns-sd -B _alab-vault._tcp local.\n\
             \x20 600   210 /Applications/aterm.app/Contents/MacOS/aterm ctl status\n\
             \x20 700     1 /usr/libexec/rapportd\n",
        );
        assert_eq!(tree.get(&501).map(|(p, _)| *p), Some(500));
        assert_eq!(whose(&tree, 500), Whose::Aterm);
        assert_eq!(whose(&tree, 501), Whose::Aterm);
        assert_eq!(whose(&tree, 401), Whose::Lane);
        assert_eq!(
            whose(&tree, 600),
            Whose::Lane,
            "the lane's tart exec of aterm"
        );
        assert_eq!(whose(&tree, 700), Whose::Other);
        assert_eq!(whose(&tree, 999), Whose::Gone);
    }

    #[test]
    fn launchd_and_lan_status_read_the_broker() {
        let print = "system/systems.alab.aterm-lan = {\n\
                     \tactive count = 1\n\
                     \tpath = /Library/LaunchDaemons/systems.alab.aterm-lan.plist\n\
                     \tstate = running\n\
                     \n\
                     \tprogram = /Library/Application Support/aterm/lan/aterm.app/Contents/MacOS/aterm\n\
                     \targuments = {\n\
                     \t\t/Library/Application Support/aterm/lan/aterm.app/Contents/MacOS/aterm\n\
                     \t\tlan\n\
                     \t\tserve\n\
                     \t}\n\
                     \tendpoints = {\n\
                     \t\tstate = active\n\
                     \t}\n\
                     \truns = 2\n\
                     \tpid = 812\n\
                     \tlast exit code = 0\n\
                     }\n";
        let d = read_launchctl(print).unwrap();
        assert_eq!(
            d.state, "running",
            "a nested block's state is not the job's"
        );
        assert_eq!(d.pid, Some(812));
        assert_eq!(d.runs, Some(2));
        assert!(d.program.ends_with("lan/aterm.app/Contents/MacOS/aterm"));
        assert_eq!(
            broker_exits(Some(&d)),
            1,
            "one adoption restart ended a broker"
        );
        assert_eq!(
            read_launchctl(
                "Could not find service \"systems.alab.aterm-lan\" in domain for system\n"
            ),
            None
        );
        assert_eq!(broker_exits(None), 0);
        let stopped = Daemon {
            state: "not running".into(),
            runs: Some(1),
            ..Daemon::default()
        };
        assert_eq!(broker_exits(Some(&stopped)), 1);

        assert_eq!(
            read_lan_status(
                "broker protocol=1 build=0.155.0+gabc1234 socket=/var/run/systems.alab.aterm-lan/broker.sock\n"
            ),
            LanStatus::Serving {
                protocol: 1,
                build: "0.155.0+gabc1234".into(),
                socket: "/var/run/systems.alab.aterm-lan/broker.sock".into(),
            }
        );
        // What a build with no `lan` verb prints under `tart exec` (no terminal on stdin).
        assert_eq!(
            read_lan_status("aterm: unknown command 'lan' (try --help)\n"),
            LanStatus::NoVerb
        );
        assert_eq!(
            read_lan_status("aterm: unknown command lan (try --help)\n"),
            LanStatus::NoVerb
        );
        assert!(matches!(
            read_lan_status("no broker: setup's elevation has not installed it\n"),
            LanStatus::Not(why) if why.contains("not installed")
        ));
        // The direct path's line (crates/aterm/src/lan_verb.rs), and the plain one beside it.
        assert_eq!(
            read_lan_status(
                "the LAN broker is not installed: until setup's Mac password installs it, aterm \
                 makes its own LAN connections as macOS allowed com.aterm.aterm (team A66A9P66Z7) \
                 on the local network (socket=/var/run/systems.alab.aterm-lan/broker.sock)\n"
            ),
            LanStatus::Direct(
                "macOS allowed com.aterm.aterm (team A66A9P66Z7) on the local network".into()
            )
        );
        assert!(matches!(
            read_lan_status(
                "the LAN broker is not installed: setup's Mac password installs it \
                 (`sudo aterm machines here apply --plain`) (socket=/var/run/systems.alab.aterm-lan/broker.sock)\n"
            ),
            LanStatus::Not(_)
        ));
    }

    #[test]
    fn the_lab_vault_is_heard_only_on_its_own_service() {
        let log = "1 INFO aterm: vault look: heard alab-secrets on _alab-lab-x1y2z3._tcp\n\
                   2 INFO aterm: vault look: heard alab-secrets on _alab-vault._tcp\n";
        assert_eq!(vault_heard(log, "_alab-lab-x1y2z3._tcp").len(), 1);
        assert!(vault_heard(log, "_alab-lab-other._tcp").is_empty());
        // NEGATIVE CONTROL: `live_vault_heard` matching any `heard` line takes the lab's too.
        assert_eq!(
            live_vault_heard(log),
            ["2 INFO aterm: vault look: heard alab-secrets on _alab-vault._tcp"]
        );
        assert!(live_vault_heard(log.lines().next().unwrap()).is_empty());
        let setup = lab_vault_setup("/a/aterm", "/h", 7485, "_alab-lab-x1y2z3._tcp");
        assert!(setup.contains("--no-advertise"), "{setup}");
        assert!(setup.contains("--listen 0.0.0.0:7485"), "{setup}");
        assert!(
            setup.contains("secrets create 'http://127.0.0.1:7485/v1/"),
            "the lane's own dial stays on loopback: {setup}"
        );
        assert!(!setup.contains("192.168."), "{setup}");
        assert!(
            setup.contains("dns-sd -R alab-secrets _alab-lab-x1y2z3._tcp local 7485"),
            "{setup}"
        );
        assert!(!setup.contains("_alab-vault._tcp"), "{setup}");
    }
}
