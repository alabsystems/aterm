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
//! Exactly one connection is SERVED, exactly one message carrying every
//! descriptor is sent, and then the socket is gone. There is no retry, no second
//! grant, and no verb.
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
//! then the Commit pipe's read end. Position is ADDRESSING only (which received
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
//! descriptors and the header are one `sendmsg`.
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

/// The most sessions one attempt may hand over this lane.
///
/// `fdpass` carries at most [`fdpass::MAX_FDS`] descriptors in one message, and
/// this transport spends two of them on the readiness and Commit pipes. The
/// protocol's own ceiling (`seamless`'s `MAX_HANDOFF_SESSIONS`, 256) is four
/// times larger, so THIS is the binding constraint and the lane check has to
/// test it: a parent with 63 panes must fall back to the fork lane rather than
/// discover at `sendmsg` time that its handoff does not fit, with the terminal
/// already parked.
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
    /// More sessions than one `SCM_RIGHTS` message can carry (see
    /// [`MAX_RENDEZVOUS_SESSIONS`]).
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
    /// The one descriptor-carrying `sendmsg`, or the body write after it,
    /// failed. Whether the peer got the descriptors is exactly what `sendmsg`
    /// answered, so this is a failed transfer and never a partial one.
    Transfer(String),

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
                "{sessions} sessions exceed the {limit} one descriptor message can carry"
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
            Self::Transfer(error) => write!(f, "the descriptor transfer failed: {error}"),
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
                Ok(pid) => return Ok(ClaimedPeer { stream, pid }),
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
    ) -> Result<i32, RendezvousError> {
        // The TIGHTER of the two bounds wins. A peer that has not yet presented
        // the secret is an unknown same-uid process, and one of those must not be
        // able to spend the attempt's whole budget by connecting and saying
        // nothing — while a real successor writes this frame in one call.
        let frame_deadline = deadline.min(Instant::now() + CLAIM_FRAME_BUDGET);
        let mut frame = [0u8; CLAIM_FRAME_LEN];
        read_frame_by_deadline(stream, &mut frame, frame_deadline, &|error| {
            RendezvousError::Accept(error.to_string())
        })?;
        if !claim_frame_matches(&frame, &self.claim) {
            return Err(RendezvousError::Claim);
        }
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
        Ok(attested)
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
}

impl ClaimedPeer {
    /// The kernel-attested dialer pid. This is the first identity the handoff
    /// has for a process it did not fork, and it is what `HandoffCandidate` and
    /// `signal_handoff_candidate` need before they may aim anything at a
    /// candidate launchd owns.
    #[must_use]
    pub(crate) fn pid(&self) -> i32 {
        self.pid
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
    pub(crate) fn transfer(
        &self,
        nonce: &str,
        sessions: &[(u64, i32, BorrowedFd<'_>)],
        ready: BorrowedFd<'_>,
        commit: BorrowedFd<'_>,
        deadline: Instant,
    ) -> Result<(), RendezvousError> {
        if sessions.is_empty() || sessions.len() > MAX_RENDEZVOUS_SESSIONS {
            return Err(RendezvousError::TooManySessions {
                sessions: sessions.len(),
                limit: MAX_RENDEZVOUS_SESSIONS,
            });
        }
        let malformed =
            || RendezvousError::Transfer("the grant body exceeds its wire format".to_string());
        let body = encode_grant_body(nonce, sessions).ok_or_else(malformed)?;
        let header = grant_header(body.len()).ok_or_else(malformed)?;
        let remaining = remaining_io_budget(deadline)?;
        self.stream
            .set_write_timeout(Some(remaining))
            .map_err(|error| RendezvousError::Transfer(error.to_string()))?;
        let mut descriptors = Vec::with_capacity(sessions.len() + RENDEZVOUS_CHANNEL_FDS);
        descriptors.extend(sessions.iter().map(|(_, _, master)| *master));
        descriptors.push(ready);
        descriptors.push(commit);
        let sent = fdpass::send_with_fds(&self.stream, &header, &descriptors)
            .map_err(|error| RendezvousError::Transfer(error.to_string()))?;
        // A short send is possible in principle on a stream socket, and the
        // descriptors went with the first byte — so the remainder is finished
        // with an ordinary write rather than resent.
        if sent < header.len() {
            (&self.stream)
                .write_all(&header[sent..])
                .map_err(|error| RendezvousError::Transfer(error.to_string()))?;
        }
        (&self.stream)
            .write_all(body.as_bytes())
            .and_then(|()| (&self.stream).flush())
            .map_err(|error| RendezvousError::Transfer(error.to_string()))?;
        Ok(())
    }
}

/// The claim frame a successor presents.
fn claim_frame(secret: &str) -> [u8; CLAIM_FRAME_LEN] {
    let mut frame = [0u8; CLAIM_FRAME_LEN];
    frame[..CLAIM_MAGIC.len()].copy_from_slice(CLAIM_MAGIC);
    // A secret of the wrong length cannot match a well-formed frame anyway, and
    // the copy is bounded so no caller can make this panic. The tail is left
    // zero, which is not a hex character and therefore not a secret.
    let bytes = secret.as_bytes();
    let carried = bytes.len().min(CLAIM_HEX_LEN);
    frame[CLAIM_MAGIC.len()..CLAIM_MAGIC.len() + carried].copy_from_slice(&bytes[..carried]);
    frame
}

/// Whether a received claim frame carries this attempt's secret.
///
/// The magic is compared ordinarily — it is a constant, and leaking that a peer
/// got a fixed prefix wrong tells nobody anything they did not already choose.
/// The SECRET is compared in constant time, because that comparison's timing is
/// the only thing that could otherwise be walked one byte at a time.
fn claim_frame_matches(frame: &[u8; CLAIM_FRAME_LEN], secret: &str) -> bool {
    if &frame[..CLAIM_MAGIC.len()] != CLAIM_MAGIC {
        return false;
    }
    crate::control_auth::constant_time_eq(&frame[CLAIM_MAGIC.len()..], secret.as_bytes())
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
/// descriptor numbers the kernel chose. Publishing it lets the existing,
/// unchanged `seamless::take_incoming` perform every authentication it already
/// performs: the manifest join, the bijection check, the tty backstop, the
/// screen-carry digest. The TRANSPORT changed; nothing that decides whether a
/// handoff is legitimate did.
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

    /// Hand every descriptor to the existing environment-shaped intake.
    ///
    /// The descriptors are RELEASED here: `seamless`'s consumers take ownership
    /// exactly as they do on the fork lane, where the numbers arrived through
    /// `execve` with no Rust owner at all. That is why this consumes `self` —
    /// afterwards the only owners are the ones `take_incoming`, `take_ready_fd`
    /// and `take_commit_fd` construct, and each of those closes what it refuses.
    ///
    /// SAFETY (env): the caller guarantees this runs on the single-threaded
    /// startup path, alongside `seamless::take_incoming` — the same contract
    /// every other handoff variable is read and cleared under.
    pub(crate) fn publish(self) {
        let Self {
            fds_wire,
            masters,
            ready,
            commit,
        } = self;
        let ready_fd = ready.as_raw_fd().to_string();
        let commit_fd = commit.as_raw_fd().to_string();
        aterm_log::env::set(crate::seamless::ENV_FDS, &fds_wire);
        aterm_log::env::set(crate::seamless::ENV_READY_FD, &ready_fd);
        aterm_log::env::set(crate::seamless::ENV_COMMIT_FD, &commit_fd);
        // Released only after the variables naming them are published, so no
        // early exit between the two can leave a name pointing at a closed
        // number.
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
pub(crate) fn rendezvous_present() -> bool {
    std::env::var_os(ENV_RENDEZVOUS).is_some() || std::env::var_os(ENV_CLAIM).is_some()
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
pub(crate) fn take_device_proof_term() -> Option<bool> {
    let value = aterm_log::env::take(ENV_PROOF_TERM)?;
    match value.to_string_lossy().as_ref() {
        "device" => Some(true),
        "fd" => Some(false),
        _ => None,
    }
}

/// Dial the rendezvous named in this launch's environment, present the claim,
/// and take delivery of every descriptor.
///
/// CONSUMES both variables whether or not it succeeds, so no updater helper, no
/// user shell and no second attempt can ever observe them. On success it also
/// unlinks the socket: the parent `_exit`s inside `seamless::commit_and_exit`
/// and runs no destructor, so on the one path that matters this is the only
/// thing that retires the node.
///
/// `expected_nonce` is the attempt nonce this process was told about in
/// `ATERM_SEAMLESS_NONCE`; a grant naming any other attempt is refused.
///
/// SAFETY (env): the caller guarantees this runs on the single-threaded startup
/// path, before any thread or session spawn.
pub(crate) fn claim_incoming(
    expected_nonce: &str,
    deadline: Instant,
) -> Result<ClaimedHandoff, RendezvousError> {
    let (path, secret) = rendezvous_env(
        aterm_log::env::take(ENV_RENDEZVOUS),
        aterm_log::env::take(ENV_CLAIM),
    )?;
    let claimed = dial_and_claim(&path, &secret, expected_nonce, deadline);
    if claimed.is_ok() {
        let _ = std::fs::remove_file(&path);
    }
    claimed
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

fn dial_and_claim(
    path: &Path,
    secret: &str,
    expected_nonce: &str,
    deadline: Instant,
) -> Result<ClaimedHandoff, RendezvousError> {
    let stream =
        CtlStream::connect(path).map_err(|error| RendezvousError::Dial(error.to_string()))?;
    // THE LISTENER MUST BE US (same uid) before the claim secret leaves this process:
    // the environment named a path, and a path is not an identity. A listener of any
    // other uid — whatever put it there — gets nothing.
    #[cfg(unix)]
    {
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        if crate::control_auth::peer_uid(&stream) != Some(uid) {
            return Err(RendezvousError::Dial(
                "rendezvous listener is not owned by this uid; refusing to present the claim"
                    .to_string(),
            ));
        }
    }
    let remaining = remaining_io_budget(deadline)?;
    stream
        .set_read_timeout(Some(remaining))
        .and_then(|()| stream.set_write_timeout(Some(remaining)))
        .map_err(|error| RendezvousError::Dial(error.to_string()))?;
    (&stream)
        .write_all(&claim_frame(secret))
        .and_then(|()| (&stream).flush())
        .map_err(|error| RendezvousError::Dial(error.to_string()))?;

    let grant_failed = |error: std::io::Error| RendezvousError::Grant(error.to_string());
    // The header is read by the descriptor-bearing receive. `fdpass` documents
    // that ALL of a message's descriptors arrive on the call that dequeues its
    // first byte, so a short read here could cost bytes and never descriptors —
    // and the buffer is the whole fixed header anyway. `max_fds` is the message
    // ceiling because the exact count is in the body, which has not been read
    // yet; the exact check happens below, and an over-count is `fdpass`'s own
    // fatal `InvalidData`.
    //
    // The read bound is RECOMPUTED here rather than inherited from the one set
    // above: a socket timeout is spent per syscall, so the write and this receive
    // would otherwise be allowed the full budget each.
    let mut header = [0u8; GRANT_HEADER_LEN];
    stream
        .set_read_timeout(Some(remaining_io_budget(deadline)?))
        .map_err(grant_failed)?;
    let received = fdpass::recv_with_fds(&stream, &mut header, fdpass::MAX_FDS)
        .map_err(|error| RendezvousError::Grant(error.to_string()))?;
    let fds = received.fds;
    if received.bytes == 0 && fds.is_empty() {
        return Err(RendezvousError::Grant(
            "the parent closed the rendezvous".to_string(),
        ));
    }
    if received.bytes < GRANT_HEADER_LEN {
        read_frame_by_deadline(
            &stream,
            &mut header[received.bytes..],
            deadline,
            &grant_failed,
        )?;
    }
    if &header[..GRANT_MAGIC.len()] != GRANT_MAGIC {
        return Err(RendezvousError::Grant("wrong magic".to_string()));
    }
    let body_len = usize::from(u16::from_be_bytes([
        header[GRANT_MAGIC.len()],
        header[GRANT_MAGIC.len() + 1],
    ]));
    if body_len > MAX_GRANT_BODY_BYTES {
        return Err(RendezvousError::Grant("oversized body".to_string()));
    }
    let mut body = vec![0u8; body_len];
    read_frame_by_deadline(&stream, &mut body, deadline, &grant_failed)?;
    let body =
        String::from_utf8(body).map_err(|_| RendezvousError::Grant("not UTF-8".to_string()))?;
    let sessions = parse_grant_body(&body, expected_nonce)?;

    let expected = sessions.len() + RENDEZVOUS_CHANNEL_FDS;
    if fds.len() != expected {
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
        source.set_read_timeout(Some(remaining)).map_err(wrap)?;
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
                Instant::now() + Duration::from_secs(10),
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
                Instant::now() + Duration::from_secs(10),
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

        let started = Instant::now();
        let refused = rendezvous.accept_claim(
            Some(own_pid()),
            Instant::now() + Duration::from_secs(30),
            &|| false,
        );
        assert_eq!(refused.err(), Some(RendezvousError::Claim));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the cap ended the attempt; nothing here waited out the deadline"
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
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let dribbler = {
            let stop = std::sync::Arc::clone(&stop);
            std::thread::spawn(move || {
                let stream = CtlStream::connect(&path).expect("connect");
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
        assert!(
            waited < budget + Duration::from_secs(1),
            "the WHOLE frame is bounded by the deadline; 70 bytes at 40 ms each would be 2.8 s, \
             and this waited {waited:?}"
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
        let started = Instant::now();
        let cancelled = rendezvous.accept_claim(
            Some(own_pid()),
            Instant::now() + Duration::from_secs(30),
            &|| true,
        );
        assert_eq!(cancelled.err(), Some(RendezvousError::Cancelled));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the abort predicate is checked between poll slices, not after the deadline"
        );
    }

    /// Bind a rendezvous under a test-only nonce, or skip when this machine has
    /// no usable control directory or a `$HOME` too long for `sun_path` — the
    /// same two refusals production falls back to the fork lane on.
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
}
