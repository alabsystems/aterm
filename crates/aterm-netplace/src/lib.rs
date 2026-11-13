// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE NETWORK PLACE: one per-Mac answer to where this Mac is on the network, read by every
//! reporter in the same words (`docs/DESIGN-network-place-2026-11-01.md`).
//!
//! Two axes that never merge: where (`alab-lan`, `remote:<hotspot|lan|cellular|vpn>`,
//! `offline`, `checking`) and the internet (`ok`, `down`, `path-up`, `unknown`, always with its
//! source), with the ALab LAN axis beside them (here, away since, learning). Who in the lab is up
//! is presence's, never the place's: a reporter hands that in ([`words::Seats`]). [`place`] decides it, purely; [`words`] says it; [`record`] keeps the keeper's
//! memory; [`heard`] is the evidence every process notes into; [`facts`] reads the Mac;
//! [`keeper`] is the one writer and [`keeper::read`] what every reader calls; [`watch`] hears
//! the files they read and the routes change, so neither looks for a change on a clock; [`moves`] tells a
//! settled place as a network move, and [`routes`] says which member addresses an automatic LAN
//! lane dials while away (the seam a future internet path plugs in at). [`proof`] is what a
//! caller that must not take memory for proof reads (the vault's automatic ALab LAN admission):
//! the facts now, the current stay and the members' answers, from the same files and the same
//! Mac as the place, so this Mac has one answer to where it is.
//!
//! Being away changes words only here: it gates no secret and no vault serving. The automatic
//! ALab LAN admission reads [`proof`], never the place's word: a remembered network is no proof.

pub mod place;
mod time;

pub use place::{
    Alab, Answer, Book, BookMember, Checking, ComputedBy, Evidence, Facts, Internet, InternetNote,
    InternetSource, InternetState, Kind, Lane, Link, NetworkId, PathFacts, Place, Projection,
    Recognised, Record, Round, RouteFacts, Where, member_key, step, step_with_deadline,
};

// The Mac's readers, the files and the keeper stand on aterm-uds's Unix-only network tables and
// private files; off Unix the place's types and words stay, and a reporter says it is unknown.
#[cfg(unix)]
pub mod facts;
#[cfg(unix)]
pub mod heard;
#[cfg(unix)]
pub mod keeper;
pub mod moves;
#[cfg(unix)]
pub mod proof;
#[cfg(unix)]
pub mod record;
pub mod routes;
#[cfg(unix)]
pub mod watch;
pub mod words;

#[cfg(any(test, debug_assertions))]
pub mod sample;

#[cfg(unix)]
pub use keeper::{read, read_with};

/// Now, unix ms (0 before the epoch).
#[must_use]
pub fn unix_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
