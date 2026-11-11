// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm side of the bridge.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;

use aterm_types::control_verbs::{framing_of, Framing};

/// The largest request line this client will write, newline excluded.
pub const REQUEST_LINE_MAX: usize = aterm_types::control_verbs::MAX_REQUEST_LINE - 1;

/// The most bytes [`Ctl::pipeline`] writes at once: what an empty Unix stream socket takes in
/// one `write(2)` without waiting for its peer to read, on every platform this client runs on.
/// macOS's is the least: its default buffer (`net.local.stream.sendspace`), 8 KiB. Linux's
/// default is larger.
pub const PIPELINE_WRITE_MAX: usize = 8 * 1024;

/// The largest framed body this client will read, and the ceiling on the byte count a reply header may declare.
pub const REPLY_BYTES_MAX: usize = 16 * 1024 * 1024;

/// The most rows a `Lines`/`Push` reply may declare.
pub const REPLY_ROWS_MAX: usize = 1024 * 1024;

/// The longest [`dial`] waits for a connect the kernel has not completed. std's connect, which
/// it replaces on Apple, had no bound.
#[cfg(target_vendor = "apple")]
const DIAL_BOUND: std::time::Duration = std::time::Duration::from_secs(60);

/// Connect to the Unix socket `sock`, its descriptor kept out of every child this process
/// spawns from before the connect, so no child spawned while it connects holds a copy
/// ([`Ctl::from_stream`] says why that matters): on Apple through
/// [`aterm_uds::connect::until`], bounded by [`DIAL_BOUND`]. Elsewhere the mark is a no-op
/// ([`aterm_uds::keep_out_of_children`]), so std's blocking connect is kept, which on Linux
/// waits out a full listen backlog rather than refusing it.
///
/// # Errors
/// The socket, the mark, or the connect.
pub fn dial(sock: impl AsRef<Path>) -> io::Result<UnixStream> {
    #[cfg(target_vendor = "apple")]
    {
        aterm_uds::connect::until(sock.as_ref(), std::time::Instant::now() + DIAL_BOUND)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        UnixStream::connect(sock)
    }
}

/// One control connection: one descriptor, read through a buffer and written directly, kept out
/// of every child this process spawns ([`aterm_uds::keep_out_of_children`]).
pub struct Ctl {
    stream: BufReader<Lane>,
    /// Latched on the first I/O failure. A stream that failed mid-request may be
    /// desynchronised, so nothing further is written to it and every later
    /// request answers the same way.
    lost: bool,
}

/// What a [`Ctl`]'s bytes travel over.
enum Lane {
    /// A connected Unix socket: every lane a process holds.
    Socket(UnixStream),
    /// The simulation's aterm, answering each request on the writing thread
    /// ([`crate::sim::endpoint::Answered`]).
    #[cfg(test)]
    Answered(crate::sim::endpoint::Answered),
}

impl Read for Lane {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Socket(s) => s.read(buf),
            #[cfg(test)]
            Self::Answered(a) => a.read(buf),
        }
    }
}

impl Write for Lane {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Socket(s) => s.write(buf),
            #[cfg(test)]
            Self::Answered(a) => a.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Socket(s) => s.flush(),
            #[cfg(test)]
            Self::Answered(a) => a.flush(),
        }
    }
}

/// A reply, already framed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// A `Status` line, newline stripped.
    Status(String),
    /// A `Lines` reply.
    Lines { header: String, rows: Vec<String> },
    /// A `Bytes` reply.
    Bytes { header: String, body: Vec<u8> },
}

impl Reply {
    /// The header (or the status line), whichever this reply has.
    #[must_use]
    pub fn header(&self) -> &str {
        match self {
            Reply::Status(s) => s,
            Reply::Lines { header, .. } | Reply::Bytes { header, .. } => header,
        }
    }

    /// Whether the reply is an `OK`.
    #[must_use]
    pub fn ok(&self) -> bool {
        is_ok_header(self.header())
    }

    /// The rows of a `Lines` reply, or an empty slice.
    #[must_use]
    pub fn rows(&self) -> &[String] {
        match self {
            Reply::Lines { rows, .. } => rows,
            _ => &[],
        }
    }

    /// The body of a `Bytes` reply, or an empty slice.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        match self {
            Reply::Bytes { body, .. } => body,
            _ => &[],
        }
    }
}

impl Ctl {
    /// Take an owned connected Unix socket.
    ///
    /// # Errors
    /// The descriptor is not a connected Unix socket, or duplication fails.
    pub fn adopt(fd: OwnedFd) -> io::Result<Self> {
        let stream = UnixStream::from(fd);
        stream.peer_addr()?;
        let owned_copy = aterm_uds::kept_out(|| stream.try_clone())?;
        drop(stream);
        Self::from_stream(owned_copy)
    }

    /// Wrap an already-open stream, marking it so no child this process spawns holds a copy: on
    /// macOS a child's exec-time close of its copy under a descriptor scan drains the socket for
    /// good, and every later read of the reply answers `EBADF`.
    ///
    /// # Errors
    /// The `fcntl(2)` errno of marking the descriptor.
    pub fn from_stream(stream: UnixStream) -> io::Result<Self> {
        aterm_uds::keep_out_of_children(&stream)?;
        Ok(Self {
            stream: BufReader::new(Lane::Socket(stream)),
            lost: false,
        })
    }

    /// The simulation's lane: aterm's end answers each request as it is written, on this thread
    /// ([`crate::sim::endpoint::Answered`]).
    #[cfg(test)]
    pub(crate) fn simulated(aterm: crate::sim::endpoint::Answered) -> Self {
        Self {
            stream: BufReader::new(Lane::Answered(aterm)),
            lost: false,
        }
    }

    /// Connect to a control socket ([`dial`]) and authenticate with the instance token.
    ///
    /// # Errors
    /// The connect or the write.
    pub fn connect(sock: &str, token: &str) -> io::Result<Self> {
        Self::authenticate(dial(sock)?, token)
    }

    /// [`Ctl::connect`] with the instance's Owner token beside the socket, read only once the connect succeeded ([`aterm_ctl::connect_then_read_token`]).
    ///
    /// # Errors
    /// The connect, the token read (`no Owner token: …`), or the write.
    pub fn open(sock: &str) -> io::Result<Self> {
        let (stream, token) = aterm_ctl::connect_then_read_token(sock)?;
        let token = token.map_err(|e| io::Error::new(e.kind(), format!("no Owner token: {e}")))?;
        Self::authenticate(stream, &token)
    }

    /// Present `token` on a connection just made.
    ///
    /// # Errors
    /// Marking the descriptor ([`Ctl::from_stream`]), an invalid token, or the write.
    pub(crate) fn authenticate(stream: UnixStream, token: &str) -> io::Result<Self> {
        if token.is_empty() || token.contains(' ') || misframing_char(token).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid control token",
            ));
        }
        let mut ctl = Self::from_stream(stream)?;
        let s = ctl.stream.get_mut();
        s.write_all(format!("AUTH {token}\n").as_bytes())?;
        s.flush()?;
        Ok(ctl)
    }

    /// The underlying stream, for a caller that must bound a read.
    ///
    /// # Panics
    /// On the simulation's lane ([`Ctl::simulated`]), which has no socket. The bridge it runs
    /// bounds no read of its verb lane and shuts it with [`Ctl::shutdown`], so nothing the
    /// simulation runs calls this.
    #[must_use]
    pub fn get_ref(&self) -> &UnixStream {
        match self.stream.get_ref() {
            Lane::Socket(s) => s,
            #[cfg(test)]
            Lane::Answered(_) => unreachable!("the simulation's verb lane has no socket"),
        }
    }

    /// Shut the connection down as `shutdown(2)` does: after `Write` or `Both` a write fails,
    /// and after `Read` or `Both` a read no longer waits on aterm. The simulation's lane shuts
    /// the same way ([`crate::sim::endpoint::Answered::shutdown`]).
    ///
    /// # Errors
    /// The socket's `shutdown(2)` errno.
    pub fn shutdown(&mut self, how: std::net::Shutdown) -> io::Result<()> {
        match self.stream.get_mut() {
            Lane::Socket(s) => s.shutdown(how),
            #[cfg(test)]
            Lane::Answered(a) => {
                a.shutdown(how);
                Ok(())
            }
        }
    }

    /// Whether this connection is lost.
    #[must_use]
    pub const fn lost(&self) -> bool {
        self.lost
    }

    /// Run one request, latching the connection as lost on an I/O failure.
    fn guarded<T>(&mut self, r: io::Result<T>) -> io::Result<T> {
        r.map_err(|e| {
            self.lost = true;
            match e.kind() {
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => io::Error::new(
                    io::ErrorKind::TimedOut,
                    "aterm did not answer before the deadline",
                ),
                _ => e,
            }
        })
    }

    /// Send one request line and read its reply, framed per the verb table.
    ///
    /// # Errors
    /// Any I/O failure, and a `Bytes`/`Lines` header whose count does not parse (a desynchronised stream, which must not be silently continued from).
    pub fn request(&mut self, line: &str) -> io::Result<Reply> {
        self.request_with_body(line, &[])
    }

    /// [`Ctl::request`] with `body` written immediately after the request line.
    ///
    /// # Errors
    /// As [`Ctl::request`].
    pub fn request_with_body(&mut self, line: &str, body: &[u8]) -> io::Result<Reply> {
        // Refused before a byte moves: `InvalidInput`, not a latched loss.
        if line.len() > REQUEST_LINE_MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "request line is {} bytes; max {REQUEST_LINE_MAX}",
                    line.len()
                ),
            ));
        }
        if let Some(c) = misframing_char(line) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "request line carries U+{:04X}; aterm ends a request at a newline and \
                     splits its fields on whitespace",
                    u32::from(c)
                ),
            ));
        }
        if self.lost {
            return Err(connection_lost());
        }
        let sent = (|| {
            let s = self.stream.get_mut();
            s.write_all(line.as_bytes())?;
            s.write_all(b"\n")?;
            if !body.is_empty() {
                s.write_all(body)?;
            }
            s.flush()
        })();
        self.guarded(sent)?;
        let verb = request_verb(line);
        let reply = self.read_reply(verb, line);
        self.guarded(reply)
    }

    /// Send `lines` in as few writes as an empty socket takes whole ([`PIPELINE_WRITE_MAX`]),
    /// each write's replies read before the next goes, and answer each line in order: a round
    /// trip a write, where [`Ctl::request`] takes one a line. aterm serves a connection's
    /// requests in the order they arrive and answers each before it reads the next, so each
    /// reply is its own line's, and once a write's replies are read aterm has read every byte of
    /// it: the next write starts on an empty socket. `each(i)` runs once line `i`'s reply has
    /// been read. A line that would not frame refuses the whole batch before a byte moves
    /// (`InvalidInput` for every line), as [`Ctl::request`] refuses it alone.
    ///
    /// NO WRITE IS CUT INSIDE A LINE. aterm serves the bytes a peer left before its end as a
    /// request even with no newline after them, so a writer killed while its write waited for
    /// aterm to read would leave aterm a line's prefix to take as the whole line. A write of
    /// whole lines that an empty socket takes at once never waits, so a kill lands before it or
    /// after it. A line longer than that goes in a write of its own, which can wait, as
    /// [`Ctl::request`]'s write of it can.
    ///
    /// # Errors
    /// Per line: as [`Ctl::request`]. An I/O failure latches the connection lost, and every line
    /// whose reply was not read answers it.
    pub fn pipeline(
        &mut self,
        lines: &[&str],
        mut each: impl FnMut(usize),
    ) -> Vec<io::Result<Reply>> {
        if let Some(bad) = lines
            .iter()
            .find(|line| line.len() > REQUEST_LINE_MAX || misframing_char(line).is_some())
        {
            let why = format!(
                "a pipelined request does not frame: {}",
                bad.chars().take(60).collect::<String>()
            );
            return lines
                .iter()
                .map(|_| Err(io::Error::new(io::ErrorKind::InvalidInput, why.clone())))
                .collect();
        }
        let mut replies = Vec::with_capacity(lines.len());
        while replies.len() < lines.len() && !self.lost {
            let at = replies.len();
            let end = write_end(lines, at);
            let mut joined = Vec::with_capacity(lines[at..end].iter().map(|l| l.len() + 1).sum());
            for line in &lines[at..end] {
                joined.extend_from_slice(line.as_bytes());
                joined.push(b'\n');
            }
            let sent = (|| {
                let s = self.stream.get_mut();
                s.write_all(&joined)?;
                s.flush()
            })();
            if let Err(e) = self.guarded(sent) {
                let why = e.to_string();
                replies.extend(
                    lines[at..]
                        .iter()
                        .map(|_| Err(io::Error::new(e.kind(), why.clone()))),
                );
                return replies;
            }
            for (i, line) in lines.iter().enumerate().take(end).skip(at) {
                if self.lost {
                    replies.push(Err(connection_lost()));
                    continue;
                }
                let reply = self.read_reply(request_verb(line), line);
                replies.push(self.guarded(reply));
                each(i);
            }
        }
        replies.extend(
            lines[replies.len()..]
                .iter()
                .map(|_| Err(connection_lost())),
        );
        replies
    }

    fn read_line(&mut self) -> io::Result<String> {
        let mut s = String::new();
        if self.stream.read_line(&mut s)? == 0 || !s.ends_with('\n') {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "aterm closed the control connection",
            ));
        }
        while s.ends_with('\n') || s.ends_with('\r') {
            s.pop();
        }
        Ok(s)
    }

    fn read_reply(&mut self, verb: &str, request: &str) -> io::Result<Reply> {
        let header = self.read_line()?;
        if !is_ok_header(&header) {
            return Ok(Reply::Status(header));
        }
        match framing_of(verb, request) {
            Framing::Lines | Framing::Push => {
                let n = count_after_ok(&header, REPLY_ROWS_MAX)?;
                let mut rows = Vec::with_capacity(n.min(256));
                for _ in 0..n {
                    rows.push(self.read_line()?);
                }
                Ok(Reply::Lines { header, rows })
            }
            Framing::Bytes => {
                let n = count_after_ok(&header, REPLY_BYTES_MAX)?;
                let mut body = Vec::with_capacity(n.saturating_add(1));
                let read = std::io::Read::by_ref(&mut self.stream)
                    .take(n as u64)
                    .read_to_end(&mut body)?;
                if read != n {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!("reply ended at {read} of {n} bytes"),
                    ));
                }
                Ok(Reply::Bytes { header, body })
            }
            Framing::Status => Ok(Reply::Status(header)),
        }
    }
}

/// Where the write [`Ctl::pipeline`] starts at line `at` of `lines` ends: the most whole lines
/// from `at` whose bytes, a newline each, an empty socket takes at once
/// ([`PIPELINE_WRITE_MAX`]), and at least line `at` itself, however long.
fn write_end(lines: &[&str], at: usize) -> usize {
    let mut end = at + 1;
    let mut bytes = lines[at].len() + 1;
    while let Some(line) = lines.get(end) {
        bytes += line.len() + 1;
        if bytes > PIPELINE_WRITE_MAX {
            break;
        }
        end += 1;
    }
    end
}

fn connection_lost() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "aterm control connection lost")
}

/// The first character of `line` that aterm would read as framing rather than
/// as part of the field it sits in: any control character (`\n` ends the
/// request, `\r` and `\t` split a field) and any whitespace but the ASCII space
/// (aterm's `split_whitespace` splits on U+00A0, U+2028 and the rest too).
fn is_ok_header(header: &str) -> bool {
    header == "OK" || header.starts_with("OK ")
}

fn misframing_char(line: &str) -> Option<char> {
    line.chars()
        .find(|c| c.is_control() || (c.is_whitespace() && *c != ' '))
}

/// The verb keyword of a request line, read by the wire's request grammar the
/// server reads it by ([`aterm_types::control_request::head`]).
fn request_verb(line: &str) -> &str {
    aterm_types::control_request::head(line).verb
}

/// The number after `OK ` in a framed header, refused above `ceiling`.
fn count_after_ok(header: &str, ceiling: usize) -> io::Result<usize> {
    let n: usize = header
        .strip_prefix("OK ")
        .and_then(|rest| rest.split(' ').next())
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("reply header has no count: {header:?}"),
            )
        })?;
    if n > ceiling {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("reply declares {n}; max {ceiling}"),
        ));
    }
    Ok(n)
}

#[cfg(test)]
#[path = "ctl_tests.rs"]
mod tests;
