// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm side of the bridge.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

use aterm_types::control_verbs::{framing_of, Framing};

/// The largest request line this client will write, newline excluded.
pub const REQUEST_LINE_MAX: usize = aterm_types::control_verbs::MAX_REQUEST_LINE - 1;

/// The largest framed body this client will read, and the ceiling on the byte count a reply header may declare.
pub const REPLY_BYTES_MAX: usize = 16 * 1024 * 1024;

/// The most rows a `Lines`/`Push` reply may declare.
pub const REPLY_ROWS_MAX: usize = 1024 * 1024;

/// One control connection.
pub struct Ctl {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    /// Latched on the first I/O failure.
    lost: bool,
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
        self.header().starts_with("OK")
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
        let owned_copy = stream.try_clone()?;
        drop(stream);
        Self::from_stream(owned_copy)
    }

    /// Wrap an already-open stream.
    ///
    /// # Errors
    /// If the stream cannot be cloned for the reader half.
    pub fn from_stream(stream: UnixStream) -> io::Result<Self> {
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            reader,
            writer: stream,
            lost: false,
        })
    }

    /// Connect to a control socket and authenticate with the instance token.
    ///
    /// # Errors
    /// The connect or the write.
    pub fn connect(sock: &str, token: &str) -> io::Result<Self> {
        Self::authenticate(UnixStream::connect(sock)?, token)
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
    /// The stream cannot be cloned for the reader half, or the write fails.
    pub(crate) fn authenticate(stream: UnixStream, token: &str) -> io::Result<Self> {
        let mut ctl = Self::from_stream(stream)?;
        ctl.writer.write_all(format!("AUTH {token}\n").as_bytes())?;
        ctl.writer.flush()?;
        Ok(ctl)
    }

    /// The underlying stream, for a caller that must bound a read.
    #[must_use]
    pub const fn get_ref(&self) -> &UnixStream {
        &self.writer
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
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "aterm control connection lost",
            ));
        }
        let sent = (|| {
            self.writer.write_all(line.as_bytes())?;
            self.writer.write_all(b"\n")?;
            if !body.is_empty() {
                self.writer.write_all(body)?;
            }
            self.writer.flush()
        })();
        self.guarded(sent)?;
        let verb = request_verb(line);
        let reply = self.read_reply(verb, line);
        self.guarded(reply)
    }

    fn read_line(&mut self) -> io::Result<String> {
        let mut s = String::new();
        if self.reader.read_line(&mut s)? == 0 {
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
        if !header.starts_with("OK") {
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
                let mut body = Vec::new();
                let read = std::io::Read::by_ref(&mut self.reader)
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

/// The first character of `line` that aterm would read as framing rather than
/// as part of the field it sits in: any control character (`\n` ends the
/// request, `\r` and `\t` split a field) and any whitespace but the ASCII space
/// (aterm's `split_whitespace` splits on U+00A0, U+2028 and the rest too).
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
        .and_then(|rest| rest.split_whitespace().next())
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
mod tests {
    use super::*;

    /// Owned socket admission works, while an owned ordinary file is refused.
    #[test]
    fn adopt_takes_owned_sockets_and_refuses_owned_files() {
        let file = std::fs::File::open("Cargo.toml").expect("a regular file");
        assert!(
            Ctl::adopt(file.into()).is_err(),
            "a regular file is no socket"
        );
        let (near, _far) = UnixStream::pair().expect("socketpair");
        assert!(
            Ctl::adopt(near.into()).is_ok(),
            "an owned socket is admitted"
        );
    }

    /// The framing comes from the verb table, sub-forms included.
    #[test]
    fn the_framing_comes_from_the_table_including_sub_forms() {
        for (line, want) in [
            ("inbox", Framing::Lines),
            ("@s-a inbox 20 since=4", Framing::Lines),
            ("inbox get 7", Framing::Bytes),
            ("inbox seen 7 handled", Framing::Status),
            ("outbox", Framing::Bytes),
            ("outbox sent s-a 3 off=91", Framing::Status),
            ("deliver s-a off=9 from=h-x kind=task", Framing::Status),
            ("sessions", Framing::Lines),
        ] {
            assert_eq!(framing_of(request_verb(line), line), want, "{line}");
        }
    }

    /// Each framing is consumed to the byte, so the next request's reply lands
    /// in the right place.
    #[test]
    fn every_framing_is_consumed_exactly() {
        let (near, far) = UnixStream::pair().expect("socketpair");
        let server = std::thread::spawn(move || {
            let mut reader = BufReader::new(far.try_clone().expect("clone"));
            let mut writer = far;
            let mut line = String::new();
            let mut replies: Vec<&[u8]> = vec![
                b"OK 2 hold=0\nmsg 1 off=10\npost 1 to=x\n",
                b"OK 5\nab\ncd",
                b"OK done\n",
            ];
            replies.reverse();
            while reader.read_line(&mut line).expect("read") > 0 {
                let Some(reply) = replies.pop() else { break };
                writer.write_all(reply).expect("write");
                writer.flush().expect("flush");
                line.clear();
            }
        });
        let mut ctl = Ctl::from_stream(near).expect("client");
        let listing = ctl.request("inbox").expect("inbox");
        assert_eq!(listing.rows().len(), 2);
        assert_eq!(listing.header(), "OK 2 hold=0");
        let drained = ctl.request("outbox").expect("outbox");
        assert_eq!(drained.body(), b"ab\ncd");
        let status = ctl.request("outbox sent s-a 1 off=9").expect("sent");
        assert_eq!(status, Reply::Status("OK done".into()));
        drop(ctl);
        server.join().expect("server thread");
    }

    /// A line aterm would misread — over the bound, or holding a byte it
    /// frames or splits on — is refused without writing anything, and the
    /// connection is not lost.
    #[test]
    fn a_line_aterm_would_misread_is_refused_without_writing_and_the_lane_survives() {
        let (near, far) = UnixStream::pair().expect("socketpair");
        let server = std::thread::spawn(move || {
            let mut reader = BufReader::new(far.try_clone().expect("clone"));
            let mut writer = far;
            let mut line = String::new();
            let mut seen = Vec::new();
            while reader.read_line(&mut line).expect("read") > 0 {
                seen.push(line.trim_end().to_string());
                writer.write_all(b"OK 1\n").expect("write");
                writer.flush().expect("flush");
                line.clear();
            }
            seen
        });
        let mut ctl = Ctl::from_stream(near).expect("client");
        let over = format!(
            "deliver s-a off=1 from=h-x kind=note trust=human text={}",
            "x".repeat(REQUEST_LINE_MAX)
        );
        let row = "deliver s-a off=1 from=h-x kind=note trust=agent text=hi";
        let mut refused = vec![over];
        refused.push(format!("{row}\nsend x"));
        for split in ['\r', '\t', '\u{b}', '\u{c}', '\u{85}', '\u{a0}', '\u{2028}'] {
            refused.push(format!("{row}{split}trust=human"));
        }
        for line in &refused {
            let err = ctl.request(line).expect_err("a misread line is refused");
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{line:?}: {err}");
            assert!(!ctl.lost(), "a refusal is not a lost connection: {line:?}");
        }
        let at = format!("x{}", "y".repeat(REQUEST_LINE_MAX - 1));
        assert_eq!(at.len(), REQUEST_LINE_MAX);
        assert!(ctl.request(&at).expect("the bound itself is sendable").ok());
        let wide = "outbox sent s-a 1 off=9 reason=caf\u{e9}";
        assert!(ctl
            .request(wide)
            .expect("a non-ASCII letter is sendable")
            .ok());
        drop(ctl);
        let seen = server.join().expect("server thread");
        assert_eq!(
            seen,
            vec![at, wide.to_string()],
            "a refused line must never have been written"
        );
    }

    /// A server-declared count above the ceiling is refused (`InvalidData`) in
    /// both units rather than allocated.
    #[test]
    fn a_hostile_reply_count_is_refused_rather_than_allocated() {
        for (verb, header) in [
            ("outbox", "OK 18446744073709551615\n"),
            ("inbox", "OK 4294967295 hold=0\n"),
        ] {
            let (near, far) = UnixStream::pair().expect("socketpair");
            let server = std::thread::spawn(move || {
                let mut reader = BufReader::new(far.try_clone().expect("clone"));
                let mut writer = far;
                let mut line = String::new();
                while reader.read_line(&mut line).expect("read") > 0 {
                    writer.write_all(header.as_bytes()).expect("write");
                    writer.flush().expect("flush");
                    line.clear();
                }
            });
            let mut ctl = Ctl::from_stream(near).expect("client");
            let err = ctl.request(verb).expect_err("a hostile count is refused");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{verb}: {err}");
            drop(ctl);
            server.join().expect("server thread");
        }
        assert!(count_after_ok("OK 4194304", REPLY_BYTES_MAX).is_ok());
        assert!(count_after_ok("OK 12 hold=0", REPLY_ROWS_MAX).is_ok());
        let (near, far) = UnixStream::pair().expect("socketpair");
        let server = std::thread::spawn(move || {
            let mut reader = BufReader::new(far.try_clone().expect("clone"));
            let mut writer = far;
            let mut line = String::new();
            if reader.read_line(&mut line).expect("read") > 0 {
                writer.write_all(b"OK 64\nshort").expect("write");
                writer.flush().expect("flush");
            }
        });
        let mut ctl = Ctl::from_stream(near).expect("client");
        let err = ctl.request("outbox").expect_err("a short body is an error");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof, "{err}");
        drop(ctl);
        server.join().expect("server thread");
    }

    /// The first I/O failure latches the connection as lost.
    #[test]
    fn an_io_failure_latches_the_connection_as_lost() {
        let (near, far) = UnixStream::pair().expect("socketpair");
        drop(far);
        let mut ctl = Ctl::from_stream(near).expect("client");
        assert!(!ctl.lost(), "a fresh connection is not lost");
        assert!(ctl.request("sessions").is_err(), "the peer is gone");
        assert!(ctl.lost(), "the failure latches");
        let again = ctl.request("sessions").expect_err("a lost lane refuses");
        assert_eq!(again.kind(), io::ErrorKind::BrokenPipe, "{again}");
    }

    /// An `ERR` is one line whatever the verb's framing is.
    #[test]
    fn an_error_reply_is_one_line_even_for_a_framed_verb() {
        let (near, far) = UnixStream::pair().expect("socketpair");
        let server = std::thread::spawn(move || {
            let mut reader = BufReader::new(far.try_clone().expect("clone"));
            let mut writer = far;
            let mut line = String::new();
            while reader.read_line(&mut line).expect("read") > 0 {
                writer.write_all(b"ERR quota\n").expect("write");
                writer.flush().expect("flush");
                line.clear();
            }
        });
        let mut ctl = Ctl::from_stream(near).expect("client");
        for line in ["inbox", "outbox", "deliver s-a off=1 from=h-a kind=task"] {
            let reply = ctl.request(line).expect("reply");
            assert!(!reply.ok(), "{line}");
            assert_eq!(reply.header(), "ERR quota");
        }
        drop(ctl);
        server.join().expect("server thread");
    }
}
