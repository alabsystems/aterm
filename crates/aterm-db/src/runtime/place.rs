// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticated Commons contacts feed the shared place without granting any
//! access or claiming application repair. Socket routes and times stay original.

use std::{net::IpAddr, path::PathBuf};

use aterm_netplace::{Answer, Lane};
use aterm_secrets::commons::binding::Binding;

use super::RepairRound;

/// One runtime's destination for shared place evidence; inert in ordinary tests.
#[derive(Default)]
pub(super) struct Publisher {
    dir: Option<PathBuf>,
}

impl Publisher {
    pub(super) fn live() -> Self {
        #[cfg(not(test))]
        let dir = aterm_types::dirs::state_dir().map(|state| aterm_netplace::record::dir(&state));
        #[cfg(test)]
        let dir = None;
        Self { dir }
    }

    pub(super) fn note(&self, answers: Vec<Answer>) {
        if let Some(dir) = &self.dir
            && !answers.is_empty()
        {
            aterm_netplace::heard::note_answers(dir, answers);
        }
    }
}

/// Current membership supplies the physical machine, never the writer incarnation.
/// The caller revalidates local custody and both bindings before publishing.
pub(super) fn answer(round: &RepairRound, local: &Binding, peer: &Binding) -> Option<Answer> {
    if !round.exchange_started
        || round.actor != Some(peer.actor())
        || local.machine() == peer.machine()
    {
        return None;
    }
    let connection = round.connection?;
    if connection.began_ms == 0
        || connection.authenticated_ms < connection.began_ms
        || connection.authenticated_ms > round.completed_at_ms
    {
        return None;
    }
    let IpAddr::V4(via) = connection.local.ip().to_canonical() else {
        return None;
    };
    if via.is_loopback() || via.is_unspecified() || via.is_multicast() {
        return None;
    }
    Some(Answer {
        lane: Lane::Commons,
        member: aterm_netplace::member_key(
            Lane::Commons,
            &aterm_codec::hex::encode_lower(peer.machine()),
        ),
        addr: Some(connection.peer.ip().to_canonical()),
        via: Some(via),
        // The socket names no interface. A Commons contact words the place and admits nobody:
        // the vault's automatic ALab LAN admission counts fleet answers only.
        iface: None,
        network: None,
        logged: false,
        began_ms: connection.began_ms,
        at_ms: connection.authenticated_ms,
    })
}

#[cfg(test)]
mod tests;
