// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm side of the bridge: a client for aterm's control protocol over one
//! stream, framed the way the verb table says each verb is framed.
//!
//! * `Status` — one line. `deliver`, `hold`, `outbox sent`, `post`.
//! * `Lines` — `OK <n> …` then exactly `n` lines. `inbox`, `sessions`.
//! * `Bytes` — `OK <nbytes>` then exactly that many bytes. `outbox`, `inbox get`.
//! * `Push` — `OK subscribe <n>` then `n` `sub` map rows, read like `Lines`.
//!
//! Misreading a framing parks forever on rows that never come or reads a body
//! as verbs, so the framing comes from
//! [`aterm_types::control_verbs::framing_of`], never a local table.
//!
//! ## Two ways in
//!
//! * An inherited descriptor (fds 3 and 4, [`aterm_uds::spawnfd`]): the
//!   descriptor is the credential, served with `Scope::Bridge` before this
//!   process existed. This is the real bridge.
//! * `--sock` + token ([`Ctl::connect`]): an ordinary Owner client, i.e.
//!   observer mode — `whoami` answers `owner`, the bridge-plane verbs are
//!   refused, and the process says so.
//!
//! ## The request line's bounds live on the writer
//!
//! aterm's control server silently drops a connection whose request line
//! reaches its `MAX_REQUEST_LINE` (`crates/aterm-gui/src/control.rs`). For the
//! bridge that is aterm's fail-closed `bridge_lost` halt over every session,
//! with nothing on this side seeing the close. So [`REQUEST_LINE_MAX`] is the
//! one bound: [`Ctl::request_with_body`] refuses a longer line and writes
//! nothing (stream framed, connection alive), and every caller that builds a
//! variable-length line sizes the WHOLE line from it — not just its last field,
//! or an attacker-sized earlier field leaves the line over the bound and the
//! record with nowhere to go.
//!
//! THE FRAMING IS THE WRITER'S TOO. aterm ends a request at its first `\n` and
//! splits its fields on any whitespace, and the bridge's lane is owner-class
//! over the active session — so one value that carried a `\n` onto a line would
//! be a second verb run with that authority, and one that carried a `\r`, a tab
//! or a Unicode space would be a second field (`trust=human`, last one wins).
//! Every value the bridge puts on a line passes a closed grammar first
//! (principals, kinds, topics, pct-encoded text), but that is a promise each
//! field keeps on its own; [`Ctl::request_with_body`] keeps the one that
//! matters for all of them at once, and refuses — writes nothing, latches
//! nothing — a line holding any control character or any whitespace but the
//! ASCII space ([`misframing_char`]). Inside a line the space is then the only
//! separator, and the writer's own `\n` the only terminator. A field that must
//! carry arbitrary bytes travels pct-encoded or as a length-framed body, which
//! this check never reads.
//!
//! ## A lost connection is latched, distinct from a refusal
//!
//! An I/O failure latches [`Ctl::lost`], so nothing talks into a stream whose
//! framing is unknown, and the run loop can tell "aterm went away" (exit and be
//! relaunched) from "this line was too long" (a local refusal, nothing written).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

use aterm_types::control_verbs::{framing_of, Framing};

/// The largest request line this client will WRITE, newline excluded.
///
/// One byte under aterm's `MAX_REQUEST_LINE` (64 KiB, newline excluded): an
/// off-by-one in either implementation must cost an error, not a fleet halt.
pub const REQUEST_LINE_MAX: usize = 64 * 1024 - 1;

/// The largest framed BODY this client will read, and the ceiling on the byte
/// count a reply header may declare.
///
/// `OK <n>` is written by whatever is on the socket — and `notify` points this
/// client at an operator-named `--sock`, which may be stale or squatted — so an
/// unchecked count is an allocation request (`OK 18446744073709551615` aborts).
/// `aterm-ctl`'s `MAX_BODY_BYTES` caps the same value for the same reason.
///
/// The largest legitimate reply is one `outbox` drain: `OUTBOX_DRAIN_BYTES_MAX`
/// (4 MiB) plus one `BODY_MAX` (256 KiB) post; 16 MiB leaves headroom. Over it
/// is a refusal, not a truncation: reading part of a body desynchronises the
/// stream.
pub const REPLY_BYTES_MAX: usize = 16 * 1024 * 1024;

/// The most rows a `Lines`/`Push` reply may declare.
///
/// Past every real roster, `inbox` page and `outbox` listing; the reserve is
/// also taken in small steps, so a large count allocates as rows arrive.
pub const REPLY_ROWS_MAX: usize = 1024 * 1024;

/// One control connection.
pub struct Ctl {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    /// Latched on the first I/O failure. A stream that failed mid-request may be
    /// desynchronised, so nothing further is written to it and every later
    /// request answers the same way.
    lost: bool,
}

/// A reply, already framed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// A `Status` line, newline stripped.
    Status(String),
    /// A `Lines` reply: the header line and its rows.
    Lines { header: String, rows: Vec<String> },
    /// A `Bytes` reply: the header line and the body it announced.
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

    /// Whether the reply is an `OK`. An `ERR …` is data, not an I/O error: the
    /// endpoint refusing a `deliver` (a quota, an unknown session) is a normal
    /// outcome the bridge must record, not a reason to drop the connection.
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
    /// Take an owned connected Unix socket. The moved descriptor is duplicated
    /// close-on-exec, then closed, so even a caller that owns an unflagged socket
    /// cannot hand that authority to children spawned later by this bridge.
    ///
    /// # Errors
    ///
    /// The descriptor is not a connected Unix socket, or duplication fails.
    /// Its ownership has moved here and it closes on every outcome.
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
    ///
    /// If the stream cannot be cloned for the reader half.
    pub fn from_stream(stream: UnixStream) -> io::Result<Self> {
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            reader,
            writer: stream,
            lost: false,
        })
    }

    /// Connect to a control socket and authenticate with the instance token —
    /// OBSERVER MODE. The caller must check `whoami` before claiming delivery.
    ///
    /// # Errors
    ///
    /// The connect, the token read, or the write.
    pub fn connect(sock: &str, token: &str) -> io::Result<Self> {
        Self::authenticate(UnixStream::connect(sock)?, token)
    }

    /// [`Ctl::connect`] with the instance's Owner token beside the socket,
    /// read only once the connect succeeded
    /// ([`aterm_ctl::connect_then_read_token`]) — never a token read for an
    /// earlier connection, which an update's successor at the same path
    /// refuses `ERR auth`.
    ///
    /// # Errors
    ///
    /// The connect, the token read (`no Owner token: …`), or the write.
    pub fn open(sock: &str) -> io::Result<Self> {
        let (stream, token) = aterm_ctl::connect_then_read_token(sock)?;
        let token = token.map_err(|e| io::Error::new(e.kind(), format!("no Owner token: {e}")))?;
        Self::authenticate(stream, &token)
    }

    /// Present `token` on a connection just made.
    ///
    /// # Errors
    ///
    /// The stream cannot be cloned for the reader half, or the write fails.
    pub(crate) fn authenticate(stream: UnixStream, token: &str) -> io::Result<Self> {
        let mut ctl = Self::from_stream(stream)?;
        ctl.writer.write_all(format!("AUTH {token}\n").as_bytes())?;
        ctl.writer.flush()?;
        Ok(ctl)
    }

    /// The underlying stream, for a caller that must bound a read.
    #[must_use]
    pub fn get_ref(&self) -> &UnixStream {
        &self.writer
    }

    /// Whether this connection has been LOST — an I/O failure on the lane, as
    /// opposed to a line this client refused to send.
    ///
    /// A refusal ([`io::ErrorKind::InvalidInput`], nothing written) is recorded
    /// and carried on from; a loss is aterm going away, and the process must
    /// exit so its supervisor can relaunch it.
    #[must_use]
    pub fn lost(&self) -> bool {
        self.lost
    }

    /// Run one request, LATCHING the connection on an I/O failure.
    ///
    /// A fired deadline surfaces as `WouldBlock` or `TimedOut` by platform, with
    /// a message naming neither; both become one `TimedOut` that says so.
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
    ///
    /// Any I/O failure, and a `Bytes`/`Lines` header whose count does not parse
    /// (a desynchronised stream, which must not be silently continued from).
    pub fn request(&mut self, line: &str) -> io::Result<Reply> {
        self.request_with_body(line, &[])
    }

    /// [`Ctl::request`] with `body` written immediately after the request line —
    /// the length-prefixed frame form (`post len=<n>` + bytes).
    ///
    /// # Errors
    ///
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
        // The same refusal for a line aterm would frame differently from how
        // it was built — see the module doc's "framing is the writer's too".
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
        // A lost lane's framing is unknown: never write to it again.
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
        let reply = self.read_reply(&verb, line);
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
        // An `ERR` is always one line, whatever the verb's framing.
        if !header.starts_with("OK") {
            return Ok(Reply::Status(header));
        }
        match framing_of(verb, request) {
            Framing::Lines | Framing::Push => {
                let n = count_after_ok(&header, REPLY_ROWS_MAX)?;
                // Reserved in small steps, not for the declared count.
                let mut rows = Vec::with_capacity(n.min(256));
                for _ in 0..n {
                    rows.push(self.read_line()?);
                }
                Ok(Reply::Lines { header, rows })
            }
            Framing::Bytes => {
                let n = count_after_ok(&header, REPLY_BYTES_MAX)?;
                // Through a `take` into a growing buffer, so memory follows
                // what arrives, not what was declared.
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
/// `None` for a line aterm reads exactly as it was built.
fn misframing_char(line: &str) -> Option<char> {
    line.chars()
        .find(|c| c.is_control() || (c.is_whitespace() && *c != ' '))
}

/// The verb keyword of a request line, selector stripped. `framing_of` wants the
/// keyword and the whole request (it reads sub-forms out of the tail itself).
fn request_verb(line: &str) -> String {
    let no_sel = line
        .strip_prefix('@')
        .and_then(|r| r.split_once(' ').map(|x| x.1))
        .unwrap_or(line);
    no_sel.split_whitespace().next().unwrap_or("").to_string()
}

/// The number after `OK ` in a framed header, refused above `ceiling`.
///
/// An error, not a clamp: reading `ceiling` bytes of a longer body would
/// desynchronise the stream. See [`REPLY_BYTES_MAX`].
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
    /// No borrowed raw descriptor is taken or closed by this safe API.
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

    /// The framing is read from the VERB TABLE, sub-forms included — the one
    /// thing this client must not keep its own copy of.
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
            assert_eq!(framing_of(&request_verb(line), line), want, "{line}");
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
        // The BYTES reply's body holds a newline: consuming it as rows would
        // leave the tail to be read as the next reply's header.
        let drained = ctl.request("outbox").expect("outbox");
        assert_eq!(drained.body(), b"ab\ncd");
        let status = ctl.request("outbox sent s-a 1 off=9").expect("sent");
        assert_eq!(status, Reply::Status("OK done".into()));
        drop(ctl);
        server.join().expect("server thread");
    }

    /// A line aterm would misread — over the bound, or holding a byte it
    /// frames or splits on — is refused without writing anything, and the
    /// connection is not lost. The negative control is the server's own
    /// transcript: without the refusal, the `\n` case puts `send x` there as a
    /// second request and every later reply lands one request late.
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
        // EXACTLY AT THE BOUND still goes, and so does a character that is
        // neither a control nor a space: the check is about framing, not ASCII.
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
        // A legitimate large count still reads.
        assert!(count_after_ok("OK 4194304", REPLY_BYTES_MAX).is_ok());
        assert!(count_after_ok("OK 12 hold=0", REPLY_ROWS_MAX).is_ok());
        // A body shorter than its header claims is an error.
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
