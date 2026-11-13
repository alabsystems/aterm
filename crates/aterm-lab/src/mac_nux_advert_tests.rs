// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

    let answered =
        answering("h.local", &["192.168.1.242", "192.168.1.242", "10.0.0.9"])(&args("-G", "v4v6"))
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
