// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The control socket's framing over a unix stream, with descriptors
//! (`docs/DESIGN-vz-guest-2026-10-23.md` §5.1).
//!
//! Every read goes through `recvmsg` ([`aterm_uds::fdpass::recv_with_fds`]), so a descriptor
//! is never dropped by a plain `read` that happened to take the bytes it rode on: each one is
//! queued as it arrives and handed to the message that carries it ([`crate::proto::Reply::Connected`]).
//! A descriptor rides on its message's first byte, so it is queued before the message is
//! whole, and the queue's order is the messages' order.

use std::collections::VecDeque;
use std::io::{self, Write as _};
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::proto::{ProtoError, Reply, Request, split};

/// One end of a control connection.
pub struct Channel {
    stream: UnixStream,
    buf: Vec<u8>,
    fds: VecDeque<OwnedFd>,
}

impl std::fmt::Debug for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Channel")
            .field("buffered", &self.buf.len())
            .field("fds", &self.fds.len())
            .finish_non_exhaustive()
    }
}

/// Send `bytes` whole on `stream`, `fd` riding on the first byte.
///
/// # Errors
/// The socket failed.
pub fn send(stream: &UnixStream, bytes: &[u8], fd: Option<BorrowedFd<'_>>) -> io::Result<()> {
    let mut sent = match fd {
        Some(fd) => aterm_uds::fdpass::send_with_fds(stream, bytes, &[fd])?,
        None => 0,
    };
    let mut w = stream;
    while sent < bytes.len() {
        match w.write(&bytes[sent..]) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(n) => sent += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

impl Channel {
    /// A channel over `stream`.
    #[must_use]
    pub fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            buf: Vec::new(),
            fds: VecDeque::new(),
        }
    }

    /// The stream, for its write half (`try_clone`) or its timeouts.
    #[must_use]
    pub const fn stream(&self) -> &UnixStream {
        &self.stream
    }

    /// Wait at most `span` for each read: a read that waits longer fails `WouldBlock` or
    /// `TimedOut`, the no-progress detector's firing (`None`: wait for ever).
    ///
    /// # Errors
    /// The socket refused the timeout.
    pub fn set_span(&self, span: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(span)
    }

    /// Send `request`.
    ///
    /// # Errors
    /// It does not encode, or the socket failed.
    pub fn send_request(&self, request: &Request) -> Result<(), ProtoError> {
        send(&self.stream, &request.encode()?, None)?;
        Ok(())
    }

    /// Send `reply`, with `fd` when it carries one.
    ///
    /// # Errors
    /// It does not encode, or the socket failed.
    pub fn send_reply(&self, reply: &Reply, fd: Option<BorrowedFd<'_>>) -> Result<(), ProtoError> {
        send(&self.stream, &reply.encode()?, fd)?;
        Ok(())
    }

    /// The next whole message's kind and payload, reading as needed, or `None` at EOF
    /// between messages.
    fn next(&mut self) -> Result<Option<(u8, Vec<u8>)>, ProtoError> {
        loop {
            if let Some((kind, payload, used)) = split(&self.buf)? {
                let payload = payload.to_vec();
                self.buf.drain(..used);
                return Ok(Some((kind, payload)));
            }
            let mut chunk = [0u8; 64 * 1024];
            let got = loop {
                match aterm_uds::fdpass::recv_with_fds(
                    &self.stream,
                    &mut chunk,
                    aterm_uds::fdpass::MAX_FDS,
                ) {
                    Ok(got) => break got,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(ProtoError::Io(e)),
                }
            };
            self.fds.extend(got.fds);
            if got.bytes == 0 {
                return if self.buf.is_empty() {
                    Ok(None)
                } else {
                    Err(ProtoError::Truncated)
                };
            }
            self.buf.extend_from_slice(&chunk[..got.bytes]);
        }
    }

    /// The next request, or `None` at EOF.
    ///
    /// # Errors
    /// The socket failed, or the message is not a request.
    pub fn recv_request(&mut self) -> Result<Option<Request>, ProtoError> {
        match self.next()? {
            Some((kind, payload)) => Request::decode(kind, &payload).map(Some),
            None => Ok(None),
        }
    }

    /// The next reply and the descriptor it carries, or `None` at EOF.
    ///
    /// # Errors
    /// The socket failed (a read past the span among them), the message is not a reply, or a
    /// `Connected` came with no descriptor.
    pub fn recv_reply(&mut self) -> Result<Option<(Reply, Option<OwnedFd>)>, ProtoError> {
        let Some((kind, payload)) = self.next()? else {
            return Ok(None);
        };
        let reply = Reply::decode(kind, &payload)?;
        let fd = if Reply::carries_descriptor(kind) {
            Some(self.fds.pop_front().ok_or(ProtoError::NoDescriptor)?)
        } else {
            None
        };
        Ok(Some((reply, fd)))
    }

    /// A second handle on the same socket, for a writer on another thread.
    ///
    /// # Errors
    /// The descriptor could not be duplicated.
    pub fn writer(&self) -> io::Result<UnixStream> {
        self.stream.try_clone()
    }

    /// The socket's descriptor, borrowed.
    #[must_use]
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    #[test]
    fn messages_and_their_descriptors_arrive_in_order_across_any_split() {
        let (a, b) = UnixStream::pair().unwrap();
        let (p1, mut p1_far) = UnixStream::pair().unwrap();
        let (p2, mut p2_far) = UnixStream::pair().unwrap();
        let sender = Channel::new(a);
        sender.send_reply(&Reply::Started, None).unwrap();
        sender
            .send_reply(&Reply::Connected { id: 1, port: 1024 }, Some(p1.as_fd()))
            .unwrap();
        sender
            .send_reply(&Reply::Booting("between".into()), None)
            .unwrap();
        sender
            .send_reply(&Reply::Connected { id: 2, port: 1024 }, Some(p2.as_fd()))
            .unwrap();
        drop(sender);
        drop((p1, p2));
        let mut r = Channel::new(b);
        assert!(matches!(
            r.recv_reply().unwrap(),
            Some((Reply::Started, None))
        ));
        let (reply, fd) = r.recv_reply().unwrap().unwrap();
        assert_eq!(reply, Reply::Connected { id: 1, port: 1024 });
        let mut first = UnixStream::from(fd.expect("first descriptor"));
        assert!(matches!(
            r.recv_reply().unwrap(),
            Some((Reply::Booting(_), None))
        ));
        let (reply, fd) = r.recv_reply().unwrap().unwrap();
        assert_eq!(reply, Reply::Connected { id: 2, port: 1024 });
        let mut second = UnixStream::from(fd.expect("second descriptor"));
        assert!(r.recv_reply().unwrap().is_none(), "EOF between messages");
        // Each descriptor is the one sent with its message, live in this process.
        first.write_all(b"one").unwrap();
        second.write_all(b"two").unwrap();
        let mut got = [0u8; 3];
        p1_far.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"one");
        p2_far.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"two");
    }

    #[test]
    fn a_stream_cut_inside_a_message_is_truncated() {
        let (a, b) = UnixStream::pair().unwrap();
        let bytes = Request::Connect { port: 1024 }.encode().unwrap();
        (&a).write_all(&bytes[..bytes.len() - 1]).unwrap();
        drop(a);
        let mut r = Channel::new(b);
        assert!(matches!(r.recv_request(), Err(ProtoError::Truncated)));
    }

    #[test]
    fn a_connected_without_its_descriptor_is_refused() {
        let (a, b) = UnixStream::pair().unwrap();
        (&a).write_all(&Reply::Connected { id: 9, port: 1 }.encode().unwrap())
            .unwrap();
        drop(a);
        let mut r = Channel::new(b);
        assert!(matches!(r.recv_reply(), Err(ProtoError::NoDescriptor)));
    }

    #[test]
    fn a_read_past_the_span_fails_rather_than_waits() {
        let (_a, b) = UnixStream::pair().unwrap();
        let mut r = Channel::new(b);
        r.set_span(Some(Duration::from_millis(20))).unwrap();
        match r.recv_reply() {
            Err(ProtoError::Io(e)) => assert!(
                matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ),
                "{e}"
            ),
            other => panic!("{other:?}"),
        }
    }
}
