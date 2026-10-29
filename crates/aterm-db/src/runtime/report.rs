// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded metadata from completed automatic repair workers. No payload or key
//! material leaves the transport through this observation.

use astream_wire::sync::Actor;
use std::net::SocketAddr;

/// One automatic owner's completed worker, including failed or cancelled work.
/// An authenticated actor proves this round's identity exchange, not successful
/// control repair, application repair or globally converged state.
#[derive(Clone, Debug)]
pub struct RepairRound {
    /// The advertised service dialled by an outbound worker; None for inbound.
    pub target: Option<SocketAddr>,
    pub actor: Option<Actor>,
    pub exchange_started: bool,
    pub cancelled: bool,
    /// Last local operation entered; None when a worker panicked before reporting.
    pub phase: Option<crate::control::Phase>,
    pub control: std::result::Result<crate::control::Report, String>,
    /// Signed membership records retained independently of application access.
    pub membership: Option<crate::control::Report>,
    pub membership_error: Option<String>,
    pub application: Option<crate::replica::Report>,
    pub application_error: Option<String>,
    pub completed_at_ms: u64,
    /// Earlier metadata evicted from this bounded queue before this record.
    /// This reports an observation gap, not missing application operations.
    pub dropped_before: usize,
}

pub(super) fn bounded_error(error: impl std::fmt::Display) -> String {
    let mut value = error.to_string();
    const MAX: usize = 512;
    if value.len() > MAX {
        let mut end = MAX - 3;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
        value.push('…');
    }
    value
}

impl RepairRound {
    pub(super) fn panicked(target: Option<SocketAddr>) -> Self {
        Self {
            target,
            actor: None,
            exchange_started: false,
            cancelled: false,
            phase: None,
            control: Err("automatic repair worker panicked".into()),
            membership: None,
            membership_error: None,
            application: None,
            application_error: None,
            completed_at_ms: aterm_time::unix_millis().unwrap_or(0),
            dropped_before: 0,
        }
    }
}
