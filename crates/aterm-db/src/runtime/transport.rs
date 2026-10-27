// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded transport workers. A generation joins every worker before its owner
//! lease can be released, including a dial still awaiting its first SYN reply.

use super::discovery::Hints;
use crate::{
    Result,
    control::{Endpoint, Repair, Side},
};
use aterm_net::lan::{Cancellation, Connection, Listener, Multicast, Scope};
use ring::rand::{SecureRandom, SystemRandom};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 31, 84);
const PORT: u16 = 41384;
const WORKERS: usize = 4;

#[derive(Default)]
struct Channel {
    stopped: bool,
    local: Option<IpAddr>,
    socket: Option<Cancellation>,
}
impl Channel {
    fn install(&mut self, connection: &Connection) -> io::Result<bool> {
        if self.stopped {
            return Ok(false);
        }
        self.socket = Some(connection.cancellation()?);
        self.local = Some(connection.local_addr()?.ip());
        Ok(true)
    }
    fn stop(&mut self) {
        self.stopped = true;
        if let Some(socket) = &self.socket {
            socket.cancel();
        }
    }
}
struct Worker {
    channel: Arc<Mutex<Channel>>,
    task: JoinHandle<(Option<SocketAddr>, Repair, bool)>,
    incoming: bool,
    target: Option<SocketAddr>,
}
impl Worker {
    fn start(
        endpoint: Arc<Endpoint>,
        connection: Option<Connection>,
        target: Option<SocketAddr>,
        scope: Scope,
        repair: Repair,
    ) -> io::Result<Self> {
        let incoming = connection.is_some();
        let channel = Arc::new(Mutex::new(Channel::default()));
        if let Some(connection) = &connection {
            channel
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .install(connection)?;
        }
        let control = channel.clone();
        let task = thread::Builder::new()
            .name("commons-repair".into())
            .spawn(move || {
                let mut repair = repair;
                repair.begin_round();
                let result = (|| -> Result<()> {
                    let (connection, side) = if let Some(connection) = connection {
                        (connection, Side::Responder)
                    } else {
                        if control.lock().unwrap_or_else(|p| p.into_inner()).stopped {
                            return Ok(());
                        }
                        let Some(target) = target else {
                            return Ok(());
                        };
                        (
                            Connection::connect(target, scope, Duration::from_secs(3))?,
                            Side::Initiator,
                        )
                    };
                    if !control
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .install(&connection)?
                    {
                        return Ok(());
                    }
                    endpoint.exchange(connection, side, &mut repair)?;
                    Ok(())
                })();
                let mut control = control.lock().unwrap_or_else(|p| p.into_inner());
                let success =
                    result.is_ok() && !control.stopped && repair.authenticated_actor().is_some();
                if control.stopped {
                    repair.begin_round();
                }
                control.socket = None;
                (target, repair, success)
            })?;
        Ok(Self {
            channel,
            task,
            incoming,
            target,
        })
    }
    fn stop(&self) {
        self.channel
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .stop();
    }
    fn stop_on(&self, local: IpAddr) {
        let mut channel = self.channel.lock().unwrap_or_else(|p| p.into_inner());
        if channel.local == Some(local) {
            channel.stop();
        }
    }
}

struct Link {
    source: IpAddr,
    listener: Option<Listener>,
    hints: Multicast,
}

pub(super) struct Transport {
    endpoint: Option<Arc<Endpoint>>,
    links: Vec<Link>,
    hints: Hints,
    workers: Vec<Worker>,
    beacon: Instant,
    dial: Instant,
    rotate: Instant,
    scope: Scope,
    contacts: Vec<(astream_wire::sync::Actor, Instant)>,
    failed: bool,
    #[cfg(test)]
    observed: std::collections::BTreeMap<astream_wire::sync::Digest, usize>,
}
impl Transport {
    pub(super) fn open(endpoint: Endpoint, addresses: &[SocketAddr]) -> Result<Self> {
        Self::open_mode(Some(endpoint), addresses)
    }
    /// Opaque presence only: no TCP listener, identity advertisement or dialing.
    pub(super) fn presence_only(addresses: &[SocketAddr]) -> Result<Self> {
        Self::open_mode(None, addresses)
    }
    fn open_mode(endpoint: Option<Endpoint>, addresses: &[SocketAddr]) -> Result<Self> {
        let mut token = [0; 32];
        SystemRandom::new()
            .fill(&mut token)
            .map_err(|_| crate::Error::new("discovery randomness unavailable"))?;
        let now = Instant::now();
        let mut transport = Self {
            endpoint: endpoint.map(Arc::new),
            links: Vec::new(),
            hints: Hints::new(token),
            workers: Vec::new(),
            beacon: now,
            dial: now,
            rotate: now + Duration::from_secs(300),
            scope: Scope::Lan,
            contacts: Vec::new(),
            failed: false,
            #[cfg(test)]
            observed: std::collections::BTreeMap::new(),
        };
        transport.refresh(addresses);
        Ok(transport)
    }
    pub(super) fn refresh(&mut self, addresses: &[SocketAddr]) {
        self.failed = false;
        for address in addresses {
            let IpAddr::V4(source) = address.ip() else {
                continue;
            };
            if self.links.iter().any(|link| link.source == address.ip()) {
                continue;
            }
            let listener = if self
                .endpoint
                .as_ref()
                .is_none_or(|endpoint| endpoint.pending())
            {
                None
            } else {
                let Ok(listener) = Listener::bind(*address, self.scope) else {
                    self.failed = true;
                    continue;
                };
                Some(listener)
            };
            if let Ok(hints) = Multicast::bind(source, GROUP, PORT, self.scope) {
                self.links.push(Link {
                    source: address.ip(),
                    listener,
                    hints,
                });
                self.beacon = Instant::now();
            } else {
                self.failed = true;
            }
        }
    }
    pub(super) fn interfaces(&self) -> usize {
        self.links.len()
    }
    pub(super) fn unavailable(&self) -> bool {
        self.links.is_empty() && self.failed
    }
    pub(super) fn peers(&self) -> usize {
        self.hints.count()
    }
    pub(super) fn unverified_peers(&self) -> usize {
        self.hints.unverified_count()
    }
    pub(super) fn take_contacts(&mut self) -> Vec<(astream_wire::sync::Actor, Instant)> {
        std::mem::take(&mut self.contacts)
    }
    pub(super) fn tick(&mut self, now: Instant) -> io::Result<()> {
        self.hints.expire_presence(now);
        if self.endpoint.is_none() {
            self.hints.expire_listen_only(now);
        }
        if now >= self.rotate {
            let mut token = [0; 32];
            SystemRandom::new()
                .fill(&mut token)
                .map_err(|_| io::Error::other("discovery randomness unavailable"))?;
            self.hints.rotate(token);
            self.rotate = now + Duration::from_secs(300);
        }
        let mut index = 0;
        while index < self.workers.len() {
            if self.workers[index].task.is_finished() {
                let worker = self.workers.swap_remove(index);
                match worker.task.join() {
                    Ok((target, repair, success)) => {
                        if let Some((actor, at)) = repair.authenticated_contact() {
                            if let Some((_, old)) =
                                self.contacts.iter_mut().find(|(id, _)| *id == actor)
                            {
                                *old = (*old).max(at);
                            } else if self.contacts.len() < 64 {
                                self.contacts.push((actor, at));
                            }
                        }
                        #[cfg(test)]
                        if let Some(report) = repair.application()
                            && let Some(table) = report.table
                        {
                            self.observed.insert(table, report.retained);
                        }
                        if let Some(address) = target {
                            self.hints.finish(address, repair, success, now);
                        }
                    }
                    Err(_) => {
                        if let Some(address) = worker.target {
                            self.hints.finish(address, Repair::default(), false, now);
                        }
                    }
                }
            } else {
                index += 1;
            }
        }
        let mut index = 0;
        while index < self.links.len() {
            let link = &self.links[index];
            let serviced = (|| -> io::Result<()> {
                if let Some(listener) = &link.listener {
                    listener.recheck()?;
                }
                link.hints.recheck()?;
                if now >= self.beacon {
                    let wire = match &link.listener {
                        Some(listener) => self.hints.wire(listener.local_addr()?.port()),
                        None => self.hints.presence_wire(),
                    };
                    link.hints.send(&wire)?;
                }
                // Bound work from an unauthenticated multicast flood per tick.
                for _ in 0..16 {
                    let Some((wire, source)) = link.hints.receive()? else {
                        break;
                    };
                    if self.scope == Scope::Lan
                        && self.links.iter().any(|link| link.source == source.ip())
                    {
                        continue;
                    }
                    self.hints.observe(&wire, source, now);
                    self.hints.observe_presence(&wire, source, now);
                }
                // Unproved inbound peers cannot occupy the outbound repair budget.
                if self.workers.len() < WORKERS
                    && self.workers.iter().filter(|worker| worker.incoming).count() < WORKERS / 2
                    && let Some(listener) = &link.listener
                    && let Some(endpoint) = &self.endpoint
                {
                    match listener.accept() {
                        Ok(connection) => self.workers.push(Worker::start(
                            endpoint.clone(),
                            Some(connection),
                            None,
                            self.scope,
                            Repair::default(),
                        )?),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(_) => {
                            // Refusing one hostile or vanished peer cannot withdraw
                            // a healthy listener. Only its own evidence can do so.
                            listener.recheck()?;
                        }
                    }
                }
                Ok(())
            })();
            if serviced.is_err() {
                self.failed = true;
                let local = self.links.remove(index).source;
                for worker in &self.workers {
                    worker.stop_on(local);
                }
            } else {
                index += 1;
            }
        }
        if now >= self.beacon {
            self.beacon = now + Duration::from_secs(5);
        }
        if now >= self.dial
            && let Some(endpoint) = &self.endpoint
            && !self.links.is_empty()
            && self.workers.len() < WORKERS
            && let Some((address, repair)) = self.hints.next(now)
        {
            match Worker::start(endpoint.clone(), None, Some(address), self.scope, repair) {
                Ok(worker) => self.workers.push(worker),
                Err(error) => {
                    self.hints.finish(address, Repair::default(), false, now);
                    return Err(error);
                }
            }
            self.dial = now + Duration::from_millis(250);
        }
        Ok(())
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        // Listener drop invalidates every accepted proof before cancellation.
        self.links.clear();
        for worker in &self.workers {
            worker.stop();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.task.join();
        }
    }
}

#[cfg(test)]
mod tests;
