// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One monotonic budget for an authorization and its nested network operations.
//! Filesystem calls and the separately bounded cleanup are not preempted here.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

pub(super) const EXPIRED: &str = "OAuth operation deadline expired";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Expired;

impl Expired {
    pub(super) fn words(self) -> &'static str {
        EXPIRED
    }

    fn io(self) -> io::Error {
        io::Error::new(io::ErrorKind::TimedOut, EXPIRED)
    }
}

/// Copying this value retains the same deadline and clock; it never starts a new budget.
#[derive(Clone, Copy)]
pub(super) struct Deadline<'a> {
    end: Instant,
    clock: &'a dyn Fn() -> Instant,
}

impl<'a> Deadline<'a> {
    pub(super) fn new(within: Duration, clock: &'a dyn Fn() -> Instant) -> Self {
        let now = clock();
        Self {
            end: now.checked_add(within).unwrap_or(now),
            clock,
        }
    }

    pub(super) fn now(self) -> Instant {
        (self.clock)()
    }
    pub(super) fn end(self) -> Instant {
        self.end
    }
    pub(super) fn expired(self) -> bool {
        self.now() >= self.end
    }

    pub(super) fn remaining(self) -> Result<Duration, Expired> {
        let left = self.end.saturating_duration_since(self.now());
        if left.is_zero() {
            Err(Expired)
        } else {
            Ok(left)
        }
    }

    pub(super) fn left(self) -> Result<Duration, &'static str> {
        self.remaining().map_err(Expired::words)
    }

    pub(super) fn capped(self, ceiling: Duration) -> Result<Duration, Expired> {
        self.remaining().map(|left| left.min(ceiling))
    }

    pub(super) fn limited(self, ceiling: Duration) -> Self {
        let now = self.now();
        Self {
            end: self.end.min(now.checked_add(ceiling).unwrap_or(now)),
            clock: self.clock,
        }
    }

    pub(super) fn pause(self, interval: Duration) -> Result<(), Expired> {
        let pause = self.capped(interval)?;
        if !pause.is_zero() {
            std::thread::sleep(pause);
        }
        Ok(())
    }
}

/// A socket whose every underlying read/write rechecks the shared budget, including calls
/// made by BufReader, read_to_end and write_all. None retains the convenience caller's caps.
pub(super) struct Stream<'a> {
    stream: TcpStream,
    deadline: Option<Deadline<'a>>,
    read_cap: Duration,
    write_cap: Duration,
}

impl<'a> Stream<'a> {
    pub(super) fn connect(
        addr: &SocketAddr,
        deadline: Option<Deadline<'a>>,
        connect_cap: Duration,
        io_cap: Duration,
    ) -> io::Result<Self> {
        let within = deadline.map_or(Ok(connect_cap), |d| {
            d.capped(connect_cap).map_err(Expired::io)
        })?;
        Ok(Self {
            stream: TcpStream::connect_timeout(addr, within)?,
            deadline,
            read_cap: io_cap,
            write_cap: io_cap,
        })
    }

    pub(super) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
            deadline: self.deadline,
            read_cap: self.read_cap,
            write_cap: self.write_cap,
        })
    }

    pub(super) fn timeout(&self, ceiling: Duration) -> io::Result<Duration> {
        self.deadline
            .map_or(Ok(ceiling), |d| d.capped(ceiling).map_err(Expired::io))
    }
}

impl Read for Stream<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream
            .set_read_timeout(Some(self.timeout(self.read_cap)?))?;
        self.stream.read(bytes)
    }
}

impl Write for Stream<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream
            .set_write_timeout(Some(self.timeout(self.write_cap)?))?;
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream
            .set_write_timeout(Some(self.timeout(self.write_cap)?))?;
        self.stream.flush()
    }
}

pub(super) fn loopback_addr(host: &str, port: u16) -> Option<SocketAddr> {
    let host = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    let ip = if host.eq_ignore_ascii_case("localhost") {
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
    } else {
        host.parse().ok()?
    };
    ip.is_loopback().then_some(SocketAddr::new(ip, port))
}
