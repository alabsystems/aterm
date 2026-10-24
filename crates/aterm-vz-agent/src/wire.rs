// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The agent's wire: `u8 kind, u32 length (LE), payload`, at most [`MAX_FRAME`] bytes of
//! payload; byte strings and lists are `u32` length-prefixed (§6 of the design).
//!
//! A command connection (host → agent, port 1024) carries one request ([`Frame::Exec`],
//! [`Frame::Mount`] or [`Frame::Poweroff`]); during an `Exec` the host sends
//! [`Frame::Stdin`], [`Frame::StdinEof`] and [`Frame::Kill`], and the agent answers with
//! [`Frame::Out`], [`Frame::Err`] and one [`Frame::Exit`]. The watch connection (agent →
//! host, port 1025) carries [`Frame::Hello`], [`Frame::Booting`], [`Frame::Ready`] and
//! [`Frame::Beat`] from the agent, and [`Frame::Clock`] from the host.

use std::fmt;
use std::io::{self, Read, Write};

/// The protocol version a [`Hello`] names.
pub const PROTO: u32 = 1;

/// The largest payload a frame may carry: 1 MiB.
pub const MAX_FRAME: usize = 1 << 20;

/// A frame's header: its kind and its payload's length.
const HEADER: usize = 5;

const EXEC: u8 = 0x01;
const MOUNT: u8 = 0x02;
const POWEROFF: u8 = 0x03;
const STDIN: u8 = 0x04;
const STDIN_EOF: u8 = 0x05;
const KILL: u8 = 0x06;
const OUT: u8 = 0x10;
const ERR: u8 = 0x11;
const EXIT: u8 = 0x12;
const HELLO: u8 = 0x20;
const BOOTING: u8 = 0x21;
const READY: u8 = 0x22;
const BEAT: u8 = 0x23;
const CLOCK: u8 = 0x24;

/// What a command's stdin is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stdin {
    /// `/dev/null`.
    Null,
    /// A pipe the host writes with [`Frame::Stdin`] and closes with [`Frame::StdinEof`].
    Pipe,
}

/// One command to run as the lane user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exec {
    /// The program and its arguments; the program is looked up on `env`'s `PATH`.
    pub argv: Vec<Vec<u8>>,
    /// Its working directory.
    pub cwd: Vec<u8>,
    /// Exactly its environment, nothing inherited.
    pub env: Vec<(Vec<u8>, Vec<u8>)>,
    /// Its stdin.
    pub stdin: Stdin,
    /// One pipe for stdout and stderr (all [`Frame::Out`], in the guest's order).
    pub merge: bool,
    /// The host's clock when it sent the request, in nanoseconds since the epoch.
    pub clock_unix_ns: u64,
}

/// One directory of the `aterm` share, bind-mounted read-only at a guest path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Share {
    /// Its directory in the share (`repo`, `git`).
    pub name: String,
    /// Where it is bound in the guest: its host path.
    pub path: Vec<u8>,
}

/// How a request ended: exactly one of these, kept apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    /// The program exited with this code.
    Exited(i32),
    /// A signal ended it.
    Signaled(i32),
    /// It never ran (a missing program or `cwd`, an unexecutable file), or the request
    /// could not be carried out: why.
    NotStarted(String),
}

/// The agent's first frame on the watch connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    /// [`PROTO`].
    pub proto: u32,
    /// The SHA-256 of the agent's own executable.
    pub agent_sha256: [u8; 32],
    /// The guest kernel's id of this boot.
    pub boot_id: String,
    /// The guest kernel's release.
    pub kernel: String,
    /// The guest's uptime when the agent said hello.
    pub uptime_ms: u64,
}

/// A start job systemd still runs, and how long the agent has seen it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// systemd's job id.
    pub id: u32,
    /// Its unit.
    pub unit: String,
    /// Its type (`start`, `verify-active`, …).
    pub kind: String,
    /// Its state (`running`, `waiting`).
    pub state: String,
    /// Since the agent first saw this job.
    pub elapsed_ms: u64,
}

/// The guest is up: `systemctl is-system-running --wait`'s answer and the failed units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ready {
    /// `running` or `degraded`.
    pub state: String,
    /// The units that failed, for a degraded boot.
    pub failed_units: Vec<String>,
}

/// Every frame either end sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// Run a command.
    Exec(Exec),
    /// Mount the `aterm` share and bind these of its directories.
    Mount(Vec<Share>),
    /// Power the guest off.
    Poweroff,
    /// Bytes for the running command's stdin.
    Stdin(Vec<u8>),
    /// Close the running command's stdin.
    StdinEof,
    /// Send this signal to the running command's process group.
    Kill(i32),
    /// The command's stdout (and its stderr, merged).
    Out(Vec<u8>),
    /// The command's stderr.
    Err(Vec<u8>),
    /// How the request ended; the last frame of a command connection.
    Exit(Exit),
    /// The watch connection's first frame.
    Hello(Hello),
    /// The start jobs still running while systemd boots.
    Booting(Vec<Job>),
    /// The boot is over.
    Ready(Ready),
    /// The agent is alive.
    Beat {
        /// The guest's uptime.
        uptime_ms: u64,
    },
    /// The host's clock, which the agent steps the guest's to.
    Clock {
        /// Nanoseconds since the epoch.
        unix_ns: u64,
    },
}

/// Why a frame could not be read or written.
#[derive(Debug)]
pub enum WireError {
    /// The transport failed.
    Io(io::Error),
    /// A payload over [`MAX_FRAME`] bytes, of this length.
    TooLarge(u64),
    /// The stream ended inside a frame.
    Truncated,
    /// A kind byte no frame has.
    UnknownKind(u8),
    /// A payload that is not its kind's: what is wrong with it.
    Malformed(&'static str),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "the connection failed: {e}"),
            Self::TooLarge(len) => {
                write!(f, "a {len}-byte frame, over the {MAX_FRAME}-byte limit")
            }
            Self::Truncated => f.write_str("the connection ended inside a frame"),
            Self::UnknownKind(kind) => write!(f, "an unknown frame kind 0x{kind:02x}"),
            Self::Malformed(what) => write!(f, "a malformed frame: {what}"),
        }
    }
}

impl std::error::Error for WireError {}

impl From<io::Error> for WireError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// A payload being written.
struct Put(Vec<u8>);

impl Put {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }

    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    /// A length or count; one past `u32` cannot fit a frame anyway.
    fn len(&mut self, n: usize) -> Result<(), WireError> {
        let n = u32::try_from(n).map_err(|_| WireError::TooLarge(n as u64))?;
        self.u32(n);
        Ok(())
    }

    fn bytes(&mut self, b: &[u8]) -> Result<(), WireError> {
        self.len(b.len())?;
        self.0.extend_from_slice(b);
        Ok(())
    }

    fn list<T>(
        &mut self,
        items: &[T],
        mut each: impl FnMut(&mut Self, &T) -> Result<(), WireError>,
    ) -> Result<(), WireError> {
        self.len(items.len())?;
        items.iter().try_for_each(|item| each(self, item))
    }
}

/// A payload being read: every read is bounded by what is left.
struct Take<'a>(&'a [u8]);

impl Take<'_> {
    fn array<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        let (head, rest) = self
            .0
            .split_first_chunk::<N>()
            .ok_or(WireError::Malformed("a field runs past the payload"))?;
        self.0 = rest;
        Ok(*head)
    }

    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.array::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn i32(&mut self) -> Result<i32, WireError> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn bytes(&mut self) -> Result<Vec<u8>, WireError> {
        let len = self.u32()? as usize;
        if len > self.0.len() {
            return Err(WireError::Malformed("a byte string runs past the payload"));
        }
        let (b, rest) = self.0.split_at(len);
        self.0 = rest;
        Ok(b.to_vec())
    }

    fn text(&mut self) -> Result<String, WireError> {
        String::from_utf8(self.bytes()?).map_err(|_| WireError::Malformed("text that is not UTF-8"))
    }

    /// A list's count, each item at least `min` bytes, so a count is never trusted past the
    /// payload that would hold it.
    fn count(&mut self, min: usize) -> Result<usize, WireError> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min) > self.0.len() {
            return Err(WireError::Malformed("a list runs past the payload"));
        }
        Ok(n)
    }

    fn list<T>(
        &mut self,
        min: usize,
        mut each: impl FnMut(&mut Self) -> Result<T, WireError>,
    ) -> Result<Vec<T>, WireError> {
        let n = self.count(min)?;
        (0..n).map(|_| each(self)).collect()
    }

    fn end(&self) -> Result<(), WireError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(WireError::Malformed("bytes after the last field"))
        }
    }
}

impl Frame {
    /// The kind byte this frame goes out under.
    #[must_use]
    pub const fn kind(&self) -> u8 {
        match self {
            Self::Exec(_) => EXEC,
            Self::Mount(_) => MOUNT,
            Self::Poweroff => POWEROFF,
            Self::Stdin(_) => STDIN,
            Self::StdinEof => STDIN_EOF,
            Self::Kill(_) => KILL,
            Self::Out(_) => OUT,
            Self::Err(_) => ERR,
            Self::Exit(_) => EXIT,
            Self::Hello(_) => HELLO,
            Self::Booting(_) => BOOTING,
            Self::Ready(_) => READY,
            Self::Beat { .. } => BEAT,
            Self::Clock { .. } => CLOCK,
        }
    }

    fn payload(&self) -> Result<Vec<u8>, WireError> {
        let mut p = Put(Vec::new());
        match self {
            Self::Exec(e) => {
                p.list(&e.argv, |p, a| p.bytes(a))?;
                p.bytes(&e.cwd)?;
                p.list(&e.env, |p, (k, v)| {
                    p.bytes(k)?;
                    p.bytes(v)
                })?;
                p.u8(match e.stdin {
                    Stdin::Null => 0,
                    Stdin::Pipe => 1,
                });
                p.u8(u8::from(e.merge));
                p.u64(e.clock_unix_ns);
            }
            Self::Mount(shares) => p.list(shares, |p, s| {
                p.bytes(s.name.as_bytes())?;
                p.bytes(&s.path)
            })?,
            Self::Poweroff | Self::StdinEof => {}
            Self::Stdin(b) | Self::Out(b) | Self::Err(b) => p.0.extend_from_slice(b),
            Self::Kill(signal) => p.i32(*signal),
            Self::Exit(exit) => match exit {
                Exit::Exited(code) => {
                    p.u8(0);
                    p.i32(*code);
                }
                Exit::Signaled(signal) => {
                    p.u8(1);
                    p.i32(*signal);
                }
                Exit::NotStarted(why) => {
                    p.u8(2);
                    p.bytes(why.as_bytes())?;
                }
            },
            Self::Hello(h) => {
                p.u32(h.proto);
                p.0.extend_from_slice(&h.agent_sha256);
                p.bytes(h.boot_id.as_bytes())?;
                p.bytes(h.kernel.as_bytes())?;
                p.u64(h.uptime_ms);
            }
            Self::Booting(jobs) => p.list(jobs, |p, j| {
                p.u32(j.id);
                p.bytes(j.unit.as_bytes())?;
                p.bytes(j.kind.as_bytes())?;
                p.bytes(j.state.as_bytes())?;
                p.u64(j.elapsed_ms);
                Ok(())
            })?,
            Self::Ready(r) => {
                p.bytes(r.state.as_bytes())?;
                p.list(&r.failed_units, |p, u| p.bytes(u.as_bytes()))?;
            }
            Self::Beat { uptime_ms } => p.u64(*uptime_ms),
            Self::Clock { unix_ns } => p.u64(*unix_ns),
        }
        Ok(p.0)
    }

    /// The whole frame, header first.
    ///
    /// # Errors
    /// Its payload is over [`MAX_FRAME`] bytes.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        let payload = self.payload()?;
        if payload.len() > MAX_FRAME {
            return Err(WireError::TooLarge(payload.len() as u64));
        }
        let mut out = Vec::with_capacity(HEADER + payload.len());
        out.push(self.kind());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&payload);
        Ok(out)
    }

    /// The frame of `kind` whose payload is `payload`.
    ///
    /// # Errors
    /// An unknown kind, or a payload that is not that kind's.
    pub fn decode(kind: u8, payload: &[u8]) -> Result<Self, WireError> {
        let mut t = Take(payload);
        let frame = match kind {
            EXEC => {
                let argv = t.list(4, Take::bytes)?;
                let cwd = t.bytes()?;
                let env = t.list(8, |t| Ok((t.bytes()?, t.bytes()?)))?;
                let stdin = match t.u8()? {
                    0 => Stdin::Null,
                    1 => Stdin::Pipe,
                    _ => {
                        return Err(WireError::Malformed(
                            "an stdin that is neither null nor pipe",
                        ));
                    }
                };
                let merge = match t.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(WireError::Malformed("a merge flag that is not 0 or 1")),
                };
                let clock_unix_ns = t.u64()?;
                Self::Exec(Exec {
                    argv,
                    cwd,
                    env,
                    stdin,
                    merge,
                    clock_unix_ns,
                })
            }
            MOUNT => Self::Mount(t.list(8, |t| {
                Ok(Share {
                    name: t.text()?,
                    path: t.bytes()?,
                })
            })?),
            POWEROFF => Self::Poweroff,
            STDIN => return Ok(Self::Stdin(payload.to_vec())),
            STDIN_EOF => Self::StdinEof,
            KILL => Self::Kill(t.i32()?),
            OUT => return Ok(Self::Out(payload.to_vec())),
            ERR => return Ok(Self::Err(payload.to_vec())),
            EXIT => Self::Exit(match t.u8()? {
                0 => Exit::Exited(t.i32()?),
                1 => Exit::Signaled(t.i32()?),
                2 => Exit::NotStarted(t.text()?),
                _ => return Err(WireError::Malformed("an exit that is none of the three")),
            }),
            HELLO => Self::Hello(Hello {
                proto: t.u32()?,
                agent_sha256: t.array()?,
                boot_id: t.text()?,
                kernel: t.text()?,
                uptime_ms: t.u64()?,
            }),
            BOOTING => Self::Booting(t.list(24, |t| {
                Ok(Job {
                    id: t.u32()?,
                    unit: t.text()?,
                    kind: t.text()?,
                    state: t.text()?,
                    elapsed_ms: t.u64()?,
                })
            })?),
            READY => Self::Ready(Ready {
                state: t.text()?,
                failed_units: t.list(4, Take::text)?,
            }),
            BEAT => Self::Beat {
                uptime_ms: t.u64()?,
            },
            CLOCK => Self::Clock { unix_ns: t.u64()? },
            other => return Err(WireError::UnknownKind(other)),
        };
        t.end()?;
        Ok(frame)
    }
}

/// Read `buf` whole: `Ok(false)` when the stream ends before its first byte.
fn fill(r: &mut impl Read, buf: &mut [u8]) -> Result<bool, WireError> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) if got == 0 => return Ok(false),
            Ok(0) => return Err(WireError::Truncated),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(WireError::Io(e)),
        }
    }
    Ok(true)
}

/// The next frame, or `None` where the stream ends between frames.
///
/// # Errors
/// The transport failed, the stream ended inside a frame, or the frame is not one
/// ([`WireError`]); an oversized length is refused before its payload is read.
pub fn read_frame(r: &mut impl Read) -> Result<Option<Frame>, WireError> {
    let mut header = [0u8; HEADER];
    if !fill(r, &mut header)? {
        return Ok(None);
    }
    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]);
    if len as usize > MAX_FRAME {
        return Err(WireError::TooLarge(u64::from(len)));
    }
    let mut payload = vec![0u8; len as usize];
    if !payload.is_empty() && !fill(r, &mut payload)? {
        return Err(WireError::Truncated);
    }
    Frame::decode(header[0], &payload).map(Some)
}

/// Write `frame` with one `write_all`, so frames from two threads behind one lock never
/// interleave.
///
/// # Errors
/// The frame is over [`MAX_FRAME`], or the transport failed.
pub fn write_frame(w: &mut impl Write, frame: &Frame) -> Result<(), WireError> {
    let bytes = frame.encode()?;
    w.write_all(&bytes)?;
    w.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_kind() -> Vec<Frame> {
        vec![
            Frame::Exec(Exec {
                argv: vec![b"cargo".to_vec(), b"test".to_vec(), Vec::new()],
                cwd: b"/Users//me/aterm".to_vec(),
                env: vec![
                    (b"PATH".to_vec(), b"/usr/bin:/bin".to_vec()),
                    (b"EMPTY".to_vec(), Vec::new()),
                ],
                stdin: Stdin::Pipe,
                merge: true,
                clock_unix_ns: 1_792_000_000_123_456_789,
            }),
            Frame::Exec(Exec {
                argv: vec![b"true".to_vec()],
                cwd: b"/".to_vec(),
                env: Vec::new(),
                stdin: Stdin::Null,
                merge: false,
                clock_unix_ns: 0,
            }),
            Frame::Mount(vec![
                Share {
                    name: "repo".into(),
                    path: b"/Users//me/aterm".to_vec(),
                },
                Share {
                    name: "git".into(),
                    path: b"/Users//me/aterm/.git".to_vec(),
                },
            ]),
            Frame::Mount(Vec::new()),
            Frame::Poweroff,
            Frame::Stdin(b"line\n".to_vec()),
            Frame::Stdin(Vec::new()),
            Frame::StdinEof,
            Frame::Kill(9),
            Frame::Out(vec![0, 255, 10]),
            Frame::Err(b"warning\n".to_vec()),
            Frame::Exit(Exit::Exited(0)),
            Frame::Exit(Exit::Exited(-1)),
            Frame::Exit(Exit::Signaled(15)),
            Frame::Exit(Exit::NotStarted("cargo: No such file or directory".into())),
            Frame::Hello(Hello {
                proto: PROTO,
                agent_sha256: [0xab; 32],
                boot_id: "1b4c…".into(),
                kernel: "6.8.0-142-generic".into(),
                uptime_ms: 1830,
            }),
            Frame::Booting(vec![Job {
                id: 117,
                unit: "systemd-networkd-wait-online.service".into(),
                kind: "start".into(),
                state: "running".into(),
                elapsed_ms: 5000,
            }]),
            Frame::Booting(Vec::new()),
            Frame::Ready(Ready {
                state: "degraded".into(),
                failed_units: vec!["fwupd.service".into()],
            }),
            Frame::Beat { uptime_ms: 99 },
            Frame::Clock { unix_ns: u64::MAX },
        ]
    }

    #[test]
    fn every_kind_round_trips_through_one_stream() {
        let frames = every_kind();
        let mut stream = Vec::new();
        for f in &frames {
            write_frame(&mut stream, f).unwrap();
        }
        let mut r = stream.as_slice();
        for f in &frames {
            assert_eq!(read_frame(&mut r).unwrap().as_ref(), Some(f));
        }
        assert!(
            read_frame(&mut r).unwrap().is_none(),
            "the end between frames is no frame"
        );
    }

    #[test]
    fn a_frame_over_the_limit_is_refused_both_ways() {
        assert!(Frame::Out(vec![0; MAX_FRAME]).encode().is_ok());
        assert!(matches!(
            Frame::Out(vec![0; MAX_FRAME + 1]).encode(),
            Err(WireError::TooLarge(n)) if n == (MAX_FRAME + 1) as u64
        ));
        // A header that claims more than the limit is refused before any payload is
        // read or allocated: none follows it here.
        let mut header = vec![OUT];
        header.extend_from_slice(&((MAX_FRAME as u32) + 1).to_le_bytes());
        assert!(matches!(
            read_frame(&mut header.as_slice()),
            Err(WireError::TooLarge(_))
        ));
        let mut huge = vec![OUT];
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            read_frame(&mut huge.as_slice()),
            Err(WireError::TooLarge(_))
        ));
    }

    #[test]
    fn a_stream_cut_inside_a_frame_is_truncated_at_every_cut() {
        let whole = Frame::Exit(Exit::NotStarted("why".into()))
            .encode()
            .unwrap();
        for cut in 1..whole.len() {
            assert!(
                matches!(read_frame(&mut &whole[..cut]), Err(WireError::Truncated)),
                "cut at {cut}"
            );
        }
        assert!(read_frame(&mut &whole[..0]).unwrap().is_none());
    }

    #[test]
    fn an_unknown_kind_is_named() {
        for kind in [0x00, 0x07, 0x13, 0x25, 0x7f, 0xff] {
            let e = read_frame(&mut [kind, 0, 0, 0, 0].as_slice()).unwrap_err();
            assert!(matches!(e, WireError::UnknownKind(k) if k == kind), "{e}");
            assert!(e.to_string().contains(&format!("0x{kind:02x}")), "{e}");
        }
    }

    #[test]
    fn a_payload_that_is_not_its_kinds_is_malformed() {
        let malformed = |kind: u8, payload: &[u8]| {
            matches!(Frame::decode(kind, payload), Err(WireError::Malformed(_)))
        };
        // Short of a fixed field.
        assert!(malformed(KILL, &[1, 0]));
        assert!(malformed(BEAT, &[0; 7]));
        // Bytes after the last field.
        assert!(malformed(KILL, &[9, 0, 0, 0, 0]));
        assert!(malformed(POWEROFF, &[0]));
        // A string or a list longer than the payload, refused before it is allocated.
        assert!(malformed(EXIT, &[2, 0xff, 0xff, 0xff, 0x7f]));
        assert!(malformed(MOUNT, &[0xff, 0xff, 0xff, 0xff]));
        // A tag none of the variants has.
        assert!(malformed(EXIT, &[3, 0, 0, 0, 0]));
        let mut exec = Frame::Exec(Exec {
            argv: vec![b"x".to_vec()],
            cwd: b"/".to_vec(),
            env: Vec::new(),
            stdin: Stdin::Null,
            merge: false,
            clock_unix_ns: 0,
        })
        .encode()
        .unwrap();
        let stdin_at = exec.len() - 10;
        exec[stdin_at] = 2;
        assert!(malformed(EXEC, &exec[HEADER..]));
        exec[stdin_at] = 0;
        exec[stdin_at + 1] = 2;
        assert!(malformed(EXEC, &exec[HEADER..]));
        // Text that is not UTF-8.
        assert!(malformed(EXIT, &[2, 1, 0, 0, 0, 0xff]));
    }
}
