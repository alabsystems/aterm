// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The SAME accepted socket carries buffered observations only after native
//! retirement and journal settlement. Write EOF/ACK never supply that closure.

use super::{alive, preparation::Prepared, refused, wire};
use aterm_uds::CtlStream;
use std::{
    collections::VecDeque,
    io::{self, Read as _},
    net::Shutdown,
    time::Instant,
};

pub(super) struct Delivery {
    stream: CtlStream,
    output: VecDeque<Vec<u8>>,
    sent: usize,
    write_closed: bool,
    acknowledged: bool,
    acknowledgement: Vec<u8>,
    failed: bool,
}

/// Bounded observations only. No field constructs native retirement, a query
/// completion, a loan return or permission for another graph operation.
#[derive(Debug)]
pub(in crate::actor::owner) struct DeliveryObservation {
    pub(super) write_closed: bool,
    pub(super) acknowledged: bool,
    pub(super) acknowledgement_prefix: Vec<u8>,
    pub(super) failed: bool,
    pub(super) remaining_frames: usize,
    pub(super) current_frame_sent: usize,
}

impl Delivery {
    #[cfg(test)]
    pub(super) fn frames_for_test(stream: CtlStream, frames: Vec<Vec<u8>>) -> io::Result<Self> {
        Self::frames(stream, frames)
    }

    pub(super) fn observation(&self) -> DeliveryObservation {
        DeliveryObservation {
            write_closed: self.write_closed,
            acknowledged: self.acknowledged,
            acknowledgement_prefix: self.acknowledgement.clone(),
            failed: self.failed,
            remaining_frames: self.output.len(),
            current_frame_sent: self.sent,
        }
    }
    /// Private native issuer only: the caller must already own its actual
    /// retired query and have settled its original member journal. No external
    /// code can call this function with a decoded completion/receipt.
    pub(super) fn after_retirement(
        stream: CtlStream,
        prepared: &Prepared,
        started: trust_resource_controller::Started,
        terminal: wire::Terminal,
    ) -> io::Result<Self> {
        if !prepared.stdout.complete() || !prepared.stderr.complete() {
            return Err(refused("Bootstrap query original streams are incomplete"));
        }
        let mut frames = vec![
            wire::Event::Prepared(Box::new(prepared.inventory.clone()))
                .encode(&prepared.request, prepared.digest)?,
            wire::Event::Started(started).encode(&prepared.request, prepared.digest)?,
        ];
        prepared.stdout.append_frames(
            wire::Stream::Stdout,
            &prepared.request,
            prepared.digest,
            &mut frames,
        )?;
        prepared.stderr.append_frames(
            wire::Stream::Stderr,
            &prepared.request,
            prepared.digest,
            &mut frames,
        )?;
        frames.push(terminal.encode(&prepared.request, prepared.digest)?);
        Self::frames(stream, frames)
    }

    fn frames(stream: CtlStream, frames: Vec<Vec<u8>>) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        let mut output = VecDeque::new();
        for frame in frames {
            let length = u32::try_from(frame.len())
                .ok()
                .filter(|n| *n <= 1024 * 1024)
                .ok_or_else(|| refused("Bootstrap query frame exceeds its fixed bound"))?;
            let mut bytes = Vec::with_capacity(frame.len() + 4);
            bytes.extend(length.to_be_bytes());
            bytes.extend(frame);
            output.push_back(bytes);
        }
        Ok(Self {
            stream,
            output,
            sent: 0,
            write_closed: false,
            acknowledged: false,
            acknowledgement: Vec::new(),
            failed: false,
        })
    }

    /// Bound one actor turn. Every error permanently vetoes acknowledgement;
    /// the owner retains this exact socket and native input lifetime on failure.
    pub(super) fn poll(&mut self, deadline: Instant) -> io::Result<bool> {
        if self.failed {
            return Err(refused("Bootstrap query delivery is unknown"));
        }
        let result = self.poll_current(deadline);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn poll_current(&mut self, deadline: Instant) -> io::Result<bool> {
        alive(deadline)?;
        if self.acknowledged {
            return self.control_closed(deadline);
        }
        // ACK must follow physical write EOF. Early bytes, including a valid
        // ACK byte sent before the terminal, are not a completion handshake.
        if !self.write_closed {
            match self.peek_control(deadline)? {
                Control::Waiting => {}
                _ => return Err(refused("Bootstrap query peer closed or acknowledged early")),
            }
        }
        for _ in 0..32 {
            let Some(bytes) = self.output.front() else {
                break;
            };
            match aterm_uds::fdpass::send_vectored(
                &self.stream,
                &[io::IoSlice::new(&bytes[self.sent..])],
            ) {
                Ok(0) => return Err(refused("Bootstrap query delivery closed")),
                Ok(count) => {
                    self.sent += count;
                    if self.sent == bytes.len() {
                        self.output.pop_front();
                        self.sent = 0;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        if !self.output.is_empty() {
            return Ok(false);
        }
        if !self.write_closed {
            self.stream.shutdown(Shutdown::Write)?;
            self.write_closed = true;
        }
        self.receive_ack(deadline, |mut stream, bytes| stream.read(bytes))
    }

    // A private native-I/O seam permits actual one-byte socket reads in tests.
    // It supplies no native closure callback or operation authority.
    fn receive_ack(
        &mut self,
        deadline: Instant,
        mut read: impl FnMut(&CtlStream, &mut [u8]) -> io::Result<usize>,
    ) -> io::Result<bool> {
        if !self.write_closed {
            return Err(refused("Bootstrap query ACK preceded original write EOF"));
        }
        // Each positive read consumes at least one of exactly five ACK bytes;
        // malformed/trailing data refuses immediately. Never wait for new data.
        // Interrupted native calls retry only under this same original bound.
        loop {
            alive(deadline)?;
            if self.acknowledged {
                return self.control_closed(deadline);
            }
            let mut bytes = [0; 6];
            match read(&self.stream, &mut bytes) {
                Ok(0) => {
                    return Err(refused(
                        "Bootstrap query closed before its complete acknowledgement",
                    ));
                }
                Ok(count) => {
                    self.acknowledgement.extend_from_slice(&bytes[..count]);
                    let exact = [0, 0, 0, 1, 1];
                    if !exact.starts_with(&self.acknowledgement) {
                        return Err(refused("Bootstrap query acknowledgement differs"));
                    }
                    self.acknowledged = self.acknowledgement.len() == exact.len();
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    alive(deadline)?;
                    return Ok(false);
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn peek_control(&self, deadline: Instant) -> io::Result<Control> {
        use std::os::fd::AsRawFd as _;
        loop {
            alive(deadline)?;
            let mut byte = 0_u8;
            // SAFETY: recv peeks at most one byte into this live buffer through
            // our owned accepted socket; it changes no shared OFD status flags.
            let count = unsafe {
                libc::recv(
                    self.stream.as_raw_fd(),
                    (&raw mut byte).cast(),
                    1,
                    libc::MSG_PEEK | libc::MSG_DONTWAIT,
                )
            };
            let result = match count {
                0 => Ok(Control::Eof),
                1.. => Ok(Control::Bytes),
                _ => {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    if error.kind() == io::ErrorKind::WouldBlock {
                        Ok(Control::Waiting)
                    } else {
                        Err(error)
                    }
                }
            };
            alive(deadline)?;
            return result;
        }
    }
    pub(super) fn control_closed(&self, deadline: Instant) -> io::Result<bool> {
        if self.failed || !self.acknowledged {
            return Err(refused("Bootstrap query acknowledgement is incomplete"));
        }
        match self.peek_control(deadline)? {
            Control::Bytes => Err(refused("Bootstrap query has trailing control bytes")),
            Control::Waiting => Ok(false),
            Control::Eof => Ok(true),
        }
    }
    /// Explicitly close this same accepted stream during failed-attempt
    /// cleanup. This neither acknowledges a query nor retires native work.
    pub(super) fn abort(&self) -> io::Result<()> {
        self.stream.shutdown(Shutdown::Both)
    }
}

enum Control {
    Waiting,
    Bytes,
    Eof,
}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;
