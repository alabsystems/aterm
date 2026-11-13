// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Whose an mDNS advert `lab mac nux` hears is: this Mac's own, or another's.
//!
//! A guest on tart's bridge hears this Mac's own registrations, and this Mac's browse lists them
//! on that bridge too: mDNSResponder makes an unscoped registration on every interface it runs
//! on, so tart's `bridge1NN` is added once a guest brings it up. On m22 (2026-10-29), the ALab
//! vault its `aterm secrets serve --advertise alab-secrets` holds (`dns-sd -P alab-secrets
//! _alab-vault._tcp local 7484 alab-secrets.local 192.168.1.242`, no `-i`) was announced over
//! bridge100 a second after the bridge came up. The guest then heard one `_alab-vault._tcp`
//! advert and this Mac listed `alab-secrets on bridge100`, though no guest process registered
//! one. The instance's name and host target (`alab-secrets.local`) do not tell the two apart,
//! since a guest's vault would register the same ones. So an advert is placed by where its host
//! target resolves on the interface it came over (`dns-sd -i <index> -L`, then `dns-sd -i <index>
//! -G v4v6` of each host it names; [`resolve`]). It is this Mac's own when every address is one
//! of this Mac's (its LAN address, its end of the bridge for a registration scoped there, or an
//! IPv6 address of an interface of its). Otherwise it is another's ([`whose`]). An advert that
//! resolved to nothing is another's too, so one nobody could place still fails a row.
//!
//! Both families are asked because one can be missing where the other answers: in E3's second
//! first-launch run (2026-11-05), the guest's `-G v4 m22.local` over the bridge heard only
//! mDNSResponder's negative answer (`0.0.0.0 … No Such Record`), which the v4 reader took for an
//! address, and m22's own member advert failed the row as another's. A negative answer names no
//! address ([`addresses`]).

use std::net::IpAddr;

use crate::Fail;

/// How long each resolve listens (`-L`, then `-G v4v6` of each host), in the guest and on this
/// Mac.
/// This Mac and a guest on its bridge answer within a second, and the browse that heard the
/// advert has usually cached the answer already.
pub const RESOLVE_SECS: u64 = 2;

/// One advert a `dns-sd -B` transcript heard (an `Add` row): its instance, and the index of the
/// interface it came over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heard {
    pub instance: String,
    pub interface: u32,
}

/// Each advert a `dns-sd -B` transcript heard (its `Add` rows), once per instance and interface,
/// in the order heard.
#[must_use]
pub fn heard(transcript: &str) -> Vec<Heard> {
    let mut out: Vec<Heard> = Vec::new();
    for line in transcript.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.get(1) != Some(&"Add") {
            continue;
        }
        let Some(interface) = f.get(3).and_then(|i| i.parse().ok()) else {
            continue;
        };
        let Some(name) = f.get(6..).filter(|name| !name.is_empty()) else {
            continue;
        };
        let advert = Heard {
            instance: name.join(" "),
            interface,
        };
        if !out.contains(&advert) {
            out.push(advert);
        }
    }
    out
}

pub use super::dns_sd_text::{addresses, reach_hosts};

/// Where one advert's host targets resolved: each host it named, with its addresses.
pub type Resolved = Vec<(String, Vec<IpAddr>)>;

/// Whose an advert is ([`whose`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Whose {
    /// This Mac's own: it named a host, each host resolved, and every address is this Mac's.
    ThisMac,
    /// Another's: where its hosts resolved (`alab-secrets.local 192.168.64.3`), or that none did.
    Another(String),
}

/// Whose an advert whose hosts resolved to `resolved` is, given this Mac's own addresses `own`.
/// It is this Mac's only when it named a host, each host resolved, and every address is in `own`.
/// Otherwise it is another's, an advert that resolved to nothing included.
#[must_use]
pub fn whose(resolved: &[(String, Vec<IpAddr>)], own: &[IpAddr]) -> Whose {
    let mine = !resolved.is_empty()
        && resolved.iter().all(|(_, addresses)| {
            !addresses.is_empty() && addresses.iter().all(|address| own.contains(address))
        });
    if mine {
        return Whose::ThisMac;
    }
    if resolved.is_empty() {
        return Whose::Another("its host did not resolve".to_owned());
    }
    Whose::Another(
        resolved
            .iter()
            .map(|(host, addresses)| {
                if addresses.is_empty() {
                    format!("{host} unresolved")
                } else {
                    let addresses: Vec<String> =
                        addresses.iter().map(ToString::to_string).collect();
                    format!("{host} {}", addresses.join(" "))
                }
            })
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// One `dns-sd` run of the arguments it is given, for [`RESOLVE_SECS`]: its transcript.
pub type DnsSd<'a> = dyn FnMut(&[String]) -> Result<String, Fail> + 'a;

/// Resolve `heard` of `service` through `dns_sd`, on the interface it came over:
/// `-i <interface> -L <instance> <service> local.`, then `-i <interface> -G v4v6 <host>` for each
/// host that names ([`reach_hosts`], [`addresses`]).
///
/// # Errors
/// A `dns-sd` run could not be made, or the run was stopped.
pub fn resolve(heard: &Heard, service: &str, dns_sd: &mut DnsSd<'_>) -> Result<Resolved, Fail> {
    let at = heard.interface.to_string();
    let lookup = dns_sd(&[
        "-i".to_owned(),
        at.clone(),
        "-L".to_owned(),
        heard.instance.clone(),
        service.to_owned(),
        "local.".to_owned(),
    ])?;
    let mut out = Vec::new();
    for host in reach_hosts(&lookup) {
        let addresses = addresses(&dns_sd(&[
            "-i".to_owned(),
            at.clone(),
            "-G".to_owned(),
            "v4v6".to_owned(),
            host.clone(),
        ])?);
        out.push((host, addresses));
    }
    Ok(out)
}

/// The adverts heard, by whose they are ([`split`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Split {
    /// Each advert that is not this Mac's own, as `<instance> on <interface> (<where it
    /// resolved>)`. Each one fails a row.
    pub others: Vec<String>,
    /// Each advert that is this Mac's own, as `<instance> heard over <interface>`. The row names
    /// these, and they never fail it.
    pub this_macs: Vec<String>,
}

impl Split {
    /// This Mac's own adverts, for a row's cell: `this Mac's own alab-secrets heard over
    /// bridge100`, or "" when there are none.
    #[must_use]
    pub fn this_macs_text(&self) -> String {
        if self.this_macs.is_empty() {
            String::new()
        } else {
            format!("this Mac's own {}", self.this_macs.join(", "))
        }
    }
}

/// Place each advert in `heard` of `service` ([`resolve`] through `dns_sd`, then [`whose`]
/// against this Mac's own addresses `own`). `name_of` names an interface by its index.
///
/// # Errors
/// A `dns-sd` run could not be made, or the run was stopped.
pub fn split(
    heard: &[Heard],
    service: &str,
    name_of: &dyn Fn(u32) -> Option<String>,
    own: &[IpAddr],
    dns_sd: &mut DnsSd<'_>,
) -> Result<Split, Fail> {
    let mut out = Split::default();
    for (advert, placed) in place(heard, service, own, dns_sd)? {
        let over =
            name_of(advert.interface).unwrap_or_else(|| format!("interface {}", advert.interface));
        match placed {
            Whose::ThisMac => out
                .this_macs
                .push(format!("{} heard over {over}", advert.instance)),
            Whose::Another(at) => out
                .others
                .push(format!("{} on {over} ({at})", advert.instance)),
        }
    }
    Ok(out)
}

/// Each advert in `heard` of `service` with whose it is ([`resolve`] through `dns_sd`, then
/// [`whose`] against `own`), in the order heard. With `own` a guest's addresses (`vault-phrase`'s
/// browse of its lab members service), [`Whose::ThisMac`] reads "the guest's own". An advert
/// whose lookup named no host says what that lookup heard ([`lookup_words`]), so the row it fails
/// carries the evidence: E3's mdns-silent row (2026-11-05) failed on this Mac's own vault advert
/// over tart's bridge with a lookup that answered nothing within the hang detector, and the run's
/// transcripts went with its folder.
///
/// # Errors
/// A `dns-sd` run could not be made, or the run was stopped.
pub fn place(
    heard: &[Heard],
    service: &str,
    own: &[IpAddr],
    dns_sd: &mut DnsSd<'_>,
) -> Result<Vec<(Heard, Whose)>, Fail> {
    heard
        .iter()
        .map(|advert| {
            let mut lookup = String::new();
            let resolved = resolve(advert, service, &mut |args: &[String]| {
                let transcript = dns_sd(args)?;
                if args.iter().any(|a| a == "-L") {
                    lookup.clone_from(&transcript);
                }
                Ok(transcript)
            })?;
            let placed = match whose(&resolved, own) {
                Whose::Another(why) if resolved.is_empty() => {
                    Whose::Another(format!("{why}; its lookup heard {}", lookup_words(&lookup)))
                }
                placed => placed,
            };
            Ok((advert.clone(), placed))
        })
        .collect()
}

/// What a `dns-sd -L` transcript heard, for the row an unplaced advert fails ([`place`]): its
/// lines on one line and cut, or "nothing" for a transcript left empty.
fn lookup_words(transcript: &str) -> String {
    let said: Vec<&str> = transcript
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if said.is_empty() {
        return "nothing".to_owned();
    }
    super::squeeze(&said.join(" | "), 200)
}

/// This Mac's own addresses now, every interface's (its end of tart's bridge included while a
/// guest runs). An interface list that cannot be read gives none, so no advert is this Mac's.
#[must_use]
pub fn this_macs_addresses() -> Vec<IpAddr> {
    aterm_uds::network::interfaces()
        .unwrap_or_default()
        .into_iter()
        .map(|at| at.address)
        .collect()
}

#[cfg(test)]
#[path = "mac_nux_advert_tests.rs"]
mod tests;
