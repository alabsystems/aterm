// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The seamless handoff's out-of-band transport: a single-use `AF_UNIX` rendezvous the outgoing
//! process binds before it launches its successor, and that the successor dials back on.
//!
//! On macOS the overlap successor must be a launchd application job of its own, or the process
//! that applied an update can never apply the next (`tests/handoff_launchd_job.rs`). A
//! LaunchServices launch inherits no descriptors, so what the fork lane hands over by inheritance
//! travels here: the PTY masters, the readiness pipe's write end and the Commit pipe's read end.
//! The control socket cannot carry them, since every name in its address space is a server
//! already bound and here the parent must move first. So the parent binds, the launch
//! environment carries the path ([`ENV_RENDEZVOUS`]) and a fresh claim secret ([`ENV_CLAIM`]),
//! and the successor dials back, which also gives this process a kernel-attested pid
//! (`LOCAL_PEERPID`) for a process it did not fork, as `signal_handoff_candidate` requires.
//!
//! `<dir>/seamless-<pid>-<16 hex of the attempt nonce>.sock`. Darwin's `sun_path` holds 103
//! bytes, which under the control directory (`$HOME/Library/Application Support/aterm`) leaves
//! too little room for many a `$HOME`, so the socket lives in [`rendezvous_dir`]: `$TMPDIR/aterm`,
//! where `$TMPDIR` is launchd's per-user `/var/folders/…/T/`. The base is admitted only if `lstat`
//! says it is a real directory owned by this uid, not group/other-writable and not `/tmp`-rooted
//! (in sticky `/tmp` another uid could pre-plant the directory as a symlink); the control
//! directory is the fallback. After the bind the node is re-proved to be our socket in our
//! directory, and the dialer checks that the listener is this uid's and the attested parent's
//! before it writes the secret ([`listener_may_receive_claim`]). The composed path is checked
//! with `control_auth::sun_path_ok` before the bind ([`RendezvousError::PathTooLong`] sends the
//! attempt to the fork lane), and [`rendezvous_path_fits`] asks the same before any nonce exists.
//! [`Rendezvous`] unlinks its node on drop, and each bind sweeps nodes whose owner pid is dead.
//!
//! # Single use
//!
//! One connection is served, one grant carrying every descriptor is sent (one message, or the
//! chunked grant's few), and the socket is gone: no retry and no verb, unlike the long-lived
//! control socket, where a handoff verb would outlive the attempt. The successor dials as soon as
//! it has booted and waits, under its own grant budget, until the parent parks and sends the
//! grant or closes the connection (the attempt stood down). A connection that presents the wrong
//! secret is refused but the wait goes on until the deadline, so one `connect(2)` cannot deny an
//! update ([`Rendezvous::accept_claim_observed`] has the bounds).
//!
//! # The three gates on the peer
//!
//! * **The claim secret** — 32 CSPRNG bytes, published only in the successor's launch
//!   environment, compared in constant time. It binds the connection to this attempt, not to the
//!   successor: a process's initial environment stays readable to every same-uid process for its
//!   whole life (`KERN_PROCARGS2`), and a successor that boot-applies and re-execs carries the
//!   claim into its new image too.
//! * **The kernel-attested peer pid** — `LOCAL_PEERPID`, compared with the pid LaunchServices
//!   reports for the instance it started. Because a successor dials before its `NSApplication`
//!   checks in, on the launched lane the pid corroborates after the dial, and a mismatch or an
//!   exited launched process refuses before Commit.
//! * **The dialer's code** ([`DialerCode`]) — the kernel's audit token (`LOCAL_PEERTOKEN`) names
//!   the dialing process, and Security.framework checks its code against the successor's
//!   requirement before any descriptor leaves, keeping out a same-uid process that copied the
//!   secret. A check `trustd` does not answer holds the connection until `trustd` answers or the
//!   dial deadline ([`Rendezvous::accept_claim_observed`]), and a refusal reaches the caller typed
//!   ([`CodeRefusal`]).
//!
//! On Darwin the audit token and `LOCAL_PEERPID` name whichever process last used the connection
//! and change at each use (`aterm_uds::peer`'s `the_peer_is_the_connections_last_user_read_live`),
//! and an `exec` at the pid moves the token's pid version. So the gate reads the token once, right
//! after the claim, requires it to name the attested pid, and judges that one token at every
//! check; just before the `sendmsg` carrying the first descriptor it reads it again and sends
//! nothing unless the kernel answers the same token ([`ClaimedPeer::transfer`]). The pid the
//! parent registers, watches and kills as its candidate is the one that token names.
//!
//! What the code gate leaves open:
//!
//! - A same-uid process running genuine aterm code with the copied secret passes, and then holds
//!   the sessions inside an aterm, which the PTY keeper's floor (its design §5.7) accepts: same-uid
//!   code can already drive every session through the control socket. The pid corroboration still
//!   refuses it before Commit when LaunchServices answers in time.
//! - The token says who used the connection last, not who will read the grant. A same-uid process
//!   can claim, keep a copy of the connection in a second process that never uses it, and exec a
//!   genuine aterm (held still with `SIGSTOP` if need be) before the gate reads the token; the
//!   copy then reads every descriptor. This is derived from the measured semantics, not run end to
//!   end. The pid corroboration refuses such a dialer before Commit, but after the descriptors
//!   left ([`RendezvousError::TransferPartial`]). Closing it needs a binding to the process that
//!   presented the claim that later use cannot move (a Mach audit trailer, or a per-attempt secret
//!   only signed aterm code can read); that design decision is open, and `aterm_keeper::identity`
//!   has the same gap.
//!
//! # The adoption proof's term
//!
//! `SCM_RIGHTS` hands the receiver the same open file descriptions at whatever descriptor
//! numbers it has free, so the fd-number term in `seamless::adoption_proof` would hash different
//! values on the two sides. The replacement is [`pty_device_term`], the PTY's device number,
//! which `dup`/`SCM_RIGHTS` preserve and a same-uid process cannot forge (`mknod(2)` needs root).
//! The transport selects the term with no negotiation: presence of the transport is the version,
//! and `app_update_handoff` refuses this lane unless the authorized `target_build` is at least
//! this build.
//!
//! # The wire
//!
//! Fixed-magic frames whose length is known before any variable byte is read:
//!
//! ```text
//! successor -> parent   "ATRZ1C" + 64 hex claim chars           (70 bytes, fixed)
//! parent -> successor   "ATRZ1G" + u16be body length            (8 bytes, fixed)
//!                       + body: "<nonce>\n<lid>:<pid>,<lid>:<pid>\n[park_us=<µs>\n]"
//! ```
//!
//! The body's optional last line is the park stamp: elapsed time from the parent's park to its
//! sample before encoding and sending the grant, on the parent's clock. The successor subtracts
//! it from its recorded claim time to project a `park` point, so `proof - park` sums parent
//! park→sample and successor claim→proof (an observation, not clock alignment). A grant without
//! it reads as no stamp.
//!
//! The descriptors ride the 8-byte header message in one `sendmsg`, so they are all present the
//! instant the receiver dequeues its first byte, in this order: every PTY master in the body's
//! order, then the readiness pipe's write end, then the Commit pipe's read end.
//!
//! One message carries at most [`fdpass::MAX_FDS`] descriptors, two of them the pipes, so that
//! grant tops out at 62 sessions. A successor that can take more says so in its claim, and only
//! when the parent offered it the capability:
//!
//! ```text
//! parent env            ATERM_HANDOFF_GRANT_CAPS=chunks1   (only to a candidate whose
//!                                                           verified Info.plist declares it)
//! successor -> parent   "ATRZ2C" + the same 64 hex claim chars    (70 bytes, fixed)
//! parent -> successor   "ATRZ2G" + u16be body length + u8 chunks  (9 bytes, fixed)
//!                       + the same body
//!                       + chunks-1 × "ATRZ2K" + u8 chunk index    (7 bytes each)
//! ```
//!
//! Each message carries at most [`fdpass::MAX_FDS`] descriptors in the one-message order, the pipe
//! ends always in the last ([`grant_chunk_sizes`] is the layout both sides compute). Up to 62
//! sessions the grant is `ATRZ1G` whatever the claim said, and a pool an `ATRZ1C` claimant cannot
//! take is refused before any descriptor leaves ([`grant_shape`]). The receiver takes every message
//! under one deadline, requires exactly the layout for the body's session count, and on any
//! refusal closes every descriptor it received. Position is addressing only (which descriptor is
//! which `local_id`) and proves nothing, which is why the proof term is the device number. The
//! body carries no descriptor numbers: a launched process inherits none.
//!
//! # Failures
//!
//! Nothing here half-transfers a session. Before the descriptor-carrying `sendmsg` nothing of ours
//! has left and the parent may roll back; after it the successor holds copies and the ordinary
//! reject/reap path owns the outcome. The chunked grant draws that line at its first `sendmsg`:
//! every later failure is a partial transfer, and a successor holding only some chunks refuses
//! the grant and closes them all.
//!
//! macOS only, gated at the `mod` declaration: the lane exists to mint a launchd application job,
//! and [`pty_device_term`] reads a Darwin fact.

use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, BorrowedFd, IntoRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::{CtlListener, CtlStream, fdpass};

use crate::handoff_env::HandoffName;

/// The rendezvous socket path published to the successor. Its absence selects the fork lane;
/// there is no version byte.
pub(crate) const ENV_RENDEZVOUS: HandoffName = crate::seamless::ENV_RENDEZVOUS;
/// The claim secret published to the successor.
pub(crate) const ENV_CLAIM: HandoffName = crate::seamless::ENV_CLAIM;

/// Random bytes in the claim secret (half its hex length): the width `control_auth` mints
/// capability tokens at.
const CLAIM_SECRET_BYTES: usize = 32;
const CLAIM_HEX_LEN: usize = CLAIM_SECRET_BYTES * 2;
const CLAIM_MAGIC: &[u8; 6] = b"ATRZ1C";
const CLAIM_FRAME_LEN: usize = CLAIM_MAGIC.len() + CLAIM_HEX_LEN;
const GRANT_MAGIC: &[u8; 6] = b"ATRZ1G";
/// The claim of a successor that reads the chunked grant: the same secret under its own magic,
/// presented only when the parent published [`GRANT_CAPS_CHUNKS`] in [`ENV_GRANT_CAPS`].
const CLAIM_CHUNKS_MAGIC: &[u8; 6] = b"ATRZ2C";
/// The chunked grant's header: magic, `u16` big-endian body length, `u8` count
/// of descriptor-bearing messages (this one included).
const GRANT_CHUNKS_MAGIC: &[u8; 6] = b"ATRZ2G";
const GRANT_CHUNKS_HEADER_LEN: usize = GRANT_CHUNKS_MAGIC.len() + 2 + 1;
/// A continuation message of the chunked grant: magic plus its `u8` index (the
/// header message is index 0). Fixed, so it is read to its end and never past it.
const GRANT_CONTINUATION_MAGIC: &[u8; 6] = b"ATRZ2K";
const GRANT_CONTINUATION_LEN: usize = GRANT_CONTINUATION_MAGIC.len() + 1;

/// The launch variable in which a parent offers its successor grant
/// capabilities, a comma-separated list. Published only to a candidate whose
/// verified bundle declares them (`ATermHandoffGrantChunks`); consumed by
/// [`claim_incoming`].
pub(crate) const ENV_GRANT_CAPS: HandoffName = HandoffName::new("ATERM_HANDOFF_GRANT_CAPS");
/// The capability token for the chunked grant, also the value of the bundle's
/// `ATermHandoffGrantChunks` key (`aterm_update::HANDOFF_GRANT_CHUNKS_TOKEN`).
pub(crate) const GRANT_CAPS_CHUNKS: &str = "chunks1";

/// The most sessions a chunked grant carries: the protocol's own ceiling.
pub(crate) const MAX_CHUNKED_SESSIONS: usize = crate::seamless::MAX_HANDOFF_SESSIONS;
/// The most descriptors one grant carries: every session plus the two pipes.
const MAX_GRANT_FDS: usize = MAX_CHUNKED_SESSIONS + RENDEZVOUS_CHANNEL_FDS;
/// The most messages one chunked grant spans ([`grant_chunk_sizes`] of
/// [`MAX_CHUNKED_SESSIONS`]); a header naming more is refused before its body.
const MAX_GRANT_CHUNKS: usize = MAX_GRANT_FDS.div_ceil(fdpass::MAX_FDS);
/// Magic plus a `u16` big-endian body length, fixed and read before the body, so the receiver
/// never sizes a buffer from a number it has not framed.
const GRANT_HEADER_LEN: usize = GRANT_MAGIC.len() + 2;
/// Ceiling on the grant body, far above what 256 sessions need: a malformed length is refused
/// before an allocation.
const MAX_GRANT_BODY_BYTES: usize = 32 * 1024;

/// Descriptors this transport carries beyond the PTY masters: the readiness
/// pipe's write end and the Commit pipe's read end.
pub(crate) const RENDEZVOUS_CHANNEL_FDS: usize = 2;

/// The most sessions the one-message grant (`ATRZ1G`) carries: [`fdpass::MAX_FDS`] less the
/// two pipes. For a successor without the chunked grant this, not the protocol's 256, is the
/// bound the lane check tests ([`grant_session_limit`]), so a parent with more panes takes the
/// fork lane instead of failing at `sendmsg` with the terminal parked.
pub(crate) const MAX_RENDEZVOUS_SESSIONS: usize = fdpass::MAX_FDS - RENDEZVOUS_CHANNEL_FDS;

/// The transport, not the protocol, bounds the one-message grant (256 is
/// `seamless::MAX_HANDOFF_SESSIONS`). Checked at compile time because a flip would surface at
/// `sendmsg` with the terminal parked instead of as a fallback.
const _: () = assert!(
    MAX_RENDEZVOUS_SESSIONS < 256,
    "the transport, not the protocol, is what bounds this lane"
);

/// How many sessions one grant may carry: [`MAX_CHUNKED_SESSIONS`] when the successor declared
/// the chunked grant, else [`MAX_RENDEZVOUS_SESSIONS`]. The lane choice, the park and
/// [`ClaimedPeer::transfer`] all bound the pool by it.
#[must_use]
pub(crate) const fn grant_session_limit(chunks: bool) -> usize {
    if chunks {
        MAX_CHUNKED_SESSIONS
    } else {
        MAX_RENDEZVOUS_SESSIONS
    }
}

/// Which grant a parent sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantShape {
    /// `ATRZ1G`: every descriptor in one message, as every build has sent it.
    OneMessage,
    /// `ATRZ2G`: the header message and its continuations.
    Chunked,
}

/// The parent's grant for a dialer that claimed the chunked grant (`chunks`, an `ATRZ2C` claim)
/// or not, with `sessions` to hand over: the one-message grant whenever one message carries
/// them, whatever the claim; past that the chunked grant, only to an `ATRZ2C` claim; otherwise a
/// refusal before any descriptor leaves. The `NativeUpdateRendezvousGrant` model's `GrantOne`,
/// `GrantChunks` and `RefuseTooMany`.
fn grant_shape(chunks: bool, sessions: usize) -> Result<GrantShape, RendezvousError> {
    let limit = grant_session_limit(chunks);
    if sessions == 0 || sessions > limit {
        return Err(RendezvousError::TooManySessions { sessions, limit });
    }
    Ok(if sessions > MAX_RENDEZVOUS_SESSIONS {
        GrantShape::Chunked
    } else {
        GrantShape::OneMessage
    })
}

/// The chunked grant's layout for `sessions` sessions: how many descriptors each message
/// carries, in order. Masters fill messages of [`fdpass::MAX_FDS`]; the readiness and Commit ends
/// close the last, in a message of their own when the masters leave no room for both. Both
/// sides compute it, so there is no count on the wire to believe.
#[must_use]
fn grant_chunk_sizes(sessions: usize) -> Vec<usize> {
    let mut sizes = Vec::new();
    let mut left = sessions;
    while left > fdpass::MAX_FDS {
        sizes.push(fdpass::MAX_FDS);
        left -= fdpass::MAX_FDS;
    }
    if left + RENDEZVOUS_CHANNEL_FDS <= fdpass::MAX_FDS {
        sizes.push(left + RENDEZVOUS_CHANNEL_FDS);
    } else {
        sizes.push(left);
        sizes.push(RENDEZVOUS_CHANNEL_FDS);
    }
    sizes
}

/// How often a held connection whose code check the process's `trustd` gate
/// refused at once is checked again ([`Rendezvous::accept_claim_observed`]).
/// While an earlier check is overdue the gate refuses a new one at once, so a
/// check costs nothing until `trustd` answers again; this only bounds how late
/// the hold notices that it has. A check the gate let through is waited for
/// instead, never asked again ([`admit_code`]).
const CODE_RECHECK_INTERVAL: Duration = Duration::from_millis(250);

/// How long one `poll` slice waits before the accept loop re-checks the abort predicate and the
/// deadline: a cancel is noticed promptly without spinning.
const ACCEPT_POLL_SLICE: Duration = Duration::from_millis(10);

/// The most of the attempt's budget one connection may spend presenting its claim frame. Until
/// the frame is checked the peer is an unknown same-uid process, which could otherwise hold the
/// parked terminal for the whole deadline by saying nothing; a real successor writes all 70
/// bytes in one call.
const CLAIM_FRAME_BUDGET: Duration = Duration::from_secs(2);

/// How many connections may be refused before the attempt gives up, so a peer that connects and
/// resets in a loop cannot spin this thread for the rest of the deadline. The path is
/// unguessable, so a real machine sees no wrong dialers at all.
const MAX_REFUSED_DIALS: usize = 32;

/// The floor on a socket read/write timeout: std refuses a zero timeout, which a deadline that
/// has just expired computes, so every remaining-time computation is clamped here and expiry is
/// reported by the deadline check instead of an `InvalidInput` from a socket option.
const MIN_IO_TIMEOUT: Duration = Duration::from_millis(1);

/// Why a rendezvous did not produce a transferred handoff. Every variant means keep the user's
/// windows: the parent maps each onto a fork-lane fallback or a rollback, and the successor exits
/// before starting a window, since the parent still owns every session it was to adopt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RendezvousError {
    /// No per-user control directory: there is nowhere private to bind.
    NoControlDir,
    /// The composed path does not fit `sockaddr_un` on this platform. `bind` and `connect` would
    /// fail `EINVAL`, which a fail-safe liveness probe would misread as "maybe live".
    PathTooLong { path: String, limit: usize },
    /// More sessions than the successor's claim lets one grant carry: one
    /// `SCM_RIGHTS` message ([`MAX_RENDEZVOUS_SESSIONS`]) for an `ATRZ1C` claim,
    /// [`MAX_CHUNKED_SESSIONS`] for an `ATRZ2C` one. Refused before any
    /// descriptor leaves.
    TooManySessions { sessions: usize, limit: usize },
    /// The CSPRNG would not produce a claim secret.
    Secret(String),
    /// `bind`, the permission tightening, or the non-blocking switch failed.
    Bind(String),
    /// The caller's abort predicate fired (for the handoff worker, a cancel poke).
    Cancelled,
    /// The deadline expired with no usable dial: not proof that no successor exists, but proof that
    /// no descriptor of ours left this process.
    Deadline,
    /// `accept`, or reading the claim frame off the accepted connection, failed.
    Accept(String),
    /// A connection did not present this attempt's secret.
    Claim,
    /// The kernel says the dialer is not the process LaunchServices started.
    PeerPid { expected: i32, actual: i32 },
    /// The kernel would not attest the peer at all.
    PeerUnattested(String),
    /// The dialer (the kernel-attested `pid`) presented the secret and its code was not admitted
    /// ([`DialerCode`]); [`CodeRefusal`] says whether it was refused or not checked. No descriptor
    /// goes to it.
    PeerCode { pid: i32, refusal: CodeRefusal },
    /// The transfer failed before the descriptor-carrying `sendmsg` succeeded (an unencodable
    /// grant, a socket the write timeout would not take, or the `sendmsg` itself). No descriptor
    /// of ours left this process, which is all `NeverTransferred` rests on.
    TransferNotSent(String),
    /// The grant's own deadline (the parent's proof deadline) passed before the grant was sent: a
    /// successor dialed and was held, and the preparation after the park outran the budget. As
    /// with [`Self::TransferNotSent`] no descriptor left, but it is not [`Self::Deadline`]'s "no
    /// successor dialed".
    GrantDeadline,
    /// The descriptors left (the `sendmsg` succeeded) and a write after it (the tail of a short
    /// header send, the body, the flush) failed. The peer holds duplicates of every master and
    /// both pipes; a successor that receives descriptors and no body refuses the grant and exits,
    /// but the parent may not assume so: this is the ordinary kill-and-prove disposition, never
    /// `NeverTransferred`.
    TransferPartial(String),

    /// Successor side: this successor's own dial or grant budget ([`ClaimDeadlines`]) ran out
    /// before the grant arrived, which is [`Self::Deadline`] seen from this side.
    ClaimDeadline,
    /// Successor side: the launch environment carries no rendezvous.
    NoRendezvous,
    /// Successor side: exactly one of the two rendezvous variables is set. Half a rendezvous is
    /// corruption, not absence ([`rendezvous_present`] answers yes to it), and naming which half
    /// arrived keeps the caller's "carries a rendezvous but could not claim it" line from
    /// contradicting itself.
    HalfRendezvous {
        present: HandoffName,
        missing: HandoffName,
    },
    /// Successor side: a rendezvous variable is present but malformed: corruption, not absence,
    /// so it must not degrade into "no handoff" and start a fresh window over a parent's live
    /// sessions.
    Env(HandoffName),
    /// Successor side: `connect` failed, most often because the parent already gave up and unlinked
    /// the socket, which is how a late dial is meant to fail.
    Dial(String),
    /// Successor side: the grant frame was absent, short, mis-magicked, or its body did not parse.
    Grant(String),
    /// Successor side: the grant is for a different attempt than the manifest environment names.
    NonceMismatch,
    /// Successor side: the descriptor count does not match the body. Fatal rather than truncating:
    /// adopting a subset would hand the parent a proof over a session set it never authorized.
    DescriptorCount { expected: usize, received: usize },
}

impl std::fmt::Display for RendezvousError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoControlDir => f.write_str("there is no private control directory to bind in"),
            Self::PathTooLong { path, limit } => write!(
                f,
                "the rendezvous path {path} is longer than this platform's {limit}-byte sun_path"
            ),
            Self::TooManySessions { sessions, limit } => write!(
                f,
                "{sessions} sessions exceed the {limit} this successor's descriptor grant can carry"
            ),
            Self::Secret(error) => write!(f, "no claim secret could be minted: {error}"),
            Self::Bind(error) => write!(f, "the rendezvous could not be bound: {error}"),
            Self::Cancelled => f.write_str("the rendezvous wait was cancelled"),
            Self::Deadline => {
                f.write_str("no successor dialed the rendezvous before the handoff deadline")
            }
            Self::Accept(error) => write!(f, "the rendezvous connection failed: {error}"),
            Self::Claim => f.write_str("the dialer did not present this attempt's claim secret"),
            Self::PeerPid { expected, actual } => write!(
                f,
                "the dialer is pid {actual}, not the launched successor {expected}"
            ),
            Self::PeerUnattested(error) => {
                write!(f, "the kernel would not attest the dialer: {error}")
            }
            Self::PeerCode { pid, refusal } => {
                write!(
                    f,
                    "the dialer (pid {pid}) presented the claim, and {refusal}"
                )
            }
            Self::GrantDeadline => f.write_str(
                "the handoff's proof deadline passed before the grant was sent (the successor \
                 had dialed; this process's preparation after the park outran it), so no \
                 descriptor left",
            ),
            Self::TransferNotSent(error) => {
                write!(
                    f,
                    "the descriptor transfer failed before any descriptor left: {error}"
                )
            }
            Self::TransferPartial(error) => write!(
                f,
                "the descriptors were delivered but the grant body was not: {error}"
            ),
            Self::ClaimDeadline => f.write_str(
                "this successor dialed, but its handoff deadline passed before the parent's \
                 grant arrived",
            ),
            Self::NoRendezvous => f.write_str("this launch carries no rendezvous"),
            Self::HalfRendezvous { present, missing } => write!(
                f,
                "this launch carries only half a rendezvous: {present} is set and {missing} is \
                 not, so there is nothing here that could be claimed"
            ),
            Self::Env(key) => write!(f, "the rendezvous variable {key} is malformed"),
            Self::Dial(error) => write!(f, "the rendezvous could not be dialed: {error}"),
            Self::Grant(detail) => write!(f, "the rendezvous grant is malformed: {detail}"),
            Self::NonceMismatch => f.write_str("the rendezvous grant names a different attempt"),
            Self::DescriptorCount { expected, received } => write!(
                f,
                "the grant describes {expected} descriptors and {received} arrived"
            ),
        }
    }
}

/// The PTY's own identity, the term the adoption proof hashes when the masters travel out of
/// band: `fstat(2)`'s `st_rdev` on a PTY master. `/dev/ptmx` is a cloning device, so each open
/// takes its own minor (its slave's `/dev/ttysNNN`), and `dup`/`SCM_RIGHTS` keep the open file
/// description and so the value. `st_ino`/`st_dev` would not do: every master shares the one
/// `/dev/ptmx` node. A same-uid process cannot make `fstat` lie, and to answer a given device
/// number it must hold that very PTY, where an fd number is whatever `dup2` makes it.
///
/// A Darwin fact, and why this module is macOS only: on Linux every master's `st_rdev` is the
/// one `/dev/ptmx` node (the minor is `ioctl(TIOCGPTN)`), so this must change before the lane
/// runs on another Unix. `dev_t` is `i32` on Darwin and `u64` on Linux; widening through `i128`
/// keeps either from wrapping into a plausible wrong value.
#[must_use]
pub(crate) fn pty_device_term(master: i32) -> Option<i32> {
    if master < 0 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fstat` writes exactly one `struct stat` through the pointer and
    // reads nothing else; the descriptor is inspected, never consumed.
    let queried = unsafe { libc::fstat(master, info.as_mut_ptr()) };
    if queried != 0 {
        return None;
    }
    // SAFETY: `fstat` returned 0, so it initialized the structure.
    let info = unsafe { info.assume_init() };
    i32::try_from(i128::from(info.st_rdev)).ok()
}

/// Re-express an attempt's adoption identities in this transport's term. The middle field of a
/// `SessionIdentity` is a transport coordinate: on the fork lane the descriptor number `execve`
/// preserves, here [`pty_device_term`] of that descriptor; `local_id` and `pid` pass through.
/// `None` when any master will not answer `fstat`: a proof missing a term would cover a
/// different session set.
#[must_use]
pub(crate) fn proof_identities_in_device_terms(
    identities: &[crate::seamless::SessionIdentity],
) -> Option<Vec<crate::seamless::SessionIdentity>> {
    identities
        .iter()
        .map(|(local_id, master, pid)| Some((*local_id, pty_device_term(*master)?, *pid)))
        .collect()
}

/// The rendezvous path one attempt binds: this process's pid and the first
/// [`RENDEZVOUS_NONCE_HEX`] hex of the attempt nonce, the pieces `seamless::write_outgoing` names
/// its manifest with, so the name is unique per attempt and names its owner. The claim secret,
/// never on disk, is the authority.
fn rendezvous_path(dir: &Path, nonce: &str) -> PathBuf {
    let short = &nonce[..nonce.len().min(RENDEZVOUS_NONCE_HEX)];
    dir.join(format!("seamless-{}-{short}.sock", std::process::id()))
}

/// How much of the attempt nonce the socket name carries.
const RENDEZVOUS_NONCE_HEX: usize = 16;

/// The per-user temporary base the rendezvous directory hangs off, admitted only where no other
/// uid can plant an entry: `lstat` says a real directory owned by this uid, not
/// group/other-writable, and not `/tmp`-rooted ([`admits_temp_base`]). `std::env::temp_dir`
/// reads `$TMPDIR`, which launchd sets to the per-user `/var/folders/…/T/` for everything it
/// starts, and whoever sets this process's environment is already inside its trust boundary.
/// With `$TMPDIR` unset, std falls back to `confstr(_CS_DARWIN_USER_TEMP_DIR)`, whose fork
/// hazard `aterm_pty::unix` describes.
#[cfg(all(unix, not(test)))]
fn admitted_temp_base() -> Option<PathBuf> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let base = std::env::temp_dir();
    let lstat = std::fs::symlink_metadata(&base).ok().map(|meta| BaseFacts {
        is_dir: meta.file_type().is_dir(),
        uid: meta.uid(),
        mode: meta.permissions().mode(),
    });
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    admits_temp_base(&base, lstat, uid).then_some(base)
}

/// What `lstat` says of a temporary base ([`admits_temp_base`]).
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
struct BaseFacts {
    /// A real directory (`lstat`: a symlink is not one).
    is_dir: bool,
    /// Its owner.
    uid: u32,
    /// Its mode bits.
    mode: u32,
}

/// [`admitted_temp_base`]'s verdict on `base`, `lstat` the facts of it (`None`:
/// none could be read) and `uid` this process's.
#[cfg(unix)]
fn admits_temp_base(base: &Path, lstat: Option<BaseFacts>, uid: u32) -> bool {
    // Absolute only: a relative `$TMPDIR` binds against this process's cwd, which a LaunchServices
    // successor (cwd `/`) cannot dial, so every attempt would park for the whole readiness deadline
    // and never fall back to the fork lane.
    if !base.is_absolute() {
        return false;
    }
    if base.starts_with("/tmp") || base.starts_with("/private/tmp") {
        return false;
    }
    lstat.is_some_and(|facts| facts.is_dir && facts.uid == uid && facts.mode & 0o022 == 0)
}

/// The directory the rendezvous socket lives in, without creating anything: the lane-choice
/// probe asks on every check and must not touch the filesystem. `None` means the control
/// directory (the fallback). In this crate's test binary, a process root under `/tmp`
/// instead of the person's `$TMPDIR`.
#[must_use]
fn rendezvous_dir_candidate() -> Option<PathBuf> {
    #[cfg(all(unix, test))]
    {
        Some(crate::test_scratch::short_process_root("aterm-rz-"))
    }
    #[cfg(all(unix, not(test)))]
    {
        admitted_temp_base().map(|base| base.join("aterm"))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The short per-uid private directory the rendezvous socket lives in, established with
/// `aterm_update_core::ensure_private_dir` (a symlink at the target is refused). Falls back to
/// the control directory when `$TMPDIR` cannot be admitted or the child cannot be established.
#[must_use]
pub(crate) fn rendezvous_dir() -> Option<PathBuf> {
    if let Some(dir) = rendezvous_dir_candidate()
        && aterm_update_core::ensure_private_dir(&dir).is_ok()
    {
        return Some(dir);
    }
    // Absolute for the same reason as `admitted_temp_base`'s base.
    crate::control_auth::socket_dir().filter(|dir| dir.is_absolute())
}

/// After the bind, prove the node is our socket inside our real directory: the directory opened
/// `O_NOFOLLOW` (a symlink swapped in after the check fails here), owned by this uid and not
/// group/other-writable, and the entry a socket this uid owns. This turns the pre-bind check
/// from a race into a proof.
#[cfg(unix)]
fn prove_bound_socket_is_ours(dir: &Path, path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{
        FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
    };
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let dir_handle = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(dir)
        .map_err(|e| format!("rendezvous directory is not a real directory of ours: {e}"))?;
    let dmeta = dir_handle
        .metadata()
        .map_err(|e| format!("rendezvous directory fstat: {e}"))?;
    if !dmeta.file_type().is_dir() || dmeta.uid() != uid || dmeta.permissions().mode() & 0o022 != 0
    {
        return Err("rendezvous directory is not owned by this uid or is shared".to_string());
    }
    let smeta =
        std::fs::symlink_metadata(path).map_err(|e| format!("rendezvous socket lstat: {e}"))?;
    if !smeta.file_type().is_socket() || smeta.uid() != uid {
        return Err("rendezvous node is not a socket owned by this uid".to_string());
    }
    Ok(())
}

/// Unlink leftover rendezvous sockets in `dir` whose embedded owner pid is no
/// longer alive: an attempt that ended without its destructor (the success path
/// `_exit`s; a crashed successor never unlinked). Best-effort, bounded to names
/// of exactly this shape, and never touches an entry whose owner still runs.
#[cfg(unix)]
fn sweep_dead_rendezvous(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(rest) = name.strip_prefix("seamless-") else {
            continue;
        };
        // A dead attempt's launch stamp goes with its node.
        let node = name.strip_suffix(LAUNCH_STAMP_SUFFIX).unwrap_or(&name);
        if !node.ends_with(".sock") {
            continue;
        }
        let Some((pid, _)) = rest.split_once('-') else {
            continue;
        };
        let Ok(pid) = pid.parse::<i32>() else {
            continue;
        };
        if pid == std::process::id() as i32 {
            continue;
        }
        // SAFETY: kill(pid, 0) probes existence only; ESRCH means gone.
        let alive = unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !alive {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Whether a rendezvous path composed for this process fits `sun_path`, without binding
/// anything. The lane choice asks before the worker mints its nonce, so this composes a
/// placeholder; the answer is exact, since every nonce `seamless` mints is 32 hex characters.
#[must_use]
pub(crate) fn rendezvous_path_fits() -> bool {
    let Some(dir) = rendezvous_dir_candidate()
        .or_else(crate::control_auth::socket_dir)
        .filter(|dir| dir.is_absolute())
    else {
        return false;
    };
    rendezvous_path(&dir, &"0".repeat(32))
        .to_str()
        .is_some_and(crate::control_auth::sun_path_ok)
}

/// The signing identifier every aterm release is cut with: the bundle's `CFBundleIdentifier`
/// (`apps/aterm-mac/Info.plist`), which `codesign` gives the main executable and which the
/// installed app's designated requirement names (the PTY keeper checks its peers against that
/// requirement, `DESIGN-pty-keeper` §11.1).
pub(crate) const RELEASE_SIGNING_IDENTIFIER: &str = "com.aterm.aterm";

/// The code a dialer must run to take the grant. [`Rendezvous::accept_claim_observed`] checks it
/// on every connection that presented the secret, through `aterm_keeper::identity` (the check
/// the PTY keeper makes of its peers), against the audit token read once at the claim and
/// required to name the attested pid. A refused connection is closed and the wait goes on; one
/// whose check `trustd` did not answer is held and checked again ([`CodeRefusal::TrustdSilent`]),
/// never passed. The launch says which code ([`Self::release`], [`Self::code_at`]); this type
/// only checks it.
pub(crate) struct DialerCode {
    rule: DialerCodeRule,
}

enum DialerCodeRule {
    /// A requirement fixed before the launch.
    Stated(aterm_keeper::identity::Identity),
    /// The designated requirement of the code at this path, read when a dialer
    /// has presented the secret: by then the successor's boot swap has put its
    /// own build there, so an ad-hoc build's cdhash requirement names the code
    /// the successor runs, not the code this process was started from.
    CodeAt(PathBuf),
    /// Test-only: the check `start` makes, waited for in line `in_line` before
    /// the dialer is held ([`DialerCode::scripted`]).
    #[cfg(all(test, target_vendor = "apple"))]
    Scripted {
        start: Box<dyn Fn() -> aterm_keeper::identity::PeerCheck + Send + Sync>,
        in_line: Duration,
    },
}

/// Why [`DialerCode`] did not admit a dialer that presented this attempt's secret, typed because
/// each is filed differently: only [`Self::Refused`] is a verdict on the code that dialled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodeRefusal {
    /// Checked and refused: the code the dialer runs does not meet the rule.
    Refused(String),
    /// Not checked: macOS's code-signing service (`trustd`) has not answered
    /// the check, within the keeper's peer-check hang detector or at once
    /// because an earlier check in this process is overdue
    /// (`aterm_uds::deadline::TRUST`). The machine's moment: the same dialer
    /// may pass once `trustd` answers, so the gate holds it, waiting for that
    /// answer, rather than closing it.
    TrustdSilent(String),
    /// Not checked for another reason: the kernel would not name the dialer, its code could not be
    /// looked up (it exited, or the file it was started from was replaced under it, as by a sibling
    /// launch's swap of the same build), or the code at the launched bundle had no requirement.
    Unchecked(String),
}

impl CodeRefusal {
    /// Whether this is a verdict on the code that dialled
    /// ([`Self::Refused`]), rather than a check that did not happen.
    #[must_use]
    pub(crate) fn is_verdict(&self) -> bool {
        matches!(self, Self::Refused(_))
    }
}

impl std::fmt::Display for CodeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(why) => {
                write!(
                    f,
                    "its code does not meet the successor's requirement ({why})"
                )
            }
            Self::TrustdSilent(why) => write!(f, "its code could not be checked: {why}"),
            Self::Unchecked(why) => write!(f, "its code could not be checked ({why})"),
        }
    }
}

impl DialerCode {
    /// Code signed as an aterm release: identifier [`RELEASE_SIGNING_IDENTIFIER`] and a Developer
    /// ID of `team`, the installed app's own designated requirement. Every update candidate passed
    /// the Developer ID half before this lane was chosen for it (`aterm_update_core::codesign`),
    /// and a candidate signed with another identifier takes the fork lane, whose gate does not ask
    /// (`app_update_handoff::launched_dialer_code`). So every successor this rule gates meets it,
    /// including one whose deferred boot swap still runs the outgoing release image.
    ///
    /// # Errors
    /// A team that is no team, or a requirement Security.framework refuses.
    pub(crate) fn release(team: &str) -> Result<Self, String> {
        let source = aterm_update_core::codesign::developer_id_requirement(team)
            .map_err(|error| error.to_string())?;
        // `codesign -R` reads requirement text after an `=`; Security.framework
        // reads the text itself.
        let developer_id = source.strip_prefix('=').unwrap_or(&source);
        let text = format!("identifier \"{RELEASE_SIGNING_IDENTIFIER}\" and {developer_id}");
        Ok(Self {
            rule: DialerCodeRule::Stated(aterm_keeper::identity::Identity::for_requirement(&text)?),
        })
    }

    /// The code at `path` (the bundle LaunchServices launches), by its own
    /// designated requirement, read at each check. For a successor no team pins
    /// and for a same-image handoff (the QA seam, a launch-record renewal), whose
    /// successor re-runs this image.
    pub(crate) fn code_at(path: PathBuf) -> Self {
        Self {
            rule: DialerCodeRule::CodeAt(path),
        }
    }

    /// This process's own code: the dialer of a test that dials from inside
    /// the test binary.
    #[cfg(test)]
    pub(crate) fn this_process() -> Self {
        Self {
            rule: DialerCodeRule::Stated(
                aterm_keeper::identity::Identity::for_self(
                    aterm_keeper::identity::IdentityPolicy::Designated,
                )
                .expect("this test binary has a designated requirement"),
            ),
        }
    }

    /// No code requirement: the same-uid floor every dialer of the `0600` socket already meets. The
    /// Tier-1's negative control.
    #[cfg(test)]
    pub(crate) fn same_uid_floor_for_test() -> Self {
        Self {
            rule: DialerCodeRule::Stated(aterm_keeper::identity::Identity::same_uid()),
        }
    }

    /// A check `start` makes — a stand-in for `trustd`, through the process's
    /// `trustd` gate ([`aterm_keeper::identity::PeerCheck::start`]) — waited for
    /// in line `in_line` before the dialer is held: so a check the gate let
    /// through can outlive its in-line wait without a ten-second test.
    #[cfg(all(test, target_vendor = "apple"))]
    fn scripted(
        in_line: Duration,
        start: impl Fn() -> aterm_keeper::identity::PeerCheck + Send + Sync + 'static,
    ) -> Self {
        Self {
            rule: DialerCodeRule::Scripted {
                start: Box::new(start),
                in_line,
            },
        }
    }

    /// One line for the log: what a dialer's code is checked against.
    pub(crate) fn describe(&self) -> String {
        match &self.rule {
            DialerCodeRule::Stated(identity) => identity.describe().to_string(),
            DialerCodeRule::CodeAt(path) => {
                format!(
                    "the designated requirement of the code at {}",
                    path.display()
                )
            }
            #[cfg(all(test, target_vendor = "apple"))]
            DialerCodeRule::Scripted { in_line, .. } => format!(
                "a scripted check, waited for in line {} ms",
                in_line.as_millis()
            ),
        }
    }

    /// How long [`admit_code`] waits in line for a check before it holds the
    /// dialer: the keeper's peer-check hang detector, as every peer check is.
    fn in_line(&self) -> Duration {
        match &self.rule {
            DialerCodeRule::Stated(_) | DialerCodeRule::CodeAt(_) => {
                aterm_keeper::identity::PEER_CHECK_HANG
            }
            #[cfg(all(test, target_vendor = "apple"))]
            DialerCodeRule::Scripted { in_line, .. } => *in_line,
        }
    }

    /// The check of whether the process `token` names — the dialer on `stream`
    /// as the gate read it once, at its claim ([`Rendezvous::gate_one`]) — runs
    /// code this rule admits, started for [`admit_code`] to wait for. The token
    /// must name the kernel-attested dialer pid `attested`, or nothing is
    /// checked ([`CodeRefusal::Unchecked`]); and it is the SAME token at every
    /// recheck of a held dialer, so a later image at that pid fails the code
    /// lookup instead of being judged in its place. A `trustd` that does not
    /// answer is never a pass ([`CodeRefusal::TrustdSilent`]).
    fn start(
        &self,
        stream: &CtlStream,
        attested: i32,
        token: Option<aterm_uds::peer::AuditToken>,
    ) -> Result<aterm_keeper::identity::PeerCheck, CodeRefusal> {
        if let Some(named) = token
            && u32::try_from(attested).ok() != Some(named.pid())
        {
            return Err(CodeRefusal::Unchecked(format!(
                "the kernel's audit token names pid {}, not the attested dialer",
                named.pid()
            )));
        }
        let read;
        let identity = match &self.rule {
            DialerCodeRule::Stated(identity) => identity,
            DialerCodeRule::CodeAt(path) => {
                read = aterm_keeper::identity::Identity::for_code_at(path).map_err(|why| {
                    CodeRefusal::Unchecked(format!(
                        "the code at {} has no requirement to check it against: {why}",
                        path.display()
                    ))
                })?;
                &read
            }
            #[cfg(all(test, target_vendor = "apple"))]
            DialerCodeRule::Scripted { start, .. } => return Ok(start()),
        };
        Ok(identity.start_check_named(stream, token))
    }
}

impl From<aterm_keeper::identity::CheckError> for CodeRefusal {
    fn from(error: aterm_keeper::identity::CheckError) -> Self {
        match error {
            aterm_keeper::identity::CheckError::Refused(why) => Self::Refused(why),
            aterm_keeper::identity::CheckError::Unanswered(why) => Self::TrustdSilent(why),
            aterm_keeper::identity::CheckError::Unchecked(why) => Self::Unchecked(why),
        }
    }
}

/// A dialer that presented this attempt's secret and was not admitted by its
/// code, as [`Rendezvous::accept_claim_observed`] reports it — while its
/// connection is still open, so the caller can witness the process alive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodeRefusedDialer {
    /// The kernel-attested pid (`LOCAL_PEERPID`).
    pub(crate) pid: i32,
    pub(crate) refusal: CodeRefusal,
    /// How the connection ended.
    pub(crate) end: CodeRefusedEnd,
}

/// How a connection [`CodeRefusedDialer`] names ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodeRefusedEnd {
    /// This process closed it on the refusal; the dialer was alive, and a
    /// successor that reads that end of file exits by its own rule (a failed
    /// claim exits before any window).
    Closed,
    /// It was held for a check `trustd` did not answer, and the dialer hung up
    /// while held: it left on its own, before any refusal of ours.
    LeftWhileHeld,
}

/// A connection [`Rendezvous::gate_one`] admitted, as it judged it.
struct Admitted {
    /// The kernel-attested dialer pid (`LOCAL_PEERPID`).
    pid: i32,
    /// The dialer claimed with `ATRZ2C`.
    chunks: bool,
    /// The dialer's audit token as the gate read it, once, and checked.
    token: Option<aterm_uds::peer::AuditToken>,
}

/// How [`Rendezvous::gate_one`] did not admit its connection.
#[derive(Debug)]
enum Gated {
    /// This connection is refused, and the wait goes on.
    Refused(RendezvousError),
    /// The wait itself ended — `end` is [`RendezvousError::Cancelled`] or
    /// [`RendezvousError::Deadline`] — while this connection was held for a
    /// code check `trustd` did not answer; `held` is that refusal.
    WaitEnded {
        end: RendezvousError,
        held: RendezvousError,
    },
}

/// The code gate over one connection that presented the secret ([`DialerCode`]),
/// HOLDING it while `trustd` does not answer: a check `trustd` has not answered
/// within the keeper's peer-check hang detector is still waited for, never
/// abandoned for a fresh one, and one the process's `trustd` gate refused at once
/// (an earlier check overdue) is asked again every [`CODE_RECHECK_INTERVAL`]; the
/// dialer is watched for a hangup and the wait's abort and deadline meanwhile (see
/// [`Rendezvous::accept_claim_observed`]). So the hold's one bound on `trustd` is
/// the wait's own deadline: a check that answers before it, however slowly, is
/// the hold's answer. Every check, the first and each recheck, judges the process
/// `token` named when the gate read it once, at the claim; never a fresh read.
/// `refused` hears of every way this connection ends unadmitted except the
/// wait's own end with the dialer still there, which the caller learns from
/// [`Gated::WaitEnded`].
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_rendezvous_admission",
        action = "DeadlineWhileHeld",
        project = "aterm_gui::app_update_handoff::launched_dialer_admission_conformance::project"
    )
)]
fn admit_code(
    stream: &CtlStream,
    pid: i32,
    token: Option<aterm_uds::peer::AuditToken>,
    code: &DialerCode,
    deadline: Instant,
    abort: &dyn Fn() -> bool,
    refused: &dyn Fn(&CodeRefusedDialer),
) -> Result<(), Gated> {
    let mut held_since: Option<Instant> = None;
    loop {
        let check = match code.start(stream, pid, token) {
            Ok(check) => check,
            Err(refusal) => {
                return Err(refuse_dialer(
                    stream,
                    pid,
                    refusal,
                    held_since.is_some(),
                    refused,
                ));
            }
        };
        // Before the hold the check is waited for in line, up to the keeper's
        // peer-check hang detector, as every peer check is; a held dialer's is
        // waited for between the watches below.
        let in_line = if held_since.is_some() {
            Duration::ZERO
        } else {
            code.in_line()
        };
        // `trustd`'s silence, and the check still waiting on it: none when the
        // gate refused this one at once.
        let (silent, mut waiting) = match check.wait(in_line) {
            Ok(Ok(())) => {
                admitted_after_hold(pid, held_since);
                return Ok(());
            }
            Ok(Err(error)) => match CodeRefusal::from(error) {
                CodeRefusal::TrustdSilent(why) => (why, None),
                refusal => {
                    return Err(refuse_dialer(
                        stream,
                        pid,
                        refusal,
                        held_since.is_some(),
                        refused,
                    ));
                }
            },
            Err(still) => (still.unanswered(), Some(still)),
        };
        let held = || RendezvousError::PeerCode {
            pid,
            refusal: CodeRefusal::TrustdSilent(silent.clone()),
        };
        if held_since.is_none() {
            held_since = Some(Instant::now());
            #[cfg(test)]
            HOLDS_FOR_TEST.with(|holds| holds.set(holds.get() + 1));
            aterm_log::warn!(
                "overlap handoff: holding the rendezvous dialer (pid {pid}) whose code could not \
                 be checked ({silent}); waiting for the check's answer (asked again every {} ms \
                 while the process's trustd gate refuses it) until it is admitted, refused or \
                 gone, or the dial deadline",
                CODE_RECHECK_INTERVAL.as_millis()
            );
        }
        let recheck_at = Instant::now() + CODE_RECHECK_INTERVAL;
        loop {
            if abort() {
                if dialer_left(stream, Duration::ZERO) {
                    refused(&CodeRefusedDialer {
                        pid,
                        refusal: CodeRefusal::TrustdSilent(silent.clone()),
                        end: CodeRefusedEnd::LeftWhileHeld,
                    });
                }
                return Err(Gated::WaitEnded {
                    end: RendezvousError::Cancelled,
                    held: held(),
                });
            }
            let now = io_now();
            if now >= deadline {
                return Err(Gated::WaitEnded {
                    end: RendezvousError::Deadline,
                    held: held(),
                });
            }
            let slice = deadline
                .saturating_duration_since(now)
                .min(ACCEPT_POLL_SLICE);
            // How long the dialer is watched for a hangup this slice: none while a
            // check took the slice waiting for its answer.
            let watch = match waiting.take() {
                Some(check) => match check.wait(slice) {
                    Ok(Ok(())) => {
                        admitted_after_hold(pid, held_since);
                        return Ok(());
                    }
                    // The gate let this check through, so its answer is a verdict or
                    // a lookup that failed; a silence is asked again.
                    Ok(Err(error)) => match CodeRefusal::from(error) {
                        CodeRefusal::TrustdSilent(_) => break,
                        refusal => {
                            return Err(refuse_dialer(stream, pid, refusal, true, refused));
                        }
                    },
                    Err(still) => {
                        waiting = Some(still);
                        Duration::ZERO
                    }
                },
                None if now >= recheck_at => break,
                None => recheck_at.saturating_duration_since(now).min(slice),
            };
            if dialer_left(stream, watch) {
                refused(&CodeRefusedDialer {
                    pid,
                    refusal: CodeRefusal::TrustdSilent(silent.clone()),
                    end: CodeRefusedEnd::LeftWhileHeld,
                });
                return Err(Gated::Refused(held()));
            }
        }
    }
}

/// The refusal of a dialer by `refusal`, told to `refused` first. A dialer that
/// died while `held` makes its own lookup fail: that is its leaving, not our
/// refusal of a live process.
fn refuse_dialer(
    stream: &CtlStream,
    pid: i32,
    refusal: CodeRefusal,
    held: bool,
    refused: &dyn Fn(&CodeRefusedDialer),
) -> Gated {
    let end = if held && dialer_left(stream, Duration::ZERO) {
        CodeRefusedEnd::LeftWhileHeld
    } else {
        CodeRefusedEnd::Closed
    };
    refused(&CodeRefusedDialer {
        pid,
        refusal: refusal.clone(),
        end,
    });
    Gated::Refused(RendezvousError::PeerCode { pid, refusal })
}

/// Says, for a dialer admitted after a hold since `held_since`, how long it was held.
fn admitted_after_hold(pid: i32, held_since: Option<Instant>) {
    if let Some(since) = held_since {
        aterm_log::info!(
            "overlap handoff: macOS's code-signing service answered; admitted the rendezvous \
             dialer (pid {pid}) after holding it {} ms",
            since.elapsed().as_millis()
        );
    }
}

/// Whether the dialer on a held connection has gone, waiting up to `wait` for
/// it to: a hangup, an error, or end of file. Bytes it sends instead are no
/// hangup (a successor sends nothing more before its grant) and are left
/// unread; the slice is slept out then, so a chatty peer cannot make the hold
/// spin.
fn dialer_left(stream: &CtlStream, wait: Duration) -> bool {
    let mut fds = [libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN | libc::POLLHUP,
        revents: 0,
    }];
    // SAFETY: `fds` is one initialised pollfd naming a descriptor `stream` owns,
    // valid for the call; the timeout is a bounded slice.
    let ready = unsafe {
        libc::poll(
            fds.as_mut_ptr(),
            1,
            i32::try_from(wait.as_millis()).unwrap_or(i32::MAX),
        )
    };
    if ready <= 0 {
        return false;
    }
    if fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return true;
    }
    let mut byte = 0u8;
    // SAFETY: a one-byte peek into a live local; `MSG_DONTWAIT` never blocks.
    let peeked = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            (&raw mut byte).cast::<libc::c_void>(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    if peeked == 0 {
        return true;
    }
    std::thread::sleep(wait);
    false
}

/// A bound, single-use rendezvous listener plus the secret that admits exactly one dialer to it.
///
/// Unlinks on drop: every rollback path in the worker drops it, so a late dial fails `ENOENT`,
/// which is how a successor slower than the parent's patience learns it has no handoff. On the
/// success path this process `_exit`s inside `seamless::commit_and_exit` and runs no destructor,
/// so the successor unlinks the socket after its claim; a node whose owner died is swept at the
/// next bind (`sweep_dead_rendezvous`).
pub(crate) struct Rendezvous {
    path: PathBuf,
    listener: CtlListener,
    claim: String,
}

impl Rendezvous {
    /// Bind the attempt's listener and mint its claim secret.
    ///
    /// Ordered so that everything which can refuse does so before anything
    /// exists to clean up: the directory, then the path length, then the
    /// secret, and only then the bind.
    pub(crate) fn bind(nonce: &str) -> Result<Self, RendezvousError> {
        let dir = rendezvous_dir().ok_or(RendezvousError::NoControlDir)?;
        Self::bind_in(&dir, nonce)
    }

    /// [`Self::bind`] in `dir`, a directory already established as this uid's
    /// own. Production binds only in [`rendezvous_dir`]; the tests also bind in
    /// a short directory of their own where that one cannot hold the socket.
    fn bind_in(dir: &Path, nonce: &str) -> Result<Self, RendezvousError> {
        use std::os::unix::fs::PermissionsExt as _;

        #[cfg(unix)]
        sweep_dead_rendezvous(dir);
        let path = rendezvous_path(dir, nonce);
        let text = path
            .to_str()
            .ok_or_else(|| RendezvousError::Bind("the control directory is not UTF-8".to_string()))?
            .to_string();
        if !crate::control_auth::sun_path_ok(&text) {
            return Err(RendezvousError::PathTooLong {
                path: text,
                limit: crate::control_auth::MAX_SUN_PATH,
            });
        }
        let claim = aterm_uds::rand::hex_token::<CLAIM_SECRET_BYTES>()
            .map_err(|error| RendezvousError::Secret(error.to_string()))?;
        let listener =
            CtlListener::bind(&path).map_err(|error| RendezvousError::Bind(error.to_string()))?;
        let bound = Self {
            path,
            listener,
            claim,
        };
        #[cfg(unix)]
        prove_bound_socket_is_ours(dir, &bound.path).map_err(RendezvousError::Bind)?;
        // The 0700 directory is the access boundary; 0600 on the node is the control socket's
        // defence in depth. From here the value owns the path, so a refusal unlinks by dropping.
        std::fs::set_permissions(&bound.path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| RendezvousError::Bind(error.to_string()))?;
        // The accept is deadline- and cancel-bounded, which a blocking `accept` cannot be: it
        // would hold the worker past the point the user's terminal must be given back.
        bound
            .listener
            .set_nonblocking(true)
            .map_err(|error| RendezvousError::Bind(error.to_string()))?;
        Ok(bound)
    }

    /// The path to publish in the successor's launch environment.
    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The claim secret to publish in the successor's launch environment. Never
    /// written to disk, never logged, and carried on no other channel.
    #[must_use]
    pub(crate) fn claim(&self) -> &str {
        &self.claim
    }

    /// Wait for the successor, check the gates, and hand back the connection.
    ///
    /// `expected_pid` is the pid LaunchServices reported for the instance it started, `None`
    /// while that answer has not arrived (a bundle's first launch can outlast the budget while its
    /// successor dials). `code` is the code the successor runs ([`DialerCode`]), what keeps a
    /// same-uid stranger out. `abort` is polled between slices. `refused` hears of every dialer
    /// that presented the secret and was not admitted by its code, before its connection is
    /// closed, so the caller can witness it alive and file its exit by that cause.
    ///
    /// A connection that fails a gate is refused; the attempt is not, or one `connect(2)` from any
    /// same-uid process would deny the update. The wait ends at the deadline, `abort`, a
    /// successful claim, or the [`MAX_REFUSED_DIALS`]th refusal, and each connection gets
    /// [`CLAIM_FRAME_BUDGET`] to present its frame. At the deadline the answer is the last refusal
    /// if there was one, else [`RendezvousError::Deadline`].
    ///
    /// A code check `trustd` did not answer holds its connection: closing the successor's
    /// connection when macOS's code-signing service stalls would make the successor read end of
    /// file and exit. Instead the connection is kept, its dialer watched for a hangup, and the
    /// answer to its code check waited for (asked again every [`CODE_RECHECK_INTERVAL`] while the
    /// process's `trustd` gate refuses it at once) until it is admitted, refused, gone, or the
    /// wait itself ends (the abort, or the deadline, which then answers with that refusal): a
    /// check `trustd` answers slowly is never cut short and asked again. Other dialers wait in
    /// the backlog meanwhile.
    pub(crate) fn accept_claim_observed(
        &self,
        expected_pid: Option<i32>,
        code: &DialerCode,
        deadline: Instant,
        abort: &dyn Fn() -> bool,
        refused: &dyn Fn(&CodeRefusedDialer),
    ) -> Result<ClaimedPeer, RendezvousError> {
        let mut refusals = 0usize;
        let mut last_refusal: Option<RendezvousError> = None;
        loop {
            let stream = match self.accept_one(deadline, abort) {
                Ok(stream) => stream,
                Err(RendezvousError::Deadline) => {
                    return Err(last_refusal.unwrap_or(RendezvousError::Deadline));
                }
                Err(error) => return Err(error),
            };
            let refusal = match self.gate_one(&stream, expected_pid, code, deadline, abort, refused)
            {
                Ok(Admitted { pid, chunks, token }) => {
                    return Ok(ClaimedPeer {
                        stream,
                        pid,
                        chunks,
                        token,
                    });
                }
                Err(Gated::Refused(refusal)) => refusal,
                // The wait ended while this connection was held for its code
                // check: the deadline answers with that refusal, as with any last
                // refusal; the abort keeps its own name.
                Err(Gated::WaitEnded { end, held }) => {
                    return Err(match end {
                        RendezvousError::Deadline => held,
                        other => other,
                    });
                }
            };
            refusals += 1;
            aterm_log::warn!(
                "overlap handoff: refused rendezvous dialer #{refusals} ({refusal}); the \
                 rendezvous stays open for the successor until the deadline"
            );
            if refusals >= MAX_REFUSED_DIALS {
                return Err(refusal);
            }
            last_refusal = Some(refusal);
            // Dropping `stream` before the next accept tells the refused dialer it was refused,
            // so a flood cannot accumulate held connections.
        }
    }

    /// [`Self::accept_claim_observed`] with no one to tell of a code refusal:
    /// the tests' spelling.
    #[cfg(test)]
    pub(crate) fn accept_claim(
        &self,
        expected_pid: Option<i32>,
        code: &DialerCode,
        deadline: Instant,
        abort: &dyn Fn() -> bool,
    ) -> Result<ClaimedPeer, RendezvousError> {
        self.accept_claim_observed(expected_pid, code, deadline, abort, &|_| {})
    }

    /// The gates over one accepted connection, answering the attested peer pid.
    ///
    /// Every [`Gated::Refused`] is a refusal of this connection, and nothing
    /// else here ends the attempt, which is why the frame read is bounded by its
    /// own slice as well as by the attempt deadline: an expiry inside this
    /// function must mean "that peer took too long", not "the handoff is over".
    /// The one exception is a connection held for a code check `trustd` did not
    /// answer ([`CodeRefusal::TrustdSilent`]): it is held for as long as the
    /// wait itself runs, so the abort and the deadline end it there
    /// ([`Gated::WaitEnded`]). Before that hold a check is waited for in line
    /// up to the keeper's peer-check hang detector
    /// (`aterm_keeper::identity::PEER_CHECK_HANG`), and then held, still waited
    /// for ([`admit_code`]).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "native_update_rendezvous_admission",
            action = "Grant",
            project = "aterm_gui::app_update_handoff::launched_dialer_admission_conformance::project"
        )
    )]
    fn gate_one(
        &self,
        stream: &CtlStream,
        expected_pid: Option<i32>,
        code: &DialerCode,
        deadline: Instant,
        abort: &dyn Fn() -> bool,
        refused: &dyn Fn(&CodeRefusedDialer),
    ) -> Result<Admitted, Gated> {
        // The tighter of the two bounds: an unknown same-uid peer must not spend the attempt's
        // whole budget by connecting and saying nothing, and a real successor writes this frame
        // in one call.
        let frame_deadline = deadline.min(Instant::now() + CLAIM_FRAME_BUDGET);
        let mut frame = [0u8; CLAIM_FRAME_LEN];
        read_frame_by_deadline(stream, &mut frame, frame_deadline, &|error| {
            RendezvousError::Accept(error.to_string())
        })
        .map_err(Gated::Refused)?;
        let Some(chunks) = claim_frame_kind(&frame, &self.claim) else {
            return Err(Gated::Refused(RendezvousError::Claim));
        };
        // Identity second, only once the secret has proven this is our attempt's dialer, so a
        // stranger learns nothing about which pid was expected.
        let attested = fdpass::peer_pid(stream)
            .map_err(|error| Gated::Refused(RendezvousError::PeerUnattested(error.to_string())))?;
        let attested = i32::try_from(attested).map_err(|_| {
            Gated::Refused(RendezvousError::PeerPid {
                expected: expected_pid.unwrap_or(-1),
                actual: -1,
            })
        })?;
        if let Some(expected) = expected_pid
            && attested != expected
        {
            return Err(Gated::Refused(RendezvousError::PeerPid {
                expected,
                actual: attested,
            }));
        }
        // Code last, and only for a dialer that presented the secret. The audit token is read
        // once, here: the kernel answers it for whoever is on the other end at each read, so
        // this one token is what every check judges and what the grant is sent against
        // ([`ClaimedPeer::transfer`]). It must name the attested pid.
        let token = aterm_uds::peer::peer_audit_token(stream);
        admit_code(stream, attested, token, code, deadline, abort, refused)?;
        // The claimed connection leaves with a write bound already on it; `transfer` refreshes
        // it against the same deadline before the `sendmsg`, so no socket this module hands out
        // can block forever on a write.
        stream
            .set_write_timeout(Some(remaining_io_budget(deadline).map_err(Gated::Refused)?))
            .map_err(|error| Gated::Refused(RendezvousError::Accept(error.to_string())))?;
        Ok(Admitted {
            pid: attested,
            chunks,
            token,
        })
    }

    /// One `accept`, bounded by the deadline and the abort predicate.
    fn accept_one(
        &self,
        deadline: Instant,
        abort: &dyn Fn() -> bool,
    ) -> Result<CtlStream, RendezvousError> {
        loop {
            if abort() {
                return Err(RendezvousError::Cancelled);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(RendezvousError::Deadline);
            }
            let slice = deadline
                .saturating_duration_since(now)
                .min(ACCEPT_POLL_SLICE);
            let mut pollfd = libc::pollfd {
                fd: self.listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one initialized pollfd naming a descriptor this value
            // owns; the timeout is a bounded slice, so the loop always returns
            // to the abort/deadline checks above.
            let polled = unsafe {
                libc::poll(
                    &mut pollfd,
                    1,
                    i32::try_from(slice.as_millis()).unwrap_or(0),
                )
            };
            if polled < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(RendezvousError::Accept(error.to_string()));
            }
            if polled == 0 {
                continue;
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    // BSD `accept(2)` hands back a socket that inherited the
                    // listener's non-blocking flag and Linux does not. Set it
                    // explicitly rather than depending on which this is, or the
                    // framed reads would answer `WouldBlock` instead of
                    // honouring their timeout.
                    stream
                        .set_nonblocking(false)
                        .map_err(|error| RendezvousError::Accept(error.to_string()))?;
                    return Ok(stream);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    // A peer that connected and reset before we accepted, or a
                    // signal. Neither is this attempt's dialer arriving.
                    continue;
                }
                // Anything else ends the wait, an accept short of a descriptor included: Darwin
                // has already dequeued that dial and closed it, so the dialer is told at once and
                // nothing is left for a retry. Room is made before the rendezvous instead
                // ([`raise_descriptor_limit_for_park`]).
                Err(error) => return Err(RendezvousError::Accept(error.to_string())),
            }
        }
    }
}

impl Drop for Rendezvous {
    fn drop(&mut self) {
        // Closing the listener fails an in-flight dial closed; the unlink fails a later dial.
        // The path carries this process's pid and this attempt's nonce, so this never removes
        // another attempt's node.
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(launch_stamp_path(&self.path));
    }
}

/// What [`launch_stamp_path`] appends to the rendezvous path.
const LAUNCH_STAMP_SUFFIX: &str = ".launch";

/// Where the successor of the attempt bound at `rendezvous` stamps the name its
/// boot-trial launch counts under ([`stamp_this_launch`]): beside the node, in the
/// same private directory.
#[must_use]
pub(crate) fn launch_stamp_path(rendezvous: &Path) -> PathBuf {
    let mut stamp = rendezvous.as_os_str().to_os_string();
    stamp.push(LAUNCH_STAMP_SUFFIX);
    PathBuf::from(stamp)
}

/// The launched successor names its own launch before its boot apply can count a trial launch.
/// LaunchServices reports a successor that dies before it checks in with no pid
/// (`PreDialDeath::TerminatedBeforeCheckIn`), and every successor dials before it checks in, so
/// its parent has no name to give that counted launch back by. So this launch stamps the name
/// the sentinel counts it under beside the rendezvous ([`aterm_update::stamp_trial_launch`]),
/// and the parent forgives exactly that launch ([`aterm_update::forgive_stamped_trial_launch`]).
/// Not on the fork lane, whose parent names its child. Best effort: an unwritten stamp leaves
/// the launch counted, which keeps the crash signal.
pub(crate) fn stamp_this_launch(env: &crate::handoff_env::HandoffEnv) {
    let Some(rendezvous) = env.os(ENV_RENDEZVOUS) else {
        return;
    };
    if let Err(error) = aterm_update::stamp_trial_launch(&launch_stamp_path(Path::new(rendezvous)))
    {
        aterm_log::warn!(
            "overlap handoff: could not stamp this launch beside the rendezvous ({error}); a \
             boot-trial launch it counts stays counted if it dies before its dial"
        );
    }
}

/// A connection that passed the gates: it presented this attempt's secret, the
/// kernel attested its pid (and, where LaunchServices had answered, that pid is
/// the launched successor's), and the code of the process its audit token named,
/// read once at the claim and naming that pid, is the successor's
/// ([`DialerCode`]).
pub(crate) struct ClaimedPeer {
    stream: CtlStream,
    pid: i32,
    /// The dialer claimed with `ATRZ2C`: it reads the chunked grant.
    chunks: bool,
    /// The dialer's audit token as the gate read and checked it. The grant
    /// leaves only while the kernel still answers this same token
    /// ([`Self::transfer`]).
    token: Option<aterm_uds::peer::AuditToken>,
}

impl ClaimedPeer {
    /// Whether the held dialer has gone: one zero-timeout `poll` for HUP/ERR/NVAL
    /// on the served stream. The parent's witness, while it holds a claim between
    /// accept and transfer, that the successor died before any descriptor left —
    /// so the attempt is stood down without a park rather than granted to a
    /// corpse. Never `true` for a merely idle peer (readable data is not a hangup).
    #[must_use]
    pub(crate) fn poll_hangup(&self) -> bool {
        #[cfg(unix)]
        {
            let mut fds = [libc::pollfd {
                fd: self.stream.as_raw_fd(),
                events: libc::POLLIN | libc::POLLHUP,
                revents: 0,
            }];
            // SAFETY: `fds` is a valid array of one initialised pollfd for the
            // length of the call; a zero timeout never blocks.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, 0) };
            if ready <= 0 {
                return false;
            }
            fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// The kernel-attested dialer pid. This is the first identity the handoff
    /// has for a process it did not fork, and it is what `HandoffCandidate` and
    /// `signal_handoff_candidate` need before they may aim anything at a
    /// candidate launchd owns.
    #[must_use]
    pub(crate) fn pid(&self) -> i32 {
        self.pid
    }

    /// Whether the kernel still names, on this connection, the very process the gate checked: a
    /// fresh audit token, compared word for word with the one the gate read and judged
    /// ([`Rendezvous::gate_one`]). Asked just before the `sendmsg` that carries the first
    /// descriptor, so a connection whose other end is no longer that process (another process
    /// using it, or a new image at the pid) gets [`RendezvousError::TransferNotSent`]. The module
    /// docs state what it cannot see.
    fn dialer_unchanged(&self) -> Result<(), RendezvousError> {
        let now = aterm_uds::peer::peer_audit_token(&self.stream);
        if now == self.token {
            return Ok(());
        }
        Err(RendezvousError::TransferNotSent(format!(
            "the dialer's audit token changed since its code was checked (pid {} then, {} now)",
            self.token
                .map_or_else(|| "unnamed".to_string(), |token| token.pid().to_string()),
            now.map_or_else(|| "unnamed".to_string(), |token| token.pid().to_string())
        )))
    }

    /// The most sessions [`Self::transfer`] can hand this dialer: what its claim (`ATRZ2C` or
    /// `ATRZ1C`) says it reads. The park compares its pool against this before it freezes
    /// anything, so a claim that cannot carry the desk stands the attempt down with every reader
    /// live.
    #[must_use]
    pub(crate) fn grant_session_limit(&self) -> usize {
        grant_session_limit(self.chunks)
    }

    /// Send every descriptor of the handoff in one message, then the body that says which is
    /// which. `sessions` is `(local_id, shell pid, master)` in transfer order, and that order is
    /// the addressing: the successor pairs its Nth received descriptor with the Nth body entry;
    /// the readiness and Commit descriptors follow the masters.
    ///
    /// One `sendmsg` for all descriptors makes this all-or-nothing: the parent's only two
    /// dispositions are "no descriptor of ours left" and "the successor has everything". A
    /// successor that received descriptors but no body refuses the grant and exits, which this
    /// process learns as proof EOF.
    ///
    /// More than 62 sessions take the chunked grant, and only for a dialer that
    /// claimed it (`ATRZ2C`); an `ATRZ1C` dialer is refused
    /// [`RendezvousError::TooManySessions`] before anything is sent. The chunked
    /// grant keeps both dispositions: its first `sendmsg` is still the line
    /// before which no descriptor has left ([`RendezvousError::TransferNotSent`]),
    /// and every failure after it — a continuation included — is
    /// [`RendezvousError::TransferPartial`], the kill-and-prove disposition. A
    /// successor holding only some chunks refuses the grant and closes them all.
    /// Up to 62 sessions the grant uses the one-message `ATRZ1G` header,
    /// whatever the dialer claimed.
    ///
    /// `parked_at`, when the readers were parked, becomes the body's park stamp:
    /// elapsed time sampled before encoding and sending this grant (module docs).
    pub(crate) fn transfer(
        &self,
        nonce: &str,
        sessions: &[(u64, i32, BorrowedFd<'_>)],
        ready: BorrowedFd<'_>,
        commit: BorrowedFd<'_>,
        deadline: Instant,
        parked_at: Option<Instant>,
    ) -> Result<(), RendezvousError> {
        let shape = grant_shape(self.chunks, sessions.len())?;
        let malformed = || {
            RendezvousError::TransferNotSent("the grant body exceeds its wire format".to_string())
        };
        let parked_for = parked_at.map(|parked| parked.elapsed());
        let body = encode_grant_body(nonce, sessions, parked_for).ok_or_else(malformed)?;
        if shape == GrantShape::Chunked {
            return self.transfer_chunked(&body, sessions, ready, commit, deadline);
        }
        let header = grant_header(body.len()).ok_or_else(malformed)?;
        let remaining = grant_budget(deadline)?;
        self.stream
            .set_write_timeout(Some(remaining))
            .map_err(|error| RendezvousError::TransferNotSent(error.to_string()))?;
        let mut descriptors = Vec::with_capacity(sessions.len() + RENDEZVOUS_CHANNEL_FDS);
        descriptors.extend(sessions.iter().map(|(_, _, master)| *master));
        descriptors.push(ready);
        descriptors.push(commit);
        self.dialer_unchanged()?;
        let sent = fdpass::send_with_fds(&self.stream, &header, &descriptors)
            .map_err(|error| RendezvousError::TransferNotSent(error.to_string()))?;
        // From here the descriptors are the peer's too: every failure below is a partial
        // transfer, and the caller owes the candidate a proof of death before it resumes its
        // readers. A short send went with its descriptors on the first byte, so the rest of
        // the header is finished with an ordinary write, never resent.
        if sent < header.len() {
            (&self.stream)
                .write_all(&header[sent..])
                .map_err(|error| RendezvousError::TransferPartial(error.to_string()))?;
        }
        (&self.stream)
            .write_all(body.as_bytes())
            .and_then(|()| (&self.stream).flush())
            .map_err(|error| RendezvousError::TransferPartial(error.to_string()))?;
        Ok(())
    }

    /// The chunked grant (see [`Self::transfer`] and the module docs): the
    /// `ATRZ2G` header carrying the first chunk of descriptors, the body, then
    /// one `ATRZ2K` continuation per further chunk, laid out by
    /// [`grant_chunk_sizes`].
    fn transfer_chunked(
        &self,
        body: &str,
        sessions: &[(u64, i32, BorrowedFd<'_>)],
        ready: BorrowedFd<'_>,
        commit: BorrowedFd<'_>,
        deadline: Instant,
    ) -> Result<(), RendezvousError> {
        let sizes = grant_chunk_sizes(sessions.len());
        let header = grant_chunks_header(body.len(), sizes.len()).ok_or_else(|| {
            RendezvousError::TransferNotSent("the grant body exceeds its wire format".to_string())
        })?;
        let mut descriptors = Vec::with_capacity(sessions.len() + RENDEZVOUS_CHANNEL_FDS);
        descriptors.extend(sessions.iter().map(|(_, _, master)| *master));
        descriptors.push(ready);
        descriptors.push(commit);
        let mut chunks = Vec::with_capacity(sizes.len());
        let mut rest = descriptors.as_slice();
        for size in &sizes {
            let (chunk, tail) = rest.split_at(*size);
            chunks.push(chunk);
            rest = tail;
        }
        debug_assert!(rest.is_empty(), "the layout places every descriptor");
        let remaining = grant_budget(deadline)?;
        self.stream
            .set_write_timeout(Some(remaining))
            .map_err(|error| RendezvousError::TransferNotSent(error.to_string()))?;
        self.dialer_unchanged()?;
        let sent = fdpass::send_with_fds(&self.stream, &header, chunks[0])
            .map_err(|error| RendezvousError::TransferNotSent(error.to_string()))?;
        // From here the first chunk is the peer's: every failure below, a continuation's too,
        // is a partial transfer the caller owes a proof for.
        let partial = |error: std::io::Error| RendezvousError::TransferPartial(error.to_string());
        if sent < header.len() {
            (&self.stream).write_all(&header[sent..]).map_err(partial)?;
        }
        (&self.stream).write_all(body.as_bytes()).map_err(partial)?;
        for (index, chunk) in chunks.iter().enumerate().skip(1) {
            let frame = grant_continuation(index).ok_or_else(|| {
                RendezvousError::TransferPartial("a chunk index past its wire format".to_string())
            })?;
            let remaining = remaining_io_budget(deadline).map_err(|_| {
                RendezvousError::TransferPartial(
                    "the grant deadline passed between its chunks".to_string(),
                )
            })?;
            self.stream
                .set_write_timeout(Some(remaining))
                .map_err(partial)?;
            let sent = fdpass::send_with_fds(&self.stream, &frame, chunk).map_err(partial)?;
            if sent < frame.len() {
                (&self.stream).write_all(&frame[sent..]).map_err(partial)?;
            }
        }
        (&self.stream).flush().map_err(partial)?;
        Ok(())
    }
}

/// A test's dialer: connect to `path` and present `secret`, keeping the stream
/// so the accepted claim can be held by the listener (the late park's shape)
/// and closed from that side. The production dialer is `dial_and_claim`. The
/// frame is built first, so the connect and the write run back to back: the
/// listener's read of it is bounded from its accept ([`CLAIM_FRAME_BUDGET`]).
#[cfg(test)]
pub(crate) fn dial_for_test(path: &Path, secret: &str) -> std::io::Result<CtlStream> {
    let frame = claim_frame(secret);
    let stream = CtlStream::connect(path)?;
    (&stream).write_all(&frame)?;
    (&stream).flush()?;
    Ok(stream)
}

/// What a test says when the gate HELD its dialer to the test's hang detector
/// because `trustd` did not answer its code check. The process's one `trustd`
/// gate (`aterm_uds::deadline::TRUST`) is shared with every code and TLS check
/// in this test process, so a check elsewhere that `trustd` left unanswered can
/// hold this one too. Not a pass, and not a verdict on the code either way:
/// [`trustd_silent`] ends the test as COULD NOT RUN.
#[cfg(test)]
pub(crate) const TRUSTD_SILENT: &str =
    "the process trustd gate was silent — the machine's, not the code's";

/// End the calling test as COULD NOT RUN ([`TRUSTD_SILENT`]), `why` naming the
/// check `trustd` did not answer.
#[cfg(test)]
#[track_caller]
pub(crate) fn trustd_silent(why: impl std::fmt::Display) -> ! {
    aterm_spec::host::could_not_run(format_args!("{TRUSTD_SILENT}: {why}"))
}

/// Whether the gate ended on a dialer it held for a code check `trustd` did not
/// answer.
#[cfg(test)]
pub(crate) fn held_for_trustd(error: &RendezvousError) -> bool {
    matches!(
        error,
        RendezvousError::PeerCode {
            refusal: CodeRefusal::TrustdSilent(_),
            ..
        }
    )
}

/// A test's accepted claim (the peer, or what was built on it), or a panic that
/// names its cause: a dialer held for `trustd` is [`TRUSTD_SILENT`], never a
/// failure of `what`.
#[cfg(test)]
pub(crate) fn admitted<T>(accepted: Result<T, RendezvousError>, what: &str) -> T {
    match accepted {
        Ok(peer) => peer,
        Err(error) if held_for_trustd(&error) => trustd_silent(error),
        Err(error) => panic!("{what}: {error}"),
    }
}

/// The cross-version guard's door onto this build's successor side (`seamless::fixture_tests`):
/// dial `path`, present `secret` as this build presents it to a parent that published no grant
/// capabilities, and answer the `ATERM_SEAMLESS_FDS` wire it would publish with the descriptors
/// it received (masters in wire order, then the readiness and Commit ends).
#[cfg(test)]
pub(crate) fn dial_and_claim_for_fixture(
    path: &Path,
    secret: &str,
    expected_nonce: &str,
    deadlines: ClaimDeadlines,
) -> Result<(String, Vec<OwnedFd>, OwnedFd, OwnedFd), RendezvousError> {
    // The stub parent listens in this process, so this process is the attested
    // parent the listener must be (`listener_may_receive_claim`).
    let parent = libc::pid_t::try_from(std::process::id())
        .ok()
        .and_then(crate::seamless::AttestedParent::attest_live_for_test);
    let claimed = dial_and_claim(path, secret, expected_nonce, deadlines, parent, false)?;
    Ok((
        claimed.fds_wire,
        claimed.masters,
        claimed.ready,
        claimed.commit,
    ))
}

/// The one-message claim (`ATRZ1C`) — what a successor offered nothing presents.
#[cfg(test)]
fn claim_frame(secret: &str) -> [u8; CLAIM_FRAME_LEN] {
    claim_frame_with(secret, false)
}

/// The claim frame, under `ATRZ2C` when `chunks` (this successor reads the
/// chunked grant and its parent offered it) and `ATRZ1C` otherwise.
fn claim_frame_with(secret: &str, chunks: bool) -> [u8; CLAIM_FRAME_LEN] {
    let mut frame = [0u8; CLAIM_FRAME_LEN];
    frame[..CLAIM_MAGIC.len()].copy_from_slice(if chunks {
        CLAIM_CHUNKS_MAGIC
    } else {
        CLAIM_MAGIC
    });
    // A secret of the wrong length cannot match a well-formed frame anyway, and
    // the copy is bounded so no caller can make this panic. The tail is left
    // zero, which is not a hex character and therefore not a secret.
    let bytes = secret.as_bytes();
    let carried = bytes.len().min(CLAIM_HEX_LEN);
    frame[CLAIM_MAGIC.len()..CLAIM_MAGIC.len() + carried].copy_from_slice(&bytes[..carried]);
    frame
}

/// Whether a received claim frame carries this attempt's secret, and if it does,
/// whether its dialer claimed the chunked grant: `Some(false)` for `ATRZ1C`,
/// `Some(true)` for `ATRZ2C`, `None` for anything else.
///
/// The magic is compared ordinarily — it is a constant, and leaking that a peer
/// got a fixed prefix wrong tells nobody anything they did not already choose.
/// The secret is compared in constant time, because that comparison's timing is
/// the only thing that could otherwise be walked one byte at a time.
fn claim_frame_kind(frame: &[u8; CLAIM_FRAME_LEN], secret: &str) -> Option<bool> {
    let magic = &frame[..CLAIM_MAGIC.len()];
    let chunks = if magic == CLAIM_MAGIC {
        false
    } else if magic == CLAIM_CHUNKS_MAGIC {
        true
    } else {
        return None;
    };
    aterm_digest::ct_eq(&frame[CLAIM_MAGIC.len()..], secret.as_bytes()).then_some(chunks)
}

/// Whether a received claim frame carries this attempt's secret, under either
/// claim magic.
#[cfg(test)]
fn claim_frame_matches(frame: &[u8; CLAIM_FRAME_LEN], secret: &str) -> bool {
    claim_frame_kind(frame, secret).is_some()
}

/// The fixed grant header: magic plus body length. `None` when the body does not
/// fit the format — a refusal rather than a truncation.
fn grant_header(body_len: usize) -> Option<[u8; GRANT_HEADER_LEN]> {
    if body_len > MAX_GRANT_BODY_BYTES {
        return None;
    }
    let mut header = [0u8; GRANT_HEADER_LEN];
    header[..GRANT_MAGIC.len()].copy_from_slice(GRANT_MAGIC);
    header[GRANT_MAGIC.len()..].copy_from_slice(&u16::try_from(body_len).ok()?.to_be_bytes());
    Some(header)
}

/// The chunked grant's fixed header: magic, body length, chunk count. `None`
/// when either does not fit — a refusal rather than a truncation.
fn grant_chunks_header(body_len: usize, chunks: usize) -> Option<[u8; GRANT_CHUNKS_HEADER_LEN]> {
    if body_len > MAX_GRANT_BODY_BYTES || chunks == 0 || chunks > MAX_GRANT_CHUNKS {
        return None;
    }
    let mut header = [0u8; GRANT_CHUNKS_HEADER_LEN];
    header[..GRANT_CHUNKS_MAGIC.len()].copy_from_slice(GRANT_CHUNKS_MAGIC);
    let length = GRANT_CHUNKS_MAGIC.len();
    header[length..length + 2].copy_from_slice(&u16::try_from(body_len).ok()?.to_be_bytes());
    header[length + 2] = u8::try_from(chunks).ok()?;
    Some(header)
}

/// The `index`th message of a chunked grant (`index >= 1`; the header message
/// is 0).
fn grant_continuation(index: usize) -> Option<[u8; GRANT_CONTINUATION_LEN]> {
    if index == 0 || index >= MAX_GRANT_CHUNKS {
        return None;
    }
    let mut frame = [0u8; GRANT_CONTINUATION_LEN];
    frame[..GRANT_CONTINUATION_MAGIC.len()].copy_from_slice(GRANT_CONTINUATION_MAGIC);
    frame[GRANT_CONTINUATION_MAGIC.len()] = u8::try_from(index).ok()?;
    Some(frame)
}

/// The park stamp's key on the grant body's optional last line.
const GRANT_PARK_KEY: &str = "park_us=";

/// `"<nonce>\n<lid>:<pid>,<lid>:<pid>\n"` — everything the successor needs to
/// pair a received descriptor with a session, and nothing that names a
/// descriptor — then the parent's sampled park duration `parked_for` as the park
/// stamp `"park_us=<µs>\n"` (module docs).
fn encode_grant_body(
    nonce: &str,
    sessions: &[(u64, i32, BorrowedFd<'_>)],
    parked_for: Option<Duration>,
) -> Option<String> {
    if !aterm_codec::hex::is_hex(nonce, 32) {
        return None;
    }
    let entries = sessions
        .iter()
        .map(|(local_id, pid, _)| format!("{local_id}:{pid}"))
        .collect::<Vec<_>>()
        .join(",");
    let park = match parked_for {
        Some(parked) => format!(
            "{GRANT_PARK_KEY}{}\n",
            u64::try_from(parked.as_micros()).ok()?,
        ),
        None => String::new(),
    };
    let body = format!("{nonce}\n{entries}\n{park}");
    (body.len() <= MAX_GRANT_BODY_BYTES).then_some(body)
}

/// A parsed grant body: `(local_id, shell pid)` in transfer order, and the park
/// stamp when the parent sent one.
#[derive(Debug, PartialEq, Eq)]
struct GrantBody {
    sessions: Vec<(u64, i32)>,
    parked_for: Option<Duration>,
}

/// Parse a grant body ([`encode_grant_body`]).
///
/// Total and allocation-bounded: the body was already length-framed by the
/// header, so nothing here reserves from a number the body itself chose.
fn parse_grant_body(body: &str, expected_nonce: &str) -> Result<GrantBody, RendezvousError> {
    let Some((nonce, rest)) = body.split_once('\n') else {
        return Err(RendezvousError::Grant("no nonce line".to_string()));
    };
    if nonce != expected_nonce {
        return Err(RendezvousError::NonceMismatch);
    }
    let Some(rest) = rest.strip_suffix('\n') else {
        return Err(RendezvousError::Grant(
            "unterminated session list".to_string(),
        ));
    };
    let (entries, parked_for) = match rest.split_once('\n') {
        None => (rest, None),
        Some((entries, park)) => {
            let micros = park
                .strip_prefix(GRANT_PARK_KEY)
                .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|digits| digits.parse::<u64>().ok())
                .ok_or_else(|| RendezvousError::Grant(format!("malformed park stamp {park}")))?;
            (entries, Some(Duration::from_micros(micros)))
        }
    };
    if entries.is_empty() {
        // An overlap with no sessions is not an overlap. The lane is only ever
        // taken with a live pool, so an empty list is a malformed grant rather
        // than a valid empty one.
        return Err(RendezvousError::Grant("no sessions".to_string()));
    }
    let mut sessions = Vec::new();
    for entry in entries.split(',') {
        let Some((local_id, pid)) = entry.split_once(':') else {
            return Err(RendezvousError::Grant(format!("malformed entry {entry}")));
        };
        let (Ok(local_id), Ok(pid)) = (local_id.parse::<u64>(), pid.parse::<i32>()) else {
            return Err(RendezvousError::Grant(format!("malformed entry {entry}")));
        };
        if pid <= 0 {
            return Err(RendezvousError::Grant(format!(
                "implausible pid in {entry}"
            )));
        }
        sessions.push((local_id, pid));
    }
    let mut ids = sessions.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(RendezvousError::Grant("duplicate session id".to_string()));
    }
    Ok(GrantBody {
        sessions,
        parked_for,
    })
}

/// What a successor holds after a successful claim: the descriptors, and the
/// `ATERM_SEAMLESS_FDS` wire that names them by the numbers this process received them at.
/// Built here because only this place knows both the body's `(local_id, pid)` order and the
/// numbers the kernel chose. Installed in the snapshot ([`ClaimedIntake::install`]), it lets
/// the unchanged `seamless::take_incoming_from` run every authentication: the manifest join,
/// the bijection check, the tty backstop, the screen-carry digest.
pub(crate) struct ClaimedHandoff {
    fds_wire: String,
    masters: Vec<OwnedFd>,
    ready: OwnedFd,
    commit: OwnedFd,
    parked_for: Option<Duration>,
}

impl ClaimedHandoff {
    /// How many sessions the grant carried — for the one log line that tells a
    /// field investigation which lane this process arrived on.
    #[must_use]
    pub(crate) fn session_count(&self) -> usize {
        self.masters.len()
    }

    /// Elapsed parent park→sample time, sampled before encoding and sending the
    /// grant, or `None` from a parent that sends no park stamp (module docs).
    #[must_use]
    pub(crate) fn parked_before_grant(&self) -> Option<Duration> {
        self.parked_for
    }

    /// The claim as an intake: the wire and the owned descriptors, in memory, so no process
    /// environment is mutated while other threads run.
    #[must_use]
    pub(crate) fn into_intake(self) -> ClaimedIntake {
        let Self {
            fds_wire,
            masters,
            ready,
            commit,
            parked_for: _,
        } = self;
        ClaimedIntake {
            fds_wire,
            masters,
            ready,
            commit,
        }
    }
}

/// A granted claim on its way to the intake: the descriptor wire plus every
/// descriptor it names, still owned.
pub(crate) struct ClaimedIntake {
    fds_wire: String,
    masters: Vec<OwnedFd>,
    ready: OwnedFd,
    commit: OwnedFd,
}

impl ClaimedIntake {
    /// Land the claimed descriptors in the handoff snapshot under the fork lane's three names
    /// (`ATERM_SEAMLESS_FDS`, `ATERM_HANDOFF_READY_FD`, `ATERM_HANDOFF_COMMIT_FD`), so
    /// `seamless::take_incoming_from`, `take_ready_fd_from` and `take_commit_fd_from` run the
    /// unchanged authentication over them.
    ///
    /// The descriptors are released here: `seamless`'s consumers take ownership as on the fork
    /// lane, where the numbers arrive through `execve` with no Rust owner, and each closes what
    /// it refuses. `env` is this process's own memory, so this is sound on any thread.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "native_update_successor_warm_before_claim",
            action = "ClaimOk",
            project = "aterm_gui::seamless::handoff_env_conformance::project"
        )
    )]
    pub(crate) fn install(self, env: &mut crate::handoff_env::HandoffEnv) {
        let Self {
            fds_wire,
            masters,
            ready,
            commit,
        } = self;
        env.set(crate::seamless::ENV_FDS, fds_wire);
        env.set(crate::seamless::ENV_READY_FD, ready.as_raw_fd().to_string());
        env.set(
            crate::seamless::ENV_COMMIT_FD,
            commit.as_raw_fd().to_string(),
        );
        // Released only after the names naming them are in place, so no early
        // exit between the two can leave a name pointing at a closed number.
        for master in masters {
            let _ = master.into_raw_fd();
        }
        let _ = ready.into_raw_fd();
        let _ = commit.into_raw_fd();
    }
}

/// Whether this launch carries a rendezvous at all: the successor's one test for which lane it
/// arrived on. Either name alone counts, while [`claim_incoming`] needs both: half an
/// environment is corruption, not absence, and a successor that started a fresh window would
/// put an empty terminal over a parent's live sessions. The claim path refuses it with
/// [`RendezvousError::HalfRendezvous`].
#[must_use]
pub(crate) fn rendezvous_present(env: &crate::handoff_env::HandoffEnv) -> bool {
    env.present(ENV_RENDEZVOUS) || env.present(ENV_CLAIM)
}

/// The parent's explicit statement of which proof term its expected adoption
/// proof was hashed over: `device` (PTY `st_rdev`, the launched lane's term) or
/// `fd` (descriptor numbers, the fork lane's). Consumed once at boot. A launched
/// attempt that falls back to the fork lane keeps its device-term proof, so the
/// successor must not infer the term from how the descriptors arrived.
pub(crate) const ENV_PROOF_TERM: HandoffName = HandoffName::new("ATERM_HANDOFF_PROOF_TERM");

/// Consume [`ENV_PROOF_TERM`]: `Some(true)` for device terms, `Some(false)` for fd numbers,
/// `None` when the parent said nothing (an older parent: infer from the lane).
#[must_use]
pub(crate) fn take_device_proof_term(env: &mut crate::handoff_env::HandoffEnv) -> Option<bool> {
    let value = env.take(ENV_PROOF_TERM)?;
    parse_proof_term(&value.to_string_lossy())
}

/// Read one [`ENV_PROOF_TERM`] value: `Some(true)` for `device`, `Some(false)`
/// for `fd`, `None` for anything else. Pure, so the cross-version guard can hand
/// it the spellings shipped parents publish.
#[must_use]
pub(crate) fn parse_proof_term(value: &str) -> Option<bool> {
    match value {
        "device" => Some(true),
        "fd" => Some(false),
        _ => None,
    }
}

/// How a launched successor dies at the [`debug_die_before_dial`] seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DebugDeathBeforeDial {
    /// `_exit` with [`DEBUG_DEATH_EXIT_CODE`], as a diagnosed launch-fatal does
    /// (the containment-deny fatal inside the warm prologue is one).
    Exit,
    /// `SIGKILL` itself, as the machine ends a starved candidate (jetsam).
    Kill,
    /// Check in with LaunchServices first (create `NSApplication`), then `_exit` as
    /// [`Self::Exit`] does: a successor LaunchServices has already answered with a pid that dies
    /// before its dial. LaunchServices answers a checked-in process with its pid during
    /// `+sharedApplication` itself, and says nothing more when it dies afterwards.
    ExitAfterCheckIn,
    /// [`Self::ExitAfterCheckIn`]'s check-in, then `SIGKILL` as [`Self::Kill`].
    KillAfterCheckIn,
}

impl DebugDeathBeforeDial {
    /// Does this death check in with LaunchServices before it dies?
    #[must_use]
    pub(crate) fn checks_in_first(self) -> bool {
        matches!(self, Self::ExitAfterCheckIn | Self::KillAfterCheckIn)
    }

    /// Does it end by `SIGKILL` (rather than `_exit`)?
    #[must_use]
    pub(crate) fn kills(self) -> bool {
        matches!(self, Self::Kill | Self::KillAfterCheckIn)
    }
}

/// The status the [`DebugDeathBeforeDial::Exit`] seam exits with:
/// `EX_SOFTWARE`, a crash's shape — never `seamless::EXIT_SUCCESSOR_PASSING`
/// (75), which a successor uses to say its refusal was a passing moment.
pub(crate) const DEBUG_DEATH_EXIT_CODE: i32 = 70;

/// A development seam (`aterm_types::dev_seam!`): a launched successor that dies before its dial,
/// on demand, in the shape of a containment-deny fatal in the warm prologue, a boot crash, or a
/// refused boot-apply re-exec. `main_entry` reads it on the launched lane only, just before the
/// dial, so the parent's side (`app_update_handoff::await_launched_dial`) can be driven on a
/// private instance. The launching process's environment merges into the successor's
/// (`app_launch_successor`'s module docs), so an instance started with it launches successors
/// that die; the instance itself, a cold launch, never reads it, and it stops at the hop to a
/// child shell (`aterm_types::domain::ENV_DENY_VARS`).
///
/// `kill` SIGKILLs; `checked-in` checks in with LaunchServices first and then exits;
/// `checked-in-kill` checks in and then SIGKILLs; any other engaged value (not unset, empty or
/// `0`) exits with [`DEBUG_DEATH_EXIT_CODE`]. A shipped binary reads `None`.
#[must_use]
pub(crate) fn debug_die_before_dial() -> Option<DebugDeathBeforeDial> {
    parse_debug_die_before_dial(aterm_types::dev_seam!("ATERM_DEBUG_DIE_BEFORE_DIAL").as_deref())
}

/// [`debug_die_before_dial`]'s reading of one value, pure.
#[must_use]
fn parse_debug_die_before_dial(value: Option<&std::ffi::OsStr>) -> Option<DebugDeathBeforeDial> {
    let value = value?.to_str()?;
    match value {
        "kill" => return Some(DebugDeathBeforeDial::Kill),
        "checked-in" => return Some(DebugDeathBeforeDial::ExitAfterCheckIn),
        "checked-in-kill" => return Some(DebugDeathBeforeDial::KillAfterCheckIn),
        _ => {}
    }
    aterm_types::control_socket::env_flag_engaged(Some(value)).then_some(DebugDeathBeforeDial::Exit)
}

/// The [`DebugDeathBeforeDial::checks_in_first`] half of the seam: create
/// this process's `NSApplication`, which checks it in with LaunchServices —
/// the moment LaunchServices answers the launch with this process's pid.
/// Main thread only (`+sharedApplication` is main-thread AppKit), and
/// `main_entry` calls it there, on the launched lane, just before it dies.
/// Windowed only: it takes the launch's proof, since in a headless process
/// the send would create the application object and its WindowServer
/// connection. `false` off the main thread or when AppKit answers nil.
pub(crate) fn debug_check_in_with_launch_services(_windowed: crate::platform::Windowed) -> bool {
    use aterm_objc::{class, sel};

    let Some(_main_thread) = crate::appkit::MainThread::new() else {
        return false;
    };
    // SAFETY: `+sharedApplication` is `-(id)`, the main-thread AppKit
    // singleton (`_main_thread` is the witness); it creates the application
    // object on first use and returns it, never an owned reference.
    let app =
        unsafe { crate::appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) };
    !app.is_null()
}

/// Dial the rendezvous named in this launch's handoff snapshot, present the claim, and take
/// delivery of every descriptor.
///
/// Consumes both names and [`ENV_GRANT_CAPS`] from `env` whether or not it succeeds. On success
/// it unlinks the socket: the parent `_exit`s inside `seamless::commit_and_exit` and runs no
/// destructor, so on that path this is what retires the node.
///
/// A grant naming any attempt but `expected_nonce` (`ATERM_SEAMLESS_NONCE`) is refused. `parent`
/// is the outgoing process `seamless::prearm_incoming_fds` attested; the claim goes only to a
/// listener the kernel says is that process ([`listener_may_receive_claim`]), and `None`
/// refuses. No process-environment access: callable from any thread.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_successor_warm_before_claim",
        action = "Dial",
        project = "aterm_gui::seamless::handoff_env_conformance::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_successor_warm_before_claim",
        action = "ClaimFail",
        project = "aterm_gui::seamless::handoff_env_conformance::project"
    )
)]
pub(crate) fn claim_incoming(
    env: &mut crate::handoff_env::HandoffEnv,
    expected_nonce: &str,
    deadlines: ClaimDeadlines,
    parent: Option<crate::seamless::AttestedParent>,
) -> Result<ClaimedHandoff, RendezvousError> {
    let chunks = offers_chunked_grant(env.take(ENV_GRANT_CAPS).as_deref());
    let (path, secret) = rendezvous_env(env.take(ENV_RENDEZVOUS), env.take(ENV_CLAIM))?;
    // Room for the grant is made before this is called, by `settle_incoming_descriptor_limit`.
    let claimed = dial_and_claim(&path, &secret, expected_nonce, deadlines, parent, chunks)
        .map_err(claim_side);
    if claimed.is_ok() {
        let _ = std::fs::remove_file(&path);
    }
    claimed
}

/// A dial's error as the successor says it: [`RendezvousError::Deadline`]'s "no successor
/// dialed" is the parent's sentence; from this side the same expiry is this successor's own
/// budget running out before the grant ([`RendezvousError::ClaimDeadline`]).
fn claim_side(error: RendezvousError) -> RendezvousError {
    match error {
        RendezvousError::Deadline => RendezvousError::ClaimDeadline,
        other => other,
    }
}

/// Whether the parent's [`ENV_GRANT_CAPS`] offers the chunked grant: its comma-separated list
/// names [`GRANT_CAPS_CHUNKS`]. Absent or anything else: no, and this successor claims `ATRZ1C`.
fn offers_chunked_grant(caps: Option<&std::ffi::OsStr>) -> bool {
    caps.and_then(std::ffi::OsStr::to_str)
        .is_some_and(|caps| caps.split(',').any(|cap| cap.trim() == GRANT_CAPS_CHUNKS))
}

/// Decide what a launch environment is, from the two values already taken out of it: a
/// dialable rendezvous, none, or half of one. Split out of [`claim_incoming`] so tests drive it
/// without mutating process-global names that other tests read.
fn rendezvous_env(
    path: Option<std::ffi::OsString>,
    secret: Option<std::ffi::OsString>,
) -> Result<(PathBuf, String), RendezvousError> {
    let (path, secret) = match (path, secret) {
        (Some(path), Some(secret)) => (path, secret),
        (None, None) => return Err(RendezvousError::NoRendezvous),
        // Half is not absence: whatever produced one name may have produced this process, so
        // the launch is refused, saying which half arrived.
        (Some(_), None) => {
            return Err(RendezvousError::HalfRendezvous {
                present: ENV_RENDEZVOUS,
                missing: ENV_CLAIM,
            });
        }
        (None, Some(_)) => {
            return Err(RendezvousError::HalfRendezvous {
                present: ENV_CLAIM,
                missing: ENV_RENDEZVOUS,
            });
        }
    };
    let secret = secret
        .into_string()
        .map_err(|_| RendezvousError::Env(ENV_CLAIM))?;
    if !aterm_codec::hex::is_hex(&secret, CLAIM_HEX_LEN) {
        return Err(RendezvousError::Env(ENV_CLAIM));
    }
    Ok((PathBuf::from(path), secret))
}

/// The two budgets of a dial. `dial` bounds the connect, the uid check and the claim write: the
/// parent holds the listener open and answers at once, or has given up and the connect fails at
/// once. `grant` bounds the wait for the descriptors: long, because the outgoing process parks
/// only at a quiet moment after the successor has dialled, holding the claim meanwhile; EOF ends
/// it at once either way.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClaimDeadlines {
    pub dial: Instant,
    pub grant: Instant,
}

/// The listener must be the attested parent before the claim secret leaves this process: the
/// environment named a path, and the parental attestation named a process. The kernel is asked
/// both halves about the listener this dial reached:
///
/// * its uid (`getpeereid`) is ours;
/// * its pid (`LOCAL_PEERPID` on the dialing end: XNU copies it from the listening socket at
///   `connect(2)`, before any `accept`, and no peer can assert it) is the attested parent's,
///   and that parent is still the process attested
///   ([`crate::seamless::AttestedParent::owns_live_listener`]: a recycled pid fails).
///
/// The uid alone would hand the claim to any same-uid process listening at the published path
/// (the birth record that attests the parent is copyable with `ps`). A refusal is fail-closed:
/// the successor exits before any window and the outgoing process keeps every session.
/// `NativeUpdateRendezvousClaim` (aterm-spec) is the model; `claim_presentation_conformance`
/// binds it.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "native_update_rendezvous_claim",
        action = "PresentClaim",
        project = "aterm_gui::handoff_rendezvous::claim_presentation_conformance::project"
    )
)]
fn listener_may_receive_claim(
    listener_uid: Option<u32>,
    own_uid: u32,
    listener_pid: Option<u32>,
    parent: Option<crate::seamless::AttestedParent>,
) -> Result<(), RendezvousError> {
    if listener_uid != Some(own_uid) {
        return Err(RendezvousError::Dial(
            "rendezvous listener is not owned by this uid; refusing to present the claim"
                .to_string(),
        ));
    }
    let Some(parent) = parent else {
        return Err(RendezvousError::Dial(
            "no attested outgoing process to present the claim to".to_string(),
        ));
    };
    if !listener_pid.is_some_and(|pid| parent.owns_live_listener(pid)) {
        return Err(RendezvousError::Dial(format!(
            "rendezvous listener (pid {}) is not the attested outgoing process; refusing to \
             present the claim",
            listener_pid.map_or_else(|| "unattested".to_string(), |pid| pid.to_string())
        )));
    }
    Ok(())
}

/// Dial `path`, claim with `secret` — under `ATRZ2C` when `chunks` (the parent
/// offered the chunked grant), `ATRZ1C` otherwise — and take delivery of the
/// grant: the one-message `ATRZ1G`, or, only after an `ATRZ2C` claim, the
/// chunked `ATRZ2G`. EVERY descriptor received is owned from the moment it
/// arrives, so any refusal below closes them all by dropping them.
fn dial_and_claim(
    path: &Path,
    secret: &str,
    expected_nonce: &str,
    deadlines: ClaimDeadlines,
    parent: Option<crate::seamless::AttestedParent>,
    chunks: bool,
) -> Result<ClaimedHandoff, RendezvousError> {
    let stream =
        CtlStream::connect(path).map_err(|error| RendezvousError::Dial(error.to_string()))?;
    // SAFETY: getuid has no preconditions.
    let own_uid = unsafe { libc::getuid() };
    listener_may_receive_claim(
        crate::control_auth::peer_uid(&stream),
        own_uid,
        fdpass::peer_pid(&stream).ok(),
        parent,
    )?;
    let remaining = remaining_io_budget(deadlines.dial)?;
    stream
        .set_read_timeout(Some(remaining))
        .and_then(|()| stream.set_write_timeout(Some(remaining)))
        .map_err(|error| RendezvousError::Dial(error.to_string()))?;
    (&stream)
        .write_all(&claim_frame_with(secret, chunks))
        .and_then(|()| (&stream).flush())
        .map_err(|error| RendezvousError::Dial(error.to_string()))?;

    let grant_failed = |error: std::io::Error| RendezvousError::Grant(error.to_string());
    // The header is read by the descriptor-bearing receive: `fdpass` delivers all of a message's
    // descriptors on the call that dequeues its first byte, so a short read costs bytes, never
    // descriptors. The buffer is the one-message header, also the chunked header's first eight
    // bytes; `max_fds` is the message ceiling because the exact count is in the body (checked
    // below; an over-count is `fdpass`'s fatal `InvalidData`). The grant deadline, not a socket
    // timeout, ends this long wait: a bare receive's expiry would read as a malformed grant
    // rather than as `ClaimDeadline`.
    let mut header = [0u8; GRANT_HEADER_LEN];
    let received = recv_fds_by_deadline(&stream, &mut header, deadlines.grant)?;
    let mut fds = received.fds;
    if received.bytes == 0 && fds.is_empty() {
        return Err(RendezvousError::Grant(
            "the parent closed the rendezvous".to_string(),
        ));
    }
    if received.bytes < GRANT_HEADER_LEN {
        read_frame_by_deadline(
            &stream,
            &mut header[received.bytes..],
            deadlines.grant,
            &grant_failed,
        )?;
    }
    // Which grant: the chunked one only after this process claimed it; an `ATRZ1C` claimant
    // refuses `ATRZ2G` as a wrong magic.
    let magic = &header[..GRANT_MAGIC.len()];
    let chunk_count = if magic == GRANT_MAGIC {
        None
    } else if chunks && magic == GRANT_CHUNKS_MAGIC {
        let mut count = [0u8; 1];
        read_frame_by_deadline(&stream, &mut count, deadlines.grant, &grant_failed)?;
        let count = usize::from(count[0]);
        if count == 0 || count > MAX_GRANT_CHUNKS {
            return Err(RendezvousError::Grant(format!(
                "a chunked grant of {count} messages"
            )));
        }
        Some(count)
    } else {
        return Err(RendezvousError::Grant("wrong magic".to_string()));
    };
    let body_len = usize::from(u16::from_be_bytes([
        header[GRANT_MAGIC.len()],
        header[GRANT_MAGIC.len() + 1],
    ]));
    if body_len > MAX_GRANT_BODY_BYTES {
        return Err(RendezvousError::Grant("oversized body".to_string()));
    }
    let mut body = vec![0u8; body_len];
    read_frame_by_deadline(&stream, &mut body, deadlines.grant, &grant_failed)?;
    let body =
        String::from_utf8(body).map_err(|_| RendezvousError::Grant("not UTF-8".to_string()))?;
    let GrantBody {
        sessions,
        parked_for,
    } = parse_grant_body(&body, expected_nonce)?;

    let expected = sessions.len() + RENDEZVOUS_CHANNEL_FDS;
    if let Some(count) = chunk_count {
        // The layout is computed, not believed: the body's session count fixes how many
        // messages there are and what each carries, every message must be exactly that under the
        // one grant deadline, and the first refusal drops (closes) everything received so far.
        if sessions.len() > MAX_CHUNKED_SESSIONS {
            return Err(RendezvousError::DescriptorCount {
                expected,
                received: fds.len(),
            });
        }
        let sizes = grant_chunk_sizes(sessions.len());
        if sizes.len() != count {
            return Err(RendezvousError::Grant(format!(
                "a grant of {} sessions in {count} messages, where its layout is {}",
                sessions.len(),
                sizes.len()
            )));
        }
        if fds.len() != sizes[0] {
            return Err(RendezvousError::DescriptorCount {
                expected,
                received: fds.len(),
            });
        }
        for (index, size) in sizes.iter().enumerate().skip(1) {
            let mut frame = [0u8; GRANT_CONTINUATION_LEN];
            let received = recv_fds_by_deadline(&stream, &mut frame, deadlines.grant)?;
            let arrived = received.fds.len();
            fds.extend(received.fds);
            if received.bytes == 0 && arrived == 0 {
                return Err(RendezvousError::Grant(format!(
                    "the parent closed the rendezvous before chunk {index} of {count}"
                )));
            }
            if received.bytes < GRANT_CONTINUATION_LEN {
                read_frame_by_deadline(
                    &stream,
                    &mut frame[received.bytes..],
                    deadlines.grant,
                    &grant_failed,
                )?;
            }
            if &frame[..GRANT_CONTINUATION_MAGIC.len()] != GRANT_CONTINUATION_MAGIC
                || usize::from(frame[GRANT_CONTINUATION_MAGIC.len()]) != index
            {
                return Err(RendezvousError::Grant(format!(
                    "chunk {index} of {count} is not the continuation its layout names"
                )));
            }
            if arrived != *size {
                return Err(RendezvousError::DescriptorCount {
                    expected,
                    received: fds.len(),
                });
            }
        }
    }
    if fds.len() != expected || expected > MAX_GRANT_FDS {
        return Err(RendezvousError::DescriptorCount {
            expected,
            received: fds.len(),
        });
    }
    // Nothing here may land on stdio. `fdpass` cannot produce such a number
    // while this process holds 0/1/2, but a descriptor that did would be
    // adopted, re-armed and eventually closed by the seamless intake — so it is
    // refused where the refusal is still free.
    if fds.iter().any(|fd| fd.as_raw_fd() < 3) {
        return Err(RendezvousError::Grant(
            "a received descriptor names stdio".to_string(),
        ));
    }
    let mut masters = fds;
    // Both pops are total: the count check above proved there are at least
    // `RENDEZVOUS_CHANNEL_FDS` more descriptors than the empty session list this
    // parser already rejected.
    let (Some(commit), Some(ready)) = (masters.pop(), masters.pop()) else {
        return Err(RendezvousError::DescriptorCount {
            expected,
            received: masters.len(),
        });
    };
    let fds_wire = sessions
        .iter()
        .zip(masters.iter())
        .map(|((local_id, pid), master)| format!("{local_id}={}:{pid}", master.as_raw_fd()))
        .collect::<Vec<_>>()
        .join(",");
    Ok(ClaimedHandoff {
        fds_wire,
        masters,
        ready,
        commit,
        parked_for,
    })
}

/// One descriptor-bearing receive of a fixed frame under one deadline: a receive that times out
/// or is interrupted is tried again with the budget re-derived, and only the deadline ends the
/// wait. Safe because an interrupted or timed-out `recvmsg` dequeued nothing, descriptors
/// included (`fdpass`).
fn recv_fds_by_deadline(
    stream: &CtlStream,
    buf: &mut [u8],
    deadline: Instant,
) -> Result<fdpass::Received, RendezvousError> {
    loop {
        arm_read_timeout(stream, remaining_io_budget(deadline)?)
            .map_err(|error| RendezvousError::Grant(error.to_string()))?;
        match fdpass::recv_with_fds(stream, buf, fdpass::MAX_FDS) {
            Ok(received) => return Ok(received),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(RendezvousError::Grant(error.to_string())),
        }
    }
}

/// Arm `stream`'s read timeout for the next receive. On a socket shut both ways (the peer sent
/// its last bytes and closed, as the outgoing process does once its grant is sent) Darwin's
/// `setsockopt(SO_RCVTIMEO)` fails `EINVAL`; such a socket cannot block a read (it returns what
/// is queued, then 0), so that refusal is ignored. Any other error is returned.
fn arm_read_timeout(stream: &CtlStream, timeout: Duration) -> std::io::Result<()> {
    match stream.set_read_timeout(Some(timeout)) {
        Err(error) if error.raw_os_error() == Some(libc::EINVAL) => Ok(()),
        other => other,
    }
}

/// Read exactly `buf.len()` bytes under one deadline over the whole frame. `set_read_timeout`
/// bounds a syscall and `read_exact` under it restarts that clock on every short read, so a peer
/// dribbling a byte before each expiry could stretch the 70-byte claim frame to about 70
/// budgets, before either gate runs. The remaining budget is re-derived before every read.
///
/// `wrap` names the side: the same expiry is an `Accept` refusal for the parent and a `Grant`
/// refusal for the successor; [`RendezvousError::Deadline`] comes from the budget on both.
fn read_frame_by_deadline(
    stream: &CtlStream,
    buf: &mut [u8],
    deadline: Instant,
    wrap: &dyn Fn(std::io::Error) -> RendezvousError,
) -> Result<(), RendezvousError> {
    let mut source = stream;
    let mut filled = 0usize;
    while filled < buf.len() {
        // The budget is re-derived from the deadline here, so the loop cannot hand the peer a
        // fresh full timeout for the next byte.
        let remaining = remaining_io_budget(deadline)?;
        arm_read_timeout(source, remaining).map_err(wrap)?;
        match source.read(&mut buf[filled..]) {
            Ok(0) => {
                return Err(wrap(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the peer closed the connection part-way through a frame",
                )));
            }
            Ok(read) => filled += read,
            // A timed-out read means the clamped budget elapsed, and an
            // interrupted one means a signal arrived. Neither is decided here:
            // both go back to the loop head, which is the single place that
            // consults the deadline.
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(wrap(error)),
        }
    }
    Ok(())
}

/// [`remaining_io_budget`] for the parent's grant, before its first `sendmsg`:
/// the deadline running out there is this process's own lateness after a dial,
/// never "no successor dialed" ([`RendezvousError::GrantDeadline`]).
fn grant_budget(deadline: Instant) -> Result<Duration, RendezvousError> {
    remaining_io_budget(deadline).map_err(|_| RendezvousError::GrantDeadline)
}

/// The socket timeout to spend on the next framed exchange, or the deadline
/// error when there is none left. Clamped away from zero because std refuses a
/// zero-duration timeout, and an expired deadline has to read as an expired
/// deadline rather than as `InvalidInput` from a socket option.
fn remaining_io_budget(deadline: Instant) -> Result<Duration, RendezvousError> {
    let now = io_now();
    if now >= deadline {
        return Err(RendezvousError::Deadline);
    }
    Ok(deadline.saturating_duration_since(now).max(MIN_IO_TIMEOUT))
}

#[cfg(test)]
thread_local! {
    static IO_CLOCK_FOR_TEST: std::cell::RefCell<Option<Box<dyn Fn() -> Instant>>> =
        const { std::cell::RefCell::new(None) };
    static HOLDS_FOR_TEST: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many dialers this thread held for a code check `trustd` did not answer ([`admit_code`]).
#[cfg(test)]
pub(crate) fn holds_for_test() -> usize {
    HOLDS_FOR_TEST.with(std::cell::Cell::get)
}

/// Set (or, with `None`, clear) the clock this thread's budgets are judged by ([`io_now`]).
#[cfg(test)]
pub(crate) fn set_io_clock_for_test(clock: Option<Box<dyn Fn() -> Instant>>) {
    IO_CLOCK_FOR_TEST.with(|slot| *slot.borrow_mut() = clock);
}

/// The instant a budget is judged at: the clock, or a test thread's ([`set_io_clock_for_test`]).
fn io_now() -> Instant {
    #[cfg(test)]
    if let Some(now) = IO_CLOCK_FOR_TEST.with(|slot| slot.borrow().as_ref().map(|clock| clock())) {
        return now;
    }
    Instant::now()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsFd as _, FromRawFd as _};

    const TEST_NONCE: &str = "0123456789abcdef0123456789abcdef";

    fn own_pid() -> i32 {
        i32::try_from(std::process::id()).expect("a pid fits pid_t")
    }

    /// This test process, attested the way a launched successor attests its
    /// outgoing parent. The in-process tests below bind the listener in this very
    /// process, so it IS the listener the dial must reach.
    fn own_parent() -> Option<crate::seamless::AttestedParent> {
        let parent = crate::seamless::AttestedParent::attest_live_for_test(own_pid());
        assert!(
            parent.is_some(),
            "this live process attests by its birth record"
        );
        parent
    }

    #[test]
    fn a_claim_frame_matches_only_its_own_secret() {
        let secret = "a".repeat(CLAIM_HEX_LEN);
        let frame = claim_frame(&secret);
        assert_eq!(frame.len(), CLAIM_FRAME_LEN);
        assert!(claim_frame_matches(&frame, &secret));

        let mut wrong = frame;
        let last = wrong.len() - 1;
        wrong[last] = b'b';
        assert!(
            !claim_frame_matches(&wrong, &secret),
            "one differing byte refuses"
        );

        let mut mismagicked = frame;
        mismagicked[0] = b'X';
        assert!(
            !claim_frame_matches(&mismagicked, &secret),
            "the frame magic is part of the claim"
        );

        assert!(
            !claim_frame_matches(&frame, &"a".repeat(CLAIM_HEX_LEN - 1)),
            "a shorter secret is not a prefix match"
        );
        // A secret of the wrong length cannot be smuggled through the bounded
        // copy that builds the frame: the tail stays zero, and zero is not hex.
        assert!(!claim_frame_matches(&claim_frame("abc"), "abc"));
    }

    #[test]
    fn a_grant_body_round_trips_in_transfer_order() {
        let held = std::io::stdin();
        let borrowed = held.as_fd();
        let sessions = vec![(7u64, 4242i32, borrowed), (3u64, 99i32, borrowed)];
        let body = encode_grant_body(TEST_NONCE, &sessions, None).expect("encodes");
        assert_eq!(body, format!("{TEST_NONCE}\n7:4242,3:99\n"));
        assert_eq!(
            parse_grant_body(&body, TEST_NONCE).expect("parses"),
            GrantBody {
                sessions: vec![(7, 4242), (3, 99)],
                parked_for: None,
            },
            "order is preserved, because order IS the addressing"
        );
    }

    /// The park stamp rides the grant: a parent that sampled 1.5 ms after parking
    /// says so on the body's last line, to the microsecond, and the
    /// successor reads it back beside the unchanged session list. A body without it —
    /// a parent from before the stamp — still parses, with no stamp (the negative
    /// control), and a stamp that is not a count of microseconds refuses the grant
    /// like any other malformed line.
    #[test]
    fn a_grant_carries_how_long_before_it_the_parent_parked() {
        let held = std::io::stdin();
        let sessions = vec![(7u64, 4242i32, held.as_fd())];
        let parked = Duration::from_micros(1_500);
        let body = encode_grant_body(TEST_NONCE, &sessions, Some(parked)).expect("encodes");
        assert_eq!(body, format!("{TEST_NONCE}\n7:4242\npark_us=1500\n"));
        assert_eq!(
            parse_grant_body(&body, TEST_NONCE),
            Ok(GrantBody {
                sessions: vec![(7, 4242)],
                parked_for: Some(parked),
            })
        );
        let older = format!("{TEST_NONCE}\n7:4242\n");
        assert_eq!(
            parse_grant_body(&older, TEST_NONCE).map(|body| body.parked_for),
            Ok(None),
            "an older parent's grant has no stamp"
        );
        for malformed in [
            "park_us=\n",
            "park_us=-1\n",
            "park_us=1.5\n",
            "park=1500\n",
            "\n",
        ] {
            let body = format!("{TEST_NONCE}\n7:4242\n{malformed}");
            assert!(parse_grant_body(&body, TEST_NONCE).is_err(), "{body:?}");
        }
        // Every representable whole-microsecond boundary still round-trips;
        // a fractional microsecond is truncated, including just below overflow.
        for parked in [
            Duration::ZERO,
            Duration::from_nanos(999),
            Duration::from_micros(u64::MAX),
            Duration::from_micros(u64::MAX) + Duration::from_nanos(999),
        ] {
            let body = encode_grant_body(TEST_NONCE, &sessions, Some(parked))
                .expect("a stamp in the reader's domain encodes");
            let parsed = parse_grant_body(&body, TEST_NONCE).expect("the stamp parses");
            assert_eq!(parsed.sessions, vec![(7, 4242)]);
            assert_eq!(
                parsed.parked_for,
                Some(Duration::from_micros(
                    u64::try_from(parked.as_micros()).expect("the ordinary boundary fits"),
                )),
            );
        }
        // Both rejected values are otherwise small, valid grant bodies; compute
        // both refusals before the one final regression marker.
        let refused = [
            Duration::from_micros(u64::MAX) + Duration::from_micros(1),
            Duration::MAX,
        ]
        .map(|parked| encode_grant_body(TEST_NONCE, &sessions, Some(parked)).is_none());
        assert!(
            refused.into_iter().all(|refused| refused),
            "GRANT_PARK_STAMP_ENCODER_EXCEEDS_READER_DOMAIN",
        );
    }

    #[test]
    fn a_malformed_grant_body_is_refused_rather_than_partially_believed() {
        assert_eq!(
            parse_grant_body(&format!("{TEST_NONCE}\n1:2\n"), &"f".repeat(32)),
            Err(RendezvousError::NonceMismatch),
            "a grant for another attempt"
        );
        for body in [
            String::new(),
            TEST_NONCE.to_string(),
            format!("{TEST_NONCE}\n"),
            format!("{TEST_NONCE}\n1:2"),
            format!("{TEST_NONCE}\n1:2,\n"),
            format!("{TEST_NONCE}\nx:2\n"),
            format!("{TEST_NONCE}\n1:y\n"),
            format!("{TEST_NONCE}\n1:0\n"),
            format!("{TEST_NONCE}\n1:-4\n"),
            format!("{TEST_NONCE}\n1:2,1:3\n"),
        ] {
            assert!(
                parse_grant_body(&body, TEST_NONCE).is_err(),
                "{body:?} must not parse"
            );
        }
    }

    #[test]
    fn a_grant_header_frames_its_body_exactly() {
        let header = grant_header(11).expect("fits");
        assert_eq!(&header[..GRANT_MAGIC.len()], GRANT_MAGIC);
        assert_eq!(
            u16::from_be_bytes([header[GRANT_MAGIC.len()], header[GRANT_MAGIC.len() + 1]]),
            11
        );
        assert!(
            grant_header(MAX_GRANT_BODY_BYTES + 1).is_none(),
            "an oversized body is refused, never truncated"
        );
    }

    /// The nonce is what binds a grant to one attempt, so a body whose nonce is
    /// not a real attempt nonce must not be encodable in the first place.
    #[test]
    fn only_a_well_formed_nonce_can_be_granted() {
        let held = std::io::stdin();
        let sessions = vec![(1u64, 2i32, held.as_fd())];
        assert!(encode_grant_body("short", &sessions, None).is_none());
        assert!(encode_grant_body(&"z".repeat(32), &sessions, None).is_none());
        assert!(encode_grant_body(TEST_NONCE, &sessions, None).is_some());
    }

    /// The device term must DISTINGUISH masters (the device, not the
    /// filesystem it sits on). That it survives the transfer is asserted by
    /// [`a_grant_whose_connection_closes_at_once_is_still_claimed`], over a
    /// real `SCM_RIGHTS` transfer.
    #[test]
    fn the_device_term_distinguishes_masters() {
        let ptys = (0..3).map(|_| open_pty()).collect::<Vec<_>>();
        let terms = ptys
            .iter()
            .map(|(master, _)| pty_device_term(*master).expect("a master answers fstat"))
            .collect::<Vec<_>>();
        let mut distinct = terms.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            terms.len(),
            "three simultaneously-open masters must have three different device numbers, or \
             the term could not exclude a substitution"
        );
        for (master, slave) in ptys {
            aterm_pty::close_fd(master);
            aterm_pty::close_fd(slave);
        }
    }

    #[test]
    fn an_expired_deadline_is_a_deadline_and_not_a_zero_timeout() {
        let expired = Instant::now() - Duration::from_secs(1);
        assert_eq!(remaining_io_budget(expired), Err(RendezvousError::Deadline));
        let live = Instant::now() + Duration::from_secs(5);
        assert!(remaining_io_budget(live).expect("live budget") >= MIN_IO_TIMEOUT);
    }

    /// The code check judges the token the gate read, and only if it names the
    /// attested dialer: the token read from the connection, naming the attested
    /// pid, is checked and passes. Negative controls: the same token against
    /// another attested pid is not checked at all, and a token for a later
    /// image at the same pid (its pid version moved on) fails the lookup, where
    /// a fresh read at a recheck would have judged whatever ran there then.
    /// Neither is a verdict, and neither is a pass. Each check goes through the
    /// gate's own hold, with the one-minute hang detector as its deadline.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn the_code_check_judges_the_token_read_at_the_claim_and_only_for_its_pid() {
        let (a, _b) = CtlStream::pair().expect("pair");
        let token = aterm_uds::peer::peer_audit_token(&a).expect("the kernel names the peer");
        let code = DialerCode::this_process();
        let judged = |attested: i32, token: aterm_uds::peer::AuditToken| match admit_code(
            &a,
            attested,
            Some(token),
            &code,
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            &|| false,
            &|_| {},
        ) {
            Ok(()) => Ok(()),
            Err(Gated::Refused(RendezvousError::PeerCode { refusal, .. })) => Err(refusal),
            Err(Gated::Refused(other)) => panic!("refused for no code check: {other}"),
            Err(Gated::WaitEnded { held, .. }) => trustd_silent(held),
        };
        if let Err(refusal) = judged(own_pid(), token) {
            panic!("the token names the attested dialer, which runs this code: {refusal:?}")
        }
        let elsewhere = judged(own_pid() + 1, token);
        assert!(
            matches!(&elsewhere, Err(CodeRefusal::Unchecked(why)) if why.contains("not the attested dialer")),
            "a token naming another pid was checked for the attested dialer: {elsewhere:?}"
        );
        let mut later = token;
        later.0[7] = later.0[7].wrapping_add(1);
        let stale = judged(own_pid(), later);
        assert!(
            matches!(stale, Err(CodeRefusal::Unchecked(_))),
            "a token for another image at the pid was judged as the live one: {stale:?}"
        );
    }

    /// A check the gate let through, answered only after its in-line wait, is
    /// the hold's answer: the dialer is held, that same check is waited for past
    /// two recheck intervals, and the connection is admitted on its answer with
    /// no second check asked, so a slow `trustd` is a delay, never a refusal. A
    /// scripted check stands in for `trustd` through the process's one `trustd`
    /// gate (`aterm_uds::deadline::TRUST`), so this runs alone in a child, and
    /// it answers in the hold's own slices, never after a sleep.
    ///
    /// Negative control: each check cut at its in-line wait, called unanswered
    /// and asked again through the same seam asks again once `trustd` has
    /// answered the first check, and has not admitted the dialer by then.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_check_answered_after_its_in_line_wait_is_waited_for_and_never_asked_again() {
        use aterm_keeper::identity::{CheckError, PeerCheck};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex, PoisonError, mpsc};

        // A check's in-line wait and its suspect age, as the keeper's
        // `PEER_CHECK_HANG` is both: short, so the hold starts at once.
        const IN_LINE: Duration = Duration::from_millis(50);

        if !crate::test_alone::alone() {
            return;
        }
        // Held slices before `trustd` answers: two recheck intervals' worth, so a
        // hold that asks again has asked by then.
        let answer_at =
            2 * usize::try_from(CODE_RECHECK_INTERVAL.as_nanos() / ACCEPT_POLL_SLICE.as_nanos())
                .expect("a slice count");
        // One dialer through the code gate, `trustd` answering the first check
        // `answer_at` held slices in and no other. `cut` is the old shape, which
        // ends once it has asked again after that answer (a re-ask that must
        // happen, so the hang detector bounds it, never a count of slices); the
        // shipping one ends only on the gate's answer or the hang detector. The
        // gate's end, then how many checks were asked, how many `trustd`
        // answered, the held slices, and the dialers turned away.
        let through_the_hold = |cut: bool| {
            let (a, _b) = CtlStream::pair().expect("pair");
            let token = aterm_uds::peer::peer_audit_token(&a).expect("the kernel names the peer");
            let (trustd_answers, unanswered) = mpsc::channel::<()>();
            let unanswered = Arc::new(Mutex::new(unanswered));
            let asked = Arc::new(AtomicUsize::new(0));
            let answered = Arc::new(AtomicUsize::new(0));
            let code = DialerCode::scripted(IN_LINE, {
                let (asked, answered) = (Arc::clone(&asked), Arc::clone(&answered));
                move || {
                    let again = asked.fetch_add(1, Ordering::SeqCst) > 0;
                    assert!(
                        cut || !again,
                        "asked again while the first check was still waited for"
                    );
                    let (unanswered, answered) = (Arc::clone(&unanswered), Arc::clone(&answered));
                    let check = PeerCheck::start(IN_LINE, move || {
                        let heard = unanswered
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .recv();
                        if heard.is_ok() {
                            answered.fetch_add(1, Ordering::SeqCst);
                        }
                        Ok(())
                    });
                    if !cut {
                        return check;
                    }
                    // The old shape: cut at its in-line wait and called unanswered.
                    // Overdue then, the cut check makes the gate refuse the checks
                    // after it at once, so the answer comes back settled, as the
                    // old one did, and the hold asks again every recheck interval.
                    let answer = check.wait(IN_LINE).unwrap_or_else(|abandoned| {
                        Err(CheckError::Unanswered(abandoned.unanswered()))
                    });
                    PeerCheck::start(IN_LINE, move || answer)
                }
            });
            let slices = std::cell::Cell::new(0usize);
            // How many checks were asked when `trustd`'s answer was heard.
            let asked_when_heard = std::cell::Cell::new(None::<usize>);
            // Every abort the gate asks is one held slice.
            let abort = || {
                let slice = slices.get() + 1;
                slices.set(slice);
                if slice == answer_at {
                    trustd_answers
                        .send(())
                        .expect("the first check waits for trustd");
                }
                if asked_when_heard.get().is_none() && answered.load(Ordering::SeqCst) > 0 {
                    asked_when_heard.set(Some(asked.load(Ordering::SeqCst)));
                }
                cut && asked_when_heard
                    .get()
                    .is_some_and(|then| asked.load(Ordering::SeqCst) > then)
            };
            let turned_away = std::cell::RefCell::new(Vec::new());
            let end = admit_code(
                &a,
                own_pid(),
                Some(token),
                &code,
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                &abort,
                &|dialer| turned_away.borrow_mut().push(dialer.clone()),
            );
            (
                end,
                asked.load(Ordering::SeqCst),
                answered.load(Ordering::SeqCst),
                slices.get(),
                turned_away.into_inner(),
            )
        };

        let (end, asked, answered, slices, turned_away) = through_the_hold(false);
        assert!(
            matches!(end, Ok(())),
            "a check trustd answered within the wait did not admit its dialer: {end:?}"
        );
        assert_eq!(asked, 1, "the check the gate let through was asked again");
        assert_eq!(answered, 1, "admitted on trustd's answer to that check");
        assert!(
            slices >= answer_at,
            "admitted only once trustd answered, {answer_at} held slices in: {slices}"
        );
        assert!(turned_away.is_empty(), "turned away: {turned_away:?}");

        // Negative control: the old shape, through the same seam.
        let (end, asked, answered, _, turned_away) = through_the_hold(true);
        assert!(
            matches!(
                end,
                Err(Gated::WaitEnded {
                    end: RendezvousError::Cancelled,
                    ..
                })
            ),
            "the shape that asks again admitted its dialer: {end:?}"
        );
        assert!(asked > 1, "the old shape asks again: {asked}");
        assert_eq!(
            answered, 1,
            "trustd answered the first check, and nobody heard it"
        );
        assert!(turned_away.is_empty(), "turned away: {turned_away:?}");

        // Every stand-in the old shape abandoned hears its channel close and returns.
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the trustd gate's helpers return",
            || (aterm_uds::deadline::TRUST.running() == 0).then_some(()),
        );
    }

    /// A dialer that claims — under `ATRZ2C` when `chunks` — and then only
    /// counts: every descriptor any frame brings it until the parent's close,
    /// one message or chunked, so a test reads what really reached the other
    /// end rather than whether a well-formed grant did.
    fn dial_and_count(
        path: PathBuf,
        claim: String,
        chunks: bool,
    ) -> std::thread::JoinHandle<usize> {
        std::thread::spawn(move || {
            let stream = CtlStream::connect(&path).expect("dial the rendezvous");
            (&stream)
                .write_all(&claim_frame_with(&claim, chunks))
                .expect("present the claim");
            // Darwin refuses the option (`EINVAL`) on a connection the other end
            // has already shut; the reads below then answer end of file at once.
            let _ = stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
            let mut received = 0;
            let mut buf = [0u8; 4096];
            while let Ok(got) =
                aterm_uds::fdpass::recv_with_fds(&stream, &mut buf, aterm_uds::fdpass::MAX_FDS)
            {
                if got.bytes == 0 && got.fds.is_empty() {
                    break;
                }
                received += got.fds.len();
            }
            received
        })
    }

    /// The chunked grant leaves only while the kernel still names the process
    /// the gate checked: the token the gate checked is replaced by one for a
    /// later image at the same pid, and `transfer` sends nothing
    /// (`TransferNotSent`, naming the change), so the dialer receives no
    /// descriptor before the end of file.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_grant_is_not_sent_when_the_dialers_token_is_not_the_one_checked() {
        let sessions = 100;
        let rendezvous = bind_for_test();
        let (reads, writes) = pipes_for_test(sessions);
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let dialer = dial_and_count(
            rendezvous.path().to_path_buf(),
            rendezvous.claim().to_string(),
            true,
        );
        let mut peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                &|| false,
            ),
            "the dialer presents the claim and is this very process",
        );
        assert!(peer.chunks, "the dialer claimed the chunked grant");
        let mut later = peer.token.expect("the gate read the dialer's token");
        assert_eq!(
            later.pid(),
            std::process::id(),
            "the token the gate kept names the attested dialer"
        );
        later.0[7] = later.0[7].wrapping_add(1);
        peer.token = Some(later);
        let granted = writes
            .iter()
            .enumerate()
            .map(|(id, write)| (id as u64, 5000 + id as i32, write.as_fd()))
            .collect::<Vec<_>>();
        assert_eq!(
            grant_shape(true, sessions),
            Ok(GrantShape::Chunked),
            "PRECONDITION: {sessions} sessions take the chunked grant"
        );
        let refused = peer.transfer(
            TEST_NONCE,
            &granted,
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            None,
        );
        assert!(
            matches!(&refused, Err(RendezvousError::TransferNotSent(why)) if why.contains("audit token changed")),
            "a chunked grant left for a token other than the one checked: {refused:?}"
        );
        drop((peer, rendezvous));
        assert_eq!(
            dialer.join().expect("dialer thread"),
            0,
            "the dialer received a descriptor the gate refused to send"
        );
        drop((reads, writes, ready_rd, ready_wr, commit_rd, commit_wr));
    }

    /// The exec stand-in's role, in its own environment: the rendezvous it
    /// dials, the claim it presents, the file whose appearance tells it to
    /// replace its image (absent with [`EXEC_DIALER_STAYS`]), and the file its
    /// new image creates once it runs.
    #[cfg(target_vendor = "apple")]
    const EXEC_DIALER_PATH: &str = "ATERM_TEST_EXEC_DIALER_RENDEZVOUS";
    #[cfg(target_vendor = "apple")]
    const EXEC_DIALER_CLAIM: &str = "ATERM_TEST_EXEC_DIALER_CLAIM";
    #[cfg(target_vendor = "apple")]
    const EXEC_DIALER_GO: &str = "ATERM_TEST_EXEC_DIALER_GO";
    #[cfg(target_vendor = "apple")]
    const EXEC_DIALER_EXECD: &str = "ATERM_TEST_EXEC_DIALER_EXECD";
    /// Set: the stand-in waits for the grant in the image that claimed (the
    /// positive control), never replacing it.
    #[cfg(target_vendor = "apple")]
    const EXEC_DIALER_STAYS: &str = "ATERM_TEST_EXEC_DIALER_STAYS";
    /// Set only in the NEW image: the claimed connection's descriptor number,
    /// inherited across the `exec`.
    #[cfg(target_vendor = "apple")]
    const EXEC_DIALER_FD: &str = "ATERM_TEST_EXEC_DIALER_FD";
    /// What the stand-in prints, with how many descriptors reached it.
    #[cfg(target_vendor = "apple")]
    const EXEC_DIALER_RECEIVED: &str = "EXEC DIALER RECEIVED";

    /// Read `stream` to its end of file and count the descriptors that came.
    #[cfg(target_vendor = "apple")]
    fn count_until_closed(stream: &CtlStream) -> usize {
        let _ = stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
        let mut received = 0;
        let mut buf = [0u8; 4096];
        while let Ok(got) =
            aterm_uds::fdpass::recv_with_fds(stream, &mut buf, aterm_uds::fdpass::MAX_FDS)
        {
            if got.bytes == 0 && got.fds.is_empty() {
                break;
            }
            received += got.fds.len();
        }
        received
    }

    /// Stand-in: a successor-shaped dialer of this test binary's code. It dials, claims and waits
    /// for the parent's go; then, unless told to stay, it `exec`s this binary again with the
    /// claimed connection kept open, and the new image counts what reaches it. A no-op in the
    /// ordinary run.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn exec_dialer_stand_in() {
        use std::os::unix::process::CommandExt as _;
        if std::env::var_os(EXEC_DIALER_FD).is_some() {
            // The image before this one handed over exactly this descriptor and exec'd. Checked
            // (`test_alone::handed`): a number holding nothing, or something opened since, would
            // read "0 DESCRIPTORS", the count expected of an image the gate refused.
            let stream = CtlStream::from(crate::test_alone::handed(EXEC_DIALER_FD));
            let execd = std::env::var_os(EXEC_DIALER_EXECD).expect("the marker's path");
            std::fs::write(execd, b"").expect("say the new image runs");
            println!(
                "{EXEC_DIALER_RECEIVED} {} DESCRIPTORS",
                count_until_closed(&stream)
            );
            return;
        }
        let (Some(path), Ok(claim), Some(go)) = (
            std::env::var_os(EXEC_DIALER_PATH),
            std::env::var(EXEC_DIALER_CLAIM),
            std::env::var_os(EXEC_DIALER_GO),
        ) else {
            return;
        };
        let stream = dial_for_test(Path::new(&path), &claim).expect("dial and claim");
        let go = PathBuf::from(go);
        let told = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(5),
            || go.exists().then_some(()),
        );
        assert!(told.is_some(), "the parent never said go");
        if std::env::var_os(EXEC_DIALER_STAYS).is_some() {
            println!(
                "{EXEC_DIALER_RECEIVED} {} DESCRIPTORS",
                count_until_closed(&stream)
            );
            return;
        }
        let fd = stream.into_raw_fd();
        let mut image = std::process::Command::new(std::env::current_exe().expect("this binary"));
        image.args(std::env::args_os().skip(1));
        // The claimed connection crosses the exec at its own number
        // (`test_alone::hand` clears its close-on-exec flag right before the
        // exec), named with what it is for the new image to check.
        crate::test_alone::hand(&mut image, EXEC_DIALER_FD, fd);
        let error = image.exec();
        panic!("exec the new image: {error}");
    }

    /// A grant is not sent once the dialer's image changed: a stand-in running this binary's code
    /// claims and passes the gate, then `exec`s at the same pid with the connection kept open;
    /// the grant is refused (`TransferNotSent`, "audit token changed") and the new image receives
    /// nothing. This measures that `LOCAL_PEERTOKEN` is answered live and an `exec` moves its pid
    /// version. Positive control: the stand-in told to stay is granted every descriptor.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_grant_is_not_sent_once_the_dialers_image_changed() {
        for stays in [true, false] {
            let rendezvous = bind_for_test();
            let scratch = aterm_tempfile::TempDir::new().expect("a scratch dir");
            let (go, execd) = (scratch.path().join("go"), scratch.path().join("execd"));
            let mut stand_in =
                crate::test_alone::StandIn::helper(module_path!(), "exec_dialer_stand_in");
            stand_in
                .command()
                .env(EXEC_DIALER_PATH, rendezvous.path())
                .env(EXEC_DIALER_CLAIM, rendezvous.claim())
                .env(EXEC_DIALER_GO, &go)
                .env(EXEC_DIALER_EXECD, &execd);
            if stays {
                stand_in.command().env(EXEC_DIALER_STAYS, "1");
            }
            let mut stand_ins = [stand_in.spawn()];
            let pid = i32::try_from(stand_ins[0].id()).expect("a pid fits pid_t");
            let peer = admitted(
                rendezvous.accept_claim(
                    Some(pid),
                    &crate::handoff_rendezvous::DialerCode::this_process(),
                    Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                    &|| false,
                ),
                "the stand-in runs this binary's code and is the attested dialer",
            );
            assert_eq!(peer.pid(), pid, "LOCAL_PEERPID names the stand-in");
            std::fs::write(&go, b"").expect("say go");
            if !stays {
                crate::test_alone::until_or_ended(
                    "the stand-in's new image runs",
                    &mut stand_ins,
                    || execd.exists(),
                );
            }
            let (reads, writes) = pipes_for_test(1);
            let (ready_rd, ready_wr) = pipe_for_test();
            let (commit_rd, commit_wr) = pipe_for_test();
            let granted = peer.transfer(
                TEST_NONCE,
                &[(11, 4242, writes[0].as_fd())],
                ready_wr.as_fd(),
                commit_rd.as_fd(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                None,
            );
            if stays {
                granted.expect("the dialer the gate checked, unchanged, is granted");
            } else {
                assert!(
                    matches!(&granted, Err(RendezvousError::TransferNotSent(why)) if why.contains("audit token changed")),
                    "a grant left for an image the gate never checked: {granted:?}"
                );
            }
            drop((peer, rendezvous));
            let [stand_in] = stand_ins;
            let ran = stand_in.passed();
            let received = ran
                .stdout
                .lines()
                .find_map(|line| line.split(EXEC_DIALER_RECEIVED).nth(1))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|count| count.parse::<usize>().ok())
                .unwrap_or_else(|| {
                    panic!("the stand-in never said what it received:\n{}", ran.stdout)
                });
            assert_eq!(
                received,
                if stays { 1 + RENDEZVOUS_CHANNEL_FDS } else { 0 },
                "stays={stays}: what reached the dialer"
            );
            drop((reads, writes, ready_rd, ready_wr, commit_rd, commit_wr));
        }
    }

    /// The parent hangs up right after the grant, and the claim still lands:
    /// the outgoing process drops the connection the instant `transfer` returns
    /// while the successor may still be between the header's receive and the
    /// body's read. Darwin answers `setsockopt(SO_RCVTIMEO)` on a socket whose
    /// both directions are shut with `EINVAL`, and a fully shut socket cannot
    /// block a read of bytes and descriptors already queued, so the timeout is
    /// not needed to read them. Repeated because the window is a race.
    ///
    /// Also asserts that the successor holds the same open file description at a
    /// different number, which is why the proof term is the PTY's device and not
    /// the fd.
    #[test]
    fn a_grant_whose_connection_closes_at_once_is_still_claimed() {
        for _ in 0..20 {
            let rendezvous = bind_for_test();
            let (master, slave) = open_pty();
            let (ready_rd, ready_wr) = pipe_for_test();
            let (commit_rd, commit_wr) = pipe_for_test();
            let claim = rendezvous.claim().to_string();
            let path = rendezvous.path().to_path_buf();
            let dialer = std::thread::spawn(move || {
                dial_and_claim(
                    &path,
                    &claim,
                    TEST_NONCE,
                    deadlines_in(120),
                    own_parent(),
                    false,
                )
            });
            let peer = admitted(
                rendezvous.accept_claim(
                    Some(own_pid()),
                    &crate::handoff_rendezvous::DialerCode::this_process(),
                    Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                    &|| false,
                ),
                "the dialer presents the claim",
            );
            // SAFETY: a descriptor this test owns for the length of the call.
            let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
            peer.transfer(
                TEST_NONCE,
                &[(11, 4242, borrowed)],
                ready_wr.as_fd(),
                commit_rd.as_fd(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                None,
            )
            .expect("transfer");
            // The outgoing process's order: the connection goes the moment
            // the grant is sent.
            drop(peer);
            drop(rendezvous);
            let claimed = dialer
                .join()
                .expect("dialer thread")
                .expect("a grant already queued is claimed though the parent hung up");
            assert_eq!(claimed.session_count(), 1);
            let received = claimed.masters[0].as_raw_fd();
            assert_eq!(
                claimed.fds_wire,
                format!("11={received}:4242"),
                "the wire names the descriptor at the number THIS process received it at"
            );
            assert_ne!(received, master, "SCM_RIGHTS installs a different number");
            assert_eq!(
                pty_device_term(received),
                pty_device_term(master),
                "the device term survives the transfer"
            );
            drop(claimed);
            drop((ready_rd, ready_wr, commit_rd, commit_wr));
            aterm_pty::close_fd(master);
            aterm_pty::close_fd(slave);
        }
    }

    /// The launched lane's shape across two real processes: the production code
    /// at both ends, with the successor a separate process — not a thread of the
    /// listener, and not a listener forked for the test. This process is the
    /// outgoing parent: it binds with [`Rendezvous::bind`], accepts with
    /// [`Rendezvous::accept_claim`] and grants with `transfer`. The successor is
    /// this test binary re-run as a child that attests this process the way a
    /// LaunchServices successor does (no live parent link, so by its birth
    /// record) and dials with [`dial_and_claim`] — so its identity gate reads
    /// `LOCAL_PEERPID` against a listener that the real bind created in another
    /// process. A kernel that named anything but the binding process there would
    /// refuse the claim and fail this test before any descriptor moved. What it
    /// does not reach is LaunchServices itself and the GUI boot around the dial.
    #[test]
    fn a_successor_process_claims_from_the_real_outgoing_listener() {
        const CHILD: &str = "RZ_CROSS_PROCESS_SUCCESSOR";
        if let Some(spec) = std::env::var_os(CHILD) {
            // The successor: `<path>\n<claim>\n<parent pid>\n<device term>`.
            let spec = spec.into_string().expect("the child spec is UTF-8");
            let mut fields = spec.split('\n');
            let mut field = || fields.next().expect("a complete child spec");
            let (path, claim) = (PathBuf::from(field()), field().to_string());
            let parent_pid: libc::pid_t = field().parse().expect("the parent pid");
            let term: i32 = field().parse().expect("the master's device term");
            let parent = crate::seamless::AttestedParent::attest_live_for_test(parent_pid);
            assert!(
                parent.is_some(),
                "the outgoing process attests by its birth record"
            );
            let claimed =
                dial_and_claim(&path, &claim, TEST_NONCE, deadlines_in(120), parent, false)
                    .expect("the successor's identity gate admits the real outgoing listener");
            assert_eq!(claimed.session_count(), 1);
            assert_eq!(
                pty_device_term(claimed.masters[0].as_raw_fd()),
                Some(term),
                "the granted master is the outgoing process's PTY"
            );
            return;
        }

        let rendezvous = bind_for_test();
        let (master, slave) = open_pty();
        let term = pty_device_term(master).expect("a master answers fstat");
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let spec = format!(
            "{}\n{}\n{}\n{term}",
            rendezvous.path().display(),
            rendezvous.claim(),
            own_pid()
        );
        // The successor: this very test as a stand-in, which must run exactly
        // this test and pass it.
        let mut child = crate::test_alone::StandIn::this_test();
        child.command().env(CHILD, spec);
        let child = std::cell::RefCell::new(child.spawn());
        let successor = i32::try_from(child.borrow().id()).expect("a pid fits pid_t");

        // A hang detector for a process start, cut short by the successor's own end. A successor
        // held for `trustd` is the machine's, named before the child's run is.
        let turned_away = std::cell::RefCell::new(Vec::new());
        let claimed = rendezvous.accept_claim_observed(
            Some(successor),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            &|| child.borrow_mut().exited(),
            &|dialer| turned_away.borrow_mut().push(dialer.clone()),
        );
        let peer = match claimed {
            Ok(peer) => peer,
            Err(error) => {
                let turned_away = turned_away.into_inner();
                if held_for_trustd(&error)
                    || turned_away
                        .iter()
                        .any(|dialer| matches!(dialer.refusal, CodeRefusal::TrustdSilent(_)))
                {
                    trustd_silent(format_args!("{error}: {turned_away:?}"));
                }
                child.into_inner().passed();
                panic!("the successor process passed without presenting its claim: {error}");
            }
        };
        assert_eq!(
            peer.pid(),
            successor,
            "LOCAL_PEERPID on accept names the successor"
        );
        // SAFETY: a descriptor this test owns for the length of the call.
        let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
        peer.transfer(
            TEST_NONCE,
            &[(11, 4242, borrowed)],
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            None,
        )
        .expect("transfer");

        child.into_inner().passed();
        drop((ready_rd, ready_wr, commit_rd, commit_wr));
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    /// Which side of the `sendmsg` a transfer failure fell on: a peer closed before the transfer
    /// makes the descriptor-carrying `sendmsg` fail (`TransferNotSent`); a peer that dequeues the
    /// header and then closes yields `Ok` or `TransferPartial`, never `TransferNotSent`. The
    /// second half is a kernel race, so it is pinned as the invariant.
    #[test]
    fn a_transfer_failure_says_whether_the_descriptors_left() {
        let rendezvous = bind_for_test();
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let (fake_master_rd, _fake_master_wr) = pipe_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();

        // (1) Closed before the send: the claim is presented and accepted, then
        //     the socket is gone before the parent transfers anything.
        let (close_tx, close_rx) = std::sync::mpsc::channel::<()>();
        let (closed_tx, closed_rx) = std::sync::mpsc::channel::<()>();
        let dialer = std::thread::spawn(move || {
            let stream = CtlStream::connect(&path).expect("dial");
            (&stream)
                .write_all(&claim_frame(&claim))
                .and_then(|()| (&stream).flush())
                .expect("claim");
            close_rx.recv().expect("told to close");
            drop(stream);
            closed_tx.send(()).expect("signal");
        });
        let peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                &|| false,
            ),
            "the claim was presented",
        );
        close_tx.send(()).expect("tell the dialer to close");
        closed_rx.recv().expect("the dialer closed");
        dialer.join().expect("dialer thread");
        // `closed_rx` proves the dialer ran `drop`, not that its socket is gone:
        // a child another test is forking holds a copy until it execs, and while
        // it does the send below is buffered, not refused. Only the hang-up the
        // parent itself observes says every copy is closed.
        wait_for_hangup(|| peer.poll_hangup());
        let borrowed = fake_master_rd.as_fd();
        let error = peer
            .transfer(
                TEST_NONCE,
                &[(11, 4242, borrowed)],
                ready_wr.as_fd(),
                commit_rd.as_fd(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                None,
            )
            .expect_err("a closed peer cannot take the descriptors");
        assert!(
            matches!(error, RendezvousError::TransferNotSent(_)),
            "the sendmsg itself failed, so no descriptor left: {error}"
        );
        drop(peer);
        drop(rendezvous);

        // (2) Closed after the header: the peer dequeues the descriptors, then
        //     closes without reading the body.
        let rendezvous = bind_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        let dialer = std::thread::spawn(move || {
            let stream = CtlStream::connect(&path).expect("dial");
            (&stream)
                .write_all(&claim_frame(&claim))
                .and_then(|()| (&stream).flush())
                .expect("claim");
            let mut header = [0u8; GRANT_HEADER_LEN];
            let received = fdpass::recv_with_fds(&stream, &mut header, fdpass::MAX_FDS)
                .expect("the header and its descriptors");
            let count = received.fds.len();
            drop(received);
            drop(stream);
            count
        });
        let peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                &|| false,
            ),
            "claimed",
        );
        let outcome = peer.transfer(
            TEST_NONCE,
            &[(11, 4242, fake_master_rd.as_fd())],
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            None,
        );
        let received = dialer.join().expect("dialer thread");
        assert_eq!(received, 3, "the master and both pipes reached the peer");
        match outcome {
            Ok(()) => {}
            Err(RendezvousError::TransferPartial(_)) => {}
            Err(other) => panic!(
                "after a delivered header the failure can never claim the descriptors \
                 stayed home: {other}"
            ),
        }
        drop((ready_rd, ready_wr, commit_rd, commit_wr));
    }

    /// A dialer that does not know the secret is refused, and when nobody better
    /// arrives the wait ends on that refusal rather than on a bare deadline —
    /// "no successor dialed" would be false, and which of the two happened is
    /// what a field investigation reads first.
    #[test]
    fn a_dialer_without_the_secret_is_refused() {
        let rendezvous = bind_for_test();
        // Connected and fully spoken inline, before the accept's deadline starts,
        // so no scheduling delay of a dialer thread can turn the refusal into a
        // bare deadline.
        let stream = CtlStream::connect(rendezvous.path()).expect("connect");
        (&stream)
            .write_all(&claim_frame(&"f".repeat(CLAIM_HEX_LEN)))
            .and_then(|()| (&stream).flush())
            .expect("the dialer presents a wrong secret");
        let refused = rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + Duration::from_millis(400),
            &|| false,
        );
        assert_eq!(refused.err(), Some(RendezvousError::Claim));
        // Held open until now, so the refusal is provably about the claim and
        // not about a peer that vanished.
        drop(stream);
    }

    /// A wrong dialer costs its own connection, not the update: it is refused and closed, and the
    /// real successor dialing second still claims everything.
    #[test]
    fn a_wrong_dialer_does_not_end_the_attempt_and_the_successor_still_claims() {
        let rendezvous = bind_for_test();
        let (master, slave) = open_pty();
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let path = rendezvous.path().to_path_buf();
        let claim = rendezvous.claim().to_string();

        // First in the accept queue: connected and fully spoken inline, before the real dial
        // exists.
        let bogus = CtlStream::connect(&path).expect("the wrong dialer connects");
        (&bogus)
            .write_all(&claim_frame(&"f".repeat(CLAIM_HEX_LEN)))
            .and_then(|()| (&bogus).flush())
            .expect("the wrong dialer presents a wrong secret");
        bogus
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .expect("bound the wrong dialer's own read");

        let successor = std::thread::spawn(move || {
            dial_and_claim(
                &path,
                &claim,
                TEST_NONCE,
                deadlines_in(120),
                own_parent(),
                false,
            )
        });
        let peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                &|| false,
            ),
            "the wrong dialer is refused and the wait resumes for the real successor",
        );
        // SAFETY: a descriptor this test owns for the length of the call.
        let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
        // Parked at least 5 ms before the grant: the successor reads that back.
        let parked_at = Instant::now().checked_sub(Duration::from_millis(5));
        peer.transfer(
            TEST_NONCE,
            &[(11, 4242, borrowed)],
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            parked_at,
        )
        .expect("transfer");
        let claimed = successor
            .join()
            .expect("dialer thread")
            .expect("the successor behind a wrong dialer still claims its handoff");
        assert_eq!(claimed.session_count(), 1);
        assert!(
            parked_at.is_none()
                || claimed
                    .parked_before_grant()
                    .is_some_and(|parked| parked >= Duration::from_millis(5)),
            "the park stamp crossed the real socket: {:?}",
            claimed.parked_before_grant()
        );

        let mut sink = [0u8; 1];
        assert_eq!(
            (&bogus).read(&mut sink).ok(),
            Some(0),
            "and the refused connection was CLOSED, which is how the wrong dialer learns it lost"
        );

        drop(claimed);
        drop((bogus, ready_rd, ready_wr, commit_rd, commit_wr));
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    /// "Keep accepting" is bounded work, not a treadmill: a flood of wrong
    /// dialers is answered [`MAX_REFUSED_DIALS`] times and then the attempt ends
    /// on the last refusal, well inside a deadline it never got to spend. The
    /// cap is what stops a peer that connects in a loop from owning this thread
    /// for the whole handoff.
    #[test]
    fn a_flood_of_wrong_dialers_is_bounded_rather_than_served_forever() {
        let rendezvous = bind_for_test();
        let path = rendezvous.path().to_path_buf();
        let wrong = claim_frame(&"f".repeat(CLAIM_HEX_LEN));
        // Queued before the accept loop starts, so the flood is what it finds.
        let flood = (0..MAX_REFUSED_DIALS)
            .map(|_| {
                let stream = CtlStream::connect(&path).expect("connect");
                (&stream)
                    .write_all(&wrong)
                    .and_then(|()| (&stream).flush())
                    .expect("present a wrong claim");
                stream
            })
            .collect::<Vec<_>>();

        // A two-minute deadline, bounded at one: an uncapped loop refuses the
        // same flood, finds the queue empty and answers the same `Claim` (the
        // last refusal) when the deadline runs out, two minutes on. The minute
        // is a hang detector, not a latency budget.
        let started = Instant::now();
        let refused = rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + Duration::from_secs(120),
            &|| false,
        );
        assert_eq!(refused.err(), Some(RendezvousError::Claim));
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(60),
            "the cap ended the attempt; nothing here waited out the deadline: {elapsed:?}"
        );
        drop(flood);
    }

    /// A dribbling dialer is refused at the deadline: `set_read_timeout` bounds one syscall, so
    /// the frame read re-derives its budget from the deadline before every read.
    #[test]
    fn a_dribbling_dialer_is_refused_at_the_deadline_rather_than_stretching_it() {
        let rendezvous = bind_for_test();
        let path = rendezvous.path().to_path_buf();
        // The right secret, delivered too slowly: the budget is under test, so the frame must be
        // one that would otherwise be accepted.
        let claim = rendezvous.claim().to_string();
        // Read before the dribble and the clock start: it asks Security.framework, which no
        // deadline under test bounds.
        let code = crate::handoff_rendezvous::DialerCode::this_process();
        // Connected inline, before the wait starts, so the refusal is always the frame read's,
        // never a bare deadline with no frame read.
        let stream = CtlStream::connect(&path).expect("connect");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dribbler = {
            let stop = std::sync::Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut wrote = 0usize;
                for byte in claim_frame(&claim) {
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    if (&stream).write_all(&[byte]).is_err() {
                        break;
                    }
                    wrote += 1;
                    let _ = (&stream).flush();
                    std::thread::sleep(Duration::from_millis(10));
                }
                wrote
            })
        };

        let budget = Duration::from_millis(300);
        let refused =
            rendezvous.accept_claim(Some(own_pid()), &code, Instant::now() + budget, &|| false);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);

        assert!(
            refused.is_err(),
            "a frame that does not arrive inside the deadline is refused, not awaited"
        );
        // The refusal alone cannot tell the reads apart (`gate_one` checks the deadline again
        // after the frame); how much of the frame was consumed can. 70 bytes 10 ms apart take at
        // least 690 ms, past the 300 ms deadline and inside `CLAIM_FRAME_BUDGET` (2 s): a read
        // bounded by the deadline gives up near 30 bytes, while one bounded only by the frame
        // budget, or restarting a per-read timeout, reads all 70. Load only slows the dribbler.
        let wrote = dribbler.join().expect("dribbler thread");
        assert!(
            wrote < CLAIM_FRAME_LEN,
            "the frame read waited past the attempt's deadline for the whole \
             {CLAIM_FRAME_LEN}-byte frame"
        );
    }

    /// Half a rendezvous environment says so: `rendezvous_present` answers yes to either name
    /// alone, and the claim path's error names both halves, never "carries no rendezvous", which
    /// the caller's "carries a rendezvous but could not claim it" line would contradict.
    #[test]
    fn half_a_rendezvous_environment_says_so_instead_of_claiming_to_be_neither() {
        let some = |value: &str| Some(std::ffi::OsString::from(value));
        let secret = "a".repeat(CLAIM_HEX_LEN);

        assert_eq!(
            rendezvous_env(some("/tmp/aterm/seamless.sock"), None).err(),
            Some(RendezvousError::HalfRendezvous {
                present: ENV_RENDEZVOUS,
                missing: ENV_CLAIM,
            })
        );
        assert_eq!(
            rendezvous_env(None, some(&secret)).err(),
            Some(RendezvousError::HalfRendezvous {
                present: ENV_CLAIM,
                missing: ENV_RENDEZVOUS,
            })
        );
        assert_eq!(
            rendezvous_env(None, None).err(),
            Some(RendezvousError::NoRendezvous),
            "and an empty environment is still plain absence"
        );
        assert_eq!(
            rendezvous_env(some("/tmp/aterm/seamless.sock"), some(&secret)).expect("both halves"),
            (PathBuf::from("/tmp/aterm/seamless.sock"), secret),
        );
    }

    /// Nobody dials: the parent gives the terminal back on its deadline, and the
    /// socket is gone afterwards so a late dial fails closed.
    #[test]
    fn a_rendezvous_nobody_dials_expires_and_unlinks() {
        let rendezvous = bind_for_test();
        let path = rendezvous.path().to_path_buf();
        assert!(path.exists(), "the listener is on disk while it is bound");
        let expired = rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + Duration::from_millis(50),
            &|| false,
        );
        assert_eq!(expired.err(), Some(RendezvousError::Deadline));
        drop(rendezvous);
        assert!(
            !path.exists(),
            "a dropped rendezvous unlinks, so a late dial fails ENOENT rather than hanging"
        );
        assert!(
            CtlStream::connect(&path).is_err(),
            "and a late dialer cannot connect to what is no longer there"
        );
    }

    /// A cancel poke does not have to wait out the deadline.
    #[test]
    fn an_aborted_wait_returns_promptly() {
        let rendezvous = bind_for_test();
        // A two-minute deadline, bounded at one: a wait that consulted the
        // abort predicate only once the deadline had passed would answer the
        // same `Cancelled` two minutes on. The minute is a hang detector, not a
        // latency budget.
        let started = Instant::now();
        let cancelled = rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + Duration::from_secs(120),
            &|| true,
        );
        assert_eq!(cancelled.err(), Some(RendezvousError::Cancelled));
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(60),
            "the abort predicate is checked between poll slices, not after the deadline: \
             {elapsed:?}"
        );
    }

    /// Dial and grant deadlines both `secs` from now.
    fn deadlines_in(secs: u64) -> ClaimDeadlines {
        let at = Instant::now() + Duration::from_secs(secs);
        ClaimDeadlines {
            dial: at,
            grant: at,
        }
    }

    /// A held claim: the parent holds the served stream and the grant still arrives whole,
    /// bounded by the dialer's grant budget, not its already spent dial budget.
    /// On the dialer's clock ([`set_io_clock_for_test`]) only its first read, before the claim
    /// write, is inside the dial budget, so only the grant budget can govern the wait.
    #[test]
    fn a_held_claim_is_granted_after_the_hold_under_the_grant_budget() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let rendezvous = bind_for_test();
        let (master, slave) = open_pty();
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        // Both budgets are hang detectors; the dialer's clock spends the dial's.
        let dial = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let deadlines = ClaimDeadlines {
            dial,
            grant: dial + 2 * aterm_spec::wait::MUST_HAPPEN,
        };
        let reads = Arc::new(AtomicUsize::new(0));
        // A dialer that gave up ends the parent's accept too, so its answer is the failure.
        let ended = Arc::new(AtomicBool::new(false));
        let dialer = {
            let (reads, ended) = (Arc::clone(&reads), Arc::clone(&ended));
            std::thread::spawn(move || {
                set_io_clock_for_test(Some(Box::new(move || {
                    if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                        Instant::now()
                    } else {
                        dial + MIN_IO_TIMEOUT
                    }
                })));
                let claimed =
                    dial_and_claim(&path, &claim, TEST_NONCE, deadlines, own_parent(), false);
                ended.store(true, Ordering::SeqCst);
                claimed
            })
        };
        let accepted = rendezvous.accept_claim(
            Some(own_pid()),
            &crate::handoff_rendezvous::DialerCode::this_process(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            &|| ended.load(Ordering::SeqCst),
        );
        if accepted.is_err() && ended.load(Ordering::SeqCst) {
            let gave_up = dialer.join().expect("dialer thread").err();
            panic!("the dialer ended before its claim was accepted: {gave_up:?}");
        }
        let peer = admitted(accepted, "claimed");
        assert!(!peer.poll_hangup(), "a held, idle dialer is not a hangup");
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the dialer's grant wait judged its budget past the dial",
            || (reads.load(Ordering::SeqCst) >= 2).then_some(()),
        );
        assert!(
            !peer.poll_hangup(),
            "…nor once the hold outlasted its dial budget"
        );
        // SAFETY: a descriptor this test owns for the length of the call.
        let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
        peer.transfer(
            TEST_NONCE,
            &[(11, 4242, borrowed)],
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            None,
        )
        .expect("transfer after the hold");
        let claimed = dialer
            .join()
            .expect("dialer thread")
            .expect("claimed after the hold");
        assert_eq!(claimed.session_count(), 1);
        drop(claimed);
        drop((ready_rd, ready_wr, commit_rd, commit_wr));
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    /// An accept short of a descriptor has already lost its dial, which is why
    /// `accept_one` ends the wait on such an error instead of waiting for room:
    /// Darwin dequeues the connection before it allocates the descriptor, and
    /// when the allocation fails it closes the connection rather than putting
    /// it back. So the dialer this process could not accept is told at once —
    /// its claim reads the end of the rendezvous and it exits before any
    /// window — and no dial is left queued for a retry to find: a wait for
    /// room would only hold the attempt to its deadline over a successor that
    /// is already gone. The cure for the exhaustion is room before the
    /// rendezvous (`raise_descriptor_limit_for_park`), not a longer wait.
    ///
    /// Run in a child of the test binary, whose table this test fills.
    #[test]
    fn an_accept_short_of_a_descriptor_ends_the_wait_because_the_kernel_dropped_the_dial() {
        if !crate::control_auth::enter_low_nofile_test_child(64) {
            return;
        }
        let rendezvous = bind_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        let parent = own_parent();
        // Settled before the table fills: reading the requirement opens this binary.
        let code = crate::handoff_rendezvous::DialerCode::this_process();
        // Fill the table, then give back exactly one slot: the dialer's socket.
        let mut filler = Vec::new();
        loop {
            match std::fs::File::open("/dev/null") {
                Ok(file) => filler.push(file),
                Err(error) if error.raw_os_error() == Some(libc::EMFILE) => break,
                Err(error) => panic!("filling the table: {error}"),
            }
        }
        drop(filler.pop());
        // Both budgets are three minutes and both "at once" checks a minute's hang detector, so
        // an end that waited for its budget fails with its own message before `test_alone`
        // SIGKILLs this child at five.
        let dialer = std::thread::spawn(move || {
            let started = Instant::now();
            let outcome =
                dial_and_claim(&path, &claim, TEST_NONCE, deadlines_in(180), parent, false);
            (outcome, started.elapsed())
        });
        // The dial is queued once the listener reads ready.
        let queued_by = Instant::now() + Duration::from_secs(60);
        loop {
            let mut pollfd = libc::pollfd {
                fd: rendezvous.listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one initialized pollfd naming the open listener.
            if unsafe { libc::poll(&mut pollfd, 1, 50) } > 0 {
                break;
            }
            assert!(Instant::now() < queued_by, "the dial never queued");
        }

        let started = Instant::now();
        let starved = rendezvous.accept_claim(
            Some(own_pid()),
            &code,
            Instant::now() + Duration::from_secs(180),
            &|| false,
        );
        match starved {
            Err(RendezvousError::Accept(detail)) => assert!(
                detail.contains(&std::io::Error::from_raw_os_error(libc::EMFILE).to_string()),
                "the attempt says what ended it: {detail}"
            ),
            Err(other) => panic!("expected the accept's own error, got {other}"),
            Ok(_) => panic!("PRECONDITION: the table was full"),
        }
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "ended at once, not at the deadline: {:?}",
            started.elapsed()
        );
        // The rendezvous is still held here, so the end the dialer reads is
        // the kernel's close of its dropped connection: the end of the
        // rendezvous if its claim was written first, else its own socket
        // refusing the rest of the dial (Darwin's `SO_RCVTIMEO` on a socket
        // shut both ways is `EINVAL`, see `arm_read_timeout`).
        let (outcome, elapsed) = dialer.join().expect("dialer thread");
        match outcome {
            Err(RendezvousError::Grant(reason)) => {
                assert_eq!(reason, "the parent closed the rendezvous");
            }
            Err(RendezvousError::Dial(_)) => {}
            Err(other) => panic!("expected the dropped dial's end, got {other}"),
            Ok(_) => panic!("expected the dropped dial's end, got a grant"),
        }
        assert!(
            elapsed < aterm_spec::wait::MUST_HAPPEN,
            "the dialer is told at once, not at its grant budget: {elapsed:?}"
        );
        drop(filler);
        drop(rendezvous);
    }

    /// A grant that never comes, the parent accepting and then neither granting nor closing,
    /// ends as the successor's own deadline (`Deadline`, said as `ClaimDeadline`), never as a
    /// malformed grant.
    #[test]
    fn a_grant_that_never_arrives_ends_as_the_claim_deadline() {
        let rendezvous = bind_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        let deadlines = ClaimDeadlines {
            dial: Instant::now() + Duration::from_secs(60),
            grant: Instant::now() + Duration::from_millis(300),
        };
        let dialer = std::thread::spawn(move || {
            dial_and_claim(&path, &claim, TEST_NONCE, deadlines, own_parent(), false)
        });
        // Held, never granted, never dropped, until the dialer has answered, by the bare accept
        // rather than the gated claim: on a loaded machine the dialer may end (correctly,
        // `Deadline`) and close before this thread runs, and the gate would then refuse it
        // `PeerUnattested` (`LOCAL_PEERPID` answers `ENOTCONN`). The property is the dialer's.
        // The minute is a hang detector.
        let held = rendezvous
            .accept_one(Instant::now() + Duration::from_secs(60), &|| false)
            .expect("accepted");
        let outcome = dialer.join().expect("dialer thread");
        assert!(
            Instant::now() >= deadlines.grant,
            "the wait lasted the grant budget"
        );
        match outcome {
            Err(RendezvousError::Deadline) => {}
            Err(other) => panic!("expected the grant budget's deadline, got {other}"),
            Ok(_) => panic!("expected the grant budget's deadline, got a grant"),
        }
        assert_eq!(
            claim_side(RendezvousError::Deadline),
            RendezvousError::ClaimDeadline,
            "which the claim says as this successor's own deadline"
        );
        drop(held);
    }

    /// Wait, bounded, for a dialer's close to reach the parent as a hang-up.
    ///
    /// A dropped `CtlStream` is closed only when every copy of its descriptor
    /// is: a child another test in this binary is forking holds one until it
    /// execs, longer on a loaded machine, and a descriptor made close-on-exec
    /// non-atomically (macOS `socket` + `FIOCLEX`) can ride through the exec
    /// itself. So a hang-up is waited for up to the one-minute hang detector,
    /// not expected at once.
    fn wait_for_hangup(hung_up: impl Fn() -> bool) {
        let seen = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(10),
            || hung_up().then_some(()),
        );
        assert!(seen.is_some(), "the closed dialer never read as a hangup");
    }

    /// Production's rendezvous base ([`super::admits_temp_base`]): absolute, a real
    /// directory of this uid that no group or other can write, off `/tmp`.
    #[cfg(unix)]
    #[test]
    fn the_rendezvous_base_is_admitted_only_private_absolute_and_off_tmp() {
        use super::{BaseFacts, admits_temp_base};
        use std::path::Path;
        let uid = 501;
        let private = BaseFacts {
            is_dir: true,
            uid,
            mode: 0o40700,
        };
        let base = Path::new("/var/folders/xy/T");
        assert!(admits_temp_base(base, Some(private), uid), "a private base");
        let readable = BaseFacts {
            mode: 0o40755,
            ..private
        };
        assert!(
            admits_temp_base(base, Some(readable), uid),
            "others may read it, as long as they cannot write it"
        );
        for (why, base, facts) in [
            ("relative", Path::new("tmp"), Some(private)),
            ("/tmp itself", Path::new("/tmp"), Some(private)),
            ("under /tmp", Path::new("/tmp/x"), Some(private)),
            (
                "under /private/tmp",
                Path::new("/private/tmp/x"),
                Some(private),
            ),
            ("unreadable", base, None),
            (
                "a symlink or a file",
                base,
                Some(BaseFacts {
                    is_dir: false,
                    ..private
                }),
            ),
            (
                "another uid's",
                base,
                Some(BaseFacts {
                    uid: uid + 1,
                    ..private
                }),
            ),
            (
                "group-writable",
                base,
                Some(BaseFacts {
                    mode: 0o40770,
                    ..private
                }),
            ),
            (
                "other-writable",
                base,
                Some(BaseFacts {
                    mode: 0o40707,
                    ..private
                }),
            ),
        ] {
            assert!(!admits_temp_base(base, facts, uid), "{why} is admitted");
        }
    }

    /// Bind a rendezvous under a test-only nonce where production binds it (in this
    /// test binary, its own directory under `/tmp`), failing loudly.
    fn bind_for_test() -> Rendezvous {
        let nonce = aterm_uds::rand::hex_token::<16>()
            .unwrap_or_else(|error| panic!("a rendezvous test cannot mint its nonce: {error}"));
        Rendezvous::bind(&nonce).unwrap_or_else(|error| panic!("bind a rendezvous: {error}"))
    }

    /// A PTY pair, both ends close-on-exec from birth: no child another test
    /// spawns meanwhile holds either end past its exec.
    fn open_pty() -> (i32, i32) {
        aterm_pty::open_pty_pair().expect("open_pty_pair")
    }

    fn pipe_for_test() -> (OwnedFd, OwnedFd) {
        let mut raw = [0i32; 2];
        // SAFETY: `pipe` fills two descriptor numbers into the array.
        assert_eq!(unsafe { libc::pipe(raw.as_mut_ptr()) }, 0, "pipe");
        // SAFETY: fresh pipe descriptors, exclusively owned from here.
        unsafe { (OwnedFd::from_raw_fd(raw[0]), OwnedFd::from_raw_fd(raw[1])) }
    }
    // ------------------------------------------------ the chunked grant (ATRZ2G)

    /// `count` pipes: `(read ends, write ends)`. A write end stands in for a PTY
    /// master on the wire (nothing on either side asks what a granted descriptor
    /// IS), and its read end is the witness that every copy of it was closed:
    /// it reads EOF only once no write end is open anywhere in this process.
    fn pipes_for_test(count: usize) -> (Vec<OwnedFd>, Vec<OwnedFd>) {
        (0..count).map(|_| pipe_for_test()).unzip()
    }

    /// `(st_dev, st_ino)` of a live descriptor: the file it names.
    fn inode(fd: i32) -> (libc::dev_t, libc::ino_t) {
        let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `fstat` writes one `struct stat` through the pointer.
        assert_eq!(unsafe { libc::fstat(fd, info.as_mut_ptr()) }, 0, "fstat");
        // SAFETY: `fstat` returned 0, so it initialized the structure.
        let info = unsafe { info.assume_init() };
        (info.st_dev, info.st_ino)
    }

    /// Wait, bounded, until every read end reads EOF — until no copy of any
    /// write end is open in this process. A hang detector, not a latency
    /// budget: a copy another test's fork holds until its exec closes late.
    fn assert_every_write_end_closed(reads: &[OwnedFd], what: &str) {
        for read in reads {
            // SAFETY: plain fcntl on a descriptor this test owns.
            unsafe {
                libc::fcntl(read.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        for (index, read) in reads.iter().enumerate() {
            loop {
                let mut byte = [0u8; 1];
                // SAFETY: a one-byte read into a live buffer on a live descriptor.
                let got = unsafe {
                    libc::read(
                        read.as_raw_fd(),
                        byte.as_mut_ptr().cast::<libc::c_void>(),
                        1,
                    )
                };
                if got == 0 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "{what}: descriptor {index} is still open somewhere in this process"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// A grant body for `sessions` sessions `0..sessions` with shell pids
    /// `5000 + id`, spelled as `encode_grant_body` spells it.
    fn body_for(sessions: usize) -> String {
        let entries = (0..sessions)
            .map(|id| format!("{id}:{}", 5000 + id))
            .collect::<Vec<_>>()
            .join(",");
        format!("{TEST_NONCE}\n{entries}\n")
    }

    /// The layout both sides compute: every descriptor placed once, no message over the
    /// `SCM_RIGHTS` ceiling, the pipes always last, no more messages than the header can name,
    /// and exactly one message up to 62 sessions.
    #[test]
    fn the_chunk_layout_places_every_descriptor_and_the_pipes_last() {
        for sessions in 1..=MAX_CHUNKED_SESSIONS {
            let sizes = grant_chunk_sizes(sessions);
            assert_eq!(
                sizes.iter().sum::<usize>(),
                sessions + RENDEZVOUS_CHANNEL_FDS,
                "{sessions}: every descriptor, once"
            );
            assert!(
                sizes
                    .iter()
                    .all(|size| (1..=fdpass::MAX_FDS).contains(size)),
                "{sessions}: {sizes:?} fits the message ceiling"
            );
            assert!(
                *sizes.last().expect("a message") >= RENDEZVOUS_CHANNEL_FDS,
                "{sessions}: the readiness and Commit ends travel last"
            );
            assert!(sizes.len() <= MAX_GRANT_CHUNKS, "{sessions}: {sizes:?}");
            assert_eq!(
                sizes.len() == 1,
                sessions <= MAX_RENDEZVOUS_SESSIONS,
                "{sessions}: one message exactly when one message carries it"
            );
        }
        assert_eq!(
            grant_chunk_sizes(MAX_CHUNKED_SESSIONS).len(),
            MAX_GRANT_CHUNKS,
            "the protocol ceiling spans the most messages the header allows"
        );
    }

    /// Both claims carry the same secret; only the magic says which grant the
    /// dialer reads, and an unknown magic is no claim at all.
    #[test]
    fn both_claim_magics_carry_the_secret_and_say_which_grant() {
        let secret = "a".repeat(CLAIM_HEX_LEN);
        assert_eq!(
            claim_frame_kind(&claim_frame_with(&secret, false), &secret),
            Some(false)
        );
        assert_eq!(
            claim_frame_kind(&claim_frame_with(&secret, true), &secret),
            Some(true)
        );
        assert_eq!(
            claim_frame_kind(&claim_frame_with(&secret, true), &"b".repeat(CLAIM_HEX_LEN)),
            None,
            "the chunked claim still needs this attempt's secret"
        );
        let mut unknown = claim_frame_with(&secret, true);
        unknown[4] = b'9';
        assert_eq!(claim_frame_kind(&unknown, &secret), None);
        assert_eq!(&claim_frame(&secret)[..6], b"ATRZ1C", "the default claim");
    }

    /// Only the parent's offer makes a successor claim the chunked grant.
    #[test]
    fn only_the_parents_offer_makes_a_chunked_claim() {
        let offered = |caps: &str| offers_chunked_grant(Some(std::ffi::OsStr::new(caps)));
        assert!(offered("chunks1"));
        assert!(offered("later2, chunks1"));
        assert!(!offered(""));
        assert!(!offered("chunks2"));
        assert!(!offered("chunks10"));
        assert!(!offers_chunked_grant(None));
    }

    /// A launched successor makes room for its grant before it dials: run in a
    /// child of the test binary whose soft descriptor limit leaves 64 to spare —
    /// launchd's 256 in miniature — the claim's raise lifts it to the floor (or
    /// the hard limit), and then a 100-session chunked grant crosses whole in
    /// that very process.
    #[test]
    fn a_successor_raises_its_descriptor_limit_before_claiming() {
        use crate::descriptor_limit::{raise_descriptor_limit_for_claim, raised_soft_limit};
        assert_eq!(raised_soft_limit(256, u64::MAX), Some(4096));
        assert_eq!(raised_soft_limit(256, 1024), Some(1024));
        assert_eq!(raised_soft_limit(10_240, u64::MAX), None, "never lowered");
        assert_eq!(raised_soft_limit(1024, 1024), None, "at the hard limit");
        if !crate::control_auth::enter_low_nofile_test_child(64) {
            return;
        }
        let raised = raise_descriptor_limit_for_claim().expect("the raise is permitted");
        let (from, to) = raised.expect("a 64-spare limit is below the floor");
        assert!(from < to, "{from} -> {to}");
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: a valid resource id and a writable out-parameter.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
            0
        );
        assert_eq!(limit.rlim_cur, to);
        a_grant_of_100_sessions_crosses_in_chunks();
        crate::descriptor_limit::tests::a_shell_spawned_after_the_raise_gets_the_launchers_limit(
            from, to,
        );
    }

    /// A grant whose deadline passed before the send is this process's lateness after a dial:
    /// `GrantDeadline` with nothing sent (one-message and chunked), never "no successor dialed".
    #[test]
    fn a_grant_past_its_deadline_says_the_successor_dialed_and_sends_nothing() {
        for (sessions, chunks) in [(3_usize, false), (100, true)] {
            let rendezvous = bind_for_test();
            let (_reads, writes) = pipes_for_test(sessions);
            let (_ready_rd, ready_wr) = pipe_for_test();
            let (commit_rd, _commit_wr) = pipe_for_test();
            let claim = rendezvous.claim().to_string();
            let path = rendezvous.path().to_path_buf();
            let dialer = std::thread::spawn(move || {
                dial_and_claim(
                    &path,
                    &claim,
                    TEST_NONCE,
                    deadlines_in(120),
                    own_parent(),
                    chunks,
                )
            });
            let peer = admitted(
                rendezvous.accept_claim(
                    Some(own_pid()),
                    &crate::handoff_rendezvous::DialerCode::this_process(),
                    Instant::now() + Duration::from_secs(60),
                    &|| false,
                ),
                "the claim is this attempt's",
            );
            let handed = writes
                .iter()
                .enumerate()
                .map(|(id, write)| (id as u64, 5000 + id as i32, write.as_fd()))
                .collect::<Vec<_>>();
            let expired = Instant::now()
                .checked_sub(Duration::from_millis(1))
                .expect("an instant in the past");
            let error = peer
                .transfer(
                    TEST_NONCE,
                    &handed,
                    ready_wr.as_fd(),
                    commit_rd.as_fd(),
                    expired,
                    None,
                )
                .expect_err("a grant past its deadline is not sent");
            assert_eq!(error, RendezvousError::GrantDeadline, "{sessions} sessions");
            // Nothing was sent: the dialer sees the parent close, not a grant.
            drop((peer, rendezvous));
            let received = dialer.join().expect("dialer thread");
            assert!(
                received.is_err(),
                "{sessions} sessions: the dialer received no grant"
            );
        }
    }

    /// A hundred sessions, more than one `SCM_RIGHTS` message carries, cross to an `ATRZ2C`
    /// claimant, each descriptor paired with its session and the two pipes last.
    #[test]
    fn a_grant_of_100_sessions_crosses_in_chunks() {
        let rendezvous = bind_for_test();
        let (reads, writes) = pipes_for_test(100);
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        let dialer = std::thread::spawn(move || {
            dial_and_claim(
                &path,
                &claim,
                TEST_NONCE,
                deadlines_in(120),
                own_parent(),
                true,
            )
        });
        let peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + Duration::from_secs(60),
                &|| false,
            ),
            "the chunked claim is this attempt's",
        );
        assert!(peer.chunks, "the dialer claimed the chunked grant");
        let sessions = writes
            .iter()
            .enumerate()
            .map(|(id, write)| (id as u64, 5000 + id as i32, write.as_fd()))
            .collect::<Vec<_>>();
        peer.transfer(
            TEST_NONCE,
            &sessions,
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + Duration::from_secs(60),
            None,
        )
        .expect("a chunked grant of 100 sessions is sent");
        let claimed = dialer
            .join()
            .expect("dialer thread")
            .expect("the chunked grant is taken whole");
        assert_eq!(claimed.session_count(), 100);
        for (id, (master, sent)) in claimed.masters.iter().zip(&writes).enumerate() {
            assert_eq!(
                inode(master.as_raw_fd()),
                inode(sent.as_raw_fd()),
                "session {id} is the descriptor sent for it"
            );
        }
        assert_eq!(
            inode(claimed.ready.as_raw_fd()),
            inode(ready_wr.as_raw_fd())
        );
        assert_eq!(
            inode(claimed.commit.as_raw_fd()),
            inode(commit_rd.as_raw_fd())
        );
        let wire = claimed.fds_wire.split(',').collect::<Vec<_>>();
        assert_eq!(wire.len(), 100);
        for (id, entry) in wire.iter().enumerate() {
            assert!(
                entry.starts_with(&format!("{id}=")) && entry.ends_with(&format!(":{}", 5000 + id)),
                "the wire pairs session {id} with its shell: {entry}"
            );
        }
        drop((claimed, peer, rendezvous));
        drop((writes, ready_wr, commit_wr, ready_rd, commit_rd));
        drop(reads);
    }

    /// An `ATRZ1C` claimant never receives a chunked grant: a hundred sessions for it are
    /// refused before a descriptor leaves, and it refuses an `ATRZ2G` header as the wrong magic.
    /// An `ATRZ2C` claimant of three sessions receives `ATRZ1G`, byte for byte.
    #[test]
    fn an_atrz1c_claim_never_receives_a_chunked_grant() {
        use std::io::Read as _;

        // An ATRZ1C claimant with a hundred sessions to take.
        let rendezvous = bind_for_test();
        let (reads, writes) = pipes_for_test(100);
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        let dialer = std::thread::spawn(move || {
            dial_and_claim(
                &path,
                &claim,
                TEST_NONCE,
                deadlines_in(120),
                own_parent(),
                false,
            )
        });
        let peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + Duration::from_secs(60),
                &|| false,
            ),
            "the one-message claim is this attempt's",
        );
        assert!(!peer.chunks);
        let sessions = writes
            .iter()
            .enumerate()
            .map(|(id, write)| (id as u64, 5000 + id as i32, write.as_fd()))
            .collect::<Vec<_>>();
        assert_eq!(
            peer.transfer(
                TEST_NONCE,
                &sessions,
                ready_wr.as_fd(),
                commit_rd.as_fd(),
                Instant::now() + Duration::from_secs(60),
                None,
            ),
            Err(RendezvousError::TooManySessions {
                sessions: 100,
                limit: MAX_RENDEZVOUS_SESSIONS,
            }),
            "refused before any descriptor left"
        );
        drop((peer, rendezvous));
        assert!(
            matches!(
                dialer.join().expect("dialer thread"),
                Err(RendezvousError::Grant(_))
            ),
            "the ATRZ1C successor reads the close and takes nothing"
        );
        drop((writes, ready_wr, commit_wr, ready_rd, commit_rd, reads));

        // An ATRZ2C claimant with three sessions: the one-message grant.
        let rendezvous = bind_for_test();
        let (master, slave) = open_pty();
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let dialer = CtlStream::connect(rendezvous.path()).expect("connect");
        (&dialer)
            .write_all(&claim_frame_with(rendezvous.claim(), true))
            .expect("claim");
        let peer = admitted(
            rendezvous.accept_claim(
                Some(own_pid()),
                &crate::handoff_rendezvous::DialerCode::this_process(),
                Instant::now() + Duration::from_secs(60),
                &|| false,
            ),
            "the chunked claim is this attempt's",
        );
        // SAFETY: a descriptor this test owns for the length of the call.
        let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
        let sessions = [
            (0u64, 4000, borrowed),
            (1, 4001, borrowed),
            (7, 4002, borrowed),
        ];
        peer.transfer(
            TEST_NONCE,
            &sessions,
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + Duration::from_secs(60),
            None,
        )
        .expect("three sessions are granted");
        dialer
            .set_read_timeout(Some(Duration::from_secs(60)))
            .expect("a read bound");
        let mut header = [0u8; GRANT_HEADER_LEN];
        let received =
            fdpass::recv_with_fds(&dialer, &mut header, fdpass::MAX_FDS).expect("the header");
        assert_eq!(received.bytes, GRANT_HEADER_LEN);
        assert_eq!(received.fds.len(), 5, "every descriptor in the one message");
        let body = encode_grant_body(TEST_NONCE, &sessions, None).expect("a body");
        assert_eq!(
            header,
            grant_header(body.len()).expect("a header"),
            "ATRZ1G, byte for byte"
        );
        let mut sent_body = vec![0u8; body.len()];
        (&dialer).read_exact(&mut sent_body).expect("the body");
        assert_eq!(sent_body, body.as_bytes());
        drop((received, dialer, peer, rendezvous));
        drop((ready_rd, ready_wr, commit_rd, commit_wr));
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);

        // An ATRZ1C claimant handed an ATRZ2G header anyway.
        let (reads, writes) = pipes_for_test(3);
        let outcome = stub_grant(
            &writes.iter().map(|fd| fd.as_raw_fd()).collect::<Vec<_>>(),
            &mut |stream: &CtlStream, fds: &[BorrowedFd<'_>]| {
                let body = body_for(1);
                let header = grant_chunks_header(body.len(), 1).expect("a header");
                fdpass::send_with_fds(stream, &header, fds)?;
                (&*stream).write_all(body.as_bytes())
            },
            false,
        );
        assert_eq!(
            outcome,
            Err(RendezvousError::Grant("wrong magic".to_string())),
            "a successor that never claimed the chunked grant refuses it"
        );
        drop(writes);
        assert_every_write_end_closed(&reads, "a refused ATRZ2G");
    }

    /// A stub parent on a fresh private socket: accept one dial, read its
    /// 70-byte claim, then `send` whatever grant the test scripts over `fds` —
    /// while this thread dials it with this build's `dial_and_claim` (claiming
    /// the chunked grant when `chunks`). Answers what the dialer answered.
    fn stub_grant(
        fds: &[i32],
        send: &mut (dyn FnMut(&CtlStream, &[BorrowedFd<'_>]) -> std::io::Result<()> + Send),
        chunks: bool,
    ) -> Result<(), RendezvousError> {
        use std::io::Read as _;

        let dir = crate::test_scratch::short_dir("a stub parent's socket");
        let path = dir.path().join("p.sock");
        let listener = aterm_uds::CtlListener::bind(&path).expect("a stub listener");
        let secret = "c".repeat(CLAIM_HEX_LEN);
        let outcome = std::thread::scope(|scope| {
            let parent = scope.spawn(|| {
                let (stream, _) = listener.accept().expect("the dial");
                stream
                    .set_read_timeout(Some(Duration::from_secs(60)))
                    .expect("a read bound");
                let mut claim = [0u8; CLAIM_FRAME_LEN];
                (&stream).read_exact(&mut claim).expect("the claim");
                let borrowed = fds
                    .iter()
                    // SAFETY: the caller keeps every descriptor open until this returns.
                    .map(|fd| unsafe { BorrowedFd::borrow_raw(*fd) })
                    .collect::<Vec<_>>();
                let _ = send(&stream, &borrowed);
                // Held until the dialer has answered, as a real parent holds it through the
                // proof wait: Darwin refuses a socket option on a stream whose peer has gone
                // (`EINVAL`), so a stub that closed at once would be refused for that and not for
                // its grant. A grant cut short says so with a write shutdown.
                stream
            });
            let outcome = dial_and_claim(
                &path,
                &secret,
                TEST_NONCE,
                deadlines_in(60),
                own_parent(),
                chunks,
            )
            .map(drop);
            drop(parent.join().expect("the stub parent"));
            outcome
        });
        drop((listener, dir));
        outcome
    }

    /// A hundred-session grant whose second message carries 30 descriptors where its layout
    /// names 38, or never comes, is refused, and every descriptor the successor received is
    /// closed.
    #[test]
    fn a_short_chunk_is_refused_and_closes_every_fd() {
        for (what, second) in [
            ("a short second chunk", Some(30usize)),
            ("no second chunk", None),
        ] {
            let (reads, writes) = pipes_for_test(100 + RENDEZVOUS_CHANNEL_FDS);
            let raw = writes.iter().map(|fd| fd.as_raw_fd()).collect::<Vec<_>>();
            let sizes = grant_chunk_sizes(100);
            assert_eq!(sizes, [64, 38], "the layout this test shortens");
            let outcome = stub_grant(
                &raw,
                &mut |stream: &CtlStream, fds: &[BorrowedFd<'_>]| {
                    let body = body_for(100);
                    let header = grant_chunks_header(body.len(), sizes.len()).expect("a header");
                    fdpass::send_with_fds(stream, &header, &fds[..sizes[0]])?;
                    (&*stream).write_all(body.as_bytes())?;
                    match second {
                        Some(short) => {
                            let frame = grant_continuation(1).expect("a continuation");
                            fdpass::send_with_fds(
                                stream,
                                &frame,
                                &fds[sizes[0]..sizes[0] + short],
                            )?;
                        }
                        None => stream.shutdown(std::net::Shutdown::Write)?,
                    }
                    Ok(())
                },
                true,
            );
            match second {
                Some(_) => assert_eq!(
                    outcome,
                    Err(RendezvousError::DescriptorCount {
                        expected: 102,
                        received: 94,
                    }),
                    "{what}: refused"
                ),
                None => assert!(
                    matches!(outcome, Err(RendezvousError::Grant(_))),
                    "{what}: refused, not {outcome:?}"
                ),
            }
            drop(writes);
            assert_every_write_end_closed(&reads, what);
        }
    }
    // ------------------------ Tier-1: the NativeUpdateRendezvousGrant model

    /// One model step, which must be admitted.
    fn step_grant(
        model: &aterm_spec::derive::Model,
        state: &mut std::collections::BTreeMap<&'static str, i64>,
        action: &'static str,
    ) {
        let successors = model.successors(action, state);
        assert_eq!(successors.len(), 1, "{action} is not admitted at {state:?}");
        *state = successors[0].clone();
    }

    /// Where the model and a parent's grant decision agree: for every successor
    /// (old or chunk-capable, declared or not), offer, claim and pool size, the
    /// model's enabled grant action is the one `decide` answers — the grant the
    /// parent sends, or its refusal before any descriptor leaves. `None` on
    /// agreement, else the first disagreement.
    fn grant_disagreement(
        decide: &dyn Fn(bool, usize) -> Result<GrantShape, RendezvousError>,
    ) -> Option<String> {
        let model = aterm_models_update::native_update_rendezvous_grant_model();
        for capable in [false, true] {
            for declared in [false, true] {
                for offered in [false, true] {
                    for large in [false, true] {
                        if (declared && !capable) || (offered && !declared) {
                            continue;
                        }
                        let mut state = model.init_state();
                        if capable {
                            step_grant(&model, &mut state, "Upgrade");
                        }
                        if declared {
                            step_grant(&model, &mut state, "Declare");
                        }
                        if large {
                            step_grant(&model, &mut state, "Grow");
                        }
                        if offered {
                            step_grant(&model, &mut state, "Offer");
                        }
                        step_grant(&model, &mut state, "Launch");
                        // The successor's claim: this build reads the chunked grant
                        // (`capable`); an older build never does, whatever it is offered.
                        let caps = offered.then(|| std::ffi::OsString::from(GRANT_CAPS_CHUNKS));
                        let claims_chunks = capable && offers_chunked_grant(caps.as_deref());
                        if model.action_enabled("ClaimChunks", &state) != claims_chunks
                            || model.action_enabled("ClaimOne", &state) == claims_chunks
                        {
                            return Some(format!(
                                "the claim of capable={capable} offered={offered}"
                            ));
                        }
                        let secret = "d".repeat(CLAIM_HEX_LEN);
                        let claimed =
                            claim_frame_kind(&claim_frame_with(&secret, claims_chunks), &secret)
                                .expect("the claim carries the secret");
                        step_grant(
                            &model,
                            &mut state,
                            if claimed { "ClaimChunks" } else { "ClaimOne" },
                        );
                        let pools: &[usize] = if large {
                            &[MAX_RENDEZVOUS_SESSIONS + 1, 100, MAX_CHUNKED_SESSIONS]
                        } else {
                            &[1, 3, MAX_RENDEZVOUS_SESSIONS]
                        };
                        for &sessions in pools {
                            let decided = decide(claimed, sessions);
                            let expected = (
                                matches!(decided, Ok(GrantShape::OneMessage)),
                                matches!(decided, Ok(GrantShape::Chunked)),
                                matches!(decided, Err(RendezvousError::TooManySessions { .. })),
                            );
                            let modelled = (
                                model.action_enabled("GrantOne", &state),
                                model.action_enabled("GrantChunks", &state),
                                model.action_enabled("RefuseTooMany", &state),
                            );
                            if expected != modelled {
                                return Some(format!(
                                    "{sessions} sessions for claim chunks={claimed}: the parent \
                                     decides {decided:?}, the model enables {modelled:?}"
                                ));
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Tier-1 for `NativeUpdateRendezvousGrant`: `offers_chunked_grant` and the claim frames
    /// (the successor's side) and `grant_shape` (the parent's) are the model's guards at every
    /// combination of successor, declaration, offer, claim and pool. Negative control: a parent
    /// that ignores the claim. Adoption is bound by `a_grant_of_100_sessions_crosses_in_chunks`
    /// and `a_short_chunk_is_refused_and_closes_every_fd`.
    #[test]
    fn rendezvous_grant_conformance() {
        assert_eq!(grant_disagreement(&grant_shape), None);
        assert!(
            grant_disagreement(&|_, sessions| grant_shape(true, sessions)).is_some(),
            "a parent that ignores the claim must disagree with the model"
        );
    }
}

/// Tier-1 for `NativeUpdateRendezvousClaim` (`native_update_rendezvous_claim_model`): the real
/// dial gate, [`listener_may_receive_claim`], against the model's `PresentClaim` guard at every
/// combination of the three listener facts, with real attestations; then the whole
/// [`dial_and_claim`] against a listener in a forked process, which also measures what the gate
/// rests on: `LOCAL_PEERPID`, read on the dialing end before any `accept`, names the listener.
#[cfg(test)]
mod claim_presentation_conformance {
    use super::{
        CLAIM_FRAME_LEN, ClaimDeadlines, RendezvousError, dial_and_claim,
        listener_may_receive_claim,
    };
    use crate::seamless::AttestedParent;
    use aterm_models_update::native_update_rendezvous_claim_model;
    use aterm_spec::derive::Model;
    use aterm_spec::interp::{State, with_buggy};
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::PathBuf;
    use std::time::Instant;

    /// What the dialer learned about the listener it reached, in the model's terms.
    #[derive(Clone, Copy, Debug)]
    pub(crate) struct ListenerFacts {
        same_uid: bool,
        is_parent: bool,
        parent_live: bool,
    }

    /// Project the facts onto the model's state just before a presentation.
    pub(crate) fn project(model: &Model, facts: ListenerFacts) -> State {
        let mut state = model.init_state();
        state.insert("same_uid", i64::from(facts.same_uid));
        state.insert("is_parent", i64::from(facts.is_parent));
        state.insert("parent_live", i64::from(facts.parent_live));
        state
    }

    fn every_combination() -> impl Iterator<Item = ListenerFacts> {
        (0u8..8).map(|bits| ListenerFacts {
            same_uid: bits & 1 != 0,
            is_parent: bits & 2 != 0,
            parent_live: bits & 4 != 0,
        })
    }

    fn own_pid() -> libc::pid_t {
        libc::pid_t::try_from(std::process::id()).expect("a pid fits pid_t")
    }

    /// Drive the real gate with kernel-shaped inputs realizing `facts`. A foreign-uid listener
    /// needs root, so the kernel's uid answer is what varies; the pid is a real live process
    /// (this one, or the one that launched this test); the parent is a real attestation, live or
    /// with a stale birth witness.
    fn real_gate(facts: ListenerFacts) -> Result<(), RendezvousError> {
        // SAFETY: getuid/getppid have no preconditions.
        let (own_uid, launcher) = unsafe { (libc::getuid(), libc::getppid()) };
        let listener_uid = if facts.same_uid {
            own_uid
        } else {
            own_uid.wrapping_add(1)
        };
        let listener_pid = if facts.is_parent { own_pid() } else { launcher };
        let parent = if facts.parent_live {
            AttestedParent::attest_live_for_test(own_pid())
                .expect("this live process attests by its birth record")
        } else {
            AttestedParent::recycled_for_test(own_pid())
        };
        listener_may_receive_claim(
            Some(listener_uid),
            own_uid,
            u32::try_from(listener_pid).ok(),
            Some(parent),
        )
    }

    #[test]
    fn the_dial_gate_admits_exactly_what_the_model_admits() {
        let model = native_update_rendezvous_claim_model();
        for facts in every_combination() {
            let admitted = real_gate(facts).is_ok();
            let modelled = model.action_enabled("PresentClaim", &project(&model, facts));
            assert_eq!(
                admitted, modelled,
                "the dial gate and the model's PresentClaim disagree at {facts:?}"
            );
        }
        // The one combination that presents is reachable at all: otherwise the
        // agreement above would hold for a gate that refuses everything.
        assert!(
            real_gate(ListenerFacts {
                same_uid: true,
                is_parent: true,
                parent_live: true,
            })
            .is_ok()
        );
    }

    /// Negative control on the shipping gate: the uid-only gate is the model's `Buggy`
    /// presentation (`PresentClaimUidOnly`). The real gate must refuse exactly where that mutant
    /// presents to the wrong listener (a same-uid impostor, a gone parent, or both) and agree
    /// everywhere else.
    #[test]
    fn the_real_gate_refuses_where_the_uid_only_mutant_presents() {
        let buggy = with_buggy(&native_update_rendezvous_claim_model(), 1);
        let mut told_apart = Vec::new();
        for facts in every_combination() {
            let mutant = buggy.action_enabled("PresentClaimUidOnly", &project(&buggy, facts));
            // The mutant is the uid-only gate, so this compares the real gate with that shape,
            // not with a copy of itself.
            assert_eq!(
                mutant, facts.same_uid,
                "the Buggy guard is the uid half at {facts:?}"
            );
            if real_gate(facts).is_ok() != mutant {
                told_apart.push(facts);
            }
        }
        assert_eq!(
            told_apart.len(),
            3,
            "the real gate refuses the uid-only gate's impostor, gone-parent and both \
             presentations, and only those: {told_apart:?}"
        );
        assert!(
            told_apart
                .iter()
                .all(|facts| facts.same_uid && !(facts.is_parent && facts.parent_live)),
            "every disagreement is a same-uid presentation to something other than the live \
             attested parent: {told_apart:?}"
        );
    }

    #[test]
    fn no_attested_parent_refuses_even_our_own_listener() {
        // SAFETY: getuid has no preconditions.
        let own_uid = unsafe { libc::getuid() };
        assert!(matches!(
            listener_may_receive_claim(Some(own_uid), own_uid, Some(std::process::id()), None),
            Err(RendezvousError::Dial(_))
        ));
        assert!(matches!(
            listener_may_receive_claim(
                Some(own_uid),
                own_uid,
                None,
                AttestedParent::attest_live_for_test(own_pid())
            ),
            Err(RendezvousError::Dial(_))
        ));
    }

    /// A same-uid rendezvous listener in a forked process. The child makes async-signal-safe
    /// calls only (libtest's threads make this process multi-threaded at fork time): it closes
    /// every inherited descriptor but its own, binds and listens, reports readiness, serves one
    /// connection, reports how many claim bytes it received, closes it (EOF to the dialer), and
    /// `_exit`s.
    struct ForkedListener {
        pid: libc::pid_t,
        report: OwnedFd,
        path: PathBuf,
        /// Removed once `Drop` has reaped the child (fields drop after it).
        _dir: aterm_tempfile::TempDir,
    }

    impl ForkedListener {
        fn start() -> Self {
            let dir = crate::test_scratch::short_dir("a forked listener's socket");
            let path = dir.path().join("l.sock");
            // SAFETY: an all-zero sockaddr_un is a valid empty address.
            let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
            let bytes = path.as_os_str().as_bytes();
            assert!(
                bytes.len() < addr.sun_path.len(),
                "the scratch path fits sun_path"
            );
            addr.sun_family = libc::sa_family_t::try_from(libc::AF_UNIX).expect("AF_UNIX");
            for (slot, byte) in addr.sun_path.iter_mut().zip(bytes) {
                *slot = libc::c_char::from_ne_bytes([*byte]);
            }
            let addr_len = libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_un>())
                .expect("sockaddr_un size");
            addr.sun_len = u8::try_from(addr_len).expect("sockaddr_un fits sun_len");
            let mut report = [0i32; 2];
            // SAFETY: `pipe` fills two descriptor numbers into the array.
            assert_eq!(unsafe { libc::pipe(report.as_mut_ptr()) }, 0, "report pipe");
            // The table's bound (the kernel's per-process maximum), read before the fork: the
            // child closes up to it.
            let table = aterm_uds::spawnfd::descriptor_bound();

            // SAFETY: fork from a multi-threaded harness, with a child that
            // performs async-signal-safe calls only — see the type's doc.
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0, "fork failed");
            if pid == 0 {
                // SAFETY (whole block): async-signal-safe syscalls on stack memory and
                // descriptors this child owns; nothing allocates, locks or unwinds, and the
                // child leaves through its one `_exit` (the site `crash_signal`'s exit gate
                // allows this file).
                unsafe {
                    for fd in 3..table {
                        if fd != report[1] {
                            libc::close(fd);
                        }
                    }
                    let tell = |byte: u8| {
                        libc::write(report[1], core::ptr::from_ref(&byte).cast(), 1);
                    };
                    let code: i32 = 'serve: {
                        let listener = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
                        if listener < 0
                            || libc::bind(listener, core::ptr::from_ref(&addr).cast(), addr_len)
                                != 0
                            || libc::listen(listener, 1) != 0
                        {
                            break 'serve 2;
                        }
                        tell(b'L');
                        let mut ready = libc::pollfd {
                            fd: listener,
                            events: libc::POLLIN,
                            revents: 0,
                        };
                        if libc::poll(&mut ready, 1, 30_000) != 1 {
                            break 'serve 3;
                        }
                        let served =
                            libc::accept(listener, core::ptr::null_mut(), core::ptr::null_mut());
                        if served < 0 {
                            break 'serve 4;
                        }
                        let mut buf = [0u8; 128];
                        let mut total = 0usize;
                        while total < CLAIM_FRAME_LEN {
                            let mut readable = libc::pollfd {
                                fd: served,
                                events: libc::POLLIN,
                                revents: 0,
                            };
                            if libc::poll(&mut readable, 1, 10_000) != 1 {
                                break;
                            }
                            let read = libc::read(served, buf.as_mut_ptr().cast(), buf.len());
                            if read <= 0 {
                                break;
                            }
                            total += read.unsigned_abs();
                        }
                        tell(u8::try_from(total.min(255)).unwrap_or(255));
                        libc::close(served);
                        0
                    };
                    libc::_exit(code);
                }
            }
            // SAFETY: the pipe's ends, exclusively owned from here; the write end
            // is closed so a child that dies early reads as EOF, not a hang.
            let (read_end, write_end) = unsafe {
                (
                    OwnedFd::from_raw_fd(report[0]),
                    OwnedFd::from_raw_fd(report[1]),
                )
            };
            drop(write_end);
            let listener = Self {
                pid,
                report: read_end,
                path,
                _dir: dir,
            };
            assert_eq!(
                listener.next_report(),
                Some(b'L'),
                "the forked listener is up"
            );
            listener
        }

        fn next_report(&self) -> Option<u8> {
            let mut byte = 0u8;
            // SAFETY: a one-byte read into a live local from a descriptor we own.
            let read = unsafe {
                libc::read(
                    self.report.as_raw_fd(),
                    core::ptr::from_mut(&mut byte).cast(),
                    1,
                )
            };
            (read == 1).then_some(byte)
        }

        /// How many claim bytes the listener received on its one connection.
        fn claim_bytes_received(&self) -> Option<u8> {
            self.next_report()
        }
    }

    impl Drop for ForkedListener {
        fn drop(&mut self) {
            // SAFETY: signal and reap the exact child this value forked.
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
                let mut status = 0;
                libc::waitpid(self.pid, &mut status, 0);
            }
        }
    }

    fn deadlines() -> ClaimDeadlines {
        let at = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        ClaimDeadlines {
            dial: at,
            grant: at,
        }
    }

    const SECRET: &str = "abababababababababababababababababababababababababababababababab";
    const NONCE: &str = "0123456789abcdef0123456789abcdef";

    /// A same-uid listener that is not the attested parent gets no claim byte, and the refusal
    /// names the forked listener's pid: the dialing end's `LOCAL_PEERPID` names the listener.
    #[test]
    fn a_same_uid_listener_that_is_not_the_attested_parent_gets_no_claim() {
        let listener = ForkedListener::start();
        let attested_elsewhere = AttestedParent::attest_live_for_test(own_pid());
        let outcome = dial_and_claim(
            &listener.path,
            SECRET,
            NONCE,
            deadlines(),
            attested_elsewhere,
            false,
        );
        match outcome {
            Err(RendezvousError::Dial(reason)) => assert!(
                reason.contains(&format!("(pid {})", listener.pid)),
                "the refusal names the listening process the kernel reported: {reason}"
            ),
            Err(other) => panic!("expected the identity refusal, got {other}"),
            Ok(_) => panic!("expected the identity refusal, got a grant"),
        }
        assert_eq!(
            listener.claim_bytes_received(),
            Some(0),
            "not one byte of the claim reached the impostor"
        );
    }

    /// The attested parent's listener receives the whole claim frame (so the dialing end's
    /// `LOCAL_PEERPID` named it). The fixture closes without a grant, so the dial ends in a
    /// `Grant` error: EOF, or Darwin's `EINVAL` on the grant-budget `setsockopt` of a
    /// disconnected socket.
    #[test]
    fn the_attested_parent_listener_receives_the_claim() {
        let listener = ForkedListener::start();
        let parent = AttestedParent::attest_live_for_test(listener.pid);
        assert!(
            parent.is_some(),
            "the forked listener attests by its birth record"
        );
        let outcome = dial_and_claim(&listener.path, SECRET, NONCE, deadlines(), parent, false);
        match outcome {
            Err(RendezvousError::Grant(_)) => {}
            Err(other) => panic!("expected a grant-stage end after the claim, got {other}"),
            Ok(_) => panic!("the fixture never grants"),
        }
        assert_eq!(
            listener.claim_bytes_received(),
            u8::try_from(CLAIM_FRAME_LEN).ok(),
            "the attested parent received the whole claim frame"
        );
    }
}

/// `native_update_rendezvous_claim` environment actions, each waived where its Tier-1
/// (`claim_presentation_conformance`) binds it.
// The machine's other actions are the listener's environment, not code in
// this crate: they are what the facts above realize.
#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_claim",
    action = "ForeignUidListener",
    reason = "Environment: a listener of another uid at the published path; the Tier-1 bind \
              realizes it as the kernel's getpeereid answer."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_claim",
    action = "SameUidImpostor",
    reason = "Environment: a same-uid process other than the attested parent listening at the \
              published path; realized by a forked listener and by a foreign live pid."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_claim",
    action = "ParentGone",
    reason = "Environment: the attested parent exited and its pid may be recycled; realized \
              by an attestation whose birth witness no longer matches."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "native_update_rendezvous_claim",
    action = "PresentClaimUidOnly",
    reason = "The Buggy=1 mutant (the pre-2026-09-27 uid-only gate); no shipping code \
              implements it, and the negative control above catches it."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn rendezvous_claim_waivers() {}
