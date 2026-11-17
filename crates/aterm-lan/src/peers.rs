// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A failed destination has one retry clock across every local caller. The clock is checked
//! on a request, never by a timer, and no lock is held while resolving or dialing.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_uds::lan::ConnectOptions;
use aterm_uds::lan::wire::Target;

use crate::server::Failure;

const FIRST_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(300);
const MAX_PEERS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Key {
    target: Target,
    port: u16,
    options: ConnectOptions,
}

impl Key {
    pub(crate) fn new(mut target: Target, port: u16, options: ConnectOptions) -> Self {
        match &mut target {
            Target::Addr(addr) => addr.set_port(port),
            Target::Host(host) => *host = host.trim_end_matches('.').to_ascii_lowercase(),
        }
        Self {
            target,
            port,
            options,
        }
    }
}

struct Peer {
    key: Key,
    live: usize,
    generation: u64,
    failed: Option<Failure>,
    delay: Duration,
    retry_at: Instant,
}

#[derive(Default)]
pub(crate) struct Peers(Mutex<Vec<Peer>>);

impl Peers {
    pub(crate) fn connect<T>(
        &self,
        key: Key,
        clock: impl Fn() -> Instant,
        dial: impl FnOnce() -> Result<T, Failure>,
        log: impl FnOnce(std::fmt::Arguments<'_>),
    ) -> Result<T, Failure> {
        let now = clock();
        let generation = {
            let mut peers = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(peer) = peers.iter_mut().find(|peer| peer.key == key) {
                if peer.failed.is_some() && (peer.live != 0 || now < peer.retry_at) {
                    return Err(peer.failed.clone().expect("this peer is failed"));
                }
                peer.live += 1;
                peer.generation
            } else {
                if peers.len() == MAX_PEERS {
                    // Never evict an in-flight owner's entry. If all slots are live, the
                    // request gets a busy answer instead of bypassing the bounded memory.
                    let Some(old) = peers.iter().position(|peer| peer.live == 0) else {
                        return Err((libc::EAGAIN, "the LAN broker's peer slots are busy".into()));
                    };
                    peers.remove(old);
                }
                peers.push(Peer {
                    key: key.clone(),
                    live: 1,
                    generation: 0,
                    failed: None,
                    delay: Duration::ZERO,
                    retry_at: now,
                });
                0
            }
        };
        let result = dial();
        let mut peers = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let index = peers
            .iter()
            .position(|peer| peer.key == key)
            .expect("a live peer is never evicted");
        let peer = &mut peers[index];
        peer.live -= 1;
        let recovered = peer.failed.is_some() && result.is_ok();
        let failed = match &result {
            Err((code, _)) => matches!(
                *code,
                libc::ETIMEDOUT
                    | libc::ECONNREFUSED
                    | libc::EHOSTUNREACH
                    | libc::ENETUNREACH
                    | libc::EHOSTDOWN
                    | libc::ENETDOWN
                    | libc::ECONNRESET
            ),
            Ok(_) => false,
        };
        let failed = failed && generation == peer.generation;
        let changed = failed && peer.failed.is_none();
        if failed {
            peer.generation = peer.generation.wrapping_add(1);
            peer.failed = result.as_ref().err().cloned();
            peer.delay = if peer.delay.is_zero() {
                FIRST_RETRY
            } else {
                peer.delay.saturating_mul(2).min(MAX_RETRY)
            };
            peer.retry_at = clock() + peer.delay;
        } else if result.is_ok() {
            // A success ends the outage. An older parallel dial's eventual failure must
            // not overwrite that fresher evidence, but healthy dials may run in parallel.
            peer.generation = peer.generation.wrapping_add(1);
            peer.failed = None;
            peer.delay = Duration::ZERO;
        }
        if peer.live == 0 && peer.failed.is_none() {
            peers.remove(index);
        }
        // Keep state transitions and their bounded writes in the same order. The socket
        // work has ended; no name resolution, connect or handoff holds this lock.
        if changed {
            let (_, why) = result.as_ref().err().expect("a failure changed state");
            log(format_args!("peer {key:?} unavailable: {why}"));
        } else if recovered {
            log(format_args!("peer {key:?} reachable"));
        }
        drop(peers);
        result
    }
}

#[cfg(test)]
#[path = "peers_tests.rs"]
mod tests;
