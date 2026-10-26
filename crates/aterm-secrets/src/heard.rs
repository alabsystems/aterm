// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the last look for the ALab vault heard on this network, kept so the next look on it
//! is skipped (`~/.config/aterm/secrets/probe`): one line, `<unix seconds> <found|elsewhere>
//! <network>`. The word `absent` is what builds that also kept a look that heard no definitive
//! answer wrote; such a record is not read.
//!
//! A look that heard the vault stands for [`FOUND_FOR`], one that heard another vault's signed
//! envelope at its address for [`ABSENT_FOR`], each only on the network it was made on
//! ([`network`]). A look that heard nothing definitive (a timeout, a refused connect, a server
//! holding no vault yet, an answer that is no vault) is not kept. A join, a sync and a configured Mac never read it: it only spares an
//! unconfigured Mac the look.

use crate::client::Paths;
use std::net::IpAddr;

/// The record's file name in [`Paths::dir`].
pub const FILE: &str = "probe";

/// How long a look that heard the vault stands.
pub const FOUND_FOR: u64 = 10 * 60;

/// How long a look that heard another vault answer at the ALab vault's address stands on its
/// network.
pub const ABSENT_FOR: u64 = 24 * 60 * 60;

/// What a look heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heard {
    /// The ALab vault answered under the key built into aterm.
    Found,
    /// Another vault answered at the ALab vault's address, under a key aterm does not pin.
    Absent,
}

/// One kept look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// When it was made (unix seconds).
    pub at: u64,
    /// What it heard.
    pub heard: Heard,
    /// The network it was made on ([`network`]).
    pub network: String,
}

impl Verdict {
    /// What this look still says at `now` on `network`: nothing once it is older than its kind
    /// stands for, dated after `now`, or made on another network.
    #[must_use]
    pub fn standing(&self, network: &str, now: u64) -> Option<Heard> {
        let stands = match self.heard {
            Heard::Found => FOUND_FOR,
            Heard::Absent => ABSENT_FOR,
        };
        (self.network == network && self.at <= now && now - self.at < stands).then_some(self.heard)
    }

    fn line(&self) -> String {
        let heard = match self.heard {
            Heard::Found => "found",
            Heard::Absent => "elsewhere",
        };
        format!("{} {heard} {}\n", self.at, self.network)
    }

    fn parse(text: &str) -> Option<Self> {
        let mut words = text.trim().splitn(3, ' ');
        let at = words.next()?.parse().ok()?;
        let heard = match words.next()? {
            "found" => Heard::Found,
            "elsewhere" => Heard::Absent,
            // `absent` (an older build's) may be a timeout kept as no vault: not read.
            _ => return None,
        };
        let network = words.next()?.to_owned();
        (!network.is_empty() && !network.contains(char::is_whitespace)).then_some(Self {
            at,
            heard,
            network,
        })
    }
}

/// The network this Mac is on now: the IPv4 subnets of its interfaces that are up (`a.b.c.d/n`,
/// sorted, comma-separated), loopback and link-local left out, read without a packet; `None`
/// with none, when there is no network to keep a look for.
#[must_use]
pub fn network() -> Option<String> {
    network_of(&crate::network::interfaces().ok()?)
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

    #[test]
    fn a_kept_look_stands_for_its_kind_and_only_on_its_network() {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        assert_eq!(read(&paths), None);
        let absent = Verdict {
            at: 1_000,
            heard: Heard::Absent,
            network: "192.168.4.0/24".into(),
        };
        write(&paths, &absent).unwrap();
        let kept = read(&paths).expect("kept");
        assert_eq!(kept, absent);
        let here = "192.168.4.0/24";
        assert_eq!(
            kept.standing(here, 1_000 + ABSENT_FOR - 1),
            Some(Heard::Absent)
        );
        assert_eq!(kept.standing(here, 1_000 + ABSENT_FOR), None);
        assert_eq!(kept.standing("10.0.0.0/8", 1_001), None);
        assert_eq!(kept.standing(here, 999), None, "a look from the future");
        let found = Verdict {
            heard: Heard::Found,
            ..absent
        };
        assert_eq!(
            found.standing(here, 1_000 + FOUND_FOR - 1),
            Some(Heard::Found)
        );
        assert_eq!(found.standing(here, 1_000 + FOUND_FOR), None);
        std::fs::write(paths.dir.join(FILE), b"1000 maybe 192.168.4.0/24\n").unwrap();
        assert_eq!(read(&paths), None, "an unknown word keeps nothing");
        std::fs::write(paths.dir.join(FILE), b"1000 absent 192.168.4.0/24\n").unwrap();
        assert_eq!(
            read(&paths),
            None,
            "an older build's failed look is not read"
        );
    }
}
