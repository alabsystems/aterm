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

/// Count descriptor-carrying reply frames queued in `buf` (including a trailing partial frame
/// whose leading kind byte has already arrived).
fn queued_descriptor_slots(mut buf: &[u8]) -> Result<usize, ProtoError> {
    let mut slots = 0usize;
    while !buf.is_empty() {
        if Reply::carries_descriptor(buf[0]) {
            slots += 1;
        }
        let Some(header) = buf.first_chunk::<{ crate::proto::HEADER }>() else {
            break;
        };
        let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
        if len > crate::proto::MAX_MESSAGE {
            return Err(ProtoError::TooLarge(len as u64));
        }
        let end = crate::proto::HEADER + len;
        let Some(rest) = buf.get(end..) else {
            break;
        };
        buf = rest;
    }
    Ok(slots)
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
        self.send_request_bytes(&request.encode()?)
    }

    /// Send an already-encoded request message.
    ///
    /// # Errors
    /// The socket failed.
    pub fn send_request_bytes(&self, bytes: &[u8]) -> Result<(), ProtoError> {
        send(&self.stream, bytes, None)?;
        Ok(())
    }

    /// Send `reply`, with `fd` when and only when it carries one.
    ///
    /// # Errors
    /// It does not encode, `fd` is missing for `Connected` or supplied for another reply, or the
    /// socket failed.
    pub fn send_reply(&self, reply: &Reply, fd: Option<BorrowedFd<'_>>) -> Result<(), ProtoError> {
        let bytes = reply.encode()?;
        match (Reply::carries_descriptor(bytes[0]), fd.is_some()) {
            (true, false) => return Err(ProtoError::NoDescriptor),
            (false, true) => {
                return Err(ProtoError::Malformed("unexpected descriptor for reply"));
            }
            (true, true) | (false, false) => {}
        }
        send(&self.stream, &bytes, fd)?;
        Ok(())
    }

    /// The next whole message's kind and byte span in `self.buf`, reading as needed, or `None`
    /// at EOF between messages.
    fn next(&mut self, allow_reply_fds: bool) -> Result<Option<(u8, usize)>, ProtoError> {
        loop {
            if let Some((kind, _, used)) = split(&self.buf)? {
                return Ok(Some((kind, used)));
            }
            if allow_reply_fds {
                let allowed = usize::from(
                    self.buf
                        .first()
                        .copied()
                        .is_some_and(Reply::carries_descriptor),
                );
                if self.fds.len() > allowed {
                    self.fds.clear();
                    return Err(ProtoError::Malformed("unexpected descriptor on reply"));
                }
            } else if !self.fds.is_empty() {
                self.fds.clear();
                return Err(ProtoError::Malformed("unexpected descriptor on request"));
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
            if !allow_reply_fds && !self.fds.is_empty() {
                self.fds.clear();
                return Err(ProtoError::Malformed("unexpected descriptor on request"));
            }
            if got.bytes == 0 {
                if !self.fds.is_empty() {
                    self.fds.clear();
                    return Err(ProtoError::Malformed("unexpected descriptor on reply"));
                }
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
    /// The socket failed, a descriptor arrived on the request stream, or the message is not a
    /// request.
    pub fn recv_request(&mut self) -> Result<Option<Request>, ProtoError> {
        let next = self.next(false)?;
        if !self.fds.is_empty() {
            if let Some((_, used)) = next {
                self.buf.drain(..used);
            }
            self.fds.clear();
            return Err(ProtoError::Malformed("unexpected descriptor on request"));
        }
        match next {
            Some((kind, used)) => {
                let decoded = Request::decode(kind, &self.buf[crate::proto::HEADER..used]);
                self.buf.drain(..used);
                decoded.map(Some)
            }
            None => Ok(None),
        }
    }

    /// The next reply and the descriptor it carries, or `None` at EOF.
    ///
    /// # Errors
    /// The socket failed (a read past the span among them), the message is not a reply, a
    /// `Connected` came with no descriptor, or an unexpected descriptor arrived.
    pub fn recv_reply(&mut self) -> Result<Option<(Reply, Option<OwnedFd>)>, ProtoError> {
        let Some((kind, used)) = self.next(true)? else {
            return Ok(None);
        };
        let decoded = Reply::decode(kind, &self.buf[crate::proto::HEADER..used]);
        self.buf.drain(..used);
        let reply = decoded?;
        let fd = if Reply::carries_descriptor(kind) {
            Some(self.fds.pop_front().ok_or(ProtoError::NoDescriptor)?)
        } else {
            None
        };
        if self.fds.len() > queued_descriptor_slots(&self.buf)? {
            self.fds.clear();
            return Err(ProtoError::Malformed("unexpected descriptor on reply"));
        }
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
#[path = "transport_tests.rs"]
mod tests;
