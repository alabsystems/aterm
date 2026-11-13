// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Mac an `atm/1` listener does not seat offers that member's desk its `Node` record
//! (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.4, B3): the listener answered `NO unknown`, its
//! log binding this Mac's node key to no seat (the four seats with no `Node` record, or a record
//! that has not reached it yet). The dial itself already went again on `atf/2`; this sends the
//! record that binds the key, from this Mac's own log, on `atf-ask/1` at the same address, so the
//! member's log binds it and a later dial meets it on the mesh. It runs only where the mesh
//! switch is on, as only a mesh dial hears `NO unknown`.

use aterm_secrets::members::desk::{self, NodeTaken};
use aterm_types::MutexExt as _;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long one address is left alone after an offer: a member's log takes the record once, and
/// one whose answer did not reach this Mac hears it again after this long at the soonest.
const EVERY: Duration = Duration::from_secs(10 * 60);

/// The offer's connect, handshake and each exchange after them.
const WITHIN: Duration = Duration::from_secs(10);

/// When each address was last offered the record, in this process.
static OFFERED: Mutex<BTreeMap<SocketAddr, Instant>> = Mutex::new(BTreeMap::new());

/// `aterm_net::fleet_member::on_unknown_to`'s offer: send `to`'s desk this Mac's `Node` record on
/// a thread of its own, at most once per [`EVERY`] per address, so the dial that heard `NO
/// unknown` never waits for it. A failed offer is let go: the next dial there that hears `NO
/// unknown` once [`EVERY`] has passed offers it again.
pub fn offer_node(to: SocketAddr) {
    if !due(&mut OFFERED.lock_or_recover(), to, Instant::now()) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("aterm-mesh-node-offer".into())
        .spawn(move || {
            let _ = offer_node_now(to);
        });
}

/// Whether `to` is offered the record at `now`, noting the offer where it is.
fn due(offered: &mut BTreeMap<SocketAddr, Instant>, to: SocketAddr, now: Instant) -> bool {
    if offered
        .get(&to)
        .is_some_and(|at| now.saturating_duration_since(*at) < EVERY)
    {
        return false;
    }
    offered.insert(to, now);
    true
}

/// Send `to`'s desk the record in this Mac's log that binds this Mac's node key.
fn offer_node_now(to: SocketAddr) -> Result<NodeTaken, String> {
    let paths = aterm_secrets::client::Paths::live().map_err(|error| error.to_string())?;
    let state = aterm_types::dirs::state_dir().ok_or("this Mac has no state directory")?;
    let me = aterm_net::fleet_member::read_node_identity(&state)
        .map_err(|error| error.to_string())?
        .ok_or("this Mac holds no node key")?;
    let record = desk::node_record(&paths, me.key().bytes())
        .map_err(|error| error.to_string())?
        .ok_or("this Mac's own log binds its node key to no seat")?;
    let (transport, _) =
        aterm_net::fleet_member::dial_ask(&to.ip().to_string(), Some(to.ip()), to.port(), WITHIN)
            .map_err(|error| error.to_string())?;
    let mut stream = transport.into_stream();
    stream
        .sock
        .set_read_timeout(Some(WITHIN))
        .map_err(|error| error.to_string())?;
    stream
        .sock
        .set_write_timeout(Some(WITHIN))
        .map_err(|error| error.to_string())?;
    desk::offer_node(&mut stream, &record).map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "mesh_offer_tests.rs"]
mod tests;
