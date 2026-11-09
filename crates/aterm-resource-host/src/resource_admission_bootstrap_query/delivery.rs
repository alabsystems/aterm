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
mod tests {
    use super::*;
    use std::{io::Write as _, time::Duration};

    #[test]
    fn actual_socket_requires_eof_before_one_exact_ack_and_keeps_accepted_owner() {
        let (mut client, server) = CtlStream::pair().unwrap();
        let mut delivery =
            Delivery::frames(server, vec![b"native buffered output".to_vec()]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        assert!(!delivery.poll(deadline).unwrap());
        let mut actual = Vec::new();
        client.read_to_end(&mut actual).unwrap();
        assert_eq!(&actual[..4], &(22u32.to_be_bytes()));
        assert_eq!(&actual[4..], b"native buffered output");
        client.write_all(&[0, 0, 0, 1, 1]).unwrap();
        assert!(!delivery.poll(deadline).unwrap());
        assert!(!delivery.control_closed(deadline).unwrap());
        client.write_all(&[1]).unwrap();
        assert!(delivery.poll(deadline).is_err());
        assert!(delivery.poll(deadline).is_err());
    }

    #[test]
    fn early_ack_and_peer_loss_never_become_a_positive_delivery() {
        for close in [false, true] {
            let (mut client, server) = CtlStream::pair().unwrap();
            if !close {
                client.write_all(&[0, 0, 0, 1, 1]).unwrap();
            }
            if close {
                drop(client);
            }
            let mut delivery = Delivery::frames(server, vec![b"not sent".to_vec()]).unwrap();
            assert!(
                delivery
                    .poll(Instant::now() + Duration::from_secs(60))
                    .is_err()
            );
            assert!(
                delivery
                    .poll(Instant::now() + Duration::from_secs(60))
                    .is_err()
            );
        }
    }

    #[test]
    fn exact_fragmented_ack_requires_the_original_peer_eof() {
        let (mut client, server) = CtlStream::pair().unwrap();
        let mut delivery = Delivery::frames(server, vec![b"actual frame".to_vec()]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        assert!(!delivery.poll(deadline).unwrap());
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).unwrap();
        for part in [&[0, 0][..], &[0, 1][..], &[1][..]] {
            client.write_all(part).unwrap();
            assert!(!delivery.poll(deadline).unwrap());
        }
        assert!(!delivery.control_closed(deadline).unwrap());
        // Publish the original peer's write EOF synchronously. Dropping its
        // socket alone may leave nonblocking observation briefly WouldBlock.
        client.shutdown(Shutdown::Write).unwrap();
        drop(client);
        assert!(delivery.poll(deadline).unwrap());
        assert!(delivery.control_closed(deadline).unwrap());
    }

    #[test]
    fn queued_fragmented_ack_and_eof_are_observed_before_the_next_socket_message() {
        let (mut client, server) = CtlStream::pair().unwrap();
        let (mut next_sender, mut next_receiver) = CtlStream::pair().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut delivery = Delivery::frames(server, vec![b"previous query data".to_vec()]).unwrap();
        assert!(!delivery.poll(deadline).unwrap());
        let mut previous = Vec::new();
        client.read_to_end(&mut previous).unwrap();
        for byte in [0, 0, 0, 1, 1] {
            client.write_all(&[byte]).unwrap();
        }
        client.shutdown(Shutdown::Write).unwrap();
        // The client finishes its original ACK/write EOF before sending a new
        // message on another accepted connection. No timer/tick intervenes.
        next_sender.write_all(b"next-root-message").unwrap();
        let mut next = [0; 17];
        next_receiver.read_exact(&mut next).unwrap();
        assert_eq!(&next, b"next-root-message");
        assert!(
            !delivery.observation().acknowledged,
            "receiving the new message did not itself poll the prior socket"
        );

        let mut reads = 0;
        let mut interrupted = 0;
        assert!(
            delivery
                .receive_ack(deadline, |mut stream, bytes| {
                    reads += 1;
                    if reads == 1 || reads == 3 {
                        interrupted += 1;
                        return Err(io::ErrorKind::Interrupted.into());
                    }
                    // Actual native one-byte reads deterministically exercise legal
                    // short reads. No synthesized bytes/EOF or completion owner.
                    stream.read(&mut bytes[..1])
                })
                .unwrap()
        );
        assert_eq!((reads, interrupted), (7, 2));
        let observed = delivery.observation();
        assert!(observed.write_closed && observed.acknowledged && !observed.failed);
        assert_eq!(observed.acknowledgement_prefix, [0, 0, 0, 1, 1]);
        assert_eq!(
            (observed.remaining_frames, observed.current_frame_sent),
            (0, 0)
        );
        assert!(delivery.poll(deadline).unwrap());
        // This checks transport causality only. Actual Owner transitions still
        // require consuming the same query's native/journal/loan closure owner.
    }

    #[test]
    fn already_queued_partial_ack_eof_is_permanent_failure_with_exact_prefix() {
        let (mut client, server) = CtlStream::pair().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut delivery = Delivery::frames(server, vec![b"actual query frames".to_vec()]).unwrap();
        assert!(!delivery.poll(deadline).unwrap());
        client.read_to_end(&mut Vec::new()).unwrap();
        client.write_all(&[0, 0]).unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        assert!(delivery.poll(deadline).is_err());
        let observed = delivery.observation();
        assert!(observed.write_closed && observed.failed && !observed.acknowledged);
        assert_eq!(observed.acknowledgement_prefix, [0, 0]);
        assert!(delivery.poll(deadline).is_err());
    }

    #[test]
    fn expired_original_ack_deadline_does_not_attempt_a_read_or_renew() {
        let (mut client, server) = CtlStream::pair().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut delivery = Delivery::frames(server, vec![b"actual query frames".to_vec()]).unwrap();
        assert!(!delivery.poll(deadline).unwrap());
        client.read_to_end(&mut Vec::new()).unwrap();
        client.write_all(&[0, 0, 0, 1, 1]).unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let expired = Instant::now();
        let mut attempted = false;
        let error = delivery
            .receive_ack(expired, |_, _| {
                attempted = true;
                Err(io::ErrorKind::Interrupted.into())
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(!attempted);
        assert!(delivery.observation().acknowledgement_prefix.is_empty());
    }
}
