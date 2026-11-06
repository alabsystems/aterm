// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Incremental private lifecycle transport. A slow reader cannot stall admission
//! or prevent the host from servicing another job's cancellation or timeout.
use aterm_types::resource_admission::{Binding, Cancel, Event, EventFrame};
use aterm_uds::CtlStream;
use std::{
    collections::VecDeque,
    io::{self, IoSlice, Read as _},
    os::fd::OwnedFd,
};

pub(in super::super) struct Events {
    stream: CtlStream,
    binding: Binding,
    output: VecDeque<Vec<u8>>,
    sent: usize,
    input: Vec<u8>,
    failed: bool,
}
impl Events {
    pub(in super::super) fn new(fd: OwnedFd, binding: Binding) -> io::Result<Self> {
        let stream = CtlStream::from(fd);
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            binding,
            output: VecDeque::new(),
            sent: 0,
            input: Vec::new(),
            failed: false,
        })
    }
    pub(in super::super) fn emit(&mut self, event: Event) -> io::Result<()> {
        // Queued, Started and Finished are bounded lifecycle facts, not logs.
        // A refusal replaces execution, so no attempt needs more than three.
        if self.output.len() >= 3 || self.failed {
            return Err(io::Error::other("private lifecycle unavailable"));
        }
        let bytes = EventFrame {
            binding: self.binding.clone(),
            event,
        }
        .encode()?;
        let mut frame = Vec::with_capacity(4 + bytes.len());
        frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        frame.extend_from_slice(&bytes);
        self.output.push_back(frame);
        self.flush();
        Ok(())
    }
    pub(in super::super) fn flush(&mut self) {
        while let Some(frame) = self.output.front() {
            match aterm_uds::fdpass::send_vectored(
                &self.stream,
                &[IoSlice::new(&frame[self.sent..])],
            ) {
                Ok(0) => {
                    self.failed = true;
                    break;
                }
                Ok(n) => {
                    self.sent += n;
                    if self.sent == frame.len() {
                        self.output.pop_front();
                        self.sent = 0;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.failed = true;
                    break;
                }
            }
        }
    }
    pub(in super::super) fn abandon(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        self.failed = true;
    }
    pub(in super::super) fn drained(&mut self) -> bool {
        self.flush();
        self.failed || self.output.is_empty()
    }
    pub(in super::super) fn cancelled(&mut self) -> bool {
        self.flush();
        if self.failed {
            return true;
        }
        // The sole inbound record is an exact cancellation for this attempt.
        // Read at most its exact encoded length, preserving partial arrivals.
        let expected = match (Cancel {
            binding: self.binding.clone(),
        })
        .encode()
        {
            Ok(bytes) => bytes.len(),
            Err(_) => {
                self.failed = true;
                return true;
            }
        };
        let mut buffer = [0u8; 256];
        loop {
            let room = (expected + 4).saturating_sub(self.input.len());
            if room == 0 {
                break;
            }
            match (&self.stream).read(&mut buffer[..room.min(256)]) {
                Ok(0) => {
                    self.failed = true;
                    return true;
                }
                Ok(n) => self.input.extend_from_slice(&buffer[..n]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.failed = true;
                    return true;
                }
            }
        }
        if self.input.len() < 4 {
            return false;
        }
        let mut header = [0u8; 4];
        header.copy_from_slice(&self.input[..4]);
        if u32::from_be_bytes(header) as usize != expected {
            self.failed = true;
            return true;
        }
        if self.input.len() < expected + 4 {
            return false;
        }
        // Malformed input also withdraws this controller's interest. It never
        // names another job, frees a claim, or authorizes numeric signaling.
        let _valid = Cancel::decode(&self.input[4..], &self.binding).is_ok();
        self.failed = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    fn pair() -> (Events, CtlStream) {
        let (host, client) = CtlStream::pair().unwrap();
        let binding = Binding {
            session: "s-a".into(),
            attempt: [7; 16],
        };
        (Events::new(host.into(), binding).unwrap(), client)
    }
    #[test]
    fn cancellation_preserves_partial_frame_without_blocking_another_tick() {
        let (mut events, mut client) = pair();
        let cancel = Cancel {
            binding: events.binding.clone(),
        }
        .encode()
        .unwrap();
        let header = (cancel.len() as u32).to_be_bytes();
        client.write_all(&header[..2]).unwrap();
        assert!(!events.cancelled());
        client.write_all(&header[2..]).unwrap();
        client.write_all(&cancel[..3]).unwrap();
        assert!(!events.cancelled());
        client.write_all(&cancel[3..]).unwrap();
        assert!(events.cancelled());
    }
    #[test]
    fn disconnect_and_oversized_header_withdraw_only_the_controller() {
        let (mut events, client) = pair();
        drop(client);
        assert!(events.cancelled());
        let (mut events, mut client) = pair();
        client.write_all(&u32::MAX.to_be_bytes()).unwrap();
        assert!(events.cancelled());
        assert!(events.input.len() <= 256);
    }
}
