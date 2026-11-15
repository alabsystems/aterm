// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded address observation from the retained child's actual diagnostics.
use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::time::Instant;

const MAX_BYTES: usize = 64 * 1024;
const MAX_LINE: usize = 8 * 1024;

#[derive(Default)]
pub(super) struct Observation {
    failed: bool,
    header: bool,
    line: Vec<u8>,
    bytes: usize,
    address: Option<SocketAddr>,
}

impl Observation {
    pub(super) fn address(&self) -> Option<SocketAddr> {
        (!self.failed).then_some(self.address).flatten()
    }

    pub(super) fn read(
        &mut self,
        reader: &mut impl Read,
        descriptor: BorrowedFd<'_>,
        deadline: Instant,
        mut current: impl FnMut() -> bool,
    ) -> io::Result<SocketAddr> {
        loop {
            if self.failed {
                return Err(invalid());
            }
            if !current() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "backend startup owner changed",
                ));
            }
            if let Some(address) = self.address {
                return Ok(address);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "backend listener observation timed out",
                ));
            }
            let mut poll = libc::pollfd {
                fd: descriptor.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // Recheck custody at least every 100ms, including a silent child.
            let timeout = aterm_uds::deadline::poll_timeout_ms(left).min(100);
            // SAFETY: one initialized pollfd names the request's retained pipe.
            let ready = unsafe { libc::poll(&mut poll, 1, timeout) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if ready == 0 {
                continue;
            }
            if poll.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "backend diagnostics descriptor ended",
                ));
            }
            let mut bytes = [0; 512];
            match reader.read(&mut bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "backend ended before listener observation",
                    ));
                }
                Ok(count) => self.push(&bytes[..count])?,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }

    fn push(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.failed {
            return Err(invalid());
        }
        let result = self.parse(bytes);
        if result.is_err() {
            self.failed = true;
            self.address = None;
        }
        result
    }

    fn parse(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.bytes = self.bytes.checked_add(bytes.len()).ok_or_else(invalid)?;
        if self.bytes > MAX_BYTES {
            return Err(invalid());
        }
        for byte in bytes {
            if *byte != b'\n' {
                if self.line.len() >= MAX_LINE {
                    return Err(invalid());
                }
                self.line.push(*byte);
                continue;
            }
            let text =
                crate::harness::login_drive::strip_ansi(&String::from_utf8_lossy(&self.line));
            self.line.clear();
            let text = text.trim_end_matches('\r');
            if text == "codex app-server (WebSockets)" {
                if self.header {
                    return Err(invalid());
                }
                self.header = true;
            } else if let Some(value) = text.strip_prefix("  listening on: ws://") {
                if !self.header || self.address.is_some() {
                    return Err(invalid());
                }
                let address: SocketAddr = value.parse().map_err(|_| invalid())?;
                if address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) || address.port() == 0 {
                    return Err(invalid());
                }
                self.address = Some(address);
            }
        }
        Ok(())
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid or excessive backend startup diagnostics",
    )
}

#[cfg(test)]
#[path = "codex_source_backend_endpoint_tests.rs"]
mod tests;
