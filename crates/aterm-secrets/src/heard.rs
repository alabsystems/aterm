// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one discovery record (`~/.config/aterm/secrets/probe`): what the last definitive look for
//! the ALab vault heard, kept so the next look on the same network is spared. One line,
//! `<unix seconds> found <network> <url> <signer>` for the vault itself, or
//! `<unix seconds> elsewhere <network>` for another vault's signed envelope at its address. A
//! record this build does not understand (an older `absent`, or a line keyed by anything but the
//! network place's strong id) is dropped, not read.
//!
//! The network key ([`network_key`]) is the network place's id of the network this Mac's traffic
//! leaves on (`aterm_netplace`, the one reader of where this Mac is): its default gateway's
//! hardware address and its subnet, read with no packet. Two routers both numbering a LAN
//! `192.168.1.0/24` have different hardware addresses, so a fact kept against one never stands on
//! the other. With no gateway neighbour entry yet the key cannot be formed and nothing is kept.
//!
//! A look that heard the vault stands for [`FOUND_FOR`], one that heard another vault's signed
//! envelope at its address for [`ABSENT_FOR`], each only on the network it was made on. A look
//! that heard nothing definitive (a timeout, a refused connect, a 404, an answer that is no
//! vault, an unavailable LAN broker) is not kept. A network change (a new gateway) or
//! [`forget`] drops the record. The record never feeds a pin
//! or the ALab signal — only an anchor does (`docs/DESIGN-init-on-by-default-2026-10-25.md`
//! §8.2, §8.3); it is a hint that spares an unconfigured Mac a look during init's pinned fallback,
//! the window lane and the first-launch Secrets stage.

use crate::client::Paths;
use std::net::IpAddr;

/// The record's file name in [`Paths::dir`].
pub const FILE: &str = "probe";

/// How long a look that heard the vault stands.
pub const FOUND_FOR: u64 = 10 * 60;

/// How long a look that heard another vault answer at the ALab vault's address stands on its
/// network.
pub const ABSENT_FOR: u64 = 24 * 60 * 60;

/// What a definitive look heard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    /// The ALab vault answered under the key built into aterm, at `url`, its signer the given
    /// fingerprint. A hint for the Peers row and the join; it never pins the signer.
    Found {
        /// The address the vault answered at.
        url: String,
        /// Its signer's fingerprint ([`crate::envelope::fingerprint`]).
        signer: String,
    },
    /// Another vault answered at the ALab vault's address, under a key aterm does not pin.
    Absent,
}

/// One kept look ([`read`], [`write`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// When it was made (unix seconds).
    pub at: u64,
    /// What it heard.
    pub heard: Heard,
    /// The network it was made on ([`network_key`]).
    pub network: String,
}

impl Verdict {
    /// What this look still says at `now` on `network`: nothing once it is older than its kind
    /// stands for, dated after `now`, or made on another network (a different gateway, even one
    /// numbering the same subnet).
    #[must_use]
    pub fn standing(&self, network: &str, now: u64) -> Option<&Heard> {
        let stands = match self.heard {
            Heard::Found { .. } => FOUND_FOR,
            Heard::Absent => ABSENT_FOR,
        };
        (self.network == network && self.at <= now && now - self.at < stands).then_some(&self.heard)
    }

    fn line(&self) -> String {
        match &self.heard {
            Heard::Found { url, signer } => {
                format!("{} found {} {url} {signer}\n", self.at, self.network)
            }
            Heard::Absent => format!("{} elsewhere {}\n", self.at, self.network),
        }
    }

    fn parse(text: &str) -> Option<Self> {
        let mut words = text.split_whitespace();
        let at = words
            .next()
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))?
            .parse()
            .ok()?;
        let kind = words.next()?;
        let network = words.next()?.to_owned();
        let heard = match kind {
            "found" => Heard::Found {
                url: words.next()?.to_owned(),
                signer: words.next()?.to_owned(),
            },
            "elsewhere" => Heard::Absent,
            // `absent` (an older build's timeout kept as no vault) and any other word: not read.
            _ => return None,
        };
        // A record keyed by anything but the network place's strong id (a subnet alone, or an
        // earlier build's hardware address and subnets) names no network this build can tell: it
        // is dropped.
        (aterm_netplace::NetworkId::parse(&network).is_some_and(|id| id.is_strong())
            && words.next().is_none())
        .then_some(Self { at, heard, network })
    }
}

/// The network this Mac is on now, as a coarse subnet fingerprint for change detection: the IPv4
/// subnets of its interfaces that are up (`a.b.c.d/n`, sorted, comma-separated), loopback and
/// link-local left out, read without a packet; `None` with none. The discovery record is keyed
/// by [`network_key`], which is sharper; this fingerprint is only the watch's cue to look again.
#[must_use]
pub fn network() -> Option<String> {
    network_of(&crate::network::interfaces().ok()?)
}

/// The discovery record's network key: the network place's strong id of the network this Mac's
/// traffic leaves on (`aterm_netplace::facts::network_now`: the default gateway's hardware
/// address and the route's subnet), read with no packet. `None` with no IPv4 default route, or
/// where the gateway has no neighbour entry yet — the key cannot be formed, so the caller keeps
/// nothing and looks afresh.
#[must_use]
pub fn network_key() -> Option<String> {
    aterm_netplace::facts::network_now().map(|id| id.as_str().to_owned())
}

fn network_of(addresses: &[crate::network::InterfaceAddress]) -> Option<String> {
    let mut subnets: Vec<String> = addresses
        .iter()
        .filter_map(|address| match (address.address, address.netmask) {
            (IpAddr::V4(ip), Some(IpAddr::V4(mask)))
                if !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() =>
            {
                let network = std::net::Ipv4Addr::from(ip.to_bits() & mask.to_bits());
                Some(format!("{network}/{}", mask.to_bits().count_ones()))
            }
            _ => None,
        })
        .collect();
    subnets.sort_unstable();
    subnets.dedup();
    (!subnets.is_empty()).then(|| subnets.join(","))
}

/// The look kept under `paths`, when one is there and reads.
#[must_use]
pub fn read(paths: &Paths) -> Option<Verdict> {
    let bytes = crate::fsx::read_owned(&paths.dir.join(FILE), 512).ok()??;
    Verdict::parse(std::str::from_utf8(&bytes).ok()?)
}

/// Drop any kept look under `paths`. A network change (a new gateway) already strands a record
/// by its key; a caller that knows a verdict formed before now no longer stands calls this.
pub fn forget(paths: &Paths) {
    let _ = crate::fsx::wipe_and_remove(&paths.dir.join(FILE));
}

/// Keep `verdict` under `paths` (`0600`, in the `0700` secrets directory).
/// # Errors
/// The directory or the file could not be written.
pub fn write(paths: &Paths, verdict: &Verdict) -> crate::Result<()> {
    crate::fsx::ensure_private_dir(&paths.dir)?;
    crate::fsx::write_atomic(&paths.dir.join(FILE), verdict.line().as_bytes(), 0o600)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::InterfaceAddress;
    use std::net::Ipv4Addr;

    fn on(name: &str, ip: [u8; 4], mask: [u8; 4]) -> InterfaceAddress {
        InterfaceAddress {
            name: name.into(),
            address: Ipv4Addr::from(ip).into(),
            netmask: Some(Ipv4Addr::from(mask).into()),
            flags: 0,
        }
    }

    #[test]
    fn a_network_is_its_ipv4_subnets_without_loopback_or_link_local() {
        let lan = [
            on("en0", [192, 168, 4, 23], [255, 255, 255, 0]),
            on("lo0", [127, 0, 0, 1], [255, 0, 0, 0]),
            on("en5", [169, 254, 3, 9], [255, 255, 0, 0]),
            on("utun4", [100, 64, 0, 7], [255, 255, 255, 255]),
            on("en1", [192, 168, 4, 99], [255, 255, 255, 0]),
        ];
        assert_eq!(
            network_of(&lan).as_deref(),
            Some("100.64.0.7/32,192.168.4.0/24")
        );
        assert_eq!(
            network_of(&[on("lo0", [127, 0, 0, 1], [255, 0, 0, 0])]),
            None
        );
    }

    /// A route on `subnet`'s `.23` behind a gateway whose hardware address is `hw`.
    fn route(subnet: [u8; 3], hw: Option<&[u8]>) -> aterm_netplace::RouteFacts {
        aterm_netplace::RouteFacts {
            interface: "en0".to_owned(),
            address: Ipv4Addr::new(subnet[0], subnet[1], subnet[2], 23),
            prefix: 24,
            gateway: Some(Ipv4Addr::new(subnet[0], subnet[1], subnet[2], 1)),
            gateway_hw: hw.map(<[u8]>::to_vec),
        }
    }

    /// The network key is the network place's strong id of the route traffic takes, read from the
    /// one source of where this Mac is: a scripted route gives that route's id, a gateway whose
    /// hardware address is unread gives none, and no route gives none.
    /// NEGATIVE CONTROL: keying by the place's weak id where the strong one cannot be formed
    /// (`.or_else(|| routes_now().0.as_ref().map(NetworkId::weak))` in `network_now`) gives a key
    /// before the gateway is read, and "no key before ARP" fails.
    #[test]
    fn a_network_key_is_the_network_places_strong_id() {
        let hardware = [0xaa, 0xbb, 0xcc, 0x11, 0x22, 0x33];
        let script = |route: Option<aterm_netplace::RouteFacts>| aterm_netplace::facts::Script {
            facts: aterm_netplace::Facts {
                route,
                ..aterm_netplace::Facts::default()
            },
            via: Vec::new(),
        };
        let home = route([192, 168, 4], Some(&hardware));
        let key = aterm_netplace::facts::script_here(script(Some(home.clone())), network_key);
        assert_eq!(
            key,
            aterm_netplace::NetworkId::of(&home).map(|id| id.as_str().to_owned())
        );
        assert!(key.is_some());
        assert_eq!(
            aterm_netplace::facts::script_here(
                script(Some(route([192, 168, 4], None))),
                network_key
            ),
            None,
            "no key before ARP"
        );
        assert_eq!(
            aterm_netplace::facts::script_here(script(None), network_key),
            None
        );
    }

    #[test]
    fn a_kept_look_stands_for_its_kind_and_carries_what_it_found() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        assert_eq!(read(&paths), None);
        let here = "n00112233445566aa";
        let absent = Verdict {
            at: 1_000,
            heard: Heard::Absent,
            network: here.into(),
        };
        write(&paths, &absent).unwrap();
        let kept = read(&paths).expect("kept");
        assert_eq!(kept, absent);
        assert_eq!(
            kept.standing(here, 1_000 + ABSENT_FOR - 1),
            Some(&Heard::Absent)
        );
        assert_eq!(kept.standing(here, 1_000 + ABSENT_FOR), None);
        assert_eq!(kept.standing(here, 999), None, "a look from the future");
        // A Found look carries the vault's url and signer through the record file.
        let found = Verdict {
            at: 2_000,
            heard: Heard::Found {
                url: "https://vault.example/v1/alab".into(),
                signer: "abc123fingerprint".into(),
            },
            network: here.into(),
        };
        write(&paths, &found).unwrap();
        assert_eq!(
            read(&paths).as_ref(),
            Some(&found),
            "the url and signer survive the round trip"
        );
        assert_eq!(
            found.standing(here, 2_000 + FOUND_FOR - 1),
            Some(&found.heard)
        );
        assert_eq!(found.standing(here, 2_000 + FOUND_FOR), None);
    }

    #[test]
    fn an_absent_stands_on_its_gateway_not_on_a_twin_subnet() {
        let key = |hw: &[u8]| {
            aterm_netplace::NetworkId::of(&route([192, 168, 1], Some(hw)))
                .unwrap()
                .as_str()
                .to_owned()
        };
        let here = key(&[0xaa, 0xbb, 0xcc, 0, 0, 1]);
        let twin = key(&[0xde, 0xad, 0xbe, 0xef, 0, 2]);
        assert_ne!(
            here, twin,
            "two gateways numbering one subnet are two networks"
        );
        let absent = Verdict {
            at: 1_000,
            heard: Heard::Absent,
            network: here.clone(),
        };
        assert_eq!(absent.standing(&here, 1_001), Some(&Heard::Absent));
        // The negative control is a key of the subnet alone: there the stale Absent would stand on
        // the new network and block the vault. With the gateway in the key it does not.
        assert_eq!(absent.standing(&twin, 1_001), None);
    }

    #[test]
    fn forget_drops_the_kept_look() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        let here = "n00112233445566aa";
        write(
            &paths,
            &Verdict {
                at: 1_000,
                heard: Heard::Absent,
                network: here.into(),
            },
        )
        .unwrap();
        assert!(read(&paths).is_some());
        forget(&paths);
        assert_eq!(read(&paths), None, "a grant invalidated the record");
        forget(&paths); // Nothing to drop is not an error.
    }

    #[test]
    fn an_unreadable_or_unmigratable_record_is_dropped() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        let file = paths.dir.join(FILE);
        crate::fsx::ensure_private_dir(&paths.dir).unwrap();
        for line in [
            "1000 maybe n00112233445566aa\n",               // an unknown word
            "1000 absent n00112233445566aa\n",              // an older build's failed look
            "1000 elsewhere 192.168.4.0/24\n",              // a subnet-only key: dropped
            "1000 elsewhere aabbcc112233@192.168.4.0/24\n", // an earlier build's key: dropped
            "1000 elsewhere w00112233445566aa\n",           // a weak id (no gateway read): dropped
            "1000 found n00112233445566aa\n",               // Found missing its url and signer
            "1000 found n00112233445566aa https://v\n",     // Found missing its signer
        ] {
            std::fs::write(&file, line).unwrap();
            assert_eq!(read(&paths), None, "{line:?}");
        }
    }
}
