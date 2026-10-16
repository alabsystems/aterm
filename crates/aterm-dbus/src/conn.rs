// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A blocking connection to a message bus over a unix socket: SASL EXTERNAL,
//! `Hello`, then calls, replies, signals and errors.
//!
//! It owns no thread and no event loop. A caller that serves calls (the AT-SPI
//! publisher) polls [`Connection`]'s descriptor and drains [`Connection::recv`]
//! with a zero timeout; [`Connection::has_buffered`] says when `recv` has
//! something to answer without the descriptor turning readable.
//!
//! Like zbus, every message this side sends after `Hello` carries its own
//! unique name as the sender (the bus would stamp it anyway), and serials
//! count up from 1, `Hello`'s.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read as _, Write as _};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::time::{Duration, Instant};

use aterm_uds::CtlStream;

use crate::address::{self, Address};
use crate::auth;
use crate::error::{Error, Result};
use crate::message::{self, Kind, Message};
use crate::value::Value;

/// The bus daemon's own name and object.
pub const BUS_NAME: &str = "org.freedesktop.DBus";
pub const BUS_PATH: &str = "/org/freedesktop/DBus";

/// The largest message a connection accepts unless told otherwise. A peer can
/// make the decoder allocate about 32 bytes per byte it sends, and AT-SPI's
/// calls and signals are small, so the default is far below the
/// specification's 128 MiB. A larger message is read past, never held.
pub const DEFAULT_MAX_INCOMING: usize = 1 << 20;

/// How long the handshake may take before [`Connection::handshake`] gives up.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(25);

// A method call must not turn unrelated bus traffic into an unbounded mailbox.
// Both limits apply: tiny messages cannot evade the count, and a few legal
// large messages cannot evade the original-wire-byte allowance. This bounds
// retained decoded payload independently of a raised incoming frame limit.
const MAX_QUEUED_MESSAGES: usize = 256;
const MAX_QUEUED_BYTES: usize = 1 << 20;

struct Received {
    message: Message,
    wire_bytes: usize,
}

#[derive(Clone, Copy)]
enum ReadLimit {
    Deadline(Option<Instant>),
    // recv(Some(ZERO)) deliberately drains bytes already available. A call
    // whose absolute deadline expired must never acquire these semantics.
    Ready,
}

impl ReadLimit {
    fn timeout(self) -> Result<Option<Duration>> {
        match self {
            Self::Ready => Ok(Some(Duration::ZERO)),
            Self::Deadline(deadline) => time_left(deadline),
        }
    }
}

fn time_left(deadline: Option<Instant>) -> Result<Option<Duration>> {
    deadline
        .map(|deadline| {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                Err(Error::Timeout)
            } else {
                Ok(left)
            }
        })
        .transpose()
}

/// A connection to a bus. See the module docs.
pub struct Connection {
    stream: CtlStream,
    rbuf: Vec<u8>,
    queue: VecDeque<Received>,
    queued_bytes: usize,
    serial: u32,
    unique_name: String,
    guid: String,
    max_incoming: usize,
    /// Bytes of an oversized message still to arrive and be dropped.
    discard: usize,
    broken: bool,
}

/// This process's effective uid, the identity EXTERNAL claims.
#[must_use]
// Skip: bottoms out at the `geteuid(2)` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
#[expect(
    unsafe_code,
    reason = "the crate's one FFI call: geteuid(2), which has no preconditions"
)]
pub fn effective_uid() -> u32 {
    // SAFETY: geteuid(2) takes nothing, cannot fail and touches no memory.
    unsafe { libc::geteuid() }
}

impl Connection {
    /// Connects to the session bus as this process's uid.
    ///
    /// # Errors
    ///
    /// As [`address::session_bus`] and [`Connection::open`].
    pub fn session() -> Result<Self> {
        Self::open(&address::session_bus()?)
    }

    /// Connects to the first of `addresses` that answers, authenticates as
    /// this process's uid and says `Hello`.
    ///
    /// # Errors
    ///
    /// The last entry's error when none connects.
    pub fn open(addresses: &[Address]) -> Result<Self> {
        let mut last = Error::Invalid("empty address");
        for address in addresses {
            match address::connect(address) {
                Ok(stream) => return Self::handshake(stream, effective_uid(), address.guid()),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Runs the handshake on a connected stream: EXTERNAL as `uid`, `BEGIN`,
    /// then `Hello`. `expect_guid` is the address's `guid=`, which the server
    /// must match.
    ///
    /// # Errors
    ///
    /// [`Error::Auth`] when the server refuses, [`Error::Remote`] when `Hello`
    /// fails, [`Error::Timeout`] after [`HANDSHAKE_TIMEOUT`].
    pub fn handshake(stream: CtlStream, uid: u32, expect_guid: Option<&str>) -> Result<Self> {
        let deadline = Instant::now().checked_add(HANDSHAKE_TIMEOUT);
        let mut conn = Self {
            stream,
            rbuf: Vec::new(),
            queue: VecDeque::new(),
            queued_bytes: 0,
            serial: 0,
            unique_name: String::new(),
            guid: String::new(),
            max_incoming: DEFAULT_MAX_INCOMING,
            discard: 0,
            broken: false,
        };
        conn.write_until(&auth::first(uid), deadline)?;
        let line = conn.read_line(deadline)?;
        let guid = auth::answer(&line)?;
        if let Some(expected) = expect_guid
            && !expected.eq_ignore_ascii_case(&guid)
        {
            return Err(Error::Auth(format!(
                "server GUID {guid} is not the address's {expected}"
            )));
        }
        conn.guid = guid;
        let mut hello = Message::method_call(Some(BUS_NAME), BUS_PATH, Some(BUS_NAME), "Hello")?;
        conn.serial = 1;
        hello.serial = 1;
        let mut out = auth::BEGIN.to_vec();
        out.extend_from_slice(&hello.encode()?);
        conn.write_until(&out, deadline)?;
        let reply = conn.wait_reply(1, deadline)?;
        conn.unique_name = reply
            .first_str()
            .filter(|name| name.starts_with(':') && crate::names::is_bus_name(name))
            .ok_or(Error::malformed("Hello answered without a unique name", 0))?
            .to_owned();
        Ok(conn)
    }

    /// The name the bus gave this connection.
    #[must_use]
    pub fn unique_name(&self) -> &str {
        &self.unique_name
    }

    /// The server's GUID from the handshake.
    #[must_use]
    pub fn server_guid(&self) -> &str {
        &self.guid
    }

    /// Raises or lowers the largest message accepted, up to the
    /// specification's 128 MiB. A larger one is dropped as it arrives and
    /// reported by [`Connection::recv`]; the connection stays usable.
    pub fn set_max_incoming(&mut self, bytes: usize) {
        self.max_incoming = bytes.min(message::MAX_MESSAGE);
    }

    /// Whether [`Connection::recv`] has an answer without reading the socket,
    /// so a poll on the descriptor would not see it: a message read and
    /// waiting, or a fault to report (bytes that frame no message, a closed
    /// connection), which a poll loop must not wait on.
    #[must_use]
    pub fn has_buffered(&self) -> bool {
        self.broken
            || !self.queue.is_empty()
            || match message::frame_len(&self.rbuf) {
                Ok(Some(n)) => n <= self.rbuf.len() || n > self.max_incoming,
                Ok(None) => false,
                Err(_) => true,
            }
    }

    /// Sends `msg` with the next serial (and this connection as its sender
    /// when it names none), returning that serial.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] for a message that cannot be encoded,
    /// [`Error::Closed`] or [`Error::Io`] when the write fails.
    pub fn send(&mut self, msg: Message) -> Result<u32> {
        self.send_until(msg, None)
    }

    fn send_until(&mut self, mut msg: Message, deadline: Option<Instant>) -> Result<u32> {
        self.serial = match self.serial.wrapping_add(1) {
            0 => 1,
            next => next,
        };
        msg.serial = self.serial;
        if msg.sender.is_none() && !self.unique_name.is_empty() {
            msg.sender = Some(self.unique_name.clone());
        }
        let bytes = msg.encode()?;
        self.write_until(&bytes, deadline)?;
        Ok(msg.serial)
    }

    /// Sends a method call and waits up to `timeout` for its answer (a timeout
    /// past what the clock can represent waits forever). Messages that arrive
    /// meanwhile are kept for [`Connection::recv`], in order.
    ///
    /// # Errors
    ///
    /// [`Error::Remote`] when the answer is an error, [`Error::Timeout`],
    /// [`Error::Invalid`] for a call that says it expects no reply, and the
    /// errors of [`Connection::send`]. The deadline includes writing the call.
    /// A timeout after a partial write closes the connection: its unfinished
    /// frame cannot be followed by a new message. While waiting, at most 256
    /// unrelated messages and one MiB of original wire bytes are retained;
    /// overflow is [`Error::Unsupported`] and closes the connection. Already
    /// retained messages remain available through [`Connection::recv`].
    pub fn call(&mut self, msg: Message, timeout: Duration) -> Result<Message> {
        if !msg.expects_reply() {
            return Err(Error::Invalid("call() of a message that expects no reply"));
        }
        let deadline = Instant::now().checked_add(timeout);
        let serial = self.send_until(msg, deadline)?;
        self.wait_reply(serial, deadline)
    }

    /// The next message: a queued one first, else one from the socket within
    /// `timeout` (`None` blocks; zero only drains what is already readable).
    /// `Ok(None)` when none arrived in time.
    ///
    /// A message that is well framed but breaks the specification, or is
    /// larger than [`Connection::set_max_incoming`] allows, is consumed and
    /// returned as its [`Error::Malformed`] or [`Error::Unsupported`]; the
    /// connection stays usable. A framing fault ends it ([`Error::Closed`]
    /// from then on).
    ///
    /// # Errors
    ///
    /// As above, plus the socket's.
    pub fn recv(&mut self, timeout: Option<Duration>) -> Result<Option<Message>> {
        if let Some(received) = self.queue.pop_front() {
            self.queued_bytes = self.queued_bytes.saturating_sub(received.wire_bytes);
            return Ok(Some(received.message));
        }
        let limit = match timeout {
            Some(Duration::ZERO) => ReadLimit::Ready,
            timeout => ReadLimit::Deadline(timeout.and_then(|t| Instant::now().checked_add(t))),
        };
        match self.read_message(limit) {
            Ok(received) => Ok(Some(received.message)),
            Err(Error::Timeout) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Answers `call` with `body`; nothing is sent when the caller asked for
    /// no reply.
    ///
    /// # Errors
    ///
    /// As [`Connection::send`].
    pub fn reply(&mut self, call: &Message, body: Vec<Value>) -> Result<()> {
        if call.expects_reply() {
            self.send(Message::method_return(call).with_body(body))?;
        }
        Ok(())
    }

    /// Answers `call` with the error `name` and its text.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] for a bad error name; as [`Connection::send`].
    pub fn reply_error(&mut self, call: &Message, name: &str, text: &str) -> Result<()> {
        if call.expects_reply() {
            self.send(Message::error(call, name, text)?)?;
        }
        Ok(())
    }

    /// Emits the signal `interface.member` from `path`.
    ///
    /// # Errors
    ///
    /// As [`Message::signal`] and [`Connection::send`].
    pub fn emit(
        &mut self,
        path: &str,
        interface: &str,
        member: &str,
        body: Vec<Value>,
    ) -> Result<()> {
        self.send(Message::signal(path, interface, member)?.with_body(body))
            .map(drop)
    }

    /// Asks the bus to route the messages `rule` matches here.
    ///
    /// # Errors
    ///
    /// As [`Connection::call`].
    pub fn add_match(&mut self, rule: &str, timeout: Duration) -> Result<()> {
        self.bus_call("AddMatch", rule, timeout)
    }

    /// Withdraws a rule [`Connection::add_match`] added.
    ///
    /// # Errors
    ///
    /// As [`Connection::call`].
    pub fn remove_match(&mut self, rule: &str, timeout: Duration) -> Result<()> {
        self.bus_call("RemoveMatch", rule, timeout)
    }

    fn bus_call(&mut self, member: &str, arg: &str, timeout: Duration) -> Result<()> {
        let msg = Message::method_call(Some(BUS_NAME), BUS_PATH, Some(BUS_NAME), member)?;
        self.call(msg.with_body(vec![Value::from(arg)]), timeout)
            .map(drop)
    }

    fn wait_reply(&mut self, serial: u32, deadline: Option<Instant>) -> Result<Message> {
        loop {
            let received = match self.read_message(ReadLimit::Deadline(deadline)) {
                Ok(received) => received,
                // A well-framed message this side cannot read is not the
                // answer it waits for: keep waiting.
                Err(Error::Malformed { .. } | Error::Unsupported(_)) if !self.broken => continue,
                Err(e) => return Err(e),
            };
            let msg = &received.message;
            let answers = matches!(msg.kind, Kind::MethodReturn | Kind::Error)
                && msg.reply_serial == Some(serial);
            if !answers {
                let total = self.queued_bytes.checked_add(received.wire_bytes);
                if self.queue.len() >= MAX_QUEUED_MESSAGES
                    || total.is_none_or(|n| n > MAX_QUEUED_BYTES)
                {
                    self.broken = true;
                    return Err(Error::Unsupported("pending message queue exhausted"));
                }
                self.queued_bytes = total.unwrap_or(MAX_QUEUED_BYTES);
                self.queue.push_back(received);
                continue;
            }
            if msg.kind == Kind::Error {
                return Err(Error::Remote {
                    name: msg.error_name.clone().unwrap_or_default(),
                    message: msg.first_str().unwrap_or_default().to_owned(),
                });
            }
            return Ok(received.message);
        }
    }

    fn write_until(&mut self, bytes: &[u8], deadline: Option<Instant>) -> Result<()> {
        if self.broken {
            return Err(Error::Closed);
        }
        let mut written = 0;
        while written < bytes.len() {
            let left = match time_left(deadline) {
                Ok(left) => left,
                Err(error) => {
                    self.broken |= written != 0;
                    return Err(error);
                }
            };
            // Refresh the remaining absolute deadline for every write. One
            // relative timeout around write_all would restart for each chunk.
            if let Err(error) = self.stream.set_write_timeout(left) {
                self.broken |= written != 0;
                return Err(Error::Io(error));
            }
            match self.stream.write(&bytes[written..]) {
                Ok(0) => {
                    self.broken = true;
                    return Err(Error::Closed);
                }
                Ok(n) => written = written.saturating_add(n),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error)
                    if deadline.is_some()
                        && matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    self.broken |= written != 0;
                    return Err(Error::Timeout);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset
                    ) =>
                {
                    self.broken = true;
                    return Err(Error::Closed);
                }
                Err(error) => {
                    self.broken |= written != 0;
                    return Err(Error::Io(error));
                }
            }
        }
        Ok(())
    }

    /// Reads more bytes by the absolute deadline, blocking, or ready-only.
    fn fill(&mut self, limit: ReadLimit) -> Result<()> {
        let mut chunk = [0u8; 8192];
        let n = loop {
            let left = limit.timeout()?;
            let polled = match left {
                Some(Duration::ZERO) => {
                    self.stream.set_nonblocking(true)?;
                    let n = self.stream.read(&mut chunk);
                    self.stream.set_nonblocking(false)?;
                    n
                }
                left => {
                    // macOS refuses SO_RCVTIMEO with EINVAL once the peer has
                    // shut the socket down. The read then returns at once
                    // (bytes still buffered, or end of stream), so it goes ahead.
                    match self.stream.set_read_timeout(left) {
                        Err(e) if e.kind() != ErrorKind::InvalidInput => return Err(Error::Io(e)),
                        _ => {}
                    }
                    self.stream.read(&mut chunk)
                }
            };
            match polled {
                Ok(n) => break n,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Err(Error::Timeout);
                }
                Err(e) if e.kind() == ErrorKind::ConnectionReset => {
                    self.broken = true;
                    return Err(Error::Closed);
                }
                Err(e) => return Err(Error::Io(e)),
            }
        };
        if n == 0 {
            self.broken = true;
            return Err(Error::Closed);
        }
        let dropped = n.min(self.discard);
        self.discard = self.discard.saturating_sub(dropped);
        self.rbuf
            .extend_from_slice(chunk.get(dropped..n).unwrap_or_default());
        Ok(())
    }

    fn read_line(&mut self, deadline: Option<Instant>) -> Result<Vec<u8>> {
        let limit = ReadLimit::Deadline(deadline);
        loop {
            limit.timeout()?;
            if let Some(end) = self.rbuf.windows(2).position(|w| w == b"\r\n") {
                let line = self.rbuf.drain(..end.saturating_add(2)).take(end).collect();
                return Ok(line);
            }
            if self.rbuf.len() > auth::MAX_LINE {
                self.broken = true;
                return Err(Error::Auth("server handshake line too long".into()));
            }
            self.fill(limit)?;
        }
    }

    fn read_message(&mut self, limit: ReadLimit) -> Result<Received> {
        if self.broken {
            return Err(Error::Closed);
        }
        loop {
            limit.timeout()?;
            match message::frame_len(&self.rbuf) {
                Err(e) => {
                    self.broken = true;
                    return Err(e);
                }
                Ok(Some(n)) if n > self.max_incoming => {
                    // Its header frames it, so the stream stays in step: drop
                    // what is here and the rest as it arrives (`fill`), and
                    // report it as any unreadable message is reported.
                    let here = n.min(self.rbuf.len());
                    self.rbuf.drain(..here);
                    self.discard = n.saturating_sub(here);
                    return Err(Error::malformed(
                        "message larger than this connection accepts",
                        4,
                    ));
                }
                Ok(Some(n)) if n <= self.rbuf.len() => {
                    let bytes: Vec<u8> = self.rbuf.drain(..n).collect();
                    return Message::decode(&bytes).map(|message| Received {
                        message,
                        wire_bytes: n,
                    });
                }
                Ok(_) => self.fill(limit)?,
            }
        }
    }
}

impl AsFd for Connection {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }
}

impl AsRawFd for Connection {
    fn as_raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
}

#[cfg(test)]
#[path = "conn_bounds_tests.rs"]
mod bounds_tests;
