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
mod tests {
    use std::fmt::Write as _;
    use std::net::Ipv4Addr;

    use super::*;

    const SERVICE: &str = "_alab-vault._tcp";

    /// This Mac's addresses while a guest runs: loopback, its LAN address and its end of tart's
    /// bridge.
    fn own() -> Vec<IpAddr> {
        ["127.0.0.1", "192.168.1.242", "192.168.64.1"]
            .iter()
            .map(|a| a.parse().unwrap())
            .collect()
    }

    /// A `dns-sd` that answers `-L` with `host` reached on the interface asked, and `-G v4` with
    /// `addresses`, as `script -q -t 0` wrote the real ones on m22 (2026-10-29), `\r`s kept.
    fn answering(host: &str, addresses: &[&str]) -> impl FnMut(&[String]) -> Result<String, Fail> {
        let host = host.to_owned();
        let addresses: Vec<String> = addresses.iter().map(|a| (*a).to_owned()).collect();
        move |args: &[String]| {
            let at = args.get(1).cloned().unwrap_or_default();
            let named = args
                .get(3..)
                .and_then(|a| a.first())
                .cloned()
                .unwrap_or_default();
            let mut t = format!("\u{4}\u{8}\u{8}Using interface {at}\r\n");
            match args.get(2).map(String::as_str) {
                Some("-L") => {
                    let _ = write!(
                        t,
                        "Lookup {named}.{SERVICE}.local.\r\n 1:58:17.415  ...STARTING...\r\n \
                         1:58:17.416  {named}.{SERVICE}.local. can be reached at {host}.:7484 \
                         (interface {at})\r\n path=/v1\r\n"
                    );
                }
                Some("-G") => {
                    let host = args.get(4).cloned().unwrap_or_default();
                    t.push_str(
                        " 1:58:19.505  ...STARTING...\r\nTimestamp     A/R  Flags         IF  \
                         Hostname                Address          TTL\r\n",
                    );
                    for a in &addresses {
                        let _ = write!(
                            t,
                            " 1:58:19.505  Add  40000002      {at}  {host}.    {a}    240\r\n"
                        );
                    }
                }
                _ => {}
            }
            Ok(t)
        }
    }

    fn name_of(index: u32) -> Option<String> {
        (index == 25).then(|| "bridge100".to_owned())
    }

    /// `instance` heard over bridge100, placed against `own` with a `dns-sd` that resolves its
    /// host to `addresses`.
    fn placed(instance: &str, addresses: &[&str], own: &[IpAddr]) -> Split {
        let heard = [Heard {
            instance: instance.to_owned(),
            interface: 25,
        }];
        split(
            &heard,
            SERVICE,
            &name_of,
            own,
            &mut answering("alab-secrets.local", addresses),
        )
        .unwrap()
    }

    /// The vault this Mac serves, registered on every interface and so announced on tart's
    /// bridge, is this Mac's own: it resolves to this Mac's LAN address, or (registered on the
    /// bridge alone) to this Mac's end of it.
    #[test]
    fn this_macs_own_advert_heard_over_the_bridge_is_not_a_guests() {
        for address in ["192.168.1.242", "192.168.64.1"] {
            let split = placed("alab-secrets", &[address], &own());
            assert!(split.others.is_empty(), "{split:?}");
            assert_eq!(split.this_macs.len(), 1, "{split:?}");
            assert!(split.this_macs[0].contains("bridge100"), "{split:?}");
        }
        // NEGATIVE CONTROL: the same advert, on a Mac that holds neither address, is another's.
        let loopback = [IpAddr::V4(Ipv4Addr::LOCALHOST)];
        let split = placed("alab-secrets", &["192.168.1.242"], &loopback);
        assert_eq!((split.others.len(), split.this_macs.len()), (1, 0));
    }

    /// A guest's advert is still a guest's when its host resolves to the guest's address on the
    /// bridge, to nothing, or to this Mac's address and the guest's both (a guest claiming the
    /// same name).
    #[test]
    fn a_guests_advert_on_the_bridge_is_a_guests() {
        for addresses in [
            &["192.168.64.3"][..],
            &[][..],
            &["192.168.1.242", "192.168.64.3"][..],
        ] {
            let split = placed("alab-secrets", addresses, &own());
            assert!(split.this_macs.is_empty(), "{addresses:?}: {split:?}");
            assert_eq!(split.others.len(), 1, "{addresses:?}: {split:?}");
        }
        // NEGATIVE CONTROL: the guest's address taken as this Mac's makes the advert this Mac's,
        // so the guest's address is what placed it.
        let mut with_guest = own();
        with_guest.push("192.168.64.3".parse().unwrap());
        let split = placed("alab-secrets", &["192.168.64.3"], &with_guest);
        assert_eq!((split.others.len(), split.this_macs.len()), (0, 1));
    }

    /// An advert whose instance names no host (a resolve that heard nothing) is never this Mac's,
    /// and the row it fails says what its lookup heard, or that it heard nothing.
    /// NEGATIVE CONTROL: `whose` alone, the reader before, names no lookup.
    #[test]
    fn an_advert_that_resolves_to_no_host_is_another_s() {
        let heard = [Heard {
            instance: "nux lab".into(),
            interface: 25,
        }];
        let split = split(&heard, SERVICE, &name_of, &own(), &mut |_: &[String]| {
            Ok(String::new())
        })
        .unwrap();
        assert_eq!((split.others.len(), split.this_macs.len()), (1, 0));
        assert!(
            split.others[0].ends_with("(its host did not resolve; its lookup heard nothing)"),
            "{:?}",
            split.others
        );
        let started = "Lookup nux lab._alab-vault._tcp.local.\r\n 9:12:01.001  ...STARTING...\r\n";
        let started_only = super::split(
            &heard,
            SERVICE,
            &name_of,
            &own(),
            &mut |args: &[String]| {
                Ok(if args.iter().any(|a| a == "-L") {
                    started.to_owned()
                } else {
                    String::new()
                })
            },
        )
        .unwrap();
        assert!(
            started_only.others[0].contains(
                "its lookup heard Lookup nux lab._alab-vault._tcp.local. | 9:12:01.001  ...STARTING..."
            ),
            "{:?}",
            started_only.others
        );
        assert_eq!(
            whose(&[], &own()),
            Whose::Another("its host did not resolve".to_owned())
        );
        // NEGATIVE CONTROL: the same advert resolved to this Mac's address is this Mac's.
        let split = placed("nux lab", &["192.168.1.242"], &own());
        assert_eq!((split.others.len(), split.this_macs.len()), (0, 1));
    }

    /// Each resolve is asked on the interface the advert came over, the instance and host each
    /// passed as one argument.
    #[test]
    fn a_resolve_asks_on_the_interface_the_advert_came_over() {
        let mut asked: Vec<Vec<String>> = Vec::new();
        let mut answer = answering("nux.local", &["192.168.64.3"]);
        let heard = Heard {
            instance: "nux lab".into(),
            interface: 25,
        };
        let resolved = resolve(&heard, SERVICE, &mut |args: &[String]| {
            asked.push(args.to_vec());
            answer(args)
        })
        .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].1, ["192.168.64.3".parse::<IpAddr>().unwrap()]);
        assert_eq!(asked.len(), 2);
        assert!(asked.iter().all(|a| a[..2] == ["-i", "25"]), "{asked:?}");
        assert!(asked[0].contains(&heard.instance), "{asked:?}");
        assert!(asked[1].contains(&resolved[0].0), "{asked:?}");
    }

    /// A browse's adverts are its `Add` rows, once per instance and interface; a `-L` names its
    /// hosts and a `-G` its addresses, each once.
    #[test]
    fn transcripts_read_back_their_adverts_hosts_and_addresses() {
        let browse = "Browsing for _alab-vault._tcp.local.\r\n 1:52:19.169  ...STARTING...\r\n\
                      Timestamp     A/R    Flags  if Domain               Service Type         Instance Name\r\n \
                      1:52:19.170  Add        3   1 local.               _alab-vault._tcp.    alab-secrets\r\n \
                      1:52:19.170  Add        3  25 local.               _alab-vault._tcp.    nux lab\r\n \
                      1:52:19.171  Add        2  25 local.               _alab-vault._tcp.    nux lab\r\n \
                      1:52:19.172  Rmv        0  15 local.               _alab-vault._tcp.    gone\r\n";
        let adverts = heard(browse);
        assert_eq!(adverts.len(), 2, "{adverts:?}");
        assert!(
            adverts
                .iter()
                .any(|h| h.instance == "nux lab" && h.interface == 25)
        );
        assert!(!adverts.iter().any(|h| h.instance == "gone"));

        let args = |op: &str, named: &str| -> Vec<String> {
            ["-i", "15", op, named, "h.local"]
                .iter()
                .map(|a| (*a).to_owned())
                .collect()
        };
        let lookup = answering("Alab-Secrets.local", &[])(&args("-L", "x")).unwrap();
        assert_eq!(
            reach_hosts(&format!("{lookup}{lookup}")),
            ["alab-secrets.local"]
        );
        assert!(reach_hosts("Lookup x\r\n ...STARTING...\r\n").is_empty());

        let answered = answering("h.local", &["192.168.1.242", "192.168.1.242", "10.0.0.9"])(
            &args("-G", "v4v6"),
        )
        .unwrap();
        assert_eq!(addresses(&answered).len(), 2);
    }

    /// The negative answer `dns-sd -G` prints for a family the host has no record of
    /// (`addrinfo_reply`'s `No Such Record`, its address the zeroed sockaddr's), as the guest heard
    /// it of `m22.local` over tart's bridge (2026-11-05).
    const NEGATIVE_V4: &str = " 8:43:01.120  Add  40000002       6  m22.local.                             0.0.0.0                                      120   No Such Record\r\n";

    /// This Mac's own member advert, whose host answered the guest's v4 question with a negative
    /// answer and its v6 one with this Mac's link-local address, is this Mac's; the same answer
    /// on a Mac that holds no such address is another's.
    #[test]
    fn an_advert_placed_by_its_v6_address_alone_is_this_macs_only_when_that_address_is() {
        let resolved = vec![(
            "m22.local".to_owned(),
            addresses(&format!(
                "{NEGATIVE_V4} 8:43:01.121  Add  40000002       6  m22.local.     FE80:0000:0000:0000:1C92:EBFC:C6EB:C0EA%en0  120\r\n"
            )),
        )];
        let mut mine = own();
        mine.push("fe80::1c92:ebfc:c6eb:c0ea".parse().unwrap());
        assert_eq!(whose(&resolved, &mine), Whose::ThisMac);
        // NEGATIVE CONTROL: without that address among this Mac's, the advert is another's.
        assert_eq!(
            whose(&resolved, &own()),
            Whose::Another("m22.local fe80::1c92:ebfc:c6eb:c0ea".to_owned())
        );
        // A host that only answered no is unresolved, whatever this Mac holds.
        let negative = vec![("m22.local".to_owned(), addresses(NEGATIVE_V4))];
        assert_eq!(
            whose(&negative, &mine),
            Whose::Another("m22.local unresolved".to_owned())
        );
    }
}
