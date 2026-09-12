// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ATERM SIDE OF THE BRIDGE — a client for aterm's control protocol over
//! one stream, framed the way the verb table says each verb is framed.
//!
//! Three framings, and getting them wrong is the classic control-socket bug:
//!
//! * `Status` — one line. `deliver`, `hold`, `outbox sent`, `post`.
//! * `Lines` — `OK <n> …` then exactly `n` lines. `inbox`, `sessions`.
//! * `Bytes` — `OK <nbytes>` then exactly that many bytes. `outbox`, `inbox get`.
//!
//! A client that read a `Status` reply as a row count parks forever on rows that
//! never come; one that read a `Bytes` header as a row count reads the body as
//! verbs. The framing per verb is [`aterm_types::control_verbs::framing_of`],
//! and this client asks THAT rather than keeping its own table, so the two
//! cannot drift.
//!
//! ## Two ways in, and they are not equal
//!
//! * An INHERITED descriptor (fds 3 and 4, [`aterm_uds::spawnfd`]). No handshake
//!   is written because there is nothing to authenticate: the authority is the
//!   descriptor, and the instance served it with `Scope::Bridge` before this
//!   process existed. This is the real bridge.
//! * A `--sock` + token connection ([`Ctl::connect`]). An ordinary Owner client,
//!   which is OBSERVER MODE: `whoami` answers `owner`, not `bridge`, so the
//!   bridge-plane verbs are refused and the process says so rather than
//!   pretending to deliver.
//!
//! ## THE REQUEST-LINE BOUND LIVES HERE, ON THE WRITER
//!
//! aterm's control server drops — silently, with no reply — any connection
//! whose request line reaches its own `MAX_REQUEST_LINE`
//! (`crates/aterm-gui/src/control.rs`). For the BRIDGE's connection that
//! silence is a fleet-wide outage: losing it is aterm's fail-closed
//! `bridge_lost` halt over every session the bridge governs, and nothing on
//! this side would ever see the socket close, so nothing would relaunch.
//!
//! Three components used to hold three different implicit limits for one value
//! — the endpoint takes a 256 KiB `post` body, aterm control drops a 64 KiB
//! request line, and the bridge built one line carrying a whole body. The
//! disagreement is made IMPOSSIBLE rather than unlikely by putting the number
//! in exactly one place, on the side that writes: [`REQUEST_LINE_MAX`] is the
//! only bound in this crate, [`Ctl::request_with_body`] refuses to write a line
//! past it (writing NOTHING, so the stream stays framed and the connection
//! stays alive), and every caller that builds a variable-length line sizes THE
//! WHOLE LINE from that same constant.
//!
//! The whole line, and not its last field, because that distinction is where the
//! first fix leaked. `deliver` sized only its `text=` budget from this number,
//! computed AFTER an attacker-sized `via=` had already been appended: the
//! subtraction saturated to zero, the body was cut to nothing, and the line was
//! still over the bound. The refusal below worked exactly as designed — nothing
//! written, lane alive — and the caller then filed a permanent local refusal as
//! a transport failure and lost the record. A caller that measures only its tail
//! has not sized its line from this constant, and a refusal this client hands
//! back is only half a bound: the other half is the caller having somewhere to
//! put a record it can never send.
//!
//! ## A LOST CONNECTION IS LATCHED, and it is a different failure from a refusal
//!
//! An I/O failure on this lane means the verb connection is gone. It is latched
//! ([`Ctl::lost`]) so a caller cannot keep talking into a stream whose framing
//! it can no longer trust, and so the bridge's run loop can tell "aterm went
//! away" (exit, and let the supervisor relaunch) from "this one line was too
//! long to send" (a local refusal that wrote nothing).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

use aterm_types::control_verbs::{framing_of, Framing};

/// The largest request line this client will WRITE, newline excluded.
///
/// aterm drops the connection at its own `MAX_REQUEST_LINE` = 64 KiB, counted
/// over the line's bytes before the newline. This is one byte under that, so a
/// line this client agrees to send can never be the line that kills the lane —
/// the margin is deliberate, because an off-by-one in EITHER implementation
/// would otherwise cost a fleet-wide halt rather than an error message.
pub const REQUEST_LINE_MAX: usize = 64 * 1024 - 1;

/// The largest framed BODY this client will read, and the ceiling on the byte
/// count a reply header may declare.
///
/// ## A SERVER-DECLARED COUNT IS AN ALLOCATION REQUEST
///
/// `OK <n>` is a number written by whatever is on the other end of the socket,
/// and `read_reply` used it to size a buffer before a single body byte had
/// arrived. `OK 18446744073709551615` asked for 16 EB and aborted the process
/// on allocation failure — not an error a caller could record, and on the
/// bridge's own lane a latched `lost()` lane is exactly what the design wants
/// instead. aterm's own client caps the same value for the same reason and says
/// so (`aterm-ctl`'s `byte_count`/`MAX_BODY_BYTES`: "a hostile `OK <huge>`
/// header"); this client, which `hook`, `mirror` and `notify` point at a
/// `--sock` path an OPERATOR names on the command line — a stale path, a
/// same-uid process that grabbed the name, a desynchronised stream — had no
/// equivalent.
///
/// The largest reply this client ever legitimately asks for is one `outbox`
/// drain: the endpoint bounds that at `OUTBOX_DRAIN_BYTES_MAX` (4 MiB) plus the
/// one post that crossed the budget (`BODY_MAX`, 256 KiB). 16 MiB is that with
/// room for a rung that raises either, and far under the size at which a single
/// allocation is itself the failure. It is a REFUSAL rather than a truncation:
/// a body this client will not read is a framing disagreement, and reading part
/// of one would desynchronise the stream.
pub const REPLY_BYTES_MAX: usize = 16 * 1024 * 1024;

/// The most rows a `Lines`/`Push` reply may declare.
///
/// The same hazard in the other unit: rows are read one at a time, so a huge
/// count cannot over-read the stream, but `Vec::with_capacity` reserved for all
/// of them up front. A million rows is past every real roster, `inbox` page and
/// `outbox` listing this client asks for; the reserve is taken in small steps
/// besides, so even a legitimate large count allocates as it arrives.
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
    /// Adopt an INHERITED descriptor. No handshake: the descriptor is the
    /// credential.
    ///
    /// # Errors
    ///
    /// If the descriptor cannot be duplicated for the reader half.
    pub fn adopt(fd: RawFd) -> io::Result<Self> {
        // THIS CALL REACHES THE CRATE'S ONE `unsafe` BLOCK — see
        // [`unsafe_adopt`], which is where the obligation is discharged. The
        // note used to say "SAFETY-adjacent, not an `unsafe` block", which was
        // true of the three lines it labelled and false of the call they
        // introduced; §11.2 puts raw-descriptor work in the `aterm-uds` cordon,
        // and a reader auditing by that rule is told here that this file is the
        // exception. `from_raw_fd` on an inherited number is the one place this
        // crate takes ownership of a descriptor it did not create: exactly once
        // per number, at startup, before anything else touches it.
        let owned = unsafe_adopt(fd);
        let stream = UnixStream::from(owned);
        Self::from_stream(stream)
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
        let stream = UnixStream::connect(sock)?;
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
    /// The distinction is the whole reason the flag exists. A refusal
    /// ([`io::ErrorKind::InvalidInput`], nothing written) is a message the
    /// bridge records and carries on from; a loss is aterm going away, which is
    /// the fail-closed halt already standing on every session this bridge
    /// governs — the process must exit so its supervisor can relaunch it.
    #[must_use]
    pub fn lost(&self) -> bool {
        self.lost
    }

    /// Run one request, LATCHING the connection on an I/O failure.
    fn guarded<T>(&mut self, r: io::Result<T>) -> io::Result<T> {
        if r.is_err() {
            self.lost = true;
        }
        r
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
        // THE BOUND, BEFORE A SINGLE BYTE MOVES. A line at or past aterm's own
        // drop threshold is refused HERE and nothing is written: the stream
        // stays framed, the connection stays alive, and the caller gets an
        // error it can record instead of a fleet-wide silence it cannot see.
        // `InvalidInput` and not a latched loss, because nothing was lost.
        if line.len() > REQUEST_LINE_MAX {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a control request line of {} bytes is over the {REQUEST_LINE_MAX}-byte bound",
                    line.len()
                ),
            ));
        }
        // A LOST LANE IS NEVER WRITTEN TO AGAIN. Its framing is unknown after a
        // failed request, and a client that carried on would read the tail of
        // one reply as the header of the next.
        if self.lost {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the aterm control connection was already lost",
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
        // An `ERR` is always ONE line whatever the verb's framing is: the server
        // never writes rows or a body behind a refusal, and a client that waited
        // for them would hang on every error.
        if !header.starts_with("OK") {
            return Ok(Reply::Status(header));
        }
        match framing_of(verb, request) {
            Framing::Lines | Framing::Push => {
                let n = count_after_ok(&header, REPLY_ROWS_MAX)?;
                // RESERVED IN SMALL STEPS, not for the declared count: the
                // rows arrive one at a time and `read_line` fails at EOF, so
                // the vector grows with what actually came.
                let mut rows = Vec::with_capacity(n.min(256));
                for _ in 0..n {
                    rows.push(self.read_line()?);
                }
                Ok(Reply::Lines { header, rows })
            }
            Framing::Bytes => {
                let n = count_after_ok(&header, REPLY_BYTES_MAX)?;
                // READ THROUGH A `take`, INTO A BUFFER THAT GROWS. Even under
                // the ceiling, `vec![0u8; n]` hands a peer one allocation of
                // whatever it declared before it has sent a byte; this reads
                // what arrives and fails short exactly as `read_exact` did.
                let mut body = Vec::new();
                let read = std::io::Read::by_ref(&mut self.reader)
                    .take(n as u64)
                    .read_to_end(&mut body)?;
                if read != n {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!("a framed reply of {n} bytes ended after {read}"),
                    ));
                }
                Ok(Reply::Bytes { header, body })
            }
            Framing::Status => Ok(Reply::Status(header)),
        }
    }
}

/// Take ownership of an inherited descriptor number.
///
/// THE ONE `unsafe` BLOCK IN `aterm-link`, AND IT IS OUTSIDE THE CORDON §11.2
/// NAMES. The design confines raw-descriptor work to `aterm-uds` — which already
/// owns `spawnfd`, and therefore already places the very descriptor adopted here
/// — so this block's right home is `aterm_uds::spawnfd::adopt`. Until it moves,
/// the fact is written where a reader will meet it rather than left to a
/// crate-wide "there is no unsafe here" that three docs used to make and that a
/// one-file scan used to guard;
/// `bridge::tests::this_modules_doc_and_unsafe_surface_match_what_it_ships` now
/// reads every file this crate ships and fails on a SECOND block or a second
/// call site.
///
/// The obligation is [`FromRawFd`]'s — that the number is a live descriptor this
/// process owns and that nothing else will close it. It holds by construction:
/// the launcher `dup2`'d it into place before `exec` ([`aterm_uds::spawnfd`]),
/// this is the only call site for that number, and it runs once at startup.
fn unsafe_adopt(fd: RawFd) -> OwnedFd {
    // SAFETY: see the doc comment above — a live inherited descriptor, adopted
    // exactly once.
    unsafe { OwnedFd::from_raw_fd(fd) }
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
/// FAIL-CLOSED, AND WITH AN ERROR RATHER THAN A CLAMP: a count this client will
/// not honour is a framing disagreement with whatever is on the socket, and
/// reading `ceiling` bytes of a body that claims more would leave the stream
/// desynchronised — the next header read would be the middle of a body. See
/// [`REPLY_BYTES_MAX`].
fn count_after_ok(header: &str, ceiling: usize) -> io::Result<usize> {
    let n: usize = header
        .strip_prefix("OK ")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("framed reply header without a count: {header:?}"),
            )
        })?;
    if n > ceiling {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a framed reply declared {n}, over this client's {ceiling} ceiling"),
        ));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A round trip over a real socketpair, against a stub that answers each
    /// framing. The point is the READER: rows counted, bytes read exactly, and a
    /// second request on the same connection landing in the right place — which
    /// only holds if the first reply was consumed to the byte.
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

    /// THE LINE BOUND IS ENFORCED ON THE WRITER, and it writes NOTHING.
    ///
    /// aterm drops a connection whose request line reaches its own 64 KiB
    /// `MAX_REQUEST_LINE` — with no reply — and for the bridge's lane that
    /// silence is a fleet-wide fail-closed halt. So an over-long line is
    /// refused here, before a byte moves; the connection is NOT lost, and the
    /// very next ordinary request on the same connection still works. A client
    /// that had written the line and then discovered the drop could not say
    /// either of those things.
    #[test]
    fn an_over_long_request_line_is_refused_without_writing_and_the_lane_survives() {
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
        let err = ctl
            .request(&over)
            .expect_err("an over-long line is refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
        assert!(!ctl.lost(), "a refusal is not a lost connection");
        // EXACTLY AT THE BOUND still goes.
        let at = format!("x{}", "y".repeat(REQUEST_LINE_MAX - 1));
        assert_eq!(at.len(), REQUEST_LINE_MAX);
        assert!(ctl.request(&at).expect("the bound itself is sendable").ok());
        drop(ctl);
        let seen = server.join().expect("server thread");
        assert_eq!(
            seen,
            vec![at],
            "the refused line must never have been written"
        );
    }

    /// **A SERVER-DECLARED COUNT IS REFUSED ABOVE THE CEILING, IN BOTH UNITS.**
    ///
    /// `OK <n>` is written by whatever is on the other end of the socket — and
    /// `hook`, `mirror` and `notify` point this client at a `--sock` path an
    /// operator names, so "whatever" includes a stale path and a same-uid
    /// process that grabbed the name. Sized straight into `vec![0u8; n]` the
    /// bytes case ABORTS the process on allocation failure, which is not an
    /// outcome any caller can record; the rows case reserved capacity for a
    /// count no reply will ever carry. Both are framing disagreements and both
    /// now answer `InvalidData`, leaving the caller a latched lane instead of a
    /// core.
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
        // AND A LEGITIMATE COUNT STILL READS. The ceiling is above every reply
        // this client asks for; a body that is merely large is not hostile.
        assert!(count_after_ok("OK 4194304", REPLY_BYTES_MAX).is_ok());
        assert!(count_after_ok("OK 12 hold=0", REPLY_ROWS_MAX).is_ok());
        // A body SHORTER than its header claims is a framing error, not a short
        // read the caller has to notice for itself.
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

    /// A LOST LANE IS LATCHED. The first I/O failure marks the connection, and
    /// every later request answers immediately rather than writing into a
    /// stream whose framing is no longer known — which is what lets the bridge
    /// tell "aterm went away" from "that one line was too long".
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

    /// An `ERR` is one line whatever the verb's framing is. A reader that waited
    /// for a body behind a refusal would hang on the first quota rejection.
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
