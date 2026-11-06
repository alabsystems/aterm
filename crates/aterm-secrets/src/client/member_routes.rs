// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A pending subscriber can reach a member when its LAN changes or its broker arrives,
//! even while its vault request is held in a long poll. This worker invokes the authenticated
//! member callback; it provides no discovery, proof or admission authority of its own.

use super::{MembersAsked, Paths, ask_members_if_due, swarm_lets};
use std::os::fd::{AsFd as _, BorrowedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

const LOOK: Duration = Duration::from_secs(1);
const REST: Duration = Duration::from_secs(1);

pub(super) enum ProjectionEvent {
    Observe,
    Admitted,
    BrokerReady,
}

pub(super) trait Changes: Send {
    fn wait(&mut self, most: Duration, also: Option<BorrowedFd<'_>>) -> bool;
}

#[cfg(test)]
impl<F: FnMut(Duration) -> bool + Send> Changes for F {
    fn wait(&mut self, most: Duration, _also: Option<BorrowedFd<'_>>) -> bool {
        self(most)
    }
}

pub(super) struct NativeChanges {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    route: Option<crate::network::RouteChanges>,
}

impl NativeChanges {
    pub(super) fn new() -> Self {
        Self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            route: crate::network::RouteChanges::open(),
        }
    }
}

impl Changes for NativeChanges {
    fn wait(&mut self, most: Duration, also: Option<BorrowedFd<'_>>) -> bool {
        let mut fds = Vec::with_capacity(2);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Some(route) = &self.route {
            fds.push(route.as_fd());
        }
        if let Some(fd) = also {
            fds.push(fd);
        }
        if fds.is_empty() {
            std::thread::sleep(most);
            return false;
        }
        let ready = if most.is_zero() {
            let interests: Vec<_> = fds
                .iter()
                .copied()
                .map(|fd| aterm_uds::ready::Watch::new(fd, aterm_uds::ready::Interest::Read))
                .collect();
            aterm_uds::ready::any_interests_now(&interests)
        } else {
            aterm_uds::ready::any_until(&fds, Instant::now() + most)
        };
        let Ok(ready) = ready else {
            std::thread::sleep(most);
            return false;
        };
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Some(route) = &self.route {
            return ready.contains(&0) && route.wait(Duration::from_millis(1));
        }
        false
    }
}

#[cfg(target_os = "macos")]
fn broker_arrived(
    broker: &mut aterm_uds::lan::Arrivals,
    last_error: &mut Option<String>,
    log: &(dyn Fn(&str) + Sync),
) -> bool {
    match broker.poll() {
        Ok(arrived) => {
            last_error.take();
            arrived
        }
        Err(error) => {
            let why = error.to_string();
            if last_error.as_ref() != Some(&why) {
                log(&format!(
                    "cannot watch the LAN broker's arrival: {why}; subscriber rounds still retry"
                ));
                *last_error = Some(why);
            }
            false
        }
    }
}

struct Ending<'a>(&'a AtomicBool);

impl Drop for Ending<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Own the readiness worker for exactly `run`'s lifetime. Event bursts are coalesced;
/// the first event is immediate and later events never create concurrent asks.
pub(super) fn while_hearing<T>(
    paths: &Paths,
    members: &Mutex<MembersAsked>,
    ask: &(dyn Fn(&Paths) -> crate::Result<usize> + Sync),
    log: &(dyn Fn(&str) + Sync),
    project: &(dyn Fn(ProjectionEvent) + Sync),
    mut changed: impl Changes,
    run: impl FnOnce() -> T,
) -> T {
    let ending = AtomicBool::new(false);
    #[cfg(target_os = "macos")]
    let mut broker = aterm_uds::lan::Arrivals::new();
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
                #[cfg(target_os = "macos")]
                let mut broker_error = None;
                while !ending.load(Ordering::Acquire) {
                    let wait = due.map_or(LOOK, |at: Instant| {
                        at.saturating_duration_since(Instant::now()).min(LOOK)
                    });
                    #[cfg(target_os = "macos")]
                    let before = broker_arrived(&mut broker, &mut broker_error, log);
                    #[cfg(target_os = "macos")]
                    let wait = if before {
                        Duration::ZERO
                    } else {
                        broker.wait_for(wait)
                    };
                    #[cfg(target_os = "macos")]
                    let route = changed.wait(wait, broker.watch_fd());
                    #[cfg(not(target_os = "macos"))]
                    let route = changed.wait(wait, None);
                    #[cfg(target_os = "macos")]
                    let broker_ready = before | broker_arrived(&mut broker, &mut broker_error, log);
                    #[cfg(not(target_os = "macos"))]
                    let broker_ready = false;
                    if route || broker_ready {
                        due.get_or_insert_with(|| {
                            last.map_or_else(Instant::now, |at| (at + REST).max(Instant::now()))
                        });
                    }
                    if ending.load(Ordering::Acquire) {
                        break;
                    }
                    if broker_ready {
                        project(ProjectionEvent::BrokerReady);
                    }
                    if swarm_lets(&mut switch, &mut held, "member convergence", log) {
                        project(ProjectionEvent::Observe);
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
                        let before = admitted(paths);
                        ask_members_if_due(paths, &mut members, ask, log);
                        let progressed = !before && admitted(paths);
                        drop(members);
                        if progressed
                            && swarm_lets(&mut switch, &mut held, "member convergence", log)
                        {
                            project(ProjectionEvent::Admitted);
                        }
                    }
                    last = Some(Instant::now());
                }
            });
        if let Err(error) = worker {
            log(&format!(
                "member readiness listener did not start: {error}; subscriber rounds still ask"
            ));
        }
        run()
    })
}

fn admitted(paths: &Paths) -> bool {
    super::config(paths)
        .ok()
        .flatten()
        .is_some_and(|config| super::alab_vault(&config))
        && super::load_state(paths).is_ok_and(|state| state.membership == super::Stored::In)
        && !super::seat_ask::asks_for_seat(paths)
}
