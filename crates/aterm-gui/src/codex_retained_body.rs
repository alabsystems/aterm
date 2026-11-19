// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Complete G1/E1 offers on the original independently peer-checked callback.
//! A receipt correlates this exact physical offer; it never supplies Native authority.
use aterm_agent::harness::codex_source_registry::Error;
use aterm_uds::CtlStream;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::time::Instant;

const BODY_LIMIT: usize = 256 * 1024 * 1024;
#[derive(Clone, Copy)]
pub(super) enum Tag {
    Command,
    Entry,
}
impl Tag {
    fn bytes(self) -> &'static [u8; 2] {
        match self {
            Self::Command => b"G1",
            Self::Entry => b"E1",
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Prepared,
    Sent,
    Received,
    Uncertain,
}

// This owner retains the exact offered frame/socket across unknown outcomes.
// Receiving its nonce establishes correlation only, never constructor/UI authority.
pub(super) struct BodyDelivery {
    callback: CtlStream,
    frame: Vec<u8>,
    nonce: [u8; 32],
    phase: Phase,
}
impl BodyDelivery {
    pub(super) fn received(&self) -> bool {
        self.phase == Phase::Received
    }
    pub(super) fn new(
        callback: CtlStream,
        tag: Tag,
        operation: &str,
        body: &[u8],
    ) -> Result<Self, Error> {
        if operation.len() != 32
            || !operation
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || body.is_empty()
            || body.len() > BODY_LIMIT
        {
            return Err(Error::Denied);
        }
        let nonce = aterm_uds::rand::hex_token::<16>().map_err(|_| Error::Unavailable)?;
        let nonce: [u8; 32] = nonce
            .as_bytes()
            .try_into()
            .map_err(|_| Error::Unavailable)?;
        let mut frame = Vec::with_capacity(74 + body.len());
        frame.extend_from_slice(tag.bytes());
        frame.extend_from_slice(operation.as_bytes());
        frame.extend_from_slice(&nonce);
        frame.extend_from_slice(&(body.len() as u64).to_be_bytes());
        frame.extend_from_slice(body);
        callback
            .set_nonblocking(true)
            .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            callback,
            frame,
            nonce,
            phase: Phase::Prepared,
        })
    }
    pub(super) fn exchange(
        &mut self,
        deadline: Instant,
        mut check: impl FnMut() -> Result<(), Error>,
    ) -> Result<(), Error> {
        match self.phase {
            Phase::Received => return check(),
            Phase::Sent | Phase::Uncertain => return Err(Error::Unavailable),
            Phase::Prepared => {}
        }
        let result = (|| {
            let mut offset = 0;
            while offset < self.frame.len() {
                self.wait(aterm_uds::ready::Interest::Write, deadline, &mut check)?;
                match (&self.callback).write(&self.frame[offset..]) {
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
            // The SDK guards the complete header/body and requires Native EOF
            // BEFORE writing its acknowledgement; keeping both write halves
            // open here would deadlock the actual paired protocol.
            check()?;
            self.callback
                .shutdown(std::net::Shutdown::Write)
                .map_err(|_| Error::Unavailable)?;
            let mut receipt = [0; 32];
            let mut offset = 0;
            while offset < receipt.len() {
                self.wait(aterm_uds::ready::Interest::Read, deadline, &mut check)?;
                match aterm_uds::fdpass::recv_with_fds(&self.callback, &mut receipt[offset..], 0) {
                    Ok(got) if got.bytes > 0 && got.fds.is_empty() => offset += got.bytes,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    _ => return Err(Error::Unavailable),
                }
            }
            if !aterm_digest::ct_eq(&receipt, &self.nonce) {
                return Err(Error::Denied);
            }
            loop {
                self.wait(aterm_uds::ready::Interest::Read, deadline, &mut check)?;
                match aterm_uds::fdpass::recv_with_fds(&self.callback, &mut [0], 0) {
                    Ok(got) if got.bytes == 0 && got.fds.is_empty() => break,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    _ => return Err(Error::Denied),
                }
            }
            check()
        })();
        self.phase = if result.is_ok() {
            // Confirmed receipt+EOF needs no replay body. Uncertain exchanges
            // retain the exact original frame/socket; no reoffer is possible.
            self.frame.clear();
            self.frame.shrink_to_fit();
            Phase::Received
        } else {
            Phase::Uncertain
        };
        result
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
}

#[cfg(test)]
#[path = "codex_retained_body_tests.rs"]
mod tests;
