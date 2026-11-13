// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LAN discovery of Macs via Bonjour (`_ssh._tcp`, `_sftp-ssh._tcp`, `_rfb._tcp`) and port-22 sweep.
//! Both go through the root LAN broker on macOS (`aterm_uds::lan`), so macOS asks nobody whether
//! aterm may use the local network: the port's [`System::bonjour`] for Bonjour, and
//! `aterm_uds::lan::connect` for the sweep's connects.

use super::inventory::{Inventory, Paths};
use super::ssh::fan_out;
use crate::model::Read;
use crate::port::{Bonjour, System};
use aterm_uds::lan::{Event, LanError};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

/// Remote Login's service type.
pub const SSH_SERVICE: &str = "_ssh._tcp";

/// Screen Sharing's service type.
pub const RFB_SERVICE: &str = "_rfb._tcp";

/// SFTP service type macOS Remote Login advertises beside [`SSH_SERVICE`].
pub const SFTP_SERVICE: &str = "_sftp-ssh._tcp";

/// How long one resolve listens for answers.
pub const RESOLVE_BUDGET: Duration = Duration::from_secs(2);

/// Resolves run at once. Each listens its whole budget (an answer can come on each interface),
/// so a lab of 65 Macs resolves in three rounds.
const RESOLVE_PARALLEL: usize = 32;

/// The widest subnet [`subnet`] sweeps: a /22, 1024 addresses.
pub const WIDEST_PREFIX: u8 = 22;

/// Connects a sweep has in flight at once.
const KNOCK_PARALLEL: usize = 64;

/// Where a row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A Bonjour advertisement.
    Bonjour,
    /// A port that answered the subnet sweep.
    Subnet,
}

/// One Mac a scan found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The advertised name (the Mac's Computer Name), or the address for a swept row.
    pub name: String,
    /// What to connect to: the advertised host (`lab-07.local`), or the swept address.
    pub address: String,
    /// Remote Login's port (Screen Sharing's for a Mac that advertises only that).
    pub port: u16,
    /// Remote Login answers (advertised `_ssh._tcp`, or port 22 accepted a connection).
    pub ssh: bool,
    /// Screen Sharing is advertised (`_rfb._tcp`).
    pub screen_sharing: bool,
    /// macOS's SFTP is advertised under the same name (`_sftp-ssh._tcp`, [`SFTP_SERVICE`]).
    pub sftp: bool,
    /// Bonjour or the sweep.
    pub source: Source,
}

/// One advertised instance, merged across the interfaces it was seen on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    /// The instance name, exactly as advertised.
    pub name: String,
    /// The interface indexes it is currently advertised on.
    pub interfaces: Vec<u32>,
}

/// What a Bonjour browse learned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Browse {
    /// The Macs it found, one row per advertised host, sorted by name.
    pub found: Vec<Found>,
    /// Advertised instances no resolve answered for within [`RESOLVE_BUDGET`].
    pub unresolved: Vec<String>,
    /// Why the `_sftp-ssh._tcp` browse failed, when it did.
    pub sftp_failed: Option<String>,
}

/// Browse `_ssh._tcp`, `_sftp-ssh._tcp` and `_rfb._tcp` for `wait`, then resolve each instance.
///
/// # Errors
/// One line when the `_ssh._tcp` or `_rfb._tcp` browse could not run: no LAN broker to ask (its
/// words name what installs it), or Bonjour's own error.
pub fn browse(sys: &dyn System, wait: Duration) -> Result<Browse, String> {
    let services = [SSH_SERVICE, RFB_SERVICE, SFTP_SERVICE];
    let mut seen = fan_out(&services, services.len(), |service| {
        browse_one(sys, service, wait)
    })
    .into_iter();
    let ssh = seen.next().unwrap_or_else(|| Ok(Vec::new()))?;
    let rfb = seen.next().unwrap_or_else(|| Ok(Vec::new()))?;
    let (sftp, sftp_failed) = match seen.next().unwrap_or_else(|| Ok(Vec::new())) {
        Ok(sftp) => (sftp, None),
        Err(why) => (Vec::new(), Some(why)),
    };

    let budget = wait.min(RESOLVE_BUDGET);
    let resolved = fan_out(&ssh, RESOLVE_PARALLEL, |inst| {
        resolve(sys, &inst.name, SSH_SERVICE, budget)
    });
    let mut out = Browse {
        sftp_failed,
        ..Browse::default()
    };
    for (inst, targets) in ssh.iter().zip(resolved) {
        if targets.is_empty() {
            out.unresolved.push(inst.name.clone());
        }
        for (host, port) in targets {
            let duplicate = out
                .found
                .iter()
                .any(|f| f.address.eq_ignore_ascii_case(&host) && f.port == port);
            if !duplicate {
                out.found.push(Found {
                    name: inst.name.clone(),
                    address: host,
                    port,
                    ssh: true,
                    screen_sharing: false,
                    sftp: sftp.iter().any(|s| s.name == inst.name),
                    source: Source::Bonjour,
                });
            }
        }
    }

    let mut lone: Vec<&Instance> = Vec::new();
    for inst in &rfb {
        let mut matched = false;
        for row in out.found.iter_mut().filter(|f| f.name == inst.name) {
            row.screen_sharing = true;
            matched = true;
        }
        if !matched {
            lone.push(inst);
        }
    }
    let resolved = fan_out(&lone, RESOLVE_PARALLEL, |inst| {
        resolve(sys, &inst.name, RFB_SERVICE, budget)
    });
    for (inst, targets) in lone.iter().zip(resolved) {
        if targets.is_empty() {
            out.unresolved.push(inst.name.clone());
        }
        for (host, port) in targets {
            if let Some(row) = out
                .found
                .iter_mut()
                .find(|f| f.address.eq_ignore_ascii_case(&host))
            {
                row.screen_sharing = true;
            } else {
                out.found.push(Found {
                    name: inst.name.clone(),
                    address: host,
                    port,
                    ssh: false,
                    screen_sharing: true,
                    sftp: false,
                    source: Source::Bonjour,
                });
            }
        }
    }
    out.found.sort_by_key(|f| f.name.to_lowercase());
    Ok(out)
}

/// One browse of `service` in `local.`, kept to `wait`, as [`instances`].
fn browse_one(sys: &dyn System, service: &str, wait: Duration) -> Result<Vec<Instance>, String> {
    let heard = sys
        .bonjour(&Bonjour::Browse(service.to_owned()), wait)
        .map_err(|why| format!("Bonjour browse of {service}: {why}"))?;
    Ok(instances(&heard, service))
}

/// Whether two service types name one (`_ssh._tcp` and `_ssh._tcp.`).
fn same_service(a: &str, b: &str) -> bool {
    a.trim_end_matches('.') == b.trim_end_matches('.')
}

/// The instances of `service` a browse heard (`heard`, in order), first seen first, each merged
/// across the interfaces it is advertised on: one withdrawn on every interface is gone.
#[must_use]
pub fn instances(heard: &[Event], service: &str) -> Vec<Instance> {
    let mut instances: Vec<Instance> = Vec::new();
    for event in heard {
        let (seen, add) = match event {
            Event::Added(seen) => (seen, true),
            Event::Removed(seen) => (seen, false),
            _ => continue,
        };
        if seen.name.is_empty() || !same_service(&seen.service, service) {
            continue;
        }
        let at = instances.iter().position(|i| i.name == seen.name);
        match (at, add) {
            (Some(i), true) => {
                if !instances[i].interfaces.contains(&seen.interface) {
                    instances[i].interfaces.push(seen.interface);
                }
            }
            (None, true) => instances.push(Instance {
                name: seen.name.clone(),
                interfaces: vec![seen.interface],
            }),
            (Some(i), false) => instances[i].interfaces.retain(|&x| x != seen.interface),
            (None, false) => {}
        }
    }
    instances.retain(|i| !i.interfaces.is_empty());
    for inst in &mut instances {
        inst.interfaces.sort_unstable();
    }
    instances
}

/// Resolve the instance `name` of `service` in `local.` within `budget`, as [`targets`]; a
/// resolve that cannot run locates nothing.
#[must_use]
pub fn resolve(
    sys: &dyn System,
    name: &str,
    service: &str,
    budget: Duration,
) -> Vec<(String, u16)> {
    let ask = Bonjour::Resolve {
        name: name.to_owned(),
        service: service.to_owned(),
    };
    sys.bonjour(&ask, budget)
        .map(|heard| targets(&heard))
        .unwrap_or_default()
}

/// The distinct `(host, port)` targets a resolve heard, each host without its trailing dot; a
/// host that is no plain DNS name, or port 0, is none.
#[must_use]
pub fn targets(heard: &[Event]) -> Vec<(String, u16)> {
    let mut targets: Vec<(String, u16)> = Vec::new();
    for event in heard {
        let Event::Resolved { host, port, .. } = event else {
            continue;
        };
        let host = host.trim_end_matches('.');
        let plain = !host.is_empty()
            && !host.starts_with(['.', '-'])
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        if plain
            && *port != 0
            && !targets
                .iter()
                .any(|(h, p)| h.eq_ignore_ascii_case(host) && p == port)
        {
            targets.push((host.to_owned(), *port));
        }
    }
    targets
}

/// What one TCP connect learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// It accepted the connection.
    Open,
    /// It refused it: a Mac is there, its Remote Login is off.
    Closed,
    /// Nothing answered in time (or the host is down, or no route reaches it).
    NoAnswer,
    /// No LAN broker to connect through (its words).
    NoBroker(String),
}

/// One bounded TCP connect; the seam that keeps the network out of tests.
pub trait Knock: Sync {
    /// Connect to `addr`, giving up after `timeout`.
    fn knock(&self, addr: SocketAddr, timeout: Duration) -> Answer;
}

/// The live connect, through the LAN broker on macOS (`aterm_uds::lan::connect`), closed again
/// at once.
#[derive(Debug, Clone, Copy, Default)]
pub struct TcpKnock;

impl Knock for TcpKnock {
    fn knock(&self, addr: SocketAddr, timeout: Duration) -> Answer {
        let dialled = aterm_uds::lan::connect(
            &aterm_uds::lan::Target::Addr(addr),
            addr.port(),
            timeout,
            aterm_uds::lan::ConnectOptions::default(),
        );
        match dialled {
            Ok(_) => Answer::Open,
            Err(LanError::Unavailable(why)) => Answer::NoBroker(why.to_string()),
            Err(LanError::Io(e)) if e.kind() == io::ErrorKind::ConnectionRefused => Answer::Closed,
            Err(LanError::Io(_)) => Answer::NoAnswer,
        }
    }
}

/// Host IPv4 addresses of `cidr` (`a.b.c.d/n`, `n` from [`WIDEST_PREFIX`] to 32).
///
/// # Errors
/// One line: not an IPv4 CIDR, or wider than a /22.
pub fn hosts_of(cidr: &str) -> Result<Vec<Ipv4Addr>, String> {
    let (addr, prefix) = cidr
        .split_once('/')
        .ok_or_else(|| format!("{cidr:?} is not a subnet (a.b.c.d/n)"))?;
    let addr: Ipv4Addr = addr
        .parse()
        .map_err(|_| format!("{cidr:?}: {addr:?} is not an IPv4 address"))?;
    let prefix: u8 = prefix
        .parse()
        .ok()
        .filter(|p| !prefix.starts_with('+') && *p <= 32)
        .ok_or_else(|| format!("{cidr:?}: /{prefix} is not a prefix length"))?;
    if prefix < WIDEST_PREFIX {
        return Err(format!(
            "{cidr}: a sweep covers a /{WIDEST_PREFIX} at most (1024 addresses); name a \
             narrower subnet"
        ));
    }
    let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
    let network = u32::from(addr) & mask;
    let size = 1_u32 << (32 - u32::from(prefix));
    let (first, last) = if prefix <= 30 {
        (network + 1, network + (size - 2))
    } else {
        (network, network + (size - 1))
    };
    Ok((first..=last).map(Ipv4Addr::from).collect())
}

/// Sweep `cidr`'s port 22 with `knock` and return the addresses that accepted.
///
/// # Errors
/// One line: a bad or too wide `cidr`, or no LAN broker to connect through (its words, which
/// name what installs it), not a subnet of Macs that are all missing.
pub fn subnet(knock: &dyn Knock, cidr: &str, timeout: Duration) -> Result<Vec<Found>, String> {
    let hosts = hosts_of(cidr)?;
    let answers = fan_out(&hosts, KNOCK_PARALLEL, |ip| {
        knock.knock(SocketAddr::V4(SocketAddrV4::new(*ip, 22)), timeout)
    });
    if let Some(why) = answers.iter().find_map(|a| match a {
        Answer::NoBroker(why) => Some(why),
        _ => None,
    }) {
        return Err(format!("{cidr} was not swept: {why}"));
    }
    Ok(hosts
        .iter()
        .zip(answers)
        .filter(|(_, a)| *a == Answer::Open)
        .map(|(ip, _)| Found {
            name: ip.to_string(),
            address: ip.to_string(),
            port: 22,
            ssh: true,
            screen_sharing: false,
            sftp: false,
            source: Source::Subnet,
        })
        .collect())
}

/// Match `found`'s offered SSH host key against `known_hosts` and the inventory.
#[must_use]
pub fn pinned_host(
    sys: &dyn System,
    paths: &Paths,
    inv: &Inventory,
    found: &Found,
) -> Read<String> {
    match offered_keys(sys, paths, found) {
        Ok(offered) => pinned_name(sys, paths, inv, &offered),
        Err(why) => Read::Unusable(why),
    }
}

/// How long [`offered_keys`] gives a Mac a scan found to show its host key.
pub const OFFERED_WITHIN: Duration = Duration::from_secs(3);

/// The ed25519 key blobs `found` presents ([`super::lan_proxy::scan`] within
/// [`OFFERED_WITHIN`], recorded under the controller's folder only while it is read).
///
/// # Errors
/// One line: Remote Login is off, the address is not one, the record could not be kept, or no
/// key came back.
pub fn offered_keys(sys: &dyn System, paths: &Paths, found: &Found) -> Result<Vec<String>, String> {
    if !found.ssh {
        return Err("Remote Login is not on, so it shows no host key".into());
    }
    if found.address.is_empty() || found.address.starts_with('-') {
        return Err(format!("{:?} is not an address", found.address));
    }
    let dir = super::lan_proxy::scan_dir(sys, paths)?;
    let (recorded, ran) =
        super::lan_proxy::scan(sys, &dir, &found.address, found.port, OFFERED_WITHIN)?;
    let offered = host_keys(&recorded);
    if offered.is_empty() {
        return Err(format!(
            "no host key from {}:{} ({})",
            found.address,
            found.port,
            ran.describe()
        ));
    }
    Ok(offered)
}

/// Which inventory host pins one of the `offered` key blobs.
#[must_use]
pub fn pinned_name(
    sys: &dyn System,
    paths: &Paths,
    inv: &Inventory,
    offered: &[String],
) -> Read<String> {
    let pins = match sys.read_file(&paths.known_hosts()) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Read::Absent,
        Err(e) => return Read::Unusable(format!("{}: {e}", paths.known_hosts().display())),
    };
    for (patterns, key) in host_keys_with_patterns(&pins) {
        if !offered.contains(&key) {
            continue;
        }
        for pattern in patterns.split(',') {
            let alias = pattern
                .strip_prefix('[')
                .and_then(|p| p.split_once("]:"))
                .map_or(pattern, |(name, _)| name);
            if let Some(host) = inv
                .hosts
                .iter()
                .find(|h| h.uuid.eq_ignore_ascii_case(alias))
            {
                return Read::Value(host.name.clone());
            }
        }
    }
    Read::Absent
}

/// The ed25519 key blobs in known_hosts lines (`<host> ssh-ed25519 <base64>`, as the scan
/// records them; `#` lines are notes).
fn host_keys(text: &str) -> Vec<String> {
    host_keys_with_patterns(text)
        .into_iter()
        .map(|(_, key)| key)
        .collect()
}

/// `(host patterns, ed25519 key blob)` for each key line of a known_hosts-shaped text;
/// comments and `@` marker lines skipped.
fn host_keys_with_patterns(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with(['#', '@']))
        .filter_map(|l| {
            let mut fields = l.split_whitespace();
            let (hosts, kind, blob) = (fields.next()?, fields.next()?, fields.next()?);
            (kind == "ssh-ed25519").then(|| (hosts.to_owned(), blob.to_owned()))
        })
        .collect()
}

#[cfg(test)]
#[path = "discover_tests.rs"]
pub(crate) mod tests;
