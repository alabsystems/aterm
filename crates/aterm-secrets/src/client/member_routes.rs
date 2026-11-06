// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A pending subscriber can reach a member as soon as its LAN changes, even while its
//! vault request is held in a long poll. This worker invokes the ordinary authenticated
//! member callback; it provides no discovery, proof or admission authority of its own.

use super::{MembersAsked, Paths, ask_members_if_due, swarm_lets};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

const LOOK: Duration = Duration::from_secs(1);
const REST: Duration = Duration::from_secs(1);

struct Ending<'a>(&'a AtomicBool);

impl Drop for Ending<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Own the route worker for exactly `run`'s lifetime. Route bursts are coalesced;
/// the first event is immediate and later events never create concurrent asks.
pub(super) fn while_hearing<T>(
    paths: &Paths,
    members: &Mutex<MembersAsked>,
    ask: &(dyn Fn(&Paths) -> crate::Result<usize> + Sync),
    log: &(dyn Fn(&str) + Sync),
    mut changed: impl FnMut(Duration) -> bool + Send,
    run: impl FnOnce() -> T,
) -> T {
    let ending = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _ending = Ending(&ending);
        let ending = &ending;
        let worker = std::thread::Builder::new()
            .name("vault-member-routes".into())
            .spawn_scoped(scope, move || {
                let mut switch = crate::swarm::Switch::new(&paths.home);
                let mut held = false;
                let mut due = None;
                let mut last = None::<Instant>;
                while !ending.load(Ordering::Acquire) {
                    let wait = due.map_or(LOOK, |at: Instant| {
                        at.saturating_duration_since(Instant::now()).min(LOOK)
                    });
                    if changed(wait) {
                        due.get_or_insert_with(|| {
                            last.map_or_else(Instant::now, |at| (at + REST).max(Instant::now()))
                        });
                    }
                    if ending.load(Ordering::Acquire) {
                        break;
                    }
                    if due.is_none_or(|at| at > Instant::now()) {
                        continue;
                    }
                    due = None;
                    // The round and route worker share both the backoff and the actual ask.
                    // Re-read sharing after waiting for a round's bounded proof to finish.
                    let mut members = members.lock().unwrap_or_else(PoisonError::into_inner);
                    if !ending.load(Ordering::Acquire)
                        && swarm_lets(&mut switch, &mut held, "member discovery", log)
                    {
                        members.at = None;
                        members.asks = 0;
                        ask_members_if_due(paths, &mut members, ask, log);
                    }
                    last = Some(Instant::now());
                }
            });
        if let Err(error) = worker {
            log(&format!(
                "member route listener did not start: {error}; subscriber rounds still ask"
            ));
        }
        run()
    })
}
