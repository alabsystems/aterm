// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Automatic sealed control and declared-table repair for approved Commons
//! devices. Discovery never activates accounts or grants terminal control.
//! One local owner serves native LAN attachments; other sessions take over
//! after it exits. Offline operation needs no peer or central service.

use crate::{Result, control::Endpoint};
use aterm_net::lan::Listener;
use aterm_secrets::{
    client::Paths,
    commons::{self, bootstrap::Bootstrap},
};
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
}
impl Runtime {
    /// Start default-on background repair using only this explicit secrets root
    /// and config file. A missing cached control record creates no state and
    /// opens no socket. Call only from a long-lived app/session host.
    pub fn start(
        paths: Paths,
        config: PathBuf,
        observe: impl Fn(Status) + Send + 'static,
    ) -> Result<Self> {
        let stop = Arc::new(Stop::default());
        let worker = stop.clone();
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
                    match serve(&paths, &config, &worker, &mut report) {
                        Ok(()) => {}
                        Err(status) => {
                            report(status);
                            worker.wait(POLL);
                        }
                    }
                }
            })?;
        Ok(Self {
            stop,
            task: Some(task),
        })
    }
    pub fn stop(mut self) {
        self.join();
    }
    fn join(&mut self) {
        self.stop.request();
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.join();
    }
}

fn enabled(config: &Path) -> Result<bool> {
    Ok(crate::catalog::policy::Policy::read(config)?.enabled)
}

fn serve(
    paths: &Paths,
    config: &Path,
    stop: &Stop,
    report: &mut impl FnMut(Status),
) -> std::result::Result<(), Status> {
    if !enabled(config).map_err(|_| Status::Refused)? {
        return Err(Status::Disabled);
    }
    let snapshot = commons::cached(paths)
        .map_err(|_| Status::Refused)?
        .ok_or(Status::Dormant)?;
    let version = *snapshot.digest();
    drop(snapshot);
    let lease = owner::Lease::acquire(&paths.dir.join("commons"))
        .map_err(|_| Status::Refused)?
        .ok_or(Status::Waiting)?;
    // Locals drop in reverse order: transport closes/drains before lease unlock.
    let bootstrap = Bootstrap::cached(paths)
        .map_err(|_| Status::Refused)?
        .ok_or(Status::Dormant)?;
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

fn addresses() -> std::result::Result<Vec<std::net::SocketAddr>, Status> {
    Listener::candidates()
        .map(|addresses| {
            addresses
                .into_iter()
                .filter(|address| address.is_ipv4())
                .collect()
        })
        .map_err(|_| Status::Offline)
}

#[cfg(test)]
mod tests;
