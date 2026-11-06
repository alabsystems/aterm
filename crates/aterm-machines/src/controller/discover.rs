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
pub(crate) mod tests {
    use super::*;
    use crate::port::{Fake, Ran};
    use std::collections::BTreeMap;

    /// A browse heard `name` of `service` come on `interface`.
    fn added(name: &str, service: &str, interface: u32) -> Event {
        Event::Added(heard(name, service, interface))
    }

    /// A browse heard `name` of `service` go on `interface`.
    fn removed(name: &str, service: &str, interface: u32) -> Event {
        Event::Removed(heard(name, service, interface))
    }

    fn heard(name: &str, service: &str, interface: u32) -> aterm_uds::lan::Instance {
        aterm_uds::lan::Instance {
            name: name.to_owned(),
            service: format!("{service}."),
            domain: "local.".to_owned(),
            interface,
        }
    }

    /// A resolve heard `host:port` on interface 5.
    fn resolved(host: &str, port: u16) -> Event {
        Event::Resolved {
            host: host.to_owned(),
            port,
            txt: Vec::new(),
            interface: 5,
        }
    }

    const AIR: &str = "m14 Air M5 32GB 4TB prime";
    const VM: &str = "Managed\u{2019}s Virtual Machine";
    const FIXTURE: &str = "Caf\u{e9} Lab\u{2019}s Mac 7 (fixture)";

    /// A browse of `_ssh._tcp` in the VM (captured with `dns-sd -B` on 16 Oct 2026): this Mac
    /// on interface 5 (en0), the VM itself on 1 (lo0) and 5.
    fn browse_ssh() -> Vec<Event> {
        vec![
            added(AIR, SSH_SERVICE, 5),
            added(VM, SSH_SERVICE, 1),
            added(VM, SSH_SERVICE, 5),
        ]
    }

    /// The same browse while a service was registered and then withdrawn: its comings and
    /// goings on both interfaces.
    fn browse_ssh_add_rmv() -> Vec<Event> {
        let mut heard = browse_ssh();
        heard.extend([
            added(FIXTURE, SSH_SERVICE, 1),
            added(FIXTURE, SSH_SERVICE, 5),
            removed(FIXTURE, SSH_SERVICE, 1),
            removed(FIXTURE, SSH_SERVICE, 5),
        ]);
        heard
    }

    /// A browse of `_rfb._tcp` in the VM: only the VM advertises Screen Sharing.
    fn browse_rfb() -> Vec<Event> {
        vec![added(VM, RFB_SERVICE, 1), added(VM, RFB_SERVICE, 5)]
    }

    /// A later session in the VM (macOS 27.0, then named lab-02), while a host with Remote
    /// Login alone was advertised (`spark-1`), as a Linux box's Avahi does.
    fn browse_ssh_lab() -> Vec<Event> {
        vec![
            added("lab-02", SSH_SERVICE, 1),
            added("lab-02", SSH_SERVICE, 5),
            added("spark-1", SSH_SERVICE, 1),
            added("spark-1", SSH_SERVICE, 5),
            added(AIR, SSH_SERVICE, 5),
        ]
    }

    fn browse_sftp_lab() -> Vec<Event> {
        vec![
            added("lab-02", SFTP_SERVICE, 1),
            added("lab-02", SFTP_SERVICE, 5),
            added(AIR, SFTP_SERVICE, 5),
        ]
    }

    fn browse_rfb_lab() -> Vec<Event> {
        vec![
            added("lab-02", RFB_SERVICE, 1),
            added("lab-02", RFB_SERVICE, 5),
        ]
    }

    fn browsing(service: &str) -> Bonjour {
        Bonjour::Browse(service.to_owned())
    }

    fn resolving(name: &str, service: &str) -> Bonjour {
        Bonjour::Resolve {
            name: name.to_owned(),
            service: service.to_owned(),
        }
    }

    #[test]
    fn an_instance_merges_the_interfaces_it_was_heard_on() {
        assert_eq!(
            instances(&browse_ssh(), SSH_SERVICE),
            [
                Instance {
                    name: AIR.into(),
                    interfaces: vec![5],
                },
                Instance {
                    name: VM.into(),
                    interfaces: vec![1, 5],
                },
            ]
        );
    }

    #[test]
    fn an_instance_withdrawn_on_every_interface_is_gone() {
        let found = instances(&browse_ssh_add_rmv(), SSH_SERVICE);
        let names: Vec<&str> = found.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, [AIR, VM]);
        let mut half = browse_ssh_add_rmv();
        half.pop();
        let found = instances(&half, SSH_SERVICE);
        let fixture = found.iter().find(|i| i.name == FIXTURE).unwrap();
        assert_eq!(fixture.interfaces, [5]);
    }

    #[test]
    fn an_event_of_another_service_or_kind_is_not_an_instance() {
        assert!(instances(&browse_rfb(), SSH_SERVICE).is_empty());
        assert_eq!(instances(&browse_rfb(), RFB_SERVICE).len(), 1);
        assert!(instances(&[resolved("lab-02.local.", 22)], SSH_SERVICE).is_empty());
        assert!(instances(&[], SSH_SERVICE).is_empty());
    }

    #[test]
    fn a_resolve_names_each_target_once_without_its_trailing_dot() {
        assert_eq!(
            targets(&[resolved("m14-Air-M5-32GB-4TB-prime.local.", 22)]),
            [("m14-Air-M5-32GB-4TB-prime.local".to_string(), 22)]
        );
        assert_eq!(
            targets(&[
                resolved("Manageds-Virtual-Machine.local.", 22),
                resolved("Manageds-Virtual-Machine.local.", 22),
            ]),
            [("Manageds-Virtual-Machine.local".to_string(), 22)]
        );
        assert!(targets(&[resolved("-x.local.", 22), resolved("a.local.", 0)]).is_empty());
        assert!(targets(&[]).is_empty());
    }

    /// The fake answering the VM's session (the SFTP browse is a later session's, when the VM
    /// was named lab-02: only this Mac's row there matches a row here).
    fn vm(browse_ssh: Vec<Event>) -> Fake {
        let fake = Fake::root();
        fake.on_bonjour(&browsing(SSH_SERVICE), Ok(browse_ssh));
        fake.on_bonjour(&browsing(RFB_SERVICE), Ok(browse_rfb()));
        fake.on_bonjour(&browsing(SFTP_SERVICE), Ok(browse_sftp_lab()));
        fake.on_bonjour(
            &resolving(AIR, SSH_SERVICE),
            Ok(vec![resolved("m14-Air-M5-32GB-4TB-prime.local.", 22)]),
        );
        fake.on_bonjour(
            &resolving(VM, SSH_SERVICE),
            Ok(vec![
                resolved("Manageds-Virtual-Machine.local.", 22),
                resolved("Manageds-Virtual-Machine.local.", 22),
            ]),
        );
        fake.on_bonjour(
            &resolving(VM, RFB_SERVICE),
            Ok(vec![resolved("Manageds-Virtual-Machine.local.", 5900)]),
        );
        fake
    }

    #[test]
    fn a_browse_finds_one_row_per_mac_and_marks_screen_sharing() {
        let fake = vm(browse_ssh());
        let got = browse(&fake, Duration::from_secs(3)).unwrap();
        assert_eq!(
            got.found,
            [
                Found {
                    name: AIR.into(),
                    address: "m14-Air-M5-32GB-4TB-prime.local".into(),
                    port: 22,
                    ssh: true,
                    screen_sharing: false,
                    sftp: true,
                    source: Source::Bonjour,
                },
                Found {
                    name: VM.into(),
                    address: "Manageds-Virtual-Machine.local".into(),
                    port: 22,
                    ssh: true,
                    screen_sharing: true,
                    sftp: false,
                    source: Source::Bonjour,
                },
            ]
        );
        assert!(got.unresolved.is_empty());
        let calls = fake.calls();
        assert_eq!(calls.len(), 5, "{calls:?}");
        assert!(
            calls.iter().all(|c| c.starts_with("bonjour ")),
            "every Bonjour request goes through the port: {calls:?}"
        );
        assert!(!calls.contains(&resolving(VM, RFB_SERVICE).key()));
    }

    /// Fake system fixture for the lab-02 session.
    pub(crate) fn lab_session() -> Fake {
        lab_session_with(Ok(browse_sftp_lab()))
    }

    /// [`lab_session`] with the SFTP browse answering `sftp`.
    pub(crate) fn lab_session_with(sftp: Result<Vec<Event>, String>) -> Fake {
        let fake = Fake::root();
        fake.on_bonjour(&browsing(SSH_SERVICE), Ok(browse_ssh_lab()));
        fake.on_bonjour(&browsing(SFTP_SERVICE), sftp);
        fake.on_bonjour(&browsing(RFB_SERVICE), Ok(browse_rfb_lab()));
        fake.on_bonjour(
            &resolving("lab-02", SSH_SERVICE),
            Ok(vec![
                resolved("lab-02.local.", 22),
                resolved("lab-02.local.", 22),
            ]),
        );
        fake.on_bonjour(
            &resolving("spark-1", SSH_SERVICE),
            Ok(vec![resolved("spark-1.local.", 22)]),
        );
        fake.on_bonjour(
            &resolving(AIR, SSH_SERVICE),
            Ok(vec![resolved("m14-Air-M5-32GB-4TB-prime.local.", 22)]),
        );
        fake
    }

    #[test]
    fn a_mac_shows_macos_sftp_beside_remote_login_and_an_ssh_only_box_does_not() {
        let got = browse(&lab_session(), Duration::from_secs(3)).unwrap();
        let flags: Vec<(&str, bool, bool, bool)> = got
            .found
            .iter()
            .map(|f| (f.address.as_str(), f.ssh, f.sftp, f.screen_sharing))
            .collect();
        assert_eq!(
            flags,
            [
                ("lab-02.local", true, true, true),
                ("m14-Air-M5-32GB-4TB-prime.local", true, true, false),
                ("spark-1.local", true, false, false),
            ]
        );
    }

    #[test]
    fn a_failed_sftp_browse_is_noted_and_the_scan_goes_on() {
        let fake = lab_session_with(Err("Bonjour error -65537".to_owned()));
        let got = browse(&fake, Duration::from_secs(3)).unwrap();
        assert_eq!(got.found.len(), 3);
        assert!(got.found.iter().all(|f| !f.sftp));
        assert!(got.sftp_failed.unwrap().contains("-65537"));
    }

    #[test]
    fn a_mac_with_only_screen_sharing_is_a_row_without_ssh() {
        let got = browse(
            &vm(vec![added(AIR, SSH_SERVICE, 5)]),
            Duration::from_secs(3),
        )
        .unwrap();
        let vm_row = got
            .found
            .iter()
            .find(|f| f.name.starts_with("Managed"))
            .unwrap();
        assert!(!vm_row.ssh);
        assert!(vm_row.screen_sharing);
        assert_eq!(vm_row.port, 5900);
    }

    #[test]
    fn an_instance_no_resolve_locates_is_listed_not_dropped() {
        let lost = Fake::root();
        lost.on_bonjour(&browsing(SSH_SERVICE), Ok(browse_ssh_add_rmv()));
        lost.on_bonjour(
            &resolving(AIR, SSH_SERVICE),
            Ok(vec![resolved("m14-Air-M5-32GB-4TB-prime.local.", 22)]),
        );
        let got = browse(&lost, Duration::from_secs(3)).unwrap();
        assert_eq!(got.found.len(), 1);
        assert_eq!(got.unresolved, [VM]);
    }

    /// A browse that cannot run (no LAN broker, or Bonjour's own error) fails the scan with its
    /// words; one whose instance names look like error codes is still a browse.
    #[test]
    fn a_browse_that_cannot_run_fails_the_scan() {
        let fake = Fake::root();
        fake.on_bonjour(
            &browsing(SSH_SERVICE),
            Err("Bonjour error -65537".to_owned()),
        );
        let err = browse(&fake, Duration::from_secs(1)).unwrap_err();
        assert!(err.contains("-65537"), "{err}");
        let absent = Fake::root();
        let why = aterm_uds::lan::Unavailable::NotInstalled.to_string();
        absent.on_bonjour(&browsing(SSH_SERVICE), Err(why.clone()));
        let err = browse(&absent, Duration::from_secs(1)).unwrap_err();
        assert!(err.ends_with(&why), "{err}");
        let named = Fake::root();
        let mut heard = browse_ssh();
        heard.push(added("Lab -65570", SSH_SERVICE, 5));
        named.on_bonjour(&browsing(SSH_SERVICE), Ok(heard));
        assert!(browse(&named, Duration::from_secs(1)).is_ok());
    }

    /// Every accepted CIDR's endpoints are representable, including the
    /// ranges ending at IPv4's maximum.
    #[test]
    fn a_subnet_ending_at_ipv4_max_never_overflows_its_endpoint() {
        let ordinary = hosts_of("10.0.0.0/24").expect("ordinary /24");
        assert_eq!(ordinary.len(), 254);
        assert_eq!(ordinary.first(), Some(&Ipv4Addr::new(10, 0, 0, 1)));
        assert_eq!(ordinary.last(), Some(&Ipv4Addr::new(10, 0, 0, 254)));
        assert_eq!(
            hosts_of("10.0.0.7/32").expect("single host"),
            [Ipv4Addr::new(10, 0, 0, 7)]
        );

        let observed: Vec<_> = [
            (
                "255.255.255.0/24",
                254,
                Ipv4Addr::new(255, 255, 255, 1),
                Ipv4Addr::new(255, 255, 255, 254),
            ),
            (
                "255.255.255.255/32",
                1,
                Ipv4Addr::BROADCAST,
                Ipv4Addr::BROADCAST,
            ),
        ]
        .into_iter()
        .map(|(cidr, count, first, last)| {
            let correct = std::panic::catch_unwind(|| hosts_of(cidr)).is_ok_and(|got| {
                got.is_ok_and(|hosts| {
                    hosts.len() == count
                        && hosts.first() == Some(&first)
                        && hosts.last() == Some(&last)
                })
            });
            (cidr, correct)
        })
        .collect();
        assert!(
            observed.iter().all(|(_, correct)| *correct),
            "valid CIDR endpoints must survive checked arithmetic: {observed:?}"
        );
    }

    #[test]
    fn a_subnet_is_its_host_addresses_and_no_wider_than_a_slash_22() {
        let hosts = hosts_of("192.168.64.0/24").unwrap();
        assert_eq!(hosts.len(), 254);
        assert_eq!(hosts[0], Ipv4Addr::new(192, 168, 64, 1));
        assert_eq!(hosts[253], Ipv4Addr::new(192, 168, 64, 254));
        assert_eq!(hosts_of("10.0.4.77/22").unwrap().len(), 1022);
        assert_eq!(
            hosts_of("10.0.0.5/32").unwrap(),
            [Ipv4Addr::new(10, 0, 0, 5)]
        );
        assert_eq!(hosts_of("10.0.0.4/31").unwrap().len(), 2);
        assert!(hosts_of("10.0.0.0/21").unwrap_err().contains("/22"));
        assert!(hosts_of("10.0.0.0").is_err());
        assert!(hosts_of("10.0.0.0/33").is_err());
        assert!(hosts_of("lab/24").is_err());
    }

    /// Answers by address; everything else is silent.
    struct Scripted(BTreeMap<Ipv4Addr, Answer>, Answer);

    impl Knock for Scripted {
        fn knock(&self, addr: SocketAddr, _timeout: Duration) -> Answer {
            match addr {
                SocketAddr::V4(v4) if v4.port() == 22 => self
                    .0
                    .get(v4.ip())
                    .cloned()
                    .unwrap_or_else(|| self.1.clone()),
                _ => panic!("knocked {addr}, not port 22"),
            }
        }
    }

    #[test]
    fn a_sweep_lists_the_addresses_whose_port_22_accepted_in_order() {
        let knock = Scripted(
            BTreeMap::from([
                (Ipv4Addr::new(192, 168, 64, 9), Answer::Open),
                (Ipv4Addr::new(192, 168, 64, 2), Answer::Open),
                (Ipv4Addr::new(192, 168, 64, 3), Answer::Closed),
            ]),
            Answer::NoAnswer,
        );
        let found = subnet(&knock, "192.168.64.0/24", Duration::from_millis(1)).unwrap();
        let addrs: Vec<&str> = found.iter().map(|f| f.address.as_str()).collect();
        assert_eq!(addrs, ["192.168.64.2", "192.168.64.9"]);
        assert!(
            found
                .iter()
                .all(|f| f.ssh && f.port == 22 && f.source == Source::Subnet)
        );
    }

    /// Without a LAN broker to connect through, a sweep is not run, naming the broker, rather
    /// than reading as a subnet with no Macs. NEGATIVE CONTROL: a subnet nothing answers on is
    /// swept, and empty.
    #[test]
    fn a_sweep_with_no_lan_broker_names_it() {
        let why = aterm_uds::lan::Unavailable::NotInstalled.to_string();
        let knock = Scripted(BTreeMap::new(), Answer::NoBroker(why.clone()));
        let err = subnet(&knock, "192.168.64.0/28", Duration::from_millis(1)).unwrap_err();
        assert_eq!(err, format!("192.168.64.0/28 was not swept: {why}"));
        let knock = Scripted(
            BTreeMap::from([(Ipv4Addr::new(192, 168, 64, 3), Answer::Closed)]),
            Answer::NoAnswer,
        );
        assert!(
            subnet(&knock, "192.168.64.0/28", Duration::from_millis(1))
                .unwrap()
                .is_empty()
        );
    }

    /// The VM's host key blob, as `ssh-keyscan -t ed25519` read it there.
    const VM_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIHv2Xf9kgkNkpV3WltnLoVx25AQOEx+mch1DHT/oZdSa";

    const VM_UUID: &str = "0A1B2C3D-0000-4000-8000-000000000002";

    fn vm_row() -> Found {
        Found {
            name: "Managed\u{2019}s Virtual Machine".into(),
            address: "Manageds-Virtual-Machine.local".into(),
            port: 22,
            ssh: true,
            screen_sharing: true,
            sftp: true,
            source: Source::Bonjour,
        }
    }

    fn lab() -> (Paths, Inventory) {
        let paths = Paths {
            root: std::path::PathBuf::from("/ctl/machines"),
        };
        let inv = crate::controller::inventory::parse(&format!(
            "schema = 1\n[[host]]\nname = \"lab-vm\"\nuuid = \"{VM_UUID}\"\naddress = \"192.168.64.2\"\nuser = \"admin\"\n"
        ))
        .unwrap();
        (paths, inv)
    }

    /// `fake` answers the scan of the VM ([`super::super::lan_proxy::scanned`]): `key`
    /// recorded, or nothing, and `ssh` ending as `ran`.
    pub(crate) fn vm_scanned(fake: &Fake, key: Option<&str>, ran: Ran) {
        super::super::lan_proxy::scanned(
            fake,
            std::path::Path::new("/ctl/machines/scan"),
            "Manageds-Virtual-Machine.local",
            22,
            OFFERED_WITHIN,
            key,
            ran,
        );
    }

    /// How `ssh` ends a scan whose key exchange recorded the key: no method authenticates.
    fn denied() -> Ran {
        Ran::exit(
            255,
            "hostkey@host: Permission denied (publickey,password).\n",
        )
    }

    #[test]
    fn a_mac_whose_key_is_pinned_under_an_inventory_uuid_is_that_host() {
        let (paths, inv) = lab();
        let fake = Fake::root();
        vm_scanned(&fake, Some(VM_KEY), denied());
        fake.file(
            "/ctl/machines/known_hosts",
            format!(
                "{} ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHv2Xf9kgkNkpV3WltnLoVx25AQOEx+mch1DHT/oZdSa\n",
                VM_UUID.to_ascii_lowercase()
            )
            .as_bytes(),
        );
        assert_eq!(
            pinned_host(&fake, &paths, &inv, &vm_row()),
            Read::Value("lab-vm".into())
        );
    }

    #[test]
    fn a_mac_whose_key_matches_no_pin_is_new_even_at_an_enrolled_address() {
        let (paths, inv) = lab();
        let fake = Fake::root();
        vm_scanned(&fake, Some(VM_KEY), denied());
        fake.file(
            "/ctl/machines/known_hosts",
            format!("{VM_UUID} ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOtherKeyOtherKeyOtherKeyOtherKeyOtherKey\n")
                .as_bytes(),
        );
        assert_eq!(pinned_host(&fake, &paths, &inv, &vm_row()), Read::Absent);
        let fresh = Fake::root();
        vm_scanned(&fresh, Some(VM_KEY), denied());
        assert_eq!(pinned_host(&fresh, &paths, &inv, &vm_row()), Read::Absent);
    }

    #[test]
    fn a_mac_that_shows_no_key_is_unknown_never_new() {
        let (paths, inv) = lab();
        let fake = Fake::root();
        vm_scanned(&fake, None, Ran::exit(255, ""));
        assert!(matches!(
            pinned_host(&fake, &paths, &inv, &vm_row()),
            Read::Unusable(_)
        ));
        let mut screen_only = vm_row();
        screen_only.ssh = false;
        assert!(matches!(
            pinned_host(&fake, &paths, &inv, &screen_only),
            Read::Unusable(_)
        ));
    }

    #[test]
    fn a_resolve_that_cannot_run_locates_nothing() {
        let fake = Fake::root();
        assert!(resolve(&fake, "x", SSH_SERVICE, Duration::from_millis(1)).is_empty());
    }

    /// A browse instance whose name looks like a target never supplies a socket address.
    /// Only a resolved event supplies the target, independently of the instance's free text.
    #[test]
    fn an_instance_name_that_spells_the_marker_does_not_choose_the_target() {
        let spoofed = " 0:00:00.001  Lab can be reached at decoy.local.:2222 (interface 1)._ssh._tcp.local. can be reached at real.local.:22 (interface 14)\n";
        assert_eq!(
            targets(&[added(spoofed, SSH_SERVICE, 14), resolved("real.local", 22)]),
            vec![("real.local".into(), 22)]
        );
    }
}
