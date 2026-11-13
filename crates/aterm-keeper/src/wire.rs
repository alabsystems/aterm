// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The AKP1 wire (`docs/DESIGN-pty-keeper-2026-09-26.md` §5.2): fixed 8-byte headers read before any variable byte, bounded bodies, at most one descriptor per frame riding the header.

use crate::identity::IdentityPolicy;

/// The magic every frame opens with.
pub const MAGIC: [u8; 4] = *b"AKP1";
/// The header's size.
pub const HEADER_LEN: usize = 8;
/// The protocol version this build speaks (in HELLO and WELCOME).
pub const PROTO: u16 = 1;
/// The largest opaque tag a record may carry (sid, owner, identity label, channel flags…), fixed at REGISTER: what changes after it rides META.
pub const MAX_TAG: usize = 1024;
/// The largest opaque META (the Repaint rung's scalar state).
pub const MAX_META: usize = 4096;
/// The largest body any frame may declare: a header, a tag and a META, plus room to grow.
pub const MAX_BODY: usize = 8192;
/// The most records one BYE names to keep past the quit ([`Frame::Bye`]).
pub const MAX_KEEPS: usize = 64;
/// The longest build/version string a HELLO or WELCOME carries.
pub const MAX_LABEL: usize = 64;
/// The longest STATUS reply text.
pub const MAX_STATUS: usize = MAX_BODY - 2;
/// The longest crash-marker directory a HELLO names ([`MarkerRef`]).
pub const MAX_MARKER_DIR: usize = 1024;

/// Header flag: this frame carries exactly one descriptor.
pub const FLAG_FD: u8 = 0x01;

/// HELLO capability bit: this peer sends BYE on its quit path (§5.4 row 2).
pub const CAP_SENDS_BYE: u8 = 0x01;
/// HELLO capability bit: a window's link (re)connecting to register what it holds, not a launch.
pub const CAP_LINK: u8 = 0x02;
/// HELLO capability bit (and WELCOME's trailing caps byte, from a keeper): this peer speaks the crash-restore journal's frames.
pub const CAP_JOURNAL: u8 = 0x04;
/// WELCOME's trailing caps bit, from a keeper: it answers a SYNC with the same number once it has read every frame written before it on that connection.
pub const CAP_SYNC: u8 = 0x08;
/// WELCOME's trailing caps bit, from a keeper: it honours a BYE's keep set ([`Frame::Bye`]),
/// leaving each record it names, its shell alive, for the next window's HELLO. An older keeper
/// reads such a BYE as a plain one (the decoder's growth rule) and closes them all, so a window
/// carries a session past its quit only to a keeper that states this. As a HELLO bit, from a
/// window: it reads an OFFER's kept flag ([`Frame::Offer`]'s `kept`), which a keeper writes only
/// to a window that states it.
pub const CAP_KEEPS: u8 = 0x10;
/// An OFFER's kept flag bit: a quit's BYE kept the record past it
/// ([`Frame::Offer`]'s `kept`), not a crash.
const OFFER_KEPT: u8 = 0x01;
/// The one companion kind this build knows: a crash-restore journal region (`aterm-restore`).
pub const COMPANION_JOURNAL: u8 = 1;

/// The frame kinds.
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
    /// W→K: quit intent, written just before `exit(0)`, with the records to keep past it.
    Bye = 7,
    /// K→W', with the master: the header, the tag, the META.
    Offer = 8,
    /// K→W: how many orphans a live sibling may adopt.
    Orphans = 9,
    /// Either way: an empty request, or the keeper's text reply.
    Status = 10,
    /// W→K: an updated window asks an older keeper to restart when safe (the keeper ignores it).
    RestartWhenSafe = 11,
    /// W→K: offer me the orphans now (a live sibling answering ORPHANS).
    Adopt = 12,
    /// K→W: a frame was refused (a reason code and the rdev it named).
    Refused = 13,
    /// W→K, with a read-only POSIX shared-memory descriptor: the journal region of a master this connection claims (the rdev, the kind, the region's length).
    Companion = 14,
    /// K→W', with a duplicate of the companion's descriptor: sent right after an OFFER that said `companions = 1`, for that OFFER's master.
    CompanionOffer = 15,
    /// W→K: drop this claim's companion and end its poison window (a window with no journal for the master: its region could not be made, or memory pressure shed it).
    CompanionRelease = 16,
    /// Either way: W→K a number; K→W the same number, written once the keeper has read every frame written before it on the connection (a round trip, to a keeper that states [`CAP_SYNC`]).
    Sync = 17,
}

impl Kind {
    /// The kind a wire byte names, or `None` for one this build does not know.
    #[inline]
    #[must_use]
    pub const fn from_byte(b: u8) -> Option<Kind> {
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
            14 => Kind::Companion,
            15 => Kind::CompanionOffer,
            16 => Kind::CompanionRelease,
            17 => Kind::Sync,
            _ => return None,
        })
    }

    /// Whether frames of this kind carry exactly one descriptor.
    #[inline]
    #[must_use]
    pub const fn carries_fd(self) -> bool {
        matches!(
            self,
            Kind::Register | Kind::Offer | Kind::Companion | Kind::CompanionOffer
        )
    }
}

/// A codec refusal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WireError {
    /// The header does not open with `AKP1`.
    BadMagic,
    /// A kind this build does not know.
    UnknownKind(u8),
    /// A declared body longer than [`MAX_BODY`].
    TooLong(usize),
    /// A body shorter than its kind's fixed fields, or a length-first part running past the end.
    Short(Kind),
    /// A tag, META, label or status text over its bound.
    Oversize(Kind),
    /// The FD flag disagrees with the kind.
    FdFlag(Kind),
    /// A HELLO class byte this build does not know.
    UnknownClass(u8),
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
            WireError::UnknownClass(c) => write!(f, "unknown peer class {c}"),
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

/// Decode a header, refusing a bad magic, an unknown kind, an overlong body and an FD flag that disagrees with the kind — before a body byte is read.
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

/// A registered master's fixed header: the fields the keeper reads.
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

/// Where a window's crash marker is (§5.4 row 3's cross-check): its log directory and the start its marker is named with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkerRef {
    /// The window's start, nanoseconds since the epoch, as its marker's name spells it.
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
    #[inline]
    const fn from_byte(b: u8) -> Option<PeerClass> {
        match b {
            1 => Some(PeerClass::App),
            2 => Some(PeerClass::Status),
            _ => None,
        }
    }
}

/// A WELCOME's policy byte. Numbers are the wire's and never change.
#[inline]
const fn policy_byte(policy: IdentityPolicy) -> u8 {
    match policy {
        IdentityPolicy::SameUid => 1,
        IdentityPolicy::Designated => 2,
    }
}

/// The policy a WELCOME's byte names; `None` for one this build does not know.
#[inline]
const fn policy_from_byte(b: u8) -> Option<IdentityPolicy> {
    match b {
        1 => Some(IdentityPolicy::SameUid),
        2 => Some(IdentityPolicy::Designated),
        _ => None,
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
        /// The window's crash marker (appended: an older window's HELLO ends before it, and an older keeper ignores it).
        marker: Option<MarkerRef>,
    },
    Welcome {
        proto: u16,
        offers: u16,
        version: String,
        /// The identity the keeper requires of its peers; `None` when an older keeper's WELCOME ends before it or the byte is unknown.
        identity: Option<IdentityPolicy>,
        /// The keeper's capabilities ([`CAP_JOURNAL`]); `None` when an older keeper's WELCOME ends before it.
        caps: Option<u8>,
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
    Bye {
        /// The records whose shells go on past the quit for the next window to adopt (appended:
        /// a BYE that keeps nothing is the older frame, and an older keeper ignores it), at
        /// most [`MAX_KEEPS`].
        keep: Vec<u64>,
    },
    Offer {
        header: MasterHeader,
        tag: Vec<u8>,
        meta: Vec<u8>,
        /// The crash-restore journal's trailing fields, written only to a HELLO that stated [`CAP_JOURNAL`]; `None` when the body ends before them (an older keeper's OFFER, or one to a window that does not journal).
        journal: Option<JournalOffer>,
        /// Whether the record outlived its last window because that window's quit kept it (its
        /// BYE's keep set) rather than because the window crashed: a flags byte after the journal
        /// fields (written as zeroes to a window that does not journal), only to a HELLO that
        /// stated [`CAP_KEEPS`]; `None` when the body ends before it (an older keeper's OFFER, or
        /// one to a window that does not read it).
        kept: Option<bool>,
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
    /// The journal region of a master the sender claims; its read-only descriptor rides the header.
    Companion {
        rdev: u64,
        kind: u8,
        len: u64,
    },
    /// The region offered with the OFFER of `rdev` just before it; a duplicate of its descriptor rides the header.
    CompanionOffer {
        rdev: u64,
        kind: u8,
        len: u64,
    },
    /// The sender's claim on `rdev` has no journal of `kind` (any more).
    CompanionRelease {
        rdev: u64,
        kind: u8,
    },
    /// A round trip: the window's number, which the keeper writes back once it has read every frame before it.
    Sync {
        seq: u64,
    },
}

/// An OFFER's trailing crash-restore fields (the crash-restore plan §5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JournalOffer {
    /// The keeper's poison brake: how many recovering windows died, since the offered region was written, before their own COMPANION.
    pub attempts: u8,
    /// How many COMPANION_OFFERs follow this OFFER (0 or 1).
    pub companions: u8,
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
    /// A REGISTER or COMPANION arrived without its descriptor.
    NoDescriptor = 6,
    /// A COMPANION whose descriptor is not a read-only POSIX shared-memory object of the kind and length it claims, within the keeper's bounds.
    NotAJournal = 7,
    /// A COMPANION, COMPANION_RELEASE or RELEASE named a master the keeper holds no record of.
    NoSuchRecord = 8,
    /// A code this build does not know (a newer keeper).
    Other = 255,
}

impl RefuseCode {
    #[inline]
    const fn from_byte(b: u8) -> RefuseCode {
        match b {
            1 => RefuseCode::RdevMismatch,
            2 => RefuseCode::TooManyMasters,
            3 => RefuseCode::TooManyClaimants,
            4 => RefuseCode::NotClaimant,
            5 => RefuseCode::WrongClass,
            6 => RefuseCode::NoDescriptor,
            7 => RefuseCode::NotAJournal,
            8 => RefuseCode::NoSuchRecord,
            _ => RefuseCode::Other,
        }
    }
}

impl Frame {
    /// The kind this frame is sent as.
    #[inline]
    #[must_use]
    pub const fn kind(&self) -> Kind {
        match self {
            Frame::Hello { .. } => Kind::Hello,
            Frame::Welcome { .. } => Kind::Welcome,
            Frame::Register { .. } => Kind::Register,
            Frame::Meta { .. } => Kind::Meta,
            Frame::Release { .. } => Kind::Release,
            Frame::Pending { .. } => Kind::Pending,
            Frame::Bye { .. } => Kind::Bye,
            Frame::Offer { .. } => Kind::Offer,
            Frame::Orphans { .. } => Kind::Orphans,
            Frame::Status { .. } => Kind::Status,
            Frame::RestartWhenSafe => Kind::RestartWhenSafe,
            Frame::Adopt => Kind::Adopt,
            Frame::Refused { .. } => Kind::Refused,
            Frame::Companion { .. } => Kind::Companion,
            Frame::CompanionOffer { .. } => Kind::CompanionOffer,
            Frame::CompanionRelease { .. } => Kind::CompanionRelease,
            Frame::Sync { .. } => Kind::Sync,
        }
    }

    /// Encode header + body.
    /// # Errors
    /// `Oversize` for a part over its bound, `TooLong` for a body over [`MAX_BODY`].
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
                identity,
                caps,
            } => {
                body.extend_from_slice(&proto.to_be_bytes());
                body.extend_from_slice(&offers.to_be_bytes());
                put_short(&mut body, version.as_bytes(), MAX_LABEL, kind)?;
                if identity.is_some() || caps.is_some() {
                    body.push(identity.map_or(0, policy_byte));
                }
                if let Some(caps) = caps {
                    body.push(*caps);
                }
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
            Frame::Sync { seq } => body.extend_from_slice(&seq.to_be_bytes()),
            Frame::Pending { pid } => body.extend_from_slice(&pid.to_be_bytes()),
            Frame::Bye { keep } => {
                if !keep.is_empty() {
                    let n = u16::try_from(keep.len())
                        .ok()
                        .filter(|_| keep.len() <= MAX_KEEPS)
                        .ok_or(WireError::Oversize(kind))?;
                    body.extend_from_slice(&n.to_be_bytes());
                    for rdev in keep {
                        body.extend_from_slice(&rdev.to_be_bytes());
                    }
                }
            }
            Frame::RestartWhenSafe | Frame::Adopt => {}
            Frame::Offer {
                header,
                tag,
                meta,
                journal,
                kept,
            } => {
                put_header(&mut body, header);
                put_long(&mut body, tag, MAX_TAG, kind)?;
                put_long(&mut body, meta, MAX_META, kind)?;
                if let Some(j) = journal.or_else(|| kept.map(|_| JournalOffer::default())) {
                    body.push(j.attempts);
                    body.push(j.companions);
                }
                if let Some(kept) = kept {
                    body.push(if *kept { OFFER_KEPT } else { 0 });
                }
            }
            Frame::Orphans { n } => body.extend_from_slice(&n.to_be_bytes()),
            Frame::Status { text } => put_long(&mut body, text.as_bytes(), MAX_STATUS, kind)?,
            Frame::Refused { code, rdev } => {
                body.push(*code as u8);
                body.extend_from_slice(&rdev.to_be_bytes());
            }
            Frame::Companion {
                rdev,
                kind: region,
                len,
            }
            | Frame::CompanionOffer {
                rdev,
                kind: region,
                len,
            } => {
                body.extend_from_slice(&rdev.to_be_bytes());
                body.push(*region);
                body.extend_from_slice(&len.to_be_bytes());
            }
            Frame::CompanionRelease { rdev, kind: region } => {
                body.extend_from_slice(&rdev.to_be_bytes());
                body.push(*region);
            }
        }
        let mut out = encode_header(kind, body.len())?.to_vec();
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Decode the body of a frame whose header was `header`.
    /// # Errors
    /// `Short` for a truncated body, `Oversize` for a part over its bound, `UnknownClass` for an unknown HELLO class byte.
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
                let class =
                    PeerClass::from_byte(class_byte).ok_or(WireError::UnknownClass(class_byte))?;
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
                identity: if r.at < body.len() {
                    policy_from_byte(r.u8()?)
                } else {
                    None
                },
                caps: if r.at < body.len() {
                    Some(r.u8()?)
                } else {
                    None
                },
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
            Kind::Sync => Frame::Sync { seq: r.u64()? },
            Kind::Pending => Frame::Pending { pid: r.u32()? },
            Kind::Bye => Frame::Bye {
                keep: if r.at < body.len() {
                    let n = usize::from(r.u16()?);
                    if n > MAX_KEEPS {
                        return Err(WireError::Oversize(kind));
                    }
                    (0..n).map(|_| r.u64()).collect::<Result<_, _>>()?
                } else {
                    Vec::new()
                },
            },
            Kind::Offer => Frame::Offer {
                header: r.master_header()?,
                tag: r.long(MAX_TAG)?,
                meta: r.long(MAX_META)?,
                journal: if r.at < body.len() {
                    Some(JournalOffer {
                        attempts: r.u8()?,
                        companions: r.u8()?,
                    })
                } else {
                    None
                },
                // A flag bit this build does not know is a newer keeper's, and says nothing here.
                kept: if r.at < body.len() {
                    Some(r.u8()? & OFFER_KEPT != 0)
                } else {
                    None
                },
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
            Kind::Companion => Frame::Companion {
                rdev: r.u64()?,
                kind: r.u8()?,
                len: r.u64()?,
            },
            Kind::CompanionOffer => Frame::CompanionOffer {
                rdev: r.u64()?,
                kind: r.u8()?,
                len: r.u64()?,
            },
            Kind::CompanionRelease => Frame::CompanionRelease {
                rdev: r.u64()?,
                kind: r.u8()?,
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
#[path = "wire_tests.rs"]
mod tests;
