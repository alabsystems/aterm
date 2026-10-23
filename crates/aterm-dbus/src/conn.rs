// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Blocking D-Bus connection over a Unix socket (`EXTERNAL` auth, `Hello`, calls, and signals).

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

/// Default maximum incoming message byte length (1 MiB).
pub const DEFAULT_MAX_INCOMING: usize = 1 << 20;

/// How long the handshake may take before [`Connection::handshake`] gives up.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(25);

const MAX_QUEUED_MESSAGES: usize = 256;
const MAX_QUEUED_BYTES: usize = 1 << 20;

struct Received {
    message: Message,
    wire_bytes: usize,
}

#[derive(Clone, Copy)]
enum ReadLimit {
    Deadline(Option<Instant>),
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
    /// How long [`Connection::send`] may wait for the bus to take its bytes
    /// ([`Connection::set_send_timeout`]); `None` waits as long as it takes.
    send_timeout: Option<Duration>,
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
    /// Connect to the session bus as this process's uid.
    ///
    /// # Errors
    /// As [`address::session_bus`] and [`Connection::open`].
    pub fn session() -> Result<Self> {
        Self::open(&address::session_bus()?)
    }

    /// Connect to the first answering address, authenticate as this uid, and call `Hello`.
    ///
    /// # Errors
    /// The last entry's error when none connects.
    pub fn open(addresses: &[Address]) -> Result<Self> {
        Self::open_within(addresses, HANDSHAKE_TIMEOUT)
    }

    /// [`Connection::open`] with each handshake bounded by `timeout`.
    ///
    /// # Errors
    /// As [`Connection::open`], or [`Error::Timeout`] if the handshake exceeds `timeout`.
    pub fn open_within(addresses: &[Address], timeout: Duration) -> Result<Self> {
        let mut last = Error::Invalid("empty address");
        for address in addresses {
            match address::connect(address) {
                Ok(stream) => {
                    let deadline = Instant::now().checked_add(timeout);
                    return Self::handshake_until(
                        stream,
                        effective_uid(),
                        address.guid(),
                        deadline,
                    );
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Run the `EXTERNAL` handshake, `BEGIN`, and `Hello` on a connected stream.
    ///
    /// # Errors
    /// [`Error::Auth`], [`Error::Remote`], or [`Error::Timeout`] after [`HANDSHAKE_TIMEOUT`].
    pub fn handshake(stream: CtlStream, uid: u32, expect_guid: Option<&str>) -> Result<Self> {
        let deadline = Instant::now().checked_add(HANDSHAKE_TIMEOUT);
        Self::handshake_until(stream, uid, expect_guid, deadline)
    }

    fn handshake_until(
        stream: CtlStream,
        uid: u32,
        expect_guid: Option<&str>,
        deadline: Option<Instant>,
    ) -> Result<Self> {
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
            send_timeout: None,
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

    /// Set the maximum incoming message length in bytes (capped at 128 MiB).
    pub fn set_max_incoming(&mut self, bytes: usize) {
        self.max_incoming = bytes.min(message::MAX_MESSAGE);
    }

    /// Whether [`Connection::recv`] has a queued message or terminal state without reading the socket.
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

    /// Set the write timeout for [`Connection::send`] (`None` waits indefinitely).
    pub const fn set_send_timeout(&mut self, timeout: Option<Duration>) {
        self.send_timeout = timeout;
    }

    /// Send `msg` with the next serial and return that serial.
    ///
    /// # Errors
    /// [`Error::Invalid`], [`Error::Closed`], [`Error::Io`], or [`Error::Timeout`].
    pub fn send(&mut self, msg: Message) -> Result<u32> {
        let deadline = self
            .send_timeout
            .and_then(|timeout| Instant::now().checked_add(timeout));
        self.send_until(msg, deadline)
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

    /// Send a method call and wait up to `timeout` for its reply.
    ///
    /// # Errors
    /// [`Error::Remote`], [`Error::Invalid`], [`Error::Timeout`], [`Error::NoReply`], [`Error::Unsupported`], or send/read failure.
    pub fn call(&mut self, msg: Message, timeout: Duration) -> Result<Message> {
        let deadline = Instant::now().checked_add(timeout);
        self.call_until(msg, deadline, deadline)
    }

    /// Receive the next queued or socket message within `timeout` (`None` blocks, zero polls).
    ///
    /// # Errors
    /// [`Error::Malformed`], [`Error::Unsupported`], [`Error::Closed`], or socket I/O failure.
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

    /// Reply to `call` with `body` (no-op when `call` expects no reply).
    ///
    /// # Errors
    /// As [`Connection::send`].
    pub fn reply(&mut self, call: &Message, body: Vec<Value>) -> Result<()> {
        if call.expects_reply() {
            self.send(Message::method_return(call).with_body(body))?;
        }
        Ok(())
    }

    /// Reply to `call` with error `name` and `text`.
    ///
    /// # Errors
    /// [`Error::Invalid`] for a bad error name, or as [`Connection::send`].
    pub fn reply_error(&mut self, call: &Message, name: &str, text: &str) -> Result<()> {
        if call.expects_reply() {
            self.send(Message::error(call, name, text)?)?;
        }
        Ok(())
    }

    /// Emit signal `interface.member` from `path`.
    ///
    /// # Errors
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

    /// Ask the bus to route messages matching `rule` to this connection.
    ///
    /// # Errors
    /// As [`Connection::call`].
    pub fn add_match(&mut self, rule: &str, timeout: Duration) -> Result<()> {
        self.bus_call("AddMatch", rule, timeout)
    }

    /// Remove a match rule previously added with [`Connection::add_match`].
    ///
    /// # Errors
    /// As [`Connection::call`].
    pub fn remove_match(&mut self, rule: &str, timeout: Duration) -> Result<()> {
        self.bus_call("RemoveMatch", rule, timeout)
    }

    fn bus_call(&mut self, member: &str, arg: &str, timeout: Duration) -> Result<()> {
        let msg = Message::method_call(Some(BUS_NAME), BUS_PATH, Some(BUS_NAME), member)?;
        self.call(msg.with_body(vec![Value::from(arg)]), timeout)
            .map(drop)
    }

    /// [`Connection::call`] with a deadline for each of its phases: the bus
    /// takes the call whole by `send_by`, and the peer answers it by
    /// `answer_by`.
    fn call_until(
        &mut self,
        msg: Message,
        send_by: Option<Instant>,
        answer_by: Option<Instant>,
    ) -> Result<Message> {
        if !msg.expects_reply() {
            return Err(Error::Invalid("call() of a message that expects no reply"));
        }
        let serial = self.send_until(msg, send_by)?;
        self.wait_reply(serial, answer_by).map_err(|e| match e {
            Error::Timeout => Error::NoReply,
            e => e,
        })
    }

    fn wait_reply(&mut self, serial: u32, deadline: Option<Instant>) -> Result<Message> {
        loop {
            let received = match self.read_message(ReadLimit::Deadline(deadline)) {
                Ok(received) => received,
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
