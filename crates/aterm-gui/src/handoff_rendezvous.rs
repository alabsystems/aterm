// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The seamless handoff's OUT-OF-BAND transport: a single-use `AF_UNIX`
//! rendezvous the outgoing process binds BEFORE it launches its successor, and
//! that the successor dials back on.
//!
//! # Why this exists at all
//!
//! On macOS the overlap successor has to be a launchd APPLICATION job of its
//! own, or the process that just applied an update can never apply the next one
//! (`tests/handoff_launchd_job.rs` states the defect and is its regression
//! guard). A LaunchServices launch mints that job — and inherits no descriptors.
//! Everything the fork lane hands over by inheritance therefore has to travel
//! some other way: the PTY masters (`ATERM_SEAMLESS_FDS`), the readiness/proof
//! pipe (`ATERM_HANDOFF_READY_FD`) and the Commit pipe
//! (`ATERM_HANDOFF_COMMIT_FD`).
//!
//! # Why a fresh listener and not the control socket
//!
//! The per-user control socket cannot address a process that does not exist yet
//! — every name in that address space (`aterm-<pid>.sock`, the `latest` link,
//! `graph/<sid>`) identifies an already-BOUND server, and here the parent is the
//! side that must move first. So the direction is inverted: the parent binds,
//! the launch carries the path plus a fresh claim secret in the environment, and
//! the successor dials back. That also hands this process the first
//! kernel-attested identity it can get for something it did not fork
//! (`LOCAL_PEERPID` at accept), which is what `signal_handoff_candidate` needs
//! before it may aim anything at a candidate launchd owns.
//!
//! # The name, and the 16 bytes of headroom it has
//!
//! The socket is `<control dir>/seamless-<pid>-<32 hex nonce>.sock`. The prefix
//! is load-bearing twice over: `seamless::discard_outgoing` already sweeps every
//! entry beginning `seamless-<pid>-<nonce>`, so every rollback path unlinks this
//! socket for free, and the nonce makes the path unguessable and unique per
//! attempt so a bind can never collide with a concurrent one.
//!
//! Darwin's `MAX_SUN_PATH` is 103 and that name is 87 bytes before its
//! directory. Under the per-user control directory (`$HOME/Library/Application
//! Support/aterm`) that left 16 bytes for `$HOME`: `/Users//example` fit with three
//! to spare and `/Users//andrewyates` — i.e. macOS's default short name for most
//! people — did not, so most Macs silently took the fork lane the whole lane was
//! built to replace (2026-08-19 review). The socket therefore lives in a SHORT
//! per-uid private directory, [`rendezvous_dir`]: `$TMPDIR/aterm`, where `$TMPDIR`
//! is the per-user temporary directory launchd hands every process
//! (`/var/folders/…/T/`, owned by this uid, mode 0700 — a place no OTHER uid can
//! plant an entry in, unlike sticky `/tmp`, which is exactly why `/tmp` is never
//! used: a foreign uid could pre-create `/tmp/aterm-<uid>` as a symlink and either
//! capture our chmod or redirect the bind into a directory it can read). The base
//! is admitted only if `lstat` says it is a real directory owned by this uid and
//! not group/other-writable and it is not `/tmp`-rooted; the `aterm` child is
//! established with the lstat-hardened `aterm_update_core::ensure_private_dir`;
//! after the bind the node is re-proved to be our socket inside our directory; and
//! the DIALER proves the listener is same-uid (`getpeereid`) before it writes the
//! claim secret. The control directory remains the fallback if `$TMPDIR` cannot be
//! admitted, so the short-`$HOME` machines that always worked keep working. The
//! name carries 16 hex of the attempt nonce (unique, unguessable; the CLAIM secret
//! is separate and never on disk) so `<TMPDIR>/aterm/seamless-<pid>-<16 hex>.sock`
//! fits `sun_path` with room to spare. The composed path is still checked with
//! `control_auth::sun_path_ok` BEFORE the bind, and a refusal falls back to the
//! fork lane rather than proceeding — [`Rendezvous::bind`] answers
//! [`RendezvousError::PathTooLong`], and [`rendezvous_path_fits`] lets the lane
//! choice ask the same question before any nonce exists (without creating
//! anything). The prefix sweep in `seamless::discard_outgoing` runs over the
//! control directory, not this one, so [`Rendezvous::bind`] sweeps this
//! directory's leftovers itself: a socket whose embedded pid is dead is an attempt
//! that can never be claimed.
//!
//! # What "single-use" means here
//!
//! Exactly one connection is SERVED, exactly one grant carrying every
//! descriptor is sent (one message, or for more than 62 sessions the chunked
//! grant's few — see below), and then the socket is gone. There is no retry, no
//! second grant, and no verb. The one served connection may be HELD between the
//! claim and the grant (2026-09-19, the late park): the successor dials as soon
//! as it has booted, holding nothing, and waits — under its own grant budget —
//! for the outgoing process to park and send the grant, or for EOF, which is the
//! parent standing the attempt down.
//!
//! A peer that presents the wrong secret is refused — but refusing a CONNECTION
//! is not refusing the ATTEMPT, and an earlier shape here conflated the two:
//! whoever connected first got the single accept, so any same-uid process could
//! end an update by dialing before the successor did, for the price of one
//! `connect(2)`. The wait now resumes after a refusal and only the deadline ends
//! it, under the two bounds [`Rendezvous::accept_claim`] documents.
//!
//! This is still deliberately the opposite posture from the control socket,
//! which is a long-lived token-authenticated verb dispatcher — a handoff verb
//! there would outlive the attempt and be reachable by every same-uid process
//! for as long as the terminal ran.
//!
//! # Two independent gates on the peer
//!
//! * THE CLAIM SECRET — 32 CSPRNG bytes, published only in the successor's
//!   launch environment, compared in constant time. It binds the connection to
//!   THIS attempt.
//! * THE KERNEL-ATTESTED PEER PID — `LOCAL_PEERPID`, cross-checked against the
//!   pid LaunchServices reported for the instance it started. It binds the
//!   connection to THIS launch, and it is the half a peer cannot assert for
//!   itself — which is exactly what an environment-published secret lacks, since
//!   anything that could read our environment could also copy the secret.
//!
//! Both must pass; neither is redundant.
//!
//! # Why the adoption proof's fd-number term cannot come along
//!
//! `SCM_RIGHTS` hands the receiver the same OPEN FILE DESCRIPTIONS at whatever
//! descriptor NUMBERS it happens to have free — measured on this machine: three
//! masters sent, three different numbers received, the same PTY device minors.
//! The fd-number term in `seamless::adoption_proof` is a property of one
//! process's descriptor table, so under this transport the two sides would hash
//! different values and every handoff would end in `AdoptionMismatch`. The
//! replacement is [`pty_device_term`] — the PTY's own device number, which
//! `dup`/`SCM_RIGHTS` preserve because it is a property of the open file
//! description, and which a same-uid process cannot forge (minting a character
//! device with a chosen `st_rdev` needs `mknod(2)`, i.e. root).
//!
//! THE TRANSPORT SELECTS THE TERM, WITH NO NEGOTIATION. A parent can only put
//! descriptors out of band if it has out-of-band transport code, and a parent
//! already in the field does not — it sends `ATERM_SEAMLESS_FDS` and is answered
//! in v1 exactly as today. Presence of the transport IS the version, which makes
//! "no parent in the field can ever be shown v2" a structural fact rather than an
//! argument about gating. The other direction — a NEW parent handing off to an
//! OLD successor — is closed by the lane choice in `app_update_handoff`, which
//! refuses this lane unless the authorized `target_build` is at least this build.
//!
//! # The wire
//!
//! Two fixed-magic frames, both length-determined before any variable byte is
//! read, so neither side ever waits on a length a peer chose freely:
//!
//! ```text
//! successor -> parent   "ATRZ1C" + 64 hex claim chars           (70 bytes, fixed)
//! parent -> successor   "ATRZ1G" + u16be body length            (8 bytes, fixed)
//!                       + body: "<nonce>\n<lid>:<pid>,<lid>:<pid>\n"
//! ```
//!
//! The DESCRIPTORS ride the 8-byte header message — one `sendmsg`, so they are
//! all present the instant the receiver dequeues its first byte — in this order:
//! every PTY master in the body's order, then the readiness pipe's write end,
//! then the Commit pipe's read end.
//!
//! # The chunked grant (more than 62 sessions)
//!
//! One message carries at most [`fdpass::MAX_FDS`] descriptors, and two of them
//! are the pipes, so the grant above tops out at 62 sessions. A successor that
//! can take MORE says so in its claim, and only a parent that offered it the
//! capability hears that:
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
//! The header message carries the first descriptors, each continuation message
//! the next, at most [`fdpass::MAX_FDS`] per message and in the same order as the
//! one-message grant; the readiness and Commit ends always travel in the last one
//! ([`grant_chunk_sizes`] is the one layout both sides compute). The parent sends
//! `ATRZ2G` only after an `ATRZ2C` claim and only for more than 62 sessions: up to
//! 62 the grant is `ATRZ1G`, byte for byte, whatever the claim said, and an
//! `ATRZ1C` claimant is never sent `ATRZ2G` (it would refuse the magic) — a pool
//! it cannot take is refused before any descriptor leaves. The receiver takes
//! every message under the one grant deadline, requires exactly the layout for
//! the body's session count (so the total is sessions + 2, at most 258), and on
//! any refusal drops — closes — every descriptor it received. Position is ADDRESSING only (which received
//! descriptor is which `local_id`); it proves nothing, which is precisely why
//! the proof term is the device number and not the ordinal — an ordinal names a
//! slot, so it detects a permutation and never a substitution. The body carries
//! no descriptor numbers at all: a launched process inherits none, so any number
//! written here would name whatever LaunchServices left in the successor's table.
//!
//! # Every failure is a refusal
//!
//! Nothing here half-transfers a session. Before the send, no descriptor of ours
//! has left the process and the parent may roll back with no candidate to prove
//! anything about. After it, the successor holds copies and the ordinary
//! reject/reap path owns the outcome. There is no state in between, because the
//! descriptors and the header are one `sendmsg`. The chunked grant keeps that
//! line at its FIRST `sendmsg`: from there every failure, a continuation's
//! included, is the partial transfer the parent owes a proof for, and a
//! successor holding only some chunks refuses the grant and closes them all.
//!
//! # Platform
//!
//! macOS only, and gated as such at the `mod` declaration rather than by a
//! runtime check, for two independent reasons: the lane exists to mint a launchd
//! application job, and [`pty_device_term`] is a Darwin reading (see its docs).
//! Building it elsewhere would compile a transport nothing can take and a proof
//! term that would not distinguish.

use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, BorrowedFd, IntoRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::{CtlListener, CtlStream, fdpass};

/// The rendezvous socket path published to the successor. Its absence is what
/// makes the fork lane the fork lane, with no version byte anywhere.
pub(crate) const ENV_RENDEZVOUS: &str = crate::seamless::ENV_RENDEZVOUS;
/// The claim secret published to the successor.
pub(crate) const ENV_CLAIM: &str = crate::seamless::ENV_CLAIM;

/// Random bytes in the claim secret, and so half its hex length. The same width
/// `control_auth` mints capability tokens at: the secret only has to survive one
/// attempt, but there is no reason to spend less entropy on it than on a token
/// that lives for a whole session.
const CLAIM_SECRET_BYTES: usize = 32;
const CLAIM_HEX_LEN: usize = CLAIM_SECRET_BYTES * 2;
const CLAIM_MAGIC: &[u8; 6] = b"ATRZ1C";
const CLAIM_FRAME_LEN: usize = CLAIM_MAGIC.len() + CLAIM_HEX_LEN;
const GRANT_MAGIC: &[u8; 6] = b"ATRZ1G";
/// The claim of a successor that reads the CHUNKED grant: the same secret,
/// under a magic that says so. Presented only when the parent published
/// [`GRANT_CAPS_CHUNKS`] in [`ENV_GRANT_CAPS`].
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
pub(crate) const ENV_GRANT_CAPS: &str = "ATERM_HANDOFF_GRANT_CAPS";
/// The capability token for the chunked grant — also the value of the bundle's
/// `ATermHandoffGrantChunks` key (`aterm_update::HANDOFF_GRANT_CHUNKS_TOKEN`).
pub(crate) const GRANT_CAPS_CHUNKS: &str = "chunks1";

/// The most sessions a CHUNKED grant carries: the protocol's own ceiling.
pub(crate) const MAX_CHUNKED_SESSIONS: usize = crate::seamless::MAX_HANDOFF_SESSIONS;
/// The most descriptors one grant carries: every session plus the two pipes.
const MAX_GRANT_FDS: usize = MAX_CHUNKED_SESSIONS + RENDEZVOUS_CHANNEL_FDS;
/// The most messages one chunked grant spans ([`grant_chunk_sizes`] of
/// [`MAX_CHUNKED_SESSIONS`]); a header naming more is refused before its body.
const MAX_GRANT_CHUNKS: usize = MAX_GRANT_FDS.div_ceil(fdpass::MAX_FDS);
/// Magic plus a `u16` big-endian body length. FIXED, and read before the body,
/// so the receiver never sizes a buffer from a number it has not yet framed.
const GRANT_HEADER_LEN: usize = GRANT_MAGIC.len() + 2;
/// Ceiling on the grant body. The protocol's own 256-session limit cannot
/// approach it; it exists so a malformed length is refused before an allocation,
/// not to express a real budget.
const MAX_GRANT_BODY_BYTES: usize = 32 * 1024;

/// Descriptors this transport carries beyond the PTY masters: the readiness
/// pipe's write end and the Commit pipe's read end.
pub(crate) const RENDEZVOUS_CHANNEL_FDS: usize = 2;

/// The most sessions ONE-MESSAGE grant (`ATRZ1G`) hands over this lane.
///
/// `fdpass` carries at most [`fdpass::MAX_FDS`] descriptors in one message, and
/// this transport spends two of them on the readiness and Commit pipes. The
/// protocol's own ceiling (`seamless`'s `MAX_HANDOFF_SESSIONS`, 256) is four
/// times larger, so for a successor that did not declare the chunked grant THIS
/// is the binding constraint and the lane check has to test it
/// ([`grant_session_limit`]): a parent with 63 panes must fall back to the fork
/// lane rather than discover at `sendmsg` time that its handoff does not fit,
/// with the terminal already parked. A successor that declared it takes up to
/// [`MAX_CHUNKED_SESSIONS`].
pub(crate) const MAX_RENDEZVOUS_SESSIONS: usize = fdpass::MAX_FDS - RENDEZVOUS_CHANNEL_FDS;

/// Which of the two ceilings actually binds, asserted at COMPILE time because a
/// build where it flipped would be one where the lane check tests the wrong
/// number — and that failure surfaces at `sendmsg`, with the user's terminal
/// already parked, rather than as a fallback.
///
/// 256 is `seamless`'s `MAX_HANDOFF_SESSIONS`, restated rather than imported
/// because it is private there. If it ever drops below this transport's ceiling,
/// this stops compiling and the lane check has to start consulting both.
const _: () = assert!(
    MAX_RENDEZVOUS_SESSIONS < 256,
    "the transport, not the protocol, is what bounds this lane"
);

/// How many sessions ONE grant may carry: [`MAX_CHUNKED_SESSIONS`] when the
/// successor declared the chunked grant, [`MAX_RENDEZVOUS_SESSIONS`] otherwise.
/// The one number the lane choice, the park and [`ClaimedPeer::transfer`] all
/// bound the pool by.
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

/// THE PARENT'S GRANT DECISION for a dialer that claimed the chunked grant
/// (`chunks`, an `ATRZ2C` claim) or not, with `sessions` to hand over: the
/// one-message grant whenever one message carries them — whatever the claim —
/// the chunked grant only past that and only to an `ATRZ2C` claim, and a refusal
/// before any descriptor leaves otherwise. The `NativeUpdateRendezvousGrant`
/// model's `GrantOne` / `GrantChunks` / `RefuseTooMany`, and its Tier-1 bind.
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

/// THE CHUNKED GRANT'S LAYOUT for `sessions` sessions: how many descriptors each
/// message carries, in order. Every message but the last carries
/// [`fdpass::MAX_FDS`] masters; the last carries the rest of the masters and then
/// the readiness and Commit ends, and it stands alone when the masters left no
/// room for both. Both sides compute it — the parent to send, the successor to
/// require exactly what arrives — so there is no count on the wire to believe.
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

/// How long one `poll` slice waits before the accept loop re-checks the caller's
/// abort predicate and the deadline. Short enough that a cancel poke during a
/// long successor boot is noticed promptly, long enough that the loop is not a
/// spin.
const ACCEPT_POLL_SLICE: Duration = Duration::from_millis(10);

/// The most of the attempt's budget ONE connection may spend presenting its
/// claim frame.
///
/// The attempt deadline cannot be the only bound on that read: until the frame
/// is checked the peer is an unknown same-uid process, and an unknown peer that
/// connects and then says nothing would otherwise hold the whole handoff — the
/// user's terminal parked — for as long as it cared to. A real successor has
/// composed all 70 bytes before it dials and writes them in a single call, so
/// seconds is generous by orders of magnitude; the cost of being wrong is a
/// refused connection on a lane that keeps accepting, and the cost of having no
/// inner bound at all is the whole update.
const CLAIM_FRAME_BUDGET: Duration = Duration::from_secs(2);

/// How many connections may be refused before the attempt gives up.
///
/// Refusing and continuing is what stops one wrong dialer from denying the
/// update (see [`Rendezvous::accept_claim`]), but "keep accepting" must not
/// become "can be kept busy for free": a peer that connects and resets in a
/// tight loop would otherwise spin this thread for the rest of the deadline.
/// The cap is far above anything a real machine produces — the path is
/// unguessable, so the expected number of wrong dialers is zero — and it turns
/// a free denial into one an attacker has to sustain.
const MAX_REFUSED_DIALS: usize = 32;

/// The floor on a socket read/write timeout. `set_read_timeout(Some(ZERO))` is
/// an error in std ("cannot set a 0 duration timeout") and a deadline that has
/// just expired computes exactly that — so every remaining-time computation is
/// clamped here, and expiry is reported by the deadline check rather than by a
/// confusing `InvalidInput` from a socket option.
const MIN_IO_TIMEOUT: Duration = Duration::from_millis(1);

/// Why a rendezvous did not produce a transferred handoff.
///
/// Every variant is the caller's cue to keep the user's windows. The split is
/// finer than a string because the two sides want different things from it: the
/// parent maps some of these onto "fall back to the fork lane" and the rest onto
/// "roll this attempt back", while the successor treats every one of them as
/// "exit before starting a window" — a successor that cannot claim its handoff
/// must never present itself as a fresh terminal, because the parent still owns
/// every session it was going to adopt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RendezvousError {
    /// No per-user control directory: there is nowhere private to bind.
    NoControlDir,
    /// The composed path does not fit `sockaddr_un` on this platform. Both
    /// `bind` and `connect` would fail `EINVAL`, and a fail-safe liveness probe
    /// would misread that as "maybe live" — so it is named here instead.
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
    /// The caller's abort predicate fired. For the handoff worker that is a
    /// cancel poke — structural activity — and it is classified as such.
    Cancelled,
    /// The deadline expired with no usable dial. NOT proof that no successor
    /// exists; it IS proof that no descriptor of ours ever left this process.
    Deadline,
    /// `accept`, or reading the claim frame off the accepted connection, failed.
    Accept(String),
    /// A connection arrived and did not present this attempt's secret. Refused
    /// without a second chance: this socket serves one attempt.
    Claim,
    /// The kernel says the dialer is not the process LaunchServices started.
    PeerPid { expected: i32, actual: i32 },
    /// The kernel would not attest the peer at all.
    PeerUnattested(String),
    /// The transfer failed BEFORE the one descriptor-carrying `sendmsg`
    /// succeeded — an unencodable grant, a socket the write timeout would not
    /// take, or the `sendmsg` itself. No descriptor of ours left this process,
    /// which is the whole of what `NeverTransferred` rests on.
    TransferNotSent(String),
    /// The grant's own deadline (the parent's proof deadline) had passed when
    /// the grant was about to be sent: a successor DID dial and was held, and
    /// this process's preparation after the park outran the budget (round six
    /// of the update audit, item 48). Like [`Self::TransferNotSent`], no
    /// descriptor of ours left — but it is a deadline that ran out here, not a
    /// refusal, and it used to read as [`Self::Deadline`]'s "no successor
    /// dialed".
    GrantDeadline,
    /// The descriptors LEFT (the `sendmsg` succeeded) and a write after it — the
    /// tail of a short header send, the body, the flush — failed (2026-09-14).
    /// The peer holds duplicates of every master and both pipes; a successor
    /// that receives descriptors and no body refuses the grant and exits, but
    /// the parent may not ASSUME that: this is the ordinary kill-and-prove
    /// disposition, never `NeverTransferred`. Before this both shapes were one
    /// variant and the warrant's stated proof was not what the code established.
    TransferPartial(String),

    /// SUCCESSOR SIDE: this successor's own dial or grant budget
    /// ([`ClaimDeadlines`]) ran out before the grant arrived — what
    /// [`Self::Deadline`] means from THIS side, whose "no successor dialed"
    /// would be said by the successor that did (round six of the update
    /// audit, item 48).
    ClaimDeadline,
    /// SUCCESSOR SIDE: the launch environment carries no rendezvous.
    NoRendezvous,
    /// SUCCESSOR SIDE: exactly ONE of the two rendezvous variables is set.
    ///
    /// [`rendezvous_present`] deliberately answers yes to half an environment —
    /// half a rendezvous is corruption, not absence, and degrading it into "no
    /// handoff" would start a fresh window over a parent's live sessions. But it
    /// is not [`RendezvousError::NoRendezvous`] either, and the difference is not
    /// cosmetic: the caller wraps whatever comes back in "this launch carries a
    /// rendezvous but could not claim it (...)", so answering the absence error
    /// made the one line a failed update leaves behind contradict itself at
    /// exactly the moment someone is reading it to find out what happened. This
    /// variant says which half arrived and which did not.
    HalfRendezvous {
        present: &'static str,
        missing: &'static str,
    },
    /// SUCCESSOR SIDE: a rendezvous variable is present but unusable. Present
    /// and malformed is corruption, not absence, so it must not degrade into
    /// "no handoff" and start a fresh window over a parent's live sessions.
    Env(&'static str),
    /// SUCCESSOR SIDE: `connect` failed — most often because the parent already
    /// gave up and unlinked the socket, which is exactly how a late dial is
    /// meant to fail.
    Dial(String),
    /// SUCCESSOR SIDE: the grant frame was absent, short, mis-magicked, or its
    /// body did not parse.
    Grant(String),
    /// SUCCESSOR SIDE: the grant is for a different attempt than the manifest
    /// environment names.
    NonceMismatch,
    /// SUCCESSOR SIDE: the descriptor count does not match the body. Fatal
    /// rather than truncating — adopting a subset would hand the parent a proof
    /// over a session set it never authorized.
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

/// The PTY's own identity — the term the adoption proof hashes once the masters
/// travel out of band.
///
/// `fstat(2)`'s `st_rdev` on a PTY master. `/dev/ptmx` is a CLONING device, so
/// every open takes its own minor — that minor is the `/dev/ttysNNN` its slave
/// gets — and `dup`/`SCM_RIGHTS` hand over the same open file description and
/// therefore the same value. `st_ino`/`st_dev` are NOT usable in its place:
/// every master shares the single `/dev/ptmx` devfs node, so they are equal
/// across unrelated PTYs and a substitution would be invisible.
///
/// STRICTLY STRONGER THAN THE FD NUMBER IT REPLACES. A same-uid process cannot
/// make `fstat` lie about a descriptor the caller already holds, and to ANSWER a
/// given device number it has to hold that very PTY; the fd number, by contrast,
/// is an integer any process may put on any descriptor with `dup2`.
///
/// PLATFORM: this reading is a Darwin fact, and it is why this module is macOS
/// only. Linux exposes the same distinction as `ioctl(fd, TIOCGPTN)` while its
/// `st_rdev` is the one `/dev/ptmx` node for every master — the value would not
/// distinguish there. If this lane ever reaches another Unix, this function is
/// the thing that has to change first, and the lane must not be enabled there
/// until it has.
///
/// `dev_t` is `i32` on Darwin and `u64` on Linux, so the value is widened
/// through `i128` before narrowing: the same expression is then honest on both,
/// and neither can wrap into a plausible-looking wrong answer.
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

/// Re-express one attempt's adoption identities in the term THIS transport can
/// prove, leaving the identities themselves untouched.
///
/// The middle field of a `SessionIdentity` is a TRANSPORT COORDINATE: on the
/// fork lane it is the descriptor number both sides agree on because `execve`
/// copies the table verbatim, and here it is the PTY that descriptor names.
/// `local_id` and `pid` mean the same thing in any process and pass through
/// unchanged.
///
/// `None` when any master will not answer `fstat` — a refusal, not a degrade: a
/// proof missing one term would be a proof over a different session set than the
/// one being handed over.
#[must_use]
pub(crate) fn proof_identities_in_device_terms(
    identities: &[crate::seamless::SessionIdentity],
) -> Option<Vec<crate::seamless::SessionIdentity>> {
    identities
        .iter()
        .map(|(local_id, master, pid)| Some((*local_id, pty_device_term(*master)?, *pid)))
        .collect()
}

/// The rendezvous path one attempt binds, composed from the SAME pieces
/// `seamless::write_outgoing` names its manifest with — this process's pid and
/// the attempt nonce (its first 16 hex: unique and unguessable; the claim secret
/// is the authority and never touches the filesystem) — so the name is unique per
/// attempt and names its owner.
fn rendezvous_path(dir: &Path, nonce: &str) -> PathBuf {
    let short = &nonce[..nonce.len().min(RENDEZVOUS_NONCE_HEX)];
    dir.join(format!("seamless-{}-{short}.sock", std::process::id()))
}

/// How much of the attempt nonce the socket name carries.
const RENDEZVOUS_NONCE_HEX: usize = 16;

/// The per-user temporary base the rendezvous directory hangs off, ADMITTED only
/// when it is a place no other uid can plant an entry in: `lstat` says a real
/// directory (not a symlink), owned by this uid, not group/other-writable, and not
/// `/tmp`-rooted (sticky, world-writable — the one base that must never be used).
/// `std::env::temp_dir` is `$TMPDIR` with a `/tmp` fallback; launchd sets `$TMPDIR`
/// to the per-user `/var/folders/…/T/` for everything it starts, and anyone who can
/// set this process's environment is already inside its trust boundary. (Not
/// `confstr(_CS_DARWIN_USER_TEMP_DIR)`: see `aterm_pty::unix` for why that call is
/// unsafe in a process that forks.)
#[cfg(unix)]
fn admitted_temp_base() -> Option<PathBuf> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let base = std::env::temp_dir();
    // ABSOLUTE ONLY. `$TMPDIR` is whatever the environment says; a relative value
    // (`TMPDIR=tmp`) composes a relative socket path that binds fine against THIS
    // process's cwd and then cannot be dialed by a LaunchServices successor, whose
    // cwd is `/` — the parent parks its terminal for the full readiness deadline and
    // the fork fallback is never taken, on every attempt (2026-08-19 round-4 audit).
    if !base.is_absolute() {
        return None;
    }
    let canonical_tmp = |p: &Path| p == Path::new("/tmp") || p == Path::new("/private/tmp");
    if canonical_tmp(&base) || base.starts_with("/tmp") || base.starts_with("/private/tmp") {
        return None;
    }
    let meta = std::fs::symlink_metadata(&base).ok()?;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    if !meta.file_type().is_dir() || meta.uid() != uid || meta.permissions().mode() & 0o022 != 0 {
        return None;
    }
    Some(base)
}

/// The directory the rendezvous socket lives in, WITHOUT creating anything — the
/// lane-choice probe asks this every check and must not mutate the filesystem.
/// `None` means the control directory (the fallback).
#[must_use]
fn rendezvous_dir_candidate() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        admitted_temp_base().map(|base| base.join("aterm"))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The SHORT per-uid private directory the rendezvous socket lives in, established
/// (lstat-hardened: a symlink at the target is refused before anything touches it,
/// the final component must be a real directory owned by this uid and not
/// group/other-writable). Falls back to the control directory when `$TMPDIR`
/// cannot be admitted or the child cannot be established, so the short-`$HOME`
/// machines that always worked keep working.
#[must_use]
pub(crate) fn rendezvous_dir() -> Option<PathBuf> {
    if let Some(dir) = rendezvous_dir_candidate()
        && aterm_update_core::ensure_private_dir(&dir).is_ok()
    {
        return Some(dir);
    }
    // The fallback carries the same requirement: a relative path binds against THIS
    // process's cwd and cannot be dialed by a LaunchServices successor (cwd `/`).
    crate::control_auth::socket_dir().filter(|dir| dir.is_absolute())
}

/// After the bind: prove the node really is OUR socket inside OUR real directory —
/// the directory opened with `O_NOFOLLOW` (a symlink swapped in between the check
/// and the bind fails here), owned by this uid and not group/other-writable, and
/// the entry a socket owned by this uid. Cheap, and it is what turns the pre-bind
/// check from a race into a proof.
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
        if !name.ends_with(".sock") {
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

/// Whether a rendezvous path composed for THIS process fits `sun_path`, without
/// binding anything or minting a nonce.
///
/// The lane choice needs this answer before the worker exists, and the nonce is
/// not minted until the worker writes the manifest — so the probe runs against a
/// nonce-shaped placeholder. That is EXACT, not approximate: `seamless`'s nonce
/// is always 32 hex characters, so every path this process can compose has the
/// same length.
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

/// A bound, single-use rendezvous listener plus the secret that admits exactly
/// one dialer to it.
///
/// UNLINKS ON DROP. Every rollback path in the worker drops this, so the socket
/// is retired the moment the attempt stops needing it and a late dial fails
/// `ENOENT` — which is precisely how a successor that boots slower than the
/// parent's patience is meant to discover it has no handoff.
/// `seamless::discard_outgoing`'s prefix sweep covers the same ground for the
/// paths that route through `HandoffWorkerCleanup`, and the successor unlinks it
/// too after a successful claim. The redundancy is deliberate, and each copy
/// covers a gap the others cannot: on the SUCCESS path this process `_exit`s
/// inside `seamless::commit_and_exit` and runs no destructor at all — and the
/// cleanup funnel does not run there either.
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
        use std::os::unix::fs::PermissionsExt as _;

        let dir = rendezvous_dir().ok_or(RendezvousError::NoControlDir)?;
        #[cfg(unix)]
        sweep_dead_rendezvous(&dir);
        let path = rendezvous_path(&dir, nonce);
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
        prove_bound_socket_is_ours(&dir, &bound.path).map_err(RendezvousError::Bind)?;
        // The 0700 directory is already the access boundary; 0600 on the node
        // itself is the same defence in depth the control socket takes, and it
        // buys a refusal here rather than a surprise at connect time. From this
        // point the value owns the path, so a refusal unlinks by dropping.
        std::fs::set_permissions(&bound.path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| RendezvousError::Bind(error.to_string()))?;
        // The accept is deadline- and cancel-bounded, which a blocking `accept`
        // cannot be: it would park the worker thread past the point at which the
        // user's terminal has to be given back.
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

    /// Wait for the successor, check both gates, and hand back the connection.
    ///
    /// `expected_pid` is what LaunchServices reported for the instance it
    /// started. `abort` is polled between slices so a cancel poke does not have
    /// to wait out the whole deadline.
    ///
    /// A CONNECTION THAT FAILS EITHER GATE IS REFUSED; THE ATTEMPT IS NOT. Only
    /// the deadline, the abort predicate and a successful claim end the wait.
    /// The earlier shape ended it on the first bad connection, which read as
    /// prudence and was not: the socket is `0600` inside a `0700` directory, so a
    /// wrong dialer is already known to be same-uid, and the claim secret is what
    /// stops it TAKING the handoff. Letting it also END the handoff handed every
    /// same-uid process a denial of UPDATE costing one `connect(2)` — a strictly
    /// worse trade than closing that connection and going back to waiting. "This
    /// connection is not our successor" and "this attempt is over" are different
    /// statements, and the deadline is what may make the second one.
    ///
    /// The work a stranger can extract is bounded twice, because "keep accepting"
    /// must not become "can be kept busy for free": one connection may spend at
    /// most [`CLAIM_FRAME_BUDGET`] presenting its frame, and at most
    /// [`MAX_REFUSED_DIALS`] refusals are served before the attempt gives up on
    /// the last of them. Past that cap a denial is possible again, but it costs a
    /// sustained flood rather than a single connect, and the parent keeps every
    /// session either way.
    ///
    /// WHEN THE WAIT RUNS OUT the answer is the LAST REFUSAL if there was one and
    /// [`RendezvousError::Deadline`] otherwise. "No successor dialed" is simply
    /// false on a machine where three did and none knew the secret, and which of
    /// those two happened is the first thing an investigation needs.
    ///
    /// `expected_pid` is `None` when the LAUNCH ANSWER NEVER ARRIVED — which is a
    /// real state, not a hypothetical: LaunchServices answering slowly is not
    /// evidence that it launched nothing, and it was measured taking longer than
    /// its budget on a bundle's first launch while the successor it started went
    /// on to dial. Refusing that dial for want of a pid to compare it against
    /// would throw away a live, correct successor.
    ///
    /// Dropping the comparison costs nothing that matters. The claim secret is
    /// the authenticator — 32 bytes a stranger cannot guess, checked first — and
    /// the socket is a `0700` node inside a `0700` directory, so a dialer is
    /// already same-uid. The pid is DEFENCE IN DEPTH against our own confusion
    /// (two attempts overlapping), not the thing keeping strangers out. When it
    /// is available it is still compared; when it is not, the attested pid is
    /// returned to the caller either way.
    pub(crate) fn accept_claim(
        &self,
        expected_pid: Option<i32>,
        deadline: Instant,
        abort: &dyn Fn() -> bool,
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
            match self.gate_one(&stream, expected_pid, deadline) {
                Ok((pid, chunks)) => {
                    return Ok(ClaimedPeer {
                        stream,
                        pid,
                        chunks,
                    });
                }
                Err(refusal) => {
                    refusals += 1;
                    aterm_log::warn!(
                        "overlap handoff: refused rendezvous dialer #{refusals} ({refusal}); the \
                         rendezvous stays open for the successor until the deadline"
                    );
                    if refusals >= MAX_REFUSED_DIALS {
                        return Err(refusal);
                    }
                    last_refusal = Some(refusal);
                    // Dropping `stream` here — before the next accept — is what
                    // tells the refused dialer it was refused, and it is why a
                    // flood cannot accumulate connections we are still holding.
                }
            }
        }
    }

    /// Both gates over ONE accepted connection, answering the attested peer pid.
    ///
    /// Every error is a refusal of THIS CONNECTION. Nothing here may end the
    /// attempt, which is why the frame read is bounded by its own slice as well
    /// as by the attempt deadline: an expiry inside this function must mean "that
    /// peer took too long", not "the handoff is over".
    fn gate_one(
        &self,
        stream: &CtlStream,
        expected_pid: Option<i32>,
        deadline: Instant,
    ) -> Result<(i32, bool), RendezvousError> {
        // The TIGHTER of the two bounds wins. A peer that has not yet presented
        // the secret is an unknown same-uid process, and one of those must not be
        // able to spend the attempt's whole budget by connecting and saying
        // nothing — while a real successor writes this frame in one call.
        let frame_deadline = deadline.min(Instant::now() + CLAIM_FRAME_BUDGET);
        let mut frame = [0u8; CLAIM_FRAME_LEN];
        read_frame_by_deadline(stream, &mut frame, frame_deadline, &|error| {
            RendezvousError::Accept(error.to_string())
        })?;
        let Some(chunks) = claim_frame_kind(&frame, &self.claim) else {
            return Err(RendezvousError::Claim);
        };
        // Identity SECOND, and only once the secret has proven this is our
        // attempt's dialer. `peer_pid` is a cheap `getsockopt`, but ordering the
        // unforgeable check after the unguessable one means a stranger learns
        // nothing about which pid we were expecting.
        let attested = fdpass::peer_pid(stream)
            .map_err(|error| RendezvousError::PeerUnattested(error.to_string()))?;
        let attested = i32::try_from(attested).map_err(|_| RendezvousError::PeerPid {
            expected: expected_pid.unwrap_or(-1),
            actual: -1,
        })?;
        if let Some(expected) = expected_pid
            && attested != expected
        {
            return Err(RendezvousError::PeerPid {
                expected,
                actual: attested,
            });
        }
        // The claimed connection leaves here with a write bound already on it.
        // `transfer` refreshes it against the same deadline immediately before
        // the one `sendmsg`; this is the belt that makes "a socket this module
        // hands out can never block forever on a write" true by construction.
        stream
            .set_write_timeout(Some(remaining_io_budget(deadline)?))
            .map_err(|error| RendezvousError::Accept(error.to_string()))?;
        Ok((attested, chunks))
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
                Err(error) => return Err(RendezvousError::Accept(error.to_string())),
            }
        }
    }
}

impl Drop for Rendezvous {
    fn drop(&mut self) {
        // Closing the listener is what makes an IN-FLIGHT dial fail closed; the
        // unlink is what makes a dial that has not happened yet fail closed. The
        // path is prefix-bound to this process's pid and this attempt's nonce,
        // so this can never remove anything another attempt owns.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A connection that passed both gates: it presented this attempt's secret and
/// the kernel says it is the launched successor.
pub(crate) struct ClaimedPeer {
    stream: CtlStream,
    pid: i32,
    /// The dialer claimed with `ATRZ2C`: it reads the chunked grant.
    chunks: bool,
}

impl ClaimedPeer {
    /// Whether the held dialer has gone: one zero-timeout `poll` for HUP/ERR/NVAL
    /// on the served stream. The parent's witness, while it HOLDS a claim between
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

    /// The most sessions [`Self::transfer`] can hand THIS dialer: what its
    /// claim (`ATRZ2C` or `ATRZ1C`) says it reads, never what the parent hoped
    /// to offer it. The park compares its pool against this BEFORE it freezes
    /// anything, so a claim that cannot carry the desk stands the attempt down
    /// with every reader live instead of after the capture.
    #[must_use]
    pub(crate) fn grant_session_limit(&self) -> usize {
        grant_session_limit(self.chunks)
    }

    /// Send every descriptor of the handoff in ONE message, then the body that
    /// says which is which.
    ///
    /// `sessions` is `(local_id, shell pid, master)` in TRANSFER ORDER, and that
    /// order is the addressing: the successor pairs its Nth received descriptor
    /// with the Nth body entry. The readiness and Commit descriptors follow the
    /// masters, in that order.
    ///
    /// ONE `sendmsg` FOR ALL DESCRIPTORS is what makes this all-or-nothing.
    /// There is no state in which the successor holds some masters and not
    /// others, so the parent's two dispositions — "no descriptor of ours ever
    /// left" and "the successor has everything" — are the only two that exist.
    /// The body that follows is ordinary stream bytes; a successor that received
    /// descriptors but no body cannot use them (it refuses the grant and exits),
    /// and this process learns that as proof EOF exactly as it would for any
    /// other refusal.
    ///
    /// MORE THAN 62 SESSIONS take the CHUNKED grant, and only for a dialer that
    /// claimed it (`ATRZ2C`); an `ATRZ1C` dialer is refused
    /// [`RendezvousError::TooManySessions`] before anything is sent. The chunked
    /// grant keeps both dispositions: its FIRST `sendmsg` is still the line
    /// before which no descriptor has left ([`RendezvousError::TransferNotSent`]),
    /// and every failure after it — a continuation included — is
    /// [`RendezvousError::TransferPartial`], the kill-and-prove disposition. A
    /// successor holding only some chunks refuses the grant and closes them all.
    /// Up to 62 sessions the grant is the one-message `ATRZ1G`, byte for byte,
    /// whatever the dialer claimed.
    pub(crate) fn transfer(
        &self,
        nonce: &str,
        sessions: &[(u64, i32, BorrowedFd<'_>)],
        ready: BorrowedFd<'_>,
        commit: BorrowedFd<'_>,
        deadline: Instant,
    ) -> Result<(), RendezvousError> {
        let shape = grant_shape(self.chunks, sessions.len())?;
        let malformed = || {
            RendezvousError::TransferNotSent("the grant body exceeds its wire format".to_string())
        };
        let body = encode_grant_body(nonce, sessions).ok_or_else(malformed)?;
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
        let sent = fdpass::send_with_fds(&self.stream, &header, &descriptors)
            .map_err(|error| RendezvousError::TransferNotSent(error.to_string()))?;
        // FROM HERE THE DESCRIPTORS ARE THE PEER'S TOO: every failure below is a
        // partial transfer, and the caller owes the candidate a proof of death
        // before it resumes its readers.
        //
        // A short send is possible in principle on a stream socket, and the
        // descriptors went with the first byte — so the remainder is finished
        // with an ordinary write rather than resent.
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

    /// The CHUNKED grant (see [`Self::transfer`] and the module docs): the
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
        let sent = fdpass::send_with_fds(&self.stream, &header, chunks[0])
            .map_err(|error| RendezvousError::TransferNotSent(error.to_string()))?;
        // FROM HERE THE FIRST CHUNK IS THE PEER'S: every failure below, in a
        // continuation too, is a partial transfer the caller owes a proof for.
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

/// The claim frame a successor presents.
/// A test's dialer: connect to `path` and present `secret`, keeping the stream
/// so the accepted claim can be HELD by the listener (the late park's shape)
/// and closed from that side. The production dialer is `dial_and_claim`.
#[cfg(test)]
pub(crate) fn dial_for_test(path: &Path, secret: &str) -> std::io::Result<CtlStream> {
    let stream = CtlStream::connect(path)?;
    (&stream).write_all(&claim_frame(secret))?;
    (&stream).flush()?;
    Ok(stream)
}

/// The cross-version guard's door onto this build's SUCCESSOR side
/// (`seamless::fixture_tests`): dial `path`, present `secret` as this build
/// presents it to a parent that published no grant capabilities (every shipped
/// parent), and answer the `ATERM_SEAMLESS_FDS` wire it would publish together
/// with the descriptors it received — masters in wire order, then the readiness
/// and Commit ends.
#[cfg(test)]
pub(crate) fn dial_and_claim_for_fixture(
    path: &Path,
    secret: &str,
    expected_nonce: &str,
    deadlines: ClaimDeadlines,
) -> Result<(String, Vec<OwnedFd>, OwnedFd, OwnedFd), RendezvousError> {
    // The stub parent listens in THIS process, so this process is the attested
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
/// whether its dialer claimed the CHUNKED grant: `Some(false)` for `ATRZ1C`,
/// `Some(true)` for `ATRZ2C`, `None` for anything else.
///
/// The magic is compared ordinarily — it is a constant, and leaking that a peer
/// got a fixed prefix wrong tells nobody anything they did not already choose.
/// The SECRET is compared in constant time, because that comparison's timing is
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
    crate::control_auth::constant_time_eq(&frame[CLAIM_MAGIC.len()..], secret.as_bytes())
        .then_some(chunks)
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

/// `"<nonce>\n<lid>:<pid>,<lid>:<pid>\n"` — everything the successor needs to
/// pair a received descriptor with a session, and nothing that names a
/// descriptor.
fn encode_grant_body(nonce: &str, sessions: &[(u64, i32, BorrowedFd<'_>)]) -> Option<String> {
    if nonce.len() != 32 || !nonce.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let entries = sessions
        .iter()
        .map(|(local_id, pid, _)| format!("{local_id}:{pid}"))
        .collect::<Vec<_>>()
        .join(",");
    let body = format!("{nonce}\n{entries}\n");
    (body.len() <= MAX_GRANT_BODY_BYTES).then_some(body)
}

/// Parse a grant body into `(local_id, shell pid)` in transfer order.
///
/// Total and allocation-bounded: the body was already length-framed by the
/// header, so nothing here reserves from a number the body itself chose.
fn parse_grant_body(body: &str, expected_nonce: &str) -> Result<Vec<(u64, i32)>, RendezvousError> {
    let Some((nonce, rest)) = body.split_once('\n') else {
        return Err(RendezvousError::Grant("no nonce line".to_string()));
    };
    if nonce != expected_nonce {
        return Err(RendezvousError::NonceMismatch);
    }
    let Some(entries) = rest.strip_suffix('\n') else {
        return Err(RendezvousError::Grant(
            "unterminated session list".to_string(),
        ));
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
    Ok(sessions)
}

/// What a successor holds after a successful claim: the descriptors, and the
/// `ATERM_SEAMLESS_FDS` wire that names them by the numbers THIS process
/// received them at.
///
/// The wire is built here rather than by the caller because this is the only
/// place that knows both halves — the body's `(local_id, pid)` order and the
/// descriptor numbers the kernel chose. Installing it in the handoff snapshot
/// ([`ClaimedIntake::install`]) lets the unchanged `seamless::take_incoming_from`
/// perform every authentication it already performs: the manifest join, the
/// bijection check, the tty backstop, the screen-carry digest. The TRANSPORT
/// changed; nothing that decides whether a handoff is legitimate did.
pub(crate) struct ClaimedHandoff {
    fds_wire: String,
    masters: Vec<OwnedFd>,
    ready: OwnedFd,
    commit: OwnedFd,
}

impl ClaimedHandoff {
    /// How many sessions the grant carried — for the one log line that tells a
    /// field investigation which lane this process arrived on.
    #[must_use]
    pub(crate) fn session_count(&self) -> usize {
        self.masters.len()
    }

    /// The claim as an intake: the wire and the owned descriptors, in memory.
    /// Until the warm-successor P1 this was `publish`, which wrote the three
    /// names into the PROCESS environment — sound only while no other thread
    /// could exist (docs/DESIGN-warm-successor-2026-09-29.md §1).
    #[must_use]
    pub(crate) fn into_intake(self) -> ClaimedIntake {
        let Self {
            fds_wire,
            masters,
            ready,
            commit,
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
    /// Land the claimed descriptors in the handoff snapshot, where the fork
    /// lane's arrived: the same three names (`ATERM_SEAMLESS_FDS`,
    /// `ATERM_HANDOFF_READY_FD`, `ATERM_HANDOFF_COMMIT_FD`) with the same
    /// strings `publish` wrote into `environ` — so `seamless::take_incoming_from`,
    /// `take_ready_fd_from` and `take_commit_fd_from` run the unchanged
    /// authentication over them.
    ///
    /// The descriptors are RELEASED here: `seamless`'s consumers take ownership
    /// exactly as they do on the fork lane, where the numbers arrived through
    /// `execve` with no Rust owner at all. That is why this consumes `self` —
    /// afterwards the only owners are the ones those intakes construct, and each
    /// of them closes what it refuses. No thread can observe `env` (it is this
    /// process's own memory), so unlike the `set_var` it replaces this is sound
    /// wherever the claim sits.
    #[cfg_attr(
        test,
        aterm_spec::refines(
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

/// Whether this launch carries a rendezvous at all — the successor's one test
/// for which lane it arrived on.
///
/// AN OR, DELIBERATELY, while [`claim_incoming`] needs BOTH names. Half an
/// environment is corruption rather than absence: something published one of the
/// two, so this process may be a successor, and a successor that starts a fresh
/// window has put an empty terminal on top of a parent's live sessions. So half
/// counts as present, this function sends the launch down the claim path, and the
/// claim path refuses it with [`RendezvousError::HalfRendezvous`] — which SAYS
/// half rather than reporting the absence that `claim_incoming` used to answer,
/// because the caller's log line quotes that error inside "this launch carries a
/// rendezvous but could not claim it", and "carries no rendezvous" contradicted
/// the sentence it was quoted into.
#[must_use]
pub(crate) fn rendezvous_present(env: &crate::handoff_env::HandoffEnv) -> bool {
    env.present(ENV_RENDEZVOUS) || env.present(ENV_CLAIM)
}

/// The parent's explicit statement of which PROOF TERM its expected adoption
/// proof was hashed over: `device` (PTY `st_rdev`, the launched lane's term) or
/// `fd` (descriptor numbers, the fork lane's). Consumed once at boot. A launched
/// attempt that falls back to the fork lane keeps its device-term proof, so the
/// successor must not infer the term from how the descriptors arrived.
pub(crate) const ENV_PROOF_TERM: &str = "ATERM_HANDOFF_PROOF_TERM";

/// Consume [`ENV_PROOF_TERM`]: `Some(true)` for device terms, `Some(false)` for fd
/// numbers, `None` when the parent said nothing (an older parent — infer from the
/// lane as before).
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

/// Dial the rendezvous named in this launch's handoff snapshot, present the
/// claim, and take delivery of every descriptor.
///
/// CONSUMES both names — and the parent's grant capabilities
/// ([`ENV_GRANT_CAPS`]) — out of `env` whether or not it succeeds, so no second
/// attempt can observe them (they left the process environment at
/// `HandoffEnv::capture`, so no helper or user shell ever could). On success it also
/// unlinks the socket: the parent `_exit`s inside `seamless::commit_and_exit`
/// and runs no destructor, so on the one path that matters this is the only
/// thing that retires the node.
///
/// `expected_nonce` is the attempt nonce this process was told about in
/// `ATERM_SEAMLESS_NONCE`; a grant naming any other attempt is refused.
/// `parent` is the outgoing process `seamless::prearm_incoming_fds` attested;
/// the claim is presented only to a listener the kernel says IS that process
/// (see [`listener_may_receive_claim`]), and `None` refuses.
///
/// No process-environment access: callable from any thread.
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "native_update_successor_warm_before_claim",
        action = "Dial",
        project = "aterm_gui::seamless::handoff_env_conformance::project"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
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
    // ROOM FOR THE GRANT, BEFORE THE DIAL (round six of the update audit,
    // item 39): see `raise_descriptor_limit_for_claim`. A failure is said and
    // the claim goes ahead — it fails exactly as it would have.
    match raise_descriptor_limit_for_claim() {
        Ok(Some((from, to))) => aterm_log::info!(
            "overlap handoff: raised this successor's soft descriptor limit from {from} to {to} \
             for the grant and the sessions it adopts"
        ),
        Ok(None) => {}
        Err(error) => aterm_log::warn!(
            "overlap handoff: could not raise this successor's soft descriptor limit ({error}); \
             a large grant may not fit it"
        ),
    }
    let claimed = dial_and_claim(&path, &secret, expected_nonce, deadlines, parent, chunks)
        .map_err(|error| match error {
            RendezvousError::Deadline => RendezvousError::ClaimDeadline,
            other => other,
        });
    if claimed.is_ok() {
        let _ = std::fs::remove_file(&path);
    }
    claimed
}

/// The soft `RLIMIT_NOFILE` a launched successor raises itself to before it
/// dials (never above its hard limit).
///
/// A LaunchServices-launched app runs at launchd's soft limit, 256 — not the
/// parent's, which a forked successor inherits. A grant of up to
/// [`MAX_CHUNKED_SESSIONS`] masters, and then a wake pipe per adopted session,
/// needs about three descriptors a session: at 256 the later chunks'
/// descriptors could not be received at all (the kernel discards rights past
/// the limit and the receive fails `EMSGSIZE`), and at a few dozen sessions
/// fewer every later open failed `EMFILE` (round six of the update audit,
/// item 39). 4096 covers the largest grant with room to spare, and stays under
/// Darwin's `OPEN_MAX` (10240), past which `setrlimit` refuses.
const SUCCESSOR_DESCRIPTOR_FLOOR: u64 = 4096;

/// The soft limit to raise to from `soft` under `hard`, or `None` when it is
/// already at least [`SUCCESSOR_DESCRIPTOR_FLOOR`] (or the hard limit).
fn raised_soft_limit(soft: u64, hard: u64) -> Option<u64> {
    let target = SUCCESSOR_DESCRIPTOR_FLOOR.min(hard);
    (target > soft).then_some(target)
}

/// The soft `RLIMIT_NOFILE` this process was LAUNCHED with, recorded by the
/// first [`raise_descriptor_limit_for_claim`] that moved it; unset when the
/// limit was never raised (a forked successor, a Dock launch, a limit that
/// already had room).
static LAUNCHER_SOFT_NOFILE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// The soft descriptor limit a shell this process spawns must be handed back
/// (round six of the update audit, item 39, review round two): `Some(soft)`
/// when the claim raised this process's own limit from `soft`, `None` when it
/// never did and the process's limit is still the launcher's.
///
/// The raise is the SUCCESSOR's need — room for the grant and the sessions it
/// adopts — never its shells'. A terminal hands its shell the launcher's soft
/// limits in both directions (`aterm_cli`'s `session_limits` records why), so a
/// tab opened after an update through the launched lane must see launchd's 256,
/// exactly as a Dock-launched aterm's does, and not the 4096 this process
/// raised itself to.
pub(crate) fn launcher_soft_descriptor_limit() -> Option<u64> {
    LAUNCHER_SOFT_NOFILE.get().copied()
}

/// The launch variable in which a FORK-LANE parent tells its successor the
/// soft descriptor limit its shells are owed (round seven, item 107): the
/// launcher's, when [`launcher_soft_descriptor_limit`] knows it. A forked
/// successor inherits the parent's RAISED soft limit and never claims, so
/// without this every tab it opened got 4096 back — the launcher's 256 held
/// for one hop only. Read by [`adopt_carried_launcher_limit`].
pub(crate) const ENV_LAUNCHER_NOFILE: &str = "ATERM_HANDOFF_LAUNCHER_NOFILE";

/// What a fork-lane parent names on its successor's command for
/// [`ENV_LAUNCHER_NOFILE`]: `None` when this process never raised its limit,
/// so its shells already get what it runs at.
#[must_use]
pub(crate) fn launcher_limit_env() -> Option<(&'static str, String)> {
    launcher_soft_descriptor_limit().map(|soft| (ENV_LAUNCHER_NOFILE, soft.to_string()))
}

/// A carried launcher limit this process may adopt: a number, and never above
/// the soft limit it runs at (`soft`) — it only ever gives the shells LESS
/// than this process has, which is the one direction the raise moved.
fn carried_launcher_limit(carried: Option<&str>, soft: u64) -> Option<u64> {
    carried?
        .parse::<u64>()
        .ok()
        .filter(|carried| *carried > 0 && *carried <= soft)
}

/// Record the launcher limit a fork-lane parent carried
/// ([`ENV_LAUNCHER_NOFILE`]) as this process's own
/// [`launcher_soft_descriptor_limit`], before any shell is spawned. Read, not
/// consumed: a boot re-exec carries the snapshot on, and the final image reads
/// it again. `Some(limit)` when it was adopted.
pub(crate) fn adopt_carried_launcher_limit(env: &crate::handoff_env::HandoffEnv) -> Option<u64> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a valid resource id and an exclusively borrowed, writable
    // out-parameter for the one call.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return None;
    }
    let carried =
        carried_launcher_limit(env.string(ENV_LAUNCHER_NOFILE).as_deref(), limit.rlim_cur)?;
    LAUNCHER_SOFT_NOFILE.set(carried).ok()?;
    Some(carried)
}

/// Raise this process's soft `RLIMIT_NOFILE` to [`raised_soft_limit`]'s
/// answer: `Ok(Some((from, to)))` when it moved, `Ok(None)` when it had room
/// already. Raising a soft limit up to the hard one needs no privilege. The
/// limit it moved FROM is recorded for [`launcher_soft_descriptor_limit`].
fn raise_descriptor_limit_for_claim() -> std::io::Result<Option<(u64, u64)>> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a valid resource id and an exclusively borrowed, writable
    // out-parameter for the one call.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let (soft, hard) = (limit.rlim_cur, limit.rlim_max);
    let Some(target) = raised_soft_limit(soft, hard) else {
        return Ok(None);
    };
    let raised = libc::rlimit {
        rlim_cur: target,
        rlim_max: limit.rlim_max,
    };
    // SAFETY: a valid resource id and a fully initialised rlimit that keeps
    // the hard limit as it was.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const raised) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // The first raise saw the launcher's value; a later one cannot happen (the
    // limit is at the floor or the hard limit now), and would not be it.
    let _ = LAUNCHER_SOFT_NOFILE.set(soft);
    Ok(Some((soft, target)))
}

/// Whether the parent's [`ENV_GRANT_CAPS`] offers the chunked grant: its
/// comma-separated list names [`GRANT_CAPS_CHUNKS`]. Absent or anything else:
/// no — this successor then claims `ATRZ1C`, as every successor before the
/// chunked grant did.
fn offers_chunked_grant(caps: Option<&std::ffi::OsStr>) -> bool {
    caps.and_then(std::ffi::OsStr::to_str)
        .is_some_and(|caps| caps.split(',').any(|cap| cap.trim() == GRANT_CAPS_CHUNKS))
}

/// Decide what a launch environment IS, from the two values already taken out of
/// it: a dialable rendezvous, no rendezvous, or half of one.
///
/// Split out of [`claim_incoming`] so this decision — the one a failed update's
/// log line quotes — can be exercised over its exact inputs. Reaching it through
/// the process environment instead would mean mutating two process-global names
/// inside a test binary that runs hundreds of tests on parallel threads, where
/// `seamless`'s own env tests read these very names.
fn rendezvous_env(
    path: Option<std::ffi::OsString>,
    secret: Option<std::ffi::OsString>,
) -> Result<(PathBuf, String), RendezvousError> {
    let (path, secret) = match (path, secret) {
        (Some(path), Some(secret)) => (path, secret),
        (None, None) => return Err(RendezvousError::NoRendezvous),
        // Half is not absence, and must not be reported as it. Whatever produced
        // one name may well have produced this process, so the launch is refused
        // — loudly, and in the words of what actually happened.
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
    if secret.len() != CLAIM_HEX_LEN || !secret.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return Err(RendezvousError::Env(ENV_CLAIM));
    }
    Ok((PathBuf::from(path), secret))
}

/// The two budgets of a dial (2026-09-19, the late park). `dial` bounds the
/// connect, the uid check and the claim write — the parent is holding the
/// listener open and answers at once, or it has given up and the connect fails
/// at once, so this is short. `grant` bounds the wait for the one descriptor
/// message: long, because the outgoing process parks only at a quiet moment
/// AFTER the successor has dialled, and holds the claim meanwhile; EOF ends it
/// at once either way.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClaimDeadlines {
    pub dial: Instant,
    pub grant: Instant,
}

/// THE LISTENER MUST BE THE ATTESTED PARENT before the claim secret leaves this
/// process. The environment named a PATH, and a path is not an identity; the
/// parental attestation named a PROCESS. So the kernel is asked both halves about
/// the listener this dial reached, and both must answer for that process:
///
/// * its uid (`getpeereid`) is ours — a listener of any other uid, whatever put
///   it there, gets nothing;
/// * its pid (`LOCAL_PEERPID`, read on the DIALING end: XNU copies it from the
///   listening socket at `connect(2)`, before any `accept`, and no peer can
///   assert it for itself) is the attested parent's, and that parent is still
///   the process attested ([`crate::seamless::AttestedParent::owns_live_listener`]:
///   the birth witness must still match, so a recycled pid fails).
///
/// Before 2026-09-27 only the uid half ran, so any same-uid process listening at
/// the published path received the claim: the birth record that attests the
/// parent is copyable (`ps`), and the listener was never compared with it. The
/// refusal is fail-closed and cheap — the successor exits before any window
/// (its caller's "could not be claimed" arm) and the outgoing process, which
/// never saw a claim, keeps every session. `NativeUpdateRendezvousClaim`
/// (aterm-spec) is the model; `claim_presentation_conformance` below binds this
/// function to it.
#[cfg_attr(
    test,
    aterm_spec::refines(
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
    // The header is read by the descriptor-bearing receive. `fdpass` documents
    // that ALL of a message's descriptors arrive on the call that dequeues its
    // first byte, so a short read here could cost bytes and never descriptors —
    // and the buffer is the one-message header, which is also the chunked
    // header's first eight bytes. `max_fds` is the message ceiling because the
    // exact count is in the body, which has not been read yet; the exact check
    // happens below, and an over-count is `fdpass`'s own fatal `InvalidData`.
    //
    // The read bound is RECOMPUTED here rather than inherited from the one set
    // above: a socket timeout is spent per syscall, so the write and this receive
    // would otherwise be allowed the full budget each.
    let mut header = [0u8; GRANT_HEADER_LEN];
    arm_read_timeout(&stream, remaining_io_budget(deadlines.grant)?).map_err(grant_failed)?;
    let received = fdpass::recv_with_fds(&stream, &mut header, fdpass::MAX_FDS)
        .map_err(|error| RendezvousError::Grant(error.to_string()))?;
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
    // WHICH GRANT. The chunked one only after this process claimed it: a
    // successor that claimed `ATRZ1C` refuses `ATRZ2G` as a wrong magic, exactly
    // as every successor before the chunked grant does.
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
    let sessions = parse_grant_body(&body, expected_nonce)?;

    let expected = sessions.len() + RENDEZVOUS_CHANNEL_FDS;
    if let Some(count) = chunk_count {
        // THE LAYOUT IS COMPUTED, NOT BELIEVED: the body's session count fixes
        // how many messages there are and what each carries. Every message is
        // required to be exactly that, under the one grant deadline, and the
        // first refusal drops — closes — everything received so far.
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
    })
}

/// One descriptor-bearing receive of a fixed frame, under ONE deadline: a
/// receive that times out or is interrupted before anything was dequeued is
/// tried again with the budget re-derived from the deadline, and the deadline
/// itself is the only thing that ends the wait. `fdpass` makes the retry safe —
/// an interrupted or timed-out `recvmsg` dequeued nothing, descriptors included.
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

/// Arm `stream`'s read timeout for the next receive. A socket whose BOTH
/// directions are already shut — the peer sent its last bytes and closed, as
/// the outgoing process does the instant its grant is sent — makes Darwin's
/// `setsockopt(SO_RCVTIMEO)` fail with EINVAL. Such a socket cannot block a
/// read (it returns what is queued, then 0), so the timeout is not needed and
/// the refusal is not a failure of the exchange: the read goes ahead and says
/// what is really there. Any other error is returned.
fn arm_read_timeout(stream: &CtlStream, timeout: Duration) -> std::io::Result<()> {
    match stream.set_read_timeout(Some(timeout)) {
        Err(error) if error.raw_os_error() == Some(libc::EINVAL) => Ok(()),
        other => other,
    }
}

/// Read exactly `buf.len()` bytes, with ONE deadline over the WHOLE frame.
///
/// `set_read_timeout` bounds a SYSCALL, not an operation, and `read_exact` under
/// it restarts that clock on every short read. A peer that dribbles one byte just
/// before each expiry therefore stretched a fixed 70-byte claim frame to about 70
/// times the budget it was supposed to fit in — with the user's terminal parked
/// for all of it, and BEFORE either gate has run, since the claim frame is read
/// before the pid check. A timeout is not a deadline; this is the deadline. The
/// remaining budget is recomputed before every read, so the frame costs what the
/// deadline allows no matter how the peer chops it up.
///
/// `wrap` names the side: the same expiry is an `Accept` refusal for the parent
/// and a `Grant` refusal for the successor, while [`RendezvousError::Deadline`]
/// comes straight from the budget and means the same thing on both.
fn read_frame_by_deadline(
    stream: &CtlStream,
    buf: &mut [u8],
    deadline: Instant,
    wrap: &dyn Fn(std::io::Error) -> RendezvousError,
) -> Result<(), RendezvousError> {
    let mut source = stream;
    let mut filled = 0usize;
    while filled < buf.len() {
        // The budget is re-derived from the deadline HERE, so the loop cannot
        // hand the peer a fresh full timeout for the next byte.
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

/// The socket timeout to spend on the next framed exchange, or the deadline
/// error when there is none left. Clamped away from zero because std refuses a
/// zero-duration timeout, and an expired deadline has to read as an expired
/// deadline rather than as `InvalidInput` from a socket option.
/// [`remaining_io_budget`] for the parent's grant, before its first `sendmsg`:
/// the deadline running out THERE is this process's own lateness after a dial,
/// never "no successor dialed" ([`RendezvousError::GrantDeadline`]).
fn grant_budget(deadline: Instant) -> Result<Duration, RendezvousError> {
    remaining_io_budget(deadline).map_err(|_| RendezvousError::GrantDeadline)
}

fn remaining_io_budget(deadline: Instant) -> Result<Duration, RendezvousError> {
    let now = Instant::now();
    if now >= deadline {
        return Err(RendezvousError::Deadline);
    }
    Ok(deadline.saturating_duration_since(now).max(MIN_IO_TIMEOUT))
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

    /// THE BUDGET THAT DECIDES THE LANE. `fdpass` carries 64 descriptors and
    /// this transport spends two on the pipes, so the protocol's own 256-session
    /// ceiling is not the binding one — and a lane check testing the wrong
    /// ceiling would discover the truth at `sendmsg` time, with the terminal
    /// already parked.
    #[test]
    fn the_session_ceiling_is_the_message_ceiling_minus_the_two_pipes() {
        assert_eq!(MAX_RENDEZVOUS_SESSIONS, fdpass::MAX_FDS - 2);
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
    }

    /// A secret of the wrong length cannot be smuggled through the bounded copy
    /// that builds the frame: the tail stays zero, and zero is not hex.
    #[test]
    fn an_undersized_secret_cannot_produce_a_matching_frame() {
        let short = "abc";
        assert!(!claim_frame_matches(&claim_frame(short), short));
    }

    #[test]
    fn a_grant_body_round_trips_in_transfer_order() {
        let held = std::io::stdin();
        let borrowed = held.as_fd();
        let sessions = vec![(7u64, 4242i32, borrowed), (3u64, 99i32, borrowed)];
        let body = encode_grant_body(TEST_NONCE, &sessions).expect("encodes");
        assert_eq!(body, format!("{TEST_NONCE}\n7:4242,3:99\n"));
        assert_eq!(
            parse_grant_body(&body, TEST_NONCE).expect("parses"),
            vec![(7, 4242), (3, 99)],
            "order is preserved, because order IS the addressing"
        );
    }

    #[test]
    fn a_grant_for_another_attempt_is_refused() {
        let body = format!("{TEST_NONCE}\n1:2\n");
        assert_eq!(
            parse_grant_body(&body, &"f".repeat(32)),
            Err(RendezvousError::NonceMismatch)
        );
    }

    #[test]
    fn a_malformed_grant_body_is_refused_rather_than_partially_believed() {
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
        assert!(encode_grant_body("short", &sessions).is_none());
        assert!(encode_grant_body(&"z".repeat(32), &sessions).is_none());
        assert!(encode_grant_body(TEST_NONCE, &sessions).is_some());
    }

    /// The device term must DISTINGUISH masters and SURVIVE a duplicate — the
    /// two halves that make it a usable replacement for the fd number. The
    /// `SCM_RIGHTS` half is asserted by
    /// [`a_claimed_rendezvous_carries_every_descriptor_to_the_dialer`] below,
    /// over a real transfer rather than as an observation.
    #[test]
    fn the_device_term_distinguishes_masters_and_survives_dup() {
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
        for ((master, _), term) in ptys.iter().zip(terms.iter()) {
            // SAFETY: duplicating a live descriptor this test owns.
            let duplicate = unsafe { libc::fcntl(*master, libc::F_DUPFD_CLOEXEC, 3) };
            assert!(duplicate >= 0, "dup a live master");
            assert_eq!(
                pty_device_term(duplicate),
                Some(*term),
                "a duplicate names the same PTY, which is the whole point of the term"
            );
            aterm_pty::close_fd(duplicate);
        }
        for (master, slave) in ptys {
            aterm_pty::close_fd(master);
            aterm_pty::close_fd(slave);
        }
    }

    #[test]
    fn a_closed_or_absent_descriptor_has_no_device_term() {
        assert_eq!(pty_device_term(-1), None);
        // A number this process has never opened cannot answer `fstat`.
        assert_eq!(pty_device_term(i32::MAX - 1), None);
    }

    /// The path length is decided by pieces that are all fixed for this process,
    /// so the pre-flight probe and the real bind can never disagree.
    #[test]
    fn the_probed_path_has_the_same_length_as_every_real_one() {
        let dir = Path::new("/tmp/aterm");
        let probe = rendezvous_path(dir, &"0".repeat(32));
        let real = rendezvous_path(dir, TEST_NONCE);
        assert_eq!(
            probe.as_os_str().len(),
            real.as_os_str().len(),
            "every nonce is 32 hex characters, so the probe is exact and not an estimate"
        );
    }

    #[test]
    fn an_expired_deadline_is_a_deadline_and_not_a_zero_timeout() {
        let expired = Instant::now() - Duration::from_secs(1);
        assert_eq!(remaining_io_budget(expired), Err(RendezvousError::Deadline));
        let live = Instant::now() + Duration::from_secs(5);
        assert!(remaining_io_budget(live).expect("live budget") >= MIN_IO_TIMEOUT);
    }

    /// THE WHOLE TRANSPORT, over a real kernel: bind, dial, claim, transfer —
    /// and then the fact that forced the proof term to change, asserted rather
    /// than recalled: the successor holds the SAME open file descriptions at
    /// DIFFERENT numbers.
    #[test]
    fn a_claimed_rendezvous_carries_every_descriptor_to_the_dialer() {
        let Some(rendezvous) = bind_for_test() else {
            return;
        };
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
                deadlines_in(10),
                own_parent(),
                false,
            )
        });

        let peer = rendezvous
            .accept_claim(
                Some(own_pid()),
                Instant::now() + Duration::from_secs(10),
                &|| false,
            )
            .expect("the dialer presents the claim and is this very process");
        assert_eq!(peer.pid(), own_pid(), "LOCAL_PEERPID names the dialer");
        // SAFETY: a descriptor this test owns for the length of the call.
        let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
        peer.transfer(
            TEST_NONCE,
            &[(11, 4242, borrowed)],
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + Duration::from_secs(10),
        )
        .expect("transfer");

        let claimed = dialer.join().expect("dialer thread").expect("claimed");
        assert_eq!(claimed.session_count(), 1);
        assert_eq!(
            claimed.fds_wire,
            format!("11={}:4242", claimed.masters[0].as_raw_fd()),
            "the wire names the descriptor at the number THIS process received it at"
        );
        assert_ne!(
            claimed.masters[0].as_raw_fd(),
            master,
            "SCM_RIGHTS installs a different number, which is why the fd-number proof term \
             cannot survive this transport"
        );
        assert_eq!(
            pty_device_term(claimed.masters[0].as_raw_fd()),
            pty_device_term(master),
            "the device term does survive, which is what replaces it"
        );
        drop(claimed);
        drop((ready_rd, ready_wr, commit_rd, commit_wr));
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    /// THE PARENT HANGS UP RIGHT AFTER THE GRANT, AND THE CLAIM STILL LANDS
    /// (2026-09-29, found by the warm-successor P1 A/B). The outgoing process
    /// drops the connection the instant `transfer` returns
    /// (`app_update_handoff`, "From here the successor holds copies of
    /// everything"), while the successor may still be between the header's
    /// receive and the body's read. Darwin answers `setsockopt(SO_RCVTIMEO)` on
    /// a socket whose both directions are shut with EINVAL, and the successor
    /// used to take that for a broken grant: it dropped every descriptor it had
    /// been given, the parent read EOF on the readiness pipe and killed it —
    /// "candidate died before proving adoption", 2 of ~45 same-image applies on
    /// a loaded machine. The bytes and descriptors were already queued; a
    /// fully shut socket cannot block a read, so the timeout is not needed to
    /// read them. Repeated because the window is a race; before the fix the
    /// first iteration failed every time it was run.
    #[test]
    fn a_grant_whose_connection_closes_at_once_is_still_claimed() {
        for _ in 0..20 {
            let Some(rendezvous) = bind_for_test() else {
                return;
            };
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
                    deadlines_in(10),
                    own_parent(),
                    false,
                )
            });
            let peer = rendezvous
                .accept_claim(
                    Some(own_pid()),
                    Instant::now() + Duration::from_secs(10),
                    &|| false,
                )
                .expect("the dialer presents the claim");
            // SAFETY: a descriptor this test owns for the length of the call.
            let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
            peer.transfer(
                TEST_NONCE,
                &[(11, 4242, borrowed)],
                ready_wr.as_fd(),
                commit_rd.as_fd(),
                Instant::now() + Duration::from_secs(10),
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
            drop(claimed);
            drop((ready_rd, ready_wr, commit_rd, commit_wr));
            aterm_pty::close_fd(master);
            aterm_pty::close_fd(slave);
        }
    }

    /// THE LAUNCHED LANE'S SHAPE ACROSS TWO REAL PROCESSES (2026-09-27): the
    /// production code at both ends, with the successor a SEPARATE process — not a
    /// thread of the listener, and not a listener forked for the test. This
    /// process is the outgoing parent: it binds with [`Rendezvous::bind`], accepts
    /// with [`Rendezvous::accept_claim`] and grants with `transfer`. The successor
    /// is this test binary re-run as a child that attests this process the way a
    /// LaunchServices successor does (no live parent link, so by its birth
    /// record) and dials with [`dial_and_claim`] — so its identity gate reads
    /// `LOCAL_PEERPID` against a listener that the real bind created in another
    /// process. A kernel that named anything but the binding process there would
    /// refuse the claim and fail this test before any descriptor moved. What it
    /// does not reach is LaunchServices itself and the GUI boot around the dial;
    /// that is `tests/handoff_launchd_job.rs`'s ignored live guard.
    #[test]
    fn a_successor_process_claims_from_the_real_outgoing_listener() {
        const CHILD: &str = "RZ_CROSS_PROCESS_SUCCESSOR";
        const EXACT: &str =
            "handoff_rendezvous::tests::a_successor_process_claims_from_the_real_outgoing_listener";
        if let Some(spec) = std::env::var_os(CHILD) {
            // THE SUCCESSOR: `<path>\n<claim>\n<parent pid>\n<device term>`.
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
                dial_and_claim(&path, &claim, TEST_NONCE, deadlines_in(10), parent, false)
                    .expect("the successor's identity gate admits the real outgoing listener");
            assert_eq!(claimed.session_count(), 1);
            assert_eq!(
                pty_device_term(claimed.masters[0].as_raw_fd()),
                Some(term),
                "the granted master is the outgoing process's PTY"
            );
            return;
        }

        let Some(rendezvous) = bind_for_test() else {
            return;
        };
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
        let child = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", EXACT, "--nocapture", "--test-threads=1"])
            .env(CHILD, spec)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn the successor process");
        let successor = i32::try_from(child.id()).expect("a pid fits pid_t");

        let peer = rendezvous
            .accept_claim(
                Some(successor),
                Instant::now() + Duration::from_secs(20),
                &|| false,
            )
            .expect("the successor process presents the claim");
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
            Instant::now() + Duration::from_secs(10),
        )
        .expect("transfer");

        let output = child.wait_with_output().expect("the successor exits");
        assert!(
            output.status.success(),
            "the successor process failed its claim\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        drop((ready_rd, ready_wr, commit_rd, commit_wr));
        aterm_pty::close_fd(master);
        aterm_pty::close_fd(slave);
    }

    /// WHICH SIDE OF THE `sendmsg` A TRANSFER FAILURE FELL ON (2026-09-14, audit
    /// AH-6). A peer that has closed BEFORE the parent transfers makes the
    /// descriptor-carrying `sendmsg` itself fail: `TransferNotSent`, the one shape
    /// `NeverTransferred` may rest on. A peer that dequeues the header — and with it
    /// every descriptor — and then closes can only produce `Ok` (the body was
    /// buffered before the close) or `TransferPartial`: never `TransferNotSent`,
    /// because the descriptors are already the peer's. The second half is a race
    /// the kernel decides, so it is pinned as the invariant rather than as one
    /// outcome.
    #[test]
    fn a_transfer_failure_says_whether_the_descriptors_left() {
        let Some(rendezvous) = bind_for_test() else {
            return;
        };
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
        let peer = rendezvous
            .accept_claim(
                Some(own_pid()),
                Instant::now() + Duration::from_secs(10),
                &|| false,
            )
            .expect("the claim was presented");
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
                Instant::now() + Duration::from_secs(5),
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
        let Some(rendezvous) = bind_for_test() else {
            return;
        };
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
        let peer = rendezvous
            .accept_claim(
                Some(own_pid()),
                Instant::now() + Duration::from_secs(10),
                &|| false,
            )
            .expect("claimed");
        let outcome = peer.transfer(
            TEST_NONCE,
            &[(11, 4242, fake_master_rd.as_fd())],
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + Duration::from_secs(5),
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
    /// arrives the wait ends on THAT refusal rather than on a bare deadline —
    /// "no successor dialed" would be false, and which of the two happened is
    /// what a field investigation reads first.
    #[test]
    fn a_dialer_without_the_secret_is_refused() {
        let Some(rendezvous) = bind_for_test() else {
            return;
        };
        let path = rendezvous.path().to_path_buf();
        let dialer = std::thread::spawn(move || {
            let stream = CtlStream::connect(&path).expect("connect");
            let _ = (&stream).write_all(&claim_frame(&"f".repeat(CLAIM_HEX_LEN)));
            // Hold the connection open, so the refusal is provably about the
            // claim and not about a peer that vanished.
            std::thread::sleep(Duration::from_millis(200));
        });
        let refused = rendezvous.accept_claim(
            Some(own_pid()),
            Instant::now() + Duration::from_millis(400),
            &|| false,
        );
        assert_eq!(refused.err(), Some(RendezvousError::Claim));
        dialer.join().expect("dialer thread");
    }

    /// A WRONG DIALER COSTS ITS OWN CONNECTION, NOT THE UPDATE. The path is
    /// `0600` inside a `0700` directory, so a wrong dialer is same-uid — and
    /// while the secret is what stops it taking the handoff, an earlier shape let
    /// it END the handoff by connecting first, which made any same-uid process a
    /// denial of update for the price of one `connect(2)`. The refusal must be of
    /// the CONNECTION: the real successor, dialing second, still gets everything.
    #[test]
    fn a_wrong_dialer_does_not_end_the_attempt_and_the_successor_still_claims() {
        let Some(rendezvous) = bind_for_test() else {
            return;
        };
        let (master, slave) = open_pty();
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let path = rendezvous.path().to_path_buf();
        let claim = rendezvous.claim().to_string();

        // FIRST in the accept queue, and inline rather than on a thread so it is
        // provably first: connected and fully spoken before the real dial exists.
        let bogus = CtlStream::connect(&path).expect("the wrong dialer connects");
        (&bogus)
            .write_all(&claim_frame(&"f".repeat(CLAIM_HEX_LEN)))
            .and_then(|()| (&bogus).flush())
            .expect("the wrong dialer presents a wrong secret");
        bogus
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("bound the wrong dialer's own read");

        let successor = std::thread::spawn(move || {
            dial_and_claim(
                &path,
                &claim,
                TEST_NONCE,
                deadlines_in(10),
                own_parent(),
                false,
            )
        });
        let peer = rendezvous
            .accept_claim(
                Some(own_pid()),
                Instant::now() + Duration::from_secs(10),
                &|| false,
            )
            .expect("the wrong dialer is refused and the wait resumes for the real successor");
        // SAFETY: a descriptor this test owns for the length of the call.
        let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
        peer.transfer(
            TEST_NONCE,
            &[(11, 4242, borrowed)],
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + Duration::from_secs(10),
        )
        .expect("transfer");
        let claimed = successor
            .join()
            .expect("dialer thread")
            .expect("the successor behind a wrong dialer still claims its handoff");
        assert_eq!(claimed.session_count(), 1);

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
        let Some(rendezvous) = bind_for_test() else {
            return;
        };
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

    /// A PEER THAT DRIBBLES CANNOT STRETCH THE DEADLINE. `set_read_timeout` is a
    /// PER-SYSCALL bound, so a `read_exact` under it restarts the clock on every
    /// byte: one byte delivered just inside each expiry turned a fixed 70-byte
    /// claim frame into roughly 70 budgets' worth of parked terminal — and this
    /// is pre-authentication, since the frame is read before the pid check. What
    /// bounds the frame now is the deadline, so a dribbler is refused AT it.
    #[test]
    fn a_dribbling_dialer_is_refused_at_the_deadline_rather_than_stretching_it() {
        let Some(rendezvous) = bind_for_test() else {
            return;
        };
        let path = rendezvous.path().to_path_buf();
        // The RIGHT secret, delivered too slowly: what is being tested is the
        // budget, so the frame must be one that would otherwise be accepted.
        let claim = rendezvous.claim().to_string();
        // Connected inline, before the wait starts, so the accept finds it at
        // once and the refusal is always the frame read's: a dial left to a
        // thread that a loaded machine starts late would end the wait on the
        // bare deadline with no frame read at all, and pass saying nothing.
        let stream = CtlStream::connect(&path).expect("connect");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dribbler = {
            let stop = std::sync::Arc::clone(&stop);
            std::thread::spawn(move || {
                for byte in claim_frame(&claim) {
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    if (&stream).write_all(&[byte]).is_err() {
                        break;
                    }
                    let _ = (&stream).flush();
                    std::thread::sleep(Duration::from_millis(40));
                }
            })
        };

        let budget = Duration::from_millis(300);
        let started = Instant::now();
        let refused = rendezvous.accept_claim(Some(own_pid()), Instant::now() + budget, &|| false);
        let waited = started.elapsed();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);

        assert!(
            refused.is_err(),
            "a frame that does not arrive inside the deadline is refused, not awaited"
        );
        // The budget is the subject, and it sits between the two outcomes: a
        // frame read bounded by the attempt's deadline is refused at `budget`
        // (300 ms), while one bounded only by its own `CLAIM_FRAME_BUDGET` is
        // refused a whole frame budget (2 s) after the accept, and one that
        // restarts a per-read timeout reads all 70 bytes (2.8 s) before anything
        // looks at the deadline. A correct tree fails this only if held off the
        // CPU for 1.7 s.
        assert!(
            waited < CLAIM_FRAME_BUDGET,
            "the WHOLE frame is bounded by the attempt's deadline, not by the frame's own \
             {CLAIM_FRAME_BUDGET:?}; 70 bytes at 40 ms each would be 2.8 s, and this waited \
             {waited:?}"
        );
        dribbler.join().expect("dribbler thread");
    }

    /// HALF A RENDEZVOUS REPORTS ITSELF AS HALF. `rendezvous_present` answers yes
    /// to either name alone (fail-closed: a successor that starts a fresh window
    /// over a parent's live sessions is the one unacceptable outcome), and the
    /// claim path used to answer "this launch carries no rendezvous" — which the
    /// caller wraps in "this launch carries a rendezvous but could not claim it
    /// (...)", so the one line a failed update leaves behind contradicted itself.
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

        for half in [
            RendezvousError::HalfRendezvous {
                present: ENV_RENDEZVOUS,
                missing: ENV_CLAIM,
            },
            RendezvousError::HalfRendezvous {
                present: ENV_CLAIM,
                missing: ENV_RENDEZVOUS,
            },
        ] {
            let said = half.to_string();
            assert!(
                said.contains(ENV_RENDEZVOUS) && said.contains(ENV_CLAIM),
                "the message names both halves so a reader can see which arrived: {said}"
            );
            assert!(
                !said.contains(&RendezvousError::NoRendezvous.to_string()),
                "and it never says the launch carries no rendezvous, which is what made the \
                 caller's line contradict itself: {said}"
            );
        }
    }

    /// Nobody dials: the parent gives the terminal back on its deadline, and the
    /// socket is gone afterwards so a late dial fails closed.
    #[test]
    fn a_rendezvous_nobody_dials_expires_and_unlinks() {
        let Some(rendezvous) = bind_for_test() else {
            return;
        };
        let path = rendezvous.path().to_path_buf();
        assert!(path.exists(), "the listener is on disk while it is bound");
        let expired = rendezvous.accept_claim(
            Some(own_pid()),
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
        let Some(rendezvous) = bind_for_test() else {
            return;
        };
        // A two-minute deadline, bounded at one: a wait that consulted the
        // abort predicate only once the deadline had passed would answer the
        // same `Cancelled` two minutes on. The minute is a hang detector, not a
        // latency budget.
        let started = Instant::now();
        let cancelled = rendezvous.accept_claim(
            Some(own_pid()),
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

    /// Bind a rendezvous under a test-only nonce, or skip when this machine has
    /// no usable control directory or a `$HOME` too long for `sun_path` — the
    /// same two refusals production falls back to the fork lane on.
    fn deadlines_in(secs: u64) -> ClaimDeadlines {
        let at = Instant::now() + Duration::from_secs(secs);
        ClaimDeadlines {
            dial: at,
            grant: at,
        }
    }

    /// THE HELD CLAIM (2026-09-19, the late park): the parent accepts the dial,
    /// HOLDS the served stream while it does other work, and the one descriptor
    /// message still arrives whole — and the hold is bounded by the dialer's
    /// GRANT budget, not its dial budget, so a dial budget already spent by the
    /// time the parent parks does not end a legitimate wait.
    #[test]
    fn a_held_claim_is_granted_after_the_hold_under_the_grant_budget() {
        let Some(rendezvous) = bind_for_test() else {
            return;
        };
        let (master, slave) = open_pty();
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        // THE HOLD WAITS OUT THE DIAL BUDGET IT WAS HANDED, NOT A GUESS AT IT (the
        // load-sensitive test audit of 2026-09-27). The dial budget used to be
        // 150 ms stamped inside the dialer against a fixed 400 ms hold, so a
        // dialer kept off the CPU for 150 ms between its connect and its claim
        // write failed `Deadline` on a correct tree. The budget is now fixed here,
        // before the dialer exists, and generous for a connect, a getsockopt and
        // a 70-byte write; the hold sleeps until that same instant has passed,
        // plus `PAST_THE_DIAL` for a receive a regressed dialer bounded by it to
        // expire. The grant therefore still arrives only after the dial budget is
        // spent — which is the property — and only the grant budget can govern it.
        const PAST_THE_DIAL: Duration = Duration::from_millis(250);
        let dial = Instant::now() + Duration::from_secs(2);
        let deadlines = ClaimDeadlines {
            dial,
            grant: dial + Duration::from_secs(30),
        };
        let dialer = std::thread::spawn(move || {
            dial_and_claim(&path, &claim, TEST_NONCE, deadlines, own_parent(), false)
        });
        let peer = rendezvous
            .accept_claim(
                Some(own_pid()),
                Instant::now() + Duration::from_secs(10),
                &|| false,
            )
            .expect("claimed");
        assert!(!peer.poll_hangup(), "a held, idle dialer is not a hangup");
        std::thread::sleep(dial.saturating_duration_since(Instant::now()) + PAST_THE_DIAL);
        assert!(Instant::now() > dial, "the hold outlasted the dial budget");
        assert!(
            !peer.poll_hangup(),
            "…nor after the hold outlasted its dial budget"
        );
        // SAFETY: a descriptor this test owns for the length of the call.
        let borrowed = unsafe { BorrowedFd::borrow_raw(master) };
        peer.transfer(
            TEST_NONCE,
            &[(11, 4242, borrowed)],
            ready_wr.as_fd(),
            commit_rd.as_fd(),
            Instant::now() + Duration::from_secs(10),
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

    /// STAND-DOWN IS EOF: a parent that drops the held peer and the rendezvous
    /// without transferring ends the dialer's wait at once with the one error the
    /// successor maps to "exit before any window", and no descriptor ever left.
    #[test]
    fn dropping_a_held_claim_ends_the_dialers_wait_with_eof_and_no_descriptors() {
        let Some(rendezvous) = bind_for_test() else {
            return;
        };
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        // A two-minute grant budget, bounded at one: a dialer that did not take
        // EOF as the end would wait the budget out and fail `Deadline` two
        // minutes on. The minute is a hang detector, not a latency budget.
        let dialer = std::thread::spawn(move || {
            let started = Instant::now();
            let outcome = dial_and_claim(
                &path,
                &claim,
                TEST_NONCE,
                deadlines_in(120),
                own_parent(),
                false,
            );
            (outcome, started.elapsed())
        });
        let peer = rendezvous
            .accept_claim(
                Some(own_pid()),
                Instant::now() + Duration::from_secs(60),
                &|| false,
            )
            .expect("claimed");
        std::thread::sleep(Duration::from_millis(100));
        drop(peer);
        drop(rendezvous);
        let (outcome, elapsed) = dialer.join().expect("dialer thread");
        match outcome {
            Err(RendezvousError::Grant(reason)) => {
                assert_eq!(reason, "the parent closed the rendezvous");
            }
            Err(other) => panic!("expected the EOF grant error, got {other}"),
            Ok(_) => panic!("expected the EOF grant error, got a grant"),
        }
        assert!(
            elapsed < Duration::from_secs(60),
            "EOF ends the wait at once, not at the grant budget: {elapsed:?}"
        );
    }

    /// A dialer that dies during the hold is seen by the parent's `poll_hangup`
    /// before any transfer, so the attempt can be stood down without a park.
    #[test]
    fn a_dialer_that_dies_during_the_hold_reads_as_a_hangup() {
        let Some(rendezvous) = bind_for_test() else {
            return;
        };
        let claim = rendezvous.claim().to_string();
        let path = rendezvous.path().to_path_buf();
        // The dialer dies once the parent has SEEN it alive, never after a guess
        // at how long an accept takes. Both steps before that need it connected:
        // `gate_one` asks the kernel for the dialer's pid, and LOCAL_PEERPID
        // answers ENOTCONN once the peer has closed (measured on Darwin 25.6,
        // the 70 claim bytes still readable), so a close first is a refused
        // claim; and the first `poll_hangup` below must read a live peer. A
        // 200 ms hold lost both to a test thread kept off the CPU that long.
        // The sender dropping (a panic before the send) releases the dialer too.
        let (seen_alive, hold) = std::sync::mpsc::channel::<()>();
        let dialer = std::thread::spawn(move || {
            let stream = CtlStream::connect(&path).expect("connect");
            (&stream)
                .write_all(&claim_frame(&claim))
                .and_then(|()| (&stream).flush())
                .expect("claim written");
            // Hold the stream until the parent has accepted it and seen it
            // alive, then die.
            let _ = hold.recv();
            drop(stream);
        });
        let peer = rendezvous
            .accept_claim(
                Some(own_pid()),
                Instant::now() + Duration::from_secs(10),
                &|| false,
            )
            .expect("claimed");
        assert!(!peer.poll_hangup(), "alive while it holds the stream");
        let _ = seen_alive.send(());
        dialer.join().expect("dialer thread");
        wait_for_hangup(|| peer.poll_hangup());
    }

    /// Wait, bounded, for a dialer's close to reach the parent as a hang-up.
    ///
    /// A dropped `CtlStream` is closed only when every copy of its descriptor
    /// is: a child another test in this binary is forking holds one until it
    /// execs, longer on a loaded machine, and a descriptor made close-on-exec
    /// non-atomically (macOS `socket` + `FIOCLEX`) can ride through the exec
    /// itself. So a hang-up is waited for, not expected at once: 10 s, where it
    /// was ~0.5 s of polls, or none — the fd-copy sweep of 2026-09-27, the class
    /// `claude_lights`' gesture tests failed a gate on.
    fn wait_for_hangup(hung_up: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !hung_up() {
            assert!(
                Instant::now() < deadline,
                "the closed dialer never read as a hangup"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn bind_for_test() -> Option<Rendezvous> {
        let nonce = aterm_uds::rand::hex_token::<16>().ok()?;
        match Rendezvous::bind(&nonce) {
            Ok(rendezvous) => Some(rendezvous),
            Err(RendezvousError::NoControlDir | RendezvousError::PathTooLong { .. }) => None,
            Err(error) => panic!("bind: {error}"),
        }
    }

    /// THE HOIST'S SAFETY CLAIM, in one test. `proof_identities_in_device_terms`
    /// is the only step moved above `park_all_readers` that touches the kernel at
    /// all, so the move is sound exactly if it (a) gives the same answer whether
    /// or not a reader has been stopped, and (b) consumes nothing — a parked
    /// window exists to keep bytes unread, and a preparation step that swallowed
    /// one would corrupt the very checkpoint the park protects.
    ///
    /// Both are asserted against a master with unread bytes waiting on it, which
    /// is the state the parked window is defined by.
    #[test]
    fn proof_identities_are_park_independent_and_consume_nothing() {
        let (master, slave) = open_pty();
        let identities = vec![(7u64, master, 4242i32)];

        let before =
            proof_identities_in_device_terms(&identities).expect("a live pty answers fstat");

        // Put unread output on the master — the exact condition a park preserves.
        // No newline: the line discipline maps NL to CR NL on the way out, and
        // this test is about what the PREPARATION did, not about termios.
        let payload = b"parked bytes";
        // SAFETY: `slave` is a live descriptor and the buffer outlives the call.
        let wrote = unsafe {
            libc::write(
                slave,
                payload.as_ptr().cast::<libc::c_void>(),
                payload.len(),
            )
        };
        assert!(wrote > 0, "seed the master with unread output");

        let after = proof_identities_in_device_terms(&identities)
            .expect("pending output does not change the device term");
        assert_eq!(
            before, after,
            "the device term is a property of the DEVICE, not of what is queued on it \
             — so taking it before the park is the same answer as taking it after"
        );
        assert_ne!(
            before[0].1, master,
            "the proof term really is the device number, not the fd number"
        );

        // (b): the bytes are still there. `poll` says readable, and the read that
        // follows returns what was written — nothing was consumed on the way past.
        let mut fds = [libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: one initialized pollfd, zero timeout.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, 0) };
        assert_eq!(ready, 1, "the seeded output is still queued");
        let mut buf = [0u8; 32];
        // SAFETY: `master` is live and the buffer is exactly `buf.len()` bytes.
        let read =
            unsafe { libc::read(master, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
        assert_eq!(
            &buf[..usize::try_from(read).expect("a non-negative read")],
            payload,
            "every byte survived the preparation step"
        );

        // SAFETY: both descriptors were opened by this test and are unused now.
        unsafe {
            libc::close(slave);
            libc::close(master);
        }
    }

    fn open_pty() -> (i32, i32) {
        let (mut master, mut slave) = (0i32, 0i32);
        // SAFETY: valid out-params; openpty fills them on success.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0, "openpty");
        (master, slave)
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

    /// THE LAYOUT BOTH SIDES COMPUTE: every descriptor placed once, no message
    /// over the `SCM_RIGHTS` ceiling, the two pipes always in the last message,
    /// never more messages than the header can name — and up to 62 sessions,
    /// exactly one message, which is the one-message grant's shape.
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

    /// THE LAUNCHER'S LIMIT OUTLIVES A FORK-LANE HOP (round seven, item 107).
    /// A launched successor records the soft limit it raised from; its next
    /// update may take the fork lane, whose child inherits the raised limit and
    /// never claims. The parent names the recorded limit on the child's
    /// command, and the child adopts it — only ever lower than what it runs at,
    /// and only a number.
    ///
    /// RED before the fix: nothing carried it, so a forked successor's shells
    /// got the raised 4096 back.
    #[test]
    fn a_forked_successor_adopts_the_carried_launcher_limit() {
        assert_eq!(carried_launcher_limit(Some("256"), 4096), Some(256));
        assert_eq!(carried_launcher_limit(Some("4096"), 4096), Some(4096));
        assert_eq!(
            carried_launcher_limit(Some("8192"), 4096),
            None,
            "never raised by it"
        );
        assert_eq!(carried_launcher_limit(Some("0"), 4096), None);
        assert_eq!(carried_launcher_limit(Some("-1"), 4096), None);
        assert_eq!(carried_launcher_limit(Some("lots"), 4096), None);
        assert_eq!(carried_launcher_limit(None, 4096), None, "an older parent");
        // The fork lane names it on the child's command, where the successor's
        // snapshot takes it (`HandoffEnv::capture`).
        assert!(
            crate::handoff_env::HANDOFF_ENV_KEYS.contains(&ENV_LAUNCHER_NOFILE),
            "the successor captures it"
        );
        let src = include_str!("app_update_handoff.rs");
        assert!(
            src.contains("crate::handoff_rendezvous::launcher_limit_env()"),
            "the fork lane names it"
        );
        let main = include_str!("lib.rs");
        assert!(
            main.contains("handoff_rendezvous::adopt_carried_launcher_limit(&handoff_env)"),
            "main_entry adopts it"
        );
    }

    /// A LAUNCHED SUCCESSOR MAKES ROOM FOR ITS GRANT BEFORE IT DIALS (round six
    /// of the update audit, item 39). Run in a child of the test binary whose
    /// soft descriptor limit leaves 64 to spare — launchd's 256 in miniature —
    /// the claim's raise lifts it to the floor (or the hard limit), and then a
    /// 100-session chunked grant crosses whole in that very process.
    ///
    /// RED without the raise: in a process that low the 100-session grant
    /// runs out of descriptors (here already while the test stages its pipes;
    /// in a launched successor, while it receives the later chunks).
    #[test]
    fn a_successor_raises_its_descriptor_limit_before_claiming() {
        assert_eq!(raised_soft_limit(256, u64::MAX), Some(4096));
        assert_eq!(raised_soft_limit(256, 1024), Some(1024));
        assert_eq!(raised_soft_limit(10_240, u64::MAX), None, "never lowered");
        assert_eq!(raised_soft_limit(1024, 1024), None, "at the hard limit");
        if !crate::control_auth::enter_low_nofile_test_child(
            "ATERM_TEST_RENDEZVOUS_LOW_NOFILE_CHILD",
            "handoff_rendezvous::tests::a_successor_raises_its_descriptor_limit_before_claiming",
            64,
        ) {
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
        a_shell_spawned_after_the_raise_gets_the_launchers_limit(from, to);
    }

    /// …AND THE RAISE STAYS THE SUCCESSOR'S (round six, item 39, review round
    /// two). A shell this process spawns after the claim — a new tab, with the
    /// daily-driver limits `spawn_session` hands the PTY — must see the soft
    /// limit the process was launched with, not the one it raised itself to.
    /// Driven through the real actuator (`Limits::apply` before `exec`) and read
    /// back by the shell's own `ulimit -n`.
    ///
    /// RED before the fix: the daily-driver modes passed `Limits::inherit()`,
    /// so the shell read the raised limit.
    fn a_shell_spawned_after_the_raise_gets_the_launchers_limit(from: u64, to: u64) {
        use std::os::unix::process::CommandExt as _;
        assert_eq!(
            launcher_soft_descriptor_limit(),
            Some(from),
            "the raise was recorded"
        );
        let limits = crate::spawn::daily_driver_limits();
        assert_eq!(limits.open_files, Some(from));
        // SAFETY: test-only root authority, used for this one child's cap.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let mut shell = std::process::Command::new("/bin/sh");
        shell.args(["-c", "ulimit -n"]);
        // SAFETY: `apply` is a bare `getrlimit`/`setrlimit` loop with a valid
        // cap — async-signal-safe, as the PTY's own post-fork child relies on.
        unsafe {
            shell.pre_exec(move || limits.apply(&cap));
        }
        let out = shell.output().expect("run the shell");
        assert!(out.status.success(), "{out:?}");
        let seen = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(
            seen,
            from.to_string(),
            "the shell sees the launcher's limit, not {to}"
        );
    }

    /// A PROOF DEADLINE THAT RAN OUT BEFORE THE SEND IS THIS PROCESS'S LATENESS
    /// (round six of the update audit, item 48). A successor dialed and was
    /// held; the grant's deadline had passed by the time it was sent (the
    /// artifacts took longer than the budget). No descriptor leaves — in the
    /// one-message grant and the chunked one — and the error says the deadline
    /// passed after a dial, never "no successor dialed".
    ///
    /// RED before the fix: `transfer` returned `Deadline`, whose only words are
    /// "no successor dialed the rendezvous before the handoff deadline", and
    /// the caller filed it `Rejected`.
    #[test]
    fn a_grant_past_its_deadline_says_the_successor_dialed_and_sends_nothing() {
        for (sessions, chunks) in [(3_usize, false), (100, true)] {
            let Some(rendezvous) = bind_for_test() else {
                return;
            };
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
                    deadlines_in(60),
                    own_parent(),
                    chunks,
                )
            });
            let peer = rendezvous
                .accept_claim(
                    Some(own_pid()),
                    Instant::now() + Duration::from_secs(60),
                    &|| false,
                )
                .expect("the claim is this attempt's");
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
                )
                .expect_err("a grant past its deadline is not sent");
            assert_eq!(error, RendezvousError::GrantDeadline, "{sessions} sessions");
            let words = error.to_string();
            assert!(
                !words.contains("no successor dialed") && words.contains("no descriptor left"),
                "{words}"
            );
            // Nothing was sent: the dialer sees the parent close, not a grant.
            drop((peer, rendezvous));
            let received = dialer.join().expect("dialer thread");
            assert!(
                received.is_err(),
                "{sessions} sessions: the dialer received no grant"
            );
        }
    }

    /// FAILS WITHOUT THE CHUNKED GRANT: a hundred sessions — more than one
    /// `SCM_RIGHTS` message carries — cross the real rendezvous to a successor
    /// that claimed `ATRZ2C`, each descriptor paired with the session it was
    /// sent for and the two pipes last. Before, `transfer` refused them
    /// (`TooManySessions`), and the update stood down as a producer failure.
    #[test]
    fn a_grant_of_100_sessions_crosses_in_chunks() {
        let Some(rendezvous) = bind_for_test() else {
            return;
        };
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
                deadlines_in(60),
                own_parent(),
                true,
            )
        });
        let peer = rendezvous
            .accept_claim(
                Some(own_pid()),
                Instant::now() + Duration::from_secs(60),
                &|| false,
            )
            .expect("the chunked claim is this attempt's");
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

    /// AN `ATRZ1C` CLAIMANT NEVER RECEIVES A CHUNKED GRANT, and an `ATRZ2C`
    /// claimant of 62 or fewer sessions receives the one-message grant byte for
    /// byte:
    ///
    /// - a hundred sessions for a successor that claimed `ATRZ1C` (every build
    ///   before the chunked grant) are refused before a descriptor leaves, and
    ///   that successor reads the close and refuses its grant;
    /// - three sessions for a successor that claimed `ATRZ2C` go as `ATRZ1G`,
    ///   the header and body exactly the one-message grant's;
    /// - a successor that claimed `ATRZ1C` refuses an `ATRZ2G` header as the
    ///   wrong magic, whatever a parent sends it.
    ///
    /// FAILS WITHOUT THE CHUNKED GRANT: an `ATRZ2C` claim was no claim at all.
    #[test]
    fn an_atrz1c_claim_never_receives_a_chunked_grant() {
        use std::io::Read as _;

        // An ATRZ1C claimant with a hundred sessions to take.
        let Some(rendezvous) = bind_for_test() else {
            return;
        };
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
                deadlines_in(60),
                own_parent(),
                false,
            )
        });
        let peer = rendezvous
            .accept_claim(
                Some(own_pid()),
                Instant::now() + Duration::from_secs(60),
                &|| false,
            )
            .expect("the one-message claim is this attempt's");
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
        let Some(rendezvous) = bind_for_test() else {
            return;
        };
        let (master, slave) = open_pty();
        let (ready_rd, ready_wr) = pipe_for_test();
        let (commit_rd, commit_wr) = pipe_for_test();
        let dialer = CtlStream::connect(rendezvous.path()).expect("connect");
        (&dialer)
            .write_all(&claim_frame_with(rendezvous.claim(), true))
            .expect("claim");
        let peer = rendezvous
            .accept_claim(
                Some(own_pid()),
                Instant::now() + Duration::from_secs(60),
                &|| false,
            )
            .expect("the chunked claim is this attempt's");
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
        let body = encode_grant_body(TEST_NONCE, &sessions).expect("a body");
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

        let dir = std::env::temp_dir().join(format!(
            "aterm-rz-stub-{}-{}",
            std::process::id(),
            aterm_uds::rand::hex_token::<4>().expect("a name")
        ));
        std::fs::create_dir_all(&dir).expect("a stub dir");
        let path = dir.join("p.sock");
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
                // HELD until the dialer has answered, as a real parent holds it
                // through the proof wait: Darwin refuses a socket option on a
                // stream whose peer has gone (`EINVAL`), so a stub that closed
                // at once would be refused for that and not for its grant. A
                // grant cut short says so with a write shutdown instead.
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
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    }

    /// FAILS WITHOUT THE CHUNKED GRANT: A SHORT CHUNK IS REFUSED, AND EVERY
    /// DESCRIPTOR THE SUCCESSOR RECEIVED IS CLOSED. A hundred-session grant
    /// whose second message carries 30 descriptors where its layout names 38,
    /// and one whose second message never comes: both are refused, and once
    /// the stub parent closes its own copies every one of the hundred and two
    /// reads EOF — nothing the successor received was kept.
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
        let model = aterm_spec::derive::native_update_rendezvous_grant_model();
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
                        // THE SUCCESSOR'S CLAIM: this build's code reads the
                        // chunked grant (`capable`); an older build's never
                        // does, whatever it is offered.
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

    /// TIER-1 FOR `NativeUpdateRendezvousGrant`: the shipping decisions —
    /// `offers_chunked_grant` and the claim frames on the successor's side,
    /// `grant_shape` on the parent's — are the model's guards at every
    /// combination of successor, declaration, offer, claim and pool. The
    /// negative control is a parent that ignores the claim (it would send
    /// `ATRZ2G` to a build that cannot read it): the same walk must catch it.
    /// Adoption is bound by the real transfers above:
    /// `a_grant_of_100_sessions_crosses_in_chunks` (the whole grant adopted) and
    /// `a_short_chunk_is_refused_and_closes_every_fd` (a grant cut short is
    /// refused with nothing held).
    #[test]
    fn rendezvous_grant_conformance() {
        assert_eq!(grant_disagreement(&grant_shape), None);
        assert!(
            grant_disagreement(&|_, sessions| grant_shape(true, sessions)).is_some(),
            "a parent that ignores the claim must disagree with the model"
        );
        // Adoption, stepped as the real transfers above run it.
        let model = aterm_spec::derive::native_update_rendezvous_grant_model();
        let mut whole = model.init_state();
        for action in [
            "Upgrade",
            "Declare",
            "Grow",
            "Offer",
            "Launch",
            "ClaimChunks",
            "GrantChunks",
            "Send",
            "Take",
            "Send",
            "Take",
            "Adopt",
        ] {
            step_grant(&model, &mut whole, action);
        }
        let mut cut = model.init_state();
        for action in [
            "Upgrade",
            "Declare",
            "Grow",
            "Offer",
            "Launch",
            "ClaimChunks",
            "GrantChunks",
            "Send",
            "Take",
            "Stop",
        ] {
            step_grant(&model, &mut cut, action);
        }
        assert!(
            !model.action_enabled("Adopt", &cut),
            "a grant cut short is never adopted"
        );
        step_grant(&model, &mut cut, "Refuse");
        assert_eq!(cut["held"], 0, "and its refusal holds nothing");
    }
}

/// Tier-1 for `NativeUpdateRendezvousClaim` (aterm-spec's
/// `native_update_rendezvous_claim_model`): the real dial gate,
/// [`listener_may_receive_claim`], against the model's `PresentClaim` guard over
/// every combination of the three listener facts, with REAL attestations (this
/// live process, and an attestation of it whose birth witness no longer
/// matches); then the whole [`dial_and_claim`] against a real listener in a
/// FORKED process, which is also the measurement the gate rests on — that
/// `LOCAL_PEERPID`, read on the dialing end before any `accept`, names the
/// listening process.
#[cfg(test)]
mod claim_presentation_conformance {
    use super::{
        CLAIM_FRAME_LEN, ClaimDeadlines, RendezvousError, dial_and_claim,
        listener_may_receive_claim,
    };
    use crate::seamless::AttestedParent;
    use aterm_spec::derive::{Model, native_update_rendezvous_claim_model};
    use aterm_spec::interp::{State, with_buggy};
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

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

    /// Drive the REAL gate with kernel-shaped inputs realizing `facts`. The uid is
    /// the one input a test cannot realize (a foreign-uid listener needs root), so
    /// it is the kernel's ANSWER that varies; the pid is a real live process
    /// either way (this one, or the process that launched this test); the parent
    /// is a real attestation, live or with a stale birth witness.
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

    /// NEGATIVE CONTROL, on the SHIPPING gate: the gate that shipped before
    /// 2026-09-27 — the uid half alone — is the model's `Buggy` presentation
    /// (`PresentClaimUidOnly`). The real [`listener_may_receive_claim`] must
    /// refuse exactly where that mutant presents to the wrong listener — a
    /// same-uid impostor, a gone parent, or both — and agree everywhere else. A
    /// gate reverted to the uid half alone agrees with the mutant at all eight
    /// combinations, and this test fails.
    #[test]
    fn the_real_gate_refuses_where_the_uid_only_mutant_presents() {
        let buggy = with_buggy(&native_update_rendezvous_claim_model(), 1);
        let mut told_apart = Vec::new();
        for facts in every_combination() {
            let mutant = buggy.action_enabled("PresentClaimUidOnly", &project(&buggy, facts));
            // The model's mutant IS the uid-only gate, so this compares the real
            // gate with the shape that shipped, not with a copy of itself.
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

    /// A same-uid rendezvous listener in a FORKED process. The child runs
    /// async-signal-safe calls only (libtest's thread pool makes this process
    /// multi-threaded at fork time): it closes every inherited descriptor but its
    /// own, binds and listens, reports readiness, serves ONE connection, reports
    /// how many claim bytes it received (then closes it — EOF to the dialer), and
    /// `_exit`s.
    struct ForkedListener {
        pid: libc::pid_t,
        report: OwnedFd,
        path: PathBuf,
        dir: PathBuf,
    }

    impl ForkedListener {
        fn start() -> Self {
            // Two fixtures in one test binary can start in the same clock tick,
            // so the name carries a per-process sequence number as well.
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
                % 1_000_000_000;
            let dir = std::env::temp_dir()
                .join(format!("aterm-rzl-{}-{seq}-{unique}", std::process::id()));
            std::fs::create_dir(&dir).expect("create the listener's scratch dir");
            let path = dir.join("l.sock");
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
            // The descriptor table's size, read BEFORE the fork (the child only
            // closes up to it), and capped so the child's close sweep stays quick.
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: `getrlimit` fills the live local it is handed.
            let table = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
                i32::try_from(limit.rlim_cur.min(1 << 16)).unwrap_or(1 << 16)
            } else {
                1 << 16
            };

            // SAFETY: fork from a multi-threaded harness, with a child that
            // performs async-signal-safe calls only — see the type's doc.
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0, "fork failed");
            if pid == 0 {
                // SAFETY (whole block): async-signal-safe syscalls on stack
                // memory and descriptors this child owns; nothing allocates,
                // locks, or unwinds, and the child leaves through its one
                // `_exit` (the forked-child site `crash_signal`'s exit gate
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
                dir,
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
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn deadlines() -> ClaimDeadlines {
        let at = Instant::now() + Duration::from_secs(10);
        ClaimDeadlines {
            dial: at,
            grant: at,
        }
    }

    const SECRET: &str = "abababababababababababababababababababababababababababababababab";
    const NONCE: &str = "0123456789abcdef0123456789abcdef";

    /// THE IMPOSTOR: a same-uid process listening at the published path while
    /// the attestation names a DIFFERENT live process (this one). Before the
    /// identity bind the uid gate passed it and the claim secret left; now the
    /// dial is refused before a byte is written — and the refusal names the
    /// forked child's pid, which is the measurement: the dialing end's
    /// `LOCAL_PEERPID` names the process that LISTENS.
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

    /// THE PARENT: the same forked listener, now the process the attestation
    /// names. The gate passes (so `LOCAL_PEERPID` on the dialing end equalled
    /// its pid) and the whole claim frame reaches it — the positive half of the
    /// bind. The fixture then closes without a grant, so the dial ends in a
    /// `Grant` error: the EOF, or — when the close lands before the grant-budget
    /// `setsockopt`, which Darwin answers `EINVAL` on a disconnected socket —
    /// that. Either way it is past both gates, never the identity refusal.
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

    // The machine's other actions are the listener's environment, not code in
    // this crate: they are what the facts above realize.
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_rendezvous_claim",
        action = "ForeignUidListener",
        reason = "Environment: a listener of another uid at the published path; the Tier-1 bind \
                  realizes it as the kernel's getpeereid answer."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_rendezvous_claim",
        action = "SameUidImpostor",
        reason = "Environment: a same-uid process other than the attested parent listening at the \
                  published path; realized by a forked listener and by a foreign live pid."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_rendezvous_claim",
        action = "ParentGone",
        reason = "Environment: the attested parent exited and its pid may be recycled; realized \
                  by an attestation whose birth witness no longer matches."
    )]
    #[aterm_spec::spec_unmodeled(
        machine = "native_update_rendezvous_claim",
        action = "PresentClaimUidOnly",
        reason = "The Buggy=1 mutant (the pre-2026-09-27 uid-only gate); no shipping code \
                  implements it, and the negative control above catches it."
    )]
    #[expect(
        dead_code,
        reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it"
    )]
    fn explicit_scope_waivers() {}
}
