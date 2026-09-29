// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE AKP1 WIRE (`docs/DESIGN-pty-keeper-2026-09-26.md` §5.2): fixed 8-byte
//! headers read before any variable byte, bounded bodies, at most one
//! descriptor per frame riding the header.
//!
//! ```text
//! header  "AKP1" | kind u8 | flags u8 | body length u16be
//! body    kind-specific, fixed fields first, every variable part length-first
//! ```
//!
//! GROWTH RULE (LAW L3 of the seamless handoff): a frame may only grow. A
//! reader takes the fields it knows from the front of a body and ignores the
//! rest, so a newer writer's longer frame is read by an older reader, and a
//! body SHORTER than its kind's fixed fields is refused. Kinds are never
//! renumbered; an unknown kind is refused by name, never guessed at.
//!
//! This module is pure bytes: no socket, no descriptor. The descriptor a
//! REGISTER or an OFFER carries travels beside the header (`fdpass`), and the
//! codec only says which kinds may carry one ([`Kind::carries_fd`]).

/// The magic every frame opens with.
pub const MAGIC: [u8; 4] = *b"AKP1";
/// The header's size.
pub const HEADER_LEN: usize = 8;
/// The protocol version this build speaks (in HELLO and WELCOME).
pub const PROTO: u16 = 1;
/// The largest opaque tag a record may carry (sid, identity label, topics…).
pub const MAX_TAG: usize = 1024;
/// The largest opaque META (the Repaint rung's scalar state).
pub const MAX_META: usize = 4096;
/// The largest body any frame may declare: a header, a tag and a META, plus
/// room to grow. A larger declared length is refused before a byte of it is read.
pub const MAX_BODY: usize = 8192;
/// The longest build/version string a HELLO or WELCOME carries.
pub const MAX_LABEL: usize = 64;
/// The longest STATUS reply text.
pub const MAX_STATUS: usize = MAX_BODY - 2;
/// The longest crash-marker directory a HELLO names ([`MarkerRef`]).
pub const MAX_MARKER_DIR: usize = 1024;

/// Header flag: this frame carries exactly one descriptor.
pub const FLAG_FD: u8 = 0x01;

/// HELLO capability bit: this peer sends BYE on its quit path (§5.4 row 2). A
/// peer without it is judged as a pre-keeper build (row 3) when it ends.
pub const CAP_SENDS_BYE: u8 = 0x01;
/// HELLO capability bit: this is a window's LINK (re)connecting to register
/// what it holds — not a launch. It is offered nothing and does not reset the
/// relaunch brake; a window's boot HELLO, which does take offers, omits it.
pub const CAP_LINK: u8 = 0x02;

/// The frame kinds. Numbers are the wire's and never change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// W→K: protocol, peer class, capabilities, build.
    Hello = 1,
    /// K→W: protocol, keeper version, how many OFFERs follow.
    Welcome = 2,
    /// W→K, with the master: the fixed header and the tag.
    Register = 3,
    /// W→K: an rdev and its current Repaint-rung META.
    Meta = 4,
    /// W→K: an rdev whose tab or pane closed.
    Release = 5,
    /// W→K: the update successor's kernel pid, before the grant.
    Pending = 6,
    /// W→K: quit intent, written just before `exit(0)`.
    Bye = 7,
    /// K→W', with the master: the header, the tag, the META.
    Offer = 8,
    /// K→W: how many orphans a live sibling may adopt.
    Orphans = 9,
    /// Either way: an empty request, or the keeper's text reply.
    Status = 10,
    /// W→K: an updated window asks an older keeper to restart when safe (P5).
    RestartWhenSafe = 11,
    /// W→K: offer me the orphans now (a live sibling answering ORPHANS).
    Adopt = 12,
    /// K→W: a frame was refused (a reason code and the rdev it named).
    Refused = 13,
}

impl Kind {
    /// The kind a wire byte names, or `None` for one this build does not know.
    #[must_use]
    pub fn from_byte(b: u8) -> Option<Kind> {
        Some(match b {
            1 => Kind::Hello,
            2 => Kind::Welcome,
            3 => Kind::Register,
            4 => Kind::Meta,
            5 => Kind::Release,
            6 => Kind::Pending,
            7 => Kind::Bye,
            8 => Kind::Offer,
            9 => Kind::Orphans,
            10 => Kind::Status,
            11 => Kind::RestartWhenSafe,
            12 => Kind::Adopt,
            13 => Kind::Refused,
            _ => return None,
        })
    }

    /// Whether frames of this kind carry exactly one descriptor. Every other
    /// kind carries none, and a descriptor that arrives with one is a protocol
    /// error (closed, and the connection dropped).
    #[must_use]
    pub fn carries_fd(self) -> bool {
        matches!(self, Kind::Register | Kind::Offer)
    }
}

/// A codec refusal. Every one ends the connection that sent it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireError {
    /// The header does not open with `AKP1`.
    BadMagic,
    /// A kind this build does not know.
    UnknownKind(u8),
    /// A declared body longer than [`MAX_BODY`].
    TooLong(usize),
    /// A body shorter than its kind's fixed fields, or a length-first part
    /// running past the end.
    Short(Kind),
    /// A tag, META, label or status text over its bound.
    Oversize(Kind),
    /// The FD flag disagrees with the kind.
    FdFlag(Kind),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::BadMagic => write!(f, "not an AKP1 frame"),
            WireError::UnknownKind(k) => write!(f, "unknown frame kind {k}"),
            WireError::TooLong(n) => write!(f, "declared body of {n} bytes exceeds {MAX_BODY}"),
            WireError::Short(k) => write!(f, "{k:?} body too short"),
            WireError::Oversize(k) => write!(f, "{k:?} carries an oversize part"),
            WireError::FdFlag(k) => write!(f, "{k:?} descriptor flag disagrees with its kind"),
        }
    }
}

/// A decoded header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub kind: Kind,
    pub flags: u8,
    pub len: usize,
}

/// Encode a header for `kind` with a body of `len` bytes.
///
/// # Errors
/// `TooLong` for a body over [`MAX_BODY`].
pub fn encode_header(kind: Kind, len: usize) -> Result<[u8; HEADER_LEN], WireError> {
    let wire_len = u16::try_from(len)
        .ok()
        .filter(|_| len <= MAX_BODY)
        .ok_or(WireError::TooLong(len))?;
    let flags = if kind.carries_fd() { FLAG_FD } else { 0 };
    let l = wire_len.to_be_bytes();
    Ok([
        MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], kind as u8, flags, l[0], l[1],
    ])
}

/// Decode a header, refusing a bad magic, an unknown kind, an overlong body
/// and an FD flag that disagrees with the kind — before a body byte is read.
///
/// # Errors
/// See [`WireError`].
pub fn decode_header(h: &[u8; HEADER_LEN]) -> Result<Header, WireError> {
    if h[..4] != MAGIC {
        return Err(WireError::BadMagic);
    }
    let kind = Kind::from_byte(h[4]).ok_or(WireError::UnknownKind(h[4]))?;
    let flags = h[5];
    let len = usize::from(u16::from_be_bytes([h[6], h[7]]));
    if len > MAX_BODY {
        return Err(WireError::TooLong(len));
    }
    if (flags & FLAG_FD != 0) != kind.carries_fd() {
        return Err(WireError::FdFlag(kind));
    }
    Ok(Header { kind, flags, len })
}

/// A process's start time as the wire carries it (seconds, microseconds).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Birth {
    pub seconds: u64,
    pub micros: u32,
}

/// A registered master's FIXED header: the fields the keeper reads. Everything
/// else about a session rides the opaque tag and META, which it never decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MasterHeader {
    /// The master's `st_rdev`: its identity across processes (F10).
    pub rdev: u64,
    /// The shell (session leader) on its slave.
    pub shell_pid: u32,
    /// The shell's kernel start time, so a recycled pid is never mistaken.
    pub shell_birth: Birth,
    /// The window's leaf id for this terminal (the journal joins on it).
    pub local_id: u64,
}

/// The encoded size of a [`MasterHeader`].
pub const MASTER_HEADER_LEN: usize = 8 + 4 + 8 + 4 + 8;

/// WHERE A WINDOW'S CRASH MARKER IS (§5.4 row 3's cross-check): the window's
/// log directory and the start its marker is named with. The keeper never takes
/// a file name from the window: it builds
/// `crash-marker-<pid>-<nanos>-<app|other>.log` itself, with the pid the KERNEL
/// named for the connection, so a HELLO can point the keeper at no file but
/// that window's own marker. The window holds the marker's owner lock for its
/// whole life and its exit path unlinks it (`aterm-gui`'s `crash_signal`), so
/// after the window's death the marker's absence says its exit path ran, and a
/// marker whose lock is free says it did not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkerRef {
    /// The window's start, nanoseconds since the epoch, as its marker's name
    /// spells it.
    pub nanos: u64,
    /// The directory the marker lives in: an absolute path, as bytes.
    pub dir: Vec<u8>,
}

/// The peer classes a HELLO names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerClass {
    /// A window: may register, claim and be offered masters.
    App = 1,
    /// `aterm keeper status` and the like: reads only.
    Status = 2,
}

impl PeerClass {
    fn from_byte(b: u8) -> Option<PeerClass> {
        match b {
            1 => Some(PeerClass::App),
            2 => Some(PeerClass::Status),
            _ => None,
        }
    }
}

/// A decoded frame body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Hello {
        proto: u16,
        class: PeerClass,
        caps: u8,
        build: String,
        /// The window's crash marker (grown after P3: an older window's HELLO
        /// ends before it, and an older keeper ignores it).
        marker: Option<MarkerRef>,
    },
    Welcome {
        proto: u16,
        offers: u16,
        version: String,
    },
    Register {
        header: MasterHeader,
        tag: Vec<u8>,
    },
    Meta {
        rdev: u64,
        meta: Vec<u8>,
    },
    Release {
        rdev: u64,
    },
    Pending {
        pid: u32,
    },
    Bye,
    Offer {
        header: MasterHeader,
        tag: Vec<u8>,
        meta: Vec<u8>,
    },
    Orphans {
        n: u16,
    },
    Status {
        text: String,
    },
    RestartWhenSafe,
    Adopt,
    Refused {
        code: RefuseCode,
        rdev: u64,
    },
}

/// Why the keeper refused a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefuseCode {
    /// The descriptor's `st_rdev` is not the rdev the header claims.
    RdevMismatch = 1,
    /// The keeper already holds its cap of masters.
    TooManyMasters = 2,
    /// The master already has its cap of claimants.
    TooManyClaimants = 3,
    /// The frame named a master this connection has no claim on.
    NotClaimant = 4,
    /// The frame is not one this peer's class may send.
    WrongClass = 5,
    /// A REGISTER arrived without its descriptor.
    NoDescriptor = 6,
    /// A code this build does not know (a newer keeper).
    Other = 255,
}

impl RefuseCode {
    fn from_byte(b: u8) -> RefuseCode {
        match b {
            1 => RefuseCode::RdevMismatch,
            2 => RefuseCode::TooManyMasters,
            3 => RefuseCode::TooManyClaimants,
            4 => RefuseCode::NotClaimant,
            5 => RefuseCode::WrongClass,
            6 => RefuseCode::NoDescriptor,
            _ => RefuseCode::Other,
        }
    }
}

impl Frame {
    /// The kind this frame is sent as.
    #[must_use]
    pub fn kind(&self) -> Kind {
        match self {
            Frame::Hello { .. } => Kind::Hello,
            Frame::Welcome { .. } => Kind::Welcome,
            Frame::Register { .. } => Kind::Register,
            Frame::Meta { .. } => Kind::Meta,
            Frame::Release { .. } => Kind::Release,
            Frame::Pending { .. } => Kind::Pending,
            Frame::Bye => Kind::Bye,
            Frame::Offer { .. } => Kind::Offer,
            Frame::Orphans { .. } => Kind::Orphans,
            Frame::Status { .. } => Kind::Status,
            Frame::RestartWhenSafe => Kind::RestartWhenSafe,
            Frame::Adopt => Kind::Adopt,
            Frame::Refused { .. } => Kind::Refused,
        }
    }

    /// Encode header + body.
    ///
    /// # Errors
    /// `Oversize` for a part over its bound, `TooLong` for a body over
    /// [`MAX_BODY`].
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        let kind = self.kind();
        let mut body = Vec::new();
        match self {
            Frame::Hello {
                proto,
                class,
                caps,
                build,
                marker,
            } => {
                body.extend_from_slice(&proto.to_be_bytes());
                body.push(*class as u8);
                body.push(*caps);
                put_short(&mut body, build.as_bytes(), MAX_LABEL, kind)?;
                if let Some(m) = marker {
                    body.extend_from_slice(&m.nanos.to_be_bytes());
                    put_long(&mut body, &m.dir, MAX_MARKER_DIR, kind)?;
                }
            }
            Frame::Welcome {
                proto,
                offers,
                version,
            } => {
                body.extend_from_slice(&proto.to_be_bytes());
                body.extend_from_slice(&offers.to_be_bytes());
                put_short(&mut body, version.as_bytes(), MAX_LABEL, kind)?;
            }
            Frame::Register { header, tag } => {
                put_header(&mut body, header);
                put_long(&mut body, tag, MAX_TAG, kind)?;
            }
            Frame::Meta { rdev, meta } => {
                body.extend_from_slice(&rdev.to_be_bytes());
                put_long(&mut body, meta, MAX_META, kind)?;
            }
            Frame::Release { rdev } => body.extend_from_slice(&rdev.to_be_bytes()),
            Frame::Pending { pid } => body.extend_from_slice(&pid.to_be_bytes()),
            Frame::Bye | Frame::RestartWhenSafe | Frame::Adopt => {}
            Frame::Offer { header, tag, meta } => {
                put_header(&mut body, header);
                put_long(&mut body, tag, MAX_TAG, kind)?;
                put_long(&mut body, meta, MAX_META, kind)?;
            }
            Frame::Orphans { n } => body.extend_from_slice(&n.to_be_bytes()),
            Frame::Status { text } => put_long(&mut body, text.as_bytes(), MAX_STATUS, kind)?,
            Frame::Refused { code, rdev } => {
                body.push(*code as u8);
                body.extend_from_slice(&rdev.to_be_bytes());
            }
        }
        let mut out = encode_header(kind, body.len())?.to_vec();
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Decode the body of a frame whose header was `header`. Fields past the
    /// ones this build knows are ignored (the growth rule).
    ///
    /// # Errors
    /// `Short` for a truncated body, `Oversize` for a part over its bound.
    pub fn decode(header: &Header, body: &[u8]) -> Result<Frame, WireError> {
        let kind = header.kind;
        let mut r = Reader {
            b: body,
            at: 0,
            kind,
        };
        Ok(match kind {
            Kind::Hello => {
                let proto = r.u16()?;
                let class_byte = r.u8()?;
                let caps = r.u8()?;
                let build = r.short_str(MAX_LABEL)?;
                // An unknown class is refused rather than defaulted: a class
                // decides what the peer may do.
                let class = PeerClass::from_byte(class_byte).ok_or(WireError::Short(kind))?;
                // The marker, when the body goes on (the growth rule: a HELLO
                // that ends here is an older window's).
                let marker = if r.at < body.len() {
                    Some(MarkerRef {
                        nanos: r.u64()?,
                        dir: r.long(MAX_MARKER_DIR)?,
                    })
                } else {
                    None
                };
                Frame::Hello {
                    proto,
                    class,
                    caps,
                    build,
                    marker,
                }
            }
            Kind::Welcome => Frame::Welcome {
                proto: r.u16()?,
                offers: r.u16()?,
                version: r.short_str(MAX_LABEL)?,
            },
            Kind::Register => Frame::Register {
                header: r.master_header()?,
                tag: r.long(MAX_TAG)?,
            },
            Kind::Meta => Frame::Meta {
                rdev: r.u64()?,
                meta: r.long(MAX_META)?,
            },
            Kind::Release => Frame::Release { rdev: r.u64()? },
            Kind::Pending => Frame::Pending { pid: r.u32()? },
            Kind::Bye => Frame::Bye,
            Kind::Offer => Frame::Offer {
                header: r.master_header()?,
                tag: r.long(MAX_TAG)?,
                meta: r.long(MAX_META)?,
            },
            Kind::Orphans => Frame::Orphans { n: r.u16()? },
            Kind::Status => {
                // An empty body is the request; a reply carries text.
                if body.is_empty() {
                    Frame::Status {
                        text: String::new(),
                    }
                } else {
                    let raw = r.long(MAX_STATUS)?;
                    Frame::Status {
                        text: String::from_utf8_lossy(&raw).into_owned(),
                    }
                }
            }
            Kind::RestartWhenSafe => Frame::RestartWhenSafe,
            Kind::Adopt => Frame::Adopt,
            Kind::Refused => Frame::Refused {
                code: RefuseCode::from_byte(r.u8()?),
                rdev: r.u64()?,
            },
        })
    }
}

fn put_header(body: &mut Vec<u8>, h: &MasterHeader) {
    body.extend_from_slice(&h.rdev.to_be_bytes());
    body.extend_from_slice(&h.shell_pid.to_be_bytes());
    body.extend_from_slice(&h.shell_birth.seconds.to_be_bytes());
    body.extend_from_slice(&h.shell_birth.micros.to_be_bytes());
    body.extend_from_slice(&h.local_id.to_be_bytes());
}

fn put_short(body: &mut Vec<u8>, part: &[u8], max: usize, kind: Kind) -> Result<(), WireError> {
    let len = u8::try_from(part.len())
        .ok()
        .filter(|_| part.len() <= max)
        .ok_or(WireError::Oversize(kind))?;
    body.push(len);
    body.extend_from_slice(part);
    Ok(())
}

fn put_long(body: &mut Vec<u8>, part: &[u8], max: usize, kind: Kind) -> Result<(), WireError> {
    let len = u16::try_from(part.len())
        .ok()
        .filter(|_| part.len() <= max)
        .ok_or(WireError::Oversize(kind))?;
    body.extend_from_slice(&len.to_be_bytes());
    body.extend_from_slice(part);
    Ok(())
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
    kind: Kind,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], WireError> {
        let end = self.at.checked_add(n).ok_or(WireError::Short(self.kind))?;
        let s = self
            .b
            .get(self.at..end)
            .ok_or(WireError::Short(self.kind))?;
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, WireError> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32, WireError> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u64(&mut self) -> Result<u64, WireError> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(u64::from_be_bytes(a))
    }
    fn master_header(&mut self) -> Result<MasterHeader, WireError> {
        Ok(MasterHeader {
            rdev: self.u64()?,
            shell_pid: self.u32()?,
            shell_birth: Birth {
                seconds: self.u64()?,
                micros: self.u32()?,
            },
            local_id: self.u64()?,
        })
    }
    fn long(&mut self, max: usize) -> Result<Vec<u8>, WireError> {
        let n = usize::from(self.u16()?);
        if n > max {
            return Err(WireError::Oversize(self.kind));
        }
        Ok(self.take(n)?.to_vec())
    }
    fn short_str(&mut self, max: usize) -> Result<String, WireError> {
        let n = usize::from(self.u8()?);
        if n > max {
            return Err(WireError::Oversize(self.kind));
        }
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round(frame: Frame) {
        let bytes = frame.encode().expect("encode");
        let mut h = [0u8; HEADER_LEN];
        h.copy_from_slice(&bytes[..HEADER_LEN]);
        let header = decode_header(&h).expect("header");
        assert_eq!(header.len, bytes.len() - HEADER_LEN);
        assert_eq!(header.flags & FLAG_FD != 0, frame.kind().carries_fd());
        let back = Frame::decode(&header, &bytes[HEADER_LEN..]).expect("decode");
        assert_eq!(back, frame);
    }

    fn header() -> MasterHeader {
        MasterHeader {
            rdev: 0x0f00_000a,
            shell_pid: 4242,
            shell_birth: Birth {
                seconds: 1_790_000_000,
                micros: 123_456,
            },
            local_id: 7,
        }
    }

    #[test]
    fn every_frame_round_trips() {
        round(Frame::Hello {
            proto: PROTO,
            class: PeerClass::App,
            caps: CAP_SENDS_BYE,
            build: "0.98.0+gdeadbeef".into(),
            marker: None,
        });
        round(Frame::Hello {
            proto: PROTO,
            class: PeerClass::App,
            caps: CAP_SENDS_BYE,
            build: "0.98.0+gdeadbeef".into(),
            marker: Some(MarkerRef {
                nanos: 1_790_000_000_123_456_789,
                dir: b"/Users//x/Library/Logs/aterm".to_vec(),
            }),
        });
        round(Frame::Welcome {
            proto: PROTO,
            offers: 3,
            version: "0.98.0".into(),
        });
        round(Frame::Register {
            header: header(),
            tag: b"sid=s-1".to_vec(),
        });
        round(Frame::Meta {
            rdev: 9,
            meta: vec![1; MAX_META],
        });
        round(Frame::Release { rdev: 9 });
        round(Frame::Pending { pid: 77 });
        round(Frame::Bye);
        round(Frame::Offer {
            header: header(),
            tag: vec![2; MAX_TAG],
            meta: vec![3; 10],
        });
        round(Frame::Orphans { n: 2 });
        round(Frame::Status {
            text: "keeper=running".into(),
        });
        round(Frame::Status {
            text: String::new(),
        });
        round(Frame::RestartWhenSafe);
        round(Frame::Adopt);
        round(Frame::Refused {
            code: RefuseCode::RdevMismatch,
            rdev: 5,
        });
    }

    #[test]
    fn headers_are_refused_before_the_body() {
        let mut h = encode_header(Kind::Bye, 0).expect("header");
        h[0] = b'X';
        assert_eq!(decode_header(&h), Err(WireError::BadMagic));
        let mut h = encode_header(Kind::Bye, 0).expect("header");
        h[4] = 200;
        assert_eq!(decode_header(&h), Err(WireError::UnknownKind(200)));
        let mut h = encode_header(Kind::Bye, 0).expect("header");
        h[6..8].copy_from_slice(&u16::try_from(MAX_BODY + 1).unwrap().to_be_bytes());
        assert_eq!(decode_header(&h), Err(WireError::TooLong(MAX_BODY + 1)));
        // A descriptor flag on a kind that carries none, and none on one that does.
        let mut h = encode_header(Kind::Bye, 0).expect("header");
        h[5] = FLAG_FD;
        assert_eq!(decode_header(&h), Err(WireError::FdFlag(Kind::Bye)));
        let mut h = encode_header(Kind::Register, 0).expect("header");
        h[5] = 0;
        assert_eq!(decode_header(&h), Err(WireError::FdFlag(Kind::Register)));
    }

    #[test]
    fn oversize_parts_are_refused_both_ways() {
        assert_eq!(
            Frame::Register {
                header: header(),
                tag: vec![0; MAX_TAG + 1],
            }
            .encode(),
            Err(WireError::Oversize(Kind::Register))
        );
        // A hand-built body declaring a META over the bound.
        let mut body = 9u64.to_be_bytes().to_vec();
        body.extend_from_slice(&u16::try_from(MAX_META + 1).unwrap().to_be_bytes());
        body.resize(body.len() + MAX_META + 1, 0);
        let h = Header {
            kind: Kind::Meta,
            flags: 0,
            len: body.len(),
        };
        assert_eq!(
            Frame::decode(&h, &body),
            Err(WireError::Oversize(Kind::Meta))
        );
    }

    /// The growth rule: a longer body than this build knows is read by its
    /// known prefix; a shorter one is refused.
    #[test]
    fn frames_only_grow() {
        let mut bytes = Frame::Release { rdev: 11 }.encode().expect("encode");
        bytes.extend_from_slice(b"future");
        let body = &bytes[HEADER_LEN..];
        let h = Header {
            kind: Kind::Release,
            flags: 0,
            len: body.len(),
        };
        assert_eq!(Frame::decode(&h, body), Ok(Frame::Release { rdev: 11 }));
        let h = Header {
            kind: Kind::Release,
            flags: 0,
            len: 3,
        };
        assert_eq!(
            Frame::decode(&h, &[0, 0, 0]),
            Err(WireError::Short(Kind::Release))
        );
    }

    /// A HELLO from a window before the marker grew is read with none, and a
    /// marker directory over its bound is refused.
    #[test]
    fn a_hello_without_a_marker_is_an_older_window_s() {
        let old = Frame::Hello {
            proto: PROTO,
            class: PeerClass::App,
            caps: CAP_SENDS_BYE,
            build: "0.97.0".into(),
            marker: None,
        }
        .encode()
        .expect("encode");
        let body = &old[HEADER_LEN..];
        // Exactly the P3 layout: proto, class, caps, build.
        assert_eq!(body.len(), 2 + 1 + 1 + 1 + "0.97.0".len());
        let mut h = [0u8; HEADER_LEN];
        h.copy_from_slice(&old[..HEADER_LEN]);
        let header = decode_header(&h).expect("header");
        assert!(matches!(
            Frame::decode(&header, body),
            Ok(Frame::Hello { marker: None, .. })
        ));
        assert_eq!(
            Frame::Hello {
                proto: PROTO,
                class: PeerClass::App,
                caps: 0,
                build: String::new(),
                marker: Some(MarkerRef {
                    nanos: 1,
                    dir: vec![b'/'; MAX_MARKER_DIR + 1],
                }),
            }
            .encode(),
            Err(WireError::Oversize(Kind::Hello))
        );
    }

    #[test]
    fn an_unknown_peer_class_is_refused() {
        let mut body = PROTO.to_be_bytes().to_vec();
        body.extend_from_slice(&[9, 0, 0]);
        let h = Header {
            kind: Kind::Hello,
            flags: 0,
            len: body.len(),
        };
        assert!(Frame::decode(&h, &body).is_err());
    }
}
