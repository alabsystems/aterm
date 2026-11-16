// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Physical frontend receipt on the original authenticated callback. The nonce
//! correlates this one descriptor transfer; it grants no frontend permission.
use aterm_agent::harness::codex_source_registry::Error;
use aterm_uds::CtlStream;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::time::Instant;

const HEADER_BYTES: usize = 34;
const RECEIPT_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Prepared,
    Sent,
    Received,
    Uncertain,
}

/// Retain the actual callback and frontend endpoint through physical receipt,
/// including a terminal uncertain transfer. A retry never sends another right.
pub(super) struct FrontendDelivery {
    callback: CtlStream,
    endpoint: CtlStream,
    header: [u8; HEADER_BYTES],
    phase: Phase,
}

impl FrontendDelivery {
    pub(super) fn new(callback: CtlStream, endpoint: CtlStream) -> Result<Self, Error> {
        let nonce = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        let mut header = [0; HEADER_BYTES];
        header[..2].copy_from_slice(b"F1");
        header[2..].copy_from_slice(nonce.as_bytes());
        callback
            .set_nonblocking(true)
            .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            callback,
            endpoint,
            header,
            phase: Phase::Prepared,
        })
    }

    pub(super) fn exchange(
        &mut self,
        deadline: Instant,
        mut check: impl FnMut() -> Result<(), Error>,
    ) -> Result<(), Error> {
        match self.phase {
            Phase::Received => check(),
            Phase::Sent | Phase::Uncertain => Err(Error::Unavailable),
            Phase::Prepared => {
                self.offer(deadline, &mut check)?;
                self.receipt(deadline, &mut check)
            }
        }
    }

    fn wait(
        &self,
        interest: aterm_uds::ready::Interest,
        deadline: Instant,
        check: &mut impl FnMut() -> Result<(), Error>,
    ) -> Result<(), Error> {
        check()?;
        aterm_uds::ready::until(self.callback.as_fd(), interest, deadline)
            .map_err(|_| Error::Unavailable)?;
        check()
    }

    fn offer(
        &mut self,
        deadline: Instant,
        check: &mut impl FnMut() -> Result<(), Error>,
    ) -> Result<(), Error> {
        if self.phase != Phase::Prepared {
            return Err(Error::Unavailable);
        }
        let result = (|| {
            let mut offset = 0;
            while offset < self.header.len() {
                self.wait(aterm_uds::ready::Interest::Write, deadline, check)?;
                let sent = if offset == 0 {
                    aterm_uds::fdpass::send_with_fds(
                        &self.callback,
                        &self.header,
                        &[self.endpoint.as_fd()],
                    )
                } else {
                    // A positive first send has already transferred the right.
                    // Complete a partial header without sending another right.
                    (&self.callback).write(&self.header[offset..])
                };
                match sent {
                    Ok(0) => return Err(Error::Unavailable),
                    Ok(count) => {
                        self.phase = Phase::Sent;
                        offset += count;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return Err(Error::Unavailable),
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.phase = Phase::Uncertain;
        }
        result
    }

    fn receipt(
        &mut self,
        deadline: Instant,
        check: &mut impl FnMut() -> Result<(), Error>,
    ) -> Result<(), Error> {
        if self.phase != Phase::Sent {
            return Err(Error::Unavailable);
        }
        let result = (|| {
            let mut receipt = [0; RECEIPT_BYTES];
            let mut offset = 0;
            while offset < receipt.len() {
                self.wait(aterm_uds::ready::Interest::Read, deadline, check)?;
                match aterm_uds::fdpass::recv_with_fds(&self.callback, &mut receipt[offset..], 0) {
                    Ok(received) if received.bytes != 0 && received.fds.is_empty() => {
                        offset += received.bytes;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    _ => return Err(Error::Unavailable),
                }
            }
            if receipt != self.header[2..] {
                return Err(Error::Denied);
            }
            // Source half-closes its write side after the fixed receipt. Require
            // that boundary: a matching prefix plus extra bytes/rights is not
            // this protocol, and a missing boundary stays uncertain at deadline.
            loop {
                self.wait(aterm_uds::ready::Interest::Read, deadline, check)?;
                match aterm_uds::fdpass::recv_with_fds(&self.callback, &mut [0], 0) {
                    Ok(received) if received.bytes == 0 && received.fds.is_empty() => break,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    _ => return Err(Error::Denied),
                }
            }
            // Physical receipt is only correlation. Original SDK, controller,
            // backend and actual foreground admission still decide completion.
            check()?;
            Ok(())
        })();
        self.phase = if result.is_ok() {
            Phase::Received
        } else {
            Phase::Uncertain
        };
        result
    }
}

#[cfg(test)]
#[path = "codex_retained_delivery_tests.rs"]
mod tests;
