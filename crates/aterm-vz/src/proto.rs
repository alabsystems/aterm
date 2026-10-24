// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The helper's control protocol (`docs/DESIGN-vz-guest-2026-10-23.md` §5.1), spoken on
//! `vz.noindex/run/aterm-linux.sock`: messages are `u8 kind, u32 length (LE), payload`, at
//! most [`MAX_MESSAGE`] bytes of payload, byte strings and lists `u32` length-prefixed, as the
//! agent's wire is. A [`Reply::Connected`] carries one descriptor (`SCM_RIGHTS`), which
//! [`crate::transport`] queues as it reads.
//!
//! Three things never change across protocol versions, so a lab from any checkout can tell a
//! helper from another one to stop: the kinds of [`Request::Attach`], [`Request::Stop`],
//! [`Request::State`] and [`Reply::Refused`], [`Reply::Stopping`], and `Attach`'s leading
//! `proto` field. A helper refuses an `Attach` of another `proto` with `Refused`, and answers
//! `Stop` whatever the session's protocol.

use std::fmt;
use std::path::PathBuf;

use aterm_vz_agent::wire::Hello;

use crate::plan::{Disk, Plan, ShareEntry};

/// The protocol version an [`Attach`] names and a [`crate::record::Record`] holds.
pub const PROTO: u32 = 1;

/// The largest payload a message may carry: 1 MiB.
pub const MAX_MESSAGE: usize = 1 << 20;

/// A message's header: its kind and its payload's length.
pub const HEADER: usize = 5;

// Frozen across versions (the module's note).
const ATTACH: u8 = 0x01;
const STOP: u8 = 0x02;
const STATE: u8 = 0x03;
const REFUSED: u8 = 0x81;
const STOPPING: u8 = 0x82;
// This version's.
const BOOT: u8 = 0x10;
const CONNECT: u8 = 0x11;
const RELEASE: u8 = 0x12;
const SHARE: u8 = 0x13;
const KEEP: u8 = 0x14;
const DETACH: u8 = 0x15;
const ATTACHED: u8 = 0x90;
const STARTED: u8 = 0x91;
const BOOTING: u8 = 0x92;
const READY: u8 = 0x93;
const CONNECTED: u8 = 0x94;
const DONE: u8 = 0x95;
const STATE_IS: u8 = 0x96;
const LOST: u8 = 0x97;
const FAILED: u8 = 0x98;
const ERROR: u8 = 0x99;

/// A session's opening: who the lab is and what it wants of the VM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attach {
    /// [`PROTO`] of the lab's checkout.
    pub proto: u32,
    /// The content address of the helper that checkout signed.
    pub helper_sha256: String,
    /// The agent that checkout built.
    pub agent_sha256: String,
    /// The `aterm` share's directories this run wants.
    pub shares: Vec<ShareEntry>,
    /// Keep the VM up once this session closes (`up`, `--keep-up`).
    pub keep: bool,
}

/// lab → helper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Open a session.
    Attach(Attach),
    /// Build, validate and start the VM.
    Boot(Box<Plan>),
    /// A vsock connection to the guest's `port`; the answer carries its descriptor.
    Connect {
        /// The guest port (the agent's 1024).
        port: u32,
    },
    /// The lab closed the connection [`Reply::Connected`] named.
    Release {
        /// Its id.
        id: u64,
    },
    /// Assign these directories to the running `aterm` share.
    Share(Vec<ShareEntry>),
    /// Set or clear `keep`.
    Keep(bool),
    /// The VM's state.
    State,
    /// Power the VM off and exit: `grace` before the hard stop.
    Stop {
        /// Milliseconds.
        grace_ms: u64,
    },
    /// Close the session (EOF does the same).
    Detach,
}

/// Where the VM is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Not booted.
    Absent,
    /// Started, the agent's `Ready` not yet seen.
    Booting,
    /// The agent said `Ready`.
    Ready,
    /// A stop is under way.
    Stopping,
    /// The VM stopped.
    Stopped,
    /// The framework refused or failed it.
    Failed,
}

impl Phase {
    /// Its name, as `status` prints it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Booting => "booting",
            Self::Ready => "ready",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }

    const fn byte(self) -> u8 {
        match self {
            Self::Absent => 0,
            Self::Booting => 1,
            Self::Ready => 2,
            Self::Stopping => 3,
            Self::Stopped => 4,
            Self::Failed => 5,
        }
    }

    const fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::Absent,
            1 => Self::Booting,
            2 => Self::Ready,
            3 => Self::Stopping,
            4 => Self::Stopped,
            5 => Self::Failed,
            _ => return None,
        })
    }
}

/// The helper's answer to `State`: the record's fields, the last beat, the volume's free
/// bytes and the console tail (§5.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateReport {
    /// The helper's pid.
    pub pid: u32,
    /// Its unique id (`p_uniqueid`).
    pub uniq: u64,
    /// Its executable's content address.
    pub helper_sha256: String,
    /// Its [`PROTO`].
    pub proto: u32,
    /// The live disk it holds.
    pub live_disk: PathBuf,
    /// Where the VM is.
    pub phase: Phase,
    /// Whether `keep` is set.
    pub keep: bool,
    /// The sessions attached now.
    pub sessions: u32,
    /// The guest's boot, once the agent said hello.
    pub boot_id: String,
    /// The guest's agent, once it said hello.
    pub agent_sha256: String,
    /// The failed units its `Ready` named.
    pub failed_units: Vec<String>,
    /// Milliseconds since the last `Beat`, once one came.
    pub beat_ms_ago: Option<u64>,
    /// The live disk's volume's free bytes, at the last read.
    pub free_bytes: Option<u64>,
    /// The console's last lines.
    pub console_tail: String,
}

/// helper → lab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// The session is open.
    Attached {
        /// Where the VM is.
        phase: Phase,
        /// `keep` after this session's.
        keep: bool,
        /// The `aterm` share was swapped to this session's: the lab asks the agent to rebind.
        swapped: bool,
    },
    /// The `Attach` was refused (another protocol, helper or agent): the lab replaces the
    /// helper (§5.1), saying why.
    Refused(String),
    /// The VM started.
    Started,
    /// Boot progress, for the lab's no-progress detector: what the boot is waiting on.
    Booting(String),
    /// The agent said `Ready`; its hello.
    Ready(Hello),
    /// A vsock connection, its descriptor beside this message.
    Connected {
        /// Its id, for `Release`.
        id: u64,
        /// The guest port.
        port: u32,
    },
    /// `Release`, `Share` or `Keep` is done.
    Done,
    /// The state.
    State(Box<StateReport>),
    /// `Stop` is under way: the helper powers off and exits.
    Stopping,
    /// The guest stopped, stopped answering, or the helper stopped it.
    Lost {
        /// Why, in the words the run reports.
        why: String,
        /// The console tail.
        tail: String,
    },
    /// A framework error: validation, start, or a stop with error.
    Failed {
        /// The error: its domain, code and description.
        error: String,
        /// The console tail.
        tail: String,
    },
    /// A request this session cannot make now.
    Error(String),
}

/// Why a message could not be read or written.
#[derive(Debug)]
pub enum ProtoError {
    /// The transport failed.
    Io(std::io::Error),
    /// A payload over [`MAX_MESSAGE`] bytes, of this length.
    TooLarge(u64),
    /// The stream ended inside a message.
    Truncated,
    /// A kind byte no message has.
    UnknownKind(u8),
    /// A payload that is not its kind's.
    Malformed(&'static str),
    /// A `Connected` arrived with no descriptor beside it.
    NoDescriptor,
}

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "the control socket failed: {e}"),
            Self::TooLarge(n) => write!(f, "a {n}-byte message, over the {MAX_MESSAGE}-byte limit"),
            Self::Truncated => f.write_str("the control socket ended inside a message"),
            Self::UnknownKind(k) => write!(f, "an unknown message kind 0x{k:02x}"),
            Self::Malformed(what) => write!(f, "a malformed message: {what}"),
            Self::NoDescriptor => f.write_str("a Connected message came with no descriptor"),
        }
    }
}

impl std::error::Error for ProtoError {}

impl From<std::io::Error> for ProtoError {
    fn from(e: std::io::Error) -> Self {
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

    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }

    fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }

    fn len(&mut self, n: usize) -> Result<(), ProtoError> {
        let n = u32::try_from(n).map_err(|_| ProtoError::TooLarge(n as u64))?;
        self.u32(n);
        Ok(())
    }

    fn bytes(&mut self, b: &[u8]) -> Result<(), ProtoError> {
        self.len(b.len())?;
        self.0.extend_from_slice(b);
        Ok(())
    }

    fn text(&mut self, s: &str) -> Result<(), ProtoError> {
        self.bytes(s.as_bytes())
    }

    fn path(&mut self, p: &std::path::Path) -> Result<(), ProtoError> {
        use std::os::unix::ffi::OsStrExt as _;
        self.bytes(p.as_os_str().as_bytes())
    }

    fn opt_u64(&mut self, v: Option<u64>) {
        match v {
            Some(v) => {
                self.u8(1);
                self.u64(v);
            }
            None => self.u8(0),
        }
    }

    fn list<T>(
        &mut self,
        items: &[T],
        mut each: impl FnMut(&mut Self, &T) -> Result<(), ProtoError>,
    ) -> Result<(), ProtoError> {
        self.len(items.len())?;
        items.iter().try_for_each(|item| each(self, item))
    }

    fn shares(&mut self, shares: &[ShareEntry]) -> Result<(), ProtoError> {
        self.list(shares, |p, s| {
            p.text(&s.name)?;
            p.path(&s.path)
        })
    }
}

/// A payload being read: every read is bounded by what is left.
struct Take<'a>(&'a [u8]);

impl Take<'_> {
    fn array<const N: usize>(&mut self) -> Result<[u8; N], ProtoError> {
        let (head, rest) = self
            .0
            .split_first_chunk::<N>()
            .ok_or(ProtoError::Malformed("a field runs past the payload"))?;
        self.0 = rest;
        Ok(*head)
    }

    fn u8(&mut self) -> Result<u8, ProtoError> {
        Ok(self.array::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, ProtoError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, ProtoError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn bool(&mut self) -> Result<bool, ProtoError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(ProtoError::Malformed("a flag that is not 0 or 1")),
        }
    }

    fn bytes(&mut self) -> Result<Vec<u8>, ProtoError> {
        let len = self.u32()? as usize;
        if len > self.0.len() {
            return Err(ProtoError::Malformed("a byte string runs past the payload"));
        }
        let (b, rest) = self.0.split_at(len);
        self.0 = rest;
        Ok(b.to_vec())
    }

    fn text(&mut self) -> Result<String, ProtoError> {
        String::from_utf8(self.bytes()?)
            .map_err(|_| ProtoError::Malformed("text that is not UTF-8"))
    }

    fn path(&mut self) -> Result<PathBuf, ProtoError> {
        use std::os::unix::ffi::OsStringExt as _;
        Ok(PathBuf::from(std::ffi::OsString::from_vec(self.bytes()?)))
    }

    fn opt_u64(&mut self) -> Result<Option<u64>, ProtoError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.u64()?)),
            _ => Err(ProtoError::Malformed("an option tag that is not 0 or 1")),
        }
    }

    fn list<T>(
        &mut self,
        min: usize,
        mut each: impl FnMut(&mut Self) -> Result<T, ProtoError>,
    ) -> Result<Vec<T>, ProtoError> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min) > self.0.len() {
            return Err(ProtoError::Malformed("a list runs past the payload"));
        }
        (0..n).map(|_| each(self)).collect()
    }

    fn shares(&mut self) -> Result<Vec<ShareEntry>, ProtoError> {
        self.list(8, |t| {
            Ok(ShareEntry {
                name: t.text()?,
                path: t.path()?,
            })
        })
    }

    fn end(&self) -> Result<(), ProtoError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(ProtoError::Malformed("bytes after the last field"))
        }
    }
}

fn put_plan(p: &mut Put, plan: &Plan) -> Result<(), ProtoError> {
    p.text(&plan.name)?;
    p.u32(plan.cpus);
    p.u64(plan.memory_bytes);
    p.path(&plan.kernel)?;
    p.path(&plan.initrd)?;
    p.text(&plan.command_line)?;
    p.list(&plan.disks, |p, d| {
        p.path(&d.path)?;
        p.bool(d.read_only);
        Ok(())
    })?;
    p.0.extend_from_slice(&plan.mac);
    p.path(&plan.boot_share)?;
    p.shares(&plan.shares)?;
    p.bool(plan.rosetta);
    p.path(&plan.console_log)?;
    p.text(&plan.agent_sha256)
}

fn take_plan(t: &mut Take<'_>) -> Result<Plan, ProtoError> {
    Ok(Plan {
        name: t.text()?,
        cpus: t.u32()?,
        memory_bytes: t.u64()?,
        kernel: t.path()?,
        initrd: t.path()?,
        command_line: t.text()?,
        disks: t.list(5, |t| {
            Ok(Disk {
                path: t.path()?,
                read_only: t.bool()?,
            })
        })?,
        mac: t.array()?,
        boot_share: t.path()?,
        shares: t.shares()?,
        rosetta: t.bool()?,
        console_log: t.path()?,
        agent_sha256: t.text()?,
    })
}

fn put_hello(p: &mut Put, h: &Hello) -> Result<(), ProtoError> {
    p.u32(h.proto);
    p.0.extend_from_slice(&h.agent_sha256);
    p.text(&h.boot_id)?;
    p.text(&h.kernel)?;
    p.u64(h.uptime_ms);
    Ok(())
}

fn take_hello(t: &mut Take<'_>) -> Result<Hello, ProtoError> {
    Ok(Hello {
        proto: t.u32()?,
        agent_sha256: t.array()?,
        boot_id: t.text()?,
        kernel: t.text()?,
        uptime_ms: t.u64()?,
    })
}

/// `payload` under `kind`, header first, refused past [`MAX_MESSAGE`].
fn frame(kind: u8, payload: Vec<u8>) -> Result<Vec<u8>, ProtoError> {
    if payload.len() > MAX_MESSAGE {
        return Err(ProtoError::TooLarge(payload.len() as u64));
    }
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

impl Request {
    /// The whole message, header first.
    ///
    /// # Errors
    /// Its payload is over [`MAX_MESSAGE`].
    pub fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        let mut p = Put(Vec::new());
        let kind = match self {
            Self::Attach(a) => {
                p.u32(a.proto);
                p.text(&a.helper_sha256)?;
                p.text(&a.agent_sha256)?;
                p.shares(&a.shares)?;
                p.bool(a.keep);
                ATTACH
            }
            Self::Boot(plan) => {
                put_plan(&mut p, plan)?;
                BOOT
            }
            Self::Connect { port } => {
                p.u32(*port);
                CONNECT
            }
            Self::Release { id } => {
                p.u64(*id);
                RELEASE
            }
            Self::Share(entries) => {
                p.shares(entries)?;
                SHARE
            }
            Self::Keep(keep) => {
                p.bool(*keep);
                KEEP
            }
            Self::State => STATE,
            Self::Stop { grace_ms } => {
                p.u64(*grace_ms);
                STOP
            }
            Self::Detach => DETACH,
        };
        frame(kind, p.0)
    }

    /// The request of `kind` whose payload is `payload`.
    ///
    /// # Errors
    /// An unknown kind, or a payload that is not that kind's.
    pub fn decode(kind: u8, payload: &[u8]) -> Result<Self, ProtoError> {
        let mut t = Take(payload);
        let request = match kind {
            ATTACH => {
                let proto = t.u32()?;
                if proto != PROTO {
                    // Another version's Attach: only its frozen first field is read, so the
                    // helper can refuse it by name.
                    return Ok(Self::Attach(Attach {
                        proto,
                        helper_sha256: String::new(),
                        agent_sha256: String::new(),
                        shares: Vec::new(),
                        keep: false,
                    }));
                }
                Self::Attach(Attach {
                    proto,
                    helper_sha256: t.text()?,
                    agent_sha256: t.text()?,
                    shares: t.shares()?,
                    keep: t.bool()?,
                })
            }
            BOOT => Self::Boot(Box::new(take_plan(&mut t)?)),
            CONNECT => Self::Connect { port: t.u32()? },
            RELEASE => Self::Release { id: t.u64()? },
            SHARE => Self::Share(t.shares()?),
            KEEP => Self::Keep(t.bool()?),
            STATE => {
                // Frozen: any later version's State may carry fields this one ignores.
                return Ok(Self::State);
            }
            STOP => {
                // Frozen: the grace is the first field; a later version's extra fields are
                // ignored, so every helper answers every lab's Stop.
                return Ok(Self::Stop { grace_ms: t.u64()? });
            }
            DETACH => Self::Detach,
            other => return Err(ProtoError::UnknownKind(other)),
        };
        t.end()?;
        Ok(request)
    }
}

impl Reply {
    /// The whole message, header first.
    ///
    /// # Errors
    /// Its payload is over [`MAX_MESSAGE`].
    pub fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        let mut p = Put(Vec::new());
        let kind = match self {
            Self::Attached {
                phase,
                keep,
                swapped,
            } => {
                p.u8(phase.byte());
                p.bool(*keep);
                p.bool(*swapped);
                ATTACHED
            }
            Self::Refused(why) => {
                p.text(why)?;
                REFUSED
            }
            Self::Started => STARTED,
            Self::Booting(what) => {
                p.text(what)?;
                BOOTING
            }
            Self::Ready(hello) => {
                put_hello(&mut p, hello)?;
                READY
            }
            Self::Connected { id, port } => {
                p.u64(*id);
                p.u32(*port);
                CONNECTED
            }
            Self::Done => DONE,
            Self::State(s) => {
                p.u32(s.pid);
                p.u64(s.uniq);
                p.text(&s.helper_sha256)?;
                p.u32(s.proto);
                p.path(&s.live_disk)?;
                p.u8(s.phase.byte());
                p.bool(s.keep);
                p.u32(s.sessions);
                p.text(&s.boot_id)?;
                p.text(&s.agent_sha256)?;
                p.list(&s.failed_units, |p, u| p.text(u))?;
                p.opt_u64(s.beat_ms_ago);
                p.opt_u64(s.free_bytes);
                p.text(&s.console_tail)?;
                STATE_IS
            }
            Self::Stopping => STOPPING,
            Self::Lost { why, tail } => {
                p.text(why)?;
                p.text(tail)?;
                LOST
            }
            Self::Failed { error, tail } => {
                p.text(error)?;
                p.text(tail)?;
                FAILED
            }
            Self::Error(why) => {
                p.text(why)?;
                ERROR
            }
        };
        frame(kind, p.0)
    }

    /// Whether this message's kind carries a descriptor.
    #[must_use]
    pub const fn carries_descriptor(kind: u8) -> bool {
        kind == CONNECTED
    }

    /// The reply of `kind` whose payload is `payload`.
    ///
    /// # Errors
    /// An unknown kind, or a payload that is not that kind's.
    pub fn decode(kind: u8, payload: &[u8]) -> Result<Self, ProtoError> {
        let mut t = Take(payload);
        let reply = match kind {
            ATTACHED => Self::Attached {
                phase: Phase::from_byte(t.u8()?)
                    .ok_or(ProtoError::Malformed("an unknown phase"))?,
                keep: t.bool()?,
                swapped: t.bool()?,
            },
            REFUSED => return Ok(Self::Refused(t.text()?)),
            STARTED => Self::Started,
            BOOTING => Self::Booting(t.text()?),
            READY => Self::Ready(take_hello(&mut t)?),
            CONNECTED => Self::Connected {
                id: t.u64()?,
                port: t.u32()?,
            },
            DONE => Self::Done,
            STATE_IS => Self::State(Box::new(StateReport {
                pid: t.u32()?,
                uniq: t.u64()?,
                helper_sha256: t.text()?,
                proto: t.u32()?,
                live_disk: t.path()?,
                phase: Phase::from_byte(t.u8()?)
                    .ok_or(ProtoError::Malformed("an unknown phase"))?,
                keep: t.bool()?,
                sessions: t.u32()?,
                boot_id: t.text()?,
                agent_sha256: t.text()?,
                failed_units: t.list(4, Take::text)?,
                beat_ms_ago: t.opt_u64()?,
                free_bytes: t.opt_u64()?,
                console_tail: t.text()?,
            })),
            STOPPING => return Ok(Self::Stopping),
            LOST => Self::Lost {
                why: t.text()?,
                tail: t.text()?,
            },
            FAILED => Self::Failed {
                error: t.text()?,
                tail: t.text()?,
            },
            ERROR => Self::Error(t.text()?),
            other => return Err(ProtoError::UnknownKind(other)),
        };
        t.end()?;
        Ok(reply)
    }
}

/// One whole message at the front of a buffer: its kind, its payload, and how many bytes of
/// the buffer it took.
pub type Message<'a> = (u8, &'a [u8], usize);

/// Split one whole message off the front of `buf`, or `None` while it is not all there.
///
/// # Errors
/// A header that claims more than [`MAX_MESSAGE`], refused before its payload arrives.
pub fn split(buf: &[u8]) -> Result<Option<Message<'_>>, ProtoError> {
    let Some(header) = buf.first_chunk::<HEADER>() else {
        return Ok(None);
    };
    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > MAX_MESSAGE {
        return Err(ProtoError::TooLarge(len as u64));
    }
    let end = HEADER + len;
    Ok(buf
        .get(HEADER..end)
        .map(|payload| (header[0], payload, end)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shares() -> Vec<ShareEntry> {
        vec![
            ShareEntry {
                name: "repo".into(),
                path: "/src/aterm".into(),
            },
            ShareEntry {
                name: "git".into(),
                path: "/src/aterm/.git".into(),
            },
        ]
    }

    fn plan() -> Plan {
        Plan {
            name: "aterm-linux".into(),
            cpus: 8,
            memory_bytes: 16 << 30,
            kernel: "/k/Image".into(),
            initrd: "/k/initrd".into(),
            command_line: crate::plan::COMMAND_LINE.into(),
            disks: vec![
                Disk {
                    path: "/live.img".into(),
                    read_only: false,
                },
                Disk {
                    path: "/seed.iso".into(),
                    read_only: true,
                },
            ],
            mac: crate::plan::mac_for("aterm-linux"),
            boot_share: "/agent/x".into(),
            shares: shares(),
            rosetta: true,
            console_log: "/run/console.log".into(),
            agent_sha256: "cd".repeat(32),
        }
    }

    fn hello() -> Hello {
        Hello {
            proto: aterm_vz_agent::wire::PROTO,
            agent_sha256: [7; 32],
            boot_id: "b00t".into(),
            kernel: "6.8.0-142-generic".into(),
            uptime_ms: 1830,
        }
    }

    fn requests() -> Vec<Request> {
        vec![
            Request::Attach(Attach {
                proto: PROTO,
                helper_sha256: "ab".repeat(32),
                agent_sha256: "cd".repeat(32),
                shares: shares(),
                keep: true,
            }),
            Request::Boot(Box::new(plan())),
            Request::Connect { port: 1024 },
            Request::Release { id: u64::MAX },
            Request::Share(Vec::new()),
            Request::Keep(false),
            Request::State,
            Request::Stop { grace_ms: 60_000 },
            Request::Detach,
        ]
    }

    fn replies() -> Vec<Reply> {
        vec![
            Reply::Attached {
                phase: Phase::Ready,
                keep: true,
                swapped: false,
            },
            Reply::Refused("another helper".into()),
            Reply::Started,
            Reply::Booting("systemd-networkd-wait-online.service (12 s)".into()),
            Reply::Ready(hello()),
            Reply::Connected { id: 3, port: 1024 },
            Reply::Done,
            Reply::State(Box::new(StateReport {
                pid: 4242,
                uniq: 99,
                helper_sha256: "ab".repeat(32),
                proto: PROTO,
                live_disk: "/vz/live/aterm-linux.img".into(),
                phase: Phase::Booting,
                keep: false,
                sessions: 2,
                boot_id: String::new(),
                agent_sha256: String::new(),
                failed_units: vec!["fwupd.service".into()],
                beat_ms_ago: Some(4_000),
                free_bytes: None,
                console_tail: "[  OK  ] Reached target local-fs.target.\n".into(),
            })),
            Reply::Stopping,
            Reply::Lost {
                why: "the guest stopped answering (no heartbeat for 60 s); VM stopped".into(),
                tail: "…".into(),
            },
            Reply::Failed {
                error: "VZErrorDomain 2: The storage device attachment is invalid.".into(),
                tail: String::new(),
            },
            Reply::Error("no session".into()),
        ]
    }

    fn one(bytes: &[u8]) -> (u8, &[u8]) {
        let (kind, payload, used) = split(bytes).unwrap().expect("whole");
        assert_eq!(used, bytes.len());
        (kind, payload)
    }

    #[test]
    fn every_request_and_reply_round_trips() {
        for r in requests() {
            let bytes = r.encode().unwrap();
            let (kind, payload) = one(&bytes);
            assert_eq!(Request::decode(kind, payload).unwrap(), r);
        }
        for r in replies() {
            let bytes = r.encode().unwrap();
            let (kind, payload) = one(&bytes);
            assert_eq!(Reply::decode(kind, payload).unwrap(), r);
            assert_eq!(
                Reply::carries_descriptor(kind),
                matches!(r, Reply::Connected { .. })
            );
        }
    }

    #[test]
    fn the_frozen_kinds_never_move() {
        let attach = requests()[0].encode().unwrap();
        assert_eq!(attach[0], 0x01);
        assert_eq!(
            &attach[HEADER..HEADER + 4],
            &PROTO.to_le_bytes(),
            "proto leads"
        );
        assert_eq!(Request::Stop { grace_ms: 1 }.encode().unwrap()[0], 0x02);
        assert_eq!(Request::State.encode().unwrap()[0], 0x03);
        assert_eq!(Reply::Refused(String::new()).encode().unwrap()[0], 0x81);
        assert_eq!(Reply::Stopping.encode().unwrap()[0], 0x82);
    }

    #[test]
    fn another_versions_attach_and_stop_still_read() {
        // A later version's Attach: its proto, then fields this version does not know.
        let mut payload = (PROTO + 1).to_le_bytes().to_vec();
        payload.extend_from_slice(&[0xff; 9]);
        assert!(matches!(
            Request::decode(0x01, &payload),
            Ok(Request::Attach(Attach { proto, .. })) if proto == PROTO + 1
        ));
        // Its Stop, with a field after the grace.
        let mut payload = 500u64.to_le_bytes().to_vec();
        payload.push(1);
        assert_eq!(
            Request::decode(0x02, &payload).unwrap(),
            Request::Stop { grace_ms: 500 }
        );
        assert_eq!(Request::decode(0x03, &[1, 2, 3]).unwrap(), Request::State);
    }

    #[test]
    fn a_split_waits_for_the_whole_message_and_refuses_an_oversized_one() {
        let bytes = Request::Connect { port: 1024 }.encode().unwrap();
        for cut in 0..bytes.len() {
            assert!(split(&bytes[..cut]).unwrap().is_none(), "cut at {cut}");
        }
        let mut two = bytes.clone();
        two.extend_from_slice(&Request::Detach.encode().unwrap());
        let (_, _, used) = split(&two).unwrap().unwrap();
        assert_eq!(used, bytes.len());
        let mut huge = vec![0x11];
        huge.extend_from_slice(&((MAX_MESSAGE as u32) + 1).to_le_bytes());
        assert!(matches!(split(&huge), Err(ProtoError::TooLarge(_))));
        assert!(matches!(
            Reply::Booting("x".repeat(MAX_MESSAGE)).encode(),
            Err(ProtoError::TooLarge(_))
        ));
    }

    #[test]
    fn unknown_kinds_and_malformed_payloads_are_named() {
        assert!(matches!(
            Request::decode(0x7f, &[]),
            Err(ProtoError::UnknownKind(0x7f))
        ));
        assert!(matches!(
            Reply::decode(0x00, &[]),
            Err(ProtoError::UnknownKind(0x00))
        ));
        assert!(matches!(
            Request::decode(0x14, &[2]),
            Err(ProtoError::Malformed(_))
        ));
        assert!(matches!(
            Request::decode(0x11, &[0, 4, 0, 0, 9]),
            Err(ProtoError::Malformed(_))
        ));
        assert!(matches!(
            Reply::decode(0x90, &[9, 0, 0]),
            Err(ProtoError::Malformed(_))
        ));
        assert!(matches!(
            Request::decode(0x13, &[0xff, 0xff, 0xff, 0x7f]),
            Err(ProtoError::Malformed(_))
        ));
    }
}
