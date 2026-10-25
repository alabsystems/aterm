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
                let success = result.is_ok() && !control.stopped && repair.actor().is_some();
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
    listener: Listener,
    hints: Multicast,
}

pub(super) struct Transport {
    endpoint: Arc<Endpoint>,
    links: Vec<Link>,
    hints: Hints,
    workers: Vec<Worker>,
    beacon: Instant,
    dial: Instant,
    rotate: Instant,
    scope: Scope,
}
impl Transport {
    pub(super) fn open(endpoint: Endpoint, addresses: &[SocketAddr]) -> Result<Self> {
        let mut token = [0; 32];
        SystemRandom::new()
            .fill(&mut token)
            .map_err(|_| crate::Error::new("discovery randomness unavailable"))?;
        let now = Instant::now();
        let mut transport = Self {
            endpoint: Arc::new(endpoint),
            links: Vec::new(),
            hints: Hints::new(token),
            workers: Vec::new(),
            beacon: now,
            dial: now,
            rotate: now + Duration::from_secs(300),
            scope: Scope::Lan,
        };
        transport.refresh(addresses);
        Ok(transport)
    }
    pub(super) fn refresh(&mut self, addresses: &[SocketAddr]) {
        for address in addresses {
            let IpAddr::V4(source) = address.ip() else {
                continue;
            };
            if self.links.iter().any(|link| link.source == address.ip()) {
                continue;
            }
            if let Ok(listener) = Listener::bind(*address, self.scope)
                && let Ok(hints) = Multicast::bind(source, GROUP, PORT, self.scope)
            {
                self.links.push(Link {
                    source: address.ip(),
                    listener,
                    hints,
                });
                self.beacon = Instant::now();
            }
        }
    }
    pub(super) fn interfaces(&self) -> usize {
        self.links.len()
    }
    pub(super) fn peers(&self) -> usize {
        self.hints.count()
    }
    pub(super) fn tick(&mut self, now: Instant) -> io::Result<()> {
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
                    Ok((Some(address), repair, success)) => {
                        self.hints.finish(address, repair, success, now)
                    }
                    Err(_) => {
                        if let Some(address) = worker.target {
                            self.hints.finish(address, Repair::default(), false, now);
                        }
                    }
                    Ok((None, _, _)) => {}
                }
            } else {
                index += 1;
            }
        }
        let mut index = 0;
        while index < self.links.len() {
            let link = &self.links[index];
            let serviced = (|| -> io::Result<()> {
                link.listener.recheck()?;
                link.hints.recheck()?;
                if now >= self.beacon {
                    link.hints
                        .send(&self.hints.wire(link.listener.local_addr()?.port()))?;
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
                }
                // Unproved inbound peers cannot occupy the outbound repair budget.
                if self.workers.len() < WORKERS
                    && self.workers.iter().filter(|worker| worker.incoming).count() < WORKERS / 2
                {
                    match link.listener.accept() {
                        Ok(connection) => self.workers.push(Worker::start(
                            self.endpoint.clone(),
                            Some(connection),
                            None,
                            self.scope,
                            Repair::default(),
                        )?),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                        Err(_) => {
                            // Refusing one hostile or vanished peer cannot withdraw
                            // a healthy listener. Only its own evidence can do so.
                            link.listener.recheck()?;
                        }
                    }
                }
                Ok(())
            })();
            if serviced.is_err() {
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
            && !self.links.is_empty()
            && self.workers.len() < WORKERS
            && let Some((address, repair)) = self.hints.next(now)
        {
            match Worker::start(
                self.endpoint.clone(),
                None,
                Some(address),
                self.scope,
                repair,
            ) {
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
