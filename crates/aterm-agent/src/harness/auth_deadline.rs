// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One budget for an authorization and its nested operations, including host sleep.
//! Filesystem calls and the separately bounded cleanup are not preempted here.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use super::wake_deadline::WakeDeadline;

pub(super) const EXPIRED: &str = "OAuth operation deadline expired";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Expired;

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
pub struct Deadline<'a> {
    end: Instant,
    clock: &'a dyn Fn() -> Instant,
    continuous_end: Option<u64>,
    continuous: &'a dyn Fn() -> Option<u64>,
    waking: Option<WakeDeadline>,
}

/// Pure endpoints retained by an existing owner without borrowing its clocks.
#[derive(Clone, Copy, Debug)]
pub(super) struct Bounds {
    end: Instant,
    continuous_end: Option<u64>,
    waking: Option<WakeDeadline>,
}

impl Bounds {
    pub(super) fn remaining(self, now: Instant, continuous: Option<u64>) -> Duration {
        let awake = self.end.saturating_duration_since(now);
        let left = match (self.continuous_end, continuous) {
            (Some(end), Some(now)) => awake.min(Duration::from_nanos(end.saturating_sub(now))),
            _ => awake,
        };
        self.waking
            .map_or(left, |deadline| left.min(deadline.remaining()))
    }
}

impl Deadline<'static> {
    /// One budget from now, counting host sleep where a continuous clock is available.
    #[must_use]
    pub fn after(within: Duration) -> Self {
        let parent = WakeDeadline::after(within);
        Self {
            waking: Some(parent),
            ..Self::with_clocks(within, &Instant::now, &aterm_uds::process::continuous_ns)
        }
    }

    /// An external monotonic ceiling, also bounded by its remaining continuous time.
    #[must_use]
    pub fn until(end: Instant) -> Self {
        let continuous = aterm_uds::process::continuous_ns();
        let left = end.saturating_duration_since(Instant::now());
        Self {
            end,
            clock: &Instant::now,
            continuous_end: continuous
                .map(|now| now.saturating_add(u64::try_from(left.as_nanos()).unwrap_or(u64::MAX))),
            continuous: &aterm_uds::process::continuous_ns,
            waking: None,
        }
    }
}

impl<'a> Deadline<'a> {
    pub(super) fn new(within: Duration, clock: &'a dyn Fn() -> Instant) -> Self {
        Self::with_clocks(within, clock, &aterm_uds::process::continuous_ns)
    }

    pub(super) fn with_clocks(
        within: Duration,
        clock: &'a dyn Fn() -> Instant,
        continuous: &'a dyn Fn() -> Option<u64>,
    ) -> Self {
        let now = clock();
        Self {
            end: now.checked_add(within).unwrap_or(now),
            clock,
            continuous_end: continuous().map(|now| {
                now.saturating_add(u64::try_from(within.as_nanos()).unwrap_or(u64::MAX))
            }),
            continuous,
            waking: None,
        }
    }

    /// `deadline`, which counts the time the host slept, on `clock`: what it leaves now, from
    /// `clock`'s now, and it has passed once `deadline` has, so a wait that looks at it after the
    /// host slept past `deadline` ends.
    pub(super) fn waking(deadline: WakeDeadline, clock: &'a dyn Fn() -> Instant) -> Self {
        Self {
            waking: Some(deadline),
            ..Self::new(deadline.remaining(), clock)
        }
    }

    pub(super) fn now(self) -> Instant {
        (self.clock)()
    }

    /// Whether either clock has exhausted the original budget.
    #[must_use]
    pub fn expired(self) -> bool {
        self.remaining().is_err()
    }

    /// The smaller amount left on the awake and continuous clocks.
    /// # Errors
    /// Either clock has reached its deadline, including a zero or unrepresentable budget.
    pub fn remaining(self) -> Result<Duration, Expired> {
        let left = self.bounds().remaining(self.now(), (self.continuous)());
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
        let waking = self.waking.map(|parent| parent.within(ceiling));
        let now = self.now();
        Self {
            end: self.end.min(now.checked_add(ceiling).unwrap_or(now)),
            clock: self.clock,
            continuous_end: match (self.continuous_end, (self.continuous)()) {
                (Some(end), Some(now)) => Some(end.min(
                    now.saturating_add(u64::try_from(ceiling.as_nanos()).unwrap_or(u64::MAX)),
                )),
                (end, _) => end,
            },
            continuous: self.continuous,
            waking,
        }
    }

    pub(super) fn counts_sleep(self) -> bool {
        self.continuous_end.is_some() || self.waking.is_some_and(WakeDeadline::counts_sleep)
    }

    /// The original transferable parent, retaining its local limits and exact awake ceiling.
    /// An injected clock without such a parent cannot be silently projected to another thread.
    pub(super) fn wake_parent(self) -> Option<WakeDeadline> {
        self.waking.map(|parent| parent.limited_by_awake(self.end))
    }

    /// A leaf's monotonic I/O ceiling, shortened by the original parent budget.
    pub(super) fn end(self) -> Instant {
        if self.waking.is_some() {
            self.io_end()
        } else {
            self.end
        }
    }

    pub(super) fn bounds(self) -> Bounds {
        Bounds {
            end: self.end,
            continuous_end: self.continuous_end,
            waking: self.waking,
        }
    }

    /// A fresh I/O ceiling shortened by time already spent asleep; this cannot extend `end`.
    pub(super) fn io_end(self) -> Instant {
        let now = self.now();
        self.end.min(
            now.checked_add(self.remaining().unwrap_or_default())
                .unwrap_or(now),
        )
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
    #[cfg(test)]
    after_read: Option<&'a dyn Fn(usize)>,
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
            #[cfg(test)]
            after_read: None,
        })
    }

    pub(super) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
            deadline: self.deadline,
            read_cap: self.read_cap,
            write_cap: self.write_cap,
            #[cfg(test)]
            after_read: None,
        })
    }

    pub(super) fn timeout(&self, ceiling: Duration) -> io::Result<Duration> {
        self.deadline
            .map_or(Ok(ceiling), |d| d.capped(ceiling).map_err(Expired::io))
    }

    #[cfg(test)]
    pub(super) fn observe_read(&mut self, observe: &'a dyn Fn(usize)) {
        self.after_read = Some(observe);
    }
}

impl Read for Stream<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let deadline = self.deadline;
        let read = io_with_budget(deadline, self.read_cap, |within| {
            self.stream.set_read_timeout(Some(within))?;
            self.stream.read(bytes)
        })?;
        #[cfg(test)]
        if let Some(observe) = self.after_read {
            observe(read);
        }
        Ok(read)
    }
}

impl Write for Stream<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let deadline = self.deadline;
        io_with_budget(deadline, self.write_cap, |within| {
            self.stream.set_write_timeout(Some(within))?;
            self.stream.write(bytes)
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream
            .set_write_timeout(Some(self.timeout(self.write_cap)?))?;
        self.stream.flush()
    }
}

// A kernel's socket timeout may stop with the host's awake clock. A short wait is only a
// clock sample: retry the same I/O on the same socket, preserving partial progress and its
// original cap, until that cap or the authorization budget actually expires.
pub(super) fn io_with_budget<T>(
    deadline: Option<Deadline<'_>>,
    cap: Duration,
    mut operation: impl FnMut(Duration) -> io::Result<T>,
) -> io::Result<T> {
    let Some(deadline) = deadline.map(|d| d.limited(cap)) else {
        return operation(cap);
    };
    loop {
        let left = deadline.remaining().map_err(Expired::io)?;
        let tick = if deadline.counts_sleep() {
            left.min(Duration::from_millis(100))
        } else {
            left
        };
        match operation(tick) {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) => {}
            result => return result,
        }
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
