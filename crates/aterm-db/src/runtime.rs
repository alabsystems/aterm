// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded local presence observation and sealed repair for approved Commons
//! devices. Unverified discovery never activates accounts or grants control.
//! One local owner serves native LAN attachments; other sessions take over
//! after it exits. Offline operation needs no peer or central service.

use crate::network_state::{ObservationHandle, Observer};
use crate::{Result, control::Endpoint};
use aterm_secrets::{
    client::Paths,
    commons::{self, bootstrap::Bootstrap},
};
use aterm_types::network_state::{MachineIdentity, Visibility};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod custody;
mod discovery;
mod owner;
mod transport;

#[cfg(any(test, debug_assertions))]
pub use discovery::db_discovery_replacement_environment;

const POLL: Duration = Duration::from_secs(2);
const TICK: Duration = Duration::from_millis(100);

/// Local observation, not a claim of global convergence. Counts of peers are
/// address hints, not authenticated members.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Dormant,
    Disabled,
    Waiting,
    Pending,
    Offline,
    Active { interfaces: usize, hints: usize },
    Refused,
}

#[derive(Default)]
struct Stop {
    requested: Mutex<bool>,
    changed: Condvar,
}
impl Stop {
    fn requested(&self) -> bool {
        *self.requested.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn request(&self) {
        *self.requested.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.changed.notify_all();
    }
    fn wait(&self, duration: Duration) {
        let lock = self.requested.lock().unwrap_or_else(|p| p.into_inner());
        let _ = self
            .changed
            .wait_timeout_while(lock, duration, |stop| !*stop);
    }
}

/// Owning process lifetime. Dropping it stops and joins all network workers.
pub struct Runtime {
    stop: Arc<Stop>,
    task: Option<JoinHandle<()>>,
    observation: ObservationHandle,
}
impl Runtime {
    /// Start default-on background repair using only this explicit secrets root
    /// and config file. Missing control permits opaque presence discovery and
    /// local observation history, never enrollment or data repair. Call only
    /// from a long-lived app/session host.
    pub fn start(
        paths: Paths,
        config: PathBuf,
        observe: impl Fn(Status) + Send + 'static,
    ) -> Result<Self> {
        let stop = Arc::new(Stop::default());
        let worker = stop.clone();
        let observation = ObservationHandle::default();
        let published = observation.clone();
        let task = thread::Builder::new()
            .name("commons-owner".into())
            .spawn(move || {
                let mut previous = None;
                let mut report = |status| {
                    if previous != Some(status) {
                        observe(status);
                        previous = Some(status);
                    }
                };
                while !worker.requested() {
                    let mut history = match Observer::open(&config, published.clone()) {
                        Ok(history) => history,
                        Err(crate::network_state::OpenFailure::Unavailable) => {
                            Observer::unavailable(&config,published.clone())
                        }
                        Err(crate::network_state::OpenFailure::Owned) => {
                            let mut retained =
                                crate::network_state::read_history(&config).unwrap_or_default();
                            retained.visibility = Visibility::Waiting;
                            retained.detail = "Another local aterm owns network observation; live visibility is not forwarded".into();
                            published.replace(retained);
                            report(Status::Waiting);
                            worker.wait(POLL);
                            continue;
                        }
                    };
                    while !worker.requested() {
                        if let Err(status) =
                            serve(&paths, &config, &worker, &mut report, &mut history)
                        {
                            let (visibility, detail) = match status {
                                Status::Disabled => {
                                    (Visibility::Disabled, "Network visibility disabled")
                                }
                                Status::Waiting => (
                                    Visibility::Waiting,
                                    "Another local process owns Commons repair",
                                ),
                                _ => (Visibility::Unavailable, "Network observation unavailable"),
                            };
                            history.observe(
                                visibility,
                                0,
                                detail,
                                aterm_time::unix_millis().unwrap_or(0),
                                Instant::now(),
                            );
                            report(status);
                            worker.wait(POLL);
                        }
                    }
                    history.observe(
                        Visibility::Unavailable,
                        0,
                        "Network observation stopped",
                        aterm_time::unix_millis().unwrap_or(0),
                        Instant::now(),
                    );
                }
            })?;
        Ok(Self {
            stop,
            task: Some(task),
            observation,
        })
    }
    pub fn stop(mut self) {
        self.join();
    }
    /// Pure shared observation; readers clone the value off the render path.
    pub fn network_observation(&self) -> ObservationHandle {
        self.observation.clone()
    }
    fn join(&mut self) {
        self.stop.request();
        if let Some(task) = self.task.take() {
            let _ = task.join();
            let mut observation = self.observation.snapshot();
            observation.visibility = Visibility::Unavailable;
            observation.detail = "Network observation stopped".into();
            observation.revision = observation.revision.saturating_add(1);
            observation.observed_at_ms = aterm_time::unix_millis().unwrap_or(0);
            for peer in &mut observation.peers {
                peer.reachability = aterm_types::network_state::Reachability::Unknown;
            }
            self.observation.replace(observation);
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.join();
    }
}

/// Whether `config`'s `[commons] enabled` lets this Mac take part in the shared data (on when
/// the file or the table is absent). A read: the converge pass's `sync.shared-data` row asks it.
///
/// # Errors
///
/// Returns an error when `config` is not an owned safe file or not UTF-8 TOML, or when its
/// `commons` entry is not a table, names an unknown key, holds a non-boolean `enabled`, or an
/// `exclude_tables` that is not a bounded array of distinct canonical table ids.
pub fn enabled(config: &Path) -> Result<bool> {
    Ok(crate::catalog::policy::Policy::read(config)?.enabled)
}

fn serve(
    paths: &Paths,
    config: &Path,
    stop: &Stop,
    report: &mut impl FnMut(Status),
    history: &mut Observer,
) -> std::result::Result<(), Status> {
    if !enabled(config).map_err(|_| Status::Refused)? {
        return Err(Status::Disabled);
    }
    let snapshot = match commons::cached(paths) {
        Ok(Some(snapshot)) => snapshot,
        _ => return serve_presence(paths, config, stop, report, history),
    };
    let version = *snapshot.digest();
    drop(snapshot);
    let lease = owner::Lease::acquire(&paths.dir.join("commons"))
        .map_err(|_| Status::Refused)?
        .ok_or(Status::Waiting)?;
    // Locals drop in reverse order: transport closes/drains before lease unlock.
    let bootstrap = Bootstrap::cached(paths)
        .map_err(|_| Status::Refused)?
        .ok_or(Status::Dormant)?;
    let request = bootstrap.request().map_err(|_| Status::Refused)?;
    // Observation failures never gate independently authorized data repair.
    let _ = history.local(machine_identity(request.binding()), bootstrap.trust());
    let endpoint = match Endpoint::from_bootstrap(&bootstrap).map_err(|_| Status::Refused)? {
        Some(endpoint) => endpoint
            .with_commons(paths.clone(), config.to_owned())
            .map_err(|_| Status::Refused)?,
        None => Endpoint::pending_from_bootstrap(&bootstrap, paths.clone())
            .map_err(|_| Status::Refused)?,
    };
    let pending = endpoint.pending();
    let custody =
        custody::Custody::capture(paths, bootstrap.journal_dir()).map_err(|_| Status::Refused)?;
    if commons::cached(paths)
        .map_err(|_| Status::Refused)?
        .ok_or(Status::Dormant)?
        .digest()
        != &version
    {
        return Ok(());
    }
    let candidates = addresses()?;
    let mut transport =
        transport::Transport::open(endpoint, &candidates).map_err(|_| Status::Refused)?;
    let mut poll = Instant::now() + POLL;
    while !stop.requested() {
        let now = Instant::now();
        if now >= poll {
            lease.recheck().map_err(|_| Status::Refused)?;
            custody.recheck().map_err(|_| Status::Refused)?;
            if !enabled(config).map_err(|_| Status::Refused)? {
                return Err(Status::Disabled);
            }
            let snapshot = commons::cached(paths)
                .map_err(|_| Status::Refused)?
                .ok_or(Status::Dormant)?;
            if snapshot.digest() != &version {
                return Ok(());
            }
            if pending && bootstrap.witness().map_err(|_| Status::Refused)?.is_some() {
                return Ok(());
            }
            transport.refresh(&addresses()?);
            poll = now + POLL;
        }
        transport.tick(now).map_err(|_| Status::Offline)?;
        let contacts = transport.take_contacts();
        if !contacts.is_empty()
            && let Ok(Some(access)) = bootstrap.access()
        {
            for (actor, authenticated_at) in contacts {
                if let Some(member) = access
                    .members()
                    .iter()
                    .find(|m| m.binding().actor() == actor)
                {
                    history.contact(
                        machine_identity(member.binding()),
                        contact_time(
                            aterm_time::unix_millis().unwrap_or(0),
                            Instant::now(),
                            authenticated_at,
                        ),
                        authenticated_at,
                    );
                }
            }
        }
        history.observe(
            if transport.unavailable() {
                Visibility::Unavailable
            } else if transport.interfaces() == 0 {
                Visibility::Offline
            } else {
                Visibility::Active
            },
            transport.unverified_peers(),
            if pending {
                "Discovery active; enrollment pending"
            } else {
                "Authenticated repair observer active"
            },
            aterm_time::unix_millis().unwrap_or(0),
            now,
        );
        report(if transport.interfaces() == 0 {
            Status::Offline
        } else if pending {
            Status::Pending
        } else {
            Status::Active {
                interfaces: transport.interfaces(),
                hints: transport.peers(),
            }
        });
        stop.wait(TICK);
    }
    Ok(())
}

fn machine_identity(binding: &aterm_secrets::commons::binding::Binding) -> MachineIdentity {
    MachineIdentity {
        machine_id: *binding.machine(),
        incarnation: *binding.incarnation(),
        label: None,
    }
}

fn contact_time(now_ms: u64, observed: Instant, authenticated: Instant) -> u64 {
    let elapsed = u64::try_from(
        observed
            .saturating_duration_since(authenticated)
            .as_millis(),
    )
    .unwrap_or(u64::MAX);
    now_ms.saturating_sub(elapsed)
}

fn serve_presence(
    paths: &Paths,
    config: &Path,
    stop: &Stop,
    report: &mut impl FnMut(Status),
    history: &mut Observer,
) -> std::result::Result<(), Status> {
    let mut transport =
        transport::Transport::presence_only(&addresses()?).map_err(|_| Status::Offline)?;
    let mut poll = Instant::now() + POLL;
    report(Status::Dormant);
    while !stop.requested() {
        let now = Instant::now();
        if now >= poll {
            if !enabled(config).map_err(|_| Status::Refused)? {
                return Err(Status::Disabled);
            }
            if matches!(commons::cached(paths), Ok(Some(_))) {
                return Ok(());
            }
            transport.refresh(&addresses()?);
            poll = now + POLL;
        }
        transport.tick(now).map_err(|_| Status::Offline)?;
        history.observe(
            if transport.unavailable() {
                Visibility::Unavailable
            } else if transport.interfaces() == 0 {
                Visibility::Offline
            } else {
                Visibility::Active
            },
            transport.unverified_peers(),
            "Discovery only; Commons control unavailable",
            aterm_time::unix_millis().unwrap_or(0),
            now,
        );
        stop.wait(TICK);
    }
    Ok(())
}

#[cfg(not(test))]
fn addresses() -> std::result::Result<Vec<std::net::SocketAddr>, Status> {
    aterm_net::lan::Listener::candidates()
        .map(|addresses| {
            addresses
                .into_iter()
                .filter(|address| address.is_ipv4())
                .collect()
        })
        .map_err(|_| Status::Offline)
}

#[cfg(test)]
fn addresses() -> std::result::Result<Vec<std::net::SocketAddr>, Status> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests;
