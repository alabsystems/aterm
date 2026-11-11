// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ALAB LAN BY PROOF, NOW, for a caller that must not take memory for proof: the vault's
//! automatic ALab LAN admission (`aterm_secrets::home_lan`). The place reads `alab-lan` for a
//! network this Mac remembers (members answered there once) or the fleet book seeds, which is
//! enough to word a report and never enough to let a Mac in.
//!
//! [`Now`] is what this Mac reads now, from the same files and the same Mac as the place: the
//! network its traffic leaves on, with the path this process read itself
//! ([`crate::facts::here_now`]: the Mac's own facts, a test's thread script, never the lab's seam
//! file); the current stay on it, as the keeper's record brought up to
//! those facts gives it (a record that lags a move, or none at all, opens a stay now, in which
//! nothing has answered yet); and the members' authenticated answers ([`crate::heard`]), each
//! with the dial's start, the local address and interface its route took, and when it came. The
//! rules a caller holds that to (which lane, how fresh, which interface, which gateway) stay the
//! caller's: nothing here decides, and nothing here writes.

use std::net::IpAddr;
use std::path::Path;

use crate::facts::Here;
#[cfg(test)]
use crate::place::step;
use crate::place::{Answer, Facts, NetworkId, observed_stay};

/// What this Mac reads of its network now ([the module](self)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Now {
    /// The route traffic leaves on (under a tunnel, the physical one), the tunnel holding the
    /// default route, and the path as this process read it now (`None`: not read).
    pub facts: Facts,
    /// The route's interface is point to point.
    pub point_to_point: bool,
    /// Every address this Mac holds, on any interface.
    pub own: Vec<IpAddr>,
    /// When the current stay began (unix ms); `None` with no stay.
    pub stay_since_ms: Option<u64>,
    /// The current stay's network: strong once its gateway's hardware address was read
    /// ([`NetworkId::of`]), weak after the settle without it, `None` before.
    pub stay_network: Option<NetworkId>,
    /// The members' authenticated answers, on every lane, as the evidence file holds them (the
    /// newest per member and lane); none where the file holds a line this build cannot read, so
    /// a torn or corrupted file proves nothing rather than the lines around the damage.
    pub answers: Vec<Answer>,
    /// When it was read (unix ms).
    pub at_ms: u64,
}

/// [`Now`] under the state root `state` (the place's record and evidence there), from `here` at
/// `now`, writing nothing.
#[must_use]
pub fn read_with(state: &Path, here: Here, now: u64) -> Now {
    let dir = crate::record::dir(state);
    let (record, (evidence, passed)) = match aterm_uds::private_file::JudgedDir::judge(&dir) {
        Ok(judged) => (
            crate::record::read_in(&judged)
                .map(|written| written.record)
                .unwrap_or_default(),
            crate::heard::read_answers_in(&dir, &judged),
        ),
        Err(_) => (
            Default::default(),
            (
                Default::default(),
                crate::heard::refused(&dir, crate::heard::FILE),
            ),
        ),
    };
    let (stay_since_ms, stay_network) = observed_stay(record, &here.facts, now);
    Now {
        stay_since_ms,
        stay_network,
        answers: if passed == 0 {
            evidence.answers
        } else {
            Vec::new()
        },
        facts: here.facts,
        point_to_point: here.point_to_point,
        own: here.own,
        at_ms: now,
    }
}

/// [`read_with`] over this Mac now ([`crate::facts::here_now`]) at this clock. It reads
/// Network.framework's path, so never on a window's main thread.
#[must_use]
pub fn read(state: &Path) -> Now {
    read_with(state, crate::facts::here_now(), crate::unix_ms_now())
}

#[cfg(test)]
#[path = "proof_tests.rs"]
mod tests;
