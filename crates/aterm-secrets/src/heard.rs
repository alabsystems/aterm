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
#[path = "heard_tests.rs"]
mod tests;
