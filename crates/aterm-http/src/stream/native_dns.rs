// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows system resolution, shared by callers of the same outstanding lookup.

use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs as _};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

/// Resolve on a helper without starting another while this host is still resolving.
pub(super) fn resolve(host: &str, within: Duration) -> io::Result<Vec<IpAddr>> {
    resolve_within(host, 0, within, |host, port| (host, port).to_socket_addrs())
        .map(|addresses| addresses.into_iter().map(|address| address.ip()).collect())
}

/// Resolve `host:port` through `resolver` on a helper thread, waiting at most
/// `within` for its answer; a lookup of `host:port` whose helper still runs is
/// waited on, not started again ([`Flights`]).
fn resolve_within<F, I>(
    host: &str,
    port: u16,
    within: Duration,
    resolver: F,
) -> io::Result<Vec<SocketAddr>>
where
    F: FnOnce(&str, u16) -> io::Result<I> + Send + 'static,
    I: Iterator<Item = SocketAddr>,
{
    resolve_within_using(
        host,
        port,
        within,
        resolver,
        |lookup| {
            std::thread::Builder::new()
                .name("aterm-http-dns".into())
                .spawn(lookup)
                .map(drop)
        },
        &FLIGHTS,
    )
}

/// This process's lookups under way ([`Flights`]).
static FLIGHTS: Flights = Flights::new();

/// The lookups under way, one per host and port. A lookup that finds one for its
/// host and port waits for that helper's answer and starts no helper of its own:
/// macOS holds a `.local` lookup (`getaddrinfo` waits in `mdns_addrinfo`) while it
/// asks the person whether aterm may find devices on the local network, so a
/// helper left to finish alone may not finish for as long as nobody answers. E3's
/// guest (2026-11-05) grew two such helpers a second, one pair per vault read;
/// now each name keeps one. An answer goes to the lookups waiting on it and is
/// kept for none after: the next lookup asks afresh.
struct Flights {
    under_way: Mutex<Vec<(String, u16, Arc<Flight>)>>,
}

/// One lookup under way, and its helper's answer once it gave one.
struct Flight {
    answer: Mutex<Option<Answer>>,
    landed: Condvar,
    /// The lookups waiting on it now ([`Flights::waiting`]).
    waiting: AtomicUsize,
}

/// A helper's answer, kept so each lookup waiting on it gets its own copy.
type Answer = Result<Vec<SocketAddr>, (io::ErrorKind, String)>;

impl Flights {
    const fn new() -> Self {
        Self {
            under_way: Mutex::new(Vec::new()),
        }
    }

    /// The flight of `host:port`, and whether this lookup started it, and so
    /// starts its helper.
    fn join(&self, host: &str, port: u16) -> (Arc<Flight>, bool) {
        let mut under_way = self
            .under_way
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some((_, _, flight)) = under_way
            .iter()
            .find(|(named, at, _)| named == host && *at == port)
        {
            return (Arc::clone(flight), false);
        }
        let flight = Arc::new(Flight {
            answer: Mutex::new(None),
            landed: Condvar::new(),
            waiting: AtomicUsize::new(0),
        });
        under_way.push((host.to_owned(), port, Arc::clone(&flight)));
        (flight, true)
    }

    /// `flight`'s helper answered, or never started: the lookups waiting on it get
    /// `answer`, and the next lookup of its host and port starts a helper of its own.
    fn land(&self, flight: &Arc<Flight>, answer: Answer) {
        self.under_way
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(_, _, under_way)| !Arc::ptr_eq(under_way, flight));
        *flight.answer.lock().unwrap_or_else(PoisonError::into_inner) = Some(answer);
        flight.landed.notify_all();
    }

    /// How many lookups wait on the flight of `host:port` now (tests).
    #[cfg(test)]
    fn waiting(&self, host: &str, port: u16) -> usize {
        self.under_way
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(named, at, _)| named == host && *at == port)
            .map_or(0, |(_, _, flight)| flight.waiting.load(Ordering::SeqCst))
    }
}

impl Flight {
    /// Its answer, waiting at most `within` for it: `None` once that passed first.
    fn wait(&self, within: Duration) -> Option<Answer> {
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let answer = self.answer.lock().unwrap_or_else(PoisonError::into_inner);
        let (answer, _) = self
            .landed
            .wait_timeout_while(answer, within, |answer| answer.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        let answer = answer.clone();
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        answer
    }
}

/// A started helper's duty to land its flight: by its answer ([`Self::land`]), or,
/// should the resolver unwind first, by saying it ended without one, so no flight
/// stays under way with no helper to answer it.
struct Landing {
    flights: &'static Flights,
    flight: Arc<Flight>,
    host: String,
    landed: bool,
}

impl Landing {
    fn land(mut self, answer: Answer) {
        self.flights.land(&self.flight, answer);
        self.landed = true;
    }
}

impl Drop for Landing {
    fn drop(&mut self) {
        if !self.landed {
            let words = "the resolver ended without an answer".to_owned();
            self.flights
                .land(&self.flight, Err((io::ErrorKind::Other, words)));
        }
    }
}

/// The resolver path with helper creation and the flights injected, so a refused
/// spawn can be checked without exhausting the process's threads or calling the
/// system resolver, and each test keeps its lookups apart.
fn resolve_within_using<F, I>(
    host: &str,
    port: u16,
    within: Duration,
    resolver: F,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<()>,
    flights: &'static Flights,
) -> io::Result<Vec<SocketAddr>>
where
    F: FnOnce(&str, u16) -> io::Result<I> + Send + 'static,
    I: Iterator<Item = SocketAddr>,
{
    /// One lookup, leaving host-specific error wording to the caller.
    fn lookup<F, I>(resolver: F, host: &str, port: u16) -> io::Result<Vec<SocketAddr>>
    where
        F: FnOnce(&str, u16) -> io::Result<I>,
        I: Iterator<Item = SocketAddr>,
    {
        resolver(host, port).map(Iterator::collect)
    }
    let (flight, starts) = flights.join(host, port);
    if starts {
        let (helper_host, helper_flight) = (host.to_owned(), Arc::clone(&flight));
        // A failed spawn drops this closure without invoking the resolver, and the
        // flight lands that failure below.
        let spawned = spawn(Box::new(move || {
            let landing = Landing {
                flights,
                flight: helper_flight,
                host: helper_host,
                landed: false,
            };
            let answer = lookup(resolver, &landing.host, port)
                .map_err(|error| (error.kind(), error.to_string()));
            landing.land(answer);
        }));
        if let Err(error) = spawned {
            let error = io::Error::new(
                error.kind(),
                format!("could not start the resolver: {error}"),
            );
            flights.land(&flight, Err((error.kind(), error.to_string())));
            return Err(error);
        }
    }
    match flight.wait(within) {
        Some(Ok(addrs)) => Ok(addrs),
        Some(Err((kind, words))) => Err(io::Error::new(kind, words)),
        None => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("no answer within {}ms", within.as_millis()),
        )),
    }
}

#[cfg(test)]
#[path = "native_dns_tests.rs"]
mod tests;
