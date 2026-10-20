// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm machines scan`: Macs on the LAN, from Bonjour (`_ssh._tcp`, `_sftp-ssh._tcp`,
//! `_rfb._tcp`) and an optional sweep of a subnet's port 22 (design §4.3).
//!
//! Bonjour comes from `/usr/bin/dns-sd`, which never exits on its own: each run is bounded
//! by the port's deadline and kept to it ([`Cmd::until_deadline`]), so what it printed by
//! then is the answer. A Mac advertises `_ssh._tcp` only when Remote Login was turned on in
//! System Settings (design §1.11); the sweep finds the others.
//!
//! The fixtures in the tests are `dns-sd` output captured in the VM (macOS 27.0): this
//! Mac's advertisement, the VM's own on two interfaces, and a service registered there
//! for the capture, whose name has spaces, an accent, a curly apostrophe and parentheses.

use super::inventory::{Inventory, Paths};
use super::ssh::{LOCAL_NETWORK_ROW, fan_out, is_local_address};
use crate::model::Read;
use crate::port::{Cmd, System};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream};
use std::time::Duration;

/// Apple's Bonjour client.
pub const DNS_SD: &str = "/usr/bin/dns-sd";

/// Remote Login's service type.
pub const SSH_SERVICE: &str = "_ssh._tcp";

/// Screen Sharing's service type.
pub const RFB_SERVICE: &str = "_rfb._tcp";

/// The SFTP service type macOS's Remote Login advertises beside [`SSH_SERVICE`], under the
/// same instance name (`/System/Library/LaunchDaemons/ssh.plist` lists both in its Bonjour
/// key; measured in the VM): what tells a Mac from another host advertising `_ssh._tcp` (a
/// Linux box's Avahi advertises that alone).
pub const SFTP_SERVICE: &str = "_sftp-ssh._tcp";

/// How long one `dns-sd -L` resolve runs. A resolve answers in milliseconds when the
/// instance is there (measured in the VM: under 2 ms); this bounds one that is not.
pub const RESOLVE_BUDGET: Duration = Duration::from_secs(2);

/// Resolves run at once. Each `dns-sd -L` runs its whole budget (it never exits), so a lab
/// of 65 Macs resolves in three rounds.
const RESOLVE_PARALLEL: usize = 32;

/// Apple's host-key scanner.
pub const SSH_KEYSCAN: &str = "/usr/bin/ssh-keyscan";

/// The widest subnet [`subnet`] sweeps: a /22, 1024 addresses.
pub const WIDEST_PREFIX: u8 = 22;

/// Connects a sweep has in flight at once.
const KNOCK_PARALLEL: usize = 64;

/// Bonjour's refusal of a process macOS's Local Network privacy has not allowed:
/// `kDNSServiceErr_PolicyDenied` in `dns_sd.h`. Not measured here (an SSH session is exempt
/// from Local Network privacy, so the VM cannot show it); read from Apple's header.
const POLICY_DENIED: &str = "-65570";

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
    /// Advertised instances no resolve answered for within [`RESOLVE_BUDGET`]: seen, but
    /// not located. Never dropped silently.
    pub unresolved: Vec<String>,
    /// Why the `_sftp-ssh._tcp` browse failed, when it did: then no row's `sftp` says
    /// anything, and `enroll --found` takes only the Macs Screen Sharing marks. The other two
    /// browses' failures fail the whole browse, as before.
    pub sftp_failed: Option<String>,
}

/// Browse `_ssh._tcp`, `_sftp-ssh._tcp` and `_rfb._tcp` for `wait`, then resolve each
/// instance.
///
/// A Mac that advertises Remote Login and Screen Sharing is one row (`ssh` and
/// `screen_sharing`); one that advertises only Screen Sharing is a row with `ssh: false`
/// (Remote Login off, or turned on without the Sharing pane, which does not advertise it).
/// `sftp` marks a Remote Login row whose instance also advertises `_sftp-ssh._tcp`, as
/// macOS's does; it needs no resolve of its own.
///
/// # Errors
/// One line when `dns-sd` could not run, or quit on its own with an error (Bonjour's
/// refusal of this process names Local Network privacy).
pub fn browse(sys: &dyn System, wait: Duration) -> Result<Browse, String> {
    let services = [SSH_SERVICE, RFB_SERVICE, SFTP_SERVICE];
    let mut seen = fan_out(&services, services.len(), |service| {
        browse_one(sys, service, wait)
    })
    .into_iter();
    let ssh = seen.next().unwrap_or_else(|| Ok(Vec::new()))?;
    let rfb = seen.next().unwrap_or_else(|| Ok(Vec::new()))?;
    // A browse aterm's Info.plist did not declare (an older build's) may be refused by Local
    // Network privacy on its own: `scan` goes on without it.
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

    // A Mac's two advertisements share its Computer Name; only a Screen-Sharing-only one
    // needs a resolve of its own.
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

/// One `dns-sd -B <service> local.` run, kept to `wait`, parsed.
fn browse_one(sys: &dyn System, service: &str, wait: Duration) -> Result<Vec<Instance>, String> {
    let cmd = Cmd::new(DNS_SD)
        .args(["-B", service, "local."])
        .budget(wait)
        .until_deadline();
    let ran = sys
        .run(&cmd)
        .map_err(|e| format!("dns-sd -B {service}: {e}"))?;
    let text = ran.out();
    let said = format!("{text}\n{}", ran.err());
    // dns-sd's own error line, never a row: an advertised name is any text another device
    // chose, `-65570` included.
    let denied = said
        .lines()
        .filter(|l| !is_browse_row(l))
        .any(|l| l.split_whitespace().any(|w| w == POLICY_DENIED));
    if denied {
        return Err(format!(
            "dns-sd -B {service}: Bonjour refused this process ({POLICY_DENIED}); \
             {LOCAL_NETWORK_ROW}"
        ));
    }
    // Killed at the deadline (`None`) is how a browse ends; an exit of its own is a failure.
    if ran.status.is_some_and(|s| s != 0) {
        return Err(format!("dns-sd -B {service}: {}", ran.describe()));
    }
    Ok(parse_browse(&text, service))
}

/// The instances a `dns-sd -B` transcript leaves advertised, in the order first seen.
///
/// Each row is `Timestamp A/R Flags if Domain Service-Type Instance-Name`; the instance name
/// is the rest of the line after the sixth column, as advertised (spaces, `’`, any Unicode).
/// `Add` and `Rmv` are per interface, so an instance is kept while any interface still has
/// it. Header lines, and rows for another service type, are not rows.
#[must_use]
pub fn parse_browse(text: &str, service: &str) -> Vec<Instance> {
    let want = format!("{service}.");
    let mut instances: Vec<Instance> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let Some((cols, name)) = columns(line, 6) else {
            continue;
        };
        let add = match cols[1] {
            "Add" => true,
            "Rmv" => false,
            _ => continue,
        };
        let (Ok(interface), true) = (cols[3].parse::<u32>(), cols[5] == want) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let at = instances.iter().position(|i| i.name == name);
        match (at, add) {
            (Some(i), true) => {
                if !instances[i].interfaces.contains(&interface) {
                    instances[i].interfaces.push(interface);
                }
            }
            (None, true) => instances.push(Instance {
                name: name.to_owned(),
                interfaces: vec![interface],
            }),
            (Some(i), false) => instances[i].interfaces.retain(|&x| x != interface),
            (None, false) => {}
        }
    }
    instances.retain(|i| !i.interfaces.is_empty());
    for inst in &mut instances {
        inst.interfaces.sort_unstable();
    }
    instances
}

/// Whether `line` is a `dns-sd -B` result row (`Add` or `Rmv` in its second column).
fn is_browse_row(line: &str) -> bool {
    columns(line, 6).is_some_and(|(cols, _)| matches!(cols[1], "Add" | "Rmv"))
}

/// The first `n` whitespace-separated columns of `line`, and the rest of it after them
/// (leading blanks dropped, inner ones kept).
fn columns(line: &str, n: usize) -> Option<(Vec<&str>, &str)> {
    let mut cols = Vec::with_capacity(n);
    let mut rest = line;
    for _ in 0..n {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace)?;
        cols.push(&rest[..end]);
        rest = &rest[end..];
    }
    Some((cols, rest.trim_start()))
}

/// Resolve one instance: `dns-sd -L <name> <service> local.`, kept to `budget`. The
/// `(host, port)` targets it answered, without the trailing dot, each once; none when it
/// did not answer in time or could not run.
#[must_use]
pub fn resolve(
    sys: &dyn System,
    name: &str,
    service: &str,
    budget: Duration,
) -> Vec<(String, u16)> {
    let cmd = Cmd::new(DNS_SD)
        .args(["-L", name, service, "local."])
        .budget(budget)
        .until_deadline();
    sys.run(&cmd)
        .map(|ran| parse_resolve(&ran.out()))
        .unwrap_or_default()
}

/// The targets in a `dns-sd -L` transcript: each `… can be reached at <host>.:<port>
/// (interface <n>)` line, once per distinct target.
#[must_use]
pub fn parse_resolve(text: &str) -> Vec<(String, u16)> {
    const REACHED: &str = " can be reached at ";
    let mut targets: Vec<(String, u16)> = Vec::new();
    for line in text.lines() {
        let Some(at) = line.find(REACHED) else {
            continue;
        };
        let rest = &line[at + REACHED.len()..];
        let hostport = rest.split(" (interface ").next().unwrap_or(rest).trim();
        let Some((host, port)) = hostport.rsplit_once(':') else {
            continue;
        };
        let host = host.trim_end_matches('.');
        let Ok(port) = port.parse::<u16>() else {
            continue;
        };
        let plain = !host.is_empty()
            && !host.starts_with('-')
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        if plain
            && !targets
                .iter()
                .any(|(h, p)| h.eq_ignore_ascii_case(host) && *p == port)
        {
            targets.push((host.to_owned(), port));
        }
    }
    targets
}

/// What one TCP connect learned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// It accepted the connection.
    Open,
    /// It refused it: a Mac is there, its Remote Login is off.
    Closed,
    /// Nothing answered in time (or the host is down).
    NoAnswer,
    /// `EHOSTUNREACH`: no route, which on-link is also how Local Network privacy refuses.
    NoRoute,
}

/// One bounded TCP connect; the seam that keeps the network out of tests.
pub trait Knock: Sync {
    /// Connect to `addr`, giving up after `timeout`.
    fn knock(&self, addr: SocketAddr, timeout: Duration) -> Answer;
}

/// The live connect: `TcpStream::connect_timeout`, closed again at once.
#[derive(Debug, Clone, Copy, Default)]
pub struct TcpKnock;

impl Knock for TcpKnock {
    fn knock(&self, addr: SocketAddr, timeout: Duration) -> Answer {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(_) => Answer::Open,
            Err(e) => match e.kind() {
                io::ErrorKind::ConnectionRefused => Answer::Closed,
                io::ErrorKind::HostUnreachable => Answer::NoRoute,
                _ => Answer::NoAnswer,
            },
        }
    }
}

/// The addresses of `cidr` (`a.b.c.d/n`, IPv4, `n` from [`WIDEST_PREFIX`] to 32): every
/// host address, without the network and broadcast addresses of a /30 or wider.
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
        .filter(|p| *p <= 32)
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

/// Sweep `cidr`'s port 22 with `knock`, `timeout` per address, and return the addresses
/// that accepted, in address order.
///
/// # Errors
/// One line: a bad or too wide `cidr`, or every address of a local subnet answering
/// `No route to host`, which is Local Network privacy refusing this process, not a
/// subnet of Macs that are all missing.
pub fn subnet(knock: &dyn Knock, cidr: &str, timeout: Duration) -> Result<Vec<Found>, String> {
    let hosts = hosts_of(cidr)?;
    let answers = fan_out(&hosts, KNOCK_PARALLEL, |ip| {
        knock.knock(SocketAddr::V4(SocketAddrV4::new(*ip, 22)), timeout)
    });
    let local = hosts
        .first()
        .is_some_and(|ip| is_local_address(&ip.to_string()));
    if local && !answers.is_empty() && answers.iter().all(|a| *a == Answer::NoRoute) {
        return Err(format!(
            "{LOCAL_NETWORK_ROW} (every address in {cidr} answered No route to host)"
        ));
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

/// Which inventory host `found` is, by its SSH host key: `Value(name)` when the ed25519 key
/// it presents is the one `known_hosts` pins under that host's UUID (`enrolled <name>`),
/// `Absent` when its key matches no pinned one (`new`), `Unusable` when its key could not be
/// read (no Remote Login, no answer). A name or address alone never marks a Mac enrolled:
/// another Mac can take either.
///
/// The mark is a hint, not a proof. `ssh-keyscan` prints the public key a host offers and
/// aborts the key exchange before the host signs anything, so a device that copied an
/// enrolled Mac's public key would show as that Mac here. Every connection the controller
/// makes (`ssh` under `StrictHostKeyChecking=yes`) checks the signature and refuses it.
#[must_use]
pub fn pinned_host(
    sys: &dyn System,
    paths: &Paths,
    inv: &Inventory,
    found: &Found,
) -> Read<String> {
    match offered_keys(sys, found) {
        Ok(offered) => pinned_name(sys, paths, inv, &offered),
        Err(why) => Read::Unusable(why),
    }
}

/// The ed25519 key blobs `found` presents (`ssh-keyscan`, three seconds).
///
/// # Errors
/// One line: Remote Login is off, the address is not one, or no key came back.
pub fn offered_keys(sys: &dyn System, found: &Found) -> Result<Vec<String>, String> {
    if !found.ssh {
        return Err("Remote Login is not on, so it shows no host key".into());
    }
    if found.address.is_empty() || found.address.starts_with('-') {
        return Err(format!("{:?} is not an address", found.address));
    }
    let cmd = Cmd::new(SSH_KEYSCAN)
        .args(["-T", "3", "-t", "ed25519", "-p"])
        .arg(found.port.to_string())
        .arg("--")
        .arg(&found.address)
        .budget(Duration::from_secs(10));
    let ran = sys.run(&cmd).map_err(|e| format!("ssh-keyscan: {e}"))?;
    let offered = host_keys(&ran.out());
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

/// Which inventory host pins one of the `offered` key blobs: `Value(name)`, `Absent` for
/// none, `Unusable` when `known_hosts` cannot be read.
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

/// The ed25519 key blobs in `ssh-keyscan` output (`<host> ssh-ed25519 <base64>` lines;
/// `#` lines are its banner notes).
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

    /// `dns-sd -B _ssh._tcp local.` in the VM, killed after four seconds: this Mac on
    /// interface 5 (en0), the VM itself on 1 (lo0) and 5.
    const BROWSE_SSH: &str = "Browsing for _ssh._tcp.local.
DATE: ---Fri 16 Oct 2026---
23:07:32.176  ...STARTING...
Timestamp     A/R    Flags  if Domain               Service Type         Instance Name
23:07:32.177  Add        3   5 local.               _ssh._tcp.           m14 Air M5 32GB 4TB prime
23:07:32.177  Add        3   1 local.               _ssh._tcp.           Managed\u{2019}s Virtual Machine
23:07:32.177  Add        2   5 local.               _ssh._tcp.           Managed\u{2019}s Virtual Machine
";

    /// The same browse while a service was registered (`dns-sd -R … 2222`) and then
    /// withdrawn: its `Add` and `Rmv` rows on both interfaces.
    const BROWSE_SSH_ADD_RMV: &str = "Browsing for _ssh._tcp.local.
DATE: ---Fri 16 Oct 2026---
23:22:36.989  ...STARTING...
Timestamp     A/R    Flags  if Domain               Service Type         Instance Name
23:22:36.989  Add        3   5 local.               _ssh._tcp.           m14 Air M5 32GB 4TB prime
23:22:36.989  Add        3   1 local.               _ssh._tcp.           Managed\u{2019}s Virtual Machine
23:22:36.989  Add        2   5 local.               _ssh._tcp.           Managed\u{2019}s Virtual Machine
23:22:38.919  Add        3   1 local.               _ssh._tcp.           Caf\u{e9} Lab\u{2019}s Mac 7 (fixture)
23:22:38.919  Add        2   5 local.               _ssh._tcp.           Caf\u{e9} Lab\u{2019}s Mac 7 (fixture)
23:22:43.243  Rmv        1   1 local.               _ssh._tcp.           Caf\u{e9} Lab\u{2019}s Mac 7 (fixture)
23:22:43.243  Rmv        0   5 local.               _ssh._tcp.           Caf\u{e9} Lab\u{2019}s Mac 7 (fixture)
";

    /// `dns-sd -B _rfb._tcp local.` in the VM: only the VM advertises Screen Sharing.
    const BROWSE_RFB: &str = "Browsing for _rfb._tcp.local.
DATE: ---Fri 16 Oct 2026---
23:07:43.772  ...STARTING...
Timestamp     A/R    Flags  if Domain               Service Type         Instance Name
23:07:43.772  Add        3   1 local.               _rfb._tcp.           Managed\u{2019}s Virtual Machine
23:07:43.772  Add        2   5 local.               _rfb._tcp.           Managed\u{2019}s Virtual Machine
";

    /// `dns-sd -L "m14 Air M5 32GB 4TB prime" _ssh._tcp local.` in the VM.
    const RESOLVE_AIR: &str = "Lookup m14 Air M5 32GB 4TB prime._ssh._tcp.local.
DATE: ---Fri 16 Oct 2026---
23:07:47.792  ...STARTING...
23:07:47.792  m14\\032Air\\032M5\\03232GB\\0324TB\\032prime._ssh._tcp.local. can be reached at m14-Air-M5-32GB-4TB-prime.local.:22 (interface 5)
";

    /// `dns-sd -L "Managed’s Virtual Machine" _ssh._tcp local.`: two answers, the first
    /// flagged more-coming.
    const RESOLVE_VM: &str = "Lookup Managed\u{2019}s Virtual Machine._ssh._tcp.local.
DATE: ---Fri 16 Oct 2026---
23:07:51.808  ...STARTING...
23:07:51.809  Managed\u{2019}s\\032Virtual\\032Machine._ssh._tcp.local. can be reached at Manageds-Virtual-Machine.local.:22 (interface 5) Flags: 1
23:07:51.809  Managed\u{2019}s\\032Virtual\\032Machine._ssh._tcp.local. can be reached at Manageds-Virtual-Machine.local.:22 (interface 5)
";

    /// `dns-sd -L "Managed’s Virtual Machine" _rfb._tcp local.`.
    const RESOLVE_VM_RFB: &str = "Lookup Managed\u{2019}s Virtual Machine._rfb._tcp.local.
DATE: ---Fri 16 Oct 2026---
23:11:19.410  ...STARTING...
23:11:19.411  Managed\u{2019}s\\032Virtual\\032Machine._rfb._tcp.local. can be reached at Manageds-Virtual-Machine.local.:5900 (interface 5) Flags: 1
23:11:19.411  Managed\u{2019}s\\032Virtual\\032Machine._rfb._tcp.local. can be reached at Manageds-Virtual-Machine.local.:5900 (interface 5)
";

    /// `dns-sd -L` of the registered fixture service.
    const RESOLVE_FIXTURE: &str = "Lookup Caf\u{e9} Lab\u{2019}s Mac 7 (fixture)._ssh._tcp.local.
DATE: ---Fri 16 Oct 2026---
23:22:40.020  ...STARTING...
23:22:40.022  Caf\u{e9}\\032Lab\u{2019}s\\032Mac\\0327\\032(fixture)._ssh._tcp.local. can be reached at Manageds-Virtual-Machine.local.:2222 (interface 5) Flags: 1
23:22:40.022  Caf\u{e9}\\032Lab\u{2019}s\\032Mac\\0327\\032(fixture)._ssh._tcp.local. can be reached at Manageds-Virtual-Machine.local.:2222 (interface 5)
";

    /// A resolve of a name nobody advertises, killed at its deadline: dns-sd flushes its
    /// header only with a result, so nothing at all was captured.
    const RESOLVE_NOBODY: &str = "";

    /// A later capture in the VM (macOS 27.0, then named lab-02), while `dns-sd -R spark-1
    /// _ssh._tcp local. 22` advertised a host with Remote Login alone, as a Linux box's Avahi
    /// does: `dns-sd -B _ssh._tcp local.`, then `_sftp-ssh._tcp` and `_rfb._tcp`. This Mac
    /// (m14) shows on interface 5 in the first two.
    const BROWSE_SSH_LAB: &str = "Browsing for _ssh._tcp.local.
DATE: ---Sun 18 Oct 2026---
 6:21:48.496  ...STARTING...
Timestamp     A/R    Flags  if Domain               Service Type         Instance Name
 6:21:48.497  Add        3   1 local.               _ssh._tcp.           lab-02
 6:21:48.497  Add        3   5 local.               _ssh._tcp.           lab-02
 6:21:48.497  Add        3   1 local.               _ssh._tcp.           spark-1
 6:21:48.497  Add        2   5 local.               _ssh._tcp.           spark-1
 6:21:48.702  Add        2   5 local.               _ssh._tcp.           m14 Air M5 32GB 4TB prime
";
    const BROWSE_SFTP_LAB: &str = "Browsing for _sftp-ssh._tcp.local.
DATE: ---Sun 18 Oct 2026---
 6:21:51.515  ...STARTING...
Timestamp     A/R    Flags  if Domain               Service Type         Instance Name
 6:21:51.516  Add        3   1 local.               _sftp-ssh._tcp.      lab-02
 6:21:51.516  Add        3   5 local.               _sftp-ssh._tcp.      lab-02
 6:21:51.516  Add        2   5 local.               _sftp-ssh._tcp.      m14 Air M5 32GB 4TB prime
";
    const BROWSE_RFB_LAB: &str = "Browsing for _rfb._tcp.local.
DATE: ---Sun 18 Oct 2026---
 6:21:54.535  ...STARTING...
Timestamp     A/R    Flags  if Domain               Service Type         Instance Name
 6:21:54.542  Add        3   1 local.               _rfb._tcp.           lab-02
 6:21:54.542  Add        2   5 local.               _rfb._tcp.           lab-02
";
    /// `dns-sd -L lab-02 _ssh._tcp local.` in that session.
    const RESOLVE_LAB: &str = "Lookup lab-02._ssh._tcp.local.
DATE: ---Sun 18 Oct 2026---
 6:21:59.636  ...STARTING...
 6:21:59.637  lab-02._ssh._tcp.local. can be reached at lab-02.local.:22 (interface 5) Flags: 1
 6:21:59.637  lab-02._ssh._tcp.local. can be reached at lab-02.local.:22 (interface 5)
";
    /// `dns-sd -L spark-1 _ssh._tcp local.` in that session, its target the box's own name
    /// (registered in the VM, it named the VM's, lab-02.local, which a Linux box would not).
    const RESOLVE_SPARK: &str = "Lookup spark-1._ssh._tcp.local.
DATE: ---Sun 18 Oct 2026---
 6:21:57.613  ...STARTING...
 6:21:57.613  spark-1._ssh._tcp.local. can be reached at spark-1.local.:22 (interface 5) Flags: 1
 6:21:57.613  spark-1._ssh._tcp.local. can be reached at spark-1.local.:22 (interface 5)
";

    fn killed(stdout: &str) -> Ran {
        Ran {
            status: None,
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn browse_key(service: &str) -> String {
        format!("/usr/bin/dns-sd -B {service} local.")
    }

    fn resolve_key(name: &str, service: &str) -> String {
        format!(
            "/usr/bin/dns-sd -L {} {service} local.",
            crate::port::shell_word(name)
        )
    }

    #[test]
    fn an_instance_name_is_the_rest_of_the_row_and_interfaces_merge() {
        let found = parse_browse(BROWSE_SSH, SSH_SERVICE);
        assert_eq!(
            found,
            [
                Instance {
                    name: "m14 Air M5 32GB 4TB prime".into(),
                    interfaces: vec![5],
                },
                Instance {
                    name: "Managed\u{2019}s Virtual Machine".into(),
                    interfaces: vec![1, 5],
                },
            ]
        );
    }

    #[test]
    fn an_instance_withdrawn_on_every_interface_is_gone() {
        let found = parse_browse(BROWSE_SSH_ADD_RMV, SSH_SERVICE);
        let names: Vec<&str> = found.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "m14 Air M5 32GB 4TB prime",
                "Managed\u{2019}s Virtual Machine"
            ]
        );
        // Withdrawn on one interface only, it stays on the other.
        let half = BROWSE_SSH_ADD_RMV
            .lines()
            .filter(|l| !l.contains("Rmv        0   5"))
            .collect::<Vec<_>>()
            .join("\n");
        let found = parse_browse(&half, SSH_SERVICE);
        let fixture = found
            .iter()
            .find(|i| i.name == "Caf\u{e9} Lab\u{2019}s Mac 7 (fixture)")
            .unwrap();
        assert_eq!(fixture.interfaces, [5]);
    }

    #[test]
    fn a_row_for_another_service_or_a_header_is_not_an_instance() {
        assert!(parse_browse(BROWSE_RFB, SSH_SERVICE).is_empty());
        assert_eq!(parse_browse(BROWSE_RFB, RFB_SERVICE).len(), 1);
        assert!(parse_browse("Timestamp     A/R    Flags  if Domain\n", SSH_SERVICE).is_empty());
        assert!(parse_browse("", SSH_SERVICE).is_empty());
    }

    #[test]
    fn a_resolve_names_each_target_once_without_its_trailing_dot() {
        assert_eq!(
            parse_resolve(RESOLVE_AIR),
            [("m14-Air-M5-32GB-4TB-prime.local".to_string(), 22)]
        );
        assert_eq!(
            parse_resolve(RESOLVE_VM),
            [("Manageds-Virtual-Machine.local".to_string(), 22)]
        );
        assert_eq!(
            parse_resolve(RESOLVE_FIXTURE),
            [("Manageds-Virtual-Machine.local".to_string(), 2222)]
        );
        assert!(parse_resolve(RESOLVE_NOBODY).is_empty());
    }

    /// The fake answering the VM's captures (the SFTP browse is a later session's, when the
    /// VM was named lab-02: only this Mac's row there matches a row here).
    fn vm(browse_ssh: &str) -> Fake {
        let fake = Fake::root();
        fake.on(&browse_key(SSH_SERVICE), killed(browse_ssh));
        fake.on(&browse_key(RFB_SERVICE), killed(BROWSE_RFB));
        fake.on(&browse_key(SFTP_SERVICE), killed(BROWSE_SFTP_LAB));
        fake.on(
            &resolve_key("m14 Air M5 32GB 4TB prime", SSH_SERVICE),
            killed(RESOLVE_AIR),
        );
        fake.on(
            &resolve_key("Managed\u{2019}s Virtual Machine", SSH_SERVICE),
            killed(RESOLVE_VM),
        );
        fake.on(
            &resolve_key("Managed\u{2019}s Virtual Machine", RFB_SERVICE),
            killed(RESOLVE_VM_RFB),
        );
        fake
    }

    #[test]
    fn a_browse_finds_one_row_per_mac_and_marks_screen_sharing() {
        let fake = vm(BROWSE_SSH);
        let got = browse(&fake, Duration::from_secs(3)).unwrap();
        assert_eq!(
            got.found,
            [
                Found {
                    name: "m14 Air M5 32GB 4TB prime".into(),
                    address: "m14-Air-M5-32GB-4TB-prime.local".into(),
                    port: 22,
                    ssh: true,
                    screen_sharing: false,
                    sftp: true,
                    source: Source::Bonjour,
                },
                Found {
                    name: "Managed\u{2019}s Virtual Machine".into(),
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
        // The three browses and one resolve per Remote Login Mac; Screen Sharing and SFTP on
        // the same Mac need none.
        let calls = fake.calls();
        assert_eq!(calls.len(), 5, "{calls:?}");
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("-L") && c.contains(RFB_SERVICE))
        );
    }

    /// The fake answering the lab-02 session's captures.
    pub(crate) fn lab_session() -> Fake {
        lab_session_with(killed(BROWSE_SFTP_LAB))
    }

    /// [`lab_session`] with the SFTP browse answering `sftp`.
    pub(crate) fn lab_session_with(sftp: Ran) -> Fake {
        let fake = Fake::root();
        fake.on(&browse_key(SSH_SERVICE), killed(BROWSE_SSH_LAB));
        fake.on(&browse_key(SFTP_SERVICE), sftp);
        fake.on(&browse_key(RFB_SERVICE), killed(BROWSE_RFB_LAB));
        fake.on(&resolve_key("lab-02", SSH_SERVICE), killed(RESOLVE_LAB));
        fake.on(&resolve_key("spark-1", SSH_SERVICE), killed(RESOLVE_SPARK));
        fake.on(
            &resolve_key("m14 Air M5 32GB 4TB prime", SSH_SERVICE),
            killed(RESOLVE_AIR),
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
        let fake = lab_session_with(Ran::exit(255, "DNSService call failed -65570"));
        let got = browse(&fake, Duration::from_secs(3)).unwrap();
        assert_eq!(got.found.len(), 3);
        assert!(got.found.iter().all(|f| !f.sftp));
        assert!(got.sftp_failed.unwrap().contains("-65570"));
    }

    #[test]
    fn a_mac_with_only_screen_sharing_is_a_row_without_ssh() {
        let only_air = BROWSE_SSH
            .lines()
            .filter(|l| !l.contains("Managed"))
            .collect::<Vec<_>>()
            .join("\n");
        let got = browse(&vm(&only_air), Duration::from_secs(3)).unwrap();
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
        lost.on(&browse_key(SSH_SERVICE), killed(BROWSE_SSH_ADD_RMV));
        lost.on(&browse_key(RFB_SERVICE), killed(""));
        lost.on(&browse_key(SFTP_SERVICE), killed(""));
        lost.on(
            &resolve_key("m14 Air M5 32GB 4TB prime", SSH_SERVICE),
            killed(RESOLVE_AIR),
        );
        lost.on(
            &resolve_key("Managed\u{2019}s Virtual Machine", SSH_SERVICE),
            killed(RESOLVE_NOBODY),
        );
        let got = browse(&lost, Duration::from_secs(3)).unwrap();
        assert_eq!(got.found.len(), 1);
        assert_eq!(got.unresolved, ["Managed\u{2019}s Virtual Machine"]);
    }

    #[test]
    fn browse_runs_dns_sd_by_its_absolute_path() {
        let fake = vm(BROWSE_SSH);
        browse(&fake, Duration::from_secs(3)).unwrap();
        for call in fake.calls() {
            assert!(call.starts_with("/usr/bin/dns-sd "), "{call}");
        }
    }

    #[test]
    fn a_dns_sd_that_quits_with_an_error_fails_the_browse() {
        let fake = Fake::root();
        fake.on(
            &browse_key(SSH_SERVICE),
            Ran::exit(255, "DNSService call failed -65537"),
        );
        fake.on(&browse_key(RFB_SERVICE), killed(BROWSE_RFB));
        fake.on(&browse_key(SFTP_SERVICE), killed(""));
        let err = browse(&fake, Duration::from_secs(1)).unwrap_err();
        assert!(err.contains("-65537"), "{err}");
        let denied = Fake::root();
        denied.on(
            &browse_key(SSH_SERVICE),
            Ran::exit(255, "DNSService call failed -65570"),
        );
        denied.on(&browse_key(RFB_SERVICE), killed(""));
        denied.on(&browse_key(SFTP_SERVICE), killed(""));
        let err = browse(&denied, Duration::from_secs(1)).unwrap_err();
        assert!(err.contains("Local Network"), "{err}");
        // A Mac named with that number is a Mac, not a refusal.
        let named = Fake::root();
        named.on(
            &browse_key(SSH_SERVICE),
            killed(&format!(
                "{BROWSE_SSH} 0:00:13.263  Add        3   5 local.               _ssh._tcp.           Lab -65570\n"
            )),
        );
        named.on(&browse_key(RFB_SERVICE), killed(""));
        named.on(&browse_key(SFTP_SERVICE), killed(""));
        assert!(browse(&named, Duration::from_secs(1)).is_ok());
        let missing = Fake::root();
        assert!(browse(&missing, Duration::from_secs(1)).is_err());
    }

    /// Every accepted CIDR's endpoints are representable, including the
    /// ranges ending at IPv4's maximum. No socket or Knock is involved.
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

        // Observe both checked-build panics before the aggregate assertion.
        // Unchecked wrapping happens to give the right endpoints; that is
        // not a reason for a valid argument to panic in a checked build.
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
                SocketAddr::V4(v4) if v4.port() == 22 => {
                    self.0.get(v4.ip()).copied().unwrap_or(self.1)
                }
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

    #[test]
    fn a_local_subnet_with_no_route_anywhere_is_local_network_privacy() {
        let knock = Scripted(BTreeMap::new(), Answer::NoRoute);
        let err = subnet(&knock, "192.168.64.0/28", Duration::from_millis(1)).unwrap_err();
        assert!(err.starts_with(LOCAL_NETWORK_ROW), "{err}");
        // One host answering at all means the process can reach the LAN.
        let knock = Scripted(
            BTreeMap::from([(Ipv4Addr::new(192, 168, 64, 3), Answer::Closed)]),
            Answer::NoRoute,
        );
        assert!(
            subnet(&knock, "192.168.64.0/28", Duration::from_millis(1))
                .unwrap()
                .is_empty()
        );
    }

    /// `ssh-keyscan -T 3 -t ed25519 -p 22 -- Manageds-Virtual-Machine.local` in the VM.
    const KEYSCAN_VM: &str = "# Manageds-Virtual-Machine.local:22 SSH-2.0-OpenSSH_10.3
manageds-virtual-machine.local ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHv2Xf9kgkNkpV3WltnLoVx25AQOEx+mch1DHT/oZdSa
";

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

    const KEYSCAN_KEY: &str =
        "/usr/bin/ssh-keyscan -T 3 -t ed25519 -p 22 -- Manageds-Virtual-Machine.local";

    #[test]
    fn a_mac_whose_key_is_pinned_under_an_inventory_uuid_is_that_host() {
        let (paths, inv) = lab();
        let fake = Fake::root();
        fake.on(KEYSCAN_KEY, Ran::ok(KEYSCAN_VM));
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
        fake.on(KEYSCAN_KEY, Ran::ok(KEYSCAN_VM));
        fake.file(
            "/ctl/machines/known_hosts",
            format!("{VM_UUID} ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOtherKeyOtherKeyOtherKeyOtherKeyOtherKey\n")
                .as_bytes(),
        );
        assert_eq!(pinned_host(&fake, &paths, &inv, &vm_row()), Read::Absent);
        // No known_hosts yet: nothing is pinned, so everything is new.
        let fresh = Fake::root();
        fresh.on(KEYSCAN_KEY, Ran::ok(KEYSCAN_VM));
        assert_eq!(pinned_host(&fresh, &paths, &inv, &vm_row()), Read::Absent);
    }

    #[test]
    fn a_mac_that_shows_no_key_is_unknown_never_new() {
        let (paths, inv) = lab();
        let fake = Fake::root();
        // Measured for a silent address: nothing printed, exit 1.
        fake.on(KEYSCAN_KEY, Ran::exit(1, ""));
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
}
